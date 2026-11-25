// The feed app on a phone: the list of stories, a story opened over it (an
// article, an episode, one from Hacker News) and back out of each layer, the
// filters, the sidebar's drawer, Up Next, Recommended, the dialogs that follow
// something and browse podcasts, an episode playing, and the empty states.
// See ../scenarios.mjs.
//
// The mock follows Longform Weekly, Shipyard Notes, Hacker News, The Old
// Radio Blog, The Slow Hour (a podcast), Frame by Frame, The Download, The
// Algorithm and TLDR. Its clock is stopped at Wednesday 25 November, 10:30.
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

/** A row of the sidebar: in the drawer on a phone, beside the list on a desktop. */
const chooseRow = (name) => async (ctx) => {
  if (!desktop()) await openDrawer(ctx)
  await press(ctx.page.locator('.side-row', { hasText: name }).first())
  await ctx.settle(500)
}

/** The sidebar's add and browse buttons. */
const sideButton = (label) => async (ctx) => {
  if (!desktop()) await openDrawer(ctx)
  await press(ctx.page.getByRole('button', { name: label }))
  await ctx.settle(600)
}

/** A tab of the list, or -- when a dialog is open -- of the dialog (the drawer
 *  has tabs of its own with the same words, which a dialog's scrim covers). */
const tab = (name) => async (ctx) => {
  const scope = ctx.page.locator('[role=dialog]')
  const within = (await scope.count()) > 0 ? scope : ctx.page
  await press(within.getByRole('tab', { name, exact: true }).first())
  await ctx.settle()
}

/** The first story row whose text has `title`. */
const openStory = (title) => async (ctx) => {
  await press(ctx.page.locator('button.row', { hasText: title }).first())
  await ctx.settle(700)
}

const scrollStoryToEnd = async ({ page, settle }) => {
  await page.evaluate(() => {
    for (const el of document.querySelectorAll('article.story .scroll'))
      el.scrollTop = el.scrollHeight
  })
  await settle()
}

const quick = (run) => async (ctx) => {
  // A scenario that cannot find its control fails in seconds, not half a minute.
  ctx.page.setDefaultTimeout(8000)
  // Which tab of the list was last used is kept in the browser, and an earlier
  // scenario left it where it liked.
  const unread = ctx.page.getByRole('tab', { name: 'Unread', exact: true }).first()
  if (run && (await unread.count())) {
    await press(unread)
    await ctx.settle()
  }
  await run?.(ctx)
}

