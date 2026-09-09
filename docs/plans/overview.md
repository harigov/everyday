# Roles, goals, and the Overview

A plan for a fifth app and the two records underneath it. Written 9 September
2026 against the tree at `371783e`. Decisions in the first section were made
in conversation and are settled; the phases after it are the proposed order
of work and are the part to argue with.

## What this is

Three things, in dependency order:

1. **Roles and goals.** A *role* is who you are being — parent, engineer,
   partner, yourself. A *goal* is an outcome under a role with a horizon:
   "Viya rides without stabilisers by spring", under parent. Both are records
   in the vault. Neither is a project. Any record that represents effort or
   attention — a project, a task, a block of time, an entry, a shelf item, a
   tracker, a subscribed calendar — can point at a goal, or straight at a
   role when there is no goal, and that pointer is what makes "where does my
   time go, by role" a query.
2. **Trackers leave the journal.** Today a tracker's definition is a field
   inside the sealed `Journal` record, so a tracker belongs to one journal.
   It becomes a vault-level record of its own, like a library `Kind`. Readings
   are already their own rows and do not move. This is what lets a habit be
   the *measure* of a goal, and lets a habits view exist at all.
3. **The Overview.** A fifth app on the bar. It owns roles, goals and
   trackers, and reads everything else: today, this week by role, goals by
   last touched, habit streaks, what is on the shelf, and the honest share of
   time that was recorded against nothing. It is a view over what the other
   apps store, in the sense the calendar is.

And one capture feature that falls out of the second:

4. **Quick track.** `#swim 60min`, typed in an entry or in a one-line popover,
   writes a reading — and creates the tracker on first use of a new name, so a
   structured note never needs a trip to settings first.

## Decisions already made

These were settled in discussion and the plan does not reopen them.

- **Role and Goal are separate records.** Roles are few and change yearly;
  goals are many and have a status and a horizon.
- **The pointer is one field, optional everywhere.** Called `purpose`. It is
  an enum shaped like `BlockSubject` — `Goal { id }` or `Role { id }` — and it
  is never required. Capture stays one line and one Enter; the inbox stays
  purposeless. A thing has one purpose, not many; tags remain the
  cross-cutting mechanism.
- **Purpose inherits.** A block's purpose is its own if set, else its task's,
  else its task's project's. Setting it once on a project attributes
  everything under it. A subscribed *calendar* carries a role (not its
  events), so one click attributes a whole work feed.
- **The app is called Overview**, not Analytics. The README says the app has
  no analytics, meaning telemetry, and the name should not muddy that
  sentence. Overview is where a day starts; Analytics is a report.
- **Overview owns roles, goals and trackers.** Not Settings (a definition you
  have to go to Settings for is one you will not make), and not Todo (goals
  are not work; half of them will have no tasks). Todo gets a goal field on
  projects and tasks, a picker that can create a goal inline, and a
  "group by goal" list option — nothing more. The rule is the library's: a
  thing is defined where its data is seen.
- **Trackers: one shape, four kinds, plus a cadence.** No split into habits
  and health. `Check` is the habit, `Dose` and `Scale` are health, `Amount`
  is either. The one schema addition is `cadence` (e.g. three times a week),
  because the daily `target` cannot express the commonest habit and streaks
  need it.
- **Ad hoc structured information is a reading, created lazily.** No free
  key-value bag on entries. Everything such a bag would hold is a number
  with a name (a reading), a span of time (an actual block), or a word (a
  tag) — three homes that already exist and are queryable.
- **Readings versus blocks.** Time spent on work is a block; a quantity
  measured about yourself is a reading. Swimming is a reading. Never both.
- **Lazy tracker creation asks nothing.** An unknown name after `#` makes a
  tracker with a kind and unit guessed from the text; a wrong guess costs one
  edit in Overview and keeps its readings. A confirmation prompt is the
  settings trip in a different coat.
- **Readings are edited beside their tracker, under their day**, as the
  README already says. Inline `#swim 60min` in prose is therefore a
  *launcher* that writes the reading once; the text stays as typed and is
  not re-parsed on save. It is not a live-bound node.

## The data model

### New records

```
Role     { id, name, color, icon, archived, sort_order, created_at, updated_at }

Goal     { id, role_id, title, notes, status, horizon: Option<Date>,
           sort_order, created_at, updated_at, completed_at }
           status ∈ { active, paused, done, dropped }   -- closed, like TaskStatus

Purpose  = Goal { id } | Role { id }                    -- serde tag = "type"

Tracker  { …everything it has today…,
           purpose: Option<Purpose>,
           cadence: Option<Cadence> }                    -- moves out of Journal

Cadence  { times: u32, per: Day | Week | Month }         -- "3 per week"
```

