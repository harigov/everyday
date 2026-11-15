// The arithmetic of tracking, with no state in it.
//
// Split from `tracking.svelte.ts` for the reason `time.ts` is split from
// `calendar.svelte.ts`: this is the part that decides what a number *means*
// -- whether a day of doses adds up or a day of severities averages, whether
// a reading is a moment or a span -- and it is worth being able to test that
// without a store, a backend or a browser.

import { formatMinutes } from './format'
import type { TargetProgress } from './habits'
import type {
  Journal,
  Metric,
  Period,
  Reading,
  Stat,
  Target,
  Tracker,
  TrackerSource,
} from './types'
import { aggregateOf } from './types'

/** Rounded the way a person writes a number: 45, not 45.0000001. */
export function formatNumber(n: number): string {
  if (!Number.isFinite(n)) return '0'
  return String(Math.round(n * 100) / 100)
}

/** How one reading of this tracker reads: `400 mg`, `6/10`, `Done`. */
export function formatValue(tracker: Tracker, value: number): string {
  if (tracker.kind === 'check') return value > 0 ? 'Done' : 'Not done'
  if (tracker.kind === 'scale') return `${formatNumber(value)}/${formatNumber(tracker.scaleMax)}`
  return tracker.unit ? `${formatNumber(value)} ${tracker.unit}` : formatNumber(value)
}

/**
 * How a *day* of this tracker reads, given everything recorded on it.
 *
 * Not the same question as the one above, and the difference is the whole
 * reason `aggregateOf` exists: two doses of 400 mg is "800 mg", two
 * headaches of 3 and 7 is "5/10 avg" and never "10/10", and a habit is a
 * tick rather than a number at all.
 */
export function formatDay(tracker: Tracker, readings: Reading[]): string {
  if (readings.length === 0) return ''
  const total = dayValue(tracker, readings)
  // A check can be recorded as *not* done -- 0 rather than no row at all --
  // which is a different fact from having forgotten to answer, and has to
  // read as one.
  if (tracker.kind === 'check') return readings.some((r) => r.value > 0) ? 'Done' : 'Not done'
  if (tracker.kind === 'scale') {
    const avg = `${formatNumber(total)}/${formatNumber(tracker.scaleMax)}`
    return readings.length > 1 ? `${avg} avg` : avg
  }
  const value = tracker.unit ? `${formatNumber(total)} ${tracker.unit}` : formatNumber(total)
  // "2 x 400 mg" would be a lie when the two doses differed, so the count is
  // shown beside the total rather than as a multiplication.
  return readings.length > 1 ? `${value} · ${readings.length}` : value
}

/** The one number a day is worth, under this tracker's aggregate. */
export function dayValue(tracker: Tracker, readings: Reading[]): number {
  if (readings.length === 0) return 0
  const sum = readings.reduce((n, r) => n + r.value, 0)
  switch (aggregateOf(tracker.kind)) {
    case 'count':
      return readings.length
    case 'mean':
      return sum / readings.length
    default:
      return sum
  }
}

/**
 * Minutes a reading occupies on the calendar, or `null` for a moment.
 *
 * Only a quantity measured in time has a length: "45 min run" is 07:00 to
 * 07:45, while a 500 mg dose is a point however it is measured. Mirrors
 * `Tracker::duration_minutes` in the core, and is the one piece of that
 * logic the interface needs its own copy of, because the grid asks about
 * every reading on screen sixty times a minute.
 */
export function durationMinutes(
  tracker: Pick<Tracker, 'kind' | 'unit'>,
  value: number,
): number | null {
  if (tracker.kind !== 'amount') return null
  const unit = tracker.unit.trim().toLowerCase()
  let minutes: number
  if (['min', 'mins', 'minute', 'minutes'].includes(unit)) minutes = value
  else if (['h', 'hr', 'hrs', 'hour', 'hours'].includes(unit)) minutes = value * 60
  else return null
  return Number.isFinite(minutes) && minutes > 0 ? minutes : null
}

/** Is this a tracker somebody records readings of, rather than a derived one? */
export function isManual(tracker: Pick<Tracker, 'source'>): boolean {
  return !tracker.source || tracker.source.type === 'manual'
}

