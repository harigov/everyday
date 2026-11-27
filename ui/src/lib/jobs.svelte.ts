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
import { daysBetween, formatMinutes, friendlyDate, plural, timeOfDay } from './format'
import { fewIds, registerApply, type ChangeWithIds } from './live-apply'
import { app, handle, isLocked } from './state.svelte'
import { isoDate } from './time'
import { VaultError } from './types'
import type { ConversationId, Job, JobId, JobKind, JobUpdate } from './types'

/** How many jobs the list holds. A log, not a database. */
const PAGE = 100

// ── What a card says about a job that keeps going ────────────────────────
//
// Pure, and apart from the store below, so the words a watch's card is made
// of -- "Next look in 3 min", "Until 6:40 PM" -- are checked without drawing
// one. Every function that reads the clock takes it as an argument.

/** How often a watch looks when its rules say nothing. `DEFAULT_EVERY_MINUTES` in the core. */
export const DEFAULT_EVERY_MINUTES = 5

/**
 * Whether a job is still going: queued, running, or asleep between steps. The
 * opposite of finished. A watch spends nearly all its life `waiting`, and
 * everything keyed on "queued or running" has to count it.
 */
export function isLive(job: Pick<Job, 'status'>): boolean {
  return job.status === 'queued' || job.status === 'running' || job.status === 'waiting'
}

/** Whether a job is a watch: one that looks, sleeps and looks again. */
export function isWatch(job: Pick<Job, 'kind'>): boolean {
  return job.kind.type === 'watch'
}

/** A job's kind narrowed to a watch, or `null`. */
export function watchOf(job: Pick<Job, 'kind'>): Extract<JobKind, { type: 'watch' }> | null {
  return job.kind.type === 'watch' ? job.kind : null
}

/**
 * Whether a watch is looking at this moment: its time has come and it has
 * not yet written down what it saw. A fresh watch is due at once.
 */
export function isLookingNow(job: Job, now = Date.now()): boolean {
  if (job.status === 'running') return true
  if (job.status !== 'waiting') return false
  const wake = job.wakeAt ? Date.parse(job.wakeAt) : NaN
  return !Number.isNaN(wake) && wake <= now
}

/** "Next look in 3 min", "Next look in 1h 30m", or "Looking now" once it is due. */
export function nextLookText(wakeAt: string | null | undefined, now = Date.now()): string {
  const wake = wakeAt ? Date.parse(wakeAt) : NaN
  if (Number.isNaN(wake)) return 'Waiting for its next look'
  const ms = wake - now
  if (ms <= 0) return 'Looking now'
  if (ms < 60_000) return 'Next look in under a minute'
  const minutes = Math.round(ms / 60_000)
  return `Next look in ${minutes < 60 ? `${minutes} min` : formatMinutes(minutes)}`
}

/** "5 min", "2 hours": how far apart looks are, for "Looks every 5 min". */
export function everyText(minutes: number | undefined): string {
  const n = minutes && minutes > 0 ? minutes : DEFAULT_EVERY_MINUTES
  if (n < 60) return `${n} min`
  return n % 60 === 0 ? plural(n / 60, 'hour') : formatMinutes(n)
}

/** "Until 6:40 PM" today, "Until tomorrow 6:40 PM", or the date and the time. */
export function untilText(iso: string | null | undefined, now = new Date()): string {
  const at = iso ? new Date(iso) : null
  if (!at || Number.isNaN(at.getTime())) return ''
  const time = timeOfDay(at)
  const ahead = -daysBetween(at, now)
  if (ahead === 0) return `Until ${time}`
  if (ahead === 1) return `Until tomorrow ${time}`
  return `Until ${friendlyDate(isoDate(at), now)}, ${time}`
}

/** What a watch has been told to tell them, as a card says it. */
export function notifyText(notifyWhen: string | undefined): string {
  return notifyWhen?.trim() || 'any change worth knowing'
}

/** Why the last look failed, in a line: "The last 2 looks failed: no answer". */
export function troubleText(failures: number, trouble: string): string {
  const lead = failures === 1 ? 'The last look failed' : `The last ${failures} looks failed`
  return trouble.trim() ? `${lead}: ${trouble.trim()}` : lead
}

