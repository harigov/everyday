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
  activityWindows,
  addWidget,
  defaultLayout,
  moveWidget,
  needsOf,
  parseLayout,
  removeWidget,
  reorderWidget,
  setDays,
  setSize,
  setSubject,
  specOf,
  trackerWindow,
  type Need,
  type Widget,
  type WidgetSize,
  type WidgetType,
} from './dashboard'
import { periodStart, streakTarget, summarise, type HabitSummary } from './habits'
import type { Showing } from './onscreen'
import { purpose } from './purpose.svelte'
import { pref } from './prefs'
import { proposals } from './proposals.svelte'
import { app, errorMessage, handle, isLocked } from './state.svelte'
import { latest } from './store/latest'
import { addDays, isoDate, localeWeekStart, minutesBetween, startOfWeek, todayIso } from './time'
import { tracking } from './tracking.svelte'
import { VaultError } from './types'
import type {
  BalanceReport,
  CalendarInfo,
  EntrySummary,
  LibraryStats,
  MailActivity,
  MeetingActivity,
  NoteSummary,
  Task,
  TaskStats,
  TrackerDay,
  WeatherReport,
} from './types'

/** How far back a heatmap looks. Long enough for a streak to mean something. */
const HABIT_DAYS = 120

/**
 * How often the Weather widget re-reads the forecast while the Overview is
 * open. A little past the service's own per-place cache window
 * (`weather_cache.rs`'s `TTL`), so a tick almost never pays for two live
 * fetches of the same place in a row, while still reading as "every fifteen
 * minutes" to somebody watching the clock.
 */
const WEATHER_POLL_MS = 15 * 60 * 1000

/** What `fetchWeather` resolves with: a report, or which of the two quiet
 *  states the Weather widget should show instead of one. */
interface WeatherOutcome {
  report: WeatherReport | null
  noLocation: boolean
  error: string | null
}

/** The blank answer `ask` falls back to when nothing on the page needs the
 *  weather, or when `refresh`'s own generation has already moved on. */
const NO_WEATHER: WeatherOutcome = { report: null, noLocation: false, error: null }

