// Behaviour checks for the pure writing-help helpers -- see `src/lib/mailwrite.ts`.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: mailwrite, close } = await load('/src/lib/mailwrite.ts')
const { splitQuoted, joinQuoted, htmlToPlainText, escapeHtml, sendLaterChoices } = mailwrite

/** A local date/time, the same way `sendLaterChoices` itself reads `now` --
 *  never an ISO/UTC string, which would drift against `getHours`/`getDay`
 *  on a CI box in a different zone than whoever wrote this test. */
function at(year, month, day, hours = 0, minutes = 0) {
  return new Date(year, month - 1, day, hours, minutes, 0, 0)
}

// ── splitQuoted / joinQuoted ─────────────────────────────────────────────
//
// Real shapes `compose::quote_html` (`everyday-mail/src/compose.rs`) and
// `compose::quote_reply` (`everyday-core/src/mail/compose.rs`) both produce:
// an attribution paragraph immediately followed by a `<blockquote>`, with
// nothing of the person's own after it.

const freshReply =
  '<p>On Mon, 14 Sep 2026 at 09:00, Alice wrote:</p>' +
  '<blockquote type="cite"><p>Original text</p></blockquote>'

// A brand new reply draft: `new_draft` sets `body_html` to exactly the
// quote, nothing typed yet.
{
  const { own, quoted } = splitQuoted(freshReply)
  assert.equal(own, '', 'nothing of the persons own yet')
  assert.equal(quoted, freshReply)
  assert.equal(joinQuoted(own, quoted), freshReply, 'round-trips')
}

// Typed text ahead of the quote.
const typedReply = `<p>Sounds good.</p>${freshReply}`
{
  const { own, quoted } = splitQuoted(typedReply)
  assert.equal(own, '<p>Sounds good.</p>')
  assert.equal(quoted, freshReply)
  assert.equal(joinQuoted(own, quoted), typedReply)
}

// Once this has been through TipTap and back, `<blockquote type="cite">`
// comes back as a bare `<blockquote>` -- StarterKit's node has no attribute
// spec for `type` -- so the split still has to find it.
const roundTripped =
  '<p>Sounds good.</p><p>On Mon, 14 Sep 2026 at 09:00, Alice wrote:</p>' +
  '<blockquote><p>Original text</p></blockquote>'
{
  const { own, quoted } = splitQuoted(roundTripped)
  assert.equal(own, '<p>Sounds good.</p>')
  assert.equal(
    quoted,
    '<p>On Mon, 14 Sep 2026 at 09:00, Alice wrote:</p><blockquote><p>Original text</p></blockquote>',
  )
  assert.equal(joinQuoted(own, quoted), roundTripped)
}

// TipTap leaves an empty paragraph after a blockquote that ends the
// document. It belongs to the quote's tail: the split must still find the
// quote, and the round trip must still be exact.
const withTrailingParagraph =
  '<p>Sounds good.</p><p>On Mon, 14 Sep 2026 at 09:00, Alice wrote:</p>' +
  '<blockquote><p>Original text</p><blockquote><p>older</p></blockquote></blockquote><p></p>'
{
  const { own, quoted } = splitQuoted(withTrailingParagraph)
  assert.equal(own, '<p>Sounds good.</p>')
  assert.ok(quoted.startsWith('<p>On Mon, 14 Sep 2026 at 09:00, Alice wrote:</p>'))
  assert.equal(joinQuoted(own, quoted), withTrailingParagraph)
}

// No quote at all -- an ordinary new message -- is all own, per the
// function's own doc.
{
  const html = '<p>Hello there.</p>'
  const { own, quoted } = splitQuoted(html)
  assert.equal(own, html)
  assert.equal(quoted, '')
  assert.equal(joinQuoted(own, quoted), html)
}

// "wrote:" in the person's own words, with nothing quoted after it, must
// not be mistaken for the marker -- the blockquote is what anchors it.
{
  const html = "<p>I wrote: let's meet Tuesday.</p>"
  const { own, quoted } = splitQuoted(html)
  assert.equal(own, html)
  assert.equal(quoted, '')
}

// A reply to a reply: the parent's own sanitised body, quoted here, already
// carries an older quote of its own. The *outer* attribution is where this
// one splits -- greedy matching to the end of the string is what keeps the
// inner blockquote from ending the match early.
{
  const olderQuote =
    '<p>On Sun, 1 Jan 2026 at 08:00, Bob wrote:</p>' +
    '<blockquote><p>Even older text</p></blockquote>'
  const outerQuote =
    '<p>On Mon, 14 Sep 2026 at 09:00, Alice wrote:</p>' +
    `<blockquote><p>Alice's reply.</p>${olderQuote}</blockquote>`
  const html = `<p>My reply.</p>${outerQuote}`
  const { own, quoted } = splitQuoted(html)
  assert.equal(own, '<p>My reply.</p>')
  assert.equal(quoted, outerQuote)
  assert.equal(joinQuoted(own, quoted), html)
}

// ── htmlToPlainText ──────────────────────────────────────────────────────