/**
 * One thing from the Health app a goal can be measured by: a metric's day
 * counted one way.
 *
 * A short list rather than the whole catalogue, because a goal is aimed at
 * what a person already thinks of as a number to hit. Sleep is not here:
 * it is a night, not a day's value of a metric. `unit` is the metric's own
 * canonical one, as `Metric::info` in the core says; the core sets a health
 * tracker's unit from the metric when it is saved, so this only says what
 * the editor reads before then.
 */
export interface HealthMeasure {
  /** What follows `health:` in the picker's value. */
  id: string
  /** What the picker and the tracker it makes call it. */
  name: string
  metric: Metric
  stat: Stat
  unit: string
  /** A name from `tracker-icons.ts`. */
  icon: string
}

/** What a goal can be measured by, in the order the picker lists them. */
export const HEALTH_MEASURES: readonly HealthMeasure[] = [
  {
    id: 'steps',
    name: 'Steps',
    metric: 'steps',
    stat: { type: 'sum' },
    unit: 'count',
    icon: 'steps',
  },
  {
    id: 'active_energy',
    name: 'Active energy',
    metric: 'active_energy',
    stat: { type: 'sum' },
    unit: 'kcal',
    icon: 'flame',
  },
  {
    id: 'exercise_time',
    name: 'Exercise minutes',
    metric: 'exercise_time',
    stat: { type: 'sum' },
    unit: 'min',
    icon: 'run',
  },
  {
    id: 'resting_heart_rate',
    name: 'Resting heart rate',
    metric: 'resting_heart_rate',
    stat: { type: 'mean' },
    unit: 'count/min',
    icon: 'heart',
  },
  {
    id: 'weight',
    name: 'Weight',
    metric: 'weight',
    stat: { type: 'last' },
    unit: 'kg',
    icon: 'scales',
  },
  {
    id: 'glucose_in_range',
    name: 'Glucose time in range',
    metric: 'blood_glucose',
    // The consensus range for glucose, in mg/dL.
    stat: { type: 'inRange', low: 70, high: 180 },
    unit: '%',
    icon: 'target',
  },
  {
    id: 'blood_glucose',
    name: 'Blood glucose (average)',
    metric: 'blood_glucose',
    stat: { type: 'mean' },
    unit: 'mg/dL',
    icon: 'pulse',
  },
]

/** The measure a picker value (`health:steps`) names, if it names one. */
export function healthMeasure(value: string): HealthMeasure | undefined {
  if (!value.startsWith('health:')) return undefined
  const id = value.slice('health:'.length)
  return HEALTH_MEASURES.find((m) => m.id === id)
}

/** The source a tracker measured by `measure` carries. */
export function healthSource(measure: HealthMeasure): Extract<TrackerSource, { type: 'health' }> {
  return { type: 'health', metric: measure.metric, stat: { ...measure.stat } }
}

/**
 * Is this tracker one health reading a day that does not add up across days?
 *
 * An average, a last reading, a lowest or highest, or a share in range is a
 * value of its day; the days of a week are not a sum of them -- seven
 * resting heart rates are not a heart rate -- and a day with no reading is
 * not a zero. So a target on one is only ever counted "a day", and a day
 * with nothing recorded is unrecorded rather than short or within a limit.
 */
export function isDayReading(tracker: Pick<Tracker, 'source'>): boolean {
  const source = tracker.source
  if (source?.type !== 'health') return false
  return ['mean', 'min', 'max', 'last', 'inRange'].includes(source.stat.type)
}

/**
 * A quantity of this tracker, the way a target or a total reads it.
 *
 * Time reads as time -- "1h 30m", not "90 min" -- because a weekly target
 * of an hour or two is thought of in hours. A days tally reads in days.
 */
export function formatAmount(
  tracker: Pick<Tracker, 'kind' | 'unit'>,
  value: number,
  tally: Target['tally'] = 'value',
): string {
  if (tally === 'days' || tracker.kind === 'check') {
    return `${formatNumber(value)} ${value === 1 ? 'day' : 'days'}`
  }
  const perUnit = durationMinutes(tracker, 1)
  if (perUnit !== null) return formatMinutes(value * perUnit)
  return tracker.unit ? `${formatNumber(value)} ${tracker.unit}` : formatNumber(value)
}

