// `lib/health.ts`: Health's rules that need no store.
//
// The type checker covers the shape of these; what it cannot cover is a rule
// whose failure is quiet. A glucose of 5.5 read as mg/dL is a reading eighteen
// times too low, a week that starts on the wrong day is a window that is
// subtly wrong every time it is looked at, a heart rate shown as "72
// count/min" is a unit nobody says, and a meal whose carbohydrate is summed
// with a float tail is a number somebody counts insulin by. None of them
// throws.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

// `locale()` reads `navigator.language`. British English, so the examples
// read as the plan writes them ("3–9 Nov"); Node's own `navigator` is a
// getter-only property, so it is redefined rather than assigned -- see
// `format.test.mjs`.
function speak(language) {
  Object.defineProperty(globalThis, 'navigator', {
    value: { userAgent: 'node', language },
    configurable: true,
  })
}
speak('en-GB')

const { module: h, close } = await load('/src/lib/health.ts')
const {
  DEFAULT_UNITS,
  GLUCOSE_BANDS,
  GLUCOSE_COLORS,
  HEALTH_ACCENT,
  METRICS,
  NO_NUTRIENTS,
  SLEEP_STAGE_COLORS,
  VITALS_METRICS,
  activityName,
  bucketFor,
  dayInstants,
  digitsFor,
  docKindName,
  durationText,
  formatMetric,
  formatNumber,
  fromDisplay,
  glucoseBand,
  glucoseBandName,
  gramsOf,
  isGlucoseCondition,
  itemFor,
  labKey,
  labOutOfRange,
  newId,
  newMeal,
  newMedication,
  rangeLabel,
  rangeOf,
  ruleText,
  scaleNutrients,
  scheduleText,
  slotAt,
  slotName,
  stageName,
  stepAnchor,
  sumNutrients,
  toDisplay,
  unitFor,
} = h

/** Intl puts thin and narrow no-break spaces around a range's dash and before
 *  AM; a test reads them as plain ones. */
const plain = (s) => s.replace(/[\u00a0\u2009\u202f]/g, ' ').replace(/\s+/g, ' ')
const near = (got, want, what, eps = 1e-6) =>
  assert.ok(Math.abs(got - want) <= eps, `${what}: ${got} is not ${want}`)

// ── rangeOf ──────────────────────────────────────────────────────────
//
// Rolling windows ending on the anchor: a body does not start again on
// Monday. The week start no longer decides anything.

assert.deepEqual(rangeOf('week', '2026-11-05', 1), { from: '2026-10-30', to: '2026-11-05' })
assert.deepEqual(
  rangeOf('week', '2026-11-05', 0),
  rangeOf('week', '2026-11-05', 1),
  'the reader’s week start does not move a rolling window',
)
assert.deepEqual(rangeOf('month', '2026-11-05', 1), { from: '2026-10-07', to: '2026-11-05' })
assert.deepEqual(rangeOf('quarter', '2026-11-05', 1), { from: '2026-08-07', to: '2026-11-05' })
assert.deepEqual(rangeOf('year', '2026-11-05', 1), { from: '2025-11-06', to: '2026-11-05' })
assert.deepEqual(
  rangeOf('month', '2028-03-01', 1),
  { from: '2028-02-01', to: '2028-03-01' },
  'thirty days across a leap February',
)

// ── stepAnchor ───────────────────────────────────────────────────────

assert.equal(stepAnchor('week', '2026-11-05', 1), '2026-11-12')
assert.equal(stepAnchor('week', '2026-11-05', -2), '2026-10-22')
assert.equal(stepAnchor('month', '2026-11-05', -1), '2026-10-06')
assert.equal(stepAnchor('quarter', '2026-11-05', 1), '2027-02-04')
assert.equal(stepAnchor('year', '2026-11-05', -1), '2025-11-05')
// A step lands on the window that follows, with no gap and no overlap.
for (const range of ['week', 'month', 'quarter', 'year']) {
  const here = rangeOf(range, '2026-11-05', 1)
  const next = rangeOf(range, stepAnchor(range, '2026-11-05', 1), 1)
  assert.equal(next.from, '2026-11-06', `${range}: the next window starts the day after this one`)
  assert.ok(next.from > here.to, `${range}: the next window starts after this one ends`)
}

