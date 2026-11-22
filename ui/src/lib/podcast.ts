// Pure helpers for podcasts and the player: durations, policy words, and the
// play request for a story's episode or a file. Shared by the Feed's views,
// the player and Files. See `docs/plans/podcasts.md`.
//
// CONTRACT (laid by the supervisor; agent G2 implements the bodies, agent G1
// imports them): keep every exported name and signature. What follows the
// contract's own functions is the Feed's views' -- the same kind of rule, here
// because it needs no store and its failures are quiet: a duration that says
// "0:60", a download that reads as finished at 99 per cent, a form that saves
// a "use the default" as a choice. None of them throws and none fails to
// compile, so they are tested on their own in `scripts/podcast.test.mjs`.
//
// Types come in as `import type`, which is erased: the player store imports
// this file's neighbours at run time, and a value import back would be a cycle
// between modules that both construct things when they load.

import type { PlayRequest } from './player.svelte'
import type {
  AddedBy,
  Consumption,
  DownloadPolicy,
  DownloadStatus,
  Enclosure,
  EnclosureKind,
  FileEntry,
  KeepPolicy,
  PodcastSettings,
  QueueItem,
  Story,
  TranscribePolicy,
} from './types'

// ── Time ─────────────────────────────────────────────────────────────────

function pad(n: number): string {
  return String(n).padStart(2, '0')
}

