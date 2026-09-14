// The list view's order: which section a task is in, where it sits in that
// section, and what dragging it somewhere else changes.
//
// No Svelte and no store in it, for the same reason `menu.ts` has none: a
// drop that quietly makes a task its own grandparent, or lands it in a
// section it then falls straight back out of, is the kind of mistake nobody
// finds by dragging things around for a minute. `scripts/tasklist.test.mjs`
// checks it instead.

import { friendlyDate } from './format'
import { addDays } from './time'
import type { Priority, Purpose, Task, TaskId, TaskStatus } from './types'

export type GroupBy = 'none' | 'status' | 'due' | 'priority' | 'purpose'

export interface Bucket {
  key: string
  label: string
  order: number
}

/**
 * The bucket a deadline falls into.
 *
 * Overdue is first and separate: something that slipped is a different
 * kind of problem from something that is merely soon, and folding the two
 * together is how a list stops being looked at.
 */
export function dueBucket(task: Task, today: string): Bucket {
  if (!task.dueDate) return { key: 'none', label: 'No date', order: 9 }
  if (task.dueDate < today) return { key: 'overdue', label: 'Overdue', order: 0 }
  if (task.dueDate === today) return { key: 'today', label: 'Today', order: 1 }
  // Within a week, the weekday alone is what people navigate by; beyond
  // it, the date. `friendlyDate` already draws that line.
  if (task.dueDate <= addDays(today, 7)) {
    return { key: task.dueDate, label: friendlyDate(task.dueDate), order: 2 }
  }
  return { key: 'later', label: 'Later', order: 8 }
}

/** How a purpose is spelled as a section key. */
export function purposeKey(purpose: Purpose | null | undefined): string {
  return purpose ? `${purpose.type}:${purpose.id}` : 'none'
}

/**
 * Tasks in their manual order: the one order the list and the board share.
 *
 * Ties -- tasks numbered before the two views shared an order, or numbered
 * in different scopes -- fall back to when each was added, and then to its
 * id. Never to the order they were loaded in: the board loads by manual
 * order and the list by due date, and a tie broken by that would put the
 * same two tasks one way round on the board and the other in the list.
 */
export function byManualOrder<T extends Pick<Task, 'sortOrder' | 'createdAt' | 'id'>>(
  tasks: T[],
): T[] {
  return [...tasks].sort(
    (a, b) =>
      a.sortOrder - b.sortOrder ||
      a.createdAt.localeCompare(b.createdAt) ||
      a.id.localeCompare(b.id),
  )
}

/**
 * Is this task drawn at the top of the list, given the set of tasks drawn?
 *
 * A subtask is, when its parent is not drawn -- out of scope, or hidden by
 * a filter -- rather than vanishing with it. `tree` and `planDrop` both ask
 * this, so what the drop logic thinks is the top level is what is on screen.
 */
export function drawnAtTop(task: Task, drawn: ReadonlySet<TaskId>): boolean {
  return !task.parentId || (drawn.has(task.id) && !drawn.has(task.parentId))
}

/**
 * The spacing a renumbered order is given. Room between neighbours is what
 * lets a later drop be one write instead of a write for every task after it.
 */
export const ORDER_STEP = 1024

/**
 * The `sortOrder` values to write so `order` reads in that order, given
 * each task's `current` value: only the ones that change.
 *
 * `moved` is the task that was dropped. If everything else is already in
 * strictly increasing order and there is an integer between its new
 * neighbours, it alone is written. Otherwise the whole order is renumbered
 * at `ORDER_STEP` apart, which makes room for the next drops.
 */
export function numberOrder(
  order: TaskId[],
  current: ReadonlyMap<TaskId, number>,
  moved: TaskId,
): Map<TaskId, number> {
  const value = (id: TaskId) => current.get(id) ?? 0
  const others = order.filter((id) => id !== moved).map(value)
  if (others.every((v, i) => i === 0 || v > others[i - 1]!)) {
    const i = order.indexOf(moved)
    const prev = i > 0 ? value(order[i - 1]!) : null
    const next = i < order.length - 1 ? value(order[i + 1]!) : null
    let slot: number | null = null
    if (prev === null && next === null) slot = 0
    else if (prev === null) slot = next! - ORDER_STEP
    else if (next === null) slot = prev + ORDER_STEP
    else if (next - prev > 1) slot = Math.floor((prev + next) / 2)
    if (slot !== null)
      return value(moved) === slot ? new Map<TaskId, number>() : new Map([[moved, slot]])
  }
  const out = new Map<TaskId, number>()
  order.forEach((id, k) => {
    if (value(id) !== k * ORDER_STEP) out.set(id, k * ORDER_STEP)
  })
  return out
}

/**
 * The top-level order after a card is dropped into `status` at `index`.
 *
 * The board shows one column at a time of an order that spans them all, so
 * a drop is placed relative to its neighbours in that column: before the
 * card it lands above, or after the last card if it lands at the foot.
 */
export function moveCard(tasks: Task[], id: TaskId, status: TaskStatus, index: number): TaskId[] {
  const roots = byManualOrder(tasks.filter((t) => !t.parentId && t.id !== id))
  const column = roots.filter((t) => t.status === status)
  const order = roots.map((t) => t.id)
  const at =
    index < column.length
      ? order.indexOf(column[Math.max(0, index)]!.id)
      : column.length > 0
        ? order.indexOf(column[column.length - 1]!.id) + 1
        : order.length
  order.splice(at, 0, id)
  return order
}

