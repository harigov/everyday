# Naming the record, without changing what anything does

> **Status: proposed.** Nothing here is built. This plan is the result of an
> architecture review and a critical second pass over it. The second pass
> found that most of the first review's *automation* would have broken
> deliberate behaviour. What is left is a smaller refactor whose only goal is
> less duplication, at **zero behaviour change**, with the safety net built
> before anything moves.
>
> **Delivery:** all phases land on one branch and one PR, as one commit per
> phase (or sub-phase). "Phases never share a PR" below is kept as
> "phases never share a commit", so each phase can still be reverted alone.

## The one rule

**Every phase is behaviour-preserving, and proves it.**

A phase may move, rename (in code only), merge or generate code. It may not
change:

- any byte written to a vault
- any string that is stored, exported or sent over the wire
- any command or tool name, argument, effect or scope
- the number, kind, ids or origin of change events
- when `updated_at` is stamped, and by whom
- what the assistant is allowed to do without asking

If a phase needs one of those to change, it has stopped being a refactor and
gets its own plan.

"Proves it" means the Phase 0 guardrails pass **unchanged** on the phase's
last commit. Any snapshot update (`UPDATE_SURFACE=1`, a golden file, a pinned
string) inside a refactor PR is a red flag and must be justified in the PR
description line by line.

## Out of scope

These were proposed in the first review and dropped after the second, each
for a reason that still holds:

| Dropped | Why |
|---|---|
| A generic `RecordStore` with `fn get<R: Record>` on the public trait | Generic methods cannot be called through `dyn`, and the store is `dyn JournalStore` in 212 places. |
| Generic `list<R>` | It would stop filters running in the database (tasks by project or status), and notes and entries read a separate sealed `summary`. |
| An automatic "touch `updated_at`" write step | Conflict detection relies on "every writer owns its own `updated_at`". A re-stamp makes every second autosave a false conflict. Imports and assistant conversations stamp deliberately. |
| Sending change events from the store | The store does not know the caller (`origin`). That breaks self-echo filtering, turns one reorder event into forty, floods SSE on import, and ends the static `CHANGE_KINDS` contract. |
| A generic search-index step | `save_note` reindexes through `reindex_note` so transcript words stay searchable. A generic step silently drops them. |
| Declarative cascades replacing SQL | Set-based deletes in one transaction would become per-row. Purpose deliberately *refuses* to cascade. |
| Capabilities derived from a domain registry | `MemStore` would claim domains it lacks. `Capabilities` also crosses the wire. |
| One `DomainModule` trait in `everyday-core` | Core cannot name the service's `Command`, and `transfer` sits between the two. It would create crate cycles. |
| Generated CRUD tools; merging tools with commands | Tools take partial updates, refuse unsafe edits, gate `Destructive`/`Outward` effects, and hide secret domains. Generated tools could skip confirmation. |
| Retiring `mock.ts` for a dev server | `MemStore` holds journals only, the browser has no HTTP transport, and four UI tests use the mock as their backend. |
| Renaming fields (`title`/`name`, `starred`/`pinned`/`favourite`, …) | The names are in sealed JSON, exported front matter, the wire and model prompts. The gain is cosmetic, and some differences are intentional. |

## Phase 0 — the regression net (no production code changes)

Everything later depends on this. Each item is a test that passes on today's
`main` and must keep passing, untouched, through every later phase.

**Already in place (keep, do not weaken):**

- `crates/everyday-service/tests/surface.rs`: the command surface
  (`surface.json`), argument spelling, and signature ↔ struct agreement.
- `crates/everyday-service/tests/mcp.rs`: the tool surface: names, effects,
  domains, scopes, sensitivity and parameters.
- The command test that every write has a `change:` or is in `INVISIBLE`.
- `crates/everyday-core/src/store/conformance/`: run against SQLite, plaintext
  SQLite, `MemStore` and, in CI only, Postgres.
- `ui/scripts/*.test.mjs`: the `conflict`, `autosave`, `live`, `live-apply`,
  `editor`, `library` and `assistant` suites in particular.
- The exhaustive `RELOADS: Record<ChangeKind, …>` in `ui/src/lib/live.svelte.ts`.

**To add:**

0.1 **Pinned persisted strings** (`everyday-core`, one test file). Assert
literally, one line per value, with no derivation from enum names:

- every `*_aad` function's output for a fixed id, all ~40 labels
  (`everyday.task.v1:<id>`, `everyday.run.v1:…`, `everyday.log.v1:…`,
  `everyday.kind.v1:…`, `everyday.op.v1:…`, `everyday.message.v1:…`,
  `everyday.mail_message.v1:…`, and the singleton labels)
