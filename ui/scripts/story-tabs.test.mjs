// `lib/story-tabs.ts`: which tabs a story's pane has, which one it opens on,
// and what a tab says when its document is not there yet.
//
// The type checker covers the shape of all this; what it cannot cover is a
// rule whose failure is quiet. A Summary that offers "Summarise" for an
// episode (which has no such command), a button for work that is already
// going, a failure that is never offered again, a remembered tab that is
// another kind's and leaves the pane with nothing to show -- none of them
// throws, and each is something somebody sees wrongly.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: tabs, close } = await load('/src/lib/story-tabs.ts')

const {
  TABS,
  defaultTab,
  offerLabel,
  offerText,
  panelFor,
  partOf,
  recallTab,
  rememberTab,
  tabKind,
  tabOr,
  tabsOf,
  workStatus,
} = tabs

const ids = (list) => list.map((t) => t.id)

// ── which tabs a story has ────────────────────────────────────────────────

{
  const episode = { media: { url: 'https://cdn.example/ep.mp3' } }
  const article = { media: null }
  const issue = { media: undefined, mailMessage: 'msg-1' }

  assert.equal(tabKind(episode), 'episode')
  assert.equal(tabKind(article), 'article')
  assert.equal(tabKind(issue), 'article', 'a newsletter issue is read, not heard')

  assert.deepEqual(ids(tabsOf(episode)), ['notes', 'transcript', 'summary'])
  assert.deepEqual(ids(tabsOf(article)), ['article', 'summary'])
  assert.deepEqual(ids(tabsOf(issue)), ['article', 'summary'])
  assert.deepEqual(
    TABS.episode.map((t) => t.label),
    ['Notes', 'Transcript', 'Summary'],
  )
  assert.deepEqual(
    TABS.article.map((t) => t.label),
    ['Article', 'Summary'],
  )

  assert.equal(defaultTab('episode'), 'notes', 'what the feed itself published, first')
  assert.equal(defaultTab('article'), 'article')
}

// What a stored value can mean: only a tab of this kind, else the default.
assert.equal(tabOr('episode', 'transcript'), 'transcript')
assert.equal(tabOr('episode', 'summary'), 'summary')
assert.equal(tabOr('article', 'summary'), 'summary')
assert.equal(tabOr('article', 'transcript'), 'article', 'an article has no transcript tab')
assert.equal(tabOr('episode', 'article'), 'notes', 'an episode has no article tab')
assert.equal(tabOr('episode', 'nonsense'), 'notes')
assert.equal(tabOr('article', null), 'article')
assert.equal(tabOr('article', undefined), 'article')
assert.equal(tabOr('episode', ''), 'notes')

// An article's own tab is the body; every other one is a part of it.
assert.equal(partOf('article'), undefined)
assert.equal(partOf('notes'), 'notes')
assert.equal(partOf('transcript'), 'transcript')
assert.equal(partOf('summary'), 'summary')

// ── remembered on this device ─────────────────────────────────────────────

{
  const stored = new Map()
  Object.defineProperty(globalThis, 'localStorage', {
    configurable: true,
    value: {
      getItem: (key) => stored.get(key) ?? null,
      setItem: (key, value) => stored.set(key, String(value)),
      removeItem: (key) => stored.delete(key),
    },
  })

  assert.equal(recallTab('episode'), 'notes', 'nothing remembered opens on the default')
  assert.equal(recallTab('article'), 'article')

  rememberTab('episode', 'transcript')
  assert.equal(recallTab('episode'), 'transcript')
  assert.equal(recallTab('article'), 'article', 'one kind’s choice is not the other’s')

  rememberTab('article', 'summary')
  assert.equal(recallTab('article'), 'summary')
  assert.equal(recallTab('episode'), 'transcript')

  // A value left by a version that had other tabs, or another kind's.
  stored.set('karmayoga.feed.tab.article', 'transcript')
  assert.equal(recallTab('article'), 'article', 'a stale value opens on the default')
  stored.set('karmayoga.feed.tab.episode', 'nonsense')
  assert.equal(recallTab('episode'), 'notes')

  // Storage that refuses -- a private window, a full disk -- is not an error.
  Object.defineProperty(globalThis, 'localStorage', {
    configurable: true,
    value: {
      getItem() {
        throw new Error('denied')
      },
      setItem() {
        throw new Error('denied')
      },
    },
  })
  assert.equal(recallTab('episode'), 'notes')
  assert.doesNotThrow(() => rememberTab('episode', 'summary'))
}

// ── what a tab shows ──────────────────────────────────────────────────────

const none = { transcript: false, summary: false }
const job = (kind, state, extra = {}) => ({ kind, storyId: 'st-1', title: 'T', state, ...extra })

// An article's own tab and an episode's notes are always the document.
assert.deepEqual(panelFor('article', 'article', none), { type: 'content' })
assert.deepEqual(panelFor('episode', 'notes', none), { type: 'content' })

// The Transcript tab.
assert.deepEqual(panelFor('episode', 'transcript', { ...none, transcript: true }), {
  type: 'content',
})
assert.deepEqual(panelFor('episode', 'transcript', none), {
  type: 'offer',
  action: 'transcribe',
  failed: false,
  error: null,
})
assert.deepEqual(
  panelFor('episode', 'transcript', {
    ...none,
    transcribing: job('transcript', 'working', { progress: 0.5 }),
  }),
  { type: 'working', kind: 'transcript', line: 'Transcribing 50%' },
  'work that is going is shown, not offered again',
)
assert.deepEqual(
  panelFor('episode', 'transcript', { ...none, transcribing: job('transcript', 'waiting') }),
  { type: 'working', kind: 'transcript', line: 'Waiting to transcribe' },
)
assert.deepEqual(
  panelFor('episode', 'transcript', {
    ...none,
    transcribing: job('transcript', 'failed', { error: 'No audio to listen to.' }),
  }),
  { type: 'offer', action: 'transcribe', failed: true, error: 'No audio to listen to.' },
  'a failure is offered again, with the reason',
)
assert.deepEqual(
  panelFor('episode', 'transcript', {
    transcript: true,
    summary: false,
    transcribing: job('transcript', 'working'),
  }),
  { type: 'content' },
  'a transcript that is there is read, whatever else is going',
)

