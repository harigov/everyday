// State for the todo app.
//
// A sibling of `state.svelte.ts` rather than more of it: the two apps share
// a vault and a lock, and nothing else. Keeping them apart is what will let
// the calendar be a third file instead of a third of one very large one.
//
// The rule this file is built around: **the task list is the truth, and it
// is edited in place.** A detail panel that held its own copy would mean
// reconciling two versions of the same task on every keystroke; instead the
// panel mutates the object already in `tasks`, the list re-renders from the
// same object, and a debounced write puts it on disk.

import { api } from './api'
import { Autosave } from './autosave'
import { purpose } from './purpose.svelte'
import { pref } from './prefs'
import { proposals, recordAs } from './proposals.svelte'
import { app, handle } from './state.svelte'
import { debounce } from './store/debounce'
import { FocusRequest } from './store/focus-request'
import { latest } from './store/latest'
import { optimisticPatch } from './store/optimistic-patch'
import { optimisticRemove } from './store/optimistic-remove'
import { guardedRefresh } from './store/refresh'
import { addDays, todayIso } from './time'
import { parseQuickAdd } from './quickadd'
import {
  ORDER_STEP,
  byManualOrder,
  drawnAtTop,
  moveCard,
  numberOrder,
  planDrop,
  proposalBelongsInScope,
  sectionPatch,
  type GroupBy,
  type ProposalScope,
  type Zone,
} from './tasklist'
import type {
  BlockKind,
  Priority,
  Project,
  ProjectId,
  Proposal,
  ProposalId,
  TagCount,
  Task,
  TaskId,
  TaskQuery,
  TaskStats,
  TaskStatus,
  TimeBlock,
} from './types'
import { PRIORITIES, TASK_STATUSES, isOpen, priorityRank } from './types'

/** How far ahead "Upcoming" looks. */
const UPCOMING_DAYS = 14

/**
 * Idle delay before a keystroke in the filter box becomes a query.
 *
 * A shade longer than the journal's search debounce because this one is
 * heavier -- filtering matches against sealed titles and notes, so it is a
 * decrypt pass rather than an index lookup.
 */
const FILTER_MS = 160

/** What the sidebar has selected. */
export type Scope =
  | { kind: 'today' }
  | { kind: 'upcoming' }
  | { kind: 'inbox' }
  | { kind: 'all' }
  | { kind: 'project'; id: ProjectId }
  /**
   * Goals, grouped under the roles they belong to.
   *
   * The odd one out: it selects no tasks at all, and the pane it opens is
   * not a task list. It is here rather than in its own app because a goal is
   * the thing tasks are *for* -- it lived in the Overview, three panes away
   * from the work that makes it happen, and moving something under a goal
   * meant remembering the goal's name in another app. `refresh` skips the
   * task query for this scope entirely; see `query`.
   */
  | { kind: 'goals' }

export type View = 'list' | 'board'
export type { GroupBy }

const viewPref = pref<View | null>(
  'everyday.todo.view',
  (raw) => (raw === 'list' || raw === 'board' ? raw : null),
  null,
)

/**
 * The status filter, as the filter bar offers it.
 *
 * `open` first and by default, because a todo list is about what is left --
 * but the rest of the statuses are here as their own chips rather than
 * hidden behind one "show finished" switch. Somebody looking for the thing
 * they cancelled last week, or for everything that is blocked, was
 * previously reduced to turning finished work back on and reading past it.
 *
 * The same shape as the library's `FILTERS`, deliberately: two apps with a
 * row of status chips over a list should not have two ideas of what such a
 * row is.
 */
export const TASK_FILTERS = ['open', 'all', ...TASK_STATUSES] as const
export type TaskFilter = (typeof TASK_FILTERS)[number]

/** The statuses a filter selects. Empty means "do not filter". */
export function statusesFor(filter: TaskFilter): TaskStatus[] {
  if (filter === 'all') return []
  if (filter === 'open') return TASK_STATUSES.filter(isOpen)
  return [filter]
}

/** What a filter chip says. The statuses use their own words elsewhere. */
export const FILTER_LABELS: Record<TaskFilter, string> = {
  open: 'Open',
  all: 'All',
  backlog: 'Backlog',
  todo: 'To do',
  doing: 'Doing',
  blocked: 'Blocked',
  done: 'Done',
  cancelled: 'Cancelled',
}