// ── rangeLabel ───────────────────────────────────────────────────────

const today = '2026-11-05'
assert.equal(rangeLabel('week', '2026-10-30', today, today), 'Last 7 days')
assert.equal(rangeLabel('month', '2026-10-07', today, today), 'Last 30 days')
assert.equal(rangeLabel('quarter', '2026-08-07', today, today), 'Last 3 months')
assert.equal(rangeLabel('year', '2025-11-06', today, today), 'Last 12 months')
{
  const label = plain(rangeLabel('week', '2026-10-26', '2026-11-01', today))
  assert.match(label, /^26 Oct ?– ?1 Nov$/, `a window across two months names both: ${label}`)
}
{
  const label = plain(rangeLabel('week', '2026-10-19', '2026-10-25', today))
  assert.match(label, /^19 ?– ?25 Oct$/, `a window in one month names it once: ${label}`)
}
{
  const label = rangeLabel('week', '2025-12-29', '2026-01-04', today)
  assert.ok(label.includes('2025') && label.includes('2026'), `across two years: ${label}`)
}
{
  const label = rangeLabel('week', '2025-11-03', '2025-11-09', today)
  assert.ok(label.includes('2025'), `another year says which: ${label}`)
}
assert.equal(rangeLabel('week', 'nonsense', 'nonsense', today), '', 'not a day is no label')
assert.equal(
  rangeLabel('month', '2026-02-30', '2026-02-30', today),
  '',
  'nor a day that is not one',
)

// ── dayInstants and bucketFor ────────────────────────────────────────

assert.deepEqual(dayInstants('2026-11-05', '2026-11-07'), {
  from: new Date(2026, 10, 5).toISOString(),
  to: new Date(2026, 10, 8).toISOString(),
})
assert.deepEqual(
  dayInstants('2026-12-31', '2026-12-31'),
  { from: new Date(2026, 11, 31).toISOString(), to: new Date(2027, 0, 1).toISOString() },
  'one day runs to the next midnight, across a year end too',
)
assert.equal(bucketFor('2026-11-05', '2026-11-05'), 'day')
assert.equal(bucketFor('2026-11-02', '2026-11-08'), 'day')
assert.equal(bucketFor('2026-10-01', '2026-12-31'), 'day', 'a quarter is 92 days: still days')
assert.equal(bucketFor('2026-08-01', '2026-11-01'), 'week', 'ninety-three days is weeks')
assert.equal(bucketFor('2026-01-01', '2026-12-31'), 'week', 'a year is weeks')
assert.equal(bucketFor('2025-01-01', '2026-12-31'), 'month', 'beyond a year is months')

// ── unit conversion ──────────────────────────────────────────────────

const IMPERIAL = {
  glucose: 'mmol/L',
  weight: 'lb',
  distance: 'mi',
  energy: 'kJ',
  temperature: 'F',
  length: 'in',
}

near(toDisplay('blood_glucose', 99, IMPERIAL), 99 / 18.0156, 'mg/dL to mmol/L')
near(toDisplay('blood_glucose', 18.0156, IMPERIAL), 1, 'one mmol/L is 18.0156 mg/dL')
near(toDisplay('blood_glucose', 99, DEFAULT_UNITS), 99, 'and unchanged in mg/dL')
near(toDisplay('weight', 80, IMPERIAL), 176.3698, 'kg to lb', 1e-3)
near(toDisplay('lean_body_mass', 60, IMPERIAL), 132.2774, 'lean mass follows weight', 1e-3)
near(toDisplay('weight', 80, DEFAULT_UNITS), 80, 'kg stays kg')
near(toDisplay('walking_running_distance', 5000, DEFAULT_UNITS), 5, 'metres to kilometres')
near(toDisplay('cycling_distance', 5000, IMPERIAL), 5000 / 1609.344, 'metres to miles')
near(toDisplay('active_energy', 100, IMPERIAL), 418.4, 'kcal to kJ')
near(toDisplay('dietary_energy', 100, DEFAULT_UNITS), 100, 'kcal stays kcal')
near(toDisplay('body_temperature', 37, IMPERIAL), 98.6, '37 C is 98.6 F')
near(toDisplay('body_temperature', 37, DEFAULT_UNITS), 37, 'C stays C')
near(
  toDisplay('wrist_temperature', 0.5, IMPERIAL),
  0.9,
  'a wrist deviation of half a degree is 0.9 F: a difference gets no 32 added',
)
near(toDisplay('height', 1.78, DEFAULT_UNITS), 178, 'metres to centimetres')
near(toDisplay('height', 1.78, IMPERIAL), 1.78 / 0.0254, 'metres to inches')
near(toDisplay('heart_rate', 72, IMPERIAL), 72, 'a heart rate has no unit to choose')
near(toDisplay('steps', 8432, IMPERIAL), 8432)

