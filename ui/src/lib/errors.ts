// What every store does when a call into the vault fails, in one place.
//
// There is one policy and it is this: a vault that locked under us is not an
// error to report, it is a screen to go to -- the auto-lock can fire in the
// middle of any call, and telling somebody "locked" in red at the top of the
// window they are about to be taken away from is noise. Anything else is
// worth saying.
//
// This lived in `state.svelte.ts`, beside `app`, because that is where it
// was first written and every store already imported that file for `app`
// and `handle` together. It moved out once `handle` itself stopped being the
// only shape a store needed: `quietly` below is the same policy for a
// background read that is not worth a banner over the window at all -- a
// sync poll, a count refreshed after a write -- and by the time that pattern
// had been copied out by hand at some fifteen call sites, with `autosave.ts`
// keeping a sixteenth copy of `isLocked` alone to avoid importing this file,
// it had earned a file of its own rather than more of `state.svelte.ts`.
// `state.svelte.ts` re-exports everything here, so nothing that already
// imports `isLocked` or `handle` from it has to change.
//
// This file does not import `state.svelte.ts`, in either direction: the two
// things `handle` needs from the session -- send us to the lock screen, put
// this message over the window -- are handed to it by `setPolicy`, which
// `state.svelte.ts` calls beside the line that constructs `app`. The reason
// is `autosave.ts`, which every test in this suite loads in isolation with
// no window, no vault and nothing but `types.ts` beside it, specifically so
// that a keystroke-timer bug does not need a browser to catch. Importing
// the session here would drag `api.ts` in behind it, which touches `window`
// at module scope, and that test would crash before its first assertion for
// a codepath it never runs. Registration keeps the policy in one place
// without making the file that states it depend on the file that applies
// it, and `isLocked`, `isConflict` and `errorMessage` stay usable by anyone
// with no session at all.
import { VaultError } from './types'

/**
 * The two things this policy needs from the open session.
 *
 * Registered rather than imported; see the note at the top of this file.
 * Until `state.svelte.ts` is loaded there is no session to send anybody to,
 * and both handlers below simply do nothing -- which is the right answer for
 * a test that loaded one store and never opened a vault.
 */
export type ErrorPolicy = {
  lock: () => Promise<void>
  report: (message: string) => void
}

let session: ErrorPolicy | null = null

/** Tell this file how to reach the open session. Called once, from `state.svelte.ts`. */
export function setPolicy(policy: ErrorPolicy): void {
  session = policy
}

/**
 * Was this the vault locking under us rather than a fault?
 *
 * Nearly every caller wants `handle` below instead. This is exported for
 * the handful that deliberately do something else with the distinction --
 * a background refresh that swallows everything, or a dialog that returns
 * its message instead of posting it over the window.
 */
export function isLocked(e: unknown): boolean {
  return e instanceof VaultError && e.code === 'locked'
}

/**
 * Was this save refused because the entry changed elsewhere?
 *
 * Distinct from a failure: nothing is wrong, two people (or two processes)
 * simply wrote the same entry, and the interface has to ask rather than
 * pick a winner.
 */
export function isConflict(e: unknown): boolean {
  return e instanceof VaultError && e.code === 'conflict'
}

export function errorMessage(e: unknown): string {
  if (e instanceof VaultError) return e.message
  if (e instanceof Error) return e.message
  return String(e)
}

/**
 * What every store does when a call into the vault fails.
 *
 * It lives here, beside `isLocked`, and not as a copy in each store. This
 * idiom was written out twenty-six times across the three original stores,
 * with three separate definitions of `isLocked` and several methods that had
 * simply forgotten to check -- so a lock during those produced an unhandled
 * rejection instead of the lock screen. The next store to be added gets the
 * behaviour by calling this rather than by remembering to copy it.
 *
 * `revert` re-reads whatever the caller had already changed optimistically,
 * for the writes that update the screen before the disk.
 */
export async function handle(e: unknown, revert?: () => Promise<unknown>): Promise<void> {
  if (isLocked(e)) {
    await session?.lock()
    return
  }
  session?.report(errorMessage(e))
  if (revert) await revert()
}

/**
 * The quiet half of `handle`: a lock still goes to the lock screen, but
 * anything else is swallowed rather than reported.
 *
 * For the reads that are not worth a banner over the window -- a calendar
 * feed's poll, a stat refreshed after a write -- where the caller has
 * nothing sensible to do with a failure beyond leaving the screen as it
 * was, except when the failure means the screen is about to change out from
 * under it anyway. This is `if (isLocked(e)) await app.lock()` with nothing
 * else in the `catch`, written out by hand at enough call sites to be worth
 * naming.
 */
export async function quietly(e: unknown): Promise<void> {
  if (!isLocked(e)) return
  await session?.lock()
}
