// Seed news for the mock backend, and the query and mutation helpers
// `mock.ts` calls into for every Feed command.
//
// Four sources, chosen so that every state the Feed has to draw can be seen
// without a network: two ordinary web feeds (one filed in a folder), a Hacker
// News front page with points and comments, and a feed that has stopped
// answering. About sixteen stories across them, some read, one saved, some
// with a body and some that are only a summary -- the ones that offer "fetch
// the full article".
//
// The sources are made up. A mock that named a real publication would be
// putting words in its mouth, and the only thing these need to be is
// plausible.
//
// Two more, for the podcasts: an audio show with two episodes already in the
// files folder, and a video show with one coming down and one that failed.
// Behind them are Up Next and Recommended, what has been heard, the
// downloads, the directory and Settings → Feed -- see `docs/plans/podcasts.md`.
//
// And the newsletters, as `docs/plans/feed-usability.md` has them: three that
// the Newsletters source *found* in Mail -- two sent from one address under
// different names, one from another -- each a source of its own with its
// issues under it, and the Newsletters source itself with nothing left under
// it. Unfollowing a found one is saying it is not a newsletter, which puts it
// on Settings → Feed's list of those.
//
// Nothing here runs at module load but building the seed: no timers, so
// loading this file cannot hold a test process open. A refresh is a function
// call that adds the next story from a short script rather than a timer that
// does it on its own, and a download moves along each time the list of
// downloads is asked for, not on a clock. Background work -- a transcript, an
// episode written up as an article, an article saved as audio -- moves the
// same way, a step each time the list of work is asked for.
//
// What needs the files catalog -- the transcripts, the kept articles, the
// files History points at -- is made the first time the Feed is read, and not
// when this file loads: building the catalog is not something a module that
// is only imported should do.

import type { BodyOrigin } from './feed'
import { mockCatalogFile, mockFilesInvoke, mockTextFile } from './mock-files'
import type {
  AddedBy,
  ConsumedQuery,
  ConsumeMode,
  Consumption,
  ContentRef,
  DownloadState,
  DownloadStatus,
  Enclosure,
  FeedHit,
  FeedSettings,
  FileDetail,
  FileEntry,
  FileOrigin,
  FilePage,
  FileId,
  FoundFeed,
  HnList,
  Item,
  OpmlImportReport,
  NewSource,
  NewsSource,
  NewsSourceId,
  NewsletterRef,
  NewsletterSender,
  PodcastGenre,
  PodcastHit,
  ProgressReport,
  QueueEntry,
  QueueItem,
  QueuePlace,
  QueueView,
  RankingStatus,
  Recommendation,
  RefreshReport,
  Retention,
  SourceSummary,
  Span,
  Story,
  StoryId,
  StoryPage,
  StoryPart,
  StoryQuery,
  WorkItem,
} from './types'
import { VaultError } from './types'

const HOUR = 3_600_000

let seq = 0
function nextId(prefix: string): string {
  seq += 1
  return `${prefix}-${seq}`
}

/** An instant `hours` ago, as the wire carries it. */
function ago(hours: number): string {
  return new Date(Date.now() - hours * HOUR).toISOString()
}

// ── What the refresher knows ─────────────────────────────────────────────

/**
 * What a fetch learned, kept apart from the source. The real refresher never
 * writes a `Source` -- a rename made while a fetch is in flight must not be
 * overwritten by it -- and the mock keeps to the same shape so that nothing
 * written against it can come to depend on the two being one record.
 */
interface Fetched {
  lastOk: string | null
  lastError: string | null
  nextAt: string | null
}

// ── Sources ──────────────────────────────────────────────────────────────

const LONGFORM = 'src-longform'
const SHIPYARD = 'src-shipyard'
const HACKER = 'src-hn'
const RADIO = 'src-radio'
const SLOW = 'src-slowhour'
const FRAME = 'src-frame'
// Found by the Newsletters source: one sender sends two of them.
const DOWNLOAD = 'src-download'
const ALGORITHM = 'src-algorithm'
const TLDR = 'src-tldr'
const LETTERS = 'src-newsletters'

function source(
  partial: Partial<NewsSource> & Pick<NewsSource, 'id' | 'kind' | 'title'>,
): NewsSource {
  return {
    folder: '',
    siteUrl: '',
    icon: null,
    images: true,
    fullText: false,
    keepOutOfInbox: false,
    mute: [],
    retention: null,
    paused: false,
    sortOrder: 0,
    createdAt: ago(24 * 30),
    updatedAt: ago(24 * 30),
    ...partial,
  }
}

const sources: NewsSource[] = [
  source({
    id: LONGFORM,
    kind: { type: 'web', url: 'https://longform.example/feed.xml' },
    title: 'Longform Weekly',
    siteUrl: 'https://longform.example',
    sortOrder: 0,
  }),
  source({
    id: SHIPYARD,
    kind: { type: 'web', url: 'https://shipyard.example/atom.xml' },
    title: 'Shipyard Notes',
    folder: 'Engineering',
    siteUrl: 'https://shipyard.example',
    sortOrder: 1,
  }),
  source({
    id: HACKER,
    kind: { type: 'hackerNews', list: 'front', minPoints: 100 },
    title: 'Hacker News',
    siteUrl: 'https://news.ycombinator.com',
    sortOrder: 2,
  }),
  // Stopped answering a while ago. Pictures are off for it, so the "images
  // held back" bar can be seen on one of its stories.
  source({
    id: RADIO,
    kind: { type: 'web', url: 'https://oldradio.example/rss' },
    title: 'The Old Radio Blog',
    folder: 'Engineering',
    siteUrl: 'https://oldradio.example',
    images: false,
    sortOrder: 3,
  }),
  // An audio show: two episodes are kept in the files folder, new ones go
  // straight into Up Next, and it plays a little quicker than normal.
  source({
    id: SLOW,
    kind: { type: 'web', url: 'https://slowhour.example/feed.xml' },
    title: 'The Slow Hour',
    siteUrl: 'https://slowhour.example',
    sortOrder: 4,
    podcast: {
      author: 'Nadia Frost',
      directoryId: '1450001',
      download: { type: 'newest', count: 2 },
      keep: { type: 'afterPlayed', days: 7 },
      queueNew: true,
      speed: 1.2,
      skipIntroS: 12,
    },
  }),
  // A video show on every default.
  source({
    id: FRAME,
    kind: { type: 'web', url: 'https://framebyframe.example/video.xml' },
    title: 'Frame by Frame',
    siteUrl: 'https://framebyframe.example',
    sortOrder: 5,
    podcast: { author: 'Okonkwo and Lee', directoryId: '1450002' },
  }),
  // Newsletters found in Mail. A found source fetches nothing of its own: its
  // issues arrive through the Newsletters source's turn, and pictures follow
  // Mail's rule, which is to hold them back.
  source({
    id: DOWNLOAD,
    kind: { type: 'mail', sender: 'newsletters@technologyreview.com', name: 'The Download' },
    title: 'The Download',
    images: false,
    found: true,
    sortOrder: 6,
  }),
  source({
    id: ALGORITHM,
    kind: { type: 'mail', sender: 'newsletters@technologyreview.com', name: 'The Algorithm' },
    title: 'The Algorithm',
    images: false,
    found: true,
    sortOrder: 7,
  }),
  source({
    id: TLDR,
    kind: { type: 'mail', sender: 'dan@tldrnewsletter.com', name: 'TLDR' },
    title: 'TLDR',
    images: false,
    found: true,
    sortOrder: 8,
  }),
  // What found them. Nothing is stored under it any more, so a sidebar that
  // lists it only while it has stories draws no row for it.
  source({
    id: LETTERS,
    kind: { type: 'newsletters' },
    title: 'Newsletters',
    images: false,
    sortOrder: 9,
  }),
]

const fetched = new Map<NewsSourceId, Fetched>([
  [LONGFORM, { lastOk: ago(1), lastError: null, nextAt: ago(-1) }],
  [SHIPYARD, { lastOk: ago(2), lastError: null, nextAt: ago(-0.5) }],
  [HACKER, { lastOk: ago(0.5), lastError: null, nextAt: ago(-0.25) }],
  [
    RADIO,
    { lastOk: ago(24 * 9), lastError: 'The server answered 404 Not Found.', nextAt: ago(-6) },
  ],
  [SLOW, { lastOk: ago(1), lastError: null, nextAt: ago(-2) }],
  [FRAME, { lastOk: ago(3), lastError: null, nextAt: ago(-1) }],
  [DOWNLOAD, { lastOk: ago(1), lastError: null, nextAt: ago(-1) }],
  [ALGORITHM, { lastOk: ago(1), lastError: null, nextAt: ago(-1) }],
  [TLDR, { lastOk: ago(1), lastError: null, nextAt: ago(-1) }],
  [LETTERS, { lastOk: ago(1), lastError: null, nextAt: ago(-1) }],
])

// ── Stories ──────────────────────────────────────────────────────────────

function story(
  partial: Partial<Story> & Pick<Story, 'id' | 'sourceId' | 'title' | 'summary' | 'publishedAt'>,
): Story {
  return {
    key: `key-${partial.id}`,
    url: '',
    author: '',
    fetchedAt: partial.publishedAt,
    imageUrl: '',
    score: null,
    comments: null,
    discussionUrl: '',
    also: [],
    mailMessage: null,
    read: false,
    saved: false,
    hasBody: false,
    expired: false,
    ...partial,
  }
}

const hnItem = (n: number) => `https://news.ycombinator.com/item?id=${n}`

/** What a feed names for an episode: the file, how long, and nothing it did not say. */
function enclosure(partial: Partial<Enclosure> & Pick<Enclosure, 'url'>): Enclosure {
  return {
    mime: 'audio/mpeg',
    length: null,
    durationMs: null,
    transcriptUrl: '',
    transcriptMime: '',
    episode: null,
    season: null,
    ...partial,
  }
}

const MINUTE = 60_000
/** The file an episode was downloaded to, as the files folder would name it. */
const episodeFileOf = (id: StoryId): FileId => `file-ep-${id}`
/** The file an article was saved as audio to. */
const audioFileOf = (id: StoryId): FileId => `file-audio-${id}`

