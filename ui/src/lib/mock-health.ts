// Seed health for the mock backend, and the one function `mock.ts` hands
// every Health command and `health_*` shell call to.
//
// One believable person, sixty days ending today: a man in his forties with
// type 2 diabetes and an underactive thyroid, a Dexcom on his arm and an
// Apple Watch on his wrist.
//
//   - the watch: heart rate every five minutes (lower asleep, higher on the
//     two runs a week), resting heart rate, HRV, steps, active energy,
//     exercise minutes, stand hours, nightly sleep in stages, VO2 max;
//   - weight and body fat weekly, a few blood pressures typed in;
//   - the CGM: a glucose reading every five minutes, with a spike after each
//     meal, a higher day now and then and three low nights;
//   - three meals a day (and a snack) made of twelve foods;
//   - metformin twice a day with food, levothyroxine at seven with its two
//     rules, and a calcium supplement taken one morning too close to it --
//     so there is a rule alert -- with good but imperfect adherence;
//   - two conditions applied from the templates, a levothyroxine dose change
//     five weeks ago (so a TSH is coming due), A1c every three months going
//     down, a TSH that went up, an allergy, two documents (one waiting for
//     its results to be accepted);
//   - a Health Auto Export source heard from an hour ago, an Apple export
//     imported six weeks ago, settings and no food-database key.
//
// Deterministic: each day's data comes from a PRNG seeded by what it is and
// the date, so a day always looks the same whatever today is. Days are local
// days -- the `tz` a command is given is checked for being there and
// otherwise ignored, which is the one way this differs from the vault.
//
// Built the first time a command asks, not when this file loads, and no
// timers anywhere: loading it cannot hold a test process open. The import
// "runs" for a short while by comparing the clock, not by waiting.

import {
  durationText,
  labKey,
  newId,
  scaleNutrients,
  slotName,
  sumNutrients,
  itemFor,
  VITALS_METRICS,
} from './health'
import { mockCatalogFile, mockFileUsed, mockFilesReleased, mockHealthFile } from './mock-files'
import { addDays, daysApart, isoDate, startOfDay, startOfWeek, todayIso } from './time'
import type {
  ActivityDay,
  AgpSlot,
  Allergy,
  CarePlan,
  Code,
  Condition,
  ConditionReport,
  DayTimeline,
  DocFile,
  DocKind,
  Dose,
  DueDose,
  Food,
  FoodHit,
  GlucoseEvent,
  GlucoseReport,
  HealthDay,
  HealthDocument,
  HealthSettings,
  HealthSource,
  ImportStatus,
  LabDue,
  LabResult,
  LabTrend,
  Latest,
  ManualReading,
  Meal,
  MealItem,
  MealResponse,
  Medication,
  Metric,
  MetricEntry,
  MetricGroup,
  MetricKind,
  Nutrients,
  NutritionDay,
  PushSetup,
  RoutePoint,
  RuleAlert,
  SamplePoint,
  SeriesPoint,
  SeriesQuery,
  SeriesResult,
  SleepNight,
  SleepSegment,
  SleepStage,
  Stat,
  TargetStanding,
  TemplateChoice,
  TemplateInfo,
  TemplateKey,
  VisitBrief,
  Workout,
  WorkoutDetail,
  Activity,
  BucketSize,
  CareTarget,
  LabSchedule,
  Measure,
} from './types'
import { VaultError } from './types'

const MIN = 60_000
const HOUR = 60 * MIN
const DAY = 24 * HOUR
/** How many days back the seed reaches, today included. */
const SEED_DAYS = 60
/** The longest range a report or a series may cover (`MAX_RANGE_DAYS`). */
const MAX_RANGE_DAYS = 800
/** A due time is answered by a dose within this long either side of it. */
const WINDOW_MIN = 120
/** Taken within this long of the due time counts as on time. */
const ON_TIME_MIN = 60

const WATCH = "Hari's Apple Watch"
const PHONE = 'iPhone'
const DEXCOM = 'Dexcom G7'
const TYPED = 'Karma Yoga'

/** A copy through JSON, which is what the real bridge does to every answer. */
function wire<T>(value: T): T {
  return value === undefined ? value : (JSON.parse(JSON.stringify(value)) as T)
}

function str(v: unknown): string {
  return typeof v === 'string' ? v : ''
}

function strOrNull(v: unknown): string | null {
  return typeof v === 'string' && v !== '' ? v : null
}

/** FNV-1a: a stable number for a string. */
function hash(seed: string): number {
  let h = 2166136261
  for (let i = 0; i < seed.length; i++) {
    h ^= seed.charCodeAt(i)
    h = Math.imul(h, 16777619)
  }
  return h >>> 0
}

/** Mulberry32: a small seeded PRNG, `[0, 1)`. */
function prng(seed: number): () => number {
  let a = seed | 0
  return () => {
    a = (a + 0x6d2b79f5) | 0
    let t = Math.imul(a ^ (a >>> 15), 1 | a)
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

function invalid(message: string): VaultError {
  return new VaultError('invalid', message)
}

function notFound(what: string): VaultError {
  return new VaultError('not_found', `That ${what} no longer exists.`)
}

function isDay(value: string): boolean {
  return /^\d{4}-\d{2}-\d{2}$/.test(value) && isoDate(startOfDay(value)) === value
}

/** Local time `h:m` on local day `day`, as epoch milliseconds. */
function at(day: string, h = 0, m = 0): number {
  const d = startOfDay(day)
  d.setHours(h, m, 0, 0)
  return d.getTime()
}

function dayOf(t: number): string {
  return isoDate(new Date(t))
}

function iso(t: number): string {
  return new Date(t).toISOString()
}

function round(n: number, digits = 0): number {
  const k = 10 ** digits
  return Math.round(n * k) / k
}

function clamp(n: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, n))
}

// ── the catalogue ────────────────────────────────────────────────────────

const SUM: Stat = { type: 'sum' }
const MEAN: Stat = { type: 'mean' }
const LAST: Stat = { type: 'last' }

function entry(
  metric: Metric,
  name: string,
  unit: string,
  kind: MetricKind,
  group: MetricGroup,
  stat: Stat,
): MetricEntry {
  return { metric, name, unit, kind, group, stat }
}

/** The mock's copy of `Metric::info`: canonical units and default stats. */
const CATALOG: MetricEntry[] = [
  entry('steps', 'Steps', 'count', 'cumulative', 'activity', SUM),
  entry(
    'walking_running_distance',
    'Walking and running distance',
    'm',
    'cumulative',
    'activity',
    SUM,
  ),
  entry('cycling_distance', 'Cycling distance', 'm', 'cumulative', 'activity', SUM),
  entry('swimming_distance', 'Swimming distance', 'm', 'cumulative', 'activity', SUM),
  entry('active_energy', 'Active energy', 'kcal', 'cumulative', 'activity', SUM),
  entry('basal_energy', 'Resting energy', 'kcal', 'cumulative', 'activity', SUM),
  entry('exercise_time', 'Exercise time', 'min', 'cumulative', 'activity', SUM),
  entry('stand_time', 'Stand time', 'min', 'cumulative', 'activity', SUM),
  entry('stand_hour', 'Stand hours', 'count', 'category', 'activity', SUM),
  entry('flights_climbed', 'Flights climbed', 'count', 'cumulative', 'activity', SUM),
  entry('time_in_daylight', 'Time in daylight', 'min', 'cumulative', 'activity', SUM),
  entry('heart_rate', 'Heart rate', 'count/min', 'discrete', 'heart', MEAN),
  entry('resting_heart_rate', 'Resting heart rate', 'count/min', 'discrete', 'heart', MEAN),
  entry('walking_heart_rate', 'Walking heart rate', 'count/min', 'discrete', 'heart', MEAN),
  entry('heart_rate_variability', 'Heart rate variability', 'ms', 'discrete', 'heart', MEAN),
  entry('heart_rate_recovery', 'Heart rate recovery', 'count/min', 'discrete', 'heart', MEAN),
  entry('vo2_max', 'Cardio fitness', 'mL/kg/min', 'discrete', 'heart', LAST),
  entry('respiratory_rate', 'Respiratory rate', 'count/min', 'discrete', 'respiratory', MEAN),
  entry('oxygen_saturation', 'Blood oxygen', '%', 'discrete', 'respiratory', MEAN),
  entry('weight', 'Weight', 'kg', 'discrete', 'body', LAST),
  entry('body_fat', 'Body fat', '%', 'discrete', 'body', LAST),
  entry('lean_body_mass', 'Lean body mass', 'kg', 'discrete', 'body', LAST),
  entry('bmi', 'Body mass index', 'kg/m2', 'discrete', 'body', LAST),
  entry('height', 'Height', 'm', 'discrete', 'body', LAST),
  entry('waist_circumference', 'Waist circumference', 'm', 'discrete', 'body', LAST),
  entry('body_temperature', 'Body temperature', 'degC', 'discrete', 'vitals', MEAN),
  entry('wrist_temperature', 'Wrist temperature', 'degC', 'discrete', 'vitals', MEAN),
  entry('blood_pressure_systolic', 'Blood pressure, systolic', 'mmHg', 'discrete', 'vitals', MEAN),
  entry(
    'blood_pressure_diastolic',
    'Blood pressure, diastolic',
    'mmHg',
    'discrete',
    'vitals',
    MEAN,
  ),
  entry('blood_glucose', 'Blood glucose', 'mg/dL', 'discrete', 'metabolic', MEAN),
  entry('insulin_basal', 'Insulin, basal', 'IU', 'cumulative', 'metabolic', SUM),
  entry('insulin_bolus', 'Insulin, bolus', 'IU', 'cumulative', 'metabolic', SUM),
  entry('dietary_energy', 'Dietary energy', 'kcal', 'cumulative', 'nutrition', SUM),
  entry('dietary_carbs', 'Carbohydrates', 'g', 'cumulative', 'nutrition', SUM),
  entry('dietary_protein', 'Protein', 'g', 'cumulative', 'nutrition', SUM),
  entry('dietary_fat', 'Fat', 'g', 'cumulative', 'nutrition', SUM),
  entry('dietary_fiber', 'Fiber', 'g', 'cumulative', 'nutrition', SUM),
  entry('dietary_sugar', 'Sugar', 'g', 'cumulative', 'nutrition', SUM),
  entry('dietary_sodium', 'Sodium', 'mg', 'cumulative', 'nutrition', SUM),
  entry('dietary_caffeine', 'Caffeine', 'mg', 'cumulative', 'nutrition', SUM),
  entry('water', 'Water', 'mL', 'cumulative', 'nutrition', SUM),
  entry('sleep_stage', 'Sleep', 'stage', 'category', 'sleep', SUM),
  entry('mindful_minutes', 'Mindful minutes', 'min', 'cumulative', 'mind', SUM),
]

const ENTRY = new Map<Metric, MetricEntry>(CATALOG.map((e) => [e.metric, e]))

/** Plausible canonical ranges for what can be typed in: a value outside is a
 *  unit mix-up (the real vault refuses it the same way). */
const PLAUSIBLE: Partial<Record<Metric, [number, number]>> = {
  weight: [20, 400],
  body_fat: [2, 70],
  height: [0.5, 2.6],
  waist_circumference: [0.3, 2.5],
  body_temperature: [30, 43],
  blood_pressure_systolic: [50, 260],
  blood_pressure_diastolic: [30, 160],
  blood_glucose: [20, 700],
  resting_heart_rate: [20, 220],
  heart_rate: [20, 250],
  oxygen_saturation: [50, 100],
  steps: [0, 200_000],
  water: [0, 20_000],
}

// ── samples ──────────────────────────────────────────────────────────────

/** One sample: start and duration in milliseconds, canonical value, source. */
interface S {
  t: number
  d: number
  v: number
  src: string
}

const samples = new Map<Metric, S[]>()

function put(metric: Metric, t: number, d: number, v: number, src: string): void {
  let list = samples.get(metric)
  if (!list) samples.set(metric, (list = []))
  list.push({ t, d, v, src })
}

/** The samples of `metric` starting in `[from, to)`, in order. */
function between(metric: Metric, from: number, to: number): S[] {
  const list = samples.get(metric) ?? []
  // Lower bound by binary search: a metric may hold tens of thousands.
  let lo = 0
  let hi = list.length
  while (lo < hi) {
    const mid = (lo + hi) >> 1
    if ((list[mid] as S).t < from) lo = mid + 1
    else hi = mid
  }
  const out: S[] = []
  for (let i = lo; i < list.length && (list[i] as S).t < to; i++) out.push(list[i] as S)
  return out
}

/** The last sample of `metric` starting at or before `t`. */
function lastAtOrBefore(metric: Metric, t: number): S | undefined {
  const list = samples.get(metric) ?? []
  let lo = 0
  let hi = list.length
  while (lo < hi) {
    const mid = (lo + hi) >> 1
    if ((list[mid] as S).t <= t) lo = mid + 1
    else hi = mid
  }
  return list[lo - 1]
}

function point(s: S): SamplePoint {
  return { start: iso(s.t), end: iso(s.t + s.d), value: s.v, source: s.src }
}

// ── what the seed is made of ─────────────────────────────────────────────

let built = false
/** When the seed was built: nothing is recorded after it. */
let NOW = 0
let TODAY = ''

let settings: HealthSettings
let sources: HealthSource[] = []
let medications: Medication[] = []
let doses: Dose[] = []
let conditions: Condition[] = []
let labs: LabResult[] = []
let documents: HealthDocument[] = []
let allergies: Allergy[] = []
let foods: Food[] = []
let meals: Meal[] = []
let workouts: Workout[] = []
let nights: Night[] = []
let fdcKey = ''
let imported: {
  startedAt: number
  fileName: string
  total: number
  cancelled: boolean
  /** An upload to put in the Files trash once it has been read. */
  trashAfter?: string
} | null = null
let importDone: ImportStatus | null = null
let seq = 0

function nextId(prefix: string): string {
  return `${prefix}-${++seq}`
}

interface Night {
  date: string
  bed: number
  wake: number
  segs: { stage: SleepStage; start: number; end: number }[]
  hrv: number
  resp: number
}

function ensure(): void {
  if (built) return
  built = true
  NOW = Date.now()
  TODAY = todayIso()
  buildFoods()
  buildNights()
  buildMeals()
  buildWorkouts()
  buildSamples()
  buildGlucose()
  buildMedications()
  buildDoses()
  buildCare()
  buildSources()
  for (const list of samples.values()) list.sort((a, b) => a.t - b.t)
}

/** The days of the seed, oldest first, today last. */
function seedDays(): string[] {
  return Array.from({ length: SEED_DAYS }, (_, i) => addDays(TODAY, i - (SEED_DAYS - 1)))
}

// ── foods and meals ──────────────────────────────────────────────────────

function nutrients(
  kcal: number,
  proteinG: number,
  carbsG: number,
  fatG: number,
  more: Partial<Nutrients> = {},
): Nutrients {
  return { kcal, proteinG, carbsG, fatG, extra: {}, ...more }
}

function food(
  id: string,
  name: string,
  per100g: Nutrients,
  servings: { name: string; grams: number }[],
  favorite = false,
): Food {
  const stamp = iso(NOW - 70 * DAY)
  return {
    id,
    name,
    brand: '',
    barcode: '',
    source: { type: 'own' },
    per100g,
    servings,
    favorite,
    lastUsed: null,
    useCount: 0,
    createdAt: stamp,
    updatedAt: stamp,
  }
}

