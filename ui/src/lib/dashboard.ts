// What the Overview is made of: a catalogue of widgets, and the rules for
// arranging them.
//
// Pure, and separate from `overview.svelte.ts` for the reason `habits.ts` and
// `balance.ts` are separate from their stores: these are rules, and the way
// they fail is quiet. A layout that silently drops a widget on the way back
// out of storage is a page somebody arranged and lost. A catalogue entry that
// declares the wrong `needs` is a widget that draws zeros forever, because
// the store never fetched what it reads. Neither throws and neither fails to
// compile, so both are tested next door in `scripts/dashboard.test.mjs`.
//
// # Where a layout lives
//
// In `localStorage`, beside the other things this interface remembers about
// how it is being looked at -- which app was open, covers or list, whether
// the month is showing above the entry list. That is the same class of thing:
// it decides what is drawn, not what is true, and none of it is in the vault.
//
// The cost is honest and worth saying: a page arranged on the laptop is not
// the page you get on the machine under the desk. Making it follow you means
// a record in the vault -- a table, a migration, two commands, a conformance
// test -- and that is a change to the storage layer rather than to this app.
// Until somebody wants it, the layout is chrome.
//
// # Why the widget list is a closed set
//
// Every entry here is a *question somebody would act on*, which is the same
// editorial rule the four fixed panes were built under and the only thing
// stopping a personal dashboard becoming a wall of numbers nobody reads. A
// widget earns its row by naming something you would do differently having
// seen it.

import type { Section } from './apps'
import type { IconName } from './icons'

// ── Sizes ────────────────────────────────────────────────────────────────

/**
 * How wide a widget is, in a six-column grid.
 *
 * Three sizes rather than a free column count. Six columns divide by two and
 * three, so these three tile without gaps in every combination -- which is
 * what makes a page somebody dragged into shape stay in shape, rather than
 * leaving a hole every time a row does not add up to six.
 */
export const WIDGET_SIZES = ['small', 'medium', 'large'] as const
export type WidgetSize = (typeof WIDGET_SIZES)[number]

/** Columns each size spans, out of the six-column grid. */
export const SPAN: Record<WidgetSize, number> = { small: 2, medium: 3, large: 6 }

export const SIZE_LABELS: Record<WidgetSize, string> = {
  small: 'Narrow',
  medium: 'Half',
  large: 'Full width',
}

// ── The catalogue ────────────────────────────────────────────────────────

/**
 * What a widget needs loaded before it can draw anything.
 *
 * Declared per entry rather than fetched wholesale, so a page with three stat
 * tiles on it does not pay for four months of tracker readings. The store
 * unions the needs of whatever is actually on the page -- see `needsOf` --
 * and asks for exactly those.
 */
export const NEEDS = [
  /** The balance report for the week on screen. */
  'balance',
  /** Daily tracker aggregates over the habit window. */
  'trackerDays',
  /** Areas and goals. */
  'purpose',
  /** Per-goal counts: what has happened against each. */
  'goalActivity',
  /** Task counts over the whole vault. */
  'taskStats',
  /** The tasks due in the week on screen. */
  'weekTasks',
  /** Shelf counts. */
  'library',
  /** Entries in the recent window. */
  'entries',
  /** The most recently touched notes. */
  'notes',
  /** Events and blocks for today, and the running timer. */
  'today',
  /**
   * The forecast where you live: current conditions, wind, air quality, and
   * the next couple of days. Its own need rather than folded into `today`,
   * because it is the one read here that leaves the machine -- see
   * `karmayoga_service::domains::weather`'s own doc on why it is not gated on
   * the assistant's "let it use the web" switch.
   */
  'weather',
  /**
   * Pending proposals -- a dream's task and block drafts.
   *
   * Not one of the reads `refresh`'s `Promise.all` fetches: the proposals
   * store already loads and reloads itself, the way `todo` and `calendar` do,
   * because every app that draws a ghost needs it too. Declaring the need
   * here is what makes "Plan for tomorrow" ask it to, the same one line
   * every other widget's need asks for its own read.
   */
  'proposals',
  /**
   * A window of mail, counted: who wrote, who you wrote to, how much
   * arrived. One read per distinct window on the page -- see
   * `activityWindows` -- because a ranked list cannot be re-cut to a
   * shorter window after the fact the way a day-by-day series can.
   */
  'mail',
  /** A window of calendar events, counted -- the same arrangement as `mail`. */
  'meetings',
  /**
   * What is unread in the Feed: every source with its count, and the newest
   * unread stories to pick a few from. Two reads, but one need, since neither
   * means anything without the other -- a count with no story to open, or a
   * story with no source to say whose it is.
   */
  'news',
  /**
   * What is under way: projects in progress, what is being read, watched or
   * played on each shelf (and the shelves, for their verbs), for "What's
   * going on". Goals come with `purpose`.
   */
  'going',
  /** Who on the Contacts shelf is due a catch-up. See docs/plans/keep-in-touch.md. */
  'people',
  /**
   * Open tasks due today or earlier, for "Tasks for today". Not `weekTasks`
   * re-cut: that is the week *on screen*, whatever state its tasks are in,
   * so paging the page back a week would take today's list with it -- and a
   * task that slipped last month is exactly the one this card is for, and
   * no week's read would hold it.
   */
  'todayTasks',
  /**
   * Inbox threads filed Priority or Important, across every account, for
   * "Important mail". Two reads -- one per category, since a thread filter
   * names one -- over every account's Inbox at once.
   */
  'importantMail',
  /** What is snoozed out of every account's Inbox, for "Snoozed mail". */
  'snoozedMail',
  /**
   * The follow-ups the top of the Inbox draws -- waiting on you, waiting on
   * them -- every account's at once, for "Mail to follow up".
   */
  'mailFollowUps',
  /**
   * Today's events on the calendars left visible, with the calendars for
   * their colours. Not `today`, which is your own blocks and the timer: a
   * card about time recorded should not pay for a calendar read, nor the
   * reverse.
   */
  'todayEvents',
  /**
   * Karma: every day of the last year that came to anything, and the total
   * since it started. One read whatever the card's size -- the narrower
   * heatmap is the same year with its older weeks left undrawn.
   */
  'karma',
] as const
export type Need = (typeof NEEDS)[number]

