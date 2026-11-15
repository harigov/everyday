// Backups for the mock backend: Settings → Backup without a disk or a bucket,
// and the one function `mock.ts` hands every backup command to.
//
// The vault starts out backing up to a folder on an external disk once a
// day, keeping seven copies, the last one made a few hours ago -- so the
// pane opens on the state most people will see most days. `?backup=` in the
// address previews the others:
//
//   ?backup=none     nowhere chosen yet
//   ?backup=failing  the disk has been unplugged for two days
//   ?backup=server   a vault whose database is on a server
//   ?backup=plain    a vault with no password
//
// # A run is worked out from the clock
//
// "Back up now" notes when it started, and every `backup_settings` after
// that works out how far along it would be by now: a second or so preparing,
// a little longer looking at what is there, most of it copying, a moment
// tidying up. The first read after its time is up finishes it -- records the
// report, adds the copy, lets go of the oldest beyond the number kept. No
// timer is set, here or at load, so this file cannot hold a test process
// open, and a pane that stops asking stops the backup costing anything.
//
// # Secrets
//
// `set_backup_keys` checks both halves are there and keeps neither; only
// `hasKeys` remembers that it happened. Nothing here can read a key back,
// which is the promise the real service makes too.

import { BACKUP_FREQUENCIES, VaultError } from './types'
import type {
  BackupProgress,
  BackupReport,
  BackupSettings,
  BackupStatus,
  BackupView,
  Destination,
  DestinationCheck,
  SnapshotInfo,
} from './types'

/** What the folder picker answers: an external disk, as Linux mounts one. */
const PICKED_FOLDER = '/media/you/Backup Disk/Karma Yoga'

const CONFIG_DIR = '/home/you/.config/karmayoga'

const HOUR = 3_600_000
const DAY = 24 * HOUR
/** `RETRY_AFTER`: how long after a failure the next try waits. */
const RETRY_AFTER = 30 * 60_000

/** The pretend vault: 24.3 GB in 4,812 files, a little of it new each day. */
const VAULT_BYTES = 26_092_000_000
const VAULT_FILES = 4_812
const NEW_EACH_RUN = 186_000_000

/** When each phase of a run ends, in milliseconds from its start. */
const PREPARED = 1_500
const LISTED = 3_000
const COPIED = 10_000
const FINISHED = 11_500

type Running = { started: number; manual: boolean }

interface MockBackups {
  settings: BackupSettings
  status: BackupStatus
  /** The copies kept at the destination, newest first. */
  copies: SnapshotInfo[]
  running: Running | null
  hasKeys: boolean
  supported: boolean
  encrypted: boolean
}

let state: MockBackups | null = null

/** Built on first use rather than at load, so `location` is only read in a
 *  browser that has one. */
function current(): MockBackups {
  state ??= seed()
  return state
}

function seed(): MockBackups {
  const variant = new URLSearchParams(globalThis.location?.search ?? '').get('backup')
  const now = Date.now()
  const disk: Destination = { type: 'folder', path: PICKED_FOLDER }
  const base: MockBackups = {
    settings: { enabled: true, destination: disk, everyHours: 24, keep: 7, includeConfig: true },
    status: emptyStatus(disk),
    copies: [],
    running: null,
    hasKeys: false,
    supported: variant !== 'server',
    encrypted: variant !== 'plain',
  }
  if (variant === 'none' || variant === 'server') {
    base.settings = { ...base.settings, enabled: false, destination: null }
    base.status = emptyStatus(null)
    return base
  }
  // The last copy that worked: a few hours ago normally, three days ago for
  // the disk that has since been unplugged. One a day before it, seven in all.
  const last = variant === 'failing' ? now - 3 * DAY : now - 5 * HOUR - 12 * 60_000
  base.copies = Array.from({ length: 7 }, (_, i) => snapshotAt(last - i * DAY))
  base.status = {
    ...base.status,
    lastAttempt: iso(last - 3 * 60_000),
    lastSuccess: iso(last),
    lastReport: report(base.copies[0]!.name, 172, 1),
  }
  if (variant === 'failing') {
    base.status = {
      ...base.status,
      lastAttempt: iso(now - 40 * 60_000),
      lastError: `${PICKED_FOLDER} is not there -- if it is on a disk, is the disk plugged in?`,
      failingSince: iso(now - 2 * DAY),
    }
  }
  return base
}