- `store-sql`'s `RecordKind::as_str` values (the `purposes.record_kind` column)
- `record_secrets.owner_kind` values
- `Purpose` kind column strings (`purpose.rs` `kind_str`)
- every `typed_id!` `KIND` string

0.2 **Wire-name snapshot for events.** A JSON snapshot, same helper as
`surface.rs`, of:

- each `events::Kind` and `Op` serialised
- a sample `Change` with `id`, with `ids`, and with `origin`
- `Capabilities::default()` and a fully-true `Capabilities`
- `CommandError` codes (`codes::ALL`)

0.3 **A frozen old vault.** Build one encrypted and one plaintext SQLite vault
**with today's code**, containing at least one record of every kind: journal,
entry with a blob, note, project, task, subtask, block, calendar, event,
shelf, item, log, tracker, reading, role, goal, purpose pointers on each
purpose-bearing kind, routine, run, proposal, conversation, message, memory,
account, secret, mailbox, mail message, thread, draft, op, recording,
transcript, voiceprint and profile.

- Check it in under `crates/everyday-vault/tests/fixtures/` (password in the
  test).
- The test opens it and compares a canonical JSON dump of every record, read
  through the public `Vault` API, with a checked-in golden file.
- It then writes one record of each kind, reopens, and dumps again.

This catches any change in encryption labels, column names, the purpose side
table, serde shape or migrations. A later phase that changes the golden file
has changed persistence.

0.4 **The change events each command emits.** An integration test in
`everyday-service/tests/` with a recording `EventSink`. It runs a scripted
sequence covering every write command in `surface.json` (reusing the
`placeholder` argument builder where possible) and snapshots the ordered list
of `(command, kind, op, id?, ids.len, origin)`.

- The assistant path is covered too: run each write tool through `run_tool`,
  which exercises `mail_tool_change` and `agent.rs` `written()`.
- The background writers are covered too: one scheduler `tick` with a due
  routine and an expiring proposal.

This catches phase 7's risk directly.

0.5 **The assistant's safety properties, as explicit tests.** Some exist
already, spread across `crates/everyday-core/src/agent/tools/mod.rs`,
`crates/everyday-service/src/agent.rs` and
`crates/everyday-service/tests/mail_agent_tools.rs`. Inventory them first,
then add whatever is missing:

- `Destructive` tools ask when `confirm_destructive` is on
- `Outward` tools always ask, and an unattended routine refuses them
- `Secret` domains are hidden from MCP
- `update_note` / `update_entry` refuse to replace a body that has blobs in it
- every update tool is a partial patch: fields not named are untouched
- proposals carry `expected_updated_at` and conflict on a stale one

0.6 **Stamping behaviour.** Tests pinning who sets `updated_at`:

- `save_note(n, expect)` stores `n.updated_at` exactly as given
- an import keeps the source's `created`
- `Vault::save_message` sets the conversation's `updated_at` to
  `max(message.created_at, previous)`
- a second autosave with the version the first returned succeeds (the
  false-conflict case)

0.7 **The UI's chat context per app.** A small `ui/scripts` test that, for
each `Section`, sets the section and reads `ChatPanel`'s context string. Move
the `$derived` body into a plain function in `lib/` so it can be loaded
without a component.

- **Today this test fails for `mail`.** Mark that case as a known failure in
  this phase; phase 1 fixes it.

**Exit criteria:**

- `make test`, `npm --prefix ui run check`, and CI (including Postgres) are
  green with the new tests.
- The golden files and snapshots are committed in a PR that contains **no
  production code changes**.

## Phase 1 — the one real bug

- Add `case 'mail'` to the chat context: the open thread's subject if one is
  selected, otherwise the mailbox name.
- The only snapshot change allowed is phase 0.7's mail expectation.

## Phase 2 — capabilities from the accessors

- Give `JournalStore::capabilities()` a default body that fills the domain
  flags from `self.tasks().is_some()`, `self.notes().is_some()`, and so on.
  Backends keep overriding only the non-domain fields (`blobs`,
  `transactional`, `human_readable`, `max_blob_bytes`).
- Add a conformance check that each flag equals its accessor's `is_some()`,
  run for every backend.

**Guard:** 0.2's `Capabilities` snapshot is unchanged, and so is the output
of `capabilities()` on SQLite, Postgres and `MemStore`. `MemStore`'s all-false
answer is the case to watch.

## Phase 3 — the app registry in the UI

- Add `ui/src/lib/apps.ts`: `APPS: Record<Section, AppModule>` with `label`,
  `icon`, `accent`, `supported()`, `create`, `chatContext` and the order.
