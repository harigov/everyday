// What the mock backend (`mock-feed.ts`) does for the usability change: the
// newsletters the Newsletters source found, a story's parts, a summary made by
// background work, automation on arrival, and the clean-up settings and the
// list of what is not a newsletter.
//
// The mock is what the interface is built and tried against with no service
// behind it, so a mock that lets a pane be written against something the
// service would not do -- a part that is there before it was made, a found
// source that a save can un-find, a list that a save forgets -- costs more
// than the code it saves. State is the module's own and carries from one
// section to the next, so they run in the order they are written.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: feed, close } = await load('/src/lib/mock-feed.ts')

const {
  mockAddNewsSource,
  mockDeleteNewsSource,
  mockFeedSettings,
  mockGetStory,
  mockListNewsSources,
  mockListStories,
  mockListeningWork,
  mockRefreshNews,
  mockSaveFeedSettings,
  mockSaveNewsSource,
  mockStoryBody,
  mockSummarizeStory,
  mockTranscribeEpisode,
  mockWriteArticle,
} = feed

const sources = () => mockListNewsSources()
const named = (title) => sources().find((s) => s.source.title === title)
const byId = (id) => sources().find((s) => s.source.id === id)
const storiesOf = (id) => mockListStories({ sources: [id], limit: 500 }).stories

/** What the work list says of one kind of work for one story, asking it once. */
const workOf = (kind, story) =>
  mockListeningWork().find((w) => w.kind === kind && w.storyId === story)

/** Ask the list of work until a piece of it is over, which is how the mock moves it along. */
function finish(kind, story) {
  for (let i = 0; i < 20; i++) {
    if (!workOf(kind, story)) return
  }
  assert.fail(`${kind} work for ${story} never finished`)
}

// ── the newsletters that were found ───────────────────────────────────────

{
  const download = named('The Download')
  const algorithm = named('The Algorithm')
  const tldr = named('TLDR')
  assert.ok(download && algorithm && tldr, 'three newsletters, each a source of its own')

  assert.deepEqual(download.source.kind, {
    type: 'mail',
    sender: 'newsletters@technologyreview.com',
    name: 'The Download',
  })
  assert.deepEqual(algorithm.source.kind, {
    type: 'mail',
    sender: 'newsletters@technologyreview.com',
    name: 'The Algorithm',
  })
  assert.deepEqual(tldr.source.kind, {
    type: 'mail',
    sender: 'dan@tldrnewsletter.com',
    name: 'TLDR',
  })
  assert.equal(
    download.source.kind.sender,
    algorithm.source.kind.sender,
    'one address sends two of them: what tells them apart is the name',
  )
  for (const s of [download, algorithm, tldr]) {
    assert.equal(s.source.found, true, `${s.source.title} was found, not followed`)
    assert.equal(s.source.images, false, 'pictures held back, as Mail holds them')
  }

  assert.deepEqual([download.total, algorithm.total, tldr.total], [3, 2, 3])
  assert.deepEqual([download.unread, algorithm.unread, tldr.unread], [2, 1, 2])
  for (const id of ['src-download', 'src-algorithm', 'src-tldr']) {
    for (const story of storiesOf(id)) {
      assert.ok(story.mailMessage, 'each issue is the message it is')
      assert.equal(story.hasBody, true)
    }
  }

  // The catch-all that found them keeps no stories of its own.
  const letters = sources().find((s) => s.source.kind.type === 'newsletters')
  assert.ok(letters, 'the Newsletters source is still there')
  assert.equal(letters.total, 0, 'with nothing left under it')
  assert.equal(storiesOf(letters.source.id).length, 0)
  assert.equal(letters.source.found, undefined)

  // Refreshing it files nothing under it; refreshing a found one brings an issue.
  const [report] = mockRefreshNews(letters.source.id)
  assert.equal(report.inserted, 0)
  assert.equal(byId(letters.source.id).total, 0)
  const [issue] = mockRefreshNews('src-tldr')
  assert.equal(issue.inserted, 1)
  assert.equal(byId('src-tldr').total, 4)
}

// ── a story's parts ───────────────────────────────────────────────────────