function emptyStatus(d: Destination | null): BackupStatus {
  return {
    destination: d ? label(d) : null,
    lastAttempt: null,
    lastSuccess: null,
    lastError: null,
    failingSince: null,
    lastReport: null,
  }
}

function report(snapshot: string, seconds: number, removed: number): BackupReport {
  return {
    snapshot,
    files: VAULT_FILES,
    bytes: VAULT_BYTES,
    uploadedBytes: NEW_EACH_RUN,
    uploadedChunks: 41,
    removedSnapshots: removed,
    removedChunks: removed * 37,
    seconds,
    skipped: [],
  }
}

function iso(ms: number): string {
  return new Date(ms).toISOString()
}

/** `snapshot_name`: `2026-11-14T030512Z`, in UTC, with no colons. */
function snapshotAt(ms: number): SnapshotInfo {
  const at = new Date(ms)
  const name = at
    .toISOString()
    .replace(/\.\d+Z$/, 'Z')
    .replace(/:/g, '')
  return { name, created: at.toISOString() }
}

/** `Destination::label`. */
function label(d: Destination): string {
  if (d.type === 'folder') return d.path
  const scheme = d.type === 's3' ? 's3' : 'gs'
  const url = d.prefix ? `${scheme}://${d.bucket}/${d.prefix}` : `${scheme}://${d.bucket}`
  return d.type === 's3' && d.endpoint ? `${url} (${d.endpoint})` : url
}

/** `Destination::normalized`. */
function normalized(d: Destination): Destination {
  const opt = (v: string | null | undefined) => (v ?? '').trim() || null
  const prefix = (p: string) => (p ?? '').trim().replace(/^\/+|\/+$/g, '')
  switch (d.type) {
    case 'folder':
      return { type: 'folder', path: (d.path ?? '').trim() }
    case 's3':
      return {
        type: 's3',
        bucket: (d.bucket ?? '').trim(),
        prefix: prefix(d.prefix),
        region: opt(d.region),
        endpoint: opt(d.endpoint)?.replace(/\/+$/, '') ?? null,
        profile: opt(d.profile),
        credentials: d.credentials === 'stored' ? 'stored' : 'thisComputer',
      }
    case 'gcs':
      return {
        type: 'gcs',
        bucket: (d.bucket ?? '').trim(),
        prefix: prefix(d.prefix),
        credentialsFile: opt(d.credentialsFile),
      }
  }
}

/** `Destination::validate`, word for word where the words are seen. */
function destinationProblem(d: Destination): string | null {
  if (d.type === 'folder') {
    if (!d.path) return 'choose a folder for backups'
    if (!d.path.startsWith('/') && !/^[A-Za-z]:[\\/]/.test(d.path)) {
      return `${d.path} is not a full path; choose the folder again`
    }
    return null
  }
  const b = d.bucket
  if (!(b.length >= 3 && b.length <= 222 && /^[a-z0-9][a-z0-9._-]*$/.test(b))) {
    return `${JSON.stringify(b)} is not a bucket name: lowercase letters, digits, dots and dashes`
  }
  if (d.prefix.split('/').some((p) => p === '.' || p === '..') || d.prefix.includes('//')) {
    return `${JSON.stringify(d.prefix)} is not a folder name inside a bucket`
  }
  if (d.type === 's3' && d.endpoint) {
    const e = d.endpoint
    if (!e.startsWith('https://') && !e.startsWith('http://')) {
      return 'the endpoint must start with https:// or http://'
    }
    if (e.startsWith('http://') && !/^http:\/\/(localhost|127\.0\.0\.1|\[::1\])(:|\/|$)/.test(e)) {
      return 'an endpoint off this computer must be https:// -- the requests carry your access key’s signature'
    }
    if (/\s/.test(e)) return 'the endpoint cannot contain spaces'
  }
  return null
}

