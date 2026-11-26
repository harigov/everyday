// The health app on a phone: Today, each pane in the drawer (activity, sleep,
// vitals, nutrition, medications, conditions, records), what each opens --
// a workout, a measurement, a meal, a medicine, a condition, a document, a
// test -- the editors and sheets over them, and the sources and import in
// Health's settings. See ../scenarios.mjs.
//
// The mock's clock is Wednesday 25 November, 10:30, with sixty days behind
// it: a watch and a phone as sources, two workouts this week, metformin
// twice a day, levothyroxine and calcium, type 2 diabetes and an underactive
// thyroid with a plan each, a Dexcom, and two lab reports (one with results
// still to be checked).
//
// The panes are reached through the sidebar, which is a drawer on a phone
// and a column beside the app on a desktop (this runs with `--desktop` too,
// which clicks: the screens are what `compare.py` holds still while the
// phone's rules change). A screen only a phone has is skipped there.

import { back, press } from '../helpers.mjs'

const desktop = () => globalThis.PHONE_CHECK_DESKTOP === true

/** The sidebar's drawer, on a phone. */
async function drawer({ page, settle }) {
  if (desktop()) return
  await press(page.getByRole('button', { name: 'Show the sidebar' }))
  await settle()
}

/** A pane, or a condition under Conditions, by the name on its row. */
const pane = (name) => async (ctx) => {
  await drawer(ctx)
  await press(ctx.page.locator('.side-row', { hasText: name }).first())
  await ctx.settle()
}

/** A button by its name, tapped. */
const tapButton =
  (name, options = {}) =>
  async ({ page, settle }) => {
    await press(page.getByRole('button', { name, ...options }).first())
    await settle()
  }

/** Several steps one after another. */
const steps =
  (...fns) =>
  async (ctx) => {
    for (const fn of fns) await fn(ctx)
  }

/** The first thing with this text, tapped. */
const tapText =
  (text, options = {}) =>
  async ({ page, settle }) => {
    await press(page.getByText(text, { exact: false, ...options }).first())
    await settle()
  }

/** The end of what scrolls: a pane's page, a sheet's contents. */
const scrollEnd = async ({ page, settle }) => {
  await page.evaluate(() => {
    for (const el of document.querySelectorAll('.scroll, .sheet-scroll, .cols, .list')) {
      el.scrollTop = el.scrollHeight
    }
  })
  await settle()
}

/** The nth of the window's chips by their label: Week, Month, 3 months, Year. */
const range = (label) => tapButton(label, { exact: true })

/** Today's way to move: the day before. */
const dayBefore = tapButton('The day before')

const activity = pane('Activity')
const sleep = pane('Sleep')
const vitals = pane('Vitals')
const nutrition = pane('Nutrition')
const medications = pane('Medications')
const conditions = pane('Conditions')
const records = pane('Records')
const diabetes = pane('Type 2 diabetes')
const thyroid = pane('Hypothyroidism')

const openWorkout = steps(activity, async ({ page, settle }) => {
  await press(page.locator('.list-row.workout').first())
  await settle()
})

const openBloodPressure = steps(vitals, tapText('Blood pressure'))

const openMealEditor = steps(nutrition, tapButton('Log a meal'))
const openMeal = steps(nutrition, tapText('Oatmeal and banana'))
const openFoodSearch = steps(openMealEditor, tapButton('Add a food'))

const openMedication = steps(medications, tapText('Metformin', { exact: true }))
const addMedication = steps(medications, tapButton('Add medication'))

const openDocument = steps(records, tapText('Lipid and kidney panels'))
/** A condition's template, by its name, on the page that lists them. */
const chooseTemplate =
  (name) =>
  async ({ page, settle }) => {
    await press(page.locator('.tcard', { hasText: name }).first())
    await settle()
  }

const openReview = steps(records, async ({ page, settle }) => {
  await press(page.locator('.list-row.doc', { hasText: 'to review' }).first())
  await settle(700)
})

const openLabs = steps(records, async ({ page, settle }) => {
  await press(page.getByRole('tab', { name: /Lab results/ }))
  await settle()
})
const openTrend = steps(openLabs, async ({ page, settle }) => {
  await press(page.locator('.list-row.test').first())
  await settle()
})

/** Health's settings: the sources a person has connected, and the import. */
const openSettings = async (ctx) => {
  await drawer(ctx)
  await press(ctx.page.locator('.side-row', { hasText: 'Health settings' }).first())
  await ctx.settle(600)
}

/** Scrolls what the selector names into view. */
const scrollTo =
  (selector) =>
  async ({ page, settle }) => {
    await page.locator(selector).first().scrollIntoViewIfNeeded()
    await settle()
  }

