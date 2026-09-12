// Where the week went, by role.
//
// Pure, and separate from the Overview's store for the reason `habits.ts` is
// separate from the tracking store: these are rules, and a rule that is
// wrong here is wrong quietly. A chart that drops the unattributed share
// looks tidier and lies. One that folds a goal into the wrong role reports a
// balanced life to somebody who does not have one. Neither fails to compile.
//
// The input is what the backend already answers: minutes per *purpose*, and
// separately the meetings somebody else booked, per role. Turning that into
// rows means one join the backend deliberately does not do — a goal to its
// role — because the goals are already in the interface and asking the
// database to decrypt them to group a bar chart would undo the whole reason
// the pointer is a clear column.

import { addDays } from './time'
import type { BalanceReport, Goal, Purpose, Role, RoleId } from './types'

/** One row of the balance view: a role, and what went to it. */
export interface RoleTotals {
  /** `null` is the unattributed row, which is always present. */
  roleId: RoleId | null
  name: string
  color: string
  icon: string
  actualMinutes: number
  plannedMinutes: number
  /** Minutes of somebody else's meetings, kept apart from your own record. */
  eventMinutes: number
  blocks: number
  /** Goals under this role that got any time at all, most first. */
  goals: { goalId: string; title: string; actualMinutes: number }[]
}

/** What the unattributed row is called and drawn in. */
export const UNFILED = {
  name: 'Not filed',
  color: 'var(--text-faint)',
  icon: '',
} as const

/**
 * Fold a report into one row per role.
 *
 * A purpose naming a goal is counted under that goal's role; a purpose
 * naming a role directly is counted under it; anything else — including a
 * pointer to a goal that has since been deleted — lands in the unattributed
 * row rather than being dropped. That last case is the one worth stating:
 * deleting a goal deliberately does not rewrite everything that pointed at
 * it, so unresolvable pointers are a normal state and not a corruption.
 *
 * Roles with nothing recorded are kept, because their absence is the
 * finding. A week where you were not a parent at all should show a row at
 * zero, not a chart with one fewer bar than last week.
 */
export function byRole(report: BalanceReport | null, roles: Role[], goals: Goal[]): RoleTotals[] {
  const rows = new Map<RoleId | null, RoleTotals>()
  const blank = (roleId: RoleId | null): RoleTotals => {
    const role = roleId ? roles.find((r) => r.id === roleId) : undefined
    return {
      roleId,
      name: role?.name ?? UNFILED.name,
      color: role?.color ?? UNFILED.color,
      icon: role?.icon ?? UNFILED.icon,
      actualMinutes: 0,
      plannedMinutes: 0,
      eventMinutes: 0,
      blocks: 0,
      goals: [],
    }
  }
  const row = (roleId: RoleId | null): RoleTotals => {
    const held = rows.get(roleId)
    if (held) return held
    const made = blank(roleId)
    rows.set(roleId, made)
    return made
  }

  // Every live role gets a row whether or not anything went to it, and the
  // unattributed row always exists.
  for (const role of roles) if (!role.archived) row(role.id)
  row(null)

  for (const entry of report?.purposes ?? []) {
    const roleId = roleOf(entry.purpose, goals)
    const into = row(roleId)
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
    // A feed with no role is somebody else's claim on time you have not
    // said anything about, which is exactly the unattributed row.
    row(entry.roleId ?? null).eventMinutes += entry.minutes
  }

  for (const r of rows.values()) r.goals.sort((a, b) => b.actualMinutes - a.actualMinutes)

  // Busiest first, and the unattributed row last however big it is: it is
  // context for the others rather than a competitor to them, and sorting it
  // to the top would make every chart about it.
  return [...rows.values()].sort((a, b) => {
    if (a.roleId === null) return 1
    if (b.roleId === null) return -1
    return (
      b.actualMinutes + b.eventMinutes - (a.actualMinutes + a.eventMinutes) ||
      a.name.localeCompare(b.name)
    )
  })
}

/** Which role a pointer belongs to, following a goal to the role above it. */
export function roleOf(purpose: Purpose | null | undefined, goals: Goal[]): RoleId | null {
  if (!purpose) return null
  if (purpose.type === 'role') return purpose.id
  return goals.find((g) => g.id === purpose.id)?.roleId ?? null
}

/** Total recorded minutes across every row, the unattributed one included. */
export function totalMinutes(rows: RoleTotals[]): number {
  return rows.reduce((sum, r) => sum + r.actualMinutes, 0)
}

/**
 * Roles that look neglected: an open goal, and nothing recorded for a while.
 *
 * The one thing this whole domain can say that no individual app can. A
 * neglected role is not a role with no goals — that is a role you are not
 * using — and it is not a role with no time this week, because one quiet
 * week is a week. It is a role you have said you want something from and
 * have not touched.
 *
 * `sinceDays` is how long counts as a while, and is the caller's to choose.
 */
export function neglected(
  rows: RoleTotals[],
  roles: Role[],
  goals: Goal[],
  lastTouched: Map<string, string | null>,
  today: string,
  sinceDays = 14,
): { role: Role; days: number | null }[] {
  const cutoff = addDays(today, -sinceDays)
  const out: { role: Role; days: number | null }[] = []

  for (const role of roles) {
    if (role.archived) continue
    const open = goals.filter(
      (g) => g.roleId === role.id && (g.status === 'active' || g.status === 'paused'),
    )
    if (open.length === 0) continue

    // Time this window counts as a touch on its own: a role you spent six
    // hours on is not neglected however stale its goals' own records are.
    const row = rows.find((r) => r.roleId === role.id)
    if (row && row.actualMinutes > 0) continue

    // The latest moment across its goals. `null` means nothing was ever
    // recorded, which is the strongest case rather than the weakest.
    let latest: string | null = null
    for (const goal of open) {
      const at = lastTouched.get(goal.id) ?? null
      if (at && (!latest || at > latest)) latest = at
    }
    if (latest && latest.slice(0, 10) > cutoff) continue
    out.push({ role, days: latest ? daysBetween(latest.slice(0, 10), today) : null })
  }

  // The stalest first, and never-touched ahead of everything.
  return out.sort((a, b) => (b.days ?? Infinity) - (a.days ?? Infinity))
}

function daysBetween(from: string, to: string): number {
  const a = new Date(`${from}T00:00:00`).getTime()
  const b = new Date(`${to}T00:00:00`).getTime()
  return Math.max(0, Math.round((b - a) / 86_400_000))
}
