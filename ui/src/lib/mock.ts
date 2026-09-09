// An in-memory stand-in for the Rust backend.
//
// This is not a toy: it implements every command the interface calls, so the
// whole app -- lock screen, journals, editor, search, media -- runs in a
// plain browser. It exists so the interface can be designed and reviewed
// without a native build, and so a broken Rust build never blocks UI work.
//
// It is reachable only from a development build. `api.ts` gates the import
// on `import.meta.env.DEV`, so this module is eliminated at build time and
// never reaches a released application.

import type {
  AddedItem,
  BlockKind,
  BlockQuery,
  BlockSubject,
  Bootstrap,
  Calendar,
  CalendarEvent,
  CalendarInfo,
  BalanceReport,
  EventQuery,
  Goal,
  GoalActivity,
  GoalQuery,
  GoalStatus,
  Item,
  ItemQuery,
  ItemStatus,
  Kind,
  KindInfo,
  LibraryStats,
  LogEntry,
  LogEvent,
  LogQuery,
  ProviderInfo,
  Purpose,
  PurposeMinutes,
  Role,
  RoleEventMinutes,
  SearchRequest,
  SearchResult,
  SourceInfo,
  SyncReport,
  Entry,
  EntryQuery,
  EntrySummary,
  Journal,
  Project,
  Reading,
  ReadingQuery,
  SearchHit,
  Task,
  TaskQuery,
  TaskStats,
  TaskStatus,
  TimeBlock,
  Tracker,
  TrackerDay,
  TrackerKind,
  VaultStatus,
} from './types'
import { TASK_STATUSES, VaultError, goalIsOpen, isAhead, isOpen, priorityRank } from './types'
import type { AgentEvent, AgentMessage, AgentSettings, Conversation, Memory } from './types'
import { DEFAULT_COLORS } from './colors'

const PASSWORD = 'everyday'

/**
 * A string argument off the command bridge.
 *
 * Arguments arrive as `unknown`, because this file stands in for a process
 * that receives JSON. `String(x)` on an object does not fail -- it yields
 * "[object Object]" -- which in a fake backend is a silent wrong answer
 * rather than a loud one. Narrow, and fall back when the shape is not what
 * was expected.
 */
function str(v: unknown, fallback = ''): string {
  return typeof v === 'string' ? v : fallback
}

function iso(daysAgo: number): string {
  const d = new Date()
  d.setDate(d.getDate() - daysAgo)
  return d.toISOString()
}

function day(daysAgo: number): string {
  // Local, not `toISOString()`: that is UTC, so an evening west of Greenwich
  // would stamp today's entry with tomorrow's date.
  const d = new Date()
  d.setDate(d.getDate() - daysAgo)
  const p = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`
}

function para(...lines: string[]) {
  return lines.map((text) => ({
    type: 'paragraph',
    content: text ? [{ type: 'text', text }] : [],
  }))
}

/** A soft gradient standing in for a photograph. */
function swatch(a: string, b: string, label: string): string {
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="800">
    <defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0%" stop-color="${a}"/><stop offset="100%" stop-color="${b}"/>
    </linearGradient></defs>
    <rect width="1200" height="800" fill="url(#g)"/>
    <text x="60" y="740" font-family="system-ui,sans-serif" font-size="34"
          fill="rgba(255,255,255,.72)">${label}</text>
  </svg>`
  return `data:image/svg+xml;base64,${btoa(unescape(encodeURIComponent(svg)))}`
}

const BLOBS: Record<string, string> = {
  ['a'.repeat(64)]: swatch('#f0a35e', '#b8434f', 'the harbour at dusk'),
  ['b'.repeat(64)]: swatch('#4a7fb5', '#1c2f4a', 'the train window'),
  ['c'.repeat(64)]: swatch('#7ba05b', '#2f4a2f', 'first frost'),
}

function tracker(
  id: string,
  name: string,
  kind: TrackerKind,
  icon: string,
  color: string,
  rest: Partial<Tracker> = {},
): Tracker {
  return {
    id,
    name,
    kind,
    icon,
    color,
    unit: '',
    defaultValue: 1,
    target: null,
    scaleMax: 10,
    onCalendar: false,
    archived: false,
    sortOrder: 0,
    createdAt: iso(400),
    updatedAt: iso(400),
    ...rest,
  }
}

// ── The library domain ───────────────────────────────────────────────────
//
// Enough of a shelf to design against: covers in four ratios, every status,
// rated and unrated items side by side, and a history with more than one
// line in it. The metadata lookup is faked further down -- see `web_search`
// -- so the add sheet can be exercised in a browser with no network.

