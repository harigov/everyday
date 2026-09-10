# The assistant as a resident: notes, routines, and a sixth and seventh app

> **Delivered.** All nine phases are built, tested and on this branch. Read
> this for the reasoning; read the commits for what was actually done. Six
> things went differently from the plan below, each for a reason worth
> keeping:
>
> - **`web_search` is not a core tool.** The plan gave it a `Domain::Web` in
>   `everyday-core/src/agent/tools.rs`, which cannot work: that crate has no
>   async runtime, no TLS stack and no way to open a socket, and the calendar
>   and library features are both built to keep that true. It is declared in
>   `everyday-service`, beside the crate that does have those things, and
>   registered onto the agent alongside the core catalogue. It is therefore
>   absent from `list_tools`, which is honest — it is not a verb the CLI or
>   the palette can run.
> - **The search index took notes, and `SearchHit` became a tagged union.**
>   The plan said "grows a second input and a result carries which kind it
>   is" and understated it: an entry hit carries a journal and a day and a
>   note hit carries neither, so the fields hang off the variant, the way
>   `Purpose` does. The journal's own box narrows to entries, and the
>   narrowing happens in the store rather than as a cast at the drawing end.
> - **`Editor.svelte` became two components rather than growing props.** The
>   canvas — toolbar, document, media, both load-bearing effects — is
>   `RichText`, and the journal and the notes app each wrap it with their own
>   page. Lifting it was the only way to keep one set of caret bugs.
> - **The autounlock keychain helper lives in `everyday-vault`, not the
>   shell.** Two front ends want it: the desktop switch and
>   `everyday serve --keychain`. The vault crate is the assembly point and
>   already owns the platform paths.
> - **`RoutineRun` carries `routine_name` and `slot`.** Neither was in the
>   plan's shape. The name so a log row still reads after a rename or a
>   delete; the slot so two ticks cannot both claim one moment, which is also
>   what makes "run once per meeting" expressible for the query triggers.
> - **A skipped run is born `seen`.** There is nothing to look at, so it must
>   not put a number on the app bar. The plan did not say, and the first
>   version of the conformance suite caught it.
>
> One bug the suite found on the way through is worth recording: `seen` lives
> in a clear column *and* inside the sealed payload, and marking every run
> seen with one `UPDATE` moved the column and left the record saying
> otherwise — so the next client to read it would have put the number
> straight back on the app bar. It is a read-modify-write now.
>
> Everything else — the two locks, the profile, the unattended refusal, the
> serial scheduler, the templates, the transcript in place of a review queue
> — is as written below.

A plan for turning the assistant from a rail beside whichever app is open
into something that lives in the service, does work on a schedule while
nobody is watching, and has a place of its own to show what it did — and
for a Notes app, because a report the assistant writes has to land
somewhere and a journal is not it. Written 9 September 2026 against the
tree at `16234c9`, on `brainstorm-proactive-assistant-memory-tasks`. The
first section records what was decided in conversation; the phases after
it are the proposed order of work and are the part to argue with.

## What this is

Four things, in dependency order. Each is a small change to a layer that
already exists; none is a new subsystem beside the others.

1. **Two locks instead of one.** Today "lock" zeroizes the data key,
   closes the store and frees the index — for the window that pressed the
   button and for every client looking at the same vault. An assistant that
   works while the vault is locked is a contradiction under that definition.
   So the lock splits: a *screen lock*, which is a client hiding what it
   shows and asking for the password to show it again, and a *vault lock*,
   which is what lock means today. The screen lock is what the button, the
   shortcut and the idle timer do. The vault lock is explicit and rare.
2. **Notes.** A sixth app: a title, a rich text body in the editor the
   journal already has, tags, and nothing else. Not filed under a day, not
   in a journal. A note is what a report is, what a plan is, what "look
   into this" is. The assistant reads and writes them as Markdown through
   the conversion the journal tools already use.
3. **Routines.** A record in the vault: a trigger, an instruction in the
   person's words, and a switch. A scheduler in `everyday-service` runs
   them, one at a time, through the same turn the rail uses, as a caller of
   its own, with the whole tool catalogue. Every run is a conversation, so
   "why did it do that" has the same answer it has for a chat.
4. **A seventh app.** *Assistant*, on the bar, gated on the `agent`
   capability. Three panes: what it did, the routines, and what it
   remembers. The rail stays exactly where it is, and works inside this app
   with the app as its context.

And two things that fall out of the third and are worth naming:

5. **What it knows.** The assistant is told the date today and nothing
   else about the world. It should know the time and the zone, and it
   should know who it is talking to: a name, a date of birth, a gender, a
   place, and a paragraph in the person's own words about their work,
   their family and what they care about. A *profile*, sealed like
   everything else, edited by hand in Settings, and read into every turn.
