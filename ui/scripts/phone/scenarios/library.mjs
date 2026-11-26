// The library app on a phone: the shelf as covers and as a list, the shelves
// in the drawer, an item opened over the grid (its notes, history, cover
// search, delete) and back out of each layer, adding a thing, and the empty
// states. See ../scenarios.mjs.
//
// The mock's shelves: Books (Dune, Pnin, ...), Movies, Games, Albums,
// Restaurants, Recipes, Drinks, Articles, People. "Pnin" is a book being
// read; "Dune" is finished and rated.
//
// A screen only a phone has -- the drawer -- is skipped in a `--desktop` run
// (see `check.mjs`): the desktop is compared screen for screen, and a
// scenario that cannot reach the screen shows the app's home instead.

import { back, press } from '../helpers.mjs'

const desktop = () => globalThis.PHONE_CHECK_DESKTOP === true

const openDrawer = async ({ page, settle }) => {
  await press(page.getByRole('button', { name: 'Show the sidebar' }))
  await settle()
}

/** A shelf, chosen in the drawer on a phone and in the sidebar on a desktop. */
const chooseShelf = (name) => async (ctx) => {
  if (!desktop()) await openDrawer(ctx)
  await press(ctx.page.locator('.side-row', { hasText: name }).first())
  await ctx.settle()
}

/** The shelf as covers: the view last used is kept in the browser. */
const showGrid = async ({ page, settle }) => {
  await press(page.getByRole('button', { name: 'Covers', exact: true }))
  await settle()
}

const openCard = (title) => async (ctx) => {
  await showGrid(ctx)
  await press(ctx.page.getByRole('button', { name: title, exact: true }).first())
  await ctx.settle(500)
}

const scrollDetailToEnd = async ({ page, settle }) => {
  await page.evaluate(() => {
    for (const el of document.querySelectorAll('.detail .scroll')) el.scrollTop = el.scrollHeight
  })
  await settle()
}

const showList = async ({ page, settle }) => {
  await press(page.getByRole('button', { name: 'List', exact: true }))
  await settle()
}

const quick = (run) => async (ctx) => {
  // A scenario that cannot find its control fails in seconds, not half a minute.
  ctx.page.setDefaultTimeout(8000)
  await run?.(ctx)
}

