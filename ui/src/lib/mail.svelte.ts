// State for the Mail app.
//
// Shaped like `library.svelte.ts`: what is on screen, what has been loaded,
// a `#generation` guard against a slow load landing after a newer one. Two
// things are unusual here, both because a mailbox can hold a hundred
// thousand threads where a shelf holds a few hundred:
//
//   - Threads are kept in `$state.raw` pages with keyset cursors
//     (`nextCursor`), appended to rather than replaced, and handed to
//     `VirtualList` as the plain array it asks for -- see that component's
//     own doc for why a deep-reactive array of thousands of rows is a tax
//     nothing here needs to pay.
//   - Actions are optimistic: the row changes on screen the instant a key is
//     pressed, the command is fired after, and a failure restores exactly
//     what was there and says why -- `mail.ts`'s `applyRowPatch`/`revertRow`
//     and `removeRow`/`restoreRow` are the pure half of that, so the only
//     thing this file adds is which fields each action touches and what it
//     calls.

import * as mailApi from './mail-api'
import { accounts } from './accounts.svelte'
import { registerApply, type ChangeWithIds } from './live-apply'
import { proposals } from './proposals.svelte'
import {
  CATEGORY_TABS,
  applyInviteResponse,
  applyRowPatch,
  isSnoozedMailbox,
  isUnifiedMailbox,
  listTarget,
  mailboxDisplayName,
  mailboxHasTabs,
  mergeSearchPage,
  needsSyntheticSnoozedMailbox,
  neighbourThread,
  promoteGmailInbox,
  refreshLimit,
  removeRow,
  restoreRow,
  revertRow,
  snoozeChoices,
  stepCategoryTab,
  sumCategoryCounts,
  syncInProgress,
  syntheticSnoozedMailboxId,
  threadRange,
  unifiedMailboxId,
  unifiedMailboxes as buildUnifiedMailboxes,
  visibleThreadList,
} from './mail'
import { seen, type Showing } from './onscreen'
import { app, handle, quietly } from './state.svelte'
import { applySingleChange } from './store/live-patch'
import { guardedRefresh } from './store/refresh'
import type {
  AccountId,
  CategoryCount,
  Draft,
  DraftId,
  Mailbox,
  MailboxId,
  MailCategory,
  MailMessageId,
  MailSyncProgress,
  ScheduledSend,
  Thread,
  ThreadDetail,
  ThreadFilter,
  ThreadId,
  ThreadPage,
} from './types'

/** How long an unread recount waits after the last optimistic action before
 *  it actually asks the backend, per Bug 9: `openThreadById`'s auto-`markRead`
 *  means holding `j` down a long list used to fire one `listThreads` per
 *  mailbox for every row passed over it. */
const UNREAD_DEBOUNCE_MS = 500

/** How long a live thread change waits before the follow-up `refresh()`
 *  Bug 4 needs -- see `#applyChanges`'s own note on why `#patchOne` alone
 *  is not enough. Debounced so a batch of changes from one sync tick costs
 *  one reload of the page, not one per thread. */
const LIVE_REFRESH_DEBOUNCE_MS = 600

/** How many threads a page loads at once. */
const PAGE = 50

/** `markMailboxRead`'s page, and the most it does in one go. */
const MARK_PAGE = 200
const MARK_CAP = 2000

/** How many threads one `archive`/`trash` call carries when Quick cleanup
 *  acts on many at once -- a sender with years of newsletters can be a few
 *  thousand, and one command per few hundred keeps each round trip short
 *  enough that a failure partway loses only the chunk it was in. */
const BATCH_CHUNK = 200

/** How long `syncAll` waits before reading status back. `sync_account` only
 *  wakes the account's task and answers at once, before the pass has begun,
 *  so a status read straight after it still says "idle" -- and the button's
 *  spinner would stop the instant it started. */
const SYNC_SETTLE_MS = 1200

/** `null` selects every account this vault has. */
export type AccountScope = AccountId | null

class MailState {
  // ── what is on screen ─────────────────────────────────────────────

  selectedAccount = $state<AccountScope>(null)
  selectedMailbox = $state<MailboxId | null>(null)
  category = $state<MailCategory | null>(null)
  selectedThread = $state<ThreadId | null>(null)
  /**
   * Threads picked together -- Ctrl/Cmd+click toggles one, Shift+click takes
   * the run from the last one clicked, `x` toggles the one under the cursor
   * and Mod+A takes every thread loaded. While any are, the reading pane
   * offers what can be done to all of them at once instead of one open
   * thread, and the action keys (`e`, `#`, `u`, `h`, …) act on all of them.
   * Empty is the ordinary one-thread-at-a-time list. See `targets`.
   */
  checked = $state.raw<ReadonlySet<ThreadId>>(new Set())
  /** Where Shift+click's range starts: the last row clicked or toggled. */
  #anchor: ThreadId | null = null
  /** Which messages in the open thread are expanded, newest first by default. */
  expanded = $state<Set<string>>(new Set())
  composing = $state<Draft | null>(null)
  /** Is `composing` an inline reply drawn under the open thread's own
   *  messages, rather than filling the reading pane in place of a thread.
   *  A brand-new message (`compose()`) always fills the pane; a reply or
   *  forward (`reply()`/`forward()`, and a suggested reply's own "send"
   *  through `openInlineDraft`) is always inline. Neither is ever a dialog:
   *  writing happens where reading does. `closeCompose` always drops this
   *  back to `false`, so a stale `true` can never make the *next* draft
   *  this store opens inline by accident. */
  composeInline = $state(false)
  /**
   * The draft just put aside to make room for something else -- a thread
   * opened over a message being written in the pane, or another draft
   * opened over it. Read, and cleared, by that draft's own sheet as it
   * goes (`MailCompose.svelte`'s `onDestroy`), which alone knows whether
   * anything was written in it, and so whether "Draft saved · Open" is
   * worth saying. Not `$state`: nothing draws it.
   */
  parkedDraft: DraftId | null = null
  /** The threads the snooze picker is open for, or `null` -- one, or every
   *  thread `checked`. A store field rather than component state -- the
   *  same reason `overview.wantsLog` is -- so the `h` shortcut can open it
   *  from `shortcuts.svelte.ts`, which has no component of its own to reach
   *  into. */
  wantsSnooze = $state<ThreadId[] | null>(null)
  /** The threads the label picker is open for -- the proper picker `l`
   *  opens in place of a `window.prompt`. */
  wantsLabel = $state<ThreadId[] | null>(null)
  /**
   * The account whose Scheduled list `MailView`'s list column is showing in
   * place of a mailbox's threads, or `null` -- a view state alongside
   * `selectedMailbox` rather than a third thing layered over it, since
   * exactly one of the two is ever on screen. Cleared by `selectMailbox`, so
   * picking any ordinary mailbox -- by click, or by a `g i`/`g d`/`g s`
   * shortcut -- always leaves it behind.
   */
  viewingScheduledFor = $state<AccountId | null>(null)
  searchQuery = $state('')
  searchResults = $state<Thread[]>([])
  searchCursor = $state<string | null>(null)
  searching = $state(false)
  /** (p) TODO: `summarize_thread`'s answer for the open thread, dismissible --
   *  see `mail-api.ts`'s own TODO(p) for the contract this waits on. */
  summary = $state<{ threadId: ThreadId; text: string } | null>(null)
  summarizing = $state(false)

  // ── what has been loaded ──────────────────────────────────────────

  mailboxes = $state<Mailbox[]>([])
  /** The "All accounts" rows -- every Inbox at once, every Sent, and so on
   *  -- once two or more accounts have mailboxes; empty otherwise. Kept out
   *  of `mailboxes`, which everything else reads as real folders that each
   *  belong to one account: see `mail.ts`'s `unifiedMailboxes`. */
  unifiedMailboxes = $state<Mailbox[]>([])
  /** Threads for the open mailbox, in list order. Read `VirtualList`'s own
   *  doc for why this is `.raw` rather than deeply reactive. */
  threads = $state.raw<Thread[]>([])
  nextCursor = $state<string | null>(null)
  loadingMore = $state(false)
  loading = $state(false)
  openThread = $state<ThreadDetail | null>(null)
  /** Unread count per mailbox, refreshed after any action that could move
   *  one. Approximate at scale -- see the module doc -- fine at a mailbox's
   *  worth of mock data. */
  unreadCounts = $state<Map<MailboxId, number>>(new Map())
  /** The open inbox's threads by category -- the tabs' badges. Empty for a
   *  mailbox with no tabs. See `refreshCategoryCounts`. */
  categoryCounts = $state<CategoryCount[]>([])
  syncStatus = $state<MailSyncProgress[]>([])
  /** True from a "Sync now" press until status has been read back after it
   *  -- see `SYNC_SETTLE_MS` -- so the button answers the press at once,
   *  before any account's own phase has had time to move. */
  syncRequested = $state(false)
  /** Every account's drafts still queued to send later, soonest first --
   *  what `MailNav`'s own Scheduled row counts and `MailScheduled.svelte`
   *  lists. Unscoped by `selectedAccount`: the nav draws one row per
   *  account regardless of which account the mailbox list is filtered to. */
  scheduled = $state<ScheduledSend[]>([])

