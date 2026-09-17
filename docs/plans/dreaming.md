# Dreaming: proposals, inferred memory, and a routine the app owns

> **Delivered.** All five phases are built and tested on
> `agent-dreaming-routine`. Read this for the reasoning; read the commits
> for what was actually done. What went differently from the plan below:
>
> - **The payload tells the three record-carrying cases apart by `type`,**
>   and a mail proposal's field is `draft_id`. `Origin` in the sketch became
>   `ProposalSource`, and `made_by` is optional.
> - **Rejected memories have a cap after all**, `MAX_REJECTED` (32). The plan
>   said they count against none, but every one is read into every prompt,
>   and an unbounded list is the thing the memory design exists to avoid.
> - **A hand save never changes a memory's origin.** Only confirming a
>   rewritten inference does; everything else goes through
>   `set_memory_origin`. Found in review.
> - **A dream confirms an inference by naming it.** Its final message may end
>   with a `Confirmed memories:` section, one id per line, and the scheduler
>   moves those memories' `last_supported` to the digest's day. The nightly
>   dream infers something new only with three days' evidence in the `why`.
> - **Dreams' instructions may be empty,** and `Routine::validate` asks for
>   instructions only from a person's routine. A schedule may name a day of
>   the month, `1..=28`, which is how the monthly dream runs on the 1st.
> - **"Run now" on a dream runs a dream,** with the digest and the drafting
>   turn, and a skip closes the queued row.
> - **The monthly dream looks ahead for birthdays**, the profile's and the
>   Contacts shelf's that `main` added while this was being built, with when
>   each person was last caught up with.
> - **A dream's draft of a mail is written only when its send can still be
>   proposed**, and discarded if proposing fails. Found in review.
> - **Reopening an assistant's mail draft in the compose sheet** is not
>   possible yet (an existing mail TODO), so a proposed send is answered from
>   the Assistant app's "Waiting for you" until it is.
> - **Phase 5's parking setting is `park_unattended`,** its own switch rather
>   than a reading of `confirm_destructive`, and covers sends as well as
>   deletions.

A plan for letting the assistant think while nobody is watching — read what
the day produced, revise what it believes about the person, and prepare work
it is not allowed to do on its own — and for the one record that makes the
last part safe: a *proposal*, which is a finished record the person accepts
or declines. Written 16 September 2026 against the tree at `95d5c58`, on
`rainy-skunk`. The first section records what was decided in conversation
and is settled; the phases after it are the proposed order of work and are
the part to argue with.

## What this is

Three things, in dependency order, and a loop that closes over them.

1. **Proposals.** A sealed record in the vault holding a *finished* Task,
   TimeBlock, Memory, Routine or Note that the assistant built but did not
   save, or a pointer to a mail Draft it wrote, or a deletion it wants. It is
   drawn where the real record would be drawn — a dotted block on the grid,
   a grey row at the foot of a task list — with accept and decline on the
   row itself. Accepting saves the record through the same store path the
   tool would have used. Declining, editing before accepting, and letting it
   expire are each recorded, because they are the signal the rest of this
   plan feeds on.
2. **Inferred memory.** A [`Memory`](../../crates/everyday-core/src/agent.rs)
   gains a provenance: told by the person, inferred by a dream, confirmed
   after being inferred, or rejected. Inferred memories reach the prompt
   under a heading that says they may be wrong; rejected ones are kept as
   "do not assume" lines so a later dream cannot re-learn them. This is the
   one kind of draft the assistant may use before it is accepted.
3. **The dream.** A built-in [`Routine`](../../crates/everyday-core/src/routine.rs)
   the application owns, in three scopes. The nightly one reads yesterday.
   The weekly one, on Sunday, reads the seven nightly outputs rather than
   the raw week. The monthly one reads the four weekly ones. It runs through
   the scheduler that already exists, with the same run log, transcript,
   tray and app-bar count, and it may write exactly three things: inferred
   memories, proposals, and at most one note.
4. **The loop.** The weekly dream reads the week's proposal outcomes and
   writes ordinary memories from them — "declines meeting-prep drafts for
   one-to-ones", "accepts tasks from call notes but shortens the titles".
   A kind declined repeatedly stops being proposed, learned rather than
   hard-coded.

