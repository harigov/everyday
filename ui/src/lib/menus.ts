// The menus more than one view needs, in one place.
//
// A task has a context menu in the list and the same one on the board; a
// block of time has one on the week grid and the same one on the month grid.
// Written twice they would drift, and the second copy is always the one that
// never learns about the new action -- so they are written here, once, and
// the views pass in only what they alone can supply: a confirmation dialog
// to open, a subtask line to reveal.
//
// Everything here is a plain function called at right-click time. Nothing is
// reactive, and it does not need to be: a menu is built from the state of
// the moment it was raised and does not outlive it.

import { DEFAULT_COLORS, colorName } from './colors'
import { SEP, tidyMenu, type MenuItem } from './menu'
import { app } from './state.svelte'
import { accounts } from './accounts.svelte'
import { calendar, DEFAULT_BLOCK_MINUTES, type Slot } from './calendar.svelte'
import { panels } from './panels.svelte'
import { matchPreset, presetsFor } from './recurrence'
import { library } from './library.svelte'
import { tracking } from './tracking.svelte'
import { purpose, samePurpose } from './purpose.svelte'
import { todo } from './todo.svelte'
import { addDays, minutesBetween, offsetInDay, todayIso } from './time'
import { formatMinutes, friendlyDate } from './format'
import { MAX_STARS, fromStars, stars } from './rating'
import { STATUS_LABELS, PRIORITY_LABELS } from './labels'
import { ITEM_STATUSES, PRIORITIES, TASK_STATUSES } from './types'
import type {
  CalendarEvent,
  CalendarId,
  CalendarInfo,
  Item,
  Purpose,
  Reading,
  Task,
  TimeBlock,
  Tracker,
} from './types'

/**
 * The palette, as a submenu.
 *
 * Shared by journals, projects and subscribed calendars, which all carry the
 * same kind of `#rrggbb` accent and all offer the same eight.
 */
export function colourItems(current: string, choose: (colour: string) => unknown): MenuItem[] {
  return DEFAULT_COLORS.map((colour) => ({
    label: colorName(colour),
    dot: colour,
    checked: colour.toLowerCase() === current.toLowerCase(),
    run: () => choose(colour),
  }))
}

/**
 * A "File under" submenu: the roles, with their goals nested under each.
 *
 * Offered from a project, a block, an entry, a shelf item and a subscribed
 * calendar. Two levels rather than one flat list, because the flat version
 * is a column of thirty rows in which "Parent" and "Viya rides without
 * stabilisers" look like peers, and they are not.
 *
 * The first row is always "Nothing in particular". Purpose is optional
 * everywhere, so taking it off has to be as easy as putting it on — and a
 * submenu whose only way out is Escape teaches people not to open it.
 */
export function purposeItems(
  current: Purpose | null | undefined,
  choose: (next: Purpose | null) => unknown,
  opts: { rolesOnly?: boolean } = {},
): MenuItem[] {
  const roles = purpose.activeRoles
  if (roles.length === 0) {
    // Nothing to file under yet. Said rather than shown as an empty menu,
    // which reads as a bug.
    return [{ label: 'No roles yet — add one in Overview', disabled: true }]
  }

  const items: MenuItem[] = [
    {
      label: 'Nothing in particular',
      checked: !current,
      run: () => choose(null),
    },
    SEP,
  ]

  for (const role of roles) {
    const goals = opts.rolesOnly
      ? []
      : purpose.goalsOf(role.id).filter((g) => g.status === 'active' || g.status === 'paused')
    const asRole: Purpose = { type: 'role', id: role.id }
    if (goals.length === 0) {
      items.push({
        label: role.name,
        dot: role.color,
        checked: samePurpose(current, asRole),
        run: () => choose(asRole),
      })
      continue
    }
    items.push({
      label: role.name,
      dot: role.color,
      items: [
        {
          label: `${role.name} itself`,
          hint: 'no particular goal',
          checked: samePurpose(current, asRole),
          run: () => choose(asRole),
        },
        SEP,
        ...goals.map((goal) => ({
          label: goal.title,
          icon: 'target' as const,
          checked: samePurpose(current, { type: 'goal', id: goal.id }),
          run: () => choose({ type: 'goal', id: goal.id }),
        })),
      ],
    })
  }
  return items
}

