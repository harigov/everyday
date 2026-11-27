# Files: a Dropbox of your own, and a place every app keeps files

> **Delivered** (15 November 2026), built by eleven agents against a contract
> laid down first, then reviewed. What went differently from the plan below,
> each for a reason worth keeping:
>
> - **No readable path, folder, name or content hash in the database.** The
>   plan let a plain file's path and hash be clear columns because "the disk
>   already says it". It does not when the folder is on an encrypted volume
>   that is locked while somebody holds a copy of the database. They are
>   keyed tokens under a subkey of the vault key (`path_key`, `dir_key`,
>   `hash_key`), with the path and hash sealed again in `idx` for the
>   reconciler; "everything under a folder" and "by name" are answered after
>   opening. Times taken stay clear, as every table's timestamps do.
> - **`.stignore` is always made to name `.jatayu`**, not only written when
>   there is none: a folder already shared with Syncthing would otherwise
>   copy a decrypted copy or a sealed upload's staging file to every device.
>   Checked on every reconciler pass too.
> - **A corrected time survives re-reading the file**: the file's own time is
>   kept in the `files.taken_by_file` property, so "go back to the file's"
>   works without reading it again.
> - **A cached file address is kept only while its `?v=` is the file's
>   version now** -- the catalog's hash *and* the disk's size and time -- so a
>   file edited before the next pass is never cached under its old version.
> - **Stacks are a filter** (`FileFilter.stack`) **and a count on the page**
>   (`FilePage.stack_counts`), not a field on the record.
> - **The reconciler's state is per vault**, so switching vaults never shows
>   one vault's last pass as the other's.
> - **Place names** come from a reverse lookup (`maps::reverse_url`), opt-in,
>   paced with every other Nominatim question and remembered by spot.
> - **A test binary's default folder is in the test sandbox**, never
>   `~/Jatayu`.
> - **Exports carry catalogued files** in `media/` beside attachments, and an
>   import whose sidecar names files the vault lacks is read from the
>   Markdown instead.
>
> **Health** (15 November 2026, the same day) keeps a clinical document's
> pages and a meal's photograph here, always sealed, with origin `health`,
> referred to as `RefKind::HealthDocument` and `RefKind::Meal`. The file
> tools leave its files out, and `/v1/files/file/{id}` wants the `health`
> scope too for one: they answer to Health's gates. See `docs/plans/health.md`.
>
> **Still open:** HEIC and RAW previews beyond the embedded EXIF thumbnail;
> video posters only once the window has drawn them; no map view; XMP
> sidecars; the Syncthing panel (its own piece of work).

A plan, written 15 November 2026 against the tree at `9e60a1a1`. Hari's
calls are marked as his.

## What this is

1. **A files folder** — `~/Jatayu` unless chosen otherwise — holding
   ordinary, unencrypted files that Syncthing, a photo viewer or a file
   manager can use where they lie (Hari's call: plain files, so other
   software works). Jatayu is meant to be his Dropbox; syncing the folder
   is somebody else's job (Syncthing today, anything tomorrow).
