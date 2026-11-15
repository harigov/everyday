// Behaviour checks for `health.svelte.ts` itself -- the class, not the pure
// helpers `health.test.mjs` covers.
//
// Driven through the real mock backend (`mock-health.ts`), the way
// `screentime-store.test.mjs` is: a fake `api` would be a second copy of what
// the mock already does, and what is worth checking here is the round trip --
// a pane that loads what is on screen and only the newest answer, the dose
// that shows at once, the meal that is saved and read back, the destructive
// act that asks first, the phone's token revoked before its source goes, and
// the import that is asked after until it is done.
//
// Written against the mock's seed: sixty days of one person with type 2
// diabetes and an underactive thyroid -- a CGM, a watch, three medications,
// two documents, an allergy, a Health Auto Export source and an Apple export.
// What it asserts about a window that has to be full of data is asserted
// about last month, which is inside the seed whenever the test is run.

import assert from 'node:assert/strict'
import { load, stubBrowser } from './harness.mjs'

const stored = new Map()

stubBrowser({
  window: globalThis,
  localStorage: {
    getItem: (key) => stored.get(key) ?? null,
    setItem: (key, value) => stored.set(key, String(value)),
    removeItem: (key) => stored.delete(key),
  },
  matchMedia: true,
  document: {
    querySelector: () => null,
    documentElement: {
      style: { setProperty: () => {} },
      classList: { toggle: () => {} },
      setAttribute: () => {},
      removeAttribute: () => {},
    },
    addEventListener: () => {},
  },
  location: new URL('http://localhost/?unlocked=1'),
  createObjectURL: () => 'blob:stub',
})
Object.defineProperty(globalThis, 'navigator', {
  value: { userAgent: 'node', language: 'en-GB' },
  configurable: true,
})

const {
  modules: [storeModule, stateModule, apiModule, helpers, timeModule, trackingModule],
  close,
} = await load(
  [
    '/src/lib/health.svelte.ts',
    '/src/lib/state.svelte.ts',
    '/src/lib/api.ts',
    '/src/lib/health.ts',
    '/src/lib/time.ts',
    '/src/lib/tracking.svelte.ts',
  ],
  { svelte: true },
)
const { health, HEALTH_VIEWS, HEALTH_RANGES } = storeModule
const { app } = stateModule
const { api, callCommand } = apiModule
const { METRICS, VITALS_METRICS, itemFor, rangeOf } = helpers
const { addDays, localeWeekStart, todayIso } = timeModule
const { tracking } = trackingModule

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

/** Wait for something that happens in the background, up to four seconds. */
async function until(what, condition) {
  for (let i = 0; i < 200; i++) {
    if (condition()) return
    await sleep(20)
  }
  assert.fail(`never happened: ${what}`)
}

const today = todayIso()
const yesterday = addDays(today, -1)
const week = rangeOf('week', today, localeWeekStart())
const lastMonth = rangeOf('month', addDays(today, -30), localeWeekStart())

await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')
assert.equal(app.supportsHealth, true, 'and carry Health')
assert.equal(HEALTH_VIEWS.length, 8)
assert.deepEqual([...HEALTH_RANGES], ['week', 'month', 'quarter', 'year'])

// ── start loads Today ────────────────────────────────────────────────

app.section = 'health'
await health.start()
assert.equal(health.loaded, true)
assert.equal(health.view, 'today', 'Today is the pane it opens on')
assert.equal(health.date, today)
assert.equal(health.anchor, today)
assert.equal(health.range, 'week')
assert.equal(health.vitalsMetric, 'resting_heart_rate')
assert.equal(health.rangeLabel, 'Today')
assert.equal(health.atPresent, true)
assert.equal(health.accent, 'var(--app-health)')
assert.equal(health.error, null)
assert.equal(health.catalog.length, 43)
assert.deepEqual(
  health.catalog.map((m) => m.metric).sort(),
  [...METRICS].sort(),
  'the vault’s catalogue is the one the interface lists',
)
assert.equal(health.settings?.units.glucose, 'mg/dL')
assert.equal(health.sources.length, 2)
assert.equal(health.sources.filter((s) => s.connector === 'healthAutoExport').length, 1)
assert.equal(health.keys?.fdcKeySet, false)
assert.equal(health.importStatus?.state, 'idle')
assert.equal(health.today?.date, today)
assert.equal(health.dosesDue.length, 4, 'metformin twice, levothyroxine and calcium once')
assert.equal(health.today?.dosesDue.length, 4)
assert.ok((health.today?.glucose?.readings ?? 0) > 0, 'the CGM has been reading today')
assert.deepEqual(health.showing.open, [], 'nothing is open')
assert.match(health.showing.view, /health summary for today/)
assert.equal(health.display('heart_rate', 72), '72 bpm')
assert.equal(health.display('steps', 8432), '8,432 steps')
assert.equal(health.display('flights_climbed', 12), '12 floors')
assert.equal(health.displayUnit('weight'), 'kg')
assert.equal(health.toDisplay('weight', 81.4), 81.4)
assert.equal(health.metricInfo('weight')?.unit, 'kg')
assert.equal(health.metricInfo('nonsense'), undefined)

