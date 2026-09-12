// Streaks, hit rates, and the year behind a habit.
//
// Pure, and separate from `tracking.svelte.ts` for the reason `tracker.ts`
// is: this is a set of rules, and the failures of a rule are quiet. A streak
// that counts a rest day as a break is not a crash, it is a number that is
// wrong by one and discouraging by a lot. A hit rate computed over periods
// that do not exist yet reports a good week as a bad month. None of that
// fails to compile and none of it throws, which is exactly the sort of thing
// this codebase puts in a `.ts` file with a test beside it.
//
// # What a period is
//
// A `Cadence` is a count and a period — three times a week. The arithmetic
// therefore has to agree with a *calendar*, and the one thing calendars
// disagree about is which day a week starts on. That is already a setting
// (`localeWeekStart`), so it is a parameter here rather than an assumption:
// counting a Sunday run into the wrong week is how a streak breaks for
// somebody in one country and holds for somebody in another.
//
// # What counts as a hit
//
// A day with any reading at all. Not the value: a `Check` records a 1, an
// `Amount` records 30, and a `Scale` records how bad the headache was — and
// "did I do it" is the same question in all three. A tracker with a daily
// `target` is the one exception, and it is deliberately *not* applied here:
// the target drives the ring on the chip, and folding it into the streak
// would mean a 25-minute run on a 30-minute target breaking a chain that a
// day off would not.

import { addDays, isoDate, startOfDay, startOfWeek } from './time'
import type { Cadence, Period, TrackerDay } from './types'

/** One period of a habit, and whether it was met. */
export interface HabitPeriod {
  /** The first day of the period, `YYYY-MM-DD`. Its identity. */
  start: string
  /** The last day, inclusive. */
  end: string
  /** Days in this period with at least one reading. */
  hits: number
  /** How many were asked for. */
  wanted: number
  met: boolean
  /**
   * Whether the period is still running.
   *
   * A week you are three days into is not a week you failed, and counting it
   * as one would make every streak break on a Tuesday.
   */
  partial: boolean
}

/** What the habits list draws for one tracker. */
export interface HabitSummary {
  /** Consecutive met periods ending at the current one. */
  streak: number
  /** The longest run of met periods anywhere in the window. */
  best: number
  /** Met periods over closed ones, `0..=1`. `null` when none have closed. */
  rate: number | null
  /** Periods that closed within the window, newest last. */
  periods: HabitPeriod[]
  /** Days with at least one reading, newest last. What the heatmap draws. */
  days: string[]
}

/** The first day of the period `iso` falls in. */
export function periodStart(iso: string, per: Period, weekStart: number): string {
  if (per === 'day') return iso
  if (per === 'week') return startOfWeek(iso, weekStart)
  const at = startOfDay(iso)
  return isoDate(new Date(at.getFullYear(), at.getMonth(), 1))
}

// Not exported: `periodEnd` is what everything outside this module actually
// wants, and it is a wrapper over this rather than a caller of it.
function nextPeriod(start: string, per: Period): string {
  if (per === 'day') return addDays(start, 1)
  if (per === 'week') return addDays(start, 7)
  const at = startOfDay(start)
  return isoDate(new Date(at.getFullYear(), at.getMonth() + 1, 1))
}

/** The last day of the period beginning at `start`, inclusive. */
export function periodEnd(start: string, per: Period): string {
  return addDays(nextPeriod(start, per), -1)
}

/**
 * Every period between two days, oldest first.
 *
 * Bounded rather than open-ended: the caller has already chosen a window,
 * and a habit tracked for three years should not cost a hundred and fifty
 * objects to draw one number.
 */
export function periodsBetween(from: string, to: string, per: Period, weekStart: number): string[] {
  const out: string[] = []
  let at = periodStart(from, per, weekStart)
  // A hard cap, because the only way this loop does not terminate is a bad
  // date string, and an interface that hangs is worse than one that draws
  // too little.
  for (let guard = 0; at <= to && guard < 1000; guard++) {
    out.push(at)
    at = nextPeriod(at, per)
  }
  return out
}

/**
 * Fold a window of daily aggregates into what a habits row draws.
 *
 * `days` is what `tracker_days` already answers — one row per tracker per
 * day — filtered to one tracker by the caller. `today` decides which period
 * is the current one, and so which is allowed to be incomplete.
 */
export function summarise(
  days: TrackerDay[],
  cadence: Cadence | null | undefined,
  window: { from: string; to: string; today: string },
  weekStart: number,
): HabitSummary {
  const per: Period = cadence?.per ?? 'day'
  const wanted = Math.max(1, cadence?.times ?? 1)

  // A day appears once however many readings it holds: "did I do it" is not
  // "how many times did I write it down".
  const hitDays = [...new Set(days.filter((d) => d.count > 0).map((d) => d.date))].sort()
  const current = periodStart(window.today, per, weekStart)

  const periods: HabitPeriod[] = periodsBetween(window.from, window.to, per, weekStart).map(
    (start) => {
      const end = periodEnd(start, per)
      const hits = hitDays.filter((d) => d >= start && d <= end).length
      return {
        start,
        end,
        hits,
        wanted,
        met: hits >= wanted,
        // The period holding today is still running, and so is anything
        // after it — a window that runs into next week has periods that
        // have not begun, and neither kind is a failure.
        partial: start >= current,
      }
    },
  )

  // The streak walks back from the current period. The current one counts
  // when it is already met and is skipped when it is not: three of three
  // runs done on a Wednesday is a streak, and one of three on a Wednesday is
  // not yet a break.
  let streak = 0
  for (let i = periods.length - 1; i >= 0; i--) {
    const p = periods[i]!
    if (p.start > current) continue
    if (p.met) {
      streak++
      continue
    }
    if (p.partial) continue
    break
  }

  let best = 0
  let run = 0
  for (const p of periods) {
    if (p.met) {
      run++
      best = Math.max(best, run)
    } else if (!p.partial) {
      run = 0
    }
  }

  // Rate over *closed* periods only. Counting the week you are two days into
  // as a miss reports every Tuesday as a decline.
  const closed = periods.filter((p) => !p.partial)
  const rate = closed.length === 0 ? null : closed.filter((p) => p.met).length / closed.length

  return { streak, best, rate, periods, days: hitDays }
}

/** How a streak reads, in the period's own words. */
export function describeStreak(streak: number, per: Period): string {
  if (streak === 0) return 'no streak'
  const noun = per === 'day' ? 'day' : per === 'week' ? 'week' : 'month'
  return `${streak} ${noun}${streak === 1 ? '' : 's'}`
}
