// The Feed's rules that need no store: how sources fold into the sidebar's
// folders, how a list is ordered and narrowed, and the short lines a row says
// about a story.
//
// Separate from `feed.svelte.ts` for the reason `mail.ts` is separate from
// `mail.svelte.ts`: these are decisions, and the way they fail is quiet. A
// folder sorted by the wrong comparison, a search that misses the source's
// own name, a "points" sort that puts a story with no score above one with a
// hundred -- none of them throws and none fails to compile, so they are
// tested on their own in `scripts/feed.test.mjs`.
//
// Types come from the store as `import type`, which is erased: the store
// imports this file at run time, and a value import back would be a cycle
// between two modules that both construct things when they load.

import type { FeedFilter, FeedFolder, FeedScope, FeedSort } from './feed.svelte'
import type { FeedSettings, HnList, NewsSource, RankingStatus, SourceSummary, Story } from './types'

// ── Words ────────────────────────────────────────────────────────────────

/** What each chip over the list says. */
export const FILTER_LABELS: Record<FeedFilter, string> = {
  unread: 'Unread',
  all: 'All',
  saved: 'Saved',
}

/** Hacker News's lists, as the add sheet and the sidebar name them. */
export const HN_LIST_LABELS: Record<HnList, string> = {
  front: 'Front page',
  show: 'Show HN',
  ask: 'Ask HN',
  top: 'Everything above a score',
}

/** One line under each list in the add sheet. */
export const HN_LIST_NOTES: Record<HnList, string> = {
  front: 'The stories on the front page at each refresh.',
  show: 'What people have made and are showing.',
  ask: 'Questions put to the whole site.',
  top: 'Every story above the points you set, newest first.',
}

/** The sidebar's name for the folder that has no name. */
export const UNFILED = 'Unfiled'

/** How long stories are kept when a source says nothing: read, then unread. */
export const DEFAULT_RETENTION = { readDays: 30, unreadDays: 90 } as const

/** The longest a source may keep a story. A year, which is also when a tombstone goes. */
export const MAX_RETENTION_DAYS = 365

// ── Addresses ────────────────────────────────────────────────────────────

/** The host of an address without a leading `www.`, or empty when it has none. */
export function hostOf(url: string): string {
  try {
    return new URL(url).host.toLowerCase().replace(/^www\./, '')
  } catch {
    return ''
  }
}

/** Whether `url` is one a browser should be sent to: a web address, nothing else. */
export function isWebUrl(url: string): boolean {
  return /^https?:\/\//i.test(url.trim()) && hostOf(url) !== ''
}

/**
 * An address as it is compared: without its scheme, a leading `www.`, a
 * trailing slash or the case of its host. `http://example.com/feed/` and
 * `https://www.Example.com/feed` are one feed, and the directory and the
 * sidebar do not always spell it the same way.
 */