// Today steps by the day, and never past today.
health.step(-1)
assert.equal(health.date, yesterday)
assert.equal(health.rangeLabel, 'Yesterday')
assert.equal(health.atPresent, false)
await until('yesterday loads', () => health.today?.date === yesterday)
health.step(1)
assert.equal(health.date, today)
health.step(1)
assert.equal(health.date, today, 'nothing to step forward to from today')
health.setDate(addDays(today, 3))
assert.equal(health.date, today, 'a day to come is cut back to today')
await until('today again', () => health.today?.date === today)

// A dose logged shows at once, on the due time it answers.
{
  const pending = health.dosesDue.find((d) => !d.dose)
  if (pending) {
    const done = health.logDose(pending.medicationId, { scheduledAt: pending.dueAt })
    const shown = health.dosesDue.find(
      (d) => d.medicationId === pending.medicationId && d.dueAt === pending.dueAt,
    )
    assert.ok(shown?.dose, 'the due time is marked before the vault has answered')
    assert.equal(shown.dose.status, 'taken')
    assert.equal(shown.dose.origin, 'manual')
    const saved = await done
    assert.ok(saved, 'and the vault took it')
    assert.equal(saved.scheduledAt, pending.dueAt)
    assert.ok(
      health.today.dosesDue.find(
        (d) => d.dueAt === pending.dueAt && d.medicationId === pending.medicationId,
      )?.dose,
      'still there once Today is read again',
    )
    await health.deleteDose(saved)
    assert.ok(
      !health.dosesDue.find(
        (d) => d.dueAt === pending.dueAt && d.medicationId === pending.medicationId,
      )?.dose,
      'and gone when it is taken back',
    )
  }
}

// A reading typed in: the dialog opens, the vault converts, the dialog closes.
health.create()
assert.equal(health.readingOpen, true, 'on Today the app bar’s new is a reading')
assert.equal(
  await health.addReading({
    metric: 'weight',
    at: new Date().toISOString(),
    value: 180,
    unit: 'lb',
    second: null,
    tz: 'UTC',
  }),
  true,
)
assert.equal(health.readingOpen, false)
{
  const [weight] = await api.healthLatest(['weight'], today)
  assert.ok(Math.abs(weight.value - 81.6466) < 0.01, `180 lb is 81.65 kg: ${weight.value}`)
  assert.equal(weight.source, 'Karma Yoga')
}
assert.equal(
  await health.addReading({
    metric: 'weight',
    at: new Date().toISOString(),
    value: 1800,
    unit: 'lb',
    second: null,
    tz: 'UTC',
  }),
  false,
  'a weight that is a unit mix-up is refused',
)
assert.equal(health.readingOpen, false)
assert.equal(
  await health.addReading({
    metric: 'blood_pressure_systolic',
    at: new Date().toISOString(),
    value: 121,
    unit: '',
    second: 79,
    tz: 'UTC',
  }),
  true,
)
{
  const [dia] = await api.healthLatest(['blood_pressure_diastolic'], today)
  assert.equal(dia.value, 79, 'the diastolic half is filed beside the systolic')
}

// ── activity ─────────────────────────────────────────────────────────

health.setView('activity')
assert.equal(stored.get('karmayoga.health.view'), 'activity', 'the pane is remembered here')
await until(
  'the week loads',
  () => health.activity.length === 7 && health.activity[0].date === week.from,
)
assert.equal(health.from, week.from)
assert.equal(health.to, week.to)
assert.equal(health.activity.at(-1).date, week.to)
assert.equal(health.rangeLabel, 'Last 7 days')
assert.ok(Array.isArray(health.workouts))
health.step(1)
assert.equal(health.from, week.from, 'nothing to step forward to from this week')
health.step(-1)
assert.equal(health.atPresent, false)
assert.equal(health.from, addDays(week.from, -7))
// Two quick steps back: only the newest answer may land.
health.step(-1)
await until('two weeks back', () => health.activity[0]?.date === addDays(week.from, -14))
await sleep(80)
assert.equal(health.activity[0].date, addDays(week.from, -14), 'and an older answer does not')
assert.ok(
  health.activity.some((d) => d.steps > 0),
  'a fortnight ago is inside the seed',
)

