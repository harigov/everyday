// What Settings → Backup says about a backup, as functions of a value.
//
// Apart from `backup.svelte.ts` for the reason `meetings-format.ts` is apart
// from its store: none of this needs the vault, a timer or Svelte's
// reactivity, and keeping it out of the store is what lets
// `scripts/backup.test.mjs` check it without booting any of them.
//
// Two things here are the Rust's rules said again, and both say where:
// `normalizeDestination` is `Destination::normalized`, so the pane can tell
// an edit from the same place typed differently, and `destinationUrl` is how
// `jatayu restore` names a destination, so the command the pane offers is
// one the command line takes.

import { dateFormat, humanBytes, plural, pluralWord, relativeTime } from './format'
import {
  BACKUP_FREQUENCIES,
  type BackupPhase,
  type BackupProgress,
  type BackupReport,
  type BackupSettings,
  type Destination,
} from './types'

export type DestinationKind = Destination['type']

/** The three-way choice under "Where", in the order it is drawn. */
export const DESTINATION_KINDS: readonly { value: DestinationKind; label: string }[] = [
  { value: 'folder', label: 'A folder' },
  { value: 's3', label: 'Amazon S3' },
  { value: 'gcs', label: 'Google Cloud Storage' },
]

/** What `BackupSettings.keep` starts at, and what a blank field means. */
export const DEFAULT_KEEP = 7

/** `MAX_KEEP` in `crates/jatayu-core/src/backup.rs`. */
export const MAX_KEEP = 100

/**
 * Where a restore lays the vault down, in the command the pane offers.
 *
 * A real folder rather than a `<new folder>` placeholder: pasted into a
 * shell, angle brackets are redirections, and a command that has to be
 * edited before it does anything is one somebody gets wrong on the one day
 * they need it. Unquoted, so the shell expands the `~`; no spaces, so it
 * needs no quotes.
 */
export const RESTORE_INTO = '~/jatayu-restored'

/** How often, as the select says it. */
export function frequencyLabel(hours: number): string {
  switch (hours) {
    case 1:
      return 'Every hour'
    case 24:
      return 'Every day'
    case 168:
      return 'Every week'
    default:
      return `Every ${hours} hours`
  }
}

/** The select's options: exactly the hours the vault accepts. */
export const FREQUENCY_CHOICES: readonly { value: number; label: string }[] =
  BACKUP_FREQUENCIES.map((hours) => ({ value: hours, label: frequencyLabel(hours) }))

/** What a running backup is doing, over its meter. */
export function phaseLabel(phase: BackupPhase): string {
  switch (phase) {
    case 'preparing':
      return 'Preparing'
    case 'listing':
      return 'Looking at what’s there'
    case 'copying':
      return 'Copying'
    case 'pruning':
      return 'Tidying up'
  }
}

/**
 * A fresh destination of `kind`, for the moment somebody picks it.
 *
 * A bucket and a folder typed for one provider come along to the other: a
 * person switching between S3 and Google Cloud Storage has usually named the
 * bucket already and only picked the wrong tab. A folder path means nothing
 * to either, and neither means anything to a folder.
 *
 * The folder inside a bucket starts as `jatayu` rather than empty, so a
 * bucket that holds other things too keeps them apart from these.
 */
export function blankDestination(
  kind: DestinationKind,
  from: Destination | null = null,
): Destination {
  const bucket = from && from.type !== 'folder' ? from.bucket : ''
  const prefix = from && from.type !== 'folder' ? from.prefix : 'jatayu'
  switch (kind) {
    case 'folder':
      return { type: 'folder', path: '' }
    case 's3':
      return {
        type: 's3',
        bucket,
        prefix,
        region: null,
        endpoint: null,
        profile: null,
        credentials: 'thisComputer',
      }
    case 'gcs':
      return { type: 'gcs', bucket, prefix, credentialsFile: null }
  }
}

