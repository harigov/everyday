// State for Money.
//
// A sibling of `feed.svelte.ts`. Everything here goes through `api` -- the
// mock backend (`mock-money.ts`) stands in for the vault in a browser.
//
// The public names below are the contract `components/Money*.svelte` are
// written against: each name and signature was laid down before the bodies
// were, and members added since are marked as such in their own comments.
//
// Rules that shape it:
//
//   1. Amounts are integers of minor units, never floats; `money.ts` formats
//      and parses them.
//   2. A person's edit shows at once and is put back if the vault refuses
//      (as the Feed's read state is).
//   3. Linking a bank never runs Plaid code here: `connectBank` asks the
//      service for a Hosted Link page, opens it in the browser with
//      `openExternal`, and polls `checkBankLink` every two seconds until it
//      answers something other than `waiting`.
//   4. The Plaid secret is sent once, by `savePlaid`, and never held: the
//      field that took it is cleared, and nothing the service answers ever
//      contains it.
//   5. A bank's words -- a payee, a transfer's memo -- are the other side's,
//      and are only ever text. Nothing here builds markup from them, and the
//      assistant is told what is on screen by reference, never by payee.
//
// What is loaded follows what is on screen. The banks, accounts, categories
// and rules are small and every pane needs some of them, so they are always
// read; the month's list and its summary are read for every pane too, since
// the sidebar and the overview both quote them. The reports that cover a
// year -- cash flow, net worth, what repeats -- are read only on the panes
// that draw them.

import { api } from './api'
import { plural } from './format'
import {
  importNote,
  linkPageUrl,
  isMonth,
  mainCurrencyOf,
  monthLabel,
  monthRange,
  netWorthOf,
  redactSecret,
  shiftMonth as stepMonth,
  shownPayee,
  sortCategories,
  syncNote,
  thisMonth,
} from './money'
import { notify } from './notify.svelte'
import { seen, type Showing } from './onscreen'
import { openExternal } from './open-external'
import { pref } from './prefs'
import { app, errorMessage, handle, isLocked, quietly } from './state.svelte'
import { debounce } from './store/debounce'
import { latest } from './store/latest'
import { optimisticRemove } from './store/optimistic-remove'
import { guardedRefresh } from './store/refresh'
import { todayIso } from './time'
import type {
  AccountKind,
  BankAccount,
  BankAccountId,
  BankLink,
  BankLinkId,
  CashFlowMonth,
  CsvMapping,
  ImportFormat,
  ImportPreview,
  LinkCheck,
  LinkSummary,
  MoneyCategory,
  MoneyCategoryId,
  MoneyImportReport,
  MoneyRule,
  MoneyRuleId,
  MoneySyncReport,
  MonthSummary,
  NetWorthPoint,
  NewTxn,
  PlaidEnvironment,
  PlaidSetup,
  Recurring,
  Txn,
  TxnId,
  TxnPage,
  TxnPatch,
  TxnQuery,
} from './types'
import { VaultError } from './types'

/** The panes of the app, as the sidebar lists them. */
export const MONEY_VIEWS = [
  'overview',
  'transactions',
  'budget',
  'reports',
  'accounts',
  'rules',
] as const
export type MoneyView = (typeof MONEY_VIEWS)[number]

/**
 * What the transactions list is narrowed to by category: one (its id), none
 * (the word `'none'`), or any (`null`). An id is a string, so the type says
 * `string`; `'none'` can never collide with one, which is a UUID.
 */
export type CategoryFilter = string | null

/** A bank sign-in in flight. */
export interface Linking {
  session: string
  url: string
  /** Set when signing in again to an existing link. */
  relink: BankLinkId | null
}

/**
 * A destructive act waiting to hear yes or no -- shaped for `ChoiceDialog`,
 * which `App.svelte` draws over whatever is on screen while one is set.
 * Added after the skeleton: it is how the methods marked "asks first" ask.
 */
export interface MoneyQuestion {
  title: string
  detail: string
  options: { label: string; danger?: boolean; run: () => void }[]
  cancel: () => void
}

/** How many transactions a page holds: the service's own default. */
const PAGE = 200
/** The most the service gives at once, which bounds how much a reload covers. */
const MOST = 1000
/** How many months the cash flow report draws. */
const CASH_FLOW_MONTHS = 12
/** What Money tints the window with: the palette's green, the default accent being orange. */
const MONEY_ACCENT = '#15803d'
/** The largest file sent to be read. The service refuses more; this saves sending it. */
const MAX_IMPORT_CHARS = 5 * 1024 * 1024
/**
 * Stop asking about a sign-in after this long. The service forgets one
 * after thirty minutes and would answer `expired` anyway; this is only so
 * that a window left open on a sign-in nobody finished stops asking.
 */
const LINK_GIVE_UP_MS = 31 * 60_000

/**
 * Which pane was last open, remembered on this device: chrome, not a fact
 * about the vault, so kept locally the way the Feed's chip is. Read in the
 * constructor, so the app does not draw the overview and visibly flip.
 */
const viewPref = pref<MoneyView>(
  'karmayoga.money.view',
  (raw) => MONEY_VIEWS.find((v) => v === raw) ?? 'overview',
  'overview',
)

/**
 * A fresh id for a category or a rule that has not been saved yet.
 *
 * The same stopgap `accounts.svelte.ts`'s `mintAccountId` documents: the
 * surface has no `new_money_category` or `new_money_rule` to mint a blank
 * one the way `new_role` does, `save_*` is a plain upsert, and `localhost`
 * and the packaged app's own origin are both secure contexts. A v4 id
 * round-trips through the wire's `Uuid` exactly as a v7 one does; it only
 * sorts differently, which nothing here relies on.
 */
function mintId(): string {
  return crypto.randomUUID()
}

class MoneyState {
  // -- where somebody is ----------------------------------------------------
  view = $state<MoneyView>('overview')
  /** `YYYY-MM`: the month the budget, the overview and the list show. */
  month = $state<string>(thisMonth())
  /** The search bar's text, over payee, description and note. */
  query = $state('')
  accountFilter = $state<BankAccountId | null>(null)
  categoryFilter = $state<CategoryFilter>(null)

  // -- what the vault holds -------------------------------------------------
  links = $state<LinkSummary[]>([])
  accounts = $state<BankAccount[]>([])
  categories = $state<MoneyCategory[]>([])
  rules = $state<MoneyRule[]>([])
  /** `null` until loaded. */
  plaid = $state<PlaidSetup | null>(null)