function buildFoods(): void {
  foods = [
    food(
      'f-oats',
      'Oatmeal, cooked',
      nutrients(71, 2.5, 12, 1.5, { fiberG: 1.7, sugarG: 0.3 }),
      [
        { name: '1 cup', grams: 234 },
        { name: '1 bowl', grams: 300 },
      ],
      true,
    ),
    food('f-egg', 'Egg, boiled', nutrients(155, 12.6, 1.1, 10.6, { sodiumMg: 124 }), [
      { name: '1 egg', grams: 50 },
    ]),
    food(
      'f-yogurt',
      'Greek yogurt, plain',
      nutrients(59, 10.2, 3.6, 0.4, { sugarG: 3.2 }),
      [
        { name: '1 cup', grams: 245 },
        { name: '1 pot', grams: 170 },
      ],
      true,
    ),
    food('f-banana', 'Banana', nutrients(89, 1.1, 22.8, 0.3, { fiberG: 2.6, sugarG: 12.2 }), [
      { name: '1 banana', grams: 118 },
    ]),
    food('f-rice', 'Basmati rice, cooked', nutrients(130, 2.7, 28.2, 0.3, { fiberG: 0.4 }), [
      { name: '1 cup', grams: 158 },
    ]),
    food(
      'f-dal',
      'Dal, cooked lentils',
      nutrients(116, 9, 20.1, 0.4, { fiberG: 7.9 }),
      [
        { name: '1 cup', grams: 198 },
        { name: '1 ladle', grams: 120 },
      ],
      true,
    ),
    food('f-chapati', 'Chapati', nutrients(297, 9.8, 46, 7.5, { fiberG: 4 }), [
      { name: '1 chapati', grams: 40 },
    ]),
    food('f-chicken', 'Chicken breast, grilled', nutrients(165, 31, 0, 3.6), [
      { name: '1 breast', grams: 120 },
    ]),
    food('f-paneer', 'Paneer', nutrients(265, 18.3, 1.2, 20.8), [
      { name: '1 serving', grams: 100 },
    ]),
    food('f-veg', 'Mixed vegetables, steamed', nutrients(45, 2.5, 8.5, 0.3, { fiberG: 3.5 }), [
      { name: '1 cup', grams: 150 },
    ]),
    food('f-almonds', 'Almonds', nutrients(579, 21, 21.6, 49.9, { fiberG: 12.5 }), [
      { name: '1 handful', grams: 28 },
    ]),
    food('f-apple', 'Apple', nutrients(52, 0.3, 13.8, 0.2, { fiberG: 2.4, sugarG: 10.4 }), [
      { name: '1 apple', grams: 182 },
    ]),
  ]
}

type Recipe = { name: string; parts: [string, number, string][] }

const BREAKFASTS: Recipe[] = [
  {
    name: 'Oatmeal and banana',
    parts: [
      ['f-oats', 1, '1 cup'],
      ['f-banana', 1, '1 banana'],
      ['f-almonds', 1, '1 handful'],
    ],
  },
  {
    name: 'Eggs and chapati',
    parts: [
      ['f-egg', 2, '1 egg'],
      ['f-chapati', 2, '1 chapati'],
    ],
  },
  {
    name: 'Yogurt, apple and almonds',
    parts: [
      ['f-yogurt', 1, '1 cup'],
      ['f-apple', 1, '1 apple'],
      ['f-almonds', 1, '1 handful'],
    ],
  },
]
const LUNCHES: Recipe[] = [
  {
    name: 'Dal, rice and vegetables',
    parts: [
      ['f-dal', 1, '1 cup'],
      ['f-rice', 1, '1 cup'],
      ['f-veg', 1, '1 cup'],
    ],
  },
  {
    name: 'Chicken, vegetables and chapati',
    parts: [
      ['f-chicken', 1, '1 breast'],
      ['f-veg', 1, '1 cup'],
      ['f-chapati', 2, '1 chapati'],
    ],
  },
  {
    name: 'Paneer, vegetables and chapati',
    parts: [
      ['f-paneer', 1, '1 serving'],
      ['f-veg', 1, '1 cup'],
      ['f-chapati', 2, '1 chapati'],
    ],
  },
]
const DINNERS: Recipe[] = [
  {
    name: 'Chicken, rice and vegetables',
    parts: [
      ['f-chicken', 1, '1 breast'],
      ['f-rice', 1.5, '1 cup'],
      ['f-veg', 1, '1 cup'],
    ],
  },
  {
    name: 'Dal, chapati and yogurt',
    parts: [
      ['f-dal', 1, '1 cup'],
      ['f-chapati', 3, '1 chapati'],
      ['f-yogurt', 1, '1 pot'],
    ],
  },
  {
    name: 'Paneer curry with rice',
    parts: [
      ['f-paneer', 1, '1 serving'],
      ['f-rice', 2, '1 cup'],
      ['f-veg', 1, '1 cup'],
    ],
  },
]
const SNACKS: Recipe[] = [
  { name: 'Apple', parts: [['f-apple', 1, '1 apple']] },
  { name: 'Almonds', parts: [['f-almonds', 1, '1 handful']] },
  { name: 'Banana', parts: [['f-banana', 1, '1 banana']] },
  { name: 'Yogurt', parts: [['f-yogurt', 1, '1 pot']] },
]

function makeMeal(
  day: string,
  slot: Meal['slot'],
  minuteOfDay: number,
  recipe: Recipe,
): Meal | null {
  const t = at(day, 0, minuteOfDay)
  if (t > NOW) return null
  const items: MealItem[] = []
  for (const [id, amount, unit] of recipe.parts) {
    const f = foods.find((x) => x.id === id)
    const item = f ? itemFor(f, amount, unit) : null
    if (item) items.push(item)
  }
  const stamp = iso(t)
  return {
    id: nextId('meal'),
    at: stamp,
    offsetMin: -new Date(t).getTimezoneOffset(),
    slot,
    name: recipe.name,
    items,
    photo: null,
    note: '',
    createdAt: stamp,
    updatedAt: stamp,
  }
}

function pick<T>(list: readonly T[], r: () => number): T {
  return list[Math.floor(r() * list.length)] as T
}

function buildMeals(): void {
  meals = []
  for (const day of seedDays()) {
    const r = prng(hash(`meals:${day}`))
    // Breakfast is never before eight, so levothyroxine at seven always has
    // its half hour of an empty stomach; dinner never before a quarter past
    // seven, when the evening metformin is due.
    const made = [
      makeMeal(day, 'breakfast', 8 * 60 + Math.floor(r() * 40), pick(BREAKFASTS, r)),
      makeMeal(day, 'lunch', 12 * 60 + 15 + Math.floor(r() * 60), pick(LUNCHES, r)),
      makeMeal(day, 'dinner', 19 * 60 + 15 + Math.floor(r() * 60), pick(DINNERS, r)),
      r() < 0.5
        ? makeMeal(
            day,
            'snack',
            (r() < 0.5 ? 10 * 60 + 30 : 16 * 60) + Math.floor(r() * 60),
            pick(SNACKS, r),
          )
        : null,
    ]
    for (const meal of made) if (meal) meals.push(meal)
  }
  meals.sort((a, b) => Date.parse(a.at) - Date.parse(b.at))
  // The latest lunch was photographed, so the meal editor has a photo to
  // show: a sealed file in Files, as the vault keeps one.
  const photographed = [...meals].reverse().find((m) => m.slot === 'lunch')
  if (photographed) {
    const photo = mockHealthFile('lunch-bowl.jpg', 1_620_000, photographed.at)
    photographed.photo = photo.id
    mockFileUsed(photo.id, {
      owner: { kind: 'meal', id: photographed.id },
      title: photographed.name,
      when: photographed.at,
    })
  }
  // What the person has eaten shows on the foods they eat most.
  for (const meal of meals) {
    for (const item of meal.items) {
      const f = foods.find((x) => x.id === item.foodId)
      if (!f) continue
      f.useCount += 1
      if (!f.lastUsed || f.lastUsed < meal.at) f.lastUsed = meal.at
    }
  }
}

function mealCarbs(meal: Meal): number {
  return meal.items.reduce((n, i) => n + i.nutrients.carbsG, 0)
}

// ── sleep ────────────────────────────────────────────────────────────────

const nightByDate = new Map<string, Night>()

function makeNight(date: string): Night {
  const r = prng(hash(`sleep:${date}`))
  const bed = at(addDays(date, -1), 22, 35) + Math.floor(r() * 75) * MIN
  const wake = at(date, 6, 15) + Math.floor(r() * 55) * MIN
  const segs: Night['segs'] = []
  let t = bed
  const push = (stage: SleepStage, minutes: number) => {
    const end = Math.min(t + Math.round(minutes) * MIN, wake)
    if (end > t) {
      segs.push({ stage, start: t, end })
      t = end
    }
  }
  push('awake', 6 + r() * 14)
  for (let cycle = 0; t < wake - 20 * MIN; cycle++) {
    push('core', 22 + r() * 18)
    push('deep', cycle === 0 ? 40 + r() * 20 : cycle === 1 ? 25 + r() * 15 : 4 + r() * 10)
    push('core', 12 + r() * 18)
    push('rem', cycle < 2 ? 8 + r() * 10 : 18 + r() * 16)
    if (r() < 0.5) push('awake', 2 + r() * 6)
  }
  if (t < wake) push('awake', (wake - t) / MIN)
  return {
    date,
    bed,
    wake,
    segs,
    hrv: round(36 + r() * 22),
    resp: round(14 + r() * 2.5, 1),
  }
}

function buildNights(): void {
  nights = []
  nightByDate.clear()
  // One more day than the seed: the evening's bedtime shapes that day's heart rate.
  for (const day of [...seedDays(), addDays(TODAY, 1)]) nightByDate.set(day, makeNight(day))
  for (const day of seedDays()) {
    const night = nightByDate.get(day)
    // A night that has not ended yet is not a night yet.
    if (night && night.wake <= NOW) nights.push(night)
  }
}

function stageAt(t: number): SleepStage | null {
  const day = dayOf(t)
  for (const date of [day, addDays(day, 1)]) {
    const night = nightByDate.get(date)
    if (!night || t < night.bed || t >= night.wake) continue
    return night.segs.find((s) => t >= s.start && t < s.end)?.stage ?? 'awake'
  }
  return null
}

function nightRecord(night: Night): SleepNight {
  const minutes = (stage: SleepStage) =>
    night.segs.filter((s) => s.stage === stage).reduce((n, s) => n + (s.end - s.start) / MIN, 0)
  const core = minutes('core')
  const deep = minutes('deep')
  const rem = minutes('rem')
  const awake = minutes('awake')
  const asleep = core + deep + rem
  const inBed = (night.wake - night.bed) / MIN
  const sleeping = night.segs.filter((s) => s.stage !== 'awake')
  const sleepStart = sleeping[0]?.start ?? night.bed
  const sleepEnd = sleeping.at(-1)?.end ?? night.wake
  const hr = between('heart_rate', sleepStart, sleepEnd)
  const efficiency = inBed > 0 ? asleep / inBed : 0
  // The app's own heuristic, with every part kept so the screen can say why.
  const parts = {
    duration: round(clamp(asleep / 480, 0, 1) * 50, 1),
    efficiency: round(clamp((efficiency - 0.7) / 0.25, 0, 1) * 20, 1),
    deep: round(clamp(deep / asleep / 0.15, 0, 1) * 15, 1),
    rem: round(clamp(rem / asleep / 0.2, 0, 1) * 15, 1),
  }
  const score = Object.values(parts).reduce((n, v) => n + v, 0)
  return {
    date: night.date,
    inBedStart: iso(night.bed),
    sleepStart: iso(sleepStart),
    sleepEnd: iso(sleepEnd),
    asleepMin: round(asleep),
    inBedMin: round(inBed),
    awakeMin: round(awake),
    coreMin: round(core),
    deepMin: round(deep),
    remMin: round(rem),
    efficiency: round(efficiency, 3),
    hr: hr.length > 0 ? round(hr.reduce((n, s) => n + s.v, 0) / hr.length, 1) : null,
    hrvMs: night.hrv,
    respiratoryRate: night.resp,
    score: round(score),
    scoreParts: parts,
    segments: night.segs.map((s) => ({ stage: s.stage, start: iso(s.start), end: iso(s.end) })),
  }
}

// ── workouts ─────────────────────────────────────────────────────────────

function buildWorkouts(): void {
  workouts = []
  seedDays().forEach((day, i) => {
    const r = prng(hash(`workout:${day}`))
    const dow = startOfDay(day).getDay()
    const make = (
      activity: Activity,
      name: string,
      h: number,
      m: number,
      minutes: number,
      extra: Partial<Workout>,
    ) => {
      const start = at(day, h, m) + Math.floor(r() * 12) * MIN
      const end = start + minutes * MIN
      if (end > NOW) return
      const stamp = iso(end)
      workouts.push({
        id: nextId('wk'),
        activity,
        name,
        start: iso(start),
        end: stamp,
        offsetMin: -startOfDay(day).getTimezoneOffset(),
        indoor: false,
        source: WATCH,
        sourceId: 'src-hae',
        externalId: '',
        note: '',
        key: '',
        hidden: false,
        createdAt: stamp,
        updatedAt: stamp,
        ...extra,
      })
    }
    // Two runs a week, Tuesday and Saturday, skipped one time in seven.
    if ((dow === 2 || dow === 6) && r() > 0.14) {
      const minutes = 34 + Math.floor(r() * 16)
      const km = round(minutes / (5.9 + r() * 0.5), 2)
      make('running', 'Outdoor Run', 7, 5, minutes, {
        activeEnergyKcal: round(km * 62),
        totalEnergyKcal: round(km * 62 + minutes * 1.3),
        distanceM: round(km * 1000),
        elevationUpM: round(18 + r() * 60),
        avgHr: round(147 + r() * 10),
        maxHr: round(166 + r() * 10),
        effort: round(6 + r() * 2, 1),
        route: `route-${i}`,
      })
    }
    if (dow === 4 && r() < 0.45) {
      make('strength_training', 'Traditional Strength Training', 17, 40, 40, {
        activeEnergyKcal: round(210 + r() * 60),
        totalEnergyKcal: round(250 + r() * 60),
        avgHr: round(112 + r() * 10),
        maxHr: round(138 + r() * 12),
        indoor: true,
      })
    }
    if (dow === 0 && r() < 0.5) {
      make('walking', 'Outdoor Walk', 10, 0, 45, {
        activeEnergyKcal: round(150 + r() * 40),
        distanceM: round(3600 + r() * 900),
        avgHr: round(98 + r() * 8),
        maxHr: round(116 + r() * 10),
      })
    }
  })
  workouts.sort((a, b) => Date.parse(a.start) - Date.parse(b.start))
}

function routeOf(w: Workout): RoutePoint[] {
  if (!w.route) return []
  const r = prng(hash(w.id))
  const start = Date.parse(w.start)
  const span = Date.parse(w.end) - start
  const n = 96
  const wobble = r() * 2
  return Array.from({ length: n + 1 }, (_, k) => {
    const a = (2 * Math.PI * k) / n
    return {
      at: iso(start + (span * k) / n),
      lat: round(37.7694 + 0.006 * Math.sin(a) * (1 + 0.2 * Math.sin(3 * a + wobble)), 6),
      lon: round(-122.4862 + 0.01 * Math.cos(a), 6),
      alt: round(24 + 18 * Math.sin(2 * a) + r() * 2, 1),
    }
  })
}

