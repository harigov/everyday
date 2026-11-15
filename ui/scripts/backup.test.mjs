// Behaviour checks for what Settings → Backup says and sends: the labels,
// telling an edit from the same place typed differently, the restore command
// the pane offers, and the vault's messages turned into sentences.

import assert from 'node:assert/strict'
import { load, stubBrowser } from './harness.mjs'

// `plural` reads `navigator.language`; a Node without one gets the stub.
stubBrowser({ navigator: true })

const { module: b, close } = await load('/src/lib/backup.ts')

// ── labels ────────────────────────────────────────────────────────────

assert.deepEqual(
  b.FREQUENCY_CHOICES.map((f) => f.label),
  ['Every hour', 'Every 6 hours', 'Every day', 'Every week'],
  'exactly the four the vault accepts, in order',
)
assert.deepEqual(
  b.FREQUENCY_CHOICES.map((f) => f.value),
  [1, 6, 24, 168],
)
assert.equal(b.frequencyLabel(12), 'Every 12 hours', 'an hour count it does not name still reads')

assert.equal(b.phaseLabel('preparing'), 'Preparing')
assert.equal(b.phaseLabel('listing'), 'Looking at what’s there')
assert.equal(b.phaseLabel('copying'), 'Copying')
assert.equal(b.phaseLabel('pruning'), 'Tidying up')

assert.deepEqual(
  b.DESTINATION_KINDS.map((k) => k.label),
  ['A folder', 'Amazon S3', 'Google Cloud Storage'],
)

// ── fresh destinations ───────────────────────────────────────────────

assert.deepEqual(b.blankDestination('folder'), { type: 'folder', path: '' })
assert.deepEqual(b.blankDestination('s3'), {
  type: 's3',
  bucket: '',
  prefix: 'karmayoga',
  region: null,
  endpoint: null,
  profile: null,
  credentials: 'thisComputer',
})
assert.deepEqual(
  b.blankDestination('gcs', {
    type: 's3',
    bucket: 'my-backups',
    prefix: 'hari',
    credentials: 'stored',
  }),
  { type: 'gcs', bucket: 'my-backups', prefix: 'hari', credentialsFile: null },
  'a bucket typed under the wrong provider comes along',
)
assert.equal(
  b.blankDestination('s3', { type: 'folder', path: '/media/disk' }).bucket,
  '',
  'a folder path means nothing to a bucket',
)

// ── normalising, and what counts as a change ─────────────────────────

assert.deepEqual(
  b.normalizeDestination({
    type: 's3',
    bucket: ' my-backups ',
    prefix: '/karmayoga/hari/ ',
    region: '  ',
    endpoint: 'https://acct.r2.cloudflarestorage.com/',
    credentials: 'thisComputer',
  }),
  {
    type: 's3',
    bucket: 'my-backups',
    prefix: 'karmayoga/hari',
    region: null,
    endpoint: 'https://acct.r2.cloudflarestorage.com',
    profile: null,
    credentials: 'thisComputer',
  },
  'Destination::normalized, as the Rust spells it',
)
assert.deepEqual(b.normalizeDestination({ type: 'folder', path: ' /mnt/backups ' }), {
  type: 'folder',
  path: '/mnt/backups',
})

const saved = {
  enabled: true,
  destination: {
    type: 's3',
    bucket: 'my-backups',
    prefix: 'karmayoga',
    region: null,
    endpoint: null,
    profile: null,
    credentials: 'thisComputer',
  },
  everyHours: 24,
  keep: 7,
  includeConfig: true,
}
assert.ok(
  b.sameSettings(
    {
      ...saved,
      destination: { ...saved.destination, prefix: 'karmayoga/', region: '', profile: undefined },
    },
    saved,
  ),
  'the same place typed differently is not an edit',
)
assert.ok(!b.sameSettings({ ...saved, keep: 8 }, saved))
assert.ok(
  !b.sameSettings({ ...saved, destination: { ...saved.destination, bucket: 'other' } }, saved),
)
assert.ok(!b.sameSettings({ ...saved, destination: null, enabled: false }, saved))

assert.equal(b.clampKeep(0), 1)
assert.equal(b.clampKeep(250), 100)
assert.equal(b.clampKeep(6.6), 7)
assert.equal(b.clampKeep(null), 7, 'a cleared field is the default, not zero')
assert.equal(b.clampKeep(Number.NaN), 7)

assert.ok(b.usesStoredKey({ ...saved.destination, credentials: 'stored' }))
assert.ok(!b.usesStoredKey(saved.destination))
assert.ok(!b.usesStoredKey({ type: 'folder', path: '/x' }))
assert.ok(!b.usesStoredKey(null))

// ── the restore command ──────────────────────────────────────────────

