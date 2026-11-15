# Backups

Sealed, date-stamped copies of the vault and of this computer's settings,
made on a schedule, to a folder or to an S3 or Google Cloud Storage bucket,
pruned to a number of copies. Settings → Backup, `karmayoga backups`, and
`karmayoga restore`.

## What it is for, and what it is not

The day the disk dies, the laptop is stolen, or a sync goes wrong: a copy of
everything, somewhere else, that opens with the same password. It is not an
export -- an export takes the encryption off so other programs can read your
writing (see the README's "Leaving with your writing") -- and it is not
`karmayoga backup <dir>`, which makes one plain copy of the vault folder,
once, by hand. That verb stays, and now copies mail's packs and a chat link's
store too, which it used to leave behind.

## Why a repository and not a zip a day

Hari's vault is 24 GB: a 7.3 GB database (80% of it mail bodies, and 1.4 GB
of zeroed free pages), 9.1 GB of mail packs and 6.8 GB of attachments. A zip
a day kept seven times is 170 GB in a bucket and 24 GB up a home connection
every night; nobody would leave that switched on.

So a destination holds a **repository**: every file cut into chunks, each
sealed and named by a keyed hash of its contents, and one sealed manifest
per copy listing the chunks that make up each file. Each manifest is a whole
copy -- restoring it needs nothing from any other -- and copies share every
chunk that did not change. Seven copies cost about one vault plus a week of
changes, and a nightly backup sends only what changed.

```text
  <folder, or bucket/prefix>/
    karmayoga-backup.json             what this is, and whose (a keyed id)
    vault-header.json                 the vault's header: what the password opens
    README.txt                        for whoever finds it in ten years
    snapshots/2026-11-14T030512Z.snapshot   one sealed manifest per copy
    chunks/3f/3f9a…                   sealed pieces, shared between copies
    lock.json                         while a backup is writing
```

The snapshot names are the "date stamped files": UTC, no colons (a FAT disk
or a Windows share refuses them), sorting as they read.

### Chunks

Fixed offsets: **64 KiB for a database, 1 MiB for anything else.**

The database is copied with SQLite's backup API, page for page, rather than
`VACUUM INTO` (which the old `backup <dir>` uses and keeps using). That is
what makes dedup work at all: a page copy has the same layout as the live
file, so a day of mail moves only the pages it touched, while `VACUUM INTO`
rebuilds the file and shifts every page after the first change. Those
touched pages are scattered (index leaves, free-list reuse), so the chunk
has to be small or each costs a megabyte: sixteen pages. Zeroed free pages
all name the same chunk, so 1.4 GB of them is stored once.

Measured on the real database (Nov 14 2026): two page copies 39 minutes
apart, with the app running and mail syncing, differed in 190 of 119,126
64 KiB chunks -- 11.9 MB to send. Cut at 1 MiB, the same change was 90 of
7,446 chunks, 94.4 MB: eight times as much for the same day.

Everything else is written once and appended to -- attachments are
content-addressed and immutable, mail packs append and are compacted into new
files -- so large chunks cost nothing in sharing and keep the object count
down. A file whose size and modification time match the previous copy's is
not read at all.

No compression of chunks: almost everything in a vault is already sealed and
does not compress. Manifests are deflated (they are mostly chunk names).

### Sealing and names

`karmayoga_core::backup::BackupKeys`: two subkeys of the vault's data key,
by label (`karmayoga.backup.seal.v1`, `karmayoga.backup.name.v1`).
XChaCha20-Poly1305 for chunks and manifests, with the chunk's name or the
snapshot's name in the associated data, so nothing can be moved under
another name. A chunk's name is 128 bits of keyed BLAKE3 over its plaintext:
the same bytes are stored once, and a bucket listing tells nobody without the
key whether you hold a well-known file. Opening a chunk also re-derives its
name and refuses a mismatch.

The key is the vault's, so **the backup opens with the vault password** and
there is no second secret to lose. `vault-header.json` is the header -- salt,
KDF parameters, the data key wrapped under the password -- kept in the clear
exactly as `vault.json` is beside a vault; only the latest is kept, so a
password changed since opens every copy and the old one opens none. An
unencrypted vault's subkeys derive from nothing secret; Settings says its
backups are no more private than it is.

### Secrets

Nothing is unsealed on the way: credentials in the vault (model keys,
account tokens, the pasted S3 key) travel inside the sealed database exactly
as they are stored. The settings folder's credentials (the server's private
key, the MCP token, paired devices) travel inside sealed chunks. Never copied:
the vault password (never stored anywhere) and the key kept in the system
keychain for opening the vault by itself.

## What a copy holds

- **The vault**: `store/karmayoga.db` (consistent, while the app writes),
  `store/media/`, `store/mail-packs/`, `store/chat/` (its SQLite stores also
  through the backup API), and `spool/` (recordings not yet written up).