const stories: Story[] = [
  story({
    id: 'st-1',
    sourceId: LONGFORM,
    title: 'The last lighthouse keepers of the North Sea',
    url: 'https://longform.example/lighthouse-keepers',
    author: 'Ines Marlow',
    publishedAt: ago(3),
    summary:
      'For two centuries somebody lived at the top of every light. The last of them left in 1998, and the people who kept the stairs swept say they never quite stopped listening for the fog horn.',
    hasBody: true,
  }),
  story({
    id: 'st-2',
    sourceId: LONGFORM,
    title: 'Why the quiet places are getting louder',
    url: 'https://longform.example/quiet-places',
    author: 'Tomas Reyes',
    publishedAt: ago(26),
    summary:
      'A survey of forty libraries, four monasteries and one very strict bookshop asks what we mean when we ask for quiet, and who has been paying for it.',
    hasBody: true,
    // Already saved as audio, so a story with its own recording can be seen.
    files: { audio: audioFileOf('st-2') },
  }),
  story({
    id: 'st-3',
    sourceId: LONGFORM,
    title: 'A short history of the pencil sharpener',
    url: 'https://longform.example/pencil-sharpener',
    author: 'Ines Marlow',
    publishedAt: ago(24 * 3),
    summary:
      'The blade in the corner of the classroom desk was patented three times, argued over in two courts, and improved by a man who only ever wanted to draw.',
    read: true,
    saved: true,
    hasBody: true,
  }),
  story({
    id: 'st-4',
    sourceId: LONGFORM,
    title: 'What the tide tables knew',
    url: 'https://longform.example/tide-tables',
    author: 'Priya Anand',
    publishedAt: ago(24 * 6),
    summary:
      'Harbour masters kept their predictions in a locked drawer and a rhyme. Reading them against a century of satellite data, one oceanographer found they were wrong in exactly one way.',
    read: true,
  }),
  story({
    id: 'st-5',
    sourceId: SHIPYARD,
    title: 'Notes on a faster build',
    url: 'https://shipyard.example/faster-build',
    author: 'Mei Okafor',
    publishedAt: ago(5),
    summary:
      'We cut a cold build from eleven minutes to ninety seconds, and most of it was deleting things. A list of what we deleted, in the order it hurt.',
    hasBody: true,
  }),
  story({
    id: 'st-6',
    sourceId: SHIPYARD,
    title: 'Rewriting the sync loop without losing the plot',
    url: 'https://shipyard.example/sync-loop',
    author: 'Mei Okafor',
    publishedAt: ago(52),
    summary:
      'The old loop had three owners and no tests. We wrote the tests first, then wrote the loop to fit them, then threw away the loop and kept the tests.',
    also: [{ sourceId: HACKER, score: 412, comments: 180, discussionUrl: hnItem(41_200_001) }],
    hasBody: true,
  }),
  story({
    id: 'st-7',
    sourceId: SHIPYARD,
    title: 'Postmortem: the Tuesday that was not',
    url: 'https://shipyard.example/tuesday',
    author: 'Dara Lindqvist',
    publishedAt: ago(24 * 4),
    summary:
      'A clock that skipped a day, a cache that remembered it, and the eleven-line fix that took two days to believe.',
    read: true,
    hasBody: true,
  }),
  story({
    id: 'st-8',
    sourceId: SHIPYARD,
    title: 'Small files, big wins',
    url: 'https://shipyard.example/small-files',
    author: 'Dara Lindqvist',
    publishedAt: ago(24 * 8),
    summary:
      'Splitting one thousand-line module into forty small ones made reviews shorter, but only after we agreed what a module was for.',
  }),
  story({
    id: 'st-9',
    sourceId: HACKER,
    title: 'Show HN: A pocket-sized weather station that posts its own forecast',
    url: 'https://weatherpocket.example',
    author: 'barometer',
    publishedAt: ago(2),
    summary:
      'A weather station the size of a deck of cards. It reads pressure, guesses tomorrow, and says so.',
    score: 612,
    comments: 214,
    discussionUrl: hnItem(41_200_002),
  }),
  story({
    id: 'st-10',
    sourceId: HACKER,
    title: 'The case against calendar invites',
    url: 'https://meetingless.example/against-invites',
    author: 'quietmeeting',
    publishedAt: ago(7),
    summary:
      'Most invitations are requests dressed as facts. A plain-text alternative, and what it cost one team.',
    score: 388,
    comments: 142,
    discussionUrl: hnItem(41_200_003),
  }),
  story({
    id: 'st-11',
    sourceId: HACKER,
    title: 'Ask HN: What are you reading this autumn?',
    url: '',
    author: 'pageturner',
    publishedAt: ago(9),
    summary: 'Not what you think you should be reading. The thing you could not put down.',
    score: 156,
    comments: 301,
    discussionUrl: hnItem(41_200_004),
  }),
  story({
    id: 'st-12',
    sourceId: HACKER,
    title: 'A decade of running a one-person SaaS',
    url: 'https://solo.example/ten-years',
    author: 'onedev',
    publishedAt: ago(14),
    summary:
      'Ten years, one server, no employees. What changed, what never did, and the month it nearly ended.',
    score: 274,
    comments: 97,
    discussionUrl: hnItem(41_200_005),
    read: true,
  }),
  story({
    id: 'st-13',
    sourceId: HACKER,
    title: 'SQLite as an application file format, revisited',
    url: 'https://dbfiles.example/sqlite-format',
    author: 'wal',
    publishedAt: ago(30),
    summary:
      'Five years after the original argument, what held up, what did not, and the one footgun everybody found.',
    score: 520,
    comments: 170,
    discussionUrl: hnItem(41_200_006),
  }),
  story({
    id: 'st-14',
    sourceId: HACKER,
    title: 'Why is my laptop fan always on',
    url: 'https://fans.example/always-on',
    author: 'thermal',
    publishedAt: ago(40),
    summary: 'A tour of the five processes that were not doing what you thought they were doing.',
    score: 143,
    comments: 88,
    discussionUrl: hnItem(41_200_007),
  }),
  story({
    id: 'st-15',
    sourceId: RADIO,
    title: 'Valve radios and the sound of the 1950s',
    url: 'https://oldradio.example/valve-sound',
    author: 'Hal',
    publishedAt: ago(24 * 40),
    summary: 'Why a warm set sounds the way it does, and why the hum was part of the design.',
    read: true,
    hasBody: true,
  }),
  story({
    id: 'st-16',
    sourceId: RADIO,
    title: 'Fixing a cracked dial cord',
    url: 'https://oldradio.example/dial-cord',
    author: 'Hal',
    publishedAt: ago(24 * 41),
    summary: 'Fishing line, a drop of glue and an evening. The pulleys are the hard part.',
    read: true,
  }),
  // The Slow Hour. The two newest are downloaded, as its policy says.
  story({
    id: 'st-17',
    sourceId: SLOW,
    title: 'Episode 41: The people who wind the clocks',
    url: 'https://slowhour.example/41',
    author: 'Nadia Frost',
    publishedAt: ago(4),
    summary:
      'A cathedral clock, a ship’s chronometer and the retired watchmaker who still visits both on Sundays.',
    hasBody: true,
    media: enclosure({
      url: 'https://cdn.slowhour.example/ep41.mp3',
      length: 7_900_000,
      durationMs: 8 * MINUTE + 14_000,
      episode: 41,
      season: 3,
    }),
    files: { media: episodeFileOf('st-17') },
  }),
  story({
    id: 'st-18',
    sourceId: SLOW,
    title: 'Episode 40: Night trains',
    url: 'https://slowhour.example/40',
    author: 'Nadia Frost',
    publishedAt: ago(50),
    summary: 'Eight minutes on the last sleeper out of the city, and who is awake on it.',
    hasBody: true,
    media: enclosure({
      url: 'https://cdn.slowhour.example/ep40.mp3',
      length: 8_600_000,
      durationMs: 9 * MINUTE,
      episode: 40,
      season: 3,
    }),
    files: { media: episodeFileOf('st-18') },
  }),
  story({
    id: 'st-19',
    sourceId: SLOW,
    title: 'Episode 39: A listening walk',
    url: 'https://slowhour.example/39',
    author: 'Nadia Frost',
    publishedAt: ago(24 * 6),
    summary:
      'Seven minutes of a street, recorded on foot. What there is to hear when you stop talking.',
    hasBody: true,
    read: true,
    media: enclosure({
      url: 'https://cdn.slowhour.example/ep39.mp3',
      length: 6_700_000,
      durationMs: 7 * MINUTE,
      episode: 39,
      season: 3,
    }),
  }),
  story({
    id: 'st-20',
    sourceId: SLOW,
    title: 'Episode 38: What the sea archive holds',
    url: 'https://slowhour.example/38',
    author: 'Nadia Frost',
    publishedAt: ago(24 * 13),
    summary: 'A cellar of logbooks, a volunteer with a lamp, and a storm nobody wrote down.',
    hasBody: true,
    media: enclosure({
      url: 'https://cdn.slowhour.example/ep38.mp3',
      length: 9_300_000,
      durationMs: 9 * MINUTE + 45_000,
      episode: 38,
      season: 3,
      transcriptUrl: 'https://slowhour.example/38.vtt',
      transcriptMime: 'text/vtt',
    }),
  }),
  // Frame by Frame. One is on its way, one could not be had.
  story({
    id: 'st-21',
    sourceId: FRAME,
    title: 'Reading the room: a lighting primer',
    url: 'https://framebyframe.example/lighting',
    author: 'Okonkwo and Lee',
    publishedAt: ago(8),
    summary: 'Three lamps, one window and nine minutes on why a face looks kind or tired.',
    hasBody: true,
    media: enclosure({
      url: 'https://cdn.framebyframe.example/lighting.mp4',
      mime: 'video/mp4',
      length: 183_000_000,
      durationMs: 9 * MINUTE,
    }),
  }),
  story({
    id: 'st-22',
    sourceId: FRAME,
    title: 'Why the long take works',
    url: 'https://framebyframe.example/long-take',
    author: 'Okonkwo and Lee',
    publishedAt: ago(24 * 3),
    summary: 'A single shot of eight minutes, and what the cut would have taken from it.',
    hasBody: true,
    media: enclosure({
      url: 'https://cdn.framebyframe.example/long-take.mp4',
      mime: 'video/mp4',
      length: 164_000_000,
      durationMs: 8 * MINUTE + 30_000,
    }),
  }),
  // The newsletters found in Mail, each issue the message it is. All older than
  // the newest of everything above, so what a list opens on is as it was.
  story({
    id: 'st-23',
    sourceId: DOWNLOAD,
    title: 'The Download: the chip that learned to wait',
    summary:
      'A processor that idles on purpose, a small town’s argument with its own weather station, and the one number worth knowing today.',
    publishedAt: ago(11),
    mailMessage: 'msg-seed-23',
    hasBody: true,
  }),
  story({
    id: 'st-24',
    sourceId: DOWNLOAD,
    title: 'The Download: who gets to forecast the storm',
    summary:
      'Three agencies, two models and a very old rule of thumb, and what happens when they disagree about the same afternoon.',
    publishedAt: ago(35),
    mailMessage: 'msg-seed-24',
    hasBody: true,
  }),
  story({
    id: 'st-25',
    sourceId: DOWNLOAD,
    title: 'The Download: the grid’s long afternoon',
    summary:
      'Why the hardest hour for the grid is the one after the sun goes down, and the batteries built for exactly that.',
    publishedAt: ago(59),
    mailMessage: 'msg-seed-25',
    read: true,
    hasBody: true,
  }),
  story({
    id: 'st-26',
    sourceId: ALGORITHM,
    title: 'The Algorithm: when a model grades its own homework',
    summary:
      'Letting a system mark its own answers is quick and cheap, and wrong in a way that is hard to see from the inside.',
    publishedAt: ago(33),
    mailMessage: 'msg-seed-26',
    hasBody: true,
  }),
  story({
    id: 'st-27',
    sourceId: ALGORITHM,
    title: 'The Algorithm: small models, big errands',
    summary:
      'The models that run on a phone are doing more of the work than the ones in the news, and nobody is keeping score.',
    publishedAt: ago(24 * 8 + 1),
    mailMessage: 'msg-seed-27',
    read: true,
    hasBody: true,
  }),
  story({
    id: 'st-28',
    sourceId: TLDR,
    title: 'TLDR: the week’s launches, in five lines',
    summary:
      'A database that forgets on purpose, a terminal that draws, and three releases that did what they said.',
    publishedAt: ago(12),
    mailMessage: 'msg-seed-28',
    hasBody: true,
  }),
  story({
    id: 'st-29',
    sourceId: TLDR,
    title: 'TLDR: a compiler’s unlikely bug',
    summary: 'One flag, one platform, and eleven years of nobody noticing.',
    publishedAt: ago(36),
    mailMessage: 'msg-seed-29',
    hasBody: true,
  }),
  story({
    id: 'st-30',
    sourceId: TLDR,
    title: 'TLDR: three tools worth a look',
    summary: 'A diff viewer, a spreadsheet that shows its working, and a font for reading logs.',
    publishedAt: ago(60),
    mailMessage: 'msg-seed-30',
    read: true,
    hasBody: true,
  }),
]

// ── Bodies ───────────────────────────────────────────────────────────────

/** A soft gradient standing in for a picture, so a body has one to draw. */
function placeholderPicture(seed: string): string {
  let hash = 0
  for (const ch of seed) hash = (hash * 31 + ch.charCodeAt(0)) >>> 0
  const hue = hash % 360
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" width="640" height="320">` +
    `<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1">` +
    `<stop offset="0%" stop-color="hsl(${hue},55%,60%)"/>` +
    `<stop offset="100%" stop-color="hsl(${(hue + 50) % 360},50%,32%)"/>` +
    `</linearGradient></defs><rect width="640" height="320" fill="url(#g)"/></svg>`
  return `data:image/svg+xml;base64,${btoa(svg)}`
}

const PARAGRAPHS = [
  'It began, as these things do, with someone noticing a number that did not add up. Nobody was looking for a story; they were looking for the reason a column of figures ended three short of where it should.',
  'The people closest to it describe a feeling of slow recognition rather than discovery. One of them said it was like finding out a room in your house had always been there, behind the shelves.',
  'None of this was written down at the time. What survives is a handful of letters, a drawer of receipts, and the memory of a few people who are now rather old and rather certain.',
  'By the second winter the arrangement had settled into a routine, and routines are very good at hiding what they cost. Ask anyone who kept one.',
  'What would have been different, if they had stopped? That is the question the rest of this piece is a long way of declining to answer, and then answering.',
]

/** A body of `count` paragraphs, with a picture after the first. */
function articleBody(item: Story, count: number): string {
  const picture =
    `<figure><img src="${placeholderPicture(item.id)}" alt="" width="640" height="320">` +
    `<figcaption>Photograph: ${escapeHtml(item.author || 'the author')}</figcaption></figure>`
  const paragraphs = Array.from(
    { length: count },
    (_, i) => `<p>${PARAGRAPHS[i % PARAGRAPHS.length]}</p>`,
  )
  return `<p><strong>${escapeHtml(item.summary)}</strong></p>${picture}${paragraphs.join('')}`
}

/** An episode's body is its show notes, which carry no photograph. */
function showNotes(item: Story): string {
  return (
    `<p>${escapeHtml(item.summary)}</p><h3>In this episode</h3>` +
    `<ul><li>Opening, and what the week held</li><li>The main piece</li>` +
    `<li>Listeners’ letters</li></ul><p>Music by a friend of the show.</p>`
  )
}

