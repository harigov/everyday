// The todo app on a phone: the list (and what shapes it -- the filters, the
// order, the quick-add line), a task opened over it and back, the task's page
// scrolled and written in with the keyboard up, the row's menu, a project
// with its subtasks and its board, a card opened, the goals pane and a goal's
// page and its targets, the projects under one goal, a proposed task, the
// drawer, and the dialogs a task raises. See ../scenarios.mjs.
//
// The mock's clock is Wednesday 25 November. Today lists "Ring the dentist
// back" and "Book the van" (overdue; the van has a proposed change under it).
// "Move house" is a project with a task, "Pack the study", that has subtasks,
// and tasks in most columns of its board. "Ship the vault rewrite" is a goal
// with a project filed under it.
//
// A screen only a phone has -- the drawer's button, the way back -- is
// skipped in a `--desktop` run (see `check.mjs`): the desktop is compared
// screen for screen, and a scenario that cannot reach the screen shows the
// app's home instead.

import { back, press } from '../helpers.mjs'

const desktop = () => globalThis.PHONE_CHECK_DESKTOP === true

const openDrawer = async ({ page, settle }) => {
  await press(page.getByRole('button', { name: 'Show the sidebar' }))
  await settle()
}

/** A sidebar row: in the drawer on a phone, and on the page on a desktop. */
const goTo = async (ctx, name) => {
  if (!desktop()) await openDrawer(ctx)
  await press(ctx.page.getByRole('button', { name }).first())
  await ctx.settle(600)
}

const openProject = (ctx) => goTo(ctx, /Move house/)

const openTask = async (ctx, title = 'Ring the dentist back') => {
  await press(ctx.page.getByText(title, { exact: true }).first())
  await ctx.settle(600)
}

/** The software keyboard, as far as the page can tell: the window shorter by
 *  what it covers, and the flag the shell raises. */
const raiseKeyboard = async ({ page, size, settle }) => {
  await page.setViewportSize({ width: size.width, height: 430 })
  await page.evaluate(() => document.documentElement.setAttribute('data-keyboard', ''))
  await settle()
}

/** Scrolled to the end of whatever scrolls. */
const toEnd = async ({ page, settle }) => {
  await page.evaluate(() => {
    for (const el of document.querySelectorAll('.scroll')) el.scrollTop = el.scrollHeight
  })
  await settle()
}

const showBoard = async (ctx) => {
  await openProject(ctx)
  await press(ctx.page.getByRole('button', { name: 'Board view' }))
  await ctx.settle(600)
}

const openGoals = (ctx) => goTo(ctx, /^All goals/)