{
  // The seed: an episode with a transcript that has not been written up
  // (st-17), one that has (st-19), and one with no transcript yet (st-18).
  const notes = mockStoryBody('st-17', 'notes')
  assert.match(notes.html, /In this episode/, 'the notes are the show notes')
  const transcript = mockStoryBody('st-17', 'transcript')
  assert.match(transcript.html, /\[00:00:00\]/, 'a transcript says when each paragraph begins')
  assert.throws(() => mockStoryBody('st-17', 'summary'), { code: 'notFound' })
  assert.throws(() => mockStoryBody('st-18', 'transcript'), { code: 'notFound' })

  // Written up: the telling is the summary, the show notes are the notes, and
  // the whole body is both.
  const whole = mockStoryBody('st-19').html
  const told = mockStoryBody('st-19', 'summary')
  assert.match(told.html, /lamp/)
  assert.doesNotMatch(told.html, /Show notes/, 'the summary is the telling alone')
  assert.equal(told.origin, 'transcript')
  const kept = mockStoryBody('st-19', 'notes').html
  assert.match(kept, /In this episode/)
  assert.doesNotMatch(kept, /lamp/, 'the notes are what the feed published, not the telling')
  assert.match(whole, /lamp/)
  assert.match(whole, /In this episode/)
  assert.equal(mockStoryBody('st-19').origin, 'transcript')

  // The same story with no part is what it always was.
  assert.match(mockStoryBody('st-17').html, /In this episode/)
  assert.equal(mockStoryBody('st-17').origin, 'feed')

  // Made by work: transcribe, then it can be read as a part, then write it up.
  mockTranscribeEpisode('st-18')
  finish('transcript', 'st-18')
  assert.ok(mockGetStory('st-18').files.transcript, 'the story links the transcript')
  assert.match(mockStoryBody('st-18', 'transcript').html, /\[00:00:00\]/)
  assert.throws(() => mockStoryBody('st-18', 'summary'), { code: 'notFound' })
  mockWriteArticle('st-18')
  finish('article', 'st-18')
  assert.match(mockStoryBody('st-18', 'summary').html, /lamp/)

  // An article has no notes of its own, a transcript or, until it is made, a summary.
  assert.match(mockStoryBody('st-1', 'notes').html, /two centuries/, 'its body is all there is')
  assert.throws(() => mockStoryBody('st-1', 'transcript'), { code: 'notFound' })
  assert.throws(() => mockStoryBody('st-1', 'summary'), { code: 'notFound' })
}

// ── summarising an article ────────────────────────────────────────────────

{
  assert.equal(mockGetStory('st-1').files?.summary, undefined)
  mockSummarizeStory('st-1')
  const first = workOf('summary', 'st-1')
  assert.equal(first.state, 'working', 'a summary is work, which the list shows')
  assert.ok(first.progress > 0 && first.progress < 1)
  assert.equal(mockGetStory('st-1').files?.summary, undefined, 'and is not there yet')
  assert.throws(() => mockStoryBody('st-1', 'summary'), { code: 'notFound' })

  finish('summary', 'st-1')
  assert.ok(mockGetStory('st-1').files.summary, 'the story links the summary')
  const made = mockStoryBody('st-1', 'summary')
  assert.match(made.html, /two centuries/, 'it is about the story')
  assert.match(made.html, /<li>/, 'and says what to take from it')

  // Asked for again, there is nothing to do.
  mockSummarizeStory('st-1')
  assert.equal(workOf('summary', 'st-1'), undefined)

  // An episode's summary is its article, which has a command of its own.
  assert.throws(() => mockSummarizeStory('st-17'), { code: 'invalid' })

  // A story that is only a headline has no text to summarise: it fails, with
  // the reason, and asking again tries again.
  mockSummarizeStory('st-9')
  const failed = workOf('summary', 'st-9')
  assert.equal(failed.state, 'failed')
  assert.match(failed.error, /Fetch the full article/)
  mockSummarizeStory('st-9')
  const again = mockListeningWork().find((w) => w.kind === 'summary' && w.storyId === 'st-9')
  assert.equal(again.state, 'failed', 'and it fails again for the same reason')
  assert.equal(
    mockListeningWork().filter((w) => w.kind === 'summary' && w.storyId === 'st-9').length,
    1,
    'one piece of work for it, not one for each try',
  )
}

// ── automation on arrival ─────────────────────────────────────────────────