/** Which picker, if the widget is about one particular thing. */
export type Subject = 'tracker'

/** The headings the catalogue is filed under, in the order it draws them. */
export const GROUPS = [
  'Today',
  'Your time',
  'Goals',
  'Habits',
  'Tasks',
  'Meetings',
  'Mail',
  'People',
  'Everything else',
] as const
export type Group = (typeof GROUPS)[number]

export interface WidgetSpec {
  /** The card's heading, and the catalogue row's label. */
  label: string
  /** One line in the catalogue: what the widget answers. */
  note: string
  group: Group
  /** The app whose colour the card wears: where what it shows comes from. */
  app: Section
  icon: IconName
  /** The size it arrives at. */
  size: WidgetSize
  /** The sizes it is worth being. A heatmap is never narrow. */
  sizes: WidgetSize[]
  needs: Need[]
  /** Set when the widget is about one record and the catalogue must ask which. */
  subject?: Subject
  /** Day windows it can be set to, longest last. Absent means it has none. */
  windows?: number[]
  /**
   * The window it arrives at, when that is not the longest. The mail and
   * meeting cards decrypt every record in their window, so they arrive at a
   * month and leave the quarter to somebody who asks for it.
   */
  window?: number
}

/**
 * Every widget the Overview can draw.
 *
 * Ordered by the heading it falls under rather than alphabetically, because
 * this object *is* the catalogue's order.
 */
