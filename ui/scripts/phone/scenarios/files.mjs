// The files app on a phone: the folders and files as tiles and as rows, a
// folder opened and the way back up, the options a toolbar keeps behind a
// menu, a file in the viewer and its information, the inspector as a page,
// picking files and what is done to the picked, the files on their way in,
// every place the sidebar lists, Photos, its facets and a picture in the
// viewer, Albums, and the empty states. See ../scenarios.mjs.
//
// What the files app remembers in the browser (the view, the grid or the
// rows) outlives a scenario, so every one starts by putting both back
// (`fresh`).
//
// A screen only a phone has -- the picking circle on a tile, the options
// menu, the page the inspector becomes -- is skipped in a `--desktop` run
// (see `check.mjs`): the desktop is compared screen for screen, and a
// scenario that cannot reach the screen shows the app's home instead.

import { back, press } from '../helpers.mjs'

const desktop = () => globalThis.PHONE_CHECK_DESKTOP === true

/** The top of the files folder, as tiles: what the app opens on first. The
 *  view and the layout are kept in the browser, which a scenario before this
 *  one may have changed. */
const fresh = async ({ page }) => {
  await page.evaluate(async () => {
    const { files } = await import('/src/lib/files.svelte.ts')
    files.setLayout('grid')
    files.openFolder('')
  })
}

const openDrawer = async ({ page, settle }) => {
  await press(page.getByRole('button', { name: 'Show the sidebar' }))
  await settle()
}

/** One of the places the sidebar lists, by name: through the drawer on a
 *  phone. */
const goTo = (name) => async (ctx) => {
  if (!desktop()) await openDrawer(ctx)
  await press(
    ctx.page
      .locator('nav[aria-label="Files"] > .side-row')
      .filter({ hasText: new RegExp(`^\\s*${name}`) })
      .first(),
  )
  await ctx.settle()
}

/** Open a folder from the list, by name. */
const openFolder = (name) => async (ctx) => {
  await press(ctx.page.getByRole('option', { name: new RegExp(`^${name}\\b`) }).first())
  await ctx.settle()
}

/** The folders on the way to a folder, from the top. */
const openPath =
  (...names) =>
  async (ctx) => {
    for (const name of names) await openFolder(name)(ctx)
  }

const fileTile = (page, name) => page.getByRole('option', { name: new RegExp(name) }).first()

/** A file opened: on a desktop a click and Enter (a double-click lands on
 *  whatever the rail that the first click opens has moved), on a phone a
 *  tap. */
const openFile = (name) => async (ctx) => {
  const tile = fileTile(ctx.page, name)
  if (desktop()) {
    await tile.click()
    await ctx.page.keyboard.press('Enter')
  } else {
    await press(tile)
  }
  await ctx.settle(600)
}

/** A picture in the timeline opened, the same way. */
const openPhoto = (n) => async (ctx) => {
  const hit = ctx.page.locator('[data-tile] .hit').nth(n)
  if (desktop()) {
    await hit.click()
    await ctx.page.keyboard.press('Enter')
  } else {
    await press(hit)
  }
  await ctx.settle(600)
}

/** The bar of what can be done to the picked files. */
const selectionBar = (page) => page.getByRole('toolbar', { name: 'Selected files' })

/** A long press: Android sends `contextmenu` at the finger. */
async function longPress(page, selector) {
  await page.evaluate((sel) => {
    const el = document.querySelector(sel)
    const r = el.getBoundingClientRect()
    el.dispatchEvent(
      new MouseEvent('contextmenu', {
        bubbles: true,
        cancelable: true,
        clientX: r.left + Math.min(r.width / 2, 60),
        clientY: r.top + Math.min(r.height / 2, 40),
      }),
    )
  }, selector)
}

/** An item of the menu that is open, by its words. */
const menuItem = (page, name) =>
  page.locator('[role=menu] [role^=menuitem]').filter({ hasText: name }).first()

/** A file picked, to act on: a click on a desktop, the picking circle on a
 *  phone. */
const pickFile = (name) => async (ctx) => {
  if (desktop()) await fileTile(ctx.page, name).click()
  else {
    const circle = ctx.page.getByRole('button', { name: `Select ${name}` }).first()
    // WebKit's own scroll for a tap does not reach into the list's scroller
    // when the tile is below the fold; a finger would simply scroll.
    await circle.scrollIntoViewIfNeeded()
    await press(circle)
  }
  await ctx.settle()
}

/** The toolbar's options, which a desktop has in the bar itself. */
const openOptions = async ({ page, settle }) => {
  await press(page.getByRole('button', { name: 'Options' }))
  await settle()
}

