// The assistant on a phone: the Assistant app (the list of conversations, a
// conversation opened over it, and back), and the assistant's panel over
// another app. See ../scenarios.mjs.
//
// The mock answers a message by what it says: "delete ..." stops at a card
// that asks first, "plan a weekend" runs a plan and tool cards, "research
// ..." starts a background job, "think ..." streams reasoning. A scenario
// types one, sends it and waits for the card it is after.

import { back, press, tap } from '../helpers.mjs'

/** A row of the conversations' list: on a phone the list is the app's home
 *  page; on a desktop it is the sidebar. */
const row = (page, text) =>
  page
    .locator(globalThis.PHONE_CHECK_DESKTOP ? '.sidebar .side-row' : '.list .side-row', {
      hasText: text,
    })
    .first()

/**
 * The thread is on screen and has what it is going to draw: the mock's jobs
 * arrive a moment after the conversation, and a card that is first the plain
 * "Started project" and then the job would make two runs differ.
 */
async function drawn(page) {
  await page
    .locator('.side-row.sel')
    .first()
    .waitFor({ state: 'attached', timeout: 8000 })
    .catch(() => {})
  await page
    .locator('article.job')
    .first()
    .waitFor({ timeout: 4000 })
    .catch(() => {})
}

/** A conversation chosen from the list by what it is called. */
const open = (title) => async (ctx) => {
  await press(row(ctx.page, title))
  await ctx.settle(600)
  await drawn(ctx.page)
}

/**
 * One of the assistant's tabs in Settings, drawn at the width of the screen.
 * The list of tabs beside it is Settings' own, and has its own scenarios; it
 * is put away here so that what is checked is this app's content.
 */
const pane =
  (from, tab) =>
  async ({ page, settle }) => {
    await press(row(page, from))
    await settle(600)
    if (tab) {
      await press(page.locator('aside.nav').getByText(tab, { exact: true }).first())
      await settle(600)
    }
    await page.addStyleTag({ content: 'aside.nav { display: none !important }' })
    await settle(300)
  }

/** Say something in the open conversation. */
async function say(ctx, text) {
  const box = ctx.page.getByRole('textbox', { name: 'Message the assistant' })
  await box.fill(text)
  await press(ctx.page.getByRole('button', { name: 'Send', exact: false }).last())
}

/**
 * The assistant over the app. A desktop window keeps it open from one page
 * to the next (it is a remembered preference), so it is opened only if it is
 * not.
 */
const panel = async ({ page, settle }) => {
  const open = page.locator('button[aria-label="Open the assistant"]')
  if ((await open.count()) > 0) await tap(page, 'button[aria-label="Open the assistant"]')
  await settle(800)
  await drawn(page)
}

/** Make the window as tall as the thread, so all of it is checked. */
async function tall(page, selector) {
  const [width, top, content] = await page.evaluate((selector) => {
    const body = document.querySelector(selector)
    const r = body.getBoundingClientRect()
    return [window.innerWidth, r.top, body.scrollHeight]
  }, selector)
  await page.setViewportSize({ width, height: Math.min(Math.ceil(top + content + 260), 6000) })
  await page.waitForTimeout(300)
}