export const WIDGETS = {
  // ── Today ──────────────────────────────────────────────────────────
  karma: {
    label: 'Karma',
    note: 'What each day came to, a square a day: earned for what you did, lost for the habits you are breaking.',
    group: 'Today',
    app: 'overview',
    icon: 'sparkle',
    size: 'large',
    sizes: ['medium', 'large'],
    needs: ['karma'],
  },
  todayEvents: {
    label: "Today's events",
    note: 'What is on the calendar today, with the one under way picked out.',
    group: 'Today',
    app: 'calendar',
    icon: 'calendar',
    size: 'medium',
    sizes: ['small', 'medium', 'large'],
    needs: ['todayEvents'],
  },
  todayTasks: {
    label: 'Tasks for today',
    note: 'Open tasks due today and the ones that have slipped, each ready to tick off.',
    group: 'Today',
    app: 'todo',
    icon: 'taskList',
    size: 'medium',
    sizes: ['small', 'medium', 'large'],
    needs: ['todayTasks'],
  },
  whatsGoingOn: {
    label: "What's going on",
    note: 'What you are in the middle of these days: projects, what you are reading and watching, the goals you are working on.',
    group: 'Today',
    app: 'overview',
    icon: 'compass',
    size: 'large',
    sizes: ['medium', 'large'],
    needs: ['going', 'purpose'],
  },
  onNow: {
    label: 'On now',
    note: 'What is being tracked this minute, and a way to stop it.',
    group: 'Today',
    app: 'calendar',
    icon: 'clock',
    size: 'small',
    sizes: ['small', 'medium'],
    needs: ['today'],
  },
  dueToday: {
    label: 'Due today',
    note: 'Open tasks dated today, and whether any have already slipped.',
    group: 'Today',
    app: 'todo',
    icon: 'sun',
    size: 'small',
    sizes: ['small', 'medium'],
    needs: ['taskStats'],
  },
  recordedToday: {
    label: 'Recorded today',
    note: 'Hours you have logged so far, against the hours you set aside.',
    group: 'Today',
    app: 'calendar',
    icon: 'day',
    size: 'small',
    sizes: ['small', 'medium'],
    needs: ['today'],
  },
  habitsToday: {
    label: "Today's habits",
    note: 'Every tracker as a chip, ticked or not, with its streak.',
    group: 'Today',
    app: 'overview',
    icon: 'tick',
    size: 'large',
    sizes: ['medium', 'large'],
    needs: ['trackerDays'],
  },
  planTomorrow: {
    label: 'Plan for tomorrow',
    note: "Tomorrow's proposed tasks and time, so you can answer them all before bed.",
    group: 'Today',
    app: 'calendar',
    icon: 'sparkle',
    size: 'medium',
    sizes: ['medium', 'large'],
    needs: ['proposals'],
  },
  weather: {
    label: 'Weather',
    note: 'Current conditions where you live, with air quality and the next couple of days.',
    group: 'Today',
    app: 'overview',
    icon: 'cloud',
    size: 'medium',
    sizes: ['small', 'medium', 'large'],
    needs: ['weather'],
  },

  // ── Your time ──────────────────────────────────────────────────────
  areaBalance: {
    label: 'Where the week went',
    note: 'One bar per area: what you recorded, against what you planned.',
    group: 'Your time',
    app: 'calendar',
    icon: 'week',
    size: 'large',
    sizes: ['medium', 'large'],
    needs: ['balance', 'purpose'],
  },
  weekInWords: {
    label: 'The week, in words',
    note: 'Two sentences about what changed, written by the quick model from the totals above.',
    group: 'Your time',
    app: 'overview',
    icon: 'sparkle',
    size: 'medium',
    sizes: ['medium', 'large'],
    needs: ['balance', 'purpose'],
  },
  areaShare: {
    label: 'Share of your week',
    note: 'The same hours as a single bar, so the proportions are the point.',
    group: 'Your time',
    app: 'calendar',
    icon: 'grid',
    size: 'medium',
    sizes: ['medium', 'large'],
    needs: ['balance', 'purpose'],
  },
  neglected: {
    label: 'Gone quiet',
    note: 'Areas with a goal still open and nothing recorded against it.',
    group: 'Your time',
    app: 'calendar',
    icon: 'alert',
    size: 'medium',
    sizes: ['medium', 'large'],
    needs: ['balance', 'purpose', 'goalActivity'],
  },

  // ── Goals ──────────────────────────────────────────────────────────
  goalProgress: {
    label: 'Goals under way',
    note: 'How far through each open goal its tasks have got.',
    group: 'Goals',
    app: 'overview',
    icon: 'target',
    size: 'medium',
    sizes: ['medium', 'large'],
    needs: ['purpose', 'goalActivity'],
  },
  goalTally: {
    label: 'Goals, counted',
    note: 'How many you are pursuing, and how many you have finished.',
    group: 'Goals',
    app: 'overview',
    icon: 'flag',
    size: 'small',
    sizes: ['small', 'medium'],
    needs: ['purpose'],
  },

  // ── Habits ─────────────────────────────────────────────────────────
  habitStreaks: {
    label: 'Streaks',
    note: 'Every tracker, longest chain first, with how often it is kept.',
    group: 'Habits',
    app: 'overview',
    icon: 'refresh',
    size: 'medium',
    sizes: ['medium', 'large'],
    needs: ['trackerDays'],
  },
  habitHeatmap: {
    label: 'A habit, day by day',
    note: 'Four months of one tracker, a square a day.',
    group: 'Habits',
    app: 'overview',
    icon: 'grid',
    size: 'large',
    sizes: ['medium', 'large'],
    needs: ['trackerDays'],
    subject: 'tracker',
  },
  trackerChart: {
    label: 'A number over time',
    note: 'Pages, minutes, glasses — one tracker plotted day by day.',
    group: 'Habits',
    app: 'overview',
    icon: 'board',
    size: 'medium',
    sizes: ['medium', 'large'],
    needs: ['trackerDays'],
    subject: 'tracker',
    windows: [14, 30, 90],
  },

  // ── Tasks ──────────────────────────────────────────────────────────
  weekTasks: {
    label: 'This week, in tasks',
    note: 'What was dated to this week, and how much of it is done.',
    group: 'Tasks',
    app: 'todo',
    icon: 'check',
    size: 'medium',
    sizes: ['medium', 'large'],
    needs: ['weekTasks'],
  },
  taskTally: {
    label: 'Tasks, counted',
    note: 'Open, finished, and how many hours have gone into them.',
    group: 'Tasks',
    app: 'todo',
    icon: 'list',
    size: 'small',
    sizes: ['small', 'medium'],
    needs: ['taskStats'],
  },

  // ── Meetings ───────────────────────────────────────────────────────
  //
  // Busy, timed events on the calendars you have left visible -- all-day,
  // cancelled and "free" events are not time spent, and the same meeting on
  // two calendars is counted once. See `karmayoga_core::insights`.
  topEvents: {
    label: 'What takes your calendar time',
    note: 'Events grouped by name, ranked by the hours they took.',
    group: 'Meetings',
    app: 'calendar',
    icon: 'calendar',
    size: 'medium',
    sizes: ['medium', 'large'],
    needs: ['meetings'],
    windows: [7, 30, 90],
    window: 30,
  },
  meetingLoad: {
    label: 'Time in meetings',
    note: 'Hours under a calendar event, a day or a week at a time, overlaps counted once.',
    group: 'Meetings',
    app: 'calendar',
    icon: 'clock',
    size: 'medium',
    sizes: ['small', 'medium', 'large'],
    needs: ['meetings'],
    windows: [7, 30, 90],
    window: 30,
  },

  // ── Mail ───────────────────────────────────────────────────────────
  //
  // The first three are the Inbox itself rather than counts of it: what to
  // read, what to answer or chase, and what is coming back. Every account's
  // at once, the way "All inboxes" is, because a page you open to see what
  // matters should not make you pick an account first.
  importantMail: {
    label: 'Important mail',
    note: 'Priority and important mail that is unread or waiting on your reply.',
    group: 'Mail',
    app: 'mail',
    icon: 'inbox',
    size: 'small',
    sizes: ['small', 'medium', 'large'],
    needs: ['importantMail'],
  },
  mailFollowUps: {
    label: 'Mail to follow up',
    note: 'Who is waiting on your reply, whose reply you are waiting on, and for how long.',
    group: 'Mail',
    app: 'mail',
    icon: 'reply',
    size: 'small',
    sizes: ['small', 'medium', 'large'],
    needs: ['mailFollowUps'],
  },
  snoozedMail: {
    label: 'Snoozed mail',
    note: 'What you put off, soonest back first, and when each one returns.',
    group: 'Mail',
    app: 'mail',
    icon: 'hourglass',
    size: 'small',
    sizes: ['small', 'medium', 'large'],
    needs: ['snoozedMail'],
  },
  topSenders: {
    label: 'Who sends you the most',
    note: 'Senders ranked by messages, with how many are still unread.',
    group: 'Mail',
    app: 'mail',
    icon: 'inbox',
    size: 'medium',
    sizes: ['medium', 'large'],
    needs: ['mail'],
    windows: [7, 30, 90],
    window: 30,
  },
  mailVolume: {
    label: 'Mail coming in',
    note: 'Messages received a day or a week at a time, beside how many you sent.',
    group: 'Mail',
    app: 'mail',
    icon: 'mail',
    size: 'medium',
    sizes: ['small', 'medium', 'large'],
    needs: ['mail'],
    windows: [7, 30, 90],
    window: 30,
  },
  mailKinds: {
    label: 'What kind of mail',
    note: 'What arrived, as one bar: important, other, newsletters and notifications.',
    group: 'Mail',
    app: 'mail',
    icon: 'tag',
    size: 'medium',
    sizes: ['medium', 'large'],
    needs: ['mail'],
    windows: [7, 30, 90],
    window: 30,
  },

  // ── People ─────────────────────────────────────────────────────────
  inTouch: {
    label: 'Who you are most in touch with',
    note: 'Mail and meetings together, person by person.',
    group: 'People',
    app: 'mail',
    icon: 'people',
    size: 'large',
    sizes: ['medium', 'large'],
    needs: ['mail', 'meetings'],
    windows: [7, 30, 90],
    window: 30,
  },
  peopleDue: {
    label: 'Who to get in touch with',
    note: 'Contacts past due for a catch-up, by how often you meant to.',
    group: 'People',
    app: 'library',
    icon: 'people',
    size: 'medium',
    sizes: ['small', 'medium', 'large'],
    needs: ['people'],
  },
  meetWith: {
    label: 'Who you meet with most',
    note: 'Hours in meetings with each person. An all-hands is not time with anybody.',
    group: 'People',
    app: 'calendar',
    icon: 'people',
    size: 'medium',
    sizes: ['medium', 'large'],
    needs: ['meetings'],
    windows: [7, 30, 90],
    window: 30,
  },
  writeTo: {
    label: 'Who you write to most',
    note: 'People you sent mail to, and how much they sent back.',
    group: 'People',
    app: 'mail',
    icon: 'mail',
    size: 'medium',
    sizes: ['medium', 'large'],
    needs: ['mail'],
    windows: [7, 30, 90],
    window: 30,
  },

  // ── Everything else ────────────────────────────────────────────────
  //
  // One card per shelf for the three shelves people are most often part-way
  // through, each reading the same `going` read "What's going on" does and
  // keeping its own shelf's rows -- see `NOW_SHELVES`.
  readingNow: {
    label: 'Reading now',
    note: 'The books you have started and not finished, with how far in you are.',
    group: 'Everything else',
    app: 'library',
    icon: 'book',
    size: 'small',
    sizes: ['small', 'medium', 'large'],
    needs: ['going'],
  },
  watchingNow: {
    label: 'Watching now',
    note: 'The shows you are part-way through.',
    group: 'Everything else',
    app: 'library',
    icon: 'film',
    size: 'small',
    sizes: ['small', 'medium', 'large'],
    needs: ['going'],
  },
  playingNow: {
    label: 'Playing now',
    note: 'The games you are in the middle of.',
    group: 'Everything else',
    app: 'library',
    icon: 'play',
    size: 'small',
    sizes: ['small', 'medium', 'large'],
    needs: ['going'],
  },
  shelves: {
    label: 'On the shelves',
    note: 'What you are part-way through, and what you finished this year.',
    group: 'Everything else',
    app: 'library',
    icon: 'book',
    size: 'small',
    sizes: ['small', 'medium'],
    needs: ['library'],
  },
  reading: {
    label: 'Reading',
    note: 'What is unread in the Feed, folder by folder, and the few stories to open first.',
    group: 'Everything else',
    app: 'feed',
    icon: 'rss',
    size: 'medium',
    sizes: ['medium', 'large'],
    needs: ['news'],
  },
  writing: {
    label: 'What you have written',
    note: 'Entries and words over the last few weeks.',
    group: 'Everything else',
    app: 'journal',
    icon: 'quote',
    size: 'medium',
    sizes: ['small', 'medium', 'large'],
    needs: ['entries'],
    windows: [30, 90, 365],
  },
  recentNotes: {
    label: 'Notes you touched last',
    note: 'The handful you were most recently in.',
    group: 'Everything else',
    app: 'notes',
    icon: 'pencil',
    size: 'medium',
    sizes: ['medium', 'large'],
    needs: ['notes'],
  },
} as const satisfies Record<string, WidgetSpec>

