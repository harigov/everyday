// The daily briefs: what each section is called and what it is for, and the
// arithmetic the brief page draws with -- the day at a glance, the counts
// along the top, how long it takes to read and to hear.
//
// Pure, and apart from `brief.svelte.ts`, for the reason `karma.ts` is apart
// from its card: these are rules, and the way they fail is quiet. A timeline
// that puts two meetings in one lane draws one over the other and nothing
// throws. They are pinned in `scripts/brief.test.mjs`.
//
// The section titles mirror `jatayu_core::brief::BriefSection::title`,
// which is what the model is asked to write under: the page and the brief
// must call a section the same thing. See `docs/plans/briefs.md`.

import { formatMinutes, timeOfDay } from './format'
import type { IconName } from './icons'
import type {
  Brief,
  BriefDigest,
  BriefEvent,
  BriefKind,
  BriefLength,
  BriefSection,
  BriefSpec,
  NewsSource,
  NewsSourceId,
  PodcastBrief,
  Routine,
  RoutineRun,
  Script,
} from './types'
import type { PlayRequest } from './player.svelte'

// ── The catalogue ────────────────────────────────────────────────────────

export interface SectionInfo {
  /** The heading in the morning brief. */
  title: string
  /** The heading in the evening brief, where it differs. */
  evening?: string
  /** The heading in the weekly brief, where it differs. */
  weekly?: string
  /** What it covers, for the settings list. */
  blurb: string
  icon: IconName
  /** The briefs it belongs to, when not all of them: there is nothing to
   *  review in the morning, and only the weekly brief has a week ahead. */
  only?: BriefKind[]
}

export const SECTION_INFO: Record<BriefSection, SectionInfo> = {
  review: {
    title: 'How today went',
    weekly: 'Last week',
    blurb: 'What got done, what slipped, and what the meetings left you to do.',
    icon: 'check',
    only: ['evening', 'weekly'],
  },
  week: {
    title: 'The week ahead',
    blurb: 'The shape of the week day by day: the busy days, the meetings that matter, deadlines.',
    icon: 'week',
    only: ['weekly'],
  },
  schedule: {
    title: 'Your day',
    evening: 'Tomorrow',
    weekly: 'Today',
    blurb: 'Meetings and events in order, clashes, and the free time worth protecting.',
    icon: 'calendar',
  },
  people: {
    title: 'Who you’re meeting',
    evening: 'Who you’re seeing tomorrow',
    weekly: 'Who you’ll see this week',
    blurb: 'Who each person is, when you last met or wrote, and what was left open.',
    icon: 'people',
  },
  tasks: {
    title: 'Due today',
    evening: 'Due tomorrow',
    weekly: 'Due this week',
    blurb: 'What is due, most important first, and the overdue worth a decision.',
    icon: 'flag',
  },
  mail: {
    title: 'Waiting on you',
    blurb: 'Threads waiting on your reply, and replies worth a nudge.',
    icon: 'mail',
  },
  weather: {
    title: 'Weather',
    evening: 'Tomorrow’s weather',
    weekly: 'The week’s weather',
    blurb: 'What the forecast where you live means for the day.',
    icon: 'sun',
  },
  goals: {
    title: 'Goals',
    weekly: 'Goals this week',
    blurb: 'Where your targets stand, and a way to catch up on one that is behind.',
    icon: 'target',
  },
  tidbit: {
    title: 'Something to know',
    blurb: 'One short, true, interesting thing connected to one of your goals.',
    icon: 'sparkle',
  },
  occasions: {
    title: 'People to remember',
    blurb: 'Birthdays coming up, and anyone overdue a catch-up.',
    icon: 'heart-pulse',
  },
  reading: {
    title: 'Worth reading',
    blurb: 'Two or three unread stories from your Feed, and why each one.',
    icon: 'newspaper',
  },
}

