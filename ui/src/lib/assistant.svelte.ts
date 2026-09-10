// State for the Assistant app.
//
// Three panes over three lists: what the assistant has done, what it is
// standing by to do, and what it remembers. The rail is not here -- it lives
// in `agent.svelte.ts` and works in every app, including this one.
//
// The one rule worth knowing is about the count on the app bar. A run is
// unseen until somebody looks at the Runs pane, and *looking* is what clears
// it: opening the pane marks what is on screen as seen. A button that had to
// be pressed would leave a number that nobody could get rid of by reading.

import { api } from './api'
import { app, handle, isLocked } from './state.svelte'
import type { Memory, RoutineId, RoutineInfo, RoutineRun, RoutineRunId, Template } from './types'

/** How many runs the log shows. A log, not a database. */
const PAGE = 200

export const PANES = ['runs', 'routines', 'memory'] as const
export type Pane = (typeof PANES)[number]

export const PANE_LABELS: Record<Pane, string> = {
  runs: 'What it did',
  routines: 'Routines',
  memory: 'What it remembers',
}

class AssistantState {
  pane = $state<Pane>('runs')
  routines = $state<RoutineInfo[]>([])
  runs = $state<RoutineRun[]>([])
  memories = $state<Memory[]>([])
  templates = $state<Template[]>([])
  /** Runs nobody has looked at. Drawn on the app bar. */
  unseen = $state(0)
  loading = $state(false)

  /** The routine open in the editor, or `null`. A draft, saved explicitly. */
  editing = $state<RoutineInfo | null>(null)
  /** The run whose transcript is open in the rail. */
  openRun = $state<RoutineRunId | null>(null)
  /** One routine's log, when a row is expanded. */
  expanded = $state<RoutineId | null>(null)

  constructor() {
    app.onLock(() => this.reset())
  }

  reset() {
    this.routines = []
    this.runs = []
    this.memories = []
    this.templates = []
    this.unseen = 0
    this.editing = null
    this.openRun = null
    this.expanded = null
    this.loading = false
  }

  get enabled(): boolean {
    return app.supportsAssistant
  }

  /** Load everything the app draws. Idempotent, so coming back refreshes. */
  async start() {
    if (!this.enabled) return
    this.loading = true
    try {
      await this.refresh()
      this.templates = app.supportsRoutines ? await api.routineTemplates() : []
    } finally {
      this.loading = false
    }
    // *After* the load, and unconditional. Reading is what clears the count,
    // and the Runs pane is the one this app opens on -- so arriving here by
    // pressing Assistant never goes through `setPane` and would otherwise
    // leave a badge that no amount of reading could clear.
    if (this.pane === 'runs') await this.markSeen()
  }

  async refresh() {
    if (!this.enabled) return
    try {
      this.memories = await api.memories()
      if (app.supportsRoutines) {
        this.routines = await api.routines()
        this.runs = await api.runs({ limit: PAGE })
        this.unseen = await api.unseenRuns()
      }
    } catch (e) {
      if (isLocked(e)) return
      await handle(e)
    }
  }

  /**
   * Only the count, for the app bar.
   *
   * Its own call because the bar is drawn in every app and must not pay for
   * three lists it is not showing.
   */
  async refreshCount() {
    if (!this.enabled || !app.supportsRoutines) return
    try {
      this.unseen = await api.unseenRuns()
    } catch {
      /* a number that could not be read is not worth reporting */
    }
  }

  setPane(pane: Pane) {
    this.pane = pane
    if (pane === 'runs') void this.markSeen()
  }

  /**
   * Mark what is on screen as looked at.
   *
   * Reading is what clears the count. The alternative -- a button -- leaves a
   * number nobody can get rid of by doing the thing the number is asking for.
   */
  async markSeen() {
    if (this.unseen === 0) return
    const ids = this.runs.filter((r) => !r.seen).map((r) => r.id)
    if (ids.length === 0) return
    try {
      await api.markRunsSeen(ids)
      for (const run of this.runs) run.seen = true
      this.unseen = await api.unseenRuns()
    } catch (e) {
      await handle(e)
    }
  }

  // ── routines ──────────────────────────────────────────────────────────

