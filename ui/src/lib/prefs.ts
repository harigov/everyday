// Local, unencrypted settings: chrome the interface remembers about itself.
//
// A dozen keys, and two ways of spelling them. Nine follow `everyday.<name>`,
// one word per piece of chrome; the assistant rail's two came later, from a
// different corner of the interface, as `everyday:<name>` -- a colon instead
// of a dot. That inconsistency is recorded here rather than fixed: somebody's
// browser already holds a value under one of these exact strings, and
// renaming the key would not migrate it, it would lose it. A vault's worth of
// remembered widths and view choices is not worth a tidier spelling.
//
// What this file does fix is the six or so lines every site repeated around
// that string: read it, decide whether what came back still means anything,
// and fall back to a sensible default when it does not -- because a version
// of the app that offered `'grid' | 'list'` and now offers a third view must
// not crash on a browser that still has the old one written down.

/** Every key this file reads or writes, so a typo in one is a compile error. */
export type PrefKey =
  | 'everyday.theme'
  | 'everyday.section'
  | 'everyday.todo.view'
  | 'everyday.calendar.view'
  | 'everyday.library.view'
  | 'everyday.library.capture'
  | 'everyday.library.enrich'
  | 'everyday.journal.calendar'
  | 'everyday.overview.layout'
  | 'everyday.tray'
  | 'everyday:assistant-open'
  | 'everyday:assistant-width'

export interface Pref<T> {
  /** The stored value, parsed and validated -- `fallback` for anything else. */
  get(): T
  /** Write `value` back, or forget the key entirely when it is `null`. */
  set(value: T): void
}

/**
 * One preference: a key, how to make sense of whatever is sitting under it,
 * and what to use when there is nothing there or it does not parse.
 *
 * `serialize` defaults to `String`, which is right for the plain strings and
 * numbers most of these keys hold. The handful that write `'on'`/`'off'` or
 * `'1'`/`'0'` instead of the bare word pass their own -- what is on disk is a
 * promise to whatever already reads it, not something this helper gets to
 * renegotiate on the way in.
 */
export function pref<T>(
  key: PrefKey,
  parse: (raw: string | null) => T,
  fallback: T,
  serialize: (value: T) => string = String,
): Pref<T> {
  return {
    get(): T {
      try {
        return parse(localStorage.getItem(key))
      } catch {
        return fallback
      }
    },
    set(value: T): void {
      if (value === null || value === undefined) localStorage.removeItem(key)
      else localStorage.setItem(key, serialize(value))
    },
  }
}

/** A preference stored as the literal words `'on'` and `'off'`, absent meaning on. */
export function onOffPref(key: PrefKey, fallback = true): Pref<boolean> {
  return pref<boolean>(
    key,
    (raw) => raw !== 'off',
    fallback,
    (v) => (v ? 'on' : 'off'),
  )
}
