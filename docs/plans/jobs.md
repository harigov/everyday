# Jobs: work the assistant does in the background

A plan for letting the assistant take on a question that needs real time —
an hour of reading rather than three searches — say so, go away and do it,
and come back to the conversation that asked with an answer. Research is
the first kind of job; reaching the web through a browser is the second,
and the shape below is chosen so that the second is an executor, not a
redesign. Written 12 October 2026 against the tree at `ac284d1`, on
`wealthy-pelican`. The first section records what was decided in
conversation and is settled; the phases after it are the part to argue
with.

## What this is

1. **A record, the `Job`.** One piece of background work: what kind it is
   and its arguments, the conversation that asked for it, where it stands,
   what it produced, and its own transcript. Sealed in the vault like
   everything else, and listed in the Assistant app beside routine runs.
2. **Three tools.** `start_research`, `job_status` and `cancel_job`. The
   first writes a queued job and returns at once, so the assistant can say
   "I've started looking into this; I'll come back here when it's done" and
   the turn ends.
3. **A worker.** One loop in `everyday-service` that picks queued jobs up
   and runs each through the same unattended turn a routine run uses, with
   a prompt, a budget and a gate policy that depend on the job's kind.
4. **Coming back.** When a job finishes, the conversation that asked for it
   gets a line saying so and a follow-up turn in which the assistant reads
   what the job produced and answers the original question — in the rail,
   or in the chat on somebody's phone if that is where they asked.

## Decisions already made

- **"Job" is the word.** `QuickJob` was renamed `QuickAsk` first, so that
  the word means one thing; the settings field is read under its old name
  too (`quickJobs`), so nobody's switches reset.
- **A research job has the whole tool catalogue,** exactly as a routine
  run does, under the same unattended rules: a destructive call is refused
  (or parked, with `park_unattended`), and the two web safeguards — a
  `web_search` after mail has been read this turn, a `read_web_page` to an
  address the model composed — refuse rather than ask, because nobody is
  there to answer. The job is told why and carries on.
- **Starting one is not confirmed.** The person asked a question; the
  assistant judged it needed time. What they get instead is a way out:
  the assistant can cancel a job (`cancel_job`), and so can they, from the
  card in the thread or from the Assistant app.
- **The answer arrives on its own.** A finished job is followed by a turn
  in the asking conversation without anybody pressing anything.
- **More kinds will follow,** a browser first. Everything that is not
  specific to research — the record, the queue, the worker, progress,
  cancel, the transcript, the notification, coming back — is shared.

## The record

```rust
pub struct Job {
    pub id: JobId,
    pub kind: JobKind,
    /// Where to come back to. `None` for a job somebody started from the
    /// Assistant app or the CLI rather than from a conversation.
    pub asked_in: Option<ConversationId>,
    pub status: JobStatus,
    /// Why it failed or was cancelled. Empty when it simply worked.
    pub reason: String,
    /// One line for the card: "Read 14 sources; writing up".
    pub progress: String,
    pub steps: u32,
    /// What it made, in order. A research job's note goes in here the moment
    /// the note exists, not at the end -- see "The note is the scratchpad".
    pub outputs: Vec<JobOutput>,
    /// The job's own transcript, kept out of the rail's history the way a
    /// run's is.
    pub conversation_id: Option<ConversationId>,
    /// Whether the follow-up turn has been had. Separate from `status` so a
    /// restart between the two can still come back.
    pub answered: bool,
    pub seen: bool,
    pub created_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
}

#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum JobKind {
    Research {
        /// The question in the person's words.
        question: String,
        /// What the asking assistant knows that the job should: who is
        /// asking, what for, what they already ruled out. Written by the
        /// model that started it.
        brief: String,
    },
    // Browse { goal: String, start_url: Option<String> },
}

pub enum JobStatus { Queued, Running, Done, Failed, Cancelled }

#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum JobOutput {
    Note { note_id: NoteId },
    // Value { text: String }, Screenshot { blob: BlobId }, for the browser.
}
```

Its own status enum rather than routine's `Outcome`: `Skipped` means
something precise for a routine (its moment passed) that a job never is,
and `Cancelled` means something a routine never is.

`Conversation` grows `job_id: Option<JobId>`, beside `run_id`, so a job's
transcript reads in the Assistant app and stays out of the rail's list.

A table in the SQL stores with the store conformance suite over it. The
Markdown backend stores no jobs, and the tools are not offered there, the
way it stores no tasks.

## The tools

In the core catalogue under a new `Domain::Jobs`, offline and under test —
none of them opens a socket; they write and read records.

- **`start_research { question, brief }`** queues a job, wakes the worker,
  and answers with its id. Offered only when the web is switched on,
  because research without `web_search` is not research. The prompt says
  when to reach for it: a question that needs more than a few searches to
  answer well, or a person asking for research outright — and to say, in
  the same reply, that it has started and will come back here.
- **`job_status { id? }`** answers one job, or the recent ones.
- **`cancel_job { id }`** stops it. A queued job is cancelled on the spot;
  a running one through the same path `cancel_turn` uses, ending
  `Cancelled` with whatever it had written kept.

