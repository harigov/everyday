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
import { registerApply, singleId, type ChangeWithIds } from './live-apply'
import {
  applyInviteResponse,
  applyRowPatch,
  isSnoozedMailbox,
  mailboxHasTabs,
  mergeSearchPage,
  neighbourThread,
  refreshLimit,
  removeRow,
  restoreRow,
  revertRow,
  snoozeChoices,
  stepCategoryTab,
  visibleThreadList,
} from './mail'
import { app, handle, quietly } from './state.svelte'
import type {
  AccountId,
  Draft,
  Mailbox,
  MailboxId,
  MailCategory,
  MailMessageId,
  MailSyncProgress,
  Thread,
  ThreadDetail,
  ThreadFilter,
  ThreadId,
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

/** `null` selects every account this vault has. */
export type AccountScope = AccountId | null

class MailState {
  // ── what is on screen ─────────────────────────────────────────────

  selectedAccount = $state<AccountScope>(null)
  selectedMailbox = $state<MailboxId | null>(null)
  category = $state<MailCategory | null>(null)
  selectedThread = $state<ThreadId | null>(null)
  /** Which messages in the open thread are expanded, newest first by default. */
  expanded = $state<Set<string>>(new Set())
  composing = $state<Draft | null>(null)
  /** The thread the snooze picker is open for, or `null`. A store field
   *  rather than component state -- the same reason `overview.wantsLog` is
   *  -- so the `h` shortcut can open it from `shortcuts.svelte.ts`, which
   *  has no component of its own to reach into. */
  wantsSnooze = $state<ThreadId | null>(null)
  /** The thread the label picker is open for -- the proper picker `l` opens
   *  in place of a `window.prompt`. */
  wantsLabel = $state<ThreadId | null>(null)
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
  syncStatus = $state<MailSyncProgress[]>([])

  /** Which load is current, so a slow one cannot land after a newer one. */
  #generation = 0
  #loaded = false

  constructor() {
    app.onLock(() => this.reset())
    registerApply('mail', (changes) => this.#applyChanges(changes))
  }

  reset() {
    this.#generation += 1
    this.selectedAccount = null
    this.selectedMailbox = null
    this.category = null
    this.selectedThread = null
    this.expanded = new Set()
    this.composing = null
    this.wantsSnooze = null
    this.wantsLabel = null
    this.summary = null
    if (this.#undoTimer) clearInterval(this.#undoTimer)
    this.#undoTimer = null
    this.sendingUndo = null
    if (this.#unreadRefreshTimer) clearTimeout(this.#unreadRefreshTimer)
    this.#unreadRefreshTimer = null
    if (this.#liveRefreshTimer) clearTimeout(this.#liveRefreshTimer)
    this.#liveRefreshTimer = null
    this.mailboxes = []
    this.threads = []
    this.nextCursor = null
    this.openThread = null
    this.unreadCounts = new Map()
    this.syncStatus = []
    this.clearSearch()
    this.#loaded = false
  }

  /** Called by `MailNav` on mount. Idempotent, like every other app's `start`. */
  async start() {
    if (!app.supportsMail || this.#loaded) return
    this.#loaded = true
    await this.refreshMailboxes()
    await this.refreshSyncStatus()
  }

  async refreshMailboxes() {
    try {
      const accountIds = await this.#accountIds()
      const lists = await Promise.all(accountIds.map((id) => mailApi.listMailboxes(id)))
      this.mailboxes = lists.flat()
      if (!this.selectedMailbox) {
        const inbox = this.mailboxes.find((m) => m.role === 'inbox')
        if (inbox) await this.selectMailbox(inbox.id)
      }
      void this.refreshUnreadCounts()
    } catch (e) {
      await handle(e)
    }
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
   *  badge is exactly the reverse -- only threads that are. */
  async refreshUnreadCounts() {
    try {
      const counts = new Map<MailboxId, number>()
      await Promise.all(
        this.mailboxes.map(async (mailbox) => {
          const filter: ThreadFilter = { snoozed: isSnoozedMailbox(mailbox) }
          const page = await mailApi.listThreads(mailbox.id, filter, null, 200)
          counts.set(
            mailbox.id,
            page.threads.reduce((sum, t) => sum + t.unreadCount, 0),
          )
        }),
      )
      this.unreadCounts = counts
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

  /** The account and mailbox picked in the nav. */
  async selectMailbox(id: MailboxId) {
    this.selectedMailbox = id
    this.selectedThread = null
    this.openThread = null
    // Finding 1: only the inbox has tabs to have set this from, so a
    // category chosen there must not go on filtering a mailbox with no tab
    // strip to clear it from.
    if (!mailboxHasTabs(this.mailboxes.find((m) => m.id === id))) this.category = null
    await this.refresh()
  }

  setCategory(category: MailCategory | null) {
    this.category = category
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
    if (!this.selectedMailbox) return
    const generation = ++this.#generation
    this.loading = true
    try {
      // Finding 1: a category filter only means anything for the inbox's
      // own tabs -- sent elsewhere, it silently narrowed a mailbox with no
      // tab strip to have set it from.
      const category = mailboxHasTabs(this.mailbox) ? this.category : null
      // Bug 3: `snoozed` was never sent at all, so a snoozed thread -- still
      // a member of whatever mailbox it was snoozed from -- came straight
      // back on the very next refresh. `false` everywhere except the
      // Snoozed pseudo-mailbox itself, which wants nothing else.
      const filter: ThreadFilter = { snoozed: isSnoozedMailbox(this.mailbox) }
      if (category) filter.category = category
      const page = await mailApi.listThreads(
        this.selectedMailbox,
        filter,
        null,
        refreshLimit(this.threads.length, PAGE),
      )
      if (generation !== this.#generation) return
      this.threads = page.threads
      this.nextCursor = page.nextCursor ?? null
      if (this.selectedThread && !this.threads.some((t) => t.id === this.selectedThread)) {
        this.selectedThread = null
        this.openThread = null
      }
    } catch (e) {
      await handle(e)
    } finally {
      if (generation === this.#generation) this.loading = false
    }
  }

  /** `VirtualList`'s `onEndReached`: the next keyset page. */
  async loadMore() {
    if (!this.selectedMailbox || !this.nextCursor || this.loadingMore) return
    this.loadingMore = true
    const cursor = this.nextCursor
    try {
      const generation = this.#generation
      // Bug 3, the same as `refresh` above -- must agree with it, or
      // scrolling to a second page would bring snoozed threads back.
      const filter: ThreadFilter = { snoozed: isSnoozedMailbox(this.mailbox) }
      if (this.category) filter.category = this.category
      const page = await mailApi.listThreads(this.selectedMailbox, filter, cursor, PAGE)
      // A mailbox or category change while this page was on its way bumps
      // the generation; its rows belong to a list no longer showing.
      if (generation !== this.#generation || cursor !== this.nextCursor) return
      this.threads = [...this.threads, ...page.threads]
      this.nextCursor = page.nextCursor ?? null
    } catch (e) {
      await quietly(e)
    } finally {
      this.loadingMore = false
    }
  }

  // ── the open thread ────────────────────────────────────────────────

  async openThreadById(id: ThreadId) {
    this.selectedThread = id
    this.summary = null
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
      if (auto && this.selectedThread === detail.thread.id) this.composing = auto
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
    if (this.selectedThread) await this.openThreadById(next.id)
    else this.selectedThread = next.id
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
   */
  #advanceIfOpen(id: ThreadId): void {
    if (this.selectedThread !== id) return
    this.selectedThread = (this.#neighbour(1) ?? this.#neighbour(-1))?.id ?? null
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
    id: ThreadId,
    patch: Partial<Thread>,
    call: (id: ThreadId) => Promise<unknown>,
  ): Promise<void> {
    const { rows, before } = applyRowPatch(this.threads, id, patch)
    this.threads = rows
    const { rows: searchRows, before: searchBefore } = applyRowPatch(this.searchResults, id, patch)
    this.searchResults = searchRows
    const openBefore = this.openThread?.thread.id === id ? this.openThread.thread : null
    if (openBefore) {
      this.openThread = { ...this.openThread!, thread: { ...openBefore, ...patch } }
    }
    try {
      await call(id)
      this.#scheduleUnreadRefresh()
    } catch (e) {
      if (before) this.threads = revertRow(this.threads, id, before)
      if (searchBefore) this.searchResults = revertRow(this.searchResults, id, searchBefore)
      if (openBefore && this.openThread?.thread.id === id) {
        this.openThread = { ...this.openThread, thread: openBefore }
      }
      await handle(e)
    }
  }

  /** Removes the row from the list on screen -- archive, trash, move,
   *  snooze -- and from a search's results too (finding 4), restoring both
   *  in place on failure. */
  async #remove(id: ThreadId, call: (id: ThreadId) => Promise<unknown>): Promise<void> {
    // Finding 5: move on to the neighbour rather than simply closing the
    // pane -- `#advanceIfOpen` must run before the row disappears below, so
    // it can still find one.
    this.#advanceIfOpen(id)
    const { rows, removed } = removeRow(this.threads, id)
    this.threads = rows
    const { rows: searchRows, removed: searchRemoved } = removeRow(this.searchResults, id)
    this.searchResults = searchRows
    try {
      await call(id)
      this.#scheduleUnreadRefresh()
    } catch (e) {
      if (removed) this.threads = restoreRow(this.threads, removed)
      if (searchRemoved) this.searchResults = restoreRow(this.searchResults, searchRemoved)
      await handle(e)
    }
  }

  markRead(id: ThreadId) {
    return this.#act(id, { unreadCount: 0 }, mailApi.markRead)
  }
  markUnread(id: ThreadId) {
    return this.#act(id, { unreadCount: 1 }, mailApi.markUnread)
  }
  /**
   * Star, best-effort. `Thread.starred` -- the real per-thread aggregate,
   * see its own doc in `types.ts` -- is the current state; the open thread's
   * own messages are consulted only as a fallback for a row not currently in
   * `this.threads` (a thread reached only through `openThread`, which does
   * not happen in the interface today, but costs nothing to keep honest).
   */
  toggleStar(id: ThreadId) {
    const row = this.threads.find((t) => t.id === id) ?? this.searchResults.find((t) => t.id === id)
    const starred =
      row?.starred ??
      (this.openThread?.thread.id === id && this.openThread.messages.some((m) => m.flags.flagged))
    return this.#act(id, { starred: !starred }, starred ? mailApi.unstar : mailApi.star)
  }
  archive(id: ThreadId) {
    return this.#remove(id, mailApi.archive)
  }
  trash(id: ThreadId) {
    return this.#remove(id, mailApi.trash)
  }
  moveTo(id: ThreadId, mailbox: MailboxId) {
    return this.#remove(id, (t) => mailApi.moveToMailbox(t, mailbox))
  }
  label(id: ThreadId, labelName: string) {
    return this.#act(id, {}, (t) => mailApi.label(t, labelName))
  }
  snooze(id: ThreadId, until: Date) {
    return this.#remove(id, (t) => mailApi.snooze(t, until.toISOString()))
  }
  unsnooze(id: ThreadId) {
    return this.#act(id, { snoozedUntil: null }, mailApi.unsnooze)
  }

  /** (p) TODO: teaches the split-inbox rules -- see `mail-api.ts`'s own
   *  TODO(p) for `set_thread_category`'s contract. */
  setCategoryFor(id: ThreadId, category: MailCategory) {
    return this.#act(id, { category }, (t) => mailApi.setThreadCategory([t], category))
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

  async compose(account?: AccountId) {
    const accountId = account ?? this.selectedAccount ?? this.mailboxes[0]?.accountId
    if (!accountId) return
    const draft = await mailApi.newDraft({ account: accountId })
    this.composing = draft
  }

  async reply(messageId: string, all: boolean) {
    const accountId = this.openThread?.thread.accountId
    if (!accountId) return
    const draft = await mailApi.newDraft({
      account: accountId,
      inReplyTo: messageId,
      replyAll: all,
    })
    this.composing = draft
  }

  async forward(messageId: string) {
    const accountId = this.openThread?.thread.accountId
    if (!accountId) return
    const draft = await mailApi.newDraft({ account: accountId, forwardOf: messageId })
    this.composing = draft
  }

  closeCompose() {
    this.composing = null
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
      this.composing = await mailApi.undoSend(pending.draft.id)
    } catch (e) {
      // The undo window can close a beat before the click lands -- the
      // server then refuses, and the toast above is already down; all that
      // is left is to say why the click did nothing.
      await handle(e)
    }
  }

  // ── search ───────────────────────────────────────────────────────

  setSearchQuery(q: string) {
    this.searchQuery = q
    this.searchCursor = null
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
    if (changes.length !== 1) return false
    const change = changes[0]!
    const id = singleId(change)
    if (!id) return false
    if (change.kind === 'draft') {
      // Nothing in the visible thread list or the sync-status line reads a
      // draft directly today -- the compose sheet owns its own working copy
      // and autosaves it, so another window's edit to the *same* draft is
      // not something this window should clobber mid-keystroke. Handled as
      // "nothing to do" rather than falling through to `RELOAD.mail`, which
      // would otherwise reload the thread list for every autosave tick.
      return true
    }
    if (change.op === 'deleted') {
      // Finding 5: a thread deleted from another window or by the assistant
      // must not stay open here just because nothing removed it from
      // `openThread` -- `#advanceIfOpen` does what archiving already does,
      // moving on to the neighbour, before the row disappears from under it.
      this.#advanceIfOpen(id)
      this.threads = this.threads.filter((t) => t.id !== id)
      this.searchResults = this.searchResults.filter((t) => t.id !== id)
      return true
    }
    if (change.op === 'created' || change.op === 'updated') {
      void this.#patchOne(id)
      // Bug 4: every backend command that can move a thread out of a
      // mailbox from elsewhere -- archive, trash, snooze, an assistant or
      // MCP action, another window's own click -- declares `Thread`/
      // `Updated`, never `Deleted`, and `Thread` carries no mailbox id for
      // `#patchOne` above to notice the row has left. Patching it in place
      // is right as far as it goes -- the subject, the snippet, the flags
      // are current -- but the row stays in a list it may no longer belong
      // to until something re-asks the backend which threads are actually
      // still here. A short, debounced refresh is that ask: harmless if the
      // row still belongs (the page comes back the same), and what actually
      // drops it if it does not, same as opening the mailbox fresh would.
      // Debounced so a batch of changes from one sync tick costs one reload,
      // and `refresh()`'s own `#generation` guard still applies, so a slow
      // one cannot land after a newer list has replaced it.
      //
      // The real fix is a mailbox-scoped change event from the backend --
      // out of scope for this pass.
      this.#scheduleLiveRefresh()
      return true
    }
    return false
  }

  #liveRefreshTimer: ReturnType<typeof setTimeout> | null = null
  #scheduleLiveRefresh() {
    if (this.#liveRefreshTimer) clearTimeout(this.#liveRefreshTimer)
    this.#liveRefreshTimer = setTimeout(() => {
      this.#liveRefreshTimer = null
      void this.refresh()
    }, LIVE_REFRESH_DEBOUNCE_MS)
  }

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

  get mailbox(): Mailbox | null {
    return this.mailboxes.find((m) => m.id === this.selectedMailbox) ?? null
  }
}

export const mail = new MailState()