/** What the views that draw a task row have to supply themselves. */
export interface TaskMenuHooks {
  /** Reveal the subtask line under this row. Absent where there is none. */
  onAddSubtask?: () => void
  /** Edit the title in place. Absent where the view has no field for it. */
  onRename?: () => void
  /** Ask before deleting: the dialog belongs to the view holding the row. */
  onDelete: () => void
}

/** Everything you can do to a task without opening it. */
export function taskMenu(task: Task, hooks: TaskMenuHooks): MenuItem[] {
  const done = task.status === 'done'
  const showing = todo.selectedTask === task.id
  const today = todayIso()

  const due = (label: string, date: string | null): MenuItem => ({
    label,
    checked: (task.dueDate ?? null) === date,
    // A time of day is meaningless without a date, so it goes with it.
    run: () => todo.patch(task.id, { dueDate: date, dueTime: date ? task.dueTime : null }),
  })

  return tidyMenu([
    {
      label: done ? 'Mark as not done' : 'Mark as done',
      icon: done ? 'circle' : 'check',
      run: () => todo.toggleDone(task.id),
    },
    {
      label: showing ? 'Close details' : 'Open details',
      icon: 'list',
      run: () => todo.open(showing ? null : task.id),
    },
    hooks.onRename && { label: 'Rename', icon: 'pencil', run: hooks.onRename },
    SEP,
    {
      label: 'Status',
      icon: 'board',
      items: TASK_STATUSES.map((status) => ({
        label: STATUS_LABELS[status],
        checked: task.status === status,
        run: () => todo.setStatus(task.id, status),
      })),
    },
    {
      label: 'Priority',
      icon: 'flag',
      items: PRIORITIES.map((priority) => ({
        label: PRIORITY_LABELS[priority],
        checked: task.priority === priority,
        run: () => todo.patch(task.id, { priority }),
      })),
    },
    {
      label: 'Due',
      icon: 'calendar',
      hint: task.dueDate ? friendlyDate(task.dueDate) : undefined,
      items: [
        due('Today', today),
        due('Tomorrow', addDays(today, 1)),
        due('In a week', addDays(today, 7)),
        SEP,
        due('No date', null),
      ],
    },
    // Only on a task that stands on its own. A subtask is a step of the job
    // above it and moves with it -- `setProject` takes the whole subtree --
    // so offering to file one somewhere else would be offering to break it
    // off from its parent, which is not what the row says.
    !task.parentId && {
      label: 'Project',
      icon: 'inbox',
      // Tidied like any other built menu: a vault with no projects in it
      // would otherwise get "Inbox" with a rule underneath it and nothing
      // under the rule.
      items: tidyMenu([
        {
          label: 'Inbox',
          checked: !task.projectId,
          run: () => todo.setProject(task.id, null),
        },
        SEP,
        ...todo.liveProjects.map((project) => ({
          label: project.name,
          dot: project.color,
          checked: task.projectId === project.id,
          run: () => todo.setProject(task.id, project.id),
        })),
      ]),
    },
    SEP,
    hooks.onAddSubtask && {
      label: 'Add a subtask',
      icon: 'plus',
      run: hooks.onAddSubtask,
    },
    SEP,
    { label: 'Delete task…', icon: 'trash', danger: true, run: hooks.onDelete },
  ])
}

/** The address an account calendar belongs to, for telling two "Calendar"s apart. */
export function accountLabel(cal: CalendarInfo): string | undefined {
  if (cal.origin.type !== 'account') return undefined
  return accounts.account(cal.origin.accountId)?.address
}