/** The order a new brief covers its sections in -- `BriefSection::defaults`. */
export const DEFAULT_SECTIONS: Record<BriefKind, BriefSection[]> = {
  morning: [
    'schedule',
    'people',
    'tasks',
    'mail',
    'weather',
    'goals',
    'occasions',
    'tidbit',
    'reading',
  ],
  evening: ['review', 'schedule', 'people', 'tasks', 'mail', 'weather', 'goals'],
  weekly: [
    'review',
    'week',
    'schedule',
    'tasks',
    'people',
    'goals',
    'weather',
    'occasions',
    'mail',
    'tidbit',
  ],
  // The daily podcast is made from the Feed, not from the day's facts, so it
  // covers no section: what it covers is `PodcastBrief.sources`.
  podcast: [],
}

/** Every section a brief of `kind` may cover. */
export function sectionsFor(kind: BriefKind): BriefSection[] {
  if (kind === 'podcast') return []
  return (Object.keys(SECTION_INFO) as BriefSection[]).filter(
    (s) => SECTION_INFO[s].only?.includes(kind) ?? true,
  )
}

/** The heading a section goes under in a brief of `kind`. */
export function sectionTitle(section: BriefSection, kind: BriefKind): string {
  const info = SECTION_INFO[section]
  if (kind === 'evening' && info.evening) return info.evening
  if (kind === 'weekly' && info.weekly) return info.weekly
  return info.title
}

export interface KindInfo {
  name: string
  blurb: string
  /** When it runs until somebody says otherwise. */
  defaultTime: string
}

export const KIND_INFO: Record<BriefKind, KindInfo> = {
  morning: {
    name: 'Morning brief',
    blurb: 'The day ahead, first thing: what matters most, who you’re meeting, what is due.',
    defaultTime: '07:00',
  },
  evening: {
    name: 'Evening brief',
    blurb: 'A short close to the day, and what tomorrow needs from you.',
    defaultTime: '20:00',
  },
  weekly: {
    name: 'Weekly brief',
    blurb:
      'On Sunday morning: today, last week, and the week ahead. It takes the morning brief’s place on its day.',
    defaultTime: '08:00',
  },
  podcast: {
    name: 'Daily podcast',
    blurb:
      'Your own show, made each morning from the podcasts, newsletters and feeds you choose: something to listen to on a run or at the gym.',
    defaultTime: '06:00',
  },
}

export interface LengthInfo {
  label: string
  /** About how long it takes to hear. */
  minutes: number
}

/** `BriefLength::spoken_words` at 150 words a minute. */
export const LENGTH_INFO: Record<BriefLength, LengthInfo> = {
  short: { label: 'Short', minutes: 1.5 },
  standard: { label: 'Standard', minutes: 3.5 },
  long: { label: 'Long', minutes: 7 },
}

// ── Brief routines ───────────────────────────────────────────────────────

/** What a routine is set to, if it is a brief. */
export function briefSpec(routine: Pick<Routine, 'kind'>): BriefSpec | null {
  const kind = routine.kind
  if (kind?.type !== 'brief') return null
  return {
    brief: kind.brief,
    sections: kind.sections ?? [],
    length: kind.length ?? 'standard',
    // Only the daily podcast has one; the others are saved without the key.
    ...(kind.podcast ? { podcast: kind.podcast } : {}),
  }
}

export function isBriefRoutine(routine: Pick<Routine, 'kind'>): boolean {
  return routine.kind?.type === 'brief'
}

/** The routine behind one brief, if it has ever been switched on. */
export function briefRoutineOf<R extends Pick<Routine, 'kind'>>(
  routines: R[],
  kind: BriefKind,
): R | undefined {
  return routines.find((r) => r.kind?.type === 'brief' && r.kind.brief === kind)
}

/** A routine's kind with a changed spec -- what the settings save. */
export function withSpec(routine: Routine, spec: BriefSpec): Routine {
  return { ...routine, kind: { type: 'brief', ...spec } }
}

/** Move `section` one place up (-1) or down (+1) in `sections`. */
export function moveSection(
  sections: BriefSection[],
  section: BriefSection,
  by: -1 | 1,
): BriefSection[] {
  const at = sections.indexOf(section)
  const to = at + by
  if (at < 0 || to < 0 || to >= sections.length) return sections
  const next = [...sections]
  next.splice(at, 1)
  next.splice(to, 0, section)
  return next
}