{
  const longform = byId('src-longform').source
  mockSaveNewsSource({ ...longform, automation: { summarize: true, audio: false } })
  const before = new Set(storiesOf('src-longform').map((s) => s.id))
  mockRefreshNews('src-longform')
  const arrived = storiesOf('src-longform').find((s) => !before.has(s.id))
  assert.ok(arrived, 'the refresh brought a story')
  assert.ok(
    mockListeningWork().some((w) => w.kind === 'summary' && w.storyId === arrived.id),
    'a source that summarises asks for it as the story arrives',
  )

  // One that does not says nothing.
  const shipyard = byId('src-shipyard').source
  assert.equal(shipyard.automation, undefined)
  const already = new Set(storiesOf('src-shipyard').map((s) => s.id))
  mockRefreshNews('src-shipyard')
  const other = storiesOf('src-shipyard').find((s) => !already.has(s.id))
  assert.ok(other)
  assert.equal(
    mockListeningWork().some((w) => w.kind === 'summary' && w.storyId === other.id),
    false,
  )
}

// ── saving a source ───────────────────────────────────────────────────────

{
  const tldr = byId('src-tldr').source
  mockSaveNewsSource({
    ...tldr,
    // What a save cannot do: say that a source was found, or that it was not.
    found: false,
    trashMail: true,
    automation: { summarize: true, audio: true },
    retention: { readDays: 5, unreadDays: 9 },
  })
  const saved = byId('src-tldr').source
  assert.equal(saved.found, true, 'whether a source was found is the service’s to say')
  assert.equal(saved.trashMail, true)
  assert.deepEqual(saved.automation, { summarize: true, audio: true })
  assert.deepEqual(saved.retention, { readDays: 5, unreadDays: 9 })

  const longform = byId('src-longform').source
  mockSaveNewsSource({ ...longform, found: true })
  assert.equal(byId('src-longform').source.found, undefined, 'nor can a save find one')

  assert.throws(() => mockSaveNewsSource({ ...tldr, id: 'nope' }), { code: 'notFound' })
}

// ── Settings → Feed ───────────────────────────────────────────────────────

{
  const first = mockFeedSettings()
  assert.deepEqual(first.cleanup, {
    feeds: { readDays: 30, unreadDays: 90 },
    newsletters: { readDays: 30, unreadDays: 90 },
  })
  assert.equal(first.notNewsletters, undefined, 'none to start with, and an empty list is no list')

  // Days are kept whole and within a year, as the service tidies them.
  const saved = mockSaveFeedSettings({
    ...first,
    cleanup: {
      feeds: { readDays: 0, unreadDays: 1000 },
      newsletters: { readDays: 7.4, unreadDays: 21 },
    },
  })
  assert.deepEqual(saved.cleanup, {
    feeds: { readDays: 1, unreadDays: 365 },
    newsletters: { readDays: 7, unreadDays: 21 },
  })
  assert.deepEqual(mockFeedSettings().cleanup, saved.cleanup, 'and what was saved is what is read')

  // A save that does not carry the clean-up keeps what was there.
  const without = mockFeedSettings()
  delete without.cleanup
  assert.deepEqual(mockSaveFeedSettings(without).cleanup, saved.cleanup)
}

// ── unfollowing what was found ────────────────────────────────────────────

{
  mockDeleteNewsSource('src-download')
  assert.equal(named('The Download'), undefined)
  assert.equal(storiesOf('src-download').length, 0, 'its issues go with it')
  assert.deepEqual(
    mockFeedSettings().notNewsletters,
    [{ sender: 'newsletters@technologyreview.com', name: 'The Download' }],
    'unfollowing a found newsletter says it is not one',
  )

  mockDeleteNewsSource('src-algorithm')
  assert.deepEqual(mockFeedSettings().notNewsletters, [
    { sender: 'newsletters@technologyreview.com', name: 'The Download' },
    { sender: 'newsletters@technologyreview.com', name: 'The Algorithm' },
  ])

  // A source somebody followed is just unfollowed.
  mockDeleteNewsSource('src-radio')
  assert.equal(mockFeedSettings().notNewsletters.length, 2)

  // A save from the page carries the list, and one that does not keeps it.
  const bare = mockFeedSettings()
  delete bare.notNewsletters
  assert.equal(mockSaveFeedSettings(bare).notNewsletters.length, 2, 'a silent save keeps the list')

  // "Allow again" is a save with one fewer; the last one leaves no list at all.
  const now = mockFeedSettings()
  const left = mockSaveFeedSettings({ ...now, notNewsletters: now.notNewsletters.slice(1) })
  assert.deepEqual(left.notNewsletters, [
    { sender: 'newsletters@technologyreview.com', name: 'The Algorithm' },
  ])
  const none = mockSaveFeedSettings({ ...left, notNewsletters: [] })
  assert.equal(none.notNewsletters, undefined)

  // Following a sender by hand takes it off the list too.
  mockDeleteNewsSource('src-tldr')
  assert.deepEqual(mockFeedSettings().notNewsletters, [
    { sender: 'dan@tldrnewsletter.com', name: 'TLDR' },
  ])
  const followed = mockAddNewsSource({ type: 'mail', sender: 'dan@tldrnewsletter.com' })
  assert.equal(followed.found, undefined, 'a source somebody followed was not found')
  assert.equal(mockFeedSettings().notNewsletters, undefined)
}