/** A phone's setup: the key is made once the person has agreed to a new one. */
const openPush = steps(openSettings, tapButton('Phone setup…'), tapButton('Make a new key'))

/**
 * A day's readings sit under its entry in the journal: the entry opened, and
 * scrolled to the foot where the chips are.
 */
const openStrip = async ({ page, settle }) => {
  await press(page.locator('[role=option], .row, .entry').first())
  await settle()
  await page.locator('.strip').first().scrollIntoViewIfNeeded()
  await settle()
}

/** A chip of the strip, by what it records. */
const tapChip =
  (name) =>
  async ({ page, settle }) => {
    await press(page.locator('.strip .chip', { hasText: name }).first())
    await settle()
  }

export default {
  health: [
    // The app as it opens: Today.
    { name: 'home' },
    { name: 'home-bottom', run: scrollEnd },
    { name: 'day-before', run: dayBefore },
    // Only a phone has the drawer to open; the desktop's sidebar is always there.
    { name: 'drawer', run: drawer },

    // Activity, over a month, a bar read, and a workout opened.
    { name: 'activity', run: activity },
    {
      name: 'activity-bar',
      run: steps(activity, async ({ page, settle }) => {
        // A finger on one bar: what it says is read out, not opened.
        const box = await page.locator('.stage').first().boundingBox()
        if (!box) return
        const x = box.x + box.width * 0.6
        const y = box.y + box.height * 0.6
        if (desktop()) await page.mouse.move(x, y)
        else await page.touchscreen.tap(x, y)
        await settle()
      }),
    },
    { name: 'activity-bottom', run: steps(activity, scrollEnd) },
    { name: 'workout', run: openWorkout },
    {
      name: 'workout-back',
      run: steps(openWorkout, async ({ page, settle }) => {
        await back(page)
        await settle()
      }),
    },

    // Sleep, with a night chosen.
    { name: 'sleep', run: sleep },
    { name: 'sleep-bottom', run: steps(sleep, scrollEnd) },

    // Vitals: the list of measurements and the one chosen.
    { name: 'vitals', run: vitals },
    { name: 'vitals-detail', run: steps(vitals, tapText('Weight')) },
    { name: 'vitals-pressure', run: openBloodPressure },
    { name: 'vitals-glucose', run: steps(vitals, tapText('Blood glucose')) },
    {
      name: 'vitals-back',
      run: steps(openBloodPressure, async ({ page, settle }) => {
        await back(page)
        await settle()
      }),
    },
    { name: 'reading', run: steps(vitals, tapText('Weight'), tapButton('Add reading')) },
    {
      name: 'reading-pressure',
      run: steps(openBloodPressure, tapButton('Add reading'), async ({ page, settle }) => {
        // Both numbers' fields, one over the other.
        await page.getByLabel('What was measured').selectOption({ label: 'Blood pressure' })
        await settle()
      }),
    },

    // Nutrition: the day, a meal edited, a food found.
    { name: 'nutrition', run: nutrition },
    { name: 'nutrition-glucose', run: steps(nutrition, tapText('Glucose response')) },
    { name: 'meal-new', run: openMealEditor },
    { name: 'meal-edit', run: openMeal },
    { name: 'meal-edit-bottom', run: steps(openMeal, scrollEnd) },
    { name: 'food-search', run: openFoodSearch },
    {
      name: 'food-make',
      run: steps(openFoodSearch, tapButton('Make a food'), scrollEnd),
    },
    {
      name: 'meal-back',
      run: steps(openFoodSearch, async ({ page, settle }) => {
        // Back closes the food search, then the editor.
        await back(page)
        await settle()
      }),
    },

    // Medications: due today, the list, a medicine opened and a new one.
    { name: 'medications', run: medications },
    { name: 'medications-bottom', run: steps(medications, scrollEnd) },
    {
      name: 'medications-other-time',
      run: steps(medications, tapButton('At another time')),
    },
    { name: 'medication-new', run: addMedication },
    { name: 'medication-new-bottom', run: steps(addMedication, scrollEnd) },
    { name: 'medication-edit', run: openMedication },
    {
      name: 'medication-weekly',
      run: steps(addMedication, async ({ page, settle }) => {
        await page.locator('#med-sched').selectOption({ label: 'On certain days of the week' })
        await settle()
      }),
    },
    {
      name: 'medication-back',
      run: steps(addMedication, async ({ page, settle }) => {
        await back(page)
        await settle()
      }),
    },

    // Conditions: the list, one opened with its plan, a template chosen.
    { name: 'conditions', run: conditions },
    { name: 'condition-diabetes', run: diabetes },
    {
      name: 'condition-diabetes-glucose',
      run: steps(diabetes, async ({ page, settle }) => {
        await page.locator('#cd-glucose').scrollIntoViewIfNeeded()
        await settle()
      }),
    },
    {
      name: 'condition-diabetes-profile',
      run: steps(diabetes, async ({ page, settle }) => {
        await page.locator('#cd-agp').scrollIntoViewIfNeeded()
        await settle()
      }),
    },
    {
      name: 'condition-diabetes-day',
      run: steps(diabetes, async ({ page, settle }) => {
        await page.locator('#cd-day').scrollIntoViewIfNeeded()
        await settle()
      }),
    },
    { name: 'condition-diabetes-bottom', run: steps(diabetes, scrollEnd) },
    { name: 'condition-thyroid', run: steps(thyroid, scrollEnd) },
    {
      name: 'condition-edit',
      run: steps(diabetes, tapButton('Edit this condition')),
    },
    { name: 'condition-visit', run: steps(diabetes, tapButton('Prepare for a visit')) },
    {
      name: 'condition-visit-written',
      run: steps(
        diabetes,
        tapButton('Prepare for a visit'),
        tapButton('Write the summary'),
        async ({ settle }) => settle(700),
      ),
    },
    {
      name: 'condition-back',
      run: steps(diabetes, async ({ page, settle }) => {
        await back(page)
        await settle()
      }),
    },
    { name: 'condition-new', run: steps(conditions, tapButton('Add a condition')) },
    {
      name: 'condition-template',
      run: steps(conditions, tapButton('Add a condition'), chooseTemplate('Type 2 diabetes')),
    },
    {
      name: 'condition-template-bottom',
      run: steps(
        conditions,
        tapButton('Add a condition'),
        chooseTemplate('Type 2 diabetes'),
        scrollEnd,
      ),
    },
    { name: 'condition-blank', run: steps(conditions, tapButton('A blank one')) },

    // Records: documents, one opened, results to check, labs and a test.
    { name: 'records', run: records },
    { name: 'records-add', run: steps(records, tapButton('Add a document')) },
    { name: 'document', run: openDocument },
    { name: 'document-bottom', run: steps(openDocument, scrollEnd) },
    { name: 'document-review', run: openReview },
    { name: 'document-review-bottom', run: steps(openReview, scrollEnd) },
    {
      name: 'document-back',
      run: steps(openDocument, async ({ page, settle }) => {
        await back(page)
        await settle()
      }),
    },
    { name: 'labs', run: openLabs },
    { name: 'lab-trend', run: openTrend },
    { name: 'lab-trend-bottom', run: steps(openTrend, scrollEnd) },
    { name: 'lab-editor', run: steps(openTrend, tapButton('Add a result')) },

    // Health's settings: the sources connected, the phone's setup, and the
    // history to import.
    { name: 'settings', run: openSettings },
    { name: 'settings-rename', run: steps(openSettings, tapButton(/^Rename /)) },
    { name: 'settings-push-ask', run: steps(openSettings, tapButton('Phone setup…')) },
    { name: 'settings-push', run: openPush },
    { name: 'settings-push-bottom', run: steps(openPush, scrollEnd) },
    { name: 'settings-import', run: steps(openSettings, scrollTo('#import-head')) },
    { name: 'settings-bottom', run: steps(openSettings, scrollEnd) },

    // A day's readings under its journal entry: the chips, the panel that
    // records one, the field that records anything.
    { name: 'tracker-strip', section: 'journal', run: openStrip },
    { name: 'tracker-amount', section: 'journal', run: steps(openStrip, tapChip('Vitamin D')) },
    { name: 'tracker-scale', section: 'journal', run: steps(openStrip, tapChip('Headache')) },
    {
      // Skipped in a `--desktop` run: there the popover hangs from a plus at
      // the foot of the page and runs past the window's right edge, which is
      // the desktop's to fix and not this check's to compare.
      name: 'tracker-log',
      section: 'journal',
      run: steps(openStrip, async ({ page, settle }) => {
        if (desktop()) return
        await press(page.locator('.strip > .slot > button.add').first())
        await settle()
      }),
    },

    // The window a pane looks at is remembered, so these come last: each
    // leaves the next with a longer one than the week the rest show.
    { name: 'activity-month', run: steps(activity, range('Month')) },
    { name: 'sleep-year', run: steps(sleep, range('Year')) },
    { name: 'nutrition-month', run: steps(nutrition, range('Month')) },
  ],
}
