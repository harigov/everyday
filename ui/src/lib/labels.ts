// The words a status or a priority is shown with, wherever a task appears.
//
// `menus.ts`, the task list, the task board and the task detail panel each
// had their own copy of this, and one of the four had already drifted:
// `none` read as "None" everywhere the priority appeared in a menu or a
// select, and as "Unprioritised" in the list's own grouping headings. "None"
// is what the detail panel shows -- the place somebody actually reads a
// task's priority off, rather than a heading above a group of them -- so
// that is the spelling kept here.

import type { Priority, TaskStatus } from './types'

/** How a task's status reads, wherever one is shown. */
export const STATUS_LABELS: Record<TaskStatus, string> = {
  backlog: 'Backlog',
  todo: 'To do',
  doing: 'Doing',
  blocked: 'Blocked',
  done: 'Done',
  cancelled: 'Cancelled',
}

/** How a task's priority reads, wherever one is shown. */
export const PRIORITY_LABELS: Record<Priority, string> = {
  none: 'None',
  low: 'Low',
  medium: 'Medium',
  high: 'High',
  urgent: 'Urgent',
}