/** `1:02:03` past an hour, `12:03` under one. */
export function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return '0:00'
  // Floored, never rounded: a clock does not say 1:00 at 59.6 seconds.
  const total = Math.floor(ms / 1000)
  const h = Math.floor(total / 3600)
  const m = Math.floor((total % 3600) / 60)
  const s = total % 60
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`
}

/**
 * A length in words: `12 min`, `1 h 5 min`, `2 h`.
 *
 * Rounded *up* to the minute, because it answers "how long until it is done":
 * thirty seconds left is "1 min", and "0 min left" would say something
 * finished that is not.
 */
export function formatSpan(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return '0 min'
  const minutes = Math.ceil(ms / 60_000)
  const h = Math.floor(minutes / 60)
  const m = minutes % 60
  if (h === 0) return `${m} min`
  return m === 0 ? `${h} h` : `${h} h ${m} min`
}

/** "12 min left", "1 h 5 min left", "Played". Empty when the length is not known. */
export function formatRemaining(positionMs: number, durationMs: number): string {
  if (!Number.isFinite(durationMs) || durationMs <= 0) return ''
  const position = Number.isFinite(positionMs) ? Math.max(0, positionMs) : 0
  const left = durationMs - position
  return left <= 0 ? 'Played' : `${formatSpan(left)} left`
}

// ── Policies, in words ───────────────────────────────────────────────────

/** "Off", "Newest episode", "Newest 3", "Every episode". */
export function downloadPolicyLabel(policy: DownloadPolicy): string {
  if (policy.type === 'off') return 'Off'
  if (policy.type === 'all') return 'Every episode'
  if (policy.type === 'newest') {
    return policy.count <= 1 ? 'Newest episode' : `Newest ${policy.count}`
  }
  return ''
}

/** "Keep forever", "7 days after played", "Keep newest 5". */
export function keepPolicyLabel(policy: KeepPolicy): string {
  if (policy.type === 'forever') return 'Keep forever'
  if (policy.type === 'afterPlayed') {
    return policy.days === 1 ? '1 day after played' : `${policy.days} days after played`
  }
  if (policy.type === 'newest') return `Keep newest ${policy.count}`
  return ''
}

/** "Only when asked", "When downloaded", "When played". */
export function transcribePolicyLabel(policy: TranscribePolicy): string {
  if (policy === 'off') return 'Only when asked'
  if (policy === 'whenDownloaded') return 'When downloaded'
  if (policy === 'whenPlayed') return 'When played'
  return ''
}

// ── What the player is asked to play ─────────────────────────────────────

/**
 * Audio or video, as the Rust side's `Enclosure::kind` decides it: by the type
 * the feed gave, else by the address's extension, else audio -- which is what
 * almost every podcast is. Kept the same on purpose: the service picked the
 * file's kind when it downloaded it, and the two must not disagree about what
 * to draw.
 */
export function enclosureKind(media: Pick<Enclosure, 'mime' | 'url'>): EnclosureKind {
  const mime = (media.mime ?? '').trim().toLowerCase()
  if (mime.startsWith('video/')) return 'video'
  if (mime.startsWith('audio/')) return 'audio'
  const path = (media.url ?? '').split(/[?#]/)[0]!.toLowerCase()
  return ['.mp4', '.m4v', '.mov', '.webm', '.mkv'].some((ext) => path.endsWith(ext))
    ? 'video'
    : 'audio'
}

/** What the player plays for a story's downloaded episode; `null` when it has none. */
export function playRequestForStory(
  story: Story,
  sourceTitle: string,
  artwork?: string | null,
  speed?: number | null,
): PlayRequest | null {
  const fileId = story.files?.media
  const media = story.media
  // Both, or neither: an enclosure with no file is an episode not yet
  // downloaded, and a file with no enclosure is not something this story says
  // how to play.
  if (!fileId || !media) return null
  return {
    what: { type: 'story', id: story.id },
    fileId,
    kind: enclosureKind(media),
    title: story.title.trim() || 'Untitled episode',
    source: sourceTitle,
    artwork: artwork || null,
    durationMs: media.durationMs ?? null,
    speed: speed ?? null,
  }
}

/**
 * {@link playRequestForStory} with its show's own say: the speed, and the
 * intro skipped when the episode has not been started.
 *
 * `started` is whether there is somewhere to resume from. The player resumes
 * by itself when no start is given, so a start is only ever offered when
 * there is nothing to resume -- an episode being returned to must not jump
 * past where it was left.
 */
export function playRequestForEpisode(
  story: Story,
  sourceTitle: string,
  podcast: Pick<PodcastSettings, 'speed' | 'skipIntroS'> | null | undefined,
  artwork: string | null | undefined,
  started: boolean,
): PlayRequest | null {
  const request = playRequestForStory(story, sourceTitle, artwork, podcast?.speed)
  if (!request) return null
  const skip = podcast?.skipIntroS
  if (!started && skip && skip > 0) request.startMs = Math.round(skip * 1000)
  return request
}

/** A file's name without its extension: `Interview.final.mp3` is `Interview.final`. */
function withoutExtension(name: string): string {
  const dot = name.lastIndexOf('.')
  // A leading dot is a hidden file's, not an extension's; a long tail is a
  // sentence that happened to end in a full stop.
  if (dot <= 0 || name.length - dot > 6) return name
  return name.slice(0, dot)
}

/** The name of the folder a file is in, or "Files" at the top of the folder and for a sealed file. */
function folderName(file: Pick<FileEntry, 'storage'>): string {
  const storage = file.storage
  if (!storage || storage.kind !== 'plain') return 'Files'
  const parts = storage.path.split('/')
  parts.pop()
  return parts[parts.length - 1] || 'Files'
}

/** What the player plays for an audio or video file; `null` for anything else. */
export function playRequestForFile(file: FileEntry): PlayRequest | null {
  if (file.kind !== 'audio' && file.kind !== 'video') return null
  // A file that is gone, or in the trash, has nothing to play.
  const state = file.status?.state
  if (state === 'missing' || state === 'trashed') return null
  return {
    what: { type: 'file', id: file.id },
    fileId: file.id,
    kind: file.kind,
    title: withoutExtension(file.name).trim() || file.name,
    source: folderName(file),
    durationMs: file.media?.durationMs ?? null,
  }
}

// ── Where an episode is up to ────────────────────────────────────────────

/** How far into something a person is, as a row draws it. */
export interface EpisodeProgress {
  /** 0 to 1: how much of the bar is filled. */
  fraction: number
  finished: boolean
  /** "12 min left", "Played", or a percentage for what has no clock. */
  label: string
}

/**
 * What a row says about how far somebody got, or `null` when they have not
 * started -- an unplayed episode has no bar, which is how it reads as new.
 *
 * Position over length where both are known, because that is the clock the
 * player kept; the record's own `progress` for what has none (a read
 * article).
 */
export function episodeProgress(
  consumption:
    Pick<Consumption, 'progress' | 'finished' | 'positionMs' | 'durationMs'> | null | undefined,
  durationMs?: number | null,
): EpisodeProgress | null {
  if (!consumption) return null
  if (consumption.finished) return { fraction: 1, finished: true, label: 'Played' }
  const length = consumption.durationMs ?? durationMs ?? 0
  const position = consumption.positionMs ?? 0
  if (position > 0 && length > 0) {
    return {
      fraction: Math.min(1, position / length),
      finished: false,
      label: formatRemaining(position, length),
    }
  }
  const progress = consumption.progress ?? 0
  if (progress > 0) {
    return {
      fraction: Math.min(1, progress),
      finished: false,
      label: `${Math.round(Math.min(1, progress) * 100)}%`,
    }
  }
  return null
}

/**
 * How much of a download has arrived, 0 to 99, or `null` when it cannot be
 * said (no size announced, or nothing started).
 *
 * Never 100: a download that has received every byte is still being written
 * to the files folder, and a ring that says "done" while the file is not
 * there yet is a play button that does nothing.
 */
export function downloadPercent(
  status: Pick<DownloadStatus, 'state' | 'received' | 'total'> | null | undefined,
): number | null {
  if (!status || status.state !== 'downloading') return null
  const total = status.total ?? 0
  if (total <= 0 || !Number.isFinite(total)) return null
  const pct = Math.floor((Math.max(0, status.received) / total) * 100)
  return Math.min(99, pct)
}

/** What a download says beside an episode: "Downloading 43%", "Waiting to download", "Download failed". */
export function downloadLine(
  status: Pick<DownloadStatus, 'state' | 'received' | 'total'> | null | undefined,
): string {
  if (!status) return ''
  if (status.state === 'waiting') return 'Waiting to download'
  if (status.state === 'failed') return 'Download failed'
  const pct = downloadPercent(status)
  return pct === null ? 'Downloading' : `Downloading ${pct}%`
}

// ── Up Next ──────────────────────────────────────────────────────────────

/** Where an Up Next entry came from, in words. */
export function addedByLabel(by: AddedBy): string {
  if (by === 'person') return 'Added by you'
  if (by === 'show') return 'New episode'
  if (by === 'recommended') return 'Recommended'
  if (by === 'daily') return 'From your daily podcast'
  if (by === 'assistant') return 'Added by the assistant'
  return ''
}

/**
 * How much of what is in Up Next is left to take in, in ms, counting only
 * what has a length: an article has none, and a guess would be worse than a
 * smaller total. Finished entries count nothing.
 */
export function queueRemainingMs(items: Pick<QueueItem, 'entry' | 'consumption'>[]): number {
  let total = 0
  for (const item of items) {
    const length = item.entry.durationMs ?? item.consumption?.durationMs ?? 0
    if (length <= 0) continue
    if (item.consumption?.finished) continue
    const position = item.consumption?.positionMs ?? 0
    total += Math.max(0, length - Math.max(0, position))
  }
  return total
}

/**
 * Which entry a dragged one should be put before, for the service's "move
 * just before `before`, or to the end".
 *
 * `over` is the entry the pointer is on and `after` whether it is on that
 * entry's lower half. Answers the id to move before, `null` for the end, or
 * `undefined` when the drop would leave things as they are -- dropped on
 * itself, or on the edge it already sits at -- so a slip of the hand is not
 * a write.
 */
export function dropBefore(
  ids: string[],
  dragged: string,
  over: string,
  after: boolean,
): string | null | undefined {
  const from = ids.indexOf(dragged)
  if (from < 0 || ids.indexOf(over) < 0 || dragged === over) return undefined
  const rest = ids.filter((id) => id !== dragged)
  const before = rest[rest.indexOf(over) + (after ? 1 : 0)] ?? null
  return before === (ids[from + 1] ?? null) ? undefined : before
}

/** A speed as a person writes it: `1×`, `1.5×`. */
export function speedLabel(speed: number): string {
  return `${Number(speed.toFixed(2))}×`
}

// ── The settings form ────────────────────────────────────────────────────
//
// A show's overrides are `null` for "follow the Feed's defaults", and a form
// has to be able to say that as a choice of its own. These turn one into the
// other and back, so "Use the Feed's default" saves as nothing at all -- which
// is what lets the show keep following the defaults if they ever change --
// instead of as a copy of today's.

export type DownloadChoice = 'default' | 'off' | 'newest' | 'all'
export type KeepChoice = 'default' | 'forever' | 'afterPlayed' | 'newest'
export type TranscribeChoice = 'default' | TranscribePolicy
export type QueueNewChoice = 'default' | 'yes' | 'no'

/** A count or a number of days from a number field: whole, at least 1, at most `max`; else `fallback`. */
export function clampCount(value: number | null | undefined, fallback: number, max = 999): number {
  if (value === null || value === undefined || !Number.isFinite(value)) return fallback
  return Math.min(max, Math.max(1, Math.round(value)))
}

/** Seconds of intro to skip: a whole number from 0 to ten minutes; anything else is none. */
export function clampSkip(value: number | null | undefined): number {
  if (value === null || value === undefined || !Number.isFinite(value)) return 0
  return Math.min(600, Math.max(0, Math.round(value)))
}

/** The number a policy carries -- its count or its days -- or `fallback` for one that has none. */
export function policyNumber(
  policy: DownloadPolicy | KeepPolicy | null | undefined,
  fallback: number,
): number {
  if (!policy) return fallback
  if (policy.type === 'newest') return policy.count
  if (policy.type === 'afterPlayed') return policy.days
  return fallback
}

/** What the form shows for each override. */
export interface PodcastForm {
  download: DownloadChoice
  /** How many, when `download` is `newest`. */
  downloadCount: number | null
  keep: KeepChoice
  /** Days for `afterPlayed`, a count for `newest`. */
  keepNumber: number | null
  queueNew: QueueNewChoice
  transcribe: TranscribeChoice
  /** `null` is the player's own speed. */
  speed: number | null
  skipIntroS: number | null
}

/** The form for a show's stored overrides. */
export function podcastForm(settings: PodcastSettings): PodcastForm {
  return {
    download: settings.download ? settings.download.type : 'default',
    downloadCount: policyNumber(settings.download, 3),
    keep: settings.keep ? settings.keep.type : 'default',
    keepNumber: policyNumber(settings.keep, settings.keep?.type === 'newest' ? 5 : 7),
    queueNew: settings.queueNew === true ? 'yes' : settings.queueNew === false ? 'no' : 'default',
    transcribe: settings.transcribe ?? 'default',
    speed: settings.speed ?? null,
    skipIntroS: settings.skipIntroS ?? 0,
  }
}

/** The overrides a form says, laid over `base` -- which keeps what the form has no field for. */
export function podcastFromForm(base: PodcastSettings, form: PodcastForm): PodcastSettings {
  let download: DownloadPolicy | null = null
  if (form.download === 'off') download = { type: 'off' }
  else if (form.download === 'all') download = { type: 'all' }
  else if (form.download === 'newest') {
    download = { type: 'newest', count: clampCount(form.downloadCount, 3) }
  }

  let keep: KeepPolicy | null = null
  if (form.keep === 'forever') keep = { type: 'forever' }
  else if (form.keep === 'afterPlayed') {
    keep = { type: 'afterPlayed', days: clampCount(form.keepNumber, 7, 365) }
  } else if (form.keep === 'newest') {
    keep = { type: 'newest', count: clampCount(form.keepNumber, 5) }
  }

  const skip = clampSkip(form.skipIntroS)
  return {
    ...base,
    download,
    keep,
    queueNew: form.queueNew === 'yes' ? true : form.queueNew === 'no' ? false : null,
    transcribe: form.transcribe === 'default' ? null : form.transcribe,
    speed: form.speed && form.speed > 0 ? form.speed : null,
    skipIntroS: skip > 0 ? skip : null,
  }
}