/**
 * Where a new event can go, as a menu: this computer first, then every
 * account calendar.
 *
 * Written once for the three places that ask -- "New events go to" in the
 * sidebar, a new event's Calendar field, and a block's in the rail -- so
 * they cannot disagree about what is on offer. A calendar whose account has
 * to be signed in to again is listed and greyed rather than left out:
 * missing, it reads as a bug; greyed, with the reason beside it, it says
 * what to do. One that can only ever be read -- a feed, or a shared
 * calendar without edit rights -- is not offered at all.
 */
export function calendarChoiceItems(
  current: CalendarId | null,
  choose: (id: CalendarId | null) => unknown,
  opts: { local?: boolean } = {},
): MenuItem[] {
  const offered = calendar.calendars.filter(
    (c) => c.origin.type === 'account' && c.access !== 'readOnly',
  )
  return tidyMenu([
    opts.local !== false && {
      label: 'This computer',
      icon: 'monitor',
      hint: 'your own time',
      checked: current === null,
      run: () => choose(null),
    },
    SEP,
    ...offered.map((c): MenuItem =>
      c.access === 'writable'
        ? {
            label: c.name,
            dot: c.color,
            hint: accountLabel(c),
            checked: current === c.id,
            run: () => choose(c.id),
          }
        : { label: c.name, dot: c.color, hint: 'sign in again', disabled: true },
    ),
  ])
}

/**
 * How a block repeats, as a submenu: the same presets the rail's Repeat
 * field offers, for the same start, with the one in force ticked.
 *
 * "Custom…" opens the block in the rail, where an interval and an ending
 * have room to be chosen; a menu of every combination would be a list
 * nobody could read.
 */
function repeatItems(block: TimeBlock): MenuItem[] {
  const current = matchPreset(block.series?.rule ?? null, block.start, block.tz)
  return tidyMenu([
    ...presetsFor(block.start, block.tz).map((preset): MenuItem => ({
      label: preset.label,
      checked: current === preset.key,
      // Picking the one already in force writes nothing: re-making a
      // series from itself would replace two years of blocks for no change.
      run: () => {
        if (current !== preset.key) void calendar.repeatBlock(block.id, preset.rule)
      },
    })),
    SEP,
    {
      label: 'Custom…',
      checked: current === 'custom',
      run: () => (calendar.selection = { kind: 'block', id: block.id }),
    },
  ])
}

/**
 * A block of time, on either grid.
 *
 * Deleting does not ask. It matches Backspace on a selected block, which has
 * never asked either: a block is a fifteen-second thing to make again, and
 * the dialog is reserved for the deletions that take a subtree with them.
 * One of a series offers the three answers as a submenu instead -- choosing
 * one *is* the answer, so there is still nothing to confirm.
 */
