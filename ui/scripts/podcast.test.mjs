// `lib/podcast.ts`, and the parts of `lib/feed.ts` the podcast views added: the
// pure rules for durations, policy words, what the player is asked to play,
// how far along an episode is, the settings form, and the sidebar's handling
// of shows.
//
// The type checker covers the shape of all this; what it cannot cover is a
// rule whose failure is quiet. A duration that reads "0:60", a download that
// shows finished at 99 per cent, a "use the default" that is saved as a copy
// of today's default, an intro skipped on an episode that was being resumed --
// none of them throws, and all of them are something somebody hears or sees
// wrongly.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const {
  modules: [podcast, feed],
  close,
} = await load(['/src/lib/podcast.ts', '/src/lib/feed.ts'])

const {
  addedByLabel,
  clampCount,
  clampSkip,
  downloadLine,
  downloadPercent,
  downloadPolicyLabel,
  dropBefore,
  enclosureKind,
  episodeProgress,
  formatDuration,
  formatRemaining,
  formatSpan,
  keepPolicyLabel,
  playRequestForEpisode,
  playRequestForFile,
  playRequestForStory,
  podcastForm,
  podcastFromForm,
  policyNumber,
  queueRemainingMs,
  speedLabel,
  transcribePolicyLabel,
} = podcast

const {
  feedKey,
  groupFolders,
  isPodcastSource,
  looksLikeAddress,
  podcastSources,
  rankingNote,
  sameFeedAddress,
  scopeTitle,
  scopeUnread,
  unfollowDetail,
} = feed

const MIN = 60_000

// ── durations ────────────────────────────────────────────────────────────

{
  assert.equal(formatDuration(0), '0:00')
  assert.equal(formatDuration(1000), '0:01')
  // Floored: a clock does not say a minute at 59.9 seconds.
  assert.equal(formatDuration(59_999), '0:59')
  assert.equal(formatDuration(61_000), '1:01')
  assert.equal(formatDuration(723_000), '12:03')
  assert.equal(formatDuration(3_599_000), '59:59')
  assert.equal(formatDuration(3_600_000), '1:00:00')
  assert.equal(formatDuration(3_723_000), '1:02:03')
  assert.equal(formatDuration(36_000_000), '10:00:00')
  assert.equal(formatDuration(-5), '0:00', 'a negative length is nothing')
  assert.equal(formatDuration(Number.NaN), '0:00')

  assert.equal(formatSpan(1), '1 min', 'rounded up: not "0 min" for what is not done')
  assert.equal(formatSpan(30_000), '1 min')
  assert.equal(formatSpan(MIN), '1 min')
  assert.equal(formatSpan(MIN + 1), '2 min')
  assert.equal(formatSpan(12 * MIN), '12 min')
  assert.equal(formatSpan(59 * MIN + 1), '1 h')
  assert.equal(formatSpan(65 * MIN), '1 h 5 min')
  assert.equal(formatSpan(120 * MIN), '2 h')
  assert.equal(formatSpan(0), '0 min')

  assert.equal(formatRemaining(0, 12 * MIN), '12 min left')
  assert.equal(formatRemaining(30 * MIN, 65 * MIN), '35 min left')
  assert.equal(formatRemaining(MIN, 66 * MIN), '1 h 5 min left')
  assert.equal(formatRemaining(12 * MIN, 12 * MIN), 'Played')
  assert.equal(formatRemaining(13 * MIN, 12 * MIN), 'Played', 'past the end is still the end')
  assert.equal(formatRemaining(5, 0), '', 'no length, nothing to say')
  assert.equal(formatRemaining(-5, MIN), '1 min left', 'a position before the start is the start')
  assert.equal(formatRemaining(Number.NaN, MIN), '1 min left')
}

// ── policies, in words ───────────────────────────────────────────────────

