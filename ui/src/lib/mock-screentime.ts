// Seed screen time for the mock backend, and the one function `mock.ts`
// hands every Screen time command and `screentime_*` shell call to.
//
// Two machines, thirty days. The *laptop* is used before work for the news,
// all day on Fridays, and most evenings and weekends; the *desktop* is the
// work machine, Monday to Thursday with a lunch break, and a game now and
// then on a Saturday. About thirty apps and sites between them -- editors,
// a terminal, chat, mail, docs, YouTube, the news -- each more likely at the
// hours it is really used, so the summary, the hourly timeline and a week
// against the week before all have a shape to draw.
//
// Each machine has a desktop collector (`gnome` on the laptop, `x11` on the
// desktop) and a browser extension (`firefox`, `chrome`), so a browser's
// time is carved by the sites the extension reported and what is left is
// "Firefox (other)", as `report.rs` does it. A few labels are seeded:
// Google split into Gmail, Docs and Calendar; Hacker News renamed from
// `ycombinator.com`; the GNOME overview ignored; this app filed under Work.
// Amazon has no suggestion and no label, so something is always
// uncategorised.
//
// Deterministic: each day's spans come from a PRNG seeded by the machine
// and the date, so the same day always looks the same, whatever today is.
// Days are local days -- the `tz` a command is given is checked for being
// there and otherwise ignored, which is the one way this differs from the
// vault.
//
// Nothing here runs at module load but building the seed: no timers, so
// loading this file cannot hold a test process open.

import { CONNECTION_PREFIX, base64Url, keyText } from './screentime'
import { addDays, daysApart, isoDate, startOfDay, todayIso } from './time'
import type {
  BrowserConnection,
  IngestReport,
  ScreenApp,
  ScreenAppRow,
  ScreenCategory,
  ScreenCategoryId,
  ScreenCategoryTotal,
  ScreenDayTotal,
  ScreenDevice,
  ScreenDeviceTotal,
  ScreenItem,
  ScreenQuery,
  ScreenReport,
  ScreenTimeline,
  ScreenTimeSettings,
  ScreenTimeStatus,
  SpanKind,
  TimelineRun,
} from './types'
import { VaultError } from './types'

const SECOND = 1000
const MINUTE = 60 * SECOND
const DAY_MS = 24 * 60 * MINUTE
/** How many days back the seed reaches. */
const SEED_DAYS = 30
/** How far apart two heartbeats may be and still be one span (`PULSE_SECS`). */
const PULSE_MS = 45 * SECOND
/** The longest range a report may cover. */
const MAX_REPORT_DAYS = 366

/** When the seed was built: nothing is recorded after it. */
const BUILT_AT = Date.now()
const SEEDED_AT = new Date(BUILT_AT - 60 * DAY_MS).toISOString()

/** A copy through JSON, which is what the real bridge does to every answer. */
function wire<T>(value: T): T {
  return value === undefined ? value : (JSON.parse(JSON.stringify(value)) as T)
}

function nowIso(): string {
  return new Date().toISOString()
}

function str(v: unknown): string {
  return typeof v === 'string' ? v : ''
}

function strArray(v: unknown): string[] {
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : []
}

/** FNV-1a: a stable number for a string. */
function hash(seed: string): number {
  let h = 2166136261
  for (let i = 0; i < seed.length; i++) {
    h ^= seed.charCodeAt(i)
    h = Math.imul(h, 16777619)
  }
  return h >>> 0
}