/** A portrait "cover", so the grid has something to be a grid of. */
function jacket(a: string, b: string, label: string): string {
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="600" height="900">
    <defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0%" stop-color="${a}"/><stop offset="100%" stop-color="${b}"/>
    </linearGradient></defs>
    <rect width="600" height="900" fill="url(#g)"/>
    <text x="48" y="820" font-family="Georgia,serif" font-size="46"
          fill="rgba(255,255,255,.86)">${label}</text>
  </svg>`
  return `data:image/svg+xml;base64,${btoa(unescape(encodeURIComponent(svg)))}`
}

const COVERS: Record<string, string> = {
  ['1'.repeat(64)]: jacket('#c98a3a', '#5b3410', 'Dune'),
  ['2'.repeat(64)]: jacket('#3f6f8f', '#16283a', 'Arrival'),
  ['3'.repeat(64)]: jacket('#7a5aa8', '#2c1b46', 'Pnin'),
  ['4'.repeat(64)]: jacket('#4d7c4a', '#1e3320', 'Outer Wilds'),
  ['5'.repeat(64)]: jacket('#a6455e', '#3d1522', 'Kind of Blue'),
}

function verbs(wishlist: string, active: string, done: string, log: string) {
  return { wishlist, active, done, log }
}

function field(key: string, label: string, fieldType: Kind['fields'][number]['fieldType']) {
  return { key, label, fieldType, placeholder: '' }
}

const kinds: KindInfo[] = [
  {
    id: 'k-book',
    slug: 'book',
    name: 'Books',
    singular: 'Book',
    icon: '\u{1f4d9}',
    color: '#b4530f',
    verbs: verbs('To read', 'Reading', 'Read', 'read'),
    fields: [
      field('author', 'Author', 'text'),
      field('pages', 'Pages', 'number'),
      field('publisher', 'Publisher', 'text'),
      field('isbn', 'ISBN', 'text'),
    ],
    source: 'openLibrary',
    progressUnit: 'page',
    sortOrder: 0,
    builtin: true,
    visible: true,
    createdAt: iso(300),
    updatedAt: iso(300),
    items: 0,
    open: 0,
  },
  {
    id: 'k-film',
    slug: 'film',
    name: 'Films',
    singular: 'Film',
    icon: '\u{1f3ac}',
    color: '#0f766e',
    verbs: verbs('To watch', 'Watching', 'Watched', 'watched'),
    fields: [field('director', 'Director', 'text'), field('runtime', 'Runtime (min)', 'number')],
    source: 'itunes',
    progressUnit: 'minute',
    sortOrder: 1,
    builtin: true,
    visible: true,
    createdAt: iso(300),
    updatedAt: iso(300),
    items: 0,
    open: 0,
  },
  {
    id: 'k-game',
    slug: 'game',
    name: 'Games',
    singular: 'Game',
    icon: '\u{1f3ae}',
    color: '#7c3aed',
    verbs: verbs('To play', 'Playing', 'Played', 'played'),
    fields: [field('developer', 'Developer', 'text'), field('platform', 'Platform', 'text')],
    source: 'wikipedia',
    progressUnit: 'hour',
    sortOrder: 2,
    builtin: true,
    visible: true,
    createdAt: iso(300),
    updatedAt: iso(300),
    items: 0,
    open: 0,
  },
  {
    id: 'k-music',
    slug: 'music',
    name: 'Music',
    singular: 'Album',
    icon: '\u{1f3b5}',
    color: '#b91c5c',
    verbs: verbs('To hear', 'Listening', 'Heard', 'listened to'),
    fields: [field('artist', 'Artist', 'text'), field('label', 'Label', 'text')],
    source: 'itunes',
    progressUnit: '',
    sortOrder: 3,
    builtin: true,
    visible: true,
    createdAt: iso(300),
    updatedAt: iso(300),
    items: 0,
    open: 0,
  },
  {
    id: 'k-restaurant',
    slug: 'restaurant',
    name: 'Restaurants',
    singular: 'Restaurant',
    icon: '\u{1f37d}\u{fe0f}',
    color: '#1d4ed8',
    verbs: verbs('To try', 'Booked', 'Been', 'ate at'),
    fields: [
      field('cuisine', 'Cuisine', 'text'),
      field('address', 'Address', 'multiline'),
      field('url', 'Website', 'url'),
    ],
    source: 'nominatim',
    progressUnit: '',
    sortOrder: 4,
    builtin: true,
    visible: true,
    createdAt: iso(300),
    updatedAt: iso(300),
    items: 0,
    open: 0,
  },
]

function seedItem(partial: Partial<Item> & { kindId: string; title: string }): Item {
  return {
    id: `i-${Math.random().toString(36).slice(2, 10)}`,
    subtitle: '',
    creator: '',
    year: null,
    status: 'wishlist',
    rating: null,
    external: [],
    cover: null,
    coverUrl: '',
    summary: '',
    notes: '',
    tags: [],
    facts: {},
    links: [],
    progress: null,
    favourite: false,
    startedOn: null,
    finishedOn: null,
    source: '',
    sortOrder: 0,
    createdAt: iso(30),
    updatedAt: iso(30),
    ...partial,
  }
}

const items: Item[] = [
  seedItem({
    id: 'i-dune',
    kindId: 'k-book',
    title: 'Dune',
    creator: 'Frank Herbert',
    year: 1965,
    status: 'done',
    rating: 90,
    external: [{ source: 'Open Library', score: 84, count: 1204, url: 'https://openlibrary.org' }],
    cover: '1'.repeat(64),
    summary: 'A desert planet, the spice that comes from it, and the empire that wants it.',
    notes: 'Better than I remembered. The ecology holds up; the politics more so.',
    tags: ['sci-fi', 'reread'],
    facts: { author: 'Frank Herbert', pages: '412', publisher: 'Chilton Books' },
    progress: { position: 412, total: 412, unit: 'page' },
    favourite: true,
    startedOn: day(58),
    finishedOn: day(24),
    source: 'openLibrary',
    createdAt: iso(60),
  }),
  seedItem({
    id: 'i-pnin',
    kindId: 'k-book',
    title: 'Pnin',
    creator: 'Vladimir Nabokov',
    year: 1957,
    status: 'active',
    cover: '3'.repeat(64),
    facts: { author: 'Vladimir Nabokov', pages: '191' },
    progress: { position: 84, total: 191, unit: 'page' },
    startedOn: day(9),
    tags: ['novel'],
    createdAt: iso(12),
  }),
  seedItem({
    id: 'i-cloud',
    kindId: 'k-book',
    title: 'The Cloud Atlas',
    creator: 'David Mitchell',
    year: 2004,
    status: 'wishlist',
    createdAt: iso(3),
  }),
  seedItem({
    id: 'i-arrival',
    kindId: 'k-film',
    title: 'Arrival',
    creator: 'Denis Villeneuve',
    year: 2016,
    status: 'done',
    rating: 95,
    external: [{ source: 'iTunes', score: 88, count: 4310, url: 'https://itunes.apple.com' }],
    cover: '2'.repeat(64),
    summary: 'A linguist is asked to talk to something that does not experience time as we do.',
    facts: { director: 'Denis Villeneuve', runtime: '116' },
    finishedOn: day(140),
    startedOn: day(140),
    source: 'itunes',
    createdAt: iso(150),
  }),
  seedItem({
    id: 'i-past-lives',
    kindId: 'k-film',
    title: 'Past Lives',
    creator: 'Celine Song',
    year: 2023,
    status: 'wishlist',
    createdAt: iso(6),
  }),
  seedItem({
    id: 'i-outer-wilds',
    kindId: 'k-game',
    title: 'Outer Wilds',
    creator: 'Mobius Digital',
    year: 2019,
    status: 'active',
    cover: '4'.repeat(64),
    facts: { developer: 'Mobius Digital', platform: 'PC' },
    progress: { position: 11, total: null, unit: 'hour' },
    startedOn: day(16),
    createdAt: iso(20),
  }),
  seedItem({
    id: 'i-kob',
    kindId: 'k-music',
    title: 'Kind of Blue',
    creator: 'Miles Davis',
    year: 1959,
    status: 'done',
    rating: 100,
    cover: '5'.repeat(64),
    facts: { artist: 'Miles Davis', label: 'Columbia' },
    favourite: true,
    finishedOn: day(70),
    createdAt: iso(200),
  }),
  seedItem({
    id: 'i-somsaa',
    kindId: 'k-restaurant',
    title: 'Som Saa',
    status: 'done',
    rating: 85,
    facts: {
      cuisine: 'Thai',
      address: '43A Commercial Street, Spitalfields, London',
      url: 'https://somsaa.example',
    },
    notes: 'Go back for the fish. Book weeks ahead.',
    finishedOn: day(33),
    createdAt: iso(90),
  }),
  seedItem({
    id: 'i-tsuki',
    kindId: 'k-restaurant',
    title: 'Tsukiji-ya',
    status: 'wishlist',
    facts: { cuisine: 'Japanese' },
    createdAt: iso(2),
  }),
  seedItem({
    id: 'i-abandoned',
    kindId: 'k-book',
    title: 'Infinite Jest',
    creator: 'David Foster Wallace',
    year: 1996,
    status: 'abandoned',
    rating: 40,
    notes: 'Twice. Not this decade.',
    progress: { position: 210, total: 1079, unit: 'page' },
    startedOn: day(320),
    createdAt: iso(330),
  }),
]

const logs: LogEntry[] = [
  {
    id: 'l-1',
    itemId: 'i-dune',
    event: 'started',
    date: day(58),
    tz: 'Europe/London',
    note: 'Picked it up again after fifteen years.',
    rating: null,
    position: null,
    minutes: null,
    createdAt: iso(58),
    updatedAt: iso(58),
  },
  {
    id: 'l-2',
    itemId: 'i-dune',
    event: 'progress',
    date: day(40),
    tz: 'Europe/London',
    note: '',
    rating: null,
    position: 240,
    minutes: null,
    createdAt: iso(40),
    updatedAt: iso(40),
  },
  {
    id: 'l-3',
    itemId: 'i-dune',
    event: 'finished',
    date: day(24),
    tz: 'Europe/London',
    note: 'Holds up.',
    rating: 90,
    position: 412,
    minutes: null,
    createdAt: iso(24),
    updatedAt: iso(24),
  },
  {
    id: 'l-4',
    itemId: 'i-arrival',
    event: 'finished',
    date: day(140),
    tz: 'Europe/London',
    note: '',
    rating: 95,
    position: null,
    minutes: 116,
    createdAt: iso(140),
    updatedAt: iso(140),
  },
  {
    id: 'l-5',
    itemId: 'i-somsaa',
    event: 'finished',
    date: day(33),
    tz: 'Europe/London',
    note: 'The fish.',
    rating: 85,
    position: null,
    minutes: null,
    createdAt: iso(33),
    updatedAt: iso(33),
  },
]

/** A log row dated today, as `new_log` mints one. */
function newLogEntry(itemId: string, event: LogEvent): LogEntry {
  const now = new Date().toISOString()
  return {
    id: `l-${Math.random().toString(36).slice(2, 10)}`,
    itemId,
    event,
    date: day(0),
    tz: Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC',
    note: '',
    rating: null,
    position: null,
    minutes: null,
    createdAt: now,
    updatedAt: now,
  }
}

/** Shelf counts, recomputed the way the backend does rather than cached. */
function kindCounts(): KindInfo[] {
  return kinds.map((k) => ({
    ...k,
    items: items.filter((i) => i.kindId === k.id).length,
    open: items.filter((i) => i.kindId === k.id && isAhead(i.status)).length,
  }))
}

function applyItemQuery(q: ItemQuery): Item[] {
  let rows = items.filter((i) => {
    if (q.kindId && i.kindId !== q.kindId) return false
    if (q.statuses?.length && !q.statuses.includes(i.status)) return false
    if (q.favourite !== null && q.favourite !== undefined && i.favourite !== q.favourite)
      return false
    // An unrated item is not a zero-rated one. Same rule as the core.
    if (q.ratingAtLeast != null && (i.rating == null || i.rating < q.ratingAtLeast)) return false
    if (q.finishedFrom && (!i.finishedOn || i.finishedOn < q.finishedFrom)) return false
    if (q.finishedTo && (!i.finishedOn || i.finishedOn > q.finishedTo)) return false
    if (q.tags?.length) {
      const have = i.tags.map((t) => t.toLowerCase())
      if (!q.tags.every((t) => have.includes(t.toLowerCase()))) return false
    }
    const needle = (q.text ?? '').trim().toLowerCase()
    if (!needle) return true
    const hay = [
      i.title,
      i.subtitle,
      i.creator,
      i.summary,
      i.notes,
      ...i.tags,
      ...Object.values(i.facts),
    ]
      .join('\n')
      .toLowerCase()
    return hay.includes(needle)
  })

  const byTitle = (a: Item, b: Item) => a.title.toLowerCase().localeCompare(b.title.toLowerCase())
  const nullsLast = <T>(a: T | null | undefined, b: T | null | undefined) =>
    (a == null ? 1 : 0) - (b == null ? 1 : 0)
  rows = rows.slice().sort((a, b) => {
    // Favourites float, exactly as `sort_items` does.
    const star = Number(b.favourite) - Number(a.favourite)
    if (star) return star
    switch (q.sort ?? 'addedDesc') {
      case 'addedAsc':
        return a.createdAt.localeCompare(b.createdAt) || byTitle(a, b)
      case 'updatedDesc':
        return b.updatedAt.localeCompare(a.updatedAt) || byTitle(a, b)
      case 'titleAsc':
        return byTitle(a, b)
      case 'ratingDesc':
        return (b.rating ?? 0) - (a.rating ?? 0) || byTitle(a, b)
      case 'finishedDesc':
        return (
          nullsLast(a.finishedOn, b.finishedOn) ||
          (b.finishedOn ?? '').localeCompare(a.finishedOn ?? '') ||
          byTitle(a, b)
        )
      case 'yearAsc':
        return nullsLast(a.year, b.year) || (a.year ?? 0) - (b.year ?? 0) || byTitle(a, b)
      case 'yearDesc':
        return nullsLast(a.year, b.year) || (b.year ?? 0) - (a.year ?? 0) || byTitle(a, b)
      case 'manual':
        return a.sortOrder - b.sortOrder || byTitle(a, b)
      default:
        return b.createdAt.localeCompare(a.createdAt) || byTitle(a, b)
    }
  })

  const from = q.offset ?? 0
  return rows.slice(from, q.limit == null ? undefined : from + q.limit)
}

function libraryStats(): LibraryStats {
  const year = new Date().getFullYear().toString()
  const rated = items.filter((i) => i.rating != null)
  return {
    kinds: kinds.length,
    items: items.length,
    wishlist: items.filter((i) => i.status === 'wishlist').length,
    active: items.filter((i) => i.status === 'active').length,
    done: items.filter((i) => i.status === 'done').length,
    finishedThisYear: logs.filter(
      (l) => (l.event === 'finished' || l.event === 'revisited') && l.date.startsWith(year),
    ).length,
    rated: rated.length,
    meanRating: rated.length
      ? Math.round(rated.reduce((sum, i) => sum + (i.rating ?? 0), 0) / rated.length)
      : null,
    byKind: kindCounts().map((k) => ({
      kindId: k.id,
      items: k.items,
      open: k.open,
      active: items.filter((i) => i.kindId === k.id && i.status === 'active').length,
    })),
  }
}

/**
 * A stand-in for the metadata sources.
 *
 * Deliberately not a network call: the point of this file is that the whole
 * interface runs in a browser with nothing behind it. It answers anything
 * with something plausible, so the add sheet, the suggestion list and the
 * "look this up again" panel can all be designed against.
 */
function fakeResults(query: string, limit: number): SearchResult[] {
  const q = query.trim()
  if (!q) return []
  const shapes = [
    { suffix: '', creator: 'Frank Herbert', year: 1965, rating: 84 },
    { suffix: ' (Messiah)', creator: 'Frank Herbert', year: 1969, rating: 71 },
    { suffix: ': the annotated edition', creator: 'Various', year: 2019, rating: null },
    { suffix: ' — a life', creator: 'Anne Carson', year: 2011, rating: 66 },
  ]
  return shapes.slice(0, limit).map((shape, i) => ({
    title: `${q[0]!.toUpperCase()}${q.slice(1)}${shape.suffix}`,
    subtitle: '',
    creator: shape.creator,
    summary: `A plausible blurb for “${q}”, written by the mock backend so the panel has something to lay out.`,
    year: shape.year,
    url: `https://example.org/${encodeURIComponent(q)}/${i}`,
    // No image: nothing in a mock should reach the network either, and the
    // placeholder is what a real cover-less result looks like anyway.
    imageUrl: '',
    rating: shape.rating,
    ratingCount: shape.rating ? 1200 - i * 137 : null,
    facts: { author: shape.creator, pages: String(280 + i * 40) },
    source: 'openLibrary',
  }))
}

// The vault's trackers, out of the journals that used to hold them.
//
// One of them measures a goal and carries a cadence, because a habits view
// drawn against five daily checks looks finished when it is not: three runs
// a week is the shape that actually needs the arithmetic.
const trackers: Tracker[] = [
  tracker('t-walk', 'Walk the dog', 'check', 'paw', '#16a34a', {
    sortOrder: 0,
    cadence: { times: 1, per: 'day' },
  }),
  tracker('t-vitd', 'Vitamin D', 'dose', 'tablet', '#f59e0b', {
    unit: 'iu',
    defaultValue: 1000,
    sortOrder: 1,
  }),
  tracker('t-head', 'Headache', 'scale', 'bolt', '#e11d48', {
    onCalendar: true,
    sortOrder: 2,
  }),
  tracker('t-run', 'Run', 'amount', 'run', '#0284c7', {
    unit: 'min',
    defaultValue: 30,
    target: 30,
    onCalendar: true,
    sortOrder: 3,
    cadence: { times: 3, per: 'week' },
    purpose: { type: 'goal', id: 'g-run' },
  }),
  tracker('t-read', 'Pages read', 'amount', 'book', '#4f46e5', {
    unit: 'pages',
    defaultValue: 20,
    target: 20,
    sortOrder: 4,
  }),
]

const journals: Journal[] = [
  {
    id: 'j-daily',
    name: 'Daily',
    color: '#c2410c',
    icon: '\u{1f342}',
    description: 'The ordinary days',
    sortOrder: 0,
    shownTrackers: ['t-walk', 't-vitd', 't-head', 't-run', 't-read'],
    createdAt: iso(400),
    updatedAt: iso(1),
  },
  {
    id: 'j-travel',
    name: 'Travel',
    color: '#0f766e',
    icon: '\u{2708}\u{fe0f}',
    description: 'Trips, trains and long walks',
    sortOrder: 1,
    shownTrackers: [],
    createdAt: iso(300),
    updatedAt: iso(9),
  },
  {
    id: 'j-notes',
    name: 'Reading',
    color: '#4338ca',
    icon: '\u{1f4d6}',
    description: 'Books and the thoughts they caused',
    sortOrder: 2,
    shownTrackers: [],
    createdAt: iso(200),
    updatedAt: iso(20),
  },
]