// Every metric with a choice, both ways and through each unit, comes back.
for (const units of [DEFAULT_UNITS, IMPERIAL, { ...IMPERIAL, glucose: 'mg/dL', length: 'cm' }]) {
  for (const metric of METRICS) {
    for (const value of [0, 0.5, 1, 7.25, 98.6, 180, 1234.5]) {
      near(
        fromDisplay(metric, toDisplay(metric, value, units), units),
        value,
        `${metric} ${value} round-trips under ${JSON.stringify(units)}`,
        1e-9,
      )
    }
  }
}
near(fromDisplay('blood_glucose', 5.5, IMPERIAL), 99.0858, 'a typed 5.5 mmol/L is 99 mg/dL', 1e-4)
near(fromDisplay('weight', 180, IMPERIAL), 81.6466, 'a typed 180 lb', 1e-3)
near(fromDisplay('body_temperature', 98.6, IMPERIAL), 37, 'a typed 98.6 F', 1e-9)

// ── unitFor and formatMetric ─────────────────────────────────────────

const CATALOG = [
  ['steps', 'count'],
  ['flights_climbed', 'count'],
  ['stand_hour', 'count'],
  ['bmi', 'kg/m2'],
  ['heart_rate', 'count/min'],
  ['resting_heart_rate', 'count/min'],
  ['respiratory_rate', 'count/min'],
  ['heart_rate_variability', 'ms'],
  ['vo2_max', 'mL/kg/min'],
  ['oxygen_saturation', '%'],
  ['body_fat', '%'],
  ['insulin_bolus', 'IU'],
  ['exercise_time', 'min'],
  ['weight', 'kg'],
  ['blood_glucose', 'mg/dL'],
  ['walking_running_distance', 'm'],
  ['active_energy', 'kcal'],
  ['body_temperature', 'degC'],
  ['wrist_temperature', 'degC'],
  ['height', 'm'],
  ['blood_pressure_systolic', 'mmHg'],
  ['dietary_carbs', 'g'],
  ['sleep_stage', 'stage'],
].map(([metric, unit]) => ({
  metric,
  name: metric,
  unit,
  kind: 'discrete',
  group: 'activity',
  stat: { type: 'mean' },
}))