const bodies = new Map<StoryId, string>()
/** Where a body came from, when it was not the feed: an extracted page, or a transcript. */
const bodyOrigins = new Map<StoryId, BodyOrigin>()
/** What an article's summary says, by story, once it is written: what its file holds. */
const summaries = new Map<StoryId, { html: string; markdown: string }>()
/** What the page an article was fetched from said about it: `StoryBody.byline` and `.siteName`. */
const abouts = new Map<StoryId, { byline: string | null; site: string | null }>()
for (const s of stories) {
  if (s.hasBody) bodies.set(s.id, s.media ? showNotes(s) : articleBody(s, 3))
}

function escapeHtml(text: string): string {
  return text
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
}

// ── Reading ──────────────────────────────────────────────────────────────

function find(id: StoryId): Story {
  const found = stories.find((s) => s.id === id)
  if (!found) throw new VaultError('notFound', 'no such story')
  return found
}

function sourceById(id: NewsSourceId): NewsSource {
  const found = sources.find((s) => s.id === id)
  if (!found) throw new VaultError('notFound', 'no such source')
  return found
}

function newestFirst(a: Story, b: Story): number {
  const byDate = Date.parse(b.publishedAt) - Date.parse(a.publishedAt)
  return byDate !== 0 ? byDate : b.id.localeCompare(a.id)
}

export function mockListNewsSources(): SourceSummary[] {
  return structuredClone(
    [...sources]
      .sort((a, b) => a.sortOrder - b.sortOrder)
      .map((s) => {
        const mine = stories.filter((x) => x.sourceId === s.id)
        const state = fetched.get(s.id)
        return {
          source: s,
          unread: mine.filter((x) => !x.read).length,
          total: mine.length,
          lastOk: state?.lastOk ?? null,
          lastError: state?.lastError ?? null,
          nextAt: state?.nextAt ?? null,
        }
      }),
  )
}

export function mockListStories(query: StoryQuery): StoryPage {
  ensureSeeded()
  const limit = Math.min(500, query.limit && query.limit > 0 ? query.limit : 50)
  const wanted = query.sources && query.sources.length > 0 ? new Set(query.sources) : null
  const since = query.since ? Date.parse(query.since) : null
  const before = query.before ?? null
  const rows = stories
    .filter((s) => {
      if (wanted && !wanted.has(s.sourceId)) return false
      if (query.unreadOnly && s.read) return false
      if (query.savedOnly && !s.saved) return false
      if (since !== null && Date.parse(s.publishedAt) < since) return false
      if (before) {
        const at = Date.parse(s.publishedAt)
        const cut = Date.parse(before.publishedAt)
        if (at > cut || (at === cut && s.id.localeCompare(before.id) >= 0)) return false
      }
      return true
    })
    .sort(newestFirst)
  const page = rows.slice(0, limit)
  const last = page.at(-1)
  return structuredClone({
    stories: page,
    next: rows.length > limit && last ? { publishedAt: last.publishedAt, id: last.id } : null,
  })
}

export function mockGetStory(id: StoryId): Story {
  ensureSeeded()
  return structuredClone(find(id))
}

export function mockMarkStoriesRead(ids: StoryId[], read: boolean): void {
  for (const id of ids) find(id).read = read
}

export function mockMarkAllStoriesRead(sourceIds: NewsSourceId[], upTo: string): StoryId[] {
  const wanted = sourceIds.length > 0 ? new Set(sourceIds) : null
  const cut = Date.parse(upTo)
  const marked: StoryId[] = []
  for (const s of stories) {
    if (s.read) continue
    // A newsletter's read state is its message's seen flag, which a sweep by
    // date does not touch; the interface marks those by id.
    if (s.mailMessage) continue
    if (wanted && !wanted.has(s.sourceId)) continue
    if (Date.parse(s.publishedAt) > cut) continue
    s.read = true
    marked.push(s.id)
  }
  return marked
}

export function mockSaveStories(ids: StoryId[], saved: boolean): void {
  for (const id of ids) find(id).saved = saved
}

/**
 * A story's body, and whether its pictures were held back -- the mock's
 * stand-in for `jatayu://news/body/{id}` and its `X-News-Images-Hidden`
 * header, neither of which a plain browser tab can answer.
 *
 * A story with no stored body is drawn from its summary, as the real route
 * does, so the reading pane is never blank for one that is only a teaser. The
 * origin is what the real document says in its head: where the body came from.
 *
 * `part` is the route's `?part=`: one document of the pane instead of the
 * whole body -- an episode's show notes or its transcript, or the summary of a
 * story. A part that does not exist yet answers not found, as the route does;
 * the pane only asks for one it knows is there.
 */
export function mockStoryBody(
  id: StoryId,
  part?: StoryPart | null,
): {
  html: string
  imagesHidden: boolean
  origin: BodyOrigin
  byline?: string | null
  site?: string | null
} {
  ensureSeeded()
  const s = find(id)
  if (part) return partOfBody(s, part)
  return {
    ...framed(
      s,
      bodies.get(id) ?? `<p>${escapeHtml(s.summary)}</p>`,
      bodyOrigins.get(id) ?? 'feed',
    ),
    ...abouts.get(id),
  }
}

/** `html` as the route serves it for `s`: with its pictures held back when its source says so. */
function framed(
  s: Story,
  html: string,
  origin: BodyOrigin,
): { html: string; imagesHidden: boolean; origin: BodyOrigin } {
  const from = sources.find((x) => x.id === s.sourceId)
  if (from && !from.images) {
    const hidden = /<img\b/i.test(html)
    html = html.replace(/<figure>[\s\S]*?<\/figure>/gi, '').replace(/<img\b[^>]*>/gi, '')
    return { html, imagesHidden: hidden, origin }
  }
  return { html, imagesHidden: false, origin }
}

/** One document of a story's pane: see `mockStoryBody`. */
function partOfBody(
  s: Story,
  part: StoryPart,
): { html: string; imagesHidden: boolean; origin: BodyOrigin } {
  switch (part) {
    case 'notes':
      // An episode's own show notes, whatever was written over its body since.
      return framed(
        s,
        s.media ? showNotes(s) : (bodies.get(s.id) ?? `<p>${escapeHtml(s.summary)}</p>`),
        'feed',
      )
    case 'transcript':
      if (!linkPresent(s.files?.transcript)) {
        throw new VaultError('notFound', 'This story has no transcript.')
      }
      return framed(s, transcriptHtml(s), 'feed')
    case 'summary': {
      // An episode's summary is the telling written from its transcript; an
      // article's is the file the summary was written to.
      if (s.media) {
        if (bodyOrigins.get(s.id) !== 'transcript') {
          throw new VaultError('notFound', 'This episode has not been written up.')
        }
        return framed(s, tellingOf(s), 'transcript')
      }
      const made = summaries.get(s.id)
      if (!made || !linkPresent(s.files?.summary)) {
        throw new VaultError('notFound', 'This story has no summary.')
      }
      return framed(s, made.html, 'feed')
    }
  }
}

export function mockFetchFullArticle(id: StoryId): Story {
  const s = find(id)
  if (!s.url) throw new VaultError('invalid', 'this story has no article to fetch')
  bodies.set(id, articleBody(s, 5))
  bodyOrigins.set(id, 'extracted')
  // What a publisher's head says: a name for the publication, and who wrote it.
  const name = hostOf(s.url).split('.')[0] ?? ''
  abouts.set(id, {
    byline: 'Morgan Ellis',
    site: name ? `The ${name.charAt(0).toUpperCase()}${name.slice(1)}` : null,
  })
  s.hasBody = true
  return structuredClone(s)
}

/** What the Library needs to write an Articles item for a story. */
export function mockStoryForLibrary(id: StoryId): {
  title: string
  url: string
  author: string
  summary: string
} {
  const s = find(id)
  return { title: s.title, url: s.url, author: s.author, summary: s.summary }
}

// ── Following ────────────────────────────────────────────────────────────

/** `shipyard.example` for `https://www.shipyard.example/x`. */
function hostOf(url: string): string {
  return new URL(url).host.replace(/^www\./, '')
}

function parseAddress(url: string): URL {
  try {
    const parsed = new URL(url.trim())
    if (parsed.protocol === 'http:' || parsed.protocol === 'https:') return parsed
  } catch {
    // Falls through to the same answer as a scheme that is not the web's.
  }
  throw new VaultError('invalid', 'That does not look like a web address.')
}

export function mockDiscoverFeeds(url: string): FoundFeed[] {
  const parsed = parseAddress(url)
  // An address that says so is how the unreachable state can be seen.
  if (/unreachable|offline/i.test(parsed.host)) {
    throw new VaultError('network', `Could not reach ${parsed.host}.`)
  }
  if (/nofeed/i.test(parsed.host)) return []
  // Already a feed: it is its own answer.
  if (/\.(xml|rss|atom)$|\/(feed|rss|atom)\/?$/i.test(parsed.pathname)) {
    return [{ url: parsed.href, title: hostOf(parsed.href) }]
  }
  const host = hostOf(parsed.href)
  return [
    { url: `${parsed.origin}/feed`, title: host },
    { url: `${parsed.origin}/comments/feed`, title: `${host} comments` },
  ]
}

const HN_TITLES: Record<HnList, string> = {
  front: 'Hacker News',
  show: 'Show HN',
  ask: 'Ask HN',
  top: 'Hacker News: top stories',
}

/** The record a `NewSource` becomes, before it is stored. */
function build(input: NewSource, order: number, now: string): NewsSource {
  const folder = input.folder?.trim() ?? ''
  switch (input.type) {
    case 'web': {
      const parsed = parseAddress(input.url)
      if (sources.some((s) => s.kind.type === 'web' && s.kind.url === parsed.href)) {
        throw new VaultError('invalid', 'You already follow that feed.')
      }
      return source({
        id: nextId('src'),
        kind: { type: 'web', url: parsed.href },
        title: input.title?.trim() || hostOf(parsed.href),
        folder,
        siteUrl: parsed.origin,
        sortOrder: order,
        createdAt: now,
        updatedAt: now,
      })
    }
    case 'podcast': {
      const parsed = parseAddress(input.url)
      if (sources.some((s) => s.kind.type === 'web' && s.kind.url === parsed.href)) {
        throw new VaultError('invalid', 'You already follow that feed.')
      }
      return source({
        id: nextId('src'),
        kind: { type: 'web', url: parsed.href },
        title: input.title?.trim() || hostOf(parsed.href),
        folder,
        siteUrl: parsed.origin,
        sortOrder: order,
        createdAt: now,
        updatedAt: now,
        // Followed from the directory: with what it said, and every setting
        // following the Feed's defaults.
        podcast: { author: input.author ?? '', directoryId: input.directoryId ?? null },
      })
    }
    case 'hackerNews': {
      const minPoints = input.minPoints ?? 0
      const followed = sources.some(
        (s) =>
          s.kind.type === 'hackerNews' &&
          s.kind.list === input.list &&
          s.kind.minPoints === minPoints,
      )
      if (followed) throw new VaultError('invalid', 'You already follow that list.')
      return source({
        id: nextId('src'),
        kind: { type: 'hackerNews', list: input.list, minPoints },
        title: HN_TITLES[input.list],
        folder,
        siteUrl: 'https://news.ycombinator.com',
        sortOrder: order,
        createdAt: now,
        updatedAt: now,
      })
    }
    case 'mail': {
      const sender = input.sender.trim().toLowerCase()
      if (!sender.includes('@')) throw new VaultError('invalid', 'That is not an email address.')
      // A newsletter the Newsletters source found is not followed: following
      // the sender by hand is a source of its own, taking everything it sends.
      if (sources.some((s) => s.kind.type === 'mail' && s.kind.sender === sender && !s.found)) {
        throw new VaultError('invalid', 'You already follow that sender.')
      }
      return source({
        id: nextId('src'),
        kind: { type: 'mail', sender },
        title: input.title?.trim() || NEWSLETTERS.find((n) => n.email === sender)?.name || sender,
        folder,
        // Newsletter pictures follow Mail's rule, which is to hold them back.
        images: false,
        sortOrder: order,
        createdAt: now,
        updatedAt: now,
      })
    }
    case 'newsletters': {
      if (sources.some((s) => s.kind.type === 'newsletters')) {
        throw new VaultError('invalid', 'You already follow your newsletters.')
      }
      return source({
        id: nextId('src'),
        kind: { type: 'newsletters' },
        title: 'Newsletters',
        folder,
        images: false,
        sortOrder: order,
        createdAt: now,
        updatedAt: now,
      })
    }
  }
}

export function mockAddNewsSource(input: NewSource): NewsSource {
  const order = Math.max(-1, ...sources.map((s) => s.sortOrder)) + 1
  const now = new Date().toISOString()
  const made = build(input, order, now)
  sources.push(made)
  fetched.set(made.id, { lastOk: null, lastError: null, nextAt: now })
  // Following a sender by hand takes it off the list of what is not a newsletter.
  if (made.kind.type === 'mail') {
    const sender = made.kind.sender
    const left = (settings.notNewsletters ?? []).filter((n) => n.sender !== sender)
    if (left.length > 0) settings.notNewsletters = left
    else delete settings.notNewsletters
  }
  return structuredClone(made)
}

