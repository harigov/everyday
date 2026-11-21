// Areas and goals, and the one thing every other app needs from them.
//
// The smallest of the stores. Almost nothing in the interface wants to *edit*
// an area — that happens in one place, the About You tab of Settings — but a
// great deal of it wants to turn a `Purpose` into a name and a colour: a task
// detail panel, a block's context menu, a chip on a card. So the two lists
// are loaded once after unlock and kept here, and `label` and `color` are
// the functions everything else calls.
//
// It also holds what has *happened* against each goal, which two apps now
// draw: the Todo app's goals pane, which is where a goal is read and edited,
// and the Overview's progress widget. That is an all-time count per goal
// rather than a window over anything, so one copy can serve both — unlike
// the balance report, which is window-shaped and is asked for by whoever is
// showing a window.

import { api } from './api'
import { app, handle } from './state.svelte'
import { guardedRefresh } from './store/refresh'
import type {
  Goal,
  GoalActivity,
  GoalId,
  GoalQuery,
  Purpose,
  Area,
  AreaId,
  AreaInfo,
} from './types'

/** A resolved purpose: what to write, and what colour to write it in. */
export interface PurposeLabel {
  name: string
  color: string
  icon: string
  /** The area behind it, whether the purpose named a goal or the area itself. */
  areaId: AreaId | null
}

/**
 * What an unresolvable pointer reads as.
 *
 * A record can outlive the goal it named — deleting a goal deliberately does
 * not rewrite everything that pointed at it — so this is a normal state and
 * not an error. It reads as unattributed, which is what the report already
 * calls time nobody filed.
 */
export const UNATTRIBUTED: PurposeLabel = {
  name: 'Unattributed',
  color: 'var(--fg-faint)',
  icon: '',
  areaId: null,
}

/** A goal with what has happened against it, as the goals pane draws it. */
export interface GoalRow {
  goal: Goal
  area: AreaInfo | undefined
  activity: GoalActivity | null
}

class PurposeState {
  areas = $state<AreaInfo[]>([])
  goals = $state<Goal[]>([])
  /** What has happened against each goal. Filled in by `refreshActivity`. */
  activity = $state<Map<GoalId, GoalActivity>>(new Map())
  loading = $state(false)

  /** The goal whose detail rail is open, in whichever app is showing one. */
  selected = $state<GoalId | null>(null)
  /** Which area's section is folded away, by id. Chrome, not vault contents. */
  collapsed = $state<Set<AreaId>>(new Set())

