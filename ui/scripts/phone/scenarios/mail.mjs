// The mail app on a phone: the list that is its home, a thread opened over it
// and back, the writing screens (new message, reply, forward), the pickers
// and menus, the filters, search, Quick cleanup, the drawer's mailboxes and
// the lists of sends, and the empty states. See ../scenarios.mjs.
//
// Each scenario starts on a fresh page, so each one does what it needs to
// get to the screen. Written to run in a `--desktop` check too (the desktop
// baseline compares those screens): a press is a click there, and the one
// phone-only control -- the thread's "back" -- is reached through `back()`.

import { back, press, tap } from '../helpers.mjs'

/** Playwright's own clock (`context.clock`, which `check.mjs` uses to stop
 *  the mock's time) injects a script into every frame the page makes --
 *  the sandboxed frame each message is drawn in too, where the sandbox
 *  refuses it and the console says so. That is the harness, not the page:
 *  this one message is let through to nobody, every other error still
 *  reaches `check.mjs`. */
const SANDBOX_NOISE = /Blocked script execution in 'about:srcdoc'/

function ignoreSandboxNoise(page) {
  for (const listener of page.listeners('console')) {
    page.off('console', listener)
    page.on('console', (message) => {
      if (!SANDBOX_NOISE.test(message.text())) listener(message)
    })
  }
}

/** The first conversation in the list, opened. */
const openThread = async ({ page, settle }) => {
  await press(page.locator('button[data-thread]').first())
  await settle()
}

/** A conversation with a PDF and a picture on it: the Attachments toggle
 *  leaves the one in the Inbox. */
const openWithAttachments = async (ctx) => {
  const { page, settle } = ctx
  await press(page.getByRole('button', { name: 'Attachments', exact: true }))
  await settle()
  await press(page.locator('button[data-thread]').first())
  await settle(600)
}

/** The drawer's sidebar, open -- already there beside the app on a desktop --
 *  with every account folded, whatever an earlier screen left it as. */
const openDrawer = async ({ page, settle }) => {
  const show = page.locator('button[aria-label="Show the sidebar"]')
  if ((await show.count()) > 0) await press(show.first())
  await settle()
  await accounts(page, false)
  await settle()
}

/** Every account in the sidebar unfolded, or folded. */
async function accounts(page, open) {
  const toggles = page.locator('button.account-toggle')
  for (let i = 0; i < (await toggles.count()); i++) {
    const toggle = toggles.nth(i)
    if (((await toggle.getAttribute('aria-expanded')) === 'true') !== open) await press(toggle)
  }
}

/** A row of the sidebar, by its name. */
const mailbox = async ({ page, settle }, name) => {
  await press(
    page
      .locator('.side-row')
      .filter({ hasText: new RegExp(`^\\s*${name}`) })
      .first(),
  )
  await settle()
}

/** What a long press on a row says: the menu of what can be done to it. */
const longPress = async ({ settle }, locator) => {
  const box = await locator.boundingBox()
  await locator.dispatchEvent('contextmenu', {
    clientX: box.x + box.width / 2,
    clientY: box.y + box.height / 2,
    bubbles: true,
    cancelable: true,
  })
  await settle()
}

/** A button in the thread's toolbar (or anywhere), by its name. */
const named = (page, name) => page.getByRole('button', { name, exact: true }).first()

/** An action on the open thread: its button where the toolbar has one, or
 *  else the "More actions" menu a phone's toolbar keeps the rest in. */
const action = async ({ page, settle }, name) => {
  const direct = page.getByRole('button', { name, exact: true })
  if ((await direct.count()) > 0) {
    await press(direct.first())
    return
  }
  await press(page.getByRole('button', { name: 'More actions' }))
  await settle(200)
  await press(page.getByRole('menuitem', { name: new RegExp(`^${name}`) }))
}

/** A chip of the thread's attachments, by what it is called. */
const chip = (page, label) => page.locator(`button[title^="Look at ${label}"]`).first()

