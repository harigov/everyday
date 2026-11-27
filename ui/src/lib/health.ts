// Pure helpers for Health: ranges, units, names, colours and nutrition
// arithmetic. No state and no `api` -- `health.svelte.ts` and the
// components import these, and `ui/scripts/health.test.mjs` tests them.
//
// The names and signatures below are the contract the components are
// written against (docs/plans/health.md, "The contract"). Members added
// since are marked as such in their own comments.
//
// Two rules run through the file:
//
//   1. The vault stores every value in its metric's canonical unit
//      (kilograms, metres, kilocalories, mg/dL, degrees Celsius) and this is
//      the one place that converts for display and back, the way
//      `jatayu_core::health::units` does on the Rust side. A glucose of
//      5.5 typed as mg/dL when it was mmol/L is a reading eighteen times too
//      low, so there is exactly one table of what converts and how.
//   2. Days travel as `YYYY-MM-DD` in the reader's local zone, as in every
//      other app; arithmetic on them is `time.ts`'s and what a reader sees
//      is `format.ts`'s cached `Intl` formatters.

import { dateFormat, formatClock } from './format'
import { addDays, daysApart, isoDate, locale, startOfDay } from './time'
import type { HealthRange } from './health.svelte'
import type {
  Activity,
  BucketSize,
  Condition,
  DocKind,
  Food,
  HealthUnits,
  LabResult,
  Meal,
  MealItem,
  MealSlot,
  MedRule,
  MedSchedule,
  Medication,
  Metric,
  MetricEntry,
  Nutrients,
  SleepStage,
} from './types'

/** The colour the window tints itself while Health is open. */
export const HEALTH_ACCENT = 'var(--app-health)'

// ── ranges ───────────────────────────────────────────────────────────────

/** How many days each window spans. */
const WINDOW_DAYS: Record<HealthRange, number> = { week: 7, month: 30, quarter: 91, year: 365 }

/**
 * First and last day of the `range` window ending on `anchor`.
 *
 * Rolling, not calendar: a body does not start again on Monday, and a
 * calendar week opened on a Sunday would hold one day -- the resting heart
 * rate measured yesterday would read as "nothing recorded". So "week" is
 * the seven days ending on `anchor`, "month" thirty, "quarter" ninety-one,
 * "year" three hundred and sixty-five. `weekStart` is kept for the
 * signature the components were written against and has nothing to decide.
 */
export function rangeOf(
  range: HealthRange,
  anchor: string,
  weekStart: number,
): { from: string; to: string } {
  void weekStart
  return { from: addDays(anchor, 1 - WINDOW_DAYS[range]), to: anchor }
}

/** `anchor` moved by `delta` whole windows. */
export function stepAnchor(range: HealthRange, anchor: string, delta: number): string {
  return addDays(anchor, WINDOW_DAYS[range] * delta)
}

/** Is this a `YYYY-MM-DD` that names a real day? */
function isDay(value: string): boolean {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(value)) return false
  return isoDate(startOfDay(value)) === value
}

/** "Last 7 days" (or 30, 3 months, 12 months) for a window ending today;
 *  otherwise its dates, "3–9 Nov", with the year only when it is not
 *  `today`'s. */
export function rangeLabel(range: HealthRange, from: string, to: string, today: string): string {
  if (!isDay(from) || !isDay(to)) return ''
  if (to === today) {
    const last: Record<HealthRange, string> = {
      week: 'Last 7 days',
      month: 'Last 30 days',
      quarter: 'Last 3 months',
      year: 'Last 12 months',
    }
    return last[range]
  }
  const year = (d: string) => d.slice(0, 4)
  const otherYear = year(from) !== year(today) || year(to) !== year(today)
  // `formatRange` knows which parts two dates share in this locale -- the
  // month once in "3–9 Nov", twice in "28 Oct – 3 Nov".
  return dateFormat({
    day: 'numeric',
    month: 'short',
    year: otherYear ? 'numeric' : undefined,
  }).formatRange(startOfDay(from), startOfDay(to))
}

/**
 * The local days `from..=to` as instants: midnight at the start of `from` to
 * midnight at the start of the day after `to`. What the commands that take
 * instants (workouts, meals, doses, deleting samples) are given for a range
 * of days. Added after the skeleton.
 */