export type WidgetType = keyof typeof WIDGETS

export const WIDGET_TYPES = Object.keys(WIDGETS) as WidgetType[]

/**
 * A catalogue entry, widened to the interface it satisfies.
 *
 * Always this rather than `WIDGETS[type]` directly, and the reason is worth
 * knowing: `WIDGETS` is `as const satisfies`, so every entry keeps its
 * *literal* type. An entry with no `windows` key has no such property to
 * read, and one whose `needs` is `['today']` has an array that cannot be
 * asked whether it includes `'trackerDays'` -- the argument narrows to
 * `never`. Both are compile errors at the point of use rather than anything
 * to do with the data, and both go away by reading through the interface.
 */
export function specOf(type: WidgetType): WidgetSpec {
  return WIDGETS[type]
}

// Not exported: nothing outside this module needs to ask what a widget type
// is, only `parseLayout` below, which is guarding a page read back from disk.
function isWidgetType(value: unknown): value is WidgetType {
  return typeof value === 'string' && value in WIDGETS
}

/**
 * Widget types a stored page may still name, and what each is called now.
 *
 * A layout outlives the build that wrote it, and a rename that dropped the
 * card would empty a corner of somebody's page for no reason they could
 * see. Areas were roles until November 2026.
 */
const RENAMED: Record<string, WidgetType> = {
  roleBalance: 'areaBalance',
  roleShare: 'areaShare',
}