/** `BackupSettings::validate`. */
function settingsProblem(s: BackupSettings): string | null {
  const every: readonly number[] = BACKUP_FREQUENCIES
  if (!every.includes(s.everyHours)) {
    return `a backup can be made every ${every.join(', ')} hours; ${s.everyHours} is not one of them`
  }
  if (!Number.isInteger(s.keep) || s.keep < 1 || s.keep > 100)
    return 'keep between 1 and 100 copies'
  if (s.destination) {
    const problem = destinationProblem(s.destination)
    if (problem) return problem
  }
  if (s.enabled && !s.destination) return 'choose where backups go before turning them on'
  return null
}

/** Whether a stored-key S3 destination has a key to sign with. */
function keyMissing(m: MockBackups, d: Destination): boolean {
  return d.type === 's3' && d.credentials === 'stored' && !m.hasKeys
}

/**
 * Where the run in progress would be by now, finishing it if its time is
 * up. Called by every read, which is what moves it along.
 */
function advance(m: MockBackups, now = Date.now()): BackupProgress | null {
  const run = m.running
  if (!run) return null
  const elapsed = now - run.started
  if (elapsed >= FINISHED) {
    finish(m, run, now)
    return null
  }
  const copying = Math.min(1, Math.max(0, (elapsed - LISTED) / (COPIED - LISTED)))
  const known = elapsed >= PREPARED
  return {
    phase:
      elapsed < PREPARED
        ? 'preparing'
        : elapsed < LISTED
          ? 'listing'
          : elapsed < COPIED
            ? 'copying'
            : 'pruning',
    manual: run.manual,
    filesDone: Math.round(VAULT_FILES * copying),
    filesTotal: known ? VAULT_FILES : 0,
    bytesDone: Math.round(VAULT_BYTES * copying),
    bytesTotal: known ? VAULT_BYTES : 0,
    // What was new is spread through the copy rather than all at the start.
    uploadedBytes: Math.round(NEW_EACH_RUN * copying),
    uploadedChunks: Math.round(41 * copying),
    started: iso(run.started),
  }
}

function finish(m: MockBackups, run: Running, now: number) {
  m.running = null
  const destination = m.settings.destination
  if (!destination) return
  m.status = { ...m.status, lastAttempt: iso(run.started) }
  if (keyMissing(m, destination)) {
    m.status = {
      ...m.status,
      lastError: 'no access key is stored for this bucket; paste one in under Settings → Backup',
      failingSince: m.status.failingSince ?? iso(run.started),
    }
    return
  }
  const made = snapshotAt(run.started)
  const copies = [made, ...m.copies.filter((c) => c.name !== made.name)]
  const removed = Math.max(0, copies.length - m.settings.keep)
  m.copies = copies.slice(0, m.settings.keep)
  m.status = {
    ...m.status,
    lastSuccess: iso(now),
    lastError: null,
    failingSince: null,
    lastReport: report(made.name, (now - run.started) / 1000, removed),
  }
}

/** `backup::next_due`: the last success plus the interval, or now. */
function nextDue(m: MockBackups): string | null {
  if (!m.settings.enabled || !m.settings.destination) return null
  const last = m.status.lastSuccess
  if (!last) return iso(0)
  const scheduled = new Date(last).getTime() + m.settings.everyHours * HOUR
  if (m.status.lastError && m.status.lastAttempt) {
    const retry =
      new Date(m.status.lastAttempt).getTime() + Math.min(RETRY_AFTER, m.settings.everyHours * HOUR)
    return iso(Math.max(scheduled, retry))
  }
  return iso(scheduled)
}