2. **A catalog in the vault**: one sealed record per file — where it is,
   what it is, what the file says about itself (EXIF, a film's length), what
   the person added (caption, tags, rating, favourite, people, place,
   albums, stacks, and an open-ended set of namespaced properties).
3. **A platform layer**: every app that keeps a file — a picture in a
   journal entry, a PDF handed to the assistant, a film from a chat, a mail
   attachment somebody saves — writes it into the folder and refers to it by
   id. Nothing large goes into the database or through the webview as one
   buffer.
4. **The Files app**: browse the folder like a file manager; add, rename,
   move, trash; preview; a Photos view (timeline by date taken, albums,
   people, places, years, cameras, favourites); Settings → Files.

Out of scope here: indexing and semantic search (later, Hari's call), and
the Syncthing integration (its own piece of work, Hari's call). The catalog
leaves room for both: `properties` under `index.*`, and nothing assumes how
files arrive.

## Decisions

- **Plain by default, sealed by choice.** A file is `Plain { path }` (in the
  folder) or `Sealed { blob }` (in the vault's blob store, readable only by
  Jatayu). `FilesSettings.encrypted` lists the apps whose new files are
  sealed; empty by default. Any file can be moved either way. Mail
  attachments, Money and app furniture (covers, Feed pictures, thumbnails)
  stay where they are and are not catalogued at all.
- **Never get in the way of protecting the folder at the OS level** (Hari's
  call): an encrypted volume, fscrypt, a Cryptomator mount. The folder can be
  anywhere. A marker file (`.jatayu/folder.json`) tells a folder that is
  there from an empty mount point: no marker where the catalog expects files
  means *unavailable*, never *empty*, and nothing — the reconciler, the trash
  sweep, a backup — acts on it. Thumbnails are sealed blobs, and the trash
  and plain uploads in progress live in `.jatayu/` inside the folder
  (which a fresh `.stignore` tells Syncthing to leave alone). What is
  decrypted from a sealed file never touches the folder, which may be
  synced: a copy opened in another program and a sealed upload in progress
  live in owner-only directories under the cache directory
  (`jatayu-open-*`, `jatayu-staging-*`), are deleted when the vault
  locks, and are swept after an hour and at the next start.
- **A folder made under Karma Yoga** has `.karmayoga/` where this has
  `.jatayu/` (`Former::files_internal_dir`). The marker is how a folder is
  told from an empty mount point, so it is renamed to `.jatayu/`, with the
  trash and uploads inside, rather than missed — missing it would make the
  folder read as unavailable — and `.stignore` loses its `.karmayoga` line
  and names `.jatayu`. The folder itself keeps its name and place (`~/Karma
  Yoga`): its path is a setting, and Syncthing or a backup knows it by that
  name. Scratch copies left under `karmayoga-open-*` and `karmayoga-staging-*`
  are swept like the new ones. See `rename-jatayu.md`.
- **The folder is the truth for plain files.** A watcher (`notify`) and a
  pass every five minutes reconcile the catalog with the disk. A file that
  moved while nobody watched is recognised by its BLAKE3 hash and keeps its
  id, caption, albums and references. A file that is gone is *missing*, kept
  with everything the person added, until it comes back or they forget it.
  When more than half the files vanish at once, nothing is marked missing:
  that is a half-mounted drive, not a person deleting.
- **Paths in the catalog are relative**, so moving the whole folder with a
  file manager and pointing Settings at the new place just works.
- **References are a table**, maintained on every save of an entry, a note
  or an assistant message, with a rebuild from those records as a backstop.
  It answers "used in" and the "Unused" view. Plain files are never deleted
  automatically — the folder is the person's.
- **Rich text refers to a file with a media node carrying `file`** instead
  of `blob`. An assistant message carries `files: [FileId]`; a picture among
  them also gets the existing 2048-pixel sealed copy, which is what a model
  is shown.
- **Postgres vaults keep blobs on disk too** (Hari's call: the Postgres
  backend was never used). The `blobs` table and its 900 MB cap go.
- **Adding files never goes through the webview as one buffer.** A window
  on the vault's machine imports by path (picker, drop). Anything else — a
  paired desktop, a browser over web access, a paste — uploads in 4 MB
  chunks to a staging file. `import_files` is refused to any caller but the
  local shell, or a remote client could copy `~/.ssh` into a folder it can
  read.
- **Metadata is read, never written.** EXIF via `kamadak-exif`, films via
  `mp4`; both pure Rust. Writing into a file would change its hash, make
  Syncthing resend it and risk the original. XMP sidecars for other photo
  tools can come later.
- **Clear columns only for what the disk already says.** A plain file's
  path, hash, size and time taken are readable on disk anyway, so they are
  clear columns for the indexes; a sealed file's are not. Captions, tags,
  people, places and properties stay sealed. Rating and favourite are clear,
  as the library's are.
- **Backups include the folder** (`files/` in the snapshot). While the folder
  is unavailable, a backup carries the previous snapshot's copy of it
  forward instead of recording it empty — otherwise a few runs during a
  locked volume would push every real copy out of "keep N".
- **Previews reuse the mail attachment viewer's rules**: images, video,
  audio, PDF (pdf.js), text. Office files still are not previewed (Hari's
  earlier call). HEIC and RAW show the JPEG most cameras embed in the EXIF
  when there is one; a film's poster is drawn by the webview and handed back.

## Where things are

| Layer | Files |
|---|---|
| Records, value types | `jatayu-core/src/files/mod.rs` |
| The folder (paths, copying in, trash, uploads, walking) | `jatayu-core/src/files/disk.rs` |
| What changed on disk (pure) | `jatayu-core/src/files/reconcile.rs` |
| Store trait, AEAD labels | `jatayu-core/src/store/files.rs` |
| SQL tables (schema v29) | `jatayu-store-sql/src/files.rs`, `schema.rs` |
| Facade | `jatayu-core/src/vault/files.rs` |
| Metadata, thumbnails, reconciler task, watcher | `jatayu-service/src/files/` |
| Commands | `jatayu-service/src/domains/files.rs` |
| Assistant tools | `jatayu-core/src/agent/tools/files.rs` |
| `jatayu://files/file/{id}`, pickers, open-with | `jatayu-app/src/protocol.rs`, `files.rs` |
| Remote and web routes | `jatayu-server/src/routes.rs`, `client.rs` |
| Interface | `ui/src/lib/files*.ts`, `ui/src/components/Files*.svelte`, `Photos*.svelte` |

## Phases

1. **Platform**: the folder, the catalog, references, the reconciler,
   streaming import and chunked upload, `jatayu://files/…` with ranges,
   backups, Postgres blobs on disk, bringing old attachments in.
2. **Files app**: browse, add, organise, preview, trash, storage, settings.
3. **Photos view**: timeline, albums, people, places, stacks, ratings.
4. **Every app on it**: any file in the composer and from Matrix; pictures
   and films in journal entries and notes; "Save to Files" on mail
   attachments.
5. **Assistant and MCP**: find, read, organise and save files, behind
   `FilesSettings.assistant_access`; reading a file taints the turn; MCP
   tokens need the `files` scope.