export function blockMenu(block: TimeBlock): MenuItem[] {
  const planned = block.kind === 'planned'
  const minutes = minutesBetween(block.start, block.end)
  const destinations = calendar.writableCalendars
  return tidyMenu([
    {
      label: 'Show details',
      icon: 'list',
      hint: formatMinutes(minutes),
      run: () => (calendar.selection = { kind: 'block', id: block.id }),
    },
    SEP,
    planned && {
      label: 'This is what happened',
      icon: 'tick',
      run: () => calendar.logAsDone(block.id),
    },
    {
      label: planned ? 'Make it a record' : 'Make it a plan again',
      icon: 'clock',
      run: () => calendar.toggleKind(block.id),
    },
    {
      label: 'Track time on it now',
      icon: 'play',
      run: () => calendar.startTimer(block.subject, block.title),
    },
    SEP,
    // An hour's own purpose, overriding whatever it would inherit from its
    // task and that task's project. Mostly used on an adhoc block: the
    // dentist appointment that belongs under looking after yourself.
    {
      label: 'File under',
      icon: 'compass',
      items: purposeItems(block.purpose, (purpose) => calendar.patch(block.id, { purpose })),
    },
    SEP,
    // Only a plan repeats: a record of time spent happened once.
    planned && {
      label: 'Repeat',
      icon: 'refresh',
      hint: block.series ? 'repeats' : undefined,
      items: repeatItems(block),
    },
    SEP,
    {
      label: 'Move to tomorrow',
      icon: 'calendar',
      run: () =>
        calendar.moveBlock(
          block.id,
          addDays(block.localDate, 1),
          offsetInDay(block.start, block.localDate),
        ),
    },
    // Your own appointment, put where other people can see it. A block of
    // work on a task stays yours; only an ad-hoc one is offered the move.
    block.subject.type === 'adhoc' &&
      destinations.length > 0 && {
        label: 'Move to a calendar',
        icon: 'share',
        items: destinations.map((c): MenuItem => ({
          label: c.name,
          dot: c.color,
          hint: accountLabel(c),
          run: () => calendar.moveBlockToCalendar(block.id, c.id),
        })),
      },
    SEP,
    block.series
      ? {
          label: 'Delete',
          icon: 'trash',
          danger: true,
          items: [
            { label: 'Only this one', run: () => calendar.removeBlock(block.id) },
            {
              label: 'This and the ones after it',
              run: () => calendar.deleteBlockSeries(block.id, 'following'),
            },
            {
              label: 'Every one in the series',
              danger: true,
              run: () => calendar.deleteBlockSeries(block.id, 'all'),
            },
          ],
        }
      : { label: 'Delete', icon: 'trash', danger: true, run: () => calendar.removeBlock(block.id) },
  ])
}

/**
 * The new event not saved yet, on either grid. There is one thing to do to
 * it from here: let it go. Saving belongs to the rail, where its fields are.
 */
export function draftMenu(): MenuItem[] {
  return tidyMenu([
    { label: 'Discard this event', icon: 'trash', danger: true, run: () => calendar.cancelDraft() },
  ])
}

/**
 * The slot the timer is drawing right now.
 *
 * Not a block: while it runs there is no record in the vault at all -- the
 * rectangle is drawn off the wall clock, and one `actual` block is written
 * when it stops. So the only thing to offer is the stop.
 */
export function runningMenu(): MenuItem[] {
  return tidyMenu([
    {
      label: 'Stop tracking',
      icon: 'stop',
      hint: formatMinutes(calendar.runningMinutes),
      run: () => calendar.stopTimer(),
    },
  ])
}

/**
 * An event on a calendar: a feed's, or an account's.
 *
 * A feed's is never written to -- it is a copy of somebody else's calendar,
 * and editing it here would not reach the people in the room. An account
 * calendar's can be, through its server, and "Edit…" opens it in the rail
 * for that; deleting asks there too, because a repeating event has to say
 * which of it is meant. What every event offers besides is the two things
 * you would otherwise do by hand: put the same hour in your own record, and
 * start the clock because the meeting has started.
 */
export function eventMenu(event: CalendarEvent): MenuItem[] {
  const cal = calendar.calendarOf(event.calendarId)
  return tidyMenu([
    {
      label: 'Show details',
      icon: 'list',
      run: () => (calendar.selection = { kind: 'event', id: event.id }),
    },
    cal?.access === 'writable' && {
      label: 'Edit…',
      icon: 'pencil',
      run: () => calendar.editEvent(event.id),
    },
    cal?.access === 'needsSignIn' && {
      label: 'Sign in again to change it…',
      icon: 'lock',
      run: () => panels.openSettings('accounts'),
    },
    SEP,
    !event.allDay && {
      label: 'Set this time aside',
      icon: 'clock',
      run: () =>
        calendar.book({
          subject: { type: 'adhoc' },
          day: event.localDate,
          startMinutes: offsetInDay(event.start, event.localDate),
          minutes: minutesBetween(event.start, event.end),
          title: event.title,
        }),
    },
    {
      label: "I'm in it now",
      icon: 'play',
      run: () => calendar.startTimer({ type: 'adhoc' }, event.title),
    },
    SEP,
    cal && {
      label: `Hide “${cal.name}”`,
      icon: 'hidden',
      run: () => calendar.toggleVisible(cal.id),
    },
  ])
}

