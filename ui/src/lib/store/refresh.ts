// The guarded reload, once: mint a token, run the loads, land them only if
// nothing newer has started since.
//
// Four stores wrote this out by hand around their own `refresh` -- a
// `#generation.next()` before the round trip, a `this.loading = true`
// bracketing it, a `try`/`catch` that reports through `handle`, and a
// `finally` that only lowers `loading` if this call is still the one anybody
// is waiting on. What differs between them is *what* is loaded and how many
// steps it takes to land it -- library and the todo app fetch several things
// in one `Promise.all` and apply them together; the notes app deliberately
// lands its list before its tag sidebar, so a failed second call does not
// throw away a list that arrived perfectly well. That is why `run` is handed
// the current-token check rather than this file deciding when to call it --
// the store still says exactly when what it has loaded may be applied, and
// how many times.
import type { Generation } from './latest'

/** What a store's own error handling wants to happen. Usually `handle`. */
export type RefreshErrorHandler = (e: unknown) => Promise<void> | void

export interface GuardedRefreshOptions {
  /** Toggled around the load, on before it starts and off once it has
   *  either landed or been superseded. Omitted by a store -- the notes
   *  app -- whose own `start()` owns the flag instead. */
  setLoading?: (loading: boolean) => void
  /** How a failure is reported. Defaults to rethrowing, for a caller that
   *  wants its own `try`/`catch` around this. */
  onError?: RefreshErrorHandler
}

/**
 * Mint a token from `generation`, run `body`, and only lower `loading` if
 * nothing newer has been minted meanwhile.
 *
 * `body` is handed `isCurrent`, the one question every store's `refresh` was
 * already asking by hand -- "has anything newer started since I began?" --
 * so it can check it as many times, at whatever points in its own loading
 * sequence, as it always did.
 */
export async function guardedRefresh(
  generation: Generation,
  body: (isCurrent: () => boolean) => Promise<void>,
  opts: GuardedRefreshOptions = {},
): Promise<void> {
  const token = generation.next()
  const isCurrent = () => generation.isCurrent(token)
  opts.setLoading?.(true)
  try {
    await body(isCurrent)
  } catch (e) {
    if (opts.onError) await opts.onError(e)
    else throw e
  } finally {
    if (isCurrent()) opts.setLoading?.(false)
  }
}
