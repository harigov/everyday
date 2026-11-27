// Work the assistant prepared and did not do.
//
// One list of pending proposals for the whole interface. Each app draws the
// ones that belong to it where the real record would be -- a task at the foot
// of its list, a block on the grid, a memory under the others -- with the same
// ghost style (`ProposalGhost.svelte`) and the same two answers. See
// docs/plans/dreaming.md.
//
// Two rules worth knowing.
//
// A proposal carries a *whole record*. `recordOf` hands it back typed, so an
// app can draw a proposed task with the task row's own formatting and open it
// in the task's own editor. Saving from that editor is `accept(p, edited)`.
//
// Accepting writes a record into some other app's store. This window's own
// writes do not come back to it as change events, so `accept` tells whoever
// registered with `onAccepted` for that kind, and the store reloads itself.

import { api } from './api'
import type { MenuItem } from './menu'
import { app, handle, isLocked } from './state.svelte'
import { latest } from './store/latest'
import type {
  DeclineReason,
  Memory,
  Note,
  Proposal,
  ProposalId,
  ProposalKind,
  ProposalQuery,
  ProposedLog,
  ProposedRecord,
  Routine,
  Skill,
  Task,
  TimeBlock,
} from './types'

/** The record a proposal carries, by kind. */
export interface RecordByKind {
  task: Task
  block: TimeBlock
  memory: Memory
  routine: Routine
  note: Note
  skill: Skill
  log: ProposedLog
}

type Listener = (p: Proposal) => void | Promise<void>

/** The reasons offered in the decline menu, in order. */
export const DECLINE_REASONS: { reason: DeclineReason; label: string }[] = [
  { reason: { type: 'notNow' }, label: 'Not now' },
  { reason: { type: 'wrongTime' }, label: 'Wrong time' },
  { reason: { type: 'neverThis' }, label: 'Never suggest this' },
]

/**
 * The decline menu: a plain decline, then each reason.
 *
 * Raised through the window's one menu (`menu.svelte.ts`) rather than drawn
 * inside whatever holds the Decline button. A dropdown drawn in place took
 * on every container it sat in: a row's opacity, a time-grid block's
 * clipping, a later row drawn over it, a block's drag starting under it.
 */
export function declineItems(answer: (reason: DeclineReason | null) => unknown): MenuItem[] {
  return [
    { label: 'Just decline', run: () => answer(null) },
    ...DECLINE_REASONS.map((r) => ({ label: r.label, run: () => answer(r.reason) })),
  ]
}

/** The record a proposal would save, if it saves one. */
export function recordOf(p: Proposal): ProposedRecord | null {
  return p.payload.type === 'create' || p.payload.type === 'replace' ? p.payload.record : null
}

/** The record a proposal would save, if it is of kind `kind`. */
export function recordAs<K extends keyof RecordByKind>(
  p: Proposal,
  kind: K,
): RecordByKind[K] | null {
  const r = recordOf(p)
  return r && r.kind === kind ? (r.value as RecordByKind[K]) : null
}

/** Wrap an edited record back up for `accept`. */
export function edited<K extends keyof RecordByKind>(
  kind: K,
  value: RecordByKind[K],
): ProposedRecord {
  return { kind, value } as ProposedRecord
}

class ProposalsState {
  /** Everything waiting for an answer, newest first. */
  pending = $state<Proposal[]>([])
  /** Pending proposals nobody has looked at. Half the app-bar number. */
  unseen = $state(0)
  loaded = $state(false)
  /** Ids with an answer on its way, so a row can disable its buttons. */
  busy = $state<ProposalId[]>([])

  #listeners = new Map<ProposalKind, Set<Listener>>()
  /** Which load is the newest, so an older answer (or one after a lock) never lands last. */
  #generation = latest()
  /** The load under way, which a caller that only needs the list loaded shares. */
  #inFlight: Promise<void> | null = null

  constructor() {
    app.onLock(() => this.reset())
  }

  reset() {
    this.#generation.next()
    this.#inFlight = null
    this.pending = []
    this.unseen = 0
    this.loaded = false
    this.busy = []
  }

  get enabled(): boolean {
    return app.supportsProposals && app.supportsAssistant
  }