health.setRange('month')
assert.equal(stored.get('karmayoga.health.range'), 'month')
health.goToday()
health.step(-1)
assert.equal(health.from, lastMonth.from, 'one month back from this one')
assert.equal(health.to, lastMonth.to)
await until(
  'last month loads',
  () => health.activity[0]?.date === lastMonth.from && health.workouts.length >= 3,
)
assert.equal(health.activity.length, 30, 'a month is thirty days')
assert.equal(
  health.activity.reduce((n, d) => n + d.workouts, 0),
  health.workouts.length,
  'the days count the workouts the list holds',
)
{
  const run = health.workouts.find((w) => w.activity === 'running')
  assert.ok(run, 'two runs a week')
  assert.equal(run.name, 'Outdoor Run')
  assert.ok((run.distanceM ?? 0) > 3000)
  health.openWorkout(run.id)
  assert.equal(health.view, 'activity')
  assert.equal(health.workoutId, run.id)
  assert.deepEqual(
    health.showing.open.map((o) => [o.kind, o.id]),
    [['workout', run.id]],
    'an open workout is told as a reference',
  )
  await until('the workout opens', () => health.workout?.workout.id === run.id)
  assert.ok(health.workout.heartRate.length > 20, 'a heart rate a minute through the run')
  assert.ok(health.workout.route.length > 50, 'and where it went')
  health.openWorkout(null)
  await until('the workout closes', () => health.workout === null)
}

// ── sleep ────────────────────────────────────────────────────────────

health.setView('sleep')
await until('sleep loads', () => health.sleep.length >= 28)
for (const night of health.sleep) {
  assert.ok(night.segments.length > 3, 'a night is stages, not a total')
  assert.ok(night.asleepMin > 240 && night.asleepMin < 600, `a plausible night: ${night.asleepMin}`)
  assert.ok(Math.abs(night.coreMin + night.deepMin + night.remMin - night.asleepMin) <= 2)
  assert.ok(night.score >= 0 && night.score <= 100)
  assert.ok(night.efficiency > 0.7 && night.efficiency <= 1)
}

// ── vitals ───────────────────────────────────────────────────────────

health.setView('vitals')
await until(
  'vitals load',
  () => health.series?.metric === 'resting_heart_rate' && health.latest.length > 0,
)
assert.ok(health.series.points.length >= 27, 'a point a day over a month')
assert.equal(health.series.unit, 'count/min')
assert.ok(health.latest.some((l) => l.metric === 'resting_heart_rate'))
assert.ok(health.latest.every((l) => VITALS_METRICS.includes(l.metric)))
assert.equal(health.seriesSecond, null)
health.setVitalsMetric('blood_pressure_systolic')
assert.equal(stored.get('karmayoga.health.vitals'), 'blood_pressure_systolic')
await until('blood pressure loads', () => health.series?.metric === 'blood_pressure_systolic')
await until('and its other half', () => health.seriesSecond?.metric === 'blood_pressure_diastolic')
{
  // The tiles are the latest as of the window's end: last month's, not today's.
  const then = health.latest.find((l) => l.metric === 'blood_pressure_systolic')
  assert.ok(then.value >= 125 && then.value <= 140, `a seeded reading: ${then.value}`)
  assert.ok(
    Date.parse(then.at) < Date.parse(`${lastMonth.to}T23:59:59`),
    'as of the end of that month',
  )
  health.goToday()
  await until('this month’s blood pressure', () => {
    return health.latest.find((l) => l.metric === 'blood_pressure_systolic')?.value === 121
  })
  const bp = health.latest.find((l) => l.metric === 'blood_pressure_systolic')
  assert.equal(
    health.display('blood_pressure_systolic', bp.value),
    '121 mmHg',
    'the one typed in a moment ago',
  )
}
health.setVitalsMetric('resting_heart_rate')
await until('back to heart rate', () => health.series?.metric === 'resting_heart_rate')
assert.equal(health.seriesSecond, null)
health.step(-1)
assert.equal(health.from, lastMonth.from, 'and back to last month')

// ── nutrition ────────────────────────────────────────────────────────

health.setView('nutrition')
await until('nutrition loads', () => health.meals.length > 60 && health.nutrition.length > 0)
assert.equal(health.nutrition.length, health.activity.length)
assert.ok(health.nutrition.filter((d) => d.meals >= 3).length >= 27, 'three meals a day')
assert.equal(health.foods.length, 12)
assert.ok(health.meals.every((m) => m.items.length > 0))
{
  const dinner = health.meals.find((m) => m.slot === 'dinner')
  const response = await health.mealResponse(dinner.id)
  assert.ok(response, 'what glucose did after a meal')
  assert.ok(response.points.length > 20)
  assert.ok(response.carbsG > 0)
  assert.equal(typeof response.peak, 'number')
}

// A meal saved shows.
health.goToday()
await until('this month’s meals load', () => health.nutrition.some((d) => d.date === today))
const kcalBefore = health.nutrition.find((d) => d.date === today).logged.kcal
health.create()
assert.ok(health.editingMeal, 'on Nutrition the app bar’s new is a meal')
assert.deepEqual(health.editingMeal.items, [])
{
  const oats = health.foods.find((f) => f.name.startsWith('Oatmeal'))
  const item = itemFor(oats, 1, '1 cup')
  const draft = { ...health.editingMeal, name: 'Test breakfast', items: [item] }
  const saved = await health.saveMeal(draft)
  assert.ok(saved, 'the vault took it')
  assert.equal(health.editingMeal, null, 'the editor closes on a save of what it was editing')
  assert.ok(
    health.meals.some((m) => m.id === saved.id),
    'and it is in the list',
  )
  const after = health.nutrition.find((d) => d.date === today).logged.kcal
  assert.ok(Math.abs(after - kcalBefore - item.nutrients.kcal) < 0.5, 'and in the day’s total')
  assert.ok(
    health.foods.find((f) => f.id === oats.id).useCount > oats.useCount,
    'the food was used',
  )

  // A delete asks first.
  health.deleteMeal(saved)
  assert.ok(health.question, 'it asks')
  assert.match(health.question.title, /Test breakfast/)
  assert.equal(health.question.confirm, 'Delete')
  assert.equal(health.question.alternative, null)
  await sleep(80)
  assert.ok(
    health.meals.some((m) => m.id === saved.id),
    'and nothing has been deleted yet',
  )
  await health.question.run()
  assert.equal(health.question, null, 'the question is gone once answered')
  assert.ok(!health.meals.some((m) => m.id === saved.id), 'and the meal with it')
}