const scenarios = [
  { name: 'home' },
  { name: 'home-all', run: tab('All') },
  { name: 'home-saved', run: tab('Saved') },
  {
    name: 'home-end',
    run: async ({ page, settle }) => {
      await page.evaluate(() => {
        for (const el of document.querySelectorAll('.story-rows')) el.scrollTop = el.scrollHeight
      })
      await settle()
    },
  },
  {
    name: 'drawer',
    run: async (ctx) => {
      if (desktop()) return
      await openDrawer(ctx)
    },
  },

  // A story opened over the list, and back.
  { name: 'story', run: openStory('last lighthouse keepers') },
  {
    name: 'story-back',
    run: async (ctx) => {
      await openStory('last lighthouse keepers')(ctx)
      if (desktop()) return
      await back(ctx.page)
      await ctx.settle()
    },
  },
  {
    name: 'story-end',
    run: async (ctx) => {
      await openStory('last lighthouse keepers')(ctx)
      await scrollStoryToEnd(ctx)
    },
  },
  {
    name: 'story-more',
    run: async (ctx) => {
      await openStory('last lighthouse keepers')(ctx)
      if (desktop()) return
      await press(ctx.page.locator('.phone-bar').getByRole('button', { name: 'More' }))
      await ctx.settle()
    },
  },
  {
    name: 'story-summary',
    run: async (ctx) => {
      await openStory('last lighthouse keepers')(ctx)
      await tab('Summary')(ctx)
    },
  },

  // An episode: its head, then getting it and playing it.
  { name: 'episode', run: openStory('Episode 41') },
  {
    name: 'episode-tabs',
    run: async (ctx) => {
      await openStory('Episode 41')(ctx)
      await tab('Transcript')(ctx)
    },
  },
  {
    name: 'episode-playing',
    run: async (ctx) => {
      await openStory('Episode 41')(ctx)
      await press(
        ctx.page.locator('article.story').getByRole('button', { name: /^Play$|^Resume$/ }),
      )
      await ctx.settle(900)
    },
  },
  {
    name: 'player',
    run: async (ctx) => {
      await openStory('Episode 41')(ctx)
      await press(
        ctx.page.locator('article.story').getByRole('button', { name: /^Play$|^Resume$/ }),
      )
      await ctx.settle(900)
      await press(ctx.page.getByRole('button', { name: /^Open the player/ }))
      await ctx.settle(600)
    },
  },
  {
    name: 'player-back',
    run: async (ctx) => {
      await openStory('Episode 41')(ctx)
      await press(
        ctx.page.locator('article.story').getByRole('button', { name: /^Play$|^Resume$/ }),
      )
      await ctx.settle(900)
      await press(ctx.page.getByRole('button', { name: /^Open the player/ }))
      await ctx.settle(600)
      if (desktop()) return
      await back(ctx.page)
      await ctx.settle()
    },
  },
  {
    name: 'list-playing',
    run: async (ctx) => {
      await tab('All')(ctx)
      await press(ctx.page.getByRole('button', { name: /^Play$/ }).first())
      await ctx.settle(900)
    },
  },

  // A source of its own, with the points and sort of Hacker News.
  { name: 'source-hn', run: chooseRow('Hacker News') },
  {
    name: 'source-hn-story',
    run: async (ctx) => {
      await chooseRow('Hacker News')(ctx)
      await openStory('pocket-sized weather station')(ctx)
    },
  },
  { name: 'source-podcast', run: chooseRow('The Slow Hour') },
  { name: 'source-folder', run: chooseRow('Engineering') },

  // The pages that are not a list of stories.
  { name: 'upnext', run: chooseRow('Up Next') },
  { name: 'recommended', run: chooseRow('Recommended') },
  {
    name: 'recommended-story',
    run: async (ctx) => {
      await chooseRow('Recommended')(ctx)
      await press(ctx.page.locator('button.row').first())
      await ctx.settle(700)
    },
  },
  { name: 'history', run: chooseRow('History') },

  // Following something, and browsing for it.
  { name: 'add', run: sideButton('Follow something') },
  {
    name: 'add-hn',
    run: async (ctx) => {
      await sideButton('Follow something')(ctx)
      await tab('Hacker News')(ctx)
    },
  },
  {
    name: 'add-mail',
    run: async (ctx) => {
      await sideButton('Follow something')(ctx)
      await tab('From your mail')(ctx)
    },
  },
  {
    name: 'add-opml',
    run: async (ctx) => {
      await sideButton('Follow something')(ctx)
      await tab('Import OPML')(ctx)
    },
  },
  {
    name: 'add-found',
    run: async (ctx) => {
      await sideButton('Follow something')(ctx)
      const field = ctx.page.getByRole('textbox', { name: 'Address' })
      await field.fill('https://example.com')
      await press(ctx.page.getByRole('button', { name: 'Find feeds' }))
      await ctx.settle(900)
    },
  },
  {
    name: 'add-back',
    run: async (ctx) => {
      await sideButton('Follow something')(ctx)
      if (desktop()) return
      await back(ctx.page)
      await ctx.settle()
    },
  },
  { name: 'directory', run: sideButton('Browse podcasts and feeds') },
  {
    name: 'directory-search',
    run: async (ctx) => {
      await sideButton('Browse podcasts and feeds')(ctx)
      await ctx.page.getByRole('searchbox', { name: 'Search podcasts' }).fill('radio')
      await ctx.settle(1200)
    },
  },
  {
    name: 'directory-feeds',
    run: async (ctx) => {
      await sideButton('Browse podcasts and feeds')(ctx)
      await tab('Feeds')(ctx)
      await ctx.page.getByRole('searchbox', { name: /^Search feeds/ }).fill('weather')
      await ctx.settle(1200)
    },
  },

  // Nothing to show.
  {
    name: 'empty-search',
    run: async ({ page, settle }) => {
      const bar = page.getByRole('combobox', { name: /^Search the Feed/ })
      await bar.fill('zzzzqx')
      await bar.press('Enter')
      await settle(900)
    },
  },
  {
    name: 'caught-up',
    run: async ({ page, settle }) => {
      await press(page.getByRole('button', { name: 'Mark all read' }))
      await settle()
    },
  },
]

export default { feed: scenarios.map((s) => ({ name: s.name, run: quick(s.run) })) }
