// Streaks, hit rates, progress against a target, and the year behind a habit.
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
// A `Target` is a bound and a period: at least three days a week, at most an
// hour a day, twelve a year. The arithmetic therefore has to agree with a
// *calendar*, and the one thing calendars disagree about is which day a week
// starts on. That is already a setting (`localeWeekStart`), so it is a
// parameter here rather than an assumption: counting a Sunday run into the
// wrong week is how a streak breaks for somebody in one country and holds
// for somebody in another.
//
// # What a period's number is
//
// Either the days in it with something recorded (`tally: 'days'` -- "three
// runs a week", where a short run and a long one are both a run), or the
// tracker's own aggregate over it (`tally: 'value'` -- minutes summed, a
// severity averaged). A day counts as recorded when it holds a real value:
// a check ticked as *not* done is a zero, and is not a day it was done.
//
// # When a period is decided
//
// A closed period is met or missed. The one still running usually is not
// decided yet -- a week you are three days into is not a week you failed --
// except that passing a maximum cannot be undone, and reaching a minimum
// with no maximum above it cannot either. Everything else in the current
// period is open, and neither extends nor breaks a streak.
//
// And one honest exception for limits. A closed period of a hand-recorded
// tracker with only a maximum, and nothing recorded in it at all, is not a
// period you stayed under an hour of TV: it is one nobody wrote anything
// down in. It reads as unrecorded and is left out of the streak and the rate
// both, rather than being counted as a success exactly when you stopped
// paying attention.

import { addDays, isoDate, startOfDay, startOfWeek } from './time'
import { aggregateOf } from './types'
import type { Period, Target, Tracker, TrackerDay } from './types'

/** Where a period's number stands against its target's bounds. */
export type Standing = 'short' | 'within' | 'over'

/** Whether a period has been decided, and which way. */
export type PeriodState = 'met' | 'missed' | 'open' | 'unrecorded'

/** One period of a tracker against one target. */
export interface HabitPeriod {
  /** The first day of the period, `YYYY-MM-DD`. Its identity. */
  start: string
  /** The last day, inclusive. */
  end: string
  /** The period's number, under the target's tally. */
  value: number
  standing: Standing
  state: PeriodState
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
  /** Met periods over decided closed ones, `0..=1`. `null` when none have. */
  rate: number | null
  /** Every period in the window, newest last. */
  periods: HabitPeriod[]
  /** Days with something recorded, newest last. What the heatmap draws. */
  days: string[]
}

/** What a tracker with no target of its own is held to: some of it, daily. */
export const DAILY: Target = { min: 1, per: 'day', tally: 'days' }

/**
 * The target a streak and a hit rate are counted against.
 *
 * Not simply the first one. A minimum amount per day -- "30 minutes a day",
 * which is what every tracker's old daily goal became -- drives the ring on
 * its chip and is deliberately kept out of the streak: counting it would
 * make a 25-minute run break a chain that a day off would not. So a days
 * target comes first, then anything that is not that ring (a limit, a
 * weekly amount, a yearly count), and failing both the tracker is held to
 * `DAILY`, which is what a streak was before targets existed.
 */
export function streakTarget(tracker: Pick<Tracker, 'targets'>): Target {
  const all = tracker.targets ?? []
  const ring = (t: Target) => t.per === 'day' && t.tally === 'value' && t.max == null
  return all.find((t) => t.tally === 'days') ?? all.find((t) => !ring(t)) ?? DAILY
}

/** The target a view with room for one draws. */
export function primaryTarget(tracker: Pick<Tracker, 'targets'>): Target | null {
  return tracker.targets?.[0] ?? null
}

/** The first day of the period `iso` falls in. */
export function periodStart(iso: string, per: Period, weekStart: number): string {
  if (per === 'day') return iso
  if (per === 'week') return startOfWeek(iso, weekStart)
  const at = startOfDay(iso)
  if (per === 'month') return isoDate(new Date(at.getFullYear(), at.getMonth(), 1))
  if (per === 'quarter') {
    return isoDate(new Date(at.getFullYear(), Math.floor(at.getMonth() / 3) * 3, 1))
  }
  return isoDate(new Date(at.getFullYear(), 0, 1))
}