export function mockSaveNewsSource(updated: NewsSource): void {
  const at = sources.findIndex((s) => s.id === updated.id)
  if (at < 0) throw new VaultError('notFound', 'no such source')
  // Whether a source was found is the service's to say, not something a save sets.
  const found = sources[at]!.found
  const next: NewsSource = { ...structuredClone(updated), updatedAt: new Date().toISOString() }
  if (found) next.found = true
  else delete next.found
  sources[at] = next
}

export function mockDeleteNewsSource(id: NewsSourceId): void {
  const gone = sourceById(id)
  const at = sources.findIndex((s) => s.id === id)
  sources.splice(at, 1)
  fetched.delete(id)
  // Unfollowing a newsletter that was found is saying it is not one, so the
  // next message from it does not bring it back.
  if (gone.found && gone.kind.type === 'mail') {
    const ref: NewsletterRef = { sender: gone.kind.sender }
    if (gone.kind.name) ref.name = gone.kind.name
    const listed = settings.notNewsletters ?? []
    const known = listed.some((n) => n.sender === ref.sender && (n.name ?? '') === (ref.name ?? ''))
    if (!known) settings.notNewsletters = [...listed, ref]
  }
  for (let i = stories.length - 1; i >= 0; i--) {
    const doomed = stories[i]!
    if (doomed.sourceId !== id) continue
    bodies.delete(doomed.id)
    bodyOrigins.delete(doomed.id)
    summaries.delete(doomed.id)
    stories.splice(i, 1)
  }
}

// ── Refreshing ───────────────────────────────────────────────────────────

/** What turns up next, by source, so a refresh has something to find. */
const FRESH: Record<NewsSourceId, { title: string; summary: string; author: string }[]> = {
  [LONGFORM]: [
    {
      title: 'The bell that rang itself',
      author: 'Priya Anand',
      summary:
        'A village bell that rang at no hour anyone had set, and the engineer who spent a year proving why.',
    },
    {
      title: 'On being the only reader left',
      author: 'Tomas Reyes',
      summary:
        'The last subscriber to a regional gazette, and the editor who kept printing for one.',
    },
  ],
  [SHIPYARD]: [
    {
      title: 'Deleting the staging environment',
      author: 'Mei Okafor',
      summary:
        'We switched it off for a month to see what broke. Mostly it was the habit of using it.',
    },
    {
      title: 'A week of only reading code',
      author: 'Dara Lindqvist',
      summary:
        'No pull requests, no features: five days of reading what we had written, and what we learned from it.',
    },
  ],
  [SLOW]: [
    {
      title: 'Episode 42: The lamplighter’s apprentice',
      author: 'Nadia Frost',
      summary: 'Nine minutes with the last person trained to light a street by hand.',
    },
    {
      title: 'Episode 43: A year of quiet mornings',
      author: 'Nadia Frost',
      summary: 'Listeners write in about the hour before everybody wakes.',
    },
  ],
  [FRAME]: [
    {
      title: 'The window as a light source',
      author: 'Okonkwo and Lee',
      summary: 'North light, south light and the curtain that fixes both.',
    },
  ],
  [DOWNLOAD]: [
    {
      title: 'The Download: a map of every unmapped road',
      author: '',
      summary:
        'A volunteer count of the roads no atlas has, and the ones that turned out to be rivers.',
    },
    {
      title: 'The Download: the battery that sleeps through winter',
      author: '',
      summary: 'Storing summer for January, one very large flask at a time.',
    },
  ],
  [ALGORITHM]: [
    {
      title: 'The Algorithm: the benchmark nobody trusts',
      author: '',
      summary:
        'A leaderboard that everyone cites and nobody believes, and what replaced it in one lab.',
    },
  ],
  [TLDR]: [
    {
      title: 'TLDR: a shell that explains itself',
      author: '',
      summary: 'Type a command and read, beside it, what it is about to do.',
    },
    {
      title: 'TLDR: the retry that made it worse',
      author: '',
      summary: 'A small outage, a helpful client, and the arithmetic of everybody asking again.',
    },
  ],
  [HACKER]: [
    {
      title: 'Show HN: I built a clock that only tells you the time when you ask twice',
      author: 'twice',
      summary: 'A deliberately slow clock, and what it did to one household.',
    },
    {
      title: 'The long road to a faster regular expression engine',
      author: 'automaton',
      summary: 'Backtracking, memoisation, and the afternoon the benchmark lied.',
    },
  ],
}

const freshCount = new Map<NewsSourceId, number>()

/** Add the next scripted story to a source, and say which. */
function addFresh(src: NewsSource, hoursAgo: number): Story {
  const n = freshCount.get(src.id) ?? 0
  freshCount.set(src.id, n + 1)
  const script = FRESH[src.id] ?? []
  const scripted = script[n % Math.max(1, script.length)]
  // Past the end of the script a title is reused with a number on it, so two
  // refreshes never put the same headline in the list twice.
  const round = script.length > 0 ? Math.floor(n / script.length) : n
  const line = scripted ?? {
    title: `A new dispatch from ${src.title}`,
    summary: `Something fresh from ${src.title}, found on the last pass.`,
    author: '',
  }
  const hn = src.kind.type === 'hackerNews'
  const id = nextId('story')
  const episode = src.podcast
    ? enclosure({
        url: `https://cdn.${hostOf(src.siteUrl || 'https://example.com')}/${id}.${src.id === FRAME ? 'mp4' : 'mp3'}`,
        mime: src.id === FRAME ? 'video/mp4' : 'audio/mpeg',
        length: src.id === FRAME ? 120_000_000 : 8_000_000,
        durationMs: (6 + (n % 4)) * MINUTE,
      })
    : null
  const made = story({
    id,
    sourceId: src.id,
    title: round > 0 ? `${line.title} (${round + 1})` : line.title,
    summary: line.summary,
    author: line.author,
    url: `https://${hostOf(src.siteUrl || 'https://example.com')}/${id}`,
    publishedAt: ago(hoursAgo),
    fetchedAt: new Date().toISOString(),
    score: hn ? 140 + n * 37 : null,
    comments: hn ? 40 + n * 11 : null,
    discussionUrl: hn ? hnItem(41_300_000 + seq) : '',
    // A newsletter issue points at the message it is.
    mailMessage: src.kind.type === 'mail' || src.kind.type === 'newsletters' ? nextId('msg') : null,
    media: episode,
    hasBody: !hn,
  })
  stories.push(made)
  // A story the judge has not seen yet.
  unranked.push(id)
  if (made.hasBody) bodies.set(id, made.media ? showNotes(made) : articleBody(made, 3))
  if (made.media && src.podcast) newEpisode(made, src.podcast.queueNew === true)
  // A source that summarises what it brings asks for it as the story arrives.
  if (!made.media && made.hasBody && src.automation?.summarize === true) {
    queueWork({ kind: 'summary', storyId: id, title: made.title })
  }
  return made
}

function refreshOne(src: NewsSource): RefreshReport {
  const state = fetched.get(src.id) ?? { lastOk: null, lastError: null, nextAt: null }
  fetched.set(src.id, state)
  const now = new Date().toISOString()
  // The Newsletters source stores nothing under itself: what it brings goes to
  // the source that claims each message, and those are refreshed as themselves.
  if (src.kind.type === 'newsletters') {
    state.lastOk = now
    state.nextAt = new Date(Date.now() + 30 * 60_000).toISOString()
    return { sourceId: src.id, inserted: 0, notModified: true, error: null }
  }
  if (src.id === RADIO) {
    // Still failing: the sidebar's dimmed dot stays, and the message with it.
    state.nextAt = new Date(Date.now() + 6 * HOUR).toISOString()
    return { sourceId: src.id, inserted: 0, notModified: false, error: state.lastError }
  }
  // A source nobody has fetched yet starts with a few, as a real first
  // fetch of a feed would; after that each pass finds one.
  const first = state.lastOk === null
  const count = first ? 3 : 1
  for (let i = 0; i < count; i++) addFresh(src, first ? 2 + i * 9 : 0.1)
  state.lastOk = now
  state.lastError = null
  state.nextAt = new Date(Date.now() + 30 * 60_000).toISOString()
  return { sourceId: src.id, inserted: count, notModified: false, error: null }
}

export function mockRefreshNews(id: NewsSourceId | null): RefreshReport[] {
  const targets = id ? [sourceById(id)] : sources.filter((s) => !s.paused)
  return targets.map(refreshOne)
}

// ── OPML ─────────────────────────────────────────────────────────────────

export function mockImportOpml(xml: string): OpmlImportReport {
  if (!/<opml\b/i.test(xml)) throw new VaultError('invalid', 'That is not an OPML file.')
  let added = 0
  let skipped = 0
  // The folder is whatever outline without an address encloses the one being
  // read, which is how every reader writes one.
  const folders: string[] = []
  const attr = (tag: string, name: string): string => {
    const match = new RegExp(`\\b${name}\\s*=\\s*"([^"]*)"`, 'i').exec(tag)
    return match?.[1]?.replaceAll('&amp;', '&') ?? ''
  }
  for (const [tag] of xml.matchAll(/<\/?outline\b[^>]*>/gi)) {
    if (tag.startsWith('</')) {
      folders.pop()
      continue
    }
    const selfClosing = tag.endsWith('/>')
    const address = attr(tag, 'xmlUrl')
    if (!address) {
      if (!selfClosing) folders.push(attr(tag, 'title') || attr(tag, 'text'))
      continue
    }
    // A closing tag only pairs with a folder, never with a feed written long-hand.
    if (!selfClosing) folders.push('')
    try {
      mockAddNewsSource({
        type: 'web',
        url: address,
        title: attr(tag, 'title') || attr(tag, 'text') || undefined,
        folder: [...folders].reverse().find((f) => f !== '') ?? '',
      })
      added += 1
    } catch {
      skipped += 1
    }
  }
  return { added, skipped }
}

export function mockExportOpml(): string {
  const web = sources.filter((s) => s.kind.type === 'web')
  const line = (s: NewsSource, indent: string) =>
    `${indent}<outline type="rss" text="${escapeHtml(s.title)}" title="${escapeHtml(s.title)}" xmlUrl="${escapeHtml(
      s.kind.type === 'web' ? s.kind.url : '',
    )}" htmlUrl="${escapeHtml(s.siteUrl)}"/>`
  const unfiled = web.filter((s) => !s.folder).map((s) => line(s, '    '))
  const folders = [...new Set(web.filter((s) => s.folder).map((s) => s.folder))].map((name) =>
    [
      `    <outline text="${escapeHtml(name)}" title="${escapeHtml(name)}">`,
      ...web.filter((s) => s.folder === name).map((s) => line(s, '      ')),
      '    </outline>',
    ].join('\n'),
  )
  return [
    '<?xml version="1.0" encoding="UTF-8"?>',
    '<opml version="2.0">',
    '  <head><title>Jatayu subscriptions</title></head>',
    '  <body>',
    ...unfiled,
    ...folders,
    '  </body>',
    '</opml>',
    '',
  ].join('\n')
}

// ── Newsletters ──────────────────────────────────────────────────────────

const NEWSLETTERS: Omit<NewsletterSender, 'followed'>[] = [
  { email: 'letters@longletter.example', name: 'The Long Letter', messages: 14 },
  { email: 'digest@oddbird.example', name: 'Odd Bird Digest', messages: 9 },
  { email: 'weekly@fieldnotes.example', name: 'Field Notes', messages: 5 },
]

export function mockNewsletterSenders(days: number): NewsletterSender[] {
  // A shorter window sees fewer of them, as a real count over fewer days does.
  const scale = Math.min(1, Math.max(0.1, days / 90))
  return NEWSLETTERS.map((n) => ({
    ...n,
    messages: Math.max(1, Math.round(n.messages * scale)),
    followed: sources.some((s) => s.kind.type === 'mail' && s.kind.sender === n.email),
  }))
}

// ── Podcasts, Up Next, and what was read and heard ───────────────────────
//
// Everything below answers the commands of `docs/plans/podcasts.md`. State is
// the module's own and is made when first asked for where it needs the files
// mock (the voice memo Up Next plays), so that loading this file does not
// build the catalog.

/** `story:<id>`, the way the service keys a thing. */
function refKey(what: ContentRef): string {
  return `${what.type}:${what.id}`
}

function sameRef(a: ContentRef, b: ContentRef): boolean {
  return a.type === b.type && a.id === b.id
}

/** A small stable number out of text, for what a mock has to make up. */
function hashOf(text: string): number {
  let hash = 0
  for (const ch of text) hash = (hash * 31 + ch.charCodeAt(0)) >>> 0
  return hash
}