export function dayInstants(from: string, to: string): { from: string; to: string } {
  return { from: startOfDay(from).toISOString(), to: startOfDay(addDays(to, 1)).toISOString() }
}

/**
 * How wide one point of a series is for a window of `from..=to`: a day up to
 * 92 days (a quarter), a week up to a year, a month beyond -- so a chart
 * never draws more than about a hundred points. Added after the skeleton.
 */
export function bucketFor(from: string, to: string): BucketSize {
  const days = daysApart(from, to) + 1
  if (days <= 92) return 'day'
  if (days <= 366) return 'week'
  return 'month'
}

// ── units ────────────────────────────────────────────────────────────────

/** What a person is shown when they have chosen nothing: the same as
 *  `HealthUnits::default` in the core. */
export const DEFAULT_UNITS: HealthUnits = {
  glucose: 'mg/dL',
  weight: 'kg',
  distance: 'km',
  energy: 'kcal',
  temperature: 'C',
  length: 'cm',
}

/** Which of the person's unit choices a metric answers to. The rest have
 *  none: a heart rate is in beats per minute whoever is looking. */
const UNIT_KIND: Partial<Record<Metric, keyof HealthUnits>> = {
  blood_glucose: 'glucose',
  weight: 'weight',
  lean_body_mass: 'weight',
  walking_running_distance: 'distance',
  cycling_distance: 'distance',
  swimming_distance: 'distance',
  active_energy: 'energy',
  basal_energy: 'energy',
  dietary_energy: 'energy',
  body_temperature: 'temperature',
  wrist_temperature: 'temperature',
  height: 'length',
  waist_circumference: 'length',
}

/** mg/dL in one mmol/L of glucose. */
const MG_DL_PER_MMOL_L = 18.0156
const LB_PER_KG = 2.2046226218
const M_PER_MILE = 1609.344
const KJ_PER_KCAL = 4.184
const M_PER_INCH = 0.0254

/** What a metric's unit is called on screen, where the vault's own spelling
 *  (`count`, `count/min`, `IU`) is not what a reader would say -- chosen by
 *  metric, so it holds whatever the catalogue calls the unit. An empty one is
 *  deliberate: a body mass index has no unit worth writing. */
const LABEL_BY_METRIC: Partial<Record<Metric, string>> = {
  steps: 'steps',
  flights_climbed: 'floors',
  stand_hour: 'hours',
  bmi: '',
  heart_rate: 'bpm',
  resting_heart_rate: 'bpm',
  walking_heart_rate: 'bpm',
  heart_rate_recovery: 'bpm',
  respiratory_rate: 'breaths/min',
  oxygen_saturation: '%',
  body_fat: '%',
  heart_rate_variability: 'ms',
  vo2_max: 'mL/kg/min',
  insulin_basal: 'U',
  insulin_bolus: 'U',
}

/** The words a canonical unit is shown as, for a metric the table above has no word for. */
function unitLabel(metric: Metric, canonical: string): string {
  const own = LABEL_BY_METRIC[metric]
  if (own !== undefined) return own
  switch (canonical) {
    case 'count/min':
      return 'bpm'
    case 'degC':
      return '°C'
    case 'degF':
      return '°F'
    case 'IU':
      return 'U'
    // A bare count is not a unit anyone says out loud.
    case 'count':
    case 'stage':
      return ''
    default:
      return canonical
  }
}

/** The one of a unit that is said for exactly one: "1 step", "1 floor". */
const SINGULAR: Record<string, string> = { steps: 'step', floors: 'floor', hours: 'hour' }

/** The unit `metric` is shown in under `units`; the canonical one (as a
 *  reader writes it: `bpm`, `°C`) when the person has no choice about it. */
export function unitFor(metric: Metric, units: HealthUnits, catalog: MetricEntry[]): string {
  const kind = UNIT_KIND[metric]
  if (kind === 'temperature') return units.temperature === 'F' ? '°F' : '°C'
  if (kind) return units[kind]
  return unitLabel(metric, catalog.find((m) => m.metric === metric)?.unit ?? '')
}

/** A canonical value in the shown unit. Exact: `fromDisplay` undoes it, and
 *  rounding is `formatMetric`'s job. */