const unit = (metric, units = DEFAULT_UNITS) => unitFor(metric, units, CATALOG)
assert.equal(unit('steps'), 'steps')
assert.equal(unit('flights_climbed'), 'floors')
assert.equal(unit('stand_hour'), 'hours')
assert.equal(unit('bmi'), '', 'a body mass index has no unit to say')
assert.equal(unit('heart_rate'), 'bpm', 'never count/min')
assert.equal(unit('resting_heart_rate'), 'bpm')
assert.equal(unit('respiratory_rate'), 'breaths/min')
assert.equal(unit('heart_rate_variability'), 'ms')
assert.equal(unit('vo2_max'), 'mL/kg/min')
assert.equal(unit('oxygen_saturation'), '%')
assert.equal(unit('body_fat'), '%')
assert.equal(unit('insulin_bolus'), 'U')
assert.equal(unit('blood_pressure_systolic'), 'mmHg')
assert.equal(unit('dietary_carbs'), 'g')
assert.equal(unit('sleep_stage'), '')
assert.equal(unit('weight'), 'kg')
assert.equal(unit('weight', IMPERIAL), 'lb')
assert.equal(unit('blood_glucose'), 'mg/dL')
assert.equal(unit('blood_glucose', IMPERIAL), 'mmol/L')
assert.equal(unit('walking_running_distance'), 'km')
assert.equal(unit('walking_running_distance', IMPERIAL), 'mi')
assert.equal(unit('active_energy'), 'kcal')
assert.equal(unit('active_energy', IMPERIAL), 'kJ')
assert.equal(unit('body_temperature'), '°C')
assert.equal(unit('body_temperature', IMPERIAL), '°F')
assert.equal(unit('wrist_temperature', IMPERIAL), '°F')
assert.equal(unit('height'), 'cm')
assert.equal(unit('height', IMPERIAL), 'in')
for (const entry of CATALOG) {
  for (const units of [DEFAULT_UNITS, IMPERIAL]) {
    const shown = unitFor(entry.metric, units, CATALOG)
    assert.ok(
      !['count', 'count/min', 'degC', 'degF', 'IU', 'stage'].includes(shown),
      `${entry.metric} must never show the vault's spelling: ${shown}`,
    )
  }
}

const fmt = (metric, value, units = DEFAULT_UNITS) => formatMetric(metric, value, units, CATALOG)
assert.equal(fmt('heart_rate', 72), '72 bpm')
assert.equal(fmt('heart_rate', 71.6), '72 bpm', 'beats are whole')
assert.equal(fmt('weight', 81.4), '81.4 kg')
assert.equal(fmt('weight', 81.40001), '81.4 kg', 'a decimal that is noise is not drawn')
assert.equal(fmt('weight', 80, IMPERIAL), '176.4 lb')
assert.equal(fmt('steps', 8432), '8,432 steps')
assert.equal(fmt('steps', 1), '1 step', 'one of a count is singular')
assert.equal(fmt('flights_climbed', 1), '1 floor')
assert.equal(fmt('flights_climbed', 12), '12 floors')
assert.equal(fmt('stand_hour', 1), '1 hour')
assert.equal(fmt('stand_hour', 11), '11 hours')
assert.equal(fmt('exercise_time', 72), '1 h 12 min', 'minutes read as a duration')
assert.equal(fmt('exercise_time', 45), '45 min')
assert.equal(fmt('oxygen_saturation', 97), '97 %')
assert.equal(fmt('bmi', 27.46), '27.5', 'no unit leaves no trailing space')
assert.equal(fmt('blood_glucose', 99), '99 mg/dL')
assert.equal(fmt('blood_glucose', 99, IMPERIAL), '5.5 mmol/L')
assert.equal(fmt('body_temperature', 36.6), '36.6 °C')
assert.equal(fmt('body_temperature', 36.6, IMPERIAL), '97.9 °F')
assert.equal(fmt('walking_running_distance', 5432), '5.43 km')
assert.equal(fmt('walking_running_distance', 5432, IMPERIAL), '3.38 mi')
assert.equal(fmt('active_energy', 100, IMPERIAL), '418 kJ')
assert.equal(fmt('height', 1.78), '178 cm')
assert.equal(fmt('height', 1.78, IMPERIAL), '70.1 in')
assert.equal(fmt('blood_pressure_systolic', 128), '128 mmHg')
assert.equal(fmt('sleep_stage', 4), 'Deep', 'a stage code reads as its name')
assert.equal(fmt('heart_rate', Number.NaN), '', 'no number is no text')
assert.equal(digitsFor('blood_glucose', IMPERIAL), 1)
assert.equal(digitsFor('blood_glucose', DEFAULT_UNITS), 0)
assert.equal(formatNumber(1234.5678, 2), '1,234.57')

speak('de-DE')
assert.equal(formatNumber(1234.5, 1), '1.234,5', 'grouped the reader’s way')
speak('en-GB')

// ── durations ────────────────────────────────────────────────────────

