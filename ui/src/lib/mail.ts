// Pure logic for the Mail app: nothing here touches the network, the vault
// or a component. Tested with `scripts/mail.test.mjs`, the way `entry-rows.ts`
// and `habits.ts` are tested -- see those for the harness.

import { friendlyDate, timeOfDay } from './format'
import { isoDate } from './time'
import type {
  Mailbox,
  MailAddress,
  MailCategory,
  MailInvite,
  MailOrigin,
  OpKind,
  RecentAction,
  RemoteImageSettings,
  Thread,
} from './types'

// ── Sender-list formatting ─────────────────────────────────────────────

/**
 * How a thread row names who is in it: "Priya Raman" for one, "Priya, Tom"
 * for a couple, "Priya, Tom +3" once it would otherwise crowd the row.
 *
 * Falls back to the address when a participant has no display name, which a
 * message straight off the wire very often does not yet.
 */
export function formatSenders(participants: MailAddress[], max = 2): string {
  const names = participants.map((p) => p.name.trim() || p.email)
  if (names.length === 0) return ''
  if (names.length <= max) return names.join(', ')
  return `${names.slice(0, max).join(', ')} +${names.length - max}`
}

// ── Date formatting for the list ───────────────────────────────────────

/**
 * The thread list's date column: a time of day for something that arrived
 * today, and `friendlyDate`'s existing rules -- "Yesterday", a weekday, a
 * full date -- for anything older. The one formatting decision this file
 * makes for itself; everything else is `format.ts`'s.
 */
export function threadListDate(instant: string): string {
  const d = new Date(instant)
  const now = new Date()
  const sameDay =
    d.getFullYear() === now.getFullYear() &&
    d.getMonth() === now.getMonth() &&
    d.getDate() === now.getDate()
  // `friendlyDate` takes a local `YYYY-MM-DD`, the same as every entry's own
  // `localDate` -- a message's `date` is a full instant, so it is narrowed
  // to the local day here before being handed over.
  return sameDay ? timeOfDay(d) : friendlyDate(isoDate(d))
}

// ── Marks from origin ───────────────────────────────────────────────────
//
// "Archived by the assistant", "Moved by an MCP client", "Couldn't archive:
// {error}" -- what `ThreadDetail.recentActions` turns into under a thread's
// subject. Person-made ops that succeeded say nothing: an action the person
// themself took needs no origin mark, and is not what this line is for.

const OP_LABELS: Record<OpKind['type'], { past: string; base: string }> = {
  markRead: { past: 'Marked read', base: 'mark it read' },
  markUnread: { past: 'Marked unread', base: 'mark it unread' },
  star: { past: 'Starred', base: 'star it' },
  unstar: { past: 'Unstarred', base: 'unstar it' },
  archive: { past: 'Archived', base: 'archive it' },
  trash: { past: 'Trashed', base: 'trash it' },
  move: { past: 'Moved', base: 'move it' },
  label: { past: 'Labelled', base: 'label it' },
  unlabel: { past: 'Unlabelled', base: 'unlabel it' },
  snooze: { past: 'Snoozed', base: 'snooze it' },
  send: { past: 'Sent', base: 'send it' },
  appendDraft: { past: 'Saved a draft copy', base: 'save a draft copy' },
}

/** Who did it, in words -- `null` for a person, who needs no mark. */
export function originPhrase(origin: MailOrigin): string | null {
  switch (origin.type) {
    case 'assistant':
      return 'the assistant'
    case 'mcp':
      return 'an MCP client'
    case 'routine':
      return 'a routine'
    case 'person':
      return null
  }
}

/**
 * One line for a thread's "recent actions" -- `null` when this op is not
 * worth a line at all, per the rule above: a person's own action that
 * succeeded.
 */
export function recentActionLine(action: RecentAction): string | null {
  const label = OP_LABELS[action.kind.type]
  if (action.state.type === 'failed') return `Couldn't ${label.base}: ${action.state.message}`
  const by = originPhrase(action.origin)
  return by ? `${label.past} by ${by}` : null
}

// ── Optimistic apply and revert of row state ───────────────────────────

/** A row shape every function below asks of its argument: an id, nothing more. */
interface HasId {
  id: string
}