/** A library item and the shelf it is on, for an Up Next entry that is one. */
export type MockItemLookup = (id: string) => { item: Item; shelf: string } | undefined

let itemLookup: MockItemLookup | null = null

/** `mock.ts` says where the library's items are; this file cannot import them. */
export function mockSetItemLookup(lookup: MockItemLookup): void {
  itemLookup = lookup
}

/** The episodes that have a file, and how long each is: what the player is given to play. */
const episodeFiles = new Map<FileId, { durationMs: number }>()
for (const s of stories) {
  if (s.files?.media) episodeFiles.set(s.files.media, { durationMs: s.media?.durationMs ?? 60_000 })
  if (s.files?.audio) episodeFiles.set(s.files.audio, { durationMs: audioLength(s) })
}

/** How long an article read aloud runs: a few minutes, a little differently for each. */
function audioLength(s: Story): number {
  return 4 * MINUTE + (hashOf(s.id) % 150) * 1000
}

// ── Playing ──────────────────────────────────────────────────────────────

/** Silent recordings by length in seconds, as the object addresses the browser made for them. */
const silences = new Map<number, string>()

/** A recording of nothing, `seconds` long, as a WAV the media element can play and seek. */
function silentWav(seconds: number): string {
  const kept = silences.get(seconds)
  if (kept !== undefined) return kept
  const rate = 8000
  const bytes = seconds * rate
  const wav = new Uint8Array(44 + bytes)
  const view = new DataView(wav.buffer)
  const text = (at: number, word: string) => {
    for (let i = 0; i < word.length; i++) wav[at + i] = word.charCodeAt(i)
  }
  text(0, 'RIFF')
  view.setUint32(4, 36 + bytes, true)
  text(8, 'WAVE')
  text(12, 'fmt ')
  view.setUint32(16, 16, true)
  view.setUint16(20, 1, true) // PCM
  view.setUint16(22, 1, true) // mono
  view.setUint32(24, rate, true)
  view.setUint32(28, rate, true) // bytes a second
  view.setUint16(32, 1, true) // bytes a sample, all channels
  view.setUint16(34, 8, true)
  text(36, 'data')
  view.setUint32(40, bytes, true)
  wav.fill(128, 44) // unsigned 8-bit silence
  // An object address and not a data one: a ten-minute recording is several
  // megabytes, which a `src` attribute should not have to carry.
  const url =
    typeof URL !== 'undefined' && typeof URL.createObjectURL === 'function'
      ? URL.createObjectURL(new Blob([wav], { type: 'audio/wav' }))
      : ''
  silences.set(seconds, url)
  return url
}

/**
 * Where the mock's player plays a file from: nothing audible, but as long as
 * the file says it is, so that seeking, progress and the end can all be tried.
 * `api.playUrl` asks for it in the mock instead of `fileUrl`, whose answer for
 * a recording is the picture Files draws it with.
 */
export function mockPlayUrl(id: string): string {
  const ms = episodeFiles.get(id)?.durationMs ?? mockCatalogFile(id)?.media.durationMs ?? 60_000
  // No more than twenty minutes: every second is eight kilobytes.
  return silentWav(Math.min(1200, Math.max(1, Math.ceil(ms / 1000))))
}

// ── Settings ─────────────────────────────────────────────────────────────

let settings: FeedSettings = {
  podcasts: {
    download: { type: 'newest', count: 1 },
    keep: { type: 'afterPlayed', days: 7 },
    queueNew: false,
    transcribe: 'whenPlayed',
  },
  ranking: {
    enabled: true,
    criteria: [
      { id: 'learn', text: 'Something I can learn from', weight: 2 },
      { id: 'sea', text: 'About the sea, ships or islands', weight: 1 },
      { id: 'interview', text: 'A long interview with somebody who builds things', weight: 2 },
      { id: 'gossip', text: 'Mostly gossip or celebrity news', weight: -3 },
    ],
    goals: true,
    windowDays: 14,
  },
  keepRead: true,
  autofill: null,
  autoplay: true,
  resumeAfterVoice: true,
  skipBackS: 15,
  skipForwardS: 30,
  cleanup: {
    feeds: { readDays: 30, unreadDays: 90 },
    newsletters: { readDays: 30, unreadDays: 90 },
  },
}

export function mockFeedSettings(): FeedSettings {
  return structuredClone(settings)
}

/** What the ranking asks and how much each answer counts: when it changes, every rank is stale. */
function fingerprint(s: FeedSettings): string {
  return JSON.stringify([s.ranking.enabled, s.ranking.criteria, s.ranking.goals])
}

/** A number of days as the service keeps it: whole, from one to a year. */
function wholeDays(days: number): number {
  return Math.min(365, Math.max(1, Math.round(days)))
}

function tidyRetention(r: Retention): Retention {
  return { readDays: wholeDays(r.readDays), unreadDays: wholeDays(r.unreadDays) }
}

export function mockSaveFeedSettings(next: FeedSettings): FeedSettings {
  const stale = fingerprint(next) !== fingerprint(settings)
  // Clean-up and the list of what is not a newsletter are kept when a save does
  // not carry them, as the service's own record keeps what it was not told.
  const cleanup = next.cleanup ?? settings.cleanup
  const notNewsletters = next.notNewsletters ?? settings.notNewsletters ?? []
  const saved: FeedSettings = {
    ...structuredClone(next),
    cleanup: {
      feeds: tidyRetention(cleanup.feeds),
      newsletters: tidyRetention(cleanup.newsletters),
    },
    ranking: {
      ...structuredClone(next.ranking),
      criteria: next.ranking.criteria.map((c) => ({
        ...c,
        weight: Math.min(3, Math.max(-3, Math.round(c.weight))),
      })),
    },
  }
  // An empty list is no list at all on the wire.
  if (notNewsletters.length > 0) saved.notNewsletters = structuredClone(notNewsletters)
  else delete saved.notNewsletters
  settings = saved
  if (stale) unranked = candidates().map((s) => s.id)
  return mockFeedSettings()
}

// ── What was read, heard and watched ─────────────────────────────────────

const consumptions = new Map<string, Consumption>()

function seedConsumption(
  what: ContentRef,
  title: string,
  source: string,
  mode: ConsumeMode,
  more: Partial<Consumption>,
): void {
  consumptions.set(refKey(what), {
    id: nextId('cons'),
    what,
    title,
    source,
    url: '',
    mode,
    firstAt: ago(30),
    lastAt: ago(30),
    progress: 0,
    finished: false,
    ...more,
  })
}

// Half-way through an episode, one heard to the end, and an article read.
seedConsumption(
  { type: 'story', id: 'st-18' },
  'Episode 40: Night trains',
  'The Slow Hour',
  'listen',
  {
    lastAt: ago(20),
    progress: 0.33,
    positionMs: 3 * MINUTE,
    durationMs: 9 * MINUTE,
    heard: [{ startMs: 0, endMs: 3 * MINUTE }],
    url: 'https://slowhour.example/40',
    mediaFile: episodeFileOf('st-18'),
  },
)
seedConsumption(
  { type: 'story', id: 'st-19' },
  'Episode 39: A listening walk',
  'The Slow Hour',
  'listen',
  {
    firstAt: ago(24 * 5),
    lastAt: ago(24 * 4),
    progress: 1,
    finished: true,
    finishedAt: ago(24 * 4),
    positionMs: 7 * MINUTE,
    durationMs: 7 * MINUTE,
    heard: [{ startMs: 0, endMs: 7 * MINUTE }],
    url: 'https://slowhour.example/39',
    mediaFile: episodeFileOf('st-19'),
  },
)
seedConsumption(
  { type: 'story', id: 'st-3' },
  'A short history of the pencil sharpener',
  'Longform Weekly',
  'read',
  {
    firstAt: ago(24 * 3),
    lastAt: ago(24 * 3),
    progress: 1,
    finished: true,
    finishedAt: ago(24 * 3),
    url: 'https://longform.example/pencil-sharpener',
  },
)

/** How much of `duration` the spans cover, 0 to 1. */
function covered(spans: Span[], duration: number): number {
  if (duration <= 0) return 0
  const heard = spans.reduce((n, s) => n + Math.max(0, Math.min(s.endMs, duration) - s.startMs), 0)
  return Math.min(1, heard / duration)
}

/** `spans` with `add` folded in: sorted, and every two that touch made one. */
function mergeSpans(spans: Span[], add: Span): Span[] {
  const all = [...spans, add]
    .filter((s) => s.endMs > s.startMs)
    .sort((a, b) => a.startMs - b.startMs)
  const out: Span[] = []
  for (const span of all) {
    const last = out.at(-1)
    if (last && span.startMs <= last.endMs + 500) last.endMs = Math.max(last.endMs, span.endMs)
    else out.push({ ...span })
  }
  return out
}

/** What the service puts on a record about a thing: its title, where it is from, and its address. */
function describe(what: ContentRef): {
  title: string
  source: string
  url: string
  mode: ConsumeMode
  durationMs: number | null
} {
  if (what.type === 'story') {
    const s = find(what.id)
    const video = s.media?.mime.startsWith('video') ?? false
    return {
      title: s.title,
      source: sources.find((x) => x.id === s.sourceId)?.title ?? '',
      url: s.url,
      mode: s.media ? (video ? 'watch' : 'listen') : 'read',
      durationMs: s.media?.durationMs ?? null,
    }
  }
  if (what.type === 'file') {
    const f = mockCatalogFile(what.id)
    if (!f) throw new VaultError('not_found', 'That file is no longer in Files.')
    const folder = f.storage.kind === 'plain' ? f.storage.path.split('/').slice(0, -1).at(-1) : ''
    return {
      title: f.name,
      source: folder || 'Files',
      url: '',
      mode: f.kind === 'video' ? 'watch' : f.kind === 'audio' ? 'listen' : 'read',
      durationMs: f.media.durationMs ?? null,
    }
  }
  const found = itemLookup?.(what.id)
  if (!found) throw new VaultError('not_found', 'That item is no longer in the Library.')
  return {
    title: found.item.title,
    source: found.shelf,
    url: '',
    mode: 'read',
    durationMs: null,
  }
}

export function mockRecordProgress(p: ProgressReport): Consumption {
  ensureQueue()
  const key = refKey(p.what)
  const now = new Date().toISOString()
  const before = consumptions.get(key)
  const c: Consumption =
    before ??
    (() => {
      const d = describe(p.what)
      return {
        id: nextId('cons'),
        what: p.what,
        title: d.title,
        source: d.source,
        url: d.url,
        mode: p.mode,
        firstAt: now,
        lastAt: now,
        progress: 0,
        finished: false,
      }
    })()
  c.lastAt = now
  c.mode = p.mode
  if (p.durationMs != null) c.durationMs = p.durationMs
  if (p.positionMs != null) c.positionMs = p.positionMs
  if (p.heard) c.heard = mergeSpans(c.heard ?? [], p.heard)
  if (p.progress != null) {
    c.progress = Math.min(1, Math.max(0, p.progress))
  } else if (c.durationMs) {
    // What was heard, or how far along it is when nothing says what was.
    const along = c.heard?.length
      ? covered(c.heard, c.durationMs)
      : (c.positionMs ?? 0) / c.durationMs
    c.progress = Math.min(1, Math.max(c.progress, along))
  }
  // Nine tenths is the end, as the service counts it; a button says so too.
  const finished = p.finished ?? (c.finished || c.progress >= 0.9)
  if (finished && !c.finished) c.finishedAt = now
  if (!finished) c.finishedAt = null
  c.finished = finished
  if (p.finished === true) c.progress = 1
  consumptions.set(key, c)
  if (finished) {
    // Finishing takes it out of Up Next, and out of the unread.
    removeFromQueue((e) => sameRef(e.what, p.what))
    if (p.what.type === 'story') {
      const s = stories.find((x) => x.id === p.what.id)
      if (s) s.read = true
    }
  }
  return structuredClone(c)
}

export function mockGetConsumption(what: ContentRef): Consumption | null {
  ensureSeeded()
  const c = consumptions.get(refKey(what))
  return c ? structuredClone(c) : null
}

export function mockListConsumed(query: ConsumedQuery): Consumption[] {
  ensureSeeded()
  const since = query.since ? Date.parse(query.since) : null
  const until = query.until ? Date.parse(query.until) : null
  const limit = Math.min(500, query.limit && query.limit > 0 ? query.limit : 50)
  return structuredClone(
    [...consumptions.values()]
      .filter((c) => {
        const at = Date.parse(c.lastAt)
        if (since !== null && at < since) return false
        if (until !== null && at >= until) return false
        return !query.finishedOnly || c.finished
      })
      .sort((a, b) => Date.parse(b.lastAt) - Date.parse(a.lastAt))
      .slice(0, limit),
  )
}

export function mockForgetConsumption(id: string): void {
  for (const [key, c] of consumptions) if (c.id === id) consumptions.delete(key)
}

