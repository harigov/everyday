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
 * Tasks in the order the list draws them: by hand, and otherwise as loaded.
 *
 * Stable, so tasks that have never been dragged keep the backend's due-date
 * order among themselves -- which is what every section showed before there
 * was a manual order to show.
 */
export function bySortOrder<T extends Pick<Task, 'sortOrder'>>(tasks: T[]): T[] {
  return tasks
    .map((task, i) => ({ task, i }))
    .sort((a, b) => a.task.sortOrder - b.task.sortOrder || a.i - b.i)
    .map((x) => x.task)
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
 * the record: a subtask whose parent is out of scope is drawn at the top
 * level, and dropping beside it has to mean the top level too.
 *
 * Two levels, as everywhere else in the list: a task that has subtasks of
 * its own cannot become one, and only a top-level row can take a drop onto
 * it.
 */
export function planDrop(
  tasks: Task[],
  draggedId: TaskId,
  target: { id: TaskId; zone: Zone; parentId: TaskId | null },
): Placement | null {
  const dragged = tasks.find((t) => t.id === draggedId)
  if (!dragged || draggedId === target.id) return null
  const hasKids = tasks.some((t) => t.parentId === draggedId)

  const loaded = new Set(tasks.map((t) => t.id))
  const siblingsUnder = (parentId: TaskId | null) =>
    bySortOrder(
      tasks.filter((t) =>
        parentId === null ? !t.parentId || !loaded.has(t.parentId) : t.parentId === parentId,
      ),
    )
      .map((t) => t.id)
      .filter((id) => id !== draggedId)

  if (target.zone === 'into') {
    if (hasKids || target.parentId !== null) return null
    return { parentId: target.id, order: [...siblingsUnder(target.id), draggedId] }
  }

  if (target.parentId !== null && hasKids) return null
  // With two levels, the only way to drop a task inside its own subtree is
  // beside one of its own subtasks -- which `hasKids` has already refused.
  const order = siblingsUnder(target.parentId)
  const i = order.indexOf(target.id)
  if (i < 0) return null
  order.splice(target.zone === 'before' ? i : i + 1, 0, draggedId)
  return { parentId: target.parentId, order }
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