{
  assert.equal(downloadPolicyLabel({ type: 'off' }), 'Off')
  assert.equal(downloadPolicyLabel({ type: 'newest', count: 1 }), 'Newest episode')
  assert.equal(downloadPolicyLabel({ type: 'newest', count: 3 }), 'Newest 3')
  assert.equal(downloadPolicyLabel({ type: 'all' }), 'Every episode')

  assert.equal(keepPolicyLabel({ type: 'forever' }), 'Keep forever')
  assert.equal(keepPolicyLabel({ type: 'afterPlayed', days: 7 }), '7 days after played')
  assert.equal(keepPolicyLabel({ type: 'afterPlayed', days: 1 }), '1 day after played')
  assert.equal(keepPolicyLabel({ type: 'newest', count: 5 }), 'Keep newest 5')

  assert.equal(transcribePolicyLabel('off'), 'Only when asked')
  assert.equal(transcribePolicyLabel('whenDownloaded'), 'When downloaded')
  assert.equal(transcribePolicyLabel('whenPlayed'), 'When played')

  assert.equal(speedLabel(1), '1×')
  assert.equal(speedLabel(1.5), '1.5×')
  assert.equal(speedLabel(1.75), '1.75×')
  assert.equal(speedLabel(2.5), '2.5×')

  assert.equal(addedByLabel('person'), 'Added by you')
  assert.equal(addedByLabel('show'), 'New episode')
  assert.equal(addedByLabel('recommended'), 'Recommended')
  assert.equal(addedByLabel('daily'), 'From your daily podcast')
  assert.equal(addedByLabel('assistant'), 'Added by the assistant')
}

// ── audio or video ───────────────────────────────────────────────────────

{
  // The same answers as the Rust side's `Enclosure::kind`.
  assert.equal(enclosureKind({ mime: 'audio/mpeg', url: 'https://x.example/a' }), 'audio')
  assert.equal(enclosureKind({ mime: 'video/mp4', url: 'https://x.example/a' }), 'video')
  assert.equal(enclosureKind({ mime: ' VIDEO/MP4 ', url: '' }), 'video', 'trimmed and lower-cased')
  assert.equal(enclosureKind({ mime: '', url: 'https://x.example/ep.mp4?token=1' }), 'video')
  assert.equal(enclosureKind({ mime: '', url: 'https://x.example/EP.M4V' }), 'video')
  assert.equal(enclosureKind({ mime: '', url: 'https://x.example/ep.mov' }), 'video')
  assert.equal(enclosureKind({ mime: '', url: 'https://x.example/ep.mkv' }), 'video')
  assert.equal(
    enclosureKind({ mime: 'application/octet-stream', url: 'https://x.example/ep.webm#t=1' }),
    'video',
    'an unhelpful type falls back to the extension',
  )
  assert.equal(enclosureKind({ mime: '', url: 'https://x.example/ep.mp3' }), 'audio')
  assert.equal(enclosureKind({ mime: '', url: '' }), 'audio', 'audio is what almost all of it is')
  assert.equal(
    enclosureKind({ mime: 'audio/x-m4a', url: 'https://x.example/ep.mp4' }),
    'audio',
    'the type the feed gave beats the extension',
  )
}

// ── what the player is asked to play ─────────────────────────────────────

function story(over = {}) {
  return {
    id: 'st1',
    sourceId: 'show',
    key: 'k',
    title: '  Episode one  ',
    url: 'https://show.example/1',
    author: '',
    publishedAt: '2026-11-01T00:00:00Z',
    fetchedAt: '2026-11-01T00:00:00Z',
    summary: '',
    imageUrl: '',
    discussionUrl: '',
    also: [],
    read: false,
    saved: false,
    hasBody: true,
    expired: false,
    media: {
      url: 'https://cdn.example/1.mp3',
      mime: 'audio/mpeg',
      durationMs: 42 * MIN,
      transcriptUrl: '',
      transcriptMime: '',
    },
    files: { media: 'file1' },
    ...over,
  }
}

{
  const req = playRequestForStory(story(), 'The Show', 'karmayoga://blob/abc', 1.5)
  assert.deepEqual(req, {
    what: { type: 'story', id: 'st1' },
    fileId: 'file1',
    kind: 'audio',
    title: 'Episode one',
    source: 'The Show',
    artwork: 'karmayoga://blob/abc',
    durationMs: 42 * MIN,
    speed: 1.5,
  })

  assert.equal(
    playRequestForStory(story({ files: {} }), 'S'),
    null,
    'not downloaded: nothing to play',
  )
  assert.equal(playRequestForStory(story({ files: undefined }), 'S'), null)
  assert.equal(playRequestForStory(story({ media: null }), 'S'), null, 'an article has no episode')
  assert.equal(playRequestForStory(story({ media: undefined }), 'S'), null)

  const video = playRequestForStory(
    story({
      media: { url: 'https://x.example/v.mp4', mime: '', transcriptUrl: '', transcriptMime: '' },
    }),
    'S',
  )
  assert.equal(video?.kind, 'video')
  assert.equal(video?.durationMs, null, 'a length the feed did not give is null, not undefined')
  assert.equal(video?.speed, null)
  assert.equal(video?.artwork, null)

  assert.equal(playRequestForStory(story({ title: '  ' }), 'S')?.title, 'Untitled episode')
}

