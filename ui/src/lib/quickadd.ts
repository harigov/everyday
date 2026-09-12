// The quick-add line.
//
// Adding one task should cost one line and one Enter, and adding twelve
// should cost twelve lines and twelve Enters. That rules out a dialog with
// six fields, so the fields live *in* the line as sigil-prefixed tokens and
// this module pulls them back out:
//
//   Book the flights #travel !high ~90m @fri @16:30
//   └── title ──────┘ └tag─┘ └prio┘ └est┘ └─when──┘
//
// Rules that keep it predictable:
//
//   * A token counts only if the sigil starts a whitespace-delimited word,
//     so `C#`, `10~15 people` and an email address survive untouched.
//   * A token that is not understood is left in the title rather than
//     silently dropped. Losing the word "urgent" because someone typed
//     `!urgnt` would be much worse than not colouring the flag.
//   * Everything is locale-free. `@12/09` is September in half the world and
//     December in the other half, so it is simply not accepted; `@2026-09-12`
//     and `@3d` are.
//
// Parsing happens in the interface rather than the core because it is a
// property of this input box, not of the domain: the CLI and any future
// importer want a `Task`, not a line of shorthand.

import type { Priority, TaskStatus } from './types'
import { isoDate, todayIso } from './time'

/** What one quick-add line means. */
export interface QuickAdd {
  title: string
  tags: string[]
  priority: Priority
  estimateMinutes: number | null
  /** `YYYY-MM-DD`. */
  dueDate: string | null
  /** `HH:MM:SS`. */
  dueTime: string | null
  status: TaskStatus | null
}

const EMPTY: QuickAdd = {
  title: '',
  tags: [],
  priority: 'none',
  estimateMinutes: null,
  dueDate: null,
  dueTime: null,
  status: null,
}

/** What the hint under the input says. Kept beside the grammar it describes. */
export const QUICK_ADD_HINT = '#tag  !high  ~90m  @fri  @16:30'

const PRIORITIES: Record<string, Priority> = {
  urgent: 'urgent',
  p1: 'urgent',
  high: 'high',
  hi: 'high',
  p2: 'high',
  medium: 'medium',
  med: 'medium',
  p3: 'medium',
  low: 'low',
  lo: 'low',
  p4: 'low',
  none: 'none',
  p5: 'none',
}

const STATUSES: Record<string, TaskStatus> = {
  backlog: 'backlog',
  someday: 'backlog',
  todo: 'todo',
  next: 'todo',
  doing: 'doing',
  wip: 'doing',
  started: 'doing',
  blocked: 'blocked',
  waiting: 'blocked',
  done: 'done',
}

// Sunday-first, matching `Date.getDay()`.
const WEEKDAYS = ['sunday', 'monday', 'tuesday', 'wednesday', 'thursday', 'friday', 'saturday']

function pad(n: number): string {
  return String(n).padStart(2, '0')
}

function addDays(iso: string, days: number): string {
  const [y, m, d] = iso.split('-').map(Number)
  // Constructing from parts and letting Date normalise the overflow is what
  // makes month ends and leap days correct without a calendar table.
  return isoDate(new Date(y ?? 1970, (m ?? 1) - 1, (d ?? 1) + days))
}

/**
 * `@…` as a calendar date. Returns null for anything not understood, which
 * leaves the token in the title.
 */