/**
 * Patch one row in place (a new array, a new row object) and hand back what
 * it was, so a failed write can restore it with {@link revertRow}.
 *
 * Rows are never mutated: every store here hands `VirtualList` a fresh array
 * on change, matching what that component's own doc asks for.
 */
export function applyRowPatch<T extends HasId>(
  rows: readonly T[],
  id: string,
  patch: Partial<T>,
): { rows: T[]; before: T | null } {
  const at = rows.findIndex((r) => r.id === id)
  if (at < 0) return { rows: rows.slice(), before: null }
  const before = rows[at]!
  const next = rows.slice()
  next[at] = { ...before, ...patch }
  return { rows: next, before }
}

/** Put `original` back where `id` was. The undo half of {@link applyRowPatch}. */
export function revertRow<T extends HasId>(rows: readonly T[], id: string, original: T): T[] {
  const at = rows.findIndex((r) => r.id === id)
  if (at < 0) return rows.slice()
  const next = rows.slice()
  next[at] = original
  return next
}

/**
 * Remove a row entirely -- what archiving or trashing does to the list on
 * screen -- keeping enough to put it back exactly where it was.
 */
export function removeRow<T extends HasId>(
  rows: readonly T[],
  id: string,
): { rows: T[]; removed: { row: T; index: number } | null } {
  const at = rows.findIndex((r) => r.id === id)
  if (at < 0) return { rows: rows.slice(), removed: null }
  const row = rows[at]!
  const next = rows.slice()
  next.splice(at, 1)
  return { rows: next, removed: { row, index: at } }
}

/** The undo half of {@link removeRow}. */
export function restoreRow<T extends HasId>(
  rows: readonly T[],
  removed: { row: T; index: number },
): T[] {
  const next = rows.slice()
  next.splice(Math.min(removed.index, next.length), 0, removed.row)
  return next
}

// ── The snooze picker's times ──────────────────────────────────────────

export interface SnoozeChoice {
  key: 'laterToday' | 'tomorrow' | 'nextWeek'
  label: string
  at: Date
}

/**
 * The snooze picker's three fixed choices, plus "pick a date" which the
 * component draws itself since it has no fixed time to test.
 *
 * Computed from `now` rather than memoised, so "later today" always means a
 * few hours from now: a fixed instant baked in at load time would drift
 * false the moment the picker had been open longer than it took to press.
 * "Later today" drops out once there would be no today left to be later in.
 */
export function snoozeChoices(now = new Date()): SnoozeChoice[] {
  const out: SnoozeChoice[] = []

  const laterToday = new Date(now)
  laterToday.setHours(laterToday.getHours() + 3, 0, 0, 0)
  if (laterToday.getDate() === now.getDate() && laterToday.getHours() < 21) {
    out.push({ key: 'laterToday', label: 'Later today', at: laterToday })
  }

  const tomorrow = new Date(now)
  tomorrow.setDate(tomorrow.getDate() + 1)
  tomorrow.setHours(8, 0, 0, 0)
  out.push({ key: 'tomorrow', label: 'Tomorrow', at: tomorrow })

  const nextWeek = new Date(now)
  nextWeek.setDate(nextWeek.getDate() + 7)
  nextWeek.setHours(8, 0, 0, 0)
  out.push({ key: 'nextWeek', label: 'Next week', at: nextWeek })

  return out
}

/**
 * The instant the custom snooze picker's `<input type="date">` value means:
 * 8am *local*, on the day the field shows.
 *
 * `new Date('2026-09-20')` parses a bare date as UTC midnight; calling
 * `.setHours(8, ...)` on that then reads back 8am in whichever zone the
 * reader is in, not the zone the date was meant to be read in -- west of
 * Greenwich that silently lands on the *previous* day. Building the date
 * from its parsed `year`/`month`/`day` parts instead means the local
 * constructor picks local midnight for that calendar day, so `setHours`
 * lands on the day the field actually shows.
 */
export function customSnoozeInstant(dateStr: string): Date {
  const [year, month, day] = dateStr.split('-').map(Number)
  const at = new Date(year!, (month ?? 1) - 1, day ?? 1)
  at.setHours(8, 0, 0, 0)
  return at
}