/** Where an update went, for the card: "Sent to your chat and the thread". */
export function sentToText(update: Pick<JobUpdate, 'delivered' | 'sentTo'>): string {
  if (!update.delivered) return 'Not sent yet'
  const labels: Record<string, string> = {
    thread: 'the thread',
    chat: 'your chat',
    voice: 'the voice session',
    call: 'a call',
    notification: 'a notification',
  }
  const places = (update.sentTo ?? []).filter((p) => p !== 'held').map((p) => labels[p] ?? p)
  const parts: string[] = []
  if (places.length > 0) {
    const list =
      places.length === 1
        ? places[0]!
        : `${places.slice(0, -1).join(', ')} and ${places[places.length - 1]}`
    parts.push(`Sent to ${list}`)
  }
  if (update.sentTo?.includes('held')) parts.push('To be spoken when quiet hours end')
  return parts.join(' · ')
}

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
  /** The newest whole reload while it is in the air, for `#patch` to wait on. */
  #reloading: Promise<void> | null = null
  /** Whether the list has been read since the vault opened: before that only
   *  the count on the bar is drawn. */
  #loaded = false
  /** The newest read asked for of each job being patched. See `#patch`. */
  #patches = new Map<JobId, number>()
  #patchCount = 0

  constructor() {
    app.onLock(() => this.reset())
    // `live.svelte.ts`'s opt-in. A running job raises a change for every
    // line of progress, each naming that one job, so the usual answer is the
    // one row read again rather than the whole list.
    registerApply('jobs', (changes) => this.#applyChanges(changes))
  }

  /** Whether this vault has background jobs at all. */
  get enabled(): boolean {
    return app.supportsJobs
  }

  /** Jobs that are still going: queued, running, or a watch asleep between looks. */
  get active(): Job[] {
    return this.jobs.filter(isLive)
  }

  /** Watches that have not ended, soonest to look first. */
  get watching(): Job[] {
    return this.jobs
      .filter((j) => isWatch(j) && isLive(j))
      .sort((a, b) => (a.wakeAt ?? '').localeCompare(b.wakeAt ?? ''))
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

  /** Reload the list and the count. What a `job` change wider than a few jobs asks for. */
  refresh(): Promise<void> {
    if (!this.enabled) return Promise.resolve()
    const run = this.#reload()
    this.#reloading = run
    void run.finally(() => {
      if (this.#reloading === run) this.#reloading = null
    })
    return run
  }

  async #reload(): Promise<void> {
    const token = ++this.#generation
    try {
      // Together, so the badge and the list never disagree about a job that
      // finished between two calls.
      const [list, unseen] = await Promise.all([api.jobs({ limit: PAGE }), api.unseenJobs()])
      if (token !== this.#generation) return
      this.jobs = list
      this.unseen = unseen
      this.#loaded = true
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

  /**
   * What a batch of `job` changes does here. Nothing is drawn from the list
   * until it has been read -- a thread with research in it, or "What it did"
   * -- so until then only the count moves. After, the few jobs a batch names
   * are read again one by one; a batch naming more asks for the list.
   */
  #applyChanges(changes: ChangeWithIds[]): boolean {
    if (!this.enabled) return true
    if (!this.#loaded) {
      void this.refreshCount()
      return true
    }
    const named = fewIds(changes)
    if (!named) return false
    for (const [id, op] of named) {
      if (op !== 'deleted') {
        void this.#patch(id)
        continue
      }
      // A read of it still in the air must not put it back.
      this.#patches.delete(id)
      this.jobs = this.jobs.filter((j) => j.id !== id)
    }
    void this.refreshCount()
    return true
  }

  /**
   * One job read again after a change named it -- after any whole reload in
   * the air, whose older copy of it would otherwise land on top. Its answer
   * lands only if it is still the newest word on that job: not if another
   * read of it, or a whole reload, was asked for since, or the vault locked.
   */
  async #patch(id: JobId): Promise<void> {
    await this.#reloading
    const generation = this.#generation
    const read = ++this.#patchCount
    this.#patches.set(id, read)
    const newest = () => this.#patches.get(id) === read && generation === this.#generation
    try {
      const job = await api.job(id)
      if (newest()) this.#put(job)
    } catch (e) {
      if (isLocked(e)) return
      // Deleted from another window before it could be read.
      if (e instanceof VaultError && e.code === 'not_found') {
        if (newest()) this.jobs = this.jobs.filter((j) => j.id !== id)
        return
      }
      await handle(e)
    } finally {
      if (this.#patches.get(id) === read) this.#patches.delete(id)
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

  /** Stop a job that is still going -- a watch stops looking. Whatever it wrote is kept. */
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
    this.#reloading = null
    this.#loaded = false
    this.#patches.clear()
    this.jobs = []
    this.unseen = 0
    this.loading = false
  }
}

export const jobs = new JobsState()