export function toDisplay(metric: Metric, value: number, units: HealthUnits): number {
  switch (UNIT_KIND[metric]) {
    case 'glucose':
      return units.glucose === 'mmol/L' ? value / MG_DL_PER_MMOL_L : value
    case 'weight':
      return units.weight === 'lb' ? value * LB_PER_KG : value
    case 'distance':
      // Canonical metres.
      return units.distance === 'mi' ? value / M_PER_MILE : value / 1000
    case 'energy':
      return units.energy === 'kJ' ? value * KJ_PER_KCAL : value
    case 'temperature':
      if (units.temperature !== 'F') return value
      // A wrist reading is a deviation from baseline, not a temperature: a
      // degree warmer is 1.8 degrees warmer in Fahrenheit, and 32 is not
      // added to a difference.
      return metric === 'wrist_temperature' ? value * 1.8 : value * 1.8 + 32
    case 'length':
      // Canonical metres.
      return units.length === 'in' ? value / M_PER_INCH : value * 100
    default:
      return value
  }
}

/** A value typed in the shown unit, back to canonical. */
export function fromDisplay(metric: Metric, value: number, units: HealthUnits): number {
  switch (UNIT_KIND[metric]) {
    case 'glucose':
      return units.glucose === 'mmol/L' ? value * MG_DL_PER_MMOL_L : value
    case 'weight':
      return units.weight === 'lb' ? value / LB_PER_KG : value
    case 'distance':
      return units.distance === 'mi' ? value * M_PER_MILE : value * 1000
    case 'energy':
      return units.energy === 'kJ' ? value / KJ_PER_KCAL : value
    case 'temperature':
      if (units.temperature !== 'F') return value
      return metric === 'wrist_temperature' ? value / 1.8 : (value - 32) / 1.8
    case 'length':
      return units.length === 'in' ? value * M_PER_INCH : value / 100
    default:
      return value
  }
}

/** Most decimals a metric is shown with: enough to see it move, not so many
 *  that the watch's own noise is. Trailing zeros are not drawn. */
const DIGITS: Partial<Record<Metric, number>> = {
  vo2_max: 1,
  respiratory_rate: 1,
  weight: 1,
  body_fat: 1,
  lean_body_mass: 1,
  bmi: 1,
  body_temperature: 1,
  wrist_temperature: 2,
  insulin_basal: 1,
  insulin_bolus: 1,
  dietary_fiber: 1,
  waist_circumference: 1,
}

/** How many decimals `metric` is shown with under `units`. */
export function digitsFor(metric: Metric, units: HealthUnits): number {
  const kind = UNIT_KIND[metric]
  if (kind === 'glucose') return units.glucose === 'mmol/L' ? 1 : 0
  if (kind === 'distance') return 2
  if (kind === 'length' && metric === 'height') return units.length === 'in' ? 1 : 0
  if (kind === 'temperature') return metric === 'wrist_temperature' ? 2 : 1
  return DIGITS[metric] ?? 0
}

const numbers = new Map<string, Intl.NumberFormat>()

/** `value` with at most `digits` decimals, grouped the reader's way: "8,432",
 *  "81.4". Built once per language and width, as `format.ts`'s formatters are. */
export function formatNumber(value: number, digits = 0): string {
  const key = `${locale()}\u0000${digits}`
  let made = numbers.get(key)
  if (!made) {
    made = new Intl.NumberFormat(locale(), { maximumFractionDigits: digits })
    numbers.set(key, made)
  }
  return made.format(value)
}

/** "72 bpm", "81.4 kg", "8,432 steps", "7 h 12 min", "97 %". */
export function formatMetric(
  metric: Metric,
  value: number,
  units: HealthUnits,
  catalog: MetricEntry[],
): string {
  if (!Number.isFinite(value)) return ''
  if (metric === 'sleep_stage') {
    const stage = STAGE_CODES[Math.round(value)]
    return stage ? stageName(stage) : ''
  }
  // Minutes are read as a duration: 72 minutes is "1 h 12 min".
  if (catalog.find((m) => m.metric === metric)?.unit === 'min') return durationText(value)
  const digits = digitsFor(metric, units)
  const shown = toDisplay(metric, value, units)
  const text = formatNumber(shown, digits)
  const unit = unitFor(metric, units, catalog)
  if (!unit) return text
  const scale = 10 ** digits
  return `${text} ${Math.round(shown * scale) / scale === 1 ? (SINGULAR[unit] ?? unit) : unit}`
}

