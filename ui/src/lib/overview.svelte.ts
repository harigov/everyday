// State for the Overview: the app that reads every other one.
//
// The fifth store, and the odd one out. Every other app owns records and
// shows them; this one owns two small records — roles and goals, which live
// in `purpose.svelte.ts` — and spends the rest of its time asking the four
// other domains what happened. That shapes what is here:
//
//   1. Nothing is cached that a window decides. The balance report is keyed
//      to a week and the habits to a span of days, so both are re-asked when
//      the window moves rather than held and filtered. The backend answers
//      each with one `GROUP BY` over an index; the interface holding a
//      quarter of them to save that is a cache that can be stale.
//   2. Nothing is written here that another app owns. Ticking a habit on the
//      Today pane goes through `tracking`; opening a task goes to `todo`.
//      This store's only writes are to goals, and even those it delegates.
//   3. Loads are generation-guarded, because five requests go out at once
//      and a lock in the middle of them must not let one land afterwards.

import { api } from './api'
import { byRole, neglected, type RoleTotals } from './balance'
import { summarise, type HabitSummary } from './habits'
import { purpose } from './purpose.svelte'
import { app, handle } from './state.svelte'
import { addDays, localeWeekStart, startOfWeek, todayIso } from './time'
import { tracking } from './tracking.svelte'
import { TRAY_ORDER, tray } from './tray.svelte'
import type { BalanceReport, Goal, GoalActivity, GoalId, Role, RoleId, TrackerDay } from './types'

/** Which pane is on screen. */
export const PANES = ['today', 'week', 'goals', 'habits'] as const
export type Pane = (typeof PANES)[number]

export const PANE_LABELS: Record<Pane, string> = {
  today: 'Today',
  week: 'This week',
  goals: 'Goals',
  habits: 'Habits',
}

/** How far back the habits pane looks. Long enough for a streak to mean something. */
const HABIT_DAYS = 120

/** A goal with what has happened against it. */
export interface GoalRow {
  goal: Goal
  role: Role | undefined
  activity: GoalActivity | null
}

class OverviewState {
  // ── what is on screen ────────────────────────────────────────────────

  pane = $state<Pane>('today')
  /** Any day in the week being shown. The pane resolves it to a Monday. */
  anchor = $state(todayIso())
  selectedGoal = $state<GoalId | null>(null)
  selectedTracker = $state<string | null>(null)
  /** Which role's section is collapsed, by id. Chrome, not vault contents. */
  collapsed = $state<Set<RoleId>>(new Set())

  // ── what has been loaded for it ──────────────────────────────────────

  report = $state<BalanceReport | null>(null)
  /** Daily aggregates over the habit window, every tracker at once. */
  habitDays = $state<TrackerDay[]>([])
  /** Per-goal activity, filled in as the goals pane draws. */
  activity = $state<Map<GoalId, GoalActivity>>(new Map())
  loading = $state(false)

  #generation = 0
  #weekStart = localeWeekStart()

  constructor() {
    // Registered once, here, rather than in `start()`. The view calls
    // `start` on every mount -- which happens on every switch back to this
    // app -- so a hook registered there would be added again each time.
    app.onLock(() => this.reset())
  }

  get enabled(): boolean {
    return app.supportsOverview
  }

