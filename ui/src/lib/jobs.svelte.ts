// The assistant's background jobs: research it started in a conversation,
// where each one stands, and what it made. See `docs/plans/jobs.md`.
//
// Drawn in two places from this one list: as a card in the thread that
// started a job (under the `start_research` call), and in Settings → "What
// it did" beside the routine runs. Unseen jobs add to the count on the app
// bar, and looking at "What it did" is what clears them -- the rule
// `assistant.svelte.ts` keeps for runs, and which calls `markSeen` below.
//
// One list rather than one per place, because a job moves while somebody
// watches it: queued, running with a new progress line every few seconds,
// done. Both views read this and a `job` change event reloads it -- see the
// `jobs` target in `live.svelte.ts` -- so the card in the thread and the row
// in Settings cannot disagree about where a job stands.

import { api } from './api'
import { app, handle, isLocked } from './state.svelte'
import { VaultError } from './types'
import type { ConversationId, Job, JobId } from './types'

/** How many jobs the list holds. A log, not a database. */
const PAGE = 100

class JobsState {
  /** The most recent jobs, newest first. */
  jobs = $state<Job[]>([])
  /** Jobs nobody has looked at. Added to the app bar's count. */
  unseen = $state(0)
  loading = $state(false)

  /**
   * Which reload is the newest.
   *
   * A job writes often while it runs, and every write is a reload, so two
   * are routinely in the air at once. The older one answering last would put
   * back a progress line the newer one had already moved past -- a card that
   * ticks backwards -- so only the newest may land. `reset` bumps it as well,
   * so a load that was in the air when the vault locked does not fill the
   * list again behind the lock screen.
   */
  #generation = 0

  /** The load in flight, so `start` asked twice waits for it. */
  #starting: Promise<void> | null = null

  constructor() {
    app.onLock(() => this.reset())
  }

  /** Whether this vault has background jobs at all. */
  get enabled(): boolean {
    return app.supportsJobs
  }

  /** Jobs that are queued or running. */
  get active(): Job[] {
    return this.jobs.filter((j) => j.status === 'queued' || j.status === 'running')
  }

  /** One job by id, from what is loaded. */
  byId(id: JobId): Job | undefined {
    return this.jobs.find((j) => j.id === id)
  }

  /** The jobs a conversation started, newest first. */
  forConversation(conversation: ConversationId): Job[] {
    return this.jobs.filter((j) => j.askedIn === conversation)
  }

  /** Load the list and the count. Asking twice while loading waits for the first. */
  start(): Promise<void> {
    if (!this.enabled) return Promise.resolve()
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

  /** Reload the list and the count. What a `job` change event asks for. */
  async refresh(): Promise<void> {
    if (!this.enabled) return
    const token = ++this.#generation
    try {
      // Together, so the badge and the list never disagree about a job that
      // finished between two calls.
      const [list, unseen] = await Promise.all([api.jobs({ limit: PAGE }), api.unseenJobs()])
      if (token !== this.#generation) return
      this.jobs = list
      this.unseen = unseen
    } catch (e) {
      if (isLocked(e)) return
      await handle(e)
    }
  }

  /**
   * Only the count, for the app bar.
   *
   * Its own call because the bar is drawn in every app and must not pay for
   * a list it is not showing.
   */
  async refreshCount(): Promise<void> {
    if (!this.enabled) return
    // Lands only if no reload started meanwhile: that one reads the count
    // too, later, and this older answer must not overwrite it.
    const token = this.#generation
    try {
      const unseen = await api.unseenJobs()
      if (token === this.#generation) this.unseen = unseen
    } catch {
      /* a number that could not be read is not worth reporting */
    }
  }

  /** Fetch one job fresh and put it in the list, whether or not it was there. */
  async load(id: JobId): Promise<Job | null> {
    if (!this.enabled) return null
    try {
      const job = await api.job(id)
      this.#put(job)
      return job
    } catch (e) {
      if (isLocked(e)) return null
      // A job somebody deleted from another window while a card still held
      // its id: not an error worth a banner, and nothing is left to show.
      if (e instanceof VaultError && e.code === 'not_found') {
        this.jobs = this.jobs.filter((j) => j.id !== id)
        return null
      }
      await handle(e)
      return null
    }
  }

  /** Stop a queued or running job. Whatever it wrote is kept. */
  async cancel(id: JobId): Promise<void> {
    try {
      this.#put(await api.cancelJob(id))
    } catch (e) {
      await handle(e)
      // It may have finished while the button was being pressed, in which
      // case the card is out of date, not the press wrong.
      await this.load(id)
    }
  }

  /** Delete a finished job and its transcript. Its note stays. */
  async remove(id: JobId): Promise<void> {
    try {
      await api.deleteJob(id)
      this.jobs = this.jobs.filter((j) => j.id !== id)
      // It may have been one of the unseen.
      await this.refreshCount()
    } catch (e) {
      await handle(e)
    }
  }

  /**
   * Mark jobs looked at. No ids: every unseen one that is loaded.
   *
   * Never an empty list to the service, where that means *all* of them --
   * including a job that finished while this list was being read and that
   * nobody has seen. Nothing to mark is no call at all.
   */
  async markSeen(ids?: JobId[]): Promise<void> {
    const wanted = ids && ids.length > 0 ? ids : this.jobs.filter((j) => !j.seen).map((j) => j.id)
    if (wanted.length === 0) return
    try {
      await api.markJobsSeen(wanted)
      const marked = new Set(wanted)
      for (const job of this.jobs) {
        if (marked.has(job.id)) job.seen = true
      }
      await this.refreshCount()
    } catch (e) {
      await handle(e)
    }
  }

  /** Have the follow-up answer again, for a job whose answer failed. */
  async answer(id: JobId): Promise<void> {
    try {
      await api.answerJob(id)
      // `answered` is what the card's button hangs on.
      await this.load(id)
    } catch (e) {
      await handle(e)
    }
  }

  /** Start research by hand, from the interface rather than the assistant. */
  async startResearch(question: string, conversationId?: ConversationId): Promise<Job | null> {
    const text = question.trim()
    if (!text || !this.enabled) return null
    try {
      const job = await api.startResearch(text, undefined, conversationId)
      this.#put(job)
      void this.refreshCount()
      return job
    } catch (e) {
      await handle(e)
      return null
    }
  }

  /**
   * Put one job in the list: in place if it is there, else where its age
   * says. Newest first is the order the service answers in, and a job
   * landing at the top regardless would jump below a newer one at the next
   * reload.
   */
  #put(job: Job) {
    const at = this.jobs.findIndex((j) => j.id === job.id)
    if (at >= 0) {
      this.jobs[at] = job
      return
    }
    const next = this.jobs.findIndex((j) => j.createdAt < job.createdAt)
    if (next < 0) this.jobs.push(job)
    else this.jobs.splice(next, 0, job)
  }

  reset() {
    this.#generation++
    this.#starting = null
    this.jobs = []
    this.unseen = 0
    this.loading = false
  }
}

export const jobs = new JobsState()
