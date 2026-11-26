// The notes app on a phone: the list of notes as the app's page, a note
// opened over it and back, the editor with the keyboard up, a new note, the
// drawer (what narrows the list and the order) and each choice made in it,
// search and its empty state, a proposed note, and the dialogs and menus a
// note raises. See ../scenarios.mjs.
//
// The mock's notes: "Boat, before the spring launch" is pinned and first;
// "Sourdough, the version that works" carries the recipes and baking tags;
// the assistant has written some and a call one, so the kind filter is
// there. "Packing day, the short version" is a proposed note, drawn at the
// top of the list.
//
// A screen only a phone has -- the drawer's button, the way back -- is
// skipped in a `--desktop` run (see `check.mjs`): the desktop is compared
// screen for screen, and a scenario that cannot reach the screen shows the
// app's home instead.

import { back, press } from '../helpers.mjs'

const desktop = () => globalThis.PHONE_CHECK_DESKTOP === true

const BOAT = 'Boat, before the spring launch'

const openNote = async ({ page, settle }, title = BOAT) => {
  await press(page.getByText(title, { exact: true }).first())
  await settle(600)
}

const openDrawer = async ({ page, settle }) => {
  await press(page.getByRole('button', { name: 'Show the sidebar' }))
  await settle()
}

/** The first tag chip with this word in it: the drawer's on a phone, the
 *  sidebar's on a desktop. */
const tapTag = async ({ page, settle }, word) => {
  await press(page.locator('.tagchip', { hasText: word }).first())
  await settle()
}

/** Something typed into the top bar's search, and Enter: the app's search. */
const search = async ({ page, settle }, text) => {
  const field = page.getByPlaceholder(/Search notes/)
  await field.fill(text)
  await field.press('Enter')
  await settle(700)
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
  const body = page.locator('.ed-content').first()
  await body.click()
  await page.keyboard.press('Control+End')
  await page.keyboard.type(text)
  await settle()
}