6. **A model that can look things up.** There is no web search tool in the
   catalogue; the assistant cannot, today, find out who is coming to a
   meeting. The `web` command domain exists and the service already has the
   only network client in the application. A `web_search` tool behind one
   switch is what the secretary's first useful job needs.

## Decisions already made

These were settled in discussion and the plan does not reopen them.

- **The service is always running.** A daemon or server mode keeps the
  vault open for any client; the desktop app is the common host of it and
  `everyday serve` is the headless one. Nothing here relies on a window
  being open, and nothing here builds a second process.
- **The assistant acts while the app is locked.** "Locked" therefore has to
  mean the screen, not the key. A vault whose key is gone cannot run
  anything, and the plan says so where it matters rather than pretending
  otherwise: the one gap left is between a service restart and the first
  password, and the last phase offers an opt-in to close it.
- **The assistant lives in the service, not in a window.** It is a fourth
  kind of caller, beside the local window, the socket peer and the paired
  device. Model credentials never leave the machine holding the vault,
  which is already true.
- **The assistant makes only what already exists.** A weekend plan is
  planned time blocks; something to read is an item on a shelf; a person to
  call is a task; a report is a note. There is no assistant-only record and
  no assistant-only view of records. The Assistant app is a view over runs,
  routines and memories, and the things a run made are found where they
  always are.
- **Nothing is restricted and nothing is reviewed, yet.** No leash on the
  tool set, no draft state on a record, no provenance table, no queue of
  things to accept. A run gets the catalogue the rail gets and its writes
  are writes. What a run did is in its transcript, one click away. If that
  turns out to be too much trust, the places to add a leash and a review
  state are noted at the end, and both are additive.
- **Routines, not scheduled tasks.** A task is a thing in the todo app.
- **The assistant makes routines too.** "Every Sunday evening, plan my
  week" typed into the rail is a routine, through tools that ship with the
  scheduler rather than after it. The confirmation is seeing it in the
  list.
- **The profile is edited by hand and read by the assistant.** Facts that
  change — a move, a new job — are what memory is for; the profile is the
  handful of things that do not. There is no tool that writes it.
- **Memory stays small.** The existing sixty-four sentences, pinned or not,
  are the whole of it. The vault — roles, goals, notes, the journal — is the
  real memory, and the assistant reaches it with tools. Memory gets a page,
  not a second store.
- **Unattended means no questions.** A run cannot ask anyone anything. The
  rule `run_tool` already states — *a caller that cannot ask a person has
  to say so explicitly* — is the rule for the scheduler: a run inherits
  `confirm_destructive`, and when it is on, a destructive call in a run is
  declined on the spot with the words the gate already uses for an
  unanswered question. When it is off, it runs. Nothing hangs.
- **One notification per run, safe to show on a lock screen.** The title
  names the routine — "Morning brief is ready" — and never carries content.
- **No notification queue.** An event with nobody listening is dropped, as
  it is today. The run is the durable record; a client that connects reads
  the count of runs nobody has seen and says so once.
- **Time is the person's, not the host's.** A service in a container has the
  wrong clock zone. The assistant settings gain a time zone; "7am" means
  that.

## The data model

### New records

```
Note       { id, title, body: RichDoc, tags: Vec<String>, pinned: bool,
             purpose: Option<Purpose>, created_at, updated_at }

Profile    { first_name, last_name, born: Option<Date>, gender: String,
             location: String, about: String, updated_at }
             -- one per vault, like AgentSettings; every field may be empty

Routine    { id, name, instructions, trigger: Trigger, grace_minutes,
             enabled, last_run_at: Option<Timestamp>, created_at, updated_at }
             trigger ∈ Schedule { at: Time, days: Vec<Weekday> }   -- "7:00, weekdays"
                     | BeforeEvent { lead_minutes, role_id: Option<RoleId> }   -- phase 7
                     | TaskDue { lead_days }                       -- phase 7
                     | Manual                                      -- run-now only

RoutineRun { id, routine_id, started_at, finished_at: Option<Timestamp>,
             outcome: Outcome,               -- ∈ { running, done, failed, skipped }
             reason: String,                 -- why it failed or was skipped
             subject: Option<String>,        -- the event or task it was about
             conversation_id: Option<ConversationId>,
             summary: String,                -- the model's last message
             seen: bool, steps: u32 }
```

`Trigger` and `Outcome` are not records and have no tables. `Trigger` is
the type of one field on `Routine`: an enum, because a clock time with
weekdays and a lead time before a meeting have different fields and a
routine has exactly one of them — the reason `Purpose` is
`Goal { id } | Role { id }` and `BlockSubject` is what it is. `Outcome` is
the type of one field on `RoutineRun`: a closed set like `TaskStatus` and
`GoalStatus`, so a run cannot be in a state the interface has no word for.
Both live inside the sealed payload.

