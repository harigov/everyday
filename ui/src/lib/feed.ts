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
import { friendlyDate, plural } from './format'
import type { IconName } from './icons'
import { episodeProgress, type EpisodeProgress } from './podcast'
import { isoDate } from './time'
import type { ArticleType } from './reading'
import { summarize } from './unsubscribe'
import type {
  Consumption,
  FeedKind,
  FeedSettings,
  HnList,
  MailCategory,
  NewsSource,
  NewsSourceId,
  NotNewslettersReport,
  RankingStatus,
  SourceSummary,
  Story,
  UnsubscribeOutcome,
} from './types'

// ── Words ────────────────────────────────────────────────────────────────

/** What each chip over the list says. */
export const FILTER_LABELS: Record<FeedFilter, string> = {
  unread: 'Unread',
  all: 'All',
  saved: 'Saved',
}

/** The chips over the sidebar, in the order they are drawn: everything, or one kind of feed. */
export const FEED_KINDS = ['all', 'newsletter', 'feed', 'podcast'] as const
/** What a kind chip can be: every kind, or one of {@link FeedKind}. */
export type KindChoice = (typeof FEED_KINDS)[number]

/** What each kind chip says. */
export const KIND_LABELS: Record<KindChoice, string> = {
  all: 'All',
  newsletter: 'Newsletters',
  feed: 'Feeds',
  podcast: 'Podcasts',
}

