// Pure logic for the Mail app: nothing here touches the network, the vault
// or a component. Tested with `scripts/mail.test.mjs`, the way `entry-rows.ts`
// and `habits.ts` are tested -- see those for the harness.

import {
  dateFormat,
  daysBetween,
  friendlyDate,
  relativeTime,
  timeOfDay,
  weekdayShort,
} from './format'
import { isoDate, startOfDay } from './time'
import type {
  AccountId,
  CategoryCount,
  Draft,
  Mailbox,
  MailboxId,
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

/**
 * "Tue 8:00": a weekday and a time together, for the two places a bare date
 * or a bare time says less than both at once -- the Snoozed view's own date
 * column (`snoozedUntilLabel`) and the Scheduled list's own one line
 * (`scheduledSendLabel`). Unlike `threadListDate`, which picks *either* a
 * time (today) *or* a date (everything else), both of these are always
 * naming a moment that is never today's own 24 hours alone -- a snooze or a
 * send-later is never due back in the next few minutes -- so the weekday is
 * always worth printing.
 */
export function weekdayAndTime(at: string): string {
  const d = new Date(at)
  return `${weekdayShort(isoDate(d))} ${timeOfDay(d)}`
}

/**
 * The Snoozed view's own date column: "Until Tue 8:00" rather than
 * `threadListDate`'s own "when it last arrived" -- what the row means here
 * is when the thread comes back, not when it was last touched.
 */
export function snoozedUntilLabel(until: string): string {
  return `Until ${weekdayAndTime(until)}`
}

/**
 * The Scheduled list's own one line: "Sends Tue 8:00 (in 3 hours)" -- the
 * same weekday-and-time half `snoozedUntilLabel` draws from, plus
 * `relativeTime`'s own "in 3 hours", the way the undo toast already spells
 * a send-later instant (`MailView.svelte`'s own `sendingUndo` toast).
 */
export function scheduledSendLabel(sendAt: string): string {
  return `Sends ${weekdayAndTime(sendAt)} (${relativeTime(sendAt)})`
}

// ── Date sections over the thread list ──────────────────────────────────
//
// Today, Yesterday, Earlier this week, Last week, Earlier this month, Last
// month, then every month still within this calendar year by name alone
// ("August"), and month-and-year once the year itself is no longer implied
// ("December 2025"). Weeks start Monday, irrespective of locale: a list of
// section headings is read as English prose ("Last week"), not as a
// calendar grid, where the week's own start day actually matters.
//
// Built for `instant` always at or before `now` -- a thread's own
// `lastDate` -- and not meant for a future instant: the Snoozed view's own
// `snoozedUntil` groups no other way than by not grouping at all
// (`MailView.svelte`'s own choice, not this function's).

export interface DateSection {
  key: string
  label: string
}

export function dateSection(instant: string, now = new Date()): DateSection {
  const d = startOfDay(isoDate(new Date(instant)))
  const ago = daysBetween(d, now)
  if (ago <= 0) return { key: 'today', label: 'Today' }
  if (ago === 1) return { key: 'yesterday', label: 'Yesterday' }

  const today = startOfDay(isoDate(now))
  const mondayOffset = (now.getDay() + 6) % 7 // days since this week's own Monday
  const thisWeekStart = new Date(today)
  thisWeekStart.setDate(thisWeekStart.getDate() - mondayOffset)
  if (d >= thisWeekStart) return { key: 'thisWeek', label: 'Earlier this week' }

  const lastWeekStart = new Date(thisWeekStart)
  lastWeekStart.setDate(lastWeekStart.getDate() - 7)
  if (d >= lastWeekStart) return { key: 'lastWeek', label: 'Last week' }

  const thisMonthStart = new Date(now.getFullYear(), now.getMonth(), 1)
  if (d >= thisMonthStart) return { key: 'thisMonth', label: 'Earlier this month' }

  const lastMonthStart = new Date(now.getFullYear(), now.getMonth() - 1, 1)
  if (d >= lastMonthStart) return { key: 'lastMonth', label: 'Last month' }

  const monthName = dateFormat({ month: 'long' }).format(d)
  const key = `${d.getFullYear()}-${d.getMonth()}`
  return d.getFullYear() === now.getFullYear()
    ? { key, label: monthName }
    : { key, label: `${monthName} ${d.getFullYear()}` }
}

/** One row `MailView.svelte`'s list column actually draws: a thread, or a
 *  `dateSection` heading ahead of a run of them. Never selectable on its
 *  own -- a header's `key` never collides with a `Thread.id`, so
 *  `VirtualList`'s own `selectedId` naturally never matches one. */
export type ListRow =
  { type: 'header'; key: string; label: string } | { type: 'thread'; thread: Thread }

/**
 * Interleaves `dateSection` headings ahead of each run of threads that
 * shares one -- a heading drawn once, immediately before the first row
 * under it, the way any grouped list reads, never once per row under it.
 *
 * `dateOf` is which field a row groups by: a thread's own `lastDate`
 * ordinarily. Threads are assumed already in the order the caller wants
 * them drawn (newest first, from the backend's own sort) -- this never
 * reorders them, only inserts headings between runs.
 */
export function withDateSections(
  threads: readonly Thread[],
  dateOf: (t: Thread) => string,
  now = new Date(),
): ListRow[] {
  const out: ListRow[] = []
  let currentKey: string | null = null
  for (const thread of threads) {
    const section = dateSection(dateOf(thread), now)
    if (section.key !== currentKey) {
      out.push({ type: 'header', key: section.key, label: section.label })
      currentKey = section.key
    }
    out.push({ type: 'thread', thread })
  }
  return out
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
  key: 'laterToday' | 'thisEvening' | 'tomorrow' | 'thisWeekend' | 'nextWeek'
  label: string
  at: Date
}

/**
 * The snooze picker's fixed choices, plus "pick a date" which the component
 * draws itself since it has no fixed time to test.
 *
 * Computed from `now` rather than memoised, so "later today" always means a
 * few hours from now: a fixed instant baked in at load time would drift
 * false the moment the picker had been open longer than it took to press.
 * Superhuman's own five, each dropping out once it would stop meaning what
 * its label says:
 *
 *   - Later today (+3h): gone once it is past 6pm -- three hours from then
 *     would spill into tomorrow, which is what "Tomorrow" already is.
 *   - This evening (a fixed 6pm): gone once it is past 5pm -- an hour out is
 *     close enough to "Later today" to be the same choice said twice.
 *   - Tomorrow (8am): always on offer.
 *   - This weekend (Saturday 9am): only Monday through Thursday -- pressed on
 *     a Friday, Saturday or Sunday, the weekend is already close enough that
 *     "Next week" is the more useful second choice.
 *   - Next week (next Monday 8am, not "+7 days"): always the same calendar
 *     Monday regardless of which day of the week this is pressed on.
 */
export function snoozeChoices(now = new Date()): SnoozeChoice[] {
  const out: SnoozeChoice[] = []
  const hour = now.getHours()
  const day = now.getDay() // 0 Sunday .. 6 Saturday

  if (hour < 18) {
    const laterToday = new Date(now)
    laterToday.setHours(laterToday.getHours() + 3, 0, 0, 0)
    out.push({ key: 'laterToday', label: 'Later today', at: laterToday })
  }

  if (hour < 17) {
    const evening = new Date(now)
    evening.setHours(18, 0, 0, 0)
    out.push({ key: 'thisEvening', label: 'This evening', at: evening })
  }

  const tomorrow = new Date(now)
  tomorrow.setDate(tomorrow.getDate() + 1)
  tomorrow.setHours(8, 0, 0, 0)
  out.push({ key: 'tomorrow', label: 'Tomorrow', at: tomorrow })

  if (day >= 1 && day <= 4) {
    const weekend = new Date(now)
    weekend.setDate(weekend.getDate() + (6 - day))
    weekend.setHours(9, 0, 0, 0)
    out.push({ key: 'thisWeekend', label: 'This weekend', at: weekend })
  }

  // The next Monday strictly after today: `(8 - day) % 7` lands on *this*
  // Monday (0 days out) when `day` already is one, which "next week" never
  // means -- the `|| 7` sends that one case a full week out instead.
  const nextWeek = new Date(now)
  const daysToNextMonday = (8 - day) % 7 || 7
  nextWeek.setDate(nextWeek.getDate() + daysToNextMonday)
  nextWeek.setHours(8, 0, 0, 0)
  out.push({ key: 'nextWeek', label: 'Next week', at: nextWeek })

  return out
}

/**
 * The instant the custom snooze picker's date and time fields mean together,
 * *local* to the reader: `timeStr`, on the day `dateStr` shows -- 8am when
 * the time field is left at its own default.
 *
 * `new Date('2026-09-20')` parses a bare date as UTC midnight; calling
 * `.setHours(...)` on that then reads back in whichever zone the reader is
 * in, not the zone the date was meant to be read in -- west of Greenwich
 * that silently lands on the *previous* day. Building the date from its
 * parsed `year`/`month`/`day` parts instead means the local constructor
 * picks local midnight for that calendar day, so `setHours` lands on the day
 * the field actually shows.
 *
 * Never in the past: guaranteed not by anything here but by the date field's
 * own `min`, `earliestSnoozeDate` below -- always tomorrow at the earliest,
 * which no time of day on it can land behind `now`.
 */
export function customSnoozeInstant(dateStr: string, timeStr = '08:00'): Date {
  const [year, month, day] = dateStr.split('-').map(Number)
  const [hours, minutes] = timeStr.split(':').map(Number)
  const at = new Date(year!, (month ?? 1) - 1, day ?? 1)
  at.setHours(hours ?? 8, minutes ?? 0, 0, 0)
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
 *
 * `bodyHtml` is the whole document `bodyDocument`/`body_document` builds --
 * `<head>`, the base style, a sender's own `<style>` block and all -- not
 * just what `<body>` renders. Bug: measuring before dropping the head counts
 * a style block's own CSS as if it were prose, which (by coincidence, not
 * design) padded the guess out for a short note but does nothing reliable
 * for a longer one or a newsletter with a real stylesheet. Dropped here, so
 * only what actually draws is ever counted.
 */
export function estimateBodyHeight(bodyHtml: string, widthPx: number): number {
  const body = bodyHtml.replace(/^[\s\S]*<body[^>]*>/i, '').replace(/<\/body>[\s\S]*$/i, '')
  const charsPerLine = Math.max(20, Math.floor(widthPx / 8.2))
  // A `<br>` or the end of a block element starts a fresh line. Without
  // this, every block's text pooled into one shared character budget, as if
  // a greeting ("Hi,"), the paragraph after it, and a two-line sign-off
  // might all share lines with each other -- undercounting exactly the
  // short-line, many-paragraph shape an ordinary note (a greeting, a
  // paragraph, "Best," on its own line, a name under it) actually has. This
  // was Bug: a four-line reply landing short enough to need its own inner
  // scrollbar in a pane with room to spare below it.
  const withBreaks = body.replace(/<(br|\/p|\/div|\/li|\/blockquote)[ >/]/gi, (m) => `\n${m}`)
  const segmentLines = withBreaks
    .split('\n')
    .map((s) =>
      s
        .replace(/<[^>]*>/g, ' ')
        .replace(/\s+/g, ' ')
        .trim(),
    )
    .filter((s) => s.length > 0)
    .map((s) => Math.max(1, Math.ceil(s.length / charsPerLine)))
  const lines = Math.max(
    3,
    segmentLines.reduce((a, b) => a + b, 0),
  )
  const blocks = (body.match(/<(p|blockquote|div|li)[ >]/gi) ?? []).length
  // 15px / 1.6 line-height and 16px body padding, both sides -- `BASE_STYLE`
  // in `mailview.rs` and `mockDocument` in `mailview.ts` agree on the body
  // style, so this agrees with both. `blockGapPx` stands in for the
  // `<p>`/`<blockquote>` margins neither style sheet resets.
  const lineHeightPx = 24
  const blockGapPx = 18
  // One spare line, because the guess errs short on the commonest reply of
  // all: a quoted message, whose `<blockquote>` is indented 40px a side and
  // so wraps sooner than the width above assumes. A line of blank under a
  // short note costs nothing; an inner scrollbar on one is the glitch.
  const headroomPx = lineHeightPx
  return Math.min(2400, lines * lineHeightPx + blocks * blockGapPx + 32 + headroomPx)
}

/**
 * {@link estimateBodyHeight}, with room for what an HTML email's layout adds
 * that its text does not say: a table row's own padding, a heading, and an
 * image's height (its `height` attribute, or a guess for one without). For
 * the compose sheet's quoted message, whose frame -- sandboxed, like a
 * message's -- cannot be measured from outside, and where a newsletter is
 * the usual thing being quoted.
 */
export function estimateQuoteHeight(html: string, widthPx: number): number {
  const rows = (html.match(/<tr[\s>]/gi) ?? []).length
  const headings = (html.match(/<h[1-6][\s>]/gi) ?? []).length
  const images = (html.match(/<img\b[^>]*>/gi) ?? []).reduce((sum, tag) => {
    const height = /\bheight\s*=\s*["']?(\d+)/i.exec(tag)
    return sum + Math.min(600, height ? Number(height[1]) : 160)
  }, 0)
  return Math.min(4000, estimateBodyHeight(html, widthPx) + rows * 28 + headings * 16 + images + 24)
}

// ── Compose: is there anything here worth keeping? ──────────────────────

/**
 * A draft with no recipient, subject or body -- what `MailCompose.svelte`'s
 * `discard()` treats as "never really started", deleting it outright rather
 * than autosaving it. Pulled out as its own function because that same
 * boolean also decides whether the sheet's autosave gets flushed or
 * forgotten on the way out: `discardDraft` and a subsequent autosave write
 * race, and the loser is either a wasted write or a draft the person just
 * asked to throw away reappearing behind their back.
 */
export function isBlankDraft(draft: Pick<Draft, 'subject' | 'bodyHtml' | 'to'>): boolean {
  return !draft.subject && !draft.bodyHtml.replace(/<[^>]*>/g, '').trim() && draft.to.length === 0
}

/**
 * An address typed into To/Cc/Bcc and committed without picking a
 * suggestion: a bare `ann@example.com`, or the `Ann Lee <ann@example.com>`
 * a pasted header line carries, with any quotes round the name dropped.
 * Trailing commas and semicolons -- what a person types to move on to the
 * next one -- are not part of it. `null` for nothing at all.
 */
export function parseTypedAddress(text: string): MailAddress | null {
  const trimmed = text
    .trim()
    .replace(/[,;]+$/, '')
    .trim()
  if (!trimmed) return null
  const angled = /^(.*?)\s*<([^<>]+)>$/.exec(trimmed)
  if (angled) {
    const name = angled[1]!
      .trim()
      .replace(/^"(.*)"$/, '$1')
      .trim()
    return { name, email: angled[2]!.trim() }
  }
  return { name: '', email: trimmed }
}

// ── (p) The split inbox: category tabs ─────────────────────────────────
//
// TODO(p): mirrors `Category::ALL`'s order in `everyday_core::mail` --
// Priority, Important, Other, Newsletter, Notification -- which is also the
// order `MailCategory` in `types.ts` declares its five members in. Written
// out again here, as a value rather than derived from the type, because a
// type has no order at runtime for a tab strip to read.
//
// Priority leads rather than following Important: a thread lands there
// either by a standing per-sender rule (`setCategoryFor(id, 'priority')`,
// the VIP rule `set_thread_category` already carries) or by being flagged
// just this once (`setThreadPriority`/`mail.setPriority`), and either way it
// is the one tab worth checking before anything else in the split.
export const CATEGORY_TABS: { key: MailCategory; label: string }[] = [
  { key: 'priority', label: 'Priority' },
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

/**
 * A tab's badge, out of `category_counts`' rows: one category's threads, or
 * every row together for "All" (`null`) -- the uncategorised included,
 * since "All" lists them too.
 */
export function categoryTabCount(
  counts: readonly CategoryCount[],
  key: MailCategory | null,
): { threads: number; unread: number } {
  const rows = key === null ? counts : counts.filter((c) => c.category === key)
  return {
    threads: rows.reduce((sum, c) => sum + c.threads, 0),
    unread: rows.reduce((sum, c) => sum + c.unread, 0),
  }
}

/** Does this mailbox draw the split-inbox category tabs -- only the inbox
 *  does, per `MailView.svelte`'s own `showTabs` -- and so is a category
 *  filter ever meaningful to send for it. A mailbox with no tabs showing a
 *  category anyway is finding 1: the filter leaking into Sent, Archive, a
 *  label, wherever there is no tab strip to have set it from.
 *
 *  Reads `role` alone, never a mailbox's name: `refreshMailboxes`'s own
 *  `promoteGmailInbox` is what makes sure the one Gmail account's `\Inbox`
 *  label that is really the Inbox already carries `role: 'inbox'` by the
 *  time anything here asks, so nothing downstream needs to know a label
 *  was ever mistaken for a folder in the first place. */
export function mailboxHasTabs(mailbox: Pick<Mailbox, 'role'> | null | undefined): boolean {
  return mailbox?.role === 'inbox'
}

// ── Gmail's own system labels, still IMAP-escaped on the wire ──────────
//
// A Gmail account's special-use mailboxes arrive named `\Inbox`,
// `\Important`, `\Starred`, and so on -- RFC 6154's own backslash-prefixed
// spelling, which a parsing bug let straight through to `remoteName`
// instead of resolving to the right `role`. A sync agent is fixing the
// parser and repairing rows already on disk; this is the interface's own
// half: cope with both an old, still-escaped row and a new, correctly-typed
// one, so nobody has to wait for a resync before "Inbox" reads as "Inbox".

/** Gmail's system labels, case-insensitively, once IMAP's own backslash
 *  escaping is stripped -- `null` for an ordinary folder (a person's own,
 *  or any name escaping could not have produced). */
export type GmailSystemLabel =
  'inbox' | 'sent' | 'drafts' | 'starred' | 'important' | 'spam' | 'trash' | 'all'

const GMAIL_SYSTEM_LABELS: Record<string, GmailSystemLabel> = {
  inbox: 'inbox',
  sent: 'sent',
  draft: 'drafts',
  drafts: 'drafts',
  starred: 'starred',
  important: 'important',
  spam: 'spam',
  trash: 'trash',
  all: 'all',
}

/** Strips IMAP's own one- or two-backslash escaping from a mailbox name,
 *  leaving everything else -- including a `[Gmail]/` folder prefix, which
 *  is not escaping and is `mailboxDisplayName`'s own concern -- untouched. */
function unescapeImapName(remoteName: string): string {
  return remoteName.replace(/^\\{1,2}/, '').trim()
}

export function gmailSystemLabel(remoteName: string): GmailSystemLabel | null {
  return GMAIL_SYSTEM_LABELS[unescapeImapName(remoteName).toLowerCase()] ?? null
}

/** The rename table `mailboxDisplayName` reads once a `[Gmail]/` prefix and
 *  any IMAP escaping are gone -- wider than `GMAIL_SYSTEM_LABELS` above,
 *  since a reader-facing name draws a line `role` has no use for ("Sent
 *  Mail" and "Sent" are the same mailbox, but neither is a `MailCategory`
 *  or anything else code branches on). */
const GMAIL_DISPLAY_NAMES: Record<string, string> = {
  inbox: 'Inbox',
  sent: 'Sent',
  'sent mail': 'Sent',
  draft: 'Drafts',
  drafts: 'Drafts',
  important: 'Important',
  starred: 'Starred',
  spam: 'Spam',
  junk: 'Spam',
  trash: 'Trash',
  bin: 'Trash',
  'all mail': 'All Mail',
}

/**
 * The one name to show for a mailbox, however Gmail happened to spell it on
 * the wire: a `[Gmail]/`/`[Google Mail]/` folder prefix and IMAP's own
 * backslash escaping both stripped, then matched case-insensitively against
 * Gmail's own names. Anything that matches none of those -- a person's own
 * folder, a label, or a provider that is not Gmail at all -- is shown
 * exactly as sent, prefix and escaping aside.
 */
export function mailboxDisplayName(box: Pick<Mailbox, 'remoteName'>): string {
  const stripped = unescapeImapName(box.remoteName.replace(/^\[(Gmail|Google Mail)\]\//i, ''))
  return GMAIL_DISPLAY_NAMES[stripped.toLowerCase()] ?? stripped
}

/**
 * `accountMailboxes`, with Gmail's own `\Inbox` label promoted to serve as
 * the account's Inbox when nothing else already carries `role: 'inbox'` --
 * an old row, synced before the fix that should have given it that role
 * directly. Returns a fresh array; a mailbox that needed no change is
 * returned unchanged (`===` the original), so a caller that only wants to
 * know whether anything moved can ask cheaply.
 *
 * Every *other* Gmail system label (`\Sent`, `\Draft(s)`, `\Starred`,
 * `\Important`, `\Spam`, `\Trash`, `\All`) is left exactly as it arrived --
 * this only ever promotes the one role nothing else has already claimed.
 * `MailNav`'s own folders list is where the rest are read as duplicates and
 * left out, since leaving them as ordinary `role: 'other'` mailboxes is
 * also what lets a reader still find one by its real name if they go
 * looking, rather than this quietly renaming eight mailboxes behind them.
 */
export function promoteGmailInbox(accountMailboxes: readonly Mailbox[]): Mailbox[] {
  if (accountMailboxes.some((m) => m.role === 'inbox')) return accountMailboxes.slice()
  const at = accountMailboxes.findIndex((m) => gmailSystemLabel(m.remoteName) === 'inbox')
  if (at < 0) return accountMailboxes.slice()
  const next = accountMailboxes.slice()
  next[at] = { ...next[at]!, role: 'inbox' }
  return next
}

/**
 * Is this the "Snoozed" pseudo-mailbox `MailNav` builds a row for -- the one
 * place a mailbox listing needs to ask the backend *for* snoozed threads
 * (`ThreadFilter.snoozed: true`) rather than hide them (`false` everywhere
 * else, per `Thread.snoozedUntil`'s own docs: a snooze does not move a
 * thread out of the mailbox it otherwise belongs to, so leaving `snoozed`
 * unset there would show it in both places at once).
 *
 * Matched by `Mailbox.pseudo`, never by name. It is not a real IMAP mailbox
 * (see `mock-mail.ts`'s own note on why), and a real folder that happens to
 * be called "Snoozed" -- Spark makes one -- matched by name would list only
 * snoozed threads, which is to say nothing, with a zero unread badge.
 */
export function isSnoozedMailbox(mailbox: Pick<Mailbox, 'pseudo'> | null | undefined): boolean {
  return mailbox?.pseudo === 'snoozed'
}

/**
 * Does this account need `mail.svelte.ts`'s own stand-in Snoozed mailbox?
 *
 * True once the account has an Inbox (nothing to filter otherwise) and no
 * mailbox of its own already answering `pseudo: 'snoozed'` -- the mock seeds
 * one for each of its two accounts; no real backend sends one at all today.
 * `refreshMailboxes` asks this per account, once its mailboxes have loaded,
 * before deciding whether to synthesise a row.
 */
export function needsSyntheticSnoozedMailbox(
  accountMailboxes: readonly Pick<Mailbox, 'role' | 'pseudo'>[],
): boolean {
  return (
    accountMailboxes.some((m) => m.role === 'inbox') &&
    !accountMailboxes.some((m) => m.pseudo === 'snoozed')
  )
}

/** The id prefix every mailbox `needsSyntheticSnoozedMailbox` calls for
 *  carries. Never sent to the backend as a mailbox id in its own right --
 *  `listTarget` below is what redirects it before any command sees it. */
const SYNTHETIC_SNOOZED_PREFIX = 'synthetic-snoozed:'

/** The id `refreshMailboxes` gives the stand-in Snoozed row it builds for
 *  one account. A plain string, not a real id any backend minted -- see
 *  `listTarget`. */
export function syntheticSnoozedMailboxId(accountId: AccountId): MailboxId {
  return `${SYNTHETIC_SNOOZED_PREFIX}${accountId}`
}

/**
 * Where listing `mailbox` should actually go: its own id, with `snoozed`
 * set by `isSnoozedMailbox` exactly as `refresh`/`loadMore`/
 * `refreshUnreadCounts` already asked it by hand before this existed --
 * `false` for an ordinary mailbox, `true` for a real Snoozed pseudo-mailbox
 * a backend (today, only the mock) sent itself.
 *
 * The one exception is the *synthetic* row `refreshMailboxes` builds for an
 * account whose backend sends no Snoozed mailbox at all: that id means
 * nothing to `list_threads`, so it is swapped here for the account's own
 * Inbox, `snoozed: true` -- "the account's Inbox, filtered," per the plan.
 * A backend-sent pseudo-mailbox is left exactly as it was: it lists under
 * its own id, because that id is real as far as whichever backend sent it
 * is concerned, and redirecting it too would mean asking the Inbox alone
 * rather than every mailbox the mock's own "Snoozed" row reads across
 * (`mock-mail.ts`'s `isPseudo`) -- doubling what the mock already answers
 * for itself rather than leaving it be.
 *
 * `null` only when there is no mailbox to ask about at all.
 */
export function listTarget(
  mailbox: Pick<Mailbox, 'id' | 'accountId' | 'pseudo'> | null | undefined,
  mailboxes: readonly Pick<Mailbox, 'id' | 'accountId' | 'role'>[],
): { mailboxId: MailboxId; snoozed: boolean } | null {
  if (!mailbox) return null
  if (!mailbox.id.startsWith(SYNTHETIC_SNOOZED_PREFIX)) {
    return { mailboxId: mailbox.id, snoozed: isSnoozedMailbox(mailbox) }
  }
  const inbox = mailboxes.find((m) => m.accountId === mailbox.accountId && m.role === 'inbox')
  return { mailboxId: inbox?.id ?? mailbox.id, snoozed: true }
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

/**
 * Every thread from `fromId` to `toId` in `list`, both ends included and in
 * list order whichever way round they are given -- Shift+click's range.
 * Only `[toId]` when `fromId` is not in `list` (no anchor yet, or one left
 * behind on a list no longer showing), and nothing when `toId` is not.
 */
export function threadRange(
  list: readonly Pick<Thread, 'id'>[],
  fromId: string | null,
  toId: string,
): string[] {
  const to = list.findIndex((t) => t.id === toId)
  if (to < 0) return []
  const from = fromId === null ? -1 : list.findIndex((t) => t.id === fromId)
  if (from < 0) return [toId]
  const [lo, hi] = from <= to ? [from, to] : [to, from]
  return list.slice(lo, hi + 1).map((t) => t.id)
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