const scenarios = [
  // The app as it opens: today's list.
  { name: 'home' },
  { name: 'home-bottom', run: toEnd },

  // What shapes the list.
  {
    name: 'filter-all',
    run: async ({ page, settle }) => {
      await press(page.getByRole('tab', { name: /^All/ }))
      await settle()
    },
  },
  {
    name: 'filter-empty',
    run: async ({ page, settle }) => {
      // Done is at the far end of the strip of chips: slid to it first.
      await page.evaluate(() => {
        const strip = document.querySelector('[role=tablist][aria-label=Status]')
        if (strip) strip.scrollLeft = strip.scrollWidth
      })
      await press(page.getByRole('tab', { name: /^Done/ }))
      await settle()
    },
  },
  {
    name: 'filter-priority',
    run: async ({ page, settle }) => {
      await page.locator('select[aria-label=Priority]').selectOption('high')
      await settle()
    },
  },
  {
    name: 'filters-scrolled',
    run: async ({ page, settle }) => {
      // The status chips are a strip: scrolled to its end.
      await page.evaluate(() => {
        const strip = document.querySelector('[role=tablist][aria-label=Status]')
        if (strip) strip.scrollLeft = strip.scrollWidth
      })
      await settle()
    },
  },
  {
    name: 'group',
    run: async ({ page, settle }) => {
      await page.getByLabel('Group by').selectOption('priority')
      await settle()
    },
  },

  // The quick-add line.
  {
    name: 'add',
    run: async ({ page, settle }) => {
      await page.getByPlaceholder('Add a task').fill('Buy milk @tomorrow !high #errands ~15m')
      await settle()
    },
  },
  {
    name: 'add-keyboard',
    run: async (ctx) => {
      await ctx.page.getByPlaceholder('Add a task').click()
      await raiseKeyboard(ctx)
      await ctx.page.getByPlaceholder('Add a task').fill('Buy milk @tomorrow !high')
      await ctx.settle()
    },
  },

  // The sidebar's half, in the drawer.
  {
    name: 'drawer',
    run: async (ctx) => {
      if (desktop()) return
      await openDrawer(ctx)
    },
  },
  {
    name: 'drawer-bottom',
    run: async (ctx) => {
      if (desktop()) return
      await openDrawer(ctx)
      await ctx.page.evaluate(() => {
        const nav = document.querySelector('aside.sidebar .side-nav')
        if (nav) nav.scrollTop = nav.scrollHeight
      })
      await ctx.settle()
    },
  },
  {
    name: 'drawer-new-project',
    run: async (ctx) => {
      if (desktop()) return
      await openDrawer(ctx)
      await press(ctx.page.getByRole('button', { name: 'New project' }))
      await ctx.settle()
    },
  },

  // A task opened over the list, and back to it.
  { name: 'task', run: (ctx) => openTask(ctx) },
  {
    name: 'task-back',
    run: async (ctx) => {
      if (desktop()) return
      await openTask(ctx)
      await back(ctx.page)
      await ctx.settle()
    },
  },
  {
    name: 'task-back-button',
    run: async (ctx) => {
      if (desktop()) return
      await openTask(ctx)
      await press(ctx.page.getByRole('button', { name: 'Back' }))
      await ctx.settle()
    },
  },
  {
    name: 'task-again',
    run: async (ctx) => {
      // Back, and the same task once more: it opens again.
      if (desktop()) return
      await openTask(ctx)
      await back(ctx.page)
      await ctx.settle()
      await openTask(ctx)
    },
  },
  {
    name: 'task-bottom',
    run: async (ctx) => {
      await openTask(ctx)
      await toEnd(ctx)
    },
  },
  {
    name: 'task-with-details',
    run: async (ctx) => {
      // The task with the most on it: a project, a reminder, tags, time.
      await openTask(ctx, 'Book the van')
    },
  },
  {
    name: 'task-purpose',
    run: async (ctx) => {
      await openTask(ctx)
      await press(
        ctx.page
          .getByRole('button', { name: /No goal or area|Same as the project|Filed under/ })
          .first(),
      )
      await ctx.settle()
    },
  },
  {
    name: 'task-keyboard',
    run: async (ctx) => {
      await openTask(ctx)
      await raiseKeyboard(ctx)
      await ctx.page.locator('textarea.title').click()
      await ctx.settle()
    },
  },
  {
    name: 'task-notes-keyboard',
    run: async (ctx) => {
      await openTask(ctx)
      await toEnd(ctx)
      await ctx.page.locator('textarea.notes').click()
      await raiseKeyboard(ctx)
    },
  },
  {
    name: 'task-book',
    run: async (ctx) => {
      await openTask(ctx)
      await toEnd(ctx)
      await press(ctx.page.getByRole('button', { name: /Book$/ }))
      await ctx.settle()
      await toEnd(ctx)
    },
  },
  {
    name: 'task-delete-asks',
    run: async (ctx) => {
      await openTask(ctx)
      await toEnd(ctx)
      await press(ctx.page.getByRole('button', { name: 'Delete task' }))
      await ctx.settle()
    },
  },
  {
    name: 'task-delete-back',
    run: async (ctx) => {
      // Confirmed: the task is gone and the list is what is left.
      await openTask(ctx)
      await toEnd(ctx)
      await press(ctx.page.getByRole('button', { name: 'Delete task' }))
      await ctx.settle()
      await press(ctx.page.locator('.sheet .btn-danger'))
      await ctx.settle(600)
    },
  },

  // A row's menu: the button on a phone, a right-click on a desktop.
  {
    name: 'row-menu',
    run: async ({ page, settle }) => {
      if (desktop()) {
        await page
          .getByText('Ring the dentist back', { exact: true })
          .first()
          .click({ button: 'right' })
      } else {
        await press(page.getByRole('button', { name: 'More actions' }).first())
      }
      await settle()
    },
  },

  // A project: its tasks, their subtasks, and its board.
  { name: 'project', run: openProject },
  {
    name: 'project-bottom',
    run: async (ctx) => {
      await openProject(ctx)
      await toEnd(ctx)
    },
  },
  {
    name: 'project-collapsed',
    run: async (ctx) => {
      await openProject(ctx)
      await press(ctx.page.getByRole('button', { name: /of \d+ done/ }).first())
      await ctx.settle()
    },
  },
  {
    name: 'project-add-subtask',
    run: async (ctx) => {
      await openProject(ctx)
      await press(ctx.page.getByRole('button', { name: 'Add a subtask' }).first())
      await ctx.settle()
    },
  },
  { name: 'board', run: showBoard },
  {
    name: 'board-next',
    run: async (ctx) => {
      // The strip slid to its second column.
      await showBoard(ctx)
      await ctx.page.evaluate(() => {
        const board = document.querySelector('.board')
        if (board) board.scrollLeft = board.clientWidth
      })
      await ctx.settle(500)
    },
  },
  {
    name: 'board-end',
    run: async (ctx) => {
      await showBoard(ctx)
      await ctx.page.evaluate(() => {
        const board = document.querySelector('.board')
        if (board) board.scrollLeft = board.scrollWidth
      })
      await ctx.settle(500)
    },
  },
  {
    name: 'board-card',
    run: async (ctx) => {
      await showBoard(ctx)
      await press(ctx.page.locator('[data-card] .cardbody').first())
      await ctx.settle(600)
    },
  },
  {
    name: 'board-card-back',
    run: async (ctx) => {
      if (desktop()) return
      await showBoard(ctx)
      await press(ctx.page.locator('[data-card] .cardbody').first())
      await ctx.settle(600)
      await back(ctx.page)
      await ctx.settle()
    },
  },
  {
    name: 'board-add',
    run: async (ctx) => {
      await showBoard(ctx)
      await press(ctx.page.getByRole('button', { name: /^Add to / }).first())
      await ctx.settle()
    },
  },
  {
    name: 'board-menu',
    run: async (ctx) => {
      await showBoard(ctx)
      await ctx.page
        .locator('[data-card]')
        .first()
        .dispatchEvent('contextmenu', { clientX: 120, clientY: 300 })
      await ctx.settle()
    },
  },

  // What the assistant proposed, under the task it would change.
  {
    name: 'proposal',
    run: async ({ page, settle }) => {
      await press(page.getByRole('button', { name: /^Open proposal: Update task/ }).first())
      await settle(600)
    },
  },

  // Goals: every goal under its area, one goal's page, and its targets.
  { name: 'goals', run: openGoals },
  {
    name: 'goals-goal',
    run: async (ctx) => {
      await openGoals(ctx)
      await press(ctx.page.locator('.goalrow', { hasText: 'Ship the vault rewrite' }))
      await ctx.settle(600)
    },
  },
  {
    name: 'goals-goal-bottom',
    run: async (ctx) => {
      await openGoals(ctx)
      await press(ctx.page.locator('.goalrow', { hasText: 'Ship the vault rewrite' }))
      await ctx.settle(600)
      await toEnd(ctx)
    },
  },
  {
    name: 'goals-goal-back',
    run: async (ctx) => {
      if (desktop()) return
      await openGoals(ctx)
      await press(ctx.page.locator('.goalrow', { hasText: 'Ship the vault rewrite' }))
      await ctx.settle(600)
      await back(ctx.page)
      await ctx.settle()
    },
  },
  {
    name: 'goals-add-target',
    run: async (ctx) => {
      await openGoals(ctx)
      await press(ctx.page.locator('.goalrow', { hasText: 'Ship the vault rewrite' }))
      await ctx.settle(600)
      await press(ctx.page.getByRole('button', { name: 'Add a target' }))
      await ctx.settle()
      await toEnd(ctx)
    },
  },
  {
    name: 'goals-edit-target',
    run: async (ctx) => {
      await openGoals(ctx)
      await press(ctx.page.locator('.goalrow', { hasText: 'Ship the vault rewrite' }))
      await ctx.settle(600)
      await press(ctx.page.getByRole('button', { name: /^Edit targets for/ }).first())
      await ctx.settle()
    },
  },
  {
    name: 'goals-new-goal',
    run: async (ctx) => {
      await openGoals(ctx)
      await ctx.page.locator('input.newgoal').first().fill('Learn to sail')
      await ctx.settle()
    },
  },
  {
    name: 'goal-projects',
    run: async (ctx) => {
      await goTo(ctx, /Ship the vault rewrite/)
    },
  },
  {
    name: 'goal-projects-empty',
    run: async (ctx) => {
      await goTo(ctx, /Viya rides without stabilisers/)
    },
  },
]

/** The keyboard (and typing at the page) is a phone's: a desktop run shows the app's home for those. */
const withKeyboard = (scenario) =>
  /keyboard|format/.test(scenario.name)
    ? { ...scenario, run: async (ctx) => (desktop() ? undefined : scenario.run(ctx)) }
    : scenario

export default {
  todo: scenarios.map(withKeyboard),
}
