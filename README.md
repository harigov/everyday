# Every Day

A private journal and a todo app for macOS, Linux and Windows. Rich text with
photos and video, projects and tasks on a list or a board, pluggable storage,
and encryption you actually hold the key to.

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

## Two apps, one vault

The sidebar switches between **Journal** and **Todo** (`Ctrl/Cmd J`). They
share a vault, a password and a lock; they share nothing else.

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
appointment), which is the shape a calendar needs. That is deliberate: the
calendar view is next, and it is a view over records that already exist
rather than a third storage layer.

## Layout

```
crates/
  everyday-core/            domain model, crypto, storage traits, search
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

If that trade is unacceptable, the storage abstraction is the answer: a
backend that seals the index columns too — at the cost of full scans — drops
in without the rest of the app noticing.

## Storage backends

Storage sits behind one trait, [`JournalStore`], so alternatives can be tried
without touching the app.

| Backend | Good for | Trade |
|---|---|---|
| `sqlite` | the default; large journals, fast queries, the todo app | opaque on disk |
| `markdown` | grep, git, editing in any editor | slower; readable only when unencrypted; no todo app |

The task domain is a *second* trait, `TaskStore`, reached through
`JournalStore::tasks()`, which returns `None` by default. Folding a kanban
board into the journal trait would oblige every backend to grow a todo
implementation it has no opinion about — a board is not a thing anyone wants
as a tree of files. So SQLite implements it, Markdown does not, and the
interface reads `capabilities.tasks` and hides the app rather than failing at
click time. The calendar will arrive through the same door.

The Markdown backend is a genuine two-way format. It writes a readable `.md`
with TOML frontmatter plus a sidecar `.json` holding the exact rich-text tree.
Edit an entry in vim and the app notices — the frontmatter records a hash of
the body as written, so a mismatch means a human has been at it, and your
Markdown wins over the sidecar.

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
make check                         # fmt, clippy, and the interface typecheck
```

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
everyday export ~/journal-backup
```

`--help` on any subcommand. Password comes from a prompt, or `EVERYDAY_PASSWORD`
for scripts.

## Keyboard

| | |
|---|---|
| `Ctrl/Cmd J` | switch between Journal and Todo |
| `Ctrl/Cmd N` | new entry, or the task capture line |
| `Ctrl/Cmd F` | search |
| `Ctrl/Cmd L` | lock now |
| `Ctrl/Cmd S` | flush pending edits (it autosaves anyway) |

## Status

The core, both storage backends, the vault lifecycle, search, the media
pipeline, the todo app and the CLI are implemented and tested — 221 tests,
plus two shared backend conformance suites. The desktop shell and interface
are complete and the interface builds and typechecks clean.

The CLI covers journals only. It is a capture-and-export tool for the journal
and has not been taught about tasks.

The one thing not verified end-to-end is the assembled desktop app, because
the machine this was built on has no WebKitGTK headers and no root to install
them. Run `./scripts/setup-linux.sh` then `./scripts/dev.sh`.

Not yet built: sync, mobile shells, a map view, task recurrence, and importers
for Day One's export format. The calendar view is next, and the records it
needs — `TimeBlock`, with its planned/actual split and its ad-hoc subject —
already exist and are already tested.

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
