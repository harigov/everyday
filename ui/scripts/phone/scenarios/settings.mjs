// Settings on a phone: the list of pages, then each page in turn, the pages'
// own sub-tabs, and back from a page to the list. See ../scenarios.mjs.
//
// These work on a desktop run too (`--desktop`, which clicks): there Settings
// is opened with the app bar's button and every page sits beside the list, so
// the screenshots are what `compare.py` holds still while the phone's rules
// change.

import { back, press } from '../helpers.mjs'

const desktop = () => globalThis.PHONE_CHECK_DESKTOP === true

/** Settings, on its list of pages. */
async function openSettings({ page, settle }) {
  if (desktop()) {
    await press(page.locator('.foot .barbtn[title^="Settings"]'))
  } else {
    await press(page.getByRole('button', { name: 'More', exact: true }))
    await settle()
    await press(page.getByRole('button', { name: 'Settings' }))
  }
  await settle()
}

/** A page of Settings by the name on its row. */
async function openPage(ctx, label) {
  await openSettings(ctx)
  const row = ctx.page
    .locator('nav[aria-label="Settings sections"] .tab')
    .filter({ has: ctx.page.locator('.side-text', { hasText: new RegExp(`^${label}$`) }) })
  await press(row.first())
  await ctx.settle()
}

/** A sub-tab of the open page. */
async function openSub(ctx, label) {
  const tab = ctx.page.getByRole('tab', { name: label, exact: true })
  // Not every sub-tab is offered everywhere: Sharing is a computer's.
  if ((await tab.count()) === 0) return
  await press(tab.first())
  await ctx.settle()
}

/** [name, row label, sub-tabs worth a screen of their own] */
const PAGES = [
  ['general', 'General'],
  ['profile', 'About You', ['Areas']],
  ['models', 'Models', ['Decision', 'Speech', 'Voice', 'Image']],
  ['ai', 'AI features', ['In the apps']],
  ['sorting', 'Mail sorting', ['Follow-ups', 'Older mail']],
  ['writing', 'Mail writing', ['Checks before sending', 'What it has learned']],
  ['connections', 'Connections'],
  ['backup', 'Backup', ['When', 'Copies']],
  ['data', 'Data'],
  ['vault', 'Vault', ['Sharing', 'About']],
  ['accounts', 'Accounts'],
  ['assistant', 'Assistant', ['Behaviour', 'Web and computer', 'Your day', 'Memory and mail']],
  ['routines', 'Routines'],
  ['briefs', 'Briefs', ['Evening brief', 'Weekly brief', 'Daily podcast']],
  ['runs', 'What it did'],
  ['memory', 'Memory'],
  ['skills', 'Skills'],
  ['proposals', 'Waiting for you'],
  ['chat', 'Chat'],
  ['money', 'Money'],
  ['screentime', 'Screen time'],
  ['files', 'Files'],
  ['feed', 'Feed'],
  ['health', 'Health'],
  ['meetings', 'Meetings', ['Calls', 'Voices']],
  ['templates', 'Templates'],
  ['keepintouch', 'Keeping in touch'],
]

const scenarios = [
  // Not a scenario on a desktop, where the list and a page are one screen.
  { name: 'list', section: 'overview', run: openSettings },
]
for (const [name, label, subs = []] of PAGES) {
  scenarios.push({
    name,
    section: 'overview',
    run: (ctx) => openPage(ctx, label),
  })
  for (const sub of subs) {
    scenarios.push({
      name: `${name}-${sub.toLowerCase().replace(/[^a-z]+/g, '-')}`,
      section: 'overview',
      run: async (ctx) => {
        await openPage(ctx, label)
        await openSub(ctx, sub)
      },
    })
  }
}

/** Something on a page, pressed: the sheets and forms a page raises. */
const onPage = (name, label, steps) =>
  scenarios.push({
    name,
    section: 'overview',
    run: async (ctx) => {
      await openPage(ctx, label)
      await steps(ctx)
    },
  })