{
  const show = { speed: 1.2, skipIntroS: 30 }
  // Not started: the intro is skipped, and the show's own speed rides along.
  const fresh = playRequestForEpisode(story(), 'S', show, null, false)
  assert.equal(fresh?.startMs, 30_000)
  assert.equal(fresh?.speed, 1.2)
  // Being returned to: the player resumes by itself, and must not be told otherwise.
  const resumed = playRequestForEpisode(story(), 'S', show, null, true)
  assert.equal(resumed?.startMs, undefined)
  assert.equal(resumed?.speed, 1.2)
  // No intro to skip.
  assert.equal(
    playRequestForEpisode(story(), 'S', { speed: null, skipIntroS: null }, null, false)?.startMs,
    undefined,
  )
  assert.equal(
    playRequestForEpisode(story(), 'S', { skipIntroS: 0 }, null, false)?.startMs,
    undefined,
  )
  assert.equal(playRequestForEpisode(story(), 'S', null, null, false)?.speed, null)
  assert.equal(playRequestForEpisode(story({ files: {} }), 'S', show, null, false), null)
}

function file(over = {}) {
  return {
    id: 'f1',
    storage: { kind: 'plain', path: 'Feed/Podcasts/The Show/Interview.final.mp3' },
    name: 'Interview.final.mp3',
    kind: 'audio',
    mime: 'audio/mpeg',
    size: 10,
    hash: '',
    modifiedAt: '2026-11-01T00:00:00Z',
    addedAt: '2026-11-01T00:00:00Z',
    updatedAt: '2026-11-01T00:00:00Z',
    origin: 'feed',
    status: { state: 'present' },
    media: { durationMs: 90_000 },
    caption: '',
    favourite: false,
    hidden: false,
    ...over,
  }
}

{
  assert.deepEqual(playRequestForFile(file()), {
    what: { type: 'file', id: 'f1' },
    fileId: 'f1',
    kind: 'audio',
    title: 'Interview.final',
    source: 'The Show',
    durationMs: 90_000,
  })
  assert.equal(playRequestForFile(file({ kind: 'video', name: 'Film.mov' }))?.kind, 'video')
  assert.equal(playRequestForFile(file({ kind: 'video', name: 'Film.mov' }))?.title, 'Film')
  assert.equal(playRequestForFile(file({ kind: 'image' })), null)
  assert.equal(playRequestForFile(file({ kind: 'document' })), null)
  assert.equal(playRequestForFile(file({ kind: 'other' })), null)
  assert.equal(playRequestForFile(file({ status: { state: 'trashed', at: 'x', from: 'y' } })), null)
  assert.equal(playRequestForFile(file({ status: { state: 'missing', since: 'x' } })), null)
  assert.equal(
    playRequestForFile(file({ storage: { kind: 'plain', path: 'song.flac' }, name: 'song.flac' }))
      ?.source,
    'Files',
    'at the top of the folder there is no folder to name',
  )
  assert.equal(
    playRequestForFile(file({ storage: { kind: 'sealed', blob: 'b' } }))?.source,
    'Files',
    'a sealed file has no folder',
  )
  assert.equal(playRequestForFile(file({ media: {} }))?.durationMs, null)
  assert.equal(
    playRequestForFile(file({ name: 'a sentence that ended.in a long tail' }))?.title,
    'a sentence that ended.in a long tail',
    'a long tail is not an extension',
  )
  assert.equal(playRequestForFile(file({ name: '.hidden' }))?.title, '.hidden')
}

// ── how far along ────────────────────────────────────────────────────────

