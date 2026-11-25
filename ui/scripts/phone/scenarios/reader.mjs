// The document reader on a phone: a book opened from the library (an item
// holding an EPUB: Read), its contents and its search as pages over it, a
// section chosen, and back out of each layer. The reader has no app of its
// own, so these are filed under the library, which is where they are reached
// from. See ../scenarios.mjs.
//
// The mock's one EPUB is *The Hound of the Baskervilles*, four chapters and
// a page of notes; there is a Markdown file and a PDF as well.
//
// A screen only a phone has -- back out of a layer -- is skipped in a
// `--desktop` run (see `check.mjs`): the desktop is compared screen for
// screen, and a scenario that cannot reach the screen shows the app's home
// instead.

import { back, press } from '../helpers.mjs'

const desktop = () => globalThis.PHONE_CHECK_DESKTOP === true

/** A book with a file on it: the library's card, then "Attach a file…" and the EPUB. */
const withBook = async ({ page, settle }, find = 'Hound', row = 'The Hound of the') => {
  await press(page.getByRole('button', { name: 'Covers', exact: true }))
  await settle()
  await press(page.getByRole('button', { name: 'Pnin', exact: true }).first())
  await settle(500)
  await press(page.getByRole('button', { name: /Attach a file/ }))
  await settle(700)
  await page.getByPlaceholder('Find a document by name').fill(find)
  await settle(700)
  await press(page.getByText(row).first())
  await press(page.getByRole('button', { name: 'Attach', exact: true }))
  await settle(700)
}

const read = async (ctx, ...book) => {
  await withBook(ctx, ...book)
  await press(ctx.page.locator('.reading .btn-primary'))
  await ctx.settle(1500)
}

/** A pane of the reader open, whichever it was left at. */
const openPane = (name) => async (ctx) => {
  const button = ctx.page.getByRole('button', { name })
  if ((await button.getAttribute('aria-pressed')) !== 'true') await press(button)
  await ctx.settle(600)
}

const contents = async (ctx) => {
  await read(ctx)
  await openPane('Contents')(ctx)
}

const find = async (ctx) => {
  await read(ctx)
  await openPane('Search in the document')(ctx)
  await ctx.page.getByRole('searchbox', { name: 'Search this document' }).fill('mortimer')
  await ctx.settle(1500)
}

const quick = (run) => async (ctx) => {
  // A scenario that cannot find its control fails in seconds, not half a minute.
  ctx.page.setDefaultTimeout(8000)
  await run(ctx)
}

const scenarios = [
  { name: 'item-with-book', run: withBook },
  { name: 'reader', run: read },
  {
    name: 'reader-scrolled',
    run: async (ctx) => {
      await read(ctx)
      await ctx.page.evaluate(() => {
        const flow = document.querySelector('[data-flow]')
        if (flow) flow.scrollTop = 1400
      })
      await ctx.settle(900)
    },
  },
  {
    name: 'reader-pdf',
    run: async (ctx) => {
      await read(ctx, 'spending', 'spending-q3')
      await ctx.settle(1500)
    },
  },
  { name: 'reader-contents', run: contents },
  {
    name: 'reader-section',
    run: async (ctx) => {
      await contents(ctx)
      await press(ctx.page.locator('.side-row[data-section]').nth(2))
      await ctx.settle(1200)
    },
  },
  { name: 'reader-search', run: find },
  {
    name: 'reader-hit',
    run: async (ctx) => {
      await find(ctx)
      await press(ctx.page.locator('button.hit').first())
      await ctx.settle(1200)
    },
  },
  {
    name: 'reader-back',
    run: async (ctx) => {
      await contents(ctx)
      if (desktop()) return
      // The contents first, then the document, then the book it came from.
      await back(ctx.page)
      await ctx.settle()
    },
  },
  {
    name: 'reader-back-out',
    run: async (ctx) => {
      await read(ctx)
      if (desktop()) return
      await back(ctx.page)
      await ctx.settle(600)
    },
  },
]

export default { library: scenarios.map((s) => ({ ...s, run: quick(s.run) })) }