`Profile` is a singleton, stored the way `AgentSettings` is — one sealed
row, nothing in the clear, on the base `JournalStore` rather than an
optional domain, because every vault has an owner. It is not
`AgentSettings` because it is about the person and not the model, and it
will be read by more than the assistant: a birthday is a calendar's
business and a location is the weather's, when either exists.

A `Note` is an `Entry` without a journal or a date. It carries the same
`RichDoc`, so the editor, the plain-text projection for search, the blob
references for garbage collection, and `from_markdown`/`to_markdown` for
the assistant are all already written.

### What the assistant is told

`system_prompt` today says which day it is and nothing else about the
world. It gains, in this order after the person's own instructions: the
profile as a paragraph — *You are talking to Hari Govardhanam, 41, male,
in Seattle. In their words: …* — with the age computed from the date of
birth rather than left for the model to get wrong; then *It is Tuesday
9 September 2026, 14:05, in America/Los_Angeles*, the clock and the zone
passed in as one `Zoned` rather than read, for the reason `today` already
is; then the memories; then what the person is looking at. `ToolContext`
takes its `today` and `tz` from the same zone, so "due this week" in a
tool and "this week" in the prompt agree.

`Conversation` gains `#[serde(default)] run_id: Option<RoutineRunId>` inside
its sealed payload. `list_conversations` already decrypts every row for its
title, so keeping runs out of the rail's history is a match in Rust, not a
column. `AgentSettings` gains `timezone: Option<String>` (the system zone
when unset) and `web: bool` (off by default; the sentence beside it says
what a search sends).

### Ids

`typed_id!(NoteId, "note")`, `typed_id!(RoutineId, "routine")` and
`typed_id!(RoutineRunId, "run")`, as every other id. The profile has none.

### The caller

`Caller::Assistant(RoutineRunId)`, origin `"assistant"`, scopes `All`. The
variant is not cosmetic: a run that called itself `Local` would have its
change events suppressed by the desktop window, which drops its own origin
so a save never reloads the list it was made in. It is also what lets the
service tell a run's storage access from a person's, which the forget-key
timer needs.

### Schema, version 8

```sql
CREATE TABLE IF NOT EXISTS notes (
    id          TEXT PRIMARY KEY NOT NULL,
    pinned      {boolean} NOT NULL DEFAULT {f},
    created_us  {int} NOT NULL,
    updated_us  {int} NOT NULL,
    data        {blob} NOT NULL
)
CREATE INDEX IF NOT EXISTS notes_by_updated ON notes (pinned, updated_us)

CREATE TABLE IF NOT EXISTS profile (
    id          {int} PRIMARY KEY NOT NULL CHECK (id = 1),
    data        {blob} NOT NULL
)

CREATE TABLE IF NOT EXISTS routines (
    id          TEXT PRIMARY KEY NOT NULL,
    created_us  {int} NOT NULL,
    updated_us  {int} NOT NULL,
    data        {blob} NOT NULL
)

CREATE TABLE IF NOT EXISTS routine_runs (
    id           TEXT PRIMARY KEY NOT NULL,
    routine_id   TEXT NOT NULL,
    started_us   {int} NOT NULL,
    seen         {boolean} NOT NULL DEFAULT {f},
    data         {blob} NOT NULL
)
CREATE INDEX IF NOT EXISTS runs_by_routine ON routine_runs (routine_id, started_us)
CREATE INDEX IF NOT EXISTS runs_unseen ON routine_runs (seen, started_us)
```

Additive and `IF NOT EXISTS`, like everything since v4, so the step
replays. No `ALTER TABLE`, which is why `run_id` on a conversation lives in
its payload. No foreign keys, for the reason `goals` has none. A note's
purpose goes in the `purposes` side table under a new `RecordKind::Note`,
like every other record's.

What the file says, in the clear: that a note was pinned and last touched
on Tuesday, that a routine exists, that a run started at 07:00 and nobody
has looked at it. What it never says: a note's title or a word of its body,
what a routine is for, what a run reported, or anything at all about the
person — the profile row is sealed whole, like a message. The leak table in
`everyday-store-sql/src/lib.rs` gains four rows saying exactly this.

Retention: `collect_garbage` keeps the last fifty runs per routine, deletes
the conversations of the runs it drops, and walks notes for blob references
as it walks entries.

### The trigger arithmetic

`next_due(trigger, last_run_at, now, tz) -> Option<Timestamp>` and
`is_due(…, grace_minutes) -> Due::Now | Due::Missed | Due::Later` live in
`everyday-core/src/routine.rs`, take the clock as an argument, and are
tested on a table of cases: a weekday schedule across a weekend, a run that
straddles a DST change, a routine that has never run, a routine missed by
more than its grace. `Missed` writes a `Skipped` run with the reason, so a
laptop that was shut for a week shows one honest row per morning rather than
seven briefs at once — or none at all, silently.

## What "locked" becomes

