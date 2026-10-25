// State for the calendar app.
//
// A sibling of `state.svelte.ts` and `todo.svelte.ts`. The comment at the top
// of the todo store said the calendar would be a file of its own rather than
// a third of one very large one, and this is it.
//
// What makes this store different from its siblings: it is the only one that
// *reads across* domains. The grid draws five things at once —
//
//   events             account calendars and subscribed feeds       (calendar)
//   planned blocks     what you intend to do                             (task)
//   actual blocks      what you did                                      (task)
//   tasks              drawn on the day they are due                     (task)
//   journal entries    a mark on the days you wrote something         (journal)
//
// — and only one of them is new. That is still most of the design: a
// calendar is a *view* over records that already existed, not a fourth place
// to keep an appointment.
//
// What changed is where an appointment of your own can live. On this
// computer it is a `TimeBlock`, and everything here that writes locally
// writes one. On an account's calendar -- Google, Microsoft, a CalDAV server
// -- it is written to *that server* (`saveDraft`, `saveEdit`, `deleteEvent`)
// and comes back the way every event does, through the calendar's own sync.
// Nothing here edits a stored event in place: the server's copy is the
// event, and the vault's is a reading of it. Which of the two a new event
// becomes is the default calendar's say -- see `newEvent`.

import { api } from './api'
import type { DayMove } from './day-move'
import { dayHeading, timeOfDay } from './format'
import { SvelteMap, SvelteSet } from 'svelte/reactivity'
import { notify } from './notify.svelte'
import { seen, type Showing } from './onscreen'
import { ask, quick } from './quick.svelte'
import { Autosave } from './autosave'
import { pref } from './prefs'
import { edited, proposals, recordAs } from './proposals.svelte'
import { carryRule, dayIn, matchPreset, midnightIn, sameRecurrence } from './recurrence'
import { app, errorMessage, handle, isLocked, quietly } from './state.svelte'
import { latest } from './store/latest'
import { optimisticPatch } from './store/optimistic-patch'
import { optimisticRemove } from './store/optimistic-remove'
import { guardedRefresh } from './store/refresh'
import { todo } from './todo.svelte'
import { durationMinutes, formatValue } from './tracker'
import { tracking } from './tracking.svelte'
import {
  MIN_BLOCK_MINUTES,
  SNAP_MINUTES,
  addDays,
  addMonths,
  blockKindFor,
  daysApart,
  daysFrom,
  instantAt,
  isoDate,
  localeWeekStart,
  minutesBetween,
  monthGrid,
  offsetInDay,
  snap,
  startOfDay,
  startOfWeek,
  todayIso,
} from './time'
import type {
  AccountId,
  BlockId,
  BlockSubject,
  CalendarEvent,
  CalendarId,
  CalendarInfo,
  EditableEvent,
  EntrySummary,
  EventDraft,
  EventId,
  EventScope,
  Project,
  ProjectId,
  Proposal,
  ProposalId,
  Reading,
  Recurrence,
  RoleId,
  SeriesScope,
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

const viewPref = pref<View | null>(
  'everyday.calendar.view',
  (raw) => (raw === 'day' || raw === 'week' || raw === 'month' ? raw : null),
  null,
)

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
  kind: 'block' | 'event' | 'reading'
  title: string
  subtitle: string
  color: string
  /** Minutes from the start of the day column it is drawn in. */
  start: number
  end: number
  /**
   * Can be dragged to another time: your own blocks, and an account
   * calendar's events where this vault may write -- see `canMove`.
   */
  movable: boolean
  /** An event drawn where it was dragged, while its server is told. */
  saving?: boolean
  block?: TimeBlock
  event?: CalendarEvent
  /**
   * Set when this slot is a pending `block` proposal, not a real one.
   *
   * `block` is still filled in -- with the record the proposal would save --
   * so the grid's existing packing, colouring and drag maths all work
   * unchanged; only the drop at the end of a drag differs, and only because
   * there is no row on disk yet to move.
   */
  proposal?: Proposal
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

/** The machine's own zone: what a new event is quoted in unless it came with one. */
function localZone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone
}

/**
 * What the detail panel is showing.
 *
 * `draft` is a new event not yet written anywhere -- see `NewEvent`. It has
 * no id because there is no record behind it yet, and leaving it (Escape, a
 * click on something else) is how it is thrown away.
 */
export type Selection =
  { kind: 'block'; id: string } | { kind: 'event'; id: string } | { kind: 'draft' } | null

/**
 * A new event being written, before it exists anywhere.
 *
 * Held in the store rather than in the editor, because two things draw it:
 * the editor in the rail, and the grid, which shows it as a dashed slot at
 * the time it would take -- and dragging that slot moves the draft's times,
 * which the editor then reads back.
 */
export interface NewEvent {
  /** The account calendar it will be written to; `null` is this computer's own time. */
  calendarId: CalendarId | null
  draft: EventDraft
  /**
   * A block of your own this draft is moving onto an account calendar. It
   * is deleted once the event has landed there and not before, so a save
   * that fails -- or a draft that is cancelled -- loses nothing.
   */
  fromBlock?: BlockId
}

/**
 * An account calendar's event being changed, from the moment its server is
 * asked for it. `draft` is the editor's working copy; `event.draft` is what
 * the server said, kept apart so "was the repeat changed?" has an answer.
 */
export type EventEdit =
  | { id: EventId; status: 'loading' }
  | { id: EventId; status: 'failed'; error: string }
  | { id: EventId; status: 'ready'; event: EditableEvent; draft: EventDraft }

/**
 * Where an account event that was dragged is drawn until its server has
 * the move: its new instants, and the days they cover for the all-day
 * band. See `moveEvent`.
 */
type EventPlace = Pick<CalendarEvent, 'start' | 'end' | 'localDate' | 'endDate'>

/**
 * What a move is waiting to hear before it is sent -- which of a series it
 * means, or that its guests are to be told. Shaped for `ChoiceDialog`.
 */
export interface MoveQuestion {
  title: string
  detail: string
  options: { label: string; danger?: boolean; run: () => void }[]
  cancel: () => void
}

/** Where `event` is drawn when it runs from `start` to `end`. */
function placeOf(event: CalendarEvent, start: string, end: string): EventPlace {
  // An all-day event ends at the midnight after its last day -- or, as a
  // sync stores it, a second before -- and a timed one on the day it ends.
  const last = event.allDay ? new Date(Date.parse(end) - 1).toISOString() : end
  const localDate = dayIn(start, event.tz)
  const endDate = dayIn(last, event.tz)
  return { start, end, localDate, endDate: endDate < localDate ? localDate : endDate }
}

/** "Thursday 23 October, 10:30 – 11:00", for a question about a move. */
function whenOf(draft: EventDraft): string {
  if (draft.allDay) return `${dayHeading(startOfDay(dayIn(draft.start, draft.tz)))}, all day`
  const start = new Date(draft.start)
  return `${dayHeading(start)}, ${timeOfDay(start)} – ${timeOfDay(new Date(draft.end))}`
}

/**
 * `at` moved `days` days on, at the same time of day: on this machine's
 * clock, the one the grid is drawn on, or in `tz` for an all-day event,
 * whose days are its zone's.
 */