/** A kept article, as the Feed keeps what has been read: Markdown under `Feed/Articles/<source>/`. */
function keptArticle(title: string, source: string, summary: string): FileId {
  const text = [`# ${title}`, '', `> ${summary}`, '', ...PARAGRAPHS.flatMap((p) => [p, ''])]
  return mockTextFile(
    `${nameFrom(title)}.md`,
    text.join('\n'),
    `Feed/Articles/${nameFrom(source)}`,
    'feed',
  ).id
}

/**
 * Two weeks of reading, listening and watching, for History to draw: read and
 * heard stories (some still in the Feed, some long gone from it, which a record
 * outlives), a few of them with the words kept, a recording and a document from
 * Files, and a book from the Library. Every record that names a story in the
 * Feed names one that is read already, so Recommended and Up Next are as they
 * were without it.
 */
function seedHistory(): void {
  const back = (days: number, extra = 0) => ago(days * 24 + extra)
  const read = (id: StoryId, at: string, more: Partial<Consumption>) => {
    const s = find(id)
    const from = sources.find((x) => x.id === s.sourceId)?.title ?? ''
    seedConsumption({ type: 'story', id }, s.title, from, 'read', {
      firstAt: at,
      lastAt: at,
      url: s.url,
      ...more,
    })
  }
  const gone = (
    id: string,
    title: string,
    from: string,
    mode: ConsumeMode,
    at: string,
    more: Partial<Consumption>,
  ) => {
    const host = from.toLowerCase().replace(/[^a-z]+/g, '')
    seedConsumption({ type: 'story', id }, title, from, mode, {
      firstAt: at,
      lastAt: at,
      url: `https://${host}.example/${id}`,
      ...more,
    })
  }
  const finished = (at: string): Partial<Consumption> => ({
    progress: 1,
    finished: true,
    finishedAt: at,
  })

  read('st-7', ago(1.5), finished(ago(1.5)))
  read('st-4', ago(5), { progress: 0.6 })
  const saas = find('st-12')
  read('st-12', ago(27), {
    ...finished(ago(27)),
    textFile: keptArticle(saas.title, 'Hacker News', saas.summary),
  })
  read('st-15', back(2, 3), finished(back(2, 3)))
  read('st-16', back(4, 2), { progress: 0.3 })

  const ferry = 'The ferry that never came'
  gone('ferry-that-never-came', ferry, 'Longform Weekly', 'read', back(5, 1), {
    ...finished(back(5, 1)),
    textFile: keptArticle(
      ferry,
      'Longform Weekly',
      'A timetable, a harbour and the people who waited for a boat cancelled years before.',
    ),
  })
  gone('lens-choices', 'Lens choices on a budget', 'Frame by Frame', 'watch', back(6, 4), {
    progress: 0.45,
    positionMs: 4 * MINUTE,
    durationMs: 9 * MINUTE,
    heard: [{ startMs: 0, endMs: 4 * MINUTE }],
  })
  const show = 'The Slow Hour'
  gone('harbour-at-night', 'Episode 36: The harbour at night', show, 'listen', back(8, 2), {
    ...finished(back(8, 2)),
    positionMs: 8 * MINUTE,
    durationMs: 8 * MINUTE,
    heard: [{ startMs: 0, endMs: 8 * MINUTE }],
    mediaFile: 'file-ep-harbour-at-night',
  })
  gone('tide-tables-aloud', 'Episode 35: Tide tables, read aloud', show, 'listen', back(9), {
    progress: 0.7,
    positionMs: 6 * MINUTE,
    durationMs: 9 * MINUTE,
    heard: [{ startMs: 0, endMs: 6 * MINUTE }],
    mediaFile: 'file-ep-tide-tables-aloud',
  })
  gone('economics-of-quiet', 'On the economics of quiet', 'Longform Weekly', 'read', back(11, 6), {
    progress: 0.8,
  })
  gone('small-wins', 'Small wins, written down', 'Shipyard Notes', 'read', back(12, 1), {
    ...finished(back(12, 1)),
  })
  gone('a-kinder-cache', 'A kinder cache', 'Shipyard Notes', 'read', back(13, 5), { progress: 0.2 })

  // The files the catalog holds: a document read in the reader, a film watched
  // in the player. A record names its file and no more, as the service's does.
  const found = (query: Record<string, unknown>) =>
    (mockFilesInvoke('list_files', { query }) as FilePage).files[0]
  const doc = found({ kinds: ['document'], text: 'reading-list' })
  if (doc) {
    seedConsumption({ type: 'file', id: doc.id }, doc.name, 'Documents', 'read', {
      firstAt: back(7, 2),
      lastAt: back(7, 2),
      ...finished(back(7, 2)),
    })
  }
  const film = found({ kinds: ['video'] })
  if (film) {
    seedConsumption({ type: 'file', id: film.id }, film.name, 'Videos', 'watch', {
      firstAt: back(10),
      lastAt: back(10),
      progress: 0.2,
      positionMs: 24_000,
      durationMs: film.media.durationMs ?? 120_000,
      heard: [{ startMs: 0, endMs: 24_000 }],
      mediaFile: film.id,
    })
  }
  const book = itemLookup?.('i-dune')
  if (book) {
    seedConsumption({ type: 'item', id: book.item.id }, book.item.title, book.shelf, 'read', {
      firstAt: back(3, 6),
      lastAt: back(3, 6),
      progress: 0.4,
    })
  }
}

// ── Downloads ────────────────────────────────────────────────────────────

interface Fetching {
  state: DownloadState
  received: number
  total: number
  error: string | null
}

const fetching = new Map<StoryId, Fetching>()

// One is coming down and one could not be had, so both can be seen.
fetching.set('st-21', {
  state: 'downloading',
  received: 48_000_000,
  total: 183_000_000,
  error: null,
})
fetching.set('st-22', {
  state: 'failed',
  received: 0,
  total: 164_000_000,
  error: 'The server answered 404 Not Found.',
})

function waitingTurn(): void {
  const busy = [...fetching.values()].some((f) => f.state === 'downloading')
  if (busy) return
  const next = [...fetching.values()].find((f) => f.state === 'waiting')
  if (next) next.state = 'downloading'
}

/** Ask for an episode's file: now if nothing else is coming down, else in turn. */
function requestDownload(id: StoryId): void {
  const s = find(id)
  if (!s.media) throw new VaultError('invalid', 'That story has no episode to download.')
  if (s.files?.media) return
  const known = fetching.get(id)
  if (known && known.state !== 'failed') return
  fetching.set(id, {
    state: 'waiting',
    received: 0,
    total: s.media.length ?? 40_000_000,
    error: null,
  })
  waitingTurn()
}

/** The file is there: the story links it, and the player can play it. */
function arrived(id: StoryId): void {
  const s = find(id)
  fetching.delete(id)
  s.files = { ...s.files, media: episodeFileOf(id) }
  episodeFiles.set(episodeFileOf(id), { durationMs: s.media?.durationMs ?? 60_000 })
  waitingTurn()
}

export function mockDownloadEpisode(id: StoryId): void {
  requestDownload(id)
}

export function mockCancelDownload(id: StoryId): void {
  fetching.delete(id)
  waitingTurn()
}

export function mockRemoveEpisodeDownload(id: StoryId): void {
  const s = find(id)
  const file = s.files?.media
  if (file) episodeFiles.delete(file)
  s.files = { ...s.files, media: null }
}

/** What is waiting, coming down or failed. Asking moves what is coming down along, as a clock would. */
export function mockListDownloads(): DownloadStatus[] {
  for (const [id, f] of [...fetching]) {
    if (f.state !== 'downloading') continue
    f.received = Math.min(f.total, f.received + Math.max(1_000_000, Math.round(f.total * 0.22)))
    if (f.received >= f.total) arrived(id)
  }
  return [...fetching].map(([id, f]) => ({
    storyId: id,
    title: find(id).title,
    state: f.state,
    received: f.received,
    total: f.total,
    ...(f.error ? { error: f.error } : {}),
  }))
}

/** A refresh found an episode: shows that say so put it in Up Next, and the policy fetches it. */
function newEpisode(made: Story, queueNew: boolean): void {
  const src = sources.find((s) => s.id === made.sourceId)
  const policy = src?.podcast?.download ?? settings.podcasts.download
  if (policy.type !== 'off') requestDownload(made.id)
  if (queueNew) {
    ensureQueue()
    queue.push(makeEntry({ type: 'story', id: made.id }, 'show'))
    renumber()
  }
}

// ── Transcripts, articles and audio ──────────────────────────────────────
//
// Three kinds of background work, done one at a time, a step each time the list
// of work is asked for -- as downloads are, and for the same reason: a timer
// would hold a test process open, and a person watching a button wants to see
// it move when they look. What a finished piece makes is as the service makes
// it: a Markdown transcript in the files folder beside the recording, linked
// from the story (`files.transcript`) or from the file's own properties
// (`feed.transcript`); an article that replaces the body, with the show notes
// kept under it; and an audio file the player can play.

/** What a made-up episode says, a paragraph at a time. */
const SPOKEN = [
  'Welcome back. This one is a short one, and it begins somewhere cold.',
  'I want to start with a sound, if you will let me: a door that has not been opened in some years, and the hush that comes in after it.',
  'The first thing anybody tells you is that it was quieter than they expected. The second is that they were wrong about why.',
  'Somebody had kept the logbooks, which is the only reason we know any of this. Page after page, in the same patient hand.',
  'If you take one thing from this, take the part about the lamp. We will come back to the lamp.',
  'That is all for this week. Thank you for listening, and for the letters, which I read, every one.',
]

/** `[hh:mm:ss]`, the way a transcript marks where a paragraph begins. */
function clockMark(ms: number): string {
  const total = Math.floor(ms / 1000)
  const two = (n: number) => String(n).padStart(2, '0')
  const hours = Math.floor(total / 3600)
  return `[${two(hours)}:${two(Math.floor((total % 3600) / 60))}:${two(total % 60)}]`
}

/** A recording as Markdown: a heading, and each paragraph with the time it begins. */
function spokenText(title: string, durationMs: number, byline: string): string {
  const step = Math.max(1000, Math.floor(durationMs / SPOKEN.length))
  const paragraphs = SPOKEN.map((line, i) => `${clockMark(i * step)} ${line}`)
  return [
    `# ${title}`,
    '',
    ...(byline ? [`_${byline}_`, ''] : []),
    ...paragraphs.flatMap((p) => [p, '']),
  ].join('\n')
}

/** A file's name without its extension. */
function stemOf(name: string): string {
  return name.replace(/\.[^./]+$/, '')
}

/** What a file or a folder is called when it is made out of a title: nothing a path cannot hold. */
function nameFrom(title: string): string {
  return title.replace(/[\\/]/g, '-').trim() || 'Untitled'
}

/** An episode's transcript, written beside where its file is kept. */
function writeStoryTranscript(s: Story): FileEntry {
  const show = sources.find((x) => x.id === s.sourceId)?.title ?? ''
  const length = s.media?.durationMs ?? 8 * MINUTE
  return mockTextFile(
    `${nameFrom(s.title)} transcript.md`,
    spokenText(s.title, length, show ? `${show}. Transcript.` : ''),
    `Feed/Podcasts/${nameFrom(show || 'Podcasts')}`,
    'feed',
  )
}

/**
 * A recording in Files, written down: the transcript beside it, the recording
 * pointing at it (`feed.transcript`) and the transcript at the recording
 * (`feed.transcript_of`).
 */
function writeFileTranscript(f: FileEntry): FileEntry {
  const dir = f.storage.kind === 'plain' ? f.storage.path.split('/').slice(0, -1).join('/') : ''
  const stem = stemOf(f.name)
  const origin: FileOrigin = f.origin === 'feed' ? 'feed' : 'files'
  const made = mockTextFile(
    `${stem} transcript.md`,
    spokenText(stem, f.media.durationMs ?? 90_000, 'Transcript.'),
    dir,
    origin,
  )
  const text = (value: string) => ({ type: 'text', value })
  mockFilesInvoke('update_files', {
    ids: [f.id],
    update: { properties: { 'feed.transcript': text(made.id) } },
  })
  mockFilesInvoke('update_files', {
    ids: [made.id],
    update: { properties: { 'feed.transcript_of': text(f.id) } },
  })
  return made
}

/** What an episode becomes as an article: the telling alone, which is its summary. */
function tellingOf(s: Story): string {
  const told = [
    'The episode opens on a sound -- a door that has not been opened in years -- and spends the next few minutes on why nobody expected the quiet that followed it.',
    'Those closest to the story say they were wrong about the cause. The logbooks, kept by one patient hand, say otherwise, and the host reads from them at length.',
    'The part to hold on to is the lamp. It comes up three times, and each time it means something a little different.',
  ]
  const paragraphs = told.map((p) => `<p>${p}</p>`).join('')
  return `<p><strong>${escapeHtml(s.summary)}</strong></p>${paragraphs}`
}