assert.equal(b.destinationUrl({ type: 'folder', path: '/mnt/backups' }), '/mnt/backups')
assert.equal(b.destinationUrl(saved.destination), 's3://my-backups/karmayoga')
assert.equal(
  b.destinationUrl({ type: 'gcs', bucket: 'b', prefix: '', credentialsFile: null }),
  'gs://b',
  'no trailing slash for the bucket itself',
)
assert.equal(
  b.destinationUrl({ ...saved.destination, endpoint: 'https://minio.local:9000' }),
  's3://my-backups/karmayoga',
  'not the label: the endpoint is a flag of its own on the command line',
)

assert.equal(b.shellWord('/mnt/backups'), '/mnt/backups')
assert.equal(b.shellWord('/media/you/Backup Disk'), "'/media/you/Backup Disk'")
assert.equal(b.shellWord("it's"), "'it'\\''s'")

assert.equal(
  b.restoreCommand({ type: 'folder', path: '/media/you/Backup Disk/Karma Yoga' }),
  "karmayoga restore '/media/you/Backup Disk/Karma Yoga' ~/karmayoga-restored",
)
assert.equal(
  b.restoreCommand(
    {
      ...saved.destination,
      region: 'eu-west-1',
      endpoint: 'https://acct.r2.cloudflarestorage.com',
      profile: 'work',
    },
    { snapshot: '2026-11-14T030512Z', config: true },
  ),
  'karmayoga restore s3://my-backups/karmayoga ~/karmayoga-restored --region eu-west-1 ' +
    '--endpoint https://acct.r2.cloudflarestorage.com --profile work ' +
    '--snapshot 2026-11-14T030512Z --config',
)
assert.ok(
  !b
    .restoreCommand({ ...saved.destination, profile: 'work', credentials: 'stored' })
    .includes('--profile'),
  'a profile means nothing to a pasted key',
)
assert.equal(
  b.restoreCommand({
    type: 'gcs',
    bucket: 'b',
    prefix: 'kv',
    credentialsFile: '/home/you/key file.json',
  }),
  "karmayoga restore gs://b/kv ~/karmayoga-restored --credentials-file '/home/you/key file.json'",
)

// ── the vault's messages, as sentences ───────────────────────────────

assert.equal(b.sentence('choose a folder for backups'), 'Choose a folder for backups.')
assert.equal(
  b.sentence('Works. Nothing is kept at /mnt/backups yet.'),
  'Works. Nothing is kept at /mnt/backups yet.',
  'one that is a sentence already is left alone',
)
assert.equal(
  b.sentence("s3://my-backups holds another vault's backups; choose somewhere else"),
  "s3://my-backups holds another vault's backups; choose somewhere else.",
  'a URL at the front is not capitalised',
)
assert.equal(b.sentence('"My-Backups" is not a bucket name'), '"My-Backups" is not a bucket name.')
assert.equal(
  b.sentence('/media/disk is not there; is the disk plugged in?'),
  '/media/disk is not there; is the disk plugged in?',
)
assert.equal(b.sentence('  '), '')

// ── durations and reports ────────────────────────────────────────────

assert.equal(b.durationLabel(0.4), 'under a second')
assert.equal(b.durationLabel(40), '40 s')
assert.equal(b.durationLabel(59.7), '1 min', 'rounds into minutes rather than saying 60 s')
assert.equal(b.durationLabel(172), '3 min')
assert.equal(b.durationLabel(3600), '1 h')
assert.equal(b.durationLabel(3900), '1 h 5 min')

const report = {
  snapshot: '2026-11-14T030512Z',
  files: 812,
  bytes: 24 * 1024 ** 3,
  uploadedBytes: 1.2 * 1024 ** 3,
  uploadedChunks: 40,
  removedSnapshots: 2,
  removedChunks: 30,
  seconds: 172,
  skipped: [],
}
assert.equal(b.reportSummary(report), '1.2 GB sent of 24.0 GB · 3 min')
assert.equal(b.reportDetail(report), '812 files · 2 older copies let go')
assert.equal(b.reportDetail({ ...report, removedSnapshots: 1 }), '812 files · 1 older copy let go')
assert.equal(b.reportDetail({ ...report, removedSnapshots: 0 }), '812 files')

const progress = {
  phase: 'copying',
  manual: true,
  filesDone: 0,
  filesTotal: 0,
  bytesDone: 0,
  bytesTotal: 0,
  uploadedBytes: 0,
  uploadedChunks: 0,
  started: '2026-11-14T03:05:12Z',
}
assert.equal(b.progressValue(progress), 0, 'nothing to divide by before the files are counted')
assert.equal(b.progressNote(progress), '0 B sent')
assert.equal(b.progressValue({ ...progress, bytesDone: 50, bytesTotal: 200 }), 0.25)
assert.equal(
  b.progressNote({ ...progress, uploadedBytes: 2048, filesDone: 12, filesTotal: 300 }),
  '2.0 KB sent · 12 of 300 files',
)

assert.equal(
  b.nextDueLabel('1970-01-01T00:00:00Z'),
  'due now',
  'a destination nothing has been sent to is due from the start of 1970',
)
assert.equal(b.nextDueLabel('2026-11-14T03:00:00Z', Date.parse('2026-11-14T04:00:00Z')), 'due now')

await close()
console.log('backup: all checks passed')