/** What the sidebar says when a chip is on and there is nothing of that kind to list. */
export const KIND_NONE: Record<FeedKind, string> = {
  newsletter: 'No newsletters yet.',
  feed: 'No feeds followed.',
  podcast: 'No podcasts followed.',
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
  if (kind.type === 'mail') return kind.name?.trim() || kind.sender
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
 * Which chip a source is under: a podcast when it has a show's settings, a
 * newsletter when it reads mail, a feed otherwise. The same answer as the
 * service's `NewsSource::kind_group`, so a count made here and one made there
 * are about the same set.
 */
export function kindOf(source: NewsSource): FeedKind {
  if (source.podcast) return 'podcast'
  const type = source.kind.type
  return type === 'mail' || type === 'newsletters' ? 'newsletter' : 'feed'
}

/** Is `source` under the chip `kind`? Every source is under All. */
export function isOfKind(source: NewsSource, kind: KindChoice): boolean {
  return kind === 'all' || kindOf(source) === kind
}

/**
 * The Newsletters source with nothing left under it. Its issues go to the
 * sources found for each newsletter, so what is left under it is only what was
 * fetched before that, and once that is gone the row is a door to an empty room.
 */
function isEmptyCatchAll(summary: SourceSummary): boolean {
  // Unless something is wrong with it: every found newsletter's issues come
  // through its turn, so a paused or failing one must have a row to be fixed
  // from.
  return (
    summary.source.kind.type === 'newsletters' &&
    summary.total === 0 &&
    !summary.source.paused &&
    !summary.lastError
  )
}

/**
 * The sources the sidebar has rows for under the chip `kind`.
 *
 * `open` is the source on screen, and its row stays whatever the chip says --
 * the way a folder stays unfolded over the open source -- since a list with
 * nothing in the sidebar marked as where it came from reads as lost.
 */
export function listedSources(
  sources: SourceSummary[],
  kind: KindChoice = 'all',
  open: NewsSourceId | null = null,
): SourceSummary[] {
  return sources.filter(
    (s) => s.source.id === open || (isOfKind(s.source, kind) && !isEmptyCatchAll(s)),
  )
}

/**
 * The sources a place in the sidebar covers, under the chip `kind`.
 *
 * Where the list of stories, the unread count and "mark all read" all get
 * their sources from, so they cannot disagree about what is in view: All is
 * every source of the kind, and a folder is the sources of the kind filed in
 * it. One source is itself, whatever the chip. Up Next, Recommended and
 * History are not lists of sources.
 */
export function scopeSources(
  scope: FeedScope,
  sources: SourceSummary[],
  kind: KindChoice = 'all',
): SourceSummary[] {
  if (scope.kind === 'source') return sources.filter((s) => s.source.id === scope.id)
  if (scope.kind === 'folder') {
    const wanted = scope.name.trim()
    return sources.filter((s) => s.source.folder.trim() === wanted && isOfKind(s.source, kind))
  }
  if (scope.kind === 'all') return sources.filter((s) => isOfKind(s.source, kind))
  return []
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
 * Every source is in one, shows and newsletters with the rest: what kind a
 * source is a chip's business, and a folder is whatever somebody put in it.
 */
export function groupFolders(sources: SourceSummary[]): FeedFolder[] {
  const byName = new Map<string, FeedFolder>()
  for (const summary of sources) {
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

/** A folder, and the rows of it the sidebar's search leaves showing. */
export interface NavFolder {
  folder: FeedFolder
  rows: SourceSummary[]
}

/**
 * The folders as the sidebar's search leaves them.
 *
 * Every word typed has to be in a source's name, in any order and in any case
 * -- "tech review" finds "MIT Technology Review". A folder whose own name
 * matches keeps every row it has, since somebody looking for "Engineering"
 * wants what is in it; a folder with no match in it is gone. The unfiled
 * sources have no folder name to match. Nothing typed is everything.
 */
export function searchNav(folders: FeedFolder[], query: string): NavFolder[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean)
  const found = (text: string) => words.every((word) => text.toLowerCase().includes(word))
  const out: NavFolder[] = []
  for (const folder of folders) {
    const wholeFolder = words.length === 0 || (folder.name !== '' && found(folder.name))
    const rows = wholeFolder
      ? folder.sources
      : folder.sources.filter((s) => found(sourceLabel(s.source)))
    if (rows.length > 0) out.push({ folder, rows })
  }
  return out
}

/** What the list's heading says for `scope`, with the chip `kind` over All. */
export function scopeTitle(
  scope: FeedScope,
  sources: SourceSummary[],
  kind: KindChoice = 'all',
): string {
  if (scope.kind === 'all') return kind === 'all' ? 'All' : `All ${KIND_LABELS[kind].toLowerCase()}`
  if (scope.kind === 'upnext') return 'Up Next'
  if (scope.kind === 'recommended') return 'Recommended'
  if (scope.kind === 'history') return 'History'
  if (scope.kind === 'folder') return scope.name.trim() || UNFILED
  const found = sources.find((s) => s.source.id === scope.id)
  return found ? sourceLabel(found.source) : 'Feed'
}

/** How many stories in `scope` are unread, from the same counts the sidebar draws. */
export function scopeUnread(
  scope: FeedScope,
  sources: SourceSummary[],
  kind: KindChoice = 'all',
): number {
  // Up Next, Recommended and History are lists of their own, counted by what they hold.
  return scopeSources(scope, sources, kind).reduce((n, s) => n + s.unread, 0)
}

/** Is this the one kind of source where points mean something? */
export function isHackerNews(source: NewsSource | undefined | null): boolean {
  return source?.kind.type === 'hackerNews'
}

/** What the confirmation before taking a found newsletter out of the Feed says. */
export const NOT_NEWSLETTER_DETAIL =
  'It goes from the Feed and will not be found again. Its emails stay in Mail.'

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

// ── Stopping several at once ─────────────────────────────────────────────
//
// The sidebar can pick sources and do one of two things to all of them:
// unsubscribe -- each newsletter's sender is asked to take you off the list,
// and each feed or show is unfollowed -- or say that newsletters are not
// newsletters, which the Feed gets wrong often enough to need saying in bulk.
// A source's own menu and settings ask the same questions about one, in the
// same words, so one and many behave alike.

/** Mail's tabs, by the names they have there: where a newsletter's emails can be moved. */
const MAIL_TABS: Record<MailCategory, string> = {
  priority: 'Priority',
  important: 'Important',
  other: 'Other',
  newsletter: 'Newsletters',
  notification: 'Notifications',
}

/**
 * Can this source be picked? Every one but the Newsletters source, which is
 * how newsletters are found rather than one of them: it has no sender to ask,
 * and saying it is not a newsletter would mean nothing.
 */
export function isPickable(source: NewsSource): boolean {
  return source.kind.type !== 'newsletters'
}

/** How many of `sources` are one sender's newsletter, and how many are feeds or shows. */
function countKinds(sources: readonly NewsSource[]): { letters: number; others: number } {
  const letters = sources.filter((s) => s.kind.type === 'mail').length
  return { letters, others: sources.length - letters }
}

/**
 * What the button that unsubscribes from `sources` says: a newsletter is
 * unsubscribed from, and feeds and shows on their own are only unfollowed.
 */
export function unsubscribeVerb(sources: readonly NewsSource[]): 'Unsubscribe' | 'Unfollow' {
  return countKinds(sources).letters > 0 ? 'Unsubscribe' : 'Unfollow'
}

/** The question before unsubscribing from `sources`. One is named. */
export function unsubscribeTitle(sources: readonly NewsSource[]): string {
  const { letters, others } = countKinds(sources)
  const [only] = sources
  if (sources.length === 1 && only) {
    const name = `“${sourceLabel(only)}”`
    return letters > 0 ? `Unsubscribe from ${name}?` : `Unfollow ${name}?`
  }
  if (letters === 0) return `Unfollow ${plural(others, 'source')}?`
  return `Unsubscribe from ${plural(sources.length, others === 0 ? 'newsletter' : 'source')}?`
}

/**
 * What unsubscribing from `sources` will do, said before anybody is asked.
 *
 * Not how each sender is asked -- a request, an email from your address or a
 * page to finish in the browser -- which is the service's to choose and each
 * outcome's to say afterwards.
 */
export function unsubscribeDetail(sources: readonly NewsSource[]): string {
  const { letters, others } = countKinds(sources)
  const said: string[] = []
  if (letters > 0) said.push(askedSentence(letters, others === 0))
  if (others > 0) said.push(unfollowedSentence(others, letters === 0))
  if (letters > 0) said.push('Emails you already have stay in Mail.')
  return said.join(' ')
}

/** What happens to `count` newsletters; `alone` when nothing else was picked with them. */
function askedSentence(count: number, alone: boolean): string {
  if (count === 1) {
    const who = alone ? 'Its sender' : 'The newsletter’s sender'
    return `${who} is asked to take you off the list, and it leaves the Feed.`
  }
  const who = alone ? 'Their senders' : 'The newsletters’ senders'
  return `${who} are asked to take you off their lists, and they leave the Feed.`
}

/** What happens to `count` feeds and shows; `alone` when no newsletter was picked with them. */
function unfollowedSentence(count: number, alone: boolean): string {
  if (count === 1) {
    const what = alone ? 'It is' : 'The other source is'
    return `${what} unfollowed, and its stories are removed from this device, saved ones included.`
  }
  const what = alone ? 'They are' : 'The other sources are'
  return `${what} unfollowed, and their stories are removed from this device, saved ones included.`
}

/**
 * What the toast says once `newsletters` newsletters have been unsubscribed
 * from and `unfollowed` feeds and shows unfollowed: how the asking went, in
 * the words Mail uses for it, and the unfollowing after that.
 */
export function unsubscribedNote(
  outcomes: readonly UnsubscribeOutcome[],
  newsletters: number,
  unfollowed: number,
): { title: string; body?: string; ok: boolean } {
  if (newsletters === 0) return { title: `Unfollowed ${plural(unfollowed, 'source')}`, ok: true }
  const said = summarize(outcomes)
  if (unfollowed === 0) return said
  const also = `Also unfollowed ${plural(unfollowed, 'other source')}.`
  return { ...said, body: said.body ? `${said.body} ${also}` : also }
}

/** The question before saying the newsletters named are not newsletters. One is named. */
export function notNewslettersTitle(names: readonly string[]): string {
  const [only] = names
  if (names.length === 1 && only !== undefined) return `“${only}” isn’t a newsletter?`
  return `${plural(names.length, 'source')} aren’t newsletters?`
}

/** What saying `count` newsletters are not newsletters does, before choosing where emails go. */
export function notNewslettersDetail(count: number): string {
  return count === 1
    ? NOT_NEWSLETTER_DETAIL
    : 'They go from the Feed and will not be found again. Their emails stay in Mail.'
}

/** One answer to "not newsletters?": where their emails go in Mail, `null` for nowhere. */
export interface NotNewsletterChoice {
  label: string
  category: MailCategory | null
}

/**
 * The answers to "not newsletters?". Their emails move out of Mail's
 * Newsletters to Other or Important, or stay where they are: the Feed lets
 * them go either way, and only the person knows which tab they belong in.
 */
export function notNewsletterChoices(count: number): NotNewsletterChoice[] {
  const their = count === 1 ? 'its' : 'their'
  return [
    { label: `Move ${their} emails to ${MAIL_TABS.other}`, category: 'other' },
    { label: `Move ${their} emails to ${MAIL_TABS.important}`, category: 'important' },
    { label: `Leave ${their} emails where they are`, category: null },
  ]
}

/** What the toast says once the newsletters named have been said not to be newsletters. */
export function notNewslettersNote(
  names: readonly string[],
  category: MailCategory | null,
  report: Pick<NotNewslettersReport, 'moved' | 'shared'> = { moved: 0, shared: 0 },
): { title: string; body: string } {
  const [only] = names
  const title =
    names.length === 1 && only !== undefined
      ? `Took “${only}” out of the Feed`
      : `Took ${plural(names.length, 'source')} out of the Feed`
  const its = names.length === 1 ? 'Its' : 'Their'
  // Says what happened in Mail, not what was asked: mail stays put when
  // another newsletter in the Feed comes from the same address, or when
  // there was nothing of theirs to move.
  let body: string
  if (!category) body = `${its} emails stay where they were in Mail.`
  else if (report.moved > 0 && report.shared === 0)
    body = `${its} emails are now under ${MAIL_TABS[category]} in Mail.`
  else if (report.moved > 0)
    body = `Moved ${plural(report.moved, 'email')} to ${MAIL_TABS[category]} in Mail. Mail from an address another newsletter in the Feed still uses stays in Newsletters.`
  else if (report.shared > 0)
    body = `${its} emails stay in Newsletters in Mail: another newsletter in the Feed comes from the same address.`
  else body = `${names.length === 1 ? 'It' : 'They'} had no emails in Mail to move.`
  return { title, body }
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

// ── History ──────────────────────────────────────────────────────────────
//
// What was read, heard and watched, newest first, in days. A record carries its
// own title, source and address and outlives its story, so everything a row
// says comes from the record and nothing here asks for the story.

/** One day of History: a heading, and what was taken in that day, newest first. */
export interface HistoryDay {
  /** The local calendar day, `YYYY-MM-DD`; empty for records with no usable time. */
  day: string
  /** "Today", "Yesterday", a weekday this week, then a date. */
  label: string
  items: Consumption[]
}

/** The local calendar day an instant falls on, or empty when it is not an instant. */
export function dayOfInstant(instant: string): string {
  const at = new Date(instant)
  return Number.isNaN(at.getTime()) ? '' : isoDate(at)
}

/** Most recently touched first; a record with no usable time last, ties by id. */
function byRecent(a: Consumption, b: Consumption): number {
  const x = Date.parse(a.lastAt)
  const y = Date.parse(b.lastAt)
  const one = Number.isNaN(x) ? Number.NEGATIVE_INFINITY : x
  const other = Number.isNaN(y) ? Number.NEGATIVE_INFINITY : y
  if (one !== other) return one > other ? -1 : 1
  return a.id < b.id ? 1 : a.id > b.id ? -1 : 0
}

/**
 * The record folded into days, newest day first.
 *
 * A day is the local calendar day of the record's last touch -- what somebody
 * would call "yesterday" -- so a thing heard at half past eleven at night is
 * on that night's day, not on the UTC one it happens to fall on. `now` is for
 * the headings and is handed in so a test never mixes its clock with the real
 * one.
 */
export function groupHistory(items: readonly Consumption[], now = new Date()): HistoryDay[] {
  const days: HistoryDay[] = []
  for (const record of [...items].sort(byRecent)) {
    const day = dayOfInstant(record.lastAt)
    let group = days.at(-1)
    if (!group || group.day !== day) {
      group = { day, label: day ? friendlyDate(day, now) : 'Earlier', items: [] }
      days.push(group)
    }
    group.items.push(record)
  }
  return days
}

/**
 * The records that match what was typed in the bar: every word found in the
 * title, the source or the address. An empty query is everything.
 */
export function filterHistory(items: readonly Consumption[], query: string): Consumption[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean)
  if (words.length === 0) return [...items]
  return items.filter((record) => {
    const haystack = [record.title, record.source, record.url].join('\n').toLowerCase()
    return words.every((word) => haystack.includes(word))
  })
}

/** The mark in front of a row: how the thing was taken in, and what it was. */
export function historyMark(record: Pick<Consumption, 'what' | 'mode' | 'mediaFile'>): IconName {
  if (record.mode === 'watch') return 'film'
  if (record.mode === 'listen') {
    if (record.what.type === 'file') return 'music'
    // An episode has its file; an article heard through Listen has none.
    return record.mediaFile ? 'podcast' : 'volume'
  }
  if (record.what.type === 'item') return 'book'
  return record.what.type === 'file' ? 'file' : 'newspaper'
}

/** What was done, in words, for a label: "Read", "Listened to", "Watched". */
export function historyModeWord(mode: Consumption['mode']): string {
  return mode === 'watch' ? 'Watched' : mode === 'listen' ? 'Listened to' : 'Read'
}

/**
 * How far a record got, as a row says it: "Finished", "12 min left", "40%". A
 * record with nothing to measure -- opened, and no more -- says so, in the
 * word for what was done, so a row is never blank where the rest have a bar.
 */
export function historyProgress(
  record: Pick<Consumption, 'mode' | 'progress' | 'finished' | 'positionMs' | 'durationMs'>,
): EpisodeProgress {
  const along = episodeProgress(record)
  if (along?.finished) return { ...along, label: 'Finished' }
  if (along) return along
  return { fraction: 0, finished: false, label: record.mode === 'read' ? 'Opened' : 'Started' }
}

// ── What a body was written from ─────────────────────────────────────────

/** Where a story's body came from: `BodyOrigin` in the core. */
export type BodyOrigin = 'feed' | 'extracted' | 'hackerNews' | 'transcript'

const BODY_ORIGINS: readonly BodyOrigin[] = ['feed', 'extracted', 'hackerNews', 'transcript']

/**
 * Where the body in a story's document came from, from the `news-body-origin`
 * meta tag the service writes into its head; `null` when the document does not
 * say. The origin rides in the document and not in a header because the
 * document is what both the shell's own protocol and the web server hand over,
 * and a header would need each of them to learn it.
 */
export function bodyOriginOf(html: string): BodyOrigin | null {
  const end = html.search(/<\/head>/i)
  if (end < 0) return null
  const found = /<meta\s+name=["']news-body-origin["']\s+content=["']([A-Za-z]+)["']/i.exec(
    html.slice(0, end),
  )
  return BODY_ORIGINS.find((origin) => origin === found?.[1]) ?? null
}

/** What the page an article was read from says about it: see {@link bodyAboutOf}. */
export interface BodyAbout {
  /** Who wrote it, as a name. */
  byline: string | null
  /** What the publication calls itself. */
  site: string | null
}

/** The five entities the service escapes a meta tag's content with, and nothing else. */
function unescapeAttribute(value: string): string {
  return value.replace(/&(amp|lt|gt|quot|#39);/g, (_, name: string) =>
    name === 'amp' ? '&' : name === 'lt' ? '<' : name === 'gt' ? '>' : name === 'quot' ? '"' : "'",
  )
}

/**
 * Who wrote an article and what the publication calls itself, from the
 * `news-byline` and `news-site` meta tags the service writes into the head of
 * a body read from the page itself (`wrap_stored` in `news/view.rs`), for the
 * same reason the origin rides there. Either is `null` when the document does
 * not say.
 */
export function bodyAboutOf(html: string): BodyAbout {
  const end = html.search(/<\/head>/i)
  const head = end < 0 ? '' : html.slice(0, end)
  const read = (name: string): string | null => {
    const found = new RegExp(`<meta\\s+name="${name}"\\s+content="([^"]*)"`, 'i').exec(head)
    const value = found ? unescapeAttribute(found[1]!).trim() : ''
    return value || null
  }
  return { byline: read('news-byline'), site: read('news-site') }
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
 * headings, code blocks line by line, and the paragraph gaps, in the type the
 * document is set in -- a story's own is {@link readingType} of the reading
 * settings, after `STYLE` in `news/view.rs`; without one, a newsletter's,
 * which is mail's document at 15px. It errs long rather than short: a little
 * blank under the last line costs nothing, an inner scrollbar inside the page
 * is the glitch.
 */
export function estimateArticleHeight(
  html: string,
  widthPx: number,
  type: ArticleType = MAIL_TYPE,
): number {
  const body = html.replace(/^[\s\S]*<body[^>]*>/i, '').replace(/<\/body>[\s\S]*$/i, '')
  const column = Math.max(200, widthPx - 2 * type.padX(widthPx))
  const charsPerLine = Math.max(20, Math.floor(column / (type.sizePx * type.letterEm)))
  const lineHeightPx = Math.ceil(type.sizePx * type.leading)
  // At least the 16px a newsletter's paragraphs were always given.
  const gapPx = Math.max(16, type.sizePx)

  // Code keeps its own lines, so it is counted apart from the prose.
  let codeLines = 0
  const prose = body.replace(/<pre[\s\S]*?<\/pre>/gi, (block) => {
    codeLines += block.replace(/<[^>]*>/g, '').split('\n').length
    return '\n'
  })
  const textPx = prose
    .replace(/<(br|\/p|\/div|\/li|\/blockquote|\/h[1-6]|\/figcaption|\/tr)[ >/]/gi, (m) => `\n${m}`)
    .split('\n')
    .reduce((sum, segment) => {
      const text = segment
        .replace(/<[^>]*>/g, ' ')
        .replace(/\s+/g, ' ')
        .trim()
      if (!text) return sum
      // A heading is set larger than the text and a caption smaller (`STYLE`),
      // so fewer or more of its letters fit a line.
      const tag = /<(h[1-3]|figcaption)\b/i.exec(segment)?.[1]?.toLowerCase() ?? ''
      const scale = TEXT_SCALE[tag] ?? 1
      // A line ends at a word, so it holds a few letters fewer than would fit:
      // little in a wide column, a sixth of it in a phone's at a large size.
      const perLine = Math.max(8, charsPerLine / scale - 3)
      const lineHeight = Math.max(lineHeightPx, Math.ceil(type.sizePx * scale * 1.25))
      return sum + Math.max(1, Math.ceil(text.length / perLine)) * lineHeight
    }, 0)

  const pictures = (body.match(/<img\b[^>]*>/gi) ?? []).reduce((sum, tag) => {
    const width = Number(/\bwidth\s*=\s*["']?(\d+)/i.exec(tag)?.[1] ?? 0)
    const height = Number(/\bheight\s*=\s*["']?(\d+)/i.exec(tag)?.[1] ?? 0)
    if (width > 0 && height > 0) return sum + Math.round(height * Math.min(1, column / width))
    return sum + Math.round(Math.min(column, 640) * 0.6)
  }, 0)
  const headings = (body.match(/<h[1-6][\s>]/gi) ?? []).length
  const blocks = (body.match(/<(p|blockquote|li|figure|pre|h[1-6])[\s>]/gi) ?? []).length

  const estimate =
    textPx +
    codeLines * Math.ceil(type.sizePx * 1.35) +
    pictures +
    headings * Math.round(gapPx * 1.5) +
    blocks * gapPx +
    type.padY +
    2 * lineHeightPx
  return Math.min(60000, Math.max(160, estimate))
}

/** How much larger than the text a heading is set, or smaller a caption: `STYLE`'s `em`s. */
const TEXT_SCALE: Record<string, number> = { h1: 1.55, h2: 1.3, h3: 1.12, figcaption: 0.8 }

/** A newsletter's document: mail's, at 15px and 1.65, with 16px all round. */
const MAIL_TYPE: ArticleType = {
  sizePx: 15,
  leading: 1.65,
  letterEm: 0.533,
  padX: () => 16,
  padY: 32,
}