// Foods and the lookups.
{
  const none = await health.searchFoods('   ')
  assert.deepEqual(none, [], 'nothing typed is nothing found')
  const hits = await health.searchFoods('quinoa')
  assert.ok(
    hits.some((h) => h.origin === 'usda'),
    'a lookup service has it',
  )
  const saved = await health.saveFood(hits.find((h) => h.origin === 'usda').food)
  assert.ok(saved)
  assert.ok(
    health.foods.some((f) => f.id === saved.id),
    'a food saved is in the list',
  )
  assert.equal((await health.lookupBarcode('5449000000996'))?.food.name, 'Cola')
  assert.equal(await health.lookupBarcode('0000'), null)
  health.deleteFood(saved)
  assert.ok(health.question)
  await health.question.run()
  assert.ok(!health.foods.some((f) => f.id === saved.id))

  const photo = await health.estimatePhoto(new File(['x'], 'plate.jpg', { type: 'image/jpeg' }))
  assert.equal(photo.items.length, 3)
  assert.ok(
    photo.items.every((i) => i.estimated === true),
    'every item is a guess',
  )
  assert.ok(photo.photo)
}

// ── medications ──────────────────────────────────────────────────────

health.setView('medications')
await until('medications load', () => health.medications.length === 3 && health.doses.length > 0)
assert.deepEqual(
  health.medications.map((m) => m.sortOrder),
  [0, 1, 2],
  'in the order they were put in',
)
assert.ok(health.doses.length > 60, 'a month of doses')
assert.equal(health.allergies.length, 1)
assert.equal(health.dosesDue.length, 4)
{
  const alert = health.alerts.find((a) => a.rule.type === 'apartFrom')
  assert.ok(alert, 'the calcium taken one morning too close to the levothyroxine')
  assert.equal(alert.medicationId, 'med-levothyroxine')
  assert.equal(alert.otherMedicationId, 'med-calcium')
  assert.match(alert.message, /keep 4 h apart/)
  const adherence = await api.medicationAdherence(null, addDays(today, -29), today, 'UTC')
  assert.ok(adherence.due > 80)
  assert.ok(adherence.taken / adherence.due > 0.8, 'good')
  assert.ok(adherence.taken < adherence.due, 'but not perfect')
}

// A dose logged shows, and comes off the supply.
{
  const calcium = health.medications.find((m) => m.id === 'med-calcium')
  const supply = calcium.supply.remaining
  const saved = await health.logDose('med-calcium', { note: 'with lunch' })
  assert.ok(saved)
  assert.equal(saved.status, 'taken')
  assert.equal(saved.note, 'with lunch')
  assert.ok(
    health.doses.some((d) => d.id === saved.id),
    'it is in the doses',
  )
  assert.equal(health.medications.find((m) => m.id === 'med-calcium').supply.remaining, supply - 1)
  await health.deleteDose(saved)
  assert.ok(!health.doses.some((d) => d.id === saved.id), 'and out of them')
}

// A medication made, saved, changed, and deleted -- asking first.
{
  health.create()
  assert.ok(health.editingMedication, 'on Medications the app bar’s new is a medication')
  assert.equal(health.editingMedication.sortOrder, 3, 'last in the order')
  const draft = {
    ...health.editingMedication,
    name: '  Test med ',
    strength: '10 mg',
    tags: [' Iron ', 'iron', 'IRON'],
  }
  const saved = await health.saveMedication(draft)
  assert.ok(saved)
  assert.equal(saved.name, 'Test med', 'a name is trimmed')
  assert.deepEqual(saved.tags, ['iron'], 'tags are spelled one way')
  assert.equal(health.editingMedication, null)
  assert.equal(health.medications.length, 4)
  assert.equal(saved.history.length, 1)
  assert.equal(saved.history[0].note, 'Started')
  const changed = await health.saveMedication({ ...saved, dose: 2 })
  assert.equal(changed.history.length, 2, 'a new dose is a change worth remembering')
  assert.equal(changed.history[1].dose, 2)

  // No: the question goes away and nothing is deleted.
  health.deleteMedication(changed)
  assert.match(health.question.title, /Test med/)
  health.question = null
  await sleep(80)
  assert.equal((await api.listMedications()).length, 4, 'a no deletes nothing')
  // Yes.
  health.deleteMedication(changed)
  await health.question.run()
  assert.equal(health.medications.length, 3)
  assert.equal(health.question, null)
}

