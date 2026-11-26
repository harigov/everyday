// The journal on a phone: the list of entries (with its month, and folded
// without it), an entry opened over it and back, the editor with the keyboard
// up, tags and the other things on an entry's page, a new entry, search and
// its empty state, the drawer, and the dialogs and menus an entry raises.
// See ../scenarios.mjs.
//
// The mock's clock is Wednesday 25 November: "First frost" is today's entry,
// and the first in the list.
//
// A screen only a phone has -- the drawer's button, the way back -- is
// skipped in a `--desktop` run (see `check.mjs`): the desktop is compared
// screen for screen, and a scenario that cannot reach the screen shows the
// app's home instead.

import { back, press } from '../helpers.mjs'

const desktop = () => globalThis.PHONE_CHECK_DESKTOP === true

const openFirst = async ({ page, settle }) => {
  await press(page.locator('[role=option], .row, .entry').first())
  await settle(600)
}

const openDrawer = async ({ page, settle }) => {
  await press(page.getByRole('button', { name: 'Show the sidebar' }))
  await settle()
}

/** Something typed into the top bar's search, and Enter: the app's search. */
const search = async ({ page, settle }, text) => {
  const field = page.getByPlaceholder(/Search the journal/)
  await field.fill(text)
  await field.press('Enter')
  await settle(700)
}

/** The month over the list, shown if it is not already: a phone starts with
 *  it folded away, and a desktop shows it. */
const showMonth = async ({ page, settle }) => {
  if ((await page.locator('.minimonth').count()) === 0) {
    await press(page.getByRole('button', { name: 'Show the month' }))
    await settle()
  }
}

/** The software keyboard, as far as the page can tell: the window shorter by
 *  what it covers, and the flag the shell raises. */
const raiseKeyboard = async ({ page, size, settle }) => {
  await page.setViewportSize({ width: size.width, height: 430 })
  await page.evaluate(() => document.documentElement.setAttribute('data-keyboard', ''))
  await settle()
}

/** Into the editor's text, at its end, and some words typed. */
const typeInto = async ({ page, settle }, text) => {
  await page.locator('.ed-content').first().click()
  await page.keyboard.press('Control+End')
  await page.keyboard.type(text)
  await settle()
}

/** Scrolled to the end of whatever scrolls. */
const toEnd = async ({ page, settle }) => {
  await page.evaluate(() => {
    for (const el of document.querySelectorAll('.scroll')) el.scrollTop = el.scrollHeight
  })
  await settle()
}