/**
 * The earliest date the custom picker should offer: tomorrow, never today.
 *
 * Today's 8am may already be behind `now` by the time this is read -- an
 * instant already past would release the thread almost immediately, which
 * is not what picking "today" in a snooze picker means. `snoozeChoices`'s
 * own "Later today" is what "later, today" is for; the custom picker only
 * needs to cover days it does not.
 */
export function earliestSnoozeDate(now = new Date()): string {
  const tomorrow = new Date(now)
  tomorrow.setDate(tomorrow.getDate() + 1)
  return isoDate(tomorrow)
}

// ── Sizing a message body without touching what the iframe renders ────

/**
 * Guess a message's rendered height in pixels, from its HTML *string* --
 * never by reading the iframe back, which `allow-same-origin` alone would
 * let happen and which `MailThread.svelte`'s own doc explains why this app
 * never grants. Strip the tags, count the characters, divide by a plausible
 * line length at the width the reader actually has. Wrong on the first
 * paint of anything unusual -- a table, an image not yet loaded -- which is
 * why the frame's container keeps `overflow-y: auto`: a short guess scrolls
 * a few pixels further than it needed to rather than clipping the message.
 */
export function estimateBodyHeight(bodyHtml: string, widthPx: number): number {
  const text = bodyHtml
    .replace(/<[^>]*>/g, ' ')
    .replace(/\s+/g, ' ')
    .trim()
  const charsPerLine = Math.max(20, Math.floor(widthPx / 8.2))
  const lines = Math.max(3, Math.ceil(text.length / charsPerLine))
  const blocks = (bodyHtml.match(/<(p|blockquote|div|li)[ >]/gi) ?? []).length
  const lineHeightPx = 21
  const blockGapPx = 12
  return Math.min(2400, lines * lineHeightPx + blocks * blockGapPx + 24)
}

// ── (p) The split inbox: category tabs ─────────────────────────────────
//
// TODO(p): mirrors `Category::ALL`'s order in `everyday_core::mail` --
// Important, Other, Newsletter, Notification -- which is also the order
// `MailCategory` in `types.ts` declares its four members in. Written out
// again here, as a value rather than derived from the type, because a type
// has no order at runtime for a tab strip to read.
export const CATEGORY_TABS: { key: MailCategory; label: string }[] = [
  { key: 'important', label: 'Important' },
  { key: 'other', label: 'Other' },
  { key: 'newsletter', label: 'Newsletters' },
  { key: 'notification', label: 'Notifications' },
]

/** `Tab`/`Shift+Tab`'s own arithmetic: the tab `step` positions from
 *  `current`, wrapping -- `null` for "All" (no category filter) wraps in
 *  alongside the four named ones, at the front. */
export function stepCategoryTab(current: MailCategory | null, step: 1 | -1): MailCategory | null {
  const order: (MailCategory | null)[] = [null, ...CATEGORY_TABS.map((t) => t.key)]
  const at = order.indexOf(current)
  const next = ((((at < 0 ? 0 : at) + step) % order.length) + order.length) % order.length
  return order[next]!
}

/** Does this mailbox draw the split-inbox category tabs -- only the inbox
 *  does, per `MailView.svelte`'s own `showTabs` -- and so is a category
 *  filter ever meaningful to send for it. A mailbox with no tabs showing a
 *  category anyway is finding 1: the filter leaking into Sent, Archive, a
 *  label, wherever there is no tab strip to have set it from. */
export function mailboxHasTabs(mailbox: Pick<Mailbox, 'role'> | null | undefined): boolean {
  return mailbox?.role === 'inbox'
}

/**
 * Is this the "Snoozed" pseudo-mailbox `MailNav` builds a row for -- the one
 * place a mailbox listing needs to ask the backend *for* snoozed threads
 * (`ThreadFilter.snoozed: true`) rather than hide them (`false` everywhere
 * else, per `Thread.snoozedUntil`'s own docs: a snooze does not move a
 * thread out of the mailbox it otherwise belongs to, so leaving `snoozed`
 * unset there would show it in both places at once).
 *
 * Matched by name, the same way `MailNav.svelte`'s own `rowsFor` finds it:
 * it is not a real IMAP mailbox (see `mock-mail.ts`'s own note on why), so
 * there is no `role` of its own for either file to test instead.
 */