// ── conditions ───────────────────────────────────────────────────────

health.setView('conditions')
await until(
  'conditions load',
  () => health.conditions.length === 2 && health.templates.length === 3,
)
assert.equal(health.conditionId, null)
assert.equal(health.conditionReport, null, 'none open, no report')
assert.equal(health.condition, null)
assert.deepEqual(health.showing.open, [])

health.openCondition('cond-t2d')
assert.equal(health.view, 'conditions')
await until(
  'the diabetes dashboard loads',
  () => health.conditionReport?.condition.id === 'cond-t2d' && health.glucose,
)
assert.equal(health.condition?.name, 'Type 2 diabetes')
assert.ok(health.glucose.readings > 1000, 'two weeks of readings')
assert.equal(health.glucose.agp.length, 288)
assert.ok(health.glucose.inRange > 0.4 && health.glucose.inRange < 1)
assert.ok(
  Math.abs(
    health.glucose.veryLow +
      health.glucose.low +
      health.glucose.inRange +
      health.glucose.high +
      health.glucose.veryHigh -
      1,
  ) < 0.01,
)
assert.equal(health.timeline?.date, health.date, 'and the day overlay')
assert.ok(health.timeline.glucose.length > 0)
assert.ok(health.conditionReport.targets.some((t) => /Time in range/.test(t.target.label)))
assert.ok(
  health.conditionReport.targets.every((t) => ['met', 'notMet', 'unknown'].includes(t.state)),
)
assert.deepEqual(
  health.showing.open.map((o) => [o.kind, o.id]),
  [['condition', 'cond-t2d']],
)

health.openCondition('cond-hypo')
await until(
  'the thyroid dashboard loads',
  () => health.conditionReport?.condition.id === 'cond-hypo',
)
assert.equal(health.glucose, null, 'no glucose dashboard for a thyroid')
assert.equal(health.timeline, null)
{
  const tsh = health.conditionReport.labsDue.find((d) => d.schedule.name === 'TSH')
  assert.ok(tsh, 'the TSH')
  assert.equal(tsh.overdue, false, 'not yet')
  assert.ok(tsh.due > today, 'but coming: six weeks after the dose change')
  assert.match(tsh.reason, /dose change/)
  assert.ok(
    health.conditionReport.alerts.length >= 1,
    'the calcium alert belongs to this condition',
  )
}

// A condition added from a template, opened, and deleted -- asking first.
{
  const made = await health.applyTemplate('type1Diabetes', {
    plan: null,
    symptoms: ['Fatigue'],
    diagnosed: '2020-01-01',
  })
  assert.ok(made)
  assert.equal(made.template, 'type1Diabetes')
  assert.equal(made.diagnosed, '2020-01-01')
  assert.equal(health.conditionId, made.id, 'what was just made is open')
  assert.equal(health.conditions.length, 3)
  await until('its dashboard loads', () => health.conditionReport?.condition.id === made.id)
  health.deleteCondition(made)
  assert.ok(health.question)
  assert.equal(health.question.alternative, null, 'no trackers made, none to offer to remove')
  await health.question.run()
  assert.equal(health.conditions.length, 2)
  assert.equal(health.conditionId, null, 'and it is no longer open')
}
health.openCondition(null)

// ── records ──────────────────────────────────────────────────────────

health.setView('records')
// A condition's dashboard reads documents and lab trends too, so wait for the
// lab list itself, which only this pane reads.
await until(
  'records load',
  () => health.documents.length === 2 && health.labTrends.length > 0 && health.labs.length >= 20,
)
{
  const a1c = health.labTrends.find((t) => t.key === '4548-4')
  assert.ok(a1c, 'A1c, by its LOINC code')
  assert.deepEqual(
    a1c.results.map((r) => r.value),
    [8.3, 8, 7.7, 7.4, 7.2],
    'oldest first, going down',
  )
  const october = health.documents.find((d) => d.id === 'doc-october')
  assert.equal(october.extraction, 'proposed')
  assert.equal(october.pendingLabs.length, 4, 'a document waiting for its results to be accepted')
}
health.openDocument('doc-october')
assert.equal(health.document?.title, 'Lab report, October')
assert.deepEqual(
  health.showing.open.map((o) => [o.kind, o.id]),
  [['healthDocument', 'doc-october']],
)
assert.equal(
  health.documentFileUrl(health.document, health.document.files[0].blob) !== '',
  true,
  'a file the document lists has an address',
)
assert.equal(
  health.documentFileUrl(health.document, 'not-a-blob-it-has'),
  '',
  'one it does not has none',
)