  /** Monday of the week being shown, whatever day the anchor is. */
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
    for (const [id, a] of this.activity) touched.set(id, a.lastTouched ?? null)
    return neglected(this.roles, purpose.roles, purpose.goals, touched, todayIso())
  }

  /** Goals grouped under their roles, in sidebar order. */
  get byRole(): { role: Role; goals: GoalRow[] }[] {
    return purpose.roles
      .filter((r) => !r.archived)
      .map((role) => ({
        role,
        goals: purpose.goals
          .filter((g) => g.roleId === role.id)
          .map((goal) => ({
            goal,
            role,
            activity: this.activity.get(goal.id) ?? null,
          }))
          .sort(byLastTouched),
      }))
  }

  /** What the habits list draws for one tracker. */
  habit(trackerId: string): HabitSummary {
    const tracker = tracking.tracker(trackerId)
    return summarise(
      this.habitDays.filter((d) => d.trackerId === trackerId),
      tracker?.cadence,
      { from: this.habitFrom, to: todayIso(), today: todayIso() },
      this.#weekStart,
    )
  }

  get habitFrom(): string {
    return addDays(todayIso(), -HABIT_DAYS)
  }

  // ── lifecycle ────────────────────────────────────────────────────────

  /**
   * Load everything this app draws. Called by the view on mount.
   *
   * Idempotent: mounting again -- which happens on every switch back to this
   * app -- refreshes rather than blanking, so the charts do not flash empty
   * on the way in.
   */
  async start() {
    if (!this.enabled) return
    await Promise.all([purpose.load(), tracking.load()])
    await this.refresh()
  }

  reset() {
    this.#generation += 1
    this.report = null
    this.habitDays = []
    this.activity = new Map()
    this.loading = false
    this.selectedGoal = null
    this.selectedTracker = null
    this.anchor = todayIso()
  }

  /** Re-read the window on screen, and every goal's activity with it. */
  async refresh() {
    if (!this.enabled) return
    const mine = ++this.#generation
    this.loading = true
    try {
      const [report, days] = await Promise.all([
        api.balance(this.weekStart, this.weekEnd),
        api.trackerDays({ from: this.habitFrom, to: todayIso() }),
      ])
      if (mine !== this.#generation) return
      this.report = report
      this.habitDays = days
      await this.refreshActivity(mine)
    } catch (e) {
      await handle(e)
    } finally {
      if (mine === this.#generation) this.loading = false
    }
  }

  /**
   * Ask what has happened against every goal.
   *
   * One call per goal, and deliberately not one call for all of them: each
   * is a handful of counts over clear index columns, there are a dozen goals
   * rather than a thousand, and a combined endpoint would be a second SQL
   * shape to keep in step with the first for no measurable gain.
   */
  private async refreshActivity(generation = this.#generation) {
    const goals = purpose.goals
    const next = new Map<GoalId, GoalActivity>()
    const answers = await Promise.all(
      goals.map(async (g) => [g.id, await api.goalActivity(g.id).catch(() => null)] as const),
    )
    if (generation !== this.#generation) return
    for (const [id, a] of answers) if (a) next.set(id, a)
    this.activity = next
  }

  // ── navigation ───────────────────────────────────────────────────────

  setPane(pane: Pane) {
    this.pane = pane
    localStorage.setItem('everyday.overview.pane', pane)
  }

  /** Move the week being shown. `0` returns to the one holding today. */
  async goWeek(by: number) {
    this.anchor = by === 0 ? todayIso() : addDays(this.weekStart, by * 7)
    await this.refresh()
  }

  toggleRole(id: RoleId) {
    const next = new Set(this.collapsed)
    if (next.has(id)) next.delete(id)
    else next.add(id)
    this.collapsed = next
  }

  // ── writes ───────────────────────────────────────────────────────────
  //
  // Thin on purpose. Roles and goals belong to `purpose`; a habit's
  // definition belongs to `tracking`. What is here is only the part that has
  // to refresh this app's own numbers afterwards.

  async saveGoal(goal: Goal) {
    await purpose.saveGoal(goal)
    await this.refreshActivity()
  }

  async addGoal(roleId: RoleId, title: string) {
    const goal = await purpose.addGoal(roleId, title)
    if (goal) this.selectedGoal = goal.id
    await this.refreshActivity()
    return goal
  }

  async deleteGoal(id: GoalId) {
    await purpose.deleteGoal(id)
    if (this.selectedGoal === id) this.selectedGoal = null
    await this.refreshActivity()
  }

  /** Put a starting set of roles in a vault that has none. */
  async seedRoles(): Promise<number> {
    const added = await purpose.seed()
    if (added > 0) await this.refresh()
    return added
  }
}

export const overview = new OverviewState()

// The quick actions this app offers from a standing stop: the tray, and the
// right-click menu on its button in the bar.
tray.register('overview', TRAY_ORDER.overview, () => {
  if (app.screen !== 'main' || !app.supportsOverview) return []
  return [
    {
      id: 'overview.today',
      label: 'How today is going',
      raise: true,
      run: async () => {
        if (await app.goTo('overview')) overview.setPane('today')
      },
    },
    {
      id: 'overview.week',
      label: 'Where the week went',
      raise: true,
      run: async () => {
        if (await app.goTo('overview')) overview.setPane('week')
      },
    },
    {
      id: 'overview.goals',
      label: 'Goals',
      raise: true,
      run: async () => {
        if (await app.goTo('overview')) overview.setPane('goals')
      },
    },
  ]
})

/** Newest activity first; a goal nothing has touched sorts last. */
function byLastTouched(a: GoalRow, b: GoalRow): number {
  const at = a.activity?.lastTouched ?? ''
  const bt = b.activity?.lastTouched ?? ''
  if (at !== bt) return bt.localeCompare(at)
  return a.goal.sortOrder - b.goal.sortOrder || a.goal.title.localeCompare(b.goal.title)
}