/** "7 h 12 min", "45 min". */
export function durationText(minutes: number): string {
  const total = Math.max(0, Math.round(Number.isFinite(minutes) ? minutes : 0))
  if (total < 60) return `${total} min`
  const hours = Math.floor(total / 60)
  const rest = total % 60
  return rest === 0 ? `${hours} h` : `${hours} h ${String(rest).padStart(2, '0')} min`
}

// ── names and colours ────────────────────────────────────────────────────

const ACTIVITY_NAMES: Record<Activity, string> = {
  walking: 'Walking',
  running: 'Running',
  cycling: 'Cycling',
  swimming: 'Swimming',
  hiking: 'Hiking',
  rowing: 'Rowing',
  elliptical: 'Elliptical',
  stair_climbing: 'Stair climbing',
  strength_training: 'Strength training',
  functional_training: 'Functional training',
  core_training: 'Core training',
  hiit: 'HIIT',
  yoga: 'Yoga',
  pilates: 'Pilates',
  dance: 'Dance',
  martial_arts: 'Martial arts',
  tennis: 'Tennis',
  soccer: 'Soccer',
  basketball: 'Basketball',
  golf: 'Golf',
  cooldown: 'Cooldown',
  mindfulness: 'Mindfulness',
  other: 'Workout',
}

export function activityName(activity: Activity): string {
  return ACTIVITY_NAMES[activity] ?? 'Workout'
}

const STAGE_NAMES: Record<SleepStage, string> = {
  inBed: 'In bed',
  awake: 'Awake',
  asleep: 'Asleep',
  core: 'Core',
  deep: 'Deep',
  rem: 'REM',
}

/** The stages by the number a sleep-stage sample stores (`SleepStage::code`). */
const STAGE_CODES: Record<number, SleepStage> = {
  0: 'inBed',
  1: 'awake',
  2: 'asleep',
  3: 'core',
  4: 'deep',
  5: 'rem',
}

export function stageName(stage: SleepStage): string {
  return STAGE_NAMES[stage] ?? stage
}

// Colour is never the only thing telling two of these apart -- a hypnogram's
// stage is also its height, a glucose band its place in the bar -- but where
// it is the thing, it is chosen to survive a red-green colour-vision
// difference: the pairs that difference confuses (red with green, orange with
// olive) are never neighbours, and every neighbouring pair differs in
// lightness as well as hue. All of them are the theme's own tokens, so they
// follow it into dark and glass themes.

/** Each sleep stage's colour, as a CSS value. Blues for sleep, deepening with
 *  depth; purple for REM; amber for being awake; grey for lying in bed. */
export const SLEEP_STAGE_COLORS: Record<SleepStage, string> = {
  inBed: 'var(--fg-faint)',
  awake: 'var(--warning)',
  asleep: 'color-mix(in oklab, var(--info) 45%, var(--bg-raised))',
  core: 'var(--info)',
  deep: 'color-mix(in oklab, var(--info) 55%, var(--fg))',
  rem: 'var(--aqi-5)',
}

export type GlucoseBand = 'veryLow' | 'low' | 'inRange' | 'high' | 'veryHigh'

/** Which consensus band a reading in mg/dL falls in: under 54 very low, 54 to
 *  69 low, 70 to 180 in range, 181 to 250 high, over 250 very high (Battelino
 *  et al., 2019 -- the bands `glucose.rs` counts time in). */
export function glucoseBand(mgdl: number): GlucoseBand {
  if (mgdl < 54) return 'veryLow'
  if (mgdl < 70) return 'low'
  if (mgdl <= 180) return 'inRange'
  if (mgdl <= 250) return 'high'
  return 'veryHigh'
}

/** The bands in the order a bar stacks them, lowest first. */
export const GLUCOSE_BANDS: readonly GlucoseBand[] = [
  'veryLow',
  'low',
  'inRange',
  'high',
  'veryHigh',
]

/** Reds below range, blue in it, gold and purple above: in range is blue, not
 *  green, so it is never the colour next to red that a red-green difference
 *  would merge. */
export const GLUCOSE_COLORS: Record<GlucoseBand, string> = {
  veryLow: 'var(--aqi-6)',
  low: 'var(--danger)',
  inRange: 'var(--info)',
  high: 'var(--star)',
  veryHigh: 'var(--aqi-5)',
}