const entries: Entry[] = [
  {
    id: 'e-1',
    journalId: 'j-daily',
    title: 'First frost',
    body: {
      type: 'doc',
      content: [
        ...para(
          'The grass was stiff underfoot this morning and the light came in low and orange across the field, the way it only does for about three weeks a year.',
        ),
        {
          type: 'media',
          attrs: {
            blob: 'c'.repeat(64),
            kind: 'image',
            mime: 'image/svg+xml',
            filename: 'frost.jpg',
            caption: 'The field at half past seven',
          },
        },
        ...para(
          'I stood at the gate longer than I meant to. There is a particular silence to a cold morning that I never manage to describe properly, and every year I try again.',
        ),
        {
          type: 'blockquote',
          content: para('Note to self: buy proper gloves before this gets serious.'),
        },
      ],
    },
    localDate: day(0),
    tz: 'Europe/London',
    createdAt: iso(0),
    updatedAt: iso(0),
    tags: ['winter', 'walking'],
    starred: true,
    pinned: false,
    attachments: [
      {
        blob: 'c'.repeat(64),
        kind: 'image',
        mime: 'image/svg+xml',
        filename: 'frost.jpg',
        byteLen: 2_400_000,
        width: 1200,
        height: 800,
        caption: 'The field at half past seven',
      },
    ],
  },
  {
    id: 'e-2',
    journalId: 'j-daily',
    title: 'A slow Sunday',
    body: {
      type: 'doc',
      content: [
        ...para(
          'Bread, coffee, and most of a book. Nothing happened, which was the entire point of it.',
        ),
        {
          type: 'bulletList',
          content: [
            {
              type: 'listItem',
              content: para('Finished the sourdough, finally got the crumb right'),
            },
            { type: 'listItem', content: para('Walked to the bridge and back') },
            { type: 'listItem', content: para('Did not open the laptop once') },
          ],
        },
      ],
    },
    localDate: day(1),
    tz: 'Europe/London',
    createdAt: iso(1),
    updatedAt: iso(1),
    tags: ['rest'],
    starred: false,
    pinned: false,
    attachments: [],
  },
  {
    id: 'e-3',
    journalId: 'j-travel',
    title: 'Arriving in Lisbon',
    body: {
      type: 'doc',
      content: [
        ...para(
          'The taxi driver took the long way along the river so I could see the bridge lit up. Worth every extra euro, and he knew it.',
        ),
        {
          type: 'media',
          attrs: {
            blob: 'a'.repeat(64),
            kind: 'image',
            mime: 'image/svg+xml',
            filename: 'harbour.jpg',
            caption: 'From the taxi window, somewhere near Belém',
          },
        },
        ...para(
          'The flat smells of woodsmoke and the window opens onto a courtyard where somebody was practising scales until about ten.',
        ),
      ],
    },
    localDate: day(8),
    tz: 'Europe/Lisbon',
    createdAt: iso(8),
    updatedAt: iso(7),
    tags: ['portugal', 'travel'],
    starred: true,
    pinned: true,
    location: {
      latitude: 38.7223,
      longitude: -9.1393,
      placeName: 'Alfama',
      locality: 'Lisbon',
      country: 'Portugal',
    },
    attachments: [
      {
        blob: 'a'.repeat(64),
        kind: 'image',
        mime: 'image/svg+xml',
        filename: 'harbour.jpg',
        byteLen: 3_100_000,
        width: 1200,
        height: 800,
        caption: 'From the taxi window, somewhere near Belém',
      },
    ],
  },
  {
    id: 'e-4',
    journalId: 'j-travel',
    title: 'The train to Porto',
    body: {
      type: 'doc',
      content: [
        ...para('Three hours of eucalyptus and red roofs.'),
        {
          type: 'media',
          attrs: {
            blob: 'b'.repeat(64),
            kind: 'image',
            mime: 'image/svg+xml',
            filename: 'train.jpg',
            caption: 'Somewhere past Coimbra',
          },
        },
        ...para(
          'I read one page and looked out of the window for the rest, which felt like the correct allocation of a morning.',
        ),
      ],
    },
    localDate: day(9),
    tz: 'Europe/Lisbon',
    createdAt: iso(9),
    updatedAt: iso(9),
    tags: ['portugal', 'trains'],
    starred: false,
    pinned: false,
    attachments: [
      {
        blob: 'b'.repeat(64),
        kind: 'image',
        mime: 'image/svg+xml',
        filename: 'train.jpg',
        byteLen: 2_800_000,
        width: 1200,
        height: 800,
        caption: 'Somewhere past Coimbra',
      },
    ],
  },
  {
    id: 'e-5',
    journalId: 'j-notes',
    title: 'On rereading',
    body: {
      type: 'doc',
      content: [
        ...para(
          'Started the Berger again. I remember almost none of it, which is either a failure of attention or the entire argument for rereading.',
        ),
        {
          type: 'blockquote',
          content: para('We only see what we look at. To look is an act of choice.'),
        },
        ...para('Twelve years since the last time. Different book now.'),
      ],
    },
    localDate: day(21),
    tz: 'Europe/London',
    createdAt: iso(21),
    updatedAt: iso(21),
    tags: ['books'],
    starred: false,
    pinned: false,
    attachments: [],
  },
]

// ── The task domain ──────────────────────────────────────────────────────

function ahead(days: number): string {
  return day(-days)
}

const projects: Project[] = [
  {
    id: 'p-house',
    name: 'Move house',
    purpose: { type: 'role', id: 'r-parent' },
    notes: 'Completion is the 12th. Everything hangs off that.',
    color: '#c2410c',
    icon: '\u{1f4e6}',
    status: 'active',
    priority: 'high',
    startDate: day(20),
    dueDate: ahead(12),
    estimateMinutes: 3_600,
    tags: ['home'],
    sortOrder: 0,
    createdAt: iso(40),
    updatedAt: iso(1),
  },
  {
    id: 'p-site',
    name: 'Rebuild the site',
    purpose: { type: 'goal', id: 'g-ship' },
    notes: 'Static, fast, and no analytics.',
    color: '#0f766e',
    icon: '\u{1f5a5}\u{fe0f}',
    status: 'active',
    priority: 'medium',
    dueDate: ahead(30),
    estimateMinutes: 1_800,
    tags: ['work'],
    sortOrder: 1,
    createdAt: iso(60),
    updatedAt: iso(3),
  },
  {
    id: 'p-garden',
    name: 'The garden',
    notes: '',
    color: '#15803d',
    icon: '\u{1f331}',
    status: 'paused',
    priority: 'low',
    tags: ['home'],
    sortOrder: 2,
    createdAt: iso(120),
    updatedAt: iso(30),
  },
]

let taskSeq = 0

function seedTask(t: Partial<Task> & { title: string }): Task {
  taskSeq += 1
  return {
    id: `t-${taskSeq}`,
    projectId: null,
    parentId: null,
    notes: '',
    status: 'todo',
    priority: 'none',
    tags: [],
    sortOrder: 0,
    createdAt: iso(10),
    updatedAt: iso(1),
    ...t,
  }
}

const tasks: Task[] = [
  // Move house: a board with something in most columns.
  seedTask({
    id: 't-pack',
    title: 'Pack the study',
    projectId: 'p-house',
    status: 'doing',
    priority: 'high',
    dueDate: ahead(2),
    estimateMinutes: 240,
    tags: ['moving'],
    notes: 'The books are the whole job. Everything else is an afternoon.',
    sortOrder: 0,
  }),
  seedTask({
    id: 't-books',
    title: 'Box up the books',
    projectId: 'p-house',
    parentId: 't-pack',
    status: 'done',
    completedAt: iso(1),
    estimateMinutes: 120,
    sortOrder: 0,
  }),
  seedTask({
    id: 't-shelves',
    title: 'Take the shelves down',
    projectId: 'p-house',
    parentId: 't-pack',
    dueDate: ahead(2),
    estimateMinutes: 60,
    sortOrder: 1,
  }),
  seedTask({
    id: 't-cables',
    title: 'Label the cables',
    projectId: 'p-house',
    parentId: 't-pack',
    priority: 'low',
    sortOrder: 2,
  }),
  seedTask({
    id: 't-van',
    title: 'Book the van',
    projectId: 'p-house',
    priority: 'urgent',
    dueDate: day(1),
    dueTime: '17:00:00',
    estimateMinutes: 30,
    tags: ['moving', 'money'],
    notes: 'Two quotes so far. The cheaper one has no tail lift.',
    sortOrder: 1,
  }),
  seedTask({
    id: 't-meter',
    title: 'Read the meters on the day',
    projectId: 'p-house',
    status: 'backlog',
    dueDate: ahead(12),
    sortOrder: 2,
  }),
  seedTask({
    id: 't-broadband',
    title: 'Chase the broadband transfer',
    projectId: 'p-house',
    status: 'blocked',
    priority: 'high',
    tags: ['admin'],
    notes: 'They said 5 working days on the 3rd. It has been nine.',
    sortOrder: 3,
  }),
  seedTask({
    id: 't-deposit',
    title: 'Get the deposit back',
    projectId: 'p-house',
    status: 'done',
    completedAt: iso(4),
    tags: ['money'],
    sortOrder: 4,
  }),

  // The site.
  seedTask({
    id: 't-type',
    title: 'Settle the type scale',
    projectId: 'p-site',
    status: 'doing',
    priority: 'medium',
    dueDate: ahead(1),
    estimateMinutes: 90,
    tags: ['design'],
    sortOrder: 0,
  }),
  seedTask({
    id: 't-build',
    title: 'Move the build to the new runner',
    projectId: 'p-site',
    dueDate: ahead(5),
    estimateMinutes: 180,
    tags: ['work'],
    sortOrder: 1,
  }),
  seedTask({
    id: 't-archive',
    title: 'Import the old archive',
    projectId: 'p-site',
    status: 'backlog',
    tags: ['work'],
    sortOrder: 2,
  }),

  // The inbox: captured, not filed. Including one that has gone past.
  seedTask({
    id: 't-dentist',
    title: 'Ring the dentist back',
    priority: 'high',
    dueDate: day(2),
    tags: ['admin'],
    sortOrder: 0,
  }),
  seedTask({ id: 't-boots', title: 'Resole the walking boots', sortOrder: 1 }),
  seedTask({
    id: 't-gift',
    title: 'Something for Ana\u{2019}s birthday',
    dueDate: ahead(6),
    tags: ['family'],
    sortOrder: 2,
  }),
]

function blockAt(
  id: string,
  taskId: string,
  daysAgo: number,
  hour: number,
  minutes: number,
  kind: BlockKind,
): TimeBlock {
  const start = new Date()
  start.setDate(start.getDate() - daysAgo)
  start.setHours(hour, 0, 0, 0)
  return {
    id,
    subject: { type: 'task', id: taskId },
    title: '',
    start: start.toISOString(),
    end: new Date(start.getTime() + minutes * 60_000).toISOString(),
    localDate: day(daysAgo),
    tz: Intl.DateTimeFormat().resolvedOptions().timeZone,
    allDay: false,
    kind,
    notes: '',
    tags: [],
    createdAt: iso(daysAgo),
    updatedAt: iso(daysAgo),
  }
}

const blocks: TimeBlock[] = [
  blockAt('b-1', 't-books', 1, 10, 150, 'actual'),
  blockAt('b-2', 't-pack', 0, 9, 120, 'planned'),
  blockAt('b-3', 't-type', 2, 14, 75, 'actual'),
  blockAt('b-4', 't-van', -1, 11, 30, 'planned'),
]

// ── The calendar domain ──────────────────────────────────────────────────
//
// Two subscribed calendars and a week of plausible meetings, so the grid can
// be designed against something that looks like a real Tuesday rather than
// against an empty page. One of them is deliberately in a failed state: a
// feed that cannot be reached is a thing the sidebar has to say well, and it
// is hard to get right if you never see it.

const calendars: CalendarInfo[] = [
  {
    id: 'c-work',
    name: 'Priya — Work',
    roleId: 'r-work',
    color: '#0369a1',
    origin: { type: 'url', url: 'https://calendar.google.com/calendar/ical/…/basic.ics' },
    provider: 'google',
    visible: true,
    refreshMinutes: 60,
    lastSyncedAt: iso(0),
    createdAt: iso(30),
    updatedAt: iso(0),
    events: 0,
  },
  {
    id: 'c-holidays',
    name: 'Public holidays',
    color: '#15803d',
    origin: { type: 'url', url: 'https://example.com/holidays.ics' },
    provider: 'other',
    visible: true,
    refreshMinutes: 1440,
    lastSyncedAt: iso(3),
    lastError: 'there is no calendar at that address any more. It may have been revoked.',
    createdAt: iso(60),
    updatedAt: iso(0),
    events: 0,
  },
]

function eventAt(
  id: string,
  calendarId: string,
  daysAhead: number,
  hour: number,
  minutes: number,
  title: string,
  extra: Partial<CalendarEvent> = {},
): CalendarEvent {
  const start = new Date()
  start.setDate(start.getDate() + daysAhead)
  start.setHours(hour, 0, 0, 0)
  const localDate = day(-daysAhead)
  return {
    id,
    calendarId,
    uid: `${id}@example.com`,
    title,
    description: '',
    location: '',
    start: start.toISOString(),
    end: new Date(start.getTime() + minutes * 60_000).toISOString(),
    localDate,
    endDate: localDate,
    tz: Intl.DateTimeFormat().resolvedOptions().timeZone,
    allDay: false,
    status: 'confirmed',
    organizer: '',
    url: '',
    busy: true,
    updatedAt: iso(1),
    ...extra,
  }
}