{
  assert.equal(episodeProgress(null), null)
  assert.equal(episodeProgress(undefined), null)
  assert.equal(episodeProgress({ progress: 0, finished: false }), null, 'not started: no bar')
  assert.deepEqual(episodeProgress({ progress: 1, finished: true }), {
    fraction: 1,
    finished: true,
    label: 'Played',
  })
  const part = episodeProgress({
    progress: 0.2,
    finished: false,
    positionMs: 10 * MIN,
    durationMs: 40 * MIN,
  })
  assert.equal(part?.fraction, 0.25, 'position over length is the clock the player kept')
  assert.equal(part?.label, '30 min left')
  assert.equal(part?.finished, false)
  // The length may come from the entry when the record has none.
  assert.equal(
    episodeProgress({ progress: 0, finished: false, positionMs: 5 * MIN }, 10 * MIN)?.fraction,
    0.5,
  )
  // No clock at all: the record's own number, for a read article.
  const read = episodeProgress({ progress: 0.456, finished: false })
  assert.equal(read?.label, '46%')
  assert.equal(read?.fraction, 0.456)
  assert.equal(
    episodeProgress({ progress: 3, finished: false })?.fraction,
    1,
    'never past the end of the bar',
  )
  assert.equal(
    episodeProgress({ progress: 0, finished: false, positionMs: 90 * MIN, durationMs: 60 * MIN })
      ?.fraction,
    1,
  )
}

// ── a download on its way ────────────────────────────────────────────────

{
  assert.equal(downloadPercent(undefined), null)
  assert.equal(downloadPercent({ state: 'waiting', received: 0, total: 100 }), null)
  assert.equal(downloadPercent({ state: 'failed', received: 50, total: 100 }), null)
  assert.equal(
    downloadPercent({ state: 'downloading', received: 50, total: null }),
    null,
    'no size, no percentage',
  )
  assert.equal(downloadPercent({ state: 'downloading', received: 50, total: 0 }), null)
  assert.equal(downloadPercent({ state: 'downloading', received: 0, total: 100 }), 0)
  assert.equal(downloadPercent({ state: 'downloading', received: 43, total: 100 }), 43)
  assert.equal(downloadPercent({ state: 'downloading', received: 999, total: 1000 }), 99)
  // Never 100: every byte received is not yet a file in the folder.
  assert.equal(downloadPercent({ state: 'downloading', received: 100, total: 100 }), 99)
  assert.equal(downloadPercent({ state: 'downloading', received: 200, total: 100 }), 99)

  assert.equal(downloadLine(undefined), '')
  assert.equal(downloadLine({ state: 'waiting', received: 0 }), 'Waiting to download')
  assert.equal(downloadLine({ state: 'failed', received: 0 }), 'Download failed')
  assert.equal(downloadLine({ state: 'downloading', received: 0 }), 'Downloading')
  assert.equal(downloadLine({ state: 'downloading', received: 43, total: 100 }), 'Downloading 43%')
}

// ── Up Next ──────────────────────────────────────────────────────────────

{
  const item = (durationMs, consumption) => ({ entry: { durationMs }, consumption })
  assert.equal(queueRemainingMs([]), 0)
  assert.equal(queueRemainingMs([item(10 * MIN), item(20 * MIN)]), 30 * MIN)
  assert.equal(
    queueRemainingMs([item(10 * MIN, { positionMs: 4 * MIN, finished: false })]),
    6 * MIN,
  )
  assert.equal(
    queueRemainingMs([item(10 * MIN, { positionMs: 10 * MIN, finished: true })]),
    0,
    'finished counts nothing',
  )
  assert.equal(
    queueRemainingMs([item(null), item(undefined), item(5 * MIN)]),
    5 * MIN,
    'an article has no length',
  )
  assert.equal(
    queueRemainingMs([item(null, { durationMs: 7 * MIN, positionMs: 1 * MIN })]),
    6 * MIN,
    'the record may know the length',
  )
  assert.equal(queueRemainingMs([item(MIN, { positionMs: 5 * MIN })]), 0, 'never negative')

  const ids = ['a', 'b', 'c', 'd']
  // Onto the upper half of `c`: before it.
  assert.equal(dropBefore(ids, 'a', 'c', false), 'c')
  // Onto the lower half of `c`: before what follows it.
  assert.equal(dropBefore(ids, 'a', 'c', true), 'd')
  // Onto the lower half of the last: to the end.
  assert.equal(dropBefore(ids, 'a', 'd', true), null)
  assert.equal(dropBefore(ids, 'b', 'd', true), null)
  // Upward.
  assert.equal(dropBefore(ids, 'd', 'a', false), 'a')
  assert.equal(dropBefore(ids, 'd', 'b', false), 'b')
  assert.equal(dropBefore(ids, 'd', 'a', true), 'b')
  // No change is no write: on itself, or where it already sits.
  assert.equal(dropBefore(ids, 'a', 'a', false), undefined)
  assert.equal(dropBefore(ids, 'a', 'b', false), undefined, 'just before what already follows it')
  assert.equal(dropBefore(ids, 'b', 'a', true), undefined, 'just after what already precedes it')
  assert.equal(dropBefore(ids, 'd', 'c', true), undefined, 'the end, where it is')
  assert.equal(dropBefore(ids, 'a', 'zzz', false), undefined, 'something not in the queue')
  assert.equal(dropBefore(ids, 'zzz', 'a', false), undefined)
}