// ── An arranged page ─────────────────────────────────────────────────────

export interface Widget {
  /**
   * This card, as distinct from another card of the same type.
   *
   * Two heatmaps of two different trackers is the ordinary case, so the type
   * cannot be the key -- and a `{#each}` keyed on the index would re-use a
   * chart's DOM for a different tracker's data when a widget above it moved.
   */
  id: string
  type: WidgetType
  size: WidgetSize
  /** Which tracker, where the widget is about one. Null until it is chosen. */
  subject: string | null
  /** The window in days, where the widget has one. */
  days: number | null
}

/**
 * The page a vault starts with.
 *
 * Deliberately short, and every card on it answers something about *today*.
 * A first run that opened on twelve charts would be a page nobody reads and
 * nobody edits, because the cost of removing eleven things is higher than the
 * cost of ignoring the tab. Adding is the easy direction; the Overview's Add
 * button exists to make it two clicks.
 *
 * Ordered as the page you open to see what matters right now reads: what is
 * on and what is due, side by side; then the mail that is waiting on you,
 * three narrow cards to a row; then what you are in the middle of. Each row
 * closes on six columns, so nothing on a first run is stretched to fill a
 * gap -- but for the last card, which is alone on its row on purpose and
 * takes the width (`fillRows`).
 */
export function defaultLayout(): Widget[] {
  return [
    widget('karma'),
    widget('todayEvents'),
    widget('todayTasks'),
    widget('importantMail'),
    widget('mailFollowUps'),
    widget('snoozedMail'),
    widget('whatsGoingOn'),
    widget('habitsToday'),
  ]
}

/** One widget of a type, at the size the catalogue says it arrives at. */
export function widget(type: WidgetType, subject: string | null = null, id?: string): Widget {
  const spec = specOf(type)
  return {
    id: id ?? `${type}:1`,
    type,
    size: spec.size,
    subject,
    days: spec.windows ? (spec.window ?? spec.windows[spec.windows.length - 1] ?? null) : null,
  }
}

/**
 * The next free id for a type, given what is already on the page.
 *
 * Counted rather than random: `crypto.randomUUID` needs a secure context the
 * packaged webview does not always provide -- the same reason ids for real
 * records are minted in Rust -- and a layout in `localStorage` has no vault
 * to ask. Deterministic also makes the rules below testable without a stub.
 */
export function nextId(list: Widget[], type: WidgetType): string {
  const taken = new Set(list.map((w) => w.id))
  for (let n = 1; ; n++) {
    const id = `${type}:${n}`
    if (!taken.has(id)) return id
  }
}

/**
 * Put a new widget on the page: in front of `before`, or at the end when that
 * is null or no longer on the page.
 *
 * The same "in front of" rule a drop uses (`reorderWidget`), so choosing a
 * place for a new card and dragging an old one there land it identically.
 */
export function addWidget(
  list: Widget[],
  type: WidgetType,
  subject: string | null = null,
  before: string | null = null,
): Widget[] {
  const card = widget(type, subject, nextId(list, type))
  const at = before === null ? -1 : list.findIndex((w) => w.id === before)
  return at < 0 ? [...list, card] : [...list.slice(0, at), card, ...list.slice(at)]
}

/**
 * Cards added to the catalogue after pages were already arranged, each put
 * at the top of a page once -- so somebody whose page predates it is shown
 * it rather than having to find it behind Add -- and never again, so one
 * they remove stays removed. Newest last.
 *
 * The nine after "What's going on" arrived together, Karma first, as the
 * cards that make the Overview a home page -- and in the order listed, which is the order
 * they land in at the top of the page: see `introduce`.
 */
export const INTRODUCED: WidgetType[] = [
  'whatsGoingOn',
  'karma',
  'todayEvents',
  'todayTasks',
  'importantMail',
  'mailFollowUps',
  'snoozedMail',
  'readingNow',
  'watchingNow',
  'playingNow',
]

/**
 * `list` with every card in `INTRODUCED` that `seen` does not name put at
 * the top -- unless the page already has one -- and the types now seen.
 *
 * The newcomers go on as one block, in `INTRODUCED`'s order, ahead of
 * whatever was there. Not one at a time to the top, which is what this did
 * while there was only ever one: prepending each in turn stacks them in
 * reverse, so a batch written down as "events, tasks, mail" arrived as
 * "mail, tasks, events" -- the list's own order read upside down, and
 * nowhere obvious to look for why.
 *
 * `arranged` is false for a page nobody has arranged -- nothing stored, so
 * `list` is `defaultLayout()` -- and then nothing is added and everything
 * counts as seen. Those cards were made for pages that predate them; a new
 * vault's page already *is* the page as it stands now, and topping it with
 * every card it chose to leave out would be a default nobody chose.
 */