/** What an episode becomes as the body: the telling, and the show notes kept under it. */
function articleFromTranscript(s: Story): string {
  return `${tellingOf(s)}<hr><h3>Show notes</h3>${showNotes(s)}`
}

/** An episode's transcript as the route draws it: each paragraph with the time it begins. */
function transcriptHtml(s: Story): string {
  const length = s.media?.durationMs ?? 8 * MINUTE
  const step = Math.max(1000, Math.floor(length / SPOKEN.length))
  const paragraphs = SPOKEN.map(
    (line, i) => `<p><strong>${clockMark(i * step)}</strong> ${line}</p>`,
  )
  return paragraphs.join('')
}

/** What a summary of an article says: the gist, and what to take from it. */
function summaryOf(s: Story): { html: string; markdown: string } {
  const points = [
    'The question that started it was a small one, and the answer was not.',
    'Most of what survives is paperwork; the people who remember are few.',
    'What would have changed, had they stopped, is the part left open.',
  ]
  const items = points.map((p) => `<li>${p}</li>`).join('')
  return {
    html: `<p>${escapeHtml(s.summary)}</p><ul>${items}</ul>`,
    markdown: [`# ${s.title}`, '', s.summary, '', ...points.map((p) => `- ${p}`), ''].join('\n'),
  }
}

/**
 * Whether a file the Feed links to -- a transcript, an audio version -- is
 * there: in the catalog and not in the trash, or one of the files this mock
 * makes up for episodes and audio, which no catalog holds.
 */
function linkPresent(id: FileId | null | undefined): boolean {
  if (!id) return false
  const found = mockCatalogFile(id)
  return found ? found.status.state === 'present' : episodeFiles.has(id)
}

/**
 * What `get_file` answers for a file the mock makes up -- an episode's, or an
 * article's audio -- so the interface can ask whether it is there like any
 * other. `null` for a file the catalog holds, which answers for itself.
 */
export function mockFeedFile(id: FileId): FileDetail | null {
  if (mockCatalogFile(id) || !episodeFiles.has(id)) return null
  const at = new Date().toISOString()
  const name = `${id}.mp3`
  const length = episodeFiles.get(id)?.durationMs
  return {
    file: {
      id,
      storage: { kind: 'plain', path: `Feed/Podcasts/${name}` },
      name,
      kind: 'audio',
      mime: 'audio/mpeg',
      size: 4_000_000,
      hash: '',
      modifiedAt: at,
      addedAt: at,
      updatedAt: at,
      origin: 'feed',
      status: { state: 'present' },
      media: length === undefined ? {} : { durationMs: length },
      caption: '',
      favourite: false,
      hidden: false,
    },
    uses: [],
    albums: [],
  }
}

/** The work asked for and not yet over. */
interface MockWork {
  kind: WorkItem['kind']
  storyId?: StoryId
  fileId?: FileId
  title: string
  /** How many times the list has been asked since it began. */
  step: number
  steps: number
  /** Why it could not be done; set once it has failed, and left until it is asked for again. */
  error: string | null
}

const WORK_STEPS: Record<WorkItem['kind'], number> = {
  transcript: 4,
  article: 3,
  audio: 3,
  summary: 3,
}

const workQueue: MockWork[] = []

/** Ask for work: nothing if it is already waiting or going, and again if it had failed. */
function queueWork(job: Pick<MockWork, 'kind' | 'storyId' | 'fileId' | 'title'>): void {
  const known = workQueue.findIndex(
    (w) => w.kind === job.kind && w.storyId === job.storyId && w.fileId === job.fileId,
  )
  if (known >= 0) {
    if (!workQueue[known]!.error) return
    workQueue.splice(known, 1)
  }
  workQueue.push({ ...job, step: 0, steps: WORK_STEPS[job.kind], error: null })
}

export function mockTranscribeEpisode(id: StoryId): void {
  const s = find(id)
  if (!s.media) throw new VaultError('invalid', 'That story is not an episode.')
  if (linkPresent(s.files?.transcript)) return
  queueWork({ kind: 'transcript', storyId: id, title: s.title })
}

export function mockTranscribeFile(id: FileId): void {
  const f = mockCatalogFile(id)
  if (!f) throw new VaultError('not_found', 'That file is no longer in Files.')
  if (f.kind !== 'audio' && f.kind !== 'video') {
    throw new VaultError('invalid', 'Only recordings and films can be transcribed.')
  }
  const written = f.properties?.['feed.transcript']
  if (written?.type === 'text' && linkPresent(written.value)) return
  queueWork({ kind: 'transcript', fileId: id, title: f.name })
}

export function mockWriteArticle(id: StoryId): void {
  const s = find(id)
  if (!linkPresent(s.files?.transcript)) {
    throw new VaultError('invalid', 'An article is written from a transcript, and this has none.')
  }
  queueWork({ kind: 'article', storyId: id, title: s.title })
}

/** Summarise an article. An episode's summary is its article, which `write_article` makes. */
export function mockSummarizeStory(id: StoryId): void {
  const s = find(id)
  if (s.media) throw new VaultError('invalid', 'An episode is summarised by writing its article.')
  if (s.expired) throw new VaultError('invalid', 'This story is past its age: there is no text.')
  if (linkPresent(s.files?.summary)) return
  queueWork({ kind: 'summary', storyId: id, title: s.title })
}

export function mockRenderStoryAudio(id: StoryId): void {
  const s = find(id)
  if (s.media) throw new VaultError('invalid', 'An episode is audio already.')
  if (s.expired) throw new VaultError('invalid', 'This story is past its age: there is no text.')
  if (linkPresent(s.files?.audio)) return
  queueWork({ kind: 'audio', storyId: id, title: s.title })
}

/** Why a piece of work cannot be done, or `null` when it can. */
function unworkable(w: MockWork): string | null {
  if (!w.storyId) return null
  const s = find(w.storyId)
  // A story that is only a headline has nothing to summarise until its article is fetched.
  if (w.kind === 'summary') {
    return s.hasBody
      ? null
      : 'There is no article text to summarise yet. Fetch the full article first.'
  }
  if (w.kind !== 'transcript') return null
  // A show that publishes its transcript needs no file; anything else is listened to.
  return s.files?.media || s.media?.transcriptUrl
    ? null
    : 'The episode has not been downloaded, and the show publishes no transcript.'
}

/** What a finished piece of work leaves behind. */
function finishWork(w: MockWork): void {
  if (w.kind === 'transcript') {
    if (w.storyId) {
      const s = find(w.storyId)
      s.files = { ...s.files, transcript: writeStoryTranscript(s).id }
    } else if (w.fileId) {
      const f = mockCatalogFile(w.fileId)
      if (f) writeFileTranscript(f)
    }
  } else if (w.kind === 'article' && w.storyId) {
    const s = find(w.storyId)
    bodies.set(s.id, articleFromTranscript(s))
    bodyOrigins.set(s.id, 'transcript')
    s.hasBody = true
  } else if (w.kind === 'summary' && w.storyId) {
    const s = find(w.storyId)
    const show = sources.find((x) => x.id === s.sourceId)?.title ?? ''
    const made = summaryOf(s)
    const day = new Date().toISOString().slice(0, 10)
    const file = mockTextFile(
      `${day} ${nameFrom(s.title)}.md`,
      made.markdown,
      `Feed/Summaries/${nameFrom(show || 'Feed')}`,
      'feed',
    )
    summaries.set(s.id, made)
    s.files = { ...s.files, summary: file.id }
  } else if (w.kind === 'audio' && w.storyId) {
    const s = find(w.storyId)
    const file = audioFileOf(s.id)
    episodeFiles.set(file, { durationMs: audioLength(s) })
    s.files = { ...s.files, audio: file }
  }
}

/**
 * The work waiting, going or failed. Asking moves the one that is going along
 * a step, as a clock would, and the next waits its turn.
 */
export function mockListeningWork(): WorkItem[] {
  const going = workQueue.find((w) => !w.error)
  if (going) {
    going.step += 1
    const reason = unworkable(going)
    if (reason) {
      going.error = reason
    } else if (going.step >= going.steps) {
      workQueue.splice(workQueue.indexOf(going), 1)
      finishWork(going)
    }
  }
  const now = workQueue.find((w) => !w.error)
  return workQueue.map((w): WorkItem => {
    const state = w.error ? 'failed' : w === now ? 'working' : 'waiting'
    return {
      kind: w.kind,
      storyId: w.storyId ?? null,
      fileId: w.fileId ?? null,
      title: w.title,
      state,
      progress: state === 'working' && w.step > 0 ? w.step / w.steps : null,
      error: w.error,
    }
  })
}

// ── What the Feed starts with that needs the files catalog ───────────────

let seeded = false

/**
 * Make what the seed needs the catalog for, once: two episodes with
 * transcripts (one written up as an article), a recording in Files that has
 * been written down, and the history. Called by every read that could show
 * any of it, and not when this file loads.
 */
function ensureSeeded(): void {
  if (seeded) return
  seeded = true
  for (const id of ['st-17', 'st-19']) {
    const s = find(id)
    s.files = { ...s.files, transcript: writeStoryTranscript(s).id }
  }
  // Episode 39 was written up from its transcript, show notes kept below.
  const walk = find('st-19')
  bodies.set(walk.id, articleFromTranscript(walk))
  bodyOrigins.set(walk.id, 'transcript')
  const memo = (
    mockFilesInvoke('list_files', { query: { kinds: ['audio'], text: 'ideas' } }) as FilePage
  ).files[0]
  if (memo) writeFileTranscript(memo)
  seedHistory()
}

// ── Up Next ──────────────────────────────────────────────────────────────

const queue: QueueEntry[] = []
let queueSeeded = false

function renumber(): void {
  queue.forEach((e, i) => (e.sortOrder = (i + 1) * 1024))
}

function makeEntry(what: ContentRef, addedBy: AddedBy): QueueEntry {
  const d = describe(what)
  return {
    id: nextId('queue'),
    what,
    title: d.title,
    source: d.source,
    mode: d.mode,
    durationMs: d.durationMs,
    addedBy,
    addedAt: new Date().toISOString(),
    sortOrder: 0,
  }
}

/** The queue as a first visit finds it: episodes, an article, a recording from Files. */
function ensureQueue(): void {
  if (queueSeeded) return
  queueSeeded = true
  const memo = (
    mockFilesInvoke('list_files', { query: { kinds: ['audio'], text: 'ideas' } }) as FilePage
  ).files[0]
  const first: [ContentRef, AddedBy][] = [
    [{ type: 'story', id: 'st-18' }, 'show'],
    [{ type: 'story', id: 'st-17' }, 'show'],
    [{ type: 'story', id: 'st-1' }, 'person'],
    [{ type: 'story', id: 'st-21' }, 'recommended'],
  ]
  if (memo) first.splice(3, 0, [{ type: 'file', id: memo.id }, 'person'])
  for (const [what, by] of first) queue.push(makeEntry(what, by))
  renumber()
  if (memo) {
    seedConsumption({ type: 'file', id: memo.id }, memo.name, 'Voice memos', 'listen', {
      lastAt: ago(24 * 2),
      progress: 0.33,
      positionMs: 31_000,
      durationMs: memo.media.durationMs ?? 94_000,
      heard: [{ startMs: 0, endMs: 31_000 }],
    })
  }
}

function removeFromQueue(match: (e: QueueEntry) => boolean): void {
  for (let i = queue.length - 1; i >= 0; i--) if (match(queue[i]!)) queue.splice(i, 1)
  renumber()
}

function itemOf(entry: QueueEntry): QueueItem {
  const what = entry.what
  const consumption = consumptions.get(refKey(what))
  const item: QueueItem = {
    entry: structuredClone(entry),
    sourceTitle: entry.source,
    downloading: false,
  }
  if (consumption) item.consumption = structuredClone(consumption)
  const resume = consumption && !consumption.finished ? (consumption.positionMs ?? null) : null
  if (what.type === 'story') {
    const s = stories.find((x) => x.id === what.id)
    if (s) {
      item.story = structuredClone(s)
      item.sourceTitle = sources.find((x) => x.id === s.sourceId)?.title ?? entry.source
      // An episode plays its file; an article plays the audio it was saved as.
      const media = s.media
      const file = media ? s.files?.media : s.files?.audio
      if (file) {
        const intro = (sources.find((x) => x.id === s.sourceId)?.podcast?.skipIntroS ?? 0) * 1000
        item.play = {
          fileId: file,
          kind: media?.mime.startsWith('video') ? 'video' : 'audio',
          durationMs: media
            ? (media.durationMs ?? null)
            : (episodeFiles.get(file)?.durationMs ?? null),
          startMs: resume ?? (media && intro > 0 ? intro : null),
        }
        // It is something to hear now, whatever it was when it was lined up.
        if (!media) {
          item.entry.mode = 'listen'
          item.entry.durationMs = item.play.durationMs
        }
      }
      const f = fetching.get(s.id)
      item.downloading = !!f && f.state !== 'failed'
    }
  } else if (what.type === 'file') {
    const f = mockCatalogFile(what.id)
    if (f) {
      item.file = structuredClone(f)
      if (f.kind === 'audio' || f.kind === 'video') {
        item.play = {
          fileId: f.id,
          kind: f.kind,
          durationMs: f.media.durationMs ?? null,
          startMs: resume,
        }
      }
    }
  } else {
    const found = itemLookup?.(what.id)
    if (found) {
      item.item = structuredClone(found.item)
      item.sourceTitle = found.shelf
    }
  }
  return item
}