/**
 * A task seen from the calendar: due in the all-day band, or waiting in the
 * rail.
 *
 * Not `taskMenu`. The calendar loads its own tasks -- the todo store holds
 * whatever *that* app's scope last asked for, and is empty entirely until it
 * has been opened -- so editing one through the todo store from here would
 * quietly do nothing. What this offers is the calendar's own verbs, and a
 * way over to the app that has the rest.
 */
export function calendarTaskMenu(task: Task): MenuItem[] {
  return tidyMenu([
    {
      label: 'Find it a slot',
      icon: 'calendar',
      hint: task.estimateMinutes ? formatMinutes(task.estimateMinutes) : undefined,
      run: () => calendar.scheduleNext(task.id),
    },
    {
      label: 'Track time on it now',
      icon: 'play',
      run: () => calendar.startTimer({ type: 'task', id: task.id }, task.title),
    },
    SEP,
    {
      label: 'Open in the todo app',
      icon: 'check',
      run: async () => {
        app.setSection('todo')
        await todo.setScope(
          task.projectId ? { kind: 'project', id: task.projectId } : { kind: 'all' },
        )
        await todo.open(task.id)
      },
    },
  ])
}

/** Where a day can be navigated to from. Shared by both grids. */
function dayItems(iso: string): MenuItem[] {
  return [
    {
      label: 'Open this day',
      icon: 'day',
      run: () => {
        calendar.view = 'day'
        calendar.goto(iso)
      },
    },
    {
      label: 'Jump to today',
      icon: 'sun',
      // Nothing to jump to when it is already on screen.
      disabled: calendar.days.includes(todayIso()),
      run: () => calendar.goToday(),
    },
  ]
}

/**
 * The empty grid, at the minute the pointer was over.
 *
 * The new-event rows say where the event will go when that is an account
 * calendar, because there it opens as a draft to be saved rather than
 * appearing at once -- and "why did nothing appear" should be answered
 * before it is asked.
 */
export function timeMenu(iso: string, startMinutes: number): MenuItem[] {
  const target = calendar.defaultCalendar
  const fresh = (label: string, minutes: number): MenuItem => ({
    label,
    icon: 'plus',
    hint: target?.name ?? formatMinutes(minutes),
    run: () => calendar.newEvent({ day: iso, startMinutes, minutes }),
  })
  return tidyMenu([
    fresh('New event here', DEFAULT_BLOCK_MINUTES),
    fresh('New half-hour event here', 30),
    {
      label: 'Track time from now',
      icon: 'play',
      run: () => calendar.startTimer({ type: 'adhoc' }, ''),
    },
    SEP,
    ...dayItems(iso),
  ])
}

/** A day as a whole: a column heading, or a cell in the month. */
export function dayMenu(iso: string): MenuItem[] {
  return tidyMenu([
    ...dayItems(iso),
    SEP,
    {
      label: 'New event at 9:00',
      icon: 'plus',
      hint: calendar.defaultCalendar?.name,
      run: () =>
        calendar.newEvent({ day: iso, startMinutes: 9 * 60, minutes: DEFAULT_BLOCK_MINUTES }),
    },
  ])
}

/**
 * A reading of a tracker, seen on the calendar.
 *
 * Deliberately short of an edit. A reading is recorded under the day it
 * belongs to and is changed there, beside the tracker that gives it meaning
 * -- which is the rule the calendar's own `select` already follows by
 * refusing to open a panel for one. So this offers the way back to that day,
 * and the switch that put the reading on the grid in the first place.
 */
