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
  canTranscribeFile,
  clampCount,
  clampSkip,
  downloadLine,
  downloadPercent,
  downloadPolicyLabel,
  dropBefore,
  enclosureKind,
  episodeProgress,
  findWork,
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
  sameWork,
  speedLabel,
  transcribePolicyLabel,
  transcriptOfFile,
  workActive,
  workLine,
  workPercent,
} = podcast

const {
  bodyOriginOf,
  dayOfInstant,
  feedKey,
  filterHistory,
  groupFolders,
  groupHistory,
  historyMark,
  historyModeWord,
  historyProgress,
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

  // A show is a row in its folder like any other source: the Podcasts chip is
  // how to see only them, and there is no group of their own.
  const folders = groupFolders([
    summary('blog', 2, { folder: 'news' }),
    summary('pod', 5, { folder: 'news', podcast: show }),
    summary('only', 3, { folder: 'shows', podcast: show }),
  ])
  assert.deepEqual(
    folders.map((f) => f.name),
    ['news', 'shows'],
    'a folder of nothing but shows is a folder',
  )
  assert.deepEqual(
    folders[0].sources.map((s) => s.source.id),
    ['blog', 'pod'],
  )
  assert.equal(folders[0].unread, 7)

  assert.equal(scopeTitle({ kind: 'upnext' }, all), 'Up Next')
  assert.equal(scopeTitle({ kind: 'recommended' }, all), 'Recommended')
  assert.equal(scopeTitle({ kind: 'all' }, all), 'All')
  assert.equal(scopeTitle({ kind: 'source', id: 'pod' }, all), 'pod')

  assert.equal(scopeUnread({ kind: 'all' }, all, 'podcast'), 6, 'the chip is what Podcasts was')
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

// ── an article saved as audio ────────────────────────────────────────────

{
  const article = (over = {}) => story({ media: null, files: { audio: 'aud1' }, ...over })
  assert.deepEqual(playRequestForStory(article(), 'Longform', 'karmayoga://blob/x', 1.25), {
    what: { type: 'story', id: 'st1' },
    fileId: 'aud1',
    kind: 'audio',
    title: 'Episode one',
    source: 'Longform',
    artwork: 'karmayoga://blob/x',
    durationMs: null,
    speed: 1.25,
  })
  assert.equal(playRequestForStory(article({ title: '  ' }), 'S')?.title, 'Untitled article')
  assert.equal(
    playRequestForStory(article({ files: {} }), 'S'),
    null,
    'not saved as audio: nothing to play',
  )
  assert.equal(
    playRequestForStory(article({ files: { media: 'file1' } }), 'S'),
    null,
    'an episode file is not an article’s audio',
  )
  // An episode plays its episode, whatever else it has.
  assert.equal(
    playRequestForStory(story({ files: { media: 'file1', audio: 'aud1' } }), 'S')?.fileId,
    'file1',
  )
  assert.equal(
    playRequestForStory(story({ files: { audio: 'aud1' } }), 'S'),
    null,
    'an episode not downloaded yet is not played from an audio file it does not use',
  )
}

// ── background work ──────────────────────────────────────────────────────

{
  const work = (over = {}) => ({
    kind: 'transcript',
    storyId: 'st1',
    fileId: null,
    title: 'T',
    state: 'working',
    progress: 0.5,
    ...over,
  })

  assert.equal(workLine(work()), 'Transcribing 50%')
  assert.equal(workLine(work({ progress: null })), 'Transcribing')
  assert.equal(workLine(work({ state: 'waiting', progress: null })), 'Waiting to transcribe')
  assert.equal(workLine(work({ state: 'failed' })), 'Transcription failed')
  assert.equal(workLine(work({ kind: 'article', progress: 0.25 })), 'Writing the article 25%')
  assert.equal(
    workLine(work({ kind: 'article', state: 'failed' })),
    'The article could not be written',
  )
  assert.equal(workLine(work({ kind: 'audio', state: 'waiting' })), 'Waiting to save as audio')
  assert.equal(workLine(work({ kind: 'audio' })), 'Saving as audio 50%')
  assert.equal(workLine(work({ kind: 'audio', state: 'failed' })), 'Saving as audio failed')
  assert.equal(workLine(undefined), '')
  assert.equal(workLine(null), '')

  // Never 100: work that has done all of it is still being written to Files.
  assert.equal(workPercent(work({ progress: 1 })), 99)
  assert.equal(workPercent(work({ progress: 0.999 })), 99)
  assert.equal(workPercent(work({ progress: 0.5 })), 50)
  assert.equal(workPercent(work({ progress: 0 })), null, 'nothing done is not a percentage')
  assert.equal(workPercent(work({ progress: null })), null)
  assert.equal(workPercent(work({ progress: Number.NaN })), null)
  assert.equal(workPercent(work({ state: 'waiting' })), null)
  assert.equal(workPercent(work({ state: 'failed' })), null)
  assert.equal(workPercent(undefined), null)

  assert.ok(workActive(work()))
  assert.ok(workActive(work({ state: 'waiting' })))
  assert.ok(!workActive(work({ state: 'failed' })))

  assert.ok(sameWork(work(), work({ progress: 0.9, state: 'waiting' })), 'progress is not identity')
  assert.ok(!sameWork(work(), work({ kind: 'article' })))
  assert.ok(!sameWork(work(), work({ storyId: 'st2' })))
  assert.ok(
    sameWork({ kind: 'audio', storyId: 's' }, { kind: 'audio', storyId: 's', fileId: null }),
    'absent and null are the same story or file',
  )

  const list = [
    work({ storyId: 'a', state: 'failed', progress: null }),
    work({ storyId: 'a', state: 'waiting', progress: null }),
    work({ kind: 'article', storyId: 'a' }),
    work({ storyId: null, fileId: 'f1' }),
  ]
  assert.equal(
    findWork(list, { story: 'a' }, 'transcript')?.state,
    'waiting',
    'work still to be done is shown before work that failed',
  )
  assert.equal(findWork(list, { story: 'a' }, 'article')?.kind, 'article')
  assert.equal(
    findWork(list, { story: 'a' })?.state,
    'waiting',
    'with no kind named, the first still to be done',
  )
  assert.equal(findWork(list, { file: 'f1' })?.fileId, 'f1')
  assert.equal(findWork(list, { file: 'f1' }, 'audio'), undefined)
  assert.equal(findWork(list, { story: 'nope' }), undefined)
  assert.equal(findWork(list, {}), undefined, 'nothing asked about is nothing found')
  assert.equal(findWork(list, { story: null, file: null }), undefined)
  assert.equal(
    findWork([work({ state: 'failed' })], { story: 'st1' })?.state,
    'failed',
    'a failure alone is still shown, for the button to say so',
  )
}

{
  const prop = (value, type = 'text') => ({ 'feed.transcript': { type, value } })
  assert.equal(transcriptOfFile({ properties: prop('tr1') }), 'tr1')
  assert.equal(transcriptOfFile({}), null)
  assert.equal(transcriptOfFile({ properties: {} }), null)
  assert.equal(transcriptOfFile({ properties: prop('') }), null)
  assert.equal(transcriptOfFile({ properties: prop(3, 'number') }), null)
  assert.equal(
    transcriptOfFile({ properties: { 'feed.transcript_of': { type: 'text', value: 'rec' } } }),
    null,
    'the file a transcript is of is not a transcript',
  )

  const present = { state: 'present' }
  assert.ok(canTranscribeFile({ kind: 'audio', status: present }))
  assert.ok(canTranscribeFile({ kind: 'video', status: present }))
  assert.ok(!canTranscribeFile({ kind: 'document', status: present }))
  assert.ok(!canTranscribeFile({ kind: 'image', status: present }))
  assert.ok(!canTranscribeFile({ kind: 'audio', status: { state: 'trashed' } }))
  assert.ok(!canTranscribeFile({ kind: 'audio', status: { state: 'missing' } }))
}

// ── History ──────────────────────────────────────────────────────────────

{
  assert.equal(scopeTitle({ kind: 'history' }, []), 'History')
  assert.equal(scopeUnread({ kind: 'history' }, []), 0)

  // Local clocks throughout, so a headline never depends on the zone the test runs in.
  const now = new Date(2026, 10, 22, 15, 0)
  const at = (days, hour, minute = 0) => new Date(2026, 10, 22 - days, hour, minute).toISOString()
  const rec = (id, lastAt, over = {}) => ({
    id,
    what: { type: 'story', id: `s-${id}` },
    title: `Title ${id}`,
    source: 'Show',
    url: '',
    mode: 'read',
    firstAt: lastAt,
    lastAt,
    progress: 0,
    finished: false,
    ...over,
  })

  const days = groupHistory(
    [
      rec('old', at(10, 9)),
      rec('b', at(0, 9)),
      rec('c', at(0, 14)),
      rec('late', at(1, 23, 30)),
      rec('mid', at(3, 12)),
    ],
    now,
  )
  assert.deepEqual(
    days.map((d) => d.items.map((i) => i.id)),
    [['c', 'b'], ['late'], ['mid'], ['old']],
    'newest day first, and newest first within it',
  )
  assert.deepEqual(
    days.map((d) => d.day),
    ['2026-11-22', '2026-11-21', '2026-11-19', '2026-11-12'],
    'a day is the local one: half past eleven at night is still that night',
  )
  assert.equal(days[0].label, 'Today')
  assert.equal(days[1].label, 'Yesterday')
  assert.ok(!/\d/.test(days[2].label), `a weekday name this week, not "${days[2].label}"`)
  assert.ok(/\d/.test(days[3].label), `a date past a week, not "${days[3].label}"`)

  const odd = groupHistory([rec('x', 'not a date'), rec('y', at(0, 8))], now)
  assert.deepEqual(
    odd.map((d) => d.label),
    ['Today', 'Earlier'],
    'a record with no usable time goes last, under its own heading',
  )
  assert.equal(groupHistory([], now).length, 0)
  const tie = groupHistory([rec('a', at(0, 9)), rec('b', at(0, 9))], now)
  assert.deepEqual(
    tie[0].items.map((i) => i.id),
    ['b', 'a'],
    'the same moment is ordered by id, so a reload never shuffles the rows',
  )
  const given = [rec('1', at(5, 9)), rec('2', at(0, 9))]
  groupHistory(given, now)
  assert.deepEqual(
    given.map((r) => r.id),
    ['1', '2'],
    'the list it was given is left as it was',
  )

  assert.equal(dayOfInstant(at(0, 9)), '2026-11-22')
  assert.equal(dayOfInstant('nonsense'), '')

  const items = [
    rec('1', at(0, 9), { title: 'Night trains', source: 'The Slow Hour' }),
    rec('2', at(0, 8), {
      title: 'Pencils',
      source: 'Longform',
      url: 'https://longform.example/pencil',
    }),
  ]
  const kept = (query) => filterHistory(items, query).map((r) => r.id)
  assert.deepEqual(kept(''), ['1', '2'], 'nothing typed is everything')
  assert.deepEqual(kept('  '), ['1', '2'])
  assert.deepEqual(kept('slow trains'), ['1'], 'every word, from the title and the source')
  assert.deepEqual(kept('LONGFORM.example'), ['2'], 'the address counts, in any case')
  assert.deepEqual(kept('zebra'), [])
  assert.notEqual(filterHistory(items, ''), items, 'a copy, so sorting it cannot move the store’s')

  const storyRef = { type: 'story', id: 's' }
  assert.equal(historyMark({ what: storyRef, mode: 'watch' }), 'film')
  assert.equal(historyMark({ what: storyRef, mode: 'listen', mediaFile: 'f' }), 'podcast')
  assert.equal(historyMark({ what: storyRef, mode: 'listen' }), 'volume', 'an article read aloud')
  assert.equal(historyMark({ what: { type: 'file', id: 'f' }, mode: 'listen' }), 'music')
  assert.equal(historyMark({ what: { type: 'item', id: 'i' }, mode: 'read' }), 'book')
  assert.equal(historyMark({ what: { type: 'file', id: 'f' }, mode: 'read' }), 'file')
  assert.equal(historyMark({ what: storyRef, mode: 'read' }), 'newspaper')
  assert.equal(historyModeWord('read'), 'Read')
  assert.equal(historyModeWord('listen'), 'Listened to')
  assert.equal(historyModeWord('watch'), 'Watched')

  assert.deepEqual(historyProgress({ mode: 'read', progress: 1, finished: true }), {
    fraction: 1,
    finished: true,
    label: 'Finished',
  })
  const heard = historyProgress({
    mode: 'listen',
    progress: 0.33,
    finished: false,
    positionMs: 3 * MIN,
    durationMs: 9 * MIN,
  })
  assert.equal(heard.label, '6 min left')
  assert.ok(Math.abs(heard.fraction - 1 / 3) < 1e-9)
  assert.equal(historyProgress({ mode: 'read', progress: 0.4, finished: false }).label, '40%')
  assert.deepEqual(historyProgress({ mode: 'read', progress: 0, finished: false }), {
    fraction: 0,
    finished: false,
    label: 'Opened',
  })
  assert.equal(
    historyProgress({ mode: 'watch', progress: 0, finished: false }).label,
    'Started',
    'a recording that was begun was started, not opened',
  )
}

// ── What a body was written from ─────────────────────────────────────────

{
  const doc = (tag) =>
    `<!DOCTYPE html><html><head><meta http-equiv="Content-Security-Policy" content="x">${tag}<style>a{}</style></head><body><p>Hi</p></body></html>`
  assert.equal(
    bodyOriginOf(doc('<meta name="news-body-origin" content="transcript">')),
    'transcript',
  )
  assert.equal(bodyOriginOf(doc("<meta name='news-body-origin' content='extracted'>")), 'extracted')
  assert.equal(
    bodyOriginOf(doc('<meta name="news-body-origin" content="hackerNews">')),
    'hackerNews',
  )
  assert.equal(bodyOriginOf(doc('')), null, 'a document that does not say, does not')
  assert.equal(bodyOriginOf(doc('<meta name="news-body-origin" content="nonsense">')), null)
  assert.equal(
    bodyOriginOf(
      '<html><head></head><body><meta name="news-body-origin" content="transcript"></body></html>',
    ),
    null,
    'only the head speaks: a body that quotes the tag says nothing of where it came from',
  )
  assert.equal(bodyOriginOf('<p>no head at all</p>'), null)
}

await close()
console.log('podcast: all checks passed')