// Accepting what was read.
{
  const pending = health.document.pendingLabs
  await health.acceptLabs('doc-october', [pending[0], pending[1]])
  assert.equal(health.document.pendingLabs.length, 2, 'what was accepted is cleared from the list')
  assert.equal(health.document.extraction, 'proposed', 'with two still waiting')
  const a1c = health.labTrends.find((t) => t.key === '4548-4')
  assert.equal(a1c.results.at(-1).value, 6.9, 'and the trend has a new point')
  assert.equal(a1c.results.at(-1).documentId, 'doc-october', 'pointing at the document')
}

// A document added from files, read, and renamed.
{
  const doc = await health.addDocument(
    [new File(['Sodium 139'], 'visit.txt', { type: 'text/plain' })],
    '  ',
    'visitNote',
    '2026-11-01',
  )
  assert.ok(doc)
  assert.equal(doc.title, 'visit', 'with no title it is called what its file is')
  assert.equal(doc.kind, 'visitNote')
  assert.equal(doc.date, '2026-11-01')
  assert.equal(doc.files.length, 1)
  assert.equal(doc.files[0].mime, 'text/plain')
  assert.equal(doc.files[0].name, 'visit.txt')
  assert.equal(doc.files[0].size, 10)
  assert.equal(health.documents.length, 3)
  assert.equal(await health.addDocument([], 'No files', 'other'), null, 'no files, no document')

  await health.extractDocument(doc.id)
  const read = health.documents.find((d) => d.id === doc.id)
  assert.equal(read.extraction, 'proposed', 'the model read it')
  assert.ok(read.pendingLabs.length > 0)
  const renamed = await health.saveDocument({ ...read, title: ' Visit, November ' })
  assert.equal(renamed.title, 'Visit, November')
  assert.equal(renamed.files.length, 1, 'its files are kept')

  health.deleteDocument(renamed)
  assert.ok(health.question)
  assert.equal(
    health.question.alternative,
    null,
    'no results accepted from it, none to offer to delete',
  )
  await health.question.run()
  assert.equal(health.documents.length, 2)
}

// A document with results accepted from it offers to delete them too.
{
  const labsBefore = (await api.listLabs()).length
  const lipids = health.documents.find((d) => d.id === 'doc-lipids')
  health.deleteDocument(lipids)
  assert.ok(health.question.alternative, 'it offers to delete the results too')
  assert.match(health.question.alternative.label, /with its results/)
  await health.question.alternative.run()
  assert.equal(health.question, null)
  assert.ok(!health.documents.some((d) => d.id === 'doc-lipids'))
  const left = await api.listLabs()
  assert.equal(left.length, labsBefore - 7, 'seven results went with it')
  assert.ok(!left.some((l) => l.documentId === 'doc-lipids'))
}

// A result deleted asks first.
{
  const lab = health.labs.find((l) => l.name === 'Eye exam')
  health.deleteLab(lab)
  assert.ok(health.question)
  await health.question.run()
  assert.ok(!health.labs.some((l) => l.id === lab.id))
}

// ── samples deleted ask first ────────────────────────────────────────

{
  const from = addDays(today, -100)
  const label = 'every weight'
  health.deleteSamples(
    'weight',
    new Date(`${from}T00:00:00`).toISOString(),
    new Date(`${addDays(today, 1)}T00:00:00`).toISOString(),
    label,
  )
  assert.equal(health.question.title, 'Delete every weight?')
  health.question = null
  assert.equal((await api.healthLatest(['weight'], today)).length, 1, 'a no deletes nothing')
  health.deleteSamples(
    'weight',
    new Date(`${from}T00:00:00`).toISOString(),
    new Date(`${addDays(today, 1)}T00:00:00`).toISOString(),
    label,
  )
  await health.question.run()
  assert.equal((await api.healthLatest(['weight'], today)).length, 0, 'a yes does')
}

// ── an allergy ───────────────────────────────────────────────────────

health.setView('medications')
await until('the allergy list loads', () => health.allergies.length === 1)
{
  const saved = await health.saveAllergy({
    ...health.allergies[0],
    id: 'allergy-new',
    substance: ' Shellfish ',
    kind: 'food',
    reaction: 'Swelling',
    severity: 'severe',
  })
  assert.equal(saved.substance, 'Shellfish')
  assert.equal(health.allergies.length, 2)
  health.deleteAllergy(saved)
  assert.match(health.question.title, /Shellfish/)
  await health.question.run()
  assert.equal(health.allergies.length, 1)
}

// ── settings and units ───────────────────────────────────────────────

{
  const before = health.settings
  const done = health.saveSettings({
    ...before,
    units: { ...before.units, weight: 'lb', glucose: 'mmol/L', temperature: 'F' },
  })
  assert.equal(health.settings.units.weight, 'lb', 'a unit chosen changes every number at once')
  assert.equal(health.display('weight', 80), '176.4 lb')
  assert.equal(health.display('blood_glucose', 99), '5.5 mmol/L')
  assert.equal(health.display('body_temperature', 36.6), '97.9 °F')
  assert.equal(
    health.toDisplay('weight', 80),
    176.4,
    'an input is in the shown unit, rounded to what is shown',
  )
  assert.equal(health.displayUnit('blood_glucose'), 'mmol/L')
  await done
  assert.equal((await api.healthSettings()).units.weight, 'lb', 'and the vault has it')
  // The vault refuses what is not a unit, and the screen goes back.
  await health.saveSettings({ ...before, units: { ...before.units, weight: 'stone' } })
  assert.equal(health.settings.units.weight, 'lb', 'a refusal puts back what the vault has')
  await health.saveSettings(before)
  assert.equal(health.settings.units.weight, 'kg')
}