export function isSnoozedMailbox(mailbox: Pick<Mailbox, 'remoteName'> | null | undefined): boolean {
  return mailbox?.remoteName === 'Snoozed'
}

// ── (i) Invitations ─────────────────────────────────────────────────

/** Is the current response to `invite` the one `choice` names -- what
 *  highlights Accept/Maybe/Decline on the invite card. */
export function isCurrentInviteResponse(
  invite: MailInvite,
  choice: 'accepted' | 'tentative' | 'declined',
): boolean {
  return invite.myResponse === choice
}

/** A cancellation is shown plainly -- no Accept/Maybe/Decline, per the
 *  plan -- because there is nothing left to RSVP to. */
export function inviteIsCancelled(invite: MailInvite): boolean {
  return invite.method === 'cancel'
}

/** The optimistic patch a click on Accept/Maybe/Decline applies before the
 *  round trip to `respond_to_invite` lands. Pure so the card's highlight
 *  updates the instant it is pressed, the same optimism every other mail
 *  action in this app already has. */
export function applyInviteResponse(
  invite: MailInvite,
  response: 'accepted' | 'tentative' | 'declined',
): MailInvite {
  return { ...invite, myResponse: response }
}

// ── Search: keyset paging ───────────────────────────────────────────

/** Append a search page to what is already on screen, without duplicating a
 *  thread a second page happens to repeat -- the index can return the same
 *  thread near a page boundary if it is written to between two requests. */
export function mergeSearchPage(existing: readonly Thread[], page: readonly Thread[]): Thread[] {
  const seen = new Set(existing.map((t) => t.id))
  const merged = existing.slice()
  for (const t of page) {
    if (seen.has(t.id)) continue
    seen.add(t.id)
    merged.push(t)
  }
  return merged
}

// ── Refresh: covering what is already on screen ─────────────────────────

/**
 * How many rows `refresh` asks for -- ordinarily one page, but widened to
 * cover whatever is already loaded, so a live refresh mid-scroll cannot
 * shrink the list out from under a reader who has scrolled past page one
 * (finding 2). Capped well short of "the whole mailbox": a mailbox scrolled
 * deep into is exactly the case a live refresh has to stay cheap for, not
 * the case to make into a full requery.
 */
export function refreshLimit(loadedCount: number, page = 50): number {
  return Math.min(Math.max(page, loadedCount), page * 20)
}

// ── Whichever list is on screen ──────────────────────────────────────────

/**
 * The list `j`/`k` and prefetch should both read: search results while a
 * query is active -- `MailView.svelte`'s own condition for which one it
 * draws -- the mailbox's own threads otherwise. Finding 3: `#neighbour` read
 * `threads` unconditionally, so `j`/`k` moved through the mailbox behind a
 * search rather than through what was on screen.
 */
export function visibleThreadList<T>(
  query: string,
  threads: readonly T[],
  searchResults: readonly T[],
): readonly T[] {
  return query.trim() ? searchResults : threads
}

/** The next or previous thread in `list`, `step` positions from
 *  `selectedId` -- `list[0]` when nothing is selected or the selection is
 *  not in `list` at all (a stale id left over from switching lists). */
export function neighbourThread(
  list: readonly Thread[],
  selectedId: string | null,
  step: 1 | -1,
): Thread | null {
  if (!selectedId) return list[0] ?? null
  const at = list.findIndex((t) => t.id === selectedId)
  if (at < 0) return list[0] ?? null
  return list[at + step] ?? null
}

// ── Remote images: allow-list matching ──────────────────────────────

/** Would the standing allow-list let this sender's remote images load --
 *  an exact address, or its domain? Mirrors `mailview::remote_images_allowed`
 *  in Rust; kept here too so the Settings → Accounts list can say "already
 *  allowed" without a round trip for every row. */
export function remoteImagesAllowed(settings: RemoteImageSettings, senderEmail: string): boolean {
  const email = senderEmail.trim().toLowerCase()
  if (settings.senders.some((s) => s.trim().toLowerCase() === email)) return true
  const domain = email.split('@')[1]
  if (!domain) return false
  return settings.domains.some((d) => d.trim().toLowerCase() === domain)
}