/**
 * Switch `section` on or off. A section switched on goes where the default
 * order puts it among those already on, so turning the weather back on does
 * not drop it at the bottom.
 */
export function toggleSection(
  sections: BriefSection[],
  section: BriefSection,
  on: boolean,
  kind: BriefKind,
): BriefSection[] {
  if (!on) return sections.filter((s) => s !== section)
  if (sections.includes(section)) return sections
  const order = [...DEFAULT_SECTIONS[kind], ...sectionsFor(kind)]
  const rank = (s: BriefSection) => order.indexOf(s)
  const next = [...sections]
  const at = next.findIndex((s) => rank(s) > rank(section))
  next.splice(at < 0 ? next.length : at, 0, section)
  return next
}

// ── The daily podcast ────────────────────────────────────────────────────

/** How long the daily podcast may run, in minutes, and how long it does until somebody says. */
export const PODCAST_MINUTES = { least: 5, most: 60, usually: 15 } as const

/** `minutes` as the podcast may have it: whole, and no shorter or longer than it may run. */
export function clampMinutes(minutes: number): number {
  if (!Number.isFinite(minutes)) return PODCAST_MINUTES.usually
  return Math.min(PODCAST_MINUTES.most, Math.max(PODCAST_MINUTES.least, Math.round(minutes)))
}

/**
 * What a spec says the podcast is made from -- or what it is made from until
 * somebody says: every source, about fifteen minutes. The speakers are the
 * spec's own, whatever they are: there is one narrator today and the page
 * does not edit them, so they are carried through a save untouched.
 */
export function podcastOf(spec: Pick<BriefSpec, 'podcast'>): PodcastBrief {
  const p = spec.podcast
  return {
    sources: p?.sources ?? [],
    minutes: clampMinutes(p?.minutes ?? PODCAST_MINUTES.usually),
    speakers: p?.speakers ?? [],
  }
}

/** Choose `id` or let go of it. Each is kept once, in the order chosen. */
export function toggleSource(
  sources: NewsSourceId[],
  id: NewsSourceId,
  on: boolean,
): NewsSourceId[] {
  if (!on) return sources.filter((s) => s !== id)
  return sources.includes(id) ? sources : [...sources, id]
}

/** What a source is, in a word, for the list of what the podcast is made from. */
export function sourceKindLabel(source: Pick<NewsSource, 'kind' | 'podcast'>): string {
  if (source.podcast) return 'Podcast'
  switch (source.kind.type) {
    case 'mail':
      return 'Newsletter'
    case 'newsletters':
      return 'Every newsletter'
    case 'hackerNews':
      return 'Hacker News'
    default:
      return 'Feed'
  }
}

/** Where a kind of source goes in the list: podcasts first, then newsletters, then the rest. */
function sourceRank(source: Pick<NewsSource, 'kind' | 'podcast'>): number {
  if (source.podcast) return 0
  return source.kind.type === 'mail' || source.kind.type === 'newsletters' ? 1 : 2
}

/** A source as a list holds it: with what the sidebar draws beside it. */
interface Choosable {
  source: Pick<NewsSource, 'kind' | 'podcast' | 'title'>
}

/** The Feed's sources as the podcast's list offers them: podcasts first, each group by title. */
export function podcastChoices<T extends Choosable>(sources: T[]): T[] {
  return [...sources].sort(
    (a, b) =>
      sourceRank(a.source) - sourceRank(b.source) ||
      a.source.title.localeCompare(b.source.title, undefined, { sensitivity: 'base' }),
  )
}

/** The player's title for a podcast made on `day`: "Daily podcast — Saturday 21 November". */
export function podcastTitle(day: string): string {
  return `${KIND_INFO.podcast.name} — ${briefDay(day)}`
}

