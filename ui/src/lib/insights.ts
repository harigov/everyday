// The rules the Overview's mail, meeting and people widgets draw by, apart
// from the store for the reason `dashboard.ts` is: they fail quietly. A
// person counted twice because one list knew their address and the other
// only their name is a wrong ranking that looks right, and a chart bucketed
// into days when it meant weeks is a comb nobody can read. Both are tested
// next door in `scripts/insights.test.mjs`.
//
// The counting itself is in Rust (`everyday_core::insights`), because who a
// message is from and what a meeting is called are sealed. What is here is
// the drawing: how a window becomes columns, and how two lists of people
// become one.

import type { MailActivity, MailCategory, MeetingActivity } from './types'
import { startOfWeek } from './time'

// ── columns over a window ────────────────────────────────────────────────

/**
 * The longest window drawn a column a day. Past it, a column a week: ninety
 * daily columns of mail is a comb, and the question at that range is the
 * trend, not the Tuesday.
 */
export const DAILY_UP_TO = 31

export interface Column {
  date: string
  value: number
}

/**
 * Daily rows as columns: one per day while the window is short, one per week
 * once it is not, each week dated by its first day.
 *
 * A week the window only partly covers -- the first one, usually -- is drawn
 * with the days it has. Padding it with days outside the window would
 * draw mail nobody asked about; dropping it would lose days somebody did.
 */
export function columns<T extends { date: string }>(
  days: T[],
  value: (day: T) => number,
  weekStart: number,
): Column[] {
  if (days.length <= DAILY_UP_TO) return days.map((d) => ({ date: d.date, value: value(d) }))
  const weeks = new Map<string, number>()
  for (const day of days) {
    const week = startOfWeek(day.date, weekStart)
    weeks.set(week, (weeks.get(week) ?? 0) + value(day))
  }
  return [...weeks].map(([date, total]) => ({ date, value: total }))
}

/** "a day" or "a week": what one of `columns`' columns is. */
export function columnUnit(days: number): 'day' | 'week' {
  return days <= DAILY_UP_TO ? 'day' : 'week'
}

// ── kinds of mail ────────────────────────────────────────────────────────

/**
 * What each category is called and coloured, in `Category::ALL`'s order.
 *
 * Colours are the first four of the journal palette, in order, so they are
 * the same four every time and never re-dealt by rank; "not sorted yet" is
 * the neutral, because it is the absence of an answer rather than a fifth.
 */
export const MAIL_KINDS: { key: MailCategory | null; label: string; color: string }[] = [
  { key: 'important', label: 'Important', color: '#c2410c' },
  { key: 'other', label: 'Other', color: '#0f766e' },
  { key: 'newsletter', label: 'Newsletters', color: '#4338ca' },
  { key: 'notification', label: 'Notifications', color: '#a21caf' },
  { key: null, label: 'Not sorted yet', color: 'var(--fg-faint)' },
]

/** The tag a sender's row wears, if their mail is mostly a machine's. */
export function machineTag(category: MailCategory | null): string | null {
  if (category === 'newsletter') return 'Newsletter'
  if (category === 'notification') return 'Notification'
  return null
}

// ── people, from both sides ──────────────────────────────────────────────

export interface Person {
  /** Lower-cased address where either side knew one, else `name:` and the name. */
  key: string
  name: string
  email: string | null
  /** Messages from them. */
  received: number
  /** Messages you sent them. */
  sent: number
  meetings: number
  minutes: number
}

/** How a person is ranked: everything you exchanged, mail and meetings alike. */
export function touchpoints(p: Person): number {
  return p.received + p.sent + p.meetings
}

/**
 * One list of the people you deal with, out of the mail list and the
 * meeting list.
 *
 * Joined on the address wherever the meeting side has one. Where it has only
 * a name -- calendar feeds prefer a name -- the name is matched against the
 * names on your mail, but only when exactly one correspondent carries it: two
 * Sams in your inbox and a "Sam" in a meeting is not a guess worth making,
 * and a wrong join is worse than two rows.
 *
 * Ranked by messages exchanged plus meetings shared, then by time together.
 */
export function inTouch(mail: MailActivity | null, meetings: MeetingActivity | null): Person[] {
  const people = new Map<string, Person>()
  const byName = new Map<string, string[]>()

  for (const c of mail?.correspondents ?? []) {
    people.set(c.email, {
      key: c.email,
      name: c.name || c.email,
      email: c.email,
      received: c.received,
      sent: c.sent,
      meetings: 0,
      minutes: 0,
    })
    if (c.name) {
      const name = c.name.trim().toLowerCase()
      byName.set(name, [...(byName.get(name) ?? []), c.email])
    }
  }

  for (const m of meetings?.people ?? []) {
    const named = byName.get(m.name.trim().toLowerCase())
    const key = m.email ?? (named?.length === 1 ? named[0]! : `name:${m.name.trim().toLowerCase()}`)
    const person = people.get(key)
    if (person) {
      person.meetings += m.meetings
      person.minutes += m.minutes
      // The calendar's name over a bare address: "ana@…" is what mail had
      // when the sender never set one.
      if (person.name === person.email && m.name !== m.email) person.name = m.name
    } else {
      people.set(key, {
        key,
        name: m.name,
        email: m.email ?? null,
        received: 0,
        sent: 0,
        meetings: m.meetings,
        minutes: m.minutes,
      })
    }
  }

  return [...people.values()].sort(
    (a, b) =>
      touchpoints(b) - touchpoints(a) || b.minutes - a.minutes || a.name.localeCompare(b.name),
  )
}