The same three as commands (`list_jobs`, `cancel_job`, `mark_jobs_seen`)
for the interface, the CLI and MCP.

## The worker

`everyday-service/src/jobs.rs`: one loop spawned beside the scheduler, not
inside it. A routine's tick runs serially and a twenty-minute research
must not make the seven o'clock brief late.

- **Woken, not polled.** `start_research` notifies it; it also looks once
  on start and on every unlock.
- **One job at a time.** A second research waits its turn. Jobs do run
  alongside the rail and the scheduler, as a routine already runs
  alongside the rail.
- **Nothing while the vault is locked.** A job the lock interrupts, or a
  restart, goes back to `Queued` once and starts over — reusing its note if
  it already made one. Interrupted twice, it fails and says why.
- **A budget per kind.** Research gets more steps than the rail's ceiling
  (a constant to start with, sixty) and a longer timeout than a routine's
  fifteen minutes (thirty). `Turn` grows a step override for this.
- **Progress from the ledger.** Each tool call the job makes updates
  `progress` and `steps` and raises a `Kind::Job` change, throttled, so an
  open card moves.

What changes per kind is an executor:

```rust
trait JobExecutor {
    fn prompt(&self, job: &Job) -> String;     // first message of the transcript
    fn budget(&self) -> Budget;                // steps, timeout
    fn context(&self) -> &'static str;         // replaces "what is on screen"
    fn outputs(&self, job: &Job, turned: &Turned) -> Vec<JobOutput>;
}
```

The turn itself is the one routine runs already take. `scheduler::resume`
has the body worth sharing — claim, transcript, timeout, announce, raise
what it wrote — and is lifted into one function both callers use, with
`Turn::unattended` becoming an enum naming either a run or a job.

## Research

**The prompt** asks for a plan first (`update_plan`, which the transcript
already draws as a checklist), then wide searching, then reading, then
writing — and for the note to say what is known, how sure, from where, and
what is still open, with every source linked.

**The note is the scratchpad.** The job creates its note at the start,
titled after the question and tagged `research`, and keeps it current as
it goes. A job that times out, fails or is cancelled has still left what it
found where the person can read it, and the restart rule above has
something to resume from.

**Pages are read through the quick model.** A page is up to twenty thousand
characters and a research job reads dozens; the conversation is resent on
every step, so reading them raw costs quadratically and outgrows most
models' context. When the quick model is configured, `read_web_page` in a
research job hands the model a QuickAsk's extract instead: what on the page
bears on the question, with the quotes that say so. A new catalogue entry,
`research.extract`, on by default — it sends a public page and the
question. Without a quick model the page is trimmed harder instead.

**Parallel sub-questions are later.** One loop first. The record leaves
room for a job to have a parent, which is what fanning a question out into
several jobs would need.

## Coming back

When a job ends `Done` and `asked_in` is set:

1. A `System` message goes into that conversation: "Research finished:
   *title*", carrying the job's id so the interface can draw it as a card.
2. A follow-up turn runs there. Its input is a system line — the job's id,
   its note, "answer the question they asked" — not a user message: the
   thread must not show the person saying something they did not.
3. If that conversation has a turn in flight, the follow-up waits for it.
4. In the rail, the open thread refreshes from the `Kind::Conversation`
   change that ends every turn; streaming the follow-up live into an open
   thread is a later nicety. Through a chat link, the answer goes back to
   that chat — which needs an outbox: today a connector only ever replies
   to a message it was handed, so the service gets a way to send one to a
   link's running task.
5. `answered` is set. If the follow-up fails, the job is still `Done`, the
   notification still says it is ready, and the card offers "Answer now".

Whatever happened, a notification — "Research is ready: *title*", keyed on
the job — and the Assistant app's count, through `seen`. A cancelled job
does not come back: whoever cancelled it already knows.

## The interface

- **A card in the thread** where the job was started: its question, the
  progress line, Cancel while it runs, Open note and "See what it did"
  (the transcript) once it has something.
- **The Assistant app** lists jobs beside routine runs in "What it did",
  with the same unread count.
- **No new settings** to begin with. The web switch gates the tool; the
  budget is a constant until there is a reason to expose it.

## The browser, later

`JobKind::Browse { goal, start_url }` with its own executor: a headless
Chromium over the DevTools protocol in a profile of its own, tools to open,
read, click, type and take a screenshot, and outputs that can be a value or
a screenshot as well as a note. What it adds that research does not is a
stricter gate: submitting a form, signing in or paying is parked for the
person rather than done, whatever `park_unattended` says. Everything else
in this plan it inherits.

## Phases

0. **Rename `QuickJob` to `QuickAsk`.** Done, as its own commit.
1. **The record and the tools.** `Job`, its store and conformance suite,
   `Conversation::job_id`, the three tools and the commands. Testable with
   no worker at all: starting a job queues it, cancelling a queued job
   cancels it.
2. **The worker and research.** The shared unattended turn lifted out of
   the scheduler, the loop, the research executor and prompt, the step
   override, progress, the restart rule, `research.extract`.
3. **Coming back.** The follow-up turn, the per-conversation wait, the
   notification, and the chat outbox.
4. **The interface.** The card, the Assistant app's list, the mock.