Today, on the one shared `Arc<Vault>`, `Vault::lock` takes the `Unlocked`
state and drops it; the key zeroizes, the index frees, every client gets
`lockState(true)` and leaves the screen it is on. `auto_lock_if_idle` is a
pull the local window makes every five seconds; a headless server with no
client attached never locks at all.

After phase 1:

| | Screen lock | Vault lock |
|---|---|---|
| What it does | This client clears its stores and shows the lock screen | Key zeroized, store closed, every client shown the lock screen |
| Who | Each client, for itself | Anybody, for everybody |
| Triggered by | `Ctrl/Cmd L`, the Lock button, the idle timer | "Lock the vault everywhere" in the palette and the tray; quit; the *forget-key* timer |
| Unlock | `verify_password` — same Argon2 cost, same rate limit, nothing reopened | `unlock`, as today |
| The assistant | keeps working | stops; runs due meanwhile are `Skipped` or caught up per their grace |

`auto_lock_seconds` in the header keeps its name and becomes the screen
timeout, which every client reads from `status` and enforces on its own
clock. A new header field, `forget_key_seconds`, is the vault-level timer,
default `0` — never, while the process runs — and the Settings → Vault pane
says so in one sentence beside it: *the key stays in memory on the machine
holding the vault until it quits or you lock the vault everywhere.* That is
the promise the daemon already makes and the README's encryption section
already states; the change is that it now also holds for a window that has
been left alone for fifteen minutes.

Two consequences to build rather than discover:

- **`touch` leaves `Vault::read` and `Vault::write`.** Today any storage
  access defers the idle timer, which would mean a routine that runs every
  morning keeps the key alive forever. The service calls `vault.touch()`
  after a command from any caller *except* `Caller::Assistant`. A vault
  being used from a phone is still a vault being used.
- **The scheduler ticks the forget-key timer.** So a headless server
  honours it with no client polling. This fixes an existing gap.

## Phases

Each phase leaves `make test` and `make lint` green and the desktop app
usable. Phases 1, 2, 3 and 4 are independent; 5 needs 1, 3 and 4; 6 needs
5; 7 needs 6; 8 stands alone.

### Phase 1 — Two locks

**Core.** `Vault::verify_password(&str) -> Result<()>` — `data_key()` and
discard, so the AEAD tag on the wrapped key is still the only verifier.
`VaultHeader.forget_key_seconds`, default 0; `Vault::forget_key_if_idle()`
beside `auto_lock_if_idle()`. `touch()` out of `read`/`write`.

**Service.** `verify_password` command, `scope: Journals`, `effect: Write`
(it burns Argon2), in the `INVISIBLE` list, routed through the server's
`unlock_gate` exactly as `unlock` is — a screen unlock is an attempt like
any other. `set_forget_key` beside `set_auto_lock`. `Command::invoke` calls
`touch` for non-assistant callers. `poll_auto_lock` polls the forget-key
timer and keeps its name on the wire; nothing removed, no `PROTOCOL` bump.

**Interface.** `app.lockScreen()` runs the reset hooks and sets
`screen = 'locked'` without calling `api.lock()`; the idle timer moves into
the client (`auto_lock_seconds` from `status`, reset on the interactions
that call `touch()` today). The lock screen calls `verify_password` when
`status.locked` is false and `unlock` when it is true, so a vault that was
locked everywhere and a screen that was locked here are the same screen
with a different first call. `Ctrl/Cmd L` is the screen. "Lock the vault
everywhere" is a palette row and a tray row under Vault. The `lockState`
event still means the vault and still throws every client to the lock
screen. The 5-second `pollAutoLock` interval, which today runs over TLS on
a remote session, becomes a local timer — an existing discrepancy fixed in
passing.

**Tests.** `verify_password` accepts the right password and refuses the
wrong one without changing lock state; a wrong screen unlock counts against
the same lockout as a wrong unlock; a routine's storage access does not
defer the forget-key timer and a window's does; `forget_key_seconds = 0`
never locks.

**Done when** a window can be locked and unlocked with the service never
losing its key, and "Lock the vault everywhere" still does what `lock` did.

### Phase 2 — Notes

**Core.** `note.rs` with `Note`, `Note::new(title)`, `searchable_text()`,
`NoteQuery { tags, pinned, limit, offset }` with `matches`/`apply` in the
shape of `EntryQuery`, and `note_aad`. `store/notes.rs` with `NoteStore`
(`list_notes(&NoteQuery)` returning summaries — title, tags, pinned, the
first line, timestamps — `get_note`, `put_note`, `delete_note`,
`all_notes` for the search index and export). `JournalStore::notes()`
accessor; `Capabilities.notes`; `Purpose` on `Note`; `RecordKind::Note`.
The search index takes notes beside entries — `SearchIndex::build` grows a
second input and a result carries which kind it is — so `/` in the Notes
app is the same search as the journal's.