Nothing here is a new subsystem beside the others. The scheduler, the run
log, the memory list, the tool catalogue and the sealed-record store each
grow by one variant or one table.

## Decisions already made

These were settled in discussion and the plan does not reopen them.

- **Only unasked work drafts.** Work asked for in the rail, or written as a
  routine in the person's own words, is done and audited — the README's
  argument against a review queue stands. Work the assistant *starts on its
  own* — today, only dreams — produces proposals. This is the line that
  keeps the assistant from becoming a suggestion engine that never
  finishes anything, and it is written down here so the next feature that
  wants to draft rather than do has to argue with it.
- **A proposal carries a record, not a tool call.** The payload is the
  domain record as it would be after saving. Tool argument schemas change
  whenever a prompt is tuned and promise nothing; records are the one thing
  this vault already keeps readable across versions, with `#[serde(default)]`
  on every field added since a row was first sealed. Replaying a stored
  tool call after an upgrade is the failure this decision exists to
  prevent.
- **Updates store the after-image, not a patch.** A patch is a set of
  argument names and inherits the schema problem by another route. A
  `Replace` carries the whole record plus the `updated_at` it was built
  against, so a record changed underneath it is refused rather than
  overwritten.
- **One switch for dreaming, off by default.** In Settings → Assistant,
  with the sentence beside it saying what it reads. Not a switch per
  source: a person who wants the assistant thinking overnight wants it
  thinking about their day, and the day is in the journal as much as the
  task list.
- **Which *kinds* may be proposed is a policy**, shaped like
  [`QuickPolicy`](../../crates/everyday-core/src/quick.rs): a set of denied
  kinds among task, block, memory, routine, note and mail. All on when
  dreaming is on; each refusable on its own.
- **Proposals are drawn in place, styled as not-yet-real.** Not a queue in
  the Assistant app that nobody visits. A pending task proposal is a row in
  the todo list; a pending block is on the calendar grid; a pending memory
  is in the memory list; a pending routine is in the routine list; a mail
  draft the assistant wrote is already in the account's drafts. The
  Assistant app also lists them all, and the count on the app bar includes
  them, but the primary surface is wherever the record would have landed.
- **Draft memories are a provenance, not a separate document.** The
  earlier idea of a standing "picture" the dream maintains is dropped in
  favour of an `origin` on `Memory`. One list, one cap per origin, one
  place to strike a line out.
- **There is no calendar event to draft.** Events arrive only from feeds —
  a URL, an imported file, or an account calendar over CalDAV, Google or
  Graph — and nothing in the vault creates one. The draftable thing on the
  grid is a *planned* [`TimeBlock`](../../crates/everyday-core/src/task.rs).
  Writing events to somebody else's server is a separate project.
- **Mail proposals point at the existing `Draft`.** It already carries an
  `Origin` saying who wrote it and a state machine of editing, queued, sent
  and discarded. A second draft record for mail would be a second answer to
  "what sees my mail".
- **The dream's arithmetic is done in Rust.** A digest of the day — tasks
  finished and rescheduled, hours by purpose, readings, events, entries
  written, proposal outcomes — is built from the queries the Overview
  already runs and handed to the model as its first message. The model
  reads specific entries only when it decides to. Thirty tool calls over a
  week of data is the alternative, and it is the wrong shape for something
  that runs every night.
- **Outcomes never go into every prompt.** The weekly dream reads them and
  writes sentences. The prompt stays the size it is.
- **The nightly dream may not create anything.** Its whole write set is
  inferred memories, proposals and one note. No tasks, no blocks, no
  routines, no mail sent, no journal entry — ever, not "until trusted".

## The data model

### New records

```
Proposal {
  id, kind, payload, caption, why, about,
  made_by, made_at, expires_at, outcome, seen
}

ProposalKind   = task | block | memory | routine | note | mail

Payload        = Create  { record: Record }
               | Replace { record: Record, expected_updated_at: Timestamp }
               | Delete  { kind: ProposalKind, id: String }
               | SendMail { draft: DraftId }

Record         = Task(Task) | Block(TimeBlock) | Memory(Memory)
               | Routine(Routine) | Note(Note)

Origin         = Run { run: RoutineRunId } | Conversation { id: ConversationId }

Outcome        = Pending
               | Accepted { at, saved_as: String, edited: bool }
               | Declined { at, reason: Option<DeclineReason> }
               | Expired  { at }

DeclineReason  = NotNow | WrongTime | NeverThis | Other(String)
```