// An episode's Summary is its article.
assert.deepEqual(panelFor('episode', 'summary', { ...none, summary: true }), { type: 'content' })
assert.deepEqual(
  panelFor('episode', 'summary', {
    transcript: true,
    summary: true,
    writing: job('article', 'working', { progress: 0.3 }),
  }),
  { type: 'content' },
  'writing it again leaves the one that is there to read',
)
assert.deepEqual(panelFor('episode', 'summary', none), {
  type: 'offer',
  action: 'write',
  failed: false,
  error: null,
})
assert.deepEqual(panelFor('episode', 'summary', { transcript: true, summary: false }), {
  type: 'offer',
  action: 'write',
  failed: false,
  error: null,
})
assert.deepEqual(
  panelFor('episode', 'summary', {
    transcript: true,
    summary: false,
    writing: job('article', 'working', { progress: 0.75 }),
  }),
  { type: 'working', kind: 'article', line: 'Writing the article 75%' },
)
assert.deepEqual(
  panelFor('episode', 'summary', { ...none, transcribing: job('transcript', 'working') }),
  { type: 'working', kind: 'transcript', line: 'Transcribing' },
  'the article waits on the transcript, so that is what is shown while it is made',
)
assert.deepEqual(
  panelFor('episode', 'summary', {
    transcript: true,
    summary: false,
    transcribing: job('transcript', 'working'),
  }),
  { type: 'offer', action: 'write', failed: false, error: null },
  'a transcript that is there does not hold the article back',
)
assert.deepEqual(
  panelFor('episode', 'summary', {
    transcript: true,
    summary: false,
    writing: job('article', 'failed', { error: 'The model did not answer.' }),
  }),
  { type: 'offer', action: 'write', failed: true, error: 'The model did not answer.' },
)
assert.deepEqual(
  panelFor('episode', 'summary', {
    ...none,
    transcribing: job('transcript', 'failed', { error: 'Not downloaded.' }),
  }),
  { type: 'offer', action: 'write', failed: true, error: 'Not downloaded.' },
  'with no transcript to write from, the transcript’s failure is the article’s',
)
assert.deepEqual(
  panelFor('episode', 'summary', {
    transcript: true,
    summary: false,
    transcribing: job('transcript', 'failed', { error: 'Not downloaded.' }),
  }),
  { type: 'offer', action: 'write', failed: false, error: null },
  'an old failure to transcribe is not the reason an article from a transcript that exists failed',
)

// An article's Summary.
assert.deepEqual(panelFor('article', 'summary', { ...none, summary: true }), { type: 'content' })
assert.deepEqual(panelFor('article', 'summary', none), {
  type: 'offer',
  action: 'summarise',
  failed: false,
  error: null,
})
assert.deepEqual(
  panelFor('article', 'summary', {
    ...none,
    summarising: job('summary', 'working', { progress: 0.5 }),
  }),
  { type: 'working', kind: 'summary', line: 'Summarising 50%' },
)
assert.deepEqual(
  panelFor('article', 'summary', { ...none, summarising: job('summary', 'waiting') }),
  { type: 'working', kind: 'summary', line: 'Waiting to summarise' },
)
assert.deepEqual(
  panelFor('article', 'summary', {
    ...none,
    summarising: job('summary', 'failed', { error: 'There is no article text to summarise yet.' }),
  }),
  {
    type: 'offer',
    action: 'summarise',
    failed: true,
    error: 'There is no article text to summarise yet.',
  },
  'a failed summary is offered again, with the reason',
)
assert.deepEqual(
  panelFor('article', 'summary', {
    transcript: false,
    summary: true,
    summarising: job('summary', 'working'),
  }),
  { type: 'content' },
)
// What an article's Summary never offers is an episode's way of making one.
assert.notEqual(panelFor('article', 'summary', none).action, 'write')
assert.notEqual(panelFor('episode', 'summary', none).action, 'summarise')

// The words on the buttons and under them.
assert.equal(offerLabel('transcribe', false), 'Transcribe')
assert.equal(offerLabel('transcribe', true), 'Transcription failed, try again')
assert.equal(offerLabel('write', false), 'Write the article')
assert.equal(offerLabel('write', true), 'Writing failed, try again')
assert.equal(offerLabel('summarise', false), 'Summarise')
assert.equal(offerLabel('summarise', true), 'Summarising failed, try again')
for (const action of ['transcribe', 'write', 'summarise']) {
  assert.ok(offerText(action).length > 0, `${action} says what it does`)
}

// `workLine` knows three kinds of work; a summary's words are this file's.
assert.equal(workStatus(job('summary', 'working')), 'Summarising')
assert.equal(workStatus(job('summary', 'working', { progress: 0.999 })), 'Summarising 99%')
assert.equal(workStatus(job('summary', 'failed')), 'The summary could not be written')
assert.equal(workStatus(job('transcript', 'working', { progress: 0.5 })), 'Transcribing 50%')
assert.equal(workStatus(job('article', 'waiting')), 'Waiting to write the article')

await close()
console.log('story-tabs: all checks passed')