function shiftDays(at: string, days: number, tz?: string | null): string {
  if (!tz) {
    // `setDate` keeps the wall-clock time across a change of clocks.
    const moved = new Date(at)
    moved.setDate(moved.getDate() + days)
    return moved.toISOString()
  }
  const day = dayIn(at, tz)
  const into = Date.parse(at) - Date.parse(midnightIn(day, tz))
  return new Date(Date.parse(midnightIn(addDays(day, days), tz)) + into).toISOString()
}

class CalendarState {
  // ── what is on screen ────────────────────────────────────────────────
  view = $state<View>('week')
  /** The day the view is anchored on, `YYYY-MM-DD`. */
  anchor = $state<string>(todayIso())
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

  #selection = $state<Selection>(null)
  /** The new event open in the rail and drawn on the grid. See `NewEvent`. */
  draft = $state<NewEvent | null>(null)
  /** The account event open for changing, if one is. See `EventEdit`. */
  editing = $state<EventEdit | null>(null)
  /** Account events dragged somewhere and not yet back from their servers, by id. */
  moves = new SvelteMap<EventId, EventPlace>()
  /** Questions moves are waiting on, oldest first. See `#askToMove`. */
  #questions = $state.raw<MoveQuestion[]>([])
  /** Events a move found to be somebody else's meeting. They stay put. */
  #theirs = new SvelteSet<EventId>()
  /**
   * The pending `block` proposal open in the rail, by id -- not the
   * `Proposal` itself, for the reason `todo`'s `#selectedProposalId` gives:
   * so answering it anywhere closes this pane too. Mutually exclusive with
   * `selection`.
   */
  #selectedProposalId = $state<ProposalId | null>(null)
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
  /** `patch`'s assign-stamp-queue, once. See `store/optimistic-patch.ts`. */
  #patcher = optimisticPatch<string, TimeBlock>(
    (id) => this.blocks.find((b) => b.id === id),
    this.#saves,
  )
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
  /**
   * Which navigation is the current one. See `refresh`.
   *
   * Missing until now: paging quickly -- `step`, `goto` and `setView` each
   * fire their own `refresh` without waiting for the last one -- let an
   * older batch of events and blocks land after a newer one and sit on
   * screen showing the wrong day until the next navigation happened to fix
   * it. This closes that race the way the todo and library stores already
   * close theirs.
   */
  #generation = latest()
  /** Has the calendar list been read since the last unlock? See `loadCalendars`. */
  #calendarsKnown = false
  /** Bumped by every lock, so a read that set off before one cannot land after it. */
  #locks = 0

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
    // This window's own accept does not come back as a change event -- see
    // `proposals.svelte.ts` -- so the store that just gained a block tells
    // itself to reload.
    proposals.onAccepted('block', () => this.refresh())
  }

  /**
   * What the rail is showing.
   *
   * A setter rather than a plain field because moving it is also how an
   * unsaved draft and an open edit are let go. Every way of looking at
   * something else -- a click on another slot, Escape, a menu's "Show
   * details", a lock -- is already an assignment here, and a draft that
   * survived them would go on being drawn on the grid beside whatever had
   * replaced it in the rail, waiting to be saved by nobody.
   */
  get selection(): Selection {
    return this.#selection
  }

  /** The question on the sheet: the oldest move still waiting on one. */
  get moveQuestion(): MoveQuestion | null {
    return this.#questions[0] ?? null
  }

  set selection(next: Selection) {
    this.#selection = next
    if (next?.kind !== 'draft') this.draft = null
    if (next?.kind !== 'event' || next.id !== this.editing?.id) this.editing = null
  }

  reset() {
    if (this.#syncTimer) clearInterval(this.#syncTimer)
    if (this.#clock) clearInterval(this.#clock)
    this.#syncTimer = null
    this.#clock = null
    this.#saves.cancel()
    this.#started = null
    // Nothing loaded before the lock may land after it.
    this.#generation.next()
    // Including the spinner: a refresh caught by the lock leaves its `finally`
    // guarded by a generation this line has just moved past, so nothing else
    // will put it down.
    this.loading = false
    this.calendars = []
    this.events = []
    this.blocks = []
    this.dueTasks = []
    this.openTasks = []
    this.projects = []
    this.entryDays = new Set()
    this.readings = []
    this.selection = null
    this.#selectedProposalId = null
    this.syncNote = null
    // A move waiting on its question is not sent; one already on its way
    // finishes or fails behind the lock, and is not drawn either way.
    const asking = this.#questions
    this.#questions = []
    for (const question of asking) question.cancel()
    this.moves.clear()
    this.#theirs.clear()
    this.#calendarsKnown = false
    this.#locks += 1
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
    const view = viewPref.get()
    if (view) this.view = view
    // See `todo.start` for why this is idempotent and safe to ask twice.
    if (!proposals.loaded) await proposals.refresh()

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
    viewPref.set(view)
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
    await guardedRefresh(
      this.#generation,
      async (isCurrent) => {
        // One round of queries per navigation, all four in parallel. Each is
        // a date-range scan over a clear index column, so paging through a
        // year is cheap even on an encrypted vault.
        const [calendars, events, blocks, dueTasks, openTasks, projects, entries, readings] =
          await Promise.all([
            api.calendars(),
            api.events({ from, to, visibleOnly: true }),
            api.blocks({ from, to }),
            api.tasks({ dueFrom: from, dueTo: to, limit: 500 }),
            api.tasks({ statuses: OPEN_STATUSES, sort: 'dueAsc', limit: 300 }),
            api.projects(),
            // "You wrote something on this day" -- they did, not the
            // assistant, whose notes are dated ahead.
            api.entries({
              from,
              to,
              notJournal: app.assistantJournalId,
              sort: 'dateAsc',
              limit: 500,
            }),
            // Recorded only: a derived tracker is never drawn on the grid, and
            // working its days out would be a scan of every block to discard.
            app.supportsTrackers
              ? api.readings({ from, to, recordedOnly: true })
              : Promise.resolve([]),
          ])
        // Only the newest navigation may land. `step`, `goto` and `setView`
        // each fire this without waiting for the last call to answer, so
        // paging quickly -- or switching from week to month and back -- put
        // two of these in the air at once, and nothing before this
        // guaranteed they landed in the order they were asked for.
        if (!isCurrent()) return
        this.calendars = calendars
        this.#calendarsKnown = true
        this.events = events
        this.blocks = blocks
        this.dueTasks = dueTasks
        this.openTasks = openTasks
        this.projects = projects
        this.entryDays = new Set(entries.map((e: EntrySummary) => e.localDate))
        this.readings = readings
        // A selection that has scrolled out of the window is not a selection.
        // A draft has nothing to scroll out of: it is not stored, and paging
        // to check another day before saving it is a thing people do.
        if (this.selection && this.selection.kind !== 'draft' && !this.selected) {
          this.selection = null
        }
      },
      { setLoading: (v) => (this.loading = v), onError: (e) => handle(e) },
    )
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
      this.readings = await api.readings({ from, to, recordedOnly: true })
    } catch (e) {
      await quietly(e)
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
      await quietly(e)
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

  /** The account calendars a new event can be written to right now. */
  get writableCalendars(): CalendarInfo[] {
    return this.calendars.filter((c) => c.access === 'writable')
  }

  /**
   * Where a new event goes when nobody says: an account calendar, or `null`
   * for this computer's own time blocks.
   *
   * Only a calendar that can still be written to counts. One whose account
   * has lost the permission keeps its flag on disk -- signing in again
   * should put things back as they were -- but until then a new event goes
   * here, to this computer, rather than to a request bound to fail.
   */
  get defaultCalendar(): CalendarInfo | null {
    return this.calendars.find((c) => c.isDefault && c.access === 'writable') ?? null
  }

  /**
   * Whether this computer is a calendar new events can go to. The person
   * can switch it off while they go to an account calendar instead; the
   * mark rides on that default -- see `Calendar.thisComputerOff`.
   */
  get thisComputerOn(): boolean {
    return !this.calendars.some((c) => c.isDefault && c.thisComputerOff)
  }

  /**
   * Where a new event goes when nobody says, or why it can go nowhere: an
   * account calendar, `null` for this computer, or -- with this computer
   * switched off and the default unable to take it -- `false`, having said
   * so.
   */
  #newEventTarget(): CalendarInfo | null | false {
    const target = this.defaultCalendar
    if (target || this.thisComputerOn) return target
    const stale = this.calendars.find((c) => c.isDefault)
    notify.info('Nowhere to put it', {
      body: stale
        ? `New events go to ${stale.name}, which needs you to sign in to its account again.`
        : 'Choose a calendar for new events first.',
      reach: 'app',
    })
    return false
  }

  /** The colour a calendar choice is drawn in: its own, or the accent for this computer. */
  colorOfCalendar(id: CalendarId | null): string {
    return (id ? this.calendarOf(id)?.color : null) ?? 'var(--accent)'
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

    out.push(...this.blockSlots(iso))
    out.push(...this.ghostBlockSlots(iso))
    out.push(...this.readingSlots(iso))
    const live = this.liveSlot(iso)
    if (live) out.push(live)

    for (const event of this.shownEvents) {
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
        // Only when the whole of it is in this column: half of a night
        // flight dragged up the morning has no honest start to move. It
        // moves by its day in the band above instead.
        movable: start >= dayStart && end <= dayEnd && this.canMove(event),
        saving: this.moves.has(event.id),
        event,
        cancelled: event.status === 'cancelled',
        free: !event.busy,
      })
    }
    return out
  }

  /**
   * The events, each where it should be drawn: where it was dragged to,
   * while its server is told -- or where the editor open on it has it, so
   * a time typed in the rail moves the slot, as it does a new event's.
   *
   * Derived, so it is worked out once per change: `slotsOn` and `allDayOn`
   * read it for every day on screen -- forty-two of them in the month --
   * and the timer strip reads it every second.
   */
  shownEvents = $derived.by((): CalendarEvent[] => {
    const open = this.editing?.status === 'ready' && this.editing.event.own ? this.editing : null
    if (this.moves.size === 0 && !open) return this.events
    return this.events.map((event) => {
      const moved = this.moves.get(event.id)
      if (moved) return { ...event, ...moved }
      if (open?.id !== event.id || open.draft.allDay !== event.allDay) return event
      const { start, end } = open.draft
      if (start === event.start && end === event.end) return event
      return { ...event, ...placeOf(event, start, end) }
    })
  })

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

  private blockSlots(iso: string): Slot[] {
    const out: Slot[] = []
    for (const block of this.blocks) {
      if (block.allDay) continue
      const start = offsetInDay(block.start, iso)
      const end = offsetInDay(block.end, iso)
      // Clipped to this column, so a block running past midnight appears at
      // the bottom of one day and the top of the next.
      if (end <= 0 || start >= 24 * 60) continue
      out.push({
        key: `block:${block.id}`,
        kind: 'block',
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
   * Pending `block` proposals as ghosts, at the time they would book.
   *
   * Drawn through the same `Slot` shape as a real block -- packed into the
   * same lanes, coloured the way the block's subject would be -- so nothing
   * about the grid's layout has to know a ghost is not yet real.
   */
  private ghostBlockSlots(iso: string): Slot[] {
    const out: Slot[] = []
    for (const p of proposals.forKind('block')) {
      if (p.payload.type !== 'create') continue
      const block = recordAs(p, 'block')
      if (!block || block.allDay) continue
      const start = offsetInDay(block.start, iso)
      const end = offsetInDay(block.end, iso)
      if (end <= 0 || start >= 24 * 60) continue
      out.push({
        key: `proposal:${p.id}`,
        kind: 'block',
        title: this.titleOfBlock(block),
        subtitle: this.subtitleOfBlock(block),
        color: this.colorOfBlock(block),
        start: Math.max(0, start),
        end: Math.min(24 * 60, Math.max(end, start + MIN_BLOCK_MINUTES)),
        movable: true,
        block,
        proposal: p,
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
    if (!timer) return null
    const start = offsetInDay(timer.since, iso)
    const end = start + Math.max(0, (this.tick - Date.parse(timer.since)) / 60_000)
    if (end <= 0 || start >= 24 * 60) return null
    return {
      key: 'live',
      kind: 'block',
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
    return this.shownEvents.filter(
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

  /** Minutes booked on `iso`, for the day header. */
  minutesOn(iso: string): number {
    return this.#sumOn(iso, () => true)
  }

  /**
   * Minutes of `iso` that count as time spent: blocks the vault has
   * settled as such (see `blockKindFor`), and the timer. For the rail's tally.
   */
  spentOn(iso: string): number {
    return this.#sumOn(iso, (b) => b.kind === 'actual')
  }

  #sumOn(iso: string, counts: (b: TimeBlock) => boolean): number {
    let minutes = 0
    for (const b of this.blocks) {
      if (b.localDate === iso && !b.allDay && counts(b)) minutes += minutesBetween(b.start, b.end)
    }
    // Whatever is running counts towards its day as it happens, or the
    // header would read "nothing" through an afternoon of solid work.
    if (this.timer && isoDate(new Date(this.timer.since)) === iso) {
      minutes += this.runningMinutes
    }
    return minutes
  }

  // ── the selection ────────────────────────────────────────────────────

  get selected(): TimeBlock | CalendarEvent | null {
    const selection = this.selection
    if (!selection || selection.kind === 'draft') return null
    return selection.kind === 'block'
      ? (this.blocks.find((b) => b.id === selection.id) ?? null)
      : (this.events.find((e) => e.id === selection.id) ?? null)
  }

  /**
   * What is on screen, for the assistant -- see `onscreen.ts`.
   *
   * The dates are ISO rather than the grid's own headings: they are for a
   * model, which is told today's date in the same form, and "Tue 6" is a
   * heading that only reads correctly beside the rest of the grid.
   */
  get showing(): Showing {
    const [from, to] = this.range
    const view =
      this.view === 'day'
        ? `the day ${this.anchor}`
        : this.view === 'week'
          ? `the week ${from} to ${to}`
          : `the month ${this.anchor.slice(0, 7)}`
    const picked = this.selected
    const kind = this.selection?.kind
    return {
      view,
      open:
        picked && kind === 'block'
          ? seen('block', picked.id, picked.title)
          : picked && kind === 'event'
            ? seen('event', picked.id, picked.title)
            : [],
    }
  }

  select(slot: Slot | null) {
    // Two of the four kinds of slot have no panel to select into. A reading
    // is edited where it was recorded, under the day's entry, which is also
    // where the tracker that gives it meaning is named; and the live slot is
    // the timer drawing itself off the wall clock, with nothing written
    // until it is stopped. Selecting nothing is the honest answer to both,
    // and testing for what a slot *has* is what keeps this total for every
    // caller rather than one `!` away from a crash.
    this.#selectedProposalId = null
    if (!slot?.block && !slot?.event) return void (this.selection = null)
    this.selection = slot.block
      ? { kind: 'block', id: slot.block.id }
      : { kind: 'event', id: slot.event!.id }
  }

  /**
   * The pending block proposal open in the rail, or `null` once it has been
   * answered from anywhere -- see `#selectedProposalId`'s own doc.
   */
  get selectedProposal(): Proposal | null {
    return this.#selectedProposalId
      ? (proposals.pending.find((p) => p.id === this.#selectedProposalId) ?? null)
      : null
  }

  /** Open a pending block proposal in the rail, in place of a real one. */
  selectProposal(p: Proposal) {
    this.selection = null
    this.#selectedProposalId = p.id
  }

  closeProposal() {
    this.#selectedProposalId = null
  }

  /**
   * Move a ghost to a new time by accepting it there, rather than by writing
   * to a block that does not exist yet. `block` is the record the proposal
   * carries -- its length is kept, only the start moves.
   */
  async acceptGhostMove(p: Proposal, block: TimeBlock, day: string, startMinutes: number) {
    const length = minutesBetween(block.start, block.end)
    const start = instantAt(day, startMinutes)
    const end = instantAt(day, startMinutes + length)
    // `block` is read out of `proposals.pending`, a `$state` array, so it and
    // everything nested in it are reactive proxies -- `$state.snapshot` is
    // what turns the whole tree back into plain objects the transport this
    // call goes over can actually clone.
    return proposals.accept(
      p,
      edited('block', $state.snapshot({ ...block, start, end, localDate: day })),
    )
  }

  // ── writing ──────────────────────────────────────────────────────────

  /**
   * Book time on this computer.
   *
   * The one way a block of your own is made. Dragging a task onto Tuesday
   * afternoon, stopping the timer, "this is what happened", and a new event
   * when no account calendar is the default are all this call with
   * different arguments -- which is what keeps "your own time is one kind
   * of record" true rather than aspirational. An account calendar's event
   * is not this: see `newEvent`.
   */
  async book(opts: {
    subject: BlockSubject
    day: string
    startMinutes: number
    minutes: number
    title?: string
    select?: boolean
  }): Promise<TimeBlock | null> {
    try {
      const start = instantAt(opts.day, opts.startMinutes)
      const minutes = Math.max(0, Math.round(opts.minutes))
      const block = await api.newBlock({
        subject: opts.subject,
        start,
        minutes,
        kind: blockKindFor(start, minutes),
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

  /**
   * Book what a sentence describes.
   *
   * The quick model reads "lunch with Sam Thursday 1pm at the usual place";
   * everything after that is an ordinary write. Answers null and does
   * nothing when the sentence names no date -- an appointment with no day is
   * not an appointment, and guessing today would put somebody's Thursday
   * lunch on a Tuesday.
   *
   * Where it is written is the default calendar's say, as for every other
   * new event, with one difference: an account calendar's event is written
   * straight away rather than opened as a draft. A sentence typed into the
   * command bar is already the whole of what was meant -- the editor would
   * only be asking for it again -- and a note saying where it went is how
   * the person finds out, since they may not be looking at the calendar.
   *
   * An hour when no end was given, because that is what `bookNow` assumes too
   * and an event of unknown length has to be drawn as something.
   *
   * The booking happens *before* the view moves, which is the opposite of the
   * obvious order and the only one that works. `goto` fires an unawaited
   * `refresh`, and that refresh ends by assigning `this.blocks` from a query
   * issued before the save -- so navigating first drops the new block off the
   * grid and clears the selection that was just made. Booking first means the
   * refresh reads it back from storage, which is where it already is.
   */
  async bookFromSentence(line: string): Promise<TimeBlock | CalendarEvent | null> {
    /** `HH:MM` to minutes since midnight. The core already validated it. */
    const clockMinutes = (clock: string): number => {
      const [h = '0', m = '0'] = clock.split(':')
      return Number(h) * 60 + Number(m)
    }

    const draft = await ask('calendar.parse', () => api.quickEventFromLine(line))
    if (!draft?.date) {
      // Say so. This is reached from the palette, which has already closed
      // over a visible pause -- so silence here is a command that appeared to
      // do nothing, which is the worst answer available. `info` rather than
      // `error`: a sentence with no day in it is a thing the person can fix,
      // not a fault.
      notify.info('Nothing to book', {
        body: `“${line}” does not say which day.`,
        reach: 'app',
      })
      return null
    }
    await this.start()
    const date = draft.date
    const start = draft.start ? clockMinutes(draft.start) : 9 * 60
    // `end` without `start` cannot say how long anything is -- "by 8am
    // Thursday" would be measured against the 09:00 default and come out
    // negative. The core drops that pairing, and this is the second guard.
    const span = draft.end && draft.start ? clockMinutes(draft.end) - start : 0
    const minutes = span > 0 ? span : 60
    const rule = draft.recurrence ?? null

    const target = this.#newEventTarget()
    if (target === false) return null
    if (!target) {
      const block = await this.book({
        subject: { type: 'adhoc' },
        day: date,
        startMinutes: start,
        minutes,
        title: draft.location ? `${draft.title} — ${draft.location}` : draft.title,
      })
      // "Every Tuesday" written as a block makes the series from it -- the
      // same call the Repeat field in the rail makes afterwards.
      if (block && rule) await this.repeatBlock(block.id, rule)
      if (block && !this.days.includes(date)) this.goto(date)
      return block
    }

    const event: EventDraft = {
      title: draft.title,
      description: '',
      location: draft.location,
      start: instantAt(date, start),
      end: instantAt(date, start + minutes),
      allDay: false,
      tz: localZone(),
      attendees: [],
      recurrence: rule,
    }
    try {
      const created = await api.createEvent(target.id, event)
      notify.success(`Added to ${target.name}`, {
        body: created ? `“${created.title}”` : `“${draft.title}” will appear once it has synced.`,
        reach: 'app',
      })
      // Moved before the reload rather than through `goto`, whose own
      // reload is not awaited -- the selection below has to land after the
      // event is in `events`, or the reload would clear it as out of range.
      if (!this.days.includes(date)) {
        this.anchor = date
        this.syncNote = null
      }
      await this.refresh()
      if (created) this.selection = { kind: 'event', id: created.id }
      return created
    } catch (e) {
      if (isLocked(e)) {
        await app.lock()
        return null
      }
      notify.error(`Could not add it to ${target.name}`, { body: errorMessage(e), reach: 'app' })
      return null
    }
  }

  /**
   * A new hour-long event, starting on the next quarter. What Ctrl/Cmd N does.
   *
   * The same key that starts an entry in the journal and a task in the todo
   * app: begin the next thing. Here that is a new event -- a block, or a
   * draft on the default calendar, as `newEvent` decides -- and the view
   * moves to today if it was somewhere else, because a new hour you cannot
   * see is indistinguishable from nothing happening.
   */
  async bookNow() {
    // The tray can ask for this a frame after the view mounted, with the
    // first load still in flight; booking into a list that is about to be
    // replaced loses the block from the grid. Already loaded, this is free.
    await this.start()
    const at = new Date()
    const start = snap(at.getHours() * 60 + at.getMinutes(), SNAP_MINUTES)
    // Booked before the view moves, for the reason `bookFromSentence` gives:
    // `goto`'s refresh would otherwise overwrite `blocks` with a query that
    // predates the save. Rare here, because the view is usually already on
    // today -- which is exactly why it would have been found late.
    await this.newEvent({ day: todayIso(), startMinutes: start, minutes: DEFAULT_BLOCK_MINUTES })
    if (!this.days.includes(todayIso())) this.goto(todayIso())
  }

  // ── new events ───────────────────────────────────────────────────────

  /**
   * Start a new event at a time. Every "new event" gesture the grid has is
   * this: a click or a drag on empty time, "New event here", Ctrl/Cmd N.
   *
   * Where it goes is the default calendar's business, and the two answers
   * behave differently on purpose. This computer's time is a block, written
   * at once and selected, exactly as it always was -- a block is fifteen
   * seconds to make again and the rail edits it in place. An account
   * calendar's event is *not* written yet, because writing it is a request
   * to a server that may send invitations to other people: it opens as a
   * draft, drawn on the grid and edited in the rail, and nothing leaves the
   * machine until Save.
   *
   * Not for scheduling a task, logging what happened or the timer: those
   * are records of your own time and stay blocks whatever the default is.
   */
  async newEvent(opts: {
    day: string
    startMinutes: number
    minutes: number
    allDay?: boolean
    title?: string
  }): Promise<void> {
    const target = this.#newEventTarget()
    if (target === false) return
    if (!target) {
      await this.book({
        subject: { type: 'adhoc' },
        day: opts.day,
        startMinutes: opts.startMinutes,
        minutes: opts.minutes,
        title: opts.title,
      })
      return
    }
    const tz = localZone()
    const minutes = Math.max(MIN_BLOCK_MINUTES, Math.round(opts.minutes))
    this.openDraft(target.id, {
      title: opts.title ?? '',
      description: '',
      location: '',
      start: opts.allDay ? midnightIn(opts.day, tz) : instantAt(opts.day, opts.startMinutes),
      end: opts.allDay
        ? midnightIn(addDays(opts.day, 1), tz)
        : instantAt(opts.day, opts.startMinutes + minutes),
      allDay: !!opts.allDay,
      tz,
      attendees: [],
      recurrence: null,
    })
  }

  /** Open `draft` in the rail as a new event bound for `calendarId`. */
  openDraft(calendarId: CalendarId | null, draft: EventDraft, fromBlock?: BlockId) {
    this.#selectedProposalId = null
    // Selection first: the setter lets go of whatever draft was open before,
    // and this one must not be let go with it.
    this.selection = { kind: 'draft' }
    this.draft = fromBlock ? { calendarId, draft, fromBlock } : { calendarId, draft }
  }

  /**
   * Change the open draft. Replaced rather than mutated, so the slot on the
   * grid and the fields in the rail redraw from the same one assignment.
   */
  patchDraft(changes: Partial<EventDraft>) {
    if (!this.draft) return
    this.draft.draft = { ...this.draft.draft, ...changes }
  }

  /** Send the open draft somewhere else: another account calendar, or this computer. */
  setDraftCalendar(id: CalendarId | null) {
    const open = this.draft
    if (!open) return
    open.calendarId = id
    // This computer has nowhere to draw an all-day block of its own -- the
    // band over the grid holds other people's days -- so one moved here
    // becomes the first hour of the working day instead of vanishing.
    if (id === null && open.draft.allDay) {
      const day = dayIn(open.draft.start, open.draft.tz)
      this.patchDraft({
        allDay: false,
        start: instantAt(day, 9 * 60),
        end: instantAt(day, 10 * 60),
      })
    }
  }

  /** Move the draft on the grid, keeping its length and what its repeat meant. */
  moveDraft(day: string, startMinutes: number) {
    const open = this.draft
    if (!open || open.draft.allDay) return
    const length = minutesBetween(open.draft.start, open.draft.end)
    this.patchDraft({
      start: instantAt(day, startMinutes),
      end: instantAt(day, startMinutes + length),
      recurrence: carryRule(open.draft.recurrence, isoDate(new Date(open.draft.start)), day),
    })
  }

  /** Change the draft's length, holding its start. */
  resizeDraft(minutes: number) {
    const open = this.draft
    if (!open || open.draft.allDay) return
    const length = Math.max(MIN_BLOCK_MINUTES, Math.round(minutes))
    this.patchDraft({
      end: new Date(Date.parse(open.draft.start) + length * 60_000).toISOString(),
    })
  }

  /** Throw the open draft away. Nothing was written, so nothing is undone. */
  cancelDraft() {
    if (this.selection?.kind === 'draft') this.selection = null
    this.draft = null
  }

  /**
   * Write the open draft, and answer why not if it could not be.
   *
   * The failure is *returned*, for the editor to show beside its Save
   * button, rather than raised over the window: a server that refused a
   * guest's address is answered by fixing the address, and a banner that
   * closed the draft would lose everything typed into it.
   */
  async saveDraft(): Promise<string | null> {
    const open = this.draft
    if (!open) return null
    const draft = $state.snapshot(open.draft)
    const { calendarId, fromBlock } = open
    /** Is that draft still the one in the rail -- nobody has moved on since Save? */
    const current = () => this.draft === open

    try {
      if (calendarId === null) {
        const block = await this.#writeLocally(draft)
        await this.refreshBlocks()
        if (current()) this.selection = { kind: 'block', id: block.id }
        return null
      }

      const created = await api.createEvent(calendarId, draft)
      // Only now that the event is on the server: a failed save above has
      // returned already, and the block it came from is still there.
      if (fromBlock) await this.#retireBlock(fromBlock, !!draft.recurrence)
      await this.refresh()
      if (current()) this.#selectWritten(created, calendarId, draft)
      return null
    } catch (e) {
      if (isLocked(e)) {
        await app.lock()
        return null
      }
      return errorMessage(e)
    }
  }

  /**
   * A draft bound for this computer, as the block it becomes.
   *
   * Written directly rather than through `book`, which reports its own
   * failures over the whole window; this one belongs beside the editor's
   * Save button. A block has no field for a place, so a location rides in
   * the title, where `bookFromSentence` has always put one.
   */
  async #writeLocally(draft: EventDraft): Promise<TimeBlock> {
    const block = await api.newBlock({
      subject: { type: 'adhoc' },
      start: draft.start,
      minutes: minutesBetween(draft.start, draft.end),
      kind: blockKindFor(draft.start, minutesBetween(draft.start, draft.end)),
    })
    const title = draft.title.trim()
    const place = draft.location.trim()
    block.title = place ? `${title} — ${place}` : title
    block.notes = draft.description
    if (draft.recurrence) await api.saveBlockSeries(block, draft.recurrence)
    else await api.saveBlock(block)
    void todo.refreshStats()
    return block
  }

  /**
   * Delete the block a draft was moved from, now that the event has landed.
   *
   * A block in a series that the draft carried on repeating takes the rest
   * of its series with it -- the repeat moved to the account calendar, and
   * leaving the copies here would put every later one on the grid twice.
   * A failure is reported but not unwound: the event is already on the
   * server, and the leftover block is one press of Delete to tidy.
   */
  async #retireBlock(id: BlockId, repeats: boolean) {
    const block = this.blocks.find((b) => b.id === id)
    try {
      if (block?.series && repeats) await api.deleteBlockSeries(id, 'following')
      else await api.deleteBlock(id)
      this.#saves.forget(id)
      void todo.refreshStats()
    } catch (e) {
      await handle(e)
    }
  }

  /**
   * Select the event a write just made.
   *
   * The server's answer when it gave one. When it did not -- the write
   * landed but the calendar's sync has not brought it back yet -- the
   * nearest thing on the grid with the same calendar, start and title, and
   * failing that a line under the sidebar saying it is on its way, rather
   * than a rail that silently goes blank.
   */
  #selectWritten(created: CalendarEvent | null, calendarId: CalendarId, draft: EventDraft) {
    const title = draft.title.trim()
    const found =
      created ??
      this.events.find(
        (e) =>
          e.calendarId === calendarId &&
          e.title.trim() === title &&
          Date.parse(e.start) === Date.parse(draft.start),
      ) ??
      null
    if (found && this.events.some((e) => e.id === found.id)) {
      this.selection = { kind: 'event', id: found.id }
      return
    }
    this.selection = null
    const name = this.calendarOf(calendarId)?.name ?? 'the calendar'
    this.syncNote = `Saved to ${name}. It will appear here once it has synced.`
  }

  /**
   * Move a block of your own onto an account's calendar.
   *
   * Not a write, yet: it opens the block as a draft bound for that calendar,
   * filled in from it, so guests and a description can be added before
   * anything is sent. The block is deleted only once the event has landed --
   * see `NewEvent.fromBlock` -- so cancelling leaves it exactly as it was.
   */
  async moveBlockToCalendar(id: BlockId, calendarId: CalendarId) {
    // Pending edits first, so the draft is filled in from what was typed a
    // second ago and not from what was last saved.
    await this.flush()
    const block = this.blocks.find((b) => b.id === id)
    if (!block) return
    this.openDraft(
      calendarId,
      {
        title: block.title,
        description: block.notes,
        location: '',
        start: block.start,
        end: block.end,
        allDay: block.allDay,
        tz: block.tz || localZone(),
        attendees: [],
        recurrence: block.series ? $state.snapshot(block.series.rule) : null,
      },
      block.id,
    )
  }

  // ── changing an account calendar's events ────────────────────────────

  /**
   * Open an account calendar's event for changing.
   *
   * Read fresh from its server first. The stored copy is a reading for the
   * grid -- guests as display names, no rule, a description the feed may
   * have shortened -- and writing back from it would drop whatever it does
   * not hold. The rail shows the wait and, if the server will not answer,
   * why.
   */
  async editEvent(id: EventId) {
    this.selection = { kind: 'event', id }
    // Not while a move of it is on its way: the server would hand back the
    // times it is moving away from, and Save would write them back over it.
    if (this.moves.has(id)) return
    this.editing = { id, status: 'loading' }
    try {
      const event = await api.loadEvent(id)
      if (this.editing?.id !== id) return
      this.editing = { id, status: 'ready', event, draft: structuredClone(event.draft) }
    } catch (e) {
      if (isLocked(e)) return void (await app.lock())
      if (this.editing?.id !== id) return
      this.editing = { id, status: 'failed', error: errorMessage(e) }
    }
  }

  /** Change the event open for editing. Replaced, like `patchDraft`. */
  patchEdit(changes: Partial<EventDraft>) {
    const open = this.editing
    if (open?.status !== 'ready') return
    open.draft = { ...open.draft, ...changes }
  }

  /** Close the editor without writing anything. The event stays selected. */
  cancelEdit() {
    this.editing = null
  }

  /**
   * Write the change, to this occurrence or the whole series. Answers why
   * not, for the editor to show -- see `saveDraft`.
   */
  async saveEdit(scope: EventScope): Promise<string | null> {
    const open = this.editing
    if (open?.status !== 'ready') return null
    let draft = $state.snapshot(open.draft)
    if (scope === 'series') {
      // Moved to another day -- typed, or dragged on the grid -- and meant
      // for all of them: the repeat follows, or this says why it cannot.
      const series = this.#forSeries(open.event, draft)
      if (typeof series === 'string') return series
      draft = series
    }
    const calendarId = open.event.calendarId
    try {
      await api.updateEvent(open.id, draft, scope)
    } catch (e) {
      if (isLocked(e)) {
        await app.lock()
        return null
      }
      return errorMessage(e)
    }
    if (this.editing === open) this.editing = null
    await this.refresh()
    // A change to a series can come back as occurrences with new ids, in
    // which case the reload has dropped the selection; the one that was
    // being looked at is found again by where it now is.
    if (this.selection === null) this.#selectWritten(null, calendarId, draft)
    return null
  }

  /**
   * An event as its server has it, or `null` when the server will not say.
   * Asked before deleting one, for whether it is part of a series.
   */
  async loadEditable(id: EventId): Promise<EditableEvent | null> {
    const open = this.editing
    if (open?.id === id && open.status === 'ready') return open.event
    try {
      return await api.loadEvent(id)
    } catch (e) {
      await quietly(e)
      return null
    }
  }

  /**
   * Delete an account calendar's event -- this occurrence, or its series.
   * Its guests are sent a cancellation by the server. Answers why not, for
   * the rail to show beside the button that asked.
   */
  async deleteEvent(id: EventId, scope: EventScope): Promise<string | null> {
    try {
      await api.deleteEvent(id, scope)
    } catch (e) {
      if (isLocked(e)) {
        await app.lock()
        return null
      }
      return errorMessage(e)
    }
    if (this.selection?.kind === 'event' && this.selection.id === id) this.selection = null
    this.events = this.events.filter((e) => e.id !== id)
    await this.refresh()
    return null
  }

  // ── moving an account calendar's events ──────────────────────────────
  //
  // A drag on the grid, a stretch of its bottom edge, a drop on another
  // day. Each is a change of time to an event that lives on a server, so
  // each goes the way the editor's Save does -- read fresh, then written --
  // and none of them touches the stored copy. What is different is the
  // wait: the event is drawn where it was dropped from the moment it lands
  // (`moves`), rather than springing back for the second or two the server
  // takes and jumping again when the sync brings it round.

  /**
   * Can `event` be picked up and moved? Only on a calendar this vault may
   * write to, and only one of this account's own -- `own` from the sync
   * where its source says, or found out by the first move that asked. Not a
   * cancelled one, nor one whose last move is still on its way, nor one
   * whose editor has made it all-day or timed when the grid still draws it
   * the other way, where a drag would mean nothing the editor could hold.
   */
  canMove(event: CalendarEvent): boolean {
    const open = this.editing
    return (
      this.calendarOf(event.calendarId)?.access === 'writable' &&
      event.status !== 'cancelled' &&
      event.own !== false &&
      !this.#theirs.has(event.id) &&
      !this.moves.has(event.id) &&
      !(open?.id === event.id && open.status === 'ready' && open.draft.allDay !== event.allDay)
    )
  }

  /** Move a timed event to `startMinutes` into `day`, keeping its length. */
  moveEvent(event: CalendarEvent, day: string, startMinutes: number): Promise<void> {
    const start = instantAt(day, startMinutes)
    const after = (from: string, to: string) =>
      new Date(Date.parse(start) + Date.parse(to) - Date.parse(from)).toISOString()
    return this.#reschedule(event, placeOf(event, start, after(event.start, event.end)), (d) => ({
      start,
      end: after(d.start, d.end),
    }))
  }

  /** Change a timed event's length, holding its start. */
  resizeEvent(event: CalendarEvent, minutes: number): Promise<void> {
    const length = Math.max(MIN_BLOCK_MINUTES, minutes) * 60_000
    const end = (start: string) => new Date(Date.parse(start) + length).toISOString()
    return this.#reschedule(event, placeOf(event, event.start, end(event.start)), (d) => ({
      start: d.start,
      end: end(d.start),
    }))
  }

  /**
   * Move an event `days` days, keeping its times: a drop onto another day,
   * in the band over the grid or in the month. An all-day event keeps to
   * its own zone's days, which is what "all day" meant to its server.
   */
  shiftEvent(event: CalendarEvent, days: number): Promise<void> {
    if (days === 0) return Promise.resolve()
    const zone = event.allDay ? event.tz || null : null
    const shown = {
      start: shiftDays(event.start, days, zone),
      end: shiftDays(event.end, days, zone),
      localDate: addDays(event.localDate, days),
      endDate: addDays(event.endDate, days),
    }
    return this.#reschedule(event, shown, (draft) => {
      const tz = draft.allDay ? draft.tz || null : null
      return { start: shiftDays(draft.start, days, tz), end: shiftDays(draft.end, days, tz) }
    })
  }

  /**
   * Something dragged out of one day and dropped on another, in the band
   * over the grid or in the month: moved as many days on, at the same time.
   */
  moveToDay(move: DayMove, day: string) {
    const days = daysApart(move.from, day)
    if (days === 0) return
    if (move.kind === 'event') {
      const event = this.events.find((e) => e.id === move.id)
      if (event) void this.shiftEvent(event, days)
      return
    }
    const block = this.blocks.find((b) => b.id === move.id)
    if (!block) return
    // By the wall clock, not by minutes since midnight: the two differ by
    // an hour on the day the clocks change, and "the same time on Thursday"
    // means the time it says.
    const start = shiftDays(block.start, days)
    const length = Date.parse(block.end) - Date.parse(block.start)
    this.patch(block.id, {
      start,
      end: new Date(Date.parse(start) + length).toISOString(),
      localDate: addDays(block.localDate, days),
    })
  }

  /**
   * The one way the grid changes an account event's time.
   *
   * `shown` is where it is drawn meanwhile; `place` works the new times out
   * of the event as its server has it, which is what is written back -- see
   * `editEvent` for why that copy and not the stored one. A refusal, a
   * failure or a Cancel puts it back where it was, by dropping `shown`.
   */
  async #reschedule(
    event: CalendarEvent,
    shown: EventPlace,
    place: (draft: EventDraft) => Pick<EventDraft, 'start' | 'end'>,
  ) {
    if (!this.canMove(event)) return
    const id = event.id
    const open = this.editing
    if (open?.id === id && open.status === 'ready' && open.event.own) {
      // Its editor is open in the rail: the move is one more change to what
      // is being edited there, written with the rest of them on Save. The
      // times only -- the repeat is left as the server had it, so Save
      // still asks which of a series is meant (`saveEdit` carries it if
      // the answer is all of them).
      this.patchEdit(place(open.draft))
      return
    }
    // An editor still asking for it, or one that could not, would open on
    // the times it is moving away from. `editEvent` will not open another
    // until this move has landed.
    if (open?.id === id) this.editing = null
    const locks = this.#locks
    this.moves.set(id, shown)
    try {
      const loaded = await api.loadEvent(id)
      if (locks !== this.#locks) return
      if (!loaded.own) {
        // Theirs to move: a change made here would only diverge from the
        // one they send. Their sync may not have said so (`own`); the
        // server just has, and it is remembered until the vault locks.
        this.#theirs.add(id)
        notify.info(`Only ${loaded.organizer || 'whoever organised it'} can move this`, {
          body: `“${event.title}” is their meeting, so it stays where it was.`,
          reach: 'app',
        })
        return
      }
      const moved = { ...loaded.draft, ...place(loaded.draft) }
      const series = loaded.recurring ? this.#forSeries(loaded, moved) : null
      const scope = await this.#askToMove(event.title, loaded, moved, series)
      if (!scope || locks !== this.#locks) return
      // One occurrence keeps the series' rule exactly as it was read.
      const draft = scope === 'series' && series && typeof series !== 'string' ? series : moved
      await api.updateEvent(id, draft, scope)
      // Asked now rather than when the drag began: a click elsewhere, or
      // Escape, while the server was answering was a choice to look at
      // something else, and the move landing should not undo it.
      const selected = this.selection?.kind === 'event' && this.selection.id === id
      await this.refresh()
      if (locks !== this.#locks) return
      // A move can come back as an occurrence with a new id -- CalDAV's
      // carry their start -- and the one that was selected is found again
      // by where it now is, as after a Save.
      if (selected && this.selection === null) this.#selectWritten(null, event.calendarId, draft)
    } catch (e) {
      if (isLocked(e)) return void (await app.lock())
      if (locks !== this.#locks) return
      notify.error(`Could not move “${event.title}”`, { body: errorMessage(e), reach: 'app' })
    } finally {
      if (locks === this.#locks) this.moves.delete(id)
    }
  }

  /**
   * `draft`, to be written to the whole of its series.
   *
   * The server moves a series the way its occurrence moved (the service's
   * `draft_for_series`): the same number of days on, at the new time. So
   * when the day changed, the repeat has to follow it, or the series would
   * start on a day its own rule never falls on -- a weekly Tuesday dragged
   * to Thursday is a weekly Thursday. Days are the event's own zone's,
   * which is where its server reads the rule. A repeat changed by hand
   * says where it falls itself and is left alone.
   *
   * Some rules cannot follow: two days a week, an ending, a count, one this
   * app cannot read (`customRecurrence`). Moving the first of "Monday and
   * Wednesday" to Tuesday has no right answer, so those answer why not,
   * for the editor to show or the move question to say.
   */
  #forSeries(loaded: EditableEvent, draft: EventDraft): EventDraft | string {
    const rule = loaded.draft.recurrence
    const tz = draft.tz || null
    const from = dayIn(loaded.draft.start, tz)
    const to = dayIn(draft.start, tz)
    if (from === to) return draft
    if (!sameRecurrence(rule, draft.recurrence, loaded.draft.start, tz)) return draft
    const preset = loaded.customRecurrence ? 'custom' : matchPreset(rule, loaded.draft.start, tz)
    if (preset === 'custom' || preset === 'none') {
      return 'Its repeat cannot follow it to another day from here, so only this one can move.'
    }
    return { ...draft, recurrence: carryRule(rule, from, to, tz) }
  }

  /**
   * What a move has to have answered before it is sent, asked in a sheet
   * over whatever is on screen: which of a series it means, and -- when
   * there are guests -- that they are about to be told, because a drag is a
   * much smaller gesture than a button that says "notify guests". A
   * one-off with nobody else on it asks nothing, the way a block does not.
   *
   * `series` is `#forSeries`'s answer: a reason in place of a draft takes
   * "All events" off the sheet and says why.
   *
   * Answers the scope to write with, or `null` for "leave it where it was".
   * Two moves waiting at once ask one after the other, in the order they
   * were dropped: the sheet shows the oldest (`moveQuestion`).
   */
  #askToMove(
    title: string,
    loaded: EditableEvent,
    moved: EventDraft,
    series: EventDraft | string | null,
  ): Promise<EventScope | null> {
    const guests = loaded.draft.attendees
    if (!loaded.recurring && guests.length === 0) return Promise.resolve('occurrence')
    const first = guests[0]
    const told =
      guests.length === 0
        ? ''
        : guests.length === 1 && first
          ? `${first.name || first.email} is sent the new time.`
          : 'Everyone invited is sent the new time.'
    const why = typeof series === 'string' ? series : ''
    const detail = [
      `To ${whenOf(moved)}.`,
      loaded.recurring ? 'It is one of a series.' : '',
      why,
      told,
    ]
      .filter(Boolean)
      .join(' ')
    return new Promise((resolve) => {
      const answer = (scope: EventScope | null) => () => {
        this.#questions = this.#questions.filter((q) => q !== question)
        resolve(scope)
      }
      const question: MoveQuestion = {
        title: `Move “${title}”?`,
        detail,
        options: !loaded.recurring
          ? [{ label: 'Move and notify guests', run: answer('occurrence') }]
          : why
            ? [{ label: 'This event', run: answer('occurrence') }]
            : [
                { label: 'This event', run: answer('occurrence') },
                { label: 'All events', run: answer('series') },
              ],
        cancel: answer(null),
      }
      this.#questions = [...this.#questions, question]
    })
  }

  // ── where new events go ──────────────────────────────────────────────

  /**
   * Choose where new events go: an account calendar that will take them,
   * or `null` for this computer's own time blocks.
   *
   * Moved on screen first, so "New events go to" in the sidebar changes
   * the moment it is picked; then read back, because the backend is what
   * guarantees at most one calendar holds the flag, and `saveCalendar`
   * deliberately cannot move it.
   */
  async setDefaultCalendar(id: CalendarId | null) {
    // The switch for this computer goes with the default -- see
    // `thisComputerOff` -- so it moves on screen with it.
    const off = !this.thisComputerOn
    for (const c of this.calendars) {
      c.isDefault = c.id === id
      c.thisComputerOff = off && c.id === id
    }
    try {
      await api.setDefaultCalendar(id)
      this.calendars = await api.calendars()
    } catch (e) {
      await handle(e, () => this.refresh())
    }
  }

  /**
   * Switch this computer on or off as a calendar new events can go to. Off
   * only while they go to an account calendar; the backend refuses it
   * otherwise, and the list read back says what it decided.
   */
  async setThisComputer(on: boolean) {
    const target = this.defaultCalendar
    for (const c of this.calendars) c.thisComputerOff = !on && c.id === target?.id
    try {
      await api.setThisComputer(on)
      this.calendars = await api.calendars()
    } catch (e) {
      await handle(e, () => this.refresh())
    }
  }

  /**
   * The calendar list on its own, once per unlock.
   *
   * For the command bar, whose "Book" row names the calendar it will write
   * to and which can be opened before the calendar app ever has been. Free
   * once the app has loaded, which reads the list too.
   */
  async loadCalendars() {
    if (this.#calendarsKnown || !app.supportsCalendar) return
    this.#calendarsKnown = true
    const locks = this.#locks
    try {
      const calendars = await api.calendars()
      if (locks === this.#locks) this.calendars = calendars
    } catch (e) {
      this.#calendarsKnown = false
      await quietly(e)
    }
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
    this.#patcher.patch(id, changes)
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
    await optimisticRemove({
      optimistic: () => {
        this.blocks = this.blocks.filter((b) => b.id !== id)
        this.#saves.forget(id)
        if (this.selection?.kind === 'block' && this.selection.id === id) this.selection = null
      },
      call: () => api.deleteBlock(id),
      onSuccess: () => void todo.refreshStats(),
      rollback: () => this.refreshBlocks(),
    })
  }

  // ── repeating blocks ─────────────────────────────────────────────────
  //
  // A repeat of your own is written out as one block per occurrence, two
  // years ahead, each a block in its own right that can be moved or deleted
  // alone -- see `BlockSeries`. What ties them is `series`, and the two
  // commands below are the only ways to act on more than one at once.

  /**
   * Make a block repeat, change how it repeats from here on, or stop it.
   *
   * Pending edits to it are written first: the series is copied from the
   * block as the backend holds it, and a title typed a second ago and not
   * yet saved would otherwise be the one thing every copy lacked.
   */
  async repeatBlock(id: BlockId, rule: Recurrence | null) {
    await this.flush()
    const block = this.blocks.find((b) => b.id === id)
    if (!block) return
    try {
      await api.saveBlockSeries($state.snapshot(block), rule ? $state.snapshot(rule) : null)
      await this.refreshBlocks()
      void todo.refreshStats()
    } catch (e) {
      await handle(e, () => this.refreshBlocks())
    }
  }

  /**
   * Carry this block's changes -- its title, notes, time of day, length --
   * onto every later block of its series. The same call as making the
   * series, with the rule it already has: the later copies are replaced by
   * fresh ones made from this one.
   */
  async applyToFollowing(id: BlockId) {
    const rule = this.blocks.find((b) => b.id === id)?.series?.rule
    if (rule) await this.repeatBlock(id, $state.snapshot(rule))
  }

  /** Delete a block and the rest of its series after it, or the whole series. */
  async deleteBlockSeries(id: BlockId, scope: SeriesScope) {
    // Written before, not forgotten: a save still queued for one of these
    // and sent after the delete would put it back.
    await this.flush()
    try {
      const gone = new Set(await api.deleteBlockSeries(id, scope))
      for (const doomed of gone) this.#saves.forget(doomed)
      this.blocks = this.blocks.filter((b) => !gone.has(b.id))
      if (this.selection?.kind === 'block' && gone.has(this.selection.id)) this.selection = null
      void todo.refreshStats()
    } catch (e) {
      await handle(e, () => this.refreshBlocks())
    }
  }

  // ── the timer ────────────────────────────────────────────────────────
  //
  // "What am I doing right now" is a note in local storage, and *one* time
  // block written when you stop. Not a block written on start and
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
   * a thing to be nagged about, so anything with a block in view drops off
   * the list the moment it is scheduled.
   */
  get unscheduled(): Task[] {
    const booked = new Set(
      this.blocks
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

  /**
   * Subscribe to one of an account's own calendars, discovered rather than
   * pasted in. The same all-or-nothing shape as {@link subscribe}: nothing
   * is left behind if the first sync fails.
   */
  async subscribeFromAccount(account: AccountId, remoteId: string): Promise<string | null> {
    return this.add(() => api.subscribeAccountCalendar(account, remoteId))
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
    // A file calendar has nothing to refetch from; a feed and an account
    // calendar both do, over different transports the backend picks
    // between on its own.
    if (!subscription || subscription.origin.type === 'file') return
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
    if (!this.calendars.some((c) => c.origin.type !== 'file')) return
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
      await quietly(e)
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