const setLayout = (layout) => async (ctx) => {
  if (desktop()) {
    await press(ctx.page.getByRole('button', { name: layout, exact: true }))
  } else {
    await openOptions(ctx)
    await press(menuItem(ctx.page, new RegExp(`^\\s*${layout}\\s*$`)))
  }
  await ctx.settle()
}

/** A long folder of pictures. */
const YOSEMITE = ['Photos', '2024', 'Yosemite']

/** The pictures the selection scenarios pick: a desktop picks from the last
 *  row, a phone from the second, which is on screen without scrolling (a
 *  finger scrolls to the last; a script's tap on the list's scroller, in
 *  WebKit, does not always). */
const FIRST = () => (desktop() ? 'DSC00112' : 'DSC00233')
const SECOND = () => (desktop() ? 'DSC00131' : 'DSC00204')

/** Start again somewhere the mock has other files: the catalog the way an
 *  unset or an unreachable folder leaves it. */
async function reopen(ctx, params) {
  const url = new URL(ctx.page.url())
  for (const [k, v] of Object.entries(params)) url.searchParams.set(k, v)
  await ctx.page.goto(url.toString())
  const password = ctx.page.locator('input[type=password]')
  await password.waitFor({ timeout: 15000 })
  await password.fill('karmayoga')
  await password.press('Enter')
  await ctx.page.locator('.shell').waitFor({ timeout: 15000 })
  await ctx.settle(600)
}

/** Add files from the page's own picker, as a phone does: the vault is not
 *  on the phone, so they upload. */
async function addFiles({ page, settle }, names) {
  const chooser = page.waitForEvent('filechooser')
  await press(page.getByRole('button', { name: /^Add$/ }).first())
  const picked = await chooser
  await picked.setFiles(
    names.map((name, i) => ({
      name,
      mimeType: 'application/pdf',
      buffer: Buffer.alloc(40_000 + i * 15_000, 1),
    })),
  )
  await settle(800)
}

/** Type into the top bar's field, as a person with a keyboard up would. */
async function search({ page, settle }, text) {
  await press(page.locator('[data-search]').first())
  await page.keyboard.type(text)
  await settle(700)
}