export function readingMenu(reading: Reading, tracker: Tracker): MenuItem[] {
  return tidyMenu([
    !!reading.entryId && {
      label: 'Open the entry it was recorded under',
      icon: 'quote',
      run: async () => {
        app.setSection('journal')
        // The journal first: opening an entry the list is not showing leaves
        // the list pointing at some other day's row.
        // A reading logged from no page has no journal to select, and the
        // entry pointer is what this row is offered for anyway.
        if (reading.journalId) await app.selectJournal(reading.journalId)
        await app.openEntry(reading.entryId!)
      },
    },
    {
      label: 'Open this day',
      icon: 'day',
      run: () => {
        calendar.view = 'day'
        calendar.goto(reading.localDate)
      },
    },
    SEP,
    {
      label: `Hide “${tracker.name}” from the calendar`,
      icon: 'hidden',
      run: () => tracking.setOnCalendar(tracker.id, false),
    },
  ])
}

/**
 * Whatever is in a slot on either grid.
 *
 * Four kinds of thing share that rectangle and only two of them are records
 * with a panel, so the dispatch is written once here rather than as a
 * lengthening ternary in each of the two grids.
 */
export function slotMenu(slot: Slot): MenuItem[] {
  if (slot.block) return blockMenu(slot.block)
  if (slot.event) return eventMenu(slot.event)
  if (slot.reading && slot.tracker) return readingMenu(slot.reading, slot.tracker)
  return runningMenu()
}

/** What the views that draw an item have to supply themselves. */
export interface ItemMenuHooks {
  /** Ask before deleting: the dialog belongs to the view holding the card. */
  onDelete: () => void
}

/**
 * Something on a shelf.
 *
 * The status submenu is the reason this exists. Marking a book read is the
 * single most common thing anybody does to a library item, and before this
 * it cost opening the item, finding the row of verbs and closing it again --
 * for something the card already knows how to say in the shelf's own words.
 */
export function itemMenu(item: Item, hooks: ItemMenuHooks): MenuItem[] {
  const kind = library.kindOf(item)
  const showing = library.selected === item.id
  const rating = item.rating ?? null

  return tidyMenu([
    {
      label: showing ? 'Close' : 'Open',
      icon: 'book',
      run: () => (showing ? library.close() : library.open(item.id)),
    },
    {
      label: item.favourite ? 'Remove from favourites' : 'Add to favourites',
      icon: 'star',
      run: () => library.toggleFavourite(item.id),
    },
    SEP,
    {
      // In the shelf's own words: "Read", "Watched", "Played". A menu that
      // said "done" over a shelf whose buttons say "Cooked" would read as a
      // different application's menu.
      label: 'Status',
      // Not a tick: the rows inside are ticked, and a tick on the row that
      // opens them reads as one of them already being chosen.
      icon: 'circle',
      hint: library.label(kind, item.status),
      items: ITEM_STATUSES.map((status) => ({
        label: library.label(kind, status),
        checked: item.status === status,
        run: () => library.setStatus(item.id, status),
      })),
    },
    {
      label: 'Rating',
      icon: 'star',
      // Whole stars only. The control offers halves because an opinion of a
      // film is about that fine; a menu offering eleven rows of them would
      // be a worse way to say the same thing.
      items: tidyMenu([
        ...Array.from({ length: MAX_STARS }, (_, i) => MAX_STARS - i).map((n) => ({
          label: `${n} ${n === 1 ? 'star' : 'stars'}`,
          checked: rating !== null && stars(rating) === n,
          run: () => library.rate(item.id, fromStars(n)),
        })),
        SEP,
        { label: 'No rating', checked: rating === null, run: () => library.rate(item.id, null) },
      ]),
    },
    SEP,
    // Only where there is one to fetch and nothing fetched yet: a cover that
    // arrived with the lookup is re-fetched from the item's own panel, where
    // the picture it would replace is on screen to be compared with.
    !!item.coverUrl &&
      !item.cover && {
        label: 'Fetch the cover',
        icon: 'image',
        run: () => library.fetchCover(item.id),
      },
    SEP,
    {
      label: 'File under',
      icon: 'compass',
      items: purposeItems(item.purpose, (purpose) => library.patch(item.id, { purpose })),
    },
    SEP,
    {
      label: `Delete ${kind?.singular.toLowerCase() ?? 'item'}…`,
      icon: 'trash',
      danger: true,
      run: hooks.onDelete,
    },
  ])
}
