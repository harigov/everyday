// Where the week went, by area.
//
// Pure, and separate from the Overview's store for the reason `habits.ts` is
// separate from the tracking store: these are rules, and a rule that is
// wrong here is wrong quietly. A chart that drops the unattributed share
// looks tidier and lies. One that folds a goal into the wrong area reports a
// balanced life to somebody who does not have one. Neither fails to compile.
//
// The input is what the backend already answers: minutes per *purpose*, and
// separately the meetings somebody else booked, per area. Turning that into
// rows means one join the backend deliberately does not do — a goal to its
// area — because the goals are already in the interface and asking the
// database to decrypt them to group a bar chart would undo the whole reason
// the pointer is a clear column.

import { addDays } from './time'
import type { BalanceReport, Goal, Purpose, Area, AreaId } from './types'

/** One row of the balance view: an area, and what went to it. */
export interface AreaTotals {
  /** `null` is the unattributed row, which is always present. */
  areaId: AreaId | null
  name: string
  color: string
  icon: string
  actualMinutes: number
  plannedMinutes: number
  /** Minutes of somebody else's meetings, kept apart from your own record. */
  eventMinutes: number
  blocks: number
  /** Goals under this area that got any time at all, most first. */
  goals: { goalId: string; title: string; actualMinutes: number }[]
}

/** What the unattributed row is called and drawn in. */
export const UNFILED = {
  name: 'No goal or area',
  // `--fg-faint`, not the `--text-faint` this was until it was traced from
  // `BalanceBars`: that token is not in `theme.css`, so the browser dropped
  // the whole declaration and drew no fill at all. The row still counted
  // toward the chart's scale -- see `BalanceBars`' `ceiling` -- so an
  // all-day event nobody could see was quietly squashing every real bar
  // down to a sliver of the track.
  color: 'var(--fg-faint)',
  icon: '',
} as const

/**
 * Fold a report into one row per area.
 *
 * A purpose naming a goal is counted under that goal's area; a purpose
 * naming an area directly is counted under it; anything else — including a
 * pointer to a goal that has since been deleted — lands in the unattributed
 * row rather than being dropped. That last case is the one worth stating:
 * deleting a goal deliberately does not rewrite everything that pointed at
 * it, so unresolvable pointers are a normal state and not a corruption.
 *
 * Areas with nothing recorded are kept, because their absence is the
 * finding. A week where you were not a parent at all should show a row at
 * zero, not a chart with one fewer bar than last week.
 */
export function byArea(report: BalanceReport | null, areas: Area[], goals: Goal[]): AreaTotals[] {
  const rows = new Map<AreaId | null, AreaTotals>()
  const blank = (areaId: AreaId | null): AreaTotals => {
    const area = areaId ? areas.find((r) => r.id === areaId) : undefined
    return {
      areaId,
      name: area?.name ?? UNFILED.name,
      color: area?.color ?? UNFILED.color,
      icon: area?.icon ?? UNFILED.icon,
      actualMinutes: 0,
      plannedMinutes: 0,
      eventMinutes: 0,
      blocks: 0,
      goals: [],
    }
  }
  const row = (areaId: AreaId | null): AreaTotals => {
    const held = rows.get(areaId)
    if (held) return held
    const made = blank(areaId)
    rows.set(areaId, made)
    return made
  }

  // Every live area gets a row whether or not anything went to it, and the
  // unattributed row always exists.
  for (const area of areas) if (!area.archived) row(area.id)
  row(null)

  for (const entry of report?.purposes ?? []) {
    const areaId = areaOf(entry.purpose, goals)
    const into = row(areaId)
    into.actualMinutes += entry.actualMinutes
    into.plannedMinutes += entry.plannedMinutes
    into.blocks += entry.blocks

    // The goal breakdown, for the row's own detail. Only actual time: a goal
    // you set four hours aside for and did not touch is a fact about the
    // plan, and the plan is already drawn beside the bar.
    if (entry.purpose?.type === 'goal' && entry.actualMinutes > 0) {
      const goal = goals.find((g) => g.id === entry.purpose!.id)
      if (goal) {
        into.goals.push({
          goalId: goal.id,
          title: goal.title,
          actualMinutes: entry.actualMinutes,
        })
      }
    }
  }

  for (const entry of report?.events ?? []) {
    // A feed with no area is somebody else's claim on time you have not
    // said anything about, which is exactly the unattributed row.
    row(entry.areaId ?? null).eventMinutes += entry.minutes
  }

  for (const r of rows.values()) r.goals.sort((a, b) => b.actualMinutes - a.actualMinutes)

  // Busiest first, and the unattributed row last however big it is: it is
  // context for the others rather than a competitor to them, and sorting it
  // to the top would make every chart about it.
  return [...rows.values()].sort((a, b) => {
    if (a.areaId === null) return 1
    if (b.areaId === null) return -1
    return (
      b.actualMinutes + b.eventMinutes - (a.actualMinutes + a.eventMinutes) ||
      a.name.localeCompare(b.name)
    )
  })
}

/** Which area a pointer belongs to, following a goal to the area above it. */
export function areaOf(purpose: Purpose | null | undefined, goals: Goal[]): AreaId | null {
  if (!purpose) return null
  if (purpose.type === 'area') return purpose.id
  return goals.find((g) => g.id === purpose.id)?.areaId ?? null
}

/** Total recorded minutes across every row, the unattributed one included. */
export function totalMinutes(rows: AreaTotals[]): number {
  return rows.reduce((sum, r) => sum + r.actualMinutes, 0)
}

/**
 * Areas that look neglected: an open goal, and nothing recorded for a while.
 *
 * The one thing this whole domain can say that no individual app can. A
 * neglected area is not an area with no goals — that is an area you are not
 * using — and it is not an area with no time this week, because one quiet
 * week is a week. It is an area you have said you want something from and
 * have not touched.
 *
 * `sinceDays` is how long counts as a while, and is the caller's to choose.
 */
export function neglected(
  rows: AreaTotals[],
  areas: Area[],
  goals: Goal[],
  lastTouched: Map<string, string | null>,
  today: string,
  sinceDays = 14,
): { area: Area; days: number | null }[] {
  const cutoff = addDays(today, -sinceDays)
  const out: { area: Area; days: number | null }[] = []

  for (const area of areas) {
    if (area.archived) continue
    const open = goals.filter(
      (g) => g.areaId === area.id && (g.status === 'active' || g.status === 'paused'),
    )
    if (open.length === 0) continue

    // Time this window counts as a touch on its own: an area you spent six
    // hours on is not neglected however stale its goals' own records are.
    const row = rows.find((r) => r.areaId === area.id)
    if (row && row.actualMinutes > 0) continue

    // The latest moment across its goals. `null` means nothing was ever
    // recorded, which is the strongest case rather than the weakest.
    let latest: string | null = null
    for (const goal of open) {
      const at = lastTouched.get(goal.id) ?? null
      if (at && (!latest || at > latest)) latest = at
    }
    if (latest && latest.slice(0, 10) > cutoff) continue
    out.push({ area, days: latest ? daysBetween(latest.slice(0, 10), today) : null })
  }

  // The stalest first, and never-touched ahead of everything.
  return out.sort((a, b) => (b.days ?? Infinity) - (a.days ?? Infinity))
}

function daysBetween(from: string, to: string): number {
  const a = new Date(`${from}T00:00:00`).getTime()
  const b = new Date(`${to}T00:00:00`).getTime()
  return Math.max(0, Math.round((b - a) / 86_400_000))
}
