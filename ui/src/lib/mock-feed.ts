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
// Nothing here runs at module load but building the seed: no timers, so
// loading this file cannot hold a test process open. A refresh is a function
// call that adds the next story from a short script rather than a timer that
// does it on its own.

import type {
  FoundFeed,
  HnList,
  OpmlImportReport,
  NewSource,
  NewsSource,
  NewsSourceId,
  NewsletterSender,
  RefreshReport,
  SourceSummary,
  Story,
  StoryId,
  StoryPage,
  StoryQuery,
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
]

const fetched = new Map<NewsSourceId, Fetched>([
  [LONGFORM, { lastOk: ago(1), lastError: null, nextAt: ago(-1) }],
  [SHIPYARD, { lastOk: ago(2), lastError: null, nextAt: ago(-0.5) }],
  [HACKER, { lastOk: ago(0.5), lastError: null, nextAt: ago(-0.25) }],
  [
    RADIO,
    { lastOk: ago(24 * 9), lastError: 'The server answered 404 Not Found.', nextAt: ago(-6) },
  ],
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

const bodies = new Map<StoryId, string>()
for (const s of stories) if (s.hasBody) bodies.set(s.id, articleBody(s, 3))

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
 * stand-in for `everyday://news/body/{id}` and its `X-News-Images-Hidden`
 * header, neither of which a plain browser tab can answer.
 *
 * A story with no stored body is drawn from its summary, as the real route
 * does, so the reading pane is never blank for one that is only a teaser.
 */
export function mockStoryBody(id: StoryId): { html: string; imagesHidden: boolean } {
  const s = find(id)
  let html = bodies.get(id) ?? `<p>${escapeHtml(s.summary)}</p>`
  const origin = sources.find((x) => x.id === s.sourceId)
  if (origin && !origin.images) {
    const hidden = /<img\b/i.test(html)
    html = html.replace(/<figure>[\s\S]*?<\/figure>/gi, '').replace(/<img\b[^>]*>/gi, '')
    return { html, imagesHidden: hidden }
  }
  return { html, imagesHidden: false }
}

export function mockFetchFullArticle(id: StoryId): Story {
  const s = find(id)
  if (!s.url) throw new VaultError('invalid', 'this story has no article to fetch')
  bodies.set(id, articleBody(s, 5))
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
      if (sources.some((s) => s.kind.type === 'mail' && s.kind.sender === sender)) {
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
  }
}

export function mockAddNewsSource(input: NewSource): NewsSource {
  const order = Math.max(-1, ...sources.map((s) => s.sortOrder)) + 1
  const now = new Date().toISOString()
  const made = build(input, order, now)
  sources.push(made)
  fetched.set(made.id, { lastOk: null, lastError: null, nextAt: now })
  return structuredClone(made)
}

export function mockSaveNewsSource(updated: NewsSource): void {
  const at = sources.findIndex((s) => s.id === updated.id)
  if (at < 0) throw new VaultError('notFound', 'no such source')
  sources[at] = { ...structuredClone(updated), updatedAt: new Date().toISOString() }
}

export function mockDeleteNewsSource(id: NewsSourceId): void {
  sourceById(id)
  const at = sources.findIndex((s) => s.id === id)
  sources.splice(at, 1)
  fetched.delete(id)
  for (let i = stories.length - 1; i >= 0; i--) {
    const gone = stories[i]!
    if (gone.sourceId !== id) continue
    bodies.delete(gone.id)
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
    mailMessage: src.kind.type === 'mail' ? nextId('msg') : null,
    hasBody: !hn,
  })
  stories.push(made)
  if (made.hasBody) bodies.set(id, articleBody(made, 3))
  return made
}

function refreshOne(src: NewsSource): RefreshReport {
  const state = fetched.get(src.id) ?? { lastOk: null, lastError: null, nextAt: null }
  fetched.set(src.id, state)
  const now = new Date().toISOString()
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
    '  <head><title>Every Day subscriptions</title></head>',
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
