// State for the calendar app.
//
// A sibling of `state.svelte.ts` and `todo.svelte.ts`. The comment at the top
// of the todo store said the calendar would be a file of its own rather than
// a third of one very large one, and this is it.
//
// What makes this store different from its siblings: it is the only one that
// *reads across* domains. The grid draws five things at once —
//
//   external events    other people's calendars, read-only          (calendar)
//   planned blocks     what you intend to do                             (task)
//   actual blocks      what you did                                      (task)
//   tasks              drawn on the day they are due                     (task)
//   journal entries    a mark on the days you wrote something         (journal)
//
// — and only one of them is new. That is the whole design: a calendar is a
// *view* over records that already existed, not a fourth place to put an
// appointment. Everything here that writes, writes a `TimeBlock`.

import { api } from './api'
import { SvelteMap } from 'svelte/reactivity'
import { ask, quick } from './quick.svelte'
import { Autosave } from './autosave'
import { app, errorMessage, handle, isLocked } from './state.svelte'
import { todo } from './todo.svelte'
import { durationMinutes, formatValue } from './tracker'
import { tracking } from './tracking.svelte'
import {
  MIN_BLOCK_MINUTES,
  SNAP_MINUTES,
  addDays,
  addMonths,
  daysFrom,
  instantAt,
  isoDate,
  localeWeekStart,
  minutesBetween,
  monthGrid,
  offsetInDay,
  snap,
  startOfWeek,
  todayIso,
} from './time'
import type {
  BlockKind,
  BlockSubject,
  CalendarEvent,
  CalendarId,
  CalendarInfo,
  EntrySummary,
  Project,
  ProjectId,
  Reading,
  RoleId,
  Task,
  TaskId,
  TimeBlock,
  Tracker,
} from './types'
import { TASK_STATUSES, isOpen } from './types'

/** How often the backend is asked to refresh calendars whose interval elapsed. */
const SYNC_POLL_MS = 5 * 60_000
/** Default length of a block created by a click rather than a drag. */
export const DEFAULT_BLOCK_MINUTES = 60
/** Where the running timer is remembered across a reload. */
const TIMER_KEY = 'everyday.calendar.timer'
/** Statuses with work left in them. Matches `TaskStatus::is_open` in the core. */
const OPEN_STATUSES = TASK_STATUSES.filter(isOpen)

export type View = 'day' | 'week' | 'month'

/** Which of the two halves of a time block the grid is showing. */
export type Layer = 'both' | 'planned' | 'actual'

/**
 * Anything the grid can draw in a time slot, reduced to what drawing needs.
 *
 * A single shape for five different records, because the grid's job — pack
 * overlapping things into lanes, position them, colour them — is identical
 * for all of them, and five nearly-identical layout passes is how a calendar
 * view becomes unmaintainable.
 *
 * Note which readings reach here and which do not. A tracked *quantity of
 * time* — "45 min run at 07:00" — is a rectangle from 07:00 to 07:45 and
 * belongs in the lane packer beside the meetings it clashes with. Everything
 * else a tracker records is a moment, not a shape: a 500 mg dose has no
 * length, and drawing it as a fifteen-minute box would be an invention. Those
 * are `Mark`s, drawn as pips on a rail down the side of the day.
 */
export interface Slot {
  key: string
  kind: 'planned' | 'actual' | 'event' | 'reading'
  title: string
  subtitle: string
  color: string
  /** Minutes from the start of the day column it is drawn in. */
  start: number
  end: number
  /** Only your own blocks can be dragged; an event belongs to a server. */
  movable: boolean
  block?: TimeBlock
  event?: CalendarEvent
  /** Set on a `reading` slot: something recorded, and what recorded it. */
  reading?: Reading
  tracker?: Tracker
  /** A cancelled meeting, drawn struck through rather than hidden. */
  cancelled?: boolean
  /** Marked free by the publisher: drawn faintly, does not read as a clash. */
  free?: boolean
  /** The timer is running in this one. */
  live?: boolean
}

/**
 * A reading drawn as a moment rather than as a shape.
 *
 * `minute` is minutes from the column's midnight, or `null` for a reading
 * that never knew its time of day — those go in the all-day band, because
 * "sometime on Tuesday" is a fact about the day and pinning it to a minute
 * would be a fact nobody recorded.
 */
export interface Mark {
  key: string
  minute: number | null
  tracker: Tracker
  reading: Reading
  /** `Ibuprofen · 400 mg`, for the tooltip and the month row. */
  label: string
}

/**
 * What is being tracked at this moment.
 *
 * Held here and in local storage rather than as a record in the vault: see
 * the timer section of the store for why an unfinished block is not written.
 */
export interface Timer {
  /** RFC 3339 instant the tracking started. */
  since: string
  subject: BlockSubject
  /** What to call it, when the subject does not name itself. */
  title: string
}

/** Read the timer back after a reload, ignoring anything malformed. */
function readTimer(): Timer | null {
  try {
    const raw = localStorage.getItem(TIMER_KEY)
    if (!raw) return null
    const parsed = JSON.parse(raw) as Timer
    return parsed && typeof parsed.since === 'string' && Number.isFinite(Date.parse(parsed.since))
      ? parsed
      : null
  } catch {
    // A note to self is not worth failing a launch over.
    return null
  }
}

/** What the detail panel is showing. */
export type Selection = { kind: 'block'; id: string } | { kind: 'event'; id: string } | null

class CalendarState {
  // ── what is on screen ────────────────────────────────────────────────
  view = $state<View>('week')
  /** The day the view is anchored on, `YYYY-MM-DD`. */
  anchor = $state<string>(todayIso())
  layer = $state<Layer>('both')
  weekStart = $state<number>(localeWeekStart())