  /**
   * Load the pending list and the count. Idempotent.
   *
   * Every app's `start` asks for this on the way in, often several at once
   * after an unlock, so a load already under way is shared rather than
   * repeated. `force` starts a fresh one regardless: what a change event or
   * a refused answer wants, since the load in the air may predate the write.
   */
  refresh(force = false): Promise<void> {
    if (!this.enabled) return Promise.resolve()
    if (this.#inFlight && !force) return this.#inFlight
    const run = this.#load()
    this.#inFlight = run
    void run.finally(() => {
      if (this.#inFlight === run) this.#inFlight = null
    })
    return run
  }

  async #load(): Promise<void> {
    const token = this.#generation.next()
    try {
      // Together: two small reads, and neither needs the other.
      const [pending, unseen] = await Promise.all([
        api.proposals({ states: ['pending'] }),
        api.unseenProposals(),
      ])
      // Superseded: whoever is waiting on this waits for the newer load instead.
      if (!this.#generation.isCurrent(token)) {
        await this.#inFlight
        return
      }
      this.pending = pending
      this.unseen = unseen
      this.loaded = true
    } catch (e) {
      if (!this.#generation.isCurrent(token) || isLocked(e)) return
      await handle(e)
    }
  }

  /** Only the count, for the app bar. */
  async refreshCount() {
    if (!this.enabled) return
    try {
      this.unseen = await api.unseenProposals()
    } catch {
      /* a number that could not be read is not worth reporting */
    }
  }

  /** Pending proposals of one kind. */
  forKind(kind: ProposalKind): Proposal[] {
    return this.pending.filter((p) => p.kind === kind)
  }

  /** Pending proposals drawn on one day (`YYYY-MM-DD`), optionally of one kind. */
  forDate(day: string, kind?: ProposalKind): Proposal[] {
    return this.pending.filter((p) => p.targetDate === day && (!kind || p.kind === kind))
  }

  /** Pending proposals drawn between two days, inclusive. */
  inRange(from: string, to: string, kind?: ProposalKind): Proposal[] {
    return this.pending.filter(
      (p) =>
        !!p.targetDate && p.targetDate >= from && p.targetDate <= to && (!kind || p.kind === kind),
    )
  }

  /** Answered and pending proposals matching a query -- for history views. */
  async query(query: ProposalQuery): Promise<Proposal[]> {
    try {
      return await api.proposals(query)
    } catch (e) {
      if (!isLocked(e)) await handle(e)
      return []
    }
  }

  isBusy(id: ProposalId): boolean {
    return this.busy.includes(id)
  }

  /**
   * Be told when a proposal of `kind` is accepted, to reload the store its
   * record went into. Returns the unsubscribe.
   */
  onAccepted(kind: ProposalKind, listener: Listener): () => void {
    let set = this.#listeners.get(kind)
    if (!set) {
      set = new Set()
      this.#listeners.set(kind, set)
    }
    set.add(listener)
    return () => set.delete(listener)
  }

  /** Say yes. Returns the closed proposal, or `null` if it failed. */
  async accept(
    p: Proposal,
    editedRecord: ProposedRecord | null = null,
    confirm = false,
  ): Promise<Proposal | null> {
    return this.#answer(p, async () => {
      const closed = await api.acceptProposal(p.id, editedRecord, confirm)
      for (const listener of this.#listeners.get(p.kind) ?? []) await listener(closed)
      return closed
    })
  }

  /** Say no, with an optional reason. */
  async decline(p: Proposal, reason: DeclineReason | null = null): Promise<Proposal | null> {
    return this.#answer(p, () => api.declineProposal(p.id, reason))
  }

  async #answer(p: Proposal, run: () => Promise<Proposal>): Promise<Proposal | null> {
    if (this.isBusy(p.id)) return null
    this.busy = [...this.busy, p.id]
    try {
      const closed = await run()
      this.pending = this.pending.filter((x) => x.id !== p.id)
      if (!p.seen) this.unseen = Math.max(0, this.unseen - 1)
      return closed
    } catch (e) {
      await handle(e)
      // It may have been answered elsewhere, or no longer apply.
      await this.refresh(true)
      return null
    } finally {
      this.busy = this.busy.filter((id) => id !== p.id)
    }
  }

  /**
   * Mark what is on screen as looked at. Reading clears the count, the same
   * rule the run log follows.
   */
  async markSeen(shown: Proposal[]) {
    const ids = shown.filter((p) => !p.seen).map((p) => p.id)
    if (ids.length === 0) return
    try {
      await api.markProposalsSeen(ids)
      for (const p of this.pending) if (ids.includes(p.id)) p.seen = true
      this.unseen = await api.unseenProposals()
    } catch (e) {
      if (!isLocked(e)) await handle(e)
    }
  }
}

export const proposals = new ProposalsState()