- `caption` is the sentence the confirmation card already shows for a tool
  call — `describe()` in
  [`tools/mod.rs`](../../crates/everyday-core/src/agent/tools/mod.rs) — stored
  at creation so a proposal still reads after the tool that made it is
  renamed.
- `why` is one line from the model, capped like a memory. It is the answer
  to "why is this here", which is the first thing anybody asks of a row
  they did not make.
- `about` is what it was reacting to, if anything: a task id, an event id,
  a note id, a mail message id. Sealed; used to draw the ghost beside its
  subject and to let the weekly dream say "the meeting-prep ones".
- `edited: bool` on `Accepted` is the flag; the *diff* is recoverable from
  the proposal's record and the saved record, both of which still exist.
  Nothing is stored twice.
- `seen` is what the app-bar count is drawn from, exactly as
  [`RoutineRun::seen`](../../crates/everyday-core/src/routine.rs) is.

### Changes to existing records

```
Memory {
  …,
  origin:          Told | Inferred | Confirmed | Rejected     (default Told)
  last_supported:  Option<Date>                               (Inferred only)
}

Routine {
  …,
  kind: Custom | Dream { scope: Day | Week | Month }          (default Custom)
}

AgentSettings {
  …,
  dreaming:  bool                                             (default false)
  proposals: ProposalPolicy { denied: BTreeSet<ProposalKind> }
}
```