/** Mulberry32: a small seeded PRNG, `[0, 1)`. */
function prng(seed: number): () => number {
  let a = seed | 0
  return () => {
    a = (a + 0x6d2b79f5) | 0
    let t = Math.imul(a ^ (a >>> 15), 1 | a)
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

function isDay(value: string): boolean {
  return /^\d{4}-\d{2}-\d{2}$/.test(value) && isoDate(startOfDay(value)) === value
}

// ── What is known without being told ─────────────────────────────────────
//
// The mock's copy of `screentime::defaults`: the starter categories, which
// apps are browsers, friendlier names, and a suggested category for
// well-known items.

const STARTERS: { builtin: string; name: string; color: string }[] = [
  { builtin: 'work', name: 'Work', color: '#0369a1' },
  { builtin: 'communication', name: 'Communication', color: '#0f766e' },
  { builtin: 'social', name: 'Social', color: '#a21caf' },
  { builtin: 'entertainment', name: 'Entertainment', color: '#be123c' },
  { builtin: 'news', name: 'News', color: '#b45309' },
  { builtin: 'learning', name: 'Learning', color: '#4338ca' },
  // No colour of its own: drawn in the palette's colour for its place, so
  // that path is on screen too.
  { builtin: 'utilities', name: 'Utilities', color: '' },
]

const BROWSERS = new Set([
  'firefox',
  'org.mozilla.firefox',
  'firefox.exe',
  'google-chrome',
  'chrome',
  'chrome.exe',
  'com.google.chrome',
  'chromium',
  'org.chromium.chromium',
  'com.apple.safari',
  'msedge.exe',
  'microsoft-edge',
  'brave-browser',
  'com.brave.browser',
  'vivaldi',
  'opera',
  'arc',
])

const FRIENDLY: Record<string, string> = {
  'org.gnome.ptyxis': 'Terminal',
  'org.gnome.nautilus': 'Files',
  'org.gnome.settings': 'Settings',
  'org.gnome.calculator': 'Calculator',
  code: 'Visual Studio Code',
  'code.exe': 'Visual Studio Code',
  firefox: 'Firefox',
  'google-chrome': 'Google Chrome',
}

const SUGGEST: Record<string, string> = {
  'app:dev.zed.zed': 'work',
  'app:code': 'work',
  'app:org.gnome.ptyxis': 'work',
  'app:org.gnome.nautilus': 'utilities',
  'app:org.gnome.settings': 'utilities',
  'app:org.gnome.calculator': 'utilities',
  'app:com.slack.slack': 'communication',
  'app:zoom': 'communication',
  'app:spotify': 'entertainment',
  'app:steam': 'entertainment',
  'site:github.com': 'work',
  'site:google.com': 'communication',
  'site:mail.google.com': 'communication',
  'site:docs.google.com': 'work',
  'site:calendar.google.com': 'work',
  'site:youtube.com': 'entertainment',
  'site:netflix.com': 'entertainment',
  'site:ycombinator.com': 'news',
  'site:nytimes.com': 'news',
  'site:theguardian.com': 'news',
  'site:bbc.co.uk': 'news',
  'site:x.com': 'social',
  'site:wikipedia.org': 'learning',
  'site:mozilla.org': 'learning',
  'site:stackoverflow.com': 'work',
  'site:linear.app': 'work',
  'site:docs.rs': 'work',
  'site:claude.ai': 'work',
}

/** Second-level suffixes a registrable domain sits under, the handful the
 *  seed and a test can reach. The vault uses the whole public suffix list. */
const SECOND_LEVEL = new Set([
  'co.uk',
  'org.uk',
  'ac.uk',
  'gov.uk',
  'com.au',
  'net.au',
  'org.au',
  'co.jp',
  'co.nz',
  'co.in',
  'com.br',
  'github.io',
])

function normalizeHost(host: string): string {
  return host.trim().toLowerCase().replace(/:\d+$/, '').replace(/\.$/, '')
}

function registrable(host: string): string {
  const labels = host.split('.').filter(Boolean)
  if (labels.length <= 2) return labels.join('.')
  const last2 = labels.slice(-2).join('.')
  return SECOND_LEVEL.has(last2) ? labels.slice(-3).join('.') : last2
}

function isBrowser(app: string): boolean {
  const id = app.trim().toLowerCase()
  return BROWSERS.has(id) || settings.browsers.some((b) => b.trim().toLowerCase() === id)
}

function suggest(key: string): string | undefined {
  const direct = SUGGEST[key]
  if (direct) return direct
  if (key.startsWith('site:')) return SUGGEST[`site:${registrable(keyText(key))}`]
  return undefined
}

// ── What the vault holds ─────────────────────────────────────────────────

interface Span {
  id: string
  device: string
  collector: string
  kind: SpanKind
  /** The app id, or for a site the browser's name. */
  app: string
  name: string
  host: string
  title: string
  start: number
  end: number
}

let spans: Span[] = []
let spanCount = 0
const categories: ScreenCategory[] = []
/** Set once the starter categories have been written, as `ensure_screen_defaults` does. */
let seeded = false
const labels: ScreenApp[] = []
let settings: ScreenTimeSettings = { idleMinutes: 3, keepTitles: true, titleDays: 0, browsers: [] }

function categoryIdOf(builtin: string): ScreenCategoryId {
  return `screen-cat-${builtin}`
}

function ensureDefaults(): void {
  if (seeded) return
  seeded = true
  STARTERS.forEach((s, i) => {
    categories.push({
      id: categoryIdOf(s.builtin),
      name: s.name,
      color: s.color,
      builtin: s.builtin,
      sortOrder: i,
      createdAt: SEEDED_AT,
      updatedAt: SEEDED_AT,
    })
  })
}

function label(partial: Partial<ScreenApp> & Pick<ScreenApp, 'id' | 'key' | 'kind'>): ScreenApp {
  return {
    name: '',
    category: null,
    uncategorized: false,
    ignored: false,
    split: false,
    createdAt: SEEDED_AT,
    updatedAt: SEEDED_AT,
    ...partial,
  }
}

labels.push(
  label({ id: 'screen-app-google', key: 'site:google.com', kind: 'site', split: true }),
  label({ id: 'screen-app-hn', key: 'site:ycombinator.com', kind: 'site', name: 'Hacker News' }),
  label({ id: 'screen-app-shell', key: 'app:gnome-shell', kind: 'app', ignored: true }),
  label({
    id: 'screen-app-karmayoga',
    key: 'app:karmayoga',
    kind: 'app',
    category: categoryIdOf('work'),
  }),
)

// ── The seed ─────────────────────────────────────────────────────────────

type Mode = 'work' | 'morning' | 'evening' | 'weekend'
type Machine = 'laptop' | 'desktop'

interface Thing {
  kind: SpanKind
  /** The app id; for a site, unused (the machine's browser is). */
  app: string
  name: string
  /** Sites only: the host. */
  host: string
  titles: string[]
  /** Minutes a visit lasts, low and high. */
  min: number
  max: number
  weight: Partial<Record<Mode, number>>
  only?: Machine
}

function app(
  id: string,
  name: string,
  titles: string[],
  min: number,
  max: number,
  weight: Thing['weight'],
  only?: Machine,
): Thing {
  return { kind: 'app', app: id, name, host: '', titles, min, max, weight, only }
}

function site(
  host: string,
  titles: string[],
  min: number,
  max: number,
  weight: Thing['weight'],
): Thing {
  return { kind: 'site', app: '', name: '', host, titles, min, max, weight }
}

const THINGS: Thing[] = [
  app(
    'dev.zed.Zed',
    'Zed',
    ['screentime.svelte.ts — karmayoga', 'report.rs — karmayoga', 'mock-screentime.ts — karmayoga'],
    8,
    45,
    { work: 9, evening: 2, weekend: 1 },
    'laptop',
  ),
  app(
    'code',
    'Visual Studio Code',
    ['money.rs — karmayoga', 'vault.rs — karmayoga', 'Cargo.toml — karmayoga'],
    10,
    50,
    { work: 10 },
    'desktop',
  ),
  app(
    'org.gnome.Ptyxis',
    '',
    ['~/projects/karmayoga', 'cargo test — karmayoga', 'htop', 'ssh nas'],
    2,
    15,
    { work: 7, evening: 1, weekend: 1 },
  ),
  app('org.gnome.Nautilus', '', ['Downloads', 'Pictures', 'Documents'], 1, 4, {
    work: 1,
    evening: 1,
    weekend: 1,
  }),
  app(
    'com.slack.Slack',
    'Slack',
    ['#karmayoga — Slack', '#general — Slack', 'Direct messages — Slack'],
    2,
    12,
    { work: 6, morning: 1 },
  ),
  app('karmayoga', 'Karma Yoga', ['Journal', 'Mail', 'Todo', 'Calendar'], 3, 20, {
    work: 3,
    morning: 3,
    evening: 3,
    weekend: 2,
  }),
  app('spotify', 'Spotify', ['Spotify Premium'], 1, 4, { work: 1, evening: 1, weekend: 1 }),
  app('zoom', 'Zoom', ['Zoom Meeting', 'Weekly sync'], 25, 55, { work: 1.2 }),
  app('org.gnome.Settings', '', ['Displays', 'Sound', 'Wi-Fi'], 1, 3, {
    work: 0.3,
    evening: 0.3,
    weekend: 0.3,
  }),
  app('org.gnome.Calculator', '', ['Calculator'], 1, 2, { work: 0.4, evening: 0.2 }),
  app('steam', 'Steam', ['Steam', 'Factorio'], 40, 120, { weekend: 3, evening: 0.6 }, 'desktop'),
  app('gnome-shell', 'GNOME Shell', ['Activities'], 1, 1, {
    work: 0.5,
    evening: 0.5,
    weekend: 0.5,
  }),
  site(
    'github.com',
    ['Pull requests · karmayoga', 'Issues · karmayoga', 'karmayoga/crates at main · karmayoga'],
    3,
    20,
    { work: 7, evening: 1, weekend: 1 },
  ),
  site('mail.google.com', ['Inbox (12) - Gmail', 'Inbox - Gmail'], 2, 12, {
    work: 4,
    morning: 3,
    evening: 1,
    weekend: 1,
  }),
  site('docs.google.com', ['Q4 planning - Google Docs', 'Budget 2027 - Google Sheets'], 5, 30, {
    work: 4,
    weekend: 0.3,
  }),
  site('calendar.google.com', ['Google Calendar'], 1, 4, { work: 2, morning: 1 }),
  site(
    'www.youtube.com',
    ['Rust in 100 seconds - YouTube', 'Lo-fi beats to code to - YouTube', 'YouTube'],
    5,
    40,
    { evening: 6, weekend: 6, work: 0.5 },
  ),
  site('news.ycombinator.com', ['Hacker News', 'Show HN | Hacker News'], 2, 12, {
    morning: 4,
    evening: 2,
    work: 1.2,
    weekend: 2,
  }),
  site('www.nytimes.com', ['The New York Times - Breaking News'], 3, 12, {
    morning: 4,
    evening: 1,
    weekend: 2,
  }),
  site('www.theguardian.com', ['News, sport and opinion | The Guardian'], 3, 10, {
    morning: 3,
    evening: 1,
    weekend: 2,
  }),
  site('www.bbc.co.uk', ['BBC News'], 2, 8, { morning: 2, weekend: 1 }),
  site('x.com', ['Home / X', 'Notifications / X'], 2, 15, {
    evening: 3,
    weekend: 2,
    work: 0.8,
    morning: 1,
  }),
  site('en.wikipedia.org', ['Hourglass - Wikipedia', 'Public Suffix List - Wikipedia'], 2, 15, {
    evening: 2,
    weekend: 2,
    work: 1,
  }),
  site('stackoverflow.com', ['How to split a range by hour - Stack Overflow'], 2, 8, { work: 3 }),
  site('developer.mozilla.org', ['Intl.DateTimeFormat.prototype.formatRange() - MDN'], 2, 10, {
    work: 2.5,
  }),
  site('www.netflix.com', ['Netflix'], 30, 90, { evening: 2, weekend: 2 }),
  site('linear.app', ['My issues - Linear', 'Screen time - Linear'], 2, 10, { work: 3 }),
  site('docs.rs', ['jiff - Rust', 'rusqlite - Rust'], 2, 10, { work: 2 }),
  site('claude.ai', ['Claude'], 3, 20, { work: 2, evening: 0.8 }),
  site('www.amazon.com', ['Amazon.com: Online Shopping'], 3, 15, { evening: 1, weekend: 1.5 }),
]

/** How each machine's collectors name themselves and its browser. */
const MACHINES: Record<
  Machine,
  { desktop: string; extension: string; browserApp: string; browserName: string; siteApp: string }
> = {
  laptop: {
    desktop: 'gnome',
    extension: 'firefox',
    browserApp: 'firefox',
    browserName: 'Firefox',
    siteApp: 'Firefox',
  },
  desktop: {
    desktop: 'x11',
    extension: 'chrome',
    browserApp: 'google-chrome',
    browserName: 'Google Chrome',
    siteApp: 'Chrome',
  },
}

/** When each machine is in use on `date`: minutes after midnight, and how. */
function sessionsFor(machine: Machine, date: string, rnd: () => number): [number, number, Mode][] {
  const weekday = startOfDay(date).getDay()
  const jitter = () => Math.round(rnd() * 20 - 10)
  const out: [number, number, Mode][] = []
  const at = (h: number, m: number) => h * 60 + m + jitter()
  const weekend = weekday === 0 || weekday === 6
  if (machine === 'laptop') {
    if (weekend) {
      if (rnd() < 0.7) out.push([at(9, 30), at(11, 45), 'weekend'])
      if (rnd() < 0.3) out.push([at(14, 0), at(16, 0), 'weekend'])
      if (rnd() < 0.85) out.push([at(20, 0), at(23, 10), 'evening'])
    } else if (weekday === 5) {
      out.push([at(9, 5), at(12, 20), 'work'], [at(13, 10), at(17, 20), 'work'])
      if (rnd() < 0.5) out.push([at(20, 0), at(22, 30), 'evening'])
    } else {
      if (rnd() < 0.6) out.push([at(7, 30), at(8, 15), 'morning'])
      if (rnd() < 0.75) out.push([at(19, 30), at(22, 15), 'evening'])
    }
  } else if (weekday >= 1 && weekday <= 4) {
    out.push([at(8, 55), at(12, 30), 'work'], [at(13, 15), at(17, 50), 'work'])
  } else if (weekday === 5) {
    if (rnd() < 0.15) out.push([at(10, 0), at(10, 40), 'work'])
  } else if (weekday === 6) {
    if (rnd() < 0.5) out.push([at(15, 0), at(18, 30), 'weekend'])
  } else if (rnd() < 0.3) {
    out.push([at(16, 0), at(19, 0), 'weekend'])
  }
  return out
}

function pick<T>(list: T[], weightOf: (t: T) => number, rnd: () => number): T {
  const total = list.reduce((n, t) => n + weightOf(t), 0)
  let r = rnd() * total
  for (const t of list) {
    r -= weightOf(t)
    if (r <= 0) return t
  }
  return list[list.length - 1]!
}

function pushSpan(span: Omit<Span, 'id'>): void {
  if (span.start >= BUILT_AT) return
  const end = Math.min(span.end, BUILT_AT)
  if (end <= span.start) return
  spanCount += 1
  spans.push({ ...span, end, id: `ss-${spanCount}` })
}

function seedDay(machine: Machine, date: string): void {
  const rnd = prng(hash(`${machine}/${date}`))
  const m = MACHINES[machine]
  const mine = THINGS.filter((t) => !t.only || t.only === machine)
  const sites = mine.filter((t) => t.kind === 'site')
  const midnight = startOfDay(date).getTime()
  const minutes = (t: Thing) => Math.round((t.min + rnd() * (t.max - t.min)) * 60) * SECOND
  const title = (t: Thing) => t.titles[Math.floor(rnd() * t.titles.length)] ?? ''
  for (const [from, to, mode] of sessionsFor(machine, date, rnd)) {
    const weight = (t: Thing) => t.weight[mode] ?? 0
    const end = midnight + to * MINUTE
    let at = midnight + from * MINUTE
    while (at < end) {
      const thing = pick(mine, weight, rnd)
      if (thing.kind === 'app') {
        const len = minutes(thing)
        pushSpan({
          device: machine,
          collector: m.desktop,
          kind: 'app',
          app: thing.app,
          name: thing.name,
          host: '',
          title: title(thing),
          start: at,
          end: Math.min(at + len, end),
        })
        at += len
      } else {
        // A stretch in the browser: one site, sometimes two or three, with
        // a moment of the browser's own between them -- a new tab, the
        // address bar -- which no site claims.
        const visits = [thing]
        if (rnd() < 0.5) visits.push(pick(sites, weight, rnd))
        if (rnd() < 0.2) visits.push(pick(sites, weight, rnd))
        const began = at
        for (const visit of visits) {
          const len = minutes(visit)
          pushSpan({
            device: machine,
            collector: m.extension,
            kind: 'site',
            app: m.siteApp,
            name: '',
            host: visit.host,
            title: title(visit),
            start: at,
            end: Math.min(at + len, end),
          })
          at += len + Math.round(rnd() * 90) * SECOND
        }
        pushSpan({
          device: machine,
          collector: m.desktop,
          kind: 'app',
          app: m.browserApp,
          name: m.browserName,
          host: '',
          title: '',
          start: began,
          end: Math.min(at, end),
        })
      }
      // Then a switch, a pause, or now and then a proper break.
      const r = rnd()
      if (r < 0.7) at += Math.round(rnd() * 15) * SECOND
      else if (r < 0.95) at += Math.round(60 + rnd() * 240) * SECOND
      else at += Math.round(8 + rnd() * 17) * MINUTE
    }
  }
}

function seed(): void {
  const today = todayIso()
  for (let back = SEED_DAYS - 1; back >= 0; back--) {
    const date = addDays(today, -back)
    seedDay('laptop', date)
    seedDay('desktop', date)
  }
  spans.sort((a, b) => a.start - b.start)
}

seed()

// ── Reconciling: the mock's `report::resolve` ────────────────────────────

interface Run {
  start: number
  end: number
  key: string
  kind: SpanKind
  name: string
  /** The name the collector gave, whatever the label says. */
  defaultName: string
  category: ScreenCategoryId | null
  suggested: boolean
  /** The category a suggestion would give, label or not. */
  suggestedCategory: ScreenCategoryId | null
  ignored: boolean
  device: string
  title: string
  host: string
  browserRest: boolean
  appId: string | null
}

interface Piece<T> {
  item: T
  start: number
  end: number
}

/**
 * `items` with every stretch `cuts` covers taken out. Both sorted by start
 * and neither overlapping itself, so one walk does it.
 */
function subtract<T extends { start: number; end: number }>(
  items: T[],
  cuts: { start: number; end: number }[],
): Piece<T>[] {
  const out: Piece<T>[] = []
  let j = 0
  for (const item of items) {
    let s = item.start
    while (j < cuts.length && cuts[j]!.end <= s) j++
    let k = j
    while (s < item.end) {
      const cut = cuts[k]
      if (!cut || cut.start >= item.end) {
        out.push({ item, start: s, end: item.end })
        break
      }
      if (cut.start > s) out.push({ item, start: s, end: cut.start })
      s = Math.max(s, cut.end)
      k++
    }
  }
  return out
}

/** Spans of one kind on one device made not to overlap: the later start wins. */
function untangle(list: Span[]): { span: Span; start: number; end: number }[] {
  const sorted = [...list].sort((a, b) => a.start - b.start)
  const out = sorted.map((span) => ({ span, start: span.start, end: span.end }))
  for (let i = 0; i + 1 < out.length; i++) {
    const next = out[i + 1]!
    if (out[i]!.end > next.start) out[i]!.end = next.start
  }
  return out.filter((r) => r.end > r.start)
}

function labelFor(key: string): ScreenApp | undefined {
  return labels.find((l) => l.key === key)
}

function keyOf(span: Span): string {
  if (span.kind === 'app') return `app:${span.app.trim().toLowerCase()}`
  const host = normalizeHost(span.host)
  const domain = registrable(host)
  return labelFor(`site:${domain}`)?.split ? `site:${host}` : `site:${domain}`
}

function categoryFor(key: string): {
  category: ScreenCategoryId | null
  suggested: boolean
  suggestedCategory: ScreenCategoryId | null
} {
  const builtin = suggest(key)
  const suggestedCategory = builtin
    ? (categories.find((c) => c.builtin === builtin)?.id ?? null)
    : null
  const own = labelFor(key)
  if (own?.uncategorized) return { category: null, suggested: false, suggestedCategory }
  if (own?.category && categories.some((c) => c.id === own.category)) {
    return { category: own.category, suggested: false, suggestedCategory }
  }
  return { category: suggestedCategory, suggested: suggestedCategory !== null, suggestedCategory }
}

function defaultNameOf(span: Span, key: string): string {
  if (span.kind === 'site') return keyText(key)
  return span.name || FRIENDLY[span.app.toLowerCase()] || span.app
}

function runOf(span: Span, start: number, end: number, browserRest: boolean): Run {
  const key = keyOf(span)
  const own = labelFor(key)
  const defaultName = defaultNameOf(span, key)
  return {
    start,
    end,
    key,
    kind: span.kind,
    name: own?.name || defaultName,
    defaultName,
    ...categoryFor(key),
    ignored: own?.ignored === true,
    device: span.device,
    title: span.title,
    host: span.kind === 'site' ? normalizeHost(span.host) : '',
    browserRest,
    appId: own?.id ?? null,
  }
}

/** Reconcile spans into runs -- see `report.rs`'s module doc. Ignored items
 *  are dropped unless `keepIgnored`. In start order. */
function resolve(list: Span[], keepIgnored = false): Run[] {
  ensureDefaults()
  const runs: Run[] = []
  const devices = new Set(list.map((s) => s.device))
  for (const device of devices) {
    const mine = list.filter((s) => s.device === device)
    const apps = untangle(mine.filter((s) => s.kind === 'app'))
    const sites = untangle(mine.filter((s) => s.kind === 'site'))
    // A site counts while the browser has focus, or while no desktop
    // collector said anything at all; another app in front takes it back.
    const sitePieces = subtract(
      sites,
      apps.filter((a) => !isBrowser(a.span.app)),
    )
    const browsers = apps.filter((a) => isBrowser(a.span.app))
    // The browser keeps what no site claimed. Called "(other)" only where
    // an extension reports sites on this device; without one a browser is
    // one app.
    const carved = sites.length > 0
    for (const p of subtract(browsers, sitePieces)) {
      runs.push(runOf(p.item.span, p.start, p.end, carved))
    }
    for (const a of apps) {
      if (!isBrowser(a.span.app)) runs.push(runOf(a.span, a.start, a.end, false))
    }
    for (const p of sitePieces) runs.push(runOf(p.item.span, p.start, p.end, false))
  }
  return runs.filter((r) => keepIgnored || !r.ignored).sort((a, b) => a.start - b.start)
}

/** Spans on `devices` (empty: all) that overlap `[from, to)`. */
function spansIn(devices: string[], from: number, to: number): Span[] {
  return spans.filter(
    (s) => s.end > from && s.start < to && (devices.length === 0 || devices.includes(s.device)),
  )
}

/** Call `add` for each stretch of `[start, end)` within one local hour. */
function byHour(start: number, end: number, add: (at: Date, ms: number) => void): void {
  let t = start
  while (t < end) {
    const at = new Date(t)
    const next = new Date(
      at.getFullYear(),
      at.getMonth(),
      at.getDate(),
      at.getHours() + 1,
    ).getTime()
    const until = Math.min(end, next)
    add(at, until - t)
    t = until
  }
}

const secs = (ms: number) => Math.round(ms / SECOND)

function requireZone(tz: unknown): void {
  if (tz !== undefined && typeof tz !== 'string') {
    throw new VaultError('invalid', 'A time zone is a name like “Europe/London”.')
  }
}

function report(q: ScreenQuery): ScreenReport {
  if (!isDay(q.from) || !isDay(q.to)) throw new VaultError('invalid', 'A report needs two days.')
  if (q.to < q.from) throw new VaultError('invalid', 'A report cannot end before it starts.')
  requireZone(q.tz)
  const length = daysApart(q.from, q.to) + 1
  if (length > MAX_REPORT_DAYS) {
    throw new VaultError('invalid', `A report covers at most ${MAX_REPORT_DAYS} days.`)
  }
  const start = startOfDay(q.from).getTime()
  const end = startOfDay(addDays(q.to, 1)).getTime()
  const before = startOfDay(addDays(q.from, -length)).getTime()
  const devices = strArray(q.devices)
  const runs = resolve(spansIn(devices, before, end)).filter(
    (r) => (!q.category || r.category === q.category) && (!q.item || r.key === q.item),
  )

  const items = new Map<string, { run: Run; ms: number }>()
  const cats = new Map<ScreenCategoryId | null, number>()
  const days = new Map<string, { ms: number; cats: Map<ScreenCategoryId | null, number> }>()
  for (let d = q.from; d <= q.to; d = addDays(d, 1)) days.set(d, { ms: 0, cats: new Map() })
  const hours = Array.from({ length: 24 }, () => 0)
  const perDevice = new Map<string, number>()
  let total = 0
  let previous = 0

  for (const run of runs) {
    previous += Math.max(0, Math.min(run.end, start) - Math.max(run.start, before))
    const s = Math.max(run.start, start)
    const e = Math.min(run.end, end)
    if (e <= s) continue
    const ms = e - s
    total += ms
    const item = items.get(run.key)
    if (item) item.ms += ms
    else items.set(run.key, { run, ms })
    cats.set(run.category, (cats.get(run.category) ?? 0) + ms)
    perDevice.set(run.device, (perDevice.get(run.device) ?? 0) + ms)
    byHour(s, e, (at, part) => {
      const hour = at.getHours()
      hours[hour] = (hours[hour] ?? 0) + part
      const day = days.get(isoDate(at))
      if (!day) return
      day.ms += part
      day.cats.set(run.category, (day.cats.get(run.category) ?? 0) + part)
    })
  }

  const totals = (m: Map<ScreenCategoryId | null, number>): ScreenCategoryTotal[] =>
    [...m]
      .map(([category, ms]) => ({ category, secs: secs(ms) }))
      .filter((t) => t.secs > 0)
      .sort((a, b) => b.secs - a.secs)
  return {
    from: q.from,
    to: q.to,
    totalSecs: secs(total),
    items: [...items.values()]
      .map(({ run, ms }): ScreenItem => ({
        key: run.key,
        kind: run.kind,
        name: run.name,
        category: run.category,
        suggested: run.suggested,
        secs: secs(ms),
        browserRest: run.browserRest,
        appId: run.appId,
      }))
      .filter((i) => i.secs > 0)
      .sort((a, b) => b.secs - a.secs || a.name.localeCompare(b.name)),
    categories: totals(cats),
    days: [...days].map(([date, d]): ScreenDayTotal => ({
      date,
      secs: secs(d.ms),
      categories: totals(d.cats),
    })),
    hours: hours.map(secs),
    devices: [...perDevice]
      .map(([device, ms]): ScreenDeviceTotal => ({ device, secs: secs(ms) }))
      .sort((a, b) => b.secs - a.secs),
    previousTotalSecs: secs(previous),
  }
}

function timeline(date: string, tz: unknown, devices: string[]): ScreenTimeline {
  if (!isDay(date)) throw new VaultError('invalid', 'A timeline is of one day.')
  requireZone(tz)
  const start = startOfDay(date).getTime()
  const end = startOfDay(addDays(date, 1)).getTime()
  const runs: TimelineRun[] = resolve(spansIn(devices, start, end))
    .map((r) => ({ ...r, start: Math.max(r.start, start), end: Math.min(r.end, end) }))
    .filter((r) => r.end > r.start)
    .map((r) => ({
      start: new Date(r.start).toISOString(),
      end: new Date(r.end).toISOString(),
      key: r.key,
      kind: r.kind,
      name: r.name,
      category: r.category,
      device: r.device,
      title: settings.keepTitles ? r.title : '',
    }))
  return { date, runs }
}

function appRows(): ScreenAppRow[] {
  const since = Date.now() - SEED_DAYS * DAY_MS
  const runs = resolve(spansIn([], since, Infinity), true)
  const rows = new Map<string, { row: ScreenAppRow; ms: number; last: number }>()
  /** Seconds per host under each registrable domain, for `hosts`. */
  const hosts = new Map<string, Map<string, number>>()
  for (const run of runs) {
    const ms = run.end - Math.max(run.start, since)
    if (run.kind === 'site') {
      const domain = registrable(run.host)
      const under = hosts.get(domain) ?? new Map<string, number>()
      under.set(run.host, (under.get(run.host) ?? 0) + ms)
      hosts.set(domain, under)
    }
    const had = rows.get(run.key)
    if (had) {
      had.ms += ms
      had.last = Math.max(had.last, run.end)
      continue
    }
    rows.set(run.key, {
      row: {
        key: run.key,
        kind: run.kind,
        defaultName: run.defaultName,
        label: labelFor(run.key) ?? null,
        suggested: run.suggestedCategory,
        hosts: [],
        recentSecs: 0,
        lastSeen: null,
      },
      ms,
      last: run.end,
    })
  }
  const hostsOf = (key: string): string[] => {
    if (!key.startsWith('site:')) return []
    const under = hosts.get(keyText(key))
    if (!under) return []
    return [...under].sort((a, b) => b[1] - a[1]).map(([host]) => host)
  }
  const seen = [...rows.values()]
    .map(({ row, ms, last }) => ({
      ...row,
      hosts: hostsOf(row.key),
      recentSecs: secs(ms),
      lastSeen: new Date(last).toISOString(),
    }))
    .sort((a, b) => b.recentSecs - a.recentSecs)
  const unseen = labels
    .filter((l) => !rows.has(l.key))
    .map((l): ScreenAppRow => ({
      key: l.key,
      kind: l.kind,
      defaultName: keyText(l.key),
      label: l,
      suggested: categoryFor(l.key).suggestedCategory,
      hosts: hostsOf(l.key),
      recentSecs: 0,
      lastSeen: null,
    }))
    .sort((a, b) => (a.label?.name || a.defaultName).localeCompare(b.label?.name || b.defaultName))
  return [...seen, ...unseen]
}

function deviceList(): ScreenDevice[] {
  const byDevice = new Map<string, { collectors: Set<string>; last: number }>()
  for (const s of spans) {
    const d = byDevice.get(s.device) ?? { collectors: new Set<string>(), last: 0 }
    d.collectors.add(s.collector)
    d.last = Math.max(d.last, s.end)
    byDevice.set(s.device, d)
  }
  return [...byDevice]
    .sort((a, b) => b[1].last - a[1].last)
    .map(([device, d]) => ({
      device,
      collectors: [...d.collectors].sort(),
      lastSeen: new Date(d.last).toISOString(),
    }))
}

// ── Writes ───────────────────────────────────────────────────────────────

function categoriesInOrder(): ScreenCategory[] {
  ensureDefaults()
  return [...categories].sort((a, b) => a.sortOrder - b.sortOrder || a.name.localeCompare(b.name))
}

function saveLabel(input: ScreenApp): void {
  ensureDefaults()
  const key = str(input.key).trim()
  const kind = input.kind
  if (!/^(app|site):.+/.test(key) || (kind !== 'app' && kind !== 'site')) {
    throw new VaultError('invalid', 'A label names an app or a site.')
  }
  if ((kind === 'site') !== key.startsWith('site:')) {
    throw new VaultError('invalid', 'A label’s kind does not match its key.')
  }
  if (input.category && !categories.some((c) => c.id === input.category)) {
    throw new VaultError('not_found', 'That category no longer exists.')
  }
  if (input.split && kind !== 'site') {
    throw new VaultError('invalid', 'Only a site can be split by host.')
  }
  const now = nowIso()
  const stored: ScreenApp = {
    id: str(input.id),
    key,
    kind,
    name: str(input.name).trim().slice(0, 200),
    category: input.uncategorized ? null : (input.category ?? null),
    uncategorized: input.uncategorized === true,
    ignored: input.ignored === true,
    split: input.split === true,
    createdAt: labels.find((l) => l.id === input.id)?.createdAt ?? now,
    updatedAt: now,
  }
  if (!stored.id) throw new VaultError('invalid', 'A label needs an id.')
  // One label per key: a second for the same item takes the first's place.
  for (let i = labels.length - 1; i >= 0; i--) {
    if (labels[i]!.id === stored.id || labels[i]!.key === key) labels.splice(i, 1)
  }
  labels.push(stored)
}

function saveCategory(input: ScreenCategory): void {
  ensureDefaults()
  const name = str(input.name).trim()
  if (!name) throw new VaultError('invalid', 'A category needs a name.')
  if (name.length > 100)
    throw new VaultError('invalid', 'A category’s name is at most 100 characters.')
  const color = str(input.color).trim()
  if (color && !/^#[0-9a-fA-F]{6}$/.test(color)) {
    throw new VaultError('invalid', 'A colour is written like #2563eb.')
  }
  const at = categories.findIndex((c) => c.id === input.id)
  const now = nowIso()
  const stored: ScreenCategory = {
    id: str(input.id),
    name,
    color,
    // Which starter it began as is the vault's to keep, whatever is sent.
    builtin: at >= 0 ? categories[at]!.builtin : '',
    sortOrder: typeof input.sortOrder === 'number' ? input.sortOrder : 0,
    createdAt: at >= 0 ? categories[at]!.createdAt : now,
    updatedAt: now,
  }
  if (!stored.id) throw new VaultError('invalid', 'A category needs an id.')
  if (at >= 0) categories[at] = stored
  else categories.push(stored)
}

function deleteCategory(id: string): void {
  ensureDefaults()
  const at = categories.findIndex((c) => c.id === id)
  if (at < 0) throw new VaultError('not_found', 'That category no longer exists.')
  categories.splice(at, 1)
  // Its items go back to whatever a suggestion says, as an unlabelled one would.
  for (const l of labels) if (l.category === id) l.category = null
}

function saveSettings(input: ScreenTimeSettings): void {
  const idle = input.idleMinutes
  if (!Number.isInteger(idle) || idle < 1 || idle > 60) {
    throw new VaultError('invalid', 'Idle time is between 1 and 60 minutes.')
  }
  const keep = input.titleDays
  if (!Number.isInteger(keep) || keep < 0) {
    throw new VaultError('invalid', 'Days titles are kept is a whole number, 0 for always.')
  }
  settings = {
    idleMinutes: idle,
    keepTitles: input.keepTitles === true,
    titleDays: keep,
    browsers: strArray(input.browsers)
      .map((b) => b.trim())
      .filter(Boolean),
  }
  if (!settings.keepTitles) for (const s of spans) s.title = ''
  // Old titles go; the time stays, as the vault keeps it.
  if (settings.titleDays > 0) {
    const cutoff = Date.now() - settings.titleDays * DAY_MS
    for (const s of spans) if (s.end < cutoff) s.title = ''
  }
}

function forgetItem(key: string): number {
  if (!/^(app|site):.+/.test(key)) throw new VaultError('invalid', 'That is not an app or a site.')
  const before = spans.length
  spans = spans.filter((s) => keyOf(s) !== key)
  return before - spans.length
}

function deleteRange(fromIso: string, toIso: string): number {
  const from = Date.parse(fromIso)
  const to = Date.parse(toIso)
  if (Number.isNaN(from) || Number.isNaN(to) || to <= from) {
    throw new VaultError('invalid', 'A range to delete needs a start before its end.')
  }
  let touched = 0
  const kept: Span[] = []
  for (const s of spans) {
    if (s.end <= from || s.start >= to) {
      kept.push(s)
      continue
    }
    touched += 1
    // What lies outside the range stays, trimmed to it.
    if (s.start < from) kept.push({ ...s, end: from })
    if (s.end > to) {
      spanCount += 1
      kept.push({ ...s, id: `ss-${spanCount}`, start: to })
    }
  }
  spans = kept.sort((a, b) => a.start - b.start)
  return touched
}

/** Heartbeats in, joined into spans -- the mock's `merge`. */
function record(args: Record<string, unknown>): IngestReport {
  const device = str(args.device).trim()
  const collector = str(args.collector).trim().toLowerCase()
  if (!device) throw new VaultError('invalid', 'A heartbeat batch names its device.')
  if (!/^[a-z0-9-]{1,40}$/.test(collector)) {
    throw new VaultError('invalid', 'A collector is named in lower-case letters, digits and -.')
  }
  const beats = Array.isArray(args.beats) ? (args.beats as Record<string, unknown>[]) : []
  if (beats.length > 5000)
    throw new VaultError('invalid', 'A batch carries at most 5000 heartbeats.')
  const report: IngestReport = { accepted: 0, rejected: 0, spans: 0 }
  const now = Date.now()
  const touched = new Set<string>()
  for (const beat of beats) {
    const kind = beat.kind
    const appId = str(beat.app).trim()
    const start = Date.parse(str(beat.start))
    const end = beat.end === undefined || beat.end === null ? start : Date.parse(str(beat.end))
    const ok =
      (kind === 'app' || kind === 'site') &&
      appId !== '' &&
      !Number.isNaN(start) &&
      !Number.isNaN(end) &&
      end >= start &&
      start >= now - SEED_DAYS * DAY_MS &&
      end <= now + 5 * MINUTE &&
      (kind === 'app' || str(beat.host).trim() !== '')
    if (!ok) {
      report.rejected += 1
      continue
    }
    report.accepted += 1
    const host = kind === 'site' ? normalizeHost(str(beat.host)) : ''
    const title = settings.keepTitles ? str(beat.title).slice(0, 500) : ''
    const last = [...spans].reverse().find((s) => s.device === device && s.collector === collector)
    if (
      last &&
      last.kind === kind &&
      last.app === appId &&
      last.host === host &&
      start - last.end <= PULSE_MS &&
      start >= last.start
    ) {
      last.end = Math.max(last.end, end)
      if (title) last.title = title
      touched.add(last.id)
      continue
    }
    spanCount += 1
    const id = `ss-${spanCount}`
    spans.push({
      id,
      device,
      collector,
      kind,
      app: appId,
      name: str(beat.name).trim(),
      host,
      title,
      start,
      end,
    })
    touched.add(id)
  }
  spans.sort((a, b) => a.start - b.start)
  report.spans = touched.size
  return report
}

// ── This machine: the shell's half ───────────────────────────────────────
//
// The laptop, on GNOME, with the extension installed and running, and
// Firefox's extension connected.

const LISTENING = 'http://127.0.0.1:7399'
const shell = {
  tracking: true,
  device: 'laptop',
  installed: true,
  enabled: true,
  /** When tracking was last turned off, for `lastSent`. */
  stoppedAt: 0,
  connections: 0,
}

function shellStatus(): ScreenTimeStatus {
  const now = Date.now()
  const sent = shell.tracking ? now - 12 * SECOND : shell.stoppedAt
  return {
    tracking: shell.tracking,
    device: shell.device,
    backend: 'gnome',
    note: '',
    extension: {
      installed: shell.installed,
      enabled: shell.enabled,
      active: shell.installed && shell.enabled && shell.tracking,
      needsLogin: false,
    },
    listening: LISTENING,
    lastSent: new Date(sent).toISOString(),
    lastError: null,
    clients: [
      { collector: 'firefox', lastSeen: new Date(now - 8 * SECOND).toISOString() },
      { collector: 'gnome', lastSeen: new Date(now - 15 * SECOND).toISOString() },
    ],
  }
}

function connectBrowser(browser: string): BrowserConnection {
  const name = browser.trim().toLowerCase()
  if (!/^[a-z0-9-]{1,40}$/.test(name)) {
    throw new VaultError('invalid', 'A browser is named like “firefox” or “chrome”.')
  }
  shell.connections += 1
  const token = `mock-heartbeats-${name}-${hash(`${name}/${shell.connections}`).toString(36)}`
  const code =
    CONNECTION_PREFIX +
    base64Url(JSON.stringify({ endpoint: LISTENING, token, device: shell.device }))
  return { code, endpoint: LISTENING, device: shell.device }
}

// ── For other mocks ──────────────────────────────────────────────────────

/**
 * Minutes of screen time per local day in `from..=to` (either open), in
 * `category` and of the item `key` (`site:youtube.com`) when given --
 * what a tracker with a `screenTime` source reads (`report::minutes_by_day`).
 * Days with nothing are absent.
 */
export function mockScreenMinutes(
  from: string | null | undefined,
  to: string | null | undefined,
  category: ScreenCategoryId | null | undefined,
  key: string | null | undefined,
): Map<string, number> {
  const out = new Map<string, number>()
  const lo = from && isDay(from) ? startOfDay(from).getTime() : -Infinity
  const hi = to && isDay(to) ? startOfDay(addDays(to, 1)).getTime() : Infinity
  for (const run of resolve(spansIn([], lo, hi))) {
    if (category && run.category !== category) continue
    if (key && run.key !== key) continue
    const s = Math.max(run.start, lo)
    const e = Math.min(run.end, hi)
    byHour(s, e, (at, ms) => {
      const day = isoDate(at)
      out.set(day, (out.get(day) ?? 0) + ms / MINUTE)
    })
  }
  return out
}

// ── The one entry point ──────────────────────────────────────────────────

export function mockScreenTime(cmd: string, args: Record<string, unknown>): unknown {
  switch (cmd) {
    case 'record_screen_time':
      return record(args)
    case 'screen_time_collector':
      return { idleMinutes: settings.idleMinutes, keepTitles: settings.keepTitles }
    case 'screen_time_report':
      return wire(report(wire((args.query as ScreenQuery | undefined) ?? ({} as ScreenQuery))))
    case 'screen_time_timeline':
      return wire(timeline(str(args.date), args.tz, strArray(args.devices)))
    case 'list_screen_apps':
      return wire(appRows())
    case 'save_screen_app':
      saveLabel(wire(args.app as ScreenApp))
      return undefined
    case 'delete_screen_app': {
      const at = labels.findIndex((l) => l.id === str(args.id))
      if (at < 0) throw new VaultError('not_found', 'That label no longer exists.')
      labels.splice(at, 1)
      return undefined
    }
    case 'list_screen_categories':
      return wire(categoriesInOrder())
    case 'save_screen_category':
      saveCategory(wire(args.category as ScreenCategory))
      return undefined
    case 'delete_screen_category':
      deleteCategory(str(args.id))
      return undefined
    case 'list_screen_devices':
      return wire(deviceList())
    case 'screen_time_settings':
      return wire(settings)
    case 'save_screen_time_settings':
      saveSettings(wire(args.settings as ScreenTimeSettings))
      return undefined
    case 'forget_screen_item':
      return forgetItem(str(args.key))
    case 'delete_screen_time':
      return deleteRange(str(args.from), str(args.to))

    case 'screentime_status':
      return shellStatus()
    case 'screentime_set_tracking':
      if (shell.tracking && args.on !== true) shell.stoppedAt = Date.now()
      shell.tracking = args.on === true
      return shellStatus()
    case 'screentime_set_device': {
      const name = str(args.name).trim()
      if (!name) throw new VaultError('invalid', 'This computer needs a name.')
      if (name.length > 100) throw new VaultError('invalid', 'A name is at most 100 characters.')
      shell.device = name
      return shellStatus()
    }
    case 'screentime_install_gnome_extension':
      shell.installed = true
      shell.enabled = true
      return shellStatus()
    case 'screentime_connect_browser':
      return connectBrowser(str(args.browser))
  }
  throw new VaultError('unknown', `no mock for command ${cmd}`)
}
