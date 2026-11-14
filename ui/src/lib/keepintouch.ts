// Keeping in touch, in words.
//
// The arithmetic is the backend's (`karmayoga_core::keepintouch`): who is due,
// how late, and in what order. This is only how the interface says it. See
// docs/plans/keep-in-touch.md.

import type { CatchUpVia, KindInfo, LogSource, PersonDue, ProposedLog } from './types'

/** The slug of the shelf of people. */
export const CONTACT_SLUG = 'contact'

export function isContactShelf(kind: KindInfo | null | undefined): boolean {
  return kind?.slug === CONTACT_SLUG
}

/** What a log row's means is called in a menu. */
export const VIA_LABELS: Record<CatchUpVia, string> = {
  call: 'Call',
  video: 'Video call',
  message: 'Message',
  email: 'Email',
  inPerson: 'In person',
  other: 'Other',
}

/** The same, mid-sentence: "last caught up 3 Oct by phone". */
const VIA_PHRASES: Record<CatchUpVia, string> = {
  call: 'by phone',
  video: 'on video',
  message: 'by message',
  email: 'by email',
  inPerson: 'in person',
  other: '',
}

/** "1 day", "2 weeks", "3 months": the coarsest unit that is still honest. */
export function span(days: number): string {
  const n = Math.abs(Math.round(days))
  const unit = (count: number, word: string) => `${count} ${word}${count === 1 ? '' : 's'}`
  if (n < 14) return unit(n, 'day')
  if (n < 60) return unit(Math.floor(n / 7), 'week')
  return unit(Math.floor(n / 30), 'month')
}

/** Where somebody stands: "Due today", "2 weeks overdue", "Due in 4 days". */
export function lateness(p: Pick<PersonDue, 'daysOver'>): string {
  if (p.daysOver === 0) return 'Due today'
  if (p.daysOver > 0) return `${span(p.daysOver)} overdue`
  return `Due in ${span(-p.daysOver)}`
}

/** "Every 2 weeks", "Every 30 days", from a frequency in days. */
export function every(days: number): string {
  if (days % 7 === 0 && days < 60) return days === 7 ? 'Every week' : `Every ${days / 7} weeks`
  return days === 1 ? 'Every day' : `Every ${days} days`
}

/** "Last caught up 3 Oct by phone", or "No catch-up yet". */
export function lastLine(
  p: Pick<PersonDue, 'lastContact' | 'lastVia'>,
  friendly: (iso: string) => string,
): string {
  if (!p.lastContact) return 'No catch-up yet'
  const how = p.lastVia ? VIA_PHRASES[p.lastVia] : ''
  return `Last caught up ${friendly(p.lastContact)}${how ? ` ${how}` : ''}`
}

/** Where an automatic row came from, for the line under it. */
export function catchUpSource(source: LogSource | null | undefined): string {
  switch (source?.type) {
    case 'entry':
      return 'From your journal'
    case 'thread':
      return 'From mail you sent'
    case 'event':
      return 'From your calendar'
    case 'task':
      return 'From a task you ticked off'
    default:
      return ''
  }
}

/**
 * A "which Ravi?" proposal answered with one of them: the record to accept,
 * naming the person chosen. `null` for somebody who is not one of the choices.
 */
export function choose(proposed: ProposedLog, itemId: string): ProposedLog | null {
  if (!proposed.candidates?.some((c) => c.itemId === itemId)) return null
  return { ...proposed, log: { ...proposed.log, itemId } }
}