/** A task and the subtasks hanging off it. What the list view draws. */
export interface TaskNode {
  task: Task
  children: TaskNode[]
}

class TodoState {
  projects = $state<Project[]>([])
  /** Every task in scope, at every level. The tree is derived from it. */
  tasks = $state<Task[]>([])
  scope = $state<Scope>({ kind: 'today' })
  view = $state<View>('list')
  groupBy = $state<GroupBy>('due')
  /** Which statuses the list shows. Open work by default. */
  statusFilter = $state<TaskFilter>('open')
  /** One priority, or every priority. */
  priorityFilter = $state<Priority | null>(null)
  /** One tag, or every tag. */
  tagFilter = $state<string | null>(null)
  /** Free-text filter over titles, notes and tags. */
  filter = $state('')

  selectedTask = $state<TaskId | null>(null)
  /**
   * The pending task proposal open in the detail pane, by id -- not the
   * `Proposal` itself, so that accepting or declining it *anywhere* (a ghost
   * row's own buttons, the Assistant app's Proposals list, another window)
   * closes this pane too, the moment it drops out of `proposals.pending`,
   * rather than leaving it open over a proposal that no longer exists to
   * answer. The two are mutually exclusive with a real task -- opening one
   * closes the other -- because the pane is one rail, not two.
   */
  #selectedProposalId = $state<ProposalId | null>(null)
  /** Time booked against the open task. Loaded when it is opened. */
  detailBlocks = $state<TimeBlock[]>([])
  /**
   * Subtasks that have been *folded away*. Everything else is open.
   *
   * Stored as the exception rather than as the rule, which is the whole of
   * how the default was changed. A task with steps under it is a task whose
   * steps are the interesting part -- they are what is left to do, and the
   * parent is only their heading -- so a list that hides them behind a
   * disclosure is a list that has hidden the work. It cost a click per row
   * to find out what was actually outstanding, every time the app opened.
   */
  #collapsed = $state<Set<TaskId>>(new Set())

  stats = $state<TaskStats | null>(null)
  tags = $state<TagCount[]>([])
  loading = $state(false)
  saving = $state(false)

  /** Tasks edited since the last write, and the timer that writes them. */
  #saves = new Autosave<TaskId>((ids) => this.#writeTasks(ids))
  /** `patch`'s assign-stamp-queue, once. See `store/optimistic-patch.ts`. */
  #patcher = optimisticPatch<TaskId, Task>((id) => this.tasks.find((t) => t.id === id), this.#saves)
  #started = false
  /** Which load is the current one. See `refresh`. */
  #generation = latest()
  /** Pending keystroke-debounce for the filter box. See `setFilter`. */
  #filterDebounce = debounce(() => void this.refresh(), FILTER_MS)

  /**
   * How to put the cursor in the capture line, registered by the view.
   *
   * The same shape as the editor's `bindBody` in the app store, and for the
   * same reason: the input belongs to a component, but the things that want
   * to type into it -- Ctrl/Cmd N, and now the tray -- do not have a
   * reference to that component and should not have to be handed one down
   * through the view tree.
   */
  #capture = new FocusRequest()

  constructor() {
    // A lock must leave nothing decrypted behind in here either.
    app.onLock(() => this.reset())
    // ...and must not throw away what the reset is about to drop.
    app.onFlush(() => this.flush())
    // This window's own accept does not come back as a change event -- see
    // `proposals.svelte.ts` -- so the store that just gained a row tells
    // itself to reload.
    proposals.onAccepted('task', () => this.refresh())
  }

  /** Register (or with `null`, retire) the capture line's focus. */
  bindCapture(fn: (() => void) | null) {
    this.#capture.bind(fn)
  }

  /** Put the cursor in the capture line, now or as soon as there is one. */
  focusCapture() {
    this.#capture.request()
  }

  reset() {
    this.#saves.cancel()
    this.#filterDebounce.cancel()
    this.#capture.forget()
    // Nothing loaded after a lock may land: the vault is shut and the rows
    // it would put on screen are the ones this reset exists to drop.
    this.#generation.next()
    this.projects = []
    this.tasks = []
    this.detailBlocks = []
    this.selectedTask = null
    this.#selectedProposalId = null
    this.#collapsed = new Set()
    this.stats = null
    this.tags = []
    this.#started = false
  }

  // ── loading ──────────────────────────────────────────────────────────