const scenarios = [
  { name: 'home' },
  {
    name: 'home-end',
    run: async ({ page, settle }) => {
      await page.evaluate(() => {
        for (const el of document.querySelectorAll('.library .scroll'))
          el.scrollTop = el.scrollHeight
      })
      await settle()
    },
  },
  { name: 'list', run: showList },
  { name: 'shelf', run: chooseShelf('Books') },
  {
    name: 'shelf-list',
    run: async (ctx) => {
      await chooseShelf('Books')(ctx)
      await showList(ctx)
    },
  },
  { name: 'shelf-contacts', run: chooseShelf('Contacts') },
  {
    name: 'drawer',
    run: async (ctx) => {
      if (desktop()) return
      await openDrawer(ctx)
    },
  },
  {
    name: 'new-shelf',
    run: async (ctx) => {
      if (!desktop()) await openDrawer(ctx)
      await press(ctx.page.getByRole('button', { name: 'New shelf' }))
      await ctx.settle()
    },
  },

  // An item opened over the shelf, and back.
  { name: 'item', run: openCard('Dune') },
  {
    name: 'item-back',
    run: async (ctx) => {
      await openCard('Dune')(ctx)
      if (desktop()) return
      await back(ctx.page)
      await ctx.settle()
    },
  },
  { name: 'item-reading', run: openCard('Pnin') },
  {
    name: 'item-end',
    run: async (ctx) => {
      await openCard('Dune')(ctx)
      await scrollDetailToEnd(ctx)
    },
  },
  {
    name: 'item-from-list',
    run: async (ctx) => {
      await showList(ctx)
      await press(ctx.page.locator('.row.list-row', { hasText: 'Arrival' }).first())
      await ctx.settle(500)
    },
  },
  {
    name: 'item-person',
    run: async (ctx) => {
      await chooseShelf('Contacts')(ctx)
      await showGrid(ctx)
      await press(ctx.page.getByRole('button', { name: 'Ada Byron', exact: true }).first())
      await ctx.settle(500)
    },
  },
  {
    name: 'item-cover-search',
    run: async (ctx) => {
      await openCard('Dune')(ctx)
      await press(ctx.page.getByRole('button', { name: 'Search for a picture' }))
      await ctx.settle(2500)
    },
  },
  {
    name: 'item-cover-address',
    run: async (ctx) => {
      await openCard('Dune')(ctx)
      await press(ctx.page.getByRole('button', { name: 'Use the address of a picture' }))
      await ctx.settle()
    },
  },
  {
    name: 'item-look-up',
    run: async (ctx) => {
      await openCard('Dune')(ctx)
      await press(ctx.page.getByRole('button', { name: /Look up again|Look this up/ }))
      await ctx.settle(900)
      await scrollDetailToEnd(ctx)
    },
  },
  {
    name: 'item-attach',
    run: async (ctx) => {
      await openCard('Dune')(ctx)
      await press(ctx.page.getByRole('button', { name: /Attach a file/ }))
      await ctx.settle(700)
    },
  },
  {
    name: 'item-delete',
    run: async (ctx) => {
      await openCard('Dune')(ctx)
      await press(ctx.page.getByRole('button', { name: 'Delete', exact: true }))
      await ctx.settle()
    },
  },
  {
    name: 'item-delete-back',
    run: async (ctx) => {
      await openCard('Dune')(ctx)
      await press(ctx.page.getByRole('button', { name: 'Delete', exact: true }))
      await ctx.settle()
      if (desktop()) return
      await back(ctx.page)
      await ctx.settle()
    },
  },

  // Adding a thing: the line, its shelf menu and the lookup's suggestions.
  {
    name: 'add-menu',
    run: async (ctx) => {
      await press(ctx.page.getByRole('button', { name: /^Adding to/ }))
      await ctx.settle()
    },
  },
  {
    name: 'add-suggestions',
    run: async (ctx) => {
      await chooseShelf('Books')(ctx)
      const field = ctx.page.getByRole('textbox', { name: /^Add a book/ })
      await field.fill('Dune')
      await ctx.settle(1500)
    },
  },
  {
    name: 'add-typed',
    run: async (ctx) => {
      await chooseShelf('Books')(ctx)
      const field = ctx.page.getByRole('textbox', { name: /^Add a book/ })
      await field.fill('Something Entirely New')
      await field.press('Enter')
      await ctx.settle(900)
    },
  },

  // Nothing to show.
  {
    name: 'empty-filter',
    run: async (ctx) => {
      // Nothing on the Games shelf is paused.
      await chooseShelf('Games')(ctx)
      await press(ctx.page.getByRole('tab', { name: 'Paused' }))
      await ctx.settle()
    },
  },
  {
    name: 'empty-shelf',
    run: async (ctx) => {
      await chooseShelf('Music')(ctx)
      await press(ctx.page.getByRole('tab', { name: 'To hear' }))
      await ctx.settle()
    },
  },
  {
    name: 'empty-favourites',
    run: async (ctx) => {
      await chooseShelf('Recipes')(ctx)
      await press(ctx.page.locator('.toolbar button.star'))
      await ctx.settle()
    },
  },
  {
    name: 'empty-search',
    run: async ({ page, settle }) => {
      const bar = page.getByRole('combobox', { name: /^Search the library/ })
      await bar.fill('zzzzqx')
      await bar.press('Enter')
      await settle(900)
    },
  },
  {
    name: 'favourites',
    run: async (ctx) => {
      await press(ctx.page.locator('.toolbar button.star'))
      await ctx.settle()
    },
  },
]

export default { library: scenarios.map((s) => ({ ...s, run: quick(s.run) })) }
