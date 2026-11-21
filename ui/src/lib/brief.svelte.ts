// The daily briefs: the two routines behind them, what they wrote, and the
// three things done with one -- switch it on, make one now, listen to it.
//
// Drawn in three places from this one store: the Overview's Brief page, the
// brief card on its Home page, and Settings → Briefs. A `routine` or
// `routineRun` change reloads it (the `assistant` target in
// `live.svelte.ts`), so a brief the scheduler finishes at seven appears
// without anybody pressing anything. See `docs/plans/briefs.md`.

import { api } from './api'
import { KIND_INFO, briefRoutineOf } from './brief'
import { listening } from './listen.svelte'
import { app, handle, isLocked } from './state.svelte'
import type { BriefKind, Routine, RoutineInfo, RoutineRun, RoutineRunId } from './types'

/** How many briefs are kept loaded: a fortnight of mornings and evenings. */
const HISTORY = 28

/** What `making` holds for a brief asked for whose run is not queued yet. */
const PENDING = 'pending' as RoutineRunId

/** How often a brief being made is looked at. */
const POLL_MS = 3_000

/** How long a brief being made is waited for before the page stops saying
 *  so: the scheduler's own ceiling, and then some. */
const GIVE_UP_MS = 20 * 60_000

class BriefState {
  /** What the briefs wrote, newest first. Finished runs, each with a brief. */
  briefs = $state<RoutineRun[]>([])
  /** Every routine, as `list_routines` gives them; the two briefs are found
   *  among them with `routineOf`. */
  routines = $state<RoutineInfo[]>([])
  loading = $state(false)
  /** Whether the first load has happened. */
  loaded = $state(false)
  /** The run asked for by "Make one now", per brief, until it ends. */
  making = $state<Partial<Record<BriefKind, RoutineRunId>>>({})
  /** Why the last thing asked of a brief did not happen. */
  error = $state<string | null>(null)

  #generation = 0
  #starting: Promise<void> | null = null
  #polls = new Map<BriefKind, ReturnType<typeof setTimeout>>()

  constructor() {
    app.onLock(() => this.reset())
  }

  /** Whether this vault keeps routines at all, which a brief is. */
  get enabled(): boolean {
    return app.supportsRoutines
  }

  /** The newest brief of either kind. */
  get latest(): RoutineRun | null {
    return this.briefs[0] ?? null
  }

  /** The newest brief of one kind. */
  latestOf(kind: BriefKind): RoutineRun | null {
    return this.briefs.find((r) => r.brief?.kind === kind) ?? null
  }

  /** The routine behind one brief, if it was ever switched on. */
  routineOf(kind: BriefKind): RoutineInfo | undefined {
    return briefRoutineOf(this.routines, kind)
  }

  /** Whether a brief is switched on. */
  isOn(kind: BriefKind): boolean {
    return this.routineOf(kind)?.enabled === true
  }

  /** Load once; asking again while loading waits for the first. */
  start(): Promise<void> {
    if (!this.enabled) return Promise.resolve()
    if (this.loaded) return this.refresh()
    this.#starting ??= this.#load().finally(() => (this.#starting = null))
    return this.#starting
  }