`Purpose` goes on `Project`, `Task`, `TimeBlock`, `Entry`, `Item`, `Tracker`
as `#[serde(default)] purpose: Option<Purpose>`. `Calendar` gets
`role_id: Option<RoleId>` instead — a feed serves a role, not one goal.

`Journal` loses `trackers: Vec<Tracker>` and gains
`shown_trackers: Vec<TrackerId>`: which vault trackers this journal's chip
strip draws. The old field stays as a `#[serde(default)]` so an unmigrated
vault still deserialises, and is emptied by the one-time move described
under phase 2.

`Reading.journal_id` becomes `Option<JournalId>`, meaning "the journal whose
page it was ticked on, if any", exactly as `entry_id` already means. A reading
logged from the Overview has neither.

### Ids

`RoleId` and `GoalId` in `crates/everyday-core/src/id.rs` via `typed_id!`, and
re-exported from `lib.rs`.

### Schema, version 7

`SCHEMA_VERSION` goes to 7 and `steps()` gains `v7(d)`. Two conventions are
bent here, deliberately, and each is the same SQL in both dialects so the
`the_two_dialects_agree_about_everything_but_types` guard still holds.

- **First use of `ALTER TABLE`.** Every step so far has only added tables.
  `ALTER TABLE x ADD COLUMN purpose_kind TEXT` / `purpose_id TEXT` on
  `projects`, `tasks`, `time_blocks`, `entries`, `items`, and
  `role_id TEXT` on `calendars`, is the plain form both SQLite and Postgres
  accept. The pointer's clear columns are what the roll-ups group by; the
  sealed copy in `data` is the source of truth, as with every other clear
  column.
- **Rebuilding `readings`** to make `journal_id` nullable. SQLite cannot
  drop a `NOT NULL`, so the step is `CREATE TABLE readings_v7 …`,
  `INSERT INTO readings_v7 SELECT …`, `DROP TABLE readings`,
  `ALTER TABLE readings_v7 RENAME TO readings`, then the four indexes
  again. Same statements on Postgres. The step runs in one transaction like
  every other.

New tables, all with the clear-index-columns-plus-sealed-`data` shape:

```
roles     (id, archived, sort_order, created_us, updated_us, data)
goals     (id, role_id, status, horizon, sort_order, created_us, updated_us,
           completed_us, data)
trackers  (id, purpose_kind, purpose_id, archived, sort_order,
           created_us, updated_us, data)
```

What stays sealed: every name, title, note, icon, colour, unit, kind and
cadence. What the file can say is that goal `7f3a…` is active under role
`91c0…` and got three hours on Tuesday, and never what either is. The
existing rule.

Indexes: `goals_by_role (role_id, status)`, `trackers_by_purpose
(purpose_kind, purpose_id)`, `blocks_by_purpose (purpose_kind, purpose_id,
kind, local_date)`, `tasks_by_purpose`, `projects_by_purpose`.

### The roll-ups

Two new clear-column aggregates, in SQL, decrypting nothing, in the manner of
`task_stats`:

- **`time_by_purpose(from, to) -> Vec<PurposeMinutes>`** — actual and planned
  minutes per resolved purpose over a date range. The resolution is
  `COALESCE(block.purpose, task.purpose, project.purpose)` over a
  `time_blocks LEFT JOIN tasks LEFT JOIN projects`, grouped by the
  coalesced pair, plus one row for `NULL` (the unattributed share). Subscribed
  events join through `calendars.role_id` and are summed separately as
  "event minutes", because a calendar event is not a block and should not be
  confused with one.
- **`goal_activity(goal) -> GoalActivity`** — open and done task counts,
  actual minutes, the latest of: last actual block, last entry, last reading,
  last library log. "Last touched" is what the Overview sorts goals by.

Goal → role is a join on the clear `goals.role_id`, so "by role" is the
"by purpose" result folded once in Rust.

Streaks and cadence hit-rates come from `tracker_days`, which already exists
and already returns a year of any tracker as one `GROUP BY`. They are computed
in the interface (`ui/src/lib/habits.ts`, pure, tested), because a streak is a
walk over days with a rule, not a query.

## Phases

Each phase leaves `main` shippable. The order is chosen so the riskiest
migration (phase 2) lands before anything is built on it, and so a user sees
nothing change until phase 3.

### Phase 1 — Roles and goals, the domain only

