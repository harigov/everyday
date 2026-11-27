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
import { notify, windowInFront } from './notify.svelte'
import { debounce } from './store/debounce'
import { proposals } from './proposals.svelte'
import {
  CATEGORY_TABS,
  applyInviteResponse,
  applyRowPatch,
  followUpRows as pickFollowUpRows,
  followUpScope,
  followUpsFirst,
  isSnoozedMailbox,
  isUnifiedMailbox,
  keepLingering,
  listTarget,
  mailAccountKey,
  mailboxDisplayName,
  mailboxHasTabs,
  matchesOnly,
  mergeSearchPage,
  needsSyntheticSnoozedMailbox,
  neighbourThread,
  onlyPhrase,
  openOnArrival,
  parseOnlyPref,
  promoteGmailInbox,
  refreshLimit,
  removeRow,
  restoreRow,
  reuseRows,
  revertRow,
  sameJson,
  serializeOnlyPref,
  snoozeChoices,
  stepCategoryTab,
  emptiableRole,
  sumCategoryCounts,
  syncInProgress,
  syntheticSnoozedMailboxId,
  threadRange,
  unifiedMailboxId,
  unifiedMailboxes as buildUnifiedMailboxes,
  unreadTargets,
  visibleThreadList,
  withOnly,
  type FollowUpRows,
  type MailOnly,
} from './mail'
import { plural } from './format'
import { pref } from './prefs'
import { seen, type Showing } from './onscreen'
import { sorting } from './sorting.svelte'
import { app, handle, quietly } from './state.svelte'
import { applySingleChange } from './store/live-patch'
import { guardedRefresh } from './store/refresh'
import { VaultError } from './types'
import type {
  AccountId,
  CategoryCount,
  UnreadBadge,
  UnreadTarget,
  Draft,
  DraftId,
  Mailbox,
  MailboxId,
  MailboxRole,
  MailCategory,
  MailMessageDetail,
  MailMessageId,
  MailSyncProgress,
  OutboxItem,
  ScheduledSend,
  Thread,
  ThreadDetail,
  ThreadDraft,
  ThreadFilter,
  ThreadId,
  ThreadPage,
  UnsubscribeOffer,
  UnsubscribeOutcome,
} from './types'

/** How long an unread recount waits after the last optimistic action before
 *  it actually asks the backend, per Bug 9: `openThreadById`'s auto-`markRead`
 *  means holding `j` down a long list used to fire one `listThreads` per
 *  mailbox for every row passed over it. */
const UNREAD_DEBOUNCE_MS = 500

/** How long a live thread change waits before the follow-up `refresh()`
 *  Bug 4 needs -- see `#applyChanges`'s own note on why `#patchOne` alone
 *  is not enough. Debounced so a batch of changes from one sync tick costs
 *  one reload of the page, not one per thread. A live round (`#scheduleRound`)
 *  runs at once when none has for this long, and at most once in it after. */
const LIVE_REFRESH_DEBOUNCE_MS = 600

/** How many threads read ahead are kept for `j`/`k` to open at once. */
const DETAILS_KEPT = 8

/** What a live round asks the backend again: the list (and with it, or not,
 *  the follow-ups over an Inbox), the badges, the Outbox, and the mailboxes
 *  themselves -- a folder or label a sync pass met for the first time. */
interface Round {
  list: boolean
  followUps: boolean
  counts: boolean
  outbox: boolean
  mailboxes: boolean
}
const NO_ROUND: Readonly<Round> = {
  list: false,
  followUps: false,
  counts: false,
  outbox: false,
  mailboxes: false,
}
/** What a pass that changed something asks for. Not the mailboxes: a pass
 *  that met a new one says so with a change of its own (`mailboxesChanged`). */
const WHOLE_ROUND: Readonly<Round> = {
  list: true,
  followUps: true,
  counts: true,
  outbox: true,
  mailboxes: false,
}

function joinRounds(a: Readonly<Round>, b: Partial<Round>): Round {
  return {
    list: a.list || !!b.list,
    followUps: a.followUps || !!b.followUps,
    counts: a.counts || !!b.counts,
    outbox: a.outbox || !!b.outbox,
    mailboxes: a.mailboxes || !!b.mailboxes,
  }
}

function anyRound(r: Readonly<Round>): boolean {
  return r.list || r.followUps || r.counts || r.outbox || r.mailboxes
}

/** What a list row says of a thread that a copy read ahead must still say:
 *  a message more, or the row moved by one, and the copy is out of date. */
function detailKey(row: Pick<Thread, 'messageCount' | 'lastDate'>): string {
  return `${row.messageCount}|${row.lastDate}`
}

/** How many threads a page loads at once. */
const PAGE = 50

const NOTHING_ONLY: ReadonlySet<MailOnly> = new Set()

const onlyPref = pref(
  'jatayu.mail.list.only',
  parseOnlyPref,
  new Map<MailboxId, ReadonlySet<MailOnly>>(),
  serializeOnlyPref,
)

/** The inbox tab last picked -- kept across mailboxes and restarts, so
 *  coming back to an inbox shows Priority again rather than All. Absent
 *  means All; a key that is no longer a tab reads as All too. */