  async #load() {
    this.loading = true
    try {
      await this.refresh()
    } finally {
      this.loading = false
    }
  }

  /** Reload the routines and the briefs. What a routine change asks for. */
  async refresh(): Promise<void> {
    if (!this.enabled) return
    const token = ++this.#generation
    try {
      const [routines, briefs] = await Promise.all([api.routines(), api.briefs(null, HISTORY)])
      if (token !== this.#generation) return
      this.routines = routines
      this.briefs = briefs
      this.loaded = true
      // A brief being made that has since appeared, or ended without one.
      for (const kind of Object.keys(this.making) as BriefKind[]) {
        const id = this.making[kind]
        if (id && briefs.some((b) => b.id === id)) this.#stopMaking(kind)
      }
    } catch (e) {
      if (isLocked(e)) return
      await handle(e)
    }
  }

  /** Switch a brief on or off. Hands back its routine, or `null` if it failed. */
  async setOn(kind: BriefKind, on: boolean): Promise<Routine | null> {
    this.error = null
    try {
      const routine = await api.setBrief(kind, on)
      await this.refresh()
      return routine
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
      return null
    }
  }

  /** Save a brief's routine: its time, delivery, sections, length, words. */
  async save(routine: Routine): Promise<Routine | null> {
    this.error = null
    try {
      const saved = await api.saveRoutine(routine)
      await this.refresh()
      return saved
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
      return null
    }
  }

  /**
   * Make a brief now, switching it on first if it never was. Queued, as every
   * "run now" is: the scheduler writes it within a minute or two, and this
   * looks every few seconds until it has.
   */
  async makeNow(kind: BriefKind): Promise<void> {
    // Taken at once, before anything is awaited, so a second press while the
    // first is on its way is not a second brief.
    if (this.isMaking(kind)) return
    this.making = { ...this.making, [kind]: PENDING }
    this.error = null
    try {
      let routine: Pick<Routine, 'id'> | undefined = this.routineOf(kind)
      if (!routine) {
        // Made switched off: asking for one brief is not asking for one every
        // day. Settings → Briefs is where it is switched on.
        routine = await api.setBrief(kind, false)
      }
      const run = await api.runRoutine(routine.id)
      this.making = { ...this.making, [kind]: run.id }
      if (run.outcome === 'done' || run.outcome === 'failed' || run.outcome === 'skipped') {
        await this.#finished(kind, run)
        return
      }
      this.#poll(kind, run.id, Date.now())
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
      this.#stopMaking(kind)
    }
  }

  /** Whether a brief of `kind` is being made. */
  isMaking(kind: BriefKind): boolean {
    return this.making[kind] !== undefined
  }

  #poll(kind: BriefKind, id: RoutineRunId, since: number) {
    const timer = setTimeout(async () => {
      this.#polls.delete(kind)
      if (this.making[kind] !== id) return
      try {
        const run = await api.run(id)
        if (run.outcome === 'done' || run.outcome === 'failed' || run.outcome === 'skipped') {
          await this.#finished(kind, run)
          return
        }
      } catch (e) {
        if (isLocked(e)) return this.#stopMaking(kind)
      }
      if (Date.now() - since > GIVE_UP_MS) {
        this.error =
          'The brief is taking longer than it should. It will appear here when it is done.'
        this.#stopMaking(kind)
        return
      }
      this.#poll(kind, id, since)
    }, POLL_MS)
    this.#polls.set(kind, timer)
  }

  async #finished(kind: BriefKind, run: RoutineRun) {
    this.#stopMaking(kind)
    if (run.outcome !== 'done' || !run.brief) {
      this.error = run.reason?.trim()
        ? `The brief could not be made: ${run.reason.trim()}`
        : 'The brief could not be made.'
    }
    await this.refresh()
  }

  #stopMaking(kind: BriefKind) {
    const timer = this.#polls.get(kind)
    if (timer) clearTimeout(timer)
    this.#polls.delete(kind)
    if (this.making[kind] === undefined) return
    const next = { ...this.making }
    delete next[kind]
    this.making = next
  }

  /** Read a brief aloud in the player in the top bar. */
  listen(run: RoutineRun): Promise<void> {
    const brief = run.brief
    if (!brief) return Promise.resolve()
    return listening.start({ type: 'brief', runId: run.id }, KIND_INFO[brief.kind].name)
  }

  /** Whether the player is reading this run's brief right now. */
  isListening(run: RoutineRun): boolean {
    const source = listening.source
    return listening.active && source?.type === 'brief' && source.runId === run.id
  }

  reset() {
    this.#generation += 1
    for (const timer of this.#polls.values()) clearTimeout(timer)
    this.#polls.clear()
    this.briefs = []
    this.routines = []
    this.making = {}
    this.loaded = false
    this.error = null
  }
}

export const briefs = new BriefState()