// ── the watch, the scale and the cuff ────────────────────────────────────

function buildSamples(): void {
  samples.clear()
  const days = seedDays()
  const runs = workouts.filter((w) => w.activity === 'running')

  days.forEach((day, di) => {
    const r = prng(hash(`watch:${day}`))
    const base = at(day)
    // Resting heart rate has been drifting down since he started running.
    const trend = 3.5 * (1 - di / (SEED_DAYS - 1))
    const dayRuns = runs.filter((w) => dayOf(Date.parse(w.start)) === day)

    // Heart rate every five minutes, every minute through a run.
    for (let slot = 0; slot < 288; slot++) {
      const t = base + slot * 5 * MIN
      if (t > NOW) break
      const stage = stageAt(t)
      const hour = (t - base) / HOUR
      let v: number
      if (stage) {
        const level = { deep: 49, core: 53, rem: 58, awake: 64, asleep: 54, inBed: 60 }[stage]
        v = level + trend * 0.6 + (r() - 0.5) * 5
      } else {
        v = 67 + trend + 5 * Math.sin(((hour - 9) / 24) * 2 * Math.PI) + (r() - 0.5) * 8
        if (r() < 0.12) v += 10 + r() * 25
        if (hour > 12 && hour < 13.5) v += 7
      }
      const run = dayRuns.find(
        (w) => t >= Date.parse(w.start) - 5 * MIN && t < Date.parse(w.end) + 10 * MIN,
      )
      if (run) {
        const into = (t - Date.parse(run.start)) / MIN
        const after = t - Date.parse(run.end)
        v = after > 0 ? 112 - (after / MIN) * 2 : Math.min(152 + r() * 12, 96 + into * 9)
      }
      put('heart_rate', t, 0, round(clamp(v, 42, 185)), WATCH)
    }
    for (const run of dayRuns) {
      const start = Date.parse(run.start)
      const end = Date.parse(run.end)
      for (let t = start; t < end; t += MIN) {
        if (t % (5 * MIN) === 0 || t > NOW) continue
        const into = (t - start) / MIN
        put('heart_rate', t, 0, round(Math.min(150 + r() * 14, 98 + into * 8)), WATCH)
      }
    }

    // One a day.
    const noon = at(day, 12)
    if (noon <= NOW) {
      put('resting_heart_rate', noon, 0, round(58 + trend + (r() - 0.5) * 3), WATCH)
      put('walking_heart_rate', at(day, 15), 0, round(96 + (r() - 0.5) * 8), WATCH)
    }
    const night = nightByDate.get(day)
    if (night && night.wake <= NOW) {
      const mid = night.bed + (night.wake - night.bed) / 2
      put('heart_rate_variability', mid, 0, night.hrv, WATCH)
      put('respiratory_rate', mid + 20 * MIN, 0, night.resp, WATCH)
      put('oxygen_saturation', mid + 30 * MIN, 0, round(96 + r() * 2.4), WATCH)
      put('wrist_temperature', mid + 40 * MIN, 0, round(-0.3 + r() * 0.7, 2), WATCH)
      for (const seg of night.segs)
        put('sleep_stage', seg.start, seg.end - seg.start, STAGE_CODE[seg.stage], WATCH)
      put('sleep_stage', night.bed, night.wake - night.bed, STAGE_CODE.inBed, PHONE)
    }

    // The day's steps, spread over the hours a person is up and about.
    const dow = startOfDay(day).getDay()
    const weekend = dow === 0 || dow === 6
    const target = (weekend ? 5200 + r() * 6000 : 6200 + r() * 3400) * (0.85 + r() * 0.3)
    const weights = [0.5, 1.4, 0.9, 0.6, 0.7, 1.3, 1, 0.6, 0.6, 0.7, 1.2, 1.4, 1, 0.6, 0.3]
    const total = weights.reduce((n, w) => n + w, 0)
    weights.forEach((w, k) => {
      const hourStart = at(day, 7 + k)
      if (hourStart > NOW) return
      const elapsed = Math.min(1, (NOW - hourStart) / HOUR)
      let steps = ((target * w) / total) * (0.7 + r() * 0.6) * elapsed
      const run = dayRuns.find(
        (x) =>
          dayOf(Date.parse(x.start)) === day && new Date(Date.parse(x.start)).getHours() === 7 + k,
      )
      if (run) steps += (run.distanceM ?? 0) / 0.95
      steps = Math.round(steps)
      const d = Math.round(HOUR * elapsed)
      put('steps', hourStart, d, steps, WATCH)
      put('walking_running_distance', hourStart, d, round(steps * 0.74), WATCH)
      put(
        'active_energy',
        hourStart,
        d,
        round(steps * 0.045 + (run ? (run.activeEnergyKcal ?? 0) * 0.8 : 0)),
        WATCH,
      )
    })
    // Exercise minutes: the run, and a brisk walk or two.
    for (const run of dayRuns) {
      put(
        'exercise_time',
        Date.parse(run.start),
        Date.parse(run.end) - Date.parse(run.start),
        round((Date.parse(run.end) - Date.parse(run.start)) / MIN),
        WATCH,
      )
    }
    for (const [h, m] of [
      [12, 30],
      [18, 5],
    ] as const) {
      const t = at(day, h, m)
      if (t + 20 * MIN <= NOW && r() < 0.75)
        put('exercise_time', t, 20 * MIN, round(8 + r() * 12), WATCH)
    }
    for (let h = 8; h <= 19; h++) {
      const t = at(day, h)
      if (t + HOUR <= NOW && r() < 0.93) put('stand_hour', t, HOUR, 1, WATCH)
    }
    for (let k = 0; k < 4; k++) {
      const t = at(day, 8 + Math.floor(r() * 11), Math.floor(r() * 50))
      if (t <= NOW) put('flights_climbed', t, 0, 1 + Math.floor(r() * 4), WATCH)
    }
    if (at(day, 13) <= NOW)
      put('time_in_daylight', at(day, 13), 40 * MIN, round(20 + r() * 50), WATCH)
    if (r() < 0.45 && at(day, 8, 20) + 10 * MIN <= NOW)
      put('mindful_minutes', at(day, 8, 20), 10 * MIN, 10, WATCH)
    // Cardio fitness is worked out about once a week.
    if (di % 7 === 3 && at(day, 9) <= NOW)
      put('vo2_max', at(day, 9), 0, round(36 + di * 0.03 + r() * 0.6, 1), WATCH)

    // The scale on Sunday mornings: weight, body fat, and the index.
    if (dow === 0 && at(day, 7, 30) <= NOW) {
      const weight = round(82.6 - (di / (SEED_DAYS - 1)) * 2.3 + (r() - 0.5) * 0.4, 1)
      put('weight', at(day, 7, 30), 0, weight, PHONE)
      put(
        'body_fat',
        at(day, 7, 30),
        0,
        round(24.6 - (di / (SEED_DAYS - 1)) * 1 + (r() - 0.5) * 0.4, 1),
        PHONE,
      )
      put('bmi', at(day, 7, 30), 0, round(weight / (1.78 * 1.78), 1), PHONE)
    }
  })
  put('height', at(days[0] as string, 7, 0), 0, 1.78, PHONE)

  // A few blood pressures, typed in.
  for (const offset of [-57, -49, -41, -33, -24, -15, -8, -2]) {
    const day = addDays(TODAY, offset)
    const r = prng(hash(`bp:${day}`))
    const t = at(day, 8, 30 + Math.floor(r() * 40))
    if (t > NOW) continue
    put('blood_pressure_systolic', t, 0, round(127 + r() * 12), TYPED)
    put('blood_pressure_diastolic', t, 0, round(78 + r() * 9), TYPED)
  }
}

const STAGE_CODE: Record<SleepStage, number> = {
  inBed: 0,
  awake: 1,
  asleep: 2,
  core: 3,
  deep: 4,
  rem: 5,
}

/** The CGM: a base that follows the clock, a rise after each meal, a higher
 *  day now and then, three nights that dipped low, and a few gaps. */
function buildGlucose(): void {
  const days = seedDays()
  const lowNights = new Set([-41, -27, -13].map((o) => addDays(TODAY, o)))
  days.forEach((day, di) => {
    const r = prng(hash(`cgm:${day}`))
    const shift = (r() - 0.5) * 14 + (r() < 0.12 ? 16 : 0) - di * 0.05
    // Yesterday's dinner is still on its way down at midnight.
    const near = meals.filter((m) => {
      const t = Date.parse(m.at)
      return t > at(day) - 4 * HOUR && t < at(addDays(day, 1))
    })
    let noise = 0
    let gap = 0
    for (let slot = 0; slot < 288; slot++) {
      const t = at(day) + slot * 5 * MIN
      if (t > NOW) break
      noise = noise * 0.85 + (r() - 0.5) * 6
      if (gap > 0) {
        gap--
        continue
      }
      if (r() < 0.0015) {
        gap = 3
        continue
      }
      const hour = slot / 12
      let v = 104 + shift + 7 * Math.sin(((hour - 6) / 24) * 2 * Math.PI) + noise
      for (const m of near) {
        const tau = (t - Date.parse(m.at)) / MIN
        if (tau < 0 || tau > 240) continue
        const peak = 50
        v += mealCarbs(m) * 0.9 * (tau / peak) * Math.exp(1 - tau / peak)
      }
      // A night that went low, between three and ten to four.
      if (lowNights.has(day) && hour >= 3 && hour < 3.85) v = Math.min(v, 58 + (r() - 0.5) * 8)
      put('blood_glucose', t, 0, round(clamp(v, 40, 340)), DEXCOM)
    }
  })
}

// ── medications and doses ────────────────────────────────────────────────

function buildMedications(): void {
  const started = addDays(TODAY, -400)
  const stamp = iso(NOW - 400 * DAY)
  const base = (over: Partial<Medication> & Pick<Medication, 'id' | 'name'>): Medication => ({
    kind: 'prescription',
    form: 'tablet',
    strength: '',
    dose: 1,
    doseUnit: 'tablet',
    schedule: { type: 'daily', times: ['08:00:00'] },
    rules: [],
    tags: [],
    status: 'active',
    started,
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
    sortOrder: 0,
    createdAt: stamp,
    updatedAt: stamp,
    ...over,
  })
  medications = [
    base({
      id: 'med-metformin',
      name: 'Metformin',
      strength: '500 mg',
      schedule: { type: 'daily', times: ['08:00:00', '20:00:00'] },
      rules: [{ type: 'withFood', minutes: 30 }],
      tags: ['diabetes'],
      conditionIds: ['cond-t2d'],
      prescriber: 'Dr. Rao',
      pharmacy: 'Corner Pharmacy',
      supply: { remaining: 52, refillDays: 7, lastRefill: addDays(TODAY, -34) },
      codes: [{ system: 'rxnorm', code: '861004', display: 'Metformin 500 MG Oral Tablet' }],
      history: [{ on: addDays(TODAY, -400), dose: 1, strength: '500 mg', note: 'Started' }],
      sortOrder: 0,
    }),
    base({
      id: 'med-levothyroxine',
      name: 'Levothyroxine',
      strength: '75 mcg',
      schedule: { type: 'daily', times: ['07:00:00'] },
      rules: [
        { type: 'emptyStomach', minutes: 30 },
        { type: 'apartFrom', tags: ['calcium', 'iron'], hours: 4 },
      ],
      tags: ['thyroid'],
      conditionIds: ['cond-hypo'],
      prescriber: 'Dr. Rao',
      pharmacy: 'Corner Pharmacy',
      supply: { remaining: 12, refillDays: 14, lastRefill: addDays(TODAY, -78) },
      codes: [{ system: 'rxnorm', code: '966224', display: 'Levothyroxine 75 MCG Oral Tablet' }],
      history: [
        { on: addDays(TODAY, -400), dose: 1, strength: '50 mcg', note: 'Started' },
        { on: addDays(TODAY, -35), dose: 1, strength: '75 mcg', note: 'Raised after a TSH of 6.8' },
      ],
      notes: 'Take with water, an hour before anything else.',
      sortOrder: 1,
    }),
    base({
      id: 'med-calcium',
      name: 'Calcium',
      kind: 'supplement',
      strength: '600 mg',
      schedule: { type: 'daily', times: ['13:00:00'] },
      tags: ['calcium'],
      supply: { remaining: 40, refillDays: 10, lastRefill: addDays(TODAY, -20) },
      sortOrder: 2,
    }),
  ]
}

/** Local `HH:MM:SS` times to instants on `date`. */
function timesOn(times: string[], date: string): number[] {
  return times.map((t) => {
    const [h, m] = t.split(':').map(Number)
    return at(date, h ?? 0, m ?? 0)
  })
}

/** When `med` is due on `date`, in order. Nothing for one not active, as
 *  needed, not started yet or stopped before `date`. */
function dueTimes(med: Medication, date: string): number[] {
  if (med.status !== 'active') return []
  if (med.started && date < med.started) return []
  if (med.stopped && date > med.stopped) return []
  const s = med.schedule
  if (s.type === 'asNeeded') return []
  if (s.type === 'weekly') {
    const weekday = ((startOfDay(date).getDay() + 6) % 7) + 1
    if (!s.days.includes(weekday)) return []
  }
  if (s.type === 'everyNDays') {
    const k = daysApart(s.from, date)
    if (k < 0 || s.n < 1 || k % s.n !== 0) return []
  }
  return timesOn(s.times, date).sort((a, b) => a - b)
}

function buildDoses(): void {
  doses = []
  const tooClose = addDays(TODAY, -9)
  for (const day of seedDays()) {
    const r = prng(hash(`doses:${day}`))
    for (const med of medications) {
      for (const due of dueTimes(med, day)) {
        // Not yet taken, if it is not yet due.
        if (due > NOW - 10 * MIN) continue
        const roll = r()
        // The morning the calcium went down too close to the levothyroxine,
        // both were taken: that is what the alert is about.
        const story =
          day === tooClose && (med.id === 'med-calcium' || med.id === 'med-levothyroxine')
        const status = story || roll < 0.92 ? 'taken' : roll < 0.95 ? 'skipped' : 'missed'
        let when = due
        if (status === 'taken') {
          if (med.id === 'med-levothyroxine') when = due + (Math.floor(r() * 26) - 5) * MIN
          else if (med.id === 'med-calcium') when = due + (Math.floor(r() * 40) - 20) * MIN
          else {
            // Metformin goes down with the meal it is taken with.
            const meal = meals.find((m) => {
              const t = Date.parse(m.at)
              return (
                Math.abs(t - due) < 90 * MIN &&
                m.slot === (due < at(day, 14) ? 'breakfast' : 'dinner')
              )
            })
            when = (meal ? Date.parse(meal.at) : due) + (Math.floor(r() * 14) - 4) * MIN
          }
          // One morning the calcium went down with the levothyroxine's coffee.
          if (med.id === 'med-calcium' && day === tooClose) when = at(day, 9, 20)
        }
        if (when > NOW) continue
        const stamp = iso(when)
        doses.push({
          id: nextId('dose'),
          medicationId: med.id,
          scheduledAt: iso(due),
          at: stamp,
          status,
          amount: null,
          origin: status === 'missed' ? 'reminder' : 'manual',
          note: '',
          key: '',
          hidden: false,
          createdAt: stamp,
          updatedAt: stamp,
        })
      }
    }
  }
  doses.sort((a, b) => Date.parse(a.at) - Date.parse(b.at))
}

