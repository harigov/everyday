// The in-place edit, once: assign the changes, stamp `updatedAt`, queue the
// write.
//
// Three stores keep the record the screen draws from and the record about to
// be written as the same object -- a task in the todo app, a block on the
// calendar, an item on a shelf -- so a card, a detail panel and a board all
// redraw from one mutation instead of a reconciliation pass. Each stamps
// `updatedAt` itself, right here, because that value is what the *next*
// write sends as its conflict base: restamping it anywhere else, or missing
// a store that should, is a save that is silently comparing itself against
// the wrong version.
//
// `touch` is exported apart from `patch` because not every edit arrives as a
// bag of changes -- toggling a favourite or turning a plan into a record
// mutates one field directly and then has to do exactly what `patch` would
// have done after its own `Object.assign`.
import { Autosave } from '../autosave'

export interface Stamped {
  updatedAt: string
}

export interface OptimisticPatch<Id, T extends Stamped> {
  /** Assign `changes` onto the record, stamp it, and queue the write. */
  patch(id: Id, changes: Partial<T>): void
  /** Stamp a record already mutated in place, and queue the write. */
  touch(id: Id): void
}

/**
 * `find` is called again inside `touch`, not closed over from `patch`'s own
 * lookup -- both run synchronously with nothing awaited in between, so it
 * costs nothing and it is what lets `touch` be called on its own by an
 * editor that mutated a field directly.
 */
export function optimisticPatch<Id, T extends Stamped>(
  find: (id: Id) => T | undefined,
  saves: Pick<Autosave<Id>, 'touch'>,
): OptimisticPatch<Id, T> {
  function touch(id: Id): void {
    const record = find(id)
    if (record) record.updatedAt = new Date().toISOString()
    saves.touch(id)
  }
  function patch(id: Id, changes: Partial<T>): void {
    const record = find(id)
    if (!record) return
    Object.assign(record, changes)
    touch(id)
  }
  return { patch, touch }
}
