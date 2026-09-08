# Every Day

A private journal, a todo app and a calendar for macOS, Linux and Windows.
Rich text with photos and video, projects and tasks on a list or a board,
your week with the plan and the record side by side, pluggable storage, and
encryption you actually hold the key to.

<!-- Screenshots live in docs/ once you have run the app. -->

## Why this stack

The brief was: native, cross-platform, extensible to iOS and Android later,
fast, small, beautiful, and with excellent font rendering. That set of
constraints points fairly firmly at one answer.

**Rust core + [Tauri 2](https://tauri.app) shell + Svelte/TipTap interface.**

- **Font rendering.** Every platform's text stack — CoreText, DirectWrite,
  Pango/FreeType — is better than anything an app can ship, and it is what the
  OS has already tuned and cached. Tauri renders in the system webview, so
  text is rasterised by the platform itself. The *shapes* are ours, though:
  the app bundles two variable faces — [Inter](https://rsms.me/inter/) for the
  interface and [Source Sans 3](https://github.com/adobe-fonts/source-sans)
  for entry bodies — because "the system UI face" means San Francisco on one
  machine and whatever fontconfig picked on another, and a journal should not
  look like a different application depending on where it is opened. Nothing
  is fetched at runtime: the woff2 files are embedded in the binary and load
  from `'self'`, so the app still starts with no network.
- **Size and memory.** The webview is already resident on every target OS, so
  the binary is a Rust core plus a few hundred KB of interface, not a bundled
  browser. That is the difference between roughly 10 MB and roughly 150 MB.
- **Rich text.** A competent editor — selection models, input methods,
  undo across embedded media, spellcheck, accessibility — is years of work in
  a native toolkit. ProseMirror already solved it. This is the single largest
  reason not to reach for a pure-native GUI here.
- **Mobile later.** Tauri 2 targets iOS and Android from the same core. The
  shell is a *library* with a `run()` entry point precisely so that adding a
  mobile target is a new shell, not a rewrite.

The honest trade: the interface runs in a webview, so it is not "native
widgets". In exchange you get native *text*, a mature editor, and one
codebase. For a journal — an app that is essentially a text canvas — that is
the right side of the trade.

## Three apps, one vault

The sidebar switches between **Journal**, **Todo** and **Calendar**
(`Ctrl/Cmd J` cycles). They share a vault, a password and a lock; they share
nothing else — except that the calendar is a view over what the other two
already store, which is the whole point of it.

The todo app has projects, tasks and subtasks — a subtask is just a task with
a parent, so the two levels the interface offers are a UI decision rather
than a schema. Everything carries a title, description, due date and time,
start date, priority, effort estimate and tags. There is a **list** view,
grouped by due date, status or priority, and a **kanban board** whose columns
are the six task statuses. Tags go on projects, tasks and blocks of time
alike, which is what a later analytics view will count.

Capture is the part that had to be fast, so adding a task is one line and one
Enter, and the line carries its own fields:

```
Book the flights #travel !high ~1h30 @fri @16:30
└── title ──────┘ └tag─┘ └prio┘ └est┘ └── when ──┘
```

`@` takes `today`, `tomorrow`, a weekday, `3d`/`2w`, `2026-09-12`, or a clock
time. Anything not understood is left in the title rather than silently
dropped. The field clears and keeps focus, and submissions queue rather than
being ignored, so twelve tasks cost twelve lines and twelve Enters.

### Where the time went

A task does not have a "scheduled at" field. It has any number of **time
blocks** pointing at it, each either *planned* or *actual*:

```
Project ──┬── Task ──┬── Task (subtask)
          │          │
          └──────────┴── TimeBlock   planned, and what actually happened
```

Keeping plan and record as separate rows is what makes "where did my time
go" answerable at all — one field overwriting the other cannot be compared
with itself. A block can also point at a project, or at nothing (a dentist
appointment), which is the shape a calendar needs.

## The calendar

Day, week and month, with four things drawn on the same grid and told apart
by weight rather than by colour — colour is already spoken for, it says which
project or which calendar something belongs to.

```
  planned block   a wash, dashed rail     you meant to do this
  actual block    solid fill, solid rail  you did
  event           outlined, tinted        somebody else's calendar
  task            an outlined chip        due that day, not booked
```

A header toggle draws **Plan**, **Record**, or both, and that toggle is the
reason the storage looks the way it does. It is not a filter bolted on
afterwards: `TimeBlock` has had a planned/actual split since the todo app
shipped, precisely so that "two hours booked, forty minutes actually spent"
would be a comparison rather than a lost field.

Scheduling is dragging. Open tasks with nothing booked against them sit in a
rail on the right; drag one onto Tuesday afternoon and it becomes a planned
block pointing at that task, as long as its estimate — and drops off the rail,
because a plan you have already made is not a thing to be nagged about. Drag
blocks to move them, drag their bottom edge to resize, drag on empty grid to
make an event out of nothing. Everything snaps to a quarter hour. (Dragging
is the better gesture, so Enter on a task in the rail finds it the first free
slot instead — a feature only a mouse can reach is one half the people using
it do not have.)

**Where the day actually went.** The rail's top strip answers "what am I
doing right now": it offers whatever you planned for this minute, or the
meeting that is on, or a line you type, and starting it writes an *actual*
block that grows while you work. Stopping puts an end on it. Two writes for a
two-hour session — between them the length is read off the wall clock rather
than from storage, so a running timer is not a write every second.

The calendar also knows about the journal: a day you wrote something on
carries a small mark, in the week header and in the month cell.

### Other people's calendars

Google, Outlook and Apple all publish a calendar as an
[iCalendar](https://www.rfc-editor.org/rfc/rfc5545) feed at a secret URL, and
all three let you revoke that URL without touching the account. Paste one in
and its events appear on the grid, read-only, in a colour you choose. A
`.ics` file can be imported instead, and any other publisher of a feed works
the same way — a team calendar, a fixture list, your country's public
holidays.

Subscriptions rather than accounts, deliberately. There is no OAuth client
registered with a vendor, no redirect server, no token to refresh and no
scope that could grow later — which is the only arrangement that keeps
working for an application that is a binary you built yourself rather than a
product with a client id. The honest trade: **the sync is one way.** Events
you create here are yours and stay here.

Some care went into the parts that are easy to get wrong:

- **Recurrence is expanded at sync time, not at draw time.** A weekly
  stand-up arrives as one `VEVENT` with an `RRULE` and is stored as one row
  per week within a window of a year back and two forward. That makes the
  grid a date-range index scan with no recurrence engine near the draw path,
  at the cost of some rows. `RECURRENCE-ID` overrides — the week the Tuesday
  stand-up moved to Wednesday — replace exactly the occurrence they name.
- **Time zones come from the system database, not from the feed.** A
  `VTIMEZONE` block is read for its `TZID` and its offset rules are ignored,
  because the machine's tz database knows about the rule change the feed was
  generated before. Windows zone names (`W. Europe Standard Time`) are
  mapped; a floating time is read in your own zone, which is what a floating
  time means.
- **A failed refresh keeps what was already there.** A captive portal, an
  expired link, a 200 with an error page in it: all of them parse to zero
  events, and all of them would otherwise empty a working calendar. Anything
  that is not an iCalendar document is refused before it can replace one, and
  the reason is recorded beside the calendar rather than raised as a dialog.
- **The feed URL is a credential** and is sealed like everything else — never
  in a clear column, and never in an error message, which is a place error
  strings have a habit of ending up.

The network. This application has no telemetry, no update check, no crash
reporter and no analytics: a request leaving the process means somebody
subscribed to a calendar. The fetch lives in the desktop shell
(`crates/everyday-app/src/feeds.rs`), which is the only file in the codebase
that opens a socket; `everyday-core` still has no async runtime, no TLS stack
and no way to reach the network at all, which is what keeps the difficult
half — RFC 5545, recurrence, zones — testable offline. The webview's own
permissions are unchanged and remain none: its content security policy allows
no outbound connection, so a feed's contents can never cause a request of
their own.

## Layout

```
crates/
  everyday-core/            domain model, crypto, storage traits, search, iCalendar
  everyday-store-sqlite/    SQLite backend (default)
  everyday-store-markdown/  plain Markdown files backend
  everyday-vault/           wires core to backends; platform paths; media serving
  everyday-cli/             `everyday` — scripted capture, export, inspection
  everyday-app/             Tauri desktop shell (window, commands, media protocol)
ui/                         Svelte 5 + TipTap interface
scripts/                    capped test runner, dev runner, Linux setup
```

`everyday-core` has no UI, no platform and no async runtime. That is what lets
the same logic back a desktop shell today and a mobile one later, and it is
why the whole test suite runs on a machine that cannot build a GUI.

## Encryption

```
password ──Argon2id(random salt, 64 MiB)──▶ KEK ──unwraps──▶ DEK ──▶ records + media
```

A random 256-bit data key encrypts everything. That key is itself stored
wrapped by a key derived from your password with Argon2id. Changing your
password re-wraps the data key — instant, no matter how large the journal.

Records are sealed with **XChaCha20-Poly1305**, each bound to its own identity
as associated data, so an attacker with write access to the store cannot move
one entry's ciphertext over another and have it decrypt. Attachments are
sealed in 256 KiB chunks, each bound to its blob address and position, which
is what lets a video seek without decrypting the whole file.

**There is no recovery.** The password is not stored and cannot be reset. The
app says so, loudly, at creation time.

### What is *not* encrypted

The SQLite backend keeps a few structural columns in the clear so date-range
queries and pagination stay index scans: `journal_id`, `local_date`,
timestamps, and the starred/pinned flags. Titles, bodies, tags, locations,
file names and media are all sealed. Someone with the database file learns
*that* you wrote on 14 July and never what you wrote.

The task tables make the same trade for the same reason — a board filters by
status and a calendar by day, and both would otherwise decrypt every row on
every draw. In the clear: the shape of the task tree (`project_id`,
`parent_id`), a task's `status`, `priority` and `due_date`, and a block's
start, end and day. Sealed: every title, description and tag. The file says
that four things are blocked and never what they are.

The calendar tables go further in one respect. A subscription URL is a bearer
credential — anyone holding one can read that calendar until it is revoked —
so it is sealed along with the calendar's name, and only `calendar_id`, the
two date columns and the start instant are in the clear. The file says that
you have three calendars and which days have something on them.

If that trade is unacceptable, the storage abstraction is the answer: a
backend that seals the index columns too — at the cost of full scans — drops
in without the rest of the app noticing.

## Storage backends

Storage sits behind one trait, [`JournalStore`], so alternatives can be tried
without touching the app.

| Backend | Good for | Trade |
|---|---|---|
| `sqlite` | the default; large journals, fast queries, the todo app and the calendar | opaque on disk |
| `markdown` | grep, git, editing in any editor | slower; readable only when unencrypted; journals only |

The task domain is a *second* trait, `TaskStore`, reached through
`JournalStore::tasks()`, which returns `None` by default. Folding a kanban
board into the journal trait would oblige every backend to grow a todo
implementation it has no opinion about — a board is not a thing anyone wants
as a tree of files. So SQLite implements it, Markdown does not, and the
interface reads `capabilities.tasks` and hides the app rather than failing at
click time.

Subscribed calendars are a *third*, `CalendarStore`, on exactly those terms.
Note how little is in it: the calendar app draws time blocks, which have
lived in the task domain since the todo app shipped, so this trait owns only
the part that was genuinely new — the feeds you subscribed to and the events
read out of them. Events are written one way only, `replace_events`, which
swaps a calendar's entire set at once. That makes a refresh atomic and total,
which is what makes it safe to run in the background without asking, and it
means nothing a sync does can touch a record you made.

The Markdown backend is a genuine two-way format. It writes a readable `.md`
with TOML frontmatter plus a sidecar `.json` holding the exact rich-text tree.
Edit an entry in vim and the app notices — the frontmatter records a hash of
the body as written, so a mismatch means a human has been at it, and your
Markdown wins over the sidecar.

### Not losing things

The parts of the design that exist only to keep data:

* **The header is written twice.** `vault.json` holds the wrapped data key,
  which exists nowhere else — lose it and every entry is ciphertext under a
  key nobody can derive. It is written fsync-then-rename-then-fsync-the-
  directory, and the previous one is kept as `vault.json.bak`. A vault whose
  live header is missing or corrupt opens from the spare and repairs itself.

* **`synchronous = FULL`.** The usual WAL pairing is `NORMAL`, under which a
  power cut can roll back every transaction since the last checkpoint. That
  is a fine trade for a cache and a poor one for a journal, and it costs
  nothing when writes come from a 700ms autosave timer rather than a loop.

* **The window waits for the save.** Closing used to fire the autosave and
  let the window go; the flush is an async round trip and lost that race
  essentially every time, taking up to 700ms of typing with it. The shell now
  cancels its own close, asks the interface to finish writing, and closes when
  it reports back — or after three seconds regardless, because an app that
  will not quit is its own bug.

* **A failed save is retried.** The dirty set is put back on failure and the
  delay widens to 30s, so a full disk or a busy database does not silently
  discard what was typed while an error banner was on screen.

* **GC will not collect anything young.** See the note under the CLI above.

* **A newer schema is refused.** An older build opening a database from a
  newer one used to skip every migration and write into a shape it did not
  understand.

* **One writer at a time.** Opening a vault takes an exclusive OS lock on the
  directory (`vault.lock`). Whoever has it may write; anyone else — a second
  copy of the app, or `everyday` on the command line — gets the vault
  **read-only** rather than being turned away, so listing, searching, `check`
  and `backup` all still work and only the writes are refused. The lock is
  the kernel's, held on an open file handle, so a crash releases it; a
  leftover lock file is never an obstacle. On the desktop a second launch
  raises the window you already have instead of opening another.

* **Saves check what they are replacing.** Every entry save carries the
  `updatedAt` it loaded, and the store writes only if that is still what is
  there. A vault in a synced folder written on another machine, or an editor
  left open across a change, no longer silently overwrites: the save is
  refused, the text stays on screen, and the editor offers *keep mine* or
  *discard mine*.

**Every backend must pass the same conformance suite**
(`everyday_core::store::conformance`), so backends do not write their own CRUD
tests — they inherit ~15 shared behaviours covering round-tripping, filtering,
pagination, blob dedup, range reads, cascade deletes, GC and Unicode. That is
what makes "swap the backend" a real claim rather than an aspiration. A
backend that answers `tasks()` inherits a further ~15 covering the task
domain: the tree, the cascades, the date windows, and the rule that deleting
a task takes the time booked against it too. A backend that does not is told
it is being skipped, because a silently skipped suite is worse than none.

## Building

### Prerequisites

- Rust 1.85+ (2024 edition)
- Node 22.12+ (see `ui/.nvmrc`)

### Run it

```sh
make setup    # WebKitGTK headers on Linux, Tauri CLI, npm packages
make run      # the desktop app, with hot reload
```

`make` on its own lists every target. It is a thin wrapper and not a build
system of its own: `make run` is `./scripts/dev.sh`, `make test` is
`./scripts/test.sh`. Use the scripts directly if you prefer them.

### Test it

```sh
make test                          # whole workspace
make test ARGS="-p everyday-core"  # one crate
make lint                          # fmt, clippy, and the interface typecheck
make fix                           # apply what `make lint` can fix on its own
```

`make lint` runs exactly what CI runs, so a red build never tells you
something you could not have found locally first. It covers both halves:
rustfmt and clippy over the crates, Prettier, ESLint and `svelte-check` over
the interface, then both test suites.

`make fix` runs the same tools in write mode -- reformat, then apply the
suggestions each linter marks as safe to make unattended. What survives a
`fix` is the list that needs a person: clippy will not, for instance, narrow
a `&Vec<String>` parameter to `&[String]` on its own, because that changes a
signature its callers depend on.

`scripts/test.sh` runs the suite inside a systemd scope with a hard memory
ceiling and swap disabled, so a runaway allocation is killed by the cgroup in
seconds rather than dragging the machine into swap thrash. It is not
ceremony — it is how a real infinite-loop-with-allocation bug in the Markdown
parser was caught. Use it.

### Work on the interface without Rust

```sh
make ui
```

The interface detects the absence of a Tauri host and falls back to a complete
in-memory backend with sample content. Lock screen, journals, editor, search
and media all work in a plain browser. The demo password is `everyday`.

## The CLI

`everyday` drives the same core, which makes it useful for scripted capture
and for verifying the stack where a GUI cannot be built.

```sh
cargo run -p everyday-cli -- init --name "My Journal"
echo "It rained all afternoon." | everyday new --journal Daily --tag weather
everyday list
everyday search rain
everyday export ~/journal-backup   # readable Markdown, one file per entry
everyday backup ~/vault-copy       # the vault itself, still sealed
everyday check                     # look for storage-level damage
```

`--help` on any subcommand. Password comes from a prompt, or `EVERYDAY_PASSWORD`
for scripts.

`backup` and `export` are different things and you probably want both.
`export` writes readable Markdown that any program can open, which is what
you want in ten years when this app is gone. `backup` copies the vault as it
is — sealed, with its attachments and its header — so it opens with the same
password and needs no restore step; that is what you want at 2am when the
disk has gone bad. `check` exits non-zero on damage, so it fits in a cron
line.

While the app has a vault open, the CLI opens it read-only: `list`, `show`,
`search`, `export`, `check` and `backup` work, and anything that writes says
which process is holding it. Close the app, or point `--vault` somewhere else.

`gc` deletes attachments no entry references, but only ones written over a
day ago. An attachment is unreferenced from the moment it is stored until
the entry embedding it is saved, so a young orphan may simply be an image
pasted into a draft in another window. `--include-recent` drops the grace
period if you know there is no such draft.

## Keyboard

| | |
|---|---|
| `Ctrl/Cmd J` | cycle Journal → Todo → Calendar |
| `Ctrl/Cmd N` | new entry, the task capture line, or an hour set aside |
| `Ctrl/Cmd F` | search |
| `Ctrl/Cmd L` | lock now |
| `Ctrl/Cmd S` | flush pending edits (it autosaves anyway) |

In the calendar: `D`, `W`, `M` for the three views, `T` for today, `←`/`→` to
page, `Delete` to remove the selected block.

## Status

The core, both storage backends, the vault lifecycle, search, the media
pipeline, the todo app, the calendar and the CLI are implemented and tested —
278 tests, plus three shared backend conformance suites and two dependency-free
interface suites (the quick-add grammar and the calendar's grid arithmetic).
The desktop shell and interface are complete and the interface builds and
typechecks clean.

The CLI covers journals only. It is a capture-and-export tool for the journal
and has not been taught about tasks or calendars.

Calendar subscriptions are the one part not exercised against a real server
here, for the obvious reason: the iCalendar reader, the recurrence expansion
and the storage are covered offline by fixtures, and what is left untested is
the HTTP request itself. Point it at a real Google or Outlook feed to finish
the job. The interface's own mock backend (`make ui`) ships two sample
calendars, one of them deliberately in a failed state, so both paths through
the "add a calendar" sheet can be seen without a server.

Not yet built: sync between machines, mobile shells, a map view, task
recurrence, writing back to a subscribed calendar (see above for why not),
and importers for Day One's export format.

## The icon on Linux

If the window shows a generic icon instead of the application's, the app is
almost certainly running uninstalled -- straight out of `target/`, which is
what `make run` does.

On X11 a window carries its own icon and Tauri sets it from the bundled PNGs.
Wayland has no equivalent: the compositor is handed an *application id*, and
GNOME resolves that to an icon by finding the `.desktop` file that claims it.
A binary that was never installed has no `.desktop` file, so there is nothing
to resolve and the window falls back to the generic icon. Nothing the process
does at runtime can change that.

Installing the package fixes it:

    make build
    sudo dpkg -i "target/release/bundle/deb/Every Day_0.1.0_amd64.deb"

To keep a locally built binary and still get the icon and the right name in
the dock and the overview:

    make desktop-entry      # writes ~/.local/share/applications/everyday-app.desktop
    make undesktop-entry    # to undo it

The entry it writes points `Exec` at the binary in `target/`, so re-run it
after a `make clean`, and prefer the package for anything but development.

Either way you may need to log out and back in before GNOME Shell notices the
new entry.

## Licence

[MIT](LICENSE). Do what you like with it; keep the copyright notice.

MIT rather than Apache-2.0 because it asks less of you: Apache additionally
requires that modified files be marked as changed and that a NOTICE file be
carried along, and it terminates on patent litigation. The one thing MIT does
not give you is Apache's express patent grant.

The bundled typefaces are third-party and keep their own licences, both the
SIL Open Font License 1.1: Inter (© The Inter Project Authors) and Source
Sans 3 (© Adobe). The interface icons follow the geometry conventions of
[Lucide](https://lucide.dev), which is ISC licensed. Full texts are in
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