// ── conditions, labs, documents ──────────────────────────────────────────

const LOINC_A1C = '4548-4'
const LOINC_TSH = '3016-3'
const LOINC_FT4 = '3024-7'

function metricMeasure(metric: Metric, stat: Stat): Measure {
  return { type: 'metric', metric, stat }
}

function diabetesPlan(insulin: boolean): CarePlan {
  const inRange = (low: number, high: number): Stat => ({ type: 'inRange', low, high })
  const targets: CareTarget[] = [
    {
      label: 'Time in range, 70 to 180 mg/dL',
      measure: metricMeasure('blood_glucose', inRange(70, 180)),
      low: 0.7,
      useReferenceRange: false,
      windowDays: 14,
      note: 'More than 70% of the time (international consensus on time in range, 2019).',
    },
    {
      label: 'Time below 70 mg/dL',
      measure: metricMeasure('blood_glucose', inRange(0, 69)),
      high: 0.04,
      useReferenceRange: false,
      windowDays: 14,
      note: 'Less than 4% of the time (international consensus, 2019).',
    },
    {
      label: 'Time below 54 mg/dL',
      measure: metricMeasure('blood_glucose', inRange(0, 53)),
      high: 0.01,
      useReferenceRange: false,
      windowDays: 14,
      note: 'Less than 1% of the time (international consensus, 2019).',
    },
    {
      label: 'Hemoglobin A1c',
      measure: { type: 'lab', code: LOINC_A1C },
      high: 7,
      useReferenceRange: false,
      windowDays: 0,
      note: 'Below 7% for most adults (ADA Standards of Care); yours may differ.',
    },
    {
      label: 'Blood pressure, systolic',
      measure: metricMeasure('blood_pressure_systolic', MEAN),
      high: 130,
      useReferenceRange: false,
      windowDays: 30,
      note: 'Below 130/80 (ADA Standards of Care).',
    },
    {
      label: 'Blood pressure, diastolic',
      measure: metricMeasure('blood_pressure_diastolic', MEAN),
      high: 80,
      useReferenceRange: false,
      windowDays: 30,
      note: 'Below 130/80 (ADA Standards of Care).',
    },
  ]
  const yearly = (code: string, name: string): LabSchedule => ({
    code,
    name,
    everyDays: 365,
    afterChangeDays: null,
    afterChangeTag: '',
  })
  return {
    watch: [
      metricMeasure('blood_glucose', inRange(70, 180)),
      metricMeasure('blood_glucose', MEAN),
      { type: 'lab', code: LOINC_A1C },
      ...(insulin ? [metricMeasure('insulin_bolus', SUM)] : []),
      metricMeasure('weight', LAST),
      metricMeasure('blood_pressure_systolic', MEAN),
      metricMeasure('active_energy', SUM),
      metricMeasure('sleep_stage', SUM),
    ],
    targets,
    labs: [
      {
        code: LOINC_A1C,
        name: 'Hemoglobin A1c',
        everyDays: 90,
        afterChangeDays: null,
        afterChangeTag: '',
      },
      yearly('2093-3', 'Lipid panel'),
      yearly('33914-3', 'Kidney function (eGFR)'),
      yearly('9318-7', 'Urine albumin/creatinine'),
      yearly('eye-exam', 'Eye exam'),
      yearly('foot-exam', 'Foot exam'),
    ],
    medRules: insulin ? { insulin: [{ type: 'withFood', minutes: 15 }] } : {},
    trackerIds: [],
    templateVersion: 1,
  }
}

function thyroidPlan(): CarePlan {
  const lab = (code: string, label: string): CareTarget => ({
    label,
    measure: { type: 'lab', code },
    useReferenceRange: true,
    windowDays: 0,
    note: 'Inside the reference range printed on each report.',
  })
  const recheck = (code: string, name: string): LabSchedule => ({
    code,
    name,
    everyDays: 365,
    afterChangeDays: 42,
    afterChangeTag: 'thyroid',
  })
  return {
    watch: [
      { type: 'lab', code: LOINC_TSH },
      { type: 'lab', code: LOINC_FT4 },
      metricMeasure('resting_heart_rate', MEAN),
      metricMeasure('weight', LAST),
      metricMeasure('sleep_stage', SUM),
    ],
    targets: [lab(LOINC_TSH, 'TSH'), lab(LOINC_FT4, 'Free T4')],
    labs: [recheck(LOINC_TSH, 'TSH'), recheck(LOINC_FT4, 'Free T4')],
    medRules: {
      thyroid: [
        { type: 'emptyStomach', minutes: 30 },
        { type: 'apartFrom', tags: ['calcium', 'iron'], hours: 4 },
      ],
    },
    trackerIds: [],
    templateVersion: 1,
  }
}

const TEMPLATES: TemplateInfo[] = [
  {
    key: 'type1Diabetes',
    name: 'Type 1 diabetes',
    summary:
      'Glucose against meals, doses and workouts; time in range, insulin and carbohydrates; A1c every three months.',
    version: 1,
    plan: diabetesPlan(true),
    symptoms: ['Fatigue', 'Thirst', 'Blurred vision', 'Low-glucose symptoms'],
    codes: [{ system: 'icd10', code: 'E10', display: 'Type 1 diabetes mellitus' }],
  },
  {
    key: 'type2Diabetes',
    name: 'Type 2 diabetes',
    summary:
      'Glucose against meals and activity; time in range, weight and blood pressure; A1c every three months.',
    version: 1,
    plan: diabetesPlan(false),
    symptoms: ['Fatigue', 'Thirst', 'Blurred vision'],
    codes: [{ system: 'icd10', code: 'E11', display: 'Type 2 diabetes mellitus' }],
  },
  {
    key: 'hypothyroidism',
    name: 'Hypothyroidism',
    summary:
      'TSH and free T4 against each report’s own range, six weeks after any dose change; levothyroxine timing rules.',
    version: 1,
    plan: thyroidPlan(),
    symptoms: ['Fatigue', 'Feeling cold', 'Low mood', 'Constipation'],
    codes: [{ system: 'icd10', code: 'E03.9', display: 'Hypothyroidism, unspecified' }],
  },
]

function template(key: TemplateKey): TemplateInfo {
  const found = TEMPLATES.find((t) => t.key === key)
  if (!found) throw invalid('That is not a condition template.')
  return found
}

function lab(
  id: string,
  code: string,
  name: string,
  value: number | null,
  unit: string,
  refLow: number | null,
  refHigh: number | null,
  daysAgo: number,
  over: Partial<LabResult> = {},
): LabResult {
  const collected = addDays(TODAY, -daysAgo)
  const stamp = iso(at(collected, 12))
  let flag: LabResult['flag'] = 'normal'
  if (value !== null && refLow !== null && value < refLow) flag = 'low'
  if (value !== null && refHigh !== null && value > refHigh) flag = 'high'
  if (value === null) flag = 'none'
  return {
    id,
    code,
    name,
    value,
    text: '',
    unit,
    refLow,
    refHigh,
    refText: '',
    flag,
    collected,
    collectedAt: null,
    lab: 'Quest Diagnostics',
    panel: '',
    documentId: null,
    origin: 'manual',
    note: '',
    createdAt: stamp,
    updatedAt: stamp,
    ...over,
  }
}

function buildCare(): void {
  const stamp = iso(NOW - 120 * DAY)
  const code = (system: string, c: string, display: string): Code => ({ system, code: c, display })
  conditions = [
    {
      id: 'cond-t2d',
      name: 'Type 2 diabetes',
      template: 'type2Diabetes',
      status: 'active',
      diagnosed: addDays(TODAY, -1500),
      codes: [code('icd10', 'E11', 'Type 2 diabetes mellitus')],
      plan: wire(template('type2Diabetes').plan),
      notes: '',
      createdAt: stamp,
      updatedAt: stamp,
    },
    {
      id: 'cond-hypo',
      name: 'Hypothyroidism',
      template: 'hypothyroidism',
      status: 'active',
      diagnosed: addDays(TODAY, -900),
      codes: [code('icd10', 'E03.9', 'Hypothyroidism, unspecified')],
      plan: wire(template('hypothyroidism').plan),
      notes: '',
      createdAt: stamp,
      updatedAt: stamp,
    },
  ]
  allergies = [
    {
      id: 'allergy-penicillin',
      substance: 'Penicillin',
      kind: 'drug',
      reaction: 'Hives',
      severity: 'moderate',
      noted: addDays(TODAY, -3650),
      notes: 'As a child.',
      createdAt: stamp,
      updatedAt: stamp,
    },
  ]

  const a1c = (id: string, ago: number, v: number) =>
    lab(id, LOINC_A1C, 'Hemoglobin A1c', v, '%', 4, 5.6, ago, { panel: 'Diabetes' })
  const kidney = 'doc-lipids'
  labs = [
    a1c('lab-a1c-1', 450, 8.3),
    a1c('lab-a1c-2', 360, 8),
    a1c('lab-a1c-3', 270, 7.7),
    a1c('lab-a1c-4', 180, 7.4),
    a1c('lab-a1c-5', 90, 7.2),
    lab('lab-tsh-1', LOINC_TSH, 'TSH', 3.9, 'mIU/L', 0.4, 4, 400),
    lab('lab-tsh-2', LOINC_TSH, 'TSH', 4.4, 'mIU/L', 0.4, 4, 300),
    lab('lab-tsh-3', LOINC_TSH, 'TSH', 5.2, 'mIU/L', 0.4, 4, 200),
    lab('lab-tsh-4', LOINC_TSH, 'TSH', 5.9, 'mIU/L', 0.4, 4, 110),
    lab('lab-tsh-5', LOINC_TSH, 'TSH', 6.8, 'mIU/L', 0.4, 4, 38),
    lab('lab-ft4-1', LOINC_FT4, 'Free T4', 0.9, 'ng/dL', 0.8, 1.8, 110),
    lab('lab-ft4-2', LOINC_FT4, 'Free T4', 0.8, 'ng/dL', 0.8, 1.8, 38),
    lab('lab-chol', '2093-3', 'Total cholesterol', 192, 'mg/dL', null, 200, 120, {
      panel: 'Lipid panel',
      documentId: kidney,
      origin: 'extracted',
    }),
    lab('lab-ldl', '13457-7', 'LDL cholesterol', 118, 'mg/dL', null, 100, 120, {
      panel: 'Lipid panel',
      documentId: kidney,
      origin: 'extracted',
    }),
    lab('lab-hdl', '2085-9', 'HDL cholesterol', 46, 'mg/dL', 40, null, 120, {
      panel: 'Lipid panel',
      documentId: kidney,
      origin: 'extracted',
    }),
    lab('lab-trig', '2571-8', 'Triglycerides', 168, 'mg/dL', null, 150, 120, {
      panel: 'Lipid panel',
      documentId: kidney,
      origin: 'extracted',
    }),
    lab('lab-egfr', '33914-3', 'eGFR', 91, 'mL/min/1.73m2', 60, null, 120, {
      panel: 'Kidney function',
      documentId: kidney,
      origin: 'extracted',
    }),
    lab('lab-uacr', '9318-7', 'Urine albumin/creatinine', 14, 'mg/g', null, 30, 120, {
      panel: 'Kidney function',
      documentId: kidney,
      origin: 'extracted',
    }),
    lab('lab-vitd', '1989-3', '25-hydroxy vitamin D', 28, 'ng/mL', 30, 100, 120, {
      documentId: kidney,
      origin: 'extracted',
    }),
    lab('lab-eye', 'eye-exam', 'Eye exam', null, '', null, null, 150, {
      text: 'Dilated exam: no retinopathy.',
      lab: 'Bay Eye Care',
      origin: 'manual',
    }),
    lab('lab-foot', 'foot-exam', 'Foot exam', null, '', null, null, 150, {
      text: 'Sensation and pulses normal.',
      lab: 'Dr. Rao',
      origin: 'manual',
    }),
  ]

  // A page is a sealed file in Files, as the vault keeps one; the document
  // lists it by the id the catalog gave it.
  const file = (name: string, size: number, daysAgo: number): DocFile => {
    const entry = mockHealthFile(name, size, iso(at(addDays(TODAY, -daysAgo), 12)))
    return { file: entry.id, name: entry.name, mime: entry.mime, size: entry.size }
  }
  const doc = (
    id: string,
    title: string,
    kind: DocKind,
    daysAgo: number,
    provider: string,
    files: DocFile[],
    over: Partial<HealthDocument>,
  ): HealthDocument => {
    const date = addDays(TODAY, -daysAgo)
    const at12 = iso(at(date, 12))
    return {
      id,
      title,
      kind,
      date,
      provider,
      files,
      text: '',
      fhir: [],
      extraction: 'none',
      extractionError: '',
      pendingLabs: [],
      conditionIds: [],
      externalId: '',
      notes: '',
      createdAt: at12,
      updatedAt: at12,
      ...over,
    }
  }
  const pending = (
    id: string,
    c: string,
    name: string,
    v: number,
    unit: string,
    lo: number | null,
    hi: number | null,
  ) =>
    lab(id, c, name, v, unit, lo, hi, 8, {
      origin: 'extracted',
      documentId: 'doc-october',
      panel: 'Metabolic panel',
    })
  documents = [
    doc(
      'doc-october',
      'Lab report, October',
      'labReport',
      8,
      'Quest Diagnostics',
      [file('quest-labs-october.pdf', 214_880, 8)],
      {
        text: 'Hemoglobin A1c 6.9 %\nGlucose, fasting 112 mg/dL\nCreatinine 0.9 mg/dL\neGFR 91 mL/min/1.73m2',
        extraction: 'proposed',
        conditionIds: ['cond-t2d'],
        pendingLabs: [
          pending('pending-a1c', LOINC_A1C, 'Hemoglobin A1c', 6.9, '%', 4, 5.6),
          pending('pending-glucose', '1558-6', 'Glucose, fasting', 112, 'mg/dL', 70, 99),
          pending('pending-creatinine', '2160-0', 'Creatinine', 0.9, 'mg/dL', 0.6, 1.3),
          pending('pending-egfr', '33914-3', 'eGFR', 91, 'mL/min/1.73m2', 60, null),
        ],
      },
    ),
    doc(
      kidney,
      'Lipid and kidney panels',
      'labReport',
      120,
      'Quest Diagnostics',
      [file('lipid-kidney-panel.pdf', 188_204, 120)],
      {
        text: 'Total cholesterol 192 mg/dL\nLDL 118 mg/dL\nHDL 46 mg/dL\nTriglycerides 168 mg/dL\neGFR 91',
        extraction: 'done',
        conditionIds: ['cond-t2d'],
      },
    ),
  ]
  for (const d of documents) usePages(d)
}

/** Files says which document a page is used in. */
function usePages(doc: HealthDocument): void {
  for (const f of doc.files) {
    mockFileUsed(f.file, {
      owner: { kind: 'healthDocument', id: doc.id },
      title: doc.title,
      when: doc.createdAt,
    })
  }
}

// ── sources and settings ─────────────────────────────────────────────────