/**
 * A target in a sentence: "at least 3 days a week", "between 1h and 2h a
 * week", "at most 1h a day", "at least 12 a year". Mirrors
 * `Target::describe` in the core, which says the same to the assistant.
 */
export function describeTarget(tracker: Pick<Tracker, 'kind' | 'unit'>, target: Target): string {
  const amount = (v: number) => formatAmount(tracker, v, target.tally)
  const { min, max } = target
  // The commonest habit of all, which "at least 1 day a day" is not how
  // anybody says.
  if (target.per === 'day' && min === 1 && max == null) {
    if (target.tally === 'days' || tracker.kind === 'check') return 'every day'
  }
  let bound: string
  if (min != null && max != null) {
    bound = min === max ? `exactly ${amount(min)}` : `between ${amount(min)} and ${amount(max)}`
  } else if (min != null) bound = `at least ${amount(min)}`
  else if (max != null) bound = `at most ${amount(max)}`
  else return 'no target'
  return `${bound} a ${target.per}`
}

const THIS: Record<Period, string> = {
  day: 'today',
  week: 'this week',
  month: 'this month',
  quarter: 'this quarter',
  year: 'this year',
}

/** How a target's current period reads, in the words every view uses. */
export interface ProgressWords {
  /** How far towards the minimum (or the limit, for a maximum), `0..`. */
  ratio: number
  /** Compact, for a sidebar row: "40m/1h", "1/12". */
  short: string
  /** One line: "40m this week · between 1h and 2h a week". */
  line: string
  /**
   * What is worth saying about it, if anything: "over by 20m", "8 behind
   * pace", "on pace", "done". Always words, so the state is never carried
   * by a colour alone.
   */
  status: string | null
  /** Past a maximum: the one state drawn in the danger colour. */
  over: boolean
}

/**
 * Put one target's current period into words.
 *
 * Pace is only spoken of for a month or longer. Over a week it is noise --
 * "0.4 days behind pace" on a Monday -- and a day has none.
 */
export function describeProgress(
  tracker: Pick<Tracker, 'kind' | 'unit'>,
  p: TargetProgress,
): ProgressWords {
  const { target, value } = p
  const amount = (v: number) => formatAmount(tracker, v, target.tally)
  const bare = (v: number) =>
    target.tally === 'days' || tracker.kind === 'check' ? formatNumber(v) : amount(v)
  const bound = target.min ?? target.max ?? 0
  const ratio = bound > 0 ? value / bound : value > 0 ? 1 : 0
  const over = p.standing === 'over'

  let status: string | null = null
  if (over && target.max != null) status = `over by ${amount(value - target.max)}`
  else if (p.state === 'met') status = 'done'
  else if (
    p.expected !== null &&
    (target.per === 'month' || target.per === 'quarter' || target.per === 'year')
  ) {
    const behind = Math.round(p.expected - value)
    status = behind >= 1 ? `${bare(behind)} behind pace` : 'on pace'
  }

  return {
    ratio,
    short: `${bare(value)}/${bare(bound)}`,
    line: `${amount(value)} ${THIS[target.per]} · ${describeTarget(tracker, target)}`,
    status,
    over,
  }
}

/**
 * The trackers one journal's chips should offer, picked out of the vault's
 * own list and in the order they were arranged.
 *
 * Ids naming a tracker that is gone are skipped rather than reported. A
 * stale id is the ordinary consequence of deleting a tracker, and every
 * journal that showed it should not have to be rewritten for that.
 */
export function shownTrackers(journal: Journal | null, all: Tracker[]): Tracker[] {
  if (!journal) return []
  return (
    journal.shownTrackers
      .map((id) => all.find((t) => t.id === id))
      // A derived tracker is never a chip: there is nothing to tick.
      .filter((t): t is Tracker => !!t && !t.archived && isManual(t))
      .sort((a, b) => a.sortOrder - b.sortOrder || a.createdAt.localeCompare(b.createdAt))
  )
}