  // -- the transactions list ------------------------------------------------
  page = $state<TxnPage | null>(null)
  loading = $state(false)
  /** The open transaction, in the detail pane. */
  openId = $state<TxnId | null>(null)
  /** Ticked rows, for acting on many at once. */
  selected = $state<TxnId[]>([])

  // -- reports --------------------------------------------------------------
  summary = $state<MonthSummary | null>(null)
  cashFlow = $state<CashFlowMonth[]>([])
  netWorth = $state<NetWorthPoint[]>([])
  recurring = $state<Recurring[]>([])

  // -- linking and syncing --------------------------------------------------
  linking = $state<Linking | null>(null)
  linkError = $state<string | null>(null)
  syncing = $state(false)

  // -- added after the skeleton ---------------------------------------------
  /** Loaded at least once since the vault opened: an empty list means empty, not "not yet". */
  loaded = $state(false)
  /** A line under the list after something happened -- a sync, an import. Cleared on navigation. */
  note = $state<string | null>(null)
  /** "Add a transaction" was asked for, by `create` or a button: its sheet is open. */
  adding = $state(false)
  /**
   * Whether the browser could be opened for the sign-in in flight. False
   * where there is no browser to hand it to -- a window on another
   * machine's vault -- and the page's address (`linking.url`) is shown to
   * copy instead.
   */
  linkOpened = $state(true)
  /** The question a destructive act is waiting on, or `null`. See `MoneyQuestion`. */
  question = $state.raw<MoneyQuestion | null>(null)
  /** How often a sign-in in flight is asked about, in milliseconds. A test shortens it. */
  pollMs = 2_000

  /** Which load is the current one, per thing loaded. See `feed.svelte.ts`'s `#load`. */
  #basicsGen = latest()
  #listGen = latest()
  #pagingGen = latest()
  #summaryGen = latest()
  #cashGen = latest()
  #worthGen = latest()
  #recurringGen = latest()
  #plaidGen = latest()
  /** Which sign-in is the current one; cancelling, finishing or a lock retires it. */
  #linkGen = latest()
  #pollTimer: ReturnType<typeof setTimeout> | null = null
  /** A page is being fetched. */
  #more = false
  /** Syncs in flight: the spinner lasts until the last of them answers. */
  #syncs = 0
  /** The row a Shift-click extends from. */
  #anchor: TxnId | null = null
  /** Writes sent and not yet answered, so the close handshake can wait for them. */
  #writes = new Set<Promise<unknown>>()
  /** The bar's search, once typing pauses. */
  #queryDebounce = debounce(() => void this.#onQuery(), 250)

  constructor() {
    // Registered once, here, for the reason the Feed gives: `MoneyView`
    // calls `start` on every mount, and a hook added there would be added
    // again each time the app is switched back to.
    app.onLock(() => this.reset())
    app.onFlush(() => this.flush())
    this.view = viewPref.get()
  }

  // ── lifecycle ────────────────────────────────────────────────────────

  /**
   * Load everything the first screen needs. Called when the app opens.
   *
   * Idempotent: mounting the view again reloads over what is there rather
   * than from nothing, so nothing blanks out on the way in.
   */
  async start(): Promise<void> {
    if (!app.supportsMoney) return
    await Promise.all([this.reload(), this.loadPlaid()])
    if (app.screen === 'main') this.loaded = true
  }

  /**
   * Forget everything: the vault locked or changed.
   *
   * The pane stays: it is chrome, not content. Everything else is what a
   * bank said about somebody's money, which is what a lock exists to put
   * away. A sign-in in flight stops being asked about -- the service drops
   * its sessions at the lock too -- and no timer is left running.
   */
  reset(): void {
    this.#queryDebounce.cancel()
    // The service drops its sessions when the vault locks, but a screen lock
    // leaves the vault open; told here, the sign-in goes either way.
    const linking = this.linking
    if (linking) void api.cancelBankLink(linking.session).catch(() => {})
    this.#endLink()
    // A question nobody can answer behind the lock screen is answered "no".
    this.question?.cancel()
    this.question = null
    // Nothing on its way may land after this: those rows are the decrypted
    // contents this reset exists to drop.
    for (const gen of [
      this.#basicsGen,
      this.#listGen,
      this.#pagingGen,
      this.#summaryGen,
      this.#cashGen,
      this.#worthGen,
      this.#recurringGen,
      this.#plaidGen,
    ]) {
      gen.next()
    }
    this.#more = false
    this.#syncs = 0
    this.#anchor = null
    this.month = thisMonth()
    this.query = ''
    this.accountFilter = null
    this.categoryFilter = null
    this.links = []
    this.accounts = []
    this.categories = []
    this.rules = []
    this.plaid = null
    this.page = null
    this.loading = false
    this.openId = null
    this.selected = []
    this.summary = null
    this.cashFlow = []
    this.netWorth = []
    this.recurring = []
    this.linking = null
    this.linkError = null
    this.syncing = false
    this.loaded = false
    this.note = null
    this.adding = false
    this.linkOpened = true
  }

