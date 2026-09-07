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
import { app, handle } from './state.svelte'
import { addDays, todayIso } from './time'
import { parseQuickAdd } from './quickadd'
import type {
  BlockKind,
  Priority,
  Project,
  ProjectId,
  TagCount,
  Task,
  TaskId,
  TaskQuery,
  TaskStats,
  TaskStatus,
  TimeBlock,
} from './types'
import { TASK_STATUSES, isOpen } from './types'

/** How far ahead "Upcoming" looks. */
const UPCOMING_DAYS = 14

/** What the sidebar has selected. */
export type Scope =
  | { kind: 'today' }
  | { kind: 'upcoming' }
  | { kind: 'inbox' }
  | { kind: 'all' }
  | { kind: 'project'; id: ProjectId }

export type View = 'list' | 'board'
export type GroupBy = 'none' | 'status' | 'due' | 'priority'

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
  /** Finished work is hidden by default; a todo list is about what is left. */
  showDone = $state(false)
  /** Free-text filter over titles, notes and tags. */
  filter = $state('')

  selectedTask = $state<TaskId | null>(null)
  /** Time booked against the open task. Loaded when it is opened. */
  detailBlocks = $state<TimeBlock[]>([])
  /** Subtasks that have been expanded in the list. */
  expanded = $state<Set<TaskId>>(new Set())

  stats = $state<TaskStats | null>(null)
  tags = $state<TagCount[]>([])
  loading = $state(false)
  saving = $state(false)

  /** Tasks edited since the last write, and the timer that writes them. */
  #saves = new Autosave<TaskId>((ids) => this.#writeTasks(ids))
  #started = false

  constructor() {
    // A lock must leave nothing decrypted behind in here either.
    app.onLock(() => this.reset())
  }

  reset() {
    this.#saves.cancel()
    this.projects = []
    this.tasks = []
    this.detailBlocks = []
    this.selectedTask = null
    this.expanded = new Set()
    this.stats = null
    this.tags = []
    this.#started = false
  }

  // ── loading ──────────────────────────────────────────────────────────

  /** First entry into the todo app after an unlock. */
  async start() {
    if (this.#started) return
    this.#started = true
    const view = localStorage.getItem('everyday.todo.view')
    if (view === 'list' || view === 'board') this.view = view
    await this.refresh()
  }

  async refresh() {
    if (!app.supportsTasks) return
    this.loading = true
    try {
      const [projects, tasks, stats, tags] = await Promise.all([
        api.projects(),
        api.tasks(this.query()),
        api.taskStats(),
        api.taskTags(),
      ])
      this.projects = projects
      this.tasks = tasks
      this.stats = stats
      this.tags = tags
      // A selection that has scrolled out of scope is not a selection.
      if (this.selectedTask && !tasks.some((t) => t.id === this.selectedTask)) {
        this.selectedTask = null
        this.detailBlocks = []
      }
    } catch (e) {
      await handle(e)
    } finally {
      this.loading = false
    }
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
    await this.refresh()
  }

  /** Can this scope be shown as a board? */
  get boardable(): boolean {
    return this.scope.kind === 'project' || this.scope.kind === 'inbox' || this.scope.kind === 'all'
  }

  setView(view: View) {
    this.view = view
    localStorage.setItem('everyday.todo.view', view)
    void this.refresh()
  }

  setFilter(text: string) {
    this.filter = text
    void this.refresh()
  }

  // ── the task tree ────────────────────────────────────────────────────

  /**
   * The tasks the list should draw: everything in scope, minus finished work
   * unless it has been asked for.
   *
   * `tasks` stays complete behind this, which is what keeps the subtask
   * counts and the header totals honest while the list is showing less.
   */
  get visible(): Task[] {
    return this.showDone ? this.tasks : this.tasks.filter((t) => isOpen(t.status))
  }

  /** Top-level tasks in scope, each with its loaded subtasks nested. */
  get tree(): TaskNode[] {
    const rows = this.visible
    const byParent = new Map<TaskId, Task[]>()
    const present = new Set(rows.map((t) => t.id))
    const roots: Task[] = []
    for (const task of rows) {
      // A subtask whose parent is out of scope -- a smart list showing one
      // step of a bigger job -- is drawn at the top level rather than
      // vanishing under a parent that is not there.
      if (task.parentId && present.has(task.parentId)) {
        const kids = byParent.get(task.parentId) ?? []
        kids.push(task)
        byParent.set(task.parentId, kids)
      } else {
        roots.push(task)
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

  toggleExpanded(id: TaskId) {
    // Reassigned rather than mutated: a `Set` is not deeply reactive, so the
    // views watching it need a new reference to notice.
    const next = new Set(this.expanded)
    if (!next.delete(id)) next.add(id)
    this.expanded = next
  }

  // ── writing ──────────────────────────────────────────────────────────

  /**
   * Apply `changes` to a task and queue the write.
   *
   * Mutating in place is what keeps the list, the board and the detail panel
   * showing the same thing without a reconciliation pass.
   */
  patch(id: TaskId, changes: Partial<Task>) {
    const task = this.tasks.find((t) => t.id === id)
    if (!task) return
    Object.assign(task, changes)
    task.updatedAt = new Date().toISOString()
    this.#saves.touch(id)
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
      task.sortOrder = this.#nextSortOrder(task.status, opts.parentId ?? null)

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

  /** One past the last task in the column (or under the parent) it joins. */
  #nextSortOrder(status: TaskStatus, parentId: TaskId | null): number {
    return (
      this.tasks
        .filter((t) => (t.parentId ?? null) === parentId && t.status === status)
        .reduce((max, t) => Math.max(max, t.sortOrder), -1) + 1
    )
  }

  async remove(id: TaskId) {
    // Drop it locally first: the subtree goes with it on disk, so waiting
    // would leave orphaned children on screen for a round trip.
    const doomed = this.#subtree(id)
    this.tasks = this.tasks.filter((t) => !doomed.has(t.id))
    if (doomed.has(this.selectedTask ?? '')) {
      this.selectedTask = null
      this.detailBlocks = []
    }
    for (const gone of doomed) this.#saves.forget(gone)
    try {
      await api.deleteTask(id)
      void this.refreshStats()
      void this.refreshTags()
    } catch (e) {
      await handle(e, () => this.refresh())
    }
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
    return this.tasks
      .filter((t) => !t.parentId && t.status === status)
      .sort((a, b) => a.sortOrder - b.sortOrder)
  }

  /** Which columns the board draws: every status that is used, plus the
   *  working ones, so an empty board still has somewhere to drop a card. */
  get columns(): TaskStatus[] {
    return TASK_STATUSES.filter(
      (s) => s !== 'cancelled' || this.tasks.some((t) => t.status === 'cancelled'),
    )
  }

  /**
   * Drop a card into `status` at `index`, renumbering what it displaced.
   *
   * Both affected columns are renumbered and written as one call, because
   * half a reorder on disk is a board that reshuffles itself on next load.
   */
  async move(id: TaskId, status: TaskStatus, index: number) {
    const moved = this.tasks.find((t) => t.id === id)
    if (!moved) return
    const from = moved.status

    const target = this.column(status).filter((t) => t.id !== id)
    target.splice(Math.max(0, Math.min(index, target.length)), 0, moved)

    const touched: Task[] = []
    const renumber = (rows: Task[]) => {
      rows.forEach((task, i) => {
        if (task.sortOrder !== i || task.id === id) {
          task.sortOrder = i
          task.updatedAt = new Date().toISOString()
          if (!touched.includes(task)) touched.push(task)
        }
      })
    }

    if (from !== status) {
      this.setStatus(id, status)
      renumber(this.column(from).filter((t) => t.id !== id))
    }
    renumber(target)

    this.#saves.touchAll(touched.map((t) => t.id))
    await this.flush()
    void this.refreshStats()
  }

  // ── the detail panel ─────────────────────────────────────────────────

  async open(id: TaskId | null) {
    this.selectedTask = id
    this.detailBlocks = []
    if (!id) return
    try {
      this.detailBlocks = await api.blocks({ taskId: id })
    } catch (e) {
      await handle(e)
    }
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

  /** Priority as a sortable number, for views that order by it. */
  rank(p: Priority): number {
    return ['none', 'low', 'medium', 'high', 'urgent'].indexOf(p)
  }
}

export const todo = new TodoState()
