// The interface's own preferences, carried across from Karma Yoga or Every Day.
//
// This application was Every Day until 14 November 2026, then Karma Yoga, and
// is Jatayu now. Under each former name every key this interface keeps in
// localStorage was spelled `<former>.<name>` or `<former>:<name>`; now they are
// `jatayu.<name>` and `jatayu:<name>` (see `prefs.ts`). The desktop shell moves
// the webview's folder to the new bundle id before it opens a window
// (`crates/jatayu-app/src/legacy.rs`), so the old values are here to be read,
// and this moves each to its new key, once. It runs as a side effect of being
// imported, and every entry point imports it first (`main.ts`, `panel/main.ts`,
// `writing/main.ts`, `voicetyping/main.ts`; `scripts/legacy.test.mjs` checks):
// a store reads its own key the moment its module loads, which is before
// anything in an entry point's body would run.
//
// Each former name is carried straight to Jatayu, newest first -- never Every
// Day to Karma Yoga to Jatayu, which would mean keeping every Karma Yoga
// spelling alive here only to rename it again. Should one storage hold the
// same preference under both prefixes (an Every Day key the first carry-over
// could not move, or a browser used with both builds), the Karma Yoga one is
// the later choice. So, for each name:
//
// - a value already under `jatayu.*` keeps it: whatever this build wrote wins;
// - otherwise the newest former's value moves, and older ones are dropped;
// - a write that fails (storage full, or refused) keeps that old key, and every
//   older one for the same name, so the next launch tries again and still
//   prefers the newest value.
//
// The same file answers the one other question the interface asks about old
// names: whether a pasted pairing link is worth sending when it was copied
// under a former name -- before the computer was updated, or from one not
// updated yet (`isPairingLink`, for `Connect.svelte`).
//
// `FORMERS` mirrors `Former::storage_prefix` and `Former::pairing_link` in
// `crates/jatayu-core/src/legacy.rs`. Delete a former's entry here when its
// entry goes there, and this file, with its imports, once nobody is coming
// from either (keeping the current spelling's half of `isPairingLink` in
// `Connect.svelte`).

/** Every name this interface went by before Jatayu, newest first. */
export const FORMERS = [
  { product: 'Karma Yoga', storagePrefix: 'karmayoga', pairingLink: 'karmayoga://pair?' },
  { product: 'Every Day', storagePrefix: 'everyday', pairingLink: 'everyday://pair?' },
] as const

/** The prefix this build's own keys carry. */
const CURRENT = 'jatayu'

/** How a pairing link copied from a Jatayu computer begins. */
const PAIRING_LINK = `${CURRENT}://pair?`

/**
 * Whether `link` looks like a pairing link, in this build's spelling or any
 * former one. Only a first check, for enabling a button: the shell reads the
 * link properly when it is sent (`jatayu_server::pairing::parse`), and says
 * what is wrong with one it cannot use, so this errs on letting it through --
 * including a link a mail client wrapped in angle brackets, which the parser
 * unwraps.
 */
export function isPairingLink(link: string): boolean {
  const text = link.trim().replace(/^</, '')
  return [PAIRING_LINK, ...FORMERS.map((f) => f.pairingLink)].some((p) => text.startsWith(p))
}

/**
 * Move every old key in `storage` to its new name. Returns how many old keys
 * it retired, counting one whose value was dropped for a newer one.
 */
export function carryOver(storage: Storage | undefined = globalThis.localStorage): number {
  if (!storage || typeof storage.key !== 'function') return 0
  const keys: string[] = []
  for (let i = 0; i < storage.length; i++) {
    const key = storage.key(i)
    if (key !== null) keys.push(key)
  }
  // `.name` / `:name`, for names whose newer value could not be written: an
  // older value must not take its place, and is kept for the next launch.
  const blocked = new Set<string>()
  let moved = 0
  for (const former of FORMERS) {
    const prefix = former.storagePrefix
    for (const key of keys) {
      if (!key.startsWith(prefix)) continue
      const rest = key.slice(prefix.length)
      if (rest[0] !== '.' && rest[0] !== ':') continue
      if (blocked.has(rest)) continue
      const renamed = CURRENT + rest
      const value = storage.getItem(key)
      try {
        if (value !== null && storage.getItem(renamed) === null) storage.setItem(renamed, value)
      } catch {
        // Full, or refused: keep the old key, so nothing is lost, and try
        // again next time.
        blocked.add(rest)
        continue
      }
      storage.removeItem(key)
      moved++
    }
  }
  return moved
}

try {
  carryOver()
} catch {
  // No storage at all (a locked-down browser): there is nothing to carry.
}