// ── the food database key ────────────────────────────────────────────

await health.saveKeys('   ')
assert.equal(health.keys.fdcKeySet, false, 'a blank key is not a key')
await health.saveKeys('abcd1234wxyz')
assert.equal(health.keys.fdcKeySet, true)
assert.equal(health.keys.fdcKeyHint, 'wxyz', 'only its last four characters come back')
assert.ok(!JSON.stringify(health.keys).includes('abcd1234'), 'never the key')
await health.clearKeys()
assert.equal(health.keys.fdcKeySet, false)

// ── sources, and the phone's token ───────────────────────────────────

const calls = []
{
  const revoke = api.healthPushRevoke
  const drop = api.deleteHealthSource
  api.healthPushRevoke = async (id) => {
    calls.push(`revoke:${id}`)
    return revoke(id)
  }
  api.deleteHealthSource = async (id) => {
    calls.push(`delete:${id}`)
    return drop(id)
  }

  const hae = health.sources.find((s) => s.connector === 'healthAutoExport')
  const setup = await health.pushSetup(hae.id)
  assert.ok(setup.url.endsWith(`/v1/health/hae/${hae.id}`))
  assert.match(setup.token, /^mock-token-/)
  assert.equal(setup.webOn, true)
  await health.reload()
  assert.equal(health.sources.find((s) => s.id === hae.id).tokenId, setup.token)

  // Pausing is a person's choice, saved; what arrival learned is not touched.
  await health.saveSource({ ...hae, name: ' Hari’s phone ', paused: true })
  {
    const now = health.sources.find((s) => s.id === hae.id)
    assert.equal(now.name, 'Hari’s phone')
    assert.equal(now.paused, true)
    assert.equal(now.status.samplesTotal, hae.status.samplesTotal)
    assert.equal(now.status.automations.length, 2)
  }
  await health.saveSource({ ...hae, name: 'Hari’s phone', paused: false })

  // The Apple export has no token: deleting it never calls for a revoke.
  const apple = health.sources.find((s) => s.connector === 'appleExport')
  health.deleteSource(apple)
  assert.ok(health.question)
  assert.match(health.question.title, /Apple Health export/)
  await health.question.run()
  assert.deepEqual(calls, [`delete:${apple.id}`])
  assert.equal(health.sources.length, 1)

  // A phone's source is revoked first, and only then deleted.
  calls.length = 0
  health.deleteSource(health.sources.find((s) => s.id === hae.id))
  await health.question.run()
  assert.deepEqual(
    calls,
    [`revoke:${hae.id}`, `delete:${hae.id}`],
    'the token goes before the source',
  )
  assert.equal(health.sources.length, 0)

  // If the token cannot be revoked the source stays.
  const second = await health.createSource('healthAutoExport', '  Second phone ')
  assert.equal(second.name, 'Second phone')
  assert.equal(second.paused, false)
  assert.equal(health.sources.length, 1)
  calls.length = 0
  api.healthPushRevoke = async (id) => {
    calls.push(`revoke:${id}`)
    throw new Error('the shell is not answering')
  }
  health.deleteSource(second)
  await health.question.run()
  assert.deepEqual(calls, [`revoke:${second.id}`], 'a refusal stops the delete')
  assert.equal(health.sources.length, 1, 'and the source is still there')
  api.healthPushRevoke = revoke
  health.deleteSource(second)
  await health.question.run()
  assert.equal(health.sources.length, 0)

  await health.revokePush('nobody')
  api.deleteHealthSource = drop
}

// ── importing an Apple export ────────────────────────────────────────

{
  await health.createSource('appleExport', '')
  assert.equal(
    health.sources.at(-1).name,
    'Apple Health export',
    'a source with no name is called what it is',
  )
  health.pollMs = 40
  await health.importApple()
  assert.equal(health.importStatus.state, 'running', 'it is asked after until it is done')
  assert.equal(health.importStatus.fileName, 'export.zip')
  await until('the import finishes', () => health.importStatus?.state === 'done')
  assert.equal(health.importStatus.report.samplesAdded, 12480)
  assert.ok(health.importStatus.read === health.importStatus.total)
  await until('the source knows', () => health.sources[0]?.status.samplesTotal >= 12480)

  // And can be stopped.
  await health.importApple()
  assert.equal(health.importStatus.state, 'running')
  await health.cancelImport()
  assert.equal(health.importStatus.state, 'cancelled')

  // A lock stops the asking: nothing lands after it.
  await health.importApple()
  assert.equal(health.importStatus.state, 'running')
  health.reset()
  assert.equal(health.importStatus, null)
  await sleep(1000)
  assert.equal(health.importStatus, null, 'no poll was left running past the lock')
}