/** What the player plays for a podcast brief, or `null` while its audio is still being made. */
export function podcastPlayRequest(brief: Pick<Brief, 'audio' | 'day'>): PlayRequest | null {
  const audio = brief.audio
  if (!audio) return null
  return {
    what: { type: 'file', id: audio },
    fileId: audio,
    kind: 'audio',
    title: podcastTitle(brief.day),
    source: KIND_INFO.podcast.name,
  }
}

/** About how many minutes a script takes to say, at 150 words a minute. */
export function scriptMinutes(script: Pick<Script, 'lines'> | null | undefined): number {
  if (!script) return 0
  const n = script.lines.reduce((sum, l) => sum + words(l.text), 0)
  return n === 0 ? 0 : Math.max(1, Math.round(n / 150))
}

// ── Reading one ──────────────────────────────────────────────────────────

/** The brief a run kept, or `null`. */
export function briefOf(run: Pick<RoutineRun, 'brief'> | null | undefined): Brief | null {
  return run?.brief ?? null
}

/** "Good morning, Hari", by the hour it is. */
export function greeting(firstName: string, now = new Date()): string {
  const hour = now.getHours()
  const part = hour < 12 ? 'Good morning' : hour < 18 ? 'Good afternoon' : 'Good evening'
  const name = firstName.trim()
  return name ? `${part}, ${name}` : part
}

function words(text: string): number {
  return text.split(/\s+/).filter(Boolean).length
}

/** About how many minutes the written brief takes to read, at 230 a minute. */
export function readingMinutes(brief: Pick<Brief, 'headline' | 'parts'>): number {
  const n = words(brief.headline) + brief.parts.reduce((sum, p) => sum + words(p.body), 0)
  return Math.max(1, Math.round(n / 230))
}

/** About how many minutes the spoken brief takes to hear, at 150 a minute. */
export function listeningMinutes(brief: Pick<Brief, 'spoken'>): number {
  return Math.max(1, Math.round(words(brief.spoken) / 150))
}

/** "Saturday 21 November" for the brief's own day. */
export function briefDay(day: string): string {
  const [y, m, d] = day.split('-').map(Number)
  const at = new Date(y ?? 1970, (m ?? 1) - 1, d ?? 1)
  return at.toLocaleDateString(undefined, { weekday: 'long', day: 'numeric', month: 'long' })
}

// ── The day at a glance ──────────────────────────────────────────────────

export interface TimelineItem {
  kind: 'event' | 'block'
  title: string
  /** Milliseconds since the epoch. */
  start: number
  end: number
  /** The calendar's colour, for an event; empty for set-aside time. */
  color: string
  clash: boolean
  video: boolean
  busy: boolean
  /** Which row it is drawn on, from 0, so that overlapping items never sit
   *  on top of one another. */
  lane: number
  location: string
  guests: number
}

export interface TimelineGap {
  start: number
  end: number
  minutes: number
}

export interface Timeline {
  /** The window drawn, on whole hours. */
  from: number
  to: number
  /** One tick per hour from `from` to `to` inclusive. */
  hours: number[]
  items: TimelineItem[]
  /** All-day events, drawn above the timed ones. */
  allDay: BriefEvent[]
  free: TimelineGap[]
  lanes: number
  /** Now, if it falls inside the window. */
  now: number | null
}

const HOUR = 3_600_000

/**
 * The day at a glance: timed events and set-aside time on rows (lanes) that
 * never overlap, the free stretches, and the window around them -- eight in
 * the morning to eight at night, widened to whole hours around anything
 * earlier or later. `null` when the day has nothing timed and nothing free.
 */