const scenarios = [
  // The list. On a phone the month starts folded away; the button shows it.
  { name: 'home' },
  {
    name: 'home-bottom',
    run: async (ctx) => {
      await toEnd(ctx)
    },
  },
  // The sidebar's half: the journals, in the drawer.
  {
    name: 'drawer',
    run: async (ctx) => {
      if (desktop()) return
      await openDrawer(ctx)
    },
  },
  {
    name: 'drawer-journal',
    run: async (ctx) => {
      if (desktop()) return
      await openDrawer(ctx)
      await press(ctx.page.getByRole('button', { name: /Travel/ }).first())
      await ctx.settle()
    },
  },

  // Search is the top bar's; the results are the list.
  { name: 'search', run: (ctx) => search(ctx, 'frost') },
  { name: 'search-empty', run: (ctx) => search(ctx, 'zzzzzz') },
  {
    name: 'search-open',
    run: async (ctx) => {
      await search(ctx, 'frost')
      await press(ctx.page.locator('.row').first())
      await ctx.settle(600)
    },
  },

  // An entry opened over the list, and back to it.
  { name: 'entry', run: openFirst },
  {
    name: 'entry-back',
    run: async (ctx) => {
      await openFirst(ctx)
      await back(ctx.page)
      await ctx.settle()
    },
  },
  {
    name: 'entry-back-button',
    run: async (ctx) => {
      if (desktop()) return
      await openFirst(ctx)
      await press(ctx.page.getByRole('button', { name: 'Back to Entries' }))
      await ctx.settle()
    },
  },
  {
    name: 'entry-bottom',
    run: async (ctx) => {
      // What is under the writing: the day's tags and what it recorded.
      await openFirst(ctx)
      await toEnd(ctx)
    },
  },
  {
    name: 'entry-long-title',
    run: async (ctx) => {
      await openFirst(ctx)
      await ctx.page
        .locator('input.title, textarea.title')
        .first()
        .fill('First frost, and the long way round by the river to the market')
      await ctx.settle()
    },
  },

  // Writing: the keyboard up, the toolbar and the title still in reach.
  {
    name: 'editor-keyboard',
    run: async (ctx) => {
      await openFirst(ctx)
      await raiseKeyboard(ctx)
      await typeInto(ctx, ' And a second thought.')
    },
  },
  {
    name: 'editor-title-keyboard',
    run: async (ctx) => {
      await openFirst(ctx)
      await raiseKeyboard(ctx)
      await ctx.page.locator('input.title, textarea.title').first().click()
      await ctx.settle()
    },
  },
  {
    name: 'editor-toolbar-end',
    run: async (ctx) => {
      await openFirst(ctx)
      await ctx.page.evaluate(() => {
        const bar = document.querySelector('[role=toolbar]')
        if (bar) bar.scrollLeft = bar.scrollWidth
      })
      await ctx.settle()
    },
  },
  {
    name: 'editor-link',
    run: async (ctx) => {
      await openFirst(ctx)
      await press(ctx.page.getByRole('button', { name: 'Link', exact: true }))
      await ctx.settle()
    },
  },

  // What is on the page besides the writing.
  {
    name: 'tag-add',
    run: async (ctx) => {
      await openFirst(ctx)
      await toEnd(ctx)
      await press(ctx.page.getByRole('button', { name: 'Tag', exact: true }))
      await ctx.settle()
    },
  },
  {
    name: 'tag-add-keyboard',
    run: async (ctx) => {
      await openFirst(ctx)
      await toEnd(ctx)
      await press(ctx.page.getByRole('button', { name: 'Tag', exact: true }))
      await ctx.settle()
      await raiseKeyboard(ctx)
    },
  },

  // A new entry: today's, which exists, and so opens.
  {
    name: 'new',
    run: async ({ page, settle }) => {
      await press(page.getByRole('button', { name: "Today's entry" }).first())
      await settle(700)
    },
  },

  // The entry's dialogs, and a row's long-press menu (Android's `contextmenu`).
  {
    name: 'delete-asks',
    run: async (ctx) => {
      await openFirst(ctx)
      await press(ctx.page.getByRole('button', { name: /^Delete( this entry)?$/ }))
      await ctx.settle()
    },
  },
  {
    name: 'row-menu',
    run: async ({ page, settle }) => {
      await page
        .locator('.row')
        .first()
        .dispatchEvent('contextmenu', { clientX: 160, clientY: 260 })
      await settle()
    },
  },

  // Last: the month's being shown is kept in the page's storage, which the
  // scenarios after it in the same browser would find showing.
  {
    name: 'month',
    run: async ({ page, settle }) => {
      // Shown by default on a desktop, folded on a phone: the button that
      // is "Show the month" there is what puts it back.
      if (desktop()) return
      await showMonth({ page, settle })
    },
  },
  {
    name: 'month-pick',
    run: async ({ page, settle }) => {
      if (desktop()) return
      await showMonth({ page, settle })
      await press(page.getByRole('button', { name: /^Tuesday, November 24/ }))
      await settle(600)
    },
  },
  {
    name: 'month-prev',
    run: async ({ page, settle }) => {
      if (desktop()) return
      await showMonth({ page, settle })
      await press(page.getByRole('button', { name: 'The month before' }))
      await settle()
    },
  },
]

/** The keyboard (and typing at the page) is a phone's: a desktop run shows the app's home for those. */
const withKeyboard = (scenario) =>
  /keyboard|format/.test(scenario.name)
    ? { ...scenario, run: async (ctx) => (desktop() ? undefined : scenario.run(ctx)) }
    : scenario

export default {
  journal: scenarios.map(withKeyboard),
}