/** Blank as `null`, anything else trimmed. */
function optional(value: string | null | undefined): string | null {
  const trimmed = (value ?? '').trim()
  return trimmed ? trimmed : null
}

/** A folder inside a bucket, without the slashes people type around it. */
function bucketFolder(prefix: string): string {
  return prefix.trim().replace(/^\/+|\/+$/g, '')
}

/**
 * The same place, spelled the same way however it was typed --
 * `Destination::normalized`. Every key is written, in one order, so two
 * normalized destinations compare equal as JSON exactly when they are the
 * same place.
 */
export function normalizeDestination(d: Destination): Destination {
  switch (d.type) {
    case 'folder':
      return { type: 'folder', path: d.path.trim() }
    case 's3':
      return {
        type: 's3',
        bucket: d.bucket.trim(),
        prefix: bucketFolder(d.prefix),
        region: optional(d.region),
        endpoint: optional(d.endpoint)?.replace(/\/+$/, '') ?? null,
        profile: optional(d.profile),
        credentials: d.credentials,
      }
    case 'gcs':
      return {
        type: 'gcs',
        bucket: d.bucket.trim(),
        prefix: bucketFolder(d.prefix),
        credentialsFile: optional(d.credentialsFile),
      }
  }
}

/** `keep` as the vault will take it: a whole number of copies, 1 to 100. */
export function clampKeep(keep: number | null | undefined): number {
  if (typeof keep !== 'number' || !Number.isFinite(keep)) return DEFAULT_KEEP
  return Math.min(MAX_KEEP, Math.max(1, Math.round(keep)))
}

/** Settings as they will be saved, so a draft and what was saved compare. */
export function normalizeSettings(s: BackupSettings): BackupSettings {
  return {
    enabled: s.enabled,
    destination: s.destination ? normalizeDestination(s.destination) : null,
    everyHours: s.everyHours,
    keep: clampKeep(s.keep),
    includeConfig: s.includeConfig,
  }
}

/** Whether saving `draft` would change anything about `saved`. */
export function sameSettings(draft: BackupSettings, saved: BackupSettings): boolean {
  return JSON.stringify(normalizeSettings(draft)) === JSON.stringify(normalizeSettings(saved))
}

/** Whether this destination signs with the access key pasted into Settings. */
export function usesStoredKey(d: Destination | null | undefined): boolean {
  return d?.type === 's3' && d.credentials === 'stored'
}

/**
 * How `jatayu restore` names a destination: the folder's path,
 * `s3://bucket/prefix` or `gs://bucket/prefix`.
 *
 * Not `Destination::label`, which adds an S3 endpoint in brackets for
 * people to read: the command line takes that as `--endpoint` instead.
 */
export function destinationUrl(d: Destination): string {
  const n = normalizeDestination(d)
  if (n.type === 'folder') return n.path
  const scheme = n.type === 's3' ? 's3' : 'gs'
  return n.prefix ? `${scheme}://${n.bucket}/${n.prefix}` : `${scheme}://${n.bucket}`
}

/**
 * One word for a POSIX shell: as it is when nothing in it is special,
 * single-quoted when something is. A path with a space in it -- "Backup
 * Disk" -- is the ordinary case, not the odd one.
 */