Net-new and additive. No visible change except a picker.

Rust:

- `crates/everyday-core/src/purpose.rs` (new): `Role`, `Goal`, `GoalStatus`,
  `Purpose`, `PurposeMinutes`, `GoalActivity`, with `new()`, `set_status()`
  keeping `completed_at` honest, `searchable_text()`, and unit tests in the
  style of `task.rs`.
- `crates/everyday-core/src/store/purpose.rs` (new): `PurposeStore` trait —
  `list_roles`, `get_role`, `put_role`, `delete_role`; `list_goals(&GoalQuery)`,
  `get_goal`, `put_goal`, `delete_goal`; `time_by_purpose`, `goal_activity`;
  `role_aad` / `goal_aad`. `JournalStore::purpose()` accessor defaulting to
  `None`; `Capabilities.goals: bool`.
- `purpose: Option<Purpose>` on `Project`, `Task`, `TimeBlock`, `Entry`,
  `Item`; `role_id` on `Calendar`. Every `put_*` in
  `crates/everyday-store-sql/src/{tasks,journals,library,calendars}.rs`
  writes the two clear columns.
- `schema.rs`: `v7(d)`, including the `readings` rebuild now so there is one
  version 7, not two. Test: a version-6 fixture opens, keeps its rows, and
  has the new columns (model: `a_version_4_database_gains_the_readings_table_
  without_losing_entries`).
- `crates/everyday-store-sql/src/purpose.rs` (new): the impl. The
  `time_by_purpose` query is the one piece of real SQL; write it once in
  `where_clause` style so list and aggregate cover the same rows.
- Conformance: `run_purpose_suite` in `store/conformance.rs` — role and goal
  CRUD, deleting a role refuses while goals point at it — unlike a project,
  which takes its tasks with it, because a role's goals are not *made of*
  the role and a year of attributed time should not vanish behind one
  click — purpose round-trips on all five records, `time_by_purpose` resolves inheritance
  and reports the unattributed row.
- `Vault`: `supports_goals`, `with_purpose`, `roles`, `save_role`,
  `delete_role`, `goals`, `save_goal`, `delete_goal`, `time_by_purpose`,
  `goal_activity`, each with `self.writable()?` where it mutates.
- Commands in `commands.rs` (`list_roles`, `save_role`, `delete_role`,
  `list_goals`, `save_goal`, `delete_goal`, `time_by_purpose`,
  `goal_activity`), registered in `lib.rs`, all `async` + `blocking`.

Interface:

- `types.ts`: `RoleId`, `GoalId`, `Role`, `Goal`, `GoalStatus`, `Purpose`,
  and `purpose?: Purpose | null` on the five record types; `roleId` on
  `Calendar`. `api.ts` wrappers. `mock.ts` cases with three seeded roles and
  five goals, one paused, one done.
- `ui/src/lib/purpose.svelte.ts`: a small store — roles, goals, load once
  after unlock, `label(purpose)` and `color(purpose)` for chips.
- `PurposePicker.svelte`: a popover listing roles as headings and goals under
  them, with a typed line that creates a goal inline under a chosen role.
  Used from `TaskDetail`, the project settings, `BlockDetail`, and the
  calendar's subscription sheet (role only).
- Todo: "Goal" in the list's group-by menu, beside due date, status and
  priority.
- Shortcuts: none yet.

### Phase 2 — Trackers leave the journal

The migration phase. Visible behaviour is unchanged when it lands.

Rust:

- `Tracker` gains `purpose` and `cadence`; `Cadence` in `tracker.rs` with
  tests for "hit this period" arithmetic.
- `TrackerStore` gains `list_trackers`, `get_tracker`, `put_tracker`,
  `delete_tracker`, `merge_trackers(from, into)` (re-points readings, then
  deletes `from`). `delete_readings_in(journal)` is removed: readings belong
  to the tracker now, and deleting a journal leaves them.
- `Reading.journal_id: Option<JournalId>`; `ReadingQuery.journal_id` stays as
  a filter.
- `Journal.shown_trackers: Vec<TrackerId>`; `Journal.trackers` retained as a
  deprecated `#[serde(default)]` field for the move.
- **The one-time move**, in `Vault::activate` after the store opens: if any
  journal still has a non-empty `trackers`, put each as a vault tracker, set
  that journal's `shown_trackers` to their ids, clear the old field, save
  the journal. Idempotent, runs inside the write claim, and is the only place
  sealed data has to be migrated in Rust rather than SQL — because SQL cannot
  read it. Test: a journal with three trackers and readings against them
  opens as three vault trackers, the strip shows the same three, every
  reading still resolves.
