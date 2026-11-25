// The calendar app on a phone: each view (day, the week as a list, month),
// an event opened over them and its editor, a new event, a block of your own,
// a proposed block, the list of what is not scheduled, and the sidebar's
// drawer with Add a calendar in it -- and back out of each layer.
// See ../scenarios.mjs.
//
// The mock's clock is Wednesday 25 November, 10:30. Today has "Team sync"
// (a repeating event on the Work account calendar, with guests: it opens in
// the editor), "Design review" (on a calendar that is read-only, with other
// people and so a brief) and a pending proposed block is tomorrow at nine.
//
// A screen only a phone has -- the "+" that starts an event, the page of
// what is not scheduled, the drawer -- is skipped in a `--desktop` run
// (see `check.mjs`): the desktop is compared screen for screen, and a
// scenario that cannot reach the screen shows the app's home instead.

import { back, press } from '../helpers.mjs'

const desktop = () => globalThis.PHONE_CHECK_DESKTOP === true

const showView =
  (name) =>
  async ({ page, settle }) => {
    await press(page.getByRole('button', { name: `${name} view` }))
    await settle()
  }

const openDay = showView('Day')

/** Day view, then the named event. */
const openEvent = (title) => async (ctx) => {
  await openDay(ctx)
  await press(ctx.page.getByText(title).first())
  await ctx.settle()
}

const openTeamSync = openEvent('Team sync')

const openEditor = async (ctx) => {
  await openTeamSync(ctx)
  await press(ctx.page.getByRole('button', { name: 'Edit', exact: true }))
  await ctx.settle(700)
}

/** "New event" from the header: phone only. */
const startNew = async ({ page, settle }) => {
  await press(page.getByRole('button', { name: 'New event' }))
  await settle()
}

const openNotScheduled = async ({ page, settle }) => {
  await press(page.getByRole('button', { name: /Not scheduled/ }))
  await settle()
}

const openDrawer = async ({ page, settle }) => {
  await press(page.getByRole('button', { name: 'Show the sidebar' }))
  await settle()
}

/** Add a calendar, from the drawer on a phone and the sidebar on a desktop. */
const openAddCalendar = async (ctx) => {
  if (!desktop()) await openDrawer(ctx)
  await press(ctx.page.getByRole('button', { name: 'Add a calendar' }).first())
  await ctx.settle()
}