export function shellWord(word: string): string {
  if (/^[A-Za-z0-9_@%+=:,./-]+$/.test(word)) return word
  return `'${word.replace(/'/g, `'\\''`)}'`
}

/**
 * The command that restores from `d`: the newest copy unless `snapshot`
 * names an older one, with whatever else the command line needs to reach
 * the same place -- an S3 region, endpoint or profile, a Google key file.
 */
export function restoreCommand(
  d: Destination,
  opts: { snapshot?: string | null; config?: boolean; into?: string } = {},
): string {
  const n = normalizeDestination(d)
  const words = ['jatayu', 'restore', shellWord(destinationUrl(n)), opts.into ?? RESTORE_INTO]
  if (n.type === 's3') {
    if (n.region) words.push('--region', shellWord(n.region))
    if (n.endpoint) words.push('--endpoint', shellWord(n.endpoint))
    // A profile only means anything to credentials read from this computer.
    if (n.profile && n.credentials === 'thisComputer') words.push('--profile', shellWord(n.profile))
  }
  if (n.type === 'gcs' && n.credentialsFile) {
    words.push('--credentials-file', shellWord(n.credentialsFile))
  }
  if (opts.snapshot) words.push('--snapshot', shellWord(opts.snapshot))
  if (opts.config) words.push('--config')
  return words.join(' ')
}

/**
 * A message from the vault, as a sentence the pane can show.
 *
 * The Rust writes its errors the way errors are written there -- "choose a
 * folder for backups" -- and the pane shows them as prose. Capitalised only
 * when the first word is an ordinary lowercase word: a message that opens
 * with a bucket's URL or a quoted name must not have it changed.
 */
export function sentence(message: string): string {
  const trimmed = message.trim()
  if (!trimmed) return trimmed
  const first = trimmed.split(/\s/, 1)[0] ?? ''
  const opened = /^[a-z][a-z’']*[,;:]?$/.test(first)
    ? trimmed.charAt(0).toUpperCase() + trimmed.slice(1)
    : trimmed
  return /[.!?…]$/.test(opened) ? opened : `${opened}.`
}

/** How long a backup took, said the way people say it: `40 s`, `3 min`, `1 h 5 min`. */
export function durationLabel(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 1) return 'under a second'
  if (seconds < 59.5) return `${Math.round(seconds)} s`
  const minutes = Math.round(seconds / 60)
  if (minutes < 60) return `${minutes} min`
  const hours = Math.floor(minutes / 60)
  const rest = minutes % 60
  return rest === 0 ? `${hours} h` : `${hours} h ${rest} min`
}

/**
 * What the last backup sent, against everything the copy holds, and how
 * long it took: `1.2 GB sent of 24.0 GB · 3 min`. The two numbers side by
 * side are the point -- they are what shows copies sharing what did not
 * change.
 */
export function reportSummary(r: BackupReport): string {
  return `${humanBytes(r.uploadedBytes)} sent of ${humanBytes(r.bytes)} · ${durationLabel(r.seconds)}`
}

/** The rest of a report: how many files, and older copies let go of. */
export function reportDetail(r: BackupReport): string {
  const parts = [plural(r.files, 'file')]
  if (r.removedSnapshots > 0) {
    parts.push(`${plural(r.removedSnapshots, 'older copy', 'older copies')} let go`)
  }
  return parts.join(' · ')
}

/** The line under a running backup's meter: `120.0 MB sent · 812 of 4,812 files`. */
export function progressNote(p: BackupProgress): string {
  const sent = `${humanBytes(p.uploadedBytes)} sent`
  if (p.filesTotal <= 0) return sent
  return `${sent} · ${p.filesDone.toLocaleString()} of ${p.filesTotal.toLocaleString()} ${pluralWord(p.filesTotal, 'file')}`
}

/** How far through the bytes a running backup is; `0` before it knows. */
export function progressValue(p: BackupProgress): number {
  return p.bytesTotal > 0 ? p.bytesDone / p.bytesTotal : 0
}

/**
 * When the next backup is due, relative to now: `in 5 hours`, or `due now`
 * once that moment has passed -- which is also what a destination nothing
 * has been sent to yet answers, its due time being the start of 1970.
 */
export function nextDueLabel(nextDue: string, now: number = Date.now()): string {
  return new Date(nextDue).getTime() <= now ? 'due now' : relativeTime(nextDue)
}

/** A moment in full, in this computer's zone: a copy's row, a tooltip. */
export function whenLabel(iso: string): string {
  return dateFormat({
    weekday: 'short',
    day: 'numeric',
    month: 'short',
    year: 'numeric',
    hour: 'numeric',
    minute: '2-digit',
  }).format(new Date(iso))
}
