// The opt-in half of `live.svelte.ts`: a store that would rather patch its
// own list from a batch of changes than ask the backend for all of it again.
//
// Its own file so a store can register one without importing `live.svelte.ts`
// itself. That file already imports every store to build `RELOAD`, so a store
// importing back from it to call `registerApply` would be two files each
// needing the other to have finished loading before either can run -- the
// classic ESM cycle, and one that would only show up once something actually
// exercised the constructor that calls `registerApply`.

import type { ChangeEvent } from './types'
import type { ReloadTarget } from './live.svelte'

/** A change as the backend sends it, `id` for one record and `ids` for a batch. */
export type ChangeWithIds = ChangeEvent

/**
 * What a store hands `registerApply`: given the changes routed to its
 * target, either apply them to what is already loaded and answer `true`, or
 * answer `false` to ask `live` for the ordinary whole-store `refresh()`
 * instead.
 *
 * Synchronous on purpose. A store that needs to fetch something to apply a
 * change -- the common case, a single row re-read after an update -- answers
 * `true` immediately and does the fetch in the background; making `live`
 * await it would only add a round trip between a change landing and the rest
 * of the batch's targets being handled, for a list that is going to redraw
 * again once the fetch lands regardless.
 */
export type Applier = (changes: ChangeWithIds[]) => boolean

const appliers = new Map<ReloadTarget, Applier>()

/** Opt `target`'s store into patched updates. See `Applier`. */
export function registerApply(target: ReloadTarget, apply: Applier): void {
  appliers.set(target, apply)
}

/** The applier registered for `target`, if any store has opted in. */
export function applierFor(target: ReloadTarget): Applier | undefined {
  return appliers.get(target)
}

/**
 * The one id a change carries, for a store that only knows how to patch a
 * single row.
 *
 * `null` for anything wider than that -- `ids` with more than one entry --
 * which is exactly the shape such a store should refuse rather than guess
 * at: patching one of several changed rows and leaving the rest stale would
 * be worse than the refresh it was trying to avoid.
 */
export function singleId(change: ChangeWithIds): string | null {
  const ids = change.ids
  if (ids && ids.length > 1) return null
  if (ids && ids.length === 1) return ids[0]!
  return change.id ?? null
}
