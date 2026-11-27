// State for the Overview: the page you arrange yourself.
//
// The odd one out among the app stores, and more so than it used to be.
// Every other app owns records and shows them. This one owns *no records at
// all* — the areas and goals it used to hold are edited in Settings and in
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

import { accounts } from './accounts.svelte'
import { api } from './api'
import { byArea, neglected, type AreaTotals } from './balance'
import { briefs } from './brief.svelte'
import { calendar } from './calendar.svelte'
import {
  activityWindows,
  addWidget,
  dayAgenda,
  defaultLayout,
  importantThreads,
  introduce,
  LAYOUT_KEY,
  MAIL_NEEDS,
  moveWidget,
  needsOf,
  parseLayout,
  removeWidget,
  reorderWidget,
  setDays,
  setSize,
  setSubject,
  soonestBack,
  specOf,
  staleAfter,
  trackerWindow,
  type Need,
  type Widget,
  type WidgetSize,
  type WidgetType,
} from './dashboard'
import { periodStart, streakTarget, summarise, type HabitSummary } from './habits'
import { promoteGmailInbox } from './mail'
import type { Showing } from './onscreen'
import { inShell } from './platform'
import { purpose } from './purpose.svelte'
import { pref } from './prefs'
import { proposals } from './proposals.svelte'
import { app, errorMessage, handle, isLocked } from './state.svelte'
import { latest } from './store/latest'
import { addDays, isoDate, localeWeekStart, minutesBetween, startOfWeek, todayIso } from './time'
import { todo } from './todo.svelte'
import { tracking } from './tracking.svelte'
import { thisMonth } from './money'
import { localZone } from './screentime'
import { isOpen, TASK_STATUSES, VaultError } from './types'
import type {
  ChangeKind,
  BalanceReport,
  CalendarEvent,
  CalendarInfo,
  EntrySummary,
  HealthDay,
  Item,
  KarmaReport,
  KindInfo,
  Project,
  LibraryStats,
  PersonDue,
  MailActivity,
  MailCategory,
  MailboxId,
  MeetingActivity,
  MonthSummary,
  NoteSummary,
  ScreenReport,
  SourceSummary,
  Story,
  Task,
  TaskId,
  TaskStats,
  TaskStatus,
  Thread,
  ThreadPage,
  ThreadFilter,
  TrackerDay,
  WeatherReport,
} from './types'

/** How far back a heatmap looks. Long enough for a streak to mean something. */
const HABIT_DAYS = 120

/** How far back the Karma card reads: a year, the widest its heatmap draws. */
export const KARMA_DAYS = 364

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

/**
 * How long a read is trusted on a later visit when no change has said
 * otherwise. The backstop for what moves with the clock rather than with a
 * write: a snoozed thread coming back, a follow-up falling due, a window
 * that never heard a change because it was not running when it was made.
 */
const FRESH_MS = 5 * 60_000

/**
 * The same, for a window that is never told of its own writes -- a phone or
 * a browser, whose server leaves them out of its stream, and the preview,
 * which is told of almost nothing. Those read the page again on each visit,
 * as they always did, unless the last read went out moments ago.
 */
const REVISIT_MS = 10_000

/**
 * How stale the mail cards may be before a change elsewhere on the page,
 * landing while it is on screen, reads them again too. A minute: those are
 * the reads that walk every Inbox, and a task ticked in another window is
 * no reason to walk them on each tick.
 */
const MAIL_LIVE_MS = 60_000

/** The reads that depend on which week is on screen. */
const WEEK_NEEDS: ReadonlySet<Need> = new Set<Need>(['balance', 'weekTasks'])

/**
 * Reads a visit makes again on every client, as it always did, past
 * `REVISIT_MS`. The forecast's place is the profile's, which changes with no
 * change event, and the service keeps each place's forecast for a few
 * minutes, so asking again costs a lookup rather than a trip to the network.
 */
const EVERY_VISIT: ReadonlySet<Need> = new Set<Need>(['weather'])

const layoutPref = pref<Widget[]>(
  'jatayu.overview.layout',
  (raw) => parseLayout(raw) ?? defaultLayout(),
  defaultLayout(),
  (widgets) => JSON.stringify($state.snapshot(widgets)),
)

