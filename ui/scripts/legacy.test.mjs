// Carrying the interface's own preferences across from Every Day: every
// `everyday.` / `everyday:` key moves to its `karmayoga` spelling, once, and a
// value this build already wrote is never overwritten. See `lib/legacy.ts`.

import { load, makeCheck } from './harness.mjs'

/** A Storage with the `length`/`key()` a real one has. */
function storage(entries) {
  const kept = new Map(entries)
  return {
    kept,
    get length() {
      return kept.size
    },
    key: (i) => [...kept.keys()][i] ?? null,
    getItem: (k) => (kept.has(k) ? kept.get(k) : null),
    setItem: (k, v) => kept.set(k, String(v)),
    removeItem: (k) => kept.delete(k),
  }
}

const { module, close } = await load('/src/lib/legacy.ts')
const { carryOver } = module
const { check, finish } = makeCheck()

const s = storage([
  ['everyday.theme', 'dark'],
  ['everyday.overview.layout', '[{"id":"a"}]'],
  ['everyday:assistant-width', '420'],
  ['everyday:week-words:2026-11-09', 'calm'],
  ['everyday.section', 'mail'],
  ['karmayoga.section', 'journal'],
  ['pane-w:mail', '300'],
])
check('every old key is counted', carryOver(s), 5)
check('a dotted key moves', s.kept.get('karmayoga.theme'), 'dark')
check('a structured value moves untouched', s.kept.get('karmayoga.overview.layout'), '[{"id":"a"}]')
check('a colon key moves', s.kept.get('karmayoga:assistant-width'), '420')
check('a key with a date in it moves', s.kept.get('karmayoga:week-words:2026-11-09'), 'calm')
check('a value this build wrote wins', s.kept.get('karmayoga.section'), 'journal')
check('a key of no app name is left alone', s.kept.get('pane-w:mail'), '300')
check(
  'no old key is left',
  [...s.kept.keys()].filter((k) => k.startsWith('everyday')),
  [],
)
check('a second run finds nothing to move', carryOver(s), 0)

const full = storage([['everyday.theme', 'dark']])
full.setItem = () => {
  throw new Error('QuotaExceededError')
}
check('a refused write moves nothing', carryOver(full), 0)
check('and keeps the old key', full.kept.get('everyday.theme'), 'dark')

check('no storage at all is not an error', carryOver(undefined), 0)

finish('legacy')
await close()
