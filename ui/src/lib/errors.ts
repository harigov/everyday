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
// `app` is reached through a dynamic `import()` inside `handle` and
// `quietly` rather than a static one at the top, and that is not decoration.
// `autosave.ts` is loaded by every test in this suite in isolation -- no
// window, no vault, nothing but `types.ts` beside it -- specifically so a
// keystroke-timer bug does not need a browser to catch. A static import of
// `state.svelte.ts` here would drag `api.ts` in behind it, which touches
// `window` at module scope, and autosave's own test would crash before its
// first assertion for a codepath it never runs. A dynamic import costs
// nothing once the app is actually running -- `state.svelte.ts` is loaded
// long before anything fails -- and costs nothing in the tests either,
// because `isLocked`, `isConflict` and `errorMessage` never touch `app` and
// so never trigger it.
import { VaultError } from './types'

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
  const { app } = await import('./state.svelte')
  if (isLocked(e)) {
    await app.lock()
    return
  }
  app.error = errorMessage(e)
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
  const { app } = await import('./state.svelte')
  await app.lock()
}
