// Carrying the interface's own preferences across from Karma Yoga and Every
// Day: every `karmayoga.` / `karmayoga:` and `everyday.` / `everyday:` key
// moves straight to its `jatayu` spelling, once; when both former names hold
// the same preference the Karma Yoga value wins; a value this build already
// wrote is never overwritten; and a refused write loses nothing. Also that a
// pasted pairing link in any spelling enables Connect, and that every window's
// entry point imports the carry-over before anything else. See `lib/legacy.ts`.

import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
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

/** The keys `s` holds, in storage order. */
const keys = (s) => [...s.kept.keys()]

/** The keys `s` still holds under a former name, sorted. */
const former = (s) =>
  keys(s)
    .filter((k) => /^(karmayoga|everyday)/.test(k))
    .sort()

const { module, close } = await load('/src/lib/legacy.ts')
const { carryOver, isPairingLink, FORMERS } = module
const { check, ok, finish } = makeCheck()

const prefixes = FORMERS.map((f) => f.storagePrefix)
check('the former names, newest first', prefixes, ['karmayoga', 'everyday'])

// Karma Yoga's keys, as the owner's webview holds them today.
{
  const s = storage([
    ['karmayoga.theme', 'dark'],
    ['karmayoga.overview.layout', '[{"id":"a"}]'],
    ['karmayoga:assistant-width', '420'],
    ['karmayoga:week-words:2026-11-23', 'steady'],
    ['pane-w:mail-list', '300'],
  ])
  check('every Karma Yoga key is counted', carryOver(s), 4)
  check('a dotted key moves', s.kept.get('jatayu.theme'), 'dark')
  check('a structured value moves untouched', s.kept.get('jatayu.overview.layout'), '[{"id":"a"}]')
  check('a colon key moves', s.kept.get('jatayu:assistant-width'), '420')
  check('a week-words key moves', s.kept.get('jatayu:week-words:2026-11-23'), 'steady')
  check('a key of no app name is left alone', s.kept.get('pane-w:mail-list'), '300')
  check('no Karma Yoga key is left', former(s), [])
  check('a second run finds nothing to move', carryOver(s), 0)
}

// Every Day's keys, on a machine that never ran Karma Yoga: straight to
// Jatayu, with no Karma Yoga spelling made on the way.
{
  const s = storage([
    ['everyday.theme', 'light'],
    ['everyday:assistant-open', 'true'],
    ['everyday:week-words:2026-11-09', 'calm'],
  ])
  check('every Every Day key is counted', carryOver(s), 3)
  check('an Every Day dotted key moves', s.kept.get('jatayu.theme'), 'light')
  check('an Every Day colon key moves', s.kept.get('jatayu:assistant-open'), 'true')
  check('an Every Day week-words key moves', s.kept.get('jatayu:week-words:2026-11-09'), 'calm')
  check('no Every Day key is left, and no Karma Yoga one made', former(s), [])
}

// Both former names hold the same preference. The Every Day keys come first in
// storage order, to show the outcome does not depend on it.
{
  const s = storage([
    ['everyday.theme', 'light'],
    ['everyday:week-words:2026-11-23', 'old'],
    ['everyday.section', 'journal'],
    ['karmayoga.theme', 'dark'],
    ['karmayoga:week-words:2026-11-23', 'new'],
  ])
  check('both generations are counted', carryOver(s), 5)
  check('the Karma Yoga value wins', s.kept.get('jatayu.theme'), 'dark')
  check('and for a week-words key too', s.kept.get('jatayu:week-words:2026-11-23'), 'new')
  check('an Every Day value with no newer one moves', s.kept.get('jatayu.section'), 'journal')
  check('neither generation is left', former(s), [])
}

// A value this build already wrote wins over both former names.
{
  const s = storage([
    ['jatayu.section', 'mail'],
    ['karmayoga.section', 'notes'],
    ['everyday.section', 'journal'],
  ])
  check('the old keys are still retired', carryOver(s), 2)
  check('the Jatayu value is kept', s.kept.get('jatayu.section'), 'mail')
  check('and the old ones are gone', former(s), [])
}

// Names that only begin like a former name are not ours.
{
  const lookalikes = ['karmayogaish.theme', 'everydays:open', 'karmayoga']
  const s = storage(lookalikes.map((k) => [k, 'x']))
  check('lookalike keys are not counted', carryOver(s), 0)
  check('and stay as they were', keys(s), lookalikes)
}

// A refused write keeps the old key.
{
  const full = storage([
    ['karmayoga.theme', 'dark'],
    ['everyday.badge', 'off'],
  ])
  full.setItem = () => {
    throw new Error('QuotaExceededError')
  }
  check('a refused write moves nothing', carryOver(full), 0)
  check('and keeps the Karma Yoga key', full.kept.get('karmayoga.theme'), 'dark')
  check('and the Every Day key', full.kept.get('everyday.badge'), 'off')
}

// When the newer value cannot be written, the older one must not take its
// place: the next launch has to find the newer one still there to prefer.
{
  const s = storage([
    ['everyday.theme', 'light'],
    ['karmayoga.theme', 'dark'],
  ])
  const write = s.setItem
  let refusals = 1
  s.setItem = (k, v) => {
    if (refusals-- > 0) throw new Error('QuotaExceededError')
    return write(k, v)
  }
  check('a refused newer value moves nothing for that name', carryOver(s), 0)
  check('no older value stands in for it', s.kept.has('jatayu.theme'), false)
  check('both old keys are kept', former(s), ['everyday.theme', 'karmayoga.theme'])
  check('the next run retires both', carryOver(s), 2)
  check('and the newer value wins after all', s.kept.get('jatayu.theme'), 'dark')
}

check('no storage at all is not an error', carryOver(undefined), 0)

// Pairing links, in every spelling.
ok('a Jatayu pairing link', isPairingLink('jatayu://pair?host=a:1&code=X'))
ok('a Karma Yoga pairing link', isPairingLink('karmayoga://pair?host=a:1&code=X'))
ok('an Every Day pairing link', isPairingLink('everyday://pair?host=a:1&code=X'))
ok('with space around it', isPairingLink('  jatayu://pair?host=a:1&code=X \n'))
ok('wrapped in angle brackets', isPairingLink('<karmayoga://pair?host=a:1&code=X>'))
ok('not a web address', !isPairingLink('https://example.com/pair?host=a:1'))
ok('not another of our addresses', !isPairingLink('jatayu://mail/img/m/t'))
ok('not nothing', !isPairingLink(''))

// Every window's entry point imports the carry-over first: a store reads its
// own key the moment its module loads, so anything imported before it could
// read a default where a carried preference was about to be.
const ui = join(dirname(fileURLToPath(import.meta.url)), '..')
for (const page of ['index.html', 'panel.html', 'writing.html', 'voice-typing.html']) {
  const html = readFileSync(join(ui, page), 'utf8')
  const entry = html.match(/<script type="module" src="\/([^"]+)"/)?.[1]
  ok(`${page} names its entry point`, entry)
  if (!entry) continue
  const first = readFileSync(join(ui, entry), 'utf8').match(/^import\s+[^\n]*$/m)?.[0]
  const legacyFirst = /^import '(\.\.?\/)+lib\/legacy';?$/.test(first ?? '')
  ok(`${entry} imports lib/legacy first, not ${first}`, legacyFirst)
}

finish('legacy')
await close()