  /**
   * Start a new routine from a template, or from nothing.
   *
   * The blank comes from the service, because the id and the timestamps are
   * the core's to allocate. `crypto.randomUUID` would want a secure context
   * the packaged webview does not always have, and would mint a v4 where
   * everything else in the vault is v7.
   */
  async draft(template?: Template) {
    if (!app.supportsRoutines) return
    try {
      const blank = await api.newRoutine()
      this.editing = {
        ...blank,
        name: template?.name ?? blank.name,
        instructions: template?.instructions ?? blank.instructions,
        trigger: template?.trigger ?? blank.trigger,
        when: '',
      }
      this.pane = 'routines'
    } catch (e) {
      await handle(e)
    }
  }

  edit(routine: RoutineInfo) {
    this.editing = { ...routine }
    this.pane = 'routines'
  }

  cancelEdit() {
    this.editing = null
  }

  async save() {
    const draft = this.editing
    if (!draft) return
    try {
      // `when` and `nextDue` are derived, so they are dropped rather than
      // sent back: the service works them out from the trigger.
      const { when: _when, nextDue: _next, ...routine } = $state.snapshot(draft)
      await api.saveRoutine(routine)
      this.editing = null
      await this.refresh()
    } catch (e) {
      await handle(e)
    }
  }

  async toggle(routine: RoutineInfo) {
    try {
      const { when: _when, nextDue: _next, ...rest } = $state.snapshot(routine)
      await api.saveRoutine({ ...rest, enabled: !rest.enabled })
      await this.refresh()
    } catch (e) {
      await handle(e)
    }
  }

  async remove(id: RoutineId) {
    try {
      await api.deleteRoutine(id)
      if (this.editing?.id === id) this.editing = null
      if (this.expanded === id) this.expanded = null
      await this.refresh()
    } catch (e) {
      await handle(e)
    }
  }

  /** Ask for a run now. Queued; the scheduler carries it out. */
  /**
   * Drop one run from the log.
   *
   * Here rather than in the view, like every other action this app offers.
   * `AssistantView` called `api.deleteRun` directly and awaited it from a
   * `void` context, so a failure -- a read-only vault, one another window had
   * locked -- left the row on screen, said nothing, and became an unhandled
   * rejection nobody sees.
   */
  async removeRun(id: RoutineRunId) {
    try {
      await api.deleteRun(id)
      await this.refresh()
    } catch (e) {
      await handle(e)
    }
  }

  async runNow(id: RoutineId) {
    try {
      await api.runRoutine(id)
      await this.refresh()
    } catch (e) {
      await handle(e)
    }
  }

  /** One routine's log, or all of them. */
  logFor(id: RoutineId): RoutineRun[] {
    return this.runs.filter((r) => r.routineId === id)
  }

  // ── memory ────────────────────────────────────────────────────────────

  /**
   * Add a fact by hand.
   *
   * Pinned, because a fact somebody typed is not one the assistant's own
   * housekeeping may evict when it runs out of room. The pin can be cleared
   * again from the same list, which is why the service no longer forces it.
   */
  async remember(text: string) {
    const trimmed = text.trim()
    if (!trimmed) return
    try {
      // Minted by the service, as every other record here is.
      const blank = await api.newMemory()
      this.memories = await api.saveMemory({ ...blank, text: trimmed })
    } catch (e) {
      await handle(e)
    }
  }

  async rewrite(memory: Memory, text: string) {
    const trimmed = text.trim()
    if (!trimmed || trimmed === memory.text) return
    try {
      this.memories = await api.saveMemory({
        ...$state.snapshot(memory),
        text: trimmed,
        updatedAt: new Date().toISOString(),
      })
    } catch (e) {
      await handle(e)
    }
  }

  async togglePinned(memory: Memory) {
    try {
      this.memories = await api.saveMemory({
        ...$state.snapshot(memory),
        pinned: !memory.pinned,
        updatedAt: new Date().toISOString(),
      })
    } catch (e) {
      await handle(e)
    }
  }

  async forget(id: string) {
    try {
      await api.deleteMemory(id)
      this.memories = await api.memories()
    } catch (e) {
      await handle(e)
    }
  }
}

export const assistant = new AssistantState()
