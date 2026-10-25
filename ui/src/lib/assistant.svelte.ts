// The assistant's standing work: what it has done, what it is standing by to
// do, what it remembers, and what it left waiting for an answer.
//
// These were the Assistant app's four panes, and are four tabs of Settings
// now. The app is the conversation -- `agent.svelte.ts` -- and the things
// here are what you set up and look over now and then rather than what you
// sit down to do, which is what Settings is for. Nothing about the lists
// themselves changed in the move; `AssistantPanes.svelte` draws them, in
// whichever tab is open.
//
// The one rule worth knowing is about the count on the app bar. A run is
// unseen until somebody looks at "What it did", and *looking* is what clears
// it: showing the tab marks what is on screen as seen -- see `show`. A button
// that had to be pressed would leave a number that nobody could get rid of
// by reading.

import { api } from './api'
import { jobs } from './jobs.svelte'
import { panels } from './panels.svelte'
import { proposals } from './proposals.svelte'
import { app, handle, isLocked } from './state.svelte'
import type {
  Account,
  ChatLinkView,
  Memory,
  MemoryId,
  RoutineId,
  RoutineInfo,
  RoutineRun,
  RoutineRunId,
  Template,
} from './types'

/** How many runs the log shows. A log, not a database. */
const PAGE = 200

export const PANES = ['runs', 'routines', 'memory', 'proposals'] as const
export type Pane = (typeof PANES)[number]

export const PANE_LABELS: Record<Pane, string> = {
  runs: 'What it did',
  routines: 'Routines',
  memory: 'What it remembers',
  proposals: 'Waiting for you',
}

class AssistantState {
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
  /**
   * Where the editor can send a result: the chat links and the mail accounts.
   * Read when the editor opens, so a link set up a minute ago is offered.
   */
  chatLinks = $state<ChatLinkView[]>([])
  mailAccounts = $state<Account[]>([])

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
    this.chatLinks = []
    this.mailAccounts = []
    this.loading = false
  }

  get enabled(): boolean {
    return app.supportsAssistant
  }

  /** The load in flight, so `show` can wait for it rather than race it. */
  #loading: Promise<void> | null = null

  /**
   * Load everything the tabs draw. Coming back refreshes, and asking twice
   * while a load is running waits for that one rather than starting another.
   */
  start(): Promise<void> {
    if (!this.enabled) return Promise.resolve()
    this.#loading ??= this.#load().finally(() => (this.#loading = null))
    return this.#loading
  }

  async #load() {
    this.loading = true
    try {
      // Jobs are their own store, loaded alongside rather than through
      // `refresh`, so that `jobs.start` is the load a caller asking twice
      // waits for and `jobs.loading` is true while it runs.
      await Promise.all([this.#refreshStanding(), jobs.start()])
      await proposals.refresh()
      this.templates = app.supportsRoutines ? await api.routineTemplates() : []
    } finally {
      this.loading = false
    }
  }

  /**
   * A tab has been put on screen: mark what it shows as looked at.
   *
   * *After* any load that is running, because what it marks is what was
   * loaded -- marking before the runs arrive marks nothing, and leaves a
   * badge that no amount of reading could clear. Called for every way a tab
   * can appear (the settings nav, a link from the conversation page, a
   * shortcut), which is why it is keyed on the tab being shown rather than
   * on how anybody got there.
   *
   * "What it did" lists background jobs beside the routine runs, so looking
   * at it clears both counts.
   */
  async show(pane: Pane) {
    if (this.#loading) await this.#loading
    if (pane === 'runs') await Promise.all([this.markSeen(), jobs.markSeen()])
    else if (pane === 'proposals') await proposals.markSeen(proposals.pending)
  }

  async refresh() {
    if (!this.enabled) return
    await Promise.all([this.#refreshStanding(), jobs.refresh()])
  }

  /** Everything `refresh` reloads but the jobs, which report their own errors. */
  async #refreshStanding() {
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
    if (!this.enabled) return
    // The jobs' count is added to the bar's. A vault can have jobs without
    // routines, so this is not behind the check below.
    void jobs.refreshCount()
    if (!app.supportsRoutines) return
    try {
      this.unseen = await api.unseenRuns()
    } catch {
      /* a number that could not be read is not worth reporting */
    }
  }

  /** Open one of the tabs. What it shows is marked seen by `show`. */
  setPane(pane: Pane) {
    panels.openSettings(pane)
  }

  /** A routine's run log, shown or hidden under its card. */
  toggleExpanded(id: RoutineId) {
    this.expanded = this.expanded === id ? null : id
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
        delivery: blank.delivery ?? { ask: false },
        when: '',
      }
      this.setPane('routines')
      await this.#destinations()
    } catch (e) {
      await handle(e)
    }
  }

  edit(routine: RoutineInfo) {
    this.editing = { ...routine, delivery: { ask: false, ...routine.delivery } }
    this.setPane('routines')
    void this.#destinations()
  }

  /**
   * The chat links and mail accounts a result could go to. A vault without
   * either simply has nothing to offer, which the editor says.
   */
  async #destinations() {
    try {
      this.chatLinks = await api.chatLinks()
    } catch {
      this.chatLinks = []
    }
    try {
      this.mailAccounts = (await api.accounts()).filter((a) => a.services.mail)
    } catch {
      this.mailAccounts = []
    }
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
   * Here rather than in the view, like every other action these tabs offer.
   * The view called `api.deleteRun` directly and awaited it from a
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

  /**
   * The memories alone -- for a surface that draws only those, such as the
   * Overview's "Needs attention", which must not pay for the run log and the
   * job list to show what the assistant has noticed.
   */
  async refreshMemories() {
    if (!this.enabled) return
    try {
      this.memories = await api.memories()
    } catch (e) {
      if (isLocked(e)) return
      await handle(e)
    }
  }

  /**
   * Answer something it noticed: `confirmed` makes it a standing instruction,
   * `rejected` strikes it out and keeps it, so a later dream cannot learn it
   * again. The memory list and the Overview's "Needs attention" both ask.
   */
  async answerNoticed(id: MemoryId, origin: 'confirmed' | 'rejected') {
    try {
      await api.setMemoryOrigin(id, origin)
      this.memories = await api.memories()
    } catch (e) {
      await handle(e)
    }
  }
}

export const assistant = new AssistantState()
