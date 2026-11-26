// The money app on a phone: the overview's cards, the month's transactions
// and one opened over the list, ticking rows and what is done to the ticked,
// typing one in, the budget and its editors, the reports' charts and tables,
// the rules and their form, the accounts and the sheets that connect a bank
// or import a statement, and the empty states. See ../scenarios.mjs.
//
// What the money app remembers in the browser (the pane it was on) outlives
// a scenario, and so does what the store holds (the month, a filter, a row
// open), so every scenario starts by putting both back (`fresh`).
//
// A screen only a phone has is skipped in a `--desktop` run (see
// `check.mjs`): the desktop is compared screen for screen, and a scenario
// that cannot reach the screen shows the app's home instead.

import { back, press } from '../helpers.mjs'

const desktop = () => globalThis.PHONE_CHECK_DESKTOP === true

/** The overview, this month, nothing narrowed, nothing open or ticked. */
const fresh = async ({ page }) => {
  await page.evaluate(async () => {
    const { money } = await import('/src/lib/money.svelte.ts')
    const { thisMonth } = await import('/src/lib/money.ts')
    money.clearSelection()
    money.open(null)
    money.closeAdd()
    money.setQuery('')
    await money.setAccount(null)
    await money.setCategory(null)
    await money.setMonth(thisMonth())
    money.setView('overview')
  })
}

const openDrawer = async ({ page, settle }) => {
  await press(page.getByRole('button', { name: 'Show the sidebar' }))
  await settle()
}

/** One of the six panes, by name: through the drawer on a phone. */
const goTo = (name) => async (ctx) => {
  if (!desktop()) await openDrawer(ctx)
  await press(
    ctx.page
      .locator('nav[aria-label="Money"] > .side-row')
      .filter({ hasText: new RegExp(`^\\s*${name}`) })
      .first(),
  )
  await ctx.settle(600)
}

/** The bottom of whatever scrolls, as a person reaching the end would. */
const toEnd = async ({ page, settle }) => {
  await page.evaluate(() => {
    for (const el of document.querySelectorAll('*')) {
      const s = getComputedStyle(el)
      if (/(auto|scroll)/.test(s.overflowY) && el.scrollHeight > el.clientHeight + 5) {
        el.scrollTop = el.scrollHeight
      }
    }
  })
  await settle()
}

/** The first transaction in the list. */
const openFirst = async (ctx) => {
  await goTo('Transactions')(ctx)
  await press(ctx.page.locator('.txn-rows button.main').first())
  await ctx.settle(600)
}

/** A transaction to tick: its box, which a desktop shows on hover. */
const tick = async (ctx, n) => {
  const box = ctx.page.getByRole('checkbox', { name: /^Tick / }).nth(n)
  if (desktop()) await box.click({ force: true })
  else await press(box)
  await ctx.settle()
}

/** Type into the top bar's field, as a person with a keyboard up would. */
async function search({ page, settle }, text) {
  await press(page.locator('[data-search]').first())
  await page.keyboard.type(text)
  await settle(900)
}

const STATEMENT = [
  'Date,Description,Amount',
  '2026-11-02,BLUE BOTTLE COFFEE,-5.50',
  '2026-11-03,GROCERY OUTLET,-64.12',
  '2026-11-04,PAYROLL ACME INC,3250.00',
  '',
].join('\n')

const openImport = async (ctx) => {
  await goTo('Accounts')(ctx)
  await press(ctx.page.getByRole('button', { name: /Import a file/ }).first())
  await ctx.settle()
}