const GLUCOSE_BAND_NAMES: Record<GlucoseBand, string> = {
  veryLow: 'Very low',
  low: 'Low',
  inRange: 'In range',
  high: 'High',
  veryHigh: 'Very high',
}

/** "Very low", "In range". Added after the skeleton. */
export function glucoseBandName(band: GlucoseBand): string {
  return GLUCOSE_BAND_NAMES[band]
}

const SLOT_NAMES: Record<MealSlot, string> = {
  breakfast: 'Breakfast',
  lunch: 'Lunch',
  dinner: 'Dinner',
  snack: 'Snack',
}

export function slotName(slot: MealSlot): string {
  return SLOT_NAMES[slot] ?? slot
}

const DOC_KIND_NAMES: Record<DocKind, string> = {
  labReport: 'Lab report',
  visitNote: 'Visit note',
  imaging: 'Imaging',
  discharge: 'Discharge summary',
  prescription: 'Prescription',
  immunization: 'Immunization',
  insurance: 'Insurance',
  fhir: 'Health record',
  ecg: 'ECG',
  other: 'Document',
}

export function docKindName(kind: DocKind): string {
  return DOC_KIND_NAMES[kind] ?? 'Document'
}

/** "Mon" for 1, "Sun" for 7, in the reader's language. */
function weekdayName(day: number): string {
  // 1 January 2024 was a Monday.
  return dateFormat({ weekday: 'short' }).format(new Date(2024, 0, day))
}

function joinList(items: string[]): string {
  return new Intl.ListFormat(locale(), { style: 'long', type: 'conjunction' }).format(items)
}

/** "Every day at 7:00 AM and 7:00 PM", "Mon, Wed, Fri at 8:00 AM",
 *  "As needed" -- times in the reader's own clock. */
export function scheduleText(schedule: MedSchedule): string {
  if (schedule.type === 'asNeeded') return 'As needed'
  const times = [...schedule.times].sort()
  const at = times.length > 0 ? ` at ${joinList(times.map(formatClock))}` : ''
  switch (schedule.type) {
    case 'daily':
      return `Every day${at}`
    case 'weekly': {
      const days = [...new Set(schedule.days)].filter((d) => d >= 1 && d <= 7).sort((a, b) => a - b)
      if (days.length === 0 || days.length === 7) return `Every day${at}`
      if (days.join() === '1,2,3,4,5') return `Weekdays${at}`
      if (days.join() === '6,7') return `Weekends${at}`
      return `${days.map(weekdayName).join(', ')}${at}`
    }
    case 'everyNDays':
      if (schedule.n <= 1) return `Every day${at}`
      return schedule.n === 2 ? `Every other day${at}` : `Every ${schedule.n} days${at}`
  }
}

/** "Empty stomach: nothing to eat for 30 min after", "4 h apart from
 *  calcium, iron". */
export function ruleText(rule: MedRule): string {
  switch (rule.type) {
    case 'emptyStomach':
      return `Empty stomach: nothing to eat for ${formatNumber(rule.minutes)} min after`
    case 'withFood':
      return `With food: a meal within ${formatNumber(rule.minutes)} min`
    case 'apartFrom':
      return `${formatNumber(rule.hours, 1)} h apart from ${rule.tags.join(', ')}`
  }
}

/** What a lab result's trend groups by: its LOINC code, or its name. */
export function labKey(lab: LabResult): string {
  return lab.code || `name:${lab.name.toLowerCase()}`
}

/** Is a result outside its own reference range (or flagged so)? A lab's own
 *  flag wins where it gave one; with none, the numbers decide. */
export function labOutOfRange(lab: LabResult): boolean {
  if (
    lab.flag === 'low' ||
    lab.flag === 'high' ||
    lab.flag === 'abnormal' ||
    lab.flag === 'critical'
  ) {
    return true
  }
  if (lab.flag === 'normal') return false
  if (lab.value === null || lab.value === undefined) return false
  if (lab.refLow !== null && lab.refLow !== undefined && lab.value < lab.refLow) return true
  return lab.refHigh !== null && lab.refHigh !== undefined && lab.value > lab.refHigh
}

// ── what the app is about, in lists ──────────────────────────────────────

/** Every metric, in catalogue order -- to tell a stored one from anything else
 *  (a remembered choice from a build with different metrics). Added after
 *  the skeleton. */
