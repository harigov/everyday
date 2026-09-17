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
import { app, handle, isLocked } from './state.svelte'
import type {
  DeclineReason,
  Memory,
  Note,
  Proposal,
  ProposalId,
  ProposalKind,
  ProposalQuery,
  ProposedRecord,
  Routine,
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
}

type Listener = (p: Proposal) => void | Promise<void>

/** The reasons offered in the decline menu, in order. */
export const DECLINE_REASONS: { reason: DeclineReason; label: string }[] = [
  { reason: { type: 'notNow' }, label: 'Not now' },
  { reason: { type: 'wrongTime' }, label: 'Wrong time' },
  { reason: { type: 'neverThis' }, label: 'Never suggest this' },
]

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

  constructor() {
    app.onLock(() => this.reset())
  }

  reset() {
    this.pending = []
    this.unseen = 0
    this.loaded = false
    this.busy = []
  }

  get enabled(): boolean {
    return app.supportsProposals && app.supportsAssistant
  }

  /** Load the pending list and the count. Idempotent. */
  async refresh() {
    if (!this.enabled) return
    try {
      this.pending = await api.proposals({ states: ['pending'] })
      this.unseen = await api.unseenProposals()
      this.loaded = true
    } catch (e) {
      if (isLocked(e)) return
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
      await this.refresh()
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