  // ── what has been loaded for it ──────────────────────────────────────
  calendars = $state<CalendarInfo[]>([])
  events = $state<CalendarEvent[]>([])
  blocks = $state<TimeBlock[]>([])
  /** Tasks with a due date in range, drawn in the all-day band. */
  dueTasks = $state<Task[]>([])
  /**
   * Every open task, for the unscheduled rail.
   *
   * Loaded here rather than read out of the todo store, which holds whatever
   * *that* app's current scope last asked for -- and is empty entirely if
   * the todo app has not been opened this session. Two stores over one vault
   * is fine; one store reading another's view state is not.
   */
  openTasks = $state<Task[]>([])
  /** Projects, for the colour of a block and the "book against" pickers. */
  projects = $state<Project[]>([])
  /** Days with a journal entry on them, so the calendar can say so. */
  entryDays = $state<Set<string>>(new Set())
  /**
   * Readings in the window, from every journal.
   *
   * Loaded whole and filtered at draw time rather than queried per tracker:
   * which trackers are drawn is decided by their *definitions*, which live
   * on the journals `app` already holds, and a query cannot ask about them
   * because the backend keeps them sealed. The window is a week or a month
   * of one clear index scan either way.
   */
  readings = $state<Reading[]>([])

  selection = $state<Selection>(null)
  loading = $state(false)
  syncing = $state(false)
  /** Set after a manual refresh, cleared on the next navigation. */
  syncNote = $state<string | null>(null)

  /**
   * What is being tracked right now, if anything.
   *
   * Note what this is *not*: a stored record. See the timer section below.
   */
  timer = $state<Timer | null>(null)
  /** Ticks once a second while the timer runs, so the elapsed figure moves. */
  now = $state<number>(Date.now())
  /**
   * The same clock, rounded down to the minute.
   *
   * The grid reads this and the elapsed readout reads `now`, so a running
   * timer redraws the digits once a second and the week once a minute --
   * rather than re-packing every lane in seven columns sixty times a minute
   * for a rectangle that grew by less than a pixel.
   */
  tick = $state<number>(Date.now())

  /** Blocks edited since the last write, and the timer that writes them. */
  #saves = new Autosave<string>((ids) => this.#writeBlocks(ids))
  #syncTimer: ReturnType<typeof setInterval> | null = null
  #clock: ReturnType<typeof setInterval> | null = null
  /**
   * The first load, once it has been asked for.
   *
   * A promise rather than a `started` flag, so a second caller *waits* for
   * the load instead of being waved through while it is still running.
   * `bookNow` from the tray is exactly that second caller -- the view has
   * only just mounted and started loading -- and waved through it wrote its
   * block into a `blocks` array that the load then replaced with the state
   * from before the write. The hour was on disk and not on the grid.
   */
  #started: Promise<void> | null = null

  constructor() {
    app.onLock(() => this.reset())
    app.onFlush(() => this.flush())
    // Read here rather than in `start`, because `start` runs when the
    // calendar view mounts and the timer is asked about before that: the
    // tray offers "track time" from launch, and a store that believed
    // nothing was running would offer to *start* one -- and `startTimer`
    // stops whatever it thinks is running first, which would drop the
    // session on disk without ever writing it as a block. Costs a
    // `localStorage` read; touches no vault and nothing decrypted.
    this.timer = readTimer()
  }

  reset() {
    if (this.#syncTimer) clearInterval(this.#syncTimer)
    if (this.#clock) clearInterval(this.#clock)
    this.#syncTimer = null
    this.#clock = null
    this.#saves.cancel()
    this.#started = null
    this.calendars = []
    this.events = []
    this.blocks = []
    this.dueTasks = []
    this.openTasks = []
    this.projects = []
    this.entryDays = new Set()
    this.readings = []
    this.selection = null
    this.syncNote = null
    // The timer is deliberately *not* cleared: it is a note to self held in
    // local storage, it names no decrypted content, and a lock taken during
    // a working session should not silently throw away what you were doing.
  }

  // ── lifecycle ────────────────────────────────────────────────────────

  /** First entry into the calendar. Idempotent, and awaitable by anyone. */
  async start() {
    this.#started ??= this.#start().catch(() => {
      // Never leave a rejected promise in the slot: every later caller would
      // inherit that one failure for the rest of the session. Forgetting it
      // is what lets the next entry into the app try again.
      this.#started = null
    })
    await this.#started
  }

  async #start() {
    const view = localStorage.getItem('everyday.calendar.view')
    if (view === 'day' || view === 'week' || view === 'month') this.view = view

    // The clock only runs while something needs it: a per-second re-render
    // of the whole grid for the sake of a "now" line nobody is watching is
    // exactly the sort of thing that shows up in a battery report.
    this.#startClock()
    await this.refresh()