function buildSources(): void {
  settings = {
    units: {
      glucose: 'mg/dL',
      weight: 'kg',
      distance: 'km',
      energy: 'kcal',
      temperature: 'C',
      length: 'cm',
    },
    goals: { moveKcal: 500, exerciseMin: 30, standHours: 12, steps: 8000, sleepHours: 8 },
    nutrition: {
      kcal: 2000,
      proteinG: 90,
      carbsG: 220,
      fatG: 70,
      fiberG: 30,
      sodiumMg: 2300,
      carbsPerMealG: 60,
    },
    priority: [WATCH, PHONE, DEXCOM],
    priorityByMetric: {},
    glucoseLow: 70,
    glucoseHigh: 180,
    doseReminders: true,
    staleHours: 24,
  }
  const hourAgo = iso(NOW - HOUR)
  sources = [
    {
      id: 'src-hae',
      connector: 'healthAutoExport',
      name: "Hari's iPhone",
      paused: false,
      status: {
        lastReceivedAt: hourAgo,
        newestSampleAt: iso(NOW - 70 * MIN),
        lastError: '',
        samplesTotal: 41_380,
        automations: [
          {
            id: 'auto-metrics',
            name: 'Health Metrics, hourly',
            period: 'Since Last Sync',
            aggregation: 'none',
            lastAt: hourAgo,
            lastCount: 312,
          },
          {
            id: 'auto-workouts',
            name: 'Workouts, hourly',
            period: 'Since Last Sync',
            aggregation: 'none',
            lastAt: hourAgo,
            lastCount: 1,
          },
        ],
        devices: [WATCH, PHONE, DEXCOM],
      },
      tokenId: 'mock-token-hae',
      createdAt: iso(NOW - 70 * DAY),
      updatedAt: hourAgo,
    },
    {
      id: 'src-apple',
      connector: 'appleExport',
      name: 'Apple Health export',
      paused: false,
      status: {
        lastReceivedAt: iso(NOW - 45 * DAY),
        newestSampleAt: iso(NOW - 46 * DAY),
        lastError: '',
        samplesTotal: 1_204_530,
        automations: [],
        devices: [WATCH, PHONE],
      },
      tokenId: '',
      createdAt: iso(NOW - 45 * DAY),
      updatedAt: iso(NOW - 45 * DAY),
    },
  ]
}

// ── adding samples up ────────────────────────────────────────────────────

function resolveStat(metric: Metric, stat: Stat | undefined): Stat {
  if (!stat || stat.type === 'auto') return ENTRY.get(metric)?.stat ?? MEAN
  return stat
}

/** `list` under `stat`; `null` for nothing to add up. */
function aggregate(list: S[], stat: Stat): number | null {
  if (list.length === 0) return null
  switch (stat.type) {
    case 'sum':
      return list.reduce((n, s) => n + s.v, 0)
    case 'mean':
    case 'auto':
      return list.reduce((n, s) => n + s.v, 0) / list.length
    case 'min':
      return Math.min(...list.map((s) => s.v))
    case 'max':
      return Math.max(...list.map((s) => s.v))
    case 'last':
      return (list.at(-1) as S).v
    case 'count':
      return list.length
    case 'inRange':
      return list.filter((s) => s.v >= stat.low && s.v <= stat.high).length / list.length
  }
}

function bucketStart(t: number, bucket: BucketSize): number {
  const d = new Date(t)
  switch (bucket) {
    case 'quarterHour':
      return Math.floor(t / (15 * MIN)) * 15 * MIN
    case 'hour':
      d.setMinutes(0, 0, 0)
      return d.getTime()
    case 'day':
      return at(isoDate(d))
    case 'week':
      return at(startOfWeek(isoDate(d), 1))
    case 'month':
      return at(`${isoDate(d).slice(0, 7)}-01`)
    case 'raw':
      return t
  }
}

function checkRange(from: string, to: string): void {
  if (!isDay(from) || !isDay(to)) throw invalid('A range is two dates, from and to.')
  const days = daysApart(from, to) + 1
  if (days < 1) throw invalid('A range cannot end before it starts.')
  if (days > MAX_RANGE_DAYS) throw invalid(`A range is at most ${MAX_RANGE_DAYS} days.`)
}

function series(query: SeriesQuery): SeriesResult {
  const metric = query.metric
  const info = ENTRY.get(metric)
  if (!info) throw invalid('That is not a metric.')
  checkRange(query.from, query.to)
  const stat = resolveStat(metric, query.stat)
  const from = at(query.from)
  const to = at(addDays(query.to, 1))
  const rows = between(metric, from, to)
  const bucket = query.bucket ?? 'day'
  let points: SeriesPoint[]
  if (bucket === 'raw') {
    if (daysApart(query.from, query.to) + 1 > 3)
      throw invalid('Raw samples are for at most three days.')
    points = rows.map((s) => ({ at: iso(s.t), value: s.v, count: 1 }))
  } else {
    const groups = new Map<number, S[]>()
    for (const s of rows) {
      const key = bucketStart(s.t, bucket)
      const list = groups.get(key)
      if (list) list.push(s)
      else groups.set(key, [s])
    }
    points = [...groups.entries()]
      .sort((a, b) => a[0] - b[0])
      .map(([key, list]): SeriesPoint => {
        const value = aggregate(list, stat) ?? 0
        const spread = stat.type === 'sum' || stat.type === 'count'
        return {
          at: iso(key),
          value: round(value, 3),
          ...(spread
            ? {}
            : { min: Math.min(...list.map((s) => s.v)), max: Math.max(...list.map((s) => s.v)) }),
          count: list.length,
        }
      })
  }
  const latest = lastAtOrBefore(metric, to - 1)
  return {
    metric,
    unit: info.unit,
    stat,
    points,
    ...(latest ? { latest: point(latest) } : {}),
  }
}

/** One value per local day under `stat`, only for days with something. */
function dailyValues(metric: Metric, stat: Stat, from: string, to: string): Map<string, number> {
  const groups = new Map<string, S[]>()
  for (const s of between(metric, at(from), at(addDays(to, 1)))) {
    const day = dayOf(s.t)
    const list = groups.get(day)
    if (list) list.push(s)
    else groups.set(day, [s])
  }
  const out = new Map<string, number>()
  for (const [day, list] of groups) {
    const value = aggregate(list, resolveStat(metric, stat))
    if (value !== null) out.set(day, value)
  }
  return out
}

/**
 * A health tracker's days, for `mock.ts`'s derived trackers: a metric's day
 * under `stat`, an `inRange` share as a percentage, as the vault's
 * `derived_days` does.
 */
export function mockHealthDaily(
  metric: Metric,
  stat: Stat | undefined,
  from?: string | null,
  to?: string | null,
): Map<string, number> {
  ensure()
  const resolved = resolveStat(metric, stat)
  const start = from && isDay(from) ? from : addDays(TODAY, -(SEED_DAYS - 1))
  const end = to && isDay(to) ? to : TODAY
  if (daysApart(start, end) < 0 || daysApart(start, end) + 1 > MAX_RANGE_DAYS) return new Map()
  const out = new Map<string, number>()
  for (const [day, value] of dailyValues(metric, resolved, start, end)) {
    out.set(day, round(resolved.type === 'inRange' ? value * 100 : value, 2))
  }
  return out
}

function latestOf(metrics: Metric[], on: string): Latest[] {
  const end = at(addDays(on, 1)) - 1
  const out: Latest[] = []
  for (const metric of metrics) {
    const s = lastAtOrBefore(metric, end)
    if (!s) continue
    const before = lastAtOrBefore(metric, s.t - 30 * DAY)
    out.push({
      metric,
      value: s.v,
      at: iso(s.t),
      source: s.src,
      ...(before ? { previous: before.v } : {}),
    })
  }
  return out
}

// ── readings typed in ────────────────────────────────────────────────────

/** `value` in `unit` as the metric's canonical unit, as `units::to_canonical`. */
function toCanonical(metric: Metric, value: number, unit: string): number {
  const u = unit.trim()
  if (u === '') return value
  const table: Record<string, number | ((v: number) => number)> = {
    lb: 1 / 2.2046226218,
    lbs: 1 / 2.2046226218,
    kg: 1,
    'mmol/L': 18.0156,
    'mg/dL': 1,
    F: (v: number) => (v - 32) / 1.8,
    '°F': (v: number) => (v - 32) / 1.8,
    C: 1,
    degC: 1,
    '°C': 1,
    cm: 0.01,
    in: 0.0254,
    m: 1,
    km: 1000,
    mi: 1609.344,
    kJ: 1 / 4.184,
    kcal: 1,
    bpm: 1,
    'count/min': 1,
    mmHg: 1,
    '%': 1,
  }
  const way = table[u]
  if (way === undefined)
    throw invalid(`“${u}” is not a unit ${metric.replace(/_/g, ' ')} can be in.`)
  return typeof way === 'function' ? way(value) : value * way
}

function addReading(reading: ManualReading): void {
  const when = Date.parse(reading.at)
  if (!Number.isFinite(when)) throw invalid('A reading needs a time.')
  const entries: [Metric, number][] = [[reading.metric, reading.value]]
  if (reading.metric === 'blood_pressure_systolic' && typeof reading.second === 'number') {
    entries.push(['blood_pressure_diastolic', reading.second])
  }
  for (const [metric, shown] of entries) {
    if (!ENTRY.has(metric)) throw invalid('That is not a metric.')
    if (!Number.isFinite(shown)) throw invalid('A reading needs a number.')
    const value = metric === reading.metric ? toCanonical(metric, shown, reading.unit ?? '') : shown
    const range = PLAUSIBLE[metric]
    if (range && (value < range[0] || value > range[1])) {
      throw invalid('That value is outside what this reading can be. Check its unit.')
    }
    put(metric, when, 0, value, TYPED)
  }
  for (const metric of entries.map((e) => e[0])) {
    ;(samples.get(metric) ?? []).sort((a, b) => a.t - b.t)
  }
}

function deleteSamples(metric: Metric, from: string, to: string, source: string | null): number {
  const list = samples.get(metric)
  if (!list) return 0
  const lo = Date.parse(from)
  const hi = Date.parse(to)
  if (!Number.isFinite(lo) || !Number.isFinite(hi) || hi < lo)
    throw invalid('A range is two instants.')
  const keep = list.filter((s) => !(s.t >= lo && s.t < hi && (source === null || s.src === source)))
  const gone = list.length - keep.length
  samples.set(metric, keep)
  return gone
}

// ── reports ──────────────────────────────────────────────────────────────

function activityDay(date: string): ActivityDay {
  const sum = (metric: Metric) => dailyValues(metric, SUM, date, date).get(date) ?? 0
  const mean = (metric: Metric) => dailyValues(metric, MEAN, date, date).get(date)
  const resting = mean('resting_heart_rate')
  const hrv = mean('heart_rate_variability') ?? nightHrv(date)
  return {
    date,
    activeKcal: round(sum('active_energy')),
    exerciseMin: round(sum('exercise_time')),
    standHours: round(sum('stand_hour')),
    steps: round(sum('steps')),
    distanceM: round(sum('walking_running_distance')),
    flights: round(sum('flights_climbed')),
    ...(resting === undefined ? {} : { restingHr: round(resting, 1) }),
    ...(hrv === undefined ? {} : { hrvMs: round(hrv, 1) }),
    workouts: workouts.filter((w) => dayOf(Date.parse(w.start)) === date).length,
  }
}

function nightHrv(date: string): number | undefined {
  const v = dailyValues('heart_rate_variability', MEAN, date, date).get(date)
  return v
}

function eachDay(from: string, to: string): string[] {
  checkRange(from, to)
  return Array.from({ length: daysApart(from, to) + 1 }, (_, i) => addDays(from, i))
}

function sleepNights(from: string, to: string): SleepNight[] {
  checkRange(from, to)
  return nights.filter((n) => n.date >= from && n.date <= to).map(nightRecord)
}

function nutritionDays(from: string, to: string): NutritionDay[] {
  return eachDay(from, to).map((date) => {
    const todays = meals.filter((m) => dayOf(Date.parse(m.at)) === date)
    return {
      date,
      logged: sumNutrients(todays.flatMap((m) => m.items.map((i) => i.nutrients))),
      imported: sumNutrients([]),
      meals: todays.length,
    }
  })
}

function percentile(sorted: number[], p: number): number {
  if (sorted.length === 0) return 0
  const k = (sorted.length - 1) * p
  const lo = Math.floor(k)
  const hi = Math.ceil(k)
  return (sorted[lo] as number) + ((sorted[hi] as number) - (sorted[lo] as number)) * (k - lo)
}

function glucoseReport(from: string, to: string): GlucoseReport {
  checkRange(from, to)
  const rows = between('blood_glucose', at(from), Math.min(at(addDays(to, 1)), NOW + 1))
  const empty: GlucoseReport = {
    from,
    to,
    readings: 0,
    coverage: 0,
    veryLow: 0,
    low: 0,
    inRange: 0,
    high: 0,
    veryHigh: 0,
    agp: [],
    events: [],
  }
  if (rows.length === 0) return empty
  // A reading stands for the time until the next, at most fifteen minutes.
  const weight = (i: number) => {
    const next = rows[i + 1]
    const row = rows[i] as S
    return Math.min(next ? next.t - row.t : 5 * MIN, 15 * MIN)
  }
  const bands = { veryLow: 0, low: 0, inRange: 0, high: 0, veryHigh: 0 }
  let covered = 0
  rows.forEach((s, i) => {
    const w = weight(i)
    covered += w
    const band =
      s.v < 54
        ? 'veryLow'
        : s.v < 70
          ? 'low'
          : s.v <= 180
            ? 'inRange'
            : s.v <= 250
              ? 'high'
              : 'veryHigh'
    bands[band] += w
  })
  const mean = rows.reduce((n, s) => n + s.v, 0) / rows.length
  const sd = Math.sqrt(rows.reduce((n, s) => n + (s.v - mean) ** 2, 0) / rows.length)
  const spanMs = Math.min(at(addDays(to, 1)), NOW) - at(from)
  // The 24 hours folded into five-minute slots, each from the readings within
  // twenty minutes of it on any day.
  const bySlot: number[][] = Array.from({ length: 288 }, () => [])
  for (const s of rows) {
    const d = new Date(s.t)
    const slot = Math.floor((d.getHours() * 60 + d.getMinutes()) / 5)
    for (let k = -4; k <= 4; k++) (bySlot[(slot + k + 288) % 288] as number[]).push(s.v)
  }
  const agp: AgpSlot[] = bySlot.map((values, slot) => {
    const sorted = [...values].sort((a, b) => a - b)
    return {
      minute: slot * 5,
      p5: round(percentile(sorted, 0.05)),
      p25: round(percentile(sorted, 0.25)),
      p50: round(percentile(sorted, 0.5)),
      p75: round(percentile(sorted, 0.75)),
      p95: round(percentile(sorted, 0.95)),
    }
  })
  return {
    from,
    to,
    readings: rows.length,
    coverage: round(clamp(covered / Math.max(spanMs, 1), 0, 1), 3),
    mean: round(mean, 1),
    gmi: round(3.31 + 0.02392 * mean, 1),
    cv: round((sd / mean) * 100, 1),
    veryLow: round(bands.veryLow / covered, 4),
    low: round(bands.low / covered, 4),
    inRange: round(bands.inRange / covered, 4),
    high: round(bands.high / covered, 4),
    veryHigh: round(bands.veryHigh / covered, 4),
    agp,
    events: glucoseEvents(rows),
  }
}