**SQL.** `schema.rs` v8 as above, all four tables in the one step;
`SCHEMA_VERSION = 8`; `notes.rs`; `set_purpose`/`forget_purposes` inside a
note's own transaction; `collect_garbage` walks `all_notes()`. Leak table
rows.

**Vault.** `supports_notes`, `save_note` with the same conditional write
`save_entry` has (`expect: Option<Timestamp>`, so two windows editing one
note meet the conflict dialog rather than a lost write), `delete_note`.

**Tools.** `Domain::Notes`, `Ordinary`, with its reason in the
`sensitivity` match. `list_notes`, `get_note` (body as Markdown),
`create_note` (title and Markdown body), `update_note`, `delete_note`
(Destructive, and the `delete_` naming test covers it). The projection
follows `entry_json`: id, title, tags, first line, updated — never the body
in a list.

**Commands.** `domains/notes.rs`, `Scope::Notes`, `Kind::Note`:
`list_notes`, `get_note`, `new_note`, `save_note`, `delete_note`. One line
in `domains/mod.rs`; `make fix` regenerates the surface and the client.

**The app.** The Overview's recipe: `SECTIONS` and `APPS` gain `notes`
between the journal and the todo app; `supportsNotes`; `NotesView` and
`NotesNav`; a `notes.svelte.ts` store registering `app.onLock`;
`TRAY_GROUPS` gains `Notes`; `ACTIONS` gains `g n`, *New note* in the tray
and the palette, and `c` in this app makes a note; `RELOADS` gains `note`;
`ChatPanel`'s context switch gains an arm (and the `overview` arm it is
missing today); `mock.ts` cases. The list is pinned first, then by last
touched, with tag filters in the nav. Right-click on a note offers *File
under* from `menus.ts` like everything else.

**The editor.** `Editor.svelte` derives its document from `app.entry`
today. It takes the document and a save callback as props instead, and the
journal passes what it always did. The autosave and conflict machinery in
`autosave.ts` is already independent of what it is saving and is reused
whole. Photos and video drop into a note exactly as into an entry, because
the blob path is the editor's, not the journal's.

**Tests.** `run_note_suite` in the conformance suite with the six usual
shapes plus: a note's purpose round-trips and goes when the note goes;
`all_notes` returns bodies and `list_notes` does not; a note is found by
search. The vault-level tool chain: `create_note` then `get_note` gives the
Markdown back; `update_note` with a stale `expect` conflicts.

**Done when** a note written in the Notes app is readable by the assistant
as Markdown, a note the assistant writes opens in the editor, and both are
found by search.

### Phase 3 — The profile and the clock

**Core.** `profile.rs` with `Profile`, `Profile::age(today)`,
`Profile::validate` (a date of birth in the future is refused; nothing else
is), and `profile_aad`. `JournalStore::profile()` and `put_profile()` on
the base trait, never `Option` — an empty profile is a perfectly good
answer. `AgentSettings.timezone: Option<String>`, validated against the
tz database on save. `system_prompt` takes `&Profile` and a `Zoned` and
says what the section above says; `ToolContext` is built from the same
zone.

**SQL.** `profile.rs` over the v8 table.

**Commands.** `profile` and `save_profile`, `scope: Journals`,
`change: Settings / Updated`.

**Interface.** Settings gains a **Profile** tab between General and
Assistant: first name, last name, date of birth, gender as a free field,
location, and *About you* as a textarea with a sentence under it saying
the assistant reads all of this. The Assistant tab gains the time zone,
defaulting to the machine's and listing the tz database.

**Tests.** The prompt names the person and their age and the clock and the
zone; an empty profile adds nothing to the prompt; a leap-day birthday
ages on the first of March; `age` is right across a birthday.

**Done when** the assistant, asked "what time is it and who am I",
answers both from the prompt without a tool.

### Phase 4 — Routines, the domain only

**Core.** `routine.rs` with `Routine`, `Trigger`, `RoutineRun`, `Outcome`,
the trigger arithmetic and its tests. `store/routines.rs` with
`RoutineStore` (`list_routines`, `get_routine`, `put_routine`,
`delete_routine`; `list_runs(&RunQuery)`, `get_run`, `put_run`,
`delete_run`, `count_unseen`, `mark_seen`) and `routine_aad` / `run_aad`.
`JournalStore::routines()` accessor; `Capabilities.routines`.
`Conversation.run_id`; `ConversationQuery.chats_only`.

**SQL.** `routines.rs` implementing the trait over the tables v8 already
made.

**Vault.** `supports_routines`, `save_routine` (name and instructions
non-empty; a `BeforeEvent` trigger with a role that does not exist is
refused, the FK the schema lacks), `delete_routine` (takes its runs and
their conversations), `list_runs`, `mark_runs_seen`.

