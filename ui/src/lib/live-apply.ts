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

const appliers = new Map<ReloadTarget, Map<string, Applier>>()

/**
 * Opt `target`'s store into patched updates. See `Applier`.
 *
 * `owner` says whose applier it is, and a second registration under the same
 * owner replaces the first. More than one store may listen on one target --
 * the status word's `settings` changes are watched by Backup and by
 * Connections, each for its own open pane -- and each names itself, so
 * neither needs to know the other is there. It defaults to the target, for
 * the usual one store per target.
 */
export function registerApply(target: ReloadTarget, apply: Applier, owner: string = target): void {
  let byOwner = appliers.get(target)
  if (!byOwner) {
    byOwner = new Map()
    appliers.set(target, byOwner)
  }
  byOwner.set(owner, apply)
}

/**
 * What `live` runs for `target`, if any store has opted in: every applier
 * registered there, each shown the batch, answering `true` only if all of
 * them applied it. One that declines still means the ordinary refresh.
 */
export function applierFor(target: ReloadTarget): Applier | undefined {
  const list = [...(appliers.get(target)?.values() ?? [])]
  if (list.length === 0) return undefined
  if (list.length === 1) return list[0]
  return (changes) => list.map((apply) => apply(changes)).every(Boolean)
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

/**
 * The records a batch names, each with what last happened to it, for a store
 * that re-reads a few rows rather than one: a running job announces itself
 * more than once inside one coalescing window, and two jobs can move in the
 * same one.
 *
 * `null` when any change is wider than one record (or names none), or when
 * more than `max` are named -- a whole reload is the cheaper answer then.
 */
export function fewIds(changes: ChangeWithIds[], max = 3): Map<string, ChangeWithIds['op']> | null {
  const named = new Map<string, ChangeWithIds['op']>()
  for (const change of changes) {
    const id = singleId(change)
    if (id === null) return null
    named.set(id, change.op)
  }
  return named.size > 0 && named.size <= max ? named : null
}