- `Memory::origin` defaults to `Told`, so every memory sealed before this
  field existed reads as it always did. `Rejected` memories are kept and
  loaded, phrased "Do not assume: …", and count against no cap. `Inferred`
  memories have their own cap, [`MAX_INFERRED`](#caps), smaller than
  [`MAX_MEMORIES`](../../crates/everyday-core/src/agent.rs), and are evicted
  oldest-`last_supported`-first rather than oldest-created-first, because
  the one that was confirmed by yesterday's data is the one to keep.
- `Routine::kind` defaults to `Custom`. A `Dream` routine is created by the
  application when `dreaming` is switched on and cannot be deleted by
  hand; switching `dreaming` off disables all three. Its schedule and grace
  are editable; its instructions field is the *person's* paragraph,
  appended to the app-owned prompt rather than replacing it.
- `AgentSettings::dreaming` has the sentence beside it: *reads yesterday's
  journal entries, tasks, calendar, time blocks, readings, notes, meeting
  notes and mail headers; writes memories it has inferred and proposals
  you can accept or decline; sends all of that to the assistant's model.*

### Ids

`ProposalId`, prefix `proposal`, in
[`id.rs`](../../crates/everyday-core/src/id.rs) via `typed_id!`.

### Schema, version 11

Additive and `IF NOT EXISTS`, like every version since 7.

```sql
CREATE TABLE IF NOT EXISTS proposals (
  id           TEXT    PRIMARY KEY NOT NULL,
  kind         TEXT    NOT NULL,      -- task | block | memory | routine | note | mail
  target_date  TEXT,                  -- the block's day or the task's due day,
                                      -- so the calendar's week query can find
                                      -- ghosts the way it finds time_blocks
  made_us      {int}   NOT NULL,
  expires_us   {int}   NOT NULL,
  outcome      TEXT    NOT NULL,      -- pending | accepted | declined | expired
  seen         {boolean} NOT NULL DEFAULT {f},
  data         {blob}  NOT NULL
);
CREATE INDEX IF NOT EXISTS proposals_pending ON proposals (outcome, kind, target_date);
CREATE INDEX IF NOT EXISTS proposals_by_made ON proposals (made_us);
```

`memories`, `routines` and `agent_settings` change nothing in the clear;
the new fields live in the sealed payload with defaults.

### Caps

| | |
|---|---|
| `MAX_INFERRED` | 24 inferred memories, on top of the 64 told or confirmed |
| `MAX_WHY_CHARS` | 200 |
| `MAX_PENDING_PROPOSALS` | 40. The dream is told how many are pending and refuses to add past the cap; a person who has forty unanswered proposals does not want a forty-first |
| Proposals per dream run | 5 nightly, 8 weekly, 8 monthly |

### Expiry

Set at creation, by kind, and never extended.

| Kind | Expires |
|---|---|
| Task | The due date, or seven days if none |
| Block | The block's end |
| Memory | Thirty days |
| Routine | Fourteen days |
| Note | Seven days |
| Mail | Whatever the `Draft` does; the proposal expires when the draft is discarded or sent by hand |
| Delete | Seven days |

A sweep in [`scheduler::tick`](../../crates/everyday-service/src/scheduler.rs)
marks the past-due pending rows `Expired`, beside the snooze release that
already runs there. An expired proposal is not a declined one and is
counted separately by the weekly dream — ignoring is a weaker signal than
a no, and repeated ignoring is still a signal.

### Version safety

The reason the payload is a record, spelled out so it is a rule rather than
a preference.

1. **The payload deserialises with serde defaults**, like every sealed blob
   in the vault. A field added to `Task` after a proposal was sealed reads as
   its default, which is what would happen to any task sealed before that
   field existed. A conformance case seals a `Proposal` with a field
   removed from its JSON and asserts it still reads.
2. **Proposals expire in days.** Few pending rows ever meet an upgrade.
3. **Accept validates before it saves.** The record's own `validate()`, and
   the references it carries: the project a task was filed under, the task a
   block is for, the goal a purpose names, the `expected_updated_at` on a
   `Replace`. Failure is reported in the caption's own words — *the project
   this was for has been deleted* — and the proposal is marked `Declined`
   with `Other` so it does not sit there. Nothing stale is ever written.
4. **A migration that changes an invariant may expire pending proposals**
   with the reason in the row. The schema version already gates this: a
   client that knows 10 refuses an 11 vault, and the step that lands 12 can
   sweep what 11 left pending if it must.

No tool name or argument is ever executed from a stored proposal. The tool
call that produced it may be kept for display, the way the transcript keeps
it, but it is display only.

## Phases

Each phase leaves `main` shippable. Phase 1 is the largest and is testable
with nothing producing proposals yet; phases 2 and 3 are independent of each
other and both wait on 1; phase 4 needs 3 and a fortnight of runs to have
anything to read.

### Phase 1 — Proposals, the domain and the ghosts

Net-new and additive. Nothing makes a proposal yet, so a person sees no
change unless a test or the CLI writes one.

Rust:

- `crates/everyday-core/src/proposal.rs` (new): `Proposal`, `ProposalKind`,
  `Payload`, `Record`, `Outcome`, `DeclineReason`, `ProposalPolicy`, with
  `new()`, `validate()`, `expiry_for(kind, record, now)`, and the caps.
  Unit tests in the style of `routine.rs`: expiry by kind, a policy denying
  a kind, `validate` refusing an empty caption.
- `crates/everyday-core/src/store/proposals.rs` (new): `ProposalStore` —
  `list_proposals(&ProposalQuery)` (kind, outcome, date window, `about`),
  `get_proposal`, `put_proposal`, `delete_proposal`,
  `expire_proposals_before(instant)`, `mark_proposals_seen`,
  `count_pending`. `Capabilities.proposals: bool`.
- `crates/everyday-store-sql/src/proposals.rs` (new) and `schema.rs`
  `v11(d)`. Test: a version-10 fixture opens, keeps its rows, and has the
  table.
- Conformance: `store/conformance/proposals.rs` — CRUD, the query by
  outcome and date, expiry sweep, seen is a read-modify-reseal (the bug the
  assistant plan records for `routine_runs.seen` must not be repeated), and
  the serde-default case from *Version safety*.
- **Accept**, in `Vault::accept_proposal(id, edited: Option<Record>)`:
  validate, resolve references, then save through the same `save_task` /
  `save_time_block` / `save_memory` / `save_routine` / `save_note` the tools
  use — so the `purposes` side table and every other companion write happen
  in the one place they already happen. `SendMail` hands the draft to the
  outbox exactly as `send_draft` does. `Delete` calls the domain's delete.
  Then the proposal is marked `Accepted { saved_as, edited }`.
- **Decline**, `Vault::decline_proposal(id, reason)`. **Expire**, the sweep.
- **The parse-from-save split.** Every `Write` and `Destructive` tool in
  [`agent/tools/`](../../crates/everyday-core/src/agent/tools/) that
  produces a record gets a `build_*` that returns the record without saving
  it, with `run_*` reduced to `build` then `save`. For `update_*` tools the
  build loads the current record, applies the arguments, and returns the
  after-image with the `updated_at` it started from. `delete_*` and
  `forget` build a `Delete`. `send_draft` builds a `SendMail`.
- **Drafting mode.** `ToolContext` gains `drafting: bool`. In
  [`dispatch`](../../crates/everyday-core/src/agent/tools/mod.rs), a `Read`
  tool runs as before; a `Write` or `Destructive` tool has its `build_*`
  called, the result wrapped in a `Proposal` with `caption` from
  `describe()`, `why` from a new optional `why` argument every writing tool
  accepts, `about` from the tool's own subject where it has one, and the
  policy consulted — a denied kind returns the same refusal an unattended
  `create_routine` gives today. The tool's result to the model says
  *proposed* rather than *created*, with the proposal id. `remember` in
  drafting mode builds a `Memory` with `origin: Inferred`. The existing
  `unattended` refusal in `create_routine` and `send_draft` is bypassed by
  drafting mode, because a proposal is precisely the safe form of both.
- **Tool arguments that the records have and the tools lack**, because a
  day plan needs them: `due_time`, `goal_id` and `role_id` on `create_task`
  and `update_task`; `goal_id` and `role_id` on `create_time_block`. These
  are additions to what the model may set and change no record.
- A test that runs **every** `Write` and `Destructive` tool in the catalogue
  in drafting mode against a seeded vault and asserts that no domain table
  changed and one proposal per call was written. This is the test that
  keeps the next tool honest.
- Commands in [`domains/`](../../crates/everyday-service/src/domains/)
  (new `proposals.rs`): `list_proposals`, `get_proposal`, `accept_proposal`,
  `decline_proposal`, `mark_proposals_seen`, `pending_proposals` (the count).
  `Kind::Proposal` in [`events.rs`](../../crates/everyday-service/src/events.rs);
  accept also emits the change for the record it saved. Registered in the
  surface; `UPDATE_SURFACE=1` and `gen:api` as the header of
  `commands.ts` says.
- Scheduler: the expiry sweep, beside `release_due_snoozes`.

Interface:

- `types.ts`: `ProposalId`, `Proposal`, `ProposalKind`, `Payload`,
  `Outcome`, `DeclineReason`, `ProposalPolicy`; `origin` and
  `lastSupported` on `Memory` (typed now, used in phase 2). `api.ts`
  wrappers. `mock.ts`: three pending proposals — a task, a block for
  tomorrow, a memory — one accepted with edits, one declined.
- `ui/src/lib/proposals.svelte.ts`: pending proposals loaded once after
  unlock, refreshed on `Kind::Proposal`, with `forKind(kind)`,
  `forDate(day)`, `accept(id, edited?)`, `decline(id, reason?)`.
- **Ghost rows.** One style, `.proposal`, used everywhere: dotted outline,
  the record's own colour at reduced opacity, the caption's verb as a small
  label ("proposed"), and two controls on the row — accept, and decline
  with a small menu of the three reasons. Escape declines nothing; a ghost
  that is not answered stays until it expires.
  - `TodoView`: pending task proposals at the foot of the list they would
    join — the inbox, or the project. Opening one shows the full record in
    the detail pane with fields editable; saving from there is accept with
    `edited`.
  - `CalendarView`: pending block proposals on the grid at their day and
    time, dotted. Dragging one is accept with the moved time.
  - The memory list in Settings → Assistant: pending memory proposals
    greyed under the real ones, and (phase 2) inferred memories with a
    strike-out control.
  - `AssistantView`: pending routine proposals in the routine list;
    a **Proposals** section listing all pending across kinds, newest first,
    with the same row style, for the person who would rather see them in
    one place; the run detail links to what a run proposed.
  - Mail: the drafts folder already shows the assistant's drafts by origin;
    a `SendMail` proposal adds the accept and decline controls to that row.
- The app-bar count becomes unseen runs plus unseen pending proposals.
  Opening any view that draws a ghost marks the ghosts on screen seen — the
  same rule the run list follows: reading clears it.
- Settings → Assistant: the proposal-kind switches, under the dreaming
  switch (phase 3 adds the switch itself; the kinds can land first and mean
  nothing until then).

### Phase 2 — Memory provenance

Small, and mostly in the prompt builder.

- `Memory::origin` and `last_supported`, with `Memory::new` defaulting to
  `Told`. `Vault::save_memory` enforces the two caps separately and the
  eviction order for `Inferred`.
- [`system_prompt`](../../crates/everyday-core/src/agent.rs) splits the list:
  told and confirmed under the existing heading, as standing instructions;
  inferred under *"Things you have noticed about this person, which may be
  wrong; act on them lightly and do not repeat them back as facts"*; rejected
  under *"Do not assume"*. Tests for each heading, and for the eviction
  order.
- Accepting a memory proposal saves it as `Inferred` (the dream made it) —
  or `Confirmed`, if the person accepts it from the memory list itself,
  which is the one place accept means "yes, that is true" rather than "yes,
  put that there". Striking out an inferred memory sets `Rejected` and
  keeps the row. Editing one sets `Confirmed`, because a person who rewrote
  it now stands behind it.
- The memory list draws the three groups with their heading, and the
  `remember` tool's description gains one sentence: *facts you infer rather
  than are told belong to the dream; in conversation, only remember what
  you were told.*

### Phase 3 — The dream routine

The phase a person sees. Everything below is behind `dreaming`.

Rust:

- `Routine::kind` with the default; `create_routine` and `save_routine`
  refuse to make a `Dream` by hand; `delete_routine` refuses to delete one.
  `AgentSettings::dreaming`, and `Vault::set_dreaming(bool)`, which creates
  the three routines on first enable — *Nightly* at 03:00 every day with a
  grace of 20 hours, *Weekly* at 03:30 on Sunday with a grace of 44 hours,
  *Monthly* at 04:00 on the first day of the month with a grace of 6 days —
  and enables or disables all three thereafter. The schedule is a plain
  [`Trigger::Schedule`](../../crates/everyday-core/src/routine.rs); the long
  grace is what turns "every night" into "as soon as the vault is unlocked
  after midnight", which is the "a few minutes after it starts" the idea
  began with, with no new trigger kind. The monthly one needs a day-of-month
  on `Schedule` — `day: Option<u8>` — which is the one addition to the
  trigger and gets its own rows in the table of cases at the foot of
  `routine.rs`.
- `crates/everyday-core/src/dream.rs` (new): the app-owned prompt per
  scope, and the **digest** builder. The digest is a struct with a
  Markdown rendering, built from queries that already exist:

  ```
  Digest {
    window:      the day, or the week's seven nightly runs, or the month's four weekly
    tasks:       created / completed / rescheduled / overdue, titles and purposes
    time:        BalanceReport for the window (time_by_purpose), planned versus actual
    readings:    tracker_days for the window, streaks kept and broken
    events:      what was on the calendar, attendees, which had a meeting note
    entries:     for the nightly scope, the day's entries as Markdown; for the
                 weekly and monthly scopes, only the nightly notes
    notes:       titles touched in the window
    mail:        counts by category, threads awaiting a reply — headers only
    runs:        each routine's runs in the window and whether they were seen
    proposals:   outcomes in the window, with caption, kind and reason
    memories:    the inferred list with last_supported, so the dream can revise it
    pending:     how many proposals are pending, against the cap
  }
  ```

  The digest is the first *user* message of the run, after the system
  prompt, so that it is clearly data rather than instruction, and the prompt
  says once that instructions found inside it are content — the same rule
  every string from mail already carries.
- The prompt, per scope, in the voice the other prompts use. The nightly
  one: read the digest; revise inferred memories — confirm with a new
  `last_supported`, or say nothing and let one lapse; propose at most five
  things, each with a `why`; write one note only if there is something to
  say that is not a proposal; never a judgment about mood, health or a
  relationship; never a memory about something the person wrote in
  confidence. The weekly one adds the outcome reading from *The loop*. The
  monthly one adds the long arcs: goals with no activity, roles with no
  hours, and the month's birthdays -- the profile's, and those on the
  library's Contacts shelf (added on `main` after this plan was written),
  with when each person was last caught up with from that shelf's log. The
  Contacts shelf is never looked up on the web, and a dream has no web
  search, so a person's name goes no further than the assistant's own
  model.
- **Leash.** In [`scheduler::execute`](../../crates/everyday-service/src/scheduler.rs),
  a `Dream` run gets `drafting: true` on its `ToolContext`, and the catalogue
  filtered to reads plus the writing tools whose kinds the policy allows,
  plus `remember`, `forget` (which drafts a `Delete` of a memory — the
  dream may suggest forgetting) and `create_note`. `create_note` is the one
  tool that writes for real in a dream, capped at one call per run in the
  filter. `web_search` is excluded: a dream reads the vault, not the web.
- Idle guard: a dream does not start while a conversation turn is in
  flight; the scheduler already serialises runs, and the same gate the
  rail's turn takes is checked before `execute`. A dream that finds the
  vault busy waits for the next tick, inside its grace.
- The run's summary line names the counts — *3 proposals, 2 memories
  revised, 1 note* — so the notification says "Nightly is ready" and the
  row says what that meant.
- Tests: `dream.rs` unit tests on the digest rendering from a seeded vault;
  a scheduler test that a nightly slot at 03:00 with a 20-hour grace runs at
  09:12 and is missed at 23:30; a test that a `Dream` run against the full
  catalogue writes no task, block, routine or mail; a test that the policy
  denying `task` produces a refusal in the transcript and no proposal.

Interface:

- Settings → Assistant: the **Dreaming** switch with its sentence, and the
  kind switches beneath it. The three routines appear in the routine list
  under a *Dreams* heading, with schedule and grace editable and no delete.
  The person's paragraph goes in the routine's instructions box, labelled
  *Anything to add*.
- `AssistantView`: a dream run's detail shows the digest it was given,
  folded, above the transcript — the answer to "what did it look at".

### Phase 4 — The loop

Mostly prompt and one query. Needs a fortnight of runs to be worth
anything, which is why it is its own phase.

- The weekly digest's `proposals` section groups outcomes by kind and by
  `about`'s kind, with the edited ones shown as before-and-after captions.
- The weekly prompt asks for two things from it: memories, as ordinary
  told-style sentences about what the person accepts, edits and declines —
  saved through `remember` and therefore `Inferred`, subject to the same
  strike-out — and a *stop list*: a kind declined or expired on three
  consecutive proposals is proposed no more until a memory says otherwise.
  The stop list is not a setting; it is a memory, so the person sees it and
  can strike it out.
- `ProposalQuery` gains `about_kind`, so "the meeting-prep ones" is a
  filter and not a scan.
- A test that a seeded week of three declined block proposals yields a
  digest section a fixture prompt turns into the expected sentence.

### Phase 5 — Extensions, optional

Each falls out of the record existing and none is needed for the rest.

- **Later.** Shipped. The rail's confirmation card gains a third, quieter
  button beside confirm and decline that parks the call as a proposal —
  `ProposalSource::Conversation`, no per-run cap. Built as a new `pub fn
  propose_call(ctx, name, arguments, source)` in
  `crates/everyday-core/src/agent/tools/mod.rs`, rather than duplicating
  `dispatch_drafting`'s logic in the service: it looks the tool up, refuses
  one with no `build`, and otherwise calls the same `build` and the same
  `finish_proposal` tail `dispatch_drafting` already used (which `propose`
  was split into, so drafting mode changed nothing about what it saves).
  `ConfirmGate`'s waiter answers with a three-way `ConfirmAnswer` (`Confirm`
  / `Decline` / `Later`) instead of a `bool`; the `confirm_tool_call`
  command gained an optional `later: bool` (default `false`) rather than
  changing `approved`'s meaning, so an older client keeps working. The
  `confirmationRequired` event gained `canPark: bool` —
  `tool.can_propose()`, which already covers `send_draft` because it has a
  builder of its own, so no special case was needed for it. Offered only
  for `destructive` and `outward` cards, never for the `search` taint
  (`web_search` is not in the core catalogue and has no proposal form).