// ── the settings form ────────────────────────────────────────────────────

{
  assert.equal(clampCount(0, 3), 1)
  assert.equal(clampCount(-4, 3), 1)
  assert.equal(clampCount(2.6, 7), 3)
  assert.equal(clampCount(5000, 3), 999)
  assert.equal(clampCount(5000, 7, 365), 365)
  assert.equal(clampCount(null, 3), 3)
  assert.equal(clampCount(undefined, 3), 3)
  assert.equal(clampCount(Number.NaN, 3), 3)
  assert.equal(clampSkip(null), 0)
  assert.equal(clampSkip(-3), 0)
  assert.equal(clampSkip(29.6), 30)
  assert.equal(clampSkip(9999), 600)

  assert.equal(policyNumber({ type: 'newest', count: 4 }, 9), 4)
  assert.equal(policyNumber({ type: 'afterPlayed', days: 14 }, 9), 14)
  assert.equal(policyNumber({ type: 'forever' }, 9), 9)
  assert.equal(policyNumber({ type: 'all' }, 9), 9)
  assert.equal(policyNumber(null, 9), 9)

  // A show that follows every default: every choice is "default", and says so
  // back as nothing at all.
  const plain = { author: 'Somebody', directoryId: '123' }
  const form = podcastForm(plain)
  assert.equal(form.download, 'default')
  assert.equal(form.keep, 'default')
  assert.equal(form.queueNew, 'default')
  assert.equal(form.transcribe, 'default')
  assert.equal(form.speed, null)
  assert.equal(form.skipIntroS, 0)
  assert.deepEqual(podcastFromForm(plain, form), {
    author: 'Somebody',
    directoryId: '123',
    download: null,
    keep: null,
    queueNew: null,
    transcribe: null,
    speed: null,
    skipIntroS: null,
  })

  // And the same through `null`s a service may send for what it has no say in.
  assert.deepEqual(
    podcastFromForm(
      {
        author: 'a',
        download: null,
        keep: null,
        queueNew: null,
        transcribe: null,
        speed: null,
        skipIntroS: null,
      },
      podcastForm({
        author: 'a',
        download: null,
        keep: null,
        queueNew: null,
        transcribe: null,
        speed: null,
        skipIntroS: null,
      }),
    ),
    {
      author: 'a',
      download: null,
      keep: null,
      queueNew: null,
      transcribe: null,
      speed: null,
      skipIntroS: null,
    },
  )

  // Every override survives a round trip through the form.
  const own = {
    author: 'Somebody',
    download: { type: 'newest', count: 5 },
    keep: { type: 'afterPlayed', days: 14 },
    queueNew: false,
    transcribe: 'whenPlayed',
    speed: 1.5,
    skipIntroS: 30,
  }
  const ownForm = podcastForm(own)
  assert.equal(ownForm.download, 'newest')
  assert.equal(ownForm.downloadCount, 5)
  assert.equal(ownForm.keep, 'afterPlayed')
  assert.equal(ownForm.keepNumber, 14)
  assert.equal(ownForm.queueNew, 'no')
  assert.equal(ownForm.transcribe, 'whenPlayed')
  assert.deepEqual(podcastFromForm(own, ownForm), own)

  assert.equal(podcastForm({ author: '', queueNew: true }).queueNew, 'yes')
  assert.deepEqual(podcastForm({ author: '', download: { type: 'off' } }).download, 'off')
  assert.deepEqual(podcastForm({ author: '', keep: { type: 'newest', count: 8 } }).keepNumber, 8)
  assert.equal(podcastForm({ author: '', keep: { type: 'forever' } }).keepNumber, 7)

  // Choosing a different policy after another kept a number it has no use for:
  // only the number that belongs to the choice is saved.
  const switched = { ...ownForm, download: 'all', keep: 'forever' }
  const saved = podcastFromForm(own, switched)
  assert.deepEqual(saved.download, { type: 'all' })
  assert.deepEqual(saved.keep, { type: 'forever' })
  // A number field left empty or silly is made sensible, not saved as nothing.
  assert.deepEqual(podcastFromForm(own, { ...ownForm, downloadCount: null }).download, {
    type: 'newest',
    count: 3,
  })
  assert.deepEqual(
    podcastFromForm(own, { ...ownForm, keep: 'afterPlayed', keepNumber: 99999 }).keep,
    { type: 'afterPlayed', days: 365 },
  )
  assert.deepEqual(podcastFromForm(own, { ...ownForm, keep: 'newest', keepNumber: null }).keep, {
    type: 'newest',
    count: 5,
  })
  // Back to the default, which is saved as no choice.
  const cleared = podcastFromForm(own, {
    ...ownForm,
    download: 'default',
    speed: null,
    skipIntroS: 0,
    queueNew: 'default',
    transcribe: 'default',
  })
  assert.equal(cleared.download, null)
  assert.equal(cleared.speed, null)
  assert.equal(cleared.skipIntroS, null)
  assert.equal(cleared.queueNew, null)
  assert.equal(cleared.transcribe, null)
  assert.equal(podcastFromForm(own, { ...ownForm, queueNew: 'yes' }).queueNew, true)
  // What the form has no field for is left as it was.
  assert.equal(podcastFromForm({ ...own, directoryId: 'x' }, ownForm).directoryId, 'x')
  assert.equal(podcastFromForm(own, ownForm).author, 'Somebody')
}