const scenarios = [
  // The app as it opens: on a phone, the notes themselves.
  { name: 'home' },
  {
    name: 'home-bottom',
    run: async ({ page, settle }) => {
      await page.evaluate(() => {
        for (const el of document.querySelectorAll('.scroll')) el.scrollTop = el.scrollHeight
      })
      await settle()
    },
  },

  // What narrows the list lives in the sidebar, which is the drawer.
  {
    name: 'drawer',
    run: async (ctx) => {
      if (desktop()) return
      await openDrawer(ctx)
    },
  },
  {
    name: 'filter-tag',
    run: async (ctx) => {
      if (!desktop()) await openDrawer(ctx)
      await tapTag(ctx, 'recipes')
    },
  },
  {
    name: 'filter-tag-clear',
    run: async (ctx) => {
      if (desktop()) return
      await openDrawer(ctx)
      await tapTag(ctx, 'recipes')
      await press(ctx.page.getByRole('button', { name: /show every note/ }))
      await ctx.settle()
    },
  },
  {
    name: 'filter-tag-none',
    run: async (ctx) => {
      // A tag with a note in it, then a search that matches none of them.
      if (!desktop()) await openDrawer(ctx)
      await tapTag(ctx, 'boats')
      await search(ctx, 'sourdough')
    },
  },
  {
    name: 'filter-kind',
    run: async (ctx) => {
      if (!desktop()) await openDrawer(ctx)
      await press(
        ctx.page.getByRole('tab', { name: /^(The assistant|.* wrote)|assistant/i }).first(),
      )
      await ctx.settle()
    },
  },
  {
    name: 'sort',
    run: async (ctx) => {
      if (desktop()) return
      await openDrawer(ctx)
      await press(ctx.page.getByRole('button', { name: 'By title' }))
      await ctx.settle()
    },
  },
  {
    // Every tag: a menu on a desktop; on a phone they are all in the drawer.
    name: 'tags-all',
    run: async (ctx) => {
      if (!desktop()) {
        await openDrawer(ctx)
        await ctx.page.evaluate(() => {
          const nav = document.querySelector('aside.sidebar .side-nav')
          if (nav) nav.scrollTop = nav.scrollHeight
        })
        await ctx.settle()
        return
      }
      await press(ctx.page.getByRole('button', { name: /All \d+ tags/ }))
      await ctx.settle()
    },
  },

  // Search is the top bar's; the results are the list.
  {
    name: 'search',
    run: (ctx) => search(ctx, 'sour'),
  },
  {
    name: 'search-empty',
    run: (ctx) => search(ctx, 'zzzzzz'),
  },

  // A note opened over the list, and back to it.
  { name: 'note', run: (ctx) => openNote(ctx) },
  {
    name: 'note-back',
    run: async (ctx) => {
      if (desktop()) return
      await openNote(ctx)
      await back(ctx.page)
      await ctx.settle()
    },
  },
  {
    name: 'note-back-button',
    run: async (ctx) => {
      if (desktop()) return
      await openNote(ctx)
      await press(ctx.page.getByRole('button', { name: 'Back to Notes' }))
      await ctx.settle()
    },
  },
  {
    name: 'note-search-hit',
    run: async (ctx) => {
      // A result opened as a note is: the search stays behind it.
      await search(ctx, 'sour')
      await press(ctx.page.locator('.side-row.hit').first())
      await ctx.settle(600)
    },
  },
  {
    name: 'note-long',
    run: async (ctx) => {
      // The tail of a page: what is under the text, and the end of the scroll.
      await openNote(ctx, 'Sourdough, the version that works')
      await ctx.page.evaluate(() => {
        for (const el of document.querySelectorAll('.scroll')) el.scrollTop = el.scrollHeight
      })
      await ctx.settle()
    },
  },

  // Writing: the keyboard up, the toolbar and the title still in reach.
  {
    name: 'editor-keyboard',
    run: async (ctx) => {
      await openNote(ctx)
      await raiseKeyboard(ctx)
      await typeInto(ctx, ' Order the new jib sheet.')
    },
  },
  {
    name: 'editor-title-keyboard',
    run: async (ctx) => {
      await openNote(ctx)
      await raiseKeyboard(ctx)
      await ctx.page.locator('input.title, textarea.title').first().click()
      await ctx.settle()
    },
  },
  {
    name: 'editor-toolbar-end',
    run: async (ctx) => {
      await openNote(ctx)
      await ctx.page.evaluate(() => {
        const bar = document.querySelector('[role=toolbar]')
        if (bar) bar.scrollLeft = bar.scrollWidth
      })
      await ctx.settle()
    },
  },
  {
    name: 'editor-format',
    run: async (ctx) => {
      // The toolbar's press must not take the keyboard with it: bold, then
      // the word typed after it.
      await openNote(ctx)
      await typeInto(ctx, ' ')
      await press(ctx.page.getByRole('button', { name: 'Bold' }))
      await ctx.page.keyboard.type('bold words')
      await ctx.settle()
    },
  },
  {
    name: 'editor-link',
    run: async (ctx) => {
      await openNote(ctx)
      await press(ctx.page.getByRole('button', { name: 'Link', exact: true }))
      await ctx.settle()
    },
  },

  // What is around the text: tags, what it is for, the offers.
  {
    name: 'tag-add',
    run: async (ctx) => {
      await openNote(ctx)
      await press(ctx.page.getByRole('button', { name: 'Tag', exact: true }))
      await ctx.settle()
    },
  },
  {
    name: 'tag-add-keyboard',
    run: async (ctx) => {
      await openNote(ctx)
      await press(ctx.page.getByRole('button', { name: 'Tag', exact: true }))
      await ctx.settle()
      await raiseKeyboard(ctx)
    },
  },
  {
    name: 'purpose',
    run: async (ctx) => {
      await openNote(ctx)
      await press(ctx.page.getByRole('button', { name: /Not filed|Filed under/ }).first())
      await ctx.settle()
    },
  },

  // A new note, blank, and the empty state of a note's tail.
  {
    name: 'new',
    run: async (ctx) => {
      await press(ctx.page.getByRole('button', { name: 'New note', exact: true }).first())
      await ctx.settle(700)
    },
  },
  {
    name: 'new-keyboard',
    run: async (ctx) => {
      await press(ctx.page.getByRole('button', { name: 'New note', exact: true }).first())
      await ctx.settle(700)
      await raiseKeyboard(ctx)
      await typeInto(ctx, 'Things to remember')
    },
  },
  {
    name: 'new-back',
    run: async (ctx) => {
      if (desktop()) return
      await press(ctx.page.getByRole('button', { name: 'New note', exact: true }).first())
      await ctx.settle(700)
      await back(ctx.page)
      await ctx.settle()
    },
  },

  // The note's dialogs.
  {
    name: 'delete-asks',
    run: async (ctx) => {
      await openNote(ctx)
      await press(ctx.page.getByRole('button', { name: /^Delete( this note)?$/ }))
      await ctx.settle()
    },
  },
  {
    name: 'delete-back',
    run: async (ctx) => {
      // Confirmed: the note is gone and the list is what is left.
      await openNote(ctx)
      await press(ctx.page.getByRole('button', { name: /^Delete( this note)?$/ }))
      await ctx.settle()
      await press(ctx.page.getByRole('button', { name: 'Delete note' }))
      await ctx.settle(600)
    },
  },

  // A list row's long-press menu (Android's `contextmenu`).
  {
    name: 'row-menu',
    run: async ({ page, settle }) => {
      await page
        .getByText(BOAT, { exact: true })
        .first()
        .dispatchEvent('contextmenu', { clientX: 120, clientY: 220 })
      await settle()
    },
  },

  // What the assistant proposed: a note, drawn at the top of the list.
  {
    name: 'proposal',
    run: async ({ page, settle }) => {
      await press(page.getByText('Packing day, the short version').first())
      await settle(600)
    },
  },
]

/** The keyboard (and typing at the page) is a phone's: a desktop run shows the app's home for those. */
const withKeyboard = (scenario) =>
  /keyboard|format/.test(scenario.name)
    ? { ...scenario, run: async (ctx) => (desktop() ? undefined : scenario.run(ctx)) }
    : scenario

export default {
  notes: scenarios.map(withKeyboard),
}