const scenarios = [
  { name: 'home' },

  // ── Reading ──────────────────────────────────────────────────────
  { name: 'thread', run: openThread },
  {
    name: 'thread-back',
    run: async (ctx) => {
      await openThread(ctx)
      await back(ctx.page)
      await ctx.settle()
    },
  },
  {
    name: 'thread-folded',
    // A conversation of several messages, with the older ones folded to a
    // line each: one tapped open beside the newest.
    run: async (ctx) => {
      const { page, settle } = ctx
      await press(page.locator('button[data-thread]').filter({ hasText: /\b3\b/ }).first())
      await settle(600)
      await press(page.locator('button.toggle[aria-expanded=false]').first())
      await settle(400)
    },
  },
  {
    name: 'thread-details',
    run: async (ctx) => {
      await openThread(ctx)
      await press(ctx.page.getByRole('button', { name: 'Who this was sent to' }).first())
      await ctx.settle()
    },
  },
  {
    name: 'thread-attachments',
    run: async (ctx) => {
      await openWithAttachments(ctx)
      await ctx.page.locator('.chips').last().scrollIntoViewIfNeeded()
      await ctx.settle()
    },
  },
  {
    name: 'viewer-image',
    run: async (ctx) => {
      await openWithAttachments(ctx)
      await press(chip(ctx.page, 'photo'))
      await ctx.settle(900)
    },
  },
  {
    name: 'viewer-pdf',
    run: async (ctx) => {
      await openWithAttachments(ctx)
      await press(chip(ctx.page, 'report.pdf'))
      await ctx.settle(2500)
    },
  },
  {
    name: 'viewer-back',
    run: async (ctx) => {
      await openWithAttachments(ctx)
      await press(chip(ctx.page, 'photo'))
      await ctx.settle(600)
      await back(ctx.page)
      await ctx.settle()
    },
  },

  // ── Writing ──────────────────────────────────────────────────────
  {
    name: 'compose',
    run: async ({ page, settle }) => {
      await tap(page, 'button[aria-label="Compose"]')
      await settle(700)
    },
  },
  {
    name: 'compose-back',
    run: async ({ page, settle }) => {
      await tap(page, 'button[aria-label="Compose"]')
      await settle(700)
      await back(page)
      await settle()
    },
  },
  {
    name: 'compose-later',
    // Send later, whose menu opens above the footer.
    run: async ({ page, settle }) => {
      await tap(page, 'button[aria-label="Compose"]')
      await settle(700)
      await press(page.getByRole('button', { name: /Send later/ }))
      await settle()
    },
  },
  {
    name: 'reply',
    run: async (ctx) => {
      await openThread(ctx)
      await press(named(ctx.page, 'Reply'))
      await ctx.settle(700)
    },
  },
  {
    name: 'reply-fields',
    // The addresses and subject, folded behind the first line of a reply.
    run: async (ctx) => {
      await openThread(ctx)
      await press(named(ctx.page, 'Reply all'))
      await ctx.settle(700)
      await press(ctx.page.locator('button.draft-to').first())
      await ctx.settle()
    },
  },
  {
    name: 'reply-back',
    // Back puts the reply away -- kept as a draft -- and shows the thread.
    run: async (ctx) => {
      await openThread(ctx)
      await press(named(ctx.page, 'Reply'))
      await ctx.settle(700)
      await back(ctx.page)
      await ctx.settle(700)
    },
  },
  {
    name: 'reply-typed',
    // Words in a reply, the keyboard's room left to them.
    run: async (ctx) => {
      await openThread(ctx)
      await press(named(ctx.page, 'Reply'))
      await ctx.settle(700)
      await ctx.page.locator('.ed-content').first().click()
      await ctx.page.keyboard.type('Thanks, that works for me. I will look at it this afternoon.')
      await ctx.settle(500)
    },
  },
  {
    name: 'thread-draft',
    // A reply put away with words in it comes back as a draft in its thread.
    run: async (ctx) => {
      await openThread(ctx)
      await press(named(ctx.page, 'Reply'))
      await ctx.settle(700)
      await ctx.page.locator('.ed-content').first().click()
      await ctx.page.keyboard.type('Thanks, that works for me.')
      await ctx.settle(800)
      // Back on a phone; the sheet's own close where there is no back.
      if (globalThis.PHONE_CHECK_DESKTOP)
        await press(
          ctx.page.getByRole('region', { name: 'Compose' }).getByRole('button', { name: 'Close' }),
        )
      else await back(ctx.page)
      await ctx.settle(900)
      await ctx.page.locator('.drafts').first().scrollIntoViewIfNeeded()
      await ctx.settle()
    },
  },
  {
    name: 'forward',
    run: async (ctx) => {
      await openThread(ctx)
      await press(named(ctx.page, 'Forward'))
      await ctx.settle(700)
    },
  },

  // ── Pickers and menus ────────────────────────────────────────────
  {
    name: 'snooze',
    run: async (ctx) => {
      await openThread(ctx)
      await press(named(ctx.page, 'Snooze'))
      await ctx.settle()
    },
  },
  {
    name: 'snooze-back',
    run: async (ctx) => {
      await openThread(ctx)
      await press(named(ctx.page, 'Snooze'))
      await ctx.settle()
      await back(ctx.page)
      await ctx.settle()
    },
  },
  {
    name: 'label',
    run: async (ctx) => {
      await openThread(ctx)
      await action(ctx, 'Label')
      await ctx.settle()
    },
  },
  {
    name: 'thread-more',
    // What a phone's toolbar keeps in its menu.
    run: async (ctx) => {
      await openThread(ctx)
      const more = ctx.page.getByRole('button', { name: 'More actions' })
      if ((await more.count()) > 0) await press(more.first())
      await ctx.settle()
    },
  },
  {
    name: 'row-menu',
    run: async (ctx) => {
      await longPress(ctx, ctx.page.locator('button[data-thread]').first())
    },
  },
  {
    name: 'row-snooze',
    // Snooze from a row's menu, which is where the list has it.
    run: async (ctx) => {
      await longPress(ctx, ctx.page.locator('button[data-thread]').first())
      await press(ctx.page.getByRole('menuitem', { name: /^Snooze/ }))
      await ctx.settle()
    },
  },
  {
    name: 'delete-forever',
    // The confirmation Delete forever asks, from Trash.
    run: async (ctx) => {
      await openDrawer(ctx)
      await mailbox(ctx, 'Trash')
      await longPress(ctx, ctx.page.locator('button[data-thread]').first())
      await press(ctx.page.getByRole('menuitem', { name: /^Delete forever/ }))
      await ctx.settle()
    },
  },

  // ── Several at once ──────────────────────────────────────────────
  {
    name: 'select',
    // A conversation picked from its menu, then a second by a tap: the bar
    // of what can be done to both.
    run: async (ctx) => {
      const { page, settle } = ctx
      await longPress(ctx, page.locator('button[data-thread]').first())
      const pick = page.getByRole('menuitem', { name: /^Select/ })
      if ((await pick.count()) === 0) return
      await press(pick.first())
      await settle()
      await press(page.locator('button[data-thread]').nth(2))
      await settle()
    },
  },

  // ── Filters, search, cleanup ─────────────────────────────────────
  {
    name: 'filter-unread',
    run: async ({ page, settle }) => {
      await press(page.getByRole('button', { name: 'Unread', exact: true }))
      await settle()
    },
  },
  {
    name: 'filter-tab',
    run: async ({ page, settle }) => {
      await press(page.getByRole('tab', { name: 'Newsletters' }))
      await settle()
    },
  },
  {
    name: 'filter-empty',
    // Starred newsletters: nothing, and a way out.
    run: async ({ page, settle }) => {
      await press(page.getByRole('tab', { name: 'Priority' }))
      await settle()
      await press(page.getByRole('button', { name: 'Starred', exact: true }))
      await settle()
    },
  },
  {
    name: 'search',
    run: async ({ page, settle }) => {
      const bar = page.getByRole('combobox').first()
      await press(bar)
      await bar.fill('report')
      await settle(800)
    },
  },
  {
    name: 'search-none',
    run: async ({ page, settle }) => {
      const bar = page.getByRole('combobox').first()
      await press(bar)
      await bar.fill('zzzzqqqq')
      await settle(800)
    },
  },
  {
    name: 'cleanup',
    run: async ({ page, settle }) => {
      await press(page.getByRole('button', { name: 'Quick cleanup' }))
      await settle(900)
    },
  },
  {
    name: 'cleanup-back',
    run: async ({ page, settle }) => {
      await press(page.getByRole('button', { name: 'Quick cleanup' }))
      await settle(900)
      await back(page)
      await settle()
    },
  },

  // ── Mailboxes ────────────────────────────────────────────────────
  { name: 'nav', run: openDrawer },
  {
    name: 'nav-trash',
    // Trash, with its "Empty Trash" bar.
    run: async (ctx) => {
      await openDrawer(ctx)
      await mailbox(ctx, 'Trash')
    },
  },
  {
    name: 'nav-sent',
    run: async (ctx) => {
      await openDrawer(ctx)
      await mailbox(ctx, 'Sent')
    },
  },
  {
    name: 'nav-drafts',
    run: async (ctx) => {
      await openDrawer(ctx)
      await mailbox(ctx, 'Drafts')
    },
  },
  {
    name: 'nav-account',
    // An account folded in the drawer, opened for its own folders.
    run: async (ctx) => {
      await openDrawer(ctx)
      await press(ctx.page.locator('button.account-toggle').first())
      await ctx.settle()
    },
  },
  {
    name: 'nav-scheduled',
    // The one queued send, under the account it is from.
    run: async (ctx) => {
      await openDrawer(ctx)
      await press(ctx.page.locator('button.account-toggle').first())
      await ctx.settle()
      await mailbox(ctx, 'Scheduled')
    },
  },
  {
    name: 'nav-scheduled-reschedule',
    run: async (ctx) => {
      await openDrawer(ctx)
      await press(ctx.page.locator('button.account-toggle').first())
      await ctx.settle()
      await mailbox(ctx, 'Scheduled')
      await press(ctx.page.getByRole('button', { name: 'Reschedule', exact: true }))
      await ctx.settle()
    },
  },
  {
    name: 'nav-outbox',
    // A message to an address the mock's server refuses, sent: it comes
    // back to the Outbox, which the drawer then offers.
    run: async (ctx) => {
      const { page, settle } = ctx
      await tap(page, 'button[aria-label="Compose"]')
      await settle(700)
      const to = page.getByRole('textbox', { name: 'To', exact: true })
      await to.fill('nobody@refused.example')
      await to.press('Enter')
      await page.getByRole('textbox', { name: 'Subject' }).fill('Hello')
      await press(page.getByRole('button', { name: 'Send', exact: true }))
      await page.waitForTimeout(6500)
      await settle()
      await openDrawer(ctx)
      await mailbox(ctx, 'Outbox')
    },
  },
  {
    name: 'nav-snoozed',
    run: async (ctx) => {
      await openDrawer(ctx)
      await mailbox(ctx, 'Snoozed')
    },
  },
]

/** The list as a first visit finds it. The toggles and the category tab are
 *  kept on the device, and each scenario's page shares them with the last:
 *  put them back, so a screen does not depend on the one before it. */
async function plainList(page) {
  for (const name of ['Unread', 'Starred', 'Attachments']) {
    const toggle = page.getByRole('button', { name, exact: true })
    if (
      (await toggle.count()) > 0 &&
      (await toggle.first().getAttribute('aria-pressed')) === 'true'
    )
      await press(toggle.first())
  }
  const all = page.getByRole('tab', { name: 'All', exact: true })
  if ((await all.count()) > 0 && (await all.first().getAttribute('aria-selected')) !== 'true')
    await press(all.first())
}

export default {
  mail: scenarios.map((scenario) => ({
    ...scenario,
    run: async (ctx) => {
      // The harness's own frame noise is let by; see `ignoreSandboxNoise`.
      ignoreSandboxNoise(ctx.page)
      await plainList(ctx.page)
      await ctx.settle(200)
      await scenario.run?.(ctx)
    },
  })),
}