export function feedKey(url: string): string {
  const raw = url.trim()
  if (!raw) return ''
  try {
    const parsed = new URL(/^[a-z][a-z0-9+.-]*:\/\//i.test(raw) ? raw : `https://${raw}`)
    const path = parsed.pathname.replace(/\/+$/, '')
    return `${parsed.host.toLowerCase().replace(/^www\./, '')}${path}${parsed.search}`
  } catch {
    return raw.toLowerCase()
  }
}

/** Whether two addresses are the same feed -- see {@link feedKey}. */
export function sameFeedAddress(a: string, b: string): boolean {
  const key = feedKey(a)
  return key !== '' && key === feedKey(b)
}

/**
 * Does what was typed look like an address rather than words to search for?
 *
 * A web address, or something shaped like one -- a host with a dot and a
 * suffix, no spaces. A guess, and a cheap one to get wrong: it only decides
 * whether the directory also offers to look for a feed *at* the text, beside
 * the search it runs either way.
 */
export function looksLikeAddress(text: string): boolean {
  const typed = text.trim()
  if (!typed || /\s/.test(typed)) return false
  if (isWebUrl(typed)) return true
  return /^(?:[a-z0-9-]+\.)+[a-z]{2,}(?:[/?#]\S*)?$/i.test(typed)
}

/** The page a source belongs to, for "Open site". Empty for mail, which has none. */
export function siteOf(source: NewsSource): string {
  const kind = source.kind
  if (kind.type === 'mail' || kind.type === 'newsletters') return ''
  const own = source.siteUrl.trim()
  if (isWebUrl(own)) return own
  if (kind.type === 'hackerNews') return 'https://news.ycombinator.com'
  // A feed's own address is rarely the page; its origin is the best guess left.
  try {
    return isWebUrl(kind.url) ? new URL(kind.url).origin : ''
  } catch {
    return ''
  }
}

/**
 * What a source is called where there is room for one name.
 *
 * Its title, then the host it comes from, then what kind of thing it is: a
 * source added a moment ago has no title until its first refresh has read one
 * out of the feed.
 */
export function sourceLabel(source: NewsSource): string {
  const title = source.title.trim()
  if (title) return title
  const kind = source.kind
  if (kind.type === 'hackerNews') return HN_LIST_LABELS[kind.list]
  if (kind.type === 'mail') return kind.sender
  if (kind.type === 'newsletters') return 'Newsletters'
  return hostOf(kind.url) || kind.url
}

// ── The sidebar ──────────────────────────────────────────────────────────

function compareText(a: string, b: string): number {
  return a.localeCompare(b, undefined, { sensitivity: 'base' }) || (a < b ? -1 : a > b ? 1 : 0)
}

/** Is this source a show? A web feed whose items turned out to be episodes. */
export function isPodcastSource(source: NewsSource | undefined | null): boolean {
  return !!source?.podcast
}

/** The shows among `sources`, in the order they were given. */
export function podcastSources(sources: SourceSummary[]): SourceSummary[] {
  return sources.filter((s) => isPodcastSource(s.source))
}

/**
 * Sources folded into the sidebar's folders.
 *
 * The unfiled sources come first, under the empty name, because they are the
 * ones with nothing to open; the folders follow alphabetically, ignoring
 * case. Within a folder the sources keep the order they were given in, which
 * is the order they were put in (`sortOrder`), not an alphabetical one -- how
 * somebody arranged their sidebar is theirs to keep.
 *
 * Shows are left out: the sidebar lists them in a group of their own, and a
 * show that was in a folder as well would be the same row twice.
 */
export function groupFolders(sources: SourceSummary[]): FeedFolder[] {
  const byName = new Map<string, FeedFolder>()
  for (const summary of sources) {
    if (isPodcastSource(summary.source)) continue
    const name = summary.source.folder.trim()
    let folder = byName.get(name)
    if (!folder) {
      folder = { name, sources: [], unread: 0 }
      byName.set(name, folder)
    }
    folder.sources.push(summary)
    folder.unread += summary.unread
  }
  const unfiled = byName.get('')
  const named = [...byName.values()]
    .filter((f) => f.name !== '')
    .sort((a, b) => compareText(a.name, b.name))
  return unfiled ? [unfiled, ...named] : named
}

/** Every folder name in use, for the datalist a source's folder field offers. */
export function folderNames(sources: SourceSummary[]): string[] {
  return groupFolders(sources)
    .map((f) => f.name)
    .filter((name) => name !== '')
}

/** What the list's heading says for `scope`. */
export function scopeTitle(scope: FeedScope, sources: SourceSummary[]): string {
  if (scope.kind === 'all') return 'All'
  if (scope.kind === 'upnext') return 'Up Next'
  if (scope.kind === 'recommended') return 'Recommended'
  if (scope.kind === 'podcasts') return 'Podcasts'
  if (scope.kind === 'folder') return scope.name.trim() || UNFILED
  const found = sources.find((s) => s.source.id === scope.id)
  return found ? sourceLabel(found.source) : 'Feed'
}

/** How many stories in `scope` are unread, from the same counts the sidebar draws. */
export function scopeUnread(scope: FeedScope, sources: SourceSummary[]): number {
  if (scope.kind === 'source') {
    return sources.find((s) => s.source.id === scope.id)?.unread ?? 0
  }
  // Up Next and Recommended are lists of their own, counted by what they hold.
  if (scope.kind === 'upnext' || scope.kind === 'recommended') return 0
  if (scope.kind === 'podcasts') {
    return podcastSources(sources).reduce((n, s) => n + s.unread, 0)
  }
  // A folder holds no shows -- they have a group of their own (see
  // `groupFolders`) -- so a show filed in one is not counted in it.
  return sources
    .filter(
      (s) =>
        scope.kind === 'all' ||
        (!isPodcastSource(s.source) && s.source.folder.trim() === scope.name.trim()),
    )
    .reduce((n, s) => n + s.unread, 0)
}

/** Is this the one kind of source where points mean something? */
export function isHackerNews(source: NewsSource | undefined | null): boolean {
  return source?.kind.type === 'hackerNews'
}

/** What the confirmation before unfollowing says is lost, and what is not. */
export function unfollowDetail(source: NewsSource): string {
  const kept =
    source.kind.type === 'mail' || source.kind.type === 'newsletters'
      ? 'The messages stay in Mail.'
      : isPodcastSource(source)
        ? 'Episodes already downloaded stay in your files, and so does anything you added to the Library.'
        : 'Anything you added to the Library stays there.'
  return `Its stories are removed from this device, saved ones included. ${kept}`
}

// ── The list ─────────────────────────────────────────────────────────────

function whenMs(story: Story): number {
  const t = Date.parse(story.publishedAt)
  return Number.isNaN(t) ? 0 : t
}

/** Newest first, ties broken the way the server pages: by id, descending. */
function byNewest(a: Story, b: Story): number {
  return whenMs(b) - whenMs(a) || (a.id < b.id ? 1 : a.id > b.id ? -1 : 0)
}

/**
 * A list in the order it is drawn.
 *
 * `points` ranks by score and then by age. A story with no score at all sorts
 * as -1, so it falls below one with a score of nothing: a story from a blog
 * has not been voted on, and a Hacker News story with no votes yet has. Never
 * mutates its input; the store holds the list in the order the server paged it.
 */
export function sortStories(stories: Story[], sort: FeedSort): Story[] {
  const out = [...stories]
  if (sort === 'points') {
    out.sort((a, b) => (b.score ?? -1) - (a.score ?? -1) || byNewest(a, b))
  } else {
    out.sort(byNewest)
  }
  return out
}

/**
 * The stories that match what was typed in the bar.
 *
 * Case-insensitive, over the title, the summary, the author and the name of
 * the source it came from -- so "verge" finds a site's stories even when no
 * title says so. Every word has to be found somewhere, not all in one place,
 * which is what makes "rust async" find a story about async in a Rust blog.
 * An empty query is everything, not nothing.
 */
export function filterStories(stories: Story[], query: string, sources: SourceSummary[]): Story[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean)
  if (words.length === 0) return stories
  const names = new Map(sources.map((s) => [s.source.id, sourceLabel(s.source).toLowerCase()]))
  return stories.filter((story) => {
    const haystack = [story.title, story.summary, story.author, names.get(story.sourceId) ?? '']
      .join('\n')
      .toLowerCase()
    return words.every((word) => haystack.includes(word))
  })
}

/** "▲ 412 · 180 comments" for a story with a score, or just what it has. */
export function pointsLine(story: Pick<Story, 'score' | 'comments'>): string {
  const parts: string[] = []
  if (story.score !== null && story.score !== undefined) {
    parts.push(`▲ ${story.score.toLocaleString()}`)
  }
  if (story.comments !== null && story.comments !== undefined) {
    parts.push(
      `${story.comments.toLocaleString()} ${story.comments === 1 ? 'comment' : 'comments'}`,
    )
  }
  return parts.join(' · ')
}

/**
 * Who wrote a story, unless that is only the source's name again.
 *
 * Most blogs put theirs in both places, and saying it twice reads as a
 * stutter.
 */
export function distinctAuthor(story: Pick<Story, 'author'>, sourceName: string): string {
  const author = story.author.trim()
  return author.toLowerCase() === sourceName.trim().toLowerCase() ? '' : author
}

/** The parts of the line under a row's title: the source, then who wrote it. */
export function bylineParts(story: Pick<Story, 'author'>, sourceName: string): string[] {
  const parts: string[] = []
  if (sourceName.trim()) parts.push(sourceName.trim())
  const author = distinctAuthor(story, sourceName)
  if (author) parts.push(author)
  return parts
}

// ── Also on ──────────────────────────────────────────────────────────────

/** One other place a story was found, ready to draw. */
export interface AlsoOn {
  sourceId: string
  /** "Hacker News", or the other source's own name. */
  name: string
  /** "412 points · 180 comments", or empty when it had neither. */
  detail: string
  /** Where the discussion is, or empty. */
  url: string
}

/**
 * The other sources a story was also found on.
 *
 * The same article from its blog and from Hacker News is one row, and this
 * is what says so: "also on Hacker News · 412 points".
 */
export function alsoOn(story: Pick<Story, 'also'>, sources: SourceSummary[]): AlsoOn[] {
  return story.also.map((also) => {
    const source = sources.find((s) => s.source.id === also.sourceId)?.source
    const name = !source
      ? 'another source'
      : source.kind.type === 'hackerNews'
        ? 'Hacker News'
        : sourceLabel(source)
    const bits: string[] = []
    if (also.score !== null && also.score !== undefined) {
      bits.push(`${also.score.toLocaleString()} ${also.score === 1 ? 'point' : 'points'}`)
    }
    if (also.comments !== null && also.comments !== undefined) {
      bits.push(`${also.comments.toLocaleString()} ${also.comments === 1 ? 'comment' : 'comments'}`)
    }
    return {
      sourceId: also.sourceId,
      name,
      detail: bits.join(' · '),
      url: isWebUrl(also.discussionUrl) ? also.discussionUrl : '',
    }
  })
}

/** {@link alsoOn} as one sentence, for a tooltip: "Also on Hacker News · 412 points". */
export function alsoLine(story: Pick<Story, 'also'>, sources: SourceSummary[]): string {
  const places = alsoOn(story, sources).map((p) => (p.detail ? `${p.name} · ${p.detail}` : p.name))
  return places.length > 0 ? `Also on ${places.join(', ')}` : ''
}

// ── The reading pane ─────────────────────────────────────────────────────

/** A body's visible words, without the document around them. */
export function plainText(html: string): string {
  return html
    .replace(/<head\b[\s\S]*?<\/head>/i, ' ')
    .replace(/<(script|style)\b[\s\S]*?<\/\1>/gi, ' ')
    .replace(/<[^>]*>/g, ' ')
    .replace(/&(?:hellip|#8230|#x2026);/gi, '…')
    .replace(/&nbsp;/gi, ' ')
    .replace(/\s+/g, ' ')
    .trim()
}

// How a feed says "this is only the start": an ellipsis, a "Read more", or
// WordPress's own footer. Anchored to the end, with only closing punctuation
// allowed after it (not `\W`, which would count every letter outside ASCII).
const CUT_OFF =
  /(?:\.{3}|…|\b(?:read more|continue reading|keep reading|read the full (?:article|story|post))\b[^.]{0,80}|\bappeared first on\b[^.]{0,80})[\s.)\]"'”’»›→]*$/i

/**
 * Does a body stop short of the article it belongs to?
 *
 * A feed that carries only an excerpt says so in the usual ways: it ends on
 * an ellipsis, on "Read more", or on WordPress's "The post … appeared first
 * on …". Only the tail is read, so a long article that happens to use one
 * mid-way is left alone. A guess, and a cheap one to get wrong: the cost of a
 * miss is a "Fetch the full article" button that is not offered, with the
 * original one click away.
 */
export function looksCutOff(html: string): boolean {
  const text = plainText(html)
  return text.length > 0 && CUT_OFF.test(text.slice(-160))
}

// ── Recommended ──────────────────────────────────────────────────────────

/** The one line under Recommended's heading about how it is ordered. */
export interface RankingNote {
  /**
   * What is the matter. `off`, `criteria` and `judge` are things Settings →
   * Feed can put right, so the view links there; the rest is only said.
   */
  kind: 'off' | 'criteria' | 'judge' | 'problem' | 'working' | 'ready'
  text: string
}

/**
 * What to tell somebody about how Recommended is ordered, or `null` while
 * nothing is known yet.
 *
 * The three reasons it is plain newest-first come first, because they are the
 * ones with something to do about them -- no list can be ranked by nothing, or
 * with nobody to judge -- and each is said as what is true and what follows
 * ("newest first"), not as a fault. Settings are checked before the status
 * because they are known the moment the page opens and the status is not.
 */
export function rankingNote(
  status: RankingStatus | null | undefined,
  settings: FeedSettings | null | undefined,
): RankingNote | null {
  const ranking = settings?.ranking
  if (ranking && !ranking.enabled) {
    return {
      kind: 'off',
      text: 'Ranking is switched off, so Recommended is newest first. Switch it on in',
    }
  }
  if (ranking && ranking.criteria.length === 0 && !ranking.goals) {
    return {
      kind: 'criteria',
      text: 'There is nothing to rank by yet, so Recommended is newest first. Say what you like to read and hear in',
    }
  }
  if (!status) return null
  if (!status.judge) {
    return {
      kind: 'judge',
      text: 'There is no model to rank with yet, so Recommended is newest first. Set one up in',
    }
  }
  if (status.problem) return { kind: 'problem', text: status.problem }
  const judge = status.judge === 'small' ? 'the Small model' : 'the decision model'
  if (status.waiting > 0) {
    return {
      kind: 'working',
      text: `Ranking with ${judge}: ${status.ranked.toLocaleString()} done, ${status.waiting.toLocaleString()} to go.`,
    }
  }
  return { kind: 'ready', text: `Ranked by your criteria with ${judge}.` }
}

// ── Settings ─────────────────────────────────────────────────────────────

/** Mute words as typed -- commas or new lines -- as a clean list. */
export function parseMute(text: string): string[] {
  const seen = new Set<string>()
  const out: string[] = []
  for (const piece of text.split(/[,\n]/)) {
    const word = piece.trim()
    const key = word.toLowerCase()
    if (!word || seen.has(key)) continue
    seen.add(key)
    out.push(word)
  }
  return out
}

/** Mute words as they are shown in the field. */
export function muteText(words: string[]): string {
  return words.join(', ')
}

/** A day count from a number field: a whole number from 1 to a year, else `fallback`. */
export function clampDays(value: number | null | undefined, fallback: number): number {
  if (value === null || value === undefined || !Number.isFinite(value)) return fallback
  return Math.min(MAX_RETENTION_DAYS, Math.max(1, Math.round(value)))
}

/** A points threshold from a number field: a whole number, never negative. */
export function clampPoints(value: number | null | undefined): number {
  if (value === null || value === undefined || !Number.isFinite(value)) return 0
  return Math.min(100_000, Math.max(0, Math.round(value)))
}

// ── The Overview ─────────────────────────────────────────────────────────

/** A folder and how much is unread in it, for the Reading card. */
export interface FolderUnread {
  name: string
  /** What to call it: the folder's name, or {@link UNFILED}. */
  label: string
  unread: number
}

/** Folders with something unread, most first; a quiet folder is not worth a row. */
export function folderUnread(sources: SourceSummary[]): FolderUnread[] {
  return groupFolders(sources)
    .filter((f) => f.unread > 0)
    .map((f) => ({ name: f.name, label: f.name || UNFILED, unread: f.unread }))
    .sort((a, b) => b.unread - a.unread || compareText(a.label, b.label))
}

/**
 * The few stories worth a look from a pile of unread ones.
 *
 * Each kind of source is ranked by what it can say: Hacker News by points,
 * everything else newest first. The two are then dealt out in turn, starting
 * with whichever has the newer story, so a front page that is always full
 * does not crowd out the sites somebody chose to follow, and the card is not
 * three blog posts when there is a story with six hundred points behind it.
 */
export function topStories(stories: Story[], sources: SourceSummary[], count = 3): Story[] {
  const hn = new Set(sources.filter((s) => isHackerNews(s.source)).map((s) => s.source.id))
  const scored = sortStories(
    stories.filter((s) => hn.has(s.sourceId)),
    'points',
  )
  const rest = sortStories(
    stories.filter((s) => !hn.has(s.sourceId)),
    'newest',
  )
  const out: Story[] = []
  let fromScored = scored.length > 0 && (rest.length === 0 || whenMs(scored[0]!) > whenMs(rest[0]!))
  while (out.length < count && (scored.length > 0 || rest.length > 0)) {
    const pool = fromScored && scored.length > 0 ? scored : rest.length > 0 ? rest : scored
    out.push(pool.shift()!)
    fromScored = !fromScored
  }
  return out
}

// ── Sizing a story's frame ───────────────────────────────────────────────

/**
 * Guess a story's rendered height in pixels from its document string, so the
 * frame can be as tall as the article and the reading pane scrolls as one
 * page. Never by reading the frame back: it is sandboxed without
 * `allow-same-origin`, for the reason `MailThread.svelte` gives.
 *
 * Mail's `estimateBodyHeight` is built for a message -- a few paragraphs,
 * capped at 2400px -- and counts no pictures. An article is long and
 * illustrated, so this counts what an article has: each picture at its
 * stated size scaled to the column (or a guess for one that does not say),
 * headings, code blocks line by line, and the paragraph gaps, at the story
 * document's own type (15px, line-height 1.65, 16px padding -- `STYLE` in
 * `news/view.rs`). It errs long rather than short: a little blank under the
 * last line costs nothing, an inner scrollbar inside the page is the glitch.
 */
export function estimateArticleHeight(html: string, widthPx: number): number {
  const body = html.replace(/^[\s\S]*<body[^>]*>/i, '').replace(/<\/body>[\s\S]*$/i, '')
  const column = Math.max(200, widthPx - 32)
  const charsPerLine = Math.max(20, Math.floor(column / 8))
  const lineHeightPx = 25

  // Code keeps its own lines, so it is counted apart from the prose.
  let codeLines = 0
  const prose = body.replace(/<pre[\s\S]*?<\/pre>/gi, (block) => {
    codeLines += block.replace(/<[^>]*>/g, '').split('\n').length
    return '\n'
  })
  const textLines = prose
    .replace(/<(br|\/p|\/div|\/li|\/blockquote|\/h[1-6]|\/figcaption|\/tr)[ >/]/gi, (m) => `\n${m}`)
    .split('\n')
    .map((s) =>
      s
        .replace(/<[^>]*>/g, ' ')
        .replace(/\s+/g, ' ')
        .trim(),
    )
    .filter((s) => s.length > 0)
    .reduce((n, s) => n + Math.max(1, Math.ceil(s.length / charsPerLine)), 0)

  const pictures = (body.match(/<img\b[^>]*>/gi) ?? []).reduce((sum, tag) => {
    const width = Number(/\bwidth\s*=\s*["']?(\d+)/i.exec(tag)?.[1] ?? 0)
    const height = Number(/\bheight\s*=\s*["']?(\d+)/i.exec(tag)?.[1] ?? 0)
    if (width > 0 && height > 0) return sum + Math.round(height * Math.min(1, column / width))
    return sum + Math.round(Math.min(column, 640) * 0.6)
  }, 0)
  const headings = (body.match(/<h[1-6][\s>]/gi) ?? []).length
  const blocks = (body.match(/<(p|blockquote|li|figure|pre|h[1-6])[\s>]/gi) ?? []).length

  const estimate =
    textLines * lineHeightPx +
    codeLines * 20 +
    pictures +
    headings * 16 +
    blocks * 16 +
    32 +
    2 * lineHeightPx
  return Math.min(60000, Math.max(160, estimate))
}
