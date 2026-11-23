// `lib/automation.ts`: what a source does by itself with each new story, how
// long it keeps them, and how a form turns those into what is stored.
//
// Both are "the kind's default, or the source's own", and the failure that is
// quiet is the one in the middle: a switch that agrees with the default saved
// as a copy of it, so a source nobody changed stops following the default the
// day the default changes; a podcast given an audio setting it ignores; a
// default read from the wrong kind. None of it throws.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: automation, close } = await load('/src/lib/automation.ts')

const {
  automationForm,
  automationFromForm,
  defaultRetention,
  isMailSender,
  retentionForm,
  retentionFromForm,
  retentionLabel,
  sameNewsletter,
  summarizeDefault,
  summarizes,
} = automation

/** A source with only what these rules read. */
function source(kind, extra = {}) {
  return { id: 'src', kind, title: 'T', folder: '', ...extra }
}
const web = (extra) => source({ type: 'web', url: 'https://example.com/feed.xml' }, extra)
const hn = (extra) => source({ type: 'hackerNews', list: 'front', minPoints: 100 }, extra)
const mail = (extra) => source({ type: 'mail', sender: 'a@b.example', name: 'The Letter' }, extra)
const catchAll = (extra) => source({ type: 'newsletters' }, extra)
const show = (extra) => web({ podcast: { author: 'N' }, ...extra })

// ── what is done with each new story ──────────────────────────────────────

// The default: a podcast writes its article, nothing else summarises.
assert.equal(summarizeDefault(show()), true)
assert.equal(summarizeDefault(web()), false)
assert.equal(summarizeDefault(hn()), false)
assert.equal(summarizeDefault(mail()), false)
assert.equal(summarizeDefault(catchAll()), false)

// What the switch shows: the source's own word, else the default -- and `null`
// is as much "no word" as absent is.
assert.equal(summarizes(show()), true)
assert.equal(summarizes(web()), false)
assert.equal(summarizes(show({ automation: { summarize: null } })), true)
assert.equal(summarizes(web({ automation: { summarize: null } })), false)
assert.equal(summarizes(show({ automation: { summarize: false } })), false, 'a show can opt out')
assert.equal(summarizes(web({ automation: { summarize: true } })), true)
assert.equal(summarizes(mail({ automation: { summarize: true, audio: true } })), true)

assert.deepEqual(automationForm(show()), { summarize: true, audio: false })
assert.deepEqual(automationForm(web()), { summarize: false, audio: false })
assert.deepEqual(automationForm(web({ automation: { summarize: true, audio: true } })), {
  summarize: true,
  audio: true,
})
assert.deepEqual(automationForm(show({ automation: { summarize: false } })), {
  summarize: false,
  audio: false,
})

// Saved: `null` whenever the switch is where the default would have it.
assert.deepEqual(automationFromForm(web(), { summarize: false, audio: false }), {
  summarize: null,
  audio: false,
})
assert.deepEqual(automationFromForm(web(), { summarize: true, audio: false }), {
  summarize: true,
  audio: false,
})
assert.deepEqual(automationFromForm(show(), { summarize: true, audio: false }), {
  summarize: null,
  audio: false,
})
assert.deepEqual(automationFromForm(show(), { summarize: false, audio: false }), {
  summarize: false,
  audio: false,
})
assert.deepEqual(automationFromForm(mail(), { summarize: false, audio: true }), {
  summarize: null,
  audio: true,
})
assert.deepEqual(
  automationFromForm(show(), { summarize: true, audio: true }),
  { summarize: null, audio: false },
  'a podcast ignores audio, so none is kept for it',
)

// Opening a form and saving it untouched changes nothing about what is stored.
for (const s of [
  web(),
  show(),
  mail(),
  web({ automation: { summarize: true, audio: true } }),
  show({ automation: { summarize: false } }),
]) {
  const saved = automationFromForm(s, automationForm(s))
  assert.equal(summarizes({ ...s, automation: saved }), summarizes(s), 'the same answer after')
  assert.equal(saved.audio, s.automation?.audio === true && !s.podcast)
}
assert.equal(
  automationFromForm(web(), automationForm(web())).summarize,
  null,
  'a source nobody changed goes on following the default',
)

// ── how long stories are kept ─────────────────────────────────────────────

