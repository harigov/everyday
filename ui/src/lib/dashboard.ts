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
  /** Roles and goals. */
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
] as const
export type Need = (typeof NEEDS)[number]

/** Which picker, if the widget is about one particular thing. */
export type Subject = 'tracker'

/** The headings the catalogue is filed under, in the order it draws them. */
export const GROUPS = ['Today', 'Your time', 'Goals', 'Habits', 'Tasks', 'Everything else'] as const
export type Group = (typeof GROUPS)[number]

export interface WidgetSpec {
  /** The card's heading, and the catalogue row's label. */
  label: string
  /** One line in the catalogue: what the widget answers. */
  note: string
  group: Group
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
}

/**
 * Every widget the Overview can draw.
 *
 * Ordered by the heading it falls under rather than alphabetically, because
 * this object *is* the catalogue's order.
 */
export const WIDGETS = {
  // ── Today ──────────────────────────────────────────────────────────
  onNow: {
    label: 'On now',
    note: 'What is being tracked this minute, and a way to stop it.',
    group: 'Today',
    icon: 'clock',
    size: 'small',
    sizes: ['small', 'medium'],
    needs: ['today'],
  },
  dueToday: {
    label: 'Due today',
    note: 'Open tasks dated today, and whether any have already slipped.',
    group: 'Today',
    icon: 'sun',
    size: 'small',
    sizes: ['small', 'medium'],
    needs: ['taskStats'],
  },
  recordedToday: {
    label: 'Recorded today',
    note: 'Hours you have logged so far, against the hours you set aside.',
    group: 'Today',
    icon: 'day',
    size: 'small',
    sizes: ['small', 'medium'],
    needs: ['today'],
  },
  habitsToday: {
    label: "Today's habits",
    note: 'Every tracker as a chip, ticked or not, with its streak.',
    group: 'Today',
    icon: 'tick',
    size: 'large',
    sizes: ['medium', 'large'],
    needs: ['trackerDays'],
  },

  // ── Your time ──────────────────────────────────────────────────────
  roleBalance: {
    label: 'Where the week went',
    note: 'One bar per role: what you recorded, against what you planned.',
    group: 'Your time',
    icon: 'week',
    size: 'large',
    sizes: ['medium', 'large'],
    needs: ['balance', 'purpose'],
  },
  weekInWords: {
    label: 'The week, in words',
    note: 'Two sentences about what changed, written by the quick model from the totals above.',
    group: 'Your time',
    icon: 'sparkle',
    size: 'medium',
    sizes: ['medium', 'large'],
    needs: ['balance', 'purpose'],
  },
  roleShare: {
    label: 'Share of your week',
    note: 'The same hours as a single bar, so the proportions are the point.',
    group: 'Your time',
    icon: 'grid',
    size: 'medium',
    sizes: ['medium', 'large'],
    needs: ['balance', 'purpose'],
  },
  neglected: {
    label: 'Gone quiet',
    note: 'Roles with a goal still open and nothing recorded against it.',
    group: 'Your time',
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
    icon: 'target',
    size: 'medium',
    sizes: ['medium', 'large'],
    needs: ['purpose', 'goalActivity'],
  },
  goalTally: {
    label: 'Goals, counted',
    note: 'How many you are pursuing, and how many you have finished.',
    group: 'Goals',
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
    icon: 'refresh',
    size: 'medium',
    sizes: ['medium', 'large'],
    needs: ['trackerDays'],
  },
  habitHeatmap: {
    label: 'A habit, day by day',
    note: 'Four months of one tracker, a square a day.',
    group: 'Habits',
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
    icon: 'check',
    size: 'medium',
    sizes: ['medium', 'large'],
    needs: ['weekTasks'],
  },
  taskTally: {
    label: 'Tasks, counted',
    note: 'Open, finished, and how many hours have gone into them.',
    group: 'Tasks',
    icon: 'list',
    size: 'small',
    sizes: ['small', 'medium'],
    needs: ['taskStats'],
  },

  // ── Everything else ────────────────────────────────────────────────
  shelves: {
    label: 'On the shelves',
    note: 'What you are part-way through, and what you finished this year.',
    group: 'Everything else',
    icon: 'book',
    size: 'small',
    sizes: ['small', 'medium'],
    needs: ['library'],
  },
  writing: {
    label: 'What you have written',
    note: 'Entries and words over the last few weeks.',
    group: 'Everything else',
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
 */
export function defaultLayout(): Widget[] {
  return [
    widget('dueToday'),
    widget('recordedToday'),
    widget('onNow'),
    widget('habitsToday'),
    widget('roleBalance'),
    widget('goalProgress'),
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
    days: spec.windows ? (spec.windows[spec.windows.length - 1] ?? null) : null,
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

// ── Storage ──────────────────────────────────────────────────────────────

export const LAYOUT_KEY = 'everyday.overview.layout'

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
    const { type, size, subject, days, id } = row as Record<string, unknown>
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