const categoryPref = pref<MailCategory | null>(
  'jatayu.mail.category',
  (raw) => CATEGORY_TABS.find((t) => t.key === raw)?.key ?? null,
  null,
)

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
  /** The inbox tab picked, `null` for All. Left alone by a mailbox with no
   *  tabs -- `#filter` and the rest only read it where `mailboxHasTabs` --
   *  so going to Sent and back finds the same tab still picked. */
  category = $state<MailCategory | null>(categoryPref.get())
  /** Which of the list's "show only" toggles each mailbox was left on --
   *  per mailbox, so Unread in the Inbox does not empty Sent, and kept across
   *  restarts, like the sidebar's own fold. See `only`. */
  #onlyByMailbox = $state.raw(onlyPref.get())
  /**
   * Rows that no longer match the toggles because of what was just done to
   * them here -- read, or unstarred -- and stay listed until the list is
   * asked for afresh by a person choosing another mailbox, tab or toggle.
   * Opening a thread marks it read, so without this Unread would pull a
   * message out from under whoever is reading it at the next sync, a minute
   * later at most, and `k` could not get back to the one before.
   */
  #lingering = new Set<ThreadId>()
  /** The rows of `threads` that the last refresh put back from `#lingering`
   *  rather than got from the backend -- which the next one must not count
   *  when it works out how far down to ask, or it would ask for one row more
   *  each time for every row held on to. */
  #putBack = new Set<ThreadId>()
  /** A tab has just been picked, and the reload that settles it is the one
   *  that closes the open thread if the new list does not have it -- where
   *  any other reload keeps it (see `refresh`). Kept until a reload lands
   *  rather than passed to one, since a sync's reload can overtake it. */
  #listPicked = false
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
  /** Which compose sheet is on screen: the id of the draft it opened on. A
   *  draft moved to another account keeps its sheet (`composingMoved`). */
  composeKey = $state<string | null>(null)
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
  /** The threads "Delete forever" is asking about, and whose Trash or Spam
   *  they go from -- the confirmation `MailView` draws for it, raised by a
   *  menu, a button or Delete pressed in Trash. */
  wantsForever = $state<{ ids: ThreadId[]; mailbox: Mailbox } | null>(null)
  /** The Trash or Spam "Empty" is asking about: one account's, or an "All
   *  accounts" row. */
  wantsEmpty = $state<Mailbox | null>(null)
  /** The threads whose senders the unsubscribe question is about -- one, or
   *  every thread picked. Raised by a row's menu, the bulk bar or Mod+U, and
   *  taken by `MailView`, which asks before anybody is written to. */
  wantsUnsubscribe = $state<ThreadId[] | null>(null)
  /**
   * The list of sends `MailView`'s list column is showing in place of a
   * mailbox's threads, or `null`: one account's Scheduled, or the Outbox --
   * one account's, or every account's (`account: null`). A view state
   * alongside `selectedMailbox` rather than a third thing layered over it,
   * since exactly one of the two is ever on screen. Cleared by
   * `selectMailbox`, so picking any ordinary mailbox -- by click, or by a
   * `g i`/`g d`/`g s` shortcut -- always leaves it behind.
   */
  sendsView = $state<{ list: 'scheduled' | 'outbox'; account: AccountId | null } | null>(null)
  searchQuery = $state('')
  /** Replaced whole on every change, never edited in place. */
  searchResults = $state.raw<Thread[]>([])
  /** Where each search result is filed -- see `searchPlace`. */
  searchPlaces = $state.raw<Record<ThreadId, MailboxRole[]>>({})
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
  /**
   * The open Inbox's follow-ups -- threads somebody is waiting on the person
   * to answer, and their own mail nobody has answered -- as `mail_follow_ups`
   * last gave them, for the top of the list. Apart from `threads`, because
   * neither kind need be on the page loaded: a reply can be older than it
   * reaches, and a nudge is from Sent. Read through `followUps`.
   */
  #followUps = $state.raw<Thread[]>([])
  /** Whose follow-ups those are -- an account's id, `''` for every
   *  account's, `null` for nobody's -- so one Inbox's never show for a moment
   *  under another while that one's are on their way. */
  #followUpsFor: string | null = null
  #followUpGeneration = 0
  nextCursor = $state<string | null>(null)
  loadingMore = $state(false)
  loading = $state(false)
  openThread = $state<ThreadDetail | null>(null)
  /**
   * `unsubscribe_offer`'s last answer, kept with the thread it was asked
   * for, so a thread opened since never wears the one before's button for
   * the moment its own answer is on its way. Read through `openOffer`.
   */
  #offer = $state.raw<{ thread: ThreadId; offer: UnsubscribeOffer | null } | null>(null)
  /** Bumped by every ask and every close, so an answer overtaken by either
   *  is dropped rather than drawn. */
  #offerAsk = 0
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
  /** Accounts a "Sync now" started a pass for that status has not yet seen
   *  finish -- what keeps the button turning after `syncRequested` settles.
   *  Only these: a routine pass in the background is no reason to spin it. */
  userSyncing = $state.raw<ReadonlySet<AccountId>>(new Set())
  /** Every account's drafts still queued to send later, soonest first --
   *  what `MailNav`'s own Scheduled row counts and `MailScheduled.svelte`
   *  lists. Unscoped by `selectedAccount`: the nav draws one row per
   *  account regardless of which account the mailbox list is filtered to. */
  scheduled = $state<ScheduledSend[]>([])
  /** Every account's Outbox -- what the server would not take, failing and
   *  being retried or failed for good -- newest first. What `MailNav`'s
   *  Outbox rows count and `MailOutbox.svelte` lists. */
  outbox = $state<OutboxItem[]>([])

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
    // Asked again after a lock: the server may have been updated since.
    this.#listedBadges = false
    this.#generation += 1
    this.selectedAccount = null
    this.selectedMailbox = null
    this.category = categoryPref.get()
    this.#lingering.clear()
    this.#putBack.clear()
    this.#listPicked = false
    this.selectedThread = null
    this.checked = new Set()
    this.#anchor = null
    this.expanded = new Set()
    this.composing = null
    this.composeInline = false
    this.parkedDraft = null
    this.wantsSnooze = null
    this.wantsLabel = null
    this.wantsForever = null
    this.wantsEmpty = null
    this.wantsUnsubscribe = null
    this.#offer = null
    this.#offerAsk += 1
    this.sendsView = null
    this.summary = null
    if (this.#undoTimer) clearInterval(this.#undoTimer)
    this.#undoTimer = null
    this.sendingUndo = null
    if (this.#unreadRefreshTimer) clearTimeout(this.#unreadRefreshTimer)
    this.#unreadRefreshTimer = null
    if (this.#openReloadTimer) clearTimeout(this.#openReloadTimer)
    this.#openReloadTimer = null
    if (this.#outboxRefreshTimer) clearTimeout(this.#outboxRefreshTimer)
    this.#outboxRefreshTimer = null
    if (this.#roundTimer) clearTimeout(this.#roundTimer)
    this.#roundTimer = null
    this.#pending = { ...NO_ROUND }
    this.#stale = { ...NO_ROUND }
    this.#roundAt = -Infinity
    this.#forgetDetails()
    this.#unreadGeneration += 1
    this.#categoryGeneration += 1
    this.#accountKey = null
    this.#unseenIn = null
    this.mailboxes = []
    this.unifiedMailboxes = []
    this.threads = []
    this.#followUps = []
    this.#followUpsFor = null
    this.#followUpGeneration += 1
    this.nextCursor = null
    this.openThread = null
    this.unreadCounts = new Map()
    this.categoryCounts = []
    this.syncStatus = []
    this.syncRequested = false
    this.userSyncing = new Set()
    this.scheduled = []
    this.outbox = []
    this.#sent.clear()
    this.clearSearch()
    this.#loaded = false
  }

  /** Called by `MailNav` on mount. Idempotent, like every other app's `start`. */
  async start() {
    if (!app.supportsMail || this.#loaded) return
    this.#loaded = true
    // See `todo.start` for why this is idempotent and safe to ask twice.
    // Alongside the first list rather than ahead of it: what proposals draw
    // here (a draft waiting in Drafts) is drawn reactively whenever it lands.
    await Promise.all([proposals.loaded ? null : proposals.refresh(), this.refreshMailboxes()])
    await this.refreshSyncStatus()
  }

  /** The read of the mailboxes under way, and whether another was asked for
   *  while it was -- see `refreshMailboxes`. */
  #mailboxesRun: Promise<void> | null = null
  #mailboxesAgain = false
  /** `mailAccountKey` of the accounts the mailboxes were last read for. */
  #accountKey: string | null = null

  /** What the mailbox list was last read for -- see `mailAccountKey`. `null`
   *  until the first read has asked which accounts there are. */
  get accountKey(): string | null {
    return this.#accountKey
  }

  /**
   * Read every account's mailboxes again, and the counts beside them.
   *
   * One at a time: asked while a read is under way, the answer is one more
   * read after it -- which sees whatever changed meanwhile, an account added
   * or the scope switched -- and every caller's promise settles once that
   * one has. Mail's first visit asks from three places at once.
   */
  refreshMailboxes(): Promise<void> {
    if (this.#mailboxesRun) {
      this.#mailboxesAgain = true
      return this.#mailboxesRun
    }
    const run = (async () => {
      do {
        this.#mailboxesAgain = false
        await this.#readMailboxes()
      } while (this.#mailboxesAgain)
    })().finally(() => {
      this.#mailboxesRun = null
    })
    this.#mailboxesRun = run
    return run
  }

  async #readMailboxes() {
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
      // The same mailboxes as before, as they nearly always are, stay the ones
      // drawn: everything reading `mailbox` would otherwise work itself out
      // again for nothing.
      const boxes = [...real, ...synthetic]
      const unified = buildUnifiedMailboxes(boxes, accountIds)
      if (!sameJson(boxes, $state.snapshot(this.mailboxes))) this.mailboxes = boxes
      if (!sameJson(unified, $state.snapshot(this.unifiedMailboxes))) {
        this.unifiedMailboxes = unified
      }
      // Nothing open yet -- or what was open is gone, the unified Inbox of
      // an account set that has since dropped to one. With two or more
      // accounts, Mail opens on every inbox at once rather than whichever
      // account happens to come first.
      let selected = false
      if (!this.selectedMailbox || !this.#findMailbox(this.selectedMailbox)) {
        const inbox =
          this.unifiedMailboxes.find((m) => m.id === unifiedMailboxId('inbox')) ??
          this.mailboxes.find((m) => m.role === 'inbox')
        if (inbox) {
          selected = true
          await this.selectMailbox(inbox.id)
        }
      }
      void this.refreshUnreadCounts()
      // Opening a mailbox counts its tabs already.
      if (!selected) void this.refreshCategoryCounts()
      void this.refreshScheduled()
      void this.refreshOutbox()
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
    if (this.selectedAccount) {
      // One account's mailboxes say nothing about the others, so whatever the
      // account list is now has been answered.
      if (accounts.list.length > 0) this.#accountKey = mailAccountKey(accounts.list)
      return [this.selectedAccount]
    }
    const list = await mailApi.listAccounts()
    this.#accountKey = mailAccountKey(list)
    return list.filter((a) => a.services.mail).map((a) => a.id)
  }

  /** Which unread recount is current, so a slow one cannot land after a
   *  newer one. */
  #unreadGeneration = 0

  /** Unread counts for every mailbox currently listed, in one call --
   *  `mail_unread_badges`, which adds up what the newest 200 threads of each
   *  row's mailboxes say, as this used to by reading each of them.
   *
   *  Filtered by `snoozed` the same way `refresh`/`loadMore` are (Bug 3): an
   *  ordinary mailbox's badge must not count a thread hidden from its own
   *  list because it is currently snoozed, and the Snoozed pseudo-mailbox's
   *  badge is exactly the reverse -- only threads that are.
   *
   *  A unified row's badge is its members' added up, and a thread filed in
   *  two of them (a Gmail label) counted once rather than twice -- see
   *  `unreadTargets` for what each row asks. */
  async refreshUnreadCounts() {
    const generation = ++this.#unreadGeneration
    const targets = unreadTargets([...this.mailboxes, ...this.unifiedMailboxes], this.mailboxes)
    try {
      const badges = targets.length > 0 ? await this.#unreadBadges(targets) : []
      if (generation !== this.#unreadGeneration) return
      const counts = new Map(badges.map((b): [MailboxId, number] => [b.key, b.unread]))
      if (!sameJson([...counts], [...this.unreadCounts])) this.unreadCounts = counts
    } catch (e) {
      await quietly(e)
    }
  }

  /** Set once a server has answered that it does not know
   *  `mail_unread_badges` -- one from before it, which a client of this build
   *  may be paired with -- so the badges are worked out the old way from
   *  then on, rather than left blank. */
  #listedBadges = false

  async #unreadBadges(targets: UnreadTarget[]): Promise<UnreadBadge[]> {
    if (!this.#listedBadges) {
      try {
        return await mailApi.unreadBadges(targets)
      } catch (e) {
        if (!(e instanceof VaultError && e.code === 'unknown_command')) throw e
        this.#listedBadges = true
      }
    }
    return mailApi.unreadBadgesListed(targets)
  }

  #categoryGeneration = 0

  /** The tabs' badges, for the mailbox on screen -- nothing to count for
   *  one without tabs. Quiet on failure: a badge that does not update is not
   *  worth a toast. Asked beside every unread recount an action or a live
   *  round makes, so an archive or a live change moves the numbers the same
   *  moment it moves the nav's. */
  async refreshCategoryCounts() {
    const generation = ++this.#categoryGeneration
    const box = this.mailbox
    const target = mailboxHasTabs(box) ? listTarget(box, this.mailboxes) : null
    if (!target) {
      if (this.categoryCounts.length > 0) this.categoryCounts = []
      return
    }
    try {
      // One count per member mailbox, added up: the unified Inbox's tabs
      // are every account's tabs at once, and no thread is in two
      // accounts' inboxes to be counted twice.
      const lists = await Promise.all(target.mailboxIds.map((id) => mailApi.categoryCounts(id)))
      const counts = sumCategoryCounts(lists)
      if (generation !== this.#categoryGeneration || this.mailbox?.id !== box?.id) return
      if (!sameJson(counts, $state.snapshot(this.categoryCounts))) this.categoryCounts = counts
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
    // Nobody can see the badges: counted once they can (`setOnScreen`).
    if (!this.#isLive()) {
      this.#stale = joinRounds(this.#stale, { counts: true })
      return
    }
    if (this.#unreadRefreshTimer) clearTimeout(this.#unreadRefreshTimer)
    this.#unreadRefreshTimer = setTimeout(() => {
      this.#unreadRefreshTimer = null
      void this.refreshUnreadCounts()
      void this.refreshCategoryCounts()
    }, UNREAD_DEBOUNCE_MS)
  }

  async refreshSyncStatus() {
    try {
      const status = await mailApi.syncStatus()
      // Kept as it was when it reads the same, unless a pass is on: `MailNav`
      // arms its next two-second read only when this is assigned, and a phase
      // that holds still -- `connecting` through a slow sign-in, one large
      // body downloading -- reads the same twice and would stop that poll.
      if (status.some(syncInProgress) || !sameJson(status, $state.snapshot(this.syncStatus))) {
        this.syncStatus = status
      }
      // A pass somebody asked for is over once status says it is.
      if (this.userSyncing.size > 0) {
        const still = [...this.userSyncing].filter((id) =>
          status.some((s) => s.accountId === id && syncInProgress(s)),
        )
        if (still.length !== this.userSyncing.size) this.userSyncing = new Set(still)
      }
    } catch (e) {
      await quietly(e)
    }
  }

  /** After a "Sync now" has woken `ids`' tasks: wait for their passes to have
   *  begun, read status, and keep the button turning for those still going
   *  (`userSyncing`). Answers whether any are. */
  async #followSync(ids: readonly AccountId[]): Promise<boolean> {
    await new Promise((resolve) => setTimeout(resolve, SYNC_SETTLE_MS))
    await this.refreshSyncStatus()
    const busy = ids.filter((id) =>
      this.syncStatus.some((s) => s.accountId === id && syncInProgress(s)),
    )
    if (busy.length > 0) this.userSyncing = new Set([...this.userSyncing, ...busy])
    return busy.length > 0
  }

  async syncNow(accountId: AccountId) {
    try {
      await mailApi.syncAccount(accountId)
      await this.refreshSyncStatus()
      void this.#followSync([accountId]).catch(() => {})
    } catch (e) {
      await handle(e)
    }
  }

  /**
   * The list column's "Sync now": a pass for each of `accountIds`, or for
   * every mail account when none are named.
   *
   * `MailNav`'s two-second poll takes over from here for as long as any
   * account is mid-pass, and reloads the mailboxes and their counts on the
   * way back to idle; the button keeps turning until then (`userSyncing`).
   * The list hears from the pass itself, as it does from any pass that
   * changed something (`#applyChanges`). A pass that was over
   * before status was even read back -- nothing new on the server, or the
   * mock, whose `sync_account` resolves only once its pretend pass is done
   * -- never gives that poll a falling edge to see, so this reloads the list
   * and the counts itself in that case: somebody pressed the button.
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
      if (!(await this.#followSync(ids))) {
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
    // Leaves a list of sends behind, the way picking a thread leaves a
    // search: exactly one of the two is ever the list column's content.
    this.sendsView = null
    this.selectedMailbox = id
    this.selectedThread = null
    this.openThread = null
    this.clearChecked()
    // `category` is kept even for a mailbox with no tabs: every reader of
    // it checks `mailboxHasTabs` first (Finding 1), so it filters nothing
    // there, and the next inbox opens on the tab last picked.
    this.#lingering.clear()
    this.#putBack.clear()
    void this.refreshCategoryCounts()
    await this.refresh()
  }

  /** The Scheduled row picked in the nav -- see `sendsView`. */
  selectScheduled(accountId: AccountId) {
    this.sendsView = { list: 'scheduled', account: accountId }
    this.selectedThread = null
    this.openThread = null
    // Picked threads would be out of sight under a list of sends, and yet
    // still what `e` and `#` act on.
    this.clearChecked()
  }

  /** An Outbox row picked in the nav: one account's, or every account's. */
  selectOutbox(accountId: AccountId | null) {
    this.sendsView = { list: 'outbox', account: accountId }
    this.selectedThread = null
    this.openThread = null
    this.clearChecked()
    void this.refreshOutbox()
  }

  setCategory(category: MailCategory | null) {
    this.category = category
    categoryPref.set(category)
    this.#lingering.clear()
    this.#putBack.clear()
    this.clearChecked()
    this.#listPicked = true
    // The follow-ups are the Inbox's, whichever tab is on: drawn from what
    // is already here (`followUpRows`), not asked for again on every Tab.
    void this.refresh({ followUps: false })
  }

  /** The toggles on for the mailbox on screen. */
  get only(): ReadonlySet<MailOnly> {
    return (this.selectedMailbox && this.#onlyByMailbox.get(this.selectedMailbox)) || NOTHING_ONLY
  }

  /** Turn toggles on or off for the mailbox on screen. The thread being read
   *  stays listed either way: a filter is no reason to lose your place. */
  setOnly(only: ReadonlySet<MailOnly>) {
    const box = this.selectedMailbox
    if (!box) return
    const byMailbox = new Map(this.#onlyByMailbox)
    if (only.size > 0) byMailbox.set(box, only)
    else byMailbox.delete(box)
    this.#onlyByMailbox = byMailbox
    onlyPref.set(byMailbox)
    this.#holdOnlyTheOpenThread()
    this.clearChecked()
    void this.refresh({ followUps: false })
  }

  toggleOnly(key: MailOnly) {
    const only = new Set(this.only)
    if (!only.delete(key)) only.add(key)
    this.setOnly(only)
  }

  /** The list is being asked for afresh, so nothing lingers any longer but the
   *  thread somebody is in the middle of reading. */
  #holdOnlyTheOpenThread() {
    this.#lingering = new Set(this.selectedThread ? [this.selectedThread] : [])
    this.#putBack.clear()
  }

  /**
   * Which of the rows a page left out have really gone from this mailbox --
   * archived on a phone, say, or by the assistant -- rather than merely been
   * hidden by a toggle, sorted into another tab or pushed past the end of the
   * page by mail arriving above them. Only what is being held on to is worth
   * asking about: everything lingering, and `open` -- the thread being read,
   * when `refresh` wants to know. Asked without the toggles or the tab, which
   * are what is being seen past.
   */
  async #goneFrom(
    target: { mailboxIds: MailboxId[]; snoozed: boolean },
    served: ReadonlySet<ThreadId>,
    open: ThreadId | null,
  ): Promise<ReadonlySet<ThreadId>> {
    const held = new Set(this.#lingering)
    if (open) held.add(open)
    const asking = [...held].filter((id) => !served.has(id))
    if (asking.length === 0) return new Set()
    const still = new Set(
      await mailApi.threadsListed(target.mailboxIds, { snoozed: target.snoozed }, asking),
    )
    return new Set(asking.filter((id) => !still.has(id)))
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
   * The thread being read is not a reload's to take away. Most reloads are
   * nobody's doing -- a sync pass, every minute or so, or a change made
   * elsewhere -- and the page can leave the open thread out for reasons that
   * say nothing about it: mail arriving above it pushes it past the end of
   * what is loaded, the sorting moves it to another tab, it no longer
   * matches a toggle, or it was never a row here at all, opened from a
   * search, a notification or another app. So its row stays while it is
   * open, and it closes only once `#goneFrom` says it has left this mailbox
   * -- archived on a phone, or by the assistant -- and not even then while a
   * reply is being written under it: closing the sheet under somebody's
   * words is the one thing a background reload must never do. The reply
   * finished or put down, the next reload lets it go.
   *
   * A tab somebody has just picked is the exception (`#listPicked`): there
   * the open thread goes if the new list does not have it, as it always has.
   *
   * `followUps: false` keeps the follow-ups already here when they are still
   * this list's -- a tab or a toggle changes which of them are drawn, never
   * which there are.
   */
  async refresh(opts: { followUps?: boolean } = {}) {
    // The top of an Inbox comes along with every reload of it -- opening it,
    // Sync now, a live change -- and goes as another list is opened.
    const scope = followUpScope(this.mailbox)
    const owner = scope ? (scope.account ?? '') : null
    if (opts.followUps !== false || owner !== this.#followUpsFor) void this.refreshFollowUps()
    // `listTarget` is where the mailbox ids actually meant for the backend
    // come from -- the mailbox itself, ordinarily, except for the synthetic
    // Snoozed row `refreshMailboxes` stands in for an account whose own
    // Inbox is what must be asked instead, and a unified row, which is
    // every account's mailbox of its role at once. `null` here is exactly
    // the old `!mailboxId`.
    const target = listTarget(this.mailbox, this.mailboxes)
    if (!target) {
      this.#listPicked = false
      return
    }
    await guardedRefresh(
      this.#refreshGeneration,
      async (isCurrent) => {
        const page = await this.#listPage(
          target.mailboxIds,
          this.#listFilter(target),
          null,
          refreshLimit(this.threads.filter((t) => !this.#putBack.has(t.id)).length, PAGE),
        )
        if (!isCurrent()) return
        // Deduped even within one page: a thread filed in two of a unified
        // row's mailboxes must never be two rows with one key.
        const rows = mergeSearchPage([], page.threads)
        const served = new Set(rows.map((t) => t.id))
        const picked = this.#listPicked
        // A row of this list's that the page has left out: whether the open
        // thread has left the mailbox, or only the page, the tab or a toggle,
        // is asked by id. In a tab just picked, only with a toggle on, as
        // before -- without one, not being on the page is enough.
        const wasRow = (id: ThreadId | null): id is ThreadId =>
          id !== null && !served.has(id) && this.threads.some((t) => t.id === id)
        const asked = this.selectedThread
        const askOpen = picked ? this.only.size > 0 && asked !== null : wasRow(asked)
        const gone = await this.#goneFrom(target, served, askOpen ? asked : null)
        if (!isCurrent()) return
        this.#listPicked = false
        for (const id of gone) this.#lingering.delete(id)
        const open = this.selectedThread
        const writing = this.composeInline
        const holding = !picked && wasRow(open) && (!gone.has(open) || writing)
        const held = holding ? new Set([...this.#lingering, open]) : this.#lingering
        // The open thread may never have had a row here to keep, if it was
        // opened from elsewhere: the copy in the reading pane does as well.
        const reading = this.openThread?.thread
        const shown =
          reading && !this.threads.some((t) => t.id === reading.id)
            ? [...this.threads, reading]
            : this.threads
        // The rows that came back as they were stay the rows drawn, and the
        // list itself when none moved: a pass that changed nothing here
        // redraws nothing.
        const next = reuseRows(keepLingering(rows, shown, held), this.threads)
        if (next !== this.threads) this.threads = next
        this.#putBack = new Set(this.threads.map((t) => t.id).filter((id) => !served.has(id)))
        this.nextCursor = page.nextCursor ?? null
        // A follow-up drawn over the page is a row on screen as much as one
        // of the page's own, though it may be older than the page reaches,
        // or from Sent: still one to keep picked, or open.
        const listed = new Set(followUpsFirst(this.followUpRows, this.threads).map((t) => t.id))
        if (this.checked.size > 0) {
          const kept = [...this.checked].filter((id) => listed.has(id))
          if (kept.length !== this.checked.size) this.checked = new Set(kept)
        }
        const unlisted =
          picked &&
          this.only.size === 0 &&
          !this.#lingering.has(open ?? '') &&
          !listed.has(open ?? '')
        const left = gone.has(open ?? '') && !listed.has(open ?? '')
        // Whatever kept it listed until now -- a row, a follow-up drawn over
        // the page, a row held on to -- a reply under it keeps it open.
        if (open && (left || unlisted) && (picked || !writing)) {
          // A reply being written under it has nowhere left to be drawn: set
          // aside, and said so, rather than left in `composing` out of sight.
          // Only ever in a tab just picked.
          if (this.composeInline) this.#park()
          this.selectedThread = null
          this.openThread = null
        }
      },
      {
        setLoading: (v) => (this.loading = v),
        // A tab's reload that failed has settled nothing, and the next one
        // is a sync's, not a pick: it must not close what is open by then.
        onError: (e) => {
          this.#listPicked = false
          return handle(e)
        },
      },
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
      const page = await this.#listPage(target.mailboxIds, this.#listFilter(target), cursor, PAGE)
      // A mailbox or category change while this page was on its way bumps
      // the generation; its rows belong to a list no longer showing.
      if (generation !== this.#generation || cursor !== this.nextCursor) return
      // `mergeSearchPage` rather than a bare append: across several
      // mailboxes, a thread filed in two of them comes back once for each,
      // on whichever pages its two rows land. A row held on to goes back where
      // its date says, or an old one left at the end of the last page would
      // sit above this one, with every heading under it wrong.
      const rows = mergeSearchPage(
        this.threads.filter((t) => !this.#putBack.has(t.id)),
        page.threads,
      )
      const served = new Set(rows.map((t) => t.id))
      this.threads = keepLingering(rows, this.threads, this.#putBack)
      this.#putBack = new Set([...this.#putBack].filter((id) => !served.has(id)))
      this.nextCursor = page.nextCursor ?? null
    } catch (e) {
      await quietly(e)
    } finally {
      this.loadingMore = false
    }
  }

  /** What `refresh` and `loadMore` both ask for, so a second page can never
   *  disagree with the first about what belongs in the list. */
  #listFilter(target: { snoozed: boolean }): ThreadFilter {
    // Bug 3: `snoozed` was never sent at all, so a snoozed thread -- still a
    // member of whatever mailbox it was snoozed from -- came straight back
    // on the very next refresh. `false` everywhere except the Snoozed view,
    // which wants nothing else.
    const filter = withOnly({ snoozed: target.snoozed }, this.only)
    // Finding 1: a category filter only means anything for the inbox's own
    // tabs -- sent elsewhere, it silently narrowed a mailbox with no tab
    // strip to have set it from.
    if (this.category && mailboxHasTabs(this.mailbox)) filter.category = this.category
    return filter
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

  // ── follow-ups at the top of the Inbox ────────────────────────────

  /**
   * Ask again which threads go at the top of the Inbox on screen: one
   * account's, or every account's under "All inboxes" -- and none for any
   * other list, which `followUpScope` decides. Called by every `refresh`,
   * which a live thread change and a sync pass both end in, so the top keeps
   * up with the follow-up pass as the list keeps up with sync. Quiet on
   * failure: an Inbox without its follow-ups is still an Inbox.
   */
  async refreshFollowUps() {
    const generation = ++this.#followUpGeneration
    const scope = followUpScope(this.mailbox)
    const owner = scope ? (scope.account ?? '') : null
    if (owner !== this.#followUpsFor) {
      this.#followUpsFor = owner
      this.#followUps = []
    }
    if (!scope) return
    try {
      const { threads } = await mailApi.followUps(scope.account)
      if (generation !== this.#followUpGeneration) return
      const next = reuseRows(threads, this.#followUps)
      if (next !== this.#followUps) this.#followUps = next
    } catch (e) {
      await quietly(e)
    }
  }

  /** The follow-ups for the Inbox on screen: none over a search or a list of
   *  sends, both of which stand in the list column's place, and none once
   *  they are switched off in Settings → Mail sorting -- a list loaded just
   *  before is not left standing until the next load. */
  get followUps(): readonly Thread[] {
    if (this.sendsView !== null || this.searchQuery.trim()) return []
    if (sorting.sorting?.followUp?.on === false) return []
    return this.#followUps
  }

  /** The follow-ups drawn at the top, for the tab and toggles on -- see
   *  `mail.ts`'s `followUpRows`. A row read or unstarred here keeps its
   *  place, as it does in the list below. */
  get followUpRows(): FollowUpRows {
    const tab = mailboxHasTabs(this.mailbox) ? this.category : null
    return pickFollowUpRows(this.followUps, tab, this.only, this.#lingering)
  }

  /**
   * "Not needed": the thread's follow-up goes, here at once and then for
   * good, until something new arrives in it. Put back, and said why, if the
   * backend refuses.
   *
   * A row drawn only at the top -- a nudge, which is from Sent -- leaves the
   * list altogether, so the cursor moves on as it does for anything leaving;
   * a reply's row merely drops to its date below, and stays open.
   */
  async dismissFollowUp(id: ThreadId) {
    if (!this.threads.some((t) => t.id === id)) this.#advanceIfOpen(new Set([id]))
    // A load already on its way asked before this, and would bring it back.
    this.#followUpGeneration++
    await this.#act([id], { followUp: null }, async (ids) => {
      await Promise.all(ids.map((t) => mailApi.dismissFollowUp(t)))
      void this.refreshFollowUps()
    })
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
    this.#unseenIn = null
    // Opened after a tab was picked: no longer what the pick may close.
    this.#listPicked = false
    this.#openVersion++
    this.summary = null
    // Asked beside the thread rather than after it: the toolbar's Unsubscribe
    // can then be there as soon as the thread is.
    void this.#loadOffer(id)
    // Opening one thread is leaving the many behind -- see `checked`.
    this.clearChecked()
    this.#anchor = id
    // Read ahead, as the thread beside the last one opened: drawn now.
    const ready = this.#takeDetail(id)
    if (ready) {
      this.#showAhead(ready)
      return
    }
    try {
      // Still being read ahead -- `j` pressed again before the next row's
      // read is back: that read, rather than a second one beside it.
      const early = this.#prefetching.get(id)
      if (early) {
        await early
        if (this.selectedThread !== id) return
        const ahead = this.#takeDetail(id)
        if (ahead) {
          this.#showAhead(ahead)
          return
        }
      }
      const detail = await mailApi.getThread(id)
      // Moved on while this was loading: a later open owns the pane now, and
      // this thread must not be shown, marked read or prefetched around.
      if (this.selectedThread !== id) return
      void this.#showOpened(detail)
    } catch (e) {
      await handle(e)
    }
  }

  /** A thread read ahead, opened: drawn at once, and read again behind it --
   *  after it is marked read, so the fresh copy says so too -- in case
   *  anything moved since that a change has not yet said. */
  #showAhead(detail: ThreadDetail) {
    void this.#showOpened(detail).then(() => this.#reloadOpenThread())
  }

  /** Put a thread just opened in the pane, `selectedThread` already being
   *  it. Answers once it has been marked read, if it needed to be. */
  #showOpened(detail: ThreadDetail): Promise<void> {
    const id = detail.thread.id
    this.openThread = detail
    // The newest message starts expanded, and so does every one not yet
    // read -- read before this open marks them so -- lest one in the
    // middle hide in a collapsed summary line; the rest collapse.
    this.expanded = openOnArrival(detail.messages)
    const reading = detail.thread.unreadCount > 0 ? this.markRead(id) : Promise.resolve()
    void this.#prefetchNeighbours(id)
    // Nothing but a draft, as a thread opened from Drafts often is: open it
    // to write, as clicking a draft does in any mail program.
    const only = detail.drafts.length === 1 ? detail.drafts[0] : null
    if (detail.messages.length === 0 && only?.draft && !this.composing) {
      this.openInlineDraft(only.draft)
    } else {
      this.#openAutoDraftIfAny(detail)
    }
    return reading
  }

  /**
   * Open Mail at a thread from outside it -- the assistant transcript's own
   * link ("Archived: Plans for Saturday →"), or the same list in Settings
   * → Sharing and the assistant's settings.
   *
   * The same two-step every other app's own cross-app link already is
   * (`app.goTo('notes')`, then `noteStore.openNote(id)` if it went, in
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
   * thread. `get_thread` already cards every draft still being written that
   * answers one of the thread's messages, so it is looked for there -- the
   * account's own only, as reading its drafts used to find it, rather than
   * one moved to another account to be sent from there.
   */
  #openAutoDraftIfAny(detail: ThreadDetail) {
    if (this.composing) return
    const ids = new Set(detail.messages.map((m) => m.id))
    const auto = detail.drafts
      .map((card) => card.draft)
      .find(
        (d): d is Draft =>
          !!d &&
          d.accountId === detail.thread.accountId &&
          d.origin.type === 'assistant' &&
          d.state.type === 'editing' &&
          !!d.inReplyTo &&
          ids.has(d.inReplyTo),
      )
    // A reply into the thread that is still open -- the same inline
    // treatment `reply()`/`forward()` give one started by hand.
    if (auto && this.selectedThread === detail.thread.id) this.openInlineDraft(auto)
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
    this.#offer = null
    this.#offerAsk += 1
  }

  /**
   * How unsubscribing from the open thread's sender would go, or `null`:
   * nothing open, its mail names no way out, or the answer has not come
   * yet. Only ever the open thread's own -- see `#offer`.
   */
  get openOffer(): UnsubscribeOffer | null {
    const open = this.openThread?.thread.id
    const held = this.#offer
    return open && held?.thread === open ? held.offer : null
  }

  /**
   * Ask what unsubscribing from `id`'s sender would do, for the toolbar.
   * Quiet on failure: the button is an extra, and a thread that cannot say
   * is read just the same. An answer for a thread somebody has since left
   * is dropped.
   */
  async #loadOffer(id: ThreadId): Promise<void> {
    const ask = ++this.#offerAsk
    try {
      const offer = await mailApi.unsubscribeOffer(id)
      if (ask !== this.#offerAsk || this.selectedThread !== id) return
      this.#offer = { thread: id, offer }
    } catch (e) {
      await quietly(e)
    }
  }

  /** Whichever list is on screen, in the order it is drawn -- see
   *  `visibleThreadList`, and `followUpsFirst` for the follow-ups over an
   *  Inbox. What `j`/`k`, Shift+click, Mod+A and the action keys all walk, so
   *  each goes where the eye already has. */
  get shownThreads(): readonly Thread[] {
    const listed = followUpsFirst(this.followUpRows, this.threads)
    return visibleThreadList(this.searchQuery, listed, this.searchResults)
  }

  /** The next and previous thread in list order, for `j`/`k` and prefetch.
   *  Finding 3: this used to read `this.threads` even while a search's
   *  results were what was actually on screen, so `j`/`k` moved through the
   *  mailbox behind the search rather than through the rows visible. */
  #neighbour(step: 1 | -1): Thread | null {
    return neighbourThread(this.shownThreads, this.selectedThread, step)
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
      const range = threadRange(this.shownThreads, from, id)
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

  /**
   * Shift+Down/Up: move the cursor a row and pick every thread from where
   * the picking started to it, the keyboard's Shift+click. The first press
   * starts from the thread under the cursor -- the open one, usually -- so
   * it and the next are picked together.
   */
  extendChecked(step: 1 | -1) {
    const shown = this.shownThreads
    const listed = (id: ThreadId | null) => id !== null && shown.some((t) => t.id === id)
    const next = this.#neighbour(step)
    if (!next) return
    // An anchor left over from a thread since removed, or from another
    // mailbox, starts the run afresh from the cursor rather than picking
    // one row at a time.
    if (!listed(this.#anchor))
      this.#anchor = listed(this.selectedThread) ? this.selectedThread : next.id
    this.selectedThread = next.id
    this.checked = new Set(threadRange(shown, this.#anchor, next.id))
  }

  /** Mod+A: every thread the list has loaded. */
  checkAll() {
    this.checked = new Set(this.shownThreads.map((t) => t.id))
  }

  clearChecked() {
    if (this.checked.size > 0) this.checked = new Set()
  }

  /** What an action key acts on: every picked thread, else the selected
   *  one, else nothing. In list order, so a batch reads top to bottom. */
  get targets(): ThreadId[] {
    if (this.checked.size > 0) {
      return this.shownThreads.filter((t) => this.checked.has(t.id)).map((t) => t.id)
    }
    return this.selectedThread ? [this.selectedThread] : []
  }

  /**
   * Prefetch the next and previous thread's detail, per "The speed budget",
   * and keep it (`#details`) for `openThreadById` to draw at once.
   * Best-effort and silent: a prefetch that fails is simply not there yet
   * when the reader arrives, which is exactly what would have happened
   * without it.
   */
  #prefetching = new Map<ThreadId, Promise<void>>()
  async #prefetchNeighbours(around: ThreadId) {
    const list = this.shownThreads
    const at = list.findIndex((t) => t.id === around)
    if (at < 0) return
    for (const t of [list[at - 1], list[at + 1]]) {
      if (!t || this.#prefetching.has(t.id)) continue
      const key = detailKey(t)
      if (this.#details.get(t.id)?.key === key) continue
      const generation = this.#detailsGeneration
      const read = mailApi
        .getThread(t.id)
        .then((detail) => {
          if (generation === this.#detailsGeneration) this.#keepDetail(t.id, detail, key)
        })
        .catch(() => {})
        .finally(() => this.#prefetching.delete(t.id))
      this.#prefetching.set(t.id, read)
    }
  }

  /**
   * Threads read ahead, oldest first, each with the `detailKey` of the row it
   * was read for. A row that has since gained a message, or been moved in
   * the list by one, no longer matches its key and is read afresh; anything
   * done to a thread here or reported from elsewhere forgets it
   * (`#forgetDetails`). At most `DETAILS_KEPT`, and none past a lock.
   */
  #details = new Map<ThreadId, { detail: ThreadDetail; key: string }>()
  /** Bumped whenever what was read ahead may be out of date, so a read still
   *  on its way when that happened is not kept. */
  #detailsGeneration = 0

  #keepDetail(id: ThreadId, detail: ThreadDetail, key: string) {
    this.#details.delete(id)
    this.#details.set(id, { detail, key })
    for (const old of this.#details.keys()) {
      if (this.#details.size <= DETAILS_KEPT) break
      this.#details.delete(old)
    }
  }

  /** The thread read ahead for `id`, taken out of `#details`, if it still
   *  matches the row on screen -- one with no row is not trusted either. */
  #takeDetail(id: ThreadId): ThreadDetail | null {
    const kept = this.#details.get(id)
    if (!kept) return null
    this.#details.delete(id)
    const row = this.shownThreads.find((t) => t.id === id)
    return row && detailKey(row) === kept.key ? kept.detail : null
  }

  /** Forget what was read ahead of `ids`, or of every thread. */
  #forgetDetails(ids?: readonly ThreadId[]) {
    if (!ids) {
      this.#details.clear()
      this.#detailsGeneration++
      return
    }
    for (const id of ids) this.#details.delete(id)
    if (ids.some((id) => this.#prefetching.has(id))) this.#detailsGeneration++
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
    const list = this.shownThreads
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
    this.#forgetDetails(ids)
    const befores: Thread[] = []
    const searchBefores: Thread[] = []
    // The top of the Inbox too: its rows are copies of their own, and a nudge
    // is drawn nowhere else.
    const followBefores: Thread[] = []
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
      const { rows: followRows, before: followBefore } = applyRowPatch(this.#followUps, id, patch)
      if (followBefore) {
        this.#followUps = followRows
        followBefores.push(followBefore)
      }
    }
    // A row this stops matching stays listed; one it matches again does not
    // need to be, should it be read elsewhere later. Both undone if this fails.
    const lingered: ThreadId[] = []
    const released: ThreadId[] = []
    if (this.only.size > 0) {
      // Each thread once, wherever it is drawn.
      const touched = new Map(
        [...followBefores, ...befores].map((t): [ThreadId, Thread] => [t.id, t]),
      )
      for (const before of touched.values()) {
        const stopsMatching = !matchesOnly({ ...before, ...patch }, this.only)
        if (stopsMatching && !this.#lingering.has(before.id)) {
          this.#lingering.add(before.id)
          lingered.push(before.id)
        } else if (!stopsMatching && this.#lingering.delete(before.id)) {
          released.push(before.id)
        }
      }
    }
    const openId = this.openThread?.thread.id
    const openBefore = openId && ids.includes(openId) ? this.openThread!.thread : null
    if (openBefore) {
      this.#openVersion++
      this.openThread = { ...this.openThread!, thread: { ...openBefore, ...patch } }
      // Told it is unread, or told it is read: either way it is decided.
      if ('unreadCount' in patch) this.#unseenIn = null
    }
    try {
      await call([...ids])
      this.#scheduleUnreadRefresh()
    } catch (e) {
      for (const id of lingered) this.#lingering.delete(id)
      for (const id of released) this.#lingering.add(id)
      for (const before of befores) this.threads = revertRow(this.threads, before.id, before)
      for (const before of searchBefores) {
        this.searchResults = revertRow(this.searchResults, before.id, before)
      }
      for (const before of followBefores) {
        this.#followUps = revertRow(this.#followUps, before.id, before)
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
  ): Promise<boolean> {
    if (ids.length === 0) return false
    this.#forgetDetails(ids)
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
    // Archived, trashed or snoozed, a thread is done with: off the top too.
    const followRemoved: { row: Thread; index: number }[] = []
    for (const id of ids) {
      const out = removeRow(this.threads, id)
      this.threads = out.rows
      if (out.removed) removed.push(out.removed)
      const searchOut = removeRow(this.searchResults, id)
      this.searchResults = searchOut.rows
      if (searchOut.removed) searchRemoved.push(searchOut.removed)
      const followOut = removeRow(this.#followUps, id)
      if (followOut.removed) {
        this.#followUps = followOut.rows
        followRemoved.push(followOut.removed)
      }
    }
    try {
      await call([...ids])
      this.#scheduleUnreadRefresh()
      return true
    } catch (e) {
      for (const r of removed.reverse()) this.threads = restoreRow(this.threads, r)
      for (const r of searchRemoved.reverse()) {
        this.searchResults = restoreRow(this.searchResults, r)
      }
      for (const r of followRemoved.reverse()) {
        this.#followUps = restoreRow(this.#followUps, r)
      }
      await handle(e)
      return false
    }
  }

  /**
   * Where each of these threads is right now, as the mailbox it would go
   * back to: the one on screen when that is a real folder, otherwise (an
   * "All accounts" row, a search) that thread's own account's folder of the
   * same kind, falling back to its Inbox. Read *before* the rows leave the
   * list, because afterwards nothing says which account a thread was in --
   * and a unified list spans several, so one "where it came from" would put
   * half of them in the wrong account.
   */
  #originsOf(ids: readonly ThreadId[]): Map<MailboxId, ThreadId[]> {
    const here = this.mailbox
    // A real folder has no `pseudo`; the synthetic Snoozed row is an id the
    // backend never saw, so it is treated like the list of several.
    const real = here && !here.pseudo && !here.id.startsWith('synthetic-snoozed:')
    const gathered = listTarget(here, this.mailboxes)?.mailboxIds[0]
    const role = this.mailboxes.find((m) => m.id === gathered)?.role ?? 'inbox'
    const origins = new Map<MailboxId, ThreadId[]>()
    // A search reaches every folder, and a result does not say which one it
    // came from: guessing by the role on screen would send an archived Inbox
    // thread "back" to Sent. Over a search there is no guess, so no Undo.
    const searching = this.searchQuery.trim() !== ''
    for (const id of ids) {
      let origin: MailboxId | undefined = real && !searching ? here.id : undefined
      if (!origin && !searching) {
        const row =
          this.threads.find((t) => t.id === id) ??
          this.searchResults.find((t) => t.id === id) ??
          this.#followUps.find((t) => t.id === id)
        const account =
          row?.accountId ??
          (this.openThread?.thread.id === id ? this.openThread.thread.accountId : undefined)
        origin = this.mailboxes.find(
          (m) => m.accountId === account && !m.pseudo && m.role === role,
        )?.id
      }
      if (origin) origins.set(origin, [...(origins.get(origin) ?? []), id])
    }
    return origins
  }

  /** Archive or trash with a way back: the toast's Undo moves each thread to
   *  the mailbox it was in, then reads the list again so it reappears. */
  async #removeWithUndo(
    ids: readonly ThreadId[],
    call: (ids: ThreadId[]) => Promise<unknown>,
    verb: 'Archived' | 'Moved to Trash' | 'Snoozed',
    key: string,
    back: (origins: Map<MailboxId, ThreadId[]>) => Promise<unknown>,
    /** Whether Undo needs to know where each thread was: a move does, an
     *  unsnooze does not. */
    needsOrigin = true,
  ): Promise<void> {
    const origins = this.#originsOf(ids)
    const count = new Set(ids).size
    if (!(await this.#remove(ids, call))) return
    const what = plural(count, 'conversation')
    notify.info(verb === 'Moved to Trash' ? `${what} moved to Trash` : `${verb} ${what}`, {
      key,
      // Nothing to put back where it came from, nothing to offer.
      ...(needsOrigin && origins.size === 0
        ? {}
        : {
            action: {
              label: 'Undo',
              run: () => {
                void back(origins)
                  .then(() => this.refresh())
                  .catch((e) => handle(e))
              },
            },
          }),
    })
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
    this.#forgetDetails()
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
            this.#followUps = applyRowPatch(this.#followUps, thread, { unreadCount: 0 }).rows
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
    // Said of the whole mailbox on screen, so there is nothing left to linger
    // over. Any other mailbox's being read changes nothing about this list.
    if (id === this.selectedMailbox && this.only.has('unread')) {
      this.#holdOnlyTheOpenThread()
      void this.refresh()
    }
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
    return this.#removeWithUndo(ids, mailApi.archiveMany, 'Archived', 'mail-archive', (o) =>
      this.#moveBack(o),
    )
  }
  /** Undo of an archive or trash: each group back to its own mailbox. */
  #moveBack(origins: Map<MailboxId, ThreadId[]>): Promise<unknown> {
    return Promise.all([...origins].map(([box, group]) => mailApi.moveManyToMailbox(group, box)))
  }
  trash(id: ThreadId) {
    return this.trashMany([id])
  }
  trashMany(ids: readonly ThreadId[]) {
    return this.#removeWithUndo(ids, mailApi.trashMany, 'Moved to Trash', 'mail-trash', (o) =>
      this.#moveBack(o),
    )
  }

  /** Trash or Spam, when that is the list on screen -- see `emptiableRole`.
   *  `null` over a search, whose results come from anywhere. */
  get emptiableRole(): 'trash' | 'spam' | null {
    if (this.sendsView !== null || this.searchQuery.trim()) return null
    return emptiableRole(this.mailbox)
  }

  /** Delete, Backspace and `#`: to Trash -- or, in Trash itself, where that
   *  would change nothing, ask whether to delete them for good. */
  deleteTargets() {
    const ids = this.targets
    const box = this.mailbox
    if (ids.length === 0) return
    if (box && this.emptiableRole === 'trash') this.wantsForever = { ids, mailbox: box }
    else void this.trashMany(ids)
  }

  /** Delete `ids` for good from `box`, a Trash or Spam -- every mailbox an
   *  "All accounts" row gathers, so each thread goes from whichever of them
   *  it is in. Gone from the list at once, like a trash; asked about first,
   *  by the caller. */
  deleteForeverMany(ids: readonly ThreadId[], box: Mailbox) {
    const from = emptiableRole(box) ? listTarget(box, this.mailboxes)?.mailboxIds : undefined
    if (!from) return Promise.resolve()
    return this.#remove(ids, (t) => mailApi.deleteForeverMany(t, from))
  }

  /**
   * Empty `box`, a Trash or Spam, or each account's at once from an "All
   * accounts" row. Answers how many conversations went, or `null` when it
   * failed and has said so.
   *
   * The list on screen empties at once when everything it gathers is being
   * emptied -- the mailbox itself, or one account's under the "All accounts"
   * row being emptied. When only some of it is (one account's Trash, under
   * that row on screen), which rows go is the backend's to say, so the list
   * is asked for again once it has.
   */
  async emptyMailbox(box: Mailbox): Promise<number | null> {
    const target = emptiableRole(box) ? listTarget(box, this.mailboxes) : null
    if (!target) return null
    const emptied = new Set(target.mailboxIds)
    this.#forgetDetails()
    const shown = this.emptiableRole ? listTarget(this.mailbox, this.mailboxes) : null
    const all = !!shown && shown.mailboxIds.every((id) => emptied.has(id))
    const some = !!shown && !all && shown.mailboxIds.some((id) => emptied.has(id))
    if (all && this.threads.length > 0) {
      this.#advanceIfOpen(new Set(this.threads.map((t) => t.id)))
      this.checked = new Set()
      this.threads = []
    }
    let count = 0
    try {
      for (const id of target.mailboxIds) count += await mailApi.emptyMailbox(id)
      if (some) void this.refresh()
      return count
    } catch (e) {
      // Some of it may have gone before the failure: the list is asked for
      // again rather than put back as it was.
      if (all || some) void this.refresh()
      await handle(e)
      return null
    } finally {
      this.#scheduleUnreadRefresh()
    }
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
    this.#forgetDetails(unique)
    const gone = new Set(unique)
    this.#advanceIfOpen(gone)
    if ([...this.checked].some((id) => gone.has(id))) {
      this.checked = new Set([...this.checked].filter((id) => !gone.has(id)))
    }
    const removedFrom = (rows: readonly Thread[]) =>
      rows.flatMap((row, index) => (gone.has(row.id) ? [{ row, index }] : []))
    const removed = removedFrom(this.threads)
    const searchRemoved = removedFrom(this.searchResults)
    const followRemoved = removedFrom(this.#followUps)
    this.threads = this.threads.filter((t) => !gone.has(t.id))
    this.searchResults = this.searchResults.filter((t) => !gone.has(t.id))
    this.#followUps = this.#followUps.filter((t) => !gone.has(t.id))
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
      this.#followUps = this.#followUps.filter((t) => !gone.has(t.id))
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
      this.#followUps = putBack(this.#followUps, followRemoved)
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
    return this.#removeWithUndo(
      ids,
      (t) => mailApi.snoozeMany(t, until.toISOString()),
      'Snoozed',
      'mail-snooze',
      () => mailApi.unsnoozeMany([...ids]),
      false,
    )
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
    return this.moveToCategory(ids, category)
  }

  /**
   * File these threads under `category` -- a standing rule for each one's
   * sender, as `set_thread_category` always is, so their other mail and
   * their new mail go there too. Answers whether it went.
   *
   * In a tab that is not `category`, the rows leave the list at once, the
   * way an archive takes them out of the Inbox, and come back where they
   * were if it fails: patched in place, a thread moved out of Newsletters
   * went on being listed under Newsletters until the next reload. Anywhere
   * else -- All, a search, a mailbox with no tabs -- the row stays and says
   * where it went. The tabs' counts are read again either way.
   */
  async moveToCategory(ids: readonly ThreadId[], category: MailCategory): Promise<boolean> {
    const unique = [...new Set(ids)]
    if (unique.length === 0) return false
    const tab = mailboxHasTabs(this.mailbox) && !this.searchQuery.trim() ? this.category : null
    let ok = false
    const call = async (t: ThreadId[]) => {
      await mailApi.setThreadCategory(t, category)
      ok = true
    }
    if (tab !== null && tab !== category) await this.#remove(unique, call)
    else await this.#act(unique, { category }, call)
    void this.refreshCategoryCounts()
    return ok
  }

  /**
   * Ask the senders of these threads to take the person off their lists --
   * only `senders` among them when given, the one an offer named -- and with
   * `archive`, archive the threads afterwards, with archiving's own Undo.
   * One outcome per sender, for the caller to say how it went and to open
   * any page left to finish; empty, having said why, when the ask itself
   * failed, in which case nothing is archived either.
   *
   * The open thread's offer is asked for again when it was among them, so
   * its button turns to "Unsubscribed".
   */
  async unsubscribeThreads(
    ids: readonly ThreadId[],
    opts: { archive?: boolean; senders?: string[] } = {},
  ): Promise<UnsubscribeOutcome[]> {
    const unique = [...new Set(ids)]
    if (unique.length === 0) return []
    let outcomes: UnsubscribeOutcome[]
    try {
      outcomes = await mailApi.unsubscribe(unique, opts.senders ?? null)
    } catch (e) {
      await handle(e)
      return []
    }
    if (opts.archive) {
      await this.archiveMany(unique)
    } else {
      const open = this.openThread?.thread.id
      if (open && unique.includes(open)) void this.#loadOffer(open)
    }
    return outcomes
  }

  /**
   * `!`: flag or unflag this one thread as priority -- never the standing
   * per-sender rule `setCategoryFor(id, 'priority')` is. Patches the same
   * `category` field `setCategoryFor` does, in place even in a tab: a flag
   * is a mark on the row, where a sender's rule files their mail away.
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
      this.#openVersion++
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
    this.composeKey = draft.id
    this.composeInline = inline
  }

  /** The draft being written now goes out from another account, as a new
   *  record (`move_draft`): the same sheet carries on with it, so
   *  `composeKey` stays as it was. */
  composingMoved(draft: Draft) {
    if (this.composing) this.composing = draft
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
    const wasInline = this.composeInline
    this.composing = null
    this.composeInline = false
    // A reply put down is a draft card in its thread now, and one sent or
    // thrown away is not. This window's own writes never come back as change
    // events, so the thread is read again here. The close button has waited
    // for the last save already.
    if (wasInline && this.openThread) void this.#reloadOpenThread()
  }

  /**
   * Carry on writing a draft from its card in the thread. One this app keeps
   * opens as it is; another device's, or old copies nobody claims, becomes
   * one of this app's first (`editThreadDraft`), and its older copies go.
   */
  async editThreadDraft(card: ThreadDraft) {
    try {
      const draft = card.draft ?? (await mailApi.editThreadDraft(card.copies.map((c) => c.id)))
      this.openInlineDraft(draft)
      if (!card.draft) void this.#reloadOpenThread()
    } catch (e) {
      await handle(e)
    }
  }

  /**
   * Delete a draft from its card: the draft and every copy of it on the
   * server, or only `copies` -- old autosaves -- leaving the draft itself.
   * Gone from the thread at once; read again if the backend refused.
   */
  async deleteThreadDraft(card: ThreadDraft, copies: MailMessageDetail[] = card.copies) {
    const draft = copies === card.copies ? card.draft : null
    const open = this.openThread
    if (draft && this.composing?.id === draft.id) this.closeCompose()
    if (open) {
      const gone = new Set(copies.map((c) => c.id))
      const drafts = open.drafts.flatMap((d) => {
        if (d !== card) return [d]
        if (draft) return []
        const left = d.copies.filter((c) => !gone.has(c.id))
        return d.draft || left.length > 0 ? [{ ...d, copies: left }] : []
      })
      this.#openVersion++
      this.openThread = { ...open, drafts }
    }
    try {
      await mailApi.deleteThreadDraft(
        draft?.id ?? null,
        copies.map((c) => c.id),
      )
      if (draft || !card.draft) notify.info('Draft deleted', { key: 'mail-draft-deleted' })
    } catch (e) {
      await handle(e)
      void this.#reloadOpenThread()
    }
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

  /** Drafts sent from this window, whose outcome -- sent, or into the
   *  Outbox -- is still to come as a change. */
  #sent = new Set<DraftId>()

  async send(draft: Draft, delaySeconds?: number, sendAt?: string) {
    try {
      await mailApi.sendDraft(draft.id, delaySeconds, sendAt)
      this.#sent.add(draft.id)
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
    // A reply shows in its thread from now, as it is going (`sending`), not
    // only once the server's copy has synced. The sheet closed before the
    // send was asked for, so the read it did then still had it as a draft.
    if (this.openThread) void this.#reloadOpenThread()
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
      // Being written again, not on its way: out of the thread's `sending`.
      if (this.openThread) void this.#reloadOpenThread()
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
      const scheduled = await mailApi.scheduledSends()
      if (!sameJson(scheduled, $state.snapshot(this.scheduled))) this.scheduled = scheduled
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

  // ── the Outbox ───────────────────────────────────────────────────

  async refreshOutbox() {
    try {
      const outbox = await mailApi.outbox()
      if (!sameJson(outbox, $state.snapshot(this.outbox))) this.outbox = outbox
    } catch (e) {
      await quietly(e)
    }
  }

  /** Open an Outbox message to look at or change. One being retried stops
   *  first -- undo is refused once a send has been tried, since it may have
   *  gone out (`stop_send` in `mail.rs`). */
  async editOutbox(item: OutboxItem) {
    try {
      const draft = item.nextAttempt ? await mailApi.stopSend(item.draft.id) : item.draft
      this.#show(draft, false)
      void this.refreshOutbox()
    } catch (e) {
      await handle(e)
    }
  }

  /** Send it again now, through the ordinary short undo window. One still
   *  being retried stops waiting for its next attempt; the draft keeps its
   *  Message-ID, so the outbox first asks whether an earlier try arrived. */
  async retryOutbox(item: OutboxItem) {
    try {
      const draft = item.nextAttempt ? await mailApi.stopSend(item.draft.id) : item.draft
      await this.send(draft, 5)
      void this.refreshOutbox()
    } catch (e) {
      await handle(e)
    }
  }

  /** Throw it away. `discard_draft` cancels a send waiting to be retried
   *  itself, so there is nothing to stop first. */
  async discardOutbox(item: OutboxItem) {
    try {
      await mailApi.discardDraft(item.draft.id)
      this.outbox = this.outbox.filter((i) => i.draft.id !== item.draft.id)
      notify.info('Message discarded', { key: 'mail-outbox-discarded' })
    } catch (e) {
      await handle(e)
      void this.refreshOutbox()
    }
  }

  // ── search ───────────────────────────────────────────────────────

  setSearchQuery(q: string) {
    this.searchQuery = q
    this.searchCursor = null
    this.clearChecked()
    if (!q.trim()) {
      // Cleared on purpose: a query typed a moment ago must not land after.
      this.#search.cancel()
      this.searchResults = []
      this.searching = false
      return
    }
    this.searching = true
    this.#search.call(q)
  }

  /** The search box's debounce: a query per pause in typing, not per key,
   *  the way the notes app's search does it. */
  #search = debounce((q: string) => void this.#runSearch(q), 160)

  async #runSearch(q: string) {
    try {
      const result = await mailApi.searchMail(q)
      if (this.searchQuery === q) {
        this.searchResults = result.threads
        this.searchPlaces = result.places ?? {}
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
        this.searchPlaces = { ...this.searchPlaces, ...(result.places ?? {}) }
        this.searchCursor = result.next ?? null
      }
    } catch (e) {
      await quietly(e)
    } finally {
      if (this.searchQuery === q) this.searching = false
    }
  }

  clearSearch() {
    this.#search.cancel()
    this.searchQuery = ''
    this.searchResults = []
    this.searchCursor = null
    this.searching = false
  }

  // ── live-apply ───────────────────────────────────────────────────

  #applyChanges(changes: ChangeWithIds[]): boolean {
    // Nothing read yet, so nothing to bring up to date: `start` reads it all.
    if (!this.#loaded) return true
    // A background sync pass that changed something says so with one thread
    // change naming no thread at all -- it may have touched hundreds, in any
    // mailbox. Declining it would send `live` to `RELOAD.mail`, which reloads
    // the list alone: new mail would appear while the sidebar's unread
    // counts and the "last synced" line went on describing the pass before.
    // So the list, the counts and the status are all asked again -- one
    // round for a burst of passes, and none while nobody can see Mail.
    if (changes.some((c) => c.kind === 'thread' && !c.id && !c.ids?.length)) {
      this.#forgetDetails()
      // A pass is also when sends sent elsewhere, or before this window
      // opened, have been tried.
      this.#scheduleRound(WHOLE_ROUND)
      // Not held back while Mail is out of sight: what arrived in the open
      // thread is there, unread, for when they come back, and the status
      // line is one cheap read.
      this.#scheduleOpenThreadReload()
      void this.refreshSyncStatus()
      return true
    }
    const applied = applySingleChange(changes, {
      // Nothing in the visible thread list or the sync-status line reads a
      // draft directly today -- the compose sheet owns its own working copy
      // and autosaves it, so another window's edit to the *same* draft is
      // not something this window should clobber mid-keystroke. Handled as
      // "nothing to do" rather than falling through to `RELOAD.mail`, which
      // would otherwise reload the thread list for every autosave tick.
      skip: (change) => {
        if (change.kind !== 'draft') return false
        // A thread read ahead may have this draft's card, or lack it.
        this.#forgetDetails()
        // A draft put down, sent or thrown away elsewhere changes the open
        // thread's draft cards. Not one still being written here: that is
        // every autosave.
        const ids = change.ids?.length ? change.ids : change.id ? [change.id] : []
        if (this.openThread && !ids.some((id) => id === this.composing?.id)) {
          this.#scheduleOpenThreadReload()
        }
        if (this.scheduled.some((s) => ids.includes(s.draft.id))) void this.refreshScheduled()
        // A send failed, failed again, or went -- one sent from here, or one
        // already in the Outbox. Not every autosave, of which there is one
        // every few seconds while somebody types.
        if (ids.some((id) => this.#sent.has(id) || this.outbox.some((i) => i.draft.id === id))) {
          this.#scheduleOutboxRefresh()
        }
        return true
      },
      onDeleted: (id) => {
        this.#forgetDetails([id])
        // Finding 5: a thread deleted from another window or by the
        // assistant must not stay open here just because nothing removed it
        // from `openThread` -- `#advanceIfOpen` does what archiving already
        // does, moving on to the neighbour, before the row disappears from
        // under it.
        this.#advanceIfOpen(new Set([id]))
        this.threads = this.threads.filter((t) => t.id !== id)
        this.searchResults = this.searchResults.filter((t) => t.id !== id)
        this.#followUps = this.#followUps.filter((t) => t.id !== id)
      },
      onUpserted: (id) => {
        this.#forgetDetails([id])
        // Out of sight, only the open thread is read again, as for a pass;
        // the list and its counts wait to be seen.
        if (!this.#isLive()) {
          if (this.openThread?.thread.id === id) this.#scheduleOpenThreadReload()
          this.#stale = joinRounds(this.#stale, { list: true, followUps: true, counts: true })
          return
        }
        void this.#patchOne(id)
        // Bug 4: every backend command that can move a thread out of a
        // mailbox from elsewhere -- archive, trash, snooze, an assistant or
        // MCP action, another window's own click -- declares `Thread`/
        // `Updated`, never `Deleted`, and `Thread` carries no mailbox id for
        // `#patchOne` above to notice the row has left. Patching it in place
        // is right as far as it goes -- the subject, the snippet, the flags
        // are current -- but the row stays in a list it may no longer belong
        // to until something re-asks the backend which threads are actually
        // still here. A round is that ask: harmless if the row still belongs
        // (the page comes back the same), and what actually drops it -- or
        // brings in one that has just moved here -- if it does not, same as
        // opening the mailbox fresh would. `refresh()`'s own generation guard
        // still applies, so a slow one cannot land after a newer list has
        // replaced it. The badges come along: it may have been read.
        //
        // The real fix is a mailbox-scoped change event from the backend --
        // out of scope for this pass.
        this.#scheduleRound({ list: true, followUps: true, counts: true })
      },
    })
    if (applied) return true
    // Wider than one record -- several threads at once, or a thread and a
    // draft together. `live` reloads the list for that while Mail can be
    // seen; while it cannot, it waits to be.
    this.#forgetDetails()
    if (this.#isLive()) return false
    this.#stale = joinRounds(this.#stale, { list: true, followUps: true, counts: true })
    return true
  }

  /** What the next live round asks again, and what was asked for while
   *  nobody could see Mail -- see `#scheduleRound`. */
  #pending: Round = { ...NO_ROUND }
  #stale: Round = { ...NO_ROUND }
  #roundTimer: ReturnType<typeof setTimeout> | null = null
  /** When the last round began, by `performance.now()`: a clock that never
   *  steps back, as the wall clock does when it is corrected after a resume.
   *  Timed off `Date.now()`, a clock set back an hour held the next round,
   *  and every change after it, for that hour. */
  #roundAt = -Infinity

  /**
   * A sync pass met a folder or label for the first time -- the service's
   * `mailbox` change, which `live.svelte.ts` routes here. The mailboxes are
   * read again, with their counts, in the next round while Mail can be seen,
   * and once it can otherwise. Nothing before Mail's first visit: `start`
   * reads them all.
   */
  mailboxesChanged() {
    if (!this.#loaded) return
    this.#scheduleRound({ mailboxes: true })
  }

  /**
   * Ask again what a change made elsewhere may have moved: at once when no
   * round has run for `LIVE_REFRESH_DEBOUNCE_MS`, otherwise once that much
   * has passed, with everything asked for meanwhile. So new mail is on the
   * list the moment its change arrives, and a burst of changes -- one mail
   * often brings two or three, the pass, its sorting, its follow-up -- costs
   * two rounds rather than one each. While nobody can see Mail, only noted,
   * and asked for the moment somebody can (`setOnScreen`, `windowShown`):
   * nothing outside Mail draws any of it.
   */
  #scheduleRound(want: Partial<Round>) {
    if (!this.#isLive()) {
      this.#stale = joinRounds(this.#stale, want)
      return
    }
    this.#pending = joinRounds(this.#pending, want)
    if (this.#roundTimer) return
    const wait = this.#roundAt + LIVE_REFRESH_DEBOUNCE_MS - performance.now()
    if (wait <= 0) {
      this.#runRound()
      return
    }
    this.#roundTimer = setTimeout(() => {
      this.#roundTimer = null
      if (this.#isLive()) {
        this.#runRound()
        return
      }
      this.#stale = joinRounds(this.#stale, this.#pending)
      this.#pending = { ...NO_ROUND }
    }, wait)
  }

  #runRound() {
    const want = this.#pending
    this.#pending = { ...NO_ROUND }
    if (!anyRound(want)) return
    this.#roundAt = performance.now()
    // The list first: it is where new mail is looked for.
    if (want.list) void this.refresh({ followUps: want.followUps })
    else if (want.followUps) void this.refreshFollowUps()
    // A read of the mailboxes counts them and asks for the Outbox itself.
    if (want.mailboxes) {
      void this.refreshMailboxes()
      return
    }
    if (want.counts) {
      void this.refreshUnreadCounts()
      void this.refreshCategoryCounts()
    }
    if (want.outbox) void this.refreshOutbox()
  }

  /** Everything that waited while nobody could see Mail, at once. */
  #catchUp() {
    if (!anyRound(this.#stale)) return
    this.#pending = joinRounds(this.#pending, this.#stale)
    this.#stale = { ...NO_ROUND }
    if (this.#roundTimer) clearTimeout(this.#roundTimer)
    this.#roundTimer = null
    this.#runRound()
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
   *
   * Only for a thread drawn here -- a row, a follow-up, the open thread. One
   * that is not, a new thread or one that has just moved into this mailbox,
   * is the round `#applyChanges` asks for to place: where it belongs depends
   * on the mailbox, the category and the sort, which only the backend knows.
   */
  async #patchOne(id: ThreadId) {
    const open = this.openThread?.thread.id === id
    const drawn =
      open || this.threads.some((t) => t.id === id) || this.#followUps.some((t) => t.id === id)
    if (!drawn) return
    const version = open ? ++this.#openVersion : this.#openVersion
    try {
      const detail = await mailApi.getThread(id)
      // At the top as well, or instead: answered meanwhile, its follow-up is
      // gone from the fresh copy, and so is its row there.
      if (this.#followUps.some((t) => t.id === id)) {
        this.#followUps = this.#followUps.map((t) => (t.id === id ? detail.thread : t))
      }
      const at = this.threads.findIndex((t) => t.id === id)
      if (at >= 0 && !sameJson(this.threads[at], detail.thread)) {
        this.threads = this.threads.map((t, i) => (i === at ? detail.thread : t))
      }
      if (this.openThread?.thread.id === id && version === this.#openVersion) {
        this.#showNewer(detail)
      }
    } catch {
      await this.refresh()
    }
  }

  #openReloadTimer: ReturnType<typeof setTimeout> | null = null
  #scheduleOpenThreadReload() {
    if (this.#openReloadTimer) clearTimeout(this.#openReloadTimer)
    this.#openReloadTimer = setTimeout(() => {
      this.#openReloadTimer = null
      void this.#reloadOpenThread()
    }, LIVE_REFRESH_DEBOUNCE_MS)
  }

  #outboxRefreshTimer: ReturnType<typeof setTimeout> | null = null
  #scheduleOutboxRefresh() {
    // Only Mail's own sidebar and list draw the Outbox.
    if (!this.#isLive()) {
      this.#stale = joinRounds(this.#stale, { outbox: true })
      return
    }
    if (this.#outboxRefreshTimer) clearTimeout(this.#outboxRefreshTimer)
    this.#outboxRefreshTimer = setTimeout(() => {
      this.#outboxRefreshTimer = null
      void this.refreshOutbox()
    }, LIVE_REFRESH_DEBOUNCE_MS)
  }

  /** Bumped by every read of the open thread and every change made to it
   *  here, so an answer overtaken by either is not drawn over it. */
  #openVersion = 0

  /**
   * A sync pass names no thread, and may have put a message in the one being
   * read -- the reply just sent, most of all, which only ever arrives this
   * way. The list has heard already; without this the pane went on showing
   * the thread as it was until somebody left it and came back.
   *
   * Only the thread in the pane. After an archive the cursor rests on the
   * next row with the pane closed, and reading that row here would open it
   * by itself a moment later, unmarked and folded.
   */
  async #reloadOpenThread() {
    const id = this.openThread?.thread.id
    if (!id || id !== this.selectedThread) return
    const version = ++this.#openVersion
    try {
      const detail = await mailApi.getThread(id)
      if (this.selectedThread !== id) return
      // Overtaken: read again, or a message this read saw waits for a pass
      // that may be a long time coming.
      if (version !== this.#openVersion) this.#scheduleOpenThreadReload()
      else this.#showNewer(detail)
    } catch (e) {
      // Deleted -- expunged on the server, say. The list closes a thread it
      // drew once it has left (`refresh`), but not one it never had, opened
      // from a search or another app: this is what closes that. Not under a
      // reply being written, which is the person's to send or put down.
      if (e instanceof VaultError && e.code === 'not_found') {
        if (this.selectedThread === id && !this.composeInline) this.closeThread()
        return
      }
      await quietly(e)
    }
  }

  /**
   * A fresh copy of the open thread. What somebody has opened and closed stays
   * as it is, and what has arrived since opens, as the newest message does when
   * a thread is opened. Drafts are never among `messages`: each autosave's copy
   * is in `drafts`, drawn as the one card it is. Nothing is opened by it: a
   * copy of a thread no longer in the pane is dropped.
   */
  #showNewer(detail: ThreadDetail) {
    const had = this.openThread?.thread.id === detail.thread.id ? this.openThread : null
    if (!had) return
    // Read again and found as it was -- a pass that brought other threads
    // mail, a thread read ahead checked behind itself -- nothing is redrawn.
    if (sameJson(detail, $state.snapshot(had))) return
    const known = new Set(had.messages.map((m) => m.id))
    const messages = detail.messages
    this.openThread = detail
    const arrived = messages.filter((m) => !known.has(m.id))
    if (arrived.length === 0) return
    // The offer is read off the newest message from the sender, which may
    // be the one that just came.
    void this.#loadOffer(detail.thread.id)
    this.expanded = new Set([...this.expanded, ...arrived.map((m) => m.id)])
    if (detail.thread.unreadCount > 0) {
      this.#unseenIn = detail.thread.id
      this.seeOpenThread()
    }
  }

  /** The open thread, while something arrived in it that nobody has seen. */
  #unseenIn: ThreadId | null = null
  /** Is `MailView` drawn? Not the same as `app.section`, which Settings leaves
   *  as it was while standing in its place. */
  #onScreen = false

  /** Can anybody see Mail: `MailView` drawn, in a window that is not hidden
   *  -- minimised, in the tray, or behind another desktop. */
  #isLive(): boolean {
    return (
      this.#onScreen && (typeof document === 'undefined' || document.visibilityState !== 'hidden')
    )
  }

  /** `MailView` coming and going. Coming back, the badges are counted again
   *  whatever was heard meanwhile: this window's own writes from outside
   *  Mail -- an archive from the Overview's inbox card -- never come back to
   *  it as changes, and every visit used to recount them. One call. */
  setOnScreen(onScreen: boolean) {
    this.#onScreen = onScreen
    if (!onScreen) return
    this.#stale = joinRounds(this.#stale, { counts: true })
    if (this.#isLive()) this.#catchUp()
    this.seeOpenThread()
  }

  /** The window shown, hidden or brought to the front, as `MailView` hears
   *  it: what waited for it is asked for now, and what arrived in the open
   *  thread may be read. Focus as well as visibility, in case a webview
   *  coming back from the tray says nothing of the one. */
  windowShown() {
    if (this.#isLive()) this.#catchUp()
    this.seeOpenThread()
  }

  /**
   * Mark read what arrived in the open thread, once somebody can see it: Mail
   * on screen, in a window in front. Until then it is still news to them --
   * and they get here by clicking its banner, so `MailView` asks again as the
   * window comes back.
   */
  seeOpenThread() {
    const id = this.#unseenIn
    if (!id || !this.#onScreen || !windowInFront()) return
    this.#unseenIn = null
    if (id === this.selectedThread) void this.markRead(id)
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
    // A list of sends is mail on its way out, not a mailbox: the account it
    // belongs to is what narrows it.
    const sends = this.sendsView
    if (sends !== null) {
      return {
        view:
          sends.list === 'scheduled'
            ? 'the Scheduled list, mail of theirs queued to send later'
            : 'the Outbox, mail of theirs the server would not take: failing and being tried again, or failed and waiting for them',
        within:
          sends.account === null
            ? []
            : seen('account', sends.account, accounts.account(sends.account)?.address ?? ''),
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
    // So "archive everything here" means what is on screen, not the mailbox.
    const narrowed =
      this.only.size > 0 && !this.searchQuery.trim()
        ? `showing only ${onlyPhrase(this.only)}`
        : null
    return {
      view: [view, narrowed].filter(Boolean).join(', ') || null,
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
