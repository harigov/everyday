// How each goal is doing against the targets of the trackers filed under it.
//
// A goal measures itself through trackers: "Learn piano" has a derived
// tracker counting the minutes filed under it, with a target of between one
// and two hours a week; "Read more" has one counting books finished, twelve a
// year. Nothing here is stored. It is the current period of each of those
// targets, worked out from `tracker_days` by the same rules the habits view
// uses (`habits.ts`), for the two places that draw it: the goal's row in the
// todo sidebar and the Targets section of its detail rail.
//
// One fetch for every goal at once, reaching back to the start of the
// longest current period any of them asks about -- the first of January, if
// anything is counted by the year. A dozen trackers over nine months is a
// few hundred rows from an indexed `GROUP BY`, and cheaper than a fetch per
// goal as the sidebar draws.

import { api } from './api'
import { periodStart, primaryTarget, progress, type TargetProgress } from './habits'
import { app, quietly } from './state.svelte'
import { latest } from './store/latest'
import { localeWeekStart, todayIso } from './time'
import { tracking } from './tracking.svelte'
import type { GoalId, Target, Tracker, TrackerDay } from './types'

/** One target of one tracker, measured over its current period. */
export interface Measure {
  tracker: Tracker
  target: Target
  progress: TargetProgress
}

class TargetsState {
  days = $state<TrackerDay[]>([])
  #generation = latest()
  #weekStart = localeWeekStart()

  constructor() {
    app.onLock(() => {
      this.#generation.next()
      this.days = []
    })
  }

  /** Every live tracker filed under this goal, targets or not. */
  trackersOf(goal: GoalId): Tracker[] {
    return tracking.live.filter((t) => t.purpose?.type === 'goal' && t.purpose.id === goal)
  }

  /** Every target on every tracker under this goal, measured, in order. */
  measuresOf(goal: GoalId): Measure[] {
    const today = todayIso()
    return this.trackersOf(goal).flatMap((tracker) =>
      (tracker.targets ?? []).map((target) => ({
        tracker,
        target,
        progress: progress(
          this.days.filter((d) => d.trackerId === tracker.id),
          tracker,
          target,
          today,
          this.#weekStart,
        ),
      })),
    )
  }

  /**
   * The one measure a goal's sidebar row has room for: the first target of
   * its first tracker that has one.
   */
  headline(goal: GoalId): Measure | null {
    const tracker = this.trackersOf(goal).find((t) => primaryTarget(t))
    if (!tracker) return null
    return this.measuresOf(goal).find((m) => m.tracker.id === tracker.id) ?? null
  }

  /** Fetch what every goal's current periods need. */
  async refresh() {
    if (!tracking.enabled) return
    // Minted before anything is awaited, and on every path: a refresh that
    // finds nothing left to measure is still the newest answer, and must
    // retire a fetch from before the last target went, or that fetch lands
    // afterwards and puts the old days back.
    const mine = this.#generation.next()
    await tracking.load()
    const today = todayIso()
    const measured = tracking.live.filter(
      (t) => t.purpose?.type === 'goal' && (t.targets?.length ?? 0) > 0,
    )
    if (measured.length === 0) {
      if (this.#generation.isCurrent(mine)) this.days = []
      return
    }
    let from = today
    for (const t of measured) {
      for (const target of t.targets ?? []) {
        const start = periodStart(today, target.per, this.#weekStart)
        if (start < from) from = start
      }
    }
    try {
      const days = await api.trackerDays({ trackerIds: measured.map((t) => t.id), from, to: today })
      if (this.#generation.isCurrent(mine)) this.days = days
    } catch (e) {
      await quietly(e)
    }
  }
}

export const targets = new TargetsState()