export function introduce(
  list: Widget[],
  seen: string[],
  arranged = true,
): { widgets: Widget[]; seen: WidgetType[] } {
  if (!arranged) return { widgets: list, seen: [...INTRODUCED] }
  const arrivals: Widget[] = []
  for (const type of INTRODUCED) {
    if (seen.includes(type) || list.some((w) => w.type === type)) continue
    arrivals.push(widget(type, null, nextId([...arrivals, ...list], type)))
  }
  // The very same array when nothing arrived, which is how the store knows
  // there is nothing to write back.
  return { widgets: arrivals.length > 0 ? [...arrivals, ...list] : list, seen: [...INTRODUCED] }
}

export function removeWidget(list: Widget[], id: string): Widget[] {
  return list.filter((w) => w.id !== id)
}

/**
 * Move a widget along the page by `by` places.
 *
 * Clamped rather than wrapped. A card at the top that jumped to the bottom
 * because somebody pressed the arrow once too often is a page that has to be
 * put back by hand, and there is no undo anywhere in this application.
 */
export function moveWidget(list: Widget[], id: string, by: number): Widget[] {
  const from = list.findIndex((w) => w.id === id)
  if (from < 0) return list
  const to = Math.min(list.length - 1, Math.max(0, from + by))
  if (to === from) return list
  const out = [...list]
  const [moved] = out.splice(from, 1)
  out.splice(to, 0, moved!)
  return out
}

/** Drop `id` in front of `before`, or at the end when that is null. */
export function reorderWidget(list: Widget[], id: string, before: string | null): Widget[] {
  if (id === before) return list
  const from = list.findIndex((w) => w.id === id)
  if (from < 0) return list
  const out = [...list]
  const [moved] = out.splice(from, 1)
  const at = before === null ? out.length : out.findIndex((w) => w.id === before)
  out.splice(at < 0 ? out.length : at, 0, moved!)
  return out
}

export function setSize(list: Widget[], id: string, size: WidgetSize): Widget[] {
  return list.map((w) => (w.id === id ? { ...w, size } : w))
}

export function setSubject(list: Widget[], id: string, subject: string | null): Widget[] {
  return list.map((w) => (w.id === id ? { ...w, subject } : w))
}

export function setDays(list: Widget[], id: string, days: number): Widget[] {
  return list.map((w) => (w.id === id ? { ...w, days } : w))
}

/**
 * Column spans, widened so every row reaches the grid's far edge.
 *
 * A row whose widgets add up to less than `totalCols` is a hole: three
 * small cards and a lone medium one three rows down both leave empty grid
 * tracks nobody put there on purpose, because the person who dragged a card
 * to "Half" was choosing a size, not choosing to leave the other half of
 * the row blank. The browser's own auto-placement already decides which
 * widgets share a row -- `grid-auto-flow` is left at its sparse default, so
 * a widget that does not fit what is left of the row wraps rather than
 * backfilling an earlier gap -- so the rows below are found by walking the
 * list in order and reproducing that same wrap rule, not by reading the
 * DOM.
 *
 * Only the last widget on a row grows, and by exactly the row's leftover
 * columns. Splitting the slack across every card on the row would mean a
 * two-column widget silently resizing itself depending on who it happens to
 * share a row with, which is not what "Narrow" meant when somebody picked
 * it -- and it would make the same widget a different width on two
 * refreshes if a card above it changed size and shifted the wrap.
 *
 * Returns a span per widget id, in the same column units `SPAN` already
 * uses, so a caller sets `--span` from this map exactly the way it would
 * set it from `SPAN` directly. `totalCols` is how many columns the grid is
 * drawing right now -- six, or three and one at the narrower breakpoints,
 * where the style block caps every span at the grid's width. Each span is
 * capped the same way here, or a narrow window would be packed as if it were
 * six columns wide: two small cards no longer fit side by side in three, so
 * each sat alone on its row beside an empty column.
 */
export function fillRows(widgets: Widget[], totalCols = 6): Map<string, number> {
  const spanOf = (w: Widget) => Math.min(SPAN[w.size], totalCols)
  const rows: Widget[][] = []
  let row: Widget[] = []
  let used = 0
  for (const w of widgets) {
    const span = spanOf(w)
    if (row.length > 0 && used + span > totalCols) {
      rows.push(row)
      row = []
      used = 0
    }
    row.push(w)
    used += span
  }
  if (row.length > 0) rows.push(row)

  const out = new Map<string, number>()
  for (const r of rows) {
    const rowUsed = r.reduce((sum, w) => sum + spanOf(w), 0)
    const leftover = totalCols - rowUsed
    r.forEach((w, i) => out.set(w.id, i === r.length - 1 ? spanOf(w) + leftover : spanOf(w)))
  }
  return out
}

/** Everything the page needs fetched, once each. */
export function needsOf(list: Widget[]): Set<Need> {
  const out = new Set<Need>()
  for (const w of list) for (const need of specOf(w.type).needs) out.add(need)
  return out
}

/**
 * The longest window any *tracker* widget on the page asks for, in days.
 *
 * Restricted to the widgets that actually read `trackerDays`, and that is the
 * whole point of the function rather than a refinement of it. Every window is
 * a number of days, but they are not all windows over the same thing: "What
 * you have written" counts entries and arrives set to a year, and taking the
 * plain maximum meant putting it beside a heatmap widened the *readings*
 * query from four months to twelve. The cost was three things at once -- a
 * year of rows fetched instead of a third of one, a heatmap captioned "four
 * months" drawing fifty-three columns, and every streak's hit rate computed
 * over a year -- none of which anybody asked for by adding a card about
 * their journal.
 */
export function trackerWindow(list: Widget[], floor = 120): number {
  return list.reduce(
    (most, w) =>
      specOf(w.type).needs.includes('trackerDays') ? Math.max(most, w.days ?? 0) : most,
    floor,
  )
}

