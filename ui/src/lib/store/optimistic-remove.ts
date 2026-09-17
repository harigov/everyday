// The optimistic delete, once: take it off the screen, ask the backend, put
// it back if the backend refused.
//
// `optimistic` is where each store still differs, and stays a plain
// callback rather than something this file tries to generalise: a task
// takes its subtasks with it and forgets each one from the autosave queue, a
// block clears the selection only if it was the block selected and forgets
// just itself, and a shelf item does not forget itself from the autosave
// queue at all, because nothing was pending on an item mid-delete in the one
// case that was ever measured. What was identical everywhere, byte for
// byte, is what happens after: call the backend, and on failure hand the
// error to `handle` along with how to put the screen back -- which is
// `() => this.refresh()` at every call site today, rather than trying to
// reconstruct exactly what was removed.
import { handle } from '../errors'

export async function optimisticRemove(opts: {
  /** Drop the row (or rows) from whatever local state shows them, before
   *  the backend has been asked. Run synchronously, first. */
  optimistic: () => void
  call: () => Promise<unknown>
  onSuccess?: () => void
  rollback: () => unknown
}): Promise<void> {
  opts.optimistic()
  try {
    await opts.call()
    opts.onSuccess?.()
  } catch (e) {
    await handle(e, async () => {
      await opts.rollback()
    })
  }
}