- **This computer's settings** (on by default): the config folder, minus
  `last-vault` (a path on this machine), `webview-carried-over`, `mail-index/`
  (rebuilt from the mail), `chat/` (a Postgres vault's live chat store),
  `*.lock`, `*.tmp`.
- **Never**: downloaded models (gigabytes, the same for everybody, downloaded
  again), the mail search index, `vault.lock`, the keychain entry.

Consistency: the database is copied first, then everything else is listed
and read, so anything the copy names was already on disk. Mail compaction
does not drop old packs while a backup runs (they are left for the next
compaction's orphan sweep), so a pack the copy names cannot vanish before it
is read. A file that disappears anyway (an attachment collected) is skipped
and named in the report.

## A run

`karmayoga_service::backup::run`, in order:

1. Make the folder if this destination has never been used; otherwise a
   missing marker means "not there" (a disk not plugged in) and the run
   fails rather than filling the empty mount point on the computer's own
   disk.
2. Check the marker's repository id against this vault's; take `lock.json`
   (fresh = touched in the last hour; refreshed every ten minutes).
3. Write the header. Read every manifest (cached on this computer, sealed,
   under `data_dir/backup-cache/<repo id>/`) and list `chunks/`.
4. Stage: databases copied into `<vault>/.backup-staging/<pid>-<rand>/` (same
   disk, outside anything copied). The vault's lock is held only to ask the
   store for its job, never for the copy, so saves carry on.
5. One blocking thread reads, names and seals; a queue of 2×parallelism
   hands sealed chunks to 24 concurrent uploads (8 for a folder). Memory
   stays at a few dozen MB.
6. Only once every chunk is there, write the manifest.
7. Prune: delete manifests beyond `keep`, then chunks no kept manifest names
   (everything listed or sent, minus what is referenced). Skipped entirely if
   any kept manifest could not be read.

A failed or stopped run leaves chunks and no manifest; the next run lists
them and sends nothing twice.

## Where

`karmayoga_service::backup::target::Target`: four verbs -- put, get, list,
delete -- over whole objects of at most a megabyte. No multipart, no ranges,
no SDK.

- **Folder**: temp file, `fsync`, rename; directories synced for everything
  but chunks.
- **S3**: SigV4 by hand (`backup/s3.rs`), virtual-hosted for Amazon (path
  style for dotted buckets and for any custom endpoint: R2, B2, MinIO,
  Wasabi, GCS's HMAC interop). Credentials from this computer: `AWS_*`
  variables, the profile's keys in `~/.aws`, else `aws configure
  export-credentials --profile …` (SSO, roles, credential processes) -- or an
  access key pasted into Settings, sealed in the vault.
- **Google Cloud Storage**: JSON API (`backup/gcs.rs`). Credentials: a named
  key file, `GOOGLE_APPLICATION_CREDENTIALS`, gcloud's application default
  credentials (authorized user or service account, signed with `ring`), else
  `gcloud auth print-access-token`.

Both reuse crates already compiled into the service (`reqwest`, `ring`,
`sha2`, `roxmltree`); the only new direct dependencies are `flate2` and
`gethostname`, both already in the lock.

## When

`BackupSettings { enabled, destination, every_hours ∈ {1, 6, 24, 168},
keep ∈ 1..=100, include_config }`, sealed in a new singleton table
(`backup_settings`, schema v28) beside a sealed `BackupStatus` in its own
column, so a run recording its outcome never overwrites a settings save.

A supervised task (`backup::task`), started at unlock and stopped by the
lock, looks every five minutes (first 90 s after unlock) and when a save
wakes it. Due `every_hours` after the last success, at once if none, and
not before 30 minutes after a failure. Due-while-asleep runs at the next
unlock. The lock also cancels a running backup: it holds keys derived from
the vault's, and the lock promises none are left in memory.

Notices: a scheduled backup failing for longer than its own interval (and an
hour) says so once, as an OS notification; the next success replaces it.

## Restore

`karmayoga restore <folder | s3://bucket/prefix | gs://bucket/prefix> <new
folder> [--snapshot NAME] [--config]`: needs no vault, asks for the password
only if the header says one exists, refuses a folder that is not empty,
writes the header last (until then it is not a vault), checks every chunk's
name and every file's length. `--config` puts settings files back into the
config folder, never over one that exists.

Restore is command-line only for now. A "restore from a backup" button on
the setup screen is the obvious next step; everything it needs is
`backup::restore`.

## Not done, on purpose or yet

- **Postgres vaults** are not backed up from here; whoever hosts the
  database does that. Settings says so.
- **Packing small chunks** into larger objects (restic's packs) would cut a
  first upload of ~280k objects to a few thousand, at the cost of repacking
  on prune. Measure first.
- **Grandfather-father-son retention** (seven dailies, four weeklies…).
  "Keep N" is what was asked for.
- **WebKit's local storage** (window preferences) is not copied.