assert.equal(durationText(0), '0 min')
assert.equal(durationText(45), '45 min')
assert.equal(durationText(60), '1 h', 'never "60 min"')
assert.equal(durationText(59.6), '1 h', 'minutes round to the nearest')
assert.equal(durationText(432), '7 h 12 min')
assert.equal(durationText(125), '2 h 05 min')
assert.equal(durationText(-5), '0 min')
assert.equal(durationText(Number.NaN), '0 min')

// ── names ────────────────────────────────────────────────────────────

assert.equal(activityName('running'), 'Running')
assert.equal(activityName('stair_climbing'), 'Stair climbing')
assert.equal(activityName('hiit'), 'HIIT')
assert.equal(activityName('martial_arts'), 'Martial arts')
assert.equal(activityName('other'), 'Workout')
assert.equal(stageName('rem'), 'REM')
assert.equal(stageName('inBed'), 'In bed')
assert.equal(slotName('breakfast'), 'Breakfast')
assert.equal(slotName('snack'), 'Snack')
assert.equal(docKindName('labReport'), 'Lab report')
assert.equal(docKindName('fhir'), 'Health record')
assert.equal(docKindName('other'), 'Document')
assert.equal(glucoseBandName('veryLow'), 'Very low')
assert.equal(glucoseBandName('inRange'), 'In range')
assert.equal(HEALTH_ACCENT, 'var(--app-health)')

// ── colours ──────────────────────────────────────────────────────────

for (const colours of [SLEEP_STAGE_COLORS, GLUCOSE_COLORS]) {
  const values = Object.values(colours)
  assert.equal(new Set(values).size, values.length, 'no two share a colour')
  for (const value of values) {
    assert.match(
      value,
      /^(var\(--[a-z0-9-]+\)|color-mix\(.+\))$/,
      `a theme token, not a hex: ${value}`,
    )
  }
}
assert.deepEqual(Object.keys(SLEEP_STAGE_COLORS).sort(), [
  'asleep',
  'awake',
  'core',
  'deep',
  'inBed',
  'rem',
])
assert.deepEqual([...GLUCOSE_BANDS], ['veryLow', 'low', 'inRange', 'high', 'veryHigh'])
assert.deepEqual(Object.keys(GLUCOSE_COLORS).sort(), [...GLUCOSE_BANDS].sort())
// In range is not green, so it is never the colour beside red that a
// red-green colour-vision difference would merge.
assert.ok(!GLUCOSE_COLORS.inRange.includes('success'))

// ── glucose bands ────────────────────────────────────────────────────

assert.equal(glucoseBand(40), 'veryLow')
assert.equal(glucoseBand(53.9), 'veryLow')
assert.equal(glucoseBand(54), 'low', '54 is low, not very low')
assert.equal(glucoseBand(69.9), 'low')
assert.equal(glucoseBand(70), 'inRange', '70 is in range')
assert.equal(glucoseBand(120), 'inRange')
assert.equal(glucoseBand(180), 'inRange', '180 is in range')
assert.equal(glucoseBand(180.1), 'high')
assert.equal(glucoseBand(250), 'high', '250 is high, not very high')
assert.equal(glucoseBand(250.1), 'veryHigh')
assert.equal(glucoseBand(400), 'veryHigh')

// ── schedules and rules ──────────────────────────────────────────────