await close()

// ── stopping many at once ─────────────────────────────────────────────────
//
// On a seed of its own, loaded afresh, since the section above has taken the
// found newsletters away -- and each of them offers a different way out.

{
  const { module: fresh, close: closeFresh } = await load('/src/lib/mock-feed.ts')
  const {
    mockAddNewsSource: follow,
    mockFeedSettings: settings,
    mockListNewsSources: list,
    mockListStories: listStories,
    mockMarkNotNewsletters,
    mockUnsubscribeNewsSources,
  } = fresh
  const followedIds = () => list().map((s) => s.source.id)
  const issuesOf = (id) => listStories({ sources: [id], limit: 500 }).stories
  const google = 'acct-google'
  const technology = 'newsletters@technologyreview.com'

  assert.ok(issuesOf('src-download').length > 0, 'the fresh seed has its newsletters back')

  // Unsubscribing: an outcome for each newsletter, in the order asked, each
  // the way its sender offers -- a one-click request, a page on their site,
  // an email to the list. A feed is unfollowed and says nothing; the
  // Newsletters source and an id nobody follows are passed over.
  const outcomes = mockUnsubscribeNewsSources([
    'src-download',
    'src-algorithm',
    'src-tldr',
    'src-radio',
    'src-newsletters',
    'src-nope',
    'src-download',
  ])
  assert.deepEqual(outcomes, [
    { account: google, email: technology, name: 'The Download', via: 'oneClick', done: true },
    {
      account: google,
      email: technology,
      name: 'The Algorithm',
      via: 'web',
      done: false,
      url: 'https://technologyreview.example/unsubscribe/the-algorithm',
    },
    { account: google, email: 'dan@tldrnewsletter.com', name: 'TLDR', via: 'mail', done: true },
  ])
  for (const id of ['src-download', 'src-algorithm', 'src-tldr', 'src-radio']) {
    assert.ok(!followedIds().includes(id), `${id} is no longer followed`)
    assert.equal(issuesOf(id).length, 0, `and ${id}'s stories went with it`)
  }
  assert.ok(followedIds().includes('src-newsletters'), 'the Newsletters source is never taken')
  assert.deepEqual(
    settings().notNewsletters,
    [
      { sender: technology, name: 'The Download' },
      { sender: technology, name: 'The Algorithm' },
      { sender: 'dan@tldrnewsletter.com', name: 'TLDR' },
    ],
    'each newsletter unsubscribed from is turned down, so it is not found again',
  )

  // A sender whose mail names no way out: still out of the Feed and turned
  // down, with an outcome that says it could not be asked.
  const long = follow({ type: 'mail', sender: 'letters@longletter.example' })
  assert.deepEqual(mockUnsubscribeNewsSources([long.id]), [
    { account: google, email: 'letters@longletter.example', name: 'The Long Letter', done: false },
  ])
  assert.ok(!followedIds().includes(long.id))
  assert.deepEqual(settings().notNewsletters.at(-1), { sender: 'letters@longletter.example' })

  // Not newsletters: a newsletter, followed or found, turned down and out of
  // the Feed; a web feed and the Newsletters source left where they are.
  const field = follow({ type: 'mail', sender: 'weekly@fieldnotes.example' })
  mockMarkNotNewsletters([field.id, 'src-longform', 'src-newsletters', 'src-nope'], 'important')
  assert.ok(!followedIds().includes(field.id), 'the newsletter leaves the Feed')
  assert.ok(followedIds().includes('src-longform'), 'a web feed is not one to say it of')
  assert.ok(followedIds().includes('src-newsletters'), 'and nor is the Newsletters source')
  assert.deepEqual(settings().notNewsletters.at(-1), { sender: 'weekly@fieldnotes.example' })

  await closeFresh()
}

console.log('feed-mock: all checks passed')