  /**
   * Which load is the current one, so a slow one cannot land after a lock.
   *
   * A raw counter rather than `latest()`, because `refreshActivity` below
   * reads it without minting a token of its own -- it rides on whatever
   * `load` or `reset` last set, and two overlapping calls to it are meant to
   * both land, last write wins, the way they always have. `latest()` would
   * mint a fresh token for each and let the first go stale the moment the
   * second started, which is a real change in which answer survives.
   * `#loadGeneration` wraps this same field in `Generation`'s shape, for
   * `load` alone, which *does* mint one token per call the way `next()`
   * already did by hand.
   */
  #generation = 0
  #loadGeneration = {
    next: (): number => ++this.#generation,
    isCurrent: (token: number): boolean => token === this.#generation,
  }
  #loaded = false

  constructor() {
    app.onLock(() => this.reset())
  }

  get enabled(): boolean {
    return app.status?.capabilities?.purpose === true
  }

  /** Areas worth offering in a picker: everything not retired. */
  get activeAreas(): AreaInfo[] {
    return this.areas.filter((r) => !r.archived)
  }

  /**
   * Where a goal can be moved: every area still in use, and its own even
   * when that one has been retired -- a picker that cannot show where the
   * goal is now reads as if it had no area at all.
   */
  areasFor(goal: Goal): AreaInfo[] {
    return this.areas.filter((a) => !a.archived || a.id === goal.areaId)
  }

  /** Goals worth offering in a picker: everything still being pursued. */
  get openGoals(): Goal[] {
    return this.goals.filter((g) => g.status === 'active' || g.status === 'paused')
  }

  reset() {
    this.#generation += 1
    this.areas = []
    this.goals = []
    this.activity = new Map()
    this.selected = null
    this.loading = false
    this.#loaded = false
  }

  /** Goals grouped under their areas, in sidebar order, freshest first. */
  get byArea(): { area: AreaInfo; goals: GoalRow[] }[] {
    return this.areas
      .filter((r) => !r.archived)
      .map((area) => ({
        area,
        goals: this.goals
          .filter((g) => g.areaId === area.id)
          .map((goal) => ({ goal, area, activity: this.activity.get(goal.id) ?? null }))
          .sort(byLastTouched),
      }))
  }

  toggleArea(id: AreaId) {
    const next = new Set(this.collapsed)
    if (next.has(id)) next.delete(id)
    else next.add(id)
    this.collapsed = next
  }

  /**
   * Ask what has happened against every goal.
   *
   * One call per goal, and deliberately not one call for all of them: each is
   * a handful of counts over clear index columns, there are a dozen goals
   * rather than a thousand, and a combined endpoint would be a second SQL
   * shape to keep in step with the first for no measurable gain.
   */
  async refreshActivity(): Promise<void> {
    if (!this.enabled) return
    const mine = this.#generation
    const answers = await Promise.all(
      this.goals.map(async (g) => [g.id, await api.goalActivity(g.id).catch(() => null)] as const),
    )
    // A lock, or a reload, that happened while these were in the air. Those
    // counts are decrypted vault contents and must not land behind it.
    if (mine !== this.#generation) return
    const next = new Map<GoalId, GoalActivity>()
    for (const [id, a] of answers) if (a) next.set(id, a)
    this.activity = next
  }

  /**
   * Load both lists, once.
   *
   * Called by anything that is about to draw a purpose — a picker opening, a
   * task detail mounting — rather than at startup, so a vault whose owner
   * never uses this domain never pays for it. `force` is for the Overview,
   * which edits them and needs to see its own writes.
   */
  async load(force = false) {
    if (!this.enabled) return
    if (this.#loaded && !force) return
    await guardedRefresh(
      this.#loadGeneration,
      async (isCurrent) => {
        const [areas, goals] = await Promise.all([api.areas(), api.goals()])
        if (!isCurrent()) return
        this.areas = areas
        this.goals = goals
        this.#loaded = true
      },
      { setLoading: (v) => (this.loading = v), onError: (e) => handle(e) },
    )
  }

  area(id: AreaId | null | undefined): AreaInfo | undefined {
    return id ? this.areas.find((r) => r.id === id) : undefined
  }

  goal(id: GoalId | null | undefined): Goal | undefined {
    return id ? this.goals.find((g) => g.id === id) : undefined
  }

  /** Goals under one area, in the order the sidebar draws them. */
  goalsOf(area: AreaId): Goal[] {
    return this.goals.filter((g) => g.areaId === area)
  }

  /**
   * Resolve a pointer to something drawable.
   *
   * A goal takes its colour from its area rather than having one of its own.
   * Colour on this axis means "which part of your life", and a goal that
   * could be a different colour from the area above it would break the one
   * chart this whole domain exists to draw.
   */
  describe(purpose: Purpose | null | undefined): PurposeLabel {
    if (!purpose) return UNATTRIBUTED
    if (purpose.type === 'area') {
      const area = this.area(purpose.id)
      return area
        ? { name: area.name, color: area.color, icon: area.icon, areaId: area.id }
        : UNATTRIBUTED
    }
    const goal = this.goal(purpose.id)
    if (!goal) return UNATTRIBUTED
    const area = this.area(goal.areaId)
    return {
      name: goal.title,
      color: area?.color ?? 'var(--accent)',
      icon: area?.icon ?? '',
      areaId: goal.areaId,
    }
  }

  label(purpose: Purpose | null | undefined): string {
    return this.describe(purpose).name
  }

  color(purpose: Purpose | null | undefined): string {
    return this.describe(purpose).color
  }

  /** Which area a pointer ultimately belongs to, following a goal to its own. */
  areaOf(purpose: Purpose | null | undefined): AreaId | null {
    return this.describe(purpose).areaId
  }

  // ── writes ───────────────────────────────────────────────────────────
  //
  // Small enough to be immediate rather than debounced. An area is edited
  // about once a year and a goal a few times in its life; an `Autosave` here
  // would be machinery guarding against a burst of typing that never comes.

  async saveArea(area: Area): Promise<void> {
    try {
      await api.saveArea($state.snapshot(area))
      await this.load(true)
    } catch (e) {
      await handle(e)
    }
  }

  /**
   * Create an area, and return it so a caller can select it at once.
   *
   * Returns `null` on failure rather than throwing: every caller is a
   * component reacting to a keystroke, and the error has already been shown.
   */
  async addArea(name: string): Promise<AreaInfo | null> {
    const trimmed = name.trim()
    if (!trimmed) return null
    try {
      const area = await api.newArea(trimmed)
      await api.saveArea(area)
      await this.load(true)
      return this.area(area.id) ?? null
    } catch (e) {
      await handle(e)
      return null
    }
  }

  /**
   * Delete an area, which the backend refuses while goals point at it.
   *
   * Returns whether it went. The refusal is not a failure of this
   * application, it is the answer, and the caller offers archiving instead.
   */
  async deleteArea(id: AreaId): Promise<boolean> {
    try {
      await api.deleteArea(id)
      await this.load(true)
      return true
    } catch (e) {
      await handle(e)
      return false
    }
  }

  /**
   * Save a goal, and say whether it went: a select that showed the new value
   * the moment it was picked has to be put back when the save is refused.
   */
  async saveGoal(goal: Goal): Promise<boolean> {
    try {
      await api.saveGoal($state.snapshot(goal))
      await this.load(true)
      await this.refreshActivity()
      return true
    } catch (e) {
      await handle(e)
      return false
    }
  }

  /** Create a goal under an area, and return it so a picker can select it. */
  async addGoal(areaId: AreaId, title: string): Promise<Goal | null> {
    const trimmed = title.trim()
    if (!trimmed) return null
    try {
      const goal = await api.newGoal(areaId, trimmed)
      await api.saveGoal(goal)
      await this.load(true)
      await this.refreshActivity()
      return this.goal(goal.id) ?? null
    } catch (e) {
      await handle(e)
      return null
    }
  }

  async deleteGoal(id: GoalId): Promise<void> {
    try {
      await api.deleteGoal(id)
      if (this.selected === id) this.selected = null
      await this.load(true)
      await this.refreshActivity()
    } catch (e) {
      await handle(e)
    }
  }

  /** Put a starting set of areas in a vault that has none. */
  async seed(): Promise<number> {
    try {
      const added = await api.seedAreas()
      if (added > 0) await this.load(true)
      return added
    } catch (e) {
      await handle(e)
      return 0
    }
  }

  /** Reload with a filter, for the Overview's own lists. */
  async query(query: GoalQuery): Promise<Goal[]> {
    if (!this.enabled) return []
    try {
      return await api.goals(query)
    } catch (e) {
      await handle(e)
      return []
    }
  }
}

export const purpose = new PurposeState()

/** Newest activity first; a goal nothing has touched sorts last. */
function byLastTouched(a: GoalRow, b: GoalRow): number {
  const at = a.activity?.lastTouched ?? ''
  const bt = b.activity?.lastTouched ?? ''
  if (at !== bt) return bt.localeCompare(at)
  return a.goal.sortOrder - b.goal.sortOrder || a.goal.title.localeCompare(b.goal.title)
}

/** Two purposes are the same when they name the same thing. */
export function samePurpose(a: Purpose | null | undefined, b: Purpose | null | undefined): boolean {
  if (!a || !b) return !a && !b
  return a.type === b.type && a.id === b.id
}
