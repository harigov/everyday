// `lib/mailview.ts`: which page a message body is drawn on, and
// `lib/mail.ts`'s `snippetText`, which keeps table borders out of a row.

import assert from 'node:assert/strict'
import { load, stubBrowser } from './harness.mjs'

// `mailview.ts` reads `api.ts`'s mock switch, which looks at `window`.
stubBrowser({ window: globalThis, location: new URL('http://localhost/'), navigator: true })

const {
  modules: [mailview, mail],
  close,
} = await load(['/src/lib/mailview.ts', '/src/lib/mail.ts'])
const { setsOwnColours, applyBodyTheme, quoteDocument } = mailview
const { snippetText } = mail

// ── setsOwnColours ────────────────────────────────────────────────────

for (const [html, want, why] of [
  ['<p style="color:#363737">Text</p>', true, 'a text colour'],
  ['<td style="font-size:13px; background-color: #fff">x</td>', true, 'a background'],
  ['<style>p{color:#333}</style><p>x</p>', true, 'a style sheet'],
  ['<table bgcolor="#ffffff"><tr><td>x</td></tr></table>', true, 'bgcolor'],
  ['<font color="red">x</font>', true, 'font color'],
  ['<p>Hello <b>there</b></p>', false, 'plain markup'],
  ['<p style="font-size:15px;margin:0">x</p>', false, 'a style with no colour in it'],
  ['<a href="https://example.com/?color=red">x</a>', false, 'a URL that says color='],
  ['<p>Background: we met last week.</p>', false, 'the word in prose'],
  ['<pre>color: red</pre>', false, 'CSS-shaped text in plain mail'],
]) {
  assert.equal(setsOwnColours(html), want, why)
}

// ── applyBodyTheme ────────────────────────────────────────────────────

const doc = (inner) =>
  '<!DOCTYPE html><html><head><style>body{color:#1c1a17;background:#f7f6f3}' +
  '@media (prefers-color-scheme: dark){body{color:#eceaf0;background:#17161a}}</style>' +
  `</head><body>${inner}</body></html>`

const plain = doc('<pre>Hi,\nsee you then.</pre>')
const styled = doc(
  '<style>@media (prefers-color-scheme: dark){p{color:#fff}}</style>' +
    '<p style="color:#363737">The conflict continues.</p>',
)
const added = (html) => html.match(/<style>([^<]*)<\/style><\/head>/)?.[1] ?? ''

assert.match(
  added(applyBodyTheme(plain, true)),
  /background:#17161a/,
  'plain mail, dark: dark page',
)
assert.match(
  added(applyBodyTheme(plain, false)),
  /background:#f7f6f3/,
  'plain mail, light: light page',
)
assert.match(
  added(applyBodyTheme(plain, false)),
  /color-scheme:light/,
  'light is written out, not left to a query about the system',
)
for (const dark of [true, false]) {
  const out = applyBodyTheme(styled, dark)
  assert.match(added(out), /background:#ffffff/, `styled mail is on white (dark=${dark})`)
  assert.match(added(out), /color:#1c1a17/)
  assert.ok(!/prefers-color-scheme\s*:\s*dark\)\{p/.test(out), "the sender's dark rules are off")
  assert.ok(out.includes('prefers-color-scheme: dark){body'), 'our own base style is untouched')
}
assert.equal(applyBodyTheme('<p>no head</p>', true), '<p>no head</p>', 'nothing to put it in')
assert.ok(
  applyBodyTheme(plain, true).includes('<pre>Hi,\nsee you then.</pre>'),
  'the body is left as it was',
)

// ── quoteDocument ─────────────────────────────────────────────────────

assert.match(quoteDocument('<p style="color:#333">x</p>', true), /background:#ffffff/)
assert.match(quoteDocument('<p>x</p>', true), /background:transparent/)

// ── snippetText ───────────────────────────────────────────────────────

const rule = '─'.repeat(199)
assert.equal(snippetText(`${rule}…`), '', 'a border and nothing else is no preview')
assert.equal(snippetText(`${rule} Pre-war oil│READ IN APP`), 'Pre-war oil READ IN APP')
assert.equal(snippetText('Thanks — see you then…'), 'Thanks — see you then…', 'dashes stay')
assert.equal(snippetText(''), '')

await close()
console.log('mailview: all checks passed')