/** Where a row was dropped on: above it, below it, or onto it. */
export type Zone = 'before' | 'after' | 'into'

/**
 * Which zone of a row the pointer is in.
 *
 * With nesting on offer, the middle half of the row means "onto" and only
 * the outer quarters mean "beside"; without it, the row splits in two. A row
 * whose subtasks are showing below it has no "after": the gap under it is
 * visibly the top of its own subtasks, so that half means "onto" -- or, for
 * a task that cannot nest, the whole row means "above".
 */
export function zoneAt(
  y: number,
  box: { top: number; height: number },
  opts: { nest: boolean; openParent: boolean },
): Zone {
  const at = (y - box.top) / box.height
  if (opts.openParent) return !opts.nest || at < 0.25 ? 'before' : 'into'
  if (!opts.nest) return at < 0.5 ? 'before' : 'after'
  return at < 0.25 ? 'before' : at > 0.75 ? 'after' : 'into'
}

/** Where a drop puts a task: under which parent, and in what order there. */
export interface Placement {
  parentId: TaskId | null
  /** Every sibling at the destination, the dropped task included, in order. */
  order: TaskId[]
}

/**
 * Work out a drop, or `null` if it is not one the list allows.
 *
 * `target.parentId` is the parent the row is *drawn* under, not the one on
 * the record: a subtask whose parent is not drawn is drawn at the top level,
 * and dropping beside it has to mean the top level too. `drawn` is the set
 * of tasks on screen, which is what decides that -- see `drawnAtTop`.
 *
 * Two levels, as everywhere else in the list: a task that has subtasks of
 * its own cannot become one, and only a task with no parent can take a drop
 * onto it. A subtask drawn at the top level still has one, and a task
 * dropped onto it would be a third level down.
 */
export function planDrop(
  tasks: Task[],
  drawn: ReadonlySet<TaskId>,
  draggedId: TaskId,
  target: { id: TaskId; zone: Zone; parentId: TaskId | null },
): Placement | null {
  const dragged = tasks.find((t) => t.id === draggedId)
  const onto = tasks.find((t) => t.id === target.id)
  if (!dragged || !onto || draggedId === target.id) return null
  const hasKids = tasks.some((t) => t.parentId === draggedId)

  const siblingsUnder = (parentId: TaskId | null) =>
    byManualOrder(
      tasks.filter((t) => (parentId === null ? drawnAtTop(t, drawn) : t.parentId === parentId)),
    )
      .map((t) => t.id)
      .filter((id) => id !== draggedId)

  if (target.zone === 'into') {
    if (hasKids || target.parentId !== null || onto.parentId) return null
    return { parentId: target.id, order: [...siblingsUnder(target.id), draggedId] }
  }

  if (target.parentId !== null && hasKids) return null
  // With two levels, the only way to drop a task inside its own subtree is
  // beside one of its own subtasks -- which `hasKids` has already refused.
  // Two subtasks of one hidden parent are both drawn at the top level, and
  // putting one beside the other is reordering the steps of that job -- not
  // lifting one out of it.
  const parentId =
    target.parentId === null && onto.parentId && dragged.parentId === onto.parentId
      ? onto.parentId
      : target.parentId
  const order = siblingsUnder(parentId)
  const i = order.indexOf(target.id)
  if (i < 0) return null
  order.splice(target.zone === 'before' ? i : i + 1, 0, draggedId)
  return { parentId, order }
}

/** The fields a top-level drop into another section rewrites. */
export type SectionPatch = Partial<{
  status: TaskStatus
  priority: Priority
  dueDate: string | null
  dueTime: string | null
  purpose: Purpose | null
}>

/**
 * What has to change for `task` to belong in the section `key`, or `null`
 * if nothing can make it.
 *
 * The board's rule, applied to the list: dropping a card in another column
 * changes its status, so dropping a row in another section changes whatever
 * that section is cut by. An empty patch means it already belongs.
 *
 * The due-date sections that span more than one date -- "Overdue" and
 * "Later" -- have no single date to give a task, so they only take the tasks
 * already in them. "Not filed" refuses a task whose project is filed, since
 * clearing the task's own purpose would leave it inheriting the project's
 * and drawn right back where it came from.
 */
export function sectionPatch(
  groupBy: GroupBy,
  key: string,
  task: Task,
  ctx: { today: string; projectPurpose: Purpose | null },
): SectionPatch | null {
  switch (groupBy) {
    case 'none':
      return {}
    case 'status':
      return task.status === key ? {} : { status: key as TaskStatus }
    case 'priority':
      return task.priority === key ? {} : { priority: key as Priority }
    case 'due': {
      if (dueBucket(task, ctx.today).key === key) return {}
      if (key === 'overdue' || key === 'later') return null
      if (key === 'none') return { dueDate: null, dueTime: null }
      return { dueDate: key === 'today' ? ctx.today : key }
    }
    case 'purpose': {
      if (purposeKey(task.purpose ?? ctx.projectPurpose) === key) return {}
      if (key === 'none') return ctx.projectPurpose ? null : { purpose: null }
      const colon = key.indexOf(':')
      return { purpose: { type: key.slice(0, colon), id: key.slice(colon + 1) } as Purpose }
    }
  }
}
