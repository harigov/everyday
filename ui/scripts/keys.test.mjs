// Behaviour checks for the keyboard-shortcut mechanics.
//
// Same reasoning as the test files beside this one. `keys.ts` is the half of
// the shortcut system that has no stores in it, which is exactly why it is
// the half worth testing: everything here is a rule, and every one of the
// rules fails silently.
//
// Three of them, and each has a way of going wrong that is invisible:
//
//   canonical chords   `mod+shift+k` and `shift+mod+k` are the same chord.
//                      A table that treats them as two has a binding that
//                      simply never fires, and nothing says so.
//
//   sequences          `g` then `j`. The half-finished state has to be held,
//                      matched against, and -- the part that is easy to get
//                      wrong -- reported as *pending*, because that is what
//                      tells the window to swallow the `g` rather than let it
//                      fall through to whatever else wanted it.
//
//   what counts as typing  a bare letter is a shortcut everywhere except in
//                      a field, where it is a letter. Get that wrong in one
//                      direction and the shortcuts do nothing; get it wrong
//                      in the other and nobody can type the word "cat" into
//                      the search box.
//
// No test framework, deliberately: one dependency-free file, run by
// `npm run check`, with the TypeScript loaded through Vite so it compiles
// exactly as the application compiles it.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: keys, close } = await load('/src/lib/keys.ts')
const { chord, chordLabel, chordOf, isTyping, keysLabel, match, sequence } = keys

/** A key event, with the four flags defaulted off. */
function press(key, flags = {}) {
  return { key, ctrlKey: false, metaKey: false, shiftKey: false, altKey: false, ...flags }
}

// ── Chords are canonical ──────────────────────────────────────────────

assert.equal(chord({ key: 'K', mod: true, shift: true }), 'mod+shift+k')
assert.equal(sequence('shift+mod+k')[0], 'mod+shift+k', 'modifier order must not matter')
assert.equal(sequence('MOD+N')[0], 'mod+n')
assert.deepEqual(sequence('g j'), ['g', 'j'])
assert.deepEqual(sequence('  g   j  '), ['g', 'j'], 'stray whitespace is not a third chord')

// Control and Command are one modifier, so one table serves both platforms.
assert.equal(chordOf(press('n', { ctrlKey: true })), 'mod+n')
assert.equal(chordOf(press('n', { metaKey: true })), 'mod+n')

// A press that is only a modifier is not a chord. If it were, holding Shift
// to type a capital would abandon a sequence half way through.
for (const key of ['Shift', 'Control', 'Meta', 'Alt']) {
  assert.equal(chordOf(press(key, { shiftKey: true })), null, key)
}

// Shift is not recorded for a printable character: `?` is Shift+/ on one
// layout and Shift+, on another, and the binding is for the character.
assert.equal(chordOf(press('?', { shiftKey: true })), '?')
assert.equal(chordOf(press('J', { shiftKey: true })), 'j')
// But it is recorded for a named key, where there is no character to go on.
assert.equal(chordOf(press('Tab', { shiftKey: true })), 'shift+Tab')

// ── Matching, including sequences ─────────────────────────────────────

const ran = []
const bindings = [
  { keys: 'g j', label: 'Journal', group: 'Go to', run: () => ran.push('journal') },
  { keys: 'g t', label: 'Todo', group: 'Go to', run: () => ran.push('todo') },
  { keys: 'c', label: 'Create', group: 'Act', run: () => ran.push('create') },
  { keys: 'mod+n', label: 'New', group: 'Act', whileTyping: true, run: () => ran.push('new') },
  {
    keys: 'd',
    label: 'Day',
    group: 'Calendar',
    when: () => inCalendar,
    run: () => ran.push('day'),
  },
]
let inCalendar = false

// A complete sequence hits, and nothing is left pending.
assert.deepEqual(match(bindings, ['g', 'j']), { hit: bindings[0], pending: false })

// Half of one hits nothing but *is* pending, which is what makes the window
// swallow the `g` instead of passing it on.
assert.equal(match(bindings, ['g']).hit, null)
assert.equal(match(bindings, ['g']).pending, true)

// A sequence that no binding starts is neither.
assert.deepEqual(match(bindings, ['q']), { hit: null, pending: false })
assert.deepEqual(match(bindings, ['g', 'q']), { hit: null, pending: false })

// A single chord that is nobody's prefix hits immediately.
assert.equal(match(bindings, ['c']).hit?.label, 'Create')
assert.equal(match(bindings, ['c']).pending, false)

// `when` decides whether a binding exists at all, so the calendar's letters
// are the calendar's and are not merely inert elsewhere.
assert.equal(match(bindings, ['d']).hit, null, 'the calendar is not open')
inCalendar = true
assert.equal(match(bindings, ['d']).hit?.label, 'Day')

// While the caret is in a field, only the bindings that asked for it match.
assert.equal(match(bindings, ['c'], true).hit, null, 'a bare letter is a letter in a field')
assert.equal(match(bindings, ['g'], true).pending, false, 'nor does it start a sequence')
assert.equal(match(bindings, ['mod+n'], true).hit?.label, 'New')

// ── What counts as typing ─────────────────────────────────────────────

const element = (tagName, extra = {}) => ({ tagName, isContentEditable: false, ...extra })
assert.equal(isTyping(element('INPUT')), true)
assert.equal(isTyping(element('TEXTAREA')), true)
// A `<select>` counts: a letter in one jumps to the option starting with it.
assert.equal(isTyping(element('SELECT')), true)
assert.equal(isTyping(element('DIV', { isContentEditable: true })), true, 'the entry editor')
assert.equal(isTyping(element('BUTTON')), false)
assert.equal(isTyping(element('DIV')), false)
assert.equal(isTyping(null), false)

// ── How they read ─────────────────────────────────────────────────────
//
// Symbols on a Mac because that is what is printed on its keys and shown in
// its own menus; words everywhere else, for the same reason.

assert.equal(chordLabel('mod+n', true), '⌘N')
assert.equal(chordLabel('mod+n', false), 'Ctrl+N')
assert.equal(chordLabel('mod+shift+k', false), 'Ctrl+Shift+K')
assert.equal(chordLabel('Escape', false), 'Esc')
assert.equal(chordLabel('ArrowLeft', false), '←')
assert.equal(keysLabel('g j', false), 'G then J')

await close()
console.log('keys: all checks passed')
