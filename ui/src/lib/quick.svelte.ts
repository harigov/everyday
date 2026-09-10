// The quick model, for any component that wants a suggestion.
//
// The interface half of `everyday_core::quick`. Rust builds every prompt,
// declares every schema and clamps every answer; the service opens the
// socket; this file is what components hold, and it exists so that the four
// things every caller of a suggestion needs are written once:
//
//   * is it even switched on, for this job
//   * a failure that is silence rather than an error
//   * cancellation, so a suggestion about the previous note cannot arrive
//     beside this one
//   * a dismissal that sticks for as long as the thing is on screen
//
// # Failure is silence, and that is not laziness
//
// Every call here swallows its error into `null`. The interaction this serves
// is a chip appearing beside a field somebody has already filled in and a
// record already saved -- so a chip that does not appear is the feature
// working slightly less well, and a red banner about a model endpoint is the
// application interrupting somebody mid-sentence about a thing they did not
// ask for. The message goes to the console for whoever is debugging it.
//
// # Nothing here waits on anything
//
// There is no call in this file that a save, an Enter or a navigation is
// allowed to block on. Callers `void` these, or await them after the write
// they belong to. See `ask` below, which cannot be made to return before its
// caller has already done the thing.

import { api } from './api'
import { agent } from './agent.svelte'
import type { QuickJobRow } from './types'
import { VaultError } from './types'

/**
 * Which jobs are switched on, mirrored from the vault.
 *
 * Loaded once when the app unlocks and again after the settings pane touches
 * it. Held here rather than asked per call because `enabled` is read on every
 * keystroke of a capture box, and a round trip to answer "should I even
 * suggest" would cost more than the suggestion.
 */
class QuickState {
  jobs = $state<QuickJobRow[]>([])
  /** Whether a quick model is configured at all. */
  configured = $state(false)

  /** Whether one named job may run right now. */
  enabled(job: string): boolean {
    if (!this.configured) return false
    return this.jobs.find((j) => j.name === job)?.on ?? false
  }

  async load() {
    try {
      this.jobs = await api.quickJobs()
      this.configured = agent.settings?.quickModel != null
    } catch {
      // Same argument as everywhere else in this file: a settings read that
      // failed means no chips, not a banner.
      this.jobs = []
      this.configured = false
    }
  }

  async setJob(name: string, on: boolean) {
    this.jobs = await api.setQuickJob({ name, on })
  }

  reset() {
    this.jobs = []
    this.configured = false
  }
}

export const quick = new QuickState()

/**
 * Run one quick call, or answer `null`.
 *
 * The single door: every suggestion in the application goes through this, so
 * the switch check, the swallow and the log line exist once. `job` is checked
 * against the policy here *as well as* in Rust, and that is not redundancy
 * worth removing — the Rust check is what makes the guarantee, and this one
 * is what stops the request being made at all.
 */
export async function ask<T>(job: string, call: () => Promise<T>): Promise<T | null> {
  if (!quick.enabled(job)) return null
  try {
    return await call()
  } catch (e) {
    // A locked vault is not a failure of this feature and is already handled
    // by whatever is drawing the lock screen.
    if (!(e instanceof VaultError && e.code === 'locked')) {
      console.debug(`quick job ${job} did not answer`, e)
    }
    return null
  }
}

/**
 * A suggestion slot for one component: at most one in flight, and the answer
 * to a stale question is discarded.
 *
 * The generation counter is the whole of it, and it is the same race
 * `web.live` exists for: open a note, open another before the first
 * suggestion lands, and without this the first note's title arrives beside
 * the second note. That is not a flicker — it is a wrong title offered for a
 * record it was never about, one tap from being applied.
 */
export function slot<T>() {
  let generation = 0
  let dismissed = false

  return {
    /** Ask, and call `onResult` only if nothing newer has been asked since. */
    async run(job: string, call: () => Promise<T>, onResult: (value: T | null) => void) {
      const mine = ++generation
      dismissed = false
      const value = await ask(job, call)
      if (mine !== generation || dismissed) return
      onResult(value)
    },
    /**
     * Throw away what is showing and refuse whatever is in flight.
     *
     * Bumping the generation as well as setting the flag is what makes a
     * dismissal stick: without it, a request that had already been sent lands
     * a moment later and the chip somebody just closed comes back.
     */
    dismiss(onResult: (value: T | null) => void) {
      generation++
      dismissed = true
      onResult(null)
    },
    /** Drop anything in flight without clearing what is drawn. */
    cancel() {
      generation++
    },
  }
}
