// The screen time app on a phone: the summary over a day, a week and a month,
// a column read, a category chosen, the timeline, the apps and sites with the
// picker that files each under a category, the categories with their colours,
// and the drawer. See ../scenarios.mjs.
//
// The mock records two devices with a few categories, so every screen has
// something on it. The sidebar is a drawer on a phone and a column beside the
// app on a desktop (this runs with `--desktop` too, which clicks: the screens
// are what `compare.py` holds still while the phone's rules change); the range
// chips are the header's on a phone and the sidebar's on a desktop.

import { back, press } from '../helpers.mjs'

const desktop = () => globalThis.PHONE_CHECK_DESKTOP === true

/** The sidebar's drawer, on a phone. */
async function drawer({ page, settle }) {
  if (desktop()) return
  await press(page.getByRole('button', { name: 'Show the sidebar' }))
  await settle()
}

/** A pane by the name on its row. */
const pane = (name) => async (ctx) => {
  await drawer(ctx)
  await press(ctx.page.locator('.side-row', { hasText: name }).first())
  await ctx.settle()
}

const steps =
  (...fns) =>
  async (ctx) => {
    for (const fn of fns) await fn(ctx)
  }

/**
 * How long the summary covers: the header's chip on a phone, the sidebar's on
 * a desktop. The pane a person last looked at is remembered, so it starts by
 * going to the summary, which is where the range is.
 */
const range = (label) => async (ctx) => {
  await pane('Summary')(ctx)
  const chips = ctx.page.getByRole('group', { name: 'How long the summary covers' })
  // Whichever of the two groups is on screen.
  await press(chips.getByRole('button', { name: label, exact: true }).first())
  await ctx.settle()
}

/** The end of what scrolls: a pane's page, the list of apps. */
const scrollEnd = async ({ page, settle }) => {
  await page.evaluate(() => {
    for (const el of document.querySelectorAll('.scroll, .list')) el.scrollTop = el.scrollHeight
  })
  await settle()
}

const timeline = pane('Timeline')
const apps = pane('Apps & sites')
const categories = pane('Categories')

/** The first app's category chip, tapped. */
const openPicker = steps(apps, async ({ page, settle }) => {
  await press(page.locator('.list-row [aria-haspopup="listbox"]').first())
  await settle()
})

const openSummaryPicker = async ({ page, settle }) => {
  await press(page.locator('.top [aria-haspopup="listbox"]').first())
  await settle()
}

export default {
  screentime: [
    // The app as it opens: today's summary.
    { name: 'home' },
    { name: 'home-bottom', run: scrollEnd },
    { name: 'drawer', run: drawer },

    // The summary's day: a column read, a category chosen, a day back.
    {
      name: 'day-column',
      run: async ({ page, settle }) => {
        await press(page.locator('.plot .slot').nth(8))
        await settle()
      },
    },
    {
      name: 'category-only',
      run: async ({ page, settle }) => {
        // Narrowed to one category: the chip that says so, and the way out.
        await press(page.locator('.entry.pick').first())
        await settle()
      },
    },
    {
      name: 'day-before',
      run: async ({ page, settle }) => {
        await press(page.getByRole('button', { name: 'The one before' }))
        await settle()
      },
    },
    { name: 'summary-picker', run: openSummaryPicker },

    // The timeline of one day, and what was in front of the person.
    { name: 'timeline', run: timeline },
    { name: 'timeline-bottom', run: steps(timeline, scrollEnd) },
    {
      name: 'timeline-before',
      run: steps(timeline, async ({ page, settle }) => {
        await press(page.getByRole('button', { name: 'The day before' }))
        await settle()
      }),
    },

    // Apps and sites, with the category picker over them.
    { name: 'apps', run: apps },
    { name: 'apps-bottom', run: steps(apps, scrollEnd) },
    {
      name: 'apps-search',
      run: steps(apps, async ({ page, settle }) => {
        // The search is the top bar's now (it narrows the Apps pane through
        // the store), not a field in the pane.
        await page.getByRole('searchbox').first().fill('c')
        await settle()
      }),
    },
    {
      name: 'apps-uncategorised',
      run: steps(apps, async ({ page, settle }) => {
        await press(page.getByRole('button', { name: /^Uncategorised/ }).first())
        await settle()
      }),
    },
    { name: 'apps-picker', run: openPicker },
    {
      name: 'apps-picker-new',
      run: steps(openPicker, async ({ page, settle }) => {
        await page.getByLabel('Find or make a category').fill('Reading')
        await settle()
      }),
    },
    {
      name: 'apps-picker-back',
      run: steps(openPicker, async ({ page, settle }) => {
        await back(page)
        await settle()
      }),
    },
    {
      name: 'apps-more',
      run: steps(apps, async ({ page, settle }) => {
        await press(page.getByRole('button', { name: /^More for/ }).first())
        await settle()
      }),
    },

    // Categories: the list, the colours, a new one.
    { name: 'categories', run: categories },
    {
      name: 'categories-colour',
      run: steps(categories, async ({ page, settle }) => {
        await press(page.getByRole('button', { name: /^Colour of/ }).first())
        await settle()
      }),
    },
    {
      name: 'categories-new',
      run: steps(categories, async ({ page, settle }) => {
        await page.getByLabel('New category').fill('Reading')
        await settle()
      }),
    },

    // The summary over a week and a month, with a column read. The range is
    // remembered, so these come last: each leaves the next with another.
    { name: 'week', run: range('Week') },
    {
      name: 'week-column',
      run: steps(range('Week'), async ({ page, settle }) => {
        // One day's column: its figures are read out, and a second tap opens it.
        const slots = page.locator('.plot .slot')
        await press(slots.nth((await slots.count()) > 2 ? 2 : 0))
        await settle()
      }),
    },
    { name: 'month', run: range('Month') },
    { name: 'month-bottom', run: steps(range('Month'), scrollEnd) },
  ],
}