export const METRICS: readonly Metric[] = [
  'steps',
  'walking_running_distance',
  'cycling_distance',
  'swimming_distance',
  'active_energy',
  'basal_energy',
  'exercise_time',
  'stand_time',
  'stand_hour',
  'flights_climbed',
  'time_in_daylight',
  'heart_rate',
  'resting_heart_rate',
  'walking_heart_rate',
  'heart_rate_variability',
  'heart_rate_recovery',
  'vo2_max',
  'respiratory_rate',
  'oxygen_saturation',
  'weight',
  'body_fat',
  'lean_body_mass',
  'bmi',
  'height',
  'waist_circumference',
  'body_temperature',
  'wrist_temperature',
  'blood_pressure_systolic',
  'blood_pressure_diastolic',
  'blood_glucose',
  'insulin_basal',
  'insulin_bolus',
  'dietary_energy',
  'dietary_carbs',
  'dietary_protein',
  'dietary_fat',
  'dietary_fiber',
  'dietary_sugar',
  'dietary_sodium',
  'dietary_caffeine',
  'water',
  'sleep_stage',
  'mindful_minutes',
]

/** The metrics the Vitals pane lists and reads the latest value of, in the
 *  order its tiles are drawn. Added after the skeleton. */
export const VITALS_METRICS: readonly Metric[] = [
  'resting_heart_rate',
  'heart_rate',
  'heart_rate_variability',
  'blood_pressure_systolic',
  'blood_pressure_diastolic',
  'oxygen_saturation',
  'respiratory_rate',
  'body_temperature',
  'wrist_temperature',
  'blood_glucose',
  'weight',
  'body_fat',
  'bmi',
  'vo2_max',
  'walking_heart_rate',
]

/** Is this condition one a glucose dashboard belongs to: diabetes by its
 *  template, or any condition whose plan watches blood glucose? Added after
 *  the skeleton. */
export function isGlucoseCondition(condition: Condition): boolean {
  if (condition.template === 'type1Diabetes' || condition.template === 'type2Diabetes') return true
  return condition.plan.watch.some((m) => m.type === 'metric' && m.metric === 'blood_glucose')
}

// ── nutrition ────────────────────────────────────────────────────────────

export const NO_NUTRIENTS: Nutrients = { kcal: 0, proteinG: 0, carbsG: 0, fatG: 0, extra: {} }

/** To the hundredth: what a plate's arithmetic is worth, and no floating-point
 *  tail (`0.1 + 0.2`) in what is stored. */
function hundredths(n: number): number {
  return Math.round(n * 100) / 100
}

/** An optional nutrient scaled, or left absent when the food does not say. */
function scaleOptional(value: number | null | undefined, factor: number): number | null {
  return value === null || value === undefined ? null : hundredths(value * factor)
}

/** `per100g` for `grams`: a food's nutrients are per 100 g (or 100 ml). */
export function scaleNutrients(per100g: Nutrients, grams: number): Nutrients {
  const factor = (Number.isFinite(grams) ? Math.max(0, grams) : 0) / 100
  const out: Nutrients = {
    kcal: hundredths(per100g.kcal * factor),
    proteinG: hundredths(per100g.proteinG * factor),
    carbsG: hundredths(per100g.carbsG * factor),
    fatG: hundredths(per100g.fatG * factor),
    extra: {},
  }
  for (const key of ['fiberG', 'sugarG', 'satFatG', 'sodiumMg'] as const) {
    const scaled = scaleOptional(per100g[key], factor)
    if (scaled !== null) out[key] = scaled
  }
  for (const [name, value] of Object.entries(per100g.extra ?? {})) {
    out.extra[name] = hundredths(value * factor)
  }
  return out
}

/** The sum. An optional nutrient is summed wherever any item has it, so a
 *  meal of one item that knows its fibre and one that does not still has a
 *  fibre figure -- a lower bound, which is what "logged" means. */