**Conformance.** `run_routine_suite` with the usual six shapes, plus:
deleting a routine takes its runs and their conversations; `chats_only`
hides a run's conversation; `count_unseen` counts what `mark_seen` clears.
Both backends inherit it.

**Done when** a routine and a run round-trip on SQLite and Postgres.

### Phase 5 — The scheduler, the unattended turn, and the routine tools

**The turn.** `Turn` gains `unattended: Option<RoutineRunId>`. When set:
the caller is `Caller::Assistant`, the tool set is `tools::available` as
for a chat, the prompt is the routine's instructions, and the context is a
fixed sentence — *this is a scheduled run of the routine "Morning brief";
nobody is watching and nobody can answer a question; if you cannot proceed,
say why and stop; if you have more than a paragraph to hand over, write a
note.* `ConfirmGate` is built with a `Pending` that answers every question
`false` at once, so the existing decline path runs and nothing waits. The
whole turn is wrapped in `tokio::time::timeout` (fifteen minutes, the
number the remote client already uses for a streamed turn); the rig client
does not share `http.rs`'s timeouts, which is why the wrapper is here.

**The runner.** `scheduler.rs` in the service: `pub async fn run(service:
Arc<Service>, stop: watch::Receiver<bool>)`. A sixty-second tick. On each:
if there is no vault, or it is locked, or it is not writable, do nothing;
else tick the forget-key timer, load routines, evaluate `is_due` against
the assistant time zone, write `Skipped` rows for the missed, and run the
due ones **one at a time** in `last_run_at` order. Each run:
`agent_credentials()` (a disabled assistant is a `Skipped` with that
reason), a `Running` row, `run_turn`, then the row rewritten `Done` or
`Failed` with the summary or the error, `last_run_at` stamped, a
`Change { kind: RoutineRun }`, and — for `Done` — one
`Notification::for_user` titled with the routine's name and keyed by
routine, so a routine that fails every morning says so once.
`feed_failed`/`feed_recovered` is the precedent and the same `reported_*`
set is used.

**Where it is spawned.** On the runtime the process already has, never a
new one — the rule `everyday-server/src/lib.rs` states for the server. The
desktop shell spawns it in `setup` beside the sink; `everyday serve` spawns
it after `start`; a remote-client window does not spawn it, because the
machine holding the vault already has. `Service::close` stops it.

**The window.** `CloseRequested` hides the window instead of destroying it
when any routine is enabled or sharing is on; "Quit Every Day" in the tray
still quits, and quitting still locks. Hiding keeps the webview, which is
what lets the existing notification path post to the notification centre
with the window out of the way. The tray gains one line under the actions,
shell-owned like Open and Quit: *Assistant: idle* / *running "Morning
brief"* / *off*.

**Tools**, `Domain::Agent`, in the catalogue for chat and for runs
alike: `list_routines`, `create_routine` (name, instructions, a time and
weekdays, enabled), `update_routine`, `delete_routine` (Destructive, so it
meets the gate), `run_routine`, `list_runs`. Trigger fields arrive as
`"07:00"` and `["mon","tue"]` and are parsed with the same errors the
palette will use. With these, "every Sunday evening, plan my week" typed
into the rail is a routine, and "what did you do this morning" is answered
from `list_runs`.

**Commands**, all `scope: Agent`: `list_routines`, `save_routine`
(`change: Routine / Updated`), `delete_routine` (Destructive),
`run_routine` (queues a `Manual` run for the next tick and returns the run
id), `list_runs`, `get_run`, `mark_runs_seen`, `unseen_runs`,
`routine_templates`. `Kind::Routine` and `Kind::RoutineRun`; `meta.rs`'s
exhaustive match gains two arms. `make fix`; no `PROTOCOL` bump.

**Tests.** The service suite gains a fake model: a `TcpListener` on
loopback answering the Chat Completions shape with a scripted reply, and a
vault whose assistant points at it. With that: a due routine runs and
leaves a `Done` row with the reply as its summary; a scripted destructive
call is declined and the run still finishes `Done` with the decline in its
transcript; a locked vault runs nothing and a missed routine is `Skipped`
with a reason; the change events from a run reach a window sink with
origin `assistant`; a hung model produces `Failed` at the timeout, not
never. The trigger arithmetic is already covered in core.

**Done when** a routine created from the rail runs on time with no window
open, and the note it wrote appears in the Notes app of a connected
client.

### Phase 6 — The Assistant app

**The recipe** is the Notes app's, one phase earlier: `SECTIONS` and
`APPS` gain `assistant` last; `supportsAssistant` on `capabilities.agent`;
the view and nav branches; the accent; a store registering `app.onLock`;
`TRAY_GROUPS` gains `Assistant`; `ACTIONS` gains `g a`, the tray rows and
palette rows; `c` here makes a routine; `RELOADS` gains `routine` and
`routineRun` and flips `memory` and `conversation` off `null`;
`ChatPanel`'s context arm; `mock.ts` cases; a `scripts/assistant.test.mjs`
for any pure module.