export default {
  assistant: [
    // The app opens on its conversations, with what happened while you were
    // away on top of them.
    {
      name: 'home',
      run: async ({ page, settle }) => {
        await drawn(page)
        await settle()
      },
    },
    {
      // The same conversations in the sidebar's drawer.
      name: 'drawer',
      run: async ({ page, settle }) => {
        await drawn(page)
        if (globalThis.PHONE_CHECK_DESKTOP) return
        await tap(page, 'button[aria-label="Show the sidebar"]')
        await settle()
      },
    },
    {
      // The Feed's History page, which is drawn by this app's `HistoryView`.
      name: 'history',
      section: 'feed',
      run: async ({ page, settle }) => {
        if (!globalThis.PHONE_CHECK_DESKTOP)
          await tap(page, 'button[aria-label="Show the sidebar"]')
        await settle()
        await press(page.locator('.sidebar .side-row', { hasText: 'History' }).first())
        await settle(700)
      },
    },

    // What the rows above the list lead to: Settings' tabs for what it did,
    // what is waiting, and its routines and memory.
    { name: 'runs', run: pane('What it did') },
    { name: 'proposals', run: pane('Waiting for you') },
    { name: 'routines', run: pane('Routines and memory') },
    { name: 'memory', run: pane('Routines and memory', 'Memory') },
    { name: 'conversation', run: open('Plan a week in Lisbon') },
    {
      name: 'conversation-back',
      run: async (ctx) => {
        await open('Plan a week in Lisbon')(ctx)
        await back(ctx.page)
        await ctx.settle()
      },
    },
    {
      // A watch that reported twice, as the application's own lines.
      name: 'conversation-watch',
      run: open('Tell me when CI on PR 412'),
    },
    {
      // Research that came back: the call, the line saying so, the answer.
      name: 'conversation-research',
      run: async (ctx) => {
        await open('Which outboard')(ctx)
        await tall(ctx.page, '.log')
        await ctx.settle()
      },
    },
    {
      // A new conversation: the greeting, the starting points and the box.
      name: 'new',
      run: async ({ page, settle }) => {
        await drawn(page)
        await press(page.getByRole('button', { name: 'New conversation' }).first())
        await settle(600)
      },
    },
    {
      // A question that stops to ask: the card with its three answers.
      name: 'asks',
      run: async (ctx) => {
        await open('Plan a week in Lisbon')(ctx)
        await say(ctx, 'Delete the task Book the van')
        await ctx.page.getByRole('button', { name: 'Delete' }).first().waitFor({ timeout: 8000 })
        await ctx.settle(500)
      },
    },
    {
      // A longer turn: a plan ticking itself off, its tool cards, and the reply.
      name: 'plan',
      run: async (ctx) => {
        await open('Plan a week in Lisbon')(ctx)
        const before = await ctx.page.locator('button.copy').count()
        await say(ctx, 'Plan a weekend away')
        // Until the reply is whole: its Copy button is the last thing drawn.
        await ctx.page.waitForFunction(
          (n) => document.querySelectorAll('button.copy').length > n,
          before,
          { timeout: 20000 },
        )
        await ctx.settle(500)
      },
    },
    {
      // A turn still going: the status line, and Stop where Send was.
      name: 'busy',
      run: async (ctx) => {
        await open('Plan a week in Lisbon')(ctx)
        await say(ctx, 'Think about something slow')
        await ctx.page.waitForTimeout(1500)
      },
    },
    {
      // A long message: the box grows, the thread stays above it.
      name: 'typing',
      run: async (ctx) => {
        await open('Plan a week in Lisbon')(ctx)
        await ctx.page
          .getByRole('textbox', { name: 'Message the assistant' })
          .fill(
            'Draft a short note to the family about the week in Lisbon: where we are staying, ' +
              'what to book ahead, and who is bringing what. Keep it warm and under a hundred words.',
          )
        await ctx.settle()
      },
    },
    {
      // Everything it has been asked, as it reads on a screen of its own.
      name: 'conversation-all',
      run: async (ctx) => {
        await open('Plan a week in Lisbon')(ctx)
        await tall(ctx.page, '.log')
        await ctx.settle()
      },
    },

    // The assistant opened over another app: the whole screen, the composer
    // on the safe edge, and the list of past conversations.
    {
      name: 'panel',
      section: 'todo',
      run: panel,
    },
    {
      name: 'panel-history',
      section: 'todo',
      run: async (ctx) => {
        await panel(ctx)
        await press(ctx.page.getByRole('button', { name: 'Past conversations' }))
        await ctx.settle()
      },
    },
    {
      name: 'panel-new',
      section: 'todo',
      run: async (ctx) => {
        await panel(ctx)
        await press(ctx.page.getByRole('button', { name: 'New conversation' }))
        await ctx.settle(600)
      },
    },
    {
      name: 'panel-typing',
      section: 'todo',
      run: async (ctx) => {
        await panel(ctx)
        await ctx.page
          .getByRole('textbox', { name: 'Message the assistant' })
          .fill(
            'Which of these should I do first, and is there anything I am forgetting about today?',
          )
        await ctx.settle()
      },
    },
    {
      // Back puts the panel away rather than leaving the app.
      name: 'panel-back',
      section: 'todo',
      run: async (ctx) => {
        await panel(ctx)
        // A desktop has no back button: its Close does what back does, and
        // leaves the rail shut for the pages checked after this one.
        if (globalThis.PHONE_CHECK_DESKTOP)
          await press(ctx.page.locator('aside.panel').getByRole('button', { name: 'Close' }))
        else await back(ctx.page)
        await ctx.settle()
      },
    },
  ],
}