export default {
  money: [
    // The overview as it opens, and its end.
    { name: 'home' },
    { name: 'overview-bottom', run: toEnd },
    {
      name: 'overview-month',
      run: async (ctx) => {
        await press(
          ctx.page
            .getByRole('group', { name: 'Month' })
            .getByRole('button', { name: 'The month before' }),
        )
        await ctx.settle(600)
      },
    },
    {
      name: 'drawer',
      run: async (ctx) => {
        if (desktop()) return
        await openDrawer(ctx)
      },
    },

    // The month's transactions, and one opened over the list.
    { name: 'transactions', run: goTo('Transactions') },
    {
      name: 'transactions-bottom',
      run: async (ctx) => {
        await goTo('Transactions')(ctx)
        await toEnd(ctx)
      },
    },
    { name: 'transaction', run: openFirst },
    {
      name: 'transaction-bottom',
      run: async (ctx) => {
        await openFirst(ctx)
        await toEnd(ctx)
      },
    },
    {
      name: 'transaction-category',
      run: async (ctx) => {
        await openFirst(ctx)
        await press(ctx.page.locator('aside[aria-label="Transaction"] [aria-haspopup]').first())
        await ctx.settle()
      },
    },
    {
      name: 'transaction-back',
      run: async (ctx) => {
        if (desktop()) return
        await openFirst(ctx)
        await back(ctx.page)
        await ctx.settle()
      },
    },
    {
      name: 'transaction-close',
      run: async (ctx) => {
        if (desktop()) return
        await openFirst(ctx)
        await press(ctx.page.getByRole('button', { name: /^Back/ }))
        await ctx.settle()
      },
    },
    {
      name: 'transactions-ticked',
      run: async (ctx) => {
        await goTo('Transactions')(ctx)
        await tick(ctx, 0)
        await tick(ctx, 2)
      },
    },
    {
      name: 'transactions-categorise',
      run: async (ctx) => {
        await goTo('Transactions')(ctx)
        await tick(ctx, 0)
        await press(ctx.page.getByRole('button', { name: /Categorise/ }))
        await ctx.settle()
      },
    },
    {
      name: 'transactions-add',
      run: async (ctx) => {
        await goTo('Transactions')(ctx)
        await press(ctx.page.getByRole('button', { name: /^Add$/ }).first())
        await ctx.settle()
      },
    },
    {
      name: 'transactions-add-category',
      run: async (ctx) => {
        await goTo('Transactions')(ctx)
        await press(ctx.page.getByRole('button', { name: /^Add$/ }).first())
        await ctx.settle()
        await press(ctx.page.locator('[role=dialog] [aria-haspopup]').first())
        await ctx.settle()
      },
    },
    {
      name: 'transactions-account',
      run: async (ctx) => {
        if (!desktop()) await openDrawer(ctx)
        await press(ctx.page.locator('nav[aria-label="Money"] .side-row.nested').first())
        await ctx.settle(600)
      },
    },
    {
      name: 'transactions-category',
      run: async (ctx) => {
        await goTo('Transactions')(ctx)
        await ctx.page.getByLabel('Category').selectOption({ label: 'Coffee' })
        await ctx.settle(600)
      },
    },
    {
      name: 'transactions-search',
      run: async (ctx) => {
        await goTo('Transactions')(ctx)
        await search(ctx, 'coffee')
      },
    },
    {
      name: 'transactions-empty',
      run: async (ctx) => {
        await goTo('Transactions')(ctx)
        await search(ctx, 'zzzz nothing')
      },
    },
    {
      name: 'transactions-empty-month',
      run: async (ctx) => {
        await goTo('Transactions')(ctx)
        for (let i = 0; i < 6; i++) {
          await press(
            ctx.page
              .getByRole('group', { name: 'Month' })
              .getByRole('button', { name: 'The month before' }),
          )
          await ctx.settle(400)
        }
      },
    },

    // The budget.
    { name: 'budget', run: goTo('Budget') },
    {
      name: 'budget-bottom',
      run: async (ctx) => {
        await goTo('Budget')(ctx)
        await toEnd(ctx)
      },
    },
    {
      name: 'budget-edit',
      run: async (ctx) => {
        await goTo('Budget')(ctx)
        await press(ctx.page.getByRole('button', { name: /^Budget for Groceries/ }))
        await ctx.settle()
      },
    },
    {
      name: 'budget-more',
      run: async (ctx) => {
        await goTo('Budget')(ctx)
        await press(ctx.page.getByRole('button', { name: 'More for Groceries' }))
        await ctx.settle()
      },
    },
    {
      name: 'budget-edit-category',
      run: async (ctx) => {
        await goTo('Budget')(ctx)
        await press(ctx.page.getByRole('button', { name: 'More for Groceries' }))
        await ctx.settle()
        await press(
          ctx.page.locator('[role=menu] [role^=menuitem]').filter({ hasText: /Edit/ }).first(),
        )
        await ctx.settle()
      },
    },
    {
      name: 'budget-new-category',
      run: async (ctx) => {
        await goTo('Budget')(ctx)
        await press(ctx.page.getByRole('button', { name: 'New category' }).first())
        await ctx.settle()
      },
    },

    // The reports.
    { name: 'reports', run: goTo('Reports') },
    {
      name: 'reports-table',
      run: async (ctx) => {
        await goTo('Reports')(ctx)
        await press(ctx.page.getByText('As a table').first())
        await ctx.settle()
      },
    },
    {
      name: 'reports-middle',
      run: async (ctx) => {
        await goTo('Reports')(ctx)
        await ctx.page.evaluate(() => {
          for (const el of document.querySelectorAll('*')) {
            const s = getComputedStyle(el)
            if (/(auto|scroll)/.test(s.overflowY) && el.scrollHeight > el.clientHeight + 5)
              el.scrollTop = el.scrollHeight / 2
          }
        })
        await ctx.settle()
      },
    },
    {
      name: 'reports-bottom',
      run: async (ctx) => {
        await goTo('Reports')(ctx)
        await toEnd(ctx)
      },
    },

    // The rules.
    { name: 'rules', run: goTo('Rules') },
    {
      name: 'rules-bottom',
      run: async (ctx) => {
        await goTo('Rules')(ctx)
        await toEnd(ctx)
      },
    },
    {
      name: 'rules-new',
      run: async (ctx) => {
        await goTo('Rules')(ctx)
        await press(ctx.page.getByRole('button', { name: 'New rule' }).first())
        await ctx.settle()
      },
    },
    {
      name: 'rules-edit',
      run: async (ctx) => {
        await goTo('Rules')(ctx)
        await press(ctx.page.getByRole('button', { name: 'Edit this rule' }).first())
        await ctx.settle()
      },
    },

    // The accounts, and what connects or fills one.
    { name: 'accounts', run: goTo('Accounts') },
    {
      name: 'accounts-bottom',
      run: async (ctx) => {
        await goTo('Accounts')(ctx)
        await toEnd(ctx)
      },
    },
    {
      name: 'accounts-menu',
      run: async (ctx) => {
        await goTo('Accounts')(ctx)
        await press(ctx.page.getByRole('button', { name: /^More for / }).first())
        await ctx.settle()
      },
    },
    {
      name: 'accounts-rename',
      run: async (ctx) => {
        await goTo('Accounts')(ctx)
        await press(ctx.page.getByRole('button', { name: /^Rename / }).first())
        await ctx.settle()
      },
    },
    {
      name: 'accounts-new',
      run: async (ctx) => {
        await goTo('Accounts')(ctx)
        await press(ctx.page.getByRole('button', { name: 'New manual account' }))
        await ctx.settle()
      },
    },
    {
      name: 'connect',
      run: async (ctx) => {
        await goTo('Accounts')(ctx)
        await press(ctx.page.getByRole('button', { name: /Connect a bank/ }).first())
        await ctx.settle(400)
      },
    },
    { name: 'import', run: openImport },
    {
      name: 'import-preview',
      run: async (ctx) => {
        await openImport(ctx)
        await ctx.page.locator('input[type=file]').setInputFiles({
          name: 'statement.csv',
          mimeType: 'text/csv',
          buffer: Buffer.from(STATEMENT),
        })
        await ctx.settle(800)
      },
    },
    {
      name: 'import-back',
      run: async (ctx) => {
        if (desktop()) return
        await openImport(ctx)
        await back(ctx.page)
        await ctx.settle()
      },
    },

    // Nothing to show: no bank and no account at all.
    {
      name: 'empty',
      run: async (ctx) => {
        await ctx.page.evaluate(async () => {
          const { money } = await import('/src/lib/money.svelte.ts')
          money.links = []
          money.accounts = []
        })
        await ctx.settle()
      },
    },
  ].map((s) => ({
    ...s,
    run: async (ctx) => {
      await fresh(ctx)
      await ctx.settle(300)
      await s.run?.(ctx)
    },
  })),
}