- `save_reading` clamps against the vault tracker; `delete_tracker` and the
  agent's `journal_of_tracker` lose their journal hop.
- Conformance: `run_tracker_suite` (there is none today; readings are tested
  per backend) — definition CRUD, merge, readings survive a journal delete,
  the sealed-name test moved from the SQLite suite.

Interface:

- `api.ts`: `trackers()`, `saveTracker()`, `deleteTracker(id)`,
  `mergeTrackers(from, into)`; `newTracker` returns a vault tracker.
- `tracking.svelte.ts`: trackers held here rather than read off
  `app.journal`; `activeTrackers(journal)` in `tracker.ts` becomes
  `shownTrackers(journal, all)`.
- `JournalSettings.svelte`: the tracker section becomes "shown on this
  journal" — a checklist over vault trackers plus "new tracker" which
  creates and ticks in one step. Editing a tracker's fields moves to Overview
  in phase 3; until then the existing form is kept, saving via
  `saveTracker`.
- `mock.ts`: trackers lifted out of the journal seeds into a `trackers`
  array; `shownTrackers` per journal reproduces today's demo exactly.

### Phase 3 — The Overview

The app. Depends on 1 and 2.

Registration, following `AppBar` and `state.svelte.ts` exactly:

- `SECTIONS` gains `'overview'`; `canShow('overview')` is
  `supportsGoals && supportsTrackers`; `APPS` gains a row with a new
  `IconName`; `App.svelte` gets an accent branch, a pane branch, and the
  store in both flush lists; `TRAY_ORDER.overview = 50`; `tray.register`
  at the foot of `overview.svelte.ts`; `OverviewNav` in `Sidebar`.
- Shortcuts: `g o`; `create()` branch makes a new goal; the search field
  wears `data-search`. Bare letters: `r` roles, `h` habits, `w` this week —
  to be settled when the panes exist.

The store, `ui/src/lib/overview.svelte.ts`, in the `library.svelte.ts`
shape: what is on screen (pane, anchor week, selected goal or tracker),
what has been loaded (roles, goals, trackers, `time_by_purpose` for the
window, `tracker_days` for the habits window, `goal_activity` per goal,
task stats, library stats), `#generation`, `Autosave` for goal and tracker
edits, tray actions ("New goal", "Log a reading").

Panes, left to right in the sidebar:

- **Today.** What is planned and due, what is on the calendar, the running
  block if any, and the day's trackers as the same `TrackerStrip` the
  journal draws — ticking here writes the same row.
- **This week.** A stacked bar per day, one colour per role, actual beside
  planned, with the unattributed share drawn in the neutral tint and never
  hidden. Below it, a row per role: recorded, planned, the gap. A role with
  an active goal and nothing recorded for a fortnight is called out.
- **Goals.** Roles as sections, goals under them, sorted by last touched,
  each with its horizon, open tasks, hours this month, and the tracker that
  measures it if one does. Selecting one opens `GoalDetail`: notes, status,
  horizon, purpose-linked projects and tasks, entries and readings, and the
  "attach" action that opens the same `PurposePicker` in reverse.
- **Habits.** One row per non-archived tracker: streak, cadence hit-rate
  over the window, and a year heatmap built the way `EntryCalendar` builds
  its month. Selecting one opens `TrackerDetail`: the existing editor form
  from `JournalSettings`, plus purpose, cadence, "shown on" journals,
  archive, and merge-into.
- **Shelf.** Items with `active` status, from `library_stats`, so the
  in-progress book and the half-watched series are one glance away.

Roles are edited in place in the sidebar through the same right-click menu
the library kinds use; a role cannot be deleted while a goal points at it.

Pure modules, tested with the existing `vite` `ssrLoadModule` harness:

- `ui/src/lib/habits.ts` — streak, cadence hit-rate, the heatmap buckets.
- `ui/src/lib/balance.ts` — folding `PurposeMinutes` into by-role rows,
  the neglect rule, the week layout arithmetic.

Charts follow the `dataviz` skill when built: the stacked bars and heatmap
are drawn as inline SVG with role colours from the journal palette, and the
neutral tint for unattributed.

### Phase 4 — Quick track

Capture. Depends on 2; independent of 3.

- `ui/src/lib/quicktrack.ts` (pure, tested in `quicktrack.test.mjs`): the
  grammar and its hint constant beside it, in the `quickadd.ts` manner.
  `swim 60min` → amount, minutes; `mood 7/10` → scale of ten; `floss` →
  check; `ibuprofen 400mg` → amount, mg (dose is never guessed). A name is
  matched against existing trackers first, case-insensitively, before any
  creation; anything unparsed stays in the line. The result is
  `{ tracker: Existing | New, value, unit, at? }`.