// ── on Overview ──────────────────────────────────────────────────────

app.section = 'health'
await health.start()
assert.equal(health.loaded, true, 'it starts again after a lock')
assert.equal(health.view, 'medications', 'on the pane it was on: chrome outlives a lock')
assert.equal(health.date, today)
{
  await tracking.load(true)
  const before = tracking.trackers.length
  await health.trackOnOverview('steps', { type: 'sum' }, 'Daily steps')
  assert.equal(tracking.trackers.length, before + 1)
  const tracker = tracking.trackers.find((t) => t.name === 'Daily steps')
  assert.deepEqual(JSON.parse(JSON.stringify(tracker.source)), {
    type: 'health',
    metric: 'steps',
    stat: { type: 'sum' },
  })
  const days = await callCommand('tracker_days', {
    query: { trackerIds: [tracker.id], from: addDays(today, -13), to: today },
  })
  assert.ok(days.length >= 10, 'a day of steps for each day')
  assert.ok(days.every((d) => d.sum > 0 && d.date >= addDays(today, -13)))
  await health.trackOnOverview('blood_glucose', { type: 'inRange', low: 70, high: 180 }, '')
  const range = tracking.trackers.find(
    (t) => t.source?.type === 'health' && t.source.metric === 'blood_glucose',
  )
  assert.equal(range.name, 'Blood glucose', 'with no name it is called what the metric is')
  const rangeDays = await callCommand('tracker_days', {
    query: { trackerIds: [range.id], from: addDays(today, -13), to: today },
  })
  assert.ok(
    rangeDays.every((d) => d.sum > 0 && d.sum <= 100),
    'a share in range is a percentage',
  )
}

// ── the assistant is told where, never what ──────────────────────────

{
  const [w] = await api.listWorkouts(
    new Date(`${addDays(today, -30)}T00:00:00`).toISOString(),
    new Date(`${today}T00:00:00`).toISOString(),
  )
  // Everything open at once: a condition, a document, a workout.
  health.conditionId = 'cond-t2d'
  health.documentId = 'doc-october'
  health.workoutId = w.id
  const SECRETS = [
    'Metformin',
    'Levothyroxine',
    'Calcium',
    'Penicillin',
    'diabetes',
    'Hypothyroidism',
    'Lab report',
    'Outdoor Run',
    'Test med',
    'mg/dL',
  ]
  const opened = []
  for (const view of HEALTH_VIEWS) {
    health.setView(view)
    const showing = health.showing
    const told = JSON.stringify(showing)
    for (const secret of SECRETS) {
      assert.ok(
        !told.includes(secret),
        `${view}: ${secret} must not be what the assistant is told: ${told}`,
      )
    }
    assert.ok(showing.view.startsWith('their '), `${view}: the pane in words: ${showing.view}`)
    opened.push(...(showing.open ?? []).map((o) => `${view}:${o.kind}`))
  }
  assert.deepEqual(
    opened,
    ['activity:workout', 'conditions:condition', 'records:healthDocument'],
    'a record is open only on the pane that draws it',
  )
  await sleep(400)
  health.conditionId = null
  health.documentId = null
  health.workoutId = null
}

// ── live reloads ─────────────────────────────────────────────────────

{
  health.setView('today')
  health.goToday()
  await sleep(200)
  app.section = 'overview'
  assert.equal(health.samplesArrived(), undefined, 'not while another app is open')
  app.section = 'health'
  health.setView('medications')
  assert.equal(
    health.samplesArrived(),
    undefined,
    'readings arriving do not move the medicine list',
  )
  health.setView('today')
  const first = health.samplesArrived()
  assert.ok(first, 'but Today is read again')
  await first
  assert.equal(health.samplesArrived(), undefined, 'at most once a minute')
}

// ── a lock ───────────────────────────────────────────────────────────

health.setView('records')
health.deleteMeal({ id: 'x', name: '', items: [] })
assert.ok(health.question)
health.reset()
assert.equal(health.question, null, 'a question goes with the lock')
assert.equal(health.loaded, false)
assert.equal(health.today, null)
assert.equal(health.series, null)
assert.equal(health.glucose, null)
assert.equal(health.conditionReport, null)
assert.equal(health.catalog.length, 0)
assert.equal(health.settings, null)
assert.equal(health.sources.length, 0)
assert.equal(health.medications.length, 0)
assert.equal(health.labs.length, 0)
assert.equal(health.documents.length, 0)
assert.equal(health.editingMeal, null)
assert.equal(health.readingOpen, false)
assert.equal(health.anchor, today, 'it comes back on today')
assert.equal(health.view, 'records', 'but on the same pane, which is chrome')
await health.flush()

await close()
console.log('health-store: all checks passed')

// Same reason `screentime-store.test.mjs` ends like this: `app.start()` arms
// the idle-vault poll, which would otherwise hold this process open for ever.
process.exit(0)
