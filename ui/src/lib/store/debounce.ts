// One keystroke timer, not four.
//
// The journal's search box, the notes app's, the library's and the todo
// app's filter each kept their own `setTimeout` handle and their own line
// for "clear whatever was already waiting before arming a new one" -- the
// same few statements, four times over, each at its own delay and each
// carrying its own chance of forgetting the `clearTimeout`. Forgetting it is
// the footgun that shows up as a search box: clear the box without also
// killing the armed timer, and a query typed a moment ago lands afterwards
// and refills a list that was just emptied on purpose.
//
// `debounce` is those statements once. The delay stays the caller's --
// filtering the todo list decrypts every task in scope to match against its
// title, which has earned a longer pause than the journal's index lookup --
// and `cancel` is the other half every one of the four needed too, for a
// clear button and for a lock that must not let a stale query fire into a
// vault that just shut.

export interface Debounced<Args extends unknown[]> {
  /** Arm the timer, replacing whatever call was already waiting. */
  call(...args: Args): void
  /** Drop a waiting call without running it. */
  cancel(): void
}

export function debounce<Args extends unknown[]>(
  fn: (...args: Args) => void,
  ms: number,
): Debounced<Args> {
  let timer: ReturnType<typeof setTimeout> | null = null
  return {
    call(...args: Args) {
      if (timer) clearTimeout(timer)
      timer = setTimeout(() => {
        timer = null
        fn(...args)
      }, ms)
    },
    cancel() {
      if (timer) clearTimeout(timer)
      timer = null
    },
  }
}