function view(m: MockBackups): BackupView {
  const running = advance(m)
  return structuredClone({
    settings: m.settings,
    status: m.status,
    running,
    nextDue: nextDue(m),
    hasKeys: m.hasKeys,
    supported: m.supported,
    encrypted: m.encrypted,
    configDir: CONFIG_DIR,
  })
}

function check(m: MockBackups, raw: Destination): DestinationCheck {
  const d = normalized(raw)
  const problem = destinationProblem(d)
  if (problem) return { ok: false, message: problem }
  if (keyMissing(m, d)) {
    return {
      ok: false,
      message: 'no access key is stored yet; paste one in and save before trying it',
    }
  }
  // Somewhere that plainly is not there, for the failure a folder most often has.
  if (d.type === 'folder' && /unplugged|missing/i.test(d.path)) {
    return {
      ok: false,
      message: `${d.path} is not there -- if it is on a disk, is the disk plugged in?`,
    }
  }
  if (d.type !== 'folder' && d.bucket.includes('someone-else')) {
    return {
      ok: false,
      message: `${label(d)} holds another vault's backups; choose somewhere else`,
    }
  }
  const saved = m.settings.destination
  const here = saved && label(normalized(saved)) === label(d) ? m.copies.length : 0
  if (here === 0) return { ok: true, message: `Works. Nothing is kept at ${label(d)} yet.` }
  return {
    ok: true,
    message:
      here === 1
        ? 'Works. One backup of this vault is kept there already.'
        : `Works. ${here} backups of this vault are kept there already.`,
  }
}

function str(v: unknown): string {
  return typeof v === 'string' ? v : ''
}

export function mockBackup(cmd: string, args: Record<string, unknown>): unknown {
  // The shell's, and answered whether or not a vault is open.
  if (cmd === 'pick_folder') return PICKED_FOLDER

  const m = current()
  switch (cmd) {
    case 'backup_settings':
      return view(m)

    case 'save_backup_settings': {
      const input = args.settings as BackupSettings
      const settings: BackupSettings = {
        enabled: input.enabled === true,
        destination: input.destination ? normalized(input.destination) : null,
        everyHours: Number(input.everyHours),
        keep: Number(input.keep),
        includeConfig: input.includeConfig !== false,
      }
      const problem = settingsProblem(settings)
      if (problem) throw new VaultError('invalid', problem)
      // A status about somewhere else is not this destination's: a new
      // place starts with nothing sent and a backup due at once.
      const was = m.status.destination
      const next = settings.destination ? label(settings.destination) : null
      if (was !== next) {
        m.status = emptyStatus(settings.destination)
        m.copies = []
      }
      m.settings = settings
      return view(m)
    }

    case 'set_backup_keys':
      if (!str(args.accessKeyId).trim() || !str(args.secretAccessKey).trim()) {
        throw new VaultError('invalid', 'an access key needs both its id and its secret')
      }
      m.hasKeys = true
      return view(m)

    case 'clear_backup_keys':
      m.hasKeys = false
      return view(m)

    case 'run_backup':
      advance(m)
      if (m.running) throw new VaultError('busy', 'a backup is already being made')
      if (!m.settings.destination) {
        throw new VaultError('busy', 'choose where backups go first')
      }
      if (!m.supported) {
        throw new VaultError(
          'busy',
          'this vault’s database is on a server, which is backed up by whoever runs it',
        )
      }
      m.running = { started: Date.now(), manual: true }
      return view(m)

    case 'cancel_backup':
      m.running = null
      return view(m)

    case 'backup_snapshots': {
      advance(m)
      const d = m.settings.destination
      if (!d) return []
      if (keyMissing(m, d)) {
        throw new VaultError('forbidden', 'no access key is stored for this bucket')
      }
      if (m.status.lastError && d.type === 'folder') {
        throw new VaultError('unreachable', m.status.lastError)
      }
      return structuredClone(m.copies)
    }

    case 'check_backup_destination':
      return check(m, args.destination as Destination)

    default:
      throw new VaultError('unknown', `no mock for command ${cmd}`)
  }
}
