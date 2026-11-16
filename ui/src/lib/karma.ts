// Karma, as the interface needs it: what a tracker is worth when it does not
// say, how a day's net is drawn on the heatmap, and -- for the mock backend
// only -- the scoring rules themselves.
//
// The rules belong to `karmayoga_core::karma`, and the real backend never
// asks this file for a score. They are mirrored here for two readers: the
// tracker editor, which says what "Default" means for the tracker in front
// of it, and `mock.ts`, which has to answer `karma` from its own records the
// way it answers every other command. Keep the two in step;
// `scripts/karma.test.mjs` pins the same cases the Rust tests do.

import type { Deed, DeedKind, KarmaDay, Tracker, TrackerDay } from './types'

/** The most a day can earn, after every source's own cap. */
export const DAY_CEILING = 100

export const TASK_POINTS = 5
export const TASK_PURPOSE_BONUS = 5
export const HABIT_DEFAULT = 5
/** The most one tracker can be set to earn or cost in a day, either way. */
export const HABIT_LIMIT = 50
export const JOURNAL_POINTS = 10
export const TIME_POINTS_PER_HOUR = 5
export const FINISHED_POINTS = 10

/** How much of each kind a day can earn. Losses are not capped. */
export const DEED_CAPS: Record<DeedKind, number> = {
  task: 40,
  habit: 40,
  journal: JOURNAL_POINTS,
  time: 20,
  finished: 20,
}

/** A short name for each kind, for a day's breakdown. */
export const DEED_LABELS: Record<DeedKind, string> = {
  task: 'Tasks',
  habit: 'Habits',
  journal: 'Journal',
  time: 'Time on goals',
  finished: 'Finished',
}

function dailyTarget(tracker: Tracker) {
  return (tracker.targets ?? []).find((t) => t.per === 'day' && (t.tally ?? 'value') === 'value')
}

/** A maximum on some target and a minimum on none, whatever the period --
 *  a habit being kept down: `karma::limit_shaped`. */
function limitShaped(tracker: Tracker): boolean {
  const targets = tracker.targets ?? []
  return targets.some((t) => t.max != null) && targets.every((t) => t.min == null)
}

/** What a tracker earns (positive) or costs (negative) when its own
 *  `karma` is not set -- `karma::default_points`. */
export function defaultKarma(tracker: Tracker): number {
  const limit = limitShaped(tracker)
  const hasMin = (tracker.targets ?? []).some((t) => t.min != null)
  const source = tracker.source?.type ?? 'manual'
  if (source === 'manual') {
    if (tracker.kind === 'check' || tracker.kind === 'amount') {
      return limit ? -HABIT_DEFAULT : HABIT_DEFAULT
    }
    return 0
  }
  if (source === 'screenTime' || source === 'health') {
    if (limit) return -HABIT_DEFAULT
    if (hasMin) return HABIT_DEFAULT
  }
  return 0
}

/** The tracker's own setting, or its default, held to the limit. */
export function karmaOf(tracker: Tracker): number {
  const k = tracker.karma ?? defaultKarma(tracker)
  return Math.max(-HABIT_LIMIT, Math.min(HABIT_LIMIT, Math.trunc(k)))
}

/** What a finished task is worth: more when it was filed under a goal or a
 *  role. */
export function taskPoints(purposed: boolean): number {
  return TASK_POINTS + (purposed ? TASK_PURPOSE_BONUS : 0)
}

/** What one tracker's day is worth -- `karma::habit_points`. */
export function habitPoints(tracker: Tracker, day: TrackerDay | undefined): number {
  const points = karmaOf(tracker)
  if (!day || day.count === 0 || points === 0) return 0
  const value =
    tracker.kind === 'check' ? day.count : tracker.kind === 'scale' ? day.sum / day.count : day.sum
  // A deliberate nought -- not done, no drinks, no minutes -- is a day it
  // did not happen, for every kind.
  if (day.sum <= 0) return 0
  const target = dailyTarget(tracker)
  const standing = !target
    ? null
    : target.min != null && value < target.min
      ? 'short'
      : target.max != null && value > target.max
        ? 'over'
        : 'within'
  const counts =
    points > 0
      ? standing === null || standing === 'within'
      : standing !== 'short' && standing !== 'within'
  return counts ? points : 0
}

/** Score a day from its deeds -- `KarmaDay::score`. */
export function scoreDay(date: string, deeds: Deed[]): KarmaDay {
  const gains = new Map<DeedKind, number>()
  let lost = 0
  for (const d of deeds) {
    if (d.points >= 0) gains.set(d.kind, (gains.get(d.kind) ?? 0) + d.points)
    else lost += d.points
  }
  let earned = 0
  for (const [kind, sum] of gains) earned += Math.min(sum, DEED_CAPS[kind])
  earned = Math.min(earned, DAY_CEILING)
  const sorted = [...deeds].sort(
    (a, b) => Number(b.points >= 0) - Number(a.points >= 0) || b.points - a.points,
  )
  return { date, earned, lost, net: earned + lost, deeds: sorted }
}

/** "+5", "−20", "0": a signed number the way a person reads one, with a
 *  real minus sign rather than a hyphen. */
export function signed(n: number): string {
  if (n > 0) return `+${n.toLocaleString()}`
  if (n < 0) return `−${Math.abs(n).toLocaleString()}`
  return '0'
}

/**
 * Which step of the heatmap a day's net falls on: `1..4` above zero,
 * `-1..-4` below, `0` for a day that came to nothing either way.
 *
 * Fixed bands rather than a scale fitted to the busiest day, so a square
 * means the same thing in March as in June, and a quiet fortnight does not
 * make an ordinary day look like a great one. The bands follow the day's
 * ceiling: a quarter of a well-lived day is a step.
 */
export function karmaLevel(net: number): number {
  if (net === 0) return 0
  const step = Math.min(4, Math.ceil(Math.abs(net) / (DAY_CEILING / 4)))
  return net > 0 ? step : -step
}
