# Renaming to Jatayu: carrying two former names across

The application was Every Day until 14 November 2026, then Karma Yoga, and is
Jatayu from 26 November 2026. The first rename taught the code to carry an
installation from one former name; this one has two, and most machines that
matter -- Hari's above all -- went through both. This note is how a first
launch of a Jatayu build gets from either to Jatayu without losing anything,
and why some old names are never touched. The README's "Coming from Karma
Yoga or Every Day" is the same thing for the person upgrading.

## Fan in, never chain

Every carry-over goes from whichever former name it finds **straight** to
Jatayu, looking newest first: Karma Yoga, then Every Day. It never runs Every
Day → Karma Yoga → Jatayu, because a chain would:

- keep every Karma Yoga spelling alive in the code only to rename it again;
- make Karma Yoga folders, links and keychain entries on a machine that never
  ran Karma Yoga, for the moment between two hops;
- double the states a move can be put off in (a busy folder, a held lock),
  and leave a half-done state if the app is killed between hops.

Newest first is also the tie-break: when both former names hold the same
thing -- a preference in the webview, a keychain entry -- the Karma Yoga one is
the later choice and wins, and the Every Day one is left alone or dropped.
Something already under Jatayu's name always wins over both.

## One record per former name

`crates/jatayu-core/src/legacy.rs` holds a `Former` per generation
(`KARMA_YOGA`, `EVERY_DAY`) and `FORMERS`, newest first. A `Former` names
everything that generation was spelled into: the product, the platform
directories, the bundle id, the binaries, the GNOME extension, the database
file, the Postgres schema, the environment prefix, the export manifest and
sidecar, the `.ics` properties, the local host name, the wire headers, the
pairing link, the Matrix device name, the media scheme, the interface's
storage prefix, the webview marker, the files folder's internal folder, the
backup marker and format, the health data's manual source, scratch-file
prefixes and screen-time app ids. A field a generation never had is empty
(Every Day had no files folder, no backups, no health data) and every loop
skips it.

Every reader and every mover loops over `FORMERS`; there are no single old
constants left. The day a name can go entirely, deleting its entry is how
every last reader of it is found. `ui/src/lib/legacy.ts` keeps its own copy
of the two fields the interface needs (storage prefix and pairing link), in
the same order.

## The alias rule

The first rename left links behind: `~/.config/everyday -> ~/.config/karmayoga`,
`~/.local/share/everyday -> ~/.local/share/karmayoga`, and
`store/everyday.db -> karmayoga.db`. Seen naively, `~/.config/everyday`
"exists" (the check follows the link), is not a link to `~/.config/jatayu`,
and the new folder is empty -- so it looks like a pending move. Renaming it
would move the **link**, leaving `~/.config/jatayu -> ~/.config/karmayoga`:
the new name pointing back at the old one, inverted, and the real folder
never moved.

So, everywhere a former path is considered:

- An old path that is a symlink whose canonical target is another name in the
  family (any former name, or Jatayu's) is an **alias**. It is never a move
  source. It counts as done when it resolves to the same place as the new
  path.
- Only a link pointing outside the family -- somebody keeping their data on
  another disk, say -- is moved like a folder, as the first rename did.
- After a move, a link is left at each **real** old path pointing at the new
  one. The `everyday` links are left as they are and keep working through the
  chain (`everyday -> karmayoga -> jatayu`): two hops, well inside any
  platform's limit, and nothing has to know they exist.

The same rule covers the database file: the source is the newest former name
that is a regular file, never a link; its write-ahead log is folded in first
(a checkpoint, then no `-wal` or `-shm` left), it is renamed to `jatayu.db`,
and a relative link is left at its old name. Backups list all three names as
the database or its logs, so a link is never copied as a second database.

## Markers

A carry-over that cannot tell "done" from "never needed" by looking at the
disk leaves a marker. The one that matters is the webview's: its folder is
named after the bundle id, and the settings folder records that it was
carried. Each generation has its own marker (`Former::webview_marker`):
Every Day's is `webview-carried-over`, the name already on disk, and Karma
Yoga's is `webview-carried-over-from-karmayoga`. Because the settings folder
moves with its markers, a Jatayu build finds Every Day's marker on Hari's
machine and must not take it to mean Karma Yoga's folder was carried. The
shell carries the newest former webview folder present and then writes the
markers for it and every older generation, so an older folder can never later
replace a newer one's preferences. Backups leave every marker out (they
describe this machine, not the vault).

## What is frozen, and why

These keep their old spelling for good, and `tests/pinned_strings.rs` holds
them:

- **Encryption labels.** `everyday.<kind>.v1:<id>`, the key-wrapping and
  blob-address labels, and the `karmayoga.*` family added while the app was
  Karma Yoga (health, the Karma book, backups and `karmayoga.backup.repository`,
  connections, files and Feed settings, the documents cache, file tokens).
  Each is an input to the cipher: changing one means decrypting and resealing
  every record to change bytes no person reads.
- **Index tokenizers.** `everyday_*` in the mail index and `karmayoga_docs`
  in the documents index are part of a schema already written into an index.
- **The `.ics` uid suffix `@everyday`.** Calendars already hold events under
  those uids; a new suffix would duplicate every one.
- **A Postgres vault's schema.** One that named none stays in `karmayoga` or
  `everyday`, whichever exists, and is read there: another machine may share
  the database. Only a new vault gets `jatayu`.
- **The server's certificate.** One made under an old local host name is
  kept, not regenerated: paired devices, the GNOME collector and MCP clients
  pin its fingerprint.

