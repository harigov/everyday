// Pure logic for the Mail app: nothing here touches the network, the vault
// or a component. Tested with `scripts/mail.test.mjs`, the way `entry-rows.ts`
// and `habits.ts` are tested -- see those for the harness.

import { friendlyDate, timeOfDay } from './format'
import { isoDate } from './time'
import type { MailAddress } from './types'

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

// ── Reply-all recipient computation ────────────────────────────────────

/**
 * Who a reply-all addresses: the original sender and every original
 * recipient, minus the account's own address and any duplicate, sender
 * first. The original Cc stays Cc; nobody who was already in To is repeated
 * in Cc.
 */
export function replyAllRecipients(
  message: { from: MailAddress; to: MailAddress[]; cc: MailAddress[] },
  ownAddress: string,
): { to: MailAddress[]; cc: MailAddress[] } {
  const own = ownAddress.trim().toLowerCase()
  const isSelf = (a: MailAddress) => a.email.trim().toLowerCase() === own
  const dedupeAgainst = (list: MailAddress[], exclude: Set<string>) => {
    const seen = new Set(exclude)
    const out: MailAddress[] = []
    for (const a of list) {
      const key = a.email.trim().toLowerCase()
      if (seen.has(key) || isSelf(a)) continue
      seen.add(key)
      out.push(a)
    }
    return out
  }
  const to = dedupeAgainst([message.from, ...message.to], new Set())
  const cc = dedupeAgainst(message.cc, new Set(to.map((a) => a.email.trim().toLowerCase())))
  return { to, cc }
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