assert.match(
  scheduleText({ type: 'daily', times: ['07:00:00', '19:00:00'] }),
  /^Every day at 0?7:00 and 19:00$/,
)
assert.match(
  scheduleText({ type: 'daily', times: ['19:00:00', '07:00:00'] }),
  /^Every day at 0?7:00 and 19:00$/,
  'times are in order whatever order they were saved in',
)
assert.match(
  scheduleText({ type: 'daily', times: ['07:00:00', '13:00:00', '19:00:00'] }),
  /^Every day at 0?7:00, 13:00,? and 19:00$/,
)
assert.equal(scheduleText({ type: 'daily', times: [] }), 'Every day')
assert.match(
  scheduleText({ type: 'weekly', days: [5, 1, 3], times: ['08:00:00'] }),
  /^Mon, Wed, Fri at 0?8:00$/,
)
assert.match(
  scheduleText({ type: 'weekly', days: [1, 2, 3, 4, 5], times: ['08:00:00'] }),
  /^Weekdays at 0?8:00$/,
)
assert.match(
  scheduleText({ type: 'weekly', days: [7, 6], times: ['08:00:00'] }),
  /^Weekends at 0?8:00$/,
)
assert.match(
  scheduleText({ type: 'weekly', days: [1, 2, 3, 4, 5, 6, 7], times: ['08:00:00'] }),
  /^Every day at /,
)
assert.match(
  scheduleText({ type: 'everyNDays', n: 2, from: '2026-11-01', times: ['09:00:00'] }),
  /^Every other day at 0?9:00$/,
)
assert.match(
  scheduleText({ type: 'everyNDays', n: 3, from: '2026-11-01', times: ['09:00:00'] }),
  /^Every 3 days at 0?9:00$/,
)
assert.equal(scheduleText({ type: 'asNeeded' }), 'As needed')
speak('en-US')
assert.equal(
  plain(scheduleText({ type: 'daily', times: ['07:00:00', '19:00:00'] })),
  'Every day at 7:00 AM and 7:00 PM',
  'in the reader’s own clock',
)
speak('en-GB')

assert.equal(
  ruleText({ type: 'emptyStomach', minutes: 30 }),
  'Empty stomach: nothing to eat for 30 min after',
)
assert.equal(ruleText({ type: 'withFood', minutes: 30 }), 'With food: a meal within 30 min')
assert.equal(
  ruleText({ type: 'apartFrom', tags: ['calcium', 'iron'], hours: 4 }),
  '4 h apart from calcium, iron',
)
assert.equal(ruleText({ type: 'apartFrom', tags: ['iron'], hours: 1.5 }), '1.5 h apart from iron')

// ── labs ─────────────────────────────────────────────────────────────

const lab = (over) => ({
  id: 'l',
  code: '',
  name: 'TSH',
  value: 2,
  text: '',
  unit: 'mIU/L',
  refLow: 0.4,
  refHigh: 4,
  refText: '',
  flag: 'none',
  collected: '2026-11-01',
  lab: '',
  panel: '',
  origin: 'manual',
  note: '',
  createdAt: '',
  updatedAt: '',
  ...over,
})
assert.equal(labKey(lab({ code: '3016-3' })), '3016-3')
assert.equal(labKey(lab({ name: 'Free T4' })), 'name:free t4', 'with no code it groups by name')
assert.equal(labOutOfRange(lab({})), false, 'inside its range')
assert.equal(labOutOfRange(lab({ value: 6.8 })), true, 'above it')
assert.equal(labOutOfRange(lab({ value: 0.2 })), true, 'below it')
assert.equal(labOutOfRange(lab({ value: 4 })), false, 'on its edge is inside')
assert.equal(labOutOfRange(lab({ value: 6.8, refHigh: null })), false, 'no upper limit to be over')
assert.equal(labOutOfRange(lab({ value: 9, flag: 'normal' })), false, 'the lab’s own word wins')
assert.equal(labOutOfRange(lab({ value: 2, flag: 'high' })), true, 'whichever way')
assert.equal(labOutOfRange(lab({ value: 2, flag: 'critical' })), true)
assert.equal(labOutOfRange(lab({ value: null, text: 'Negative' })), false, 'text is not a number')

// ── nutrition ────────────────────────────────────────────────────────