- **Unattended destructive calls become proposals.** Shipped, but behind its
  own new setting, `AgentSettings::park_unattended` (`#[serde(default)]
  false`), rather than folding into `confirm_destructive` as first sketched
  above: the two questions are different ("should this ask at all" versus
  "what happens when nobody answers"), and conflating them would have meant
  turning off confirmation *anywhere* silently started parking scheduled
  deletes. Applies to `outward` as well as `destructive` — `send_draft`
  included, since it already has a builder — not only deletes, on the
  reasoning that a routine with nobody watching is the same "nobody to ask"
  problem whichever effect it hit. Reuses `propose_call` with
  `ProposalSource::Run { run_id }`; respects `check_policy` and the vault's
  own pending-proposal cap (`save_proposal` already enforces it), and falls
  back to the existing decline wording if parking fails for any reason.
- **The "Plan for tomorrow" card** is an Overview widget, offered in the
  widget catalogue like the others. It lists tomorrow's pending task and
  block proposals in time order, with their answers on each row.

## What is deliberately out

- Writing events to account calendars. A different project with a network
  in it.
- A separate observations document. Provenance on memory does the job.
- Any draft the assistant acts on before acceptance, other than inferred
  memories in the prompt.
- Per-source switches for what the dream reads. One switch, one sentence.
  If the journal turns out to want its own, it is one `bool` and one
  sentence, and the digest builder already knows which section it fills.