export default {
  calendar: [
    // The app as it opens: this week, as a list on a phone.
    { name: 'home' },
    {
      name: 'week',
      run: async (ctx) => {
        await openDay(ctx)
        await showView('Week')(ctx)
      },
    },
    { name: 'day', run: openDay },
    { name: 'month', run: showView('Month') },
    {
      name: 'month-day',
      run: async (ctx) => {
        await showView('Month')(ctx)
        if (desktop()) return
        await press(ctx.page.getByRole('button', { name: /^Thursday/ }).first())
        await ctx.settle()
      },
    },
    {
      name: 'month-prev',
      run: async (ctx) => {
        await showView('Month')(ctx)
        await press(ctx.page.getByRole('button', { name: 'Previous', exact: true }))
        await ctx.settle()
      },
    },

    // An event opened over the day, and back to it.
    { name: 'event', run: openTeamSync },
    {
      name: 'event-back',
      run: async (ctx) => {
        await openTeamSync(ctx)
        await back(ctx.page)
        await ctx.settle()
      },
    },
    // Somebody else's meeting, on a calendar that is read-only: a brief,
    // and the actions that are not editing.
    { name: 'event-readonly', run: openEvent('Design review') },
    // Changing an event: the editor over the event, and back to it.
    { name: 'editor', run: openEditor },
    {
      name: 'editor-bottom',
      run: async (ctx) => {
        await openEditor(ctx)
        await ctx.page.evaluate(() => {
          for (const el of document.querySelectorAll('.scroll')) el.scrollTop = el.scrollHeight
        })
        await ctx.settle()
      },
    },
    {
      name: 'editor-repeat',
      run: async (ctx) => {
        await openEditor(ctx)
        await ctx.page.getByLabel('Repeat').selectOption('custom')
        await ctx.settle()
      },
    },
    {
      name: 'editor-save-asks',
      run: async (ctx) => {
        await openEditor(ctx)
        // A series: saving asks which part, in place.
        await press(ctx.page.getByRole('button', { name: /^Save/ }).first())
        await ctx.settle()
      },
    },
    {
      name: 'editor-back',
      run: async (ctx) => {
        await openEditor(ctx)
        await back(ctx.page)
        await ctx.settle()
      },
    },

    // A new event: the editor for this computer, then for an account's
    // calendar, which can have guests.
    {
      name: 'new',
      run: async (ctx) => {
        if (desktop()) return
        await startNew(ctx)
      },
    },
    {
      name: 'new-account',
      run: async (ctx) => {
        if (desktop()) return
        await startNew(ctx)
        await press(ctx.page.getByRole('button', { name: 'This computer' }).first())
        await ctx.settle()
        await press(ctx.page.getByRole('menu').getByText('Work', { exact: true }))
        await ctx.settle()
      },
    },
    {
      name: 'new-back',
      run: async (ctx) => {
        if (desktop()) return
        await startNew(ctx)
        await back(ctx.page)
        await ctx.settle()
      },
    },

    // A block of your own, made from the list of what is not scheduled.
    {
      name: 'not-scheduled',
      run: async (ctx) => {
        if (desktop()) return
        await openNotScheduled(ctx)
      },
    },
    {
      name: 'not-scheduled-back',
      run: async (ctx) => {
        if (desktop()) return
        await openNotScheduled(ctx)
        await back(ctx.page)
        await ctx.settle()
      },
    },
    {
      name: 'block',
      run: async (ctx) => {
        if (desktop()) return
        await openNotScheduled(ctx)
        await press(ctx.page.locator('button.todo').first())
        await ctx.settle(600)
      },
    },
    {
      name: 'block-back',
      run: async (ctx) => {
        if (desktop()) return
        await openNotScheduled(ctx)
        await press(ctx.page.locator('button.todo').first())
        await ctx.settle(600)
        await back(ctx.page)
        await ctx.settle()
      },
    },

    // What the assistant proposed: tomorrow at nine, in the week's list.
    {
      name: 'proposal',
      run: async (ctx) => {
        // The view last used is kept in the browser, and an earlier scenario
        // left it on the day.
        await showView('Week')(ctx)
        const ghost = ctx.page.getByText('Deep work').first()
        // On a desktop the ghost's own buttons lie over the middle of it.
        if (desktop()) await ghost.click({ force: true })
        else await press(ghost)
        await ctx.settle()
      },
    },

    // The sidebar's half, in the drawer; and adding a calendar.
    {
      name: 'drawer',
      run: async (ctx) => {
        if (desktop()) return
        await openDrawer(ctx)
      },
    },
    { name: 'add-calendar', run: openAddCalendar },
    {
      name: 'add-calendar-account',
      run: async (ctx) => {
        await openAddCalendar(ctx)
        await press(ctx.page.getByRole('button', { name: 'From an account' }))
        await ctx.settle()
        await press(ctx.page.getByRole('button', { name: /@gmail\.com/ }).first())
        await ctx.settle()
      },
    },
    {
      name: 'add-calendar-back',
      run: async (ctx) => {
        if (desktop()) return
        await openAddCalendar(ctx)
        await back(ctx.page)
        await ctx.settle()
      },
    },

    // Last: starting the timer is kept in the page's storage, which the
    // scenarios after it in the same browser would find running.
    {
      name: 'event-timer',
      run: async (ctx) => {
        await openEvent('Design review')(ctx)
        await press(ctx.page.getByRole('button', { name: /in it now/ }))
        await ctx.settle()
        if (!desktop()) {
          await back(ctx.page)
          await ctx.settle()
        }
      },
    },
  ],
}