export function mockListQueue(): QueueView {
  ensureSeeded()
  ensureQueue()
  return { items: queue.map(itemOf) }
}

export function mockAddToQueue(what: ContentRef, place: QueuePlace | undefined): QueueEntry {
  ensureQueue()
  const kept = queue.find((e) => sameRef(e.what, what))
  const entry = kept ?? makeEntry(what, 'person')
  // An episode with no file is asked for as it is queued, so it can play when its turn comes.
  if (!kept && what.type === 'story') {
    const s = find(what.id)
    if (s.media && !s.files?.media) requestDownload(what.id)
  }
  if (kept) queue.splice(queue.indexOf(kept), 1)
  const at =
    place?.type === 'next'
      ? 0
      : place?.type === 'before'
        ? queue.findIndex((e) => e.id === place.id)
        : -1
  queue.splice(at < 0 ? queue.length : at, 0, entry)
  renumber()
  return structuredClone(entry)
}

export function mockRemoveFromQueue(ids: string[]): void {
  ensureQueue()
  removeFromQueue((e) => ids.includes(e.id))
}

export function mockMoveInQueue(id: string, before: string | null): void {
  ensureQueue()
  const at = queue.findIndex((e) => e.id === id)
  if (at < 0) throw new VaultError('not_found', 'That is no longer in Up Next.')
  const [moved] = queue.splice(at, 1)
  const to = before ? queue.findIndex((e) => e.id === before) : -1
  queue.splice(to < 0 ? queue.length : to, 0, moved!)
  renumber()
}

// ── Recommended ──────────────────────────────────────────────────────────

/** What the mock's judge thought of some stories, so the page opens on a believable order. */
const RANKED: Record<StoryId, number> = {
  'st-5': 0.74,
  'st-17': 0.61,
  'st-1': 0.52,
  'st-9': 0.4,
  'st-10': 0.18,
  'st-14': -0.12,
}

/** Stories the judge has not got to, in the order it will. */
let unranked: StoryId[] = []

/** Unread, unqueued, unfinished, and inside the window: what Recommended is made of. */
function candidates(): Story[] {
  ensureQueue()
  const cut = Date.now() - settings.ranking.windowDays * 24 * HOUR
  return stories
    .filter(
      (s) =>
        !s.read &&
        !s.expired &&
        Date.parse(s.publishedAt) >= cut &&
        !queue.some((e) => e.what.type === 'story' && e.what.id === s.id) &&
        !consumptions.get(`story:${s.id}`)?.finished,
    )
    .sort(newestFirst)
}

/** Whether there is a judge to ask: the ranking is on and has something to ask. */
function judging(): boolean {
  const r = settings.ranking
  return r.enabled && (r.goals || r.criteria.some((c) => c.text.trim() !== ''))
}

function scoreOf(s: Story): number {
  const fixed = RANKED[s.id]
  if (fixed !== undefined) return fixed
  return Math.round((-0.3 + ((hashOf(s.id) % 1000) / 1000) * 1.2) * 100) / 100
}

/** The criteria a story answered yes to: the ones with weight for it, picked the way a model would, by feel. */
function reasonsOf(s: Story): string[] {
  const score = scoreOf(s)
  if (score <= 0) return []
  return settings.ranking.criteria
    .filter((c) => c.weight > 0 && hashOf(`${s.id}${c.id}`) % 3 !== 0)
    .slice(0, 3)
    .map((c) => c.text)
}

export function mockRecommended(limit: number | null | undefined): Recommendation[] {
  ensureSeeded()
  const pending = new Set(unranked)
  const ranked = judging()
  const rows = candidates().map((s): Recommendation => {
    const scored = ranked && !pending.has(s.id)
    const progress = consumptions.get(`story:${s.id}`)?.progress
    return {
      story: structuredClone(s),
      sourceTitle: sources.find((x) => x.id === s.sourceId)?.title ?? '',
      score: scored ? scoreOf(s) : null,
      reasons: scored ? reasonsOf(s) : [],
      queued: false,
      progress: progress && progress > 0 ? progress : null,
    }
  })
  // Best first, then newest; what has not been ranked follows what has.
  rows.sort(
    (a, b) =>
      (b.score ?? Number.NEGATIVE_INFINITY) - (a.score ?? Number.NEGATIVE_INFINITY) ||
      newestFirst(a.story, b.story),
  )
  return typeof limit === 'number' && limit > 0 ? rows.slice(0, limit) : rows
}

/** Where the judge is. Asking lets it get through a few more, as a worker would while the page waited. */
export function mockRankingStatus(): RankingStatus {
  const live = candidates().map((s) => s.id)
  unranked = unranked.filter((id) => live.includes(id))
  if (!judging()) return { judge: null, ranked: 0, waiting: 0, problem: null }
  unranked.splice(0, 3)
  return {
    judge: 'decision',
    ranked: live.length - unranked.length,
    waiting: unranked.length,
    problem: null,
  }
}

export function mockRerank(): void {
  unranked = candidates().map((s) => s.id)
}

// ── The directory ────────────────────────────────────────────────────────

/** Apple's genres, as the charts take them. */
const GENRES: PodcastGenre[] = [
  { id: 1301, name: 'Arts' },
  { id: 1321, name: 'Business' },
  { id: 1303, name: 'Comedy' },
  { id: 1304, name: 'Education' },
  { id: 1487, name: 'History' },
  { id: 1512, name: 'Health & Fitness' },
  { id: 1489, name: 'News' },
  { id: 1533, name: 'Science' },
  { id: 1324, name: 'Society & Culture' },
  { id: 1318, name: 'Technology' },
  { id: 1309, name: 'TV & Film' },
]

interface Show {
  id: string
  title: string
  author: string
  feed: string
  genre: number
  episodes: number
  explicit?: boolean
}

/** The shows the mock's directory knows, best first, none of them real. */
const SHOWS: Show[] = [
  {
    id: '1450001',
    title: 'The Slow Hour',
    author: 'Nadia Frost',
    feed: 'https://slowhour.example/feed.xml',
    genre: 1301,
    episodes: 43,
  },
  {
    id: '1450101',
    title: 'Deep Water',
    author: 'Marta Okoye',
    feed: 'https://deepwater.example/rss',
    genre: 1533,
    episodes: 112,
  },
  {
    id: '1450102',
    title: 'Ten Minutes of History',
    author: 'Hugh Penrose',
    feed: 'https://tenminutes.example/feed',
    genre: 1487,
    episodes: 380,
  },
  {
    id: '1450002',
    title: 'Frame by Frame',
    author: 'Okonkwo and Lee',
    feed: 'https://framebyframe.example/video.xml',
    genre: 1309,
    episodes: 58,
  },
  {
    id: '1450103',
    title: 'The Small Business Hour',
    author: 'Ruth Adeyemi',
    feed: 'https://smallbiz.example/rss',
    genre: 1321,
    episodes: 204,
  },
  {
    id: '1450104',
    title: 'Bytes and Bolts',
    author: 'Sam Varga',
    feed: 'https://bytesandbolts.example/podcast.xml',
    genre: 1318,
    episodes: 91,
  },
  {
    id: '1450105',
    title: 'Nothing Much Happened',
    author: 'The Wrens',
    feed: 'https://nothingmuch.example/feed',
    genre: 1303,
    episodes: 150,
    explicit: true,
  },
  {
    id: '1450106',
    title: 'Weather Words',
    author: 'Ines Calder',
    feed: 'https://weatherwords.example/rss',
    genre: 1304,
    episodes: 64,
  },
  {
    id: '1450107',
    title: 'Morning Rounds',
    author: 'Dr. Priya Nair',
    feed: 'https://morningrounds.example/feed.xml',
    genre: 1512,
    episodes: 77,
  },
  {
    id: '1450108',
    title: 'The Evening Edition',
    author: 'Colm Brady',
    feed: 'https://eveningedition.example/rss',
    genre: 1489,
    episodes: 910,
  },
  {
    id: '1450109',
    title: 'Other People’s Kitchens',
    author: 'Leila Haddad',
    feed: 'https://kitchens.example/feed',
    genre: 1324,
    episodes: 129,
  },
]

/** A stand-in for artwork: a tile in a colour that follows the name, with its first letter. */
function artwork(title: string): string {
  const hue = hashOf(title) % 360
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" width="300" height="300">` +
    `<rect width="300" height="300" fill="hsl(${hue} 45% 38%)"/>` +
    `<text x="150" y="195" font-family="sans-serif" font-size="150" text-anchor="middle" ` +
    `fill="hsl(${hue} 60% 88%)">${escapeHtml(title.charAt(0).toUpperCase())}</text></svg>`
  return `data:image/svg+xml;base64,${btoa(svg)}`
}

function followedFeed(url: string): boolean {
  const href = (() => {
    try {
      return new URL(url).href
    } catch {
      return url
    }
  })()
  return sources.some((s) => s.kind.type === 'web' && s.kind.url === href)
}

function hitFor(show: Show): PodcastHit {
  return {
    directoryId: show.id,
    title: show.title,
    author: show.author,
    feedUrl: show.feed,
    artwork: artwork(show.title),
    genre: GENRES.find((g) => g.id === show.genre)?.name ?? '',
    episodes: show.episodes,
    explicit: show.explicit ?? false,
    followed: followedFeed(show.feed),
  }
}

/** Does every word of `query` appear in `text`? */
function matches(query: string, text: string): boolean {
  const hay = text.toLowerCase()
  return query
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean)
    .every((word) => hay.includes(word))
}

export function mockSearchPodcasts(query: string, limit: number | null | undefined): PodcastHit[] {
  const q = query.trim()
  if (!q) return []
  // An address that says so is how the unreachable state can be seen.
  if (/unreachable|offline/i.test(q)) {
    throw new VaultError('network', 'The podcast directory could not be reached.')
  }
  const genre = (id: number) => GENRES.find((g) => g.id === id)?.name ?? ''
  return SHOWS.filter((s) => matches(q, `${s.title} ${s.author} ${genre(s.genre)}`))
    .slice(0, limit && limit > 0 ? limit : 25)
    .map(hitFor)
}

export function mockTopPodcasts(
  genre: number | null | undefined,
  limit: number | null | undefined,
): PodcastHit[] {
  return SHOWS.filter((s) => genre == null || s.genre === genre)
    .slice(0, limit && limit > 0 ? limit : 25)
    .map(hitFor)
}

export function mockPodcastGenres(): PodcastGenre[] {
  return structuredClone(GENRES)
}

const FEEDS: Omit<FeedHit, 'followed'>[] = [
  {
    feedUrl: 'https://longform.example/feed.xml',
    title: 'Longform Weekly',
    siteUrl: 'https://longform.example',
    description: 'One long piece, every week, about something that took somebody years.',
    subscribers: 18_400,
    icon: '',
  },
  {
    feedUrl: 'https://tidewatch.example/rss',
    title: 'Tidewatch',
    siteUrl: 'https://tidewatch.example',
    description: 'Notes from the people who keep harbours, lights and tide gauges.',
    subscribers: 2_150,
    icon: '',
  },
  {
    feedUrl: 'https://deskset.example/atom.xml',
    title: 'Desk Set',
    siteUrl: 'https://deskset.example',
    description: 'What people keep on their desks, and why it is there.',
    subscribers: 9_020,
    icon: '',
  },
  {
    feedUrl: 'https://smallmachines.example/feed',
    title: 'Small Machines',
    siteUrl: 'https://smallmachines.example',
    description: 'Clocks, typewriters, music boxes: how they work and how they break.',
    subscribers: 5_730,
    icon: '',
  },
  {
    feedUrl: 'https://quietnews.example/rss',
    title: 'The Quiet News',
    siteUrl: 'https://quietnews.example',
    description: 'A morning digest with the volume turned down.',
    subscribers: 31_000,
    icon: '',
  },
]

export function mockSearchFeeds(query: string, limit: number | null | undefined): FeedHit[] {
  const q = query.trim()
  if (!q) return []
  return FEEDS.filter((f) => matches(q, `${f.title} ${f.description} ${f.siteUrl}`))
    .slice(0, limit && limit > 0 ? limit : 25)
    .map((f) => ({ ...f, icon: artwork(f.title), followed: followedFeed(f.feedUrl) }))
}