assert.equal(htmlToPlainText(''), '')
assert.equal(
  htmlToPlainText('<p>Hello</p><p>World</p>'),
  'Hello\n\nWorld',
  'paragraphs blank-line apart',
)
assert.equal(
  htmlToPlainText('<p>Line1<br>Line2</p>'),
  'Line1\nLine2',
  'a <br> is one newline, not two',
)
assert.equal(
  htmlToPlainText('<p>Rock &amp; Roll &lt;3&gt; &quot;fun&quot; &#39;times&#39;</p>'),
  'Rock & Roll <3> "fun" \'times\'',
  'entities decode',
)
assert.equal(htmlToPlainText('<p>Tight&nbsp;spaces</p>'), 'Tight spaces')

// ── escapeHtml ───────────────────────────────────────────────────────────

assert.equal(
  escapeHtml(`<b>Bold</b> & "quoted" 'and' a tag`),
  '&lt;b&gt;Bold&lt;/b&gt; &amp; &quot;quoted&quot; &#39;and&#39; a tag',
)
// Order matters: `&` must be escaped first, or the entities this just wrote
// (`&lt;`, say) would themselves be escaped a second time.
assert.equal(escapeHtml('&lt;'), '&amp;lt;')

// ── sendLaterChoices ─────────────────────────────────────────────────────
//
// January 2026, picked for its weekdays: the 4th is a Sunday, the 5th a
// Monday, the 7th a Wednesday, the 10th a Saturday.

function keysOf(choices) {
  return choices.map((c) => c.key)
}

// An ordinary weekday morning: every preset is offered.
{
  const choices = sendLaterChoices(at(2026, 1, 7, 9, 0))
  assert.deepEqual(keysOf(choices), [
    'laterToday',
    'thisEvening',
    'tomorrowMorning',
    'tomorrowAfternoon',
    'mondayMorning',
  ])
  const byKey = Object.fromEntries(choices.map((c) => [c.key, c]))
  assert.equal(byKey.laterToday.label, 'Later today')
  assert.deepEqual(
    byKey.laterToday.at,
    at(2026, 1, 7, 12, 0),
    '+3h, no rounding needed on the hour',
  )
  assert.deepEqual(byKey.thisEvening.at, at(2026, 1, 7, 18, 0))
  assert.deepEqual(byKey.tomorrowMorning.at, at(2026, 1, 8, 8, 0))
  assert.deepEqual(byKey.tomorrowAfternoon.at, at(2026, 1, 8, 13, 0))
  assert.deepEqual(byKey.mondayMorning.at, at(2026, 1, 12, 8, 0), 'the coming Monday, 5 days out')
}

// "Later today" rounds to the nearest hour -- down under the half-hour,
// up at or past it.
{
  const down = sendLaterChoices(at(2026, 1, 7, 10, 20))
  assert.deepEqual(down.find((c) => c.key === 'laterToday').at, at(2026, 1, 7, 13, 0))
  const up = sendLaterChoices(at(2026, 1, 7, 10, 40))
  assert.deepEqual(up.find((c) => c.key === 'laterToday').at, at(2026, 1, 7, 14, 0))
}

// Past 17:00, "This evening" no longer makes sense -- it would be less than
// an hour out, or already behind.
{
  const choices = sendLaterChoices(at(2026, 1, 7, 17, 30))
  assert.ok(!keysOf(choices).includes('thisEvening'), 'too late for "This evening"')
  assert.ok(keysOf(choices).includes('laterToday'), 'but "Later today" still is one')
  assert.deepEqual(
    choices.find((c) => c.key === 'laterToday').at,
    at(2026, 1, 7, 21, 0),
    '17:30 + 3h = 20:30, rounded up',
  )
}

// Past 18:00, neither of the "today" presets is offered any more.
{
  const choices = sendLaterChoices(at(2026, 1, 7, 19, 0))
  assert.ok(!keysOf(choices).includes('laterToday'))
  assert.ok(!keysOf(choices).includes('thisEvening'))
  assert.deepEqual(keysOf(choices), ['tomorrowMorning', 'tomorrowAfternoon', 'mondayMorning'])
}

// On a Sunday, "Monday morning" is left out -- "Tomorrow morning" already
// means the same thing.
{
  const choices = sendLaterChoices(at(2026, 1, 4, 9, 0))
  assert.ok(!keysOf(choices).includes('mondayMorning'), 'Sunday: Tomorrow already covers it')
  assert.deepEqual(
    choices.find((c) => c.key === 'tomorrowMorning').at,
    at(2026, 1, 5, 8, 0),
    'tomorrow, which is the Monday',
  )
}

// On a Saturday, "Monday morning" is two days out, distinct from tomorrow.
{
  const choices = sendLaterChoices(at(2026, 1, 10, 9, 0))
  assert.deepEqual(choices.find((c) => c.key === 'mondayMorning').at, at(2026, 1, 12, 8, 0))
}

// On a Monday itself, "Monday morning" means a week out, not today.
{
  const choices = sendLaterChoices(at(2026, 1, 5, 9, 0))
  assert.deepEqual(choices.find((c) => c.key === 'mondayMorning').at, at(2026, 1, 12, 8, 0))
}

await close()
console.log('mailwrite: all checks passed')