**Panes**, in the sidebar like the Overview's:

- **Runs.** Newest first, unseen ones marked. A row is the routine's name,
  when it ran, the outcome, and the summary as Markdown through the
  existing renderer. Opening a run opens its conversation in the rail, so
  what it did — every tool call, every note it wrote — is the transcript,
  and "why did you suggest this" is a question the assistant can answer.
  Opening the pane marks what is on screen seen.
- **Routines.** The list with name, trigger in words ("Weekdays at 7:00"),
  last run and its outcome, next due. An editor: name, instructions,
  trigger (time and days first; the rest in phase 7), grace, enabled. *Run
  now* and a run log per routine. Templates from `routine_templates` as the
  empty state and behind the plus: *Morning brief*, *Weekly review*,
  *Weekend planner*, *Something to read*, and *Meeting prep* greyed until
  phase 7. Templates are offered, never imposed, and a template is only a
  filled-in editor.
- **Memory.** The list that lives in Settings → Assistant today, moved.
  Add, edit, pin and unpin (`save_memory` stops forcing `pinned: true`; a
  page-authored memory defaults to pinned), and *from this conversation*
  opening the source thread in the rail — including a run's. The settings
  tab keeps model, key, instructions and behaviour, and loses the list.

**The bar.** The Assistant icon carries the unseen count as a small
numeral; `unseen_runs` is one indexed query and reloads on `routineRun`.
That count, and a single toast on connect when it is non-zero, is how
anyone finds out the assistant did something. Nothing else interrupts.

**Done when** a run appears on every connected client, opening it on one
clears the count on the others, and a routine can be made from a template
without touching Settings.

### Phase 7 — Triggers beyond the clock, and the web

**Attendees.** `ics.rs` reads `ATTENDEE` into `Event.attendees:
Vec<String>` (sealed; name or address, as `organizer` is). One fixture with
three attendees and one with none; the calendar conformance suite round-trips
the field.

**Triggers.** `BeforeEvent { lead_minutes, role_id }` is evaluated on the
tick as a query, not a subscription: events starting within the lead
window, on a calendar with that role (or any, when unset), whose id is not
already a run's `subject`. `TaskDue { lead_days }` the same over tasks. The
prompt for such a run carries the subject — the event's title, time,
organizer and attendees — after the routine's instructions. Polling on the
minute is what makes a missed window an honest `Skipped` rather than a
subscription that never fired.

**The web.** A `web_search` tool in a new `Domain::Web`, `Effect::Read`,
calling the same `websearch::search` the library uses, results projected to
title, address and snippet. `Domain::sensitivity` will not compile until it
has an arm, which is the point: it is `Ordinary`, and the reason is written
there — a search result is a prompt-injection path, and the answer to that
is the transcript, not secrecy. Offered, to chat and to runs alike, only
when `AgentSettings.web` is on; the switch carries the sentence: *this
sends the names of people and the words of your question to a search
engine.* The `web` commands keep `require_unlocked`, which under two locks
means what it should: the key, not the screen.

**Tests.** The tick queries for both triggers, in core against a seeded
vault; a meeting inside the lead window runs once and not twice; the
`web_search` tool is absent from the catalogue when the switch is off.

**Done when** *Meeting prep* runs an hour before a subscribed event with
attendees, looks them up, and leaves a note.

### Phase 8 — Surviving a restart

Opt-in, per machine, off by default, in Settings → Vault beside *Share on
the network*: *Open this vault without a password when the app starts.*
When on, the data key is wrapped under a random secret held in the
operating system's keychain — the `keyring` crate the device tokens already
use, and the same refusal when there is none: the switch is greyed rather
than the key written to a file. `Vault::unlock_with_wrapped` beside
`unlock`; `bootstrap` tries it before showing the lock screen. The sentence
beside the switch is the trade: *your vault is then as safe as your login
on this computer, not as safe as your password.* Turning it off removes the
wrapped key. `everyday serve` gets `--keychain` for the same thing on a
headless machine.

### Phase 9 — Words

- README: a section for notes and one for the assistant as a resident —
  the profile, routines, the transcript, two locks, the restart switch — in
  the voice of
  the existing ones; "Five apps, one vault" becomes seven and the bar
  paragraph gains a sentence; the shortcut table gains `G` then `N` and
  `G` then `A`; the *Lists notice writes they did not make* paragraph loses
  "locks for everybody looking at it" and gains the two-lock sentence; the
  *What is not encrypted* section gains its four rows; the Status
  section's counts.
- `everyday-store-sql/src/lib.rs` leak table and the `schema.rs` v8
  rationale comment.