Readers accept every former spelling instead of moving anything: media
addresses in stored mail, drafts and Feed stories (`current_scheme`), the
headers a copy of the app speaks to another in, pairing links, export
archives and sidecars, `.ics` properties, backup repositories, health
readings typed by hand, screen-time app ids, and scratch files left in the
temporary and cache folders. Environment variables under an old prefix are
not read, only named in a warning at startup.

## Not carried

Some things belong to the platform, keyed by an id that changed, and are
redone by hand: GNOME's global shortcuts (the portal's app id), macOS
permissions (the bundle id), the phone app (a new package id, so a new
sandbox: install and pair again), the Firefox add-on (its id changed: paste
a new code), and the launcher (`make desktop-entry`).

The panel's webview folder in the cache (`~/.cache/karmayoga/panel-webview`)
is moved to the new cache folder in one rename, or removed when the new one
already has one (it is the stale copy), and the old cache folder goes once
it is empty (`carry_panel_cache` in the app's `legacy.rs`).

A documents cache a former name made for a vault whose path did not move
(`karmayoga-docs-<hash>`) is adopted, so nothing is parsed again; other
former scratch -- caches named for the default vault's old path, decrypted
copies, downloads, transcripts, browser profiles -- is removed once it has
been idle for an hour and no build under a former name is running.

## Hari's machine

What is there, and what a first launch of Jatayu does with it:

| Today | After |
|---|---|
| `~/.config/karmayoga/` (real) | renamed to `~/.config/jatayu/`; link left at `karmayoga` |
| `~/.local/share/karmayoga/` (real, 33 GB) | renamed to `~/.local/share/jatayu/`; link left |
| `~/.config/everyday`, `~/.local/share/everyday` (absolute links to `karmayoga`) | aliases: untouched, resolve through the new links |
| `store/karmayoga.db` 7.8 GB, with `-wal` and `-shm` | checkpointed, renamed to `store/jatayu.db`; link `karmayoga.db -> jatayu.db` |
| `store/everyday.db -> karmayoga.db` | alias: untouched, resolves in two hops |
| `last-vault` = `~/.local/share/karmayoga` | rewritten to `~/.local/share/jatayu` |
| `webview-carried-over` (Every Day's marker) | moves with the folder; not taken as Karma Yoga's |
| `~/.local/share/app.karmayogi.KarmaYoga/` | renamed to `app.jatayu.Jatayu/`; both markers written |
| 24 `karmayoga.` / `karmayoga:` keys in its localStorage | renamed to `jatayu.` / `jatayu:` by `lib/legacy.ts` |
| keyring `app.karmayogi.KarmaYoga` / `vault-key:…/karmayoga` | filed under `app.jatayu.Jatayu` / `vault-key:…/jatayu`; old item removed |
| `screentime@karmayogi.app`, enabled | `screentime@jatayu.app` enabled, old one unlisted and removed |
| `~/Karma Yoga/.karmayoga/` and `.stignore` naming it | `.karmayoga` renamed to `.jatayu`; `.stignore` rewritten; the folder keeps its name |
| `server.names` = `127.0.0.1 everyday.local localhost` | kept: the certificate and its pins stay |

Before anything moves, the old app has to be closed: a running `karmayoga-app`
or `karmayoga` (including an MCP stdio bridge), or any process with a file
open under the old folders, puts the folder move off to the next launch, and
the folders are used in place meanwhile.

## Test plan

- **Unit tests, per crate, in temporary folders** (the vault's test sandbox;
  nothing reads the real home folder inside a test binary):
  - folders: Hari's layout built in a temporary folder -- real `karmayoga`
    folders, absolute `everyday` links to them -- moves the real folders,
    leaves links at the real old paths, leaves the `everyday` links alone, and
    a second run reports nothing to do; an Every Day-only machine goes
    straight to Jatayu with no `karmayoga` path made; a link pointing outside
    the family is moved like a folder; a non-empty new folder is a conflict,
    not an overwrite;
  - the database: `karmayoga.db` with `everyday.db -> karmayoga.db` ends as
    `jatayu.db` with both old names resolving to it; a busy database is left;
    the backup filter takes all three names;
  - the webview: with Every Day's marker present, Karma Yoga's folder is still
    carried; with both former folders present, only the newer moves;
  - the keychain: the spellings tried include the Karma Yoga service and path;
  - readers: each former scheme, header, pairing link, export manifest, `.ics`
    property and backup marker is accepted.
- **The frozen vault fixture**, unchanged on disk, opened once as it is
  (`store/everyday.db`) and once rearranged in its temporary copy the way
  Hari's is (`karmayoga.db` plus the `everyday.db` link).
- **The interface**: `ui/scripts/legacy.test.mjs` -- Karma Yoga keys,
  Every Day keys, both present (Karma Yoga wins), a Jatayu value already
  present wins, the dynamic `week-words:<date>` keys, a refused write keeping
  the newer value for next time, pairing links in every spelling, and every
  entry point importing the carry-over first.
- **On Hari's machine**, before the first launch: quit the app, run
  `jatayu migrate --dry-run` and read what it would do; then launch, and check
  with `ls -l` and `readlink -f` that the real folders and `jatayu.db` are
  where the table says and every old path resolves to them; that the vault
  opens by itself (the keychain), the theme and layout came back (the
  webview), Settings → Files shows the folder in use, screen time still
  records, and the paired devices and MCP clients still connect.