export function timelineOf(digest: BriefDigest, now = Date.now()): Timeline | null {
  const timed = digest.events.filter((e) => !e.allDay)
  const raw: Omit<TimelineItem, 'lane'>[] = [
    ...timed.map((e) => ({
      kind: 'event' as const,
      title: e.title,
      start: Date.parse(e.start),
      end: Date.parse(e.end),
      color: e.color,
      clash: e.clash,
      video: e.video,
      busy: e.busy,
      location: e.location,
      guests: e.guests.length,
    })),
    ...digest.blocks.map((b) => ({
      kind: 'block' as const,
      title: b.title,
      start: Date.parse(b.start),
      end: Date.parse(b.end),
      color: '',
      clash: false,
      video: false,
      busy: true,
      location: '',
      guests: 0,
    })),
  ].filter((i) => Number.isFinite(i.start) && Number.isFinite(i.end) && i.end > i.start)
  const free = digest.free
    .map((f) => ({ start: Date.parse(f.start), end: Date.parse(f.end), minutes: f.minutes }))
    .filter((f) => Number.isFinite(f.start) && Number.isFinite(f.end))
  if (raw.length === 0 && free.length === 0) return null

  // The day the brief is about, eight to eight, in this window's zone.
  const day = digest.ahead ?? digest.day
  const base = day ? new Date(`${day}T00:00:00`) : new Date(raw[0]?.start ?? free[0]!.start)
  base.setHours(0, 0, 0, 0)
  let from = base.getTime() + 8 * HOUR
  let to = base.getTime() + 20 * HOUR
  for (const i of [...raw, ...free]) {
    from = Math.min(from, Math.floor(i.start / HOUR) * HOUR)
    to = Math.max(to, Math.ceil(i.end / HOUR) * HOUR)
  }
  // Not wider than a day, whatever a stray event says.
  to = Math.min(to, from + 24 * HOUR)

  raw.sort((a, b) => a.start - b.start || b.end - a.end)
  const laneEnds: number[] = []
  const items: TimelineItem[] = raw.map((i) => {
    let lane = laneEnds.findIndex((end) => end <= i.start)
    if (lane < 0) {
      lane = laneEnds.length
      laneEnds.push(i.end)
    } else {
      laneEnds[lane] = i.end
    }
    return { ...i, lane }
  })
  const hours: number[] = []
  for (let t = from; t <= to; t += HOUR) hours.push(t)
  return {
    from,
    to,
    hours,
    items,
    allDay: digest.events.filter((e) => e.allDay),
    free,
    lanes: Math.max(1, laneEnds.length),
    now: now >= from && now <= to ? now : null,
  }
}

/** Where `t` falls across `timeline`, as a percentage of its width. */
export function at(timeline: Pick<Timeline, 'from' | 'to'>, t: number): number {
  const span = timeline.to - timeline.from
  if (span <= 0) return 0
  return Math.min(100, Math.max(0, ((t - timeline.from) / span) * 100))
}

// ── The counts along the top ─────────────────────────────────────────────

export interface BriefStat {
  id: 'meetings' | 'free' | 'tasks' | 'mail' | 'weather'
  /** The figure: "3", "4h 15m", "9°". */
  value: string
  /** What it counts: "meetings · 2h 15m". */
  label: string
  /** Something to watch: an overdue count, a clash. */
  warn?: boolean
}

/** Timed, busy events, and the minutes they take. `BriefDigest::meeting_load`. */
export function meetingLoad(digest: Pick<BriefDigest, 'events'>): {
  meetings: number
  minutes: number
} {
  const timed = digest.events.filter((e) => !e.allDay && e.busy)
  const minutes = timed.reduce(
    (sum, e) => sum + Math.max(0, (Date.parse(e.end) - Date.parse(e.start)) / 60_000),
    0,
  )
  return { meetings: timed.length, minutes: Math.round(minutes) }
}

/**
 * The handful of numbers worth seeing before reading a word: meetings and
 * how long they take, the longest free stretch, what is due, what mail is
 * waiting, and the weather. Only those the brief covers and has something
 * to say about.
 */