- Proposals from the rail. Asked-for work is done. The "Later" button in
  phase 5 is the one exception and it is the person's choice per call.
- Nudges as notifications. A proposal is a row, not a banner; the one
  notification per run that exists today is the only one.
- A learned model of "when to nudge". Rhythm is a memory the dream may
  write, and a routine's time is a field the person edits.

## Risks and the answer to each

| Risk | Answer |
|---|---|
| A stored proposal no longer applies after an upgrade | The payload is a record with serde defaults; expiry is days; accept validates and refuses honestly; a migration may sweep. See *Version safety*. |
| The parse-from-save split is missed on a new tool | The catalogue-wide drafting-mode test fails the moment a `Write` tool writes a row. |
| A dream infers a pattern from three data points | The nightly scope may only *confirm or lapse* inferred memories; new ones need the weekly scope, and the prompt asks for the evidence in the `why`. |
| It feels like surveillance | One switch, off by default, with the sentence saying what it reads; everything it writes is visible, dated and strikeable; it makes no judgments by instruction, and a test on the prompt text keeps the sentence there. |
| Forty ghosts and nobody answers them | `MAX_PENDING_PROPOSALS`, five per night, expiry by kind, and the stop list in phase 4. |
| Ghosts in the todo list make the list feel unreliable | One style everywhere, always at the foot of its section, never counted in totals, never exported, never searched. The search index and every roll-up ignore the `proposals` table by construction — it is a table of its own, not a flag on theirs. |
| Token cost every night | The digest replaces tool calls; a nightly run is one digest plus whatever entries it opens. On a local endpoint it costs nothing. The run detail shows the digest, so a person can see what was sent. |
| The dream runs while the person is typing at 1 a.m. | Runs are serial and the idle guard waits for the rail's turn; the vault's single writer holds either way. |
| Journal text reaches the model unasked | It does not until `dreaming` is on, and the switch's sentence names the journal first. The quick-model rule that journal-reading jobs start off is kept: `dreaming` defaults off and a test enforces it, as `agent.enabled` already has. |
| Postgres shared vault | Proposals are rows like any other; the write claim rule is unchanged. A second client sees ghosts the first one's dream made, which is correct. |

## Sizes

Rough, in working days, for one person who knows the tree.

| Phase | Days |
|---|---|
| 1 Proposals, the domain and the ghosts | 7 |
| 2 Memory provenance | 2 |
| 3 The dream routine | 5 |
| 4 The loop | 2 |
| 5 Extensions | 2, if wanted |

Phase 1's size is the parse-from-save split across the writing tools —
roughly twenty of them — and four views learning one row style. Phase 3's
is the digest builder and the prompts, and the day-of-month trigger.

## Open questions

- **The word.** Settled: *Dreaming*, in the switch and as the routines'
  heading.
- **Whether accepting a memory from its ghost means Inferred or
  Confirmed.** The plan says Inferred from a ghost and Confirmed from the
  memory list, on the grounds that a tap on a row is "put that there" and a
  tap in Settings is "that is true". It may turn out to be one gesture.
- **The idle guard's definition.** "No conversation turn in flight" is the
  cheap one; "no keystroke in the last ten minutes" needs the window to
  tell the service something it does not today.