onPage('accounts-menu', 'Accounts', async ({ page, settle }) => {
  await press(page.locator('button.more').first())
  await settle()
})
onPage('accounts-detail', 'Accounts', async ({ page, settle }) => {
  await press(page.locator('.rows .row .main').first())
  await settle()
})
onPage('accounts-add', 'Accounts', async ({ page, settle }) => {
  await press(page.getByRole('button', { name: 'Add account' }))
  await settle()
})
onPage('connections-add', 'Connections', async ({ page, settle }) => {
  await press(page.getByRole('button', { name: 'Add connection' }))
  await settle()
})
onPage('connections-detail', 'Connections', async ({ page, settle }) => {
  await press(page.locator('.conn .rows button').first())
  await settle()
})
onPage('routines-new', 'Routines', async ({ page, settle }) => {
  await press(page.getByRole('button', { name: 'New routine' }))
  await settle()
})
onPage('memory-add', 'Memory', async ({ page, settle }) => {
  await press(page.getByRole('button', { name: 'Add a fact' }))
  await settle()
})
onPage('skills-new', 'Skills', async ({ page, settle }) => {
  await press(page.getByRole('button', { name: 'New skill' }))
  await settle()
})
onPage('templates-edit', 'Templates', async ({ page, settle }) => {
  await press(page.getByRole('button', { name: 'Edit', exact: true }).first())
  await settle()
})
onPage('chat-add', 'Chat', async ({ page, settle }) => {
  await press(page.getByRole('button', { name: 'Add a Matrix bot' }))
  await settle()
})
onPage('about-you-area-menu', 'About You', async ({ page, settle }) => {
  await openSub({ page, settle }, 'Areas')
  await press(page.locator('button.more').first())
  await settle()
})

scenarios.push(
  {
    // A journal's own settings, from the drawer.
    name: 'journal-settings',
    section: 'journal',
    run: async ({ page, settle }) => {
      await press(
        page.locator(
          'button[aria-label="Show the sidebar"], button[aria-label="Hide the sidebar"]',
        ),
      )
      await settle()
      await press(page.locator('button.cog').first())
      await settle()
    },
  },
  {
    // A followed source's settings, from its menu in the drawer.
    name: 'feed-source-settings',
    section: 'feed',
    run: async ({ page, settle }) => {
      await press(
        page.locator(
          'button[aria-label="Show the sidebar"], button[aria-label="Hide the sidebar"]',
        ),
      )
      await settle()
      const row = await page.evaluate(() => {
        const rows = [...document.querySelectorAll('nav button, nav [role=button]')]
        const el = rows.find(
          (e) =>
            e.textContent.trim().length > 2 &&
            e.getAttribute('class')?.includes('side-row') &&
            e.querySelector('img, svg, .side-icon') &&
            !/All|Up Next|Recommended|History|Add|Find|Directory/i.test(e.textContent),
        )
        if (!el) return null
        el.setAttribute('data-probe-row', '1')
        return el.textContent.trim()
      })
      if (!row) return
      await page.evaluate(() => {
        const el = document.querySelector('[data-probe-row]')
        const r = el.getBoundingClientRect()
        el.dispatchEvent(
          new MouseEvent('contextmenu', {
            bubbles: true,
            cancelable: true,
            clientX: r.left + 80,
            clientY: r.top + 10,
          }),
        )
      })
      await settle()
      await press(page.getByRole('menuitem', { name: /^Settings/ }))
      await settle()
    },
  },
  {
    // A page opened, then back: the list again.
    name: 'page-back',
    section: 'overview',
    run: async (ctx) => {
      await openPage(ctx, 'Models')
      if (!desktop()) {
        await back(ctx.page)
        await ctx.settle()
      }
    },
  },
  {
    // Back from the list closes Settings and shows the app it covered.
    name: 'list-back',
    section: 'todo',
    run: async (ctx) => {
      await openSettings(ctx)
      if (!desktop()) {
        await back(ctx.page)
        await ctx.settle()
      }
    },
  },
)

export default { settings: scenarios }
