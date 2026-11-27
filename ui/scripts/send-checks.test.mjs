// Checks before sending, through the real mock backend as the interface calls
// it: what Settings → Mail writing saves (`save_mail_writing`'s `sendChecks`)
// and what Send asks (`check_draft`). The interface is the part that decides
// what a result means -- a failed check or a skipped one warns, nothing to
// check sends at once -- so this holds the mock to the contract that decision
// reads: empty `results` and no `skipped` is "nothing to check".
//
// `mockCheckDraft` waits a moment, as a model does, inside the call itself;
// no timer is left behind for Node to wait on.

import assert from 'node:assert/strict'
import { load, stubBrowser } from './harness.mjs'

stubBrowser({ window: globalThis, location: new URL('http://localhost/') })

const { module: mock, close } = await load('/src/lib/mock.ts')
const { mockInvoke } = mock

await mockInvoke('unlock', { password: 'jatayu' })

const ACCOUNT = 'acct-google'

/** A draft stored with `bodyHtml`, as `written()` leaves one before a check. */
async function stored(bodyHtml) {
  const draft = await mockInvoke('new_draft', { account: ACCOUNT })
  draft.bodyHtml = bodyHtml
  await mockInvoke('save_draft', { draft })
  return draft.id
}

/** What Settings has now, saved back with `sendChecks` set. */
async function saveChecks(sendChecks) {
  const writing = await mockInvoke('mail_writing', {})
  return mockInvoke('save_mail_writing', { writing: { ...writing, sendChecks } })
}

// ── Nothing switched on: nothing to check, and no wait ──────────────────

{
  const writing = await mockInvoke('mail_writing', {})
  assert.deepEqual(writing.sendChecks ?? [], [], 'there are no checks until some are added')

  const id = await stored('<p>Need it ASAP!!!</p>')
  assert.deepEqual(
    await mockInvoke('check_draft', { id }),
    { results: [] },
    'no checks: empty results, no skipped, so Send goes at once',
  )
}

// ── What is saved comes back ────────────────────────────────────────────

{
  const checks = [
    { text: 'The tone is friendly', on: true },
    { text: 'The grammar and spelling are correct' },
    { text: 'It is formal', on: false },
  ]
  const saved = await saveChecks(checks)
  assert.deepEqual(saved.sendChecks, checks, 'save_mail_writing keeps sendChecks')
  assert.deepEqual(
    (await mockInvoke('mail_writing', {})).sendChecks,
    checks,
    'and mail_writing reads them back',
  )
  const writing = await mockInvoke('mail_writing', {})
  assert.equal(writing.learn, true, 'the rest of the record is as it was')
}

// ── A check that does not pass says what to change ──────────────────────

{
  // Only the two switched on are asked, in the order Settings lists them.
  const rude = await mockInvoke('check_draft', { id: await stored('<p>Send it ASAP!!!</p>') })
  assert.deepEqual(
    rude.results.map((r) => r.check),
    ['The tone is friendly', 'The grammar and spelling are correct'],
    'the check switched off is not asked',
  )
  const [tone, grammar] = rude.results
  assert.equal(tone.passed, false, 'ASAP and !!! are not friendly')
  assert.equal(typeof tone.note, 'string')
  assert.ok(tone.note.length > 0, 'a failed check says what to change')
  assert.equal(grammar.passed, true)
  assert.ok(!grammar.note, 'a passed check has no note')
  assert.equal(rude.model, 'small')
  assert.ok(!rude.skipped, 'they ran')

  const polite = await mockInvoke('check_draft', {
    id: await stored('<p>Could you send it on Thursday? Thanks.</p>'),
  })
  assert.deepEqual(
    polite.results.map((r) => r.passed),
    [true, true],
  )

  const typos = await mockInvoke('check_draft', {
    id: await stored('<p>Please send teh file when i can.</p>'),
  })
  assert.deepEqual(
    typos.results.map((r) => r.passed),
    [true, false],
    'a slip fails the grammar check, and not the tone one',
  )
  assert.ok(typos.results[1].note)
}

// ── Only the person's own words are read ────────────────────────────────

{
  // Signature and the quoted message under a reply are not what they wrote.
  const body =
    '<p>Thanks, Thursday works.</p><div data-signature=""><p>Hari, ASAP!!!</p></div>' +
    '<p>On Mon, 14 Sep 2026 at 09:00, Alice wrote:</p>' +
    '<blockquote type="cite"><p>Send it ASAP!!! and teh file</p></blockquote>'
  const report = await mockInvoke('check_draft', { id: await stored(body) })
  assert.deepEqual(
    report.results.map((r) => r.passed),
    [true, true],
  )
}

// ── Switched off, they are not asked at all ─────────────────────────────

{
  await saveChecks([{ text: 'The tone is friendly', on: false }])
  const id = await stored('<p>ASAP!!!</p>')
  assert.deepEqual(await mockInvoke('check_draft', { id }), { results: [] })
}

// ── What a save refuses, as `MailWriting::validate` does ────────────────

{
  const refused = (sendChecks, message) =>
    assert.rejects(saveChecks(sendChecks), (e) => {
      assert.match(e.message, message)
      return true
    })
  const check = (text) => ({ text, on: true })

  // At the limits: twelve, and two hundred characters, are fine.
  const full = Array.from({ length: 12 }, (_, i) => check(i === 0 ? 'x'.repeat(200) : `Check ${i}`))
  assert.equal((await saveChecks(full)).sendChecks.length, 12)

  await refused([check('  ')], /needs some words/)
  await refused([check('one\ntwo')], /one line/)
  await refused([check('x'.repeat(201))], /under 200 characters/)
  await refused(
    Array.from({ length: 13 }, (_, i) => check(`Check ${i}`)),
    /12 checks/,
  )
  assert.deepEqual(
    (await mockInvoke('mail_writing', {})).sendChecks,
    full,
    'a refused save leaves what was there',
  )
}

// ── Checks that cannot run at all say so in `off`, and ask nobody ───────

{
  // Writing help is off on the Fastmail account in the mock, as in `mock.ts`.
  const draft = await mockInvoke('new_draft', { account: 'acct-fastmail' })
  draft.bodyHtml = '<p>Send it ASAP!!!</p>'
  await mockInvoke('save_draft', { draft })

  await saveChecks([{ text: 'The tone is friendly', on: true }])
  const report = await mockInvoke('check_draft', { id: draft.id })
  assert.deepEqual(report.results, [], 'nothing was asked, so nothing failed')
  assert.match(report.off, /me@fastmail\.com/)
  assert.match(report.off, /writing help/, 'it names what is off, and the fix')
  assert.ok(!report.skipped, 'off is not "could not run this time"')
  assert.ok(!report.model, 'no model answered')

  // With nothing switched on there is nothing to say about it.
  await saveChecks([])
  assert.deepEqual(await mockInvoke('check_draft', { id: draft.id }), { results: [] })
}

// ── A check that cannot be asked is an error, never a pass ──────────────

{
  await saveChecks([{ text: 'The tone is friendly', on: true }])
  await assert.rejects(
    mockInvoke('check_draft', { id: 'draft-that-is-gone' }),
    /no such draft/,
    'a draft that is gone throws, and the interface warns with it',
  )
  await mockInvoke('lock', {})
  await assert.rejects(mockInvoke('check_draft', { id: 'whatever' }), 'a locked vault throws')
}

await close()
console.log('send-checks: all checks passed')