const events: CalendarEvent[] = [
  eventAt('ev-1', 'c-work', 0, 9, 15, 'Stand-up', { location: 'Meeting room 4' }),
  eventAt('ev-2', 'c-work', 0, 11, 60, 'Design review', {
    organizer: 'Priya Raman',
    description: 'Bring the two options and the numbers behind them.',
  }),
  eventAt('ev-3', 'c-work', 0, 15, 30, 'One-to-one', { status: 'tentative' }),
  eventAt('ev-4', 'c-work', 1, 9, 15, 'Stand-up', { location: 'Meeting room 4' }),
  eventAt('ev-5', 'c-work', 1, 13, 90, 'Quarterly planning'),
  eventAt('ev-6', 'c-work', 2, 9, 15, 'Stand-up', { location: 'Meeting room 4' }),
  eventAt('ev-7', 'c-work', 2, 16, 45, 'Retro', { status: 'cancelled', busy: false }),
  eventAt('ev-8', 'c-work', 3, 10, 120, 'Workshop', { location: 'Off site' }),
  eventAt('ev-9', 'c-holidays', 4, 0, 1440, 'Spring bank holiday', {
    allDay: true,
    busy: false,
  }),
]

/** Open tasks grouped by project; `null` is the inbox. Mirrors the SQL. */
function openPerProject(): Map<string | null, number> {
  const out = new Map<string | null, number>()
  for (const t of tasks) {
    if (!isOpen(t.status)) continue
    const key = t.projectId ?? null
    out.set(key, (out.get(key) ?? 0) + 1)
  }
  return out
}

function putTask(t: Task) {
  const i = tasks.findIndex((x) => x.id === t.id)
  if (i >= 0) tasks[i] = structuredClone(t)
  else tasks.push(structuredClone(t))
}

/** `roots` plus every task nested under them, at any depth. */
function subtreeOf(roots: string[]): Set<string> {
  const out = new Set(roots)
  let grew = true
  while (grew) {
    grew = false
    for (const t of tasks) {
      if (t.parentId && out.has(t.parentId) && !out.has(t.id)) {
        out.add(t.id)
        grew = true
      }
    }
  }
  return out
}

function dropTasks(doomed: Set<string>) {
  for (let i = tasks.length - 1; i >= 0; i--) {
    if (doomed.has(tasks[i]!.id)) tasks.splice(i, 1)
  }
  for (let i = blocks.length - 1; i >= 0; i--) {
    const s = blocks[i]!.subject
    if (s.type === 'task' && doomed.has(s.id)) blocks.splice(i, 1)
  }
}

/**
 * The JavaScript twin of `TaskQuery::matches` and `apply` in the core.
 *
 * It has to agree with the Rust, or the interface would be designed against
 * behaviour the real backend does not have -- which is the one way a mock
 * this thorough can still mislead.
 */
function applyTaskQuery(q: TaskQuery): Task[] {
  let rows = tasks.filter((t) => {
    const project = q.project ?? { scope: 'any' }
    if (project.scope === 'inbox' && t.projectId) return false
    if (project.scope === 'project' && t.projectId !== project.id) return false

    const parent = q.parent ?? { scope: 'any' }
    if (parent.scope === 'topLevel' && t.parentId) return false
    if (parent.scope === 'of' && t.parentId !== parent.id) return false

    if (q.statuses?.length && !q.statuses.includes(t.status)) return false
    if (q.priorityAtLeast && priorityRank(t.priority) < priorityRank(q.priorityAtLeast)) {
      return false
    }
    if (q.hasDue != null && !!t.dueDate !== q.hasDue) return false
    // An undated task is outside every date window, not inside all of them.
    if (q.dueFrom || q.dueTo) {
      if (!t.dueDate) return false
      if (q.dueFrom && t.dueDate < q.dueFrom) return false
      if (q.dueTo && t.dueDate > q.dueTo) return false
    }
    if (q.text?.trim()) {
      const hay = `${t.title}\n${t.notes}\n${t.tags.join(' ')}`.toLowerCase()
      if (!hay.includes(q.text.trim().toLowerCase())) return false
    }
    return (q.tags ?? []).every((want) =>
      t.tags.some((have) => have.toLowerCase() === want.toLowerCase()),
    )
  })

  const byOrder = (a: Task, b: Task) =>
    a.sortOrder - b.sortOrder || a.createdAt.localeCompare(b.createdAt)
  rows = [...rows].sort((a, b) => {
    switch (q.sort ?? 'manual') {
      case 'dueAsc':
        if (!a.dueDate && !b.dueDate) return byOrder(a, b)
        if (!a.dueDate) return 1
        if (!b.dueDate) return -1
        return a.dueDate.localeCompare(b.dueDate) || byOrder(a, b)
      case 'priorityDesc':
        return priorityRank(b.priority) - priorityRank(a.priority) || byOrder(a, b)
      case 'createdDesc':
        return b.createdAt.localeCompare(a.createdAt)
      case 'updatedDesc':
        return b.updatedAt.localeCompare(a.updatedAt)
      case 'completedDesc':
        return (b.completedAt ?? '').localeCompare(a.completedAt ?? '')
      case 'titleAsc':
        return a.title.localeCompare(b.title)
      default:
        return byOrder(a, b)
    }
  })

  const from = q.offset ?? 0
  return rows.slice(from, q.limit ? from + q.limit : undefined).map((t) => structuredClone(t))
}

// A reference to keep the status list honest against the core's ordering.
void TASK_STATUSES

// Start unlocked when the URL says so. Only the mock honours this, and it
// exists so the interface can be opened straight to the main view when
// reviewing or screenshotting it.
let unlocked = new URLSearchParams(location.search).has('unlocked')
/**
 * A fortnight of readings, generated rather than typed out.
 *
 * Deterministic on purpose -- a mock that reshuffles itself every reload is
 * no use for judging a layout. The shape is meant to be *realistic* rather
 * than tidy: the walk is missed twice, the headaches cluster, and two of the
 * runs are on days with no journal entry at all, because a reading does not
 * require one.
 */
const readings: Reading[] = (() => {
  const out: Reading[] = []
  let n = 0
  const push = (
    trackerId: string,
    daysAgo: number,
    value: number,
    hour: number | null,
    note = '',
  ) => {
    const d = new Date()
    d.setDate(d.getDate() - daysAgo)
    const p = (x: number) => String(x).padStart(2, '0')
    const localDate = `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`
    let at: string | null = null
    if (hour !== null) {
      const t = new Date(d)
      t.setHours(hour, (n * 7) % 60, 0, 0)
      at = t.toISOString()
    }
    out.push({
      id: `r-${n++}`,
      journalId: 'j-daily',
      trackerId,
      entryId: null,
      localDate,
      at,
      tz: Intl.DateTimeFormat().resolvedOptions().timeZone,
      value,
      note,
      createdAt: at ?? iso(daysAgo),
      updatedAt: at ?? iso(daysAgo),
    })
  }
  for (let d = 0; d < 15; d++) {
    if (d !== 4 && d !== 11) push('t-walk', d, 1, 8 + (d % 3))
    push('t-vitd', d, 1000, 8)
    if (d % 3 === 0) push('t-read', d, 15 + ((d * 7) % 30), null)
    if ([1, 3, 6, 8, 13].includes(d)) push('t-run', d, 25 + ((d * 5) % 20), 7)
    if ([2, 3, 9].includes(d)) push('t-head', d, 3 + (d % 4), 15, d === 3 ? 'after the flight' : '')
    if (d === 3) push('t-head', d, 6, 21)
  }
  return out
})()

// ── Roles and goals ──────────────────────────────────────────────────────
//
// Five roles and six goals, deliberately uneven: one role with three goals,
// one with none, one goal finished and one paused. A balance view drawn
// against a tidy set of five equal rows looks finished when it is not, and
// the interesting cases here are the empty role and the goal nothing has
// touched since March.

const roles: Role[] = [
  role('r-work', 'Work', '#0369a1', '\u{1f4bc}', 0),
  role('r-parent', 'Parent', '#c2410c', '\u{1f3e1}', 1),
  role('r-health', 'Health', '#15803d', '\u{1f331}', 2),
  role('r-friends', 'Friends', '#a21caf', '\u{1f465}', 3),
  role('r-self', 'Myself', '#4338ca', '\u{1f9ed}', 4),
]

function role(id: string, name: string, color: string, icon: string, sortOrder: number): Role {
  return {
    id,
    name,
    color,
    icon,
    notes: '',
    archived: false,
    sortOrder,
    createdAt: iso(200),
    updatedAt: iso(30),
  }
}

const goals: Goal[] = [
  goal('g-ship', 'r-work', 'Ship the vault rewrite', 'active', ahead(60), 0),
  goal('g-hire', 'r-work', 'Hire a second engineer', 'paused', null, 1),
  goal('g-review', 'r-work', 'Finish the pay review', 'done', null, 2),
  goal('g-bike', 'r-parent', 'Viya rides without stabilisers', 'active', ahead(180), 0),
  goal('g-run', 'r-health', 'Run 10k without stopping', 'active', ahead(90), 0),
  // Nothing points at this one, which is what makes it the interesting row:
  // the Overview has to say "not touched since March" well.
  goal('g-write', 'r-self', 'Write something every week', 'active', null, 0),
]

function goal(
  id: string,
  roleId: string,
  title: string,
  status: GoalStatus,
  horizon: string | null,
  sortOrder: number,
): Goal {
  return {
    id,
    roleId,
    title,
    notes: '',
    status,
    horizon,
    sortOrder,
    createdAt: iso(150),
    updatedAt: iso(10),
    completedAt: status === 'done' ? iso(20) : null,
  }
}

/**
 * Resolve one block's purpose the way the SQL does: its own, else its
 * task's, else that task's project's.
 *
 * Duplicated here rather than imported because that is what a mock backend
 * is — the *behaviour* of the store, reimplemented, so that a bug in one
 * shows up as a disagreement with the other rather than being shared by
 * both.
 */
function purposeOfBlock(b: TimeBlock): Purpose | null {
  if (b.purpose) return b.purpose
  const taskId = b.subject.type === 'task' ? b.subject.id : null
  const task = taskId ? tasks.find((t) => t.id === taskId) : undefined
  if (task?.purpose) return task.purpose
  const projectId = task?.projectId ?? (b.subject.type === 'project' ? b.subject.id : null)
  const project = projectId ? projects.find((p) => p.id === projectId) : undefined
  return project?.purpose ?? null
}

function samePurpose(a: Purpose | null, b: Purpose | null): boolean {
  if (!a || !b) return a === b
  return a.type === b.type && a.id === b.id
}

function balanceReport(from: string, to: string): BalanceReport {
  const within = (d: string) => d >= from && d <= to
  const purposes: PurposeMinutes[] = []
  for (const b of blocks) {
    if (!within(b.localDate)) continue
    const purpose = purposeOfBlock(b)
    let row = purposes.find((r) => samePurpose(r.purpose ?? null, purpose))
    if (!row) {
      row = { purpose, actualMinutes: 0, plannedMinutes: 0, blocks: 0 }
      purposes.push(row)
    }
    const minutes = Math.max(
      0,
      Math.round((new Date(b.end).getTime() - new Date(b.start).getTime()) / 60_000),
    )
    if (b.kind === 'actual') row.actualMinutes += minutes
    else row.plannedMinutes += minutes
    row.blocks += 1
  }

  const byRole = new Map<string | null, RoleEventMinutes>()
  for (const e of events) {
    if (e.endDate < from || e.localDate > to) continue
    const cal = calendars.find((c) => c.id === e.calendarId)
    const roleId = cal?.roleId ?? null
    const row = byRole.get(roleId) ?? { roleId, minutes: 0, events: 0 }
    row.minutes += Math.max(
      0,
      Math.round((new Date(e.end).getTime() - new Date(e.start).getTime()) / 60_000),
    )
    row.events += 1
    byRole.set(roleId, row)
  }
  return { purposes, events: [...byRole.values()] }
}