/**
 * Every window the cards reading `need` ask for, shortest first, once each.
 *
 * Not the longest alone, the way `trackerWindow` and the writing card's
 * window are. Those draw a series a day at a time, and a shorter card can
 * cut its days out of a longer read. A ranked list cannot: the top senders of
 * a quarter are not the top senders of its last week. So each distinct window
 * is its own read -- and on a page where every mail card is left at the month
 * it arrives at, that is still one.
 */
export function activityWindows(list: Widget[], need: 'mail' | 'meetings'): number[] {
  const out = new Set<number>()
  for (const w of list) {
    if (w.days !== null && specOf(w.type).needs.includes(need)) out.add(w.days)
  }
  return [...out].sort((a, b) => a - b)
}

/**
 * Order items so the one with the earliest time sorts first, and one with no
 * time at all -- a task proposed with a due date but no due *time* -- sorts
 * after every timed one rather than before it, which `''` would otherwise do
 * by plain string comparison. What "Plan for tomorrow" draws its rows in.
 *
 * `timeOf` answers a `HH:MM` (or any lexically comparable clock string) or
 * `null`; ties keep their original order, since `Array.prototype.sort` is
 * stable and nothing here needs a second key to break one.
 */
export function byTimeOrLast<T>(items: T[], timeOf: (item: T) => string | null): T[] {
  return [...items].sort((a, b) => {
    const ta = timeOf(a)
    const tb = timeOf(b)
    if (ta === null) return tb === null ? 0 : 1
    if (tb === null) return -1
    return ta < tb ? -1 : ta > tb ? 1 : 0
  })
}

// ── The home cards ───────────────────────────────────────────────────────
//
// The ordering rules behind the cards that make this page somewhere to start
// the day: today's tasks and events, the mail waiting, what is on the shelf.
// Here rather than in the component for the reason everything else in this
// file is: an agenda that put four o'clock before nine, or a snoozed list
// that put next month first, would draw without complaint and be wrong.
// Each takes the least shape it reads, so the tests can hand it plain
// objects rather than build whole records.

/**
 * Which shelf each "… now" card is about, by the shelf's stable `slug` --
 * never its name, which the person can change, and which the builtin TV
 * shelf has already been renamed under once ("Series" to "TV Shows").
 */
export const NOW_SHELVES = {
  readingNow: 'book',
  watchingNow: 'series',
  playingNow: 'game',
} as const satisfies Partial<Record<WidgetType, string>>

/** The shelf slug a widget type is about, or `null` for one that is not a shelf card. */
export function nowShelf(type: WidgetType): string | null {
  return (NOW_SHELVES as Partial<Record<WidgetType, string>>)[type] ?? null
}

/**
 * Today's list, from tasks due today or earlier: whatever slipped first,
 * oldest due date at the top, then today's by the clock with untimed ones
 * after the timed -- `byTimeOrLast`'s rule, applied first so the date sort
 * after it (stable) keeps it within each day. Each row says whether it is
 * overdue, so the card can mark it without doing date arithmetic in markup.
 *
 * A task with no due date, or one due after `today`, is not today's and is
 * dropped rather than trusted to the query that fetched it: the list can
 * outlive midnight on a page left open.
 */
export function tasksForToday<T extends { dueDate?: string | null; dueTime?: string | null }>(
  tasks: T[],
  today: string,
): { task: T; overdue: boolean }[] {
  return byTimeOrLast(
    tasks.filter((t) => !!t.dueDate && t.dueDate <= today),
    (t) => t.dueTime ?? null,
  )
    .sort((a, b) => (a.dueDate! < b.dueDate! ? -1 : a.dueDate! > b.dueDate! ? 1 : 0))
    .map((task) => ({ task, overdue: task.dueDate! < today }))
}

/**
 * The "Important mail" list, from one listing per category: each thread
 * once -- a thread can be in two Inboxes, and the two listings are asked
 * separately -- kept if it is unread or somebody is waiting on your reply,
 * newest first. A read thread nobody is waiting on is mail already dealt
 * with, however important it was filed as.
 */
export function importantThreads<
  T extends {
    id: string
    lastDate: string
    unreadCount: number
    followUp?: { kind: string } | null
  },
>(lists: T[][]): T[] {
  const seen = new Set<string>()
  const out: T[] = []
  for (const thread of lists.flat()) {
    if (seen.has(thread.id)) continue
    seen.add(thread.id)
    if (thread.unreadCount > 0 || thread.followUp?.kind === 'reply') out.push(thread)
  }
  return out.sort((a, b) => Date.parse(b.lastDate) - Date.parse(a.lastDate))
}

/**
 * Snoozed threads, the one coming back soonest first -- the order that
 * matters for something you put off, where newest-first (the order the
 * listing arrives in) would put next month's above this afternoon's. One
 * with no `snoozedUntil` goes last; it should not be in a snoozed listing at
 * all, but a list that sorted it first would be led by the one row with
 * nothing to say.
 */
export function soonestBack<T extends { snoozedUntil?: string | null }>(threads: T[]): T[] {
  const at = (t: T) => (t.snoozedUntil ? Date.parse(t.snoozedUntil) : Number.POSITIVE_INFINITY)
  return [...threads].sort((a, b) => at(a) - at(b))
}

/**
 * A day's events as an agenda reads: all-day ones first, then by when each
 * starts. Compared as instants, not strings: two calendars in two time zones
 * write the same moment with two different offsets.
 */