export function statsOf(digest: BriefDigest): BriefStat[] {
  const covers = (s: BriefSection) => digest.sections.includes(s)
  const out: BriefStat[] = []
  if (covers('schedule')) {
    const { meetings, minutes } = meetingLoad(digest)
    const clash = digest.events.some((e) => e.clash)
    out.push({
      id: 'meetings',
      value: String(meetings),
      label:
        meetings === 0
          ? 'meetings'
          : `${meetings === 1 ? 'meeting' : 'meetings'} · ${formatMinutes(minutes)}`,
      warn: clash,
    })
    const longest = [...digest.free].sort((a, b) => b.minutes - a.minutes)[0]
    if (longest) {
      out.push({
        id: 'free',
        value: formatMinutes(longest.minutes),
        label: `free from ${timeOfDay(new Date(longest.start))}`,
      })
    }
  }
  if (covers('tasks')) {
    const due = digest.due.length
    out.push({
      id: 'tasks',
      value: String(due),
      label: digest.overdueCount > 0 ? `due · ${digest.overdueCount} overdue` : 'due',
      warn: digest.overdueCount > 0,
    })
  }
  if (covers('mail')) {
    const waiting = digest.mail.filter((t) => t.waiting === 'onYou').length
    if (waiting > 0 || digest.unreadImportant > 0) {
      out.push({
        id: 'mail',
        value: String(waiting || digest.unreadImportant),
        label: waiting ? (waiting === 1 ? 'reply owed' : 'replies owed') : 'important unread',
      })
    }
  }
  const w = digest.weather
  if (covers('weather') && w) {
    const hi = w.high != null ? `${Math.round(w.high)}°` : ''
    const lo = w.low != null ? `${Math.round(w.low)}°` : ''
    const rain = w.rainChance != null && w.rainChance >= 30 ? ` · rain ${w.rainChance}%` : ''
    out.push({
      id: 'weather',
      value: hi || (w.now != null ? `${Math.round(w.now)}°` : '–'),
      label: `${lo ? `low ${lo} · ` : ''}${w.condition.toLowerCase()}${rain}`,
    })
  }
  return out
}

// ── Goals ────────────────────────────────────────────────────────────────

/**
 * How far along a target is, 0 to 1, for a bar: the value against its aim.
 * For a limit it is how much of the allowance is used.
 */
export function goalFraction(goal: { value: number; aim?: number | null }): number {
  if (!goal.aim || goal.aim <= 0) return goal.value > 0 ? 1 : 0
  return Math.min(1, Math.max(0, goal.value / goal.aim))
}

/** Where an even pace would have it by now, 0 to 1, or `null`. */
export function paceFraction(goal: { aim?: number | null; onPace?: number | null }): number | null {
  if (goal.onPace == null || !goal.aim || goal.aim <= 0) return null
  return Math.min(1, Math.max(0, goal.onPace / goal.aim))
}

// ── Which section draws what ─────────────────────────────────────────────

/** Whether a section has a picture to draw beside its words. */
export function hasFigure(section: BriefSection | null | undefined, digest: BriefDigest): boolean {
  switch (section) {
    case 'schedule':
      return timelineOf(digest) !== null || digest.events.some((e) => e.allDay)
    case 'people':
      return digest.people.length > 0
    case 'tasks':
      return digest.due.length > 0 || digest.overdueCount > 0
    case 'mail':
      return digest.mail.length > 0
    case 'weather':
      return digest.weather != null
    case 'goals':
      return digest.goals.length > 0
    case 'occasions':
      return digest.occasions.length > 0
    case 'reading':
      return digest.stories.length > 0
    case 'review':
      return digest.done.length > 0 || digest.karmaToday != null
    case 'week':
      return digest.week.length > 0
    default:
      return false
  }
}

// ── The weather's picture ────────────────────────────────────────────────

/** Which weather icon a forecast's condition reads as, in words. */
export function weatherIcon(condition: string): IconName {
  const c = condition.toLowerCase()
  if (/thunder|storm|lightning/.test(c)) return 'storm'
  if (/snow|sleet|hail|flurr|blizzard|\bice\b|\bicy\b/.test(c)) return 'snow'
  if (/rain|drizzle|shower/.test(c)) return 'rain'
  if (/fog|mist|haze|smoke/.test(c)) return 'fog'
  if (/wind|gale|breez/.test(c)) return 'wind'
  if (/cloud|overcast|grey|gray/.test(c)) return 'cloud'
  return 'sun'
}