function goalActivity(id: string): GoalActivity {
  const points = (p: Purpose | null | undefined) => p?.type === 'goal' && p.id === id
  const out: GoalActivity = {
    openTasks: 0,
    doneTasks: 0,
    projects: projects.filter((p) => points(p.purpose)).length,
    actualMinutes: 0,
    entries: entries.filter((e) => points(e.purpose)).length,
    readings: 0,
    items: items.filter((i) => points(i.purpose)).length,
    lastTouched: null,
  }
  const touch = (at: string | null | undefined) => {
    if (at && (!out.lastTouched || at > out.lastTouched)) out.lastTouched = at
  }
  for (const t of tasks) {
    const project = t.projectId ? projects.find((p) => p.id === t.projectId) : undefined
    if (!points(t.purpose ?? project?.purpose)) continue
    if (t.status === 'done' || t.status === 'cancelled') out.doneTasks += 1
    else out.openTasks += 1
    touch(t.updatedAt)
  }
  for (const b of blocks) {
    if (b.kind !== 'actual' || !points(purposeOfBlock(b))) continue
    out.actualMinutes += Math.max(
      0,
      Math.round((new Date(b.end).getTime() - new Date(b.start).getTime()) / 60_000),
    )
    touch(b.start)
  }
  return out
}

let nextId = 100

// ── The assistant ────────────────────────────────────────────────────────
//
// A scripted one. There is no model behind the mock and there must not be:
// the whole point of this module is an interface that runs with no Rust, no
// vault and no network, and a mock that reached for an API key would be none
// of those things. What it does instead is exercise every shape the panel
// has to draw -- prose arriving in pieces, a tool card, a confirmation, and
// a failure -- so the panel can be built and reviewed without a provider.

let agentSettings: AgentSettings = {
  enabled: true,
  model: {
    provider: 'openAi',
    model: 'gpt-5.1-mini',
    baseUrl: null,
    temperature: null,
    maxTokens: null,
  },
  instructions: '',
  confirmDestructive: true,
  maxSteps: 24,
  remember: true,
  hasKey: true,
}
let agentKey = 'sk-mock'
const conversations: Conversation[] = []
const agentMessages: AgentMessage[] = []
const memories: Memory[] = [
  {
    id: 'mem-1',
    text: 'Plans the week on Sunday evening.',
    sourceId: null,
    pinned: false,
    createdAt: iso(-20),
    updatedAt: iso(-20),
  },
]

function plainText(node: unknown): string {
  if (!node || typeof node !== 'object') return ''
  const n = node as Record<string, unknown>
  if (n.type === 'text') return str(n.text)
  const caption = (n.attrs as Record<string, unknown> | undefined)?.caption
  const own = n.type === 'media' && typeof caption === 'string' ? caption : ''
  const kids = Array.isArray(n.content) ? n.content.map(plainText).join(' ') : ''
  return [own, kids].filter(Boolean).join(' ')
}

function summarize(e: Entry): EntrySummary {
  const text = plainText(e.body)
  return {
    id: e.id,
    journalId: e.journalId,
    title: e.title || text.slice(0, 60) || 'Untitled entry',
    excerpt: text.slice(0, 240),
    localDate: e.localDate,
    createdAt: e.createdAt,
    updatedAt: e.updatedAt,
    tags: e.tags,
    starred: e.starred,
    pinned: e.pinned,
    wordCount: text.split(/\s+/).filter(Boolean).length,
    attachmentCount: e.attachments.length,
    cover: e.attachments.find((a) => a.kind === 'image')?.blob,
    place: e.location?.placeName ?? e.location?.locality,
  }
}

function status(): VaultStatus {
  return {
    name: 'My Journal',
    backend: 'sqlite',
    unlocked,
    encrypted: true,
    autoLockSeconds: 900,
    forgetKeySeconds: 0,
    path: '/Users/you/Library/Application Support/EveryDay',
    // `?readonly=1` to review the read-only banner without a second process.
    writable: !new URLSearchParams(location.search).has('readonly'),
    stats: unlocked
      ? {
          journals: journals.length,
          entries: entries.length,
          blobs: Object.keys(BLOBS).length + Object.keys(COVERS).length,
          blobBytes: 8_300_000,
        }
      : undefined,
    capabilities: unlocked
      ? {
          blobs: true,
          transactional: true,
          humanReadable: false,
          tasks: true,
          calendars: true,
          library: true,
          trackers: true,
          goals: true,
          agent: true,
        }
      : undefined,
  }
}