  /** Reload what the current view shows (links, accounts, categories, the list, the reports). */
  async reload(): Promise<void> {
    if (!app.supportsMoney) return
    // A sync, an import or a rules run that was answered after the screen
    // locked must not fill the lists back in behind the lock screen: what a
    // reset dropped stays dropped until somebody unlocks.
    if (app.screen !== 'main') return
    if (!isMonth(this.month)) this.month = thisMonth()
    await this.#loadBasics(false)
    await Promise.all([this.#loadList(false), this.#loadSummary(false), this.#loadExtras(false)])
  }

  /**
   * Wait for anything outstanding. Part of the close handshake.
   *
   * Nothing here is held back to be written later; what can be outstanding
   * is a recategorisation sent and not yet answered, which a window that
   * closed without waiting for it would lose.
   */
  async flush(): Promise<void> {
    await Promise.allSettled([...this.#writes])
  }

  /** Keep a write in view until it settles. See `flush`. */
  #track<T>(write: Promise<T>): Promise<T> {
    this.#writes.add(write)
    const done = () => this.#writes.delete(write)
    void write.then(done, done)
    return write
  }

  // ── navigation ───────────────────────────────────────────────────────

  setView(view: MoneyView): void {
    if (view === this.view) return
    this.view = view
    viewPref.set(view)
    this.note = null
    // The detail pane and the ticks belong to the list; another pane is
    // another place.
    if (view !== 'transactions') {
      this.#closeList()
      this.adding = false
    }
    void this.#loadExtras(true)
  }

  /** Show `month` (`YYYY-MM`) and reload what depends on it. */
  async setMonth(month: string): Promise<void> {
    if (!isMonth(month)) return
    this.month = month
    this.#closeList()
    this.note = null
    await Promise.all([
      this.#loadList(true),
      this.#loadSummary(false),
      this.view === 'reports' ? this.#loadCashFlow(false) : Promise.resolve(),
    ])
  }

  /** One month back (-1) or forward (+1). */
  async shiftMonth(delta: number): Promise<void> {
    await this.setMonth(stepMonth(isMonth(this.month) ? this.month : thisMonth(), delta))
  }

  /**
   * Debounced: the search bar types into this.
   *
   * Only transactions are searched, so typing from the overview takes the
   * list along with it: what was typed is a payee somebody is looking for,
   * and the list is where it will be.
   */
  setQuery(query: string): void {
    this.query = query
    if (query.trim() && this.view !== 'transactions') this.setView('transactions')
    this.#queryDebounce.call()
  }

  async setAccount(id: BankAccountId | null): Promise<void> {
    this.accountFilter = id
    this.#closeList()
    this.note = null
    await this.#loadList(true)
  }

  async setCategory(filter: CategoryFilter): Promise<void> {
    this.categoryFilter = filter
    this.#closeList()
    this.note = null
    await this.#loadList(true)
  }

  /** The next page of the list, appended. */
  async loadMore(): Promise<void> {
    const offset = this.page?.nextOffset
    if (offset === null || offset === undefined || this.#more || !app.supportsMoney) return
    this.#more = true
    const token = this.#pagingGen.next()
    try {
      const next = await api.listTransactions(this.#query(offset, PAGE))
      const current = this.page
      // A month, a filter or a search changed while this was on its way;
      // these rows belong to a list that is no longer showing.
      if (!this.#pagingGen.isCurrent(token) || !current || current.nextOffset !== offset) return
      const have = new Set(current.txns.map((t) => t.id))
      const fresh = next.txns.filter((t) => !have.has(t.id))
      this.page = { ...next, txns: [...current.txns, ...fresh] }
    } catch (e) {
      await quietly(e)
    } finally {
      this.#more = false
    }
  }

  /**
   * "Start the next thing" (`c`). Added after the skeleton.
   *
   * A transaction typed into an account kept by hand, where there is one --
   * a bank sends its own, so that is the only kind anybody types. With none,
   * the Accounts pane, where one is started or a bank connected.
   */
  create(): void {
    if (this.accounts.some((a) => !a.linkId && !a.hidden)) {
      this.setView('transactions')
      this.adding = true
    } else {
      this.setView('accounts')
    }
  }

  /** Open the add-a-transaction sheet. Added after the skeleton. */
  openAdd(): void {
    this.adding = true
  }

  closeAdd(): void {
    this.adding = false
  }

  // ── transactions ─────────────────────────────────────────────────────

  open(id: TxnId | null): void {
    this.openId = id
  }

  /**
   * Open the transaction `delta` rows below (positive) or above (negative):
   * `j` and `k`. Added after the skeleton. With nothing open either lands
   * on the first row; the end of what is loaded fetches on.
   */
  async move(delta: number): Promise<void> {
    let rows = this.page?.txns ?? []
    if (rows.length === 0) return
    const at = rows.findIndex((t) => t.id === this.openId)
    let to = at < 0 ? 0 : at + delta
    if (this.page?.nextOffset !== null && this.page?.nextOffset !== undefined) {
      if (to >= rows.length) {
        await this.loadMore()
        rows = this.page?.txns ?? []
      } else if (to >= rows.length - 5) {
        void this.loadMore()
      }
    }
    to = Math.min(rows.length - 1, Math.max(0, to))
    const target = rows[to]
    if (target) this.open(target.id)
  }

  /** Tick or untick one row; with `extend`, everything from the last ticked to it. */
  toggleSelect(id: TxnId, extend = false): void {
    const rows = this.page?.txns ?? []
    if (extend && this.#anchor) {
      const a = rows.findIndex((t) => t.id === this.#anchor)
      const b = rows.findIndex((t) => t.id === id)
      if (a >= 0 && b >= 0) {
        const [lo, hi] = a < b ? [a, b] : [b, a]
        const ticked = new Set(this.selected)
        const range = rows.slice(lo, hi + 1).map((t) => t.id)
        this.selected = [...this.selected, ...range.filter((r) => !ticked.has(r))]
        return
      }
    }
    this.selected = this.selected.includes(id)
      ? this.selected.filter((x) => x !== id)
      : [...this.selected, id]
    this.#anchor = id
  }

  selectAll(): void {
    this.selected = (this.page?.txns ?? []).map((t) => t.id)
  }

  clearSelection(): void {
    this.selected = []
    this.#anchor = null
  }

  /** Set (or with `null`, clear) the category of these transactions. */
  async categorize(ids: TxnId[], category: MoneyCategoryId | null): Promise<void> {
    if (ids.length === 0) return
    const before = this.#remember(ids)
    for (const t of this.#rows(ids)) {
      t.category = category
      t.categorizedBy = category ? { by: 'person' } : { by: 'nobody' }
    }
    await this.#update(ids, { category }, before, true)
  }

  async setNote(id: TxnId, note: string): Promise<void> {
    const text = note.trim()
    const row = this.#rows([id])[0]
    if (row && row.note === text) return
    const before = this.#remember([id])
    if (row) row.note = text
    await this.#update([id], { note: text }, before, false)
  }

  /** The person's name for the payee; empty restores the bank's. */
  async setPayee(id: TxnId, payee: string): Promise<void> {
    const row = this.#rows([id])[0]
    let name = payee.trim()
    // The bank's own name typed back in is no rename at all.
    if (row && name === row.payee.trim()) name = ''
    if (row && row.payeeOverride === name) return
    const before = this.#remember([id])
    if (row) row.payeeOverride = name
    await this.#update([id], { payeeOverride: name }, before, false)
  }

  async setExcluded(ids: TxnId[], excluded: boolean): Promise<void> {
    if (ids.length === 0) return
    const before = this.#remember(ids)
    for (const t of this.#rows(ids)) t.excluded = excluded
    await this.#update(ids, { excluded }, before, true)
  }

  async addManual(txn: NewTxn): Promise<Txn | null> {
    if (!app.supportsMoney) return null
    try {
      const added = await this.#track(api.addManualTransaction(txn))
      this.adding = false
      // Typed into another month than the one on screen, it would seem to
      // have gone nowhere; say where it went.
      const month = added.date.slice(0, 7)
      this.note = month === this.month ? null : `Added, in ${monthLabel(month)}.`
      await Promise.all([this.#loadList(false), this.#loadSummary(true)])
      return added
    } catch (e) {
      await handle(e)
      return null
    }
  }

  /** Only in accounts kept by hand (see `canDelete`); asks first. */
  async deleteTxns(ids: TxnId[]): Promise<void> {
    if (ids.length === 0) return
    const rows = this.#rows(ids)
    if (rows.some((t) => !this.canDelete(t))) {
      // Said here rather than asked and then refused: the next sync would
      // bring a bank's line straight back.
      await handle(
        new VaultError(
          'invalid',
          'A transaction in a connected bank’s account cannot be deleted: the next sync would bring it back. Leave it out of the reports instead.',
        ),
      )
      return
    }
    const only = rows.length === 1 ? rows[0] : undefined
    const yes = await this.#ask(
      only ? `Delete “${shownPayee(only)}”?` : `Delete ${plural(ids.length, 'transaction')}?`,
      'Typed in or imported, and gone from this vault for good. Importing the same file again would bring imported ones back.',
      'Delete',
    )
    if (!yes) return
    const gone = new Set(ids)
    // Awaited, so the vault's view has landed before this answers -- and a
    // reload still in flight cannot put back what the next press removes.
    let done = false
    await optimisticRemove({
      optimistic: () => {
        if (this.openId && gone.has(this.openId)) this.openId = null
        this.selected = this.selected.filter((id) => !gone.has(id))
        const page = this.page
        if (page) {
          const kept = page.txns.filter((t) => !gone.has(t.id))
          this.page = {
            ...page,
            txns: kept,
            total: Math.max(0, page.total - (page.txns.length - kept.length)),
          }
        }
      },
      call: () => this.#track(api.deleteTransactions(ids)),
      onSuccess: () => (done = true),
      rollback: () => this.#reloadMonth(),
    })
    if (done) await this.#reloadMonth()
  }

  // ── banks and accounts ───────────────────────────────────────────────

  /**
   * Start a Hosted Link sign-in (or, with `relink`, sign in again), open it, poll.
   *
   * Only an address the service answered is opened, and only a secure one
   * on Plaid's own domain (`isLinkPageUrl`): the service is trusted, and
   * this is the second lock on the same door -- a page this window hands to
   * the browser is a page somebody will type a bank password into.
   */
  async connectBank(relink: BankLinkId | null = null): Promise<void> {
    if (!app.supportsMoney) return
    // One sign-in at a time: a second press starts over.
    if (this.linking) await this.cancelLink()
    this.linkError = null
    this.note = null
    // Minted before the service is asked, so a lock, a reset or a second
    // press while it answers makes this one stale -- and a stale sign-in is
    // cancelled rather than opened in the browser behind the lock screen.
    const token = this.#linkGen.next()
    let start: { session: string; url: string }
    try {
      start = await api.startBankLink(relink)
    } catch (e) {
      if (isLocked(e)) await handle(e)
      // Beside the button that was pressed, not a banner over the window:
      // "add your keys first" is about this button.
      else this.linkError = errorMessage(e)
      return
    }
    if (!this.#linkGen.isCurrent(token)) {
      void api.cancelBankLink(start.session).catch(() => {})
      return
    }
    const page = linkPageUrl(start.url)
    if (!page) {
      this.linkError = 'The bank’s sign-in page had an address this app will not open.'
      void api.cancelBankLink(start.session).catch(() => {})
      return
    }
    this.linking = { session: start.session, url: page, relink }
    this.linkOpened = true
    // The address as the URL parser wrote it back, not as it arrived: what
    // was checked is exactly what is opened.
    const opened = await openExternal(page)
    if (!this.#linkGen.isCurrent(token)) return
    this.linkOpened = opened
    this.#schedulePoll(token, start.session, relink, Date.now())
  }

  async cancelLink(): Promise<void> {
    const linking = this.linking
    this.#endLink()
    if (!linking) return
    try {
      await api.cancelBankLink(linking.session)
    } catch (e) {
      await quietly(e)
    }
  }

  /** Sync one link, or every link. */
  async syncNow(link: BankLinkId | null = null): Promise<MoneySyncReport[]> {
    if (!app.supportsMoney) return []
    // Counted, not flagged, as the Feed's refresh is: "Sync now" on two banks
    // at once keeps the spinner until the second answers.
    this.#syncs += 1
    this.syncing = true
    try {
      const reports = await api.syncBankLinks(link)
      this.note = syncNote(reports)
      await this.reload()
      return reports
    } catch (e) {
      await handle(e)
      return []
    } finally {
      this.#syncs = Math.max(0, this.#syncs - 1)
      this.syncing = this.#syncs > 0
    }
  }

  async saveLink(link: BankLink): Promise<void> {
    const plain = $state.snapshot(link)
    const summary = this.links.find((l) => l.link.id === plain.id)
    if (summary) summary.link = plain
    try {
      await this.#track(api.saveBankLink(plain))
      await this.#loadBasics(true)
    } catch (e) {
      await handle(e, () => this.#loadBasics(true))
    }
  }

  /** Asks first. With `keepHistory`, the accounts stay as manual ones. */
  async disconnect(id: BankLinkId, keepHistory: boolean): Promise<void> {
    const summary = this.links.find((l) => l.link.id === id)
    if (!summary) return
    const accounts = plural(summary.accounts, 'account')
    const yes = await this.#ask(
      `Disconnect “${summary.link.name}”?`,
      keepHistory
        ? `Its ${accounts} and their transactions stay, kept by hand from now on. Nothing more arrives from the bank.`
        : `Its ${accounts} and every transaction in them are deleted from this vault.`,
      'Disconnect',
    )
    if (!yes) return
    const theirs = new Set(this.accounts.filter((a) => a.linkId === id).map((a) => a.id))
    // Awaited, so the vault's view has landed before this answers -- and a
    // reload still in flight cannot put back what the next press removes.
    let done = false
    await optimisticRemove({
      optimistic: () => {
        this.links = this.links.filter((l) => l.link.id !== id)
        if (keepHistory) {
          // What the vault does to them: kept, and kept by hand from now on.
          this.accounts = this.accounts.map((a) =>
            theirs.has(a.id) ? { ...a, linkId: null, providerAccountId: '' } : a,
          )
          return
        }
        this.accounts = this.accounts.filter((a) => !theirs.has(a.id))
        if (this.accountFilter && theirs.has(this.accountFilter)) this.accountFilter = null
      },
      call: () => this.#track(api.deleteBankLink(id, keepHistory)),
      onSuccess: () => (done = true),
      rollback: () => this.reload(),
    })
    if (done) await this.reload()
  }

  async saveAccount(account: BankAccount): Promise<void> {
    const plain = $state.snapshot(account)
    const at = this.accounts.findIndex((a) => a.id === plain.id)
    const hiding = at >= 0 && this.accounts[at]!.hidden !== plain.hidden
    if (at >= 0) this.accounts[at] = plain
    try {
      await this.#track(api.saveBankAccount(plain))
      // Hiding takes an account out of the list and every report; anything
      // else is the sidebar's alone.
      if (hiding) await this.reload()
      else await this.#loadBasics(true)
    } catch (e) {
      await handle(e, () => this.#loadBasics(true))
    }
  }

  async createManualAccount(
    name: string,
    kind: AccountKind,
    currency: string,
  ): Promise<BankAccount | null> {
    if (!app.supportsMoney) return null
    try {
      const made = await this.#track(
        api.createManualAccount(name.trim(), kind, currency.trim().toUpperCase()),
      )
      await this.#loadBasics(true)
      return made
    } catch (e) {
      await handle(e)
      return null
    }
  }

  /** Manual accounts only; asks first. */
  async deleteAccount(id: BankAccountId): Promise<void> {
    const target = this.accountById(id)
    if (!target) return
    if (target.linkId) {
      await handle(
        new VaultError(
          'invalid',
          'An account from a bank comes back with the next sync. Hide it, or disconnect the bank.',
        ),
      )
      return
    }
    const yes = await this.#ask(
      `Delete “${target.name}”?`,
      'It is deleted from this vault with every transaction in it.',
      'Delete',
    )
    if (!yes) return
    // Awaited, so the vault's view has landed before this answers -- and a
    // reload still in flight cannot put back what the next press removes.
    let done = false
    await optimisticRemove({
      optimistic: () => {
        this.accounts = this.accounts.filter((a) => a.id !== id)
        if (this.accountFilter === id) this.accountFilter = null
      },
      call: () => this.#track(api.deleteBankAccount(id)),
      onSuccess: () => (done = true),
      rollback: () => this.reload(),
    })
    if (done) await this.reload()
  }

  async previewImport(
    content: string,
    format: ImportFormat | null = null,
  ): Promise<ImportPreview | null> {
    if (!app.supportsMoney || !(await this.#importable(content))) return null
    try {
      // Amounts read in the main currency's places, which is right for every
      // currency but the handful with none or three; the import itself
      // reads them in the chosen account's.
      return await api.previewImport(content, format, this.mainCurrency)
    } catch (e) {
      await handle(e)
      return null
    }
  }

  async importFile(
    accountId: BankAccountId,
    content: string,
    format: ImportFormat | null = null,
    mapping: CsvMapping | null = null,
  ): Promise<MoneyImportReport | null> {
    if (!app.supportsMoney || !(await this.#importable(content))) return null
    try {
      const report = await this.#track(
        api.importTransactions(
          accountId,
          content,
          format,
          mapping ? $state.snapshot(mapping) : null,
        ),
      )
      await this.reload()
      this.note = importNote(report)
      return report
    } catch (e) {
      await handle(e)
      return null
    }
  }

  /** Too large to send is said here, rather than shipped across and refused. */
  async #importable(content: string): Promise<boolean> {
    if (content.length <= MAX_IMPORT_CHARS) return true
    await handle(new VaultError('invalid', 'That file is larger than 5 MB.'))
    return false
  }

  // ── categories, budgets, rules ───────────────────────────────────────

  async saveCategory(category: MoneyCategory): Promise<void> {
    await this.#saveCategory({ ...$state.snapshot(category), updatedAt: new Date().toISOString() })
  }

  /** Asks first; what was in it becomes uncategorised. */
  async deleteCategory(id: MoneyCategoryId): Promise<void> {
    const target = this.categoryById(id)
    if (!target) return
    const yes = await this.#ask(
      `Delete “${target.name}”?`,
      'What is in it becomes uncategorised, and rules that put things there stop doing so.',
      'Delete',
    )
    if (!yes) return
    // Awaited, so the vault's view has landed before this answers -- and a
    // reload still in flight cannot put back what the next press removes.
    let done = false
    await optimisticRemove({
      optimistic: () => {
        this.categories = this.categories.filter((c) => c.id !== id)
        if (this.categoryFilter === id) this.categoryFilter = null
      },
      call: () => this.#track(api.deleteMoneyCategory(id)),
      onSuccess: () => (done = true),
      rollback: () => this.reload(),
    })
    if (done) await this.reload()
  }

  /** Set a category's monthly budget (minor units), or clear it with `null`. */
  async setBudget(id: MoneyCategoryId, amount: number | null): Promise<void> {
    const category = this.categoryById(id)
    if (!category) return
    if (amount !== null && (!Number.isInteger(amount) || amount < 0)) {
      await handle(new VaultError('invalid', 'A budget is an amount of zero or more.'))
      return
    }
    // The budget's row moves at once; the summary read back afterwards
    // settles anything this could not know, such as a row that is new.
    const summary = this.summary
    const row = summary?.categories.find((c) => c.categoryId === id)
    if (summary && row) {
      summary.budgeted += (amount ?? 0) - (row.budget ?? 0)
      row.budget = amount
    }
    await this.#saveCategory({
      ...$state.snapshot(category),
      budget: amount,
      updatedAt: new Date().toISOString(),
    })
  }

  /** A new expense category with this name, saved; `null` if refused. */
  async newCategory(name: string, group = ''): Promise<MoneyCategory | null> {
    const trimmed = name.trim()
    if (!trimmed) return null
    const now = new Date().toISOString()
    const made: MoneyCategory = {
      id: mintId(),
      name: trimmed,
      group: group.trim(),
      kind: 'expense',
      color: '',
      archived: false,
      // Last of all, which `groupCategories` puts last within its heading.
      sortOrder: this.categories.reduce((n, c) => Math.max(n, c.sortOrder + 1), 0),
      createdAt: now,
      updatedAt: now,
    }
    if (!(await this.#saveCategory(made))) return null
    return this.categoryById(made.id) ?? made
  }

  /**
   * Store a category: on screen first, then the vault, then read back with
   * the month's summary, whose budgets it may have moved. Answers whether
   * the vault took it.
   */
  async #saveCategory(category: MoneyCategory): Promise<boolean> {
    if (!app.supportsMoney) return false
    const at = this.categories.findIndex((c) => c.id === category.id)
    if (at >= 0) this.categories[at] = category
    else this.categories = [...this.categories, category]
    try {
      await this.#track(api.saveMoneyCategory(category))
      await Promise.all([this.#loadBasics(true), this.#loadSummary(true)])
      return true
    } catch (e) {
      await handle(e, () => Promise.all([this.#loadBasics(true), this.#loadSummary(true)]))
      return false
    }
  }

  async saveRule(rule: MoneyRule): Promise<void> {
    if (!app.supportsMoney) return
    const plain = { ...$state.snapshot(rule), updatedAt: new Date().toISOString() }
    const at = this.rules.findIndex((r) => r.id === plain.id)
    if (at >= 0) this.rules[at] = plain
    else this.rules = [...this.rules, plain]
    try {
      await this.#track(api.saveMoneyRule(plain))
      await this.#loadBasics(true)
    } catch (e) {
      await handle(e, () => this.#loadBasics(true))
    }
  }

  /**
   * A rule nobody has saved yet, with a fresh id and nothing in it, last in
   * the order -- what a "New rule" form starts from and hands to `saveRule`.
   * Added after the skeleton.
   */
  blankRule(): MoneyRule {
    const now = new Date().toISOString()
    return {
      id: mintId(),
      text: '',
      exact: false,
      accountId: null,
      minAmount: null,
      maxAmount: null,
      category: null,
      rename: '',
      exclude: false,
      sortOrder: this.rules.reduce((n, r) => Math.max(n, r.sortOrder + 1), 0),
      createdAt: now,
      updatedAt: now,
    }
  }

  /**
   * Delete a rule, with a way back. Not asked first -- the skeleton does not
   * mark it so, and a rule is a line somebody can write again -- but the
   * toast that says so can put it back as it was, id and all.
   */
  async deleteRule(id: MoneyRuleId): Promise<void> {
    const target = this.rules.find((r) => r.id === id)
    if (!target) return
    const kept = $state.snapshot(target)
    await optimisticRemove({
      optimistic: () => {
        this.rules = this.rules.filter((r) => r.id !== id)
      },
      call: () => this.#track(api.deleteMoneyRule(id)),
      onSuccess: () => {
        notify.info('Rule deleted', {
          key: 'money-rule-deleted',
          timeout: 10_000,
          action: { label: 'Undo', run: () => void this.saveRule(kept) },
        })
        void this.#loadBasics(true)
      },
      rollback: () => this.#loadBasics(true),
    })
  }

  /** Run the rules (or one) over everything whose category is still open. */
  async applyRules(rule: MoneyRuleId | null = null): Promise<number> {
    if (!app.supportsMoney) return 0
    try {
      const changed = await this.#track(api.applyMoneyRules(rule))
      this.note =
        changed === 0 ? 'Nothing new to sort.' : `Sorted ${plural(changed, 'transaction')}.`
      await this.#reloadMonth()
      return changed
    } catch (e) {
      await handle(e)
      return 0
    }
  }

  /**
   * "Always put this payee in that category": a rule from a transaction, applied at once.
   *
   * The rule matches the bank's own payee, whole -- what the next one from
   * the same place will say -- or, where the bank gave none, its line. A
   * name the person gave the payee rides along as the rule's rename, so the
   * next one is called the same. This transaction is set as the person's
   * own choice; the rest follow the rule.
   */
  async ruleFromTxn(txn: Txn, category: MoneyCategoryId): Promise<void> {
    if (!app.supportsMoney) return
    const payee = txn.payee.trim()
    const made: MoneyRule = {
      ...this.blankRule(),
      text: payee || txn.description.trim(),
      exact: payee !== '',
      category,
      rename: txn.payeeOverride.trim(),
    }
    if (!made.text) return
    try {
      await this.#track(api.saveMoneyRule(made))
      await this.#track(api.updateTransactions([txn.id], { category }))
      const more = await this.#track(api.applyMoneyRules(made.id))
      this.note =
        more > 0
          ? `Saved the rule, and sorted ${plural(more, 'more transaction')}.`
          : 'Saved the rule.'
    } catch (e) {
      await handle(e)
    }
    await this.reload()
  }

  // ── Plaid keys ───────────────────────────────────────────────────────

  async loadPlaid(): Promise<void> {
    if (!app.supportsMoney) return
    const token = this.#plaidGen.next()
    try {
      const setup = await api.plaidSetup()
      if (this.#plaidGen.isCurrent(token)) this.plaid = setup
    } catch (e) {
      await handle(e)
    }
  }

  /**
   * Send the keys, once. Answers whether the vault took them.
   *
   * `secret` lives in this call's arguments and nowhere else: not in state,
   * not in a pref, not in a log line, not in a toast. What comes back is
   * `PlaidSetup`, which by construction cannot carry it. And should an
   * error message ever quote what was sent, the secret is cut out of it
   * before it reaches the screen.
   */
  async savePlaid(
    clientId: string,
    secret: string,
    environment: PlaidEnvironment,
  ): Promise<boolean> {
    if (!app.supportsMoney) return false
    try {
      const setup = await this.#track(
        api.savePlaidCredentials(clientId.trim(), secret.trim(), environment),
      )
      this.#plaidGen.next()
      this.plaid = setup
      return true
    } catch (e) {
      if (isLocked(e)) {
        await handle(e)
        return false
      }
      const code = e instanceof VaultError ? e.code : 'unknown'
      await handle(new VaultError(code, redactSecret(errorMessage(e), secret)))
      return false
    }
  }

  async clearPlaid(): Promise<void> {
    if (!app.supportsMoney) return
    try {
      await this.#track(api.clearPlaidCredentials())
      await this.loadPlaid()
    } catch (e) {
      await handle(e)
    }
  }

  // ── derived ──────────────────────────────────────────────────────────

  /** The open transaction, if it is in the loaded page. */
  get txn(): Txn | null {
    const id = this.openId
    if (!id) return null
    return this.page?.txns.find((t) => t.id === id) ?? null
  }

  /** Accounts that are not hidden, in sidebar order. */
  get visibleAccounts(): BankAccount[] {
    return this.accounts.filter((a) => !a.hidden)
  }

  /** Assets minus liabilities across visible accounts in the main currency, minor units. */
  get netWorthNow(): number {
    return netWorthOf(this.accounts, this.mainCurrency)
  }

  /** What reports are counted in: the summary's currency, else the accounts' commonest, else USD. */
  get mainCurrency(): string {
    return this.summary?.currency ?? mainCurrencyOf(this.accounts)
  }

  /** Categories not archived, in order. */
  get activeCategories(): MoneyCategory[] {
    return sortCategories(this.categories.filter((c) => !c.archived))
  }

  /**
   * May this transaction be deleted? The vault's own rule: by account, not by
   * where it came from. One in a connected bank's account would come straight
   * back with the next sync; one in an account kept by hand -- typed, imported,
   * or kept when its bank was disconnected -- would not.
   */
  canDelete(txn: Txn): boolean {
    const account = this.accountById(txn.accountId)
    return !!account && account.linkId == null
  }

  categoryById(id: MoneyCategoryId | null | undefined): MoneyCategory | undefined {
    if (!id) return undefined
    return this.categories.find((c) => c.id === id)
  }

  accountById(id: BankAccountId | null | undefined): BankAccount | undefined {
    if (!id) return undefined
    return this.accounts.find((a) => a.id === id)
  }

  /** Links needing attention: a sign-in again, or an error. */
  get attention(): LinkSummary[] {
    return this.links.filter((l) => l.needsLogin || !!l.lastError)
  }

  /** The colour the window tints itself while Money is open. */
  get accent(): string {
    return MONEY_ACCENT
  }

  /**
   * What is on screen, for the assistant -- see `onscreen.ts`.
   *
   * `view` is the interface's own words -- the pane and the month -- and
   * never a payee or a category's name, which are a bank's words and the
   * person's. The account narrowing the list goes by id, and the open
   * transaction by id; the labels beside them are only for the strip above
   * the composer, and the service describes both from the vault.
   */
  get showing(): Showing {
    const month = monthLabel(this.month)
    const where: Record<MoneyView, string> = {
      overview: `the overview for ${month}`,
      transactions: `transactions for ${month}`,
      budget: `the budget for ${month}`,
      reports: 'reports: cash flow, spending by category and net worth',
      accounts: 'their accounts and linked banks',
      rules: 'the rules that sort transactions',
    }
    const listing = this.view === 'transactions'
    const words = [
      where[this.view],
      listing && this.categoryFilter === 'none' ? 'uncategorised only' : null,
      listing && this.categoryFilter && this.categoryFilter !== 'none'
        ? 'narrowed to one category'
        : null,
    ].filter(Boolean)
    const txn = listing ? this.txn : null
    return {
      view: words.join(', '),
      within: listing
        ? seen('bankAccount', this.accountFilter, this.accountById(this.accountFilter)?.name ?? '')
        : [],
      open: seen('txn', txn?.id, txn ? shownPayee(txn) : ''),
      query: listing ? this.query : null,
    }
  }

  // ── asking ───────────────────────────────────────────────────────────

  /**
   * Ask before something that cannot be taken back, and answer whether the
   * answer was yes. One question at a time: asking a second while the first
   * is up answers the first "no", since only one sheet can be on screen.
   */
  #ask(title: string, detail: string, yes: string): Promise<boolean> {
    this.question?.cancel()
    return new Promise((resolve) => {
      const answer = (value: boolean) => () => {
        if (this.question === question) this.question = null
        resolve(value)
      }
      const question: MoneyQuestion = {
        title,
        detail,
        options: [{ label: yes, danger: true, run: answer(true) }],
        cancel: answer(false),
      }
      this.question = question
    })
  }

  // ── linking ──────────────────────────────────────────────────────────

  #schedulePoll(token: number, session: string, relink: BankLinkId | null, began: number): void {
    if (this.#pollTimer) clearTimeout(this.#pollTimer)
    this.#pollTimer = setTimeout(() => {
      this.#pollTimer = null
      void this.#check(token, session, relink, began)
    }, this.pollMs)
  }

  /** Ask once how the sign-in is going, and act on the answer. */
  async #check(
    token: number,
    session: string,
    relink: BankLinkId | null,
    began: number,
  ): Promise<void> {
    if (!this.#linkGen.isCurrent(token)) return
    let answer: LinkCheck
    try {
      answer = await api.checkBankLink(session)
    } catch (e) {
      if (!this.#linkGen.isCurrent(token)) return
      this.#endLink()
      if (isLocked(e)) await handle(e)
      else this.linkError = errorMessage(e)
      return
    }
    // Cancelled, started over, or locked while the answer was on its way.
    if (!this.#linkGen.isCurrent(token)) return
    switch (answer.state) {
      case 'waiting':
        if (Date.now() - began > LINK_GIVE_UP_MS) {
          this.#endLink()
          this.linkError = 'The sign-in timed out.'
          void api.cancelBankLink(session).catch(() => {})
          return
        }
        this.#schedulePoll(token, session, relink, began)
        return
      case 'done':
        this.#endLink()
        this.note = relink ? 'Signed in again.' : 'Bank connected.'
        await this.reload()
        return
      case 'failed':
        this.#endLink()
        this.linkError = answer.message || 'The bank sign-in did not finish.'
        return
      case 'expired':
        this.#endLink()
        this.linkError = 'The sign-in timed out.'
        return
    }
  }

  /** Stop asking about the sign-in in flight: no timer left, no answer landed. */
  #endLink(): void {
    this.#linkGen.next()
    if (this.#pollTimer) clearTimeout(this.#pollTimer)
    this.#pollTimer = null
    this.linking = null
  }

  // ── loading ──────────────────────────────────────────────────────────

  /** The list on screen as a query: the month, and whatever narrows it. */
  #query(offset: number, limit: number): TxnQuery {
    const { from, to } = monthRange(this.month)
    const query: TxnQuery = { from, to, offset, limit }
    if (this.accountFilter) query.accounts = [this.accountFilter]
    if (this.categoryFilter === 'none') query.uncategorized = true
    else if (this.categoryFilter) query.categories = [this.categoryFilter]
    const text = this.query.trim()
    if (text) query.text = text
    return query
  }

  /** The banks, accounts, categories and rules: small, and wanted by every pane. */
  async #loadBasics(quiet: boolean): Promise<void> {
    if (!app.supportsMoney) return
    const token = this.#basicsGen.next()
    try {
      const [links, accounts, categories, rules] = await Promise.all([
        api.listBankLinks(),
        api.listBankAccounts(),
        api.listMoneyCategories(),
        api.listMoneyRules(),
      ])
      if (!this.#basicsGen.isCurrent(token)) return
      this.links = links
      this.accounts = accounts
      this.categories = categories
      this.rules = rules
      // Narrowed to something deleted elsewhere: everything, rather than an
      // empty list under a name the sidebar no longer has.
      const accountGone =
        this.accountFilter !== null && !accounts.some((a) => a.id === this.accountFilter)
      const categoryGone =
        this.categoryFilter !== null &&
        this.categoryFilter !== 'none' &&
        !categories.some((c) => c.id === this.categoryFilter)
      if (accountGone) this.accountFilter = null
      if (categoryGone) this.categoryFilter = null
      if (accountGone || categoryGone) void this.#loadList(true)
    } catch (e) {
      if (quiet) await quietly(e)
      else await handle(e)
    }
  }

  /**
   * Reload the list. Numbered, as the Feed's is: a month, a filter and a
   * search can all be in the air at once, and only the newest may land.
   * `fresh` is a navigation -- one page, from the top, ticks cleared --
   * otherwise it covers as much as was loaded, so an edit somewhere does
   * not shrink a list somebody had scrolled three pages down.
   */
  async #loadList(fresh: boolean): Promise<void> {
    if (!app.supportsMoney) return
    // A page on its way belongs to the list being replaced.
    this.#pagingGen.next()
    await guardedRefresh(
      this.#listGen,
      async (isCurrent) => {
        const covering = fresh ? PAGE : Math.min(MOST, Math.max(PAGE, this.page?.txns.length ?? 0))
        const page = await api.listTransactions(this.#query(0, covering))
        if (!isCurrent()) return
        this.page = page
        if (fresh) {
          this.selected = []
          this.#anchor = null
        } else {
          const listed = new Set(page.txns.map((t) => t.id))
          this.selected = this.selected.filter((id) => listed.has(id))
        }
      },
      { setLoading: (v) => (this.loading = v), onError: (e) => handle(e) },
    )
  }

  async #loadSummary(quiet: boolean): Promise<void> {
    if (!app.supportsMoney || !isMonth(this.month)) return
    const token = this.#summaryGen.next()
    try {
      const summary = await api.moneyMonth(this.month)
      if (this.#summaryGen.isCurrent(token)) this.summary = summary
    } catch (e) {
      if (quiet) await quietly(e)
      else await handle(e)
    }
  }

  async #loadCashFlow(quiet: boolean): Promise<void> {
    if (!app.supportsMoney || !isMonth(this.month)) return
    const token = this.#cashGen.next()
    try {
      const months = await api.moneyCashFlow(this.month, CASH_FLOW_MONTHS)
      if (this.#cashGen.isCurrent(token)) this.cashFlow = months
    } catch (e) {
      if (quiet) await quietly(e)
      else await handle(e)
    }
  }

  /** Net worth over the last year, to today. */
  async #loadNetWorth(quiet: boolean): Promise<void> {
    if (!app.supportsMoney) return
    const token = this.#worthGen.next()
    try {
      const from = `${stepMonth(thisMonth(), -(CASH_FLOW_MONTHS - 1))}-01`
      const points = await api.moneyNetWorth(from, todayIso())
      if (this.#worthGen.isCurrent(token)) this.netWorth = points
    } catch (e) {
      if (quiet) await quietly(e)
      else await handle(e)
    }
  }

  async #loadRecurring(quiet: boolean): Promise<void> {
    if (!app.supportsMoney) return
    const token = this.#recurringGen.next()
    try {
      const found = await api.moneyRecurring()
      if (this.#recurringGen.isCurrent(token)) this.recurring = found
    } catch (e) {
      if (quiet) await quietly(e)
      else await handle(e)
    }
  }

  /** The reports that cover a year, for the panes that draw them. */
  async #loadExtras(quiet: boolean): Promise<void> {
    if (this.view === 'overview') {
      await Promise.all([this.#loadNetWorth(quiet), this.#loadRecurring(quiet)])
    } else if (this.view === 'reports') {
      await Promise.all([
        this.#loadCashFlow(quiet),
        this.#loadNetWorth(quiet),
        this.#loadRecurring(quiet),
      ])
    }
  }

  /** The list and the month's summary, after a write that moved either. */
  async #reloadMonth(): Promise<void> {
    await Promise.all([this.#loadList(false), this.#loadSummary(true)])
  }

  /** The search, once typing pauses: a new list, from the top. */
  #onQuery(): void {
    this.#closeList()
    void this.#loadList(true)
  }

  /** The detail pane and the ticks, which belong to one list. */
  #closeList(): void {
    this.openId = null
    this.selected = []
    this.#anchor = null
  }

  // ── in place ─────────────────────────────────────────────────────────

  /** These transactions, as the list on screen holds them. */
  #rows(ids: TxnId[]): Txn[] {
    const wanted = new Set(ids)
    return (this.page?.txns ?? []).filter((t) => wanted.has(t.id))
  }

  /** What these rows say now, to put back if the vault refuses an edit. */
  #remember(ids: TxnId[]): Map<TxnId, Txn> {
    const kept = new Map<TxnId, Txn>()
    for (const t of this.#rows(ids)) kept.set(t.id, $state.snapshot(t))
    return kept
  }

  /**
   * Send a person's change to rows already changed on screen; on a refusal,
   * put them back as `before` remembers them. `totals`: the change can move
   * the month's numbers -- a category, an exclusion -- so the summary is
   * read again.
   */
  async #update(
    ids: TxnId[],
    patch: TxnPatch,
    before: Map<TxnId, Txn>,
    totals: boolean,
  ): Promise<void> {
    try {
      await this.#track(api.updateTransactions(ids, patch))
      if (totals) void this.#loadSummary(true)
    } catch (e) {
      await handle(e, async () => {
        for (const t of this.page?.txns ?? []) {
          const was = before.get(t.id)
          if (was) Object.assign(t, was)
        }
      })
    }
  }
}

export const money = new MoneyState()