  /** First entry into the todo app after an unlock. */
  async start() {
    if (this.#started) return
    this.#started = true
    const view = viewPref.get()
    if (view) this.view = view
    // The proposals store is loaded once, after unlock, by whichever app
    // opens first -- WP5's wiring does this too, but idempotently, so it is
    // safe to ask again here rather than hope the todo app is never first.
    if (!proposals.loaded) await proposals.refresh()
    await this.refresh()
  }

  /**
   * Re-read everything the todo app draws for the current scope and filter.
   *
   * Numbered, because these overlap: a scope change and a filter keystroke
   * are two queries in the air at once, and the backend is under no
   * obligation to answer the older one last. Only the newest load is allowed
   * to land, so the list can never end up showing the results of a filter
   * that has already been typed past.
   */
  async refresh() {
    if (!app.supportsTasks) return
    await guardedRefresh(
      this.#generation,
      async (isCurrent) => {
        const [projects, tasks, stats, tags] = await Promise.all([
          api.projects(),
          this.showingGoals ? Promise.resolve([]) : api.tasks(this.query()),
          api.taskStats(),
          api.taskTags(),
        ])
        if (!isCurrent()) return
        this.projects = projects
        this.tasks = tasks
        this.stats = stats
        this.tags = tags
        // A selection that has scrolled out of scope is not a selection.
        if (this.selectedTask && !tasks.some((t) => t.id === this.selectedTask)) {
          this.selectedTask = null
          this.detailBlocks = []
        }
      },
      { setLoading: (v) => (this.loading = v), onError: (e) => handle(e) },
    )
  }

  /**
   * The backend query for the current scope.
   *
   * Note what is *not* here: any filter on status. Hiding finished work is a
   * property of the view, applied in `visible` below, and it has to be --
   * otherwise a card reading "1/3 subtasks done" would have to say "0/2",
   * because the done one was never loaded to be counted. Loading everything
   * in scope and cutting it here also makes the Done toggle instant instead
   * of a round trip.
   */
  query(): TaskQuery {
    const today = todayIso()
    const base: TaskQuery = {
      // Sorting is done here rather than in the view so that paging and
      // grouping see the same order the backend chose.
      sort: this.view === 'board' ? 'manual' : 'dueAsc',
      text: this.filter.trim(),
      limit: 2000,
    }

    switch (this.scope.kind) {
      case 'today':
        // Overdue as well as due: something that slipped yesterday is more
        // today's problem than today's own work is.
        return { ...base, dueTo: today }
      case 'upcoming':
        return { ...base, dueFrom: today, dueTo: addDays(today, UPCOMING_DAYS) }
      case 'inbox':
        return { ...base, project: { scope: 'inbox' } }
      case 'project':
        return { ...base, project: { scope: 'project', id: this.scope.id } }
      case 'all':
        return base
      case 'goals':
        // Nothing. The goals pane draws no task list, and asking for two
        // thousand tasks to throw them away would be a query per switch to
        // a pane that never reads the answer.
        return { ...base, limit: 0 }
    }
  }

  async setScope(scope: Scope) {
    await this.flush()
    this.scope = scope
    this.selectedTask = null
    this.detailBlocks = []
    // A board only makes sense where cards have somewhere to sit: the smart
    // lists are date queries, and dragging a card between columns in one
    // would silently change a status the list was not filtered on.
    if (!this.boardable && this.view === 'board') this.view = 'list'
    // The goals pane reads roles, goals and what has happened against each.
    // Loaded on the way in rather than by the component, so that arriving
    // from a widget with a goal already selected does not draw an empty
    // rail for a frame.
    if (scope.kind === 'goals') {
      await purpose.load()
      await purpose.refreshActivity()
    }
    await this.refresh()
  }

  /** Is the pane a list of tasks at all? */
  get showingGoals(): boolean {
    return this.scope.kind === 'goals'
  }

  /** Can this scope be shown as a board? */
  get boardable(): boolean {
    return this.scope.kind === 'project' || this.scope.kind === 'inbox' || this.scope.kind === 'all'
  }

  setView(view: View) {
    this.view = view
    viewPref.set(view)
    void this.refresh()
  }

  /**
   * Filter the list by free text. Debounced, like the journal's search and
   * the library's.
   *
   * It was not, and a filter keystroke is not a cheap thing to repeat: each
   * one is four backend calls, one of which decrypts every task in scope to
   * match against its title and notes. Typing six characters fired that
   * twenty-four times and drew the list from whichever answer arrived last.
   */
  setFilter(text: string) {
    this.filter = text
    this.#filterDebounce.call()
  }

  // ── the task tree ────────────────────────────────────────────────────

  /**
   * The tasks the list should draw: everything in scope that the filter bar
   * has not narrowed away.
   *
   * Applied here rather than in the query, and that is not an accident --
   * see `query` for the argument. `tasks` stays complete behind this, which
   * is what keeps a card reading "1/3 subtasks done" honest while the list
   * is showing one of them, and what makes every chip in the bar instant
   * rather than a round trip.
   */
  get visible(): Task[] {
    const statuses = statusesFor(this.statusFilter)
    return this.tasks.filter(
      (t) =>
        (statuses.length === 0 || statuses.includes(t.status)) &&
        (this.priorityFilter === null || t.priority === this.priorityFilter) &&
        (this.tagFilter === null || t.tags.includes(this.tagFilter)),
    )
  }

  /** How many tasks in scope each status chip would show. */
  countFor(filter: TaskFilter): number {
    const statuses = statusesFor(filter)
    if (statuses.length === 0) return this.tasks.length
    return this.tasks.filter((t) => statuses.includes(t.status)).length
  }

  setStatusFilter(filter: TaskFilter) {
    this.statusFilter = filter
  }

  /** Is anything beyond the default narrowing the list? What "Clear" offers. */
  get narrowed(): boolean {
    return (
      this.statusFilter !== 'open' ||
      this.priorityFilter !== null ||
      this.tagFilter !== null ||
      this.filter.trim() !== ''
    )
  }

  clearFilters() {
    this.statusFilter = 'open'
    this.priorityFilter = null
    this.tagFilter = null
    if (this.filter) this.setFilter('')
  }

  /** The priorities worth offering: the ones actually in use, most first. */
  get usedPriorities(): Priority[] {
    return PRIORITIES.filter((p) => p !== 'none' && this.tasks.some((t) => t.priority === p))
      .slice()
      .sort((a, b) => priorityRank(b) - priorityRank(a))
  }

  /**
   * Top-level tasks in scope, each with its loaded subtasks nested, in manual
   * order -- the board's order, and whatever the list was dragged into.
   */
  get tree(): TaskNode[] {
    const rows = byManualOrder(this.visible)
    const byParent = new Map<TaskId, Task[]>()
    const drawn = this.#drawn()
    const roots: Task[] = []
    for (const task of rows) {
      // A subtask whose parent is not drawn -- a smart list showing one step
      // of a bigger job, or a filter hiding the job -- is drawn at the top
      // level rather than vanishing under a parent that is not there.
      if (drawnAtTop(task, drawn)) {
        roots.push(task)
      } else {
        const kids = byParent.get(task.parentId!) ?? []
        kids.push(task)
        byParent.set(task.parentId!, kids)
      }
    }
    const node = (task: Task): TaskNode => ({
      task,
      children: (byParent.get(task.id) ?? []).map(node),
    })
    return roots.map(node)
  }

  /** Subtask progress for a card or row: `[done, total]`. */
  progressOf(id: TaskId): [number, number] {
    const kids = this.tasks.filter((t) => t.parentId === id)
    return [kids.filter((k) => k.status === 'done').length, kids.length]
  }

  /** The task the detail panel is showing, straight out of the list. */
  get detail(): Task | null {
    return this.tasks.find((t) => t.id === this.selectedTask) ?? null
  }

  projectOf(id: ProjectId | null | undefined): Project | null {
    return this.projects.find((p) => p.id === id) ?? null
  }

  /**
   * Open tasks in a project, counted over the whole vault by the backend.
   *
   * `null` asks for the inbox. Counting the loaded rows instead would report
   * whatever happens to be on screen, which in a smart list is nearly always
   * the wrong number.
   */
  openCount(id: ProjectId | null): number {
    return this.stats?.openByProject.find((c) => (c.projectId ?? null) === id)?.open ?? 0
  }

  /**
   * Move a task to another project, taking its subtasks with it.
   *
   * A subtask left behind in the old project is a step of a job that is no
   * longer there, and it would be swept away with that project's tasks the
   * next time it was deleted -- silently, since its parent would survive.
   */
  setProject(id: TaskId, projectId: ProjectId | null) {
    for (const descendant of this.#subtree(id)) {
      this.patch(descendant, { projectId })
    }
  }

  get project(): Project | null {
    return this.scope.kind === 'project' ? this.projectOf(this.scope.id) : null
  }

  /** The accent for the current scope. */
  get accent(): string {
    return this.project?.color ?? 'var(--accent)'
  }

  /** Are this task's subtasks showing? They are, unless they were folded. */
  isExpanded(id: TaskId): boolean {
    return !this.#collapsed.has(id)
  }

  toggleExpanded(id: TaskId) {
    // Reassigned rather than mutated: a `Set` is not deeply reactive, so the
    // views watching it need a new reference to notice.
    const next = new Set(this.#collapsed)
    if (!next.delete(id)) next.add(id)
    this.#collapsed = next
  }

  // ── writing ──────────────────────────────────────────────────────────

  /**
   * Apply `changes` to a task and queue the write.
   *
   * Mutating in place is what keeps the list, the board and the detail panel
   * showing the same thing without a reconciliation pass.
   */
  patch(id: TaskId, changes: Partial<Task>) {
    this.#patcher.patch(id, changes)
  }

  /** Write every pending edit now. Safe when nothing is dirty. */
  flush(): Promise<void> {
    return this.#saves.flush()
  }

  /** One write for however many tasks were edited. */
  async #writeTasks(ids: ReadonlySet<TaskId>) {
    const pending = this.tasks.filter((t) => ids.has(t.id))
    if (pending.length === 0) return
    this.saving = true
    try {
      await api.saveTasks($state.snapshot(pending))
      // Ticking something off has to move the sidebar counts with it.
      void this.refreshStats()
    } catch (e) {
      await handle(e)
      // Report it *and* rethrow. `Autosave` reads the rejection as "these
      // are still unwritten" and puts them back in the dirty set; swallowing
      // it here would leave the edits in memory only, with nothing left to
      // write them.
      throw e
    } finally {
      this.saving = false
    }
  }

  /**
   * Move a task to `status`, keeping `completedAt` honest.
   *
   * The same rule the core enforces, applied here too so the row updates the
   * instant it is clicked rather than after a round trip.
   */
  setStatus(id: TaskId, status: TaskStatus) {
    const task = this.tasks.find((t) => t.id === id)
    if (!task) return
    this.patch(id, {
      status,
      completedAt: status === 'done' ? (task.completedAt ?? new Date().toISOString()) : null,
    })
  }

  /** Tick or untick. What the checkbox does. */
  toggleDone(id: TaskId) {
    const task = this.tasks.find((t) => t.id === id)
    if (!task) return
    this.setStatus(id, task.status === 'done' ? 'todo' : 'done')
  }

  // ── capture ──────────────────────────────────────────────────────────

  /**
   * Add a task from a quick-add line.
   *
   * Returns the new task so the caller can keep focus and go straight into
   * the next one, which is the entire point of the input this comes from.
   */
  async add(
    line: string,
    opts: { parentId?: TaskId; status?: TaskStatus } = {},
  ): Promise<Task | null> {
    const parsed = parseQuickAdd(line)
    if (!parsed.title) return null

    // Where a new task lands: inside the project being viewed, or -- in a
    // smart list, where there is no project to speak of -- the inbox.
    const parent = opts.parentId ? this.tasks.find((t) => t.id === opts.parentId) : null
    const projectId = parent?.projectId ?? (this.scope.kind === 'project' ? this.scope.id : null)

    try {
      const task = await api.newTask({
        projectId,
        parentId: opts.parentId ?? null,
        status: opts.status ?? parsed.status ?? null,
      })
      task.title = parsed.title
      task.tags = parsed.tags
      task.priority = parsed.priority
      task.estimateMinutes = parsed.estimateMinutes
      task.dueTime = parsed.dueTime
      task.dueDate = parsed.dueDate ?? this.defaultDueDate()
      // New work goes to the end of whatever it is joining, not the top: a
      // list that reorders itself as you fill it in is impossible to type
      // into.
      task.sortOrder = this.#nextSortOrder(opts.parentId ?? null)

      await api.saveTask($state.snapshot(task))
      // Show it immediately rather than waiting for a round trip, and leave
      // it there until the next refresh even if it does not match the
      // current filter -- adding a task in "Upcoming" and watching it
      // disappear because it has no date reads as a failure, where a list
      // that is briefly broader than its query does not.
      this.tasks = [...this.tasks, task]
      void this.refreshStats()
      void this.refreshTags()
      return task
    } catch (e) {
      await handle(e)
      return null
    }
  }

  /**
   * The due date a task should get when the line did not say.
   *
   * Only the Today list infers one. Adding to "Today" and having it land in
   * a list you are not looking at is the single most annoying thing a todo
   * app can do.
   */
  private defaultDueDate(): string | null {
    return this.scope.kind === 'today' ? todayIso() : null
  }

  /**
   * After every task it joins, whatever their status: the end of the list,
   * and so the end of whichever board column it lands in too.
   */
  #nextSortOrder(parentId: TaskId | null): number {
    const siblings = this.tasks.filter((t) => (t.parentId ?? null) === parentId)
    if (siblings.length === 0) return 0
    return Math.max(...siblings.map((t) => t.sortOrder)) + ORDER_STEP
  }

