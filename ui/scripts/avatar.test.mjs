// `lib/avatar.ts`: which initials a sender gets, and which colour.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: avatar, close } = await load('/src/lib/avatar.ts')
const { initialsOf, avatarColor, accountColor, AVATAR_COLORS, ACCOUNT_COLORS } = avatar

// ── initials ──────────────────────────────────────────────────────────

assert.equal(initialsOf('Priya Raman', 'priya@example.com'), 'PR', 'first and last name')
assert.equal(initialsOf('Prof. Kemi Adeyemi', 'k@example.com'), 'PA', 'first and last word')
assert.equal(initialsOf('CP Comboios', 'tickets@cp.example'), 'CC')
assert.equal(initialsOf('GitHub', 'notifications@github.com'), 'G', 'one word, one letter')
assert.equal(initialsOf('', 'marcus.webb@example.com'), 'MW', 'no name: the address words')
assert.equal(initialsOf('', 'newsletter@economist.example'), 'N')
assert.equal(initialsOf('"Weber, Jonas"', 'jonas@example.com'), 'WJ', 'quotes are not letters')
assert.equal(initialsOf('élodie durand', 'e@example.com'), 'ÉD', 'any script, upper-cased')
assert.equal(initialsOf('  ', ''), '?', 'nothing to take a letter from')

// ── colours ───────────────────────────────────────────────────────────

const c = avatarColor('priya@example.com')
assert.ok(AVATAR_COLORS.includes(c), 'an avatar colour comes from the palette')
assert.equal(avatarColor(' Priya@Example.com '), c, 'the same address is the same colour')
assert.equal(
  new Set(['a@x', 'b@x', 'c@x', 'd@x', 'e@x', 'f@x'].map(avatarColor)).size > 1,
  true,
  'different addresses spread across the palette',
)

assert.equal(accountColor('b', ['a', 'b', 'c']), ACCOUNT_COLORS[1], 'by place in the list')
assert.equal(accountColor('gone', ['a']), ACCOUNT_COLORS[0], 'an unlisted account still gets one')
const many = Array.from({ length: ACCOUNT_COLORS.length + 1 }, (_, i) => `acct-${i}`)
assert.equal(accountColor(many.at(-1), many), ACCOUNT_COLORS[0], 'wraps past the palette')

await close()
console.log('avatar: all checks passed')
