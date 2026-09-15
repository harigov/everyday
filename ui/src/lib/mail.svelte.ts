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
//
// `registerApply('mail', …)` is wired below so a single-thread change from
// another window patches the loaded page in place -- but nothing routes to
// it yet. `docs/plans/mail.md`'s write commands do not exist in the real
// backend, so no `ChangeKind` names a thread, a mailbox or a draft for
// `live.svelte.ts`'s `RELOADS` table to point here. The applier is written
// and tested against now regardless, so wiring it up later is one line in
// that table rather than a new store method.

import * as mailApi from './mail-api'
import type { Draft } from './mail-api'
import { registerApply, singleId, type ChangeWithIds } from './live-apply'
import { applyRowPatch, removeRow, restoreRow, revertRow, snoozeChoices } from './mail'
import { app, handle, quietly } from './state.svelte'
import type {
  AccountId,
  Mailbox,
  MailboxId,
  MailCategory,
  Thread,
  ThreadDetail,
  ThreadId,
} from './types'

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
  /** The palette-free reply/reply-all/forward affordance opens the same sheet. */
  searchQuery = $state('')
  searchResults = $state<mailApi.MailSearchHit[]>([])
  searching = $state(false)

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
  syncStatus = $state<mailApi.SyncStatus[]>([])

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
    if (this.#undoTimer) clearInterval(this.#undoTimer)
    this.#undoTimer = null
    this.sendingUndo = null
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
   *  module doc for why this is "good enough", not exact at scale. */
  async refreshUnreadCounts() {
    try {
      const counts = new Map<MailboxId, number>()
      await Promise.all(
        this.mailboxes.map(async (mailbox) => {
          const page = await mailApi.listThreads(mailbox.id, undefined, null, 200)
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

  async refreshSyncStatus() {
    try {
      this.syncStatus = await mailApi.syncStatus()
    } catch (e) {
      await quietly(e)
    }
  }

  /** The account and mailbox picked in the nav. */
  async selectMailbox(id: MailboxId) {
    this.selectedMailbox = id
    this.selectedThread = null
    this.openThread = null
    await this.refresh()
  }

  setCategory(category: MailCategory | null) {
    this.category = category
    void this.refresh()
  }

  async selectAccount(id: AccountScope) {
    this.selectedAccount = id
    this.selectedMailbox = null
    await this.refreshMailboxes()
  }

  /** Reload the first page of the open mailbox. */
  async refresh() {
    if (!this.selectedMailbox) return
    const generation = ++this.#generation
    this.loading = true
    try {
      const page = await mailApi.listThreads(
        this.selectedMailbox,
        this.category ? { category: this.category } : undefined,
        null,
        PAGE,
      )
      if (generation !== this.#generation) return
      this.threads = page.threads
      this.nextCursor = page.nextCursor ?? null
      if (this.selectedThread && !page.threads.some((t) => t.id === this.selectedThread)) {
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
      const page = await mailApi.listThreads(
        this.selectedMailbox,
        this.category ? { category: this.category } : undefined,
        cursor,
        PAGE,
      )
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
    try {
      const detail = await mailApi.getThread(id)
      this.openThread = detail
      // The newest message starts expanded; every earlier one collapsed to
      // its one-line summary, as the plan asks.
      const newest = detail.messages.at(-1)
      this.expanded = newest ? new Set([newest.id]) : new Set()
      if (detail.thread.unreadCount > 0) void this.markRead(id)
      void this.#prefetchNeighbours(id)
    } catch (e) {
      await handle(e)
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
  }

  /** The next and previous thread in list order, for `j`/`k` and prefetch. */
  #neighbour(step: 1 | -1): Thread | null {
    if (!this.selectedThread) return this.threads[0] ?? null
    const at = this.threads.findIndex((t) => t.id === this.selectedThread)
    if (at < 0) return this.threads[0] ?? null
    return this.threads[at + step] ?? null
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
    const at = this.threads.findIndex((t) => t.id === around)
    if (at < 0) return
    for (const t of [this.threads[at - 1], this.threads[at + 1]]) {
      if (!t || this.#prefetching.has(t.id)) continue
      this.#prefetching.add(t.id)
      mailApi
        .getThread(t.id)
        .catch(() => {})
        .finally(() => this.#prefetching.delete(t.id))
    }
  }

  // ── actions ──────────────────────────────────────────────────────
  //
  // Every one of these follows the same shape: patch the row (or remove it),
  // call the command, and on failure put back exactly what was there and
  // say so. `errors.ts`'s `handle` is what says so.

  async #act(
    id: ThreadId,
    patch: Partial<Thread>,
    call: (id: ThreadId) => Promise<void>,
  ): Promise<void> {
    const { rows, before } = applyRowPatch(this.threads, id, patch)
    this.threads = rows
    if (this.openThread?.thread.id === id) {
      this.openThread = { ...this.openThread, thread: { ...this.openThread.thread, ...patch } }
    }
    try {
      await call(id)
      void this.refreshUnreadCounts()
    } catch (e) {
      if (before) this.threads = revertRow(this.threads, id, before)
      await handle(e)
    }
  }

  /** Removes the row from the list on screen -- archive, trash, move,
   *  snooze -- restoring it in place on failure. */
  async #remove(id: ThreadId, call: (id: ThreadId) => Promise<void>): Promise<void> {
    const { rows, removed } = removeRow(this.threads, id)
    this.threads = rows
    if (this.selectedThread === id) this.closeThread()
    try {
      await call(id)
      void this.refreshUnreadCounts()
    } catch (e) {
      if (removed) this.threads = restoreRow(this.threads, removed)
      await handle(e)
    }
  }

  markRead(id: ThreadId) {
    return this.#act(id, { unreadCount: 0 }, mailApi.markRead)
  }
  markUnread(id: ThreadId) {
    return this.#act(id, { unreadCount: 1 }, mailApi.markUnread)
  }
  toggleStar(id: ThreadId) {
    const t = this.threads.find((x) => x.id === id)
    const starred = !(t?.starred ?? false)
    return this.#act(id, { starred }, starred ? mailApi.star : mailApi.unstar)
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
  label(id: ThreadId, label: string) {
    return this.#act(id, {}, (t) => mailApi.label(t, label))
  }
  snooze(id: ThreadId, until: Date) {
    return this.#remove(id, (t) => mailApi.snooze(t, until.toISOString()))
  }
  unsnooze(id: ThreadId) {
    return this.#act(id, { snoozedUntil: null }, mailApi.unsnooze)
  }

  /** The snooze picker's fixed choices, for the component to draw. Named
   *  apart from the imported `snoozeChoices` so a reader is never asking
   *  whether this calls itself. */
  snoozeOptions() {
    return snoozeChoices()
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
   */
  sendingUndo = $state<{ draft: Draft; secondsLeft: number } | null>(null)
  #undoTimer: ReturnType<typeof setInterval> | null = null

  async send(draft: Draft, delaySeconds: number) {
    await mailApi.sendDraft(draft.id, delaySeconds)
    this.closeCompose()
    if (this.#undoTimer) clearInterval(this.#undoTimer)
    this.sendingUndo = { draft, secondsLeft: delaySeconds }
    this.#undoTimer = setInterval(() => {
      if (!this.sendingUndo) return
      const left = this.sendingUndo.secondsLeft - 1
      if (left <= 0) {
        if (this.#undoTimer) clearInterval(this.#undoTimer)
        this.#undoTimer = null
        this.sendingUndo = null
        return
      }
      this.sendingUndo = { ...this.sendingUndo, secondsLeft: left }
    }, 1000)
  }

  /** Reopens the draft in the compose sheet, exactly where sending left it. */
  async undoSend() {
    const pending = this.sendingUndo
    if (!pending) return
    await mailApi.undoSend(pending.draft.id)
    if (this.#undoTimer) clearInterval(this.#undoTimer)
    this.#undoTimer = null
    this.sendingUndo = null
    this.composing = pending.draft
  }

  // ── search ───────────────────────────────────────────────────────

  setSearchQuery(q: string) {
    this.searchQuery = q
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
      const { hits } = await mailApi.searchMail(q)
      if (this.searchQuery === q) this.searchResults = hits
    } catch (e) {
      await quietly(e)
    } finally {
      if (this.searchQuery === q) this.searching = false
    }
  }

  clearSearch() {
    this.searchQuery = ''
    this.searchResults = []
    this.searching = false
  }

  // ── live-apply ───────────────────────────────────────────────────

  #applyChanges(changes: ChangeWithIds[]): boolean {
    if (changes.length !== 1) return false
    const change = changes[0]!
    const id = singleId(change)
    if (!id) return false
    if (change.op === 'deleted') {
      this.threads = this.threads.filter((t) => t.id !== id)
      return true
    }
    if (change.op === 'created' || change.op === 'updated') {
      void this.#patchOne(id)
      return true
    }
    return false
  }

  async #patchOne(id: ThreadId) {
    try {
      const detail = await mailApi.getThread(id)
      const at = this.threads.findIndex((t) => t.id === id)
      if (at >= 0) this.threads = this.threads.map((t, i) => (i === at ? detail.thread : t))
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