/** The local day an instant falls on, which is what a reading is filed under. */
function localDayOf(at: string): string {
  const d = new Date(at)
  const p = (x: number) => String(x).padStart(2, '0')
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`
}

/**
 * The filter `list_readings` and `tracker_days` share, as the store does.
 *
 * `paginate` is the one thing they do not share: a cap belongs to a list,
 * and an aggregate that honoured it would return silently partial totals.
 * The real backend leaves `LIMIT` out of its `GROUP BY` for the same reason.
 */
function matchReadings(q: ReadingQuery, paginate = true): Reading[] {
  return readings
    .filter((r) => {
      if (q.journalId && r.journalId !== q.journalId) return false
      if (q.trackerIds?.length && !q.trackerIds.includes(r.trackerId)) return false
      if (q.entryId && r.entryId !== q.entryId) return false
      if (q.from && r.localDate < q.from) return false
      if (q.to && r.localDate > q.to) return false
      if (q.timedOnly && !r.at) return false
      return true
    })
    .sort(
      (a, b) =>
        a.localDate.localeCompare(b.localDate) ||
        (a.at ?? '').localeCompare(b.at ?? '') ||
        a.id.localeCompare(b.id),
    )
    .slice(0, (paginate ? q.limit : null) ?? undefined)
}

/** What the real vault does on the way in: a value has to mean something. */
function clampReading(tracker: Tracker | undefined, value: number): number {
  if (!Number.isFinite(value)) return 0
  if (!tracker) return Math.max(0, value)
  if (tracker.kind === 'check') return value > 0 ? 1 : 0
  if (tracker.kind === 'scale') return Math.min(Math.max(value, 0), tracker.scaleMax)
  return Math.max(0, value)
}

function requireUnlocked() {
  if (!unlocked) throw new VaultError('locked', 'vault is locked')
}

export const mockInvoke = async <T>(
  cmd: string,
  payload: Record<string, unknown> | Uint8Array = {},
): Promise<T> => {
  // `put_blob` is the one command whose payload is a bare buffer rather than
  // a bag of named arguments -- see `api.putBlob`. Nothing in the mock reads
  // those bytes, so the shape is normalised here and every case below can go
  // on assuming an object.
  const args: Record<string, unknown> = payload instanceof Uint8Array ? {} : payload
  // A touch of latency, so loading states are exercised rather than skipped.
  await new Promise((r) => setTimeout(r, 40))

  switch (cmd) {
    case 'bootstrap':
      return {
        vaultExists: true,
        defaultPath: '/Users/you/Library/Application Support/EveryDay',
        backends: [
          {
            id: 'sqlite',
            name: 'On this computer',
            description:
              'A SQLite database in the vault folder — fastest, works offline (recommended)',
            settings: [],
          },
          {
            id: 'postgres',
            name: 'On a Postgres server',
            description:
              'A Postgres database — Supabase or your own, reachable from more than one computer',
            settings: [
              {
                key: 'url',
                label: 'Connection URL',
                placeholder: 'postgresql://user:password@host:5432/database',
                required: true,
                secret: true,
              },
              {
                key: 'schema',
                label: 'Schema',
                placeholder: 'everyday',
                required: false,
                secret: false,
              },
            ],
          },
        ],
        status: status(),
        protocol: 1,
        // The mock is one machine with one vault. Server mode is a Rust
        // feature end to end -- the pinning, the pairing, the socket -- and
        // pretending to have paired devices here would be a screen that could
        // never be exercised against anything.
        remotes: [],
        remote: null,
      } satisfies Bootstrap as T

    case 'unlock':
      if (args.password !== PASSWORD) throw new VaultError('bad_password', 'incorrect password')
      unlocked = true
      return status() as T

    case 'create_vault':
      unlocked = true
      return status() as T

    case 'open_vault':
    case 'status':
      return status() as T

    case 'lock':
      unlocked = false
      return status() as T

    case 'change_password':
      if (args.current !== PASSWORD) throw new VaultError('bad_password', 'incorrect password')
      return undefined as T

    case 'verify_password':
      if (args.password !== PASSWORD) throw new VaultError('bad_password', 'incorrect password')
      return undefined as T

    case 'set_auto_lock':
    case 'set_forget_key':
    case 'touch':
      return undefined as T

    case 'poll_auto_lock':
      return false as T

    case 'list_journals':
      requireUnlocked()
      return [...journals].sort((a, b) => a.sortOrder - b.sortOrder) as T

    case 'new_journal': {
      requireUnlocked()
      const now = new Date().toISOString()
      return {
        id: `j-${nextId++}`,
        name: args.name as string,
        color: DEFAULT_COLORS[journals.length % DEFAULT_COLORS.length]!,
        icon: '\u{1f4d3}',
        description: '',
        sortOrder: journals.length,
        trackers: [],
        createdAt: now,
        updatedAt: now,
      } as T
    }

    case 'save_journal': {
      requireUnlocked()
      const j = args.journal as Journal
      const i = journals.findIndex((x) => x.id === j.id)
      if (i >= 0) journals[i] = j
      else journals.push(j)
      return undefined as T
    }

    case 'delete_journal': {
      requireUnlocked()
      const id = args.id as string
      journals.splice(
        journals.findIndex((j) => j.id === id),
        1,
      )
      for (let i = entries.length - 1; i >= 0; i--) {
        if (entries[i]!.journalId === id) entries.splice(i, 1)
      }
      return undefined as T
    }

    case 'list_entries': {
      requireUnlocked()
      const q = (args.query ?? {}) as EntryQuery
      let rows = entries.map(summarize)
      if (q.journalId) rows = rows.filter((r) => r.journalId === q.journalId)
      if (q.starred) rows = rows.filter((r) => r.starred)
      // Both bounds are inclusive, as the core's `EntryQuery` documents.
      // Dates are `YYYY-MM-DD`, so a string comparison is a date comparison.
      if (q.from) rows = rows.filter((r) => r.localDate >= q.from!)
      if (q.to) rows = rows.filter((r) => r.localDate <= q.to!)
      if (q.tags?.length) {
        rows = rows.filter((r) =>
          q.tags!.every((t) => r.tags.some((x) => x.toLowerCase() === t.toLowerCase())),
        )
      }
      rows.sort((a, b) => {
        if (a.pinned !== b.pinned) return a.pinned ? -1 : 1
        if (q.sort === 'dateAsc') return a.localDate.localeCompare(b.localDate)
        if (q.sort === 'titleAsc') return a.title.localeCompare(b.title)
        if (q.sort === 'updatedDesc') return b.updatedAt.localeCompare(a.updatedAt)
        return b.localDate.localeCompare(a.localDate)
      })
      return rows.slice(q.offset ?? 0, q.limit ? (q.offset ?? 0) + q.limit : undefined) as T
    }

    case 'get_entry': {
      requireUnlocked()
      const e = entries.find((x) => x.id === args.id)
      if (!e) throw new VaultError('not_found', 'entry not found')
      return structuredClone(e) as T
    }

    case 'new_entry': {
      requireUnlocked()
      const now = new Date()
      return {
        id: `e-${nextId++}`,
        journalId: args.journalId as string,
        title: '',
        body: { type: 'doc', content: [{ type: 'paragraph' }] },
        localDate: day(0),
        tz: Intl.DateTimeFormat().resolvedOptions().timeZone,
        createdAt: now.toISOString(),
        updatedAt: now.toISOString(),
        tags: [],
        starred: false,
        pinned: false,
        attachments: [],
      } satisfies Entry as T
    }

    case 'save_entry': {
      requireUnlocked()
      const e = args.entry as Entry
      const expect = args.expect as string | null
      const stored = entries.find((x) => x.id === e.id)
      // The same version check the real backend makes, so the mock can drive
      // the conflict banner without a Rust build behind it.
      if (stored ? stored.updatedAt !== expect : expect !== null) {
        throw new VaultError('conflict', 'entry was changed elsewhere since you loaded it')
      }
      const i = entries.findIndex((x) => x.id === e.id)
      if (i >= 0) entries[i] = structuredClone(e)
      else entries.unshift(structuredClone(e))
      return undefined as T
    }

    case 'save_entry_force': {
      requireUnlocked()
      const e = args.entry as Entry
      const i = entries.findIndex((x) => x.id === e.id)
      if (i >= 0) entries[i] = structuredClone(e)
      else entries.unshift(structuredClone(e))
      return undefined as T
    }

    case 'delete_entry': {
      requireUnlocked()
      entries.splice(
        entries.findIndex((e) => e.id === args.id),
        1,
      )
      return undefined as T
    }

    case 'search': {
      requireUnlocked()
      const q = str(args.query).trim().toLowerCase()
      if (!q) return [] as T
      const hits: SearchHit[] = []
      for (const e of entries) {
        if (args.journalId && e.journalId !== args.journalId) continue
        const text = `${e.title}\n${plainText(e.body)}\n${e.tags.join(' ')}`
        const at = text.toLowerCase().indexOf(q)
        if (at < 0) continue
        const start = Math.max(0, at - 60)
        const snippet = (start > 0 ? '…' : '') + text.slice(start, at + 140)
        const rel = at - start + (start > 0 ? 1 : 0)
        hits.push({
          id: e.id,
          journalId: e.journalId,
          title: e.title,
          localDate: e.localDate,
          score: 1,
          snippet,
          highlights: [[rel, rel + q.length]],
        })
      }
      return hits.slice(0, Number(args.limit ?? 10)) as T
    }

    case 'put_blob':
      requireUnlocked()
      return 'd'.repeat(64) as T

    // There is no window to close in the mock, but the command must exist
    // so the close handshake does not throw if something calls it.
    case 'ready_to_close':
      return undefined as T

    case 'list_tags':
      requireUnlocked()
      return [...new Set(entries.flatMap((e) => e.tags))].sort() as T

    // ── The task domain ──────────────────────────────────────────────

    case 'list_projects':
      requireUnlocked()
      return [...projects].sort((a, b) => a.sortOrder - b.sortOrder) as T

    case 'new_project': {
      requireUnlocked()
      const now = new Date().toISOString()
      return {
        id: `p-${nextId++}`,
        name: args.name as string,
        notes: '',
        color: DEFAULT_COLORS[projects.length % DEFAULT_COLORS.length]!,
        icon: '\u{1f5c2}\u{fe0f}',
        status: 'active',
        priority: 'none',
        tags: [],
        sortOrder: projects.length,
        createdAt: now,
        updatedAt: now,
      } satisfies Project as T
    }

    case 'save_project': {
      requireUnlocked()
      const p = args.project as Project
      const i = projects.findIndex((x) => x.id === p.id)
      if (i >= 0) projects[i] = structuredClone(p)
      else projects.push(structuredClone(p))
      return undefined as T
    }

    case 'delete_project': {
      requireUnlocked()
      const id = args.id as string
      const doomed = subtreeOf(tasks.filter((t) => t.projectId === id).map((t) => t.id))
      dropTasks(doomed)
      for (let i = blocks.length - 1; i >= 0; i--) {
        const subject = blocks[i]!.subject
        if (subject.type === 'project' && subject.id === id) blocks.splice(i, 1)
      }
      projects.splice(
        projects.findIndex((p) => p.id === id),
        1,
      )
      return undefined as T
    }

    case 'list_tasks': {
      requireUnlocked()
      return applyTaskQuery(args.query ?? {}) as T
    }

    case 'get_task': {
      requireUnlocked()
      const t = tasks.find((x) => x.id === args.id)
      if (!t) throw new VaultError('not_found', 'task not found')
      return structuredClone(t) as T
    }

    case 'new_task': {
      requireUnlocked()
      const now = new Date().toISOString()
      return {
        id: `t-${nextId++}`,
        projectId: (args.projectId as string | null) ?? null,
        parentId: (args.parentId as string | null) ?? null,
        title: '',
        notes: '',
        status: (args.status as TaskStatus | null) ?? 'todo',
        priority: 'none',
        tags: [],
        sortOrder: 0,
        createdAt: now,
        updatedAt: now,
      } satisfies Task as T
    }

    case 'save_task':
      requireUnlocked()
      putTask(args.task as Task)
      return undefined as T

    case 'save_tasks':
      requireUnlocked()
      for (const t of args.tasks as Task[]) putTask(t)
      return undefined as T

    case 'delete_task':
      requireUnlocked()
      dropTasks(subtreeOf([args.id as string]))
      return undefined as T

    case 'list_blocks': {
      requireUnlocked()
      const q = (args.query ?? {}) as BlockQuery
      return blocks
        .filter((b) => {
          if (q.from && b.localDate < q.from) return false
          if (q.to && b.localDate > q.to) return false
          if (q.kind && b.kind !== q.kind) return false
          if (q.taskId && !(b.subject.type === 'task' && b.subject.id === q.taskId)) return false
          if (q.projectId && !(b.subject.type === 'project' && b.subject.id === q.projectId)) {
            return false
          }
          return true
        })
        .sort((a, b) => a.start.localeCompare(b.start))
        .map((b) => structuredClone(b)) as T
    }

    case 'new_block': {
      requireUnlocked()
      const start = new Date(args.start as string)
      const minutes = args.minutes as number
      const now = new Date().toISOString()
      const p = (n: number) => String(n).padStart(2, '0')
      return {
        id: `b-${nextId++}`,
        subject: args.subject as BlockSubject,
        title: '',
        start: start.toISOString(),
        end: new Date(start.getTime() + minutes * 60_000).toISOString(),
        localDate: `${start.getFullYear()}-${p(start.getMonth() + 1)}-${p(start.getDate())}`,
        tz: Intl.DateTimeFormat().resolvedOptions().timeZone,
        allDay: false,
        kind: (args.kind as BlockKind | null) ?? 'planned',
        notes: '',
        tags: [],
        createdAt: now,
        updatedAt: now,
      } satisfies TimeBlock as T
    }

    case 'save_block': {
      requireUnlocked()
      const b = args.block as TimeBlock
      const i = blocks.findIndex((x) => x.id === b.id)
      if (i >= 0) blocks[i] = structuredClone(b)
      else blocks.push(structuredClone(b))
      return undefined as T
    }

    case 'delete_block':
      requireUnlocked()
      blocks.splice(
        blocks.findIndex((b) => b.id === args.id),
        1,
      )
      return undefined as T

    case 'task_tags': {
      requireUnlocked()
      const counts = new Map<string, number>()
      for (const tags of [
        ...projects.map((p) => p.tags),
        ...tasks.map((t) => t.tags),
        ...blocks.map((b) => b.tags),
      ]) {
        for (const tag of tags) counts.set(tag, (counts.get(tag) ?? 0) + 1)
      }
      return [...counts]
        .map(([tag, count]) => ({ tag, count }))
        .sort((a, b) => b.count - a.count || a.tag.localeCompare(b.tag)) as T
    }

    case 'task_stats': {
      requireUnlocked()
      const minutes = (kind: BlockKind) =>
        blocks
          .filter((b) => b.kind === kind)
          .reduce(
            (sum, b) =>
              sum + Math.max(0, Math.round((Date.parse(b.end) - Date.parse(b.start)) / 60_000)),
            0,
          )
      return {
        projects: projects.length,
        activeProjects: projects.filter((p) => p.status === 'active' || p.status === 'paused')
          .length,
        tasks: tasks.length,
        openTasks: tasks.filter((t) => isOpen(t.status)).length,
        doneTasks: tasks.filter((t) => t.status === 'done').length,
        blocks: blocks.length,
        loggedMinutes: minutes('actual'),
        plannedMinutes: minutes('planned'),
        dueToday: tasks.filter((t) => isOpen(t.status) && t.dueDate && t.dueDate <= day(0)).length,
        overdue: tasks.filter((t) => isOpen(t.status) && t.dueDate && t.dueDate < day(0)).length,
        openByProject: [...openPerProject()].map(([projectId, open]) => ({
          projectId,
          open,
        })),
      } satisfies TaskStats as T
    }

    // ── The calendar domain ──────────────────────────────────────────

    case 'list_calendars':
      requireUnlocked()
      return calendars.map((c) => ({
        ...c,
        events: events.filter((e) => e.calendarId === c.id).length,
      })) as T

    case 'save_calendar': {
      requireUnlocked()
      const c = args.calendar as Calendar
      const i = calendars.findIndex((x) => x.id === c.id)
      const withCount = { ...c, events: events.filter((e) => e.calendarId === c.id).length }
      if (i >= 0) calendars[i] = withCount
      else calendars.push(withCount)
      return undefined as T
    }

    case 'delete_calendar': {
      requireUnlocked()
      const id = args.id as string
      const i = calendars.findIndex((c) => c.id === id)
      if (i >= 0) calendars.splice(i, 1)
      for (let j = events.length - 1; j >= 0; j--) {
        if (events[j]!.calendarId === id) events.splice(j, 1)
      }
      return undefined as T
    }

    case 'list_events': {
      requireUnlocked()
      const q = (args.query ?? {}) as EventQuery
      const visible = new Set(calendars.filter((c) => c.visible).map((c) => c.id))
      return events
        .filter((e) => {
          // The same overlap test the SQL does: any day the event covers.
          if (q.from && e.endDate < q.from) return false
          if (q.to && e.localDate > q.to) return false
          if (q.calendarId && e.calendarId !== q.calendarId) return false
          if (q.visibleOnly && !visible.has(e.calendarId)) return false
          if (q.text?.trim()) {
            const hay = `${e.title}\n${e.description}\n${e.location}`.toLowerCase()
            if (!hay.includes(q.text.trim().toLowerCase())) return false
          }
          return true
        })
        .sort((a, b) => Number(b.allDay) - Number(a.allDay) || a.start.localeCompare(b.start))
        .slice(0, q.limit ?? undefined) as T
    }

    case 'get_event': {
      requireUnlocked()
      const e = events.find((x) => x.id === args.id)
      if (!e) throw new VaultError('not_found', `event ${str(args.id)} not found`)
      return e as T
    }

    case 'subscribe_calendar':
    case 'import_calendar': {
      requireUnlocked()
      // No network here, obviously. What the mock does provide is the two
      // shapes the interface has to handle -- a subscription that works and
      // one that does not -- so both paths through the sheet are reachable
      // without a server to point at.
      const address = str(args.url) || str(args.label)
      if (/fail|nope|localhost/i.test(address)) {
        throw new VaultError(
          'network',
          'there is no calendar at that address any more. It may have been revoked.',
        )
      }
      const added: CalendarInfo = {
        id: `c-${Math.random().toString(36).slice(2, 8)}`,
        name: str(args.name).trim() || 'Subscribed calendar',
        color: str(args.color, '#4338ca'),
        origin:
          cmd === 'import_calendar'
            ? { type: 'file', label: str(args.label, 'calendar.ics') }
            : { type: 'url', url: address },
        provider: /google/i.test(address)
          ? 'google'
          : /outlook|office/i.test(address)
            ? 'outlook'
            : /icloud/i.test(address)
              ? 'apple'
              : 'other',
        visible: true,
        refreshMinutes: cmd === 'import_calendar' ? 0 : 60,
        lastSyncedAt: new Date().toISOString(),
        createdAt: new Date().toISOString(),
        updatedAt: new Date().toISOString(),
        events: 0,
      }
      calendars.push(added)
      return added as T
    }

    case 'sync_calendar':
    case 'sync_due_calendars': {
      requireUnlocked()
      const reports: SyncReport[] = calendars
        .filter((c) => c.origin.type === 'url' && !c.lastError)
        .map((c) => ({
          calendarId: c.id,
          events: events.filter((e) => e.calendarId === c.id).length,
          skipped: 0,
        }))
      return (cmd === 'sync_calendar' ? (reports[0] ?? { events: 0, skipped: 0 }) : reports) as T
    }

    case 'new_tracker': {
      requireUnlocked()
      const now = new Date().toISOString()
      return {
        id: `t-${nextId++}`,
        name: str(args.name),
        kind: args.kind as TrackerKind,
        icon: 'dot',
        color: '#e11d48',
        unit: '',
        defaultValue: 1,
        target: null,
        scaleMax: 10,
        onCalendar: false,
        archived: false,
        sortOrder: 0,
        createdAt: now,
        updatedAt: now,
      } satisfies Tracker as T
    }

    case 'list_readings': {
      requireUnlocked()
      return matchReadings((args.query ?? {}) as ReadingQuery).map((r) => structuredClone(r)) as T
    }

    case 'tracker_days': {
      requireUnlocked()
      const days = new Map<string, TrackerDay>()
      for (const r of matchReadings(args.query ?? {}, false)) {
        const key = `${r.localDate}/${r.trackerId}`
        const d = days.get(key) ?? {
          trackerId: r.trackerId,
          date: r.localDate,
          count: 0,
          sum: 0,
          max: 0,
          firstAt: null,
          lastAt: null,
        }
        d.count += 1
        d.sum += r.value
        d.max = Math.max(d.max, r.value)
        if (r.at) {
          d.firstAt = d.firstAt && d.firstAt < r.at ? d.firstAt : r.at
          d.lastAt = d.lastAt && d.lastAt > r.at ? d.lastAt : r.at
        }
        days.set(key, d)
      }
      return [...days.values()].sort(
        (a, b) => a.date.localeCompare(b.date) || a.trackerId.localeCompare(b.trackerId),
      ) as T
    }

    case 'log_reading': {
      requireUnlocked()
      const date = str(args.date)
      const today = day(0)
      const at = (args.at as string | null) ?? (date === today ? new Date().toISOString() : null)
      const tracker = trackers.find((t) => t.id === args.trackerId)
      const raw = Number(args.value)
      const value = clampReading(tracker, raw)
      const now = new Date().toISOString()
      const reading: Reading = {
        id: `r-${nextId++}`,
        // Both optional: a reading logged from the Overview or the tray was
        // ticked on no page at all.
        journalId: (args.journalId as string | null) ?? null,
        trackerId: str(args.trackerId),
        entryId: (args.entryId as string | null) ?? null,
        localDate: at ? localDayOf(at) : date,
        at,
        tz: Intl.DateTimeFormat().resolvedOptions().timeZone,
        value,
        note: '',
        createdAt: now,
        updatedAt: now,
      }
      readings.push(reading)
      return structuredClone(reading) as T
    }

    case 'save_reading': {
      requireUnlocked()
      const r = structuredClone(args.reading as Reading)
      const tracker = trackers.find((t) => t.id === r.trackerId)
      r.value = clampReading(tracker, r.value)
      r.updatedAt = new Date().toISOString()
      const i = readings.findIndex((x) => x.id === r.id)
      if (i >= 0) readings[i] = r
      else readings.push(r)
      return undefined as T
    }

    case 'delete_reading': {
      requireUnlocked()
      const i = readings.findIndex((r) => r.id === args.id)
      if (i >= 0) readings.splice(i, 1)
      return undefined as T
    }

    case 'list_trackers':
      requireUnlocked()
      return structuredClone(trackers) as T

    case 'save_tracker': {
      requireUnlocked()
      const t = structuredClone(args.tracker as Tracker)
      const at = trackers.findIndex((x) => x.id === t.id)
      if (at >= 0) trackers[at] = t
      else trackers.push(t)
      return undefined as T
    }

    case 'merge_trackers': {
      requireUnlocked()
      const from = str(args.from)
      const into = str(args.into)
      let moved = 0
      for (const r of readings) {
        if (r.trackerId === from) {
          r.trackerId = into
          moved++
        }
      }
      const at = trackers.findIndex((t) => t.id === from)
      if (at >= 0) trackers.splice(at, 1)
      // Every journal drawing the old chip draws the new one; left alone,
      // the strip would silently stop showing anything.
      for (const j of journals) {
        if (!j.shownTrackers.includes(from)) continue
        j.shownTrackers = j.shownTrackers.filter((id) => id !== from)
        if (!j.shownTrackers.includes(into)) j.shownTrackers.push(into)
      }
      return moved as T
    }

    case 'delete_tracker': {
      requireUnlocked()
      const id = str(args.id)
      const at = trackers.findIndex((t) => t.id === id)
      if (at >= 0) trackers.splice(at, 1)
      let gone = 0
      for (let i = readings.length - 1; i >= 0; i--) {
        if (readings[i]!.trackerId === id) {
          readings.splice(i, 1)
          gone++
        }
      }
      return gone as T
    }

    case 'calendar_providers':
      requireUnlocked()
      return [
        {
          id: 'google',
          label: 'Google Calendar',
          hint: 'Settings \u2192 your calendar \u2192 Integrate calendar \u2192 Secret address in iCal format.',
        },
        {
          id: 'outlook',
          label: 'Outlook',
          hint: 'Settings \u2192 Calendar \u2192 Shared calendars \u2192 Publish a calendar, then copy the ICS link.',
        },
        {
          id: 'apple',
          label: 'Apple Calendar',
          hint: 'iCloud.com \u2192 Calendar \u2192 the share icon beside a calendar \u2192 Public Calendar, then copy the link.',
        },
        {
          id: 'other',
          label: 'Another calendar',
          hint: 'Any address publishing an iCalendar (.ics) feed \u2014 a team calendar, a fixture list, your country\u2019s public holidays.',
        },
      ] satisfies ProviderInfo[] as T

    // ── The library domain ───────────────────────────────────────────

    case 'list_kinds':
      requireUnlocked()
      return kindCounts() as T

    case 'new_kind': {
      requireUnlocked()
      const name = str(args.name, 'Shelf')
      const singular = str(args.singular) || name
      const now = new Date().toISOString()
      return {
        id: `k-${Math.random().toString(36).slice(2, 8)}`,
        slug:
          singular
            .toLowerCase()
            .replace(/[^a-z0-9]/g, '')
            .slice(0, 24) || 'custom',
        name,
        singular,
        icon: '\u{1f516}',
        color: DEFAULT_COLORS[kinds.length % DEFAULT_COLORS.length]!,
        verbs: verbs('To try', 'Underway', 'Done', 'finished'),
        fields: [],
        source: '',
        progressUnit: '',
        sortOrder: kinds.length,
        builtin: false,
        visible: true,
        createdAt: now,
        updatedAt: now,
      } satisfies Kind as T
    }

    case 'save_kind': {
      requireUnlocked()
      const kind = args.kind as Kind
      const at = kinds.findIndex((k) => k.id === kind.id)
      const withCounts: KindInfo = { ...kind, items: 0, open: 0 }
      if (at >= 0) kinds[at] = { ...kinds[at]!, ...kind }
      else kinds.push(withCounts)
      return undefined as T
    }

    case 'delete_kind': {
      requireUnlocked()
      const id = str(args.id)
      // The cascade the real backend makes: the shelf, its items, their log.
      const doomed = new Set(items.filter((i) => i.kindId === id).map((i) => i.id))
      for (let i = logs.length - 1; i >= 0; i--) if (doomed.has(logs[i]!.itemId)) logs.splice(i, 1)
      for (let i = items.length - 1; i >= 0; i--) if (doomed.has(items[i]!.id)) items.splice(i, 1)
      const at = kinds.findIndex((k) => k.id === id)
      if (at >= 0) kinds.splice(at, 1)
      return undefined as T
    }

    case 'list_items': {
      requireUnlocked()
      const q = (args.query ?? {}) as ItemQuery
      return applyItemQuery(q) as T
    }

    case 'get_item': {
      requireUnlocked()
      const item = items.find((i) => i.id === args.id)
      if (!item) throw new VaultError('not_found', 'no such item')
      return item as T
    }

    case 'add_item': {
      requireUnlocked()
      const title = str(args.title).trim()
      if (!title) throw new VaultError('invalid', 'give it a name and it will be added')
      const item = seedItem({
        kindId: str(args.kindId),
        title,
        createdAt: new Date().toISOString(),
        updatedAt: new Date().toISOString(),
      })
      // The lookup is best-effort in the real backend too: the item lands
      // either way, and `lookedUp` says whether anything was found.
      const hit = args.lookup ? fakeResults(title, 1)[0] : undefined
      if (hit) {
        item.creator = hit.creator
        item.year = hit.year ?? null
        item.summary = hit.summary
        item.source = hit.source
        item.external = hit.rating
          ? [{ source: 'Open Library', score: hit.rating, count: hit.ratingCount, url: hit.url }]
          : []
        item.links = [{ label: 'Open Library', url: hit.url }]
      }
      items.unshift(item)
      return { item, lookedUp: Boolean(hit) } satisfies AddedItem as T
    }

    case 'save_item': {
      requireUnlocked()
      const item = args.item as Item
      const at = items.findIndex((i) => i.id === item.id)
      if (at >= 0) items[at] = item
      else items.unshift(item)
      return undefined as T
    }

    case 'save_items': {
      requireUnlocked()
      for (const item of args.items as Item[]) {
        const at = items.findIndex((i) => i.id === item.id)
        if (at >= 0) items[at] = item
        else items.unshift(item)
      }
      return undefined as T
    }

    case 'delete_item': {
      requireUnlocked()
      const id = str(args.id)
      for (let i = logs.length - 1; i >= 0; i--) if (logs[i]!.itemId === id) logs.splice(i, 1)
      const at = items.findIndex((i) => i.id === id)
      if (at >= 0) items.splice(at, 1)
      return undefined as T
    }

    case 'set_item_status': {
      requireUnlocked()
      const item = items.find((i) => i.id === args.id)
      if (!item) throw new VaultError('not_found', 'no such item')
      const status = args.status as ItemStatus
      const today = day(0)
      item.status = status
      // The same date bookkeeping `Item::set_status` does: fill a blank,
      // never overwrite what somebody set.
      if (status === 'active' && !item.startedOn) item.startedOn = today
      if (status === 'done') {
        item.startedOn ??= today
        item.finishedOn ??= today
      }
      if (status === 'wishlist') {
        item.startedOn = null
        item.finishedOn = null
      }
      item.updatedAt = new Date().toISOString()
      const event: LogEvent | null =
        status === 'active'
          ? 'started'
          : status === 'done'
            ? 'finished'
            : status === 'paused' || status === 'abandoned'
              ? 'stopped'
              : null
      if (args.log && event) logs.push(newLogEntry(item.id, event))
      return item as T
    }

    case 'set_item_progress': {
      requireUnlocked()
      const item = items.find((i) => i.id === args.id)
      if (!item) throw new VaultError('not_found', 'no such item')
      const position = Number(args.position) || 0
      const kind = kinds.find((k) => k.id === item.kindId)
      const total = (args.total as number | null) ?? item.progress?.total ?? null
      item.progress = { position, total, unit: item.progress?.unit || kind?.progressUnit || 'step' }
      if (item.status === 'wishlist') {
        item.status = 'active'
        item.startedOn ??= day(0)
      }
      item.updatedAt = new Date().toISOString()
      if (args.log) {
        const entry = newLogEntry(item.id, 'progress')
        entry.position = position
        logs.push(entry)
      }
      return item as T
    }

    case 'list_logs': {
      requireUnlocked()
      const q = (args.query ?? {}) as LogQuery
      const rows = logs
        .filter((l) => {
          if (q.itemId && l.itemId !== q.itemId) return false
          if (q.from && l.date < q.from) return false
          if (q.to && l.date > q.to) return false
          return !q.events?.length || q.events.includes(l.event)
        })
        .sort((a, b) => b.date.localeCompare(a.date) || b.createdAt.localeCompare(a.createdAt))
      return (q.limit == null ? rows : rows.slice(0, q.limit)) as T
    }

    case 'new_log':
      requireUnlocked()
      return newLogEntry(str(args.itemId), args.event as LogEvent) as T

    case 'save_log': {
      requireUnlocked()
      const entry = args.log as LogEntry
      const at = logs.findIndex((l) => l.id === entry.id)
      if (at >= 0) logs[at] = entry
      else logs.push(entry)
      return undefined as T
    }

    case 'delete_log': {
      requireUnlocked()
      const at = logs.findIndex((l) => l.id === args.id)
      if (at >= 0) logs.splice(at, 1)
      return undefined as T
    }

    case 'library_stats':
      requireUnlocked()
      return libraryStats() as T

    // ── Roles and goals ──────────────────────────────────────────────

    case 'list_roles': {
      requireUnlocked()
      return roles.map((r) => ({
        ...r,
        goals: goals.filter((g) => g.roleId === r.id).length,
        open: goals.filter((g) => g.roleId === r.id && goalIsOpen(g.status)).length,
      })) as T
    }

    case 'new_role': {
      requireUnlocked()
      const now = new Date().toISOString()
      return {
        id: `r-${nextId++}`,
        name: str(args.name, 'Role').trim(),
        color: '#c2410c',
        icon: '\u{1f9ed}',
        notes: '',
        archived: false,
        sortOrder: roles.length,
        createdAt: now,
        updatedAt: now,
      } as T
    }

    case 'save_role': {
      requireUnlocked()
      const role = args.role as Role
      const at = roles.findIndex((r) => r.id === role.id)
      if (at >= 0) roles[at] = role
      else roles.push(role)
      return undefined as T
    }

    case 'delete_role': {
      requireUnlocked()
      const id = str(args.id)
      // Refused while goals point at it, as the real store refuses — so the
      // interface's message for this case can actually be seen.
      const held = goals.filter((g) => g.roleId === id).length
      if (held > 0) {
        throw new VaultError(
          'invalid',
          `this role still has ${held} goal${held === 1 ? '' : 's'} under it. ` +
            'Move them to another role, or archive this one to keep its history.',
        )
      }
      const at = roles.findIndex((r) => r.id === id)
      if (at >= 0) roles.splice(at, 1)
      return undefined as T
    }

    case 'seed_roles':
      requireUnlocked()
      // The mock ships with roles, so this always answers zero — which is
      // the branch the Overview takes on a vault that already has some.
      return 0 as T

    case 'list_goals': {
      requireUnlocked()
      const query = (args.query ?? {}) as GoalQuery
      let out = goals.slice()
      if (query.roleId) out = out.filter((g) => g.roleId === query.roleId)
      if (query.statuses?.length) out = out.filter((g) => query.statuses!.includes(g.status))
      if (query.horizonTo) out = out.filter((g) => !!g.horizon && g.horizon <= query.horizonTo!)
      out.sort(
        (a, b) =>
          a.sortOrder - b.sortOrder || a.title.toLowerCase().localeCompare(b.title.toLowerCase()),
      )
      if (query.limit) out = out.slice(0, query.limit)
      return structuredClone(out) as T
    }

    case 'get_goal': {
      requireUnlocked()
      const goal = goals.find((g) => g.id === str(args.id))
      if (!goal) throw new VaultError('notFound', 'no such goal')
      return structuredClone(goal) as T
    }

    case 'new_goal': {
      requireUnlocked()
      const now = new Date().toISOString()
      return {
        id: `g-${nextId++}`,
        roleId: str(args.roleId),
        title: str(args.title).trim(),
        notes: '',
        status: 'active',
        horizon: null,
        sortOrder: goals.length,
        createdAt: now,
        updatedAt: now,
        completedAt: null,
      } as T
    }

    case 'save_goal':
    case 'save_goals': {
      requireUnlocked()
      const incoming = (cmd === 'save_goal' ? [args.goal] : (args.goals ?? [])) as Goal[]
      for (const goal of incoming) {
        if (!roles.some((r) => r.id === goal.roleId)) {
          throw new VaultError('notFound', 'no such role')
        }
        const at = goals.findIndex((g) => g.id === goal.id)
        if (at >= 0) goals[at] = goal
        else goals.push(goal)
      }
      return undefined as T
    }

    case 'delete_goal': {
      requireUnlocked()
      const at = goals.findIndex((g) => g.id === str(args.id))
      if (at >= 0) goals.splice(at, 1)
      return undefined as T
    }

    case 'time_by_purpose':
      requireUnlocked()
      return balanceReport(str(args.from), str(args.to)) as T

    case 'goal_activity':
      requireUnlocked()
      return goalActivity(str(args.id)) as T

    // ── Web search ───────────────────────────────────────────────────

    case 'web_search': {
      requireUnlocked()
      const request = (args.request ?? {}) as SearchRequest
      return fakeResults(request.query, request.limit || 8) as T
    }

    case 'lookup_metadata':
      requireUnlocked()
      return fakeResults(str(args.query), (args.limit as number) ?? 8) as T

    case 'search_sources':
      return [
        { id: 'web', label: 'the web', hasImages: false },
        { id: 'wikipedia', label: 'Wikipedia', hasImages: true },
        { id: 'openLibrary', label: 'Open Library', hasImages: true },
        { id: 'itunes', label: 'iTunes', hasImages: true },
        { id: 'nominatim', label: 'OpenStreetMap', hasImages: false },
      ] satisfies SourceInfo[] as T

    case 'apply_metadata': {
      requireUnlocked()
      const item = items.find((i) => i.id === args.id)
      if (!item) throw new VaultError('not_found', 'no such item')
      const hit = args.result as SearchResult
      const overwrite = Boolean(args.overwrite)
      // The rule the core enforces and this has to mirror or the mock lies:
      // metadata fills gaps, and never touches notes, your rating or status.
      const fill = (current: string, next: string) =>
        next.trim() && (overwrite || !current.trim()) ? next.trim() : current
      item.title = fill(item.title, hit.title)
      item.creator = fill(item.creator, hit.creator)
      item.summary = hit.summary || item.summary
      if (hit.year != null && (overwrite || item.year == null)) item.year = hit.year
      const kind = kinds.find((k) => k.id === item.kindId)
      for (const [key, value] of Object.entries(hit.facts)) {
        if (!kind?.fields.some((f) => f.key === key)) continue
        if (overwrite || !item.facts[key]) item.facts[key] = value
      }
      if (hit.rating != null) {
        const label = 'Open Library'
        const existing = item.external.find((r) => r.source === label)
        const rating = { source: label, score: hit.rating, count: hit.ratingCount, url: hit.url }
        if (existing) Object.assign(existing, rating)
        else item.external.push(rating)
      }
      if (hit.url && !item.links.some((l) => l.url === hit.url)) {
        item.links.push({ label: 'Open Library', url: hit.url })
      }
      item.source = item.source && !overwrite ? item.source : hit.source
      item.updatedAt = new Date().toISOString()
      return item as T
    }

    case 'fetch_image':
      requireUnlocked()
      // Any of the seeded jackets, so the "get the cover" button visibly
      // does something without a network.
      return (Object.keys(COVERS)[0] ?? '1'.repeat(64)) as T

    case 'agent_settings':
      requireUnlocked()
      return { ...agentSettings, hasKey: agentKey !== '' } as T

    case 'save_agent_settings': {
      requireUnlocked()
      agentSettings = args.settings as AgentSettings
      return { ...agentSettings, hasKey: agentKey !== '' } as T
    }

    case 'set_agent_key':
      requireUnlocked()
      agentKey = str(args.key)
      return undefined as T

    case 'clear_agent_key':
      requireUnlocked()
      agentKey = ''
      return undefined as T

    case 'list_conversations': {
      requireUnlocked()
      const rows = [...conversations]
        .sort((a, b) => b.updatedAt.localeCompare(a.updatedAt))
        .map((c) => ({
          ...c,
          messages: agentMessages.filter((m) => m.conversationId === c.id).length,
        }))
      const limit = args.limit as number | undefined
      return (limit == null ? rows : rows.slice(0, limit)) as T
    }

    case 'new_conversation': {
      requireUnlocked()
      const now = new Date().toISOString()
      return { id: `conv-${nextId++}`, title: '', createdAt: now, updatedAt: now } as T
    }

    case 'conversation_messages':
      requireUnlocked()
      return agentMessages.filter((m) => m.conversationId === args.id) as T

    case 'delete_conversation': {
      requireUnlocked()
      const i = conversations.findIndex((c) => c.id === args.id)
      if (i >= 0) conversations.splice(i, 1)
      for (let n = agentMessages.length - 1; n >= 0; n--) {
        if (agentMessages[n]?.conversationId === args.id) agentMessages.splice(n, 1)
      }
      return undefined as T
    }

    case 'confirm_tool_call':
      requireUnlocked()
      return mockConfirm(str(args.callId), args.approved === true) as T

    case 'list_memories':
      requireUnlocked()
      return memories as T

    case 'save_memory': {
      requireUnlocked()
      const memory = { ...(args.memory as Memory), pinned: true }
      const i = memories.findIndex((m) => m.id === memory.id)
      if (i >= 0) memories[i] = memory
      else memories.push(memory)
      return [] as T
    }

    case 'delete_memory': {
      requireUnlocked()
      const i = memories.findIndex((m) => m.id === args.id)
      if (i >= 0) memories.splice(i, 1)
      return undefined as T
    }

    default:
      throw new VaultError('unknown', `no mock for command ${cmd}`)
  }
}

/** Confirmations the scripted turn below is waiting on. */
const mockPending = new Map<string, (approved: boolean) => void>()

function mockConfirm(callId: string, approved: boolean): boolean {
  const resolve = mockPending.get(callId)
  if (!resolve) return false
  mockPending.delete(callId)
  resolve(approved)
  return true
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))

/**
 * A scripted turn.
 *
 * Chosen to walk the panel through every state it has to draw rather than to
 * be convincing: a word-at-a-time reply always, a tool card when the prompt
 * mentions a task, and a confirmation when it mentions deleting. Typing
 * "fail" gets the failure path, which is otherwise the hardest state to see.
 */
export async function mockSendMessage(
  conversationId: string,
  prompt: string,
  _context: string | null,
  onEvent: (event: AgentEvent) => void,
): Promise<void> {
  requireUnlocked()
  if (!agentSettings.enabled) {
    throw new VaultError('invalid', 'the assistant is switched off')
  }

  const now = new Date().toISOString()
  if (!conversations.some((c) => c.id === conversationId)) {
    conversations.push({ id: conversationId, title: '', createdAt: now, updatedAt: now })
  }
  const thread = conversations.find((c) => c.id === conversationId)
  if (thread) {
    thread.updatedAt = now
    if (!thread.title) thread.title = prompt.split(/\s+/).slice(0, 8).join(' ')
  }
  agentMessages.push({
    id: `msg-${nextId++}`,
    conversationId,
    role: 'user',
    content: prompt,
    toolCalls: [],
    toolCallId: null,
    failed: false,
    createdAt: now,
  })

  const messageId = `msg-${nextId++}`
  onEvent({ type: 'started', messageId })

  const lower = prompt.toLowerCase()
  if (lower.includes('fail')) {
    await sleep(300)
    onEvent({ type: 'failed', message: 'The API key was refused. Check it in Settings.' })
    return
  }

  let reply: string
  if (lower.includes('delet')) {
    const callId = 'call-mock-delete'
    onEvent({
      type: 'confirmationRequired',
      callId,
      name: 'delete_task',
      subject: 'Order the timber',
      arguments: { task_id: '0192f3a1-mock' },
    })
    const approved = await new Promise<boolean>((resolve) => mockPending.set(callId, resolve))
    if (approved) {
      onEvent({ type: 'toolStarted', callId, name: 'delete_task', arguments: {} })
      await sleep(250)
      onEvent({
        type: 'toolFinished',
        callId,
        name: 'delete_task',
        ok: true,
        summary: 'deleted task Order the timber',
      })
      reply = 'Deleted "Order the timber".'
    } else {
      reply = 'Left it alone. What would you like to do instead?'
    }
  } else if (lower.includes('task') || lower.includes('todo')) {
    const callId = 'call-mock-list'
    onEvent({ type: 'toolStarted', callId, name: 'list_tasks', arguments: { open_only: true } })
    await sleep(350)
    onEvent({ type: 'toolFinished', callId, name: 'list_tasks', ok: true, summary: '3 results' })
    // Markdown, because that is what a model answers with whatever it is
    // asked. A mock that replies in plain prose is a mock in which the panel
    // cannot be reviewed: the one thing to look at here is whether a list
    // renders as a list and a command renders as code.
    reply = [
      'You have **three** open:',
      '',
      '1. Order the timber — *overdue since Monday*',
      '2. Ring the vet',
      '3. Book the MOT',
      '',
      'Two of them are in `Move house`. Say the word and I will move the third.',
    ].join('\n')
  } else if (lower.includes('markdown')) {
    // Everything the renderer draws, for reviewing it in one screen.
    reply = [
      '## What I can format',
      '',
      'Prose with **bold**, *italic*, ~~struck out~~ and `inline code`.',
      '',
      '- A bullet',
      '  - and one nested under it',
      '- [A link](https://example.org)',
      '',
      '> A quotation, for something you said earlier.',
      '',
      '```sh',
      'everyday search rain --json',
      '```',
      '',
      '| Shelf | Open |',
      '| --- | ---: |',
      '| Books | 4 |',
      '| Films | 2 |',
    ].join('\n')
  } else {
    reply = 'This is a scripted reply from the mock backend. There is no model behind it.'
  }

  // Split on whitespace but *keep* it, so the newlines a Markdown reply is
  // made of survive the streaming. Splitting on ' ' alone joined every line
  // of a list into one paragraph, which is the exact failure the renderer
  // exists to fix -- reproduced by the harness meant to demonstrate it.
  for (const chunk of reply.split(/(?<=\s)/)) {
    await sleep(35)
    onEvent({ type: 'delta', text: chunk })
  }

  agentMessages.push({
    id: messageId,
    conversationId,
    role: 'assistant',
    content: reply,
    toolCalls: [],
    toolCallId: null,
    failed: false,
    createdAt: new Date().toISOString(),
  })
  onEvent({ type: 'finished', messageId })
}

export function mockMediaUrl(blob: string): string {
  return BLOBS[blob] ?? COVERS[blob] ?? swatch('#8a8a8a', '#4a4a4a', 'missing media')
}