- Replace, one PR each or all at once, the lists in `SECTIONS`, `canShow`,
  `AppBar` `APPS`, `App.svelte` `ACCENTS`, `shortcuts` `CREATE` and the "Go to"
  rows, and the `ChatPanel` switch.
- **Leave alone:** the `{#if}` view chains in `App.svelte` and `Sidebar.svelte`
  can use a component map, but only if the journal's two-component layout
  stays byte-identical in the DOM.
- **Keep:** the tray's grouping by `label` must produce the same menu. Add a
  test that snapshots `tray.entriesFor` for every app before the change.

**Guard:** 0.7, `menu.test`, `keys.test`, `actions.test`, `svelte-check`, and a
headless screenshot of each app, before and after, in mock mode.

## Phase 4 — composable store helpers (UI)

- Pull the duplicated patterns into `ui/src/lib/store/` as **functions or
  small classes that stores compose**, not a base class:
  - `guardedRefresh`: `latest()` plus loading plus drop the missing selection
  - `optimisticPatch`: assign plus stamp `updatedAt` plus `saves.touch`
  - `optimisticRemove`: filter plus `forget` plus rollback via `refresh`
  - `patchOneFromChange`: the `#applyChanges` / `#patchOne` pair
- Migrate one store per PR, in order of test coverage: library, todo,
  calendar, notes, purpose, accounts, then mail last.
- Mail's debounced refresh and notes' `DocBinding` stay in their stores.
- Move the three hand-rolled `#generation` counters onto `latest()`.
- **Keep, until this phase is finished:** `optimisticPatch` must stamp
  `updatedAt` exactly where the store stamps it today. That value is the
  client's next conflict base.

**Guard:** `autosave`, `conflict`, `live`, `live-apply`, `library` and
`tasklist` tests unchanged. For each migrated store, add a test *before*
migrating it if its refresh, patch or delete path has none.

Moving the journal out of `AppState` is **deferred to its own plan**. The
flush order on lock and the keep-mine flow need more than a refactor's care.

## Phase 5 — a TypeScript drift check (no caller changes)

- Generate TS declarations from the Rust record types (`specta` or `ts-rs`,
  behind a dev-only feature) into a scratch file.
- A script compares field names and optionality against `types.ts` and fails
  on a difference that is not in an allowlist.
- `types.ts` stays the source the UI imports. This phase only adds a check.
- Replacing `types.ts` wholesale is a later decision, made once the allowlist
  shows how far apart the two are.
- **Verify** the chosen crate builds on `rust-version = 1.85` *and* on CI's
  stable.

**Guard:** no UI source changes, so nothing to regress. A failure is a real
drift worth fixing in its own PR.

## Phase 6 — the `Record` descriptor in core

- Move the *identity* half of `store-sql`'s `Record` into `everyday-core`: a
  `RecordDescriptor` trait with `const KIND: RecordKind`, `type Id`, `fn id()`,
  and `fn aad(id)`.
  - `aad` stays a **literal per record**, pointing at the existing `*_aad`
    function.
- One `RecordKind` enum in core, with explicit per-surface string methods:
  - `aad_word()`
  - `purpose_column()` (only for the eight that have one)
  - `secret_owner()`
  - each a hand-written `match` with literal strings
- `events::Kind`, `ProposalKind`, `AboutKind` and `tools::Domain` stay their
  own enums. Each gains a `From`/`TryFrom` to `RecordKind`, with a test that
  the mapping is total where it should be.
- `store-sql`'s `Record` keeps `columns()`, `purpose()` and the generic
  `get`/`upsert`/`delete_by_id`, now as an extension of the core descriptor.
  Nothing becomes generic on `dyn JournalStore`.
- Optional, low risk: an opt-in `Timestamped` trait with `touch()` that
  **callers** use. It replaces the hand-written `x.updated_at =
  Timestamp::now()` lines one to one, and adds no automatic stamping.
- Optional, low risk: an opt-in `Completable` trait whose default
  `set_status` is the body copied today in `Project`, `Task` and `Goal`.
  `Item` keeps its own.

**Guard:** 0.1 and 0.3 unchanged is the whole point of this phase. Also run
conformance on Postgres locally (`make test-postgres`) — it otherwise runs
only in CI.

## Phase 7 — one place for change events per command

- Add a `Touched` collector to `Vault` writes: a list of `(RecordKind, id)`
  appended inside `Vault::write`, and taken by the service after the
  command.
- `Command::invoke` keeps its declared `change:`. For now the collector is
  used only to **assert**, in debug builds and tests, that the declared
  change matches what was touched.
