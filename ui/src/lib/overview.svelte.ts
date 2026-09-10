// State for the Overview: the page you arrange yourself.
//
// The odd one out among the app stores, and more so than it used to be.
// Every other app owns records and shows them. This one owns *no records at
// all* — the roles and goals it used to hold are edited in Settings and in
// the todo app now — and spends its whole life asking the other domains what
// happened, on behalf of whatever widgets somebody has put on the page.
//
// That shapes everything here:
//
//   1. Only what is on the page is fetched. Every widget declares what it
//      needs (`dashboard.ts`), the needs are unioned, and nothing else is
//      asked for. A page of three stat tiles costs three counts; the same
//      page with a heatmap added costs four months of readings, and only
//      then.
//   2. Nothing is cached that a window decides. The balance report is keyed
//      to a week and the readings to a span of days, so both are re-asked
//      when the window moves rather than held and filtered.
//   3. Nothing is written here that another app owns. Ticking a habit goes
//      through `tracking`; opening a task goes to `todo`; a goal is edited
//      in the todo app's goals pane. What this store writes is the layout,
//      and the layout is not in the vault.
//   4. Loads are generation-guarded, because several requests go out at once
//      and a lock in the middle of them must not let one land afterwards.

import { api } from './api'
import { byRole, neglected, type RoleTotals } from './balance'
import { calendar } from './calendar.svelte'
import {
  addWidget,
  defaultLayout,
  LAYOUT_KEY,
  moveWidget,
  needsOf,
  parseLayout,
  removeWidget,
  reorderWidget,
  setDays,
  setSize,
  setSubject,
  trackerWindow,
  type Need,
  type Widget,
  type WidgetSize,
  type WidgetType,
} from './dashboard'
import { summarise, type HabitSummary } from './habits'
import { purpose } from './purpose.svelte'
import { app, handle } from './state.svelte'
import { addDays, isoDate, localeWeekStart, minutesBetween, startOfWeek, todayIso } from './time'
import { tracking } from './tracking.svelte'
import type {
  BalanceReport,
  EntrySummary,
  LibraryStats,
  NoteSummary,
  Task,
  TaskStats,
  TrackerDay,
} from './types'

/** How far back a heatmap looks. Long enough for a streak to mean something. */
const HABIT_DAYS = 120

class OverviewState {
  // ── what is on screen ────────────────────────────────────────────────

  /**
   * The page, in the order it is drawn.
   *
   * Read from `localStorage` in the constructor rather than in `start`: the
   * grid has already drawn by the time a load comes back, so reading it late
   * would show the default page for a frame and then visibly replace it.
   */
  widgets = $state<Widget[]>([])
  /** Whether the cards are showing their handles. Off on every fresh mount. */
  editing = $state(false)
  /** Any day in the week being shown. The widgets resolve it to a Monday. */
  anchor = $state(todayIso())
  /**
   * A request to open the reading field, made before the view was there to
   * take it. The ordinary case for a tray action: the request arrives while
   * another app is on screen and this view mounts a frame later. The same
   * arrangement the todo app's capture line uses.
   */
  wantsLog = $state(false)

  // ── what has been loaded for it ──────────────────────────────────────

  report = $state<BalanceReport | null>(null)
  /** Daily aggregates over the tracker window, every tracker at once. */
  habitDays = $state<TrackerDay[]>([])
  taskStats = $state<TaskStats | null>(null)
  /** Tasks dated into the week on screen, whatever their state. */
  weekTasks = $state<Task[]>([])
  libraryStats = $state<LibraryStats | null>(null)
  entries = $state<EntrySummary[]>([])
  notes = $state<NoteSummary[]>([])
  /** Minutes recorded and planned for today, from the blocks alone. */
  bookedToday = $state<{ logged: number; planned: number }>({ logged: 0, planned: 0 })
  loading = $state(false)

  #generation = 0
  #weekStart = localeWeekStart()

  constructor() {
    // Registered once, here, rather than in `start()`. The view calls
    // `start` on every mount -- which happens on every switch back to this
    // app -- so a hook registered there would be added again each time.
    app.onLock(() => this.reset())
    this.widgets = parseLayout(localStorage.getItem(LAYOUT_KEY)) ?? defaultLayout()
  }

