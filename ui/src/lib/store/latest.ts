// The "only the newest load may land" guard, once.
//
// Four stores kept their own copy of this: a private counter, bumped before
// every request goes out and again by `reset`, and an `if` after each await
// comparing the counter to whatever it captured before the round trip. That
// is the same three lines every time -- mint a token, await, check that
// nothing newer has been minted since -- and it is what stops a scope change
// and a filter keystroke, or two navigations in quick succession, from
// racing into the backend and letting the *older* answer land last.
//
// The calendar's `refresh` and the notes app's had never been given it. A
// page forward on the calendar followed quickly by a page back could let the
// first request's answer -- for a month nobody is looking at any more --
// overwrite the second's, and the grid would sit there showing the wrong
// month until the next navigation happened to fix it. Narrowing a note list
// by tag while an older, broader load was still in flight had the same
// shape. Both are real races, not theoretical ones, and both are fixed by
// giving those two stores what the other four already had.

/** A token minted for one load, and the means to ask whether it is still the newest. */
export interface Generation {
  /** Mint a token for a load that is about to start, retiring whatever came before it. */
  next(): number
  /** Has nothing newer been minted since `token` was handed out? */
  isCurrent(token: number): boolean
}

export function latest(): Generation {
  let current = 0
  return {
    next(): number {
      return ++current
    },
    isCurrent(token: number): boolean {
      return token === current
    },
  }
}
