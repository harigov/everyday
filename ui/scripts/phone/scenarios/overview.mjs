// The overview app on a phone: the page of cards, its header and tools, the
// dialog that adds a card, arranging and placing, and the brief pages. See
// ../scenarios.mjs.
//
// A phone's screen is a window onto a long page, and the check only looks at
// what is in the window -- so the `-all` scenarios stand the window up to the
// height of the whole page first (`tall`), which puts every card, tile and
// figure in front of it.

import { press } from '../helpers.mjs'

/** Make the window as tall as the page's scroller, so all of it is checked. */
async function tall(page, selector = '.overview .body') {
  const [width, top, content] = await page.evaluate((selector) => {
    const body = document.querySelector(selector)
    const r = body.getBoundingClientRect()
    return [window.innerWidth, r.top, body.scrollHeight]
  }, selector)
  // Under the scroller there is the bottom bar; over it, the frame.
  const height = Math.ceil(top + content + 140)
  await page.setViewportSize({ width, height: Math.min(height, 7000) })
  await page.waitForTimeout(300)
}

const brief = async ({ page, settle }) => {
  await press(page.getByRole('tab', { name: 'Brief' }))
  await settle(700)
}

const kind = (name) => async (ctx) => {
  await brief(ctx)
  await press(ctx.page.getByRole('button', { name, exact: true }))
  await ctx.settle(700)
}

/** Does a laptop's window stand in for the phone? It has no "page options"
 *  button: the same things are buttons in the header. */
const desktop = () => globalThis.PHONE_CHECK_DESKTOP === true

/** The page's tools: on a phone behind its options button, in a menu. */
const options = async ({ page, settle }) => {
  await press(page.getByRole('button', { name: 'Page options' }))
  await settle()
}

const arrange = async (ctx) => {
  if (desktop()) {
    await press(ctx.page.getByRole('button', { name: 'Arrange' }))
  } else {
    await options(ctx)
    await press(ctx.page.getByRole('menuitem', { name: 'Arrange this page' }))
  }
  await ctx.settle()
}

/**
 * The Overview keeps which page it was on, and its cards, in the page's
 * storage -- and every scenario of every app shares one browser context, so
 * a scenario that chose the Brief or took the cards off would hand the next
 * page it opens, the frame's scenarios among them, the Overview as it left
 * it. Wiped once a scenario has set its screen up: the screen is in memory
 * and stays as it is; the pages after it start as a new visit would.
 */
const forgetting = (scenario) => ({
  ...scenario,
  run: async (ctx) => {
    await scenario.run?.(ctx)
    await ctx.page.evaluate(() => {
      localStorage.removeItem('karmayoga.overview.page')
      localStorage.removeItem('karmayoga.overview.layout')
    })
  },
})

const scenarios = [
  { name: 'home' },
  {
    name: 'home-all',
    run: async ({ page, settle }) => {
      await tall(page)
      await settle()
    },
  },
  {
    // What is behind the page's "more" button.
    name: 'menu',
    run: async (ctx) => {
      if (!desktop()) await options(ctx)
    },
  },
  {
    name: 'record',
    run: async (ctx) => {
      if (desktop()) {
        await press(ctx.page.getByRole('button', { name: 'Record something' }))
      } else {
        await options(ctx)
        await press(ctx.page.getByRole('menuitem', { name: 'Record a reading…' }))
      }
      await ctx.settle()
    },
  },
  {
    name: 'add',
    run: async ({ page, settle }) => {
      await press(page.getByRole('button', { name: 'Add', exact: true }))
      await settle()
    },
  },
  {
    // A card picked, and the page asking where it goes.
    name: 'placing',
    run: async ({ page, settle }) => {
      await press(page.getByRole('button', { name: 'Add', exact: true }))
      await settle()
      await press(page.locator('.tile').first())
      await settle()
    },
  },
  { name: 'arrange', run: arrange },
  {
    name: 'arrange-all',
    run: async (ctx) => {
      await arrange(ctx)
      await tall(ctx.page)
      await ctx.settle()
    },
  },
  {
    // Every card taken off: the empty page.
    name: 'empty',
    run: async ({ page, settle }) => {
      await arrange({ page, settle })
      const remove = page.locator('button[aria-label$="off the page"]')
      for (let i = 0; i < 40 && (await remove.count()) > 0; i++) {
        await press(remove.first())
        await page.waitForTimeout(60)
      }
      await settle()
    },
  },
  {
    // The assistant's proposals folded away.
    name: 'attention-folded',
    run: async ({ page, settle }) => {
      await press(page.getByRole('button', { name: 'Hide what needs attention' }))
      await settle()
    },
  },
  { name: 'brief', run: brief },
  {
    name: 'brief-all',
    run: async (ctx) => {
      await brief(ctx)
      await tall(ctx.page)
      await ctx.settle()
    },
  },
  { name: 'brief-weekly', run: kind('Weekly') },
  {
    name: 'brief-weekly-all',
    run: async (ctx) => {
      await kind('Weekly')(ctx)
      await tall(ctx.page)
      await ctx.settle()
    },
  },
  {
    name: 'brief-podcast-all',
    run: async (ctx) => {
      await kind('Podcast')(ctx)
      await tall(ctx.page)
      await ctx.settle()
    },
  },
]

export default { overview: scenarios.map(forgetting) }