  get enabled(): boolean {
    return app.supportsOverview
  }

  /** What the page as arranged actually reads. */
  get needs(): Set<Need> {
    return needsOf(this.widgets)
  }

  // ── the week the page is showing ─────────────────────────────────────

  get weekStart(): string {
    return startOfWeek(this.anchor, this.#weekStart)
  }

  get weekEnd(): string {
    return addDays(this.weekStart, 6)
  }

  get weekDays(): string[] {
    return Array.from({ length: 7 }, (_, i) => addDays(this.weekStart, i))
  }

  /** Is the week on screen the one today falls in? */
  get thisWeek(): boolean {
    return this.weekStart === startOfWeek(todayIso(), this.#weekStart)
  }

  /** Move the week being shown. `0` returns to the one holding today. */
  async goWeek(by: number) {
    this.anchor = by === 0 ? todayIso() : addDays(this.weekStart, by * 7)
    await this.refresh()
  }

  // ── derived, for the widgets ─────────────────────────────────────────

  /** The balance report, folded into one row per role. */
  get roles(): RoleTotals[] {
    return byRole(this.report, purpose.roles, purpose.goals)
  }

  /**
   * Roles with an open goal and nothing to show for it.
   *
   * The one thing this app can say that no other can: a goal has to be read
   * across the journal, the todo app, the calendar and the shelf before
   * anybody can tell it has gone quiet.
   */
  get neglectedRoles() {
    const touched = new Map<string, string | null>()
    for (const [id, a] of purpose.activity) touched.set(id, a.lastTouched ?? null)
    return neglected(this.roles, purpose.roles, purpose.goals, touched, todayIso())
  }

  /** The window a heatmap or a chart draws over, oldest day first. */
  get habitFrom(): string {
    return addDays(todayIso(), -trackerWindow(this.widgets, HABIT_DAYS))
  }

  /**
   * Today's minutes, with whatever is running now counted in.
   *
   * A timer started twenty minutes ago has no block behind it yet, so the
   * query above cannot see it -- and a card reading "nothing recorded" part
   * way through an afternoon of solid work is the one number here that would
   * be read as broken. The same correction `calendar.totalsOn` makes.
   */
  get recordedToday(): { logged: number; planned: number } {
    const { logged, planned } = this.bookedToday
    const running =
      calendar.timer && isoDate(new Date(calendar.timer.since)) === todayIso()
        ? calendar.runningMinutes
        : 0
    return { logged: logged + running, planned }
  }

  /** What a habit widget draws for one tracker. */
  habit(trackerId: string): HabitSummary {
    const tracker = tracking.tracker(trackerId)
    return summarise(
      this.habitDays.filter((d) => d.trackerId === trackerId),
      tracker?.cadence,
      { from: this.habitFrom, to: todayIso(), today: todayIso() },
      this.#weekStart,
    )
  }

  // ── the layout ───────────────────────────────────────────────────────
  //
  // Every one of these is the pure rule from `dashboard.ts` plus a write to
  // `localStorage`, and none of them touches the vault. Kept as one-liners
  // deliberately: the rules are tested next door, and a second opinion about
  // what "move up" means would be a second implementation of it.

  add(type: WidgetType, subject: string | null = null) {
    this.widgets = addWidget(this.widgets, type, subject)
    this.save()
    // A card that needs something nothing else on the page did is why this
    // is not a pure state change: without it the new widget draws its empty
    // state until something else happened to refresh.
    void this.refresh()
  }

  remove(id: string) {
    this.widgets = removeWidget(this.widgets, id)
    this.save()
  }

  move(id: string, by: number) {
    this.widgets = moveWidget(this.widgets, id, by)
    this.save()
  }

  /** Drop `id` in front of `before`, or at the end when that is null. */
  reorder(id: string, before: string | null) {
    this.widgets = reorderWidget(this.widgets, id, before)
    this.save()
  }

  resize(id: string, size: WidgetSize) {
    this.widgets = setSize(this.widgets, id, size)
    this.save()
  }

  retarget(id: string, subject: string | null) {
    this.widgets = setSubject(this.widgets, id, subject)
    this.save()
  }

  rewindow(id: string, days: number) {
    this.widgets = setDays(this.widgets, id, days)
    this.save()
    void this.refresh()
  }

  /** Put the page back to the one a new vault opens on. */
  restoreDefaults() {
    this.widgets = defaultLayout()
    this.save()
    void this.refresh()
  }

  private save() {
    localStorage.setItem(LAYOUT_KEY, JSON.stringify($state.snapshot(this.widgets)))
  }

  // ── lifecycle ────────────────────────────────────────────────────────

  /**
   * Load whatever the page asks for. Called by the view on mount.
   *
   * Idempotent: mounting again -- which happens on every switch back to this
   * app -- refreshes rather than blanking, so the charts do not flash empty
   * on the way in.
   */
  async start() {
    if (!this.enabled) return
    this.editing = false
    // A card drawing the running timer needs the wall clock ticking, and
    // this app can be the first one on screen after a launch -- before the
    // calendar view has mounted and started it for itself.
    if (this.needs.has('today')) calendar.watchClock()
    await this.refresh()
  }

  reset() {
    this.#generation += 1
    this.report = null
    this.habitDays = []
    this.taskStats = null
    this.weekTasks = []
    this.libraryStats = null
    this.entries = []
    this.notes = []
    this.bookedToday = { logged: 0, planned: 0 }
    this.loading = false
    this.editing = false
    this.anchor = todayIso()
  }

  /**
   * Re-read everything the page needs, and nothing it does not.
   *
   * One `Promise.all` over the needs rather than a chain, because these are
   * independent reads of one open vault and running them in series would
   * make a six-widget page six round trips deep. Each failure is swallowed
   * into its own empty answer for the same reason the goal counts are: one
   * shelf count that could not be read should leave a blank card, not take
   * the whole page down with it. A lock is the exception, and `handle` is
   * what tells the difference.
   */
  async refresh() {
    if (!this.enabled) return
    const mine = ++this.#generation
    const needs = this.needs
    this.loading = true
    try {
      const today = todayIso()
      const [report, days, taskStats, weekTasks, libraryStats, entries, notes, blocks] =
        await Promise.all([
          needs.has('balance') ? api.balance(this.weekStart, this.weekEnd) : null,
          needs.has('trackerDays')
            ? api.trackerDays({ from: this.habitFrom, to: today })
            : Promise.resolve([]),
          needs.has('taskStats') ? api.taskStats() : null,
          needs.has('weekTasks')
            ? api.tasks({ dueFrom: this.weekStart, dueTo: this.weekEnd, limit: 500 })
            : Promise.resolve([]),
          needs.has('library') ? api.libraryStats() : null,
          needs.has('entries')
            ? api.entries({ from: this.entriesFrom, to: today, limit: 400 })
            : Promise.resolve([]),
          needs.has('notes') ? api.notes({ limit: 6 }) : Promise.resolve([]),
          needs.has('today') ? api.blocks({ from: today, to: today }) : Promise.resolve([]),
        ])
      if (mine !== this.#generation) return

      this.report = report
      this.habitDays = days
      this.taskStats = taskStats
      this.weekTasks = weekTasks
      this.libraryStats = libraryStats
      this.entries = entries
      this.notes = notes
      this.bookedToday = blocks.reduce(
        (sum, b) => {
          if (b.localDate !== today) return sum
          const minutes = minutesBetween(b.start, b.end)
          if (b.kind === 'actual') sum.logged += minutes
          else sum.planned += minutes
          return sum
        },
        { logged: 0, planned: 0 },
      )

      // Roles, goals and what has happened against them belong to `purpose`,
      // which the todo app and the pickers also read. Asked for here rather
      // than duplicated, and only when something on the page draws it.
      if (needs.has('purpose')) await purpose.load(true)
      if (needs.has('trackerDays')) await tracking.load()
      if (needs.has('goalActivity')) await purpose.refreshActivity()
    } catch (e) {
      await handle(e)
    } finally {
      if (mine === this.#generation) this.loading = false
    }
  }

  /** How far back the writing widget's longest window reaches. */
  get entriesFrom(): string {
    const longest = this.widgets
      .filter((w) => w.type === 'writing')
      .reduce((most, w) => Math.max(most, w.days ?? 0), 30)
    return addDays(todayIso(), -longest)
  }
}

export const overview = new OverviewState()