const layoutPref = pref<Widget[]>(
  'everyday.overview.layout',
  (raw) => parseLayout(raw) ?? defaultLayout(),
  defaultLayout(),
  (widgets) => JSON.stringify($state.snapshot(widgets)),
)

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

  /**
   * Mail and meetings, counted, keyed by the window in days each was counted
   * over. A record of windows rather than one answer because two cards can
   * ask for two -- see `activityWindows` for why the longer cannot stand in
   * for the shorter. Replaced whole on every load, never patched, so a
   * window nobody asks for any more does not linger.
   */
  mail = $state<Record<number, MailActivity>>({})
  meetings = $state<Record<number, MeetingActivity>>({})
  /** The calendars, for a meeting row's colour and the name under it. */
  calendars = $state<CalendarInfo[]>([])

  /** The forecast where you live, for the Weather widget. `null` until a
   *  read lands, or while the card is showing one of the two states below
   *  instead. */
  weather = $state<WeatherReport | null>(null)
  /** No place was named and the profile has none either -- the Weather
   *  widget's own empty state, distinct from `weatherError`: this names
   *  Settings rather than reading like a network fault. */
  weatherNoLocation = $state(false)
  /** The Weather widget's own quiet message, when its read failed for any
   *  other reason. Never raised as a banner over the window -- see
   *  `fetchWeather` -- because a stranger's server being unreachable is not
   *  worth interrupting the rest of the page for. */
  weatherError = $state<string | null>(null)

  #generation = latest()
  #weekStart = localeWeekStart()
  #weatherGeneration = latest()
  #weatherTimer: ReturnType<typeof setInterval> | null = null

  constructor() {
    // Registered once, here, rather than in `start()`. The view calls
    // `start` on every mount -- which happens on every switch back to this
    // app -- so a hook registered there would be added again each time.
    app.onLock(() => this.reset())
    this.widgets = layoutPref.get()
  }

  get enabled(): boolean {
    return app.supportsOverview
  }

  /** What the page as arranged actually reads. */
  get needs(): Set<Need> {
    return needsOf(this.widgets)
  }

  /**
   * What is on screen, for the assistant -- see `onscreen.ts`.
   *
   * The cards by name rather than described: it is a page of whatever its
   * owner put on it, so "where the week adds up by role" would be a claim
   * about somebody else's page. An empty page is one somebody is allowed to
   * have, and is said as such.
   */
  get showing(): Showing {
    const cards = this.widgets.map((w) => specOf(w.type).label.toLowerCase()).slice(0, 8)
    return {
      view:
        (cards.length > 0 ? `the cards ${cards.join(', ')}` : 'a page with no cards on it yet') +
        `, for the week ${this.weekStart} to ${this.weekEnd}`,
    }
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

  /**
   * Tomorrow, local -- not `addDays(todayIso(), 1)` inlined at every call
   * site, and not UTC: an evening west of Greenwich is not one arithmetic on
   * `Date.prototype.toISOString()` gets right. What "Plan for tomorrow"
   * reads `proposals.forDate` against.
   */
  get tomorrow(): string {
    return addDays(todayIso(), 1)
  }

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

  /**
   * The first day the tracker data has to reach back to.
   *
   * The heatmap's window, or further when a tracker's target runs over a
   * longer period: "12 a year" is judged on the whole year, and a window
   * that began in June would count half of it.
   */
  get dataFrom(): string {
    const today = todayIso()
    let from = this.habitFrom
    for (const tracker of tracking.live) {
      const start = periodStart(today, streakTarget(tracker).per, this.#weekStart)
      if (start < from) from = start
    }
    return from
  }

  /** What a habit widget draws for one tracker. */
  habit(trackerId: string): HabitSummary {
    const tracker = tracking.tracker(trackerId)
    return summarise(
      this.habitDays.filter((d) => d.trackerId === trackerId),
      { kind: tracker?.kind ?? 'check', source: tracker?.source },
      tracker ? streakTarget(tracker) : null,
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

  /** Put a widget on the page, in front of `before` or at the end. */
  add(type: WidgetType, subject: string | null = null, before: string | null = null) {
    this.widgets = addWidget(this.widgets, type, subject, before)
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
    layoutPref.set(this.widgets)
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
    await this.refresh()
  }

  reset() {
    this.#generation.next()
    this.#weatherGeneration.next()
    this.#stopWeatherPoll()
    this.report = null
    this.habitDays = []
    this.taskStats = null
    this.weekTasks = []
    this.libraryStats = null
    this.entries = []
    this.notes = []
    this.mail = {}
    this.meetings = {}
    this.calendars = []
    this.bookedToday = { logged: 0, planned: 0 }
    this.weather = null
    this.weatherNoLocation = false
    this.weatherError = null
    this.loading = false
    this.editing = false
    this.anchor = todayIso()
  }

  /**
   * Re-read everything the page needs, and nothing it does not.
   *
   * One `Promise.all` over the needs rather than a chain, because these are
   * independent reads of one open vault and running them in series would
   * make a six-widget page six round trips deep.
   *
   * Every read is asked for through `ask`, which is what makes a failure
   * local: one shelf count that could not be read leaves a blank card rather
   * than taking the whole page down with it, and the other seven still land.
   * The first failure is kept and handed to `handle` *after* the assignments,
   * so a lock still locks -- and the `reset` that follows from it clears the
   * partial results this call just wrote, which is the order that has to hold.
   */
  async refresh() {
    if (!this.enabled) return
    const mine = this.#generation.next()
    const needs = this.needs
    this.loading = true
    /** The first read that failed, if any. Reported once, at the end. */
    let failure: unknown = null

    /**
     * One read, or its empty answer.
     *
     * `want` decides whether the round trip happens at all -- a page with no
     * shelf card never asks for shelf counts -- and the `catch` is what keeps
     * one failed read from being eight.
     */
    const ask = <T>(want: boolean, read: () => Promise<T>, blank: T): Promise<T> =>
      want
        ? read().catch((e: unknown) => {
            if (failure === null) failure = e
            return blank
          })
        : Promise.resolve(blank)

    // A card drawing the running timer needs the wall clock ticking, and this
    // app can be the first one on screen after a launch -- before the calendar
    // view has mounted and started it for itself. Here rather than in `start`
    // because `start` runs once per mount and a widget can be *added* to the
    // page at any point after it; the elapsed figure on a freshly added "On
    // now" card would otherwise sit frozen, which is the exact failure
    // `watchClock` exists to prevent.
    if (needs.has('today')) calendar.watchClock()
    // The proposals store loads and reloads itself -- see `todo.start` for
    // why this is idempotent -- so "Plan for tomorrow" only has to ask once,
    // the same as every other app view does on its own way in.
    if (needs.has('proposals') && !proposals.loaded) await proposals.refresh()
    // The Weather widget redraws itself every fifteen minutes on its own,
    // independent of whatever else makes this page refresh -- see
    // `pollWeather`. Started or stopped here so adding or removing the
    // widget takes effect on the very next refresh rather than the next
    // mount.
    if (needs.has('weather')) this.#startWeatherPoll()
    else this.#stopWeatherPoll()

    // One read per distinct window, each ending today. Asked only where the
    // backend carries the domain: a vault without mail has nothing to count,
    // and asking anyway would put an "unsupported" banner over the page.
    const today = todayIso()
    const counted = <T>(
      need: 'mail' | 'meetings',
      supported: boolean,
      read: (from: string, to: string) => Promise<T>,
    ): Promise<Record<number, T>> =>
      Promise.all(
        (needs.has(need) && supported ? activityWindows(this.widgets, need) : []).map(
          async (days) =>
            [
              days,
              await ask<T | null>(true, () => read(addDays(today, -(days - 1)), today), null),
            ] as const,
        ),
      ).then((rows) => {
        const out: Record<number, T> = {}
        for (const [days, value] of rows) if (value !== null) out[days] = value
        return out
      })

    try {
      const [
        [report, days, taskStats, weekTasks, libraryStats, entries, notes, blocks, weather],
        mail,
        meetings,
        calendars,
      ] = await Promise.all([
        Promise.all([
          ask(needs.has('balance'), () => api.balance(this.weekStart, this.weekEnd), null),
          ask(
            needs.has('trackerDays'),
            () => api.trackerDays({ from: this.dataFrom, to: today }),
            [],
          ),
          ask(needs.has('taskStats'), () => api.taskStats(), null),
          ask(
            needs.has('weekTasks'),
            () => api.tasks({ dueFrom: this.weekStart, dueTo: this.weekEnd, limit: 500 }),
            [],
          ),
          ask(needs.has('library'), () => api.libraryStats(), null),
          ask(
            needs.has('entries'),
            () => api.entries({ from: this.entriesFrom, to: today, limit: 400 }),
            [],
          ),
          ask(needs.has('notes'), () => api.notes({ limit: 6 }), []),
          ask(needs.has('today'), () => api.blocks({ from: today, to: today }), []),
          ask(needs.has('weather'), () => this.fetchWeather(), NO_WEATHER),
        ]),
        counted('mail', app.supportsMail, api.mailActivity),
        counted('meetings', app.supportsCalendar, api.meetingActivity),
        ask(needs.has('meetings') && app.supportsCalendar, () => api.calendars(), []),
      ])
      if (!this.#generation.isCurrent(mine)) return

      this.report = report
      this.habitDays = days
      this.taskStats = taskStats
      this.weekTasks = weekTasks
      this.libraryStats = libraryStats
      this.entries = entries
      this.notes = notes
      this.mail = mail
      this.meetings = meetings
      this.calendars = calendars
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
      this.weather = weather.report
      this.weatherNoLocation = weather.noLocation
      this.weatherError = weather.error

      // Roles, goals and what has happened against them belong to `purpose`,
      // which the todo app and the pickers also read. Asked for here rather
      // than duplicated, and only when something on the page draws it.
      if (needs.has('purpose')) await purpose.load(true)
      if (needs.has('trackerDays')) await tracking.load()
      if (needs.has('goalActivity')) await purpose.refreshActivity()

      // Last, and after everything above has landed. `handle` locks the
      // window when the failure was a locked vault, and the `reset` that
      // follows clears the partial page this call just drew.
      if (failure !== null) await handle(failure)
    } catch (e) {
      await handle(e)
    } finally {
      if (this.#generation.isCurrent(mine)) this.loading = false
    }
  }

  /**
   * Read the forecast and fold the answer into one of three outcomes: a
   * report, "no place to ask about", or anything else. Used both by
   * `refresh`'s own batch above and by `pollWeather` below, so a vault
   * locking mid-read is the only failure this does not already resolve --
   * `ask` and `pollWeather` each treat that the same way every other read
   * on this page does, by letting it through to `handle`.
   *
   * Never reported as a banner over the window: `weatherError` is read by
   * `OverviewWidget.svelte` as a quiet line under the card instead, because
   * a stranger's server being unreachable is not worth interrupting
   * somebody's dashboard for.
   */
  private async fetchWeather(): Promise<WeatherOutcome> {
    try {
      return { report: await api.weather(), noLocation: false, error: null }
    } catch (e) {
      if (isLocked(e)) throw e
      if (e instanceof VaultError && e.code === 'no_location') {
        return { report: null, noLocation: true, error: null }
      }
      return { report: null, noLocation: false, error: errorMessage(e) }
    }
  }

  /**
   * `fetchWeather`, on its own rather than inside `refresh`'s wider batch --
   * what the fifteen-minute timer calls, so a Weather widget sitting alone
   * on an otherwise-quiet page does not pay for the rest of the dashboard
   * every time it redraws itself.
   */
  private async pollWeather(): Promise<void> {
    const mine = this.#weatherGeneration.next()
    try {
      const outcome = await this.fetchWeather()
      if (!this.#weatherGeneration.isCurrent(mine)) return
      this.weather = outcome.report
      this.weatherNoLocation = outcome.noLocation
      this.weatherError = outcome.error
    } catch (e) {
      await handle(e)
    }
  }

  #startWeatherPoll() {
    if (this.#weatherTimer) return
    this.#weatherTimer = setInterval(() => void this.pollWeather(), WEATHER_POLL_MS)
  }

  #stopWeatherPoll() {
    if (this.#weatherTimer) clearInterval(this.#weatherTimer)
    this.#weatherTimer = null
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