- `docs/PLAN-server-and-actions.md`'s *Still to do*: the local socket in
  the desktop app is still not wired, and this plan does not wire it; say
  so beside the browser-extension line.

## What is deliberately out, and where it would go

- **A leash on a run's tools.** If it is ever wanted, it is a field on
  `Routine` and a filter on `tools::available` in one place in `run_turn`.
  Nothing in the schema changes.
- **A draft state and a review queue.** If it is ever wanted, it is a side
  table like `purposes` — record kind, record id, run id, seen — written by
  the `create_*` tools when `ToolContext` carries a run, and a pane that
  lists it. Nothing built here has to be undone for it.
- **A notification queue or push channel.** The run is the record; a phone
  shell can add push when it exists.
- **A second key, or a key scoped to the assistant.** Everything is sealed
  under one data key.
- **Locking on system sleep.** No portable signal; the forget-key timer
  covers the case for anyone who wants it.
- **The CLI.** `everyday routines` and `everyday note` need the socket
  client that `everyday new`-through-a-running-app also needs; neither
  exists and this plan does not build it.
- **Routines that run in parallel.** One at a time keeps cost and the single
  writer honest.
- **Event triggers as subscriptions.** The tick is the subscription.
- **Notes in folders, or a notebook record.** Tags and pins, like entries;
  a folder is a tag with an opinion about hierarchy.
- **Step-up authentication.** `save_agent_settings` and `set_agent_key` can
  be overwritten by any paired device and by anything on the socket; that
  is true today and is the passwords app's problem to solve. This plan marks
  both `sensitive: true` so the first step-up check catches them, and does
  no more.
- **A memory taxonomy.** Sentences, pinned or not.
- **A tool that writes the profile.** "I moved to Boston" is a memory;
  the profile is what does not change, and Settings is where it is typed.

## Risks and the answer to each

| Risk | Answer |
|---|---|
| Phase 1 weakens the at-rest posture of a window left alone | Only the *screen* timer changes meaning; the key was in memory for as long as the process ran under the daemon already. The forget-key timer and the Vault pane's sentence make the trade visible; quit still locks. |
| A run with the whole catalogue deletes something | `confirm_destructive` is on by default and a run cannot answer, so the call is declined and the decline is in the transcript. Somebody who turns it off has read the sentence beside it. |
| The scheduler starts a second runtime | It is `async fn run` on the host's runtime, as the server is; a test in the shell crate asserts one `Runtime` in the process. |
| The confirm gate parks an unattended turn forever | The run's `Pending` answers `false` immediately; the timeout is the backstop; a test hangs a fake model and expects `Failed`. |
| A run costs money every morning forever | `max_steps` from settings, one run at a time, `Skipped` rather than catch-up past the grace, and the run log makes spend visible. A cap per routine is one field if it turns out to be needed. |
| Prompt injection from a fetched page or a subscribed feed drives a run | No secret domain, declined deletes, a transcript that says what was done, and the web switch off unless the person turns it on. The leash and the review state are the next two answers, and both are additive. |
| `Caller::Local` origin suppresses the run's changes in the window | `Caller::Assistant`, origin `assistant`; a test checks a window sink receives it. |
| The trigger arithmetic across DST and weekends | Table-driven tests in core with the clock as an argument. |
| `Kind`, `RecordKind` and `Domain::sensitivity` exhaustive matches | They will not compile until every arm exists, which is what they are for. |
| Lifting `Editor.svelte` off `app.entry` breaks the journal | The journal is the first caller of the new props and the only behaviour change is where the document comes from; a manual walk of the journal is the proof, and `autosave.test.mjs` is untouched. |
| The search index doubles its inputs | `SearchIndex::build` takes two slices; the index is rebuilt on unlock as today and notes are a handful beside a journal. |

## Sizes

Rough, in working days, for one person who knows the tree.

| Phase | Days |
|---|---|
| 1 Two locks | 3 |
| 2 Notes | 4 |
| 3 The profile and the clock | 1 |
| 4 Routines, the domain | 2 |
| 5 Scheduler, unattended turn, routine tools | 5 |
| 6 The Assistant app | 4 |
| 7 Triggers and the web | 3 |
| 8 Surviving a restart | 2 |
| 9 Words | 1 |

## Open questions

- **Should a run be allowed to `remember`?** The plan says yes, with
  `source_id` pointing at the run so the Memory page can say where a fact
  came from. The alternative is that only a person's chat can teach it,
  which is safer and duller.
- **Close hides to the tray only when something is running.** The
  alternative is always, which is what macOS users expect and Linux and
  Windows users do not.
- **`forget_key_seconds` defaults to never.** The alternative is a long
  default, eight hours, which means a morning brief on a laptop that slept
  overnight is `Skipped` until the first password of the day.
- **Where Notes sits on the bar.** Between Journal and Todo, as written,
  because it is the other place words go; or last before Assistant, because
  it arrived last.