export function sumNutrients(all: Nutrients[]): Nutrients {
  const out: Nutrients = { kcal: 0, proteinG: 0, carbsG: 0, fatG: 0, extra: {} }
  for (const n of all) {
    out.kcal += n.kcal
    out.proteinG += n.proteinG
    out.carbsG += n.carbsG
    out.fatG += n.fatG
    for (const key of ['fiberG', 'sugarG', 'satFatG', 'sodiumMg'] as const) {
      const value = n[key]
      if (value !== null && value !== undefined) out[key] = (out[key] ?? 0) + value
    }
    for (const [name, value] of Object.entries(n.extra ?? {})) {
      out.extra[name] = (out.extra[name] ?? 0) + value
    }
  }
  out.kcal = hundredths(out.kcal)
  out.proteinG = hundredths(out.proteinG)
  out.carbsG = hundredths(out.carbsG)
  out.fatG = hundredths(out.fatG)
  for (const key of ['fiberG', 'sugarG', 'satFatG', 'sodiumMg'] as const) {
    const value = out[key]
    if (value !== null && value !== undefined) out[key] = hundredths(value)
  }
  for (const name of Object.keys(out.extra)) out.extra[name] = hundredths(out.extra[name] ?? 0)
  return out
}

/** Grams in `amount` of `unit` for `food` (`g`, `ml`, or a serving's name);
 *  `null` for a unit the food has no serving called, or a nonsense amount. */
export function gramsOf(food: Food, amount: number, unit: string): number | null {
  if (!Number.isFinite(amount) || amount < 0) return null
  const wanted = unit.trim().toLowerCase()
  // A millilitre is a gram, which is how a drink's "per 100 ml" is read.
  if (wanted === 'g' || wanted === 'ml') return amount
  const serving = food.servings.find((s) => s.name.trim().toLowerCase() === wanted)
  return serving ? amount * serving.grams : null
}

/** A meal item for `amount` of `unit` of `food`, nutrients worked out now --
 *  so editing the food later does not rewrite what was eaten. Named and
 *  spelt as the vault's `nutrition::item` does: the brand in brackets after
 *  the name, `g` and `ml` in lower case, a serving under its own name. */
export function itemFor(food: Food, amount: number, unit: string): MealItem | null {
  const grams = gramsOf(food, amount, unit)
  if (grams === null) return null
  const wanted = unit.trim().toLowerCase()
  const kept =
    wanted === 'g' || wanted === 'ml'
      ? wanted
      : (food.servings.find((s) => s.name.trim().toLowerCase() === wanted)?.name ?? unit)
  const brand = food.brand.trim()
  return {
    foodId: food.id,
    name: brand ? `${food.name} (${brand})` : food.name,
    amount,
    unit: kept,
    grams,
    nutrients: scaleNutrients(food.per100g, grams),
    estimated: food.source.type === 'estimate',
  }
}

// ── making things ────────────────────────────────────────────────────────
//
// Added after the skeleton: the blank record an editor starts from. The vault
// takes a record's id from the interface (`save_*` is a plain upsert), as
// Money's and Screen time's do.

/** A fresh id for a record that has not been saved yet. */
export function newId(): string {
  return crypto.randomUUID()
}

/** The slot a meal at this hour of the day is, until somebody says otherwise. */
export function slotAt(at: Date): MealSlot {
  const hour = at.getHours() + at.getMinutes() / 60
  if (hour >= 4 && hour < 10.5) return 'breakfast'
  if (hour >= 10.5 && hour < 15) return 'lunch'
  if (hour >= 17 && hour < 21.5) return 'dinner'
  return 'snack'
}

/** An empty meal at `at`, in the slot that hour usually is. */
export function newMeal(at: Date = new Date()): Meal {
  const now = new Date().toISOString()
  return {
    id: newId(),
    at: at.toISOString(),
    offsetMin: -at.getTimezoneOffset(),
    slot: slotAt(at),
    name: '',
    items: [],
    photo: null,
    note: '',
    createdAt: now,
    updatedAt: now,
  }
}

/** A tablet once a day at eight: what the medication editor starts from. */
export function newMedication(sortOrder = 0): Medication {
  const now = new Date().toISOString()
  return {
    id: newId(),
    name: '',
    kind: 'prescription',
    form: 'tablet',
    strength: '',
    dose: 1,
    doseUnit: 'tablet',
    schedule: { type: 'daily', times: ['08:00:00'] },
    rules: [],
    tags: [],
    status: 'active',
    started: isoDate(new Date()),
    stopped: null,
    stopReason: '',
    history: [],
    conditionIds: [],
    prescriber: '',
    pharmacy: '',
    supply: null,
    remind: true,
    codes: [],
    sourceNames: [],
    notes: '',
    sortOrder,
    createdAt: now,
    updatedAt: now,
  }
}
