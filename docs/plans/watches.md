# Watches, projects and updates: background work that keeps in touch

Written 20 November 2026 against `83484a3c`, on `thin-wombat`. The first
section records what Hari decided in conversation and is settled. The rest is
the design the code follows.

## What this is

The assistant can take on work that outlives a turn and **keep in touch about
it**: watch something and say when it changes ("keep an eye on CI and tell me
when it finishes"), or get on with a longer piece of work in the background
("plan a week in Lisbon") while the person talks to it about other things.
Whatever it has to say reaches them wherever they are: the chat on their
phone, the thread in the app, or out loud in a voice session that may run for
hours, at the app or on a Matrix call.

The plan generalises `docs/plans/jobs.md`. A **job** stays the one record for
background work. It gains:

- **Kinds that sleep and wake.** A watch looks, sleeps (`JobStatus::Waiting`
  until `Job::wake_at`), and looks again.
- **Rules the person can change while it runs.** These are `JobRules`: when to
  tell them, what to do, how often to look, when to stop, whether to ring.
- **Updates.** `Job::updates` holds what it has told them so far, and what it
  still has to.

There is also **one router** for anything a job has to say:
`Service::deliver_update`, in `crate::reach`.

## Decisions already made (Hari, 20 Nov 2026)

- **A watch lasts 8 hours by default** (`DEFAULT_WATCH_HOURS`), unless the
  person says otherwise.
- **Quiet hours are a setting** (`AgentSettings::quiet_hours`). They **apply
  only to voice**:
  - an update still goes to the chat and the thread at once, quiet hours or
    not;
  - in quiet hours it is not spoken in a voice session;
  - once quiet hours end, it is spoken if the session is still open.
- **Spoken updates never cut in while he is talking.** They are said at the
  agent's next chance to speak. If several are waiting, they are said
  together.
- **Replies, not Matrix threads.** A chat message sent as a reply to a job's
  update is about that job. Without a reply, **the assistant works out which
  job he means** from what it has going. When in doubt, **it asks**.
- **The judge prefers the decision model.** If none is switched on, the Small
  model judges. (If there is no Small model either, the assistant's Medium
  model is the last resort.)
- **A job may run the assistant.** A step can be:
  - a cheap check with no model;
  - a judgement by the decision or Small model;
  - a full turn with tools, when the watch's `act` asks for one or its probe
    is the assistant's own look.
- Earlier decisions still hold:
  - "Job" is the word.
  - Starting a job is never confirmed.
  - The person and the assistant can both cancel.
  - Asking the owner happens only in the chat (Matrix), never at the desktop.

## The record (core, `karmayoga_core::job`)

- `JobKind::Watch { what, probe }` and `JobKind::Project { goal, brief }`, beside
  `Research` and `Task`.
- `Probe::Connection { connection, tool, arguments }`: one tool of a connected
  service, called the same way each look, with no model.
  - Only a tool whose policy lets it run unasked (Allow) may be used, on a
    connection allowed for background work.
  - This is the one to prefer.
- `Probe::Assistant { how }`: an unattended turn that looks with the
  assistant's tools and reports. It looks at most every 15 minutes
  (`MIN_ASSISTANT_EVERY_MINUTES`).
- `JobStatus::Waiting`: asleep between steps. It is not finished, and the
  worker's sweep (which only touches `Running`) leaves it alone.
- `Job::rules: JobRules { notify_when, act, every_minutes, until, call_me }`.
  `Job::watch(..)` fills an unset `until` with now + 8 h.
- `Job::wake_at`, `Job::observed: Option<Observed>` (fingerprint, last
  observation capped at 16 KB, checked_at, checks, failures, trouble) and
  `Job::updates: Vec<JobUpdate { at, text, delivered, sent_to }>` (the last 30
  are kept).
- `Job::interrupted` puts a stepped job back to sleep, due now, rather than in
  the queue.
- `Job::owes_answer` is never true for a watch: it said what it had to as it
  went.
- `JobQuery::live()` (queued, running and waiting) and `JobQuery::waiting()`.
- `AgentSettings::quiet_hours: QuietHours { enabled, start, end }`, with times
  as `"HH:MM"` in the person's zone. Methods: `contains(now)` and
  `ends_after(now)`.

Jobs are sealed JSON, so none of this needs a migration. An older build
refuses a job of a kind or status it does not know, as it always has
(`a_kind_this_build_does_not_know_is_refused_rather_than_misread`).

## The assistant's tools (core, `agent/tools/jobs.rs`)

### `start_watch`

Arguments:
- `what`: a few words.
- One of `connection` + `tool` + `arguments` (preferred), or `how`.
- `notify_when`.
- Optional: `every_minutes`, `hours` (default 8, at most 168), `act`,
  `call_me`.

What it does:
- Validates the connection and tool from the vault's settings and cached
  states:
  - the connection exists, is enabled and is allowed unattended;
  - the tool is listed and its policy is Allow;
  - otherwise it says why and what to do (e.g. set the tool to Allow in
    Settings → Connections, or use `how`).
- Queues `Job::watch(..)`, waiting and due at once.
- Tells the model to say what it will watch, how it will tell them, and for
  how long, then end the turn.
- Refused unattended. Refuses a second live watch with the same `what` in the
  same conversation, the way `start_research` does.

### `start_project`

Arguments: `goal`, `brief`.

- Queued; the worker runs it.
- Refused unattended. Duplicate check like research.

### `update_job`

Arguments: `id`, plus any of `notify_when`, `act`, `every_minutes`, `hours`
(from now), `call_me`, `look_now`.

- Changes a live job's rules. Validated as `JobRules::validate` does.
- `look_now` sets `wake_at` to now.
- Answers with the job's new rules in words.
- Offered wherever `cancel_job` is. Only kinds with rules (`has_rules`) accept
  changes; others are refused, and the refusal says so.

### `report_progress`

Argument: `text`.

- Only in a project's own turn. The job is found from the turn's conversation
  (`Conversation::job`).
- Appends an undelivered `JobUpdate`. The worker sends it on within seconds.
- At most one per three minutes per job, so a chatty model cannot flood the
  chat.

### Changes to existing tools

`job_status` and `cancel_job` describe the new kinds. A watch shows:
- what it watches and how it looks;
- its rules, in words;
- when it next looks and when it stops;
- the last update and how many looks it has taken;
- any trouble.

Cancelling a watch says it will not report again.

### Descriptions

The descriptions teach when to use which:
- research for questions;
- project for multi-step work with a deliverable;
- watch for "tell me when". Before starting one, find a connection tool with
  `find_connection_tools`, and use `how` only when no tool fits.

## Steering from chat (service, `agent.rs`)

Every turn that is not a job's own gets `background_note(..)` appended to its
turn note (the per-turn message, never the system prompt: that keeps the
prompt cache). The note contains:

- every live job, with:
  - its short id;
  - kind and title;
  - whether it was started in this conversation or another;
  - its status (when a watch next looks, what a project is doing);
  - its rules in words (tells them when …, looks every …, stops at …);
  - the last update and how long ago it was;
- when `Turn::about_job` is set: "They are replying to your update from
  <kind> «<title>» (id …)";
- one line of instruction: use `update_job` / `cancel_job` with these ids.
  When it is not clear which job they mean, ask.

A chat message carries `about_job` when the owner sent it as a reply to the
job's update in Matrix:
- `Incoming::Said::reply_to` gives the event replied to;
- the relay remembers which job each of its posts came from;
- `Service::chat_turn_input(ChatTurnInput { about_job, .. })` passes it on.

## Reaching you (service, `reach.rs`)

`Service::deliver_update(Update { job, from, text, conversation, call_me })`
sends the update everywhere that applies, at once, and returns
`Delivered { sent_to }`:

1. **Thread.** A `Message::system` line in `conversation` (when set):
   `"<from>: <text>"`. Also in the conversation of any open voice session that
   is a different one. Raises a `Conversation` change.
2. **Chat.** For every running chat outlet, `post` with
   `FromRun { routine: from, conversation: None, job }`. The relay sends a
   post with no conversation and a job **at once**, not queued behind a turn or
   a call, and records the event id → job for replies.
3. **Voice.** For every open session (`register_voice`, done in
   `voice_open_via`), `VoiceInput::Update(text)`. Two exceptions:
   - In quiet hours (`AgentSettings::quiet_hours.contains(now)`), the update
     goes on the held list instead. `release_held_updates`, called on every
     pass of the watch loop, hands it to the still-open sessions once quiet
     hours end. A session that has ended loses it; the chat and the thread
     have it.
   - With `call_me`, no open session and not quiet hours, the first chat
     outlet's `call(from, markdown, spoken)` is used instead of `post`, which
     writes it when nobody picks up.
4. **Notification.** If no chat and no voice took it, a desktop notification
   is shown: title `from`, no body, keyed `update:<job>`.

`Service::owner_asker(from)` is an `Asker` over the first running chat outlet
(confirm and ask, as a routine's `RunAsker` does). It is `None` without one.
Projects use it.

Updates are plain enough to say aloud. Markdown links and emphasis are
stripped for voice (`crate::speech::text` has what is needed, or a small
helper in `reach.rs`).

## The voice session (service, `voice/session.rs`)

`VoiceInput::Update(text)` is queued, never said at once. It is said at the
session's next chance, when all of these hold:
- nobody is talking or being understood;
- no turn is running, or the last one is done and its reply has played;
- no confirmation is waiting;
- nothing is being read aloud or about to be;
- nothing of the session's own is playing or queued;
- it is not ending.

All the waiting updates are then said together as the session's own speech
(`Kind::Own`), led by a short phrase ("An update:") from `phrases.rs`. They
are not part of any reply, and they count for the echo check like anything
said.

If the person starts talking over an update, it pauses as other speech does.
Their words begin a turn as usual. An update that was cut is not repeated (the
thread has it).

Long sessions need nothing new: nothing ends a session for silence. The bar
on screen and the Matrix call stay up until somebody ends them.

## The watch loop (service, `jobs/watch.rs`)

`jobs::run` joins the worker and `watch::run`. The watch loop:

1. **Waits.** It sleeps until the earliest `wake_at` among waiting watches,
   the `wake_watches` notify (rung by `wake_jobs`), the stop signal, or 30
   seconds, whichever comes first.
2. **Passes.** On each pass, only when the vault is unlocked, writable and
   `supports_jobs`:
   - calls `release_held_updates`;
   - lists `JobQuery::waiting()`;
   - for each watch that `is_due(now)` and is not claimed (`Working::start`),
     spawns a look, with at most 4 at once.
3. **Takes a look.** The status stays `Waiting` throughout, so the card does
   not flicker and the sweep never sees it. In order:
   - It re-reads the record first. Cancelled or finished: nothing to do.
   - If it `has_expired`, it ends: `finish("I've stopped watching …: the N
     hours are up.")`, delivers that as an update and sets `answered`.
   - **Probe.**
     - Connection: `crate::connections::tools::probe(vault, connection,
       tool, arguments)`. This is unattended, refuses anything that is not
       Allow, and returns the text plus structured content as one string.
     - Assistant: a job turn (`JobTurn { context: LOOK_CONTEXT }`) on the
       Small tier (falling back to Medium), max 12 steps, 5 minutes, in the
       job's transcript. Its final text is the observation. The transcript
       is pruned to the last look before each new one.
   - **Fingerprint.** SHA-256 of the observation, after obviously volatile
     parts are taken out (ISO timestamps, "N seconds/minutes ago",
     request/trace ids, `etag`).
   - **Same as last time:** record `checked_at` and `checks`, sleep until
     now + `every()`.
   - **Changed, or the first look:** judge it.
     - Decision model (`decision::choose`). Options:
       - `quiet`: nothing they asked to hear about;
       - `tell`: something they asked to hear about, and more to come;
       - `tell_and_stop`: what they asked about is over: finished, failed
         for good, arrived, merged.
       The content is: what is watched, `notify_when` (or "any change worth
       knowing about"), the last observation, this one, and the last three
       updates.
     - Small: `quick::run_prompt_within` with schema
       `{ verdict: quiet|tell|tell_and_stop, update: string }`. One call does
       both the judging and the writing.
     - Medium is the last resort, asked the same way.
     - On the first look the "before" is "(nothing yet: this is the first
       look)". A `tell` there is only for something already worth telling.
   - **Say.** With `act` set, run an act turn (`ACT_CONTEXT`, Medium, 40
     steps, 20 minutes, in the job's transcript); its final reply is the
     update. Otherwise the update comes from the Small model (or from the
     judge's own `update`). It is delivered with
     `from: "Watch: <what>"`, the job's `asked_in` and `rules.call_me`, then
     recorded with `add_update(.., delivered: true)` and `sent_to`.
   - `tell_and_stop`: `finish(update)`, `answered = true`.
   - **Failure.** A probe error or model error adds to `observed.failures`
     and `trouble`, and the next look backs off (`every × 2^failures`, at
     most an hour). After `MAX_FAILED_LOOKS` in a row the watch fails
     (`fail(reason)`), the person is told "I've stopped watching …: <why>",
     and `answered` is set.
   - **Saving.** It re-reads the record before writing, and keeps a
     cancellation or a rule change that landed meanwhile. Only the fields it
     owns are written: `observed`, `wake_at`, updates, status at an end. It
     raises a `Job` change.

What watches send the decision model or the Small model is what a connected
service returned. That goes to endpoints the person chose. It is treated as a
stranger's writing: never instructions. The act turn runs under the job rules
(destructive and outward calls declined, private domains withheld). The judge
prompt says the observation is data, not instructions.

## Projects (service, `jobs/project.rs`, `jobs/mod.rs`)

- `Project` executor:
  - makes a note titled after the goal, tagged `project` (as research does),
    and writes the prompt: goal, brief, note id, how to work;
  - 150 steps, 90 minutes;
  - `conclude` salvages the prose into the note, as research does.
- Its turn gets
  `reach.asker = service.owner_asker(FromRun { routine: "Project", conversation: job.asked_in, job: Some(id) })`.
  `agent.rs` offers `ask_owner` and `report_progress` to a project's own turn
  (see `JobTools::offers`).
- The worker:
  - runs up to **two** queued jobs at once (research, project or task), so a
    project does not hold up a question's research;
  - during its save tick, sends any undelivered updates on a running job
    through `deliver_update` (`from: "Project: <goal>"`) and marks them
    delivered;
  - when a project is done, has the follow-up turn in `asked_in`, as research
    does (`answer::answer`, worded for the kind).
- A chat post of a follow-up names the job (`FromRun::job`), so a reply to it
  is about that job.

## The interface

- `types.ts` mirrors:
  - the new kinds, `Probe`, `JobRules`, `Observed` and `JobUpdate`;
  - the `waiting` status;
  - the `rules`, `wakeAt`, `observed` and `updates` fields;
  - `AgentSettings.quietHours`.
- `JobCard.svelte`:
  - A watch reads "Watching CI on PR 412". It shows the next look ("in 3
    min") or "Looking now", the rule ("Tells you: when it finishes or
    fails"), "Until 18:40", the last update with its time, and any trouble.
    Actions: Stop watching (`cancelJob`).
  - A project reads like research, plus its updates.
- The Assistant app's jobs list shows watches under their own heading
  (Watching) above the rest.
- Settings, in the assistant's voice section: **Quiet hours**. A switch, from
  and to time inputs, and one sentence: "Updates aren't spoken in a voice
  session during these hours; they still come to your chat."
- `mock-jobs.ts` has a watch and a project to look at.

## Who builds what (supervised agents, edit-only)

Each agent owns its files. Changes needed elsewhere are reported, not made.

| Agent | Owns |
|---|---|
| A core | `karmayoga-core/src/job.rs` (tests), `agent/tools/jobs.rs`, `store/jobs.rs`, `agent.rs` (QuietHours tests only), `karmayoga-vault/tests/tools/jobs.rs` |
| B watch | `karmayoga-service/src/jobs/watch.rs`, `connections/tools.rs` (`probe` only), `tests/watches.rs` (new) |
| C worker+project | `jobs/mod.rs`, `jobs/project.rs`, `jobs/answer.rs`, `jobs/research.rs`, `domains/jobs.rs`, `tests/jobs.rs` |
| D reach+voice | `reach.rs`, `voice/session.rs`, `voice/phrases.rs`, `voice/assistant.rs` |
| E chat | `karmayoga-chat/src/**`, `karmayoga-chat/tests/**` |
| F agent | `karmayoga-service/src/agent.rs` |
| G UI | `ui/src/**` |