// Not exported: `periodEnd` is what everything outside this module actually
// wants, and it is a wrapper over this rather than a caller of it.
function nextPeriod(start: string, per: Period): string {
  if (per === 'day') return addDays(start, 1)
  if (per === 'week') return addDays(start, 7)
  const at = startOfDay(start)
  const months = per === 'month' ? 1 : per === 'quarter' ? 3 : 12
  return isoDate(new Date(at.getFullYear(), at.getMonth() + months, 1))
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

/** Days from `a` to `b` inclusive. Local dates, so no daylight-saving drift. */
function spanDays(a: string, b: string): number {
  return Math.round((startOfDay(b).getTime() - startOfDay(a).getTime()) / 86_400_000) + 1
}

/**
 * Did this day hold a real value?
 *
 * A severity of 0 is a real answer -- "no headache today" -- so a scale
 * counts any reading. Everywhere else a zero is a check deliberately
 * ticked as not done, and that is not a day it was done.
 */
export function recorded(day: TrackerDay, tracker: Pick<Tracker, 'kind'>): boolean {
  return tracker.kind === 'scale' ? day.count > 0 : day.sum > 0
}

/**
 * The number a stretch of days comes to under a target's tally.
 *
 * `days` must already be the ones inside the stretch and belong to one
 * tracker. A check always counts days, whatever the target says: a tick has
 * no value worth adding up.
 */
export function periodValue(
  days: TrackerDay[],
  tracker: Pick<Tracker, 'kind'>,
  tally: Target['tally'],
): number {
  if (tally === 'days' || tracker.kind === 'check') {
    return new Set(days.filter((d) => recorded(d, tracker)).map((d) => d.date)).size
  }
  const sum = days.reduce((n, d) => n + d.sum, 0)
  if (aggregateOf(tracker.kind) !== 'mean') return sum
  // A mean over every reading in the period, not a mean of daily means: a
  // day with four headaches says more than a day with one.
  const count = days.reduce((n, d) => n + d.count, 0)
  return count === 0 ? 0 : sum / count
}

/** Where `value` stands. Both bounds are inclusive. */
export function standingOf(target: Target, value: number): Standing {
  if (target.min != null && value < target.min) return 'short'
  if (target.max != null && value > target.max) return 'over'
  return 'within'
}

function stateOf(
  target: Target,
  value: number,
  partial: boolean,
  anything: boolean,
  manual: boolean,
): PeriodState {
  const standing = standingOf(target, value)
  if (!partial) {
    if (manual && target.min == null && !anything) return 'unrecorded'
    return standing === 'within' ? 'met' : 'missed'
  }
  // Still running. Past a maximum is past it for good; a minimum reached
  // with no maximum above it is reached for good. Anything else can go
  // either way before the period ends.
  if (standing === 'over') return 'missed'
  if (target.max == null && standing === 'within') return 'met'
  return 'open'
}

/**
 * Fold a window of daily aggregates into what a habits row draws.
 *
 * `days` is what `tracker_days` already answers -- one row per tracker per
 * day -- filtered to one tracker by the caller. `today` decides which
 * period is the current one, and so which is allowed to be incomplete. With
 * no target, the tracker is held to `DAILY`.
 */
export function summarise(
  days: TrackerDay[],
  tracker: Pick<Tracker, 'kind' | 'source'>,
  target: Target | null | undefined,
  window: { from: string; to: string; today: string },
  weekStart: number,
): HabitSummary {
  const goal = target ?? DAILY
  const manual = !tracker.source || tracker.source.type === 'manual'
  const hitDays = [...new Set(days.filter((d) => recorded(d, tracker)).map((d) => d.date))].sort()
  const current = periodStart(window.today, goal.per, weekStart)

  const periods: HabitPeriod[] = periodsBetween(window.from, window.to, goal.per, weekStart)
    // A closed period that began before the window only has some of its
    // days in `days`, and would read as missed for the want of the rest.
    // The current one is kept regardless; a caller drawing a year's target
    // asks for the year.
    .filter((start) => start >= window.from || start === current)
    .map((start) => {
      const end = periodEnd(start, goal.per)
      const inside = days.filter((d) => d.date >= start && d.date <= end)
      const value = periodValue(inside, tracker, goal.tally)
      // The period holding today is still running, and so is anything
      // after it -- a window that runs into next week has periods that
      // have not begun, and neither kind is a failure.
      const partial = start >= current
      return {
        start,
        end,
        value,
        standing: standingOf(goal, value),
        state: stateOf(
          goal,
          value,
          partial,
          inside.some((d) => d.count > 0),
          manual,
        ),
        partial,
      }
    })

  // The streak walks back from the current period. A met period extends
  // it, a missed one ends it, and one not yet decided -- or never recorded
  // -- is stepped over: one of three runs on a Wednesday is not yet a break.
  let streak = 0
  for (let i = periods.length - 1; i >= 0; i--) {
    const p = periods[i]!
    if (p.start > current) continue
    if (p.state === 'met') streak++
    else if (p.state === 'missed') break
  }

  let best = 0
  let run = 0
  for (const p of periods) {
    if (p.state === 'met') {
      run++
      best = Math.max(best, run)
    } else if (p.state === 'missed') {
      run = 0
    }
  }

  // Rate over *closed*, decided periods only. Counting the week you are two
  // days into as a miss reports every Tuesday as a decline.
  const decided = periods.filter((p) => !p.partial && (p.state === 'met' || p.state === 'missed'))
  const rate =
    decided.length === 0 ? null : decided.filter((p) => p.state === 'met').length / decided.length

  return { streak, best, rate, periods, days: hitDays }
}

/** Where the current period of one target has got to. */
export interface TargetProgress {
  target: Target
  start: string
  end: string
  value: number
  standing: Standing
  state: PeriodState
  /**
   * What the minimum would have come to by today at an even pace, or `null`
   * where pace means nothing: a daily target, or one with no minimum.
   *
   * "5 of 12 books" means nothing without the date. On the 26th of
   * September, 9 would be on pace.
   */
  expected: number | null
}

/**
 * The current period of `target`, measured.
 *
 * `days` must cover at least the current period -- from `periodStart(today)`
 * -- for the tracker the target belongs to.
 */
export function progress(
  days: TrackerDay[],
  tracker: Pick<Tracker, 'kind' | 'source'>,
  target: Target,
  today: string,
  weekStart: number,
): TargetProgress {
  const start = periodStart(today, target.per, weekStart)
  const end = periodEnd(start, target.per)
  const inside = days.filter((d) => d.date >= start && d.date <= today)
  const value = periodValue(inside, tracker, target.tally)
  const manual = !tracker.source || tracker.source.type === 'manual'
  const expected =
    target.per === 'day' || target.min == null
      ? null
      : (target.min * spanDays(start, today)) / spanDays(start, end)
  return {
    target,
    start,
    end,
    value,
    standing: standingOf(target, value),
    state: stateOf(
      target,
      value,
      true,
      inside.some((d) => d.count > 0),
      manual,
    ),
    expected,
  }
}

/** How a streak reads, in the period's own words. */
export function describeStreak(streak: number, per: Period): string {
  if (streak === 0) return 'no streak'
  return `${streak} ${per}${streak === 1 ? '' : 's'}`
}
