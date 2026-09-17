// Small, pure pieces of the dreaming feature that more than one component
// needs, kept out of the stores and the views so they can be checked without
// either. See docs/plans/dreaming.md.

import type { DreamScope, Memory, ProposalKind, Trigger } from './types'

// ── The dream routines ────────────────────────────────────────────────────

/** What a dream's scope is called on the routine list and its own card. */
export const DREAM_SCOPE_LABELS: Record<DreamScope, string> = {
  day: 'Every night',
  week: 'Every week',
  month: 'Every month',
}

/** `1` → `'1st'`, `2` → `'2nd'`, `3` → `'3rd'`, `11`–`13` → `'…th'`. */
export function ordinal(n: number): string {
  const mod100 = n % 100
  if (mod100 >= 11 && mod100 <= 13) return `${n}th`
  switch (n % 10) {
    case 1:
      return `${n}st`
    case 2:
      return `${n}nd`
    case 3:
      return `${n}rd`
    default:
      return `${n}th`
  }
}

/**
 * A schedule trigger's words, for the one shape a plain `when` string from
 * the service cannot yet say: a day of the month rather than a day of the
 * week. `null` for every other trigger, so a caller falls back to `when`.
 */
export function monthlySchedule(t: Trigger): string | null {
  if (t.type !== 'schedule' || t.dayOfMonth == null) return null
  return `Monthly on the ${ordinal(t.dayOfMonth)} at ${t.at}`
}

// ── The memory list's three groups ───────────────────────────────────────

export interface MemoryGroups {
  /** Told outright, or an inference the person has stood behind. */
  told: Memory[]
  /** Inferred by a dream and not yet answered. May be wrong. */
  noticed: Memory[]
  /** Struck out, and kept so a later dream cannot learn them again. */
  unassumed: Memory[]
}

/**
 * Split a memory list into the three groups the memory pane draws.
 *
 * `origin` is absent on anything sealed before provenance existed, and reads
 * as `'told'` -- see `Memory.origin` in `types.ts`.
 */
export function groupMemories(memories: Memory[]): MemoryGroups {
  const told: Memory[] = []
  const noticed: Memory[] = []
  const unassumed: Memory[] = []
  for (const m of memories) {
    switch (m.origin) {
      case 'inferred':
        noticed.push(m)
        break
      case 'rejected':
        unassumed.push(m)
        break
      default:
        told.push(m)
    }
  }
  return { told, noticed, unassumed }
}

// ── What each kind of proposal is called ─────────────────────────────────

/** The label beside each kind's switch in Settings, and each heading in the
 *  "Waiting for you" pane. Order follows `PROPOSAL_KINDS`. */
export const PROPOSAL_KIND_LABELS: Record<ProposalKind, string> = {
  task: 'Tasks',
  block: 'Time on the calendar',
  memory: 'Memories',
  routine: 'Routines',
  note: 'Notes',
  mail: 'Mail to send',
}

// ── The transcript fold ───────────────────────────────────────────────────

/** The line a dream's opening message carries ahead of its digest. */
export const DIGEST_MARKER = '--- digest ---'

export interface SplitDigest {
  /** What the message says before the marker -- what is drawn as its text. */
  text: string
  /** The Markdown after the marker, or `null` when there is no such line. */
  digest: string | null
}

/**
 * Split a dream run's opening message into its words and its digest.
 *
 * The marker is a whole line, matched exactly rather than sniffed for, so a
 * person's own message that happens to mention "digest" is never folded by
 * mistake -- only the one the core writes ahead of a dream's first user
 * message is shaped this way.
 */
export function splitDigest(content: string): SplitDigest {
  const lines = content.split('\n')
  const at = lines.indexOf(DIGEST_MARKER)
  if (at < 0) return { text: content, digest: null }
  return {
    text: lines.slice(0, at).join('\n').trimEnd(),
    digest: lines
      .slice(at + 1)
      .join('\n')
      .trim(),
  }
}
