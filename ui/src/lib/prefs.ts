// Local, unencrypted settings: chrome the interface remembers about itself.
//
// A dozen keys, and two ways of spelling them. Nine follow `jatayu.<name>`,
// one word per piece of chrome; the assistant rail's two came later, from a
// different corner of the interface, as `jatayu:<name>` -- a colon instead
// of a dot. That inconsistency is recorded here rather than fixed: somebody's
// browser already holds a value under one of these exact strings, and
// renaming a key without carrying its value across would lose it. A vault's
// worth of remembered widths and view choices is not worth a tidier spelling.
// (The two renames these keys have had -- `everyday`, then `karmayoga`, both
// now `jatayu` -- are carried across by `legacy.ts`, straight from either,
// before anything reads them.)
//
// What this file does fix is the six or so lines every site repeated around
// that string: read it, decide whether what came back still means anything,
// and fall back to a sensible default when it does not -- because a version
// of the app that offered `'grid' | 'list'` and now offers a third view must
// not crash on a browser that still has the old one written down.

/** Every key this file reads or writes, so a typo in one is a compile error. */
export type PrefKey =
  | 'jatayu.theme'
  | 'jatayu.section'
  | 'jatayu.todo.view'
  | 'jatayu.todo.group'
  | 'jatayu.calendar.view'
  | 'jatayu.library.view'
  | 'jatayu.library.capture'
  | 'jatayu.library.enrich'
  | 'jatayu.feed.filter'
  | 'jatayu.feed.kind'
  | 'jatayu.feed.folders.closed'
  | 'jatayu.feed.newsletters'
  | 'jatayu.money.view'
  | 'jatayu.screentime.view'
  | 'jatayu.screentime.range'
  | 'jatayu.screentime.devices'
  | 'jatayu.files.view'
  | 'jatayu.files.layout'
  | 'jatayu.files.sort'
  | 'jatayu.files.descending'
  | 'jatayu.health.view'
  | 'jatayu.health.range'
  | 'jatayu.health.vitals'
  | 'jatayu.journal.calendar'
  | 'jatayu.overview.layout'
  | 'jatayu.overview.introduced'
  | 'jatayu.tray'
  | 'jatayu.tray.meeting'
  | 'jatayu.badge'
  | 'jatayu.sidebar.folded'
  | 'jatayu.phone.bar'
  | 'jatayu.mail.accounts.open'
  | 'jatayu.mail.list.only'
  | 'jatayu.mail.category'
  | 'jatayu:assistant-open'
  | 'jatayu:assistant-width'

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