    // Refreshing subscriptions is polled from here rather than driven by a
    // timer in the backend, so a locked vault is never fetched into and a
    // window nobody has opened never wakes the radio.
    void this.syncDue(false)
    this.#syncTimer = setInterval(() => void this.syncDue(false), SYNC_POLL_MS)
  }

  // ── the window on screen ─────────────────────────────────────────────

  /** The days the current view draws, in order. */
  get days(): string[] {
    switch (this.view) {
      case 'day':
        return [this.anchor]
      case 'week':
        return daysFrom(startOfWeek(this.anchor, this.weekStart), 7)
      case 'month':
        return monthGrid(this.anchor, this.weekStart)
    }
  }

  /** Inclusive bounds of everything loaded, `[from, to]`. */
  get range(): [string, string] {
    const days = this.days
    return [days[0]!, days[days.length - 1]!]
  }

  setView(view: View) {
    this.view = view
    localStorage.setItem('everyday.calendar.view', view)
    void this.refresh()
  }

  /** Move one page forward or back, in the unit the view is made of. */
  step(direction: -1 | 1) {
    this.anchor =
      this.view === 'month'
        ? addMonths(this.anchor, direction)
        : addDays(this.anchor, direction * (this.view === 'week' ? 7 : 1))
    this.syncNote = null
    void this.refresh()
  }

  goto(iso: string) {
    this.anchor = iso
    this.syncNote = null
    void this.refresh()
  }

  goToday() {
    this.goto(todayIso())
  }

  /** Is `iso` inside the month the view is anchored on? Month view only. */
  inAnchorMonth(iso: string): boolean {
    return iso.slice(0, 7) === this.anchor.slice(0, 7)
  }

  // ── loading ──────────────────────────────────────────────────────────

  async refresh() {
    if (!app.supportsCalendar) return
    const [from, to] = this.range
    this.loading = true
    try {
      // One round of queries per navigation, all four in parallel. Each is a
      // date-range scan over a clear index column, so paging through a year
      // is cheap even on an encrypted vault.
      const [calendars, events, blocks, dueTasks, openTasks, projects, entries, readings] =
        await Promise.all([
          api.calendars(),
          api.events({ from, to, visibleOnly: true }),
          api.blocks({ from, to }),
          api.tasks({ dueFrom: from, dueTo: to, limit: 500 }),
          api.tasks({ statuses: OPEN_STATUSES, sort: 'dueAsc', limit: 300 }),
          api.projects(),
          api.entries({ from, to, sort: 'dateAsc', limit: 500 }),
          app.supportsTrackers ? api.readings({ from, to }) : Promise.resolve([]),
        ])
      this.calendars = calendars
      this.events = events
      this.blocks = blocks
      this.dueTasks = dueTasks
      this.openTasks = openTasks
      this.projects = projects
      this.entryDays = new Set(entries.map((e: EntrySummary) => e.localDate))
      this.readings = readings
      // A selection that has scrolled out of the window is not a selection.
      if (this.selection && !this.selected) this.selection = null
    } catch (e) {
      await handle(e)
    } finally {
      this.loading = false
    }
  }

  /**
   * Re-read only the readings.
   *
   * Called when the calendar is shown again, because readings are written in
   * the *journal* app -- ticking a chip under an entry -- and this store has
   * no way to hear about that. One index scan, rather than re-running the
   * whole eight-query navigation load for a tab switch.
   */
  async refreshReadings() {
    if (!app.supportsTrackers) return
    const [from, to] = this.range
    try {
      this.readings = await api.readings({ from, to })
    } catch (e) {
      if (isLocked(e)) await app.lock()
    }
  }

  /** Re-read only the blocks. What a write needs; skips four other queries. */
  private async refreshBlocks() {
    const [from, to] = this.range
    try {
      // A day early on the leading edge, because `blockSlots` promises that
      // a block running past midnight is drawn at the bottom of one day and
      // the top of the next -- and it can only keep that promise for a block
      // it has. A block is filed under the day it *starts*, and the query is
      // a range on that column, so the one spilling into the first day on
      // screen belongs to a day that is not. Nothing is needed at the other
      // end: a block starting on the last day is already loaded, and its
      // spill has no column to be drawn in.
      this.blocks = await api.blocks({ from: addDays(from, -1), to })
    } catch (e) {
      if (isLocked(e)) await app.lock()
    }
  }

  // ── what the grid draws ──────────────────────────────────────────────

  /**
   * The trackers this calendar is allowed to draw, by id.
   *
   * Opt-in per tracker rather than per kind, because the question is whether
   * the *time* on a reading is real. A migraine at 14:20 belongs on a grid;
   * "flossed", ticked at bedtime for the whole day, is a pin at an hour that
   * means nothing. The switch is in the journal's settings.
   */
  get drawnTrackers(): Map<string, Tracker> {
    const out = new Map<string, Tracker>()
    for (const tracker of tracking.trackers) {
      if (tracker.onCalendar && !tracker.archived) out.set(tracker.id, tracker)
    }
    return out
  }

  /** The calendar an event came from, for its colour and its name. */
  calendarOf(id: CalendarId): CalendarInfo | null {
    return this.calendars.find((c) => c.id === id) ?? null
  }

  projectOf(id: ProjectId | null | undefined): Project | null {
    return this.projects.find((p) => p.id === id) ?? null
  }

  /** The colour a block should be drawn in: its project's, or the accent. */
  colorOfBlock(block: TimeBlock): string {
    return this.colorOfSubject(block.subject)
  }

  colorOfSubject(subject: BlockSubject): string {
    const project =
      subject.type === 'project'
        ? this.projectOf(subject.id)
        : subject.type === 'task'
          ? this.projectOf(this.taskOf(subject.id)?.projectId ?? null)
          : null
    return project?.color ?? 'var(--accent)'
  }

  /** What to call a subject when nothing has given it a title of its own. */
  titleOfSubject(subject: BlockSubject): string {
    if (subject.type === 'task') return this.taskOf(subject.id)?.title ?? 'Task'
    if (subject.type === 'project') return this.projectOf(subject.id)?.name ?? 'Project'
    return 'Untitled'
  }

  /** A task the calendar knows about, from either of the lists it loads. */
  taskOf(id: TaskId): Task | null {
    return this.dueTasks.find((t) => t.id === id) ?? this.openTasks.find((t) => t.id === id) ?? null
  }

  /** What a block should be called: its own title, or its subject's name. */
  titleOfBlock(block: TimeBlock): string {
    return block.title.trim() || this.titleOfSubject(block.subject)
  }

  /**
   * Everything to draw in one day's column, laid out in minutes from its
   * midnight.
   *
   * All-day things are *not* here — they go in the band above the grid,
   * because an event with no time is not a shape in a timeline and drawing
   * it as a 24-hour bar buries the day underneath it.
   */
  slotsOn(iso: string): Slot[] {
    const out: Slot[] = []
    const dayStart = 0
    const dayEnd = 24 * 60

    if (this.layer !== 'actual') out.push(...this.blockSlots(iso, 'planned'))
    if (this.layer !== 'planned') out.push(...this.blockSlots(iso, 'actual'))
    // A reading is a record of something that happened, so it belongs with
    // the record layer and disappears under Plan. That falls out of what the
    // toggle already means rather than being a rule of its own.
    if (this.layer !== 'planned') out.push(...this.readingSlots(iso))
    const live = this.liveSlot(iso)
    if (live) out.push(live)

    for (const event of this.events) {
      if (event.allDay) continue
      const start = offsetInDay(event.start, iso)
      const end = offsetInDay(event.end, iso)
      // Clipped to this column: a meeting running past midnight appears at
      // the bottom of one day and the top of the next, which is what it did.
      if (end <= dayStart || start >= dayEnd) continue
      const calendar = this.calendarOf(event.calendarId)
      out.push({
        key: `event:${event.id}`,
        kind: 'event',
        title: this.tidyTitle(event.title),
        subtitle: event.location || calendar?.name || '',
        color: calendar?.color ?? 'var(--fg-subtle)',
        start: Math.max(dayStart, start),
        end: Math.min(dayEnd, end),
        movable: false,
        event,
        cancelled: event.status === 'cancelled',
        free: !event.busy,
      })
    }
    return out
  }

  /**
   * Readings that occupy a *length* of time, as rectangles.
   *
   * Only a quantity measured in time qualifies: 45 minutes of running
   * started at 07:00 is 07:00–07:45. `at` is the start rather than the end,
   * which is the reading the strip's time field offers and the one a person
   * means by "when did you do it".
   */
  private readingSlots(iso: string): Slot[] {
    const drawn = this.drawnTrackers
    const out: Slot[] = []
    for (const reading of this.readings) {
      if (!reading.at) continue
      const tracker = drawn.get(reading.trackerId)
      if (!tracker) continue
      const minutes = durationMinutes(tracker, reading.value)
      if (minutes === null) continue
      const start = offsetInDay(reading.at, iso)
      const end = start + minutes
      if (end <= 0 || start >= 24 * 60) continue
      out.push({
        key: `reading:${reading.id}`,
        kind: 'reading',
        title: tracker.name,
        subtitle: formatValue(tracker, reading.value),
        color: tracker.color,
        start: Math.max(0, start),
        end: Math.min(24 * 60, end),
        movable: false,
        reading,
        tracker,
      })
    }
    return out
  }

  /**
   * Readings drawn as moments: pips down the side of a day.
   *
   * Everything a tracker records that is not a length of time — a dose, a
   * severity, a habit ticked at the moment it happened.
   */
  marksOn(iso: string): Mark[] {
    if (this.layer === 'planned') return []
    const drawn = this.drawnTrackers
    const out: Mark[] = []
    for (const reading of this.readings) {
      if (reading.localDate !== iso || !reading.at) continue
      const tracker = drawn.get(reading.trackerId)
      if (!tracker || durationMinutes(tracker, reading.value) !== null) continue
      out.push({
        key: `mark:${reading.id}`,
        minute: offsetInDay(reading.at, iso),
        tracker,
        reading,
        label: `${tracker.name} · ${formatValue(tracker, reading.value)}`,
      })
    }
    return out.sort((a, b) => (a.minute ?? 0) - (b.minute ?? 0))
  }

  /**
   * Readings filed under a day with no time of day, for the all-day band.
   *
   * The band is where they belong and the reason `at` is optional at all: a
   * habit ticked while writing up last Tuesday is true of Tuesday and says
   * nothing about the minute it was typed.
   */
  untimedMarksOn(iso: string): Mark[] {
    if (this.layer === 'planned') return []
    const drawn = this.drawnTrackers
    return this.readings
      .filter((r) => r.localDate === iso && !r.at && drawn.has(r.trackerId))
      .map((reading) => {
        const tracker = drawn.get(reading.trackerId)!
        return {
          key: `mark:${reading.id}`,
          minute: null,
          tracker,
          reading,
          label: `${tracker.name} · ${formatValue(tracker, reading.value)}`,
        }
      })
  }

  private blockSlots(iso: string, kind: BlockKind): Slot[] {
    const out: Slot[] = []
    for (const block of this.blocks) {
      if (block.kind !== kind || block.allDay) continue
      const start = offsetInDay(block.start, iso)
      const end = offsetInDay(block.end, iso)
      // Clipped to this column, so a block running past midnight appears at
      // the bottom of one day and the top of the next.
      if (end <= 0 || start >= 24 * 60) continue
      out.push({
        key: `block:${block.id}`,
        kind,
        title: this.titleOfBlock(block),
        subtitle: this.subtitleOfBlock(block),
        color: this.colorOfBlock(block),
        start: Math.max(0, start),
        end: Math.min(24 * 60, Math.max(end, start + MIN_BLOCK_MINUTES)),
        movable: true,
        block,
      })
    }
    return out
  }

  /**
   * The block being tracked right now, drawn but not stored.
   *
   * It has no id, cannot be dragged and is not in `blocks`, because it does
   * not exist yet — see the timer section below for why.
   */
  private liveSlot(iso: string): Slot | null {
    const timer = this.timer
    if (!timer || this.layer === 'planned') return null
    const start = offsetInDay(timer.since, iso)
    const end = start + Math.max(0, (this.tick - Date.parse(timer.since)) / 60_000)
    if (end <= 0 || start >= 24 * 60) return null
    return {
      key: 'live',
      kind: 'actual',
      title: timer.title || this.titleOfSubject(timer.subject),
      subtitle: 'Tracking now',
      color: this.colorOfSubject(timer.subject),
      start: Math.max(0, start),
      end: Math.min(24 * 60, end),
      movable: false,
      live: true,
    }
  }

  private subtitleOfBlock(block: TimeBlock): string {
    if (block.subject.type === 'task') {
      const task = this.taskOf(block.subject.id)
      return this.projectOf(task?.projectId ?? null)?.name ?? ''
    }
    if (block.subject.type === 'project') return this.projectOf(block.subject.id)?.name ?? ''
    return block.notes.split('\n')[0] ?? ''
  }

  /** All-day events and multi-day ones, for the band above the grid. */
  allDayOn(iso: string): CalendarEvent[] {
    return this.events.filter(
      (e) => (e.allDay || e.localDate !== e.endDate) && e.localDate <= iso && e.endDate >= iso,
    )
  }

  /** Open tasks due on `iso`, for the band above the grid. */
  tasksOn(iso: string): Task[] {
    return this.dueTasks.filter((t) => t.dueDate === iso)
  }

  /** Was anything written in the journal on `iso`? */
  hasEntry(iso: string): boolean {
    return this.entryDays.has(iso)
  }

  /** Minutes booked on `iso`, split by plan and record. For the day header. */
  totalsOn(iso: string): { planned: number; logged: number } {
    let planned = 0
    let logged = 0
    for (const b of this.blocks) {
      if (b.localDate !== iso) continue
      const mins = minutesBetween(b.start, b.end)
      if (b.kind === 'actual') logged += mins
      else planned += mins
    }
    // Whatever is running counts towards its day as it happens, or the
    // header would read "nothing logged" through an afternoon of solid work.
    if (this.timer && isoDate(new Date(this.timer.since)) === iso) {
      logged += this.runningMinutes
    }
    return { planned, logged }
  }

  // ── the selection ────────────────────────────────────────────────────

  get selected(): TimeBlock | CalendarEvent | null {
    if (!this.selection) return null
    return this.selection.kind === 'block'
      ? (this.blocks.find((b) => b.id === this.selection!.id) ?? null)
      : (this.events.find((e) => e.id === this.selection!.id) ?? null)
  }

  select(slot: Slot | null) {
    // Two of the four kinds of slot have no panel to select into. A reading
    // is edited where it was recorded, under the day's entry, which is also
    // where the tracker that gives it meaning is named; and the live slot is
    // the timer drawing itself off the wall clock, with nothing written
    // until it is stopped. Selecting nothing is the honest answer to both,
    // and testing for what a slot *has* is what keeps this total for every
    // caller rather than one `!` away from a crash.
    if (!slot?.block && !slot?.event) return void (this.selection = null)
    this.selection = slot.block
      ? { kind: 'block', id: slot.block.id }
      : { kind: 'event', id: slot.event!.id }
  }

  // ── writing ──────────────────────────────────────────────────────────

  /**
   * Book time.
   *
   * The single write this app makes. Dragging a task onto Tuesday afternoon,
   * clicking an empty slot, and starting the timer are all this call with
   * different arguments, which is what keeps "the calendar has one kind of
   * record" true rather than aspirational.
   */
  async book(opts: {
    subject: BlockSubject
    day: string
    startMinutes: number
    minutes: number
    kind?: BlockKind
    title?: string
    select?: boolean
  }): Promise<TimeBlock | null> {
    try {
      const block = await api.newBlock({
        subject: opts.subject,
        start: instantAt(opts.day, opts.startMinutes),
        minutes: Math.max(0, Math.round(opts.minutes)),
        kind: opts.kind ?? 'planned',
      })
      if (opts.title) block.title = opts.title
      await api.saveBlock(block)
      this.blocks = [...this.blocks, block]
      if (opts.select !== false) this.selection = { kind: 'block', id: block.id }
      void todo.refreshStats()
      return block
    } catch (e) {
      await handle(e)
      return null
    }
  }

  /**
   * Book what a sentence describes.
   *
   * The quick model reads "lunch with Sam Thursday 1pm at the usual place";
   * everything after that is the ordinary `book`. Answers null and does
   * nothing when the sentence names no date -- an appointment with no day is
   * not an appointment, and guessing today would put somebody's Thursday
   * lunch on a Tuesday.
   *
   * An hour when no end was given, because that is what `bookNow` assumes
   * too and a block of unknown length has to be drawn as something.
   */
  /**
   * A readable title for a subscribed event.
   *
   * `[EXT] FW: Re: Weekly Sync // Zoom` is what a work feed actually
   * contains, and it is what the grid has to draw in a 90px column. This
   * answers the tidied title once the model has said, and the raw one until
   * then -- never a blank, and never a spinner in a calendar cell.
   *
   * Three things make it safe to do at all:
   *
   *   * **It is display only.** The event record is never written. The feed
   *     will be re-fetched and it is not ours to rewrite.
   *   * **It is cached on the raw string**, in memory. The same six meeting
   *     names recur every week forever, so a month of grid is a handful of
   *     requests rather than one per cell per render.
   *   * **It defaults off**, because the titles of somebody's meetings are
   *     the names of the people in them.
   */
  #titles = new SvelteMap<string, string>()
  #asking = new Set<string>()

  tidyTitle(raw: string): string {
    if (!raw.trim() || !quick.enabled('calendar.title')) return raw
    const known = this.#titles.get(raw)
    if (known !== undefined) return known || raw
    if (!this.#asking.has(raw)) {
      this.#asking.add(raw)
      void ask('calendar.title', () => api.quickEventTitle(raw)).then((tidy) => {
        // An empty answer is the common and correct one -- most titles are
        // already clean -- and it is cached as empty so it is asked once.
        this.#titles.set(raw, tidy ?? '')
      })
    }
    return raw
  }

  async bookFromSentence(line: string): Promise<TimeBlock | null> {
    /** `HH:MM` to minutes since midnight. The core already validated it. */
    const clockMinutes = (clock: string): number => {
      const [h = '0', m = '0'] = clock.split(':')
      return Number(h) * 60 + Number(m)
    }

    const draft = await ask('calendar.parse', () => api.quickEventFromLine(line))
    if (!draft?.date) return null
    await this.start()
    const start = draft.start ? clockMinutes(draft.start) : 9 * 60
    const minutes = draft.end ? Math.max(15, clockMinutes(draft.end) - start) : 60
    if (!this.days.includes(draft.date)) this.goto(draft.date)
    return this.book({
      subject: { type: 'adhoc' },
      day: draft.date,
      startMinutes: start,
      minutes,
      kind: 'planned',
      title: draft.location ? `${draft.title} — ${draft.location}` : draft.title,
    })
  }

  /**
   * Set an hour aside, starting on the next quarter. What Ctrl/Cmd N does.
   *
   * The same key that starts an entry in the journal and a task in the todo
   * app: begin the next thing. Here that is time on the calendar, and the
   * view moves to today if it was somewhere else, because booking an hour
   * you cannot see is indistinguishable from nothing happening.
   */
  async bookNow() {
    // The tray can ask for this a frame after the view mounted, with the
    // first load still in flight; booking into a list that is about to be
    // replaced loses the block from the grid. Already loaded, this is free.
    await this.start()
    const at = new Date()
    const start = snap(at.getHours() * 60 + at.getMinutes(), SNAP_MINUTES)
    if (!this.days.includes(todayIso())) this.goto(todayIso())
    await this.book({
      subject: { type: 'adhoc' },
      day: todayIso(),
      startMinutes: start,
      minutes: DEFAULT_BLOCK_MINUTES,
    })
  }

  /** Book a planned block for a task, defaulting to its own estimate. */
  async scheduleTask(id: TaskId, day: string, startMinutes: number) {
    const task = this.taskOf(id)
    return this.book({
      subject: { type: 'task', id },
      day,
      startMinutes,
      minutes: task?.estimateMinutes || DEFAULT_BLOCK_MINUTES,
    })
  }

  /**
   * Book a task into the first gap that will hold it.
   *
   * What the keyboard does instead of dragging. Dragging is the better
   * gesture and the one the rail is built around, but a feature only a mouse
   * can reach is a feature half the people using it do not have — so Enter
   * on a task in the rail finds it a slot rather than doing nothing.
   *
   * "First gap" means: today if today is on screen, otherwise the first day
   * of the window; from now, or from nine in the morning, whichever is
   * later; and skipping anything already booked or booked *for* you.
   */
  async scheduleNext(id: TaskId): Promise<TimeBlock | null> {
    const task = this.taskOf(id)
    const length = task?.estimateMinutes || DEFAULT_BLOCK_MINUTES
    const iso = this.days.includes(todayIso()) ? todayIso() : this.days[0]!

    const at = new Date()
    const floor =
      iso === todayIso() ? Math.max(9 * 60, snap(at.getHours() * 60 + at.getMinutes())) : 9 * 60

    // Every day in the window, so a task still lands somewhere when today is
    // full -- and the last resort is the end of the window rather than
    // nothing happening.
    for (const day of this.days.slice(this.days.indexOf(iso))) {
      const taken = this.slotsOn(day)
        .map((s) => [s.start, s.end] as const)
        .sort((a, b) => a[0] - b[0])
      let start = day === iso ? floor : 9 * 60
      for (const [from, to] of taken) {
        if (start + length <= from) break
        if (to > start) start = snap(to, SNAP_MINUTES)
      }
      // Nothing after eight in the evening: a slot nobody will use is not a
      // slot, and pushing work into the night is not this app's decision.
      if (start + length <= 20 * 60) return this.scheduleTask(id, day, start)
    }
    return this.scheduleTask(id, iso, floor)
  }

  /**
   * Apply `changes` to a block and queue the write.
   *
   * Mutated in place, like a task in the todo app, so the grid, the detail
   * panel and the day totals all redraw from the same object without a
   * reconciliation pass.
   */
  patch(id: string, changes: Partial<TimeBlock>) {
    const block = this.blocks.find((b) => b.id === id)
    if (!block) return
    Object.assign(block, changes)
    block.updatedAt = new Date().toISOString()
    this.#saves.touch(id)
  }

  /**
   * Move a block to a new day and time, keeping its length.
   *
   * `localDate` moves with it, and has to: it is the clear column the week
   * query scans, so a block whose instants said Tuesday and whose filed day
   * still said Monday would vanish from both.
   */
  moveBlock(id: string, day: string, startMinutes: number) {
    const block = this.blocks.find((b) => b.id === id)
    if (!block) return
    const length = minutesBetween(block.start, block.end)
    this.patch(id, {
      start: instantAt(day, startMinutes),
      end: instantAt(day, startMinutes + length),
      localDate: day,
    })
  }

  /** Change a block's length, holding its start. */
  resizeBlock(id: string, minutes: number) {
    const block = this.blocks.find((b) => b.id === id)
    if (!block) return
    const start = offsetInDay(block.start, block.localDate)
    this.patch(id, {
      end: instantAt(block.localDate, start + Math.max(MIN_BLOCK_MINUTES, minutes)),
    })
  }

  flush(): Promise<void> {
    return this.#saves.flush()
  }

  /**
   * One `save_block` per edited block.
   *
   * Not a batch like the task store's: a block is a single record and the
   * backend has no plural command for it, because nothing in the calendar
   * renumbers a column the way dragging a card across a board does.
   */
  async #writeBlocks(ids: ReadonlySet<string>) {
    const pending = this.blocks.filter((b) => ids.has(b.id))
    try {
      for (const block of pending) {
        await api.saveBlock($state.snapshot(block))
      }
      void todo.refreshStats()
    } catch (e) {
      await handle(e, () => this.refreshBlocks())
      // Rethrown so `Autosave` requeues these blocks -- see `#writeTasks`.
      throw e
    }
  }

  async removeBlock(id: string) {
    this.blocks = this.blocks.filter((b) => b.id !== id)
    this.#saves.forget(id)
    if (this.selection?.kind === 'block' && this.selection.id === id) this.selection = null
    try {
      await api.deleteBlock(id)
      void todo.refreshStats()
    } catch (e) {
      await handle(e, () => this.refreshBlocks())
    }
  }

  /** Turn a plan into a record, or back. What "I actually did this" is. */
  toggleKind(id: string) {
    const block = this.blocks.find((b) => b.id === id)
    if (!block) return
    this.patch(id, { kind: block.kind === 'planned' ? 'actual' : 'planned' })
  }

  /**
   * Copy a planned block into an actual one covering the same slot.
   *
   * The common case at the end of a session: it went roughly to plan. Two
   * rows rather than one edited row, because the point of the pair is that
   * they can be compared, and a plan overwritten by its outcome cannot be.
   */
  async logAsDone(id: string) {
    const block = this.blocks.find((b) => b.id === id)
    if (!block || block.kind !== 'planned') return
    await this.book({
      subject: $state.snapshot(block.subject),
      day: block.localDate,
      startMinutes: offsetInDay(block.start, block.localDate),
      minutes: minutesBetween(block.start, block.end),
      kind: 'actual',
      title: block.title,
    })
  }

  // ── the timer ────────────────────────────────────────────────────────
  //
  // "What am I doing right now" is a note in local storage, and *one* actual
  // time block written when you stop. Not a block written on start and
  // closed on stop, which is the obvious design and is wrong in two ways:
  //
  //   * closing the window mid-session leaves a zero-length block on disk
  //     that nothing will ever tidy up, and
  //   * an unfinished record is a lie about the past. A block that says
  //     "09:00 to 09:00" is not a shorter version of what happened; it is a
  //     claim that nothing did.
  //
  // While it runs, the block on the grid is synthetic -- drawn from the
  // wall clock, not from storage -- so a two-hour session costs one write.

  /** Minutes the timer has been running. */
  get runningMinutes(): number {
    if (!this.timer) return 0
    return Math.max(0, Math.round((this.now - Date.parse(this.timer.since)) / 60_000))
  }

  /** Start tracking. Anything already running is written out first. */
  async startTimer(subject: BlockSubject, title = '') {
    await this.stopTimer()
    this.timer = {
      since: new Date().toISOString(),
      subject: $state.snapshot(subject),
      title,
    }
    localStorage.setItem(TIMER_KEY, JSON.stringify(this.timer))
    this.now = Date.now()
    this.tick = this.now
    this.#startClock()
  }

  /** Stop tracking, writing what happened as a single actual block. */
  async stopTimer() {
    const timer = this.timer
    this.timer = null
    localStorage.removeItem(TIMER_KEY)
    this.#startClock()
    if (!timer) return

    const since = new Date(timer.since)
    const minutes = Math.round((Date.now() - since.getTime()) / 60_000)
    // A timer stopped inside the minute it was started is a misclick, not a
    // record of anything.
    if (minutes < 1) return

    const written = await this.book({
      // Filed under the day it *started*: an evening session that runs past
      // midnight belongs to the evening, and the block's own instants say
      // where it ended.
      subject: timer.subject,
      day: isoDate(since),
      startMinutes: since.getHours() * 60 + since.getMinutes(),
      minutes,
      kind: 'actual',
      title: timer.title,
      select: false,
    })
    if (!written) {
      // The write failed -- most likely the vault locked itself while the
      // work was going on. Put the timer back rather than swallowing an
      // afternoon: pressing stop again after unlocking will save it.
      this.timer = timer
      localStorage.setItem(TIMER_KEY, JSON.stringify(timer))
      this.#startClock()
    }
  }

  /**
   * Keep the elapsed readout moving, without loading a window.
   *
   * The Overview can draw the running timer on a card, and can be the first
   * thing on screen after a launch -- before the calendar view has mounted
   * and called `start`. The clock is otherwise only started there, so the
   * figure would sit frozen at whatever the wall clock said when this module
   * was imported, which reads as a timer that has quietly stopped.
   */
  watchClock() {
    this.#startClock()
  }

  /**
   * One second while something is being timed, half a minute otherwise.
   *
   * A per-second re-render of the whole grid for the sake of a "now" line
   * nobody is watching is exactly the sort of thing that turns up in a
   * battery report.
   */
  #startClock() {
    if (this.#clock) clearInterval(this.#clock)
    const period = this.timer ? 1_000 : 30_000
    this.#clock = setInterval(() => {
      this.now = Date.now()
      // The grid only cares about whole minutes; see `tick`.
      if (this.now - this.tick >= 60_000) this.tick = this.now
    }, period)
  }

  // ── unscheduled work ─────────────────────────────────────────────────

  /**
   * Open tasks with nothing booked against them in this window.
   *
   * The right-hand rail's list, and the thing you drag onto the grid.
   * Deliberately not "every open task": a plan you have already made is not
   * a thing to be nagged about, so anything with a planned block in view
   * drops off the list the moment it is scheduled.
   */
  get unscheduled(): Task[] {
    const booked = new Set(
      this.blocks
        .filter((b) => b.kind === 'planned')
        .map((b) => (b.subject.type === 'task' ? b.subject.id : null))
        .filter(Boolean) as TaskId[],
    )
    const [, to] = this.range
    return (
      this.openTasks
        .filter((t) => !booked.has(t.id))
        // Everything due inside the window, plus a fortnight's grace either
        // side of it, plus everything undated -- which is the backlog, and the
        // whole reason this rail is worth dragging from.
        .filter((t) => !t.dueDate || t.dueDate <= addDays(to, 14))
        .slice(0, 40)
    )
  }

  // ── subscriptions ────────────────────────────────────────────────────

  /**
   * Subscribe to a feed. Returns `null` on success, or why it failed.
   *
   * The failure is *returned* rather than pushed into `app.error`, unlike
   * every other write in the interface. It belongs beside the address field
   * that caused it -- a typo in a URL is answered by fixing the URL, and a
   * dialog that closes and posts its complaint over the whole window is one
   * you have to re-open with the address typed in again.
   */
  async subscribe(name: string, url: string, color: string): Promise<string | null> {
    return this.add(() => api.subscribeCalendar({ name: name.trim(), url: url.trim(), color }))
  }

  /** The same, for a `.ics` file the browser read for us. */
  async importFile(
    name: string,
    label: string,
    color: string,
    ics: string,
  ): Promise<string | null> {
    return this.add(() => api.importCalendar({ name: name.trim(), label, color, ics }))
  }

  private async add(run: () => Promise<CalendarInfo>): Promise<string | null> {
    this.syncing = true
    try {
      const added = await run()
      this.calendars = [...this.calendars, added]
      await this.refresh()
      this.syncNote = `${added.name}: ${added.events} ${added.events === 1 ? 'event' : 'events'}.`
      return null
    } catch (e) {
      if (isLocked(e)) {
        await app.lock()
        return null
      }
      return errorMessage(e)
    } finally {
      this.syncing = false
    }
  }

  /**
   * Refresh one feed, now.
   *
   * Apart from `syncDue` because that one belongs to the timer: it walks
   * every subscription and honours each one's interval, which is not what
   * was asked for by somebody pointing at a single calendar. A failure is
   * recorded on the subscription by the backend, so the reload puts the
   * warning beside the calendar it belongs to -- which is where this app
   * says a broken feed is reported, rather than over the whole window.
   */
  async syncOne(id: CalendarId) {
    if (!app.supportsCalendar || this.syncing) return
    if (app.status?.writable === false) return
    const subscription = this.calendarOf(id)
    if (!subscription || subscription.origin.type !== 'url') return
    this.syncing = true
    try {
      const report = await api.syncCalendar(id)
      await this.refresh()
      this.syncNote = `Refreshed ${subscription.name}, ${report.events} events.`
    } catch (e) {
      if (isLocked(e)) return void (await app.lock())
      this.syncNote = `Could not refresh ${subscription.name}.`
      await this.refresh()
    } finally {
      this.syncing = false
    }
  }

  /** Refresh every feed. `force` ignores each one's interval. */
  async syncDue(force: boolean) {
    if (!app.supportsCalendar || this.syncing) return
    // Nothing a sync fetches can be stored on a read-only vault, so the
    // background pass would be network traffic for its own sake.
    if (app.status?.writable === false) return
    if (!this.calendars.some((c) => c.origin.type === 'url')) return
    this.syncing = true
    try {
      const reports = await api.syncDueCalendars(force)
      if (reports.length > 0) await this.refresh()
      if (force) {
        const total = reports.reduce((sum, r) => sum + r.events, 0)
        this.syncNote =
          reports.length === 0
            ? 'Nothing to refresh.'
            : `Refreshed ${reports.length} ${reports.length === 1 ? 'calendar' : 'calendars'}, ${total} events.`
      }
    } catch (e) {
      // A feed that is down is recorded on the calendar it belongs to and
      // shown beside it. It is not an error over the whole application.
      if (isLocked(e)) await app.lock()
    } finally {
      this.syncing = false
    }
  }

  async toggleVisible(id: CalendarId) {
    const calendar = this.calendars.find((c) => c.id === id)
    if (!calendar) return
    calendar.visible = !calendar.visible
    try {
      await api.saveCalendar($state.snapshot(calendar))
      await this.refresh()
    } catch (e) {
      await handle(e)
    }
  }

  async setCalendarColor(id: CalendarId, color: string) {
    const calendar = this.calendars.find((c) => c.id === id)
    if (!calendar) return
    calendar.color = color
    try {
      await api.saveCalendar($state.snapshot(calendar))
    } catch (e) {
      await handle(e)
    }
  }

  /**
   * Say which role a feed serves.
   *
   * A role and not a purpose: a work calendar is work, and the forty
   * meetings on it are not each yours to file under an outcome. One click
   * here attributes a year of somebody else's claims on your time, which is
   * the cheapest large win in the balance report.
   */
  async setCalendarRole(id: CalendarId, roleId: RoleId | null) {
    const calendar = this.calendars.find((c) => c.id === id)
    if (!calendar) return
    calendar.roleId = roleId
    try {
      await api.saveCalendar($state.snapshot(calendar))
    } catch (e) {
      await handle(e)
    }
  }

  async unsubscribe(id: CalendarId) {
    this.calendars = this.calendars.filter((c) => c.id !== id)
    this.events = this.events.filter((e) => e.calendarId !== id)
    try {
      await api.deleteCalendar(id)
    } catch (e) {
      await handle(e, () => this.refresh())
    }
  }

  /** Projects worth offering in a picker: everything not archived. */
  get liveProjects(): Project[] {
    return this.projects.filter((p) => p.status !== 'archived')
  }
}

export const calendar = new CalendarState()
