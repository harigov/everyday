// `live.svelte.ts`'s opt-in, in two halves: deciding whether one change is
// small enough to patch, and fetching-and-placing the one row it names.
//
// Three stores register an `Applier` -- see `live-apply.ts` -- and each
// wrote the same two questions by hand: is this a single record, wide
// enough that patching it is worth doing at all; and once a fetch of that
// one record lands, does it replace a row already on screen or does it need
// placing somewhere new. The placing differs enough to stay a callback --
// the notes list prepends a row that just started matching the tag filter,
// the accounts list appends, and the mail store does neither, asking `live`
// for the whole mailbox again because *where* a thread belongs depends on
// the mailbox, the category and the sort, none of which this file knows.
import { isLocked } from '../errors'
import { type ChangeWithIds, singleId } from '../live-apply'

/**
 * Is `changes` one record's worth of change, and if so, what should happen?
 *
 * Answers `false` for anything wider than a single id -- several ids in one
 * batch, or a kind of change a store's own `skip` does not recognise -- so
 * `live` falls back to that store's ordinary `refresh()`, which is always
 * correct if slower. `skip` is for the one case that is single-record and
 * still not worth doing anything about: mail's own drafts, which nothing in
 * the thread list reads directly.
 */
export function applySingleChange(
  changes: ChangeWithIds[],
  handlers: {
    skip?: (change: ChangeWithIds, id: string) => boolean
    onDeleted: (id: string) => void
    onUpserted: (id: string) => void
  },
): boolean {
  if (changes.length !== 1) return false
  const change = changes[0]!
  const id = singleId(change)
  if (!id) return false
  if (handlers.skip?.(change, id)) return true
  if (change.op === 'deleted') {
    handlers.onDeleted(id)
    return true
  }
  if (change.op === 'created' || change.op === 'updated') {
    handlers.onUpserted(id)
    return true
  }
  return false
}

/**
 * Fetch the one record `applySingleChange` named, and hand it to `apply` --
 * the store's own decision about where it goes. A lock is not a failure --
 * `onLocked` runs and nothing else does, the same as everywhere else a
 * background read meets a vault that just shut. Anything else falls back to
 * `fallback`, which is `() => this.refresh()` at every call site today: the
 * one row could not be resolved, so the whole list is asked for again rather
 * than left possibly wrong.
 */
export async function patchOneFromChange<T>(opts: {
  fetch: () => Promise<T>
  apply: (value: T) => void | Promise<void>
  onLocked?: () => void
  fallback: () => Promise<void>
}): Promise<void> {
  try {
    const value = await opts.fetch()
    await opts.apply(value)
  } catch (e) {
    if (isLocked(e)) {
      opts.onLocked?.()
      return
    }
    await opts.fallback()
  }
}