export function parseDueDate(word: string, today = todayIso()): string | null {
  const w = word.toLowerCase()

  if (w === 'today' || w === 'tod') return today
  if (w === 'tomorrow' || w === 'tom' || w === 'tmr') return addDays(today, 1)

  // An explicit calendar date, and only in the unambiguous spelling.
  if (/^\d{4}-\d{2}-\d{2}$/.test(w)) {
    const [y, m, d] = w.split('-').map(Number)
    const probe = new Date(y!, m! - 1, d)
    // Rejects 2026-02-31, which Date would silently roll into March.
    return probe.getMonth() === m! - 1 && probe.getDate() === d! ? w : null
  }

  // Relative: `@3d`, `@+3`, `@2w`.
  const relative = /^\+?(\d{1,3})(d|w)?$/.exec(w)
  if (relative) {
    const n = Number(relative[1])
    return addDays(today, relative[2] === 'w' ? n * 7 : n)
  }

  // A weekday name or its first three letters. "Friday" said on a Friday
  // means today, not a week away -- which is what people mean by it.
  const index = WEEKDAYS.findIndex((d) => d === w || (w.length === 3 && d.startsWith(w)))
  if (index >= 0) {
    const [y, m, d] = today.split('-').map(Number)
    const ahead = (index - new Date(y!, m! - 1, d).getDay() + 7) % 7
    return addDays(today, ahead)
  }

  return null
}

/** `@…` as a time of day: `16:30`, `9am`, `9:30pm`. */
export function parseTime(word: string): string | null {
  const m = /^(\d{1,2})(?::(\d{2}))?(am|pm)?$/i.exec(word)
  if (!m) return null
  let hour = Number(m[1])
  const minute = Number(m[2] ?? 0)
  const meridiem = m[3]?.toLowerCase()

  if (meridiem) {
    if (hour < 1 || hour > 12) return null
    if (meridiem === 'pm' && hour !== 12) hour += 12
    if (meridiem === 'am' && hour === 12) hour = 0
  } else if (!m[2]) {
    // A bare number is a date offset (`@3` = in three days), never a time.
    // Only `@9am` or `@9:00` are hours.
    return null
  }
  if (hour > 23 || minute > 59) return null
  return `${pad(hour)}:${pad(minute)}:00`
}

/** `~…` as an effort in minutes: `90`, `90m`, `2h`, `1h30`, `1h30m`. */
export function parseEstimate(word: string): number | null {
  const w = word.toLowerCase()
  const hm = /^(\d{1,3})h(?:(\d{1,2})m?)?$/.exec(w)
  if (hm) return Number(hm[1]) * 60 + Number(hm[2] ?? 0)
  const mins = /^(\d{1,4})m?$/.exec(w)
  if (mins) return Number(mins[1])
  return null
}

/**
 * Pull the fields out of a quick-add line.
 *
 * Never throws and never returns a partially-applied result: whatever is not
 * recognised stays in the title, so nothing a person typed disappears.
 */
export function parseQuickAdd(line: string, today = todayIso()): QuickAdd {
  const out: QuickAdd = { ...EMPTY, tags: [] }
  const title: string[] = []

  for (const word of line.split(/\s+/)) {
    if (!word) continue
    const body = word.slice(1)
    const sigil = word[0]

    if (sigil === '#' && body) {
      // Tags are compared case-insensitively everywhere, but stored as
      // typed: `#Q3-planning` should read back the way it was written.
      if (!out.tags.some((t) => t.toLowerCase() === body.toLowerCase())) out.tags.push(body)
      continue
    }

    if (sigil === '!' && body) {
      const key = body.toLowerCase()
      if (key in PRIORITIES) {
        out.priority = PRIORITIES[key]!
        continue
      }
      if (key in STATUSES) {
        out.status = STATUSES[key]!
        continue
      }
      // Not a flag we know -- so it is an exclamation, and it belongs to the
      // title.
      title.push(word)
      continue
    }

    if (sigil === '~' && body) {
      const minutes = parseEstimate(body)
      if (minutes !== null && minutes > 0) {
        out.estimateMinutes = minutes
        continue
      }
      title.push(word)
      continue
    }

    if (sigil === '@' && body) {
      const time = parseTime(body)
      if (time) {
        out.dueTime = time
        continue
      }
      const date = parseDueDate(body, today)
      if (date) {
        out.dueDate = date
        continue
      }
      title.push(word)
      continue
    }

    title.push(word)
  }

  // A time with no date means today: "@16:30" is this afternoon, not an hour
  // floating free of any day.
  if (out.dueTime && !out.dueDate) out.dueDate = today

  out.title = title.join(' ').trim()
  return out
}