/** Runs below 70 or above 180 that lasted at least fifteen minutes. */
function glucoseEvents(rows: S[]): GlucoseEvent[] {
  const out: GlucoseEvent[] = []
  for (const side of ['low', 'high'] as const) {
    let run: S[] = []
    const flush = () => {
      const first = run[0]
      const last = run.at(-1)
      if (first && last && last.t - first.t + 5 * MIN >= 15 * MIN) {
        const values = run.map((s) => s.v)
        const extreme = side === 'low' ? Math.min(...values) : Math.max(...values)
        out.push({
          start: iso(first.t),
          end: iso(last.t + 5 * MIN),
          extreme,
          kind:
            side === 'low'
              ? extreme < 54
                ? 'veryLow'
                : 'low'
              : extreme > 250
                ? 'veryHigh'
                : 'high',
        })
      }
      run = []
    }
    for (const s of rows) {
      const inside = side === 'low' ? s.v < 70 : s.v > 180
      const last = run.at(-1)
      if (inside && (!last || s.t - last.t <= 15 * MIN)) run.push(s)
      else {
        flush()
        if (inside) run.push(s)
      }
    }
    flush()
  }
  return out.sort((a, b) => a.start.localeCompare(b.start))
}

function mealResponse(id: string): MealResponse {
  const meal = meals.find((m) => m.id === id)
  if (!meal) throw notFound('meal')
  const t = Date.parse(meal.at)
  const before = between('blood_glucose', t - 30 * MIN, t + 1).at(-1)
  const after = between('blood_glucose', t, t + 3 * HOUR)
  const baseline = before?.v
  const peak = after.length > 0 ? after.reduce((a, b) => (b.v > a.v ? b : a)) : undefined
  const recovered =
    peak && baseline !== undefined
      ? after.find((s) => s.t > peak.t && s.v <= baseline + 10)
      : undefined
  const iauc =
    baseline === undefined
      ? undefined
      : after.reduce((n, s) => n + Math.max(0, s.v - baseline) * 5, 0)
  return {
    mealId: id,
    ...(baseline === undefined ? {} : { baseline }),
    ...(peak ? { peak: peak.v, peakAfterMin: Math.round((peak.t - t) / MIN) } : {}),
    ...(peak && baseline !== undefined ? { rise: peak.v - baseline } : {}),
    ...(recovered ? { recoveredAfterMin: Math.round((recovered.t - t) / MIN) } : {}),
    ...(iauc === undefined ? {} : { iauc: round(iauc) }),
    points: after.map(point),
    carbsG: round(mealCarbs(meal), 1),
  }
}

function dayTimeline(date: string): DayTimeline {
  if (!isDay(date)) throw invalid('A day is a date.')
  const lo = at(date)
  const hi = at(addDays(date, 1))
  const heart = new Map<number, S[]>()
  for (const s of between('heart_rate', lo, hi)) {
    const key = bucketStart(s.t, 'quarterHour')
    const list = heart.get(key)
    if (list) list.push(s)
    else heart.set(key, [s])
  }
  return {
    date,
    glucose: between('blood_glucose', lo, hi).map(point),
    heartRate: [...heart.entries()]
      .sort((a, b) => a[0] - b[0])
      .map(([key, list]) => ({
        at: iso(key),
        value: round(aggregate(list, MEAN) as number, 1),
        min: Math.min(...list.map((s) => s.v)),
        max: Math.max(...list.map((s) => s.v)),
        count: list.length,
      })),
    meals: meals.filter((m) => Date.parse(m.at) >= lo && Date.parse(m.at) < hi),
    doses: doses.filter((d) => Date.parse(d.at) >= lo && Date.parse(d.at) < hi),
    insulin: [],
    workouts: workouts.filter((w) => Date.parse(w.start) >= lo && Date.parse(w.start) < hi),
    sleep: sleepSegments(lo, hi),
  }
}

function sleepSegments(lo: number, hi: number): SleepSegment[] {
  const out: SleepSegment[] = []
  for (const n of nightByDate.values()) {
    if (n.wake > NOW) continue
    for (const s of n.segs) {
      if (s.end > lo && s.start < hi) {
        out.push({
          stage: s.stage,
          start: iso(Math.max(s.start, lo)),
          end: iso(Math.min(s.end, hi)),
        })
      }
    }
  }
  return out.sort((a, b) => a.start.localeCompare(b.start))
}

// ── doses ────────────────────────────────────────────────────────────────

/** Every due time on `date` and the dose that answered it (by `scheduledAt`
 *  first, then the nearest unclaimed one in the window). */
function dayDoses(date: string, only?: string): DueDose[] {
  const out: DueDose[] = []
  const claimed = new Set<string>()
  const now = Date.now()
  for (const med of medications) {
    if (only && med.id !== only) continue
    for (const due of dueTimes(med, date)) {
      let found = doses.find(
        (d) => d.medicationId === med.id && d.scheduledAt && Date.parse(d.scheduledAt) === due,
      )
      if (!found) {
        found = doses
          .filter(
            (d) =>
              d.medicationId === med.id &&
              !d.scheduledAt &&
              !claimed.has(d.id) &&
              Math.abs(Date.parse(d.at) - due) <= WINDOW_MIN * MIN,
          )
          .sort((a, b) => Math.abs(Date.parse(a.at) - due) - Math.abs(Date.parse(b.at) - due))[0]
      }
      if (found) claimed.add(found.id)
      out.push({
        medicationId: med.id,
        name: med.name,
        dueAt: iso(due),
        ...(found ? { dose: found } : {}),
        overdue: !found && now > due + WINDOW_MIN * MIN,
      })
    }
  }
  return out.sort((a, b) => a.dueAt.localeCompare(b.dueAt))
}

function adherence(medicationId: string | null, from: string, to: string) {
  checkRange(from, to)
  const out = { due: 0, taken: 0, skipped: 0, missed: 0, onTime: 0 }
  for (const date of eachDay(from, to)) {
    for (const d of dayDoses(date, medicationId ?? undefined)) {
      if (d.dose) {
        if (d.dose.status === 'taken') {
          out.taken++
          if (Math.abs(Date.parse(d.dose.at) - Date.parse(d.dueAt)) <= ON_TIME_MIN * MIN)
            out.onTime++
        } else if (d.dose.status === 'skipped') out.skipped++
        else out.missed++
      } else if (d.overdue) out.missed++
      else continue
      out.due++
    }
  }
  return out
}

function mealWords(meal: Meal): string {
  return meal.name ? meal.name : slotName(meal.slot)
}

/** Every timing rule a dose and a meal broke between two local dates. */
function ruleAlerts(from: string, to: string): RuleAlert[] {
  checkRange(from, to)
  const lo = at(from)
  const hi = at(addDays(to, 1))
  const taken = doses.filter((d) => d.status === 'taken')
  const out: RuleAlert[] = []
  for (const med of medications) {
    const mine = taken.filter(
      (d) => d.medicationId === med.id && Date.parse(d.at) >= lo && Date.parse(d.at) < hi,
    )
    for (const rule of med.rules) {
      for (const dose of mine) {
        const when = Date.parse(dose.at)
        if (rule.type === 'emptyStomach') {
          const meal = meals.find((m) => {
            const t = Date.parse(m.at)
            return t >= when && t < when + rule.minutes * MIN
          })
          if (meal) {
            const gap = Math.round((Date.parse(meal.at) - when) / MIN)
            out.push({
              medicationId: med.id,
              at: meal.at,
              rule,
              message: `${mealWords(meal)} ${gap} minutes after ${med.name} (wait ${rule.minutes})`,
              mealId: meal.id,
            })
          }
        } else if (rule.type === 'withFood') {
          const near = meals.some((m) => Math.abs(Date.parse(m.at) - when) <= rule.minutes * MIN)
          if (!near) {
            out.push({
              medicationId: med.id,
              at: dose.at,
              rule,
              message: `${med.name} taken with no meal within ${rule.minutes} minutes`,
            })
          }
        } else {
          for (const other of medications) {
            if (other.id === med.id || !other.tags.some((t) => rule.tags.includes(t))) continue
            for (const o of taken.filter((d) => d.medicationId === other.id)) {
              const gap = Math.abs(Date.parse(o.at) - when)
              if (gap >= rule.hours * HOUR) continue
              out.push({
                medicationId: med.id,
                at: Date.parse(o.at) > when ? o.at : dose.at,
                rule,
                message: `${other.name} ${durationText(gap / MIN)} from ${med.name} (keep ${rule.hours} h apart)`,
                otherMedicationId: other.id,
              })
            }
          }
        }
      }
    }
  }
  return out.sort((a, b) => a.at.localeCompare(b.at))
}

// ── conditions ───────────────────────────────────────────────────────────

function latestLab(code: string): LabResult | undefined {
  return labs
    .filter((l) => l.code === code)
    .sort((a, b) => a.collected.localeCompare(b.collected))
    .at(-1)
}

function standing(target: CareTarget, date: string): TargetStanding {
  const judge = (
    value: number | null | undefined,
    low: number | null | undefined,
    high: number | null | undefined,
  ) => {
    if (value === null || value === undefined) return 'unknown'
    if (low !== null && low !== undefined && value < low) return 'notMet'
    if (high !== null && high !== undefined && value > high) return 'notMet'
    return 'met'
  }
  if (target.measure.type === 'lab') {
    const result = latestLab(target.measure.code)
    if (!result || result.value === null || result.value === undefined) {
      return { target, state: 'unknown' }
    }
    const state = target.useReferenceRange
      ? judge(result.value, result.refLow, result.refHigh)
      : judge(result.value, target.low, target.high)
    return { target, value: result.value, at: iso(at(result.collected, 9)), state }
  }
  const windowDays = Math.max(1, target.windowDays)
  const rows = between(
    target.measure.metric,
    at(addDays(date, -(windowDays - 1))),
    Math.min(at(addDays(date, 1)), NOW + 1),
  )
  const value = aggregate(rows, resolveStat(target.measure.metric, target.measure.stat))
  if (value === null) return { target, state: 'unknown' }
  return {
    target,
    value: round(value, 3),
    at: iso((rows.at(-1) as S).t),
    state: judge(value, target.low, target.high),
  }
}

function labDue(schedule: LabSchedule, date: string): LabDue {
  const last = labs
    .filter((l) => l.code === schedule.code)
    .map((l) => l.collected)
    .sort()
    .at(-1)
  let due: string | null =
    last && schedule.everyDays > 0 ? addDays(last, schedule.everyDays) : last ? null : date
  let reason = last ? `${schedule.everyDays} days after the last one` : 'No result yet'
  if (schedule.afterChangeDays && schedule.afterChangeTag) {
    const changes = medications
      .filter((m) => m.tags.includes(schedule.afterChangeTag))
      .flatMap((m) => m.history.map((h) => h.on))
      .sort()
    const change = changes.at(-1)
    if (change && (!last || change > last)) {
      due = addDays(change, schedule.afterChangeDays)
      const weeks = Math.round(schedule.afterChangeDays / 7)
      const shown = new Date(`${change}T12:00:00`).toLocaleDateString('en-GB', {
        day: 'numeric',
        month: 'short',
      })
      reason = `${weeks} weeks after the dose change on ${shown}`
    }
  }
  return {
    schedule,
    ...(last ? { last } : {}),
    ...(due ? { due } : {}),
    reason,
    overdue: due !== null && due < date,
  }
}

function conditionReport(id: string, date: string): ConditionReport {
  const condition = conditions.find((c) => c.id === id)
  if (!condition) throw notFound('condition')
  const mine = medications.filter((m) => m.conditionIds.includes(id))
  const from = addDays(date, -29)
  const total = { due: 0, taken: 0, skipped: 0, missed: 0, onTime: 0 }
  for (const m of mine) {
    const a = adherence(m.id, from, date)
    total.due += a.due
    total.taken += a.taken
    total.skipped += a.skipped
    total.missed += a.missed
    total.onTime += a.onTime
  }
  return {
    condition,
    targets: condition.plan.targets.map((t) => standing(t, date)),
    labsDue: condition.plan.labs.map((l) => labDue(l, date)),
    medications: mine,
    adherence: total,
    alerts: ruleAlerts(from, date).filter((a) => mine.some((m) => m.id === a.medicationId)),
  }
}

function healthToday(date: string): HealthDay {
  if (!isDay(date)) throw invalid('A day is a date.')
  const night = nights.find((n) => n.date === date)
  const glucose = glucoseReport(date, date)
  const soon = addDays(date, 30)
  const staleMs = settings.staleHours * HOUR
  return {
    date,
    activity: activityDay(date),
    goals: settings.goals,
    ...(night ? { sleep: nightRecord(night) } : {}),
    latest: latestOf(
      [
        'resting_heart_rate',
        'heart_rate_variability',
        'blood_pressure_systolic',
        'blood_pressure_diastolic',
        'weight',
        'oxygen_saturation',
        'respiratory_rate',
        'vo2_max',
      ],
      date,
    ),
    dosesDue: dayDoses(date),
    nutrition: nutritionDays(date, date)[0] as NutritionDay,
    workouts: workouts.filter((w) => dayOf(Date.parse(w.start)) === date),
    ...(glucose.readings > 0 ? { glucose } : {}),
    alerts: ruleAlerts(date, date),
    labsDue: conditions
      .filter((c) => c.status === 'active')
      .flatMap((c) => c.plan.labs.map((l) => labDue(l, date)))
      .filter((d) => d.due && d.due <= soon),
    staleSources: sources.filter((s) => {
      const heard = s.status.lastReceivedAt ? Date.parse(s.status.lastReceivedAt) : 0
      return (
        s.connector === 'healthAutoExport' &&
        !s.paused &&
        staleMs > 0 &&
        Date.now() - heard > staleMs
      )
    }),
  }
}

// ── labs ─────────────────────────────────────────────────────────────────

function labTrends(): LabTrend[] {
  const groups = new Map<string, LabResult[]>()
  for (const l of labs) {
    const key = labKey(l)
    const list = groups.get(key)
    if (list) list.push(l)
    else groups.set(key, [l])
  }
  return [...groups.entries()]
    .map(([key, list]): LabTrend => {
      const results = [...list].sort((a, b) => a.collected.localeCompare(b.collected))
      const latest = results.at(-1) as LabResult
      return { key, name: latest.name, unit: latest.unit, results }
    })
    .sort((a, b) => a.name.localeCompare(b.name))
}

// ── saving ───────────────────────────────────────────────────────────────

function upsert<T extends { id: string }>(list: T[], record: T): T {
  const at = list.findIndex((r) => r.id === record.id)
  if (at >= 0) list[at] = record
  else list.push(record)
  return record
}

function remove<T extends { id: string }>(list: T[], id: string, what: string): T {
  const at = list.findIndex((r) => r.id === id)
  if (at < 0) throw notFound(what)
  return list.splice(at, 1)[0] as T
}

function needId(record: { id?: unknown } | undefined, what: string): void {
  if (!record || typeof record.id !== 'string' || !record.id)
    throw invalid(`A ${what} needs an id.`)
}

function saveMedication(input: Medication): Medication {
  needId(input, 'medication')
  const med = wire(input)
  med.name = med.name.trim()
  if (!med.name) throw invalid('A medication needs a name.')
  const before = medications.find((m) => m.id === med.id)
  const now = new Date().toISOString()
  // A new dose or strength is a change worth remembering: what a recheck is
  // counted from.
  const last = med.history.at(-1)
  if (before && (before.dose !== med.dose || before.strength !== med.strength)) {
    if (!last || last.dose !== med.dose || last.strength !== med.strength) {
      med.history = [
        ...med.history,
        { on: TODAY, dose: med.dose, strength: med.strength, note: '' },
      ]
    }
  } else if (!before && med.history.length === 0) {
    med.history = [
      { on: med.started ?? TODAY, dose: med.dose, strength: med.strength, note: 'Started' },
    ]
  }
  med.updatedAt = now
  if (!before) med.createdAt = now
  return upsert(medications, med)
}