  /**
   * Which load is current, so a slow one cannot land after a newer one.
   *
   * A raw counter, the way `purpose.svelte.ts`'s is, and for the same
   * reason: `loadMore` below reads it without minting a token of its own --
   * it is guarded against racing *itself* by `loadingMore`, and simply asks
   * whether a `refresh` has started since its own page went out, which is a
   * question a peek answers correctly and a fresh mint would not: minting
   * inside `loadMore` would let it invalidate a `refresh` that was already
   * in flight when `loadMore` began, which never happened before and is not
   * this migration's to introduce. `#refreshGeneration` wraps this same
   * field in `Generation`'s shape for `refresh` alone, which does mint one
   * token per call, the way `++` already did by hand.
   */
  #generation = 0
  #refreshGeneration = {
    next: (): number => ++this.#generation,
    isCurrent: (token: number): boolean => token === this.#generation,
  }
  #loaded = false

  constructor() {
    app.onLock(() => this.reset())
    registerApply('mail', (changes) => this.#applyChanges(changes))
    // This window's own accept does not come back as a change event -- see
    // `proposals.svelte.ts` -- so the store that just sent a draft tells
    // itself to reload.
    proposals.onAccepted('mail', () => this.refresh())
  }

  reset() {
    this.#generation += 1
    this.selectedAccount = null
    this.selectedMailbox = null
    this.category = null
    this.selectedThread = null
    this.checked = new Set()
    this.#anchor = null
    this.expanded = new Set()
    this.composing = null
    this.composeInline = false
    this.parkedDraft = null
    this.wantsSnooze = null
    this.wantsLabel = null
    this.viewingScheduledFor = null
    this.summary = null
    if (this.#undoTimer) clearInterval(this.#undoTimer)
    this.#undoTimer = null
    this.sendingUndo = null
    if (this.#unreadRefreshTimer) clearTimeout(this.#unreadRefreshTimer)
    this.#unreadRefreshTimer = null
    if (this.#liveRefreshTimer) clearTimeout(this.#liveRefreshTimer)
    this.#liveRefreshTimer = null
    this.mailboxes = []
    this.unifiedMailboxes = []
    this.threads = []
    this.nextCursor = null
    this.openThread = null
    this.unreadCounts = new Map()
    this.categoryCounts = []
    this.syncStatus = []
    this.syncRequested = false
    this.scheduled = []
    this.clearSearch()
    this.#loaded = false
  }

  /** Called by `MailNav` on mount. Idempotent, like every other app's `start`. */
  async start() {
    if (!app.supportsMail || this.#loaded) return
    this.#loaded = true
    // See `todo.start` for why this is idempotent and safe to ask twice.
    if (!proposals.loaded) await proposals.refresh()
    await this.refreshMailboxes()
    await this.refreshSyncStatus()
  }

  async refreshMailboxes() {
    try {
      const accountIds = await this.#accountIds()
      const lists = await Promise.all(accountIds.map((id) => mailApi.listMailboxes(id)))
      const fetched = lists.flat()
      // `promoteGmailInbox` first, per account: a Gmail account synced
      // before the backend fix that resolves `\Inbox` to `role: 'inbox'`
      // directly has that label sitting at `role: 'other'` instead, and
      // everything below -- `needsSyntheticSnoozedMailbox`'s own search for
      // an Inbox, `mailboxHasTabs`, `listTarget`'s Inbox lookup -- reads
      // `role` alone, never a name, so this is the one place that gap has
      // to be closed.
      const real = accountIds.flatMap((id) =>
        promoteGmailInbox(fetched.filter((m) => m.accountId === id)),
      )
      // No real backend sends a Snoozed mailbox of its own yet -- only the
      // mock does, for its own two accounts -- so every other account gets
      // a stand-in here, the only place this needs to be known at all:
      // `listTarget` is where every read of it already goes through.
      const synthetic: Mailbox[] = accountIds
        .filter((id) => needsSyntheticSnoozedMailbox(real.filter((m) => m.accountId === id)))
        .map((accountId): Mailbox => ({
          id: syntheticSnoozedMailboxId(accountId),
          accountId,
          remoteName: 'Snoozed',
          role: 'other',
          pseudo: 'snoozed',
          uidvalidity: 0,
          uidnext: 0,
          highestModseq: 0,
        }))
      this.mailboxes = [...real, ...synthetic]
      this.unifiedMailboxes = buildUnifiedMailboxes(this.mailboxes, accountIds)
      // Nothing open yet -- or what was open is gone, the unified Inbox of
      // an account set that has since dropped to one. With two or more
      // accounts, Mail opens on every inbox at once rather than whichever
      // account happens to come first.
      if (!this.selectedMailbox || !this.#findMailbox(this.selectedMailbox)) {
        const inbox =
          this.unifiedMailboxes.find((m) => m.id === unifiedMailboxId('inbox')) ??
          this.mailboxes.find((m) => m.role === 'inbox')
        if (inbox) await this.selectMailbox(inbox.id)
      }
      void this.refreshUnreadCounts()
      void this.refreshScheduled()
    } catch (e) {
      await handle(e)
    }
  }

  /** A mailbox by id, the "All accounts" rows included. */
  #findMailbox(id: MailboxId | null): Mailbox | undefined {
    if (id === null) return undefined
    return this.mailboxes.find((m) => m.id === id) ?? this.unifiedMailboxes.find((m) => m.id === id)
  }

  async #accountIds(): Promise<AccountId[]> {
    if (this.selectedAccount) return [this.selectedAccount]
    const list = await mailApi.listAccounts()
    return list.filter((a) => a.services.mail).map((a) => a.id)
  }

  /** Approximate unread counts for every mailbox currently listed. See the
   *  module doc for why this is "good enough", not exact at scale.
   *
   *  Filtered by `snoozed` the same way `refresh`/`loadMore` are (Bug 3): an
   *  ordinary mailbox's badge must not count a thread hidden from its own
   *  list because it is currently snoozed, and the Snoozed pseudo-mailbox's
   *  badge is exactly the reverse -- only threads that are.
   *
   *  A unified row's badge is its members' added up, each member read once
   *  however many rows want it -- the unified Inbox and each account's own
   *  Inbox ask the very same question -- and a thread filed in two of them
   *  (a Gmail label) counted once rather than twice. */
  async refreshUnreadCounts() {
    void this.refreshCategoryCounts()
    try {
      const pages = new Map<string, Promise<Thread[]>>()
      const threadsIn = (mailboxId: MailboxId, snoozed: boolean): Promise<Thread[]> => {
        const key = `${snoozed ? 'snoozed' : 'shown'}:${mailboxId}`
        let page = pages.get(key)
        if (!page) {
          const filter: ThreadFilter = { snoozed }
          page = mailApi.listThreads(mailboxId, filter, null, 200).then((p) => p.threads)
          pages.set(key, page)
        }
        return page
      }
      const counts = new Map<MailboxId, number>()
      await Promise.all(
        [...this.mailboxes, ...this.unifiedMailboxes].map(async (mailbox) => {
          const target = listTarget(mailbox, this.mailboxes)
          if (!target) return
          const lists = await Promise.all(
            target.mailboxIds.map((id) => threadsIn(id, target.snoozed)),
          )
          const seenIds = new Set<ThreadId>()
          let unread = 0
          for (const thread of lists.flat()) {
            if (seenIds.has(thread.id)) continue
            seenIds.add(thread.id)
            unread += thread.unreadCount
          }
          counts.set(mailbox.id, unread)
        }),
      )
      this.unreadCounts = counts
    } catch (e) {
      await quietly(e)
    }
  }

  /** The tabs' badges, for the mailbox on screen -- nothing to count for
   *  one without tabs. Quiet on failure: a badge that does not update is not
   *  worth a toast. Rides along with every unread recount, so an archive or
   *  a live change moves the numbers the same moment it moves the nav's. */
  async refreshCategoryCounts() {
    const box = this.mailbox
    const target = mailboxHasTabs(box) ? listTarget(box, this.mailboxes) : null
    if (!target) {
      this.categoryCounts = []
      return
    }
    try {
      // One count per member mailbox, added up: the unified Inbox's tabs
      // are every account's tabs at once, and no thread is in two
      // accounts' inboxes to be counted twice.
      const lists = await Promise.all(target.mailboxIds.map((id) => mailApi.categoryCounts(id)))
      const counts = sumCategoryCounts(lists)
      if (this.mailbox?.id === box?.id) this.categoryCounts = counts
    } catch (e) {
      await quietly(e)
    }
  }

  /**
   * Debounced entry point for the unread recount every optimistic action
   * asks for (Bug 9). `#act`/`#remove` used to call `refreshUnreadCounts`
   * directly, and `openThreadById`'s auto-`markRead` means holding `j` down
   * a mailbox fires one of those calls -- a 200-thread `listThreads` per
   * mailbox -- for every row passed over, not just the one landed on.
   */
  #unreadRefreshTimer: ReturnType<typeof setTimeout> | null = null
  #scheduleUnreadRefresh() {
    if (this.#unreadRefreshTimer) clearTimeout(this.#unreadRefreshTimer)
    this.#unreadRefreshTimer = setTimeout(() => {
      this.#unreadRefreshTimer = null
      void this.refreshUnreadCounts()
    }, UNREAD_DEBOUNCE_MS)
  }

  async refreshSyncStatus() {
    try {
      this.syncStatus = await mailApi.syncStatus()
    } catch (e) {
      await quietly(e)
    }
  }

  async syncNow(accountId: AccountId) {
    try {
      await mailApi.syncAccount(accountId)
      await this.refreshSyncStatus()
    } catch (e) {
      await handle(e)
    }
  }

  /**
   * The list column's "Sync now": a pass for each of `accountIds`, or for
   * every mail account when none are named.
   *
   * `MailNav`'s two-second poll takes over from here for as long as any
   * account is mid-pass, and reloads the list and the counts on the way
   * back to idle. A pass that was over before status was even read back --
   * nothing new on the server, or the mock, whose `sync_account` resolves
   * only once its pretend pass is done -- never gives that poll a falling
   * edge to see, so this reloads them itself in that case.
   */
  async syncAll(accountIds?: AccountId[]) {
    // A second press while the first is still settling would only wake the
    // same tasks again -- and its `finally` would stop the first one's
    // spinner early.
    if (this.syncRequested) return
    this.syncRequested = true
    try {
      const ids = accountIds ?? (await this.#mailAccountIds())
      if (ids.length === 0) return
      await Promise.all(ids.map((id) => mailApi.syncAccount(id)))
      await new Promise((resolve) => setTimeout(resolve, SYNC_SETTLE_MS))
      await this.refreshSyncStatus()
      const still = this.syncStatus.some((s) => ids.includes(s.accountId) && syncInProgress(s))
      if (!still) {
        void this.refresh()
        this.#scheduleUnreadRefresh()
      }
    } catch (e) {
      await handle(e)
    } finally {
      this.syncRequested = false
    }
  }

  /** Every account with mail switched on, whatever `selectedAccount` says --
   *  unlike `#accountIds`, which narrows to it. */
  async #mailAccountIds(): Promise<AccountId[]> {
    const list = await mailApi.listAccounts()
    return list.filter((a) => a.services.mail).map((a) => a.id)
  }

  /** The account and mailbox picked in the nav. */
  async selectMailbox(id: MailboxId) {
    // Leaves the Scheduled list behind, the way picking a thread leaves a
    // search: exactly one of the two is ever the list column's content.
    this.viewingScheduledFor = null
    this.selectedMailbox = id
    this.selectedThread = null
    this.openThread = null
    this.clearChecked()
    // Finding 1: only the inbox has tabs to have set this from, so a
    // category chosen there must not go on filtering a mailbox with no tab
    // strip to clear it from. The unified Inbox is an inbox, and keeps it.
    if (!mailboxHasTabs(this.#findMailbox(id))) this.category = null
    void this.refreshCategoryCounts()
    await this.refresh()
  }

  /** The Scheduled row picked in the nav -- see `viewingScheduledFor`. */
  selectScheduled(accountId: AccountId) {
    this.viewingScheduledFor = accountId
    this.selectedThread = null
    this.openThread = null
  }

  setCategory(category: MailCategory | null) {
    this.category = category
    this.clearChecked()
    void this.refresh()
  }

  /** `Tab`/`Shift+Tab`: the next or previous category tab -- see
   *  `mail.ts`'s `stepCategoryTab` for the fixed order (All, then
   *  `CATEGORY_TABS`). */
  stepCategory(step: 1 | -1) {
    this.setCategory(stepCategoryTab(this.category, step))
  }

  async selectAccount(id: AccountScope) {
    this.selectedAccount = id
    this.selectedMailbox = null
    await this.refreshMailboxes()
  }

  /**
   * Reload the open mailbox -- covering however much of it is already
   * loaded, not just page one (`refreshLimit`), so a live refresh mid-scroll
   * neither shrinks the list nor loses track of a thread that is merely
   * further down it than page one reaches (finding 2).
   *
   * Because this refetches the whole loaded range rather than a narrow first
   * page, the open thread's absence from the result is trustworthy: it means
   * the backend no longer has it here -- deleted, or moved out of this
   * mailbox -- not merely "not on this page", which is what the previous,
   * page-one-only check was mistaking it for.
   */
  async refresh() {
    // `listTarget` is where the mailbox ids actually meant for the backend
    // come from -- the mailbox itself, ordinarily, except for the synthetic
    // Snoozed row `refreshMailboxes` stands in for an account whose own
    // Inbox is what must be asked instead, and a unified row, which is
    // every account's mailbox of its role at once. `null` here is exactly
    // the old `!mailboxId`.
    const target = listTarget(this.mailbox, this.mailboxes)
    if (!target) return
    await guardedRefresh(
      this.#refreshGeneration,
      async (isCurrent) => {
        // Finding 1: a category filter only means anything for the inbox's
        // own tabs -- sent elsewhere, it silently narrowed a mailbox with no
        // tab strip to have set it from.
        const category = mailboxHasTabs(this.mailbox) ? this.category : null
        // Bug 3: `snoozed` was never sent at all, so a snoozed thread --
        // still a member of whatever mailbox it was snoozed from -- came
        // straight back on the very next refresh. `false` everywhere except
        // the Snoozed view, which wants nothing else.
        const filter: ThreadFilter = { snoozed: target.snoozed }
        if (category) filter.category = category
        const page = await this.#listPage(
          target.mailboxIds,
          filter,
          null,
          refreshLimit(this.threads.length, PAGE),
        )
        if (!isCurrent()) return
        // Deduped even within one page: a thread filed in two of a unified
        // row's mailboxes must never be two rows with one key.
        this.threads = mergeSearchPage([], page.threads)
        this.nextCursor = page.nextCursor ?? null
        if (this.checked.size > 0) {
          const listed = new Set(this.threads.map((t) => t.id))
          const kept = [...this.checked].filter((id) => listed.has(id))
          if (kept.length !== this.checked.size) this.checked = new Set(kept)
        }
        if (this.selectedThread && !this.threads.some((t) => t.id === this.selectedThread)) {
          this.selectedThread = null
          this.openThread = null
        }
      },
      { setLoading: (v) => (this.loading = v), onError: (e) => handle(e) },
    )
  }

  /** `VirtualList`'s `onEndReached`: the next keyset page. */
  async loadMore() {
    const target = listTarget(this.mailbox, this.mailboxes)
    if (!target || !this.nextCursor || this.loadingMore) return
    this.loadingMore = true
    const cursor = this.nextCursor
    try {
      const generation = this.#generation
      // Bug 3, the same as `refresh` above -- must agree with it, or
      // scrolling to a second page would bring snoozed threads back.
      const filter: ThreadFilter = { snoozed: target.snoozed }
      if (this.category) filter.category = this.category
      const page = await this.#listPage(target.mailboxIds, filter, cursor, PAGE)
      // A mailbox or category change while this page was on its way bumps
      // the generation; its rows belong to a list no longer showing.
      if (generation !== this.#generation || cursor !== this.nextCursor) return
      // `mergeSearchPage` rather than a bare append: across several
      // mailboxes, a thread filed in two of them comes back once for each,
      // on whichever pages its two rows land.
      this.threads = mergeSearchPage(this.threads, page.threads)
      this.nextCursor = page.nextCursor ?? null
    } catch (e) {
      await quietly(e)
    } finally {
      this.loadingMore = false
    }
  }

  /** One page of `mailboxIds` -- `list_threads` for the one mailbox nearly
   *  every view is, `list_threads_across` for a unified row's several. */
  #listPage(
    mailboxIds: MailboxId[],
    filter: ThreadFilter,
    cursor: string | null,
    limit: number,
  ): Promise<ThreadPage> {
    const [only] = mailboxIds
    if (mailboxIds.length === 1 && only !== undefined) {
      return mailApi.listThreads(only, filter, cursor, limit)
    }
    return mailApi.listThreadsAcross(mailboxIds, filter, cursor, limit)
  }

  // ── the open thread ────────────────────────────────────────────────

  async openThreadById(id: ThreadId) {
    // Switching to a different thread leaves an inline reply behind -- the
    // draft already autosaves itself, so closing the sheet is all this
    // needs to do; see `composeInline`'s own doc. Not gated when `id` is
    // the thread already open: reopening the same one must not drop a
    // reply somebody is still writing.
    if (this.composeInline && this.selectedThread !== id) this.closeCompose()
    // A message filling the pane makes way for the thread -- set aside, not
    // lost: see `parkedDraft`.
    if (this.composing && !this.composeInline) this.#park()
    this.selectedThread = id
    this.summary = null
    // Opening one thread is leaving the many behind -- see `checked`.
    this.clearChecked()
    this.#anchor = id
    try {
      const detail = await mailApi.getThread(id)
      // Moved on while this was loading: a later open owns the pane now, and
      // this thread must not be shown, marked read or prefetched around.
      if (this.selectedThread !== id) return
      this.openThread = detail
      // The newest message starts expanded; every earlier one collapsed to
      // its one-line summary, as the plan asks.
      const newest = detail.messages.at(-1)
      this.expanded = newest ? new Set([newest.id]) : new Set()
      if (detail.thread.unreadCount > 0) void this.markRead(id)
      void this.#prefetchNeighbours(id)
      void this.#openAutoDraftIfAny(detail)
    } catch (e) {
      await handle(e)
    }
  }

  /**
   * Open Mail at a thread from outside it -- the assistant transcript's own
   * link ("Archived: Plans for Saturday →"), or the same list in Settings
   * → Sharing and the assistant's settings.
   *
   * The same two-step every other app's own cross-app link already is
   * (`app.goTo('notes').then(() => noteStore.openNote(id))` in
   * `OverviewWidget.svelte`, say): switch the app bar to Mail through its
   * existing routing, then reuse `openThreadById` rather than a second way
   * of opening a thread.
   */
  async openFromElsewhere(id: ThreadId) {
    if (!(await app.goTo('mail'))) return
    await this.openThreadById(id)
  }

  /**
   * (p) TODO: find the auto-draft phase 7 writes ahead of a reply, and open
   * it in the compose sheet -- "the reply box opens pre-filled with it and
   * labelled 'Drafted by the assistant'," per `docs/plans/mail.md`'s
   * Superhuman layer. An auto-draft is an ordinary `Draft` whose
   * `origin.type === 'assistant'` and `inReplyTo` names a message in this
   * thread; there is no dedicated lookup for it, so this is the one place
   * `list_drafts` is read for something other than the Drafts mailbox.
   */
  async #openAutoDraftIfAny(detail: ThreadDetail) {
    if (this.composing) return
    try {
      const drafts = await mailApi.listDrafts(detail.thread.accountId)
      const ids = new Set(detail.messages.map((m) => m.id))
      const auto = drafts.find(
        (d) =>
          d.origin.type === 'assistant' &&
          d.state.type === 'editing' &&
          d.inReplyTo &&
          ids.has(d.inReplyTo),
      )
      // A reply into the thread that is still open -- the same inline
      // treatment `reply()`/`forward()` give one started by hand -- unless
      // something else began being written while the drafts were loading,
      // which is the person's own and stays where it is.
      if (auto && this.selectedThread === detail.thread.id && !this.composing) {
        this.openInlineDraft(auto)
      }
    } catch (e) {
      await quietly(e)
    }
  }

  toggleExpanded(messageId: string) {
    const next = new Set(this.expanded)
    if (next.has(messageId)) next.delete(messageId)
    else next.add(messageId)
    this.expanded = next
  }

  closeThread() {
    if (this.composeInline) this.closeCompose()
    this.selectedThread = null
    this.openThread = null
    this.summary = null
  }

  /** Whichever list is on screen -- see `visibleThreadList`. */
  get #shownThreads(): readonly Thread[] {
    return visibleThreadList(this.searchQuery, this.threads, this.searchResults)
  }

  /** The next and previous thread in list order, for `j`/`k` and prefetch.
   *  Finding 3: this used to read `this.threads` even while a search's
   *  results were what was actually on screen, so `j`/`k` moved through the
   *  mailbox behind the search rather than through the rows visible. */
  #neighbour(step: 1 | -1): Thread | null {
    return neighbourThread(this.#shownThreads, this.selectedThread, step)
  }

  async moveSelection(step: 1 | -1) {
    const next = this.#neighbour(step)
    if (!next) return
    // With threads picked, `j`/`k` only move the cursor, so `x` can pick
    // the next one -- opening it would drop the picked ones.
    if (this.selectedThread && this.checked.size === 0) await this.openThreadById(next.id)
    else this.selectedThread = next.id
  }

  // ── picking several threads ──────────────────────────────────────

  /**
   * A click on a thread row: Shift takes the run from the last row clicked
   * to this one (added to what is already picked when Ctrl/Cmd is held
   * too); Ctrl/Cmd adds this one or takes it away; a plain click opens it,
   * leaving any picked ones behind. A first Ctrl+click while a thread is
   * open picks that one too, the way a file manager's selection grows from
   * the item already selected rather than starting over.
   */
  clickThread(id: ThreadId, mods: { shift?: boolean; toggle?: boolean } = {}) {
    if (mods.shift) {
      const from = this.#anchor ?? this.selectedThread
      const range = threadRange(this.#shownThreads, from, id)
      this.checked = new Set(mods.toggle ? [...this.checked, ...range] : range)
      return
    }
    if (mods.toggle) {
      const next = new Set(this.checked)
      if (next.size === 0 && this.selectedThread && this.selectedThread !== id) {
        next.add(this.selectedThread)
      }
      if (next.has(id)) next.delete(id)
      else next.add(id)
      this.checked = next
      this.#anchor = id
      return
    }
    void this.openThreadById(id)
  }

  /** `x`: pick the thread under the cursor, or put it back. */
  toggleChecked(id: ThreadId) {
    const next = new Set(this.checked)
    if (next.has(id)) next.delete(id)
    else next.add(id)
    this.checked = next
    this.#anchor = id
  }

  /** Mod+A: every thread the list has loaded. */
  checkAll() {
    this.checked = new Set(this.#shownThreads.map((t) => t.id))
  }

  clearChecked() {
    if (this.checked.size > 0) this.checked = new Set()
  }

  /** What an action key acts on: every picked thread, else the selected
   *  one, else nothing. In list order, so a batch reads top to bottom. */
  get targets(): ThreadId[] {
    if (this.checked.size > 0) {
      return this.#shownThreads.filter((t) => this.checked.has(t.id)).map((t) => t.id)
    }
    return this.selectedThread ? [this.selectedThread] : []
  }

  /**
   * Prefetch the next and previous thread's detail, per "The speed budget".
   * Best-effort and silent: a prefetch that fails is simply not there yet
   * when the reader arrives, which is exactly what would have happened
   * without it.
   */
  #prefetching = new Set<ThreadId>()
  async #prefetchNeighbours(around: ThreadId) {
    const list = this.#shownThreads
    const at = list.findIndex((t) => t.id === around)
    if (at < 0) return
    for (const t of [list[at - 1], list[at + 1]]) {
      if (!t || this.#prefetching.has(t.id)) continue
      this.#prefetching.add(t.id)
      mailApi
        .getThread(t.id)
        .catch(() => {})
        .finally(() => this.#prefetching.delete(t.id))
    }
  }

  /**
   * If `id` is the open thread, move `selectedThread` on to its neighbour
   * before whatever is about to remove `id`'s row does -- `#neighbour`'s
   * lookup depends on the row still being in the list it reads, so this has
   * to run first. Superhuman's own convention for archiving into the thread
   * that was next, which a thread removed by a live change (finding 5)
   * deserves exactly as much as one removed by the reader's own `e`.
   *
   * `leaving` may be several threads at once -- Quick cleanup's "everything
   * from this sender" -- in which case the neighbour is the nearest row
   * that is not itself about to go, rather than the next row over, which
   * may well be the same sender's next newsletter.
   */
  #advanceIfOpen(ids: ReadonlySet<ThreadId>): void {
    const open = this.selectedThread
    if (!open || !ids.has(open)) return
    // The thread this reply was inline under is about to disappear from
    // under it -- archived, trashed, snoozed, moved, or deleted by another
    // window. Same as `closeThread`'s own guard; see `composeInline`.
    if (this.composeInline) this.closeCompose()
    // The nearest neighbour that is not leaving too -- several can go at
    // once, from a multi-thread selection.
    const list = this.#shownThreads
    const at = list.findIndex((t) => t.id === open)
    const after = list.slice(at + 1).find((t) => !ids.has(t.id))
    const before = list
      .slice(0, Math.max(at, 0))
      .reverse()
      .find((t) => !ids.has(t.id))
    this.selectedThread = (after ?? before)?.id ?? null
    this.openThread = null
    this.summary = null
  }

  // ── actions ──────────────────────────────────────────────────────
  //
  // Every one of these follows the same shape: patch the row (or remove it),
  // call the command, and on failure put back exactly what was there and
  // say so. `errors.ts`'s `handle` is what says so.

  /**
   * Patch one row wherever it is showing -- the mailbox's own list and a
   * search's results both, per finding 4, since either can have the row on
   * screen at once -- and the open thread's own copy, reverting all three on
   * failure. Before this, a failed star left the star lit in the reading
   * pane and a failed action of any kind left a search result stale.
   */
  async #act(
    ids: readonly ThreadId[],
    patch: Partial<Thread>,
    call: (ids: ThreadId[]) => Promise<unknown>,
  ): Promise<void> {
    if (ids.length === 0) return
    const befores: Thread[] = []
    const searchBefores: Thread[] = []
    for (const id of ids) {
      const { rows, before } = applyRowPatch(this.threads, id, patch)
      this.threads = rows
      if (before) befores.push(before)
      const { rows: searchRows, before: searchBefore } = applyRowPatch(
        this.searchResults,
        id,
        patch,
      )
      this.searchResults = searchRows
      if (searchBefore) searchBefores.push(searchBefore)
    }
    const openId = this.openThread?.thread.id
    const openBefore = openId && ids.includes(openId) ? this.openThread!.thread : null
    if (openBefore) {
      this.openThread = { ...this.openThread!, thread: { ...openBefore, ...patch } }
    }
    try {
      await call([...ids])
      this.#scheduleUnreadRefresh()
    } catch (e) {
      for (const before of befores) this.threads = revertRow(this.threads, before.id, before)
      for (const before of searchBefores) {
        this.searchResults = revertRow(this.searchResults, before.id, before)
      }
      if (openBefore && this.openThread?.thread.id === openBefore.id) {
        this.openThread = { ...this.openThread, thread: openBefore }
      }
      await handle(e)
    }
  }

  /** Removes the row from the list on screen -- archive, trash, move,
   *  snooze -- and from a search's results too (finding 4), restoring both
   *  in place on failure. */
  async #remove(
    ids: readonly ThreadId[],
    call: (ids: ThreadId[]) => Promise<unknown>,
  ): Promise<void> {
    if (ids.length === 0) return
    const leaving = new Set(ids)
    // Finding 5: move on to the neighbour rather than simply closing the
    // pane -- `#advanceIfOpen` must run before the row disappears below, so
    // it can still find one.
    this.#advanceIfOpen(leaving)
    if ([...this.checked].some((id) => leaving.has(id))) {
      this.checked = new Set([...this.checked].filter((id) => !leaving.has(id)))
    }
    // Put back in the reverse of the order they came out, so each lands at
    // the index it was taken from.
    const removed: { row: Thread; index: number }[] = []
    const searchRemoved: { row: Thread; index: number }[] = []
    for (const id of ids) {
      const out = removeRow(this.threads, id)
      this.threads = out.rows
      if (out.removed) removed.push(out.removed)
      const searchOut = removeRow(this.searchResults, id)
      this.searchResults = searchOut.rows
      if (searchOut.removed) searchRemoved.push(searchOut.removed)
    }
    try {
      await call([...ids])
      this.#scheduleUnreadRefresh()
    } catch (e) {
      for (const r of removed.reverse()) this.threads = restoreRow(this.threads, r)
      for (const r of searchRemoved.reverse()) {
        this.searchResults = restoreRow(this.searchResults, r)
      }
      await handle(e)
    }
  }

  markRead(id: ThreadId) {
    return this.markReadMany([id])
  }
  markReadMany(ids: readonly ThreadId[]) {
    return this.#act(ids, { unreadCount: 0 }, mailApi.markReadMany)
  }

  /**
   * Mark everything unread in one mailbox read -- the folder list's
   * right-click.
   *
   * Paged through the unread filter and sent a page at a time, as the batch
   * the real command already takes, rather than one call per thread: a
   * folder somebody wants to clear is usually one with a great deal in it.
   * Capped, so a mailbox of fifty thousand unread newsletters is a handful
   * of round trips and an honest count rather than a minute of silence; the
   * count says how many were done, and doing it again does the next lot.
   * The rows already drawn are updated as each page lands, and the counts
   * in the sidebar once at the end.
   *
   * Through `listTarget`, like every other read of a mailbox: a unified row
   * is each of its member mailboxes in turn, under the one shared cap, and
   * the synthetic Snoozed row is its account's Inbox -- an id no backend
   * would have recognised when this asked for it by its own.
   */
  async markMailboxRead(id: MailboxId): Promise<number> {
    const target = listTarget(this.#findMailbox(id), this.mailboxes)
    if (!target) return 0
    const filter: ThreadFilter = { unread: true, snoozed: target.snoozed }
    let done = 0
    try {
      for (const mailboxId of target.mailboxIds) {
        let cursor: string | null = null
        do {
          const page: ThreadPage = await mailApi.listThreads(mailboxId, filter, cursor, MARK_PAGE)
          const ids = page.threads.filter((t) => t.unreadCount > 0).map((t) => t.id)
          if (ids.length > 0) await mailApi.markReadAll(ids)
          for (const thread of ids) {
            this.threads = applyRowPatch(this.threads, thread, { unreadCount: 0 }).rows
            this.searchResults = applyRowPatch(this.searchResults, thread, {
              unreadCount: 0,
            }).rows
          }
          done += ids.length
          cursor = page.nextCursor ?? null
        } while (cursor && done < MARK_CAP)
        if (done >= MARK_CAP) break
      }
    } catch (e) {
      await handle(e)
    }
    this.#scheduleUnreadRefresh()
    return done
  }
  markUnread(id: ThreadId) {
    return this.markUnreadMany([id])
  }
  markUnreadMany(ids: readonly ThreadId[]) {
    return this.#act(ids, { unreadCount: 1 }, mailApi.markUnreadMany)
  }
  /**
   * Star, best-effort. `Thread.starred` -- the real per-thread aggregate,
   * see its own doc in `types.ts` -- is the current state; the open thread's
   * own messages are consulted only as a fallback for a row not currently in
   * `this.threads` (a thread reached only through `openThread`, which does
   * not happen in the interface today, but costs nothing to keep honest).
   */
  toggleStar(id: ThreadId) {
    return this.toggleStarMany([id])
  }
  /** Several at once: star them all unless every one already is, then
   *  unstar them all -- what a mail client's one Star button does to a
   *  mixed selection. */
  toggleStarMany(ids: readonly ThreadId[]) {
    const starred = ids.every((id) => this.#isStarred(id))
    return this.#act(ids, { starred: !starred }, starred ? mailApi.unstarMany : mailApi.starMany)
  }
  #isStarred(id: ThreadId): boolean {
    const row = this.threads.find((t) => t.id === id) ?? this.searchResults.find((t) => t.id === id)
    return (
      row?.starred ??
      (this.openThread?.thread.id === id && this.openThread.messages.some((m) => m.flags.flagged))
    )
  }
  archive(id: ThreadId) {
    return this.archiveMany([id])
  }
  archiveMany(ids: readonly ThreadId[]) {
    return this.#remove(ids, mailApi.archiveMany)
  }
  trash(id: ThreadId) {
    return this.trashMany([id])
  }
  trashMany(ids: readonly ThreadId[]) {
    return this.#remove(ids, mailApi.trashMany)
  }

  /** Quick cleanup's Archive: every thread in `ids` at once. Answers whether
   *  all of it went -- see `#removeMany`. */
  archiveThreads(ids: ThreadId[]): Promise<boolean> {
    return this.#removeMany(ids, mailApi.archiveMany)
  }

  /** Quick cleanup's Delete -- to Trash, never gone for good. */
  trashThreads(ids: ThreadId[]): Promise<boolean> {
    return this.#removeMany(ids, mailApi.trashMany)
  }

  /**
   * `#remove`, for however many threads Quick cleanup names -- a sender
   * with years of newsletters is thousands, where a selection in the list
   * is a handful. Every row leaves the list on screen and a search's
   * results at once, then the batch goes out `BATCH_CHUNK` at a time. A
   * chunk that fails puts back only its own rows and the ones after it --
   * every chunk before it already happened on the server, and drawing
   * those rows again would show threads that are no longer there. Answers
   * `true` only when every chunk went, which is what lets the dialog drop
   * the senders it acted on.
   */
  async #removeMany(
    ids: ThreadId[],
    call: (chunk: ThreadId[]) => Promise<unknown>,
  ): Promise<boolean> {
    const unique = [...new Set(ids)]
    if (unique.length === 0) return true
    const gone = new Set(unique)
    this.#advanceIfOpen(gone)
    if ([...this.checked].some((id) => gone.has(id))) {
      this.checked = new Set([...this.checked].filter((id) => !gone.has(id)))
    }
    const removedFrom = (rows: readonly Thread[]) =>
      rows.flatMap((row, index) => (gone.has(row.id) ? [{ row, index }] : []))
    const removed = removedFrom(this.threads)
    const searchRemoved = removedFrom(this.searchResults)
    this.threads = this.threads.filter((t) => !gone.has(t.id))
    this.searchResults = this.searchResults.filter((t) => !gone.has(t.id))
    let sent = 0
    try {
      while (sent < unique.length) {
        await call(unique.slice(sent, sent + BATCH_CHUNK))
        sent += BATCH_CHUNK
      }
      // A refresh already on its way when these rows were dropped asked the
      // backend before the removal reached it, and lands with them still in
      // it -- a live change, or the sync event, can start one at any
      // moment. So they are dropped again in case it has landed, and the
      // list is asked for once more in case it has not: the newer ask
      // supersedes the stale one before it can land at all.
      this.threads = this.threads.filter((t) => !gone.has(t.id))
      this.searchResults = this.searchResults.filter((t) => !gone.has(t.id))
      if (this.loading) void this.refresh()
      return true
    } catch (e) {
      const failed = new Set(unique.slice(sent))
      // In their original order, so each lands back where it was -- the
      // same thing `restoreRow` does for one -- and never twice, if a
      // refresh in the meantime already brought it back.
      const putBack = (rows: Thread[], was: { row: Thread; index: number }[]) =>
        was
          .filter((r) => failed.has(r.row.id))
          .reduce((acc, r) => (acc.some((t) => t.id === r.row.id) ? acc : restoreRow(acc, r)), rows)
      this.threads = putBack(this.threads, removed)
      this.searchResults = putBack(this.searchResults, searchRemoved)
      await handle(e)
      return false
    } finally {
      this.#scheduleUnreadRefresh()
    }
  }
  moveTo(id: ThreadId, mailbox: MailboxId) {
    return this.moveManyTo([id], mailbox)
  }
  moveManyTo(ids: readonly ThreadId[], mailbox: MailboxId) {
    return this.#remove(ids, (t) => mailApi.moveManyToMailbox(t, mailbox))
  }
  label(id: ThreadId, labelName: string) {
    return this.labelMany([id], labelName)
  }
  labelMany(ids: readonly ThreadId[], labelName: string) {
    return this.#act(ids, {}, (t) => mailApi.labelMany(t, labelName))
  }
  snooze(id: ThreadId, until: Date) {
    return this.snoozeMany([id], until)
  }
  snoozeMany(ids: readonly ThreadId[], until: Date) {
    return this.#remove(ids, (t) => mailApi.snoozeMany(t, until.toISOString()))
  }
  /**
   * Unsnooze. In the Snoozed view itself this must also drop the row --
   * `snoozedUntil: null` no longer matches the `filter.snoozed: true` that
   * view is asking for, and leaving it patched in place rather than removed
   * would be the one row in the list whose own content disagrees with why
   * it is still there. Everywhere else -- a menu reachable from any mailbox,
   * since an ordinary list never shows a currently-snoozed row at all -- the
   * patch alone is right, the same as it always was.
   */
  unsnooze(id: ThreadId) {
    return this.unsnoozeMany([id])
  }
  unsnoozeMany(ids: readonly ThreadId[]) {
    if (isSnoozedMailbox(this.mailbox)) return this.#remove(ids, mailApi.unsnoozeMany)
    return this.#act(ids, { snoozedUntil: null }, mailApi.unsnoozeMany)
  }

  /** (p) TODO: teaches the split-inbox rules -- see `mail-api.ts`'s own
   *  TODO(p) for `set_thread_category`'s contract. */
  setCategoryFor(id: ThreadId, category: MailCategory) {
    return this.setCategoryForMany([id], category)
  }
  setCategoryForMany(ids: readonly ThreadId[], category: MailCategory) {
    return this.#act(ids, { category }, (t) => mailApi.setThreadCategory(t, category))
  }

  /**
   * `!`: flag or unflag this one thread as priority -- never the standing
   * per-sender rule `setCategoryFor(id, 'priority')` is. Patches the same
   * `category` field `setCategoryFor` does, and so leaves a row on screen
   * exactly the way that one already does -- see its own TODO(p) above.
   */
  setPriority(id: ThreadId, on: boolean) {
    return this.setPriorityMany([id], on)
  }
  setPriorityMany(ids: readonly ThreadId[], on: boolean) {
    return this.#act(ids, { category: on ? 'priority' : 'important' }, (t) =>
      mailApi.setThreadPriority(t, on),
    )
  }

  /** The snooze picker's fixed choices, for the component to draw. Named
   *  apart from the imported `snoozeChoices` so a reader is never asking
   *  whether this calls itself. */
  snoozeOptions() {
    return snoozeChoices()
  }

  // ── (i) invitations ──────────────────────────────────────────────

  /** TODO(i): see `mail-api.ts`'s `respondToInvite`. Patches the open
   *  thread's copy of the message optimistically -- the invite card
   *  highlights the pressed button before the round trip lands, the same
   *  optimism every batch action above already has. */
  async respondToInvite(messageId: MailMessageId, response: 'accepted' | 'tentative' | 'declined') {
    const before = this.openThread
    if (before) {
      this.openThread = {
        ...before,
        messages: before.messages.map((m) =>
          m.id === messageId && m.invite
            ? { ...m, invite: applyInviteResponse(m.invite, response) }
            : m,
        ),
      }
    }
    try {
      await mailApi.respondToInvite(messageId, response)
    } catch (e) {
      if (before) this.openThread = before
      await handle(e)
    }
  }

  // ── (p) summarising a thread ─────────────────────────────────────

  /** TODO(p): `summarize_thread`'s result, shown in a dismissible panel.
   *  Only offered when the account's `mailAi.summaries` is on -- the caller
   *  (`MailView.svelte`) checks that before this is ever reachable, and this
   *  checks it again so a stale keyboard shortcut cannot bypass it. */
  async summarizeOpenThread() {
    const detail = this.openThread
    if (!detail || this.summarizing) return
    const account = accounts.account(detail.thread.accountId)
    if (!account?.mailAi?.summaries) return
    this.summarizing = true
    try {
      const { summary } = await mailApi.summarizeThread(detail.thread.id)
      this.summary = { threadId: detail.thread.id, text: summary }
    } catch (e) {
      await handle(e)
    } finally {
      this.summarizing = false
    }
  }

  dismissSummary() {
    this.summary = null
  }

  // ── compose ──────────────────────────────────────────────────────

  /** A brand-new message fills the reading pane -- there is no thread
   *  under it for an inline reply to sit beneath. */
  async compose(account?: AccountId) {
    const accountId = account ?? this.selectedAccount ?? this.mailboxes[0]?.accountId
    if (!accountId) return
    const draft = await mailApi.newDraft({ account: accountId })
    this.#show(draft, false)
  }

  /** Opens `draft` inline, under the open thread's own messages -- what
   *  `reply`/`forward` below use, and what a suggested reply's own "send"
   *  reaches for directly (`mailwrite.ts`'s agent). */
  openInlineDraft(draft: Draft): void {
    this.#show(draft, true)
  }

  /** Opens `draft` filling the reading pane -- what "Draft saved · Open"
   *  calls to bring a draft put aside back. */
  openDraft(draft: Draft): void {
    this.#show(draft, false)
  }

  /** The one way a draft goes on screen: whatever was being written before
   *  it is set aside first (see `parkedDraft`), never dropped silently. */
  #show(draft: Draft, inline: boolean) {
    if (this.composing && this.composing.id !== draft.id) this.#park()
    this.composing = draft
    this.composeInline = inline
  }

  #park() {
    if (!this.composing) return
    this.parkedDraft = this.composing.id
    this.closeCompose()
  }

  async reply(messageId: string, all: boolean) {
    const accountId = this.openThread?.thread.accountId
    if (!accountId) return
    const draft = await mailApi.newDraft({
      account: accountId,
      inReplyTo: messageId,
      replyAll: all,
    })
    this.openInlineDraft(draft)
  }

  async forward(messageId: string) {
    const accountId = this.openThread?.thread.accountId
    if (!accountId) return
    const draft = await mailApi.newDraft({ account: accountId, forwardOf: messageId })
    this.openInlineDraft(draft)
  }

  closeCompose() {
    this.composing = null
    this.composeInline = false
  }

  /**
   * Send a draft and open the undo window.
   *
   * Held here rather than in `MailCompose.svelte`: sending closes the
   * compose sheet, which unmounts it, and a toast whose state lived on that
   * component would go with it before it had drawn a single frame. The
   * store outlives the sheet, so the toast does too.
   *
   * `at` is the real instant `sendDraft` was asked to fire at, computed from
   * whichever of `delaySeconds`/`sendAt` this call itself passed -- not
   * re-read from the server's answer, because there is nothing to re-read:
   * `sendDraft` returns the `Draft`, whose `state` only names the queued
   * op's id (`DraftState.Queued.op`), never the op's own `notBefore`, and
   * nothing in the surface fetches an `Op` by id today. This is still
   * exactly right, not a guess: for "send later" the backend uses `sendAt`
   * verbatim (`send_draft` in `mail.rs`), and for an ordinary send this
   * store only ever passes `UNDO_WINDOW_S` (`MailCompose.svelte`), already
   * inside the backend's own 5-30s clamp (`undo_send_delay`) and so never
   * altered by it.
   */
  sendingUndo = $state<{
    draft: Draft
    at: number
    secondsLeft: number
    scheduled: boolean
  } | null>(null)
  #undoTimer: ReturnType<typeof setInterval> | null = null

  async send(draft: Draft, delaySeconds?: number, sendAt?: string) {
    try {
      await mailApi.sendDraft(draft.id, delaySeconds, sendAt)
    } catch (e) {
      // The compose sheet is already gone by the time this runs (see
      // `MailCompose.svelte`'s own `send`) -- without this, the backend's
      // "a message needs at least one recipient" (or anything else it
      // refuses for) vanished with no toast, no undo bar, and a draft the
      // person had no idea was still sitting there, editable, in Drafts.
      await handle(e)
      return
    }
    this.closeCompose()
    if (this.#undoTimer) clearInterval(this.#undoTimer)
    const at = sendAt ? new Date(sendAt).getTime() : Date.now() + (delaySeconds ?? 0) * 1000
    const scheduled = Boolean(sendAt)
    // A "send later" is now one more row `scheduled_sends` answers with --
    // the Scheduled nav row and `MailScheduled.svelte` want to know the
    // moment it is queued, not only once something later reloads mailboxes.
    if (scheduled) void this.refreshScheduled()
    // Recomputed from `at` on every tick rather than decremented, so the
    // toast never drifts from what was actually asked for even if a tick is
    // late -- and so a "send later" toast, `at` hours out, counts down to
    // *that*, not to whatever `delaySeconds` would have meant for an
    // ordinary send (Bug 11: it used to show `delaySeconds` verbatim,
    // regardless of how it was actually going to be spent).
    const tick = () => {
      const secondsLeft = Math.max(0, Math.round((at - Date.now()) / 1000))
      this.sendingUndo = { draft, at, secondsLeft, scheduled }
      if (secondsLeft <= 0) {
        if (this.#undoTimer) clearInterval(this.#undoTimer)
        this.#undoTimer = null
        this.sendingUndo = null
      }
    }
    tick()
    this.#undoTimer = setInterval(tick, 1000)
  }

  /** Reopens the draft in the compose sheet, exactly where sending left it. */
  async undoSend() {
    const pending = this.sendingUndo
    if (!pending) return
    if (this.#undoTimer) clearInterval(this.#undoTimer)
    this.#undoTimer = null
    this.sendingUndo = null
    try {
      // `vault.undo_send` answers with the reverted `Draft`, not merely
      // acknowledging -- reopening exactly that, rather than the `Draft` as
      // it was before `send()` ran, is what makes a second edit made after
      // `sendDraft`'s own local write (there is none today, but nothing rules
      // one out) show up when Undo reopens the sheet.
      const reverted = await mailApi.undoSend(pending.draft.id)
      // Inline again if it is a reply into the thread still open -- the
      // same thing that was true the moment before it was sent -- filling
      // the pane otherwise: a reply into a thread no longer open, or a
      // brand-new message, has no thread on screen to sit under.
      this.#show(
        reverted,
        reverted.inReplyTo != null &&
          (this.openThread?.messages.some((m) => m.id === reverted.inReplyTo) ?? false),
      )
      // Only a scheduled send was ever in `scheduled` to begin with -- see
      // `send`'s own note -- but asking unconditionally costs one cheap,
      // already-debounced-by-nothing-else read rather than a second flag
      // to keep in step with `sendingUndo.scheduled`.
      void this.refreshScheduled()
    } catch (e) {
      // The undo window can close a beat before the click lands -- the
      // server then refuses, and the toast above is already down; all that
      // is left is to say why the click did nothing.
      await handle(e)
    }
  }

  // ── scheduled sends ──────────────────────────────────────────────

  /** Every account's queued-to-send-later drafts, soonest first -- what
   *  `MailNav`'s Scheduled row counts and `MailScheduled.svelte` lists.
   *  Refreshed on `start`, after a "send later" (`send`'s own call), after
   *  undo/cancel/reschedule, and whenever mailboxes refresh. */
  async refreshScheduled() {
    try {
      this.scheduled = await mailApi.scheduledSends()
    } catch (e) {
      await quietly(e)
    }
  }

  /** Edit: reopens a queued send in the compose sheet, by id -- for a row
   *  read from `scheduled` rather than from this window's own `sendingUndo`,
   *  which only ever remembers the one send this window itself just made. */
  async editScheduled(draftId: DraftId) {
    try {
      // Always filling the pane: reached from the Scheduled list, which has
      // no open thread for an inline reply to sit under -- unlike
      // `undoSend`'s own reopening, this has nothing to decide.
      this.#show(await mailApi.undoSend(draftId), false)
      void this.refreshScheduled()
    } catch (e) {
      await handle(e)
    }
  }

  /** Cancel: the same undo, with the draft left in Drafts rather than
   *  reopened. */
  async cancelScheduled(draftId: DraftId) {
    try {
      await mailApi.undoSend(draftId)
      void this.refreshScheduled()
    } catch (e) {
      await handle(e)
    }
  }

  /** Reschedule: undo, then queue again at the new instant. */
  async rescheduleScheduled(draftId: DraftId, at: Date) {
    try {
      const draft = await mailApi.undoSend(draftId)
      await mailApi.sendDraft(draft.id, null, at.toISOString())
      void this.refreshScheduled()
    } catch (e) {
      await handle(e)
    }
  }

  /** Send now: undo the queued send, then send it through the ordinary
   *  short undo window -- `send`'s own toast, same as composing fresh. */
  async sendScheduledNow(draftId: DraftId) {
    try {
      const draft = await mailApi.undoSend(draftId)
      void this.refreshScheduled()
      await this.send(draft, 8)
    } catch (e) {
      await handle(e)
    }
  }

  // ── search ───────────────────────────────────────────────────────

  setSearchQuery(q: string) {
    this.searchQuery = q
    this.searchCursor = null
    this.clearChecked()
    if (!q.trim()) {
      this.searchResults = []
      this.searching = false
      return
    }
    this.searching = true
    void this.#runSearch(q)
  }

  async #runSearch(q: string) {
    try {
      const result = await mailApi.searchMail(q)
      if (this.searchQuery === q) {
        this.searchResults = result.threads
        this.searchCursor = result.next ?? null
      }
    } catch (e) {
      await quietly(e)
    } finally {
      if (this.searchQuery === q) this.searching = false
    }
  }

  /** The next keyset page of the current search, appended without
   *  duplicating a thread a page boundary happens to repeat -- see
   *  `mail.ts`'s `mergeSearchPage`. */
  async loadMoreSearchResults() {
    const q = this.searchQuery
    if (!q.trim() || !this.searchCursor || this.searching) return
    this.searching = true
    try {
      const result = await mailApi.searchMail(q, null, this.searchCursor)
      if (this.searchQuery === q) {
        this.searchResults = mergeSearchPage(this.searchResults, result.threads)
        this.searchCursor = result.next ?? null
      }
    } catch (e) {
      await quietly(e)
    } finally {
      if (this.searchQuery === q) this.searching = false
    }
  }

  clearSearch() {
    this.searchQuery = ''
    this.searchResults = []
    this.searchCursor = null
    this.searching = false
  }

  // ── live-apply ───────────────────────────────────────────────────

  #applyChanges(changes: ChangeWithIds[]): boolean {
    // A background sync pass that changed something says so with one thread
    // change naming no thread at all -- it may have touched hundreds, in any
    // mailbox. Declining it would send `live` to `RELOAD.mail`, which reloads
    // the list alone: new mail would appear while the sidebar's unread
    // counts and the "last synced" line went on describing the pass before.
    // So the list, the counts and the status are all asked again, the first
    // two debounced so a burst of passes costs one of each.
    if (changes.some((c) => c.kind === 'thread' && !c.id && !c.ids?.length)) {
      this.#scheduleLiveRefresh()
      this.#scheduleUnreadRefresh()
      void this.refreshSyncStatus()
      return true
    }
    return applySingleChange(changes, {
      // Nothing in the visible thread list or the sync-status line reads a
      // draft directly today -- the compose sheet owns its own working copy
      // and autosaves it, so another window's edit to the *same* draft is
      // not something this window should clobber mid-keystroke. Handled as
      // "nothing to do" rather than falling through to `RELOAD.mail`, which
      // would otherwise reload the thread list for every autosave tick.
      skip: (change) => change.kind === 'draft',
      onDeleted: (id) => {
        // Finding 5: a thread deleted from another window or by the
        // assistant must not stay open here just because nothing removed it
        // from `openThread` -- `#advanceIfOpen` does what archiving already
        // does, moving on to the neighbour, before the row disappears from
        // under it.
        this.#advanceIfOpen(new Set([id]))
        this.threads = this.threads.filter((t) => t.id !== id)
        this.searchResults = this.searchResults.filter((t) => t.id !== id)
      },
      onUpserted: (id) => {
        void this.#patchOne(id)
        // Bug 4: every backend command that can move a thread out of a
        // mailbox from elsewhere -- archive, trash, snooze, an assistant or
        // MCP action, another window's own click -- declares `Thread`/
        // `Updated`, never `Deleted`, and `Thread` carries no mailbox id for
        // `#patchOne` above to notice the row has left. Patching it in place
        // is right as far as it goes -- the subject, the snippet, the flags
        // are current -- but the row stays in a list it may no longer belong
        // to until something re-asks the backend which threads are actually
        // still here. A short, debounced refresh is that ask: harmless if
        // the row still belongs (the page comes back the same), and what
        // actually drops it if it does not, same as opening the mailbox
        // fresh would. Debounced so a batch of changes from one sync tick
        // costs one reload, and `refresh()`'s own generation guard still
        // applies, so a slow one cannot land after a newer list has
        // replaced it.
        //
        // The real fix is a mailbox-scoped change event from the backend --
        // out of scope for this pass.
        this.#scheduleLiveRefresh()
      },
    })
  }

  #liveRefreshTimer: ReturnType<typeof setTimeout> | null = null
  #scheduleLiveRefresh() {
    if (this.#liveRefreshTimer) clearTimeout(this.#liveRefreshTimer)
    this.#liveRefreshTimer = setTimeout(() => {
      this.#liveRefreshTimer = null
      void this.refresh()
      void this.refreshCategoryCounts()
    }, LIVE_REFRESH_DEBOUNCE_MS)
  }

  /**
   * Left hand-written rather than composing `patchOneFromChange`: that
   * helper treats a lock specially, the way `notes` and `accounts` both do
   * -- skip the fallback, let `handle` send the window to the lock screen
   * from wherever else is watching. This one does not; a lock here falls
   * into the same bare `catch` as any other failure and calls `refresh()`,
   * which is what actually reaches the lock screen, through its own
   * `handle`. Routing it through the helper would skip that call on a lock
   * and change what happens, not merely how it is spelled.
   */
  async #patchOne(id: ThreadId) {
    try {
      const detail = await mailApi.getThread(id)
      const at = this.threads.findIndex((t) => t.id === id)
      if (at >= 0) {
        this.threads = this.threads.map((t, i) => (i === at ? detail.thread : t))
      } else {
        // Not in the list: a new thread, or one that has just moved into this
        // mailbox. Where it belongs depends on the mailbox, the category and
        // the sort, which only the backend knows, so the page is asked for
        // again rather than the row guessed into place.
        await this.refresh()
      }
      if (this.selectedThread === id) this.openThread = detail
    } catch {
      await this.refresh()
    }
  }

  // ── derived ──────────────────────────────────────────────────────

  /** The open mailbox -- one of an account's own, or an "All accounts" row. */
  get mailbox(): Mailbox | null {
    return this.#findMailbox(this.selectedMailbox) ?? null
  }

  /**
   * What is on screen, for the assistant -- see `onscreen.ts`.
   *
   * Most specific first in `open`: a draft being written is what "this"
   * means while the compose sheet is up, over the thread it may be replying
   * to. A message somebody expanded by hand is named after the thread, so
   * "reply to this one" can mean an older message than the latest -- the
   * newest expanded one, which is the one most recently read. Starred and
   * Snoozed are views rather than mailboxes the vault holds, so they are
   * named as a `view` rather than sent as references nothing could look up.
   */
  get showing(): Showing {
    const draft = seen('draft', this.composing?.id, this.composing?.subject ?? '')
    // The Scheduled list is mail queued to go out later, not a mailbox: the
    // account it belongs to is what narrows it.
    const scheduledFor = this.viewingScheduledFor
    if (scheduledFor !== null) {
      return {
        view: 'the Scheduled list, mail of theirs queued to send later',
        within: seen('account', scheduledFor, accounts.account(scheduledFor)?.address ?? ''),
        open: draft,
      }
    }
    const box = this.mailbox
    const tab = mailboxHasTabs(box) ? CATEGORY_TABS.find((t) => t.key === this.category) : undefined
    const detail = this.openThread
    // Several threads picked replace the open one in the reading pane.
    const shown =
      detail && detail.thread.id === this.selectedThread && this.checked.size === 0 ? detail : null
    const expanded = shown?.messages.filter((m) => this.expanded.has(m.id)).at(-1)
    const latest = shown?.messages.at(-1)
    // An "All accounts" row is a view too, and the one pseudo-mailbox with
    // tabs: the unified Inbox's tab is worth naming alongside it.
    const onTab = tab ? `, on the ${tab.label} tab` : ''
    const view =
      box && isUnifiedMailbox(box)
        ? `${mailboxDisplayName(box)}, from every account${onTab}`
        : box?.pseudo
          ? mailboxDisplayName(box)
          : tab
            ? `the ${tab.label} tab`
            : null
    return {
      view,
      within: box?.pseudo
        ? []
        : box
          ? seen('mailbox', box.id, mailboxDisplayName(box))
          : seen(
              'account',
              this.selectedAccount,
              accounts.account(this.selectedAccount ?? '')?.address ?? '',
            ),
      open: [
        ...draft,
        ...seen('thread', shown?.thread.id, shown?.thread.subject ?? ''),
        ...(expanded && expanded.id !== latest?.id
          ? seen(
              'message',
              expanded.id,
              `${expanded.from.name || expanded.from.email}, ${expanded.subject}`,
            )
          : []),
      ],
      query: this.searchQuery,
    }
  }
}

export const mail = new MailState()