/**
 * The Overview's two pages: Home, the cards somebody arranged, and Brief,
 * the newest brief written out in full with its pictures. Not a card on the
 * page, because a brief is read top to bottom and wants the whole width and
 * height to be read in, which a card in a grid of them never has.
 */
export const OVERVIEW_PAGES = ['home', 'brief'] as const
export type OverviewPage = (typeof OVERVIEW_PAGES)[number]

/**
 * Where the page last picked is kept, on this device. Written out here
 * rather than through `prefs.ts`, whose keys are a closed list of their own
 * -- the same arrangement `listen.svelte.ts` keeps its speed under.
 */
const PAGE_KEY = 'jatayu.overview.page'

/** The page kept from last time, or Home. */
function savedPage(): OverviewPage {
  try {
    const kept = localStorage.getItem(PAGE_KEY)
    return OVERVIEW_PAGES.find((p) => p === kept) ?? 'home'
  } catch {
    return 'home'
  }
}

/** Which of `INTRODUCED` this page has already been shown -- see `introduce`. */
const introducedPref = pref<string[]>(
  'jatayu.overview.introduced',
  (raw) => {
    const parsed: unknown = raw ? JSON.parse(raw) : []
    return Array.isArray(parsed) ? parsed.filter((t): t is string => typeof t === 'string') : []
  },
  [],
  (types) => JSON.stringify(types),
)

/**
 * Has a page ever been written down? Read raw, apart from `layoutPref`,
 * whose `get` answers `defaultLayout()` for "nothing stored" and so cannot
 * say which it was. A string that does not parse into a single card counts
 * as nothing, the same as `parseLayout` treats it.
 */
function hasArrangedPage(): boolean {
  try {
    return parseLayout(localStorage.getItem(LAYOUT_KEY)) !== null
  } catch {
    return true
  }
}

/** Every status a task is still open in. */
const OPEN_STATUSES = TASK_STATUSES.filter(isOpen)

/**
 * How many threads each Inbox read asks for. Well past the most rows a
 * full-width card draws, so the unread-or-waiting cut `importantThreads`
 * makes after the read still has a page's worth left -- and nowhere near
 * the thousands an "All inboxes" scroll can reach, because every thread
 * listed is a thread decrypted.
 */
const MAIL_ROWS = 50

/**
 * One listing over every Inbox in `ids`, as the Mail app's "All inboxes"
 * asks it: across them at once when there are several, and through the
 * plain one-mailbox read when there is only one, which is the command a
 * single account's Inbox has always been listed with.
 */
function listInboxes(ids: MailboxId[], filter: ThreadFilter): Promise<Thread[]> {
  return inboxPage(ids, filter, null).then((p) => p.threads)
}

function inboxPage(ids: MailboxId[], filter: ThreadFilter, cursor: string | null) {
  return ids.length === 1
    ? api.threads(ids[0]!, filter, cursor, MAIL_ROWS)
    : api.threadsAcross(ids, filter, cursor, MAIL_ROWS)
}

/**
 * The most snoozed threads "Snoozed mail" will page through. The list runs
 * newest first and the card runs soonest back first, so the first page is
 * not enough: the thread coming back this afternoon can be an old one. A
 * snoozed pile is small for almost everybody; this only stops a runaway.
 */
const SNOOZED_LIMIT = 500