// ── shows in the sidebar ─────────────────────────────────────────────────

function source(id, over = {}) {
  return {
    id,
    kind: { type: 'web', url: `https://${id}.example/feed.xml` },
    title: id,
    folder: '',
    siteUrl: '',
    images: true,
    fullText: false,
    keepOutOfInbox: false,
    mute: [],
    paused: false,
    sortOrder: 0,
    createdAt: '2026-10-01T00:00:00Z',
    updatedAt: '2026-10-01T00:00:00Z',
    ...over,
  }
}

const summary = (id, unread = 0, over = {}) => ({ source: source(id, over), unread, total: unread })
const show = { author: 'A' }

{
  assert.equal(isPodcastSource(source('blog')), false)
  assert.equal(isPodcastSource(source('pod', { podcast: show })), true)
  assert.equal(isPodcastSource(source('pod', { podcast: null })), false)
  assert.equal(isPodcastSource(null), false)
  assert.equal(isPodcastSource(undefined), false)

  const all = [
    summary('blog', 2),
    summary('pod', 5, { podcast: show }),
    summary('pod2', 1, { podcast: show }),
  ]
  assert.deepEqual(
    podcastSources(all).map((s) => s.source.id),
    ['pod', 'pod2'],
  )

  // A show is in the sidebar's Podcasts group and not in a folder as well.
  const folders = groupFolders([
    summary('blog', 2, { folder: 'news' }),
    summary('pod', 5, { folder: 'news', podcast: show }),
    summary('only', 3, { folder: 'shows', podcast: show }),
  ])
  assert.deepEqual(
    folders.map((f) => f.name),
    ['news'],
    'a folder of nothing but shows is not a folder',
  )
  assert.deepEqual(
    folders[0].sources.map((s) => s.source.id),
    ['blog'],
  )
  assert.equal(folders[0].unread, 2)

  assert.equal(scopeTitle({ kind: 'upnext' }, all), 'Up Next')
  assert.equal(scopeTitle({ kind: 'recommended' }, all), 'Recommended')
  assert.equal(scopeTitle({ kind: 'podcasts' }, all), 'Podcasts')
  assert.equal(scopeTitle({ kind: 'all' }, all), 'All')
  assert.equal(scopeTitle({ kind: 'source', id: 'pod' }, all), 'pod')

  assert.equal(scopeUnread({ kind: 'podcasts' }, all), 6)
  assert.equal(scopeUnread({ kind: 'all' }, all), 8)
  assert.equal(scopeUnread({ kind: 'upnext' }, all), 0)
  assert.equal(scopeUnread({ kind: 'recommended' }, all), 0)
  assert.equal(scopeUnread({ kind: 'source', id: 'pod' }, all), 5)

  assert.match(unfollowDetail(source('pod', { podcast: show })), /downloaded/)
  assert.doesNotMatch(unfollowDetail(source('blog')), /downloaded/)
}