function logDose(input: Dose): Dose {
  needId(input, 'dose')
  const dose = wire(input)
  const med = medications.find((m) => m.id === dose.medicationId)
  if (!med) throw notFound('medication')
  if (!Number.isFinite(Date.parse(dose.at))) throw invalid('A dose needs a time.')
  if (dose.status === 'missed')
    throw invalid('A missed dose is written by the reminders, not by hand.')
  const fresh = !doses.some((d) => d.id === dose.id)
  const now = new Date().toISOString()
  dose.updatedAt = now
  if (fresh) dose.createdAt = now
  upsert(doses, dose)
  // A new taken dose comes off the supply.
  if (fresh && dose.status === 'taken' && med.supply) {
    med.supply.remaining = Math.max(0, med.supply.remaining - (dose.amount ?? med.dose))
  }
  return dose
}

function applyTemplate(key: TemplateKey, choice: TemplateChoice): Condition {
  const info = template(key)
  const now = new Date().toISOString()
  const condition: Condition = {
    id: nextId('cond'),
    name: info.name,
    template: key,
    status: 'active',
    diagnosed: choice.diagnosed ?? null,
    codes: wire(info.codes),
    plan: wire(choice.plan ?? info.plan),
    notes: '',
    createdAt: now,
    updatedAt: now,
  }
  conditions.push(condition)
  return condition
}

function extract(id: string, text: string | null): HealthDocument {
  const doc = documents.find((d) => d.id === id)
  if (!doc) throw notFound('document')
  if (doc.files.length === 0) throw invalid('That document has no files to read.')
  if (text) doc.text = text.slice(0, 120_000)
  const date = doc.date ?? TODAY
  doc.extraction = 'proposed'
  doc.extractionError = ''
  const found = (
    name: string,
    c: string,
    v: number,
    unit: string,
    lo: number | null,
    hi: number | null,
  ) =>
    lab(nextId('lab-x'), c, name, v, unit, lo, hi, daysApart(date, TODAY), {
      origin: 'extracted',
      documentId: doc.id,
    })
  doc.pendingLabs = [
    found('Sodium', '2951-2', 139, 'mmol/L', 135, 145),
    found('Potassium', '2823-3', 4.4, 'mmol/L', 3.5, 5.1),
    found('Creatinine', '2160-0', 0.9, 'mg/dL', 0.6, 1.3),
  ]
  // One the model was unsure of: its doubt rides in the result's note until
  // the person saves the result, as the real reading does it.
  doc.pendingLabs = doc.pendingLabs.map((l) =>
    l.name === 'Potassium' ? { ...l, note: 'a digit is hard to read' } : l,
  )
  doc.updatedAt = new Date().toISOString()
  return doc
}

function acceptLabs(id: string, input: LabResult[]): LabResult[] {
  const doc = documents.find((d) => d.id === id)
  if (!doc) throw notFound('document')
  const saved: LabResult[] = []
  const now = new Date().toISOString()
  for (const lab of wire(input)) {
    if (!lab.name.trim()) throw invalid('A result needs a name.')
    const made: LabResult = {
      ...lab,
      id: lab.id || nextId('lab'),
      documentId: id,
      origin: 'extracted',
      createdAt: now,
      updatedAt: now,
    }
    upsert(labs, made)
    saved.push(made)
  }
  const accepted = new Set(saved.map((l) => l.id))
  doc.pendingLabs = doc.pendingLabs.filter((l) => !accepted.has(l.id))
  if (doc.pendingLabs.length === 0) doc.extraction = 'done'
  doc.updatedAt = now
  return saved
}

// ── foods ────────────────────────────────────────────────────────────────

/** What a lookup service would find: a few USDA foods and two barcodes. */
const LOOKUP: {
  name: string
  per100g: Nutrients
  fdcId: number
  serving: string
  grams: number
}[] = [
  {
    name: 'Hummus, commercial',
    per100g: nutrients(166, 7.9, 14.3, 9.6, { fiberG: 6 }),
    fdcId: 174,
    serving: '2 tbsp',
    grams: 30,
  },
  {
    name: 'Salmon, Atlantic, cooked',
    per100g: nutrients(206, 22, 0, 12.4),
    fdcId: 175,
    serving: '1 fillet',
    grams: 154,
  },
  {
    name: 'Quinoa, cooked',
    per100g: nutrients(120, 4.4, 21.3, 1.9, { fiberG: 2.8 }),
    fdcId: 176,
    serving: '1 cup',
    grams: 185,
  },
  {
    name: 'Avocado',
    per100g: nutrients(160, 2, 8.5, 14.7, { fiberG: 6.7 }),
    fdcId: 177,
    serving: '1 avocado',
    grams: 201,
  },
  {
    name: 'Sweet potato, baked',
    per100g: nutrients(90, 2, 20.7, 0.2, { fiberG: 3.3 }),
    fdcId: 178,
    serving: '1 medium',
    grams: 114,
  },
]

function lookupFood(over: {
  name: string
  per100g: Nutrients
  source: Food['source']
  serving: string
  grams: number
  brand?: string
  barcode?: string
}): Food {
  const now = new Date().toISOString()
  return {
    id: nextId('food'),
    name: over.name,
    brand: over.brand ?? '',
    barcode: over.barcode ?? '',
    source: over.source,
    per100g: over.per100g,
    servings: [{ name: over.serving, grams: over.grams }],
    favorite: false,
    lastUsed: null,
    useCount: 0,
    createdAt: now,
    updatedAt: now,
  }
}

function listFoods(query: string): Food[] {
  const q = query.trim().toLowerCase()
  return foods
    .filter((f) => !q || `${f.name} ${f.brand} ${f.barcode}`.toLowerCase().includes(q))
    .sort(
      (a, b) =>
        Number(b.favorite) - Number(a.favorite) ||
        b.useCount - a.useCount ||
        a.name.localeCompare(b.name),
    )
}

function searchFoods(query: string): FoodHit[] {
  const q = query.trim().toLowerCase()
  if (!q) return []
  const mine = listFoods(q).map((food): FoodHit => ({ food, origin: 'mine' }))
  const elsewhere = LOOKUP.filter((f) => f.name.toLowerCase().includes(q)).map((f): FoodHit => ({
    food: lookupFood({
      name: f.name,
      per100g: f.per100g,
      source: { type: 'usda', fdcId: f.fdcId },
      serving: f.serving,
      grams: f.grams,
    }),
    origin: 'usda',
  }))
  return [...mine, ...elsewhere]
}

function lookupBarcode(code: string): FoodHit | null {
  const codes: Record<
    string,
    { name: string; brand: string; per100g: Nutrients; serving: string; grams: number }
  > = {
    '5449000000996': {
      name: 'Cola',
      brand: 'Coca-Cola',
      per100g: nutrients(42, 0, 10.6, 0, { sugarG: 10.6 }),
      serving: '1 can',
      grams: 330,
    },
    '8901058000290': {
      name: 'Instant noodles',
      brand: 'Maggi',
      per100g: nutrients(436, 9.1, 60.2, 17.1, { sodiumMg: 1190 }),
      serving: '1 pack',
      grams: 70,
    },
  }
  const found = codes[code.trim()]
  if (!found) return null
  return {
    food: lookupFood({
      name: found.name,
      brand: found.brand,
      barcode: code.trim(),
      per100g: found.per100g,
      source: { type: 'openFoodFacts', code: code.trim() },
      serving: found.serving,
      grams: found.grams,
    }),
    origin: 'openFoodFacts',
  }
}

function saveMeal(input: Meal): Meal {
  needId(input, 'meal')
  const meal = wire(input)
  if (!Number.isFinite(Date.parse(meal.at))) throw invalid('A meal needs a time.')
  const before = meals.find((m) => m.id === meal.id)
  const fresh = !before
  const now = new Date().toISOString()
  meal.updatedAt = now
  // A photo the saved meal no longer has goes to the Files trash; one it has
  // is used by it.
  if (before?.photo && before.photo !== meal.photo) {
    mockFilesReleased([before.photo], { kind: 'meal', id: meal.id })
  }
  if (meal.photo) {
    mockFileUsed(meal.photo, {
      owner: { kind: 'meal', id: meal.id },
      title: meal.name,
      when: meal.at,
    })
  }
  if (fresh) {
    meal.createdAt = now
    // What was eaten shows on the foods eaten.
    for (const item of meal.items) {
      const f = foods.find((x) => x.id === item.foodId)
      if (!f) continue
      f.useCount += 1
      f.lastUsed = meal.at
    }
  }
  upsert(meals, meal)
  meals.sort((a, b) => Date.parse(a.at) - Date.parse(b.at))
  return meal
}

function saveFood(input: Food): Food {
  needId(input, 'food')
  const f = wire(input)
  f.name = f.name.trim()
  if (!f.name) throw invalid('A food needs a name.')
  const now = new Date().toISOString()
  f.updatedAt = now
  if (!foods.some((x) => x.id === f.id)) f.createdAt = now
  return upsert(foods, f)
}

// ── the visit brief ──────────────────────────────────────────────────────

function visitBrief(conditionId: string | null, since: string): VisitBrief {
  if (!isDay(since)) throw invalid('A brief starts on a date.')
  const picked = conditions.filter((c) => !conditionId || c.id === conditionId)
  if (conditionId && picked.length === 0) throw notFound('condition')
  const lines: string[] = [`# Visit summary since ${since}`, '']
  for (const c of picked) {
    const report = conditionReport(c.id, TODAY)
    lines.push(`## ${c.name}`, '')
    for (const t of report.targets) {
      lines.push(
        `- ${t.target.label}: ${t.state === 'met' ? 'met' : t.state === 'notMet' ? 'not met' : 'no data'}`,
      )
    }
    for (const d of report.labsDue) {
      if (d.overdue || (d.due && d.due <= addDays(TODAY, 30)))
        lines.push(`- ${d.schedule.name} is due (${d.reason})`)
    }
    lines.push(
      `- Doses taken ${report.adherence.taken} of ${report.adherence.due} in the last 30 days`,
      '',
    )
  }
  const recent = labs
    .filter((l) => l.collected >= since)
    .sort((a, b) => a.collected.localeCompare(b.collected))
  if (recent.length > 0) {
    lines.push('## Results since then', '')
    for (const l of recent) lines.push(`- ${l.name}: ${l.value ?? l.text} ${l.unit}`.trim())
    lines.push('')
  }
  lines.push('## Medications', '')
  for (const m of medications.filter((x) => x.status === 'active'))
    lines.push(`- ${m.name} ${m.strength}`.trim())
  if (allergies.length > 0) {
    lines.push('', '## Allergies', '')
    for (const a of allergies) lines.push(`- ${a.substance}: ${a.reaction}`.trim())
  }
  return { markdown: lines.join('\n'), since, ...(conditionId ? { conditionId } : {}) }
}

// ── importing an Apple export ────────────────────────────────────────────

/** How long a pretend import "runs" before it is done. */
const IMPORT_MS = 800

function importStatus(): ImportStatus {
  if (!imported) {
    return (
      importDone ?? {
        state: 'idle',
        fileName: '',
        read: 0,
        total: 0,
        report: emptyReport(),
        error: '',
      }
    )
  }
  const elapsed = Date.now() - imported.startedAt
  if (imported.cancelled) {
    const status: ImportStatus = {
      state: 'cancelled',
      fileName: imported.fileName,
      read: Math.round(imported.total * clamp(elapsed / IMPORT_MS, 0, 1)),
      total: imported.total,
      report: emptyReport(),
      error: '',
      startedAt: iso(imported.startedAt),
      finishedAt: iso(Date.now()),
    }
    // However it ended: an upload made only to be imported is done with.
    if (imported.trashAfter) mockFilesReleased([imported.trashAfter])
    importDone = status
    imported = null
    return status
  }
  if (elapsed < IMPORT_MS) {
    return {
      state: 'running',
      fileName: imported.fileName,
      read: Math.round((imported.total * elapsed) / IMPORT_MS),
      total: imported.total,
      report: emptyReport(),
      error: '',
      startedAt: iso(imported.startedAt),
    }
  }
  const status: ImportStatus = {
    state: 'done',
    fileName: imported.fileName,
    read: imported.total,
    total: imported.total,
    report: {
      samplesAdded: 12_480,
      samplesDuplicate: 903_212,
      samplesRejected: 3,
      workoutsAdded: 4,
      workoutsUpdated: 0,
      dosesAdded: 0,
      dosesUnmatched: 0,
      documentsAdded: 0,
      skipped: { HKQuantityTypeIdentifierAppleWalkingSteadiness: 211 },
    },
    error: '',
    startedAt: iso(imported.startedAt),
    finishedAt: iso(imported.startedAt + IMPORT_MS),
  }
  const apple = sources.find((s) => s.connector === 'appleExport')
  if (apple) {
    apple.status.lastReceivedAt = status.finishedAt ?? null
    apple.status.samplesTotal += status.report.samplesAdded
  }
  // Read: an upload made only to be imported is done with.
  if (imported.trashAfter) mockFilesReleased([imported.trashAfter])
  importDone = status
  imported = null
  return status
}

function emptyReport() {
  return {
    samplesAdded: 0,
    samplesDuplicate: 0,
    samplesRejected: 0,
    workoutsAdded: 0,
    workoutsUpdated: 0,
    dosesAdded: 0,
    dosesUnmatched: 0,
    documentsAdded: 0,
    skipped: {},
  }
}

function startImport(fileName: string, trashAfter?: string): ImportStatus {
  if (imported) throw invalid('An import is already running.')
  imported = {
    startedAt: Date.now(),
    fileName,
    total: 482_000_000,
    cancelled: false,
    ...(trashAfter ? { trashAfter } : {}),
  }
  importDone = null
  return importStatus()
}

// ── the dispatcher ───────────────────────────────────────────────────────

/** Every vault command this file answers; each wants the vault open. */
export const MOCK_HEALTH_COMMANDS: ReadonlySet<string> = new Set([
  'health_catalog',
  'health_settings',
  'save_health_settings',
  'list_health_sources',
  'save_health_source',
  'delete_health_source',
  'record_health_push',
  'import_apple_health',
  'import_apple_health_file',
  'apple_import_status',
  'cancel_apple_import',
  'health_keys',
  'save_health_keys',
  'clear_health_keys',
  'health_today',
  'health_series',
  'health_samples',
  'health_latest',
  'add_health_reading',
  'delete_health_samples',
  'health_activity',
  'health_sleep',
  'health_glucose',
  'health_nutrition',
  'health_day_timeline',
  'list_workouts',
  'get_workout',
  'save_workout',
  'delete_workout',
  'list_medications',
  'save_medication',
  'delete_medication',
  'list_doses',
  'log_dose',
  'delete_dose',
  'doses_due',
  'medication_alerts',
  'medication_adherence',
  'list_allergies',
  'save_allergy',
  'delete_allergy',
  'list_conditions',
  'save_condition',
  'delete_condition',
  'condition_templates',
  'apply_condition_template',
  'condition_report',
  'list_labs',
  'save_lab',
  'delete_lab',
  'lab_trends',
  'list_health_documents',
  'get_health_document',
  'add_health_document',
  'save_health_document',
  'delete_health_document',
  'extract_health_document',
  'accept_document_labs',
  'list_foods',
  'save_food',
  'delete_food',
  'search_foods',
  'lookup_barcode',
  'list_meals',
  'save_meal',
  'delete_meal',
  'estimate_meal_photo',
  'meal_response',
  'health_visit_brief',
])