const scenarios = [
  // The folder as it opens: its folders as cards.
  { name: 'home' },
  // A folder with folders and one with files, and the way back up.
  { name: 'folder', run: openFolder('Documents') },
  { name: 'folder-pictures', run: openPath(...YOSEMITE) },
  {
    name: 'folder-up',
    run: async (ctx) => {
      await openPath('Documents', 'Taxes')(ctx)
      if (desktop()) return
      await press(ctx.page.getByRole('button', { name: /^Back to/ }))
      await ctx.settle()
    },
  },
  {
    name: 'folder-empty',
    run: openPath('Documents', 'Scans'),
  },

  // What the toolbar keeps behind a menu, and the questions it asks.
  {
    name: 'options',
    run: async (ctx) => {
      if (desktop()) return
      await openOptions(ctx)
    },
  },
  {
    name: 'options-back',
    run: async (ctx) => {
      if (desktop()) return
      await openOptions(ctx)
      await back(ctx.page)
      await ctx.settle()
    },
  },
  {
    name: 'new-folder',
    run: async (ctx) => {
      if (desktop()) {
        await press(ctx.page.getByRole('button', { name: 'New folder' }))
      } else {
        await openOptions(ctx)
        await press(menuItem(ctx.page, 'New folder'))
      }
      await ctx.settle()
    },
  },
  {
    name: 'new-folder-back',
    run: async (ctx) => {
      if (desktop()) return
      await openOptions(ctx)
      await press(menuItem(ctx.page, 'New folder'))
      await ctx.settle()
      await back(ctx.page)
      await ctx.settle()
    },
  },

  // A file in the viewer, with its information, and back to the list.
  {
    name: 'viewer',
    run: async (ctx) => {
      await openPath(...YOSEMITE)(ctx)
      await openFile('DSC00112')(ctx)
    },
  },
  {
    name: 'viewer-info',
    run: async (ctx) => {
      await openPath(...YOSEMITE)(ctx)
      await openFile('DSC00112')(ctx)
      await press(ctx.page.getByRole('button', { name: 'Information' }))
      await ctx.settle(600)
    },
  },
  {
    name: 'viewer-info-back',
    run: async (ctx) => {
      if (desktop()) return
      await openPath(...YOSEMITE)(ctx)
      await openFile('DSC00112')(ctx)
      await press(ctx.page.getByRole('button', { name: 'Information' }))
      await ctx.settle(600)
      await back(ctx.page)
      await ctx.settle()
    },
  },
  {
    name: 'viewer-next',
    run: async (ctx) => {
      await openPath(...YOSEMITE)(ctx)
      await openFile('DSC00158')(ctx)
      await press(ctx.page.getByRole('button', { name: 'Next' }))
      await ctx.settle(600)
    },
  },
  {
    name: 'viewer-text',
    run: async (ctx) => {
      await openPath('Documents', 'Notes')(ctx)
      await openFile('reading-list')(ctx)
    },
  },
  {
    name: 'viewer-back',
    run: async (ctx) => {
      if (desktop()) return
      await openPath(...YOSEMITE)(ctx)
      await openFile('DSC00112')(ctx)
      await back(ctx.page)
      await ctx.settle()
    },
  },
  {
    name: 'viewer-recording',
    run: async (ctx) => {
      await openPath('Documents', 'Voice memos')(ctx)
      await openFile('ideas')(ctx)
    },
  },

  // Picking files, and what is done to the picked.
  {
    name: 'selection',
    run: async (ctx) => {
      await openPath(...YOSEMITE)(ctx)
      await pickFile(FIRST())(ctx)
      await pickFile(SECOND())(ctx)
    },
  },
  {
    name: 'selection-one',
    run: async (ctx) => {
      await openPath(...YOSEMITE)(ctx)
      await pickFile(FIRST())(ctx)
    },
  },
  {
    name: 'selection-more',
    run: async (ctx) => {
      if (desktop()) return
      await openPath(...YOSEMITE)(ctx)
      await pickFile(FIRST())(ctx)
      await press(selectionBar(ctx.page).getByRole('button', { name: 'More', exact: true }))
      await ctx.settle()
    },
  },
  {
    name: 'selection-move',
    run: async (ctx) => {
      await openPath(...YOSEMITE)(ctx)
      await pickFile(FIRST())(ctx)
      await pickFile(SECOND())(ctx)
      await press(ctx.page.getByRole('button', { name: 'Move', exact: true }))
      await ctx.settle(600)
    },
  },
  {
    name: 'selection-tag',
    run: async (ctx) => {
      if (desktop()) return
      await openPath(...YOSEMITE)(ctx)
      await pickFile(FIRST())(ctx)
      await press(selectionBar(ctx.page).getByRole('button', { name: 'More', exact: true }))
      await ctx.settle()
      await press(menuItem(ctx.page, /Tag/))
      await ctx.settle()
    },
  },
  {
    name: 'selection-clear',
    run: async (ctx) => {
      await openPath(...YOSEMITE)(ctx)
      await pickFile(FIRST())(ctx)
      await press(ctx.page.getByRole('button', { name: 'Clear', exact: true }))
      await ctx.settle()
    },
  },
  // One file's details, as a page of their own.
  {
    name: 'inspector',
    run: async (ctx) => {
      await openPath('Documents', 'Taxes')(ctx)
      await pickFile('2025-return')(ctx)
      if (!desktop()) {
        await press(ctx.page.getByRole('button', { name: 'Details', exact: true }))
        await ctx.settle(600)
      }
    },
  },
  {
    name: 'inspector-picture',
    run: async (ctx) => {
      await openPath(...YOSEMITE)(ctx)
      await pickFile(FIRST())(ctx)
      if (!desktop()) {
        await press(ctx.page.getByRole('button', { name: 'Details', exact: true }))
        await ctx.settle(600)
      }
    },
  },
  {
    name: 'inspector-bottom',
    run: async (ctx) => {
      await openPath(...YOSEMITE)(ctx)
      await pickFile(FIRST())(ctx)
      if (!desktop()) {
        await press(ctx.page.getByRole('button', { name: 'Details', exact: true }))
        await ctx.settle(600)
      }
      await ctx.page.evaluate(() => {
        for (const el of document.querySelectorAll('.scroll')) el.scrollTop = el.scrollHeight
      })
      await ctx.settle()
    },
  },
  {
    name: 'inspector-back',
    run: async (ctx) => {
      if (desktop()) return
      await openPath('Documents', 'Taxes')(ctx)
      await pickFile('2025-return')(ctx)
      await press(ctx.page.getByRole('button', { name: 'Details', exact: true }))
      await ctx.settle(600)
      await back(ctx.page)
      await ctx.settle()
    },
  },

  // The files on their way in: from the page's picker, as a phone sends
  // them. The mock keeps them "sending" long enough to look at.
  {
    name: 'uploads',
    run: async (ctx) => {
      await reopen(ctx, { files: 'remote' })
      await addFiles(ctx, [
        'receipt-nov.pdf',
        'a-rather-long-file-name-for-a-scan-of-the-lease.pdf',
      ])
    },
  },
  {
    name: 'drawer',
    run: async (ctx) => {
      if (desktop()) return
      await openDrawer(ctx)
    },
  },
  {
    name: 'drawer-folder',
    run: async (ctx) => {
      if (desktop()) return
      await openPath('Documents')(ctx)
      await openDrawer(ctx)
    },
  },

  // Every place the sidebar lists.
  { name: 'recent', run: goTo('Recent') },
  { name: 'favourites', run: goTo('Favourites') },
  { name: 'encrypted', run: goTo('Encrypted') },
  { name: 'unused', run: goTo('Unused') },
  { name: 'missing', run: goTo('Missing') },
  { name: 'trash', run: goTo('Trash') },
  {
    name: 'trash-selection',
    run: async (ctx) => {
      await goTo('Trash')(ctx)
      await pickFile('old-draft')(ctx)
    },
  },
  {
    name: 'missing-selection',
    run: async (ctx) => {
      await goTo('Missing')(ctx)
      await pickFile('2022-return')(ctx)
    },
  },

  // Nothing to show.
  {
    name: 'search-empty',
    run: async (ctx) => {
      await search(ctx, 'zzzz nothing')
    },
  },
  {
    name: 'search',
    run: async (ctx) => {
      await openPath('Documents')(ctx)
      await search(ctx, 'return')
    },
  },
  {
    name: 'not-set-up',
    run: async (ctx) => {
      await reopen(ctx, { files: 'unset' })
    },
  },
  {
    name: 'unavailable',
    run: async (ctx) => {
      await reopen(ctx, { files: 'unavailable' })
    },
  },

  // Photos.
  { name: 'photos', run: goTo('Photos') },
  {
    name: 'photos-small',
    run: async (ctx) => {
      await goTo('Photos')(ctx)
      await press(ctx.page.getByRole('button', { name: 'Small', exact: true }))
      await ctx.settle()
      await press(ctx.page.getByRole('button', { name: 'Medium', exact: true }))
      await ctx.settle()
    },
  },
  {
    name: 'photos-large',
    run: async (ctx) => {
      await goTo('Photos')(ctx)
      await press(ctx.page.getByRole('button', { name: 'Large', exact: true }))
      await ctx.settle()
      await press(ctx.page.getByRole('button', { name: 'Medium', exact: true }))
      await ctx.settle()
    },
  },
  {
    name: 'photos-facets',
    run: async (ctx) => {
      await goTo('Photos')(ctx)
      if (desktop()) return
      await press(ctx.page.getByRole('button', { name: /Browse by/ }))
      await ctx.settle()
    },
  },
  {
    name: 'photos-facets-back',
    run: async (ctx) => {
      if (desktop()) return
      await goTo('Photos')(ctx)
      await press(ctx.page.getByRole('button', { name: /Browse by/ }))
      await ctx.settle()
      await back(ctx.page)
      await ctx.settle()
    },
  },
  {
    name: 'photos-facet',
    run: async (ctx) => {
      await goTo('Photos')(ctx)
      if (desktop()) {
        await press(ctx.page.getByRole('button', { name: /^2025/ }).first())
      } else {
        await press(ctx.page.getByRole('button', { name: /Browse by/ }))
        await ctx.settle()
        await press(ctx.page.getByRole('button', { name: /^2025/ }).first())
      }
      await ctx.settle(600)
    },
  },
  {
    name: 'photo',
    run: async (ctx) => {
      await goTo('Photos')(ctx)
      await openPhoto(2)(ctx)
    },
  },
  {
    name: 'photo-next',
    run: async (ctx) => {
      await goTo('Photos')(ctx)
      await openPhoto(2)(ctx)
      await press(ctx.page.getByRole('button', { name: 'Next' }))
      await ctx.settle(600)
    },
  },
  {
    name: 'photo-info',
    run: async (ctx) => {
      await goTo('Photos')(ctx)
      await openPhoto(2)(ctx)
      await press(ctx.page.getByRole('button', { name: 'Information' }))
      await ctx.settle(600)
    },
  },
  {
    name: 'photo-back',
    run: async (ctx) => {
      if (desktop()) return
      await goTo('Photos')(ctx)
      await press(ctx.page.locator('[data-tile] .hit').nth(2))
      await ctx.settle(600)
      await back(ctx.page)
      await ctx.settle()
    },
  },
  {
    name: 'photos-selection',
    run: async (ctx) => {
      await goTo('Photos')(ctx)
      const tiles = ctx.page.locator('[data-tile]')
      if (desktop()) {
        await tiles.nth(1).locator('.hit').click()
        await tiles
          .nth(3)
          .locator('.hit')
          .click({ modifiers: ['Control'] })
      } else {
        await press(tiles.nth(1).getByRole('button', { name: 'Select' }))
        await press(tiles.nth(3).getByRole('button', { name: 'Select' }))
      }
      await ctx.settle()
    },
  },
  {
    name: 'photos-album-picker',
    run: async (ctx) => {
      await goTo('Photos')(ctx)
      const tiles = ctx.page.locator('[data-tile]')
      if (desktop()) {
        await tiles.nth(1).locator('.hit').click()
        await ctx.settle()
        await press(selectionBar(ctx.page).getByRole('button', { name: 'Album', exact: true }))
        await ctx.settle()
        // On a desktop the toolbar's Album button is a menu with "New album…"
        // in it, and the picker is the timeline's.
        const entry = menuItem(ctx.page, /New album/)
        if (await entry.count()) {
          await press(entry)
          await ctx.settle()
        }
      } else {
        // A long press on a picture: its menu's "Add to album…" is the
        // timeline's own picker.
        await longPress(ctx.page, '[data-tile] .hit')
        await ctx.settle()
        await press(menuItem(ctx.page, /Add to album/))
        await ctx.settle()
      }
    },
  },
  {
    name: 'photos-menu',
    run: async (ctx) => {
      if (desktop()) return
      await goTo('Photos')(ctx)
      await longPress(ctx.page, '[data-tile] .hit')
      await ctx.settle()
    },
  },
  {
    name: 'file-menu',
    run: async (ctx) => {
      if (desktop()) return
      await openPath(...YOSEMITE)(ctx)
      await longPress(ctx.page, '[role=option].tile')
      await ctx.settle(600)
    },
  },

  // Albums.
  { name: 'albums', run: goTo('Albums') },
  {
    name: 'album',
    run: async (ctx) => {
      await goTo('Albums')(ctx)
      await press(ctx.page.getByRole('button', { name: /^Kyoto/ }).first())
      await ctx.settle(600)
    },
  },
  {
    name: 'album-back',
    run: async (ctx) => {
      if (desktop()) return
      await goTo('Albums')(ctx)
      await press(ctx.page.getByRole('button', { name: /^Kyoto/ }).first())
      await ctx.settle(600)
      await press(ctx.page.getByRole('button', { name: 'All albums' }))
      await ctx.settle()
    },
  },
  {
    name: 'album-selection',
    run: async (ctx) => {
      await goTo('Albums')(ctx)
      await press(ctx.page.getByRole('button', { name: /^Kyoto/ }).first())
      await ctx.settle(600)
      const tiles = ctx.page.locator('[data-tile]')
      if (desktop()) await tiles.nth(1).locator('.hit').click()
      else await press(tiles.nth(1).getByRole('button', { name: 'Select' }))
      await ctx.settle()
    },
  },
  {
    name: 'album-new',
    run: async (ctx) => {
      await goTo('Albums')(ctx)
      await press(ctx.page.getByRole('button', { name: 'New album' }).first())
      await ctx.settle()
    },
  },

  // Last: these leave the browser showing the rows.
  {
    name: 'list',
    run: async (ctx) => {
      await openPath('Documents')(ctx)
      await setLayout('List')(ctx)
    },
  },
  {
    name: 'list-files',
    run: async (ctx) => {
      await openPath('Documents', 'Taxes')(ctx)
      await setLayout('List')(ctx)
    },
  },
  {
    name: 'list-pictures',
    run: async (ctx) => {
      await openPath(...YOSEMITE)(ctx)
      await setLayout('List')(ctx)
    },
  },
  {
    name: 'list-selection',
    run: async (ctx) => {
      await openPath('Documents', 'Taxes')(ctx)
      await setLayout('List')(ctx)
      await pickFile('2025-return')(ctx)
    },
  },
  {
    name: 'list-open',
    run: async (ctx) => {
      await openPath(...YOSEMITE)(ctx)
      await setLayout('List')(ctx)
      await openFile('DSC00158')(ctx)
      await press(ctx.page.getByRole('dialog').getByRole('button', { name: 'Close', exact: true }))
      await ctx.settle()
    },
  },
]

export default {
  files: scenarios.map((s) => ({
    ...s,
    run: async (ctx) => {
      await fresh(ctx)
      await ctx.settle(300)
      await s.run?.(ctx)
    },
  })),
}