// ── addresses ────────────────────────────────────────────────────────────

{
  assert.equal(feedKey('https://www.Example.com/feed/'), 'example.com/feed')
  assert.equal(feedKey('http://example.com/feed'), 'example.com/feed')
  assert.equal(feedKey('example.com/feed'), 'example.com/feed')
  assert.equal(feedKey('https://example.com/feed?x=1'), 'example.com/feed?x=1')
  assert.equal(feedKey('  '), '')
  assert.equal(sameFeedAddress('http://example.com/feed/', 'https://www.example.com/feed'), true)
  assert.equal(sameFeedAddress('https://example.com/a', 'https://example.com/b'), false)
  assert.equal(sameFeedAddress('', ''), false, 'nothing is not the same as nothing')

  assert.equal(looksLikeAddress('https://example.com/feed.xml'), true)
  assert.equal(looksLikeAddress('example.com'), true)
  assert.equal(looksLikeAddress('www.example.co.uk/podcast/feed'), true)
  assert.equal(looksLikeAddress('  npr.org  '), true)
  assert.equal(looksLikeAddress('lex fridman'), false)
  assert.equal(looksLikeAddress('Lex Fridman Podcast'), false)
  assert.equal(looksLikeAddress('serial'), false)
  assert.equal(looksLikeAddress('example.com is a site'), false)
  assert.equal(looksLikeAddress(''), false)
}

// ── Recommended's note ───────────────────────────────────────────────────

{
  const settings = (over = {}) => ({
    ranking: {
      enabled: true,
      criteria: [{ id: 'c', text: 'Is it about rust?', weight: 2 }],
      goals: false,
      windowDays: 14,
    },
    ...over,
  })
  const ranking = (over) => ({ ...settings().ranking, ...over })

  assert.equal(rankingNote(null, null), null, 'nothing known, nothing said')
  assert.equal(rankingNote(null, settings())?.kind ?? null, null)
  assert.equal(rankingNote({ judge: 'decision', ranked: 0, waiting: 0 }, null)?.kind, 'ready')

  assert.equal(rankingNote(null, { ranking: ranking({ enabled: false }) })?.kind, 'off')
  assert.equal(rankingNote(null, { ranking: ranking({ criteria: [] }) })?.kind, 'criteria')
  // Goals alone are something to rank by.
  assert.equal(
    rankingNote(
      { judge: 'small', ranked: 1, waiting: 0 },
      { ranking: ranking({ criteria: [], goals: true }) },
    )?.kind,
    'ready',
  )
  assert.equal(rankingNote({ judge: null, ranked: 0, waiting: 0 }, settings())?.kind, 'judge')
  assert.equal(rankingNote({ judge: undefined, ranked: 0, waiting: 0 }, null)?.kind, 'judge')

  const problem = rankingNote(
    { judge: 'decision', ranked: 3, waiting: 2, problem: 'The model is not loaded' },
    settings(),
  )
  assert.equal(problem?.kind, 'problem')
  assert.equal(problem?.text, 'The model is not loaded')

  const working = rankingNote({ judge: 'small', ranked: 12, waiting: 30 }, settings())
  assert.equal(working?.kind, 'working')
  assert.match(working?.text ?? '', /Small model/)
  assert.match(working?.text ?? '', /12/)
  assert.match(working?.text ?? '', /30/)
  assert.match(
    rankingNote({ judge: 'decision', ranked: 12, waiting: 0 }, settings())?.text ?? '',
    /decision model/,
  )
}

await close()
console.log('podcast: all checks passed')