  async remove(id: TaskId) {
    await optimisticRemove({
      // Drop it locally first: the subtree goes with it on disk, so waiting
      // would leave orphaned children on screen for a round trip.
      optimistic: () => {
        const doomed = this.#subtree(id)
        this.tasks = this.tasks.filter((t) => !doomed.has(t.id))
        if (doomed.has(this.selectedTask ?? '')) {
          this.selectedTask = null
          this.detailBlocks = []
        }
        for (const gone of doomed) this.#saves.forget(gone)
      },
      call: () => api.deleteTask(id),
      onSuccess: () => {
        void this.refreshStats()
        void this.refreshTags()
      },
      rollback: () => this.refresh(),
    })
  }

  #subtree(root: TaskId): Set<TaskId> {
    const out = new Set<TaskId>([root])
    let grew = true
    while (grew) {
      grew = false
      for (const t of this.tasks) {
        if (t.parentId && out.has(t.parentId) && !out.has(t.id)) {
          out.add(t.id)
          grew = true
        }
      }
    }
    return out
  }

  // ── the board ────────────────────────────────────────────────────────

  /**
   * Top-level cards in one column, in manual order.
   *
   * Read from `tasks` rather than from `tree`: a board's Done column is the
   * point of having a board, so it is never subject to the list's Done
   * toggle.
   */
  column(status: TaskStatus): Task[] {
    return byManualOrder(this.tasks.filter((t) => !t.parentId && t.status === status))
  }

  /** Which columns the board draws: every status that is used, plus the
   *  working ones, so an empty board still has somewhere to drop a card. */
  get columns(): TaskStatus[] {
    return TASK_STATUSES.filter(
      (s) => s !== 'cancelled' || this.tasks.some((t) => t.status === 'cancelled'),
    )
  }

  /**
   * Drop a card into `status` at `index`.
   *
   * The order is the list's too, so the card is placed among every top-level
   * task rather than only its column's. Whatever has to be renumbered is
   * written in one call, because half a reorder on disk is a board that
   * reshuffles itself on next load.
   */
  async move(id: TaskId, status: TaskStatus, index: number) {
    const moved = this.tasks.find((t) => t.id === id)
    if (!moved) return
    if (moved.status !== status) this.setStatus(id, status)
    this.#renumber(moveCard(this.tasks, id, status, index), id)
    await this.flush()
    void this.refreshStats()
  }

  /** Write the `sortOrder`s that make `order` read in order. */
  #renumber(order: TaskId[], moved: TaskId) {
    const current = new Map(this.tasks.map((t) => [t.id, t.sortOrder]))
    for (const [id, sortOrder] of numberOrder(order, current, moved)) {
      this.patch(id, { sortOrder })
    }
  }

  /** The tasks the list has on screen, for deciding what is top level. */
  #drawn(): Set<TaskId> {
    return new Set(this.visible.map((t) => t.id))
  }

  // ── the list ─────────────────────────────────────────────────────────

  /**
   * Can `id` be dropped here? What the list asks on every `dragover`, so the
   * cursor refuses a drop before the pointer is released rather than after.
   *
   * `section` is the grouping key of the row dropped on; it only matters
   * when the drop leaves the task at the top level, since a subtask is
   * drawn in its parent's section whatever its own fields say.
   */
  canDrop(
    id: TaskId,
    target: { id: TaskId; zone: Zone; parentId: TaskId | null },
    section: string,
  ) {
    const plan = planDrop(this.tasks, this.#drawn(), id, target)
    if (!plan) return false
    return plan.parentId !== null || this.#sectionPatchFor(id, section) !== null
  }

  /**
   * Drop a row in the list, beside another or onto it.
   *
   * What has to be renumbered is written in one call, for the reason `move`
   * gives. A task that becomes a subtask takes its new
   * parent's project, because a subtask filed somewhere other than its
   * parent is the thing `setProject` exists to prevent.
   */
  async place(
    id: TaskId,
    target: { id: TaskId; zone: Zone; parentId: TaskId | null },
    section: string,
  ) {
    const task = this.tasks.find((t) => t.id === id)
    const plan = planDrop(this.tasks, this.#drawn(), id, target)
    if (!task || !plan) return
    const joining = plan.parentId === null ? this.#sectionPatchFor(id, section) : {}
    if (!joining) return

    if ((task.parentId ?? null) !== plan.parentId) {
      const parent = plan.parentId ? this.tasks.find((t) => t.id === plan.parentId) : null
      this.patch(id, {
        parentId: plan.parentId,
        ...(parent && (parent.projectId ?? null) !== (task.projectId ?? null)
          ? { projectId: parent.projectId ?? null }
          : {}),
      })
      // Dropped onto a folded task, it would vanish into the fold.
      if (parent && !this.isExpanded(parent.id)) this.toggleExpanded(parent.id)
    }

    const { status, ...rest } = joining
    if (status) this.setStatus(id, status)
    if (Object.keys(rest).length > 0) this.patch(id, rest)

    this.#renumber(plan.order, id)

    await this.flush()
  }

  #sectionPatchFor(id: TaskId, section: string) {
    const task = this.tasks.find((t) => t.id === id)
    if (!task) return null
    return sectionPatch(this.groupBy, section, task, {
      today: todayIso(),
      projectPurpose: this.projectOf(task.projectId)?.purpose ?? null,
    })
  }

  // ── the detail panel ─────────────────────────────────────────────────

  async open(id: TaskId | null) {
    this.selectedTask = id
    this.#selectedProposalId = null
    this.detailBlocks = []
    if (!id) return
    try {
      this.detailBlocks = await api.blocks({ taskId: id })
    } catch (e) {
      await handle(e)
    }
  }

  /**
   * The pending task proposal open in the detail pane, or `null` once it has
   * been answered from anywhere -- see `#selectedProposalId`'s own doc.
   */
  get selectedProposal(): Proposal | null {
    return this.#selectedProposalId
      ? (proposals.pending.find((p) => p.id === this.#selectedProposalId) ?? null)
      : null
  }

  /** Open a pending task proposal in the detail pane, in place of a real task. */
  openProposal(p: Proposal) {
    this.selectedTask = null
    this.detailBlocks = []
    this.#selectedProposalId = p.id
  }

  closeProposal() {
    this.#selectedProposalId = null
  }

  // ── proposals ────────────────────────────────────────────────────────
  //
  // Ghosts: pending task proposals drawn where the task they would create
  // would land, at the foot of the section it would join. See
  // `tasklist.ts`'s `proposalBelongsInScope` and `sectionKeyOf` for the
  // pure arithmetic; this is just the scope this view is currently showing.

  /** This view's scope, reduced to what a ghost's placement needs. */
  get proposalScope(): ProposalScope {
    switch (this.scope.kind) {
      case 'upcoming':
        return { kind: 'upcoming', to: addDays(todayIso(), UPCOMING_DAYS) }
      case 'project':
        return { kind: 'project', id: this.scope.id }
      default:
        return { kind: this.scope.kind }
    }
  }

  /**
   * Pending `create` proposals for a task that would land in what is on
   * screen right now -- the inbox, this project, or (in a date-based smart
   * list) on a due date within it.
   */
  get ghostTasks(): Proposal[] {
    const today = todayIso()
    const scope = this.proposalScope
    return proposals.forKind('task').filter((p) => {
      if (p.payload.type !== 'create') return false
      const rec = recordAs(p, 'task')
      return !!rec && proposalBelongsInScope(scope, rec, today)
    })
  }

  /** The pending `replace` proposal for a task, drawn beneath its row. */
  replaceProposalFor(id: TaskId): Proposal | null {
    return (
      proposals
        .forKind('task')
        .find((p) => p.payload.type === 'replace' && recordAs(p, 'task')?.id === id) ?? null
    )
  }

  /** The pending `delete` proposal for a task, drawn as a chip on its row. */
  deleteProposalFor(id: TaskId): Proposal | null {
    return (
      proposals.forKind('task').find((p) => p.payload.type === 'delete' && p.payload.id === id) ??
      null
    )
  }

  /** Minutes booked against the open task, split by plan and record. */
  get detailMinutes(): { planned: number; logged: number } {
    let planned = 0
    let logged = 0
    for (const b of this.detailBlocks) {
      const mins = Math.max(0, Math.round((Date.parse(b.end) - Date.parse(b.start)) / 60_000))
      if (b.kind === 'actual') logged += mins
      else planned += mins
    }
    return { planned, logged }
  }

  /**
   * Book time against the open task.
   *
   * `start` is a local `YYYY-MM-DDTHH:MM` as an `<input type=datetime-local>`
   * produces it; the core resolves the zone and the day it is filed under.
   */
  async addBlock(start: string, minutes: number, kind: BlockKind) {
    const id = this.selectedTask
    if (!id) return
    try {
      const block = await api.newBlock({
        subject: { type: 'task', id },
        start: new Date(start).toISOString(),
        minutes,
        kind,
      })
      await api.saveBlock(block)
      this.detailBlocks = [...this.detailBlocks, block].sort((a, b) =>
        a.start.localeCompare(b.start),
      )
      void this.refreshStats()
    } catch (e) {
      await handle(e)
    }
  }

  async removeBlock(id: string) {
    this.detailBlocks = this.detailBlocks.filter((b) => b.id !== id)
    try {
      await api.deleteBlock(id)
      void this.refreshStats()
    } catch (e) {
      await handle(e)
    }
  }

  // ── projects ─────────────────────────────────────────────────────────

  async newProject(name: string, color: string): Promise<boolean> {
    try {
      const project = await api.newProject(name)
      project.color = color
      project.sortOrder = this.projects.length
      await api.saveProject($state.snapshot(project))
      this.projects = [...this.projects, project]
      await this.setScope({ kind: 'project', id: project.id })
      return true
    } catch (e) {
      await handle(e)
      return false
    }
  }

  async saveProject(project: Project) {
    const i = this.projects.findIndex((p) => p.id === project.id)
    if (i >= 0) this.projects[i] = project
    try {
      await api.saveProject($state.snapshot(project))
    } catch (e) {
      await handle(e)
    }
  }

  async removeProject(id: ProjectId) {
    try {
      await api.deleteProject(id)
    } catch (e) {
      await handle(e)
      return
    }
    this.projects = this.projects.filter((p) => p.id !== id)
    if (this.scope.kind === 'project' && this.scope.id === id) {
      await this.setScope({ kind: 'today' })
    } else {
      await this.refresh()
    }
  }

  /** Projects worth showing in the sidebar: everything not archived. */
  get liveProjects(): Project[] {
    return this.projects.filter((p) => p.status !== 'archived')
  }

  /**
   * Re-read the counts the sidebar draws.
   *
   * Cheap enough to run after every write: the backend answers it entirely
   * from clear index columns and decrypts nothing. Public because booking
   * time in the calendar moves these numbers too -- `blocks` and
   * `loggedMinutes` are in here -- and the sidebar is shared.
   */
  async refreshStats() {
    try {
      this.stats = await api.taskStats()
    } catch {
      /* the header counts are not worth failing a save over */
    }
  }

  /**
   * Re-read the tag list behind the tag field's autocomplete.
   *
   * Kept apart from the counts because this one *is* a decrypt-and-count
   * pass over the vault -- tags are sealed -- so it runs when a tag can have
   * appeared or gone, not on every autosave.
   */
  private async refreshTags() {
    try {
      this.tags = await api.taskTags()
    } catch {
      /* autocomplete is not worth failing a save over */
    }
  }

  // ── derived counts for the sidebar ───────────────────────────────────

  /**
   * Open tasks due today or already overdue, across the whole vault.
   *
   * From the backend rather than from the loaded rows, so the badge is there
   * -- and right -- while you are looking at some other list, which is
   * exactly when it is worth having.
   */
  get dueTodayCount(): number {
    return this.stats?.dueToday ?? 0
  }

  /** Is this task past its deadline? */
  overdue(task: Task): boolean {
    return isOpen(task.status) && !!task.dueDate && task.dueDate < todayIso()
  }
}

export const todo = new TodoState()