const OATS = {
  kcal: 71,
  proteinG: 2.5,
  carbsG: 12,
  fatG: 1.5,
  fiberG: 1.7,
  extra: { calcium_mg: 9 },
}
{
  const cup = scaleNutrients(OATS, 234)
  near(cup.kcal, 166.14, 'kcal for a cup')
  near(cup.proteinG, 5.85, 'protein')
  near(cup.carbsG, 28.08, 'carbohydrate')
  near(cup.fatG, 3.51, 'fat')
  near(cup.fiberG, 3.98, 'a nutrient the food knows is scaled')
  assert.ok(!('sugarG' in cup), 'one it does not know is left absent, not made zero')
  near(cup.extra.calcium_mg, 21.06, 'and the rest by name')
  assert.equal(scaleNutrients(OATS, 100).kcal, 71)
  assert.equal(scaleNutrients(OATS, 0).kcal, 0)
  assert.equal(scaleNutrients(OATS, -5).kcal, 0, 'a negative amount is nothing')
  assert.equal(scaleNutrients(OATS, Number.NaN).kcal, 0, 'so is no number')
  assert.equal(scaleNutrients({ ...OATS, kcal: 0.1 }, 300).kcal, 0.3, 'no floating-point tail')
}
{
  const a = scaleNutrients(OATS, 100)
  const b = {
    kcal: 100,
    proteinG: 4,
    carbsG: 20,
    fatG: 2,
    sugarG: 3,
    extra: { calcium_mg: 1, iron_mg: 2 },
  }
  const sum = sumNutrients([a, b])
  assert.equal(sum.kcal, 171)
  assert.equal(sum.proteinG, 6.5)
  assert.equal(sum.carbsG, 32)
  assert.equal(sum.fiberG, 1.7, 'fibre where only one item has it')
  assert.equal(sum.sugarG, 3, 'sugar likewise')
  assert.ok(!('sodiumMg' in sum), 'and none where nobody has it')
  assert.deepEqual(sum.extra, { calcium_mg: 10, iron_mg: 2 })
  assert.equal(
    sumNutrients([
      { ...NO_NUTRIENTS, kcal: 0.1 },
      { ...NO_NUTRIENTS, kcal: 0.2 },
    ]).kcal,
    0.3,
  )
  const none = sumNutrients([])
  assert.deepEqual(none, { kcal: 0, proteinG: 0, carbsG: 0, fatG: 0, extra: {} })
  none.extra.x = 1
  assert.deepEqual(NO_NUTRIENTS.extra, {}, 'an empty sum is its own object, not the shared one')
}

const OATMEAL = {
  id: 'f-oats',
  name: 'Oatmeal, cooked',
  brand: '',
  barcode: '',
  source: { type: 'own' },
  per100g: OATS,
  servings: [
    { name: '1 cup', grams: 234 },
    { name: '1 bowl', grams: 300 },
  ],
  favorite: false,
  useCount: 0,
  createdAt: '',
  updatedAt: '',
}
assert.equal(gramsOf(OATMEAL, 150, 'g'), 150)
assert.equal(gramsOf(OATMEAL, 150, 'ml'), 150, 'a millilitre is a gram')
assert.equal(gramsOf(OATMEAL, 2, '1 cup'), 468)
assert.equal(
  gramsOf(OATMEAL, 0.5, ' 1 CUP '),
  117,
  'a serving is found whatever its case and spacing',
)
assert.equal(gramsOf(OATMEAL, 1, 'cup'), null, 'a unit the food has no serving called')
assert.equal(gramsOf(OATMEAL, -1, 'g'), null)
assert.equal(gramsOf(OATMEAL, Number.NaN, 'g'), null)
{
  const item = itemFor(OATMEAL, 2, '1 cup')
  assert.equal(item.foodId, 'f-oats')
  assert.equal(item.name, 'Oatmeal, cooked')
  assert.equal(item.amount, 2)
  assert.equal(item.unit, '1 cup')
  assert.equal(item.grams, 468)
  near(item.nutrients.kcal, 332.28, 'worked out at the amount')
  assert.equal(item.estimated, false)
  assert.equal(
    itemFor({ ...OATMEAL, source: { type: 'estimate' } }, 100, 'g').estimated,
    true,
    'a guess says so',
  )
  assert.equal(itemFor(OATMEAL, 1, 'plate'), null, 'no such serving')
  assert.equal(itemFor(OATMEAL, 1, ' 1 CUP ').unit, '1 cup', 'a serving is kept under its own name')
  assert.equal(itemFor(OATMEAL, 80, ' G ').unit, 'g', 'grams in lower case')
  assert.equal(
    itemFor({ ...OATMEAL, brand: ' Quaker ' }, 1, 'g').name,
    'Oatmeal, cooked (Quaker)',
    'the brand follows the name',
  )
}
assert.deepEqual(
  NO_NUTRIENTS,
  { kcal: 0, proteinG: 0, carbsG: 0, fatG: 0, extra: {} },
  'the shared zero was never written to',
)