- `LogReading.svelte`: the popover. One field, the hint under it, existing
  trackers listed below for one-click logging, and a preview line that
  reflects the parse — "Swimming, 60 min, today" or "New tracker: swim,
  minutes". Enter writes, clears, keeps focus; submissions are chained, not
  guarded, as `QuickAdd.svelte` does. Opened from a plus chip at the end of
  `TrackerStrip`, from the Overview's Today pane, and from a tray action
  registered on the journal source.
- Lazy creation lives in the store, not the component: `tracking.log(line,
  { journalId?, entryId?, date })` resolves or creates the tracker, adds it
  to the journal's `shown_trackers` when logged from an entry, and writes
  the reading.
- The editor: a `#` suggestion in `Editor.svelte` listing trackers, with an
  "new tracker" row for an unknown name, in the `AddItem.svelte` listbox
  shape. Committing on space or Enter calls the same `tracking.log`, and
  leaves the text as typed. This needs `@tiptap/suggestion` — one new
  dependency, to be added to `THIRD-PARTY-NOTICES.md`. If that dependency is
  unwelcome the popover alone ships and the inline path waits.

### Phase 5 — The assistant

Tools in `agent/tools.rs`, each a `tool!` row and a `run_*`; no other
registration. Names follow the `delete_` rule the
`every_tool_that_deletes_says_so` test enforces.

- `list_roles`, `list_goals`, `create_goal`, `update_goal`, `delete_goal`
  (Destructive), `set_purpose(kind, id, purpose)`.
- `list_readings(tracker, from, to)` — the per-reading range the current
  `tracker_summary` cannot give, which is what "line up pool days against
  the mood of the day after" needs.
- `log_reading` accepts a tracker *name* and creates lazily, sharing the
  guess rules with `quicktrack.ts` by porting them to Rust in `tracker.rs`
  (one function, tested in both places against the same table of cases).
- `time_by_role(from, to)`.
- `overview` gains a goals section and reports by role.
- `Domain::Goals`, gated on `supports_goals`, so the
  `the_catalogue_covers_every_domain_the_vault_has` test stays true.

### Phase 6 — Words

- README: a section for the Overview and one for roles and goals in the
  voice of the existing ones; the trackers section rewritten for
  vault-level definitions and quick track; the shortcut table; the
  "Four apps, one vault" heading and the bar paragraph; the Status section's
  counts; "an analytics view over the readings above" leaves the not-yet-
  built list.
- `crates/everyday-store-sql/src/lib.rs` clear/sealed table and the
  `schema.rs` version-7 rationale comment.

## What is deliberately out

- The CLI. It covers journals only and this does not change that.
- Recurring tasks. A habit is a tracker, not a task that comes back.
- A many-to-many purpose. One pointer; tags for the rest.
- Writing readings from prose on every save. The inline path writes once.
- A per-kind habit/health split.
- Any network use. Nothing here opens a socket.

## Risks and the answer to each

| Risk | Answer |
|---|---|
| The `readings` rebuild on a big vault | One transaction, indexes recreated after the copy; test against a fixture with a year of rows and time it. |
| The sealed-definition move runs on an old client against a new vault | Version 7 is refused by clients that only know 6 (`UnsupportedVaultVersion`), which is the existing rule. |
| Postgres shared vault mid-migration | The write claim is held for the move; a second client sees `VaultInUse`. |
| Lazy trackers proliferate | Merge-into in `TrackerDetail`, and the name match before creation. |
| The dashboard becomes a wall of tiles | Each pane answers one question; nothing is drawn that does not change what you do next. |
| `@tiptap/suggestion` | Optional; the popover is the primary path. |

## Sizes

Rough, in working days, for one person who knows the tree.

| Phase | Days |
|---|---|
| 1 Roles and goals | 4 |
| 2 Trackers leave the journal | 3 |
| 3 The Overview | 6 |
| 4 Quick track | 3 |
| 5 The assistant | 2 |
| 6 Words | 1 |

## Open questions

Assumptions the plan proceeds on; say so if any is wrong.

- Deleting a journal keeps its readings. They belong to the tracker now.
- Deleting a role with goals under it is refused, not cascaded.
- `Entry` carries a purpose (so a journal entry can be evidence for a goal),
  even though most entries will never set one.
- The bar letter for the app is `o`, as in `g o`.