/** The shell's `health_*` commands: this computer's, answered locked or not. */
export const MOCK_HEALTH_SHELL_COMMANDS: ReadonlySet<string> = new Set([
  'health_push_setup',
  'health_push_revoke',
  'health_import_apple',
])

/**
 * Answer one Health command, or one of the shell's `health_*` calls. Throws
 * a `VaultError` for what the real vault would refuse.
 */
export function mockHealth(cmd: string, args: Record<string, unknown>): unknown {
  ensure()
  switch (cmd) {
    case 'health_catalog':
      return wire(CATALOG)
    case 'health_settings':
      return wire(settings)
    case 'save_health_settings': {
      const next = wire(args.settings as HealthSettings)
      if (!['mg/dL', 'mmol/L'].includes(next.units.glucose))
        throw invalid('Glucose is mg/dL or mmol/L.')
      if (!['kg', 'lb'].includes(next.units.weight)) throw invalid('Weight is kg or lb.')
      if (!['km', 'mi'].includes(next.units.distance)) throw invalid('Distance is km or mi.')
      if (!['kcal', 'kJ'].includes(next.units.energy)) throw invalid('Energy is kcal or kJ.')
      if (!['C', 'F'].includes(next.units.temperature)) throw invalid('Temperature is C or F.')
      if (!['cm', 'in'].includes(next.units.length)) throw invalid('Length is cm or in.')
      settings = next
      return undefined
    }
    case 'list_health_sources':
      return wire(sources)
    case 'save_health_source': {
      const input = wire(args.source as HealthSource)
      needId(input, 'source')
      const name = input.name.trim()
      if (!name) throw invalid('A source needs a name.')
      const found = sources.find((s) => s.id === input.id)
      const now = new Date().toISOString()
      // What a person chose is saved; what arrival learned is kept as stored.
      if (found) {
        found.name = name
        found.paused = input.paused
        found.updatedAt = now
        return wire(found)
      }
      const made: HealthSource = {
        ...input,
        name,
        status: { lastError: '', samplesTotal: 0, automations: [], devices: [] },
        tokenId: '',
        createdAt: now,
        updatedAt: now,
      }
      sources.push(made)
      return wire(made)
    }
    case 'delete_health_source':
      remove(sources, str(args.id), 'source')
      return undefined
    case 'record_health_push': {
      const source = sources.find((s) => s.id === str(args.sourceId))
      if (!source) throw notFound('source')
      if (source.paused) throw new VaultError('paused', 'That source is paused.')
      source.status.lastReceivedAt = new Date().toISOString()
      return emptyReport()
    }
    case 'import_apple_health':
      return wire(startImport(str(args.path).split(/[\\/]/).pop() || 'export.zip'))
    case 'import_apple_health_file': {
      // An export that is in Files already, brought by a sync tool or
      // uploaded from a window on another machine.
      const entry = mockCatalogFile(str(args.file))
      if (!entry) throw notFound('file')
      return wire(startImport(entry.name, args.trashAfter === true ? entry.id : undefined))
    }
    case 'apple_import_status':
      return wire(importStatus())
    case 'cancel_apple_import': {
      if (!imported) return false
      imported.cancelled = true
      return true
    }
    case 'health_keys':
      return { fdcKeySet: fdcKey !== '', fdcKeyHint: fdcKey.slice(-4) }
    case 'save_health_keys': {
      const key = str(args.fdcKey).trim()
      if (!key) throw invalid('A key is not empty.')
      fdcKey = key
      return undefined
    }
    case 'clear_health_keys':
      fdcKey = ''
      return undefined

    case 'health_today':
      return wire(healthToday(str(args.date)))
    case 'health_series':
      return wire(series(args.query as SeriesQuery))
    case 'health_samples': {
      const metric = args.metric as Metric
      const from = Date.parse(str(args.from))
      const to = Date.parse(str(args.to))
      if (!ENTRY.has(metric)) throw invalid('That is not a metric.')
      if (!Number.isFinite(from) || !Number.isFinite(to) || to < from)
        throw invalid('A range is two instants.')
      if (to - from > 3 * DAY) throw invalid('Samples are read for at most three days.')
      return wire(between(metric, from, to).map(point))
    }
    case 'health_latest': {
      const on = str(args.on)
      if (!isDay(on)) throw invalid('A day is a date.')
      const metrics = (Array.isArray(args.metrics) ? args.metrics : VITALS_METRICS) as Metric[]
      return wire(latestOf(metrics, on))
    }
    case 'add_health_reading':
      addReading(wire(args.reading as ManualReading))
      return undefined
    case 'delete_health_samples':
      return deleteSamples(
        args.metric as Metric,
        str(args.from),
        str(args.to),
        strOrNull(args.source),
      )
    case 'health_activity':
      return wire(eachDay(str(args.from), str(args.to)).map(activityDay))
    case 'health_sleep':
      return wire(sleepNights(str(args.from), str(args.to)))
    case 'health_glucose':
      return wire(glucoseReport(str(args.from), str(args.to)))
    case 'health_nutrition':
      return wire(nutritionDays(str(args.from), str(args.to)))
    case 'health_day_timeline':
      return wire(dayTimeline(str(args.date)))

    case 'list_workouts': {
      const from = Date.parse(str(args.from))
      const to = Date.parse(str(args.to))
      return wire(workouts.filter((w) => Date.parse(w.start) >= from && Date.parse(w.start) < to))
    }
    case 'get_workout': {
      const workout = workouts.find((w) => w.id === str(args.id))
      if (!workout) throw notFound('workout')
      const detail: WorkoutDetail = {
        workout,
        heartRate: between('heart_rate', Date.parse(workout.start), Date.parse(workout.end)).map(
          point,
        ),
        route: routeOf(workout),
      }
      return wire(detail)
    }
    case 'save_workout': {
      const workout = wire(args.workout as Workout)
      needId(workout, 'workout')
      if (Date.parse(workout.end) <= Date.parse(workout.start))
        throw invalid('A workout ends after it starts.')
      workout.updatedAt = new Date().toISOString()
      return wire(upsert(workouts, workout))
    }
    case 'delete_workout':
      remove(workouts, str(args.id), 'workout')
      return undefined

    case 'list_medications':
      return wire(
        [...medications].sort((a, b) => a.sortOrder - b.sortOrder || a.name.localeCompare(b.name)),
      )
    case 'save_medication':
      return wire(saveMedication(args.medication as Medication))
    case 'delete_medication': {
      const gone = remove(medications, str(args.id), 'medication')
      doses = doses.filter((d) => d.medicationId !== gone.id)
      return undefined
    }
    case 'list_doses': {
      const from = Date.parse(str(args.from))
      const to = Date.parse(str(args.to))
      const only = strOrNull(args.medicationId)
      return wire(
        doses.filter(
          (d) =>
            Date.parse(d.at) >= from &&
            Date.parse(d.at) < to &&
            (only === null || d.medicationId === only),
        ),
      )
    }
    case 'log_dose':
      return wire(logDose(args.dose as Dose))
    case 'delete_dose':
      remove(doses, str(args.id), 'dose')
      return undefined
    case 'doses_due':
      if (!isDay(str(args.date))) throw invalid('A day is a date.')
      return wire(dayDoses(str(args.date)))
    case 'medication_alerts':
      return wire(ruleAlerts(str(args.from), str(args.to)))
    case 'medication_adherence':
      return wire(adherence(strOrNull(args.medicationId), str(args.from), str(args.to)))
    case 'list_allergies':
      return wire(allergies)
    case 'save_allergy': {
      const allergy = wire(args.allergy as Allergy)
      needId(allergy, 'allergy')
      allergy.substance = allergy.substance.trim()
      if (!allergy.substance) throw invalid('An allergy needs a substance.')
      allergy.updatedAt = new Date().toISOString()
      return wire(upsert(allergies, allergy))
    }
    case 'delete_allergy':
      remove(allergies, str(args.id), 'allergy')
      return undefined

    case 'list_conditions':
      return wire(conditions)
    case 'save_condition': {
      const condition = wire(args.condition as Condition)
      needId(condition, 'condition')
      condition.name = condition.name.trim()
      if (!condition.name) throw invalid('A condition needs a name.')
      condition.updatedAt = new Date().toISOString()
      return wire(upsert(conditions, condition))
    }
    case 'delete_condition':
      remove(conditions, str(args.id), 'condition')
      for (const m of medications) m.conditionIds = m.conditionIds.filter((c) => c !== str(args.id))
      return undefined
    case 'condition_templates':
      return wire(TEMPLATES)
    case 'apply_condition_template':
      return wire(
        applyTemplate(args.template as TemplateKey, wire((args.choice ?? {}) as TemplateChoice)),
      )
    case 'condition_report':
      return wire(conditionReport(str(args.id), str(args.date)))

    case 'list_labs': {
      const key = strOrNull(args.key)
      return wire(
        labs
          .filter((l) => key === null || labKey(l) === key)
          .sort((a, b) => b.collected.localeCompare(a.collected) || a.name.localeCompare(b.name)),
      )
    }
    case 'save_lab': {
      const result = wire(args.lab as LabResult)
      needId(result, 'result')
      if (!result.name.trim()) throw invalid('A result needs a name.')
      if (!isDay(result.collected)) throw invalid('A result needs the date it was collected.')
      const now = new Date().toISOString()
      if (!labs.some((l) => l.id === result.id)) result.createdAt = now
      result.updatedAt = now
      return wire(upsert(labs, result))
    }
    case 'delete_lab':
      remove(labs, str(args.id), 'result')
      return undefined
    case 'lab_trends':
      return wire(labTrends())

    case 'list_health_documents':
      return wire([...documents].sort((a, b) => (b.date ?? '').localeCompare(a.date ?? '')))
    case 'get_health_document': {
      const doc = documents.find((d) => d.id === str(args.id))
      if (!doc) throw notFound('document')
      return wire(doc)
    }
    case 'add_health_document': {
      // The pages were added through Files first; what is made of them takes
      // their names, types and sizes from the catalog.
      const ids = (Array.isArray(args.files) ? args.files : []) as unknown[]
      const title = str(args.title).trim()
      if (!title) throw invalid('A document needs a title.')
      if (ids.length === 0) throw invalid('A document needs a file.')
      const files = ids.map((id): DocFile => {
        const entry = mockCatalogFile(str(id))
        if (!entry) throw notFound('file')
        return { file: entry.id, name: entry.name, mime: entry.mime, size: entry.size }
      })
      const date = strOrNull(args.date)
      const now = new Date().toISOString()
      const doc: HealthDocument = {
        id: nextId('doc'),
        title,
        kind: (args.kind as DocKind) ?? 'other',
        date,
        provider: '',
        files,
        text: '',
        fhir: [],
        extraction: 'none',
        extractionError: '',
        pendingLabs: [],
        conditionIds: [],
        externalId: '',
        notes: '',
        createdAt: now,
        updatedAt: now,
      }
      documents.push(doc)
      usePages(doc)
      return wire(doc)
    }
    case 'save_health_document': {
      const input = wire(args.document as HealthDocument)
      const doc = documents.find((d) => d.id === input.id)
      if (!doc) throw notFound('document')
      if (!input.title.trim()) throw invalid('A document needs a title.')
      // What a person may edit; files, text and extraction state are kept.
      doc.title = input.title.trim()
      doc.kind = input.kind
      doc.date = input.date ?? null
      doc.provider = input.provider
      doc.notes = input.notes
      doc.conditionIds = input.conditionIds
      doc.pendingLabs = input.pendingLabs
      doc.updatedAt = new Date().toISOString()
      return wire(doc)
    }
    case 'delete_health_document': {
      const gone = remove(documents, str(args.id), 'document')
      // Its pages go to the Files trash, where they can be restored.
      mockFilesReleased(
        gone.files.map((f) => f.file),
        { kind: 'healthDocument', id: gone.id },
      )
      if (args.deleteLabs === true) labs = labs.filter((l) => l.documentId !== gone.id)
      else for (const l of labs) if (l.documentId === gone.id) l.documentId = null
      return undefined
    }
    case 'extract_health_document':
      return wire(extract(str(args.id), strOrNull(args.text)))
    case 'accept_document_labs':
      return wire(
        acceptLabs(str(args.id), (Array.isArray(args.labs) ? args.labs : []) as LabResult[]),
      )

    case 'list_foods':
      return wire(listFoods(str(args.query)))
    case 'save_food':
      return wire(saveFood(args.food as Food))
    case 'delete_food':
      remove(foods, str(args.id), 'food')
      return undefined
    case 'search_foods':
      return wire(searchFoods(str(args.query)))
    case 'lookup_barcode':
      return wire(lookupBarcode(str(args.code)))
    case 'list_meals': {
      const from = Date.parse(str(args.from))
      const to = Date.parse(str(args.to))
      return wire(meals.filter((m) => Date.parse(m.at) >= from && Date.parse(m.at) < to))
    }
    case 'save_meal':
      return wire(saveMeal(args.meal as Meal))
    case 'delete_meal': {
      const gone = remove(meals, str(args.id), 'meal')
      if (gone.photo) mockFilesReleased([gone.photo], { kind: 'meal', id: gone.id })
      return undefined
    }
    case 'estimate_meal_photo': {
      // The picture was added through Files first.
      if (!mockCatalogFile(str(args.photo))) throw notFound('photo')
      const guess = (
        name: string,
        grams: number,
        per100g: Nutrients,
        doubt?: string,
      ): MealItem => ({
        foodId: null,
        name,
        amount: grams,
        unit: 'g',
        grams,
        nutrients: scaleNutrients(per100g, grams),
        estimated: true,
        ...(doubt ? { doubt } : {}),
      })
      return wire([
        guess('Grilled chicken', 150, nutrients(165, 31, 0, 3.6)),
        guess('Rice', 180, nutrients(130, 2.7, 28.2, 0.3), '150-250 g; the bowl is deep'),
        guess('Green salad', 80, nutrients(20, 1.4, 3.6, 0.2)),
      ])
    }
    case 'meal_response':
      return wire(mealResponse(str(args.id)))
    case 'health_visit_brief':
      return wire(visitBrief(strOrNull(args.conditionId), str(args.since)))

    // This machine's half: the shell's own.
    case 'health_push_setup': {
      const source = sources.find((s) => s.id === str(args.sourceId))
      if (!source) throw notFound('source')
      // Minting again revokes the old one, as the shell does.
      const token = `mock-token-${newId().slice(0, 8)}`
      source.tokenId = token
      const setup: PushSetup = {
        url: `https://studio.tail1234.ts.net:7400/v1/health/hae/${source.id}`,
        token,
        webOn: true,
        problem: '',
      }
      return setup
    }
    case 'health_push_revoke': {
      const source = sources.find((s) => s.id === str(args.sourceId))
      // Whether there was a token to revoke.
      const had = !!source && source.tokenId !== ''
      if (source) source.tokenId = ''
      return had
    }
    case 'health_import_apple':
      // The picker answers a file; the mock always picks the same one.
      return wire(startImport('export.zip'))
  }
  throw new VaultError('unknown', `no mock for command ${cmd}`)
}