// ── making things ────────────────────────────────────────────────────

assert.equal(slotAt(new Date(2026, 10, 5, 8, 0)), 'breakfast')
assert.equal(slotAt(new Date(2026, 10, 5, 10, 29)), 'breakfast')
assert.equal(slotAt(new Date(2026, 10, 5, 10, 30)), 'lunch')
assert.equal(slotAt(new Date(2026, 10, 5, 12, 30)), 'lunch')
assert.equal(slotAt(new Date(2026, 10, 5, 15, 30)), 'snack', 'between lunch and dinner')
assert.equal(slotAt(new Date(2026, 10, 5, 19, 0)), 'dinner')
assert.equal(slotAt(new Date(2026, 10, 5, 23, 0)), 'snack')
assert.equal(slotAt(new Date(2026, 10, 5, 3, 0)), 'snack')
{
  const at = new Date(2026, 10, 5, 12, 40)
  const meal = newMeal(at)
  assert.equal(meal.slot, 'lunch')
  assert.equal(meal.at, at.toISOString())
  assert.equal(meal.offsetMin, -at.getTimezoneOffset())
  assert.deepEqual(meal.items, [])
  assert.equal(meal.photo, null)
  assert.notEqual(newMeal(at).id, meal.id, 'each meal has its own id')
}
{
  const med = newMedication(3)
  assert.equal(med.sortOrder, 3)
  assert.deepEqual(med.schedule, { type: 'daily', times: ['08:00:00'] })
  assert.equal(med.status, 'active')
  assert.equal(med.kind, 'prescription')
  assert.equal(med.remind, true)
  assert.match(med.started, /^\d{4}-\d{2}-\d{2}$/)
  assert.deepEqual([med.rules, med.tags, med.history], [[], [], []])
}
assert.notEqual(newId(), newId())

// ── lists ────────────────────────────────────────────────────────────

assert.equal(METRICS.length, 43, 'the whole catalogue')
assert.equal(new Set(METRICS).size, METRICS.length, 'each once')
for (const metric of VITALS_METRICS) assert.ok(METRICS.includes(metric), `${metric} is a metric`)
assert.equal(new Set(VITALS_METRICS).size, VITALS_METRICS.length)
assert.ok(VITALS_METRICS.includes('resting_heart_rate'), 'what Vitals opens on is listed')

const condition = (over) => ({
  id: 'c',
  name: 'x',
  template: null,
  status: 'active',
  codes: [],
  plan: { watch: [], targets: [], labs: [], medRules: {}, trackerIds: [], templateVersion: 1 },
  notes: '',
  createdAt: '',
  updatedAt: '',
  ...over,
})
assert.equal(isGlucoseCondition(condition({ template: 'type2Diabetes' })), true)
assert.equal(isGlucoseCondition(condition({ template: 'type1Diabetes' })), true)
assert.equal(isGlucoseCondition(condition({ template: 'hypothyroidism' })), false)
assert.equal(
  isGlucoseCondition(
    condition({
      plan: {
        watch: [{ type: 'metric', metric: 'blood_glucose', stat: { type: 'mean' } }],
        targets: [],
        labs: [],
        medRules: {},
        trackerIds: [],
        templateVersion: 1,
      },
    }),
  ),
  true,
  'a plan that watches glucose gets the glucose dashboard, template or not',
)
assert.equal(
  isGlucoseCondition(
    condition({
      plan: {
        watch: [{ type: 'lab', code: '4548-4' }],
        targets: [],
        labs: [],
        medRules: {},
        trackerIds: [],
        templateVersion: 1,
      },
    }),
  ),
  false,
  'an A1c alone is not a CGM',
)

await close()
console.log('health: all checks passed')