/** Every snoozed thread in `ids`, up to `SNOOZED_LIMIT`, every page read. */
async function allSnoozed(ids: MailboxId[]): Promise<Thread[]> {
  const out: Thread[] = []
  let cursor: string | null = null
  do {
    const page: ThreadPage = await inboxPage(ids, { snoozed: true }, cursor)
    out.push(...page.threads)
    cursor = page.nextCursor ?? null
  } while (cursor && out.length < SNOOZED_LIMIT)
  return out
}

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

  #page = $state<OverviewPage>(savedPage())

  /**
   * Which page is showing. Remembered, so the window opens on the one left
   * open -- somebody who reads the brief first thing every morning should
   * not have to ask for it every morning. Set from outside too: the brief
   * card's Read, and Settings → Briefs' "Open in Overview", put the Brief
   * page up with `overview.page = 'brief'`.
   */
  get page(): OverviewPage {
    return this.#page
  }

  set page(page: OverviewPage) {
    const was = this.#page
    this.#page = page
    // Leaving Home ends arranging it: the handles belong to cards that are
    // no longer on screen.
    if (page !== 'home') this.editing = false
    try {
      localStorage.setItem(PAGE_KEY, page)
    } catch {
      // Not kept; this session still has it.
    }
    // The Brief page draws none of the cards, so a visit to it read none of
    // them (see `start`); coming back to Home is when they are wanted.
    if (page === 'home' && was !== 'home' && app.section === 'overview') void this.#visit()
  }
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
  /** Everybody with a frequency, most overdue first. */
  peopleDue = $state<PersonDue[]>([])
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

  /**
   * The Feed's sources with their unread counts, and the newest unread stories
   * across them, for the Reading card. Both come with the one `news` need:
   * neither draws anything without the other.
   */
  newsSources = $state<SourceSummary[]>([])
  newsStories = $state<Story[]>([])

  /**
   * What is under way, for "What's going on": projects in progress, what is
   * being read, watched or played, and the shelves those are on, for their
   * names and verbs. Goals are `purpose`'s.
   */
  goingProjects = $state<Project[]>([])
  goingItems = $state<Item[]>([])
  goingShelves = $state<KindInfo[]>([])

  /** Karma over the last year and in all, for the Karma card. */
  karma = $state<KarmaReport | null>(null)

  /**
   * Open tasks due today or before, for "Tasks for today". A row ticked on
   * the card stays here, done, until the next load -- see `tickTask` -- so
   * a tick by mistake can be taken back where it was made.
   */
  todayTasks = $state<Task[]>([])
  /** Priority and Important Inbox threads that are unread or want a reply,
   *  every account's, newest first -- see `importantThreads`. */
  importantMail = $state<Thread[]>([])
  /** What is snoozed out of every account's Inbox, soonest back first. */
  snoozedMail = $state<Thread[]>([])
  /** Every account's follow-ups, as `mail_follow_ups` gives them: reply ones
   *  first, newest first. The card splits them by kind. */
  followUps = $state<Thread[]>([])
  /** Today's events on the visible calendars, all-day first, then by start. */
  todayEvents = $state<CalendarEvent[]>([])

  /** This month in Money, in the main currency, for "Money this month". */
  moneyMonth = $state<MonthSummary | null>(null)
  /** Today in Health -- doses due, steps, last night's sleep. */
  healthDay = $state<HealthDay | null>(null)
  /** Today's screen time, one report over the one day. */
  screenToday = $state<ScreenReport | null>(null)

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

  /**
   * Which refresh each need is being read by, until its answer lands. Each
   * refresh takes the next number from `#reads`. A newer read of a need
   * retires an older one's answer for that need alone, so a visit reading
   * two stale cards and a tick re-reading one card do not throw each other's
   * work away; and a lock clears the lot, so nothing on its way lands.
   */
  #reading = new Map<Need, number>()
  #reads = 0
  /** When each need was last read (the request, not the answer), and on which day. */
  #fetched = new Map<Need, { at: number; day: string }>()
  /** Needs a change has made stale since they were last read -- see `noteChange`. */
  #dirty = new Set<Need>()
  /**
   * The needs a read has already answered, so `loading` is true only for what
   * has never been fetched. The page refreshes itself on every live change,
   * and "loading…" in the header on each of those is a flash over figures
   * that did not go anywhere; a card just added, which needs something new,
   * still reads as loading until its own data lands.
   */
  #loadedNeeds = new Set<Need>()
  #weekStart = localeWeekStart()
  #weatherGeneration = latest()
  #weatherTimer: ReturnType<typeof setInterval> | null = null

  constructor() {
    // Registered once, here, rather than in `start()`. The view calls
    // `start` on every mount -- which happens on every switch back to this
    // app -- so a hook registered there would be added again each time.
    app.onLock(() => this.reset())
    // A card added to the catalogue since this page was arranged arrives at
    // its top, once -- see `introduce`.
    const stored = layoutPref.get()
    // Nothing under the key means nobody has arranged this page, and the
    // default it fell back to is already the current one -- see `introduce`
    // on why that is not a page to top with every newcomer.
    const { widgets, seen } = introduce(stored, introducedPref.get(), hasArrangedPage())
    this.widgets = widgets
    if (widgets !== stored) layoutPref.set(widgets)
    introducedPref.set(seen)
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
   * owner put on it, so "where the week adds up by area" would be a claim
   * about somebody else's page. An empty page is one somebody is allowed to
   * have, and is said as such.
   */
  get showing(): Showing {
    if (this.page === 'brief') {
      return { view: 'the Brief page: the newest brief written out, with its timeline and figures' }
    }
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
    // Only what is read by the week: paging through weeks of balance is no
    // reason to walk every Inbox again on each step.
    await this.refresh(WEEK_NEEDS)
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

  /** The balance report, folded into one row per area. */
  get areas(): AreaTotals[] {
    return byArea(this.report, purpose.areas, purpose.goals)
  }

  /**
   * Areas with an open goal and nothing to show for it.
   *
   * The one thing this app can say that no other can: a goal has to be read
   * across the journal, the todo app, the calendar and the shelf before
   * anybody can tell it has gone quiet.
   */
  get neglectedAreas() {
    const touched = new Map<string, string | null>()
    for (const [id, a] of purpose.activity) touched.set(id, a.lastTouched ?? null)
    return neglected(this.areas, purpose.areas, purpose.goals, touched, todayIso())
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
   * be read as broken. The same correction `calendar.minutesOn` makes.
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
   * app -- reads again only what has gone stale since (see `#stale`), and
   * never blanks, so the charts do not flash empty on the way in.
   */
  async start() {
    if (!this.enabled) return
    this.editing = false
    await this.#visit()
  }

  /**
   * What a visit reads: the stale part of Home, or nothing at all while the
   * Brief page is up -- it draws none of the cards, and the briefs keep
   * themselves current. The setter for `page` reads Home on the way back.
   */
  async #visit() {
    if (!this.enabled) return
    const stale = this.page === 'home' ? this.#stale() : new Set<Need>()
    if (stale.size > 0) await this.refresh(stale)
    else await this.#keepUp(this.needs)
  }

  /**
   * What the live router runs while the page is on screen and a change that
   * can move its figures came in: what changes made stale, rather than every
   * read on the page. The mail cards wait for the next visit unless their
   * own reads are a minute old -- see `MAIL_LIVE_MS`.
   */
  async liveRefresh() {
    if (!this.enabled || this.page !== 'home') return
    const now = Date.now()
    const want = new Set<Need>()
    for (const need of this.#stale()) {
      const read = this.#fetched.get(need)
      if (MAIL_NEEDS.has(need) && read && now - read.at < MAIL_LIVE_MS) continue
      want.add(need)
    }
    if (want.size > 0) await this.refresh(want)
  }

  /**
   * Told of every change event (this window's own included) by the live
   * router, before it decides whether to reload anything: what the next
   * visit has to fetch again. Marking only, never a read -- it is called for
   * every heartbeat a screen-time collector sends.
   */
  noteChange(kind: ChangeKind): void {
    const trackers = tracking.trackers
    // No trackers known is "cannot tell": they may not have been read yet.
    const screenTrackers =
      trackers.length === 0 || trackers.some((t) => t.source?.type === 'screenTime')
    for (const need of staleAfter(kind, screenTrackers)) this.#dirty.add(need)
  }

  /**
   * Whether every write reaches `noteChange`, this window's own included: a
   * desktop window over a vault in this process. Anywhere else a visit reads
   * the page again, at most once in `REVISIT_MS`.
   */
  #hearsOwnWrites(): boolean {
    return inShell && !app.remote
  }

  /**
   * The needs on the page that have to be read again before they can be
   * trusted: what a change made stale, what was never read, what was read on
   * another day (every card here is about today or a window ending today),
   * and what was read longer ago than this window trusts a read for. Not
   * what is being read already, unless a change came in after that read
   * went out: its answer may not hold the change, so a newer read takes over.
   */
  #stale(): Set<Need> {
    const now = Date.now()
    const today = todayIso()
    const hears = this.#hearsOwnWrites()
    const out = new Set<Need>()
    for (const need of this.needs) {
      if (this.#reading.has(need) && !this.#dirty.has(need)) continue
      const trusted = hears && !EVERY_VISIT.has(need) ? FRESH_MS : REVISIT_MS
      const read = this.#fetched.get(need)
      if (this.#dirty.has(need) || !read || read.day !== today || now - read.at >= trusted) {
        out.add(need)
      }
    }
    return out
  }

  reset() {
    this.#reading.clear()
    this.#fetched.clear()
    this.#dirty.clear()
    this.#weatherGeneration.next()
    this.#stopWeatherPoll()
    this.report = null
    this.habitDays = []
    this.taskStats = null
    this.weekTasks = []
    this.libraryStats = null
    this.peopleDue = []
    this.entries = []
    this.notes = []
    this.mail = {}
    this.meetings = {}
    this.calendars = []
    this.newsSources = []
    this.newsStories = []
    // Titles, senders, subjects and what is on the shelf: decrypted, so not
    // to be left behind a lock any more than the counts above are.
    this.goingProjects = []
    this.goingItems = []
    this.goingShelves = []
    this.karma = null
    this.todayTasks = []
    this.importantMail = []
    this.snoozedMail = []
    this.followUps = []
    this.todayEvents = []
    this.moneyMonth = null
    this.healthDay = null
    this.screenToday = null
    this.#loadedNeeds.clear()
    this.bookedToday = { logged: 0, planned: 0 }
    this.weather = null
    this.weatherNoLocation = false
    this.weatherError = null
    this.loading = false
    this.editing = false
    this.anchor = todayIso()
  }

  /**
   * What the cards on the page need running beside their reads, whether or
   * not anything is read this time.
   */
  async #keepUp(needs: Set<Need>) {
    // A card drawing the running timer needs the wall clock ticking, and this
    // app can be the first one on screen after a launch -- before the calendar
    // view has mounted and started it for itself. Here rather than in `start`
    // because `start` runs once per mount and a widget can be *added* to the
    // page at any point after it; the elapsed figure on a freshly added "On
    // now" card would otherwise sit frozen, which is the exact failure
    // `watchClock` exists to prevent.
    if (needs.has('today')) calendar.watchClock()
    // "Today's events" dims what is over and picks out what is on, against
    // the same wall clock -- so it needs it ticking for the same reason.
    if (needs.has('todayEvents') && app.supportsCalendar) calendar.watchClock()
    // The Weather widget redraws itself every fifteen minutes on its own,
    // independent of whatever else makes this page refresh -- see
    // `pollWeather`. Started or stopped here so adding or removing the
    // widget takes effect on the very next refresh rather than the next
    // mount.
    if (needs.has('weather')) this.#startWeatherPoll()
    else this.#stopWeatherPoll()
    // The briefs store keeps itself current (a routine or run change reloads
    // it) and the Brief page and Settings read it too, so a visit asks it
    // only for what `visit` says is wanted. Not awaited -- its own `loading`
    // is what the card reads, and nothing else on the page waits on it.
    if (needs.has('brief')) void briefs.visit()
    // The proposals store loads and reloads itself -- see `todo.start` for
    // why this is idempotent -- so "Plan for tomorrow" only has to ask once,
    // the same as every other app view does on its own way in.
    // Not awaited: the card reads `proposals.pending` reactively.
    if (needs.has('proposals') && !proposals.loaded) void proposals.refresh()
  }

  /**
   * Re-read what the page needs, and nothing it does not: everything on it,
   * or those of `want` that are on it.
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
  async refresh(want?: ReadonlySet<Need>) {
    if (!this.enabled) return
    const mine = ++this.#reads
    const needs = this.needs
    /** What this call reads. */
    const fetch = new Set([...needs].filter((need) => !want || want.has(need)))
    const started = Date.now()
    const today = todayIso()
    for (const need of fetch) {
      this.#reading.set(need, mine)
      // Taken off now rather than when the answer lands: a change made while
      // the read is on its way may not be in it, and marks the need again.
      this.#dirty.delete(need)
    }
    this.#settleLoading()
    /** The first read that failed, if any. Reported once, at the end. */
    let failure: unknown = null
    /** The needs whose read failed: read again on the next visit. */
    const failed = new Set<Need>()

    /**
     * One read, or its empty answer.
     *
     * Whether the round trip happens at all is this call's to decide -- a
     * page with no shelf card never asks for shelf counts, and a visit that
     * found only the tasks stale reads only the tasks -- and the `catch` is
     * what keeps one failed read from being eight.
     */
    const ask = <T>(
      need: Need | readonly Need[],
      read: () => Promise<T>,
      blank: T,
      supported = true,
    ): Promise<T> => {
      const asked = typeof need === 'string' ? [need] : need
      if (!supported || !asked.some((n) => fetch.has(n))) return Promise.resolve(blank)
      return read().catch((e: unknown) => {
        if (failure === null) failure = e
        for (const n of asked) failed.add(n)
        return blank
      })
    }

    // One read per distinct window, each ending today. Asked only where the
    // backend carries the domain: a vault without mail has nothing to count,
    // and asking anyway would put an "unsupported" banner over the page.
    const counted = <T>(
      need: 'mail' | 'meetings',
      supported: boolean,
      read: (from: string, to: string) => Promise<T>,
    ): Promise<Record<number, T>> =>
      Promise.all(
        (fetch.has(need) && supported ? activityWindows(this.widgets, need) : []).map(
          async (days) =>
            [
              days,
              await ask<T | null>(need, () => read(addDays(today, -(days - 1)), today), null),
            ] as const,
        ),
      ).then((rows) => {
        const out: Record<number, T> = {}
        for (const [days, value] of rows) if (value !== null) out[days] = value
        return out
      })

    try {
      await this.#keepUp(needs)
      if (fetch.size === 0) return

      // The Inbox cards draw "Me" the way the mail list does, from each
      // account's own addresses, and the Inbox reads below are over the
      // accounts that keep mail. The account list loads once and keeps itself
      // current from then on, so this is a round trip on the first refresh
      // and nothing after it -- awaited here rather than raced, because two
      // loads of it at once land only the later one, and a read that waited
      // on the earlier would find the list still empty.
      const inbox = app.supportsMail && (fetch.has('importantMail') || fetch.has('snoozedMail'))
      if (app.supportsMail && (inbox || fetch.has('mailFollowUps'))) await accounts.load()
      const inboxes = inbox ? this.#inboxes() : Promise.resolve([] as MailboxId[])

      const [
        [
          report,
          days,
          taskStats,
          weekTasks,
          libraryStats,
          entries,
          notes,
          blocks,
          weather,
          newsSources,
          newsStories,
          goingProjects,
          goingItems,
          goingShelves,
          peopleDue,
        ],
        mail,
        meetings,
        calendars,
        [
          todayTasks,
          importantMail,
          snoozedMail,
          followUps,
          todayEvents,
          moneyMonth,
          healthDay,
          screenToday,
        ],
        karma,
      ] = await Promise.all([
        Promise.all([
          ask('balance', () => api.balance(this.weekStart, this.weekEnd), null),
          ask('trackerDays', () => api.trackerDays({ from: this.dataFrom, to: today }), []),
          ask('taskStats', () => api.taskStats(), null),
          ask(
            'weekTasks',
            () => api.tasks({ dueFrom: this.weekStart, dueTo: this.weekEnd, limit: 500 }),
            [],
          ),
          ask('library', () => api.libraryStats(), null),
          ask(
            'entries',
            // Their writing: not the notes the assistant leaves itself.
            () =>
              api.entries({
                from: this.entriesFrom,
                to: today,
                notJournal: app.assistantJournalId,
                limit: 400,
              }),
            [],
          ),
          ask('notes', () => api.notes({ limit: 6 }), []),
          ask('today', () => api.blocks({ from: today, to: today }), []),
          ask('weather', () => this.fetchWeather(), NO_WEATHER),
          // Asked only where the backend carries the Feed, as mail is: a vault
          // without it has nothing to count, and asking anyway would put an
          // "unsupported" banner over the page.
          ask('news', () => api.newsSources(), [], app.supportsNews),
          ask(
            'news',
            () => api.stories({ unreadOnly: true, limit: 20 }).then((page) => page.stories),
            [],
            app.supportsNews,
          ),
          ask(
            'going',
            () => api.projects().then((all) => all.filter((p) => p.status === 'active')),
            [],
            app.supportsTasks,
          ),
          ask(
            'going',
            () => api.items({ statuses: ['active'], sort: 'updatedDesc', limit: 60 }),
            [],
            app.supportsLibrary,
          ),
          ask('going', () => api.kinds(), [], app.supportsLibrary),
          ask('people', () => api.peopleDue(), [], app.supportsLibrary),
        ]),
        counted('mail', app.supportsMail, api.mailActivity),
        counted('meetings', app.supportsCalendar, api.meetingActivity),
        // The calendars give a meeting row its colour, and an event on "Today's
        // events" too.
        ask(['meetings', 'todayEvents'], () => api.calendars(), [], app.supportsCalendar),
        // The home cards: today's tasks and events, and the three Inbox
        // cards. Each through `ask`, so an account whose server is down
        // leaves its card empty rather than the page.
        Promise.all([
          ask(
            'todayTasks',
            // Everything due up to today that is still open: the todo app's
            // own Today list, which counts what slipped as today's problem.
            () => api.tasks({ dueTo: today, statuses: OPEN_STATUSES, sort: 'dueAsc', limit: 200 }),
            [],
            app.supportsTasks,
          ),
          ask(
            'importantMail',
            async () => {
              const ids = await inboxes
              if (ids.length === 0) return []
              // One read per category, since a filter names one. Snoozed
              // threads are left out, as the Inbox itself leaves them out.
              const lists = await Promise.all(
                (['priority', 'important'] as MailCategory[]).map((category) =>
                  listInboxes(ids, { category, snoozed: false }),
                ),
              )
              return importantThreads(lists)
            },
            [],
            app.supportsMail,
          ),
          ask(
            'snoozedMail',
            async () => {
              const ids = await inboxes
              return ids.length === 0 ? [] : soonestBack(await allSnoozed(ids))
            },
            [],
            app.supportsMail,
          ),
          ask(
            'mailFollowUps',
            () => api.mailFollowUps(null).then((list) => list.threads),
            [],
            app.supportsMail,
          ),
          ask(
            'todayEvents',
            // Visible calendars only, as the Calendar draws them: one somebody
            // hid is one they asked not to be shown.
            () =>
              api
                .events({ from: today, to: today, visibleOnly: true })
                .then((events) => dayAgenda(events)),
            [],
            app.supportsCalendar,
          ),
          ask('moneyMonth', () => api.moneyMonth(thisMonth()), null, app.supportsMoney),
          ask('healthToday', () => api.healthToday(today, localZone()), null, app.supportsHealth),
          ask(
            'screenToday',
            () => api.screenTimeReport({ from: today, to: today, tz: localZone(), devices: [] }),
            null,
            app.supportsScreenTime,
          ),
        ]),
        // A year of Karma, whatever the card's size: the narrower heatmap
        // draws fewer of the same weeks.
        ask('karma', () => api.karma(addDays(today, -KARMA_DAYS), today), null),
      ])

      // What lands is what this call read and nothing newer is reading:
      // fields of needs it did not read keep what they hold.
      const landed = new Set<Need>()
      for (const need of fetch) {
        if (this.#reading.get(need) !== mine) continue
        this.#reading.delete(need)
        landed.add(need)
      }
      if (landed.size === 0) return

      if (landed.has('balance')) this.report = report
      if (landed.has('trackerDays')) this.habitDays = days
      if (landed.has('taskStats')) this.taskStats = taskStats
      if (landed.has('weekTasks')) this.weekTasks = weekTasks
      if (landed.has('library')) this.libraryStats = libraryStats
      if (landed.has('entries')) this.entries = entries
      if (landed.has('notes')) this.notes = notes
      if (landed.has('mail')) this.mail = mail
      if (landed.has('meetings')) this.meetings = meetings
      if (landed.has('meetings') || landed.has('todayEvents')) this.calendars = calendars
      if (landed.has('news')) {
        this.newsSources = newsSources
        this.newsStories = newsStories
      }
      if (landed.has('going')) {
        this.goingProjects = goingProjects
        this.goingItems = goingItems
        this.goingShelves = goingShelves
      }
      if (landed.has('people')) this.peopleDue = peopleDue
      if (landed.has('todayTasks')) this.todayTasks = todayTasks
      if (landed.has('importantMail')) this.importantMail = importantMail
      if (landed.has('snoozedMail')) this.snoozedMail = snoozedMail
      if (landed.has('mailFollowUps')) this.followUps = followUps
      if (landed.has('todayEvents')) this.todayEvents = todayEvents
      if (landed.has('moneyMonth')) this.moneyMonth = moneyMonth
      if (landed.has('healthToday')) this.healthDay = healthDay
      if (landed.has('screenToday')) this.screenToday = screenToday
      if (landed.has('karma')) this.karma = karma
      if (landed.has('today')) {
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
      }
      if (landed.has('weather')) {
        this.weather = weather.report
        this.weatherNoLocation = weather.noLocation
        this.weatherError = weather.error
        // A forecast that could not be read is asked again on the next visit.
        if (weather.error !== null) failed.add('weather')
      }
      for (const need of landed) {
        this.#loadedNeeds.add(need)
        if (failed.has(need)) this.#dirty.add(need)
        else this.#fetched.set(need, { at: started, day: today })
      }

      // Areas, goals and what has happened against them belong to `purpose`,
      // which the todo app and the pickers also read. Asked for here rather
      // than duplicated, and only when something on the page draws it.
      if (landed.has('purpose')) await purpose.load(true)
      if (landed.has('trackerDays')) await tracking.load()
      if (landed.has('goalActivity')) await purpose.refreshActivity()

      // Last, and after everything above has landed. `handle` locks the
      // window when the failure was a locked vault, and the `reset` that
      // follows clears the partial page this call just drew.
      if (failure !== null) await handle(failure)
    } catch (e) {
      await handle(e)
    } finally {
      // Anything still marked as this call's never landed: read it again.
      for (const need of fetch) {
        if (this.#reading.get(need) !== mine) continue
        this.#reading.delete(need)
        this.#dirty.add(need)
      }
      this.#settleLoading()
    }
  }

  /**
   * "Loading…" while a need that has never been answered is being read, and
   * not for a read of something already on screen: the page re-reads itself
   * as changes come in, and the word in the header on each of those would be
   * a flash over figures that did not go anywhere.
   */
  #settleLoading() {
    this.loading = [...this.needs].some(
      (need) => !this.#loadedNeeds.has(need) && this.#reading.has(need),
    )
  }

  /**
   * Every mail account's Inbox, by id: the mailboxes "All inboxes" gathers.
   *
   * The same two rules the Mail app's own `refreshMailboxes` keeps, because
   * this is the same question asked from another page. Only accounts that
   * keep mail -- a calendar-only account has no Inbox to ask about -- and
   * each account's list through `promoteGmailInbox` first, since a Gmail
   * account synced before the label fix has its Inbox filed as an ordinary
   * label, and reading `role` alone would skip it. A pseudo-mailbox (the
   * mock's own Snoozed and Starred) is never an Inbox, whatever it says.
   */
  async #inboxes(): Promise<MailboxId[]> {
    const ids = accounts.list.filter((a) => a.services.mail).map((a) => a.id)
    const lists = await Promise.all(ids.map((id) => api.mailboxes(id).then(promoteGmailInbox)))
    return lists
      .flat()
      .filter((m) => m.role === 'inbox' && !m.pseudo)
      .map((m) => m.id)
  }

  /**
   * Tick a task on "Tasks for today" done, or a ticked one back to open.
   *
   * The write is the todo store's -- see rule 3 at the top of this file --
   * and the card's row is replaced with what it wrote rather than dropped,
   * so a tick by mistake can be undone where it was made. The row leaves on
   * the next load, which only asks for what is open. The counts on "Due
   * today" and "Tasks, counted" move with it, when either is on the page:
   * a list that lost a task under a tile still claiming it would be the
   * page disagreeing with itself.
   *
   * Drawn ticked at once, as the todo app's own tick is, and put back if the
   * write does not go: the box should not wait on a commit. A second press
   * on the same row while its write is on its way is let go, so two writes
   * of the one task cannot race each other to the vault.
   */
  async tickTask(task: Task) {
    if (this.#ticking.has(task.id)) return
    this.#ticking.add(task.id)
    // The card's copy, as it was: the direction is read from it, as before.
    const before = $state.snapshot(task)
    const status: TaskStatus = before.status === 'done' ? 'todo' : 'done'
    const completedAt = status === 'done' ? (before.completedAt ?? new Date().toISOString()) : null
    this.#replaceToday({ ...before, status, completedAt })
    let saved: Task | null = null
    try {
      saved = await todo.toggleDoneOf(before)
    } finally {
      this.#ticking.delete(task.id)
      // What was written, or the row as it was. `toggleDoneOf` has already
      // said why, when it could not write.
      this.#replaceToday(saved ?? before)
    }
    if (!saved) return
    if (this.needs.has('taskStats')) {
      try {
        this.taskStats = await api.taskStats()
      } catch {
        /* a stale count is not worth a banner over a tick that landed */
      }
    }
  }

  /** Rows of "Tasks for today" being ticked, by task. See `tickTask`. */
  #ticking = new Set<TaskId>()

  /** Put `task` in place of the row with its id on "Tasks for today". */
  #replaceToday(task: Task) {
    this.todayTasks = this.todayTasks.map((t) => (t.id === task.id ? task : t))
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