export function dayAgenda<T extends { allDay: boolean; start: string }>(events: T[]): T[] {
  return [...events].sort(
    (a, b) => Number(b.allDay) - Number(a.allDay) || Date.parse(a.start) - Date.parse(b.start),
  )
}

/**
 * Where an event stands against `now`, in milliseconds: over, under way, or
 * still to come. An all-day event is never "under way" -- the whole day is
 * not one meeting, and highlighting it from midnight would leave the actual
 * meeting in progress looking like the second thing on -- and never over
 * until the day is, which is when it leaves the card anyway.
 */
export function eventPhase(
  event: { allDay: boolean; start: string; end: string },
  now: number,
): 'past' | 'now' | 'later' {
  if (event.allDay) return 'later'
  if (Date.parse(event.end) <= now) return 'past'
  if (Date.parse(event.start) <= now) return 'now'
  return 'later'
}

// ── The Weather widget ───────────────────────────────────────────────────

/**
 * Which weather icon (see `icons.ts`) a condition's own words call for.
 *
 * `describe` in `karmayoga_core::weather` is where the words come from --
 * WMO code 63 arrives as `"moderate rain"`, never as a bare number -- so
 * this only has to recognise a double handful of phrases, not fifty codes.
 * An unrecognised one (a wording this has not seen, or `"unknown (WMO code
 * n)"` for a code nobody has mapped yet) draws `cloud`, which is the right
 * shape for everything between clear and a storm anyway.
 */
export function weatherIcon(condition: string): IconName {
  const c = condition.trim().toLowerCase()
  if (c.includes('thunder')) return 'storm'
  if (c.includes('snow')) return 'snow'
  if (c.includes('drizzle') || c.includes('rain')) return 'rain'
  if (c.includes('fog')) return 'fog'
  if (c === 'clear sky' || c === 'mainly clear') return 'sun'
  return 'cloud'
}

/** The six AQI bands, worst last -- what `--aqi-1` through `--aqi-6` in
 *  `theme.css` are keyed on. */
export type AqiTier = 1 | 2 | 3 | 4 | 5 | 6

const US_AQI_TIER: Record<string, AqiTier> = {
  good: 1,
  moderate: 2,
  'unhealthy for sensitive groups': 3,
  unhealthy: 4,
  'very unhealthy': 5,
  hazardous: 6,
}

const EUROPEAN_AQI_TIER: Record<string, AqiTier> = {
  good: 1,
  fair: 2,
  moderate: 3,
  poor: 4,
  'very poor': 5,
  'extremely poor': 6,
}

/**
 * Which of the six AQI bands `category` -- as `karmayoga_core::air_quality`
 * spells it, read case-insensitively -- falls into on `scale`. `null` for a
 * word this does not recognise, so a widget can fall back to no colour at
 * all rather than guess at one.
 *
 * Takes the scale rather than matching the word alone because the same word
 * is a different band on each one: European "Moderate" is its third band,
 * American "Moderate" is its second. See `karmayoga_core::air_quality`'s
 * `us_category`/`european_category`, which this mirrors in order rather
 * than in the band thresholds themselves -- those stay in the core, as
 * tested pure functions of their own, and the service already did the
 * number-to-word step before this ever sees it.
 */
export function aqiTier(scale: 'us' | 'european', category: string | null): AqiTier | null {
  if (!category) return null
  const table = scale === 'us' ? US_AQI_TIER : EUROPEAN_AQI_TIER
  return table[category.trim().toLowerCase()] ?? null
}

// ── Storage ──────────────────────────────────────────────────────────────

export const LAYOUT_KEY = 'karmayoga.overview.layout'

/**
 * Read a stored layout back, forgivingly.
 *
 * Every field is checked and every bad one is replaced rather than rejected,
 * because the alternative is a page that empties itself. This string outlives
 * builds: a widget removed from the catalogue, a size renamed, a hand-edited
 * `localStorage`, or simply a newer version of the app writing a field this
 * one has never heard of -- none of those is a reason to throw somebody's
 * page away. Anything unrecognisable is dropped card by card, and a layout
 * with nothing left in it is treated as "never arranged one", which is what
 * `null` means to the caller.
 */
export function parseLayout(raw: string | null): Widget[] | null {
  if (!raw) return null
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    return null
  }
  if (!Array.isArray(parsed)) return null

  const out: Widget[] = []
  for (const row of parsed) {
    if (typeof row !== 'object' || row === null) continue
    const { type: written, size, subject, days, id } = row as Record<string, unknown>
    const type = typeof written === 'string' ? (RENAMED[written] ?? written) : written
    if (!isWidgetType(type)) continue
    const spec = specOf(type)
    // Ids are trusted from storage, so a hand-edited file can repeat one --
    // and two cards sharing a key is a Svelte error rather than a wonky
    // layout. Minted here against what has already been read, so the first
    // card keeps the id it was written with and only the clash is renamed.
    const kept = out.some((w) => w.id === id) || typeof id !== 'string' || !id
    out.push({
      id: kept ? nextId(out, type) : id,
      type,
      // A size the catalogue no longer offers for this type falls back to
      // the one it arrives at, rather than to a span that would tile wrong.
      size: isSize(size) && (spec.sizes as readonly string[]).includes(size) ? size : spec.size,
      subject: typeof subject === 'string' && subject ? subject : null,
      days:
        typeof days === 'number' && spec.windows?.includes(days)
          ? days
          : (widget(type).days ?? null),
    })
  }
  return out.length > 0 ? out : null
}

function isSize(value: unknown): value is WidgetSize {
  return typeof value === 'string' && (WIDGET_SIZES as readonly string[]).includes(value)
}