const cleanup = {
  feeds: { readDays: 14, unreadDays: 60 },
  newsletters: { readDays: 7, unreadDays: 21 },
}

assert.deepEqual(defaultRetention(web(), cleanup), cleanup.feeds)
assert.deepEqual(defaultRetention(hn(), cleanup), cleanup.feeds)
assert.deepEqual(defaultRetention(show(), cleanup), cleanup.feeds, 'a show keeps like a feed')
assert.deepEqual(defaultRetention(mail(), cleanup), cleanup.newsletters)
assert.deepEqual(defaultRetention(catchAll(), cleanup), cleanup.newsletters)
assert.deepEqual(defaultRetention(web(), null), { readDays: 30, unreadDays: 90 }, 'not read yet')
assert.deepEqual(defaultRetention(mail(), undefined), { readDays: 30, unreadDays: 90 })

assert.equal(retentionLabel({ readDays: 30, unreadDays: 90 }), '30 days read, 90 unread')
assert.equal(retentionLabel({ readDays: 1, unreadDays: 1 }), '1 day read, 1 unread')

// A source with nothing of its own opens on the default, ready to edit.
assert.deepEqual(retentionForm(web(), cleanup), { custom: false, readDays: 14, unreadDays: 60 })
assert.deepEqual(retentionForm(mail({ retention: null }), cleanup), {
  custom: false,
  readDays: 7,
  unreadDays: 21,
})
assert.deepEqual(retentionForm(web({ retention: { readDays: 3, unreadDays: 9 } }), cleanup), {
  custom: true,
  readDays: 3,
  unreadDays: 9,
})

// And saves as nothing at all, unless it was given a say.
assert.equal(
  retentionFromForm({ custom: false, readDays: 14, unreadDays: 60 }, web(), cleanup),
  null,
  '"the default" is stored as no retention, never as a copy of today’s numbers',
)
assert.equal(retentionFromForm({ custom: false, readDays: 5, unreadDays: 5 }, web(), cleanup), null)
assert.deepEqual(retentionFromForm({ custom: true, readDays: 3, unreadDays: 9 }, web(), cleanup), {
  readDays: 3,
  unreadDays: 9,
})
assert.deepEqual(
  retentionFromForm({ custom: true, readDays: 14, unreadDays: 60 }, web(), cleanup),
  { readDays: 14, unreadDays: 60 },
  'a choice made is kept even where it matches the default of the day',
)
// Numbers from a field: whole, from one day to a year, and a blank falls back.
assert.deepEqual(
  retentionFromForm({ custom: true, readDays: 0, unreadDays: 1000 }, web(), cleanup),
  { readDays: 1, unreadDays: 365 },
)
assert.deepEqual(
  retentionFromForm({ custom: true, readDays: 12.6, unreadDays: Number.NaN }, mail(), cleanup),
  { readDays: 13, unreadDays: 21 },
  'a number that is not one is the kind’s default for that box',
)
assert.deepEqual(
  retentionFromForm({ custom: true, readDays: null, unreadDays: null }, web(), null),
  { readDays: 30, unreadDays: 90 },
)

// ── mail ──────────────────────────────────────────────────────────────────

assert.equal(isMailSender(mail()), true)
assert.equal(isMailSender(mail({ found: true })), true, 'found or followed, a sender’s mail')
assert.equal(isMailSender(catchAll()), false, 'the catch-all is no sender: it has no mail to bin')
assert.equal(isMailSender(web()), false)

const download = { sender: 'newsletters@technologyreview.com', name: 'The Download' }
assert.equal(sameNewsletter(download, { ...download }), true)
assert.equal(
  sameNewsletter(download, { sender: ' Newsletters@TechnologyReview.com ', name: 'the download' }),
  true,
  'compared as the service does: trimmed, without regard to case',
)
assert.equal(sameNewsletter(download, { ...download, name: 'The Algorithm' }), false)
assert.equal(
  sameNewsletter(download, { sender: 'dan@tldrnewsletter.com', name: 'The Download' }),
  false,
)
assert.equal(
  sameNewsletter({ sender: 'a@b.example' }, { sender: 'a@b.example', name: '' }),
  true,
  'no name and an empty one are the same',
)
assert.equal(sameNewsletter({ sender: 'a@b.example' }, { sender: 'a@b.example', name: 'X' }), false)

await close()
console.log('automation: all checks passed')
