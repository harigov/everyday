// The interface's own preferences, carried across from Every Day.
//
// Before the rename every key this interface keeps in localStorage was spelled
// `everyday.<name>` or `everyday:<name>`; now they are `karmayoga.<name>` and
// `karmayoga:<name>` (see `prefs.ts`). The desktop shell moves the webview's
// folder to the new bundle id before it opens a window
// (`crates/karmayoga-app/src/legacy.rs`), so the old values are here to be
// read, and this moves each to its new key, once. It runs as a side effect of
// being imported, and every entry point imports it first (`main.ts`,
// `writing/main.ts`): a store reads its own key the moment its module loads,
// which is before anything in an entry point's body would run.
//
// A key already set under its new name keeps its value: whatever this build
// wrote wins over whatever the old one left. Delete this file, and its two
// imports, once nobody is coming from Every Day any more.

const OLD = /^everyday([.:])/

/** Move every old key in `storage` to its new name. Returns how many moved. */
export function carryOver(storage: Storage | undefined = globalThis.localStorage): number {
  if (!storage || typeof storage.key !== 'function') return 0
  const old: string[] = []
  for (let i = 0; i < storage.length; i++) {
    const key = storage.key(i)
    if (key !== null && OLD.test(key)) old.push(key)
  }
  let moved = 0
  for (const key of old) {
    const renamed = key.replace(OLD, 'karmayoga$1')
    const value = storage.getItem(key)
    try {
      if (value !== null && storage.getItem(renamed) === null) storage.setItem(renamed, value)
    } catch {
      // Full, or refused: keep the old key, so nothing is lost, and try again
      // next time.
      continue
    }
    storage.removeItem(key)
    moved++
  }
  return moved
}

try {
  carryOver()
} catch {
  // No storage at all (a locked-down browser): there is nothing to carry.
}