- Then move the 9 hand-written `events().changed(..)` calls in `domains/`, and
  the scheduler's, to one helper that emits from the collector **once per
  command or tick**, with the caller's origin, batching ids the way `Change`
  already does.
- `INVISIBLE` stays.

**Guard:** 0.4 unchanged: same count, order, kinds, ids and origin. Also the
UI's `live` test, and an SSE smoke test that an import still produces one
batched event per kind.

## Phase 8 — shared tool helpers (no generated tools)

- Extract the repeated *mechanics*:
  - `load_by_id::<Note>(args, "note_id")`
  - `delete_proposal(kind, id, caption)`
  - `done(...)`
  - one `ToolContext` constructor to replace the four copies in `agent.rs`
    and `meta.rs`
- Every tool's name, description, effect, domain, schema and argument
  mapping stays written by hand, in place.

**Guard:** `mcp.rs` snapshot unchanged, 0.4's tool-path section unchanged,
0.5 unchanged.

## Phase 9 — service internals (each its own PR)

In increasing order of risk:

1. **Pure file splits**:
   - `websearch.rs` → `websearch/{mod,sources/*}`
   - `ics.rs` → `{read,rrule,write}`
   - `mailsync/tests.rs` by subsystem
   - `meeting/pipeline.rs` stages

   Moves only. `git diff -M` should show renames plus `mod` lines.
2. **An injected `Clock`** (`Arc<dyn Clock>` on `Service`, `SystemClock` by
   default), replacing `Timestamp::now()` in the service crate call by call.
   It must advance with tokio's paused time in tests. Add a test that a paused
   test sees the same values as before.
3. **Unify the retry mechanism, not the policies.** A `RetryPolicy` value
   type, with today's outbox table, supervisor jitter and `Retry-After`
   handling each expressed as its own policy. One `is_transient`. Pin each
   policy's delay sequence in a test **before** switching callers.
4. **A `CalendarProvider` trait** over CalDAV, Google and Graph, shaped like
   `Transcriber`. `accountcal` tests plus `caldav_docker` must be unchanged.
5. **Split `Service` into runtimes** (`MailRuntime`, `MeetingRuntime`,
   `RoutineRuntime`), each with `on_unlock` / `on_lock`. Add a test first that
   locks and unlocks with work in flight in each runtime and checks what
   `close()` clears today. The order of stopping the supervisor versus
   clearing state is the risk.

## How each PR is checked

A PR in this plan merges only when all of these are true:

- [ ] The description names the phase and says "no behaviour change".
- [ ] No golden or snapshot file changed, or each change is explained and
      belongs to phase 0 or phase 1.
- [ ] `make test` green locally, **with `--no-fail-fast`**. A red test binary
      otherwise hides every later one.
- [ ] `make test-postgres` green for any PR touching `everyday-core/src/store*`,
      `everyday-store-*` or `everyday-vault`. CI runs it too, but finding out
      there is late.
- [ ] Clippy with CI's toolchain, not the local 1.93:
      `PATH=$HOME/.cache/ed-rust/1.98/bin:$PATH CARGO_TARGET_DIR=$HOME/.cache/ed-rust/target cargo clippy --all-targets -- -D warnings`,
      plus `-p everyday-app` and the `speech` feature run where touched.
- [ ] `npm --prefix ui run check` green for any UI change.
- [ ] Any UI change: a headless before/after screenshot of the affected app in
      mock mode.
- [ ] Known flakes are not "fixed" by the refactor or blamed on it:
  - `format.test` fails in the evening in Pacific time (UTC date vs local
    "Today")
  - `meetings_spool` intermittently reports "expected a failure: Null"

  Rerun, and note it in the PR.
- [ ] The PR is small enough to revert on its own. Phases never share a PR.

## Order and rollback

Phases 0 → 1 → 2 are a prerequisite chain. After that, 3–4 (UI) and 5–9 (Rust)
are independent tracks and can interleave, except that 7 needs 6's
`RecordKind` and 8 is easier after 7.

Each phase is a series of self-contained PRs. Rolling back is `git revert` of
the PR, because no phase changes stored data or the wire. That is the
property phase 0.1–0.3 exists to guarantee. A phase that finds itself needing
a migration, a snapshot update or a new wire field has left this plan and
stops there.

## What success looks like

- The Phase 0 files are byte-identical at the end to the day they landed,
  except for the one expected mail line in 0.7.
- Adding a record kind touches the core descriptor, its store and vault code,
  its commands and tools (by hand, with shared helpers), one UI app entry, and
  whichever surface enums it belongs to. The tests say which are missing,
  rather than a reviewer's memory.
- Kind strings live in one enum, each with its literal per-surface spelling.
- Change events have one emitter, asserted against what was written.
