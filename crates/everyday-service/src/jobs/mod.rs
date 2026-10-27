//! The worker that runs the assistant's background jobs.
//!
//! See `docs/plans/jobs.md`. One loop, beside the routine scheduler rather
//! than inside it, so a twenty-minute research cannot make the seven o'clock
//! brief late. Woken by [`Service::wake_jobs`] when a job is queued, and on
//! its own now and then in case a wake was missed.
//!
//! # Rules it keeps
//!
//! **One at a time.** The oldest queued job runs to its end before the next
//! is looked at. It runs alongside the rail and the scheduler, as a routine
//! already does, but not alongside another job: two long model turns at once
//! would double the bill for no one's benefit.
//!
//! **Nothing while locked.** A locked vault has no key and a read-only one
//! cannot be written to, and a job writes constantly. Whatever the lock
//! interrupts goes back to the queue (once; see [`everyday_core::job::MAX_ATTEMPTS`])
//! and starts over when somebody unlocks.
//!
//! **A row that says `Running` and is not ours is somebody else's corpse.**
//! The app was quit, the machine slept, the vault was switched. The sweep at
//! the top of every pass puts such a job back in the queue, or fails it if it
//! has been started too often already. Our own is told apart by
//! [`Service::running_job`], not by how old it looks.
//!
//! **A turn is never dropped from a `select!`.** The same rule the scheduler
//! keeps: a turn cancelled half way would leave a row saying `Running` with
//! nothing to finish it. Stopping, cancelling and timing out all ask the turn
//! to end and wait for it, except the timeout, which is a deliberate drop and
//! is recorded as one.
//!
//! **Coming back is off the worker's path.** A job that ends owes the
//! conversation that asked: an answer if it is done (`answer::answer`), a line
//! saying so if it failed (`answer::report_failure`, no model). Both are
//! spawned rather than awaited, because the first waits for the thread to go
//! quiet, and both are offered again on every pass for any job that still
//! owes, so one a crash skipped is caught.
//!
//! # What is shared and what is research
//!
//! Everything here is generic: the queue, the sweep, the record's life from
//! `Running` to an end, progress, cancelling, the transcript, the
//! notification. What changes with the kind of job is an [`Executor`]: what to
//! set up before the turn (research makes its note), the first message, the
//! budget, and what to check when the turn is done. [`research`] is the only
//! one; the browser the plan describes would be a second.

pub mod answer;
pub mod research;
pub mod task;

use crate::agent::{AgentEvent, JobTurn, Sink, Turn, Turned};
use crate::events::{Change, Kind, Level, Notification, Op};
use crate::service::{Service, blocking};
use everyday_core::{
    Conversation, ConversationId, Job, JobId, JobKind, JobQuery, JobStatus, Vault,
};
use jiff::Timestamp;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;

/// How long the loop waits for a wake before looking anyway.
///
/// A job is queued by a tool call or a command in this same process, and
/// both ring [`Service::wake_jobs`], so this is not what normally starts one.
/// It is what finds the job a wake was lost for, the ones another device
/// queued, and everything there is to do the moment a vault is unlocked.
const FALLBACK: Duration = Duration::from_secs(30);

/// How often a running job's record is read to see whether somebody has
/// cancelled it. A cancel is answered within this, which is as fast as a
/// person pressing a button needs and slow enough to cost nothing.
const WATCH_EVERY: Duration = Duration::from_secs(2);

/// The most often a job's progress is written to the vault, and so the most
/// often an open card is told to redraw. A research job makes a tool call
/// every few seconds; saving and announcing each one would be a write and a
/// window repaint for a line nobody can read that fast.
const SAVE_EVERY: Duration = Duration::from_secs(3);

/// How far back a finished job is still owed a follow-up. An answer that
/// arrives a week late is not an answer, and bounding the look keeps a pass
/// from reading every job there ever was.
const OWED_FOR_DAYS: i64 = 7;

/// Why a job found `Running` with nobody carrying it out went back to the
/// queue.
const INTERRUPTED: &str = "the app stopped while it was running";

/// The worker's session state, held by [`Service`].
#[derive(Default)]
pub struct JobRuntime {
    /// Rung by [`Service::wake_jobs`]; the loop waits on it between passes.
    pub(crate) wake: tokio::sync::Notify,
    /// The job being worked on right now, if any.
    pub(crate) running: Mutex<Option<JobId>>,
    /// The jobs a task is coming back to a conversation for right now, so
    /// that the next pass does not start a second. `answer` keeps its own
    /// claim and its own pause after a failure; this only keeps the worker
    /// from spawning a task a pass for a follow-up that is waiting its turn.
    coming_back: Mutex<HashSet<JobId>>,
}

impl JobRuntime {
    /// Take the right to come back for `job`: false if a task is on it.
    fn begin_answer(&self, job: JobId) -> bool {
        self.coming_back.lock().unwrap().insert(job)
    }

    fn end_answer(&self, job: JobId) {
        self.coming_back.lock().unwrap().remove(&job);
    }
}

impl Service {
    /// Tell the worker there may be a job to pick up. Cheap, and harmless
    /// when there is none.
    pub fn wake_jobs(&self) {
        self.jobs.wake.notify_one();
    }

    /// The job the worker is on right now, if any.
    pub fn running_job(&self) -> Option<JobId> {
        *self.jobs.running.lock().unwrap()
    }
}

/// What a kind of job brings to the generic worker.
///
/// The plan's own sketch had four methods; the shape here is the same with
/// the first message folded into the setup, because for research the prompt
/// needs the note's id and the note is made by the setup.
pub(crate) trait Executor: Send + Sync {
    /// The most tool calls one turn may make.
    fn max_steps(&self) -> u32;

    /// The longest one turn may take before it is abandoned.
    fn timeout(&self) -> Duration;

    /// What the turn is told in place of what is on screen. See
    /// [`crate::agent::JobTurn::context`].
    fn context(&self) -> &'static str;

    /// Make whatever the job writes into, if it is not there already, and
    /// say what the first message of its transcript is. Called on every
    /// attempt, so a job started again finds what the last one left rather
    /// than making it twice. May change `job` (research adds its note to
    /// the outputs); the caller saves it.
    fn prepare(&self, vault: &Vault, job: &mut Job) -> everyday_core::Result<Prepared>;

    /// The turn is over and `reply` is everything the model said in it --
    /// the steps on the way as well as its last word, which is the job's
    /// summary. Check that the job left something worth the name, and mend
    /// it if it can be mended. An error fails the job, with the error as the
    /// reason.
    fn conclude(&self, vault: &Vault, job: &Job, reply: &str)
    -> everyday_core::Result<Vec<Change>>;

    /// The job is over -- done, failed or cancelled -- and written down as
    /// such. Whatever else should move with it, moved; what was written, for
    /// open windows. Nothing, for a kind with nothing to move.
    fn ended(&self, _vault: &Vault, _job: &Job) -> Vec<Change> {
        Vec::new()
    }
}

/// What [`Executor::prepare`] hands back.
pub(crate) struct Prepared {
    /// The first message of the job's transcript.
    pub prompt: String,
    /// What it wrote while setting up, for open windows to hear about.
    pub changes: Vec<Change>,
}

/// The executor for a kind of job.
fn executor_for(kind: &JobKind) -> &'static dyn Executor {
    match kind {
        JobKind::Research { .. } => &research::Research,
        JobKind::Task { .. } => &task::TaskJob,
    }
}

/// Run the worker until `stop` says otherwise. Spawned once beside the
/// scheduler, on the runtime the rest of the process uses.
pub async fn run(service: Arc<Service>, mut stop: watch::Receiver<bool>) {
    tracing::info!("the assistant's job worker is awake");
    loop {
        if *stop.borrow() {
            break;
        }
        // Deliberately *not* inside the `select!` below, for the reason the
        // scheduler gives: a pass cancelled half way would drop a live model
        // turn. Stopping is asked of the turn instead -- see `execute` -- and
        // this waits for it to end.
        let worked = pass_with(&service, Some(stop.clone())).await;
        if *stop.borrow() {
            break;
        }
        // A job that ran to its end may have left another behind it, and
        // there is no reason to make it wait for the fallback.
        if worked {
            continue;
        }
        tokio::select! {
            _ = service.jobs.wake.notified() => {}
            _ = tokio::time::sleep(FALLBACK) => {}
            _ = stop.changed() => break,
        }
    }
    tracing::info!("the assistant's job worker has stopped");
}

/// One pass: sweep what a dead process left, answer what is owed, and run
/// the oldest queued job to its end. Public so a test can drive it.
pub async fn pass(service: &Arc<Service>) {
    pass_with(service, None).await;
}

/// [`pass`], with the loop's stop signal when it has one. Returns whether a
/// job ran to an end, which is what tells [`run`] to look again at once.
/// A job put back in the queue is not an end: looking again immediately
/// would only pick it up to be interrupted in the same way.
async fn pass_with(service: &Arc<Service>, stop: Option<watch::Receiver<bool>>) -> bool {
    let Some(vault) = service.get() else { return false };
    // Nothing while locked, and nothing in a second copy of the application
    // that holds no write claim. Not even the sweep: it writes.
    if !vault.is_unlocked() || !vault.is_writable() || !vault.supports_jobs() {
        return false;
    }

    let mine = service.running_job();
    let now = service.now();
    let stock = blocking({
        let vault = vault.clone();
        move || Ok(take_stock(&vault, mine, now))
    })
    .await;
    let Ok(stock) = stock else { return false };

    for change in stock.changes {
        service.events().changed(change);
    }
    for job in &stock.swept {
        tracing::info!(
            job = %job.id,
            status = job.status.as_str(),
            "took back a job a stopped process left"
        );
        service.events().changed(job_change(job.id));
        announce(service, job);
        // Failed by the sweep, having been started too often: the thread that
        // asked is owed the news as much as for any other failure.
        spawn_come_back(service, &vault, job);
    }
    for job in &stock.owed {
        spawn_come_back(service, &vault, job);
    }

    // Oldest first: `JobQuery::apply` sorts newest first, which is right for
    // a list and wrong for a queue.
    let Some(job) = stock.queued.into_iter().min_by_key(|job| job.created_at) else {
        return false;
    };
    execute(service, &vault, job, stop).await
}

/// What a pass found in the vault.
#[derive(Default)]
struct Stock {
    /// Jobs a dead process left running, as the sweep has dealt with them:
    /// back in the queue, or failed.
    swept: Vec<Job>,
    /// Waiting to run, the swept ones that went back in the queue included.
    queued: Vec<Job>,
    /// Finished jobs whose conversation has not been come back to: an
    /// answer for a job that is done, the news for one that failed.
    owed: Vec<Job>,
    /// What the sweep's failures moved -- a task of the assistant's going
    /// back to do -- for open windows.
    changes: Vec<Change>,
}

/// The sweep, and the two lists a pass works from. One trip to the blocking
/// pool: nothing in it awaits anything.
///
/// `mine` is the job this process is carrying out, if any. The worker only
/// ever sweeps between jobs, so it is `None` there; it is passed because the
/// rule is "not this process's", not "not the worker's", and a test or a
/// second caller must not have it taken from under them.
fn take_stock(vault: &Vault, mine: Option<JobId>, now: Timestamp) -> Stock {
    let mut stock = Stock::default();
    for mut job in vault.jobs(&JobQuery::unfinished()).unwrap_or_default() {
        match job.status {
            JobStatus::Running if Some(job.id) != mine => {
                let requeued = job.interrupted(INTERRUPTED);
                if vault.save_job(&job).is_err() {
                    continue;
                }
                if requeued {
                    stock.queued.push(job.clone());
                } else {
                    // Failed for good: over, like any other ending, so what
                    // moves with its end moves now.
                    stock.changes.extend(executor_for(&job.kind).ended(vault, &job));
                }
                stock.swept.push(job);
            }
            JobStatus::Queued => stock.queued.push(job),
            _ => {}
        }
    }

    let since = now - jiff::SignedDuration::from_hours(OWED_FOR_DAYS * 24);
    let recent = JobQuery {
        statuses: vec![JobStatus::Done, JobStatus::Failed],
        since: Some(since),
        limit: Some(50),
        ..Default::default()
    };
    stock.owed =
        vault.jobs(&recent).unwrap_or_default().into_iter().filter(owes_the_conversation).collect();
    stock
}

/// What the conversation that asked is owed by a job that has ended, if
/// anything: the follow-up for one that is done, the news for one that
/// failed. A cancelled job owes nothing -- whoever cancelled it knows.
fn owes_the_conversation(job: &Job) -> bool {
    // One of the assistant's own tasks owes the person its result -- or why
    // there is none -- wherever they chose. See `task::deliver`.
    if task::is_task(job) {
        return matches!(job.status, JobStatus::Done | JobStatus::Failed) && !job.answered;
    }
    match job.status {
        JobStatus::Done => job.owes_answer(),
        JobStatus::Failed => job.asked_in.is_some() && !job.answered,
        JobStatus::Queued | JobStatus::Running | JobStatus::Cancelled => false,
    }
}

/// Come back to the conversation that asked, off the worker's own path, if
/// the job owes it anything.
///
/// Spawned rather than awaited because the follow-up waits for any turn
/// already running in that conversation, and a person who is still chatting
/// there would otherwise hold up every job behind this one. Both calls are
/// safe to repeat -- `answer` has a claim and a pause of its own, and
/// `report_failure` marks the job so it speaks once -- which is what lets a
/// pass offer every job that still owes, to catch one a crash skipped.
fn spawn_come_back(service: &Arc<Service>, vault: &Arc<Vault>, job: &Job) {
    if !owes_the_conversation(job) || !service.jobs.begin_answer(job.id) {
        return;
    }
    let id = job.id;
    let failed = job.status == JobStatus::Failed;
    let own_task = task::is_task(job);
    let service = service.clone();
    let vault = vault.clone();
    tokio::spawn(async move {
        let result = if own_task {
            task::deliver(&service, &vault, id).await
        } else if failed {
            answer::report_failure(&service, &vault, id).await
        } else {
            answer::answer(&service, &vault, id).await
        };
        if let Err(e) = result {
            tracing::warn!(
                job = %id,
                error = %e,
                "could not come back to the conversation that asked"
            );
        }
        service.jobs.end_answer(id);
    });
}

/// `service.jobs.running` for as long as this is alive, so the sweep can tell
/// the job this process is carrying out from one a dead process left.
/// Released on drop for the reason [`Service::claim_run`]'s guard is: on the
/// error path somebody forgets, a job would sit "running" until a restart.
struct Working<'a>(&'a Service);

impl<'a> Working<'a> {
    fn start(service: &'a Service, id: JobId) -> Self {
        *service.jobs.running.lock().unwrap() = Some(id);
        Self(service)
    }
}

impl Drop for Working<'_> {
    fn drop(&mut self) {
        *self.0.jobs.running.lock().unwrap() = None;
    }
}

/// How a job's turn ended, before the record is told.
enum Ended {
    /// The turn came back, finished or stopped.
    Turned(Turned),
    /// It could not be started, or it failed. The reason.
    Failed(String),
    /// It was still going when its time ran out.
    TimedOut,
}

/// [`Ended`], with what else the record needs to know to settle.
struct Outcome {
    ended: Ended,
    /// How many tools it had called, for the endings that do not say.
    steps: u32,
    /// Whether the worker is stopping, so the end is an interruption and not
    /// a verdict.
    closing: bool,
}

/// What a job was set up with: the record as it now stands, its transcript,
/// the first message, and what setting up wrote.
struct Staged {
    job: Job,
    transcript: ConversationId,
    prompt: String,
    changes: Vec<Change>,
}

/// Carry one queued job through to an end. Whether it reached one.
async fn execute(
    service: &Arc<Service>,
    vault: &Arc<Vault>,
    queued: Job,
    stop: Option<watch::Receiver<bool>>,
) -> bool {
    let id = queued.id;
    let executor = executor_for(&queued.kind);

    // Taken before the row says `Running`, so there is no moment when it does
    // and nothing claims it.
    let _working = Working::start(service, id);

    // From the record rather than the copy the pass listed: somebody may have
    // cancelled it in between, and that must not be started over.
    let begun = blocking({
        let vault = vault.clone();
        move || {
            let mut job = vault.job(id)?;
            if job.status != JobStatus::Queued {
                return Ok(None);
            }
            job.begin();
            vault.save_job(&job)?;
            Ok(Some(job))
        }
    })
    .await;
    let job = match begun {
        Ok(Some(job)) => job,
        // Not queued any more. There is nothing to do and nothing wrong.
        Ok(None) => return true,
        Err(e) => {
            tracing::warn!(job = %id, error = %e, "could not start a job");
            return false;
        }
    };
    service.events().changed(job_change(id));
    tracing::info!(job = %id, kind = job.kind.as_str(), "running a job");

    // The credential check is a failure here, where the scheduler makes it a
    // skip: a routine that cannot run has missed a moment, and a job that
    // cannot run has been asked for and will not be answered, which is the
    // person's to hear about. Read as `core::Result` rather than converted
    // through `CommandError` first, so the reason is the text a person reads.
    let unusable = blocking({
        let vault = vault.clone();
        // The large model's, which is the medium one's when there is no
        // large one -- the same resolution the turn itself makes.
        move || {
            Ok(vault
                .agent_credentials(everyday_core::llm::ModelTier::Large)
                .map(|_| ())
                .map_err(|e| e.to_string()))
        }
    })
    .await;
    if let Some(reason) = unusable.unwrap_or_else(|e| Err(e.to_string())).err() {
        let outcome = Outcome { ended: Ended::Failed(reason), steps: 0, closing: false };
        return settle(service, vault, executor, id, outcome).await;
    }

    let staged = blocking({
        let vault = vault.clone();
        move || stage(&vault, executor, id)
    })
    .await;
    let Staged { job, transcript, prompt, changes } = match staged {
        Ok(Some(staged)) => staged,
        // Cancelled while it was being set up.
        Ok(None) => return true,
        Err(e) => {
            let outcome = Outcome { ended: Ended::Failed(e.message), steps: 0, closing: false };
            return settle(service, vault, executor, id, outcome).await;
        }
    };
    for change in changes {
        service.events().changed(change);
    }
    service.events().changed(job_change(id));

    // What the sink hears of the turn, and the save loop below writes out.
    let live: Arc<Mutex<Live>> = Arc::default();
    let channel: Sink = {
        let live = live.clone();
        Arc::new(move |event: AgentEvent| {
            if let AgentEvent::ToolStarted { name, arguments, .. } = event {
                live.lock().unwrap().started(&name, &arguments);
            }
        })
    };
    let pending = service.pending();
    let turn = Turn {
        service: service.clone(),
        vault: vault.clone(),
        pending: pending.clone(),
        conversation: transcript,
        prompt,
        pictures: Vec::new(),
        on_screen: None,
        channel,
        // A job is unattended in every way that matters, and says so with
        // `job` rather than with a routine run's id: it has none.
        unattended: None,
        drafting: None,
        via: None,
        job: Some(JobTurn { id, question: job.title(), context: executor.context() }),
        follow_up: None,
        max_steps: Some(executor.max_steps()),
        tier: None,
        // A job asks nobody: it is refused what it would need somebody for.
        reach: crate::agent::Reach::default(),
    };

    let turn = tokio::time::timeout(executor.timeout(), crate::agent::run_turn(turn));
    tokio::pin!(turn);
    let shutdown = closing_down(stop);
    tokio::pin!(shutdown);
    let mut cancel_tick = ticker(WATCH_EVERY);
    let mut save_tick = ticker(SAVE_EVERY);
    let mut closing = false;

    // The turn runs inside this loop rather than beside it, so the watching
    // happens while the turn is not being polled. A read of one row and a
    // write of another is milliseconds, and the turn spends its life waiting
    // on a model; in exchange nothing is ever left half done when it ends.
    let done = loop {
        tokio::select! {
            done = &mut turn => break done,
            _ = cancel_tick.tick() => {
                // Whoever cancelled it -- a tool, a command, another device --
                // could only change the record. Stopping the turn is ours to
                // do, and asked for again each time round in case the first
                // landed before the turn had registered itself.
                if is_cancelled(vault, id).await {
                    pending.cancel(transcript);
                }
            }
            _ = save_tick.tick() => {
                save_progress(service, vault, id, &live).await;
            }
            _ = &mut shutdown, if !closing => {
                // The worker is being stopped. Ask the turn to end, and wait
                // for it: it ends at once, and what it had done is kept.
                closing = true;
                pending.cancel(transcript);
            }
        }
    };

    let steps = live.lock().unwrap().steps;
    let ended = match done {
        Ok(Ok(turned)) => Ended::Turned(turned),
        Ok(Err(e)) => Ended::Failed(e.message),
        Err(_) => Ended::TimedOut,
    };
    settle(service, vault, executor, id, Outcome { ended, steps, closing }).await
}

/// Set a started job up: the thing it writes into, and its transcript.
///
/// Read again from the record, for the reason `execute` reads it before
/// starting. On a restart this finds the note and the transcript the last
/// attempt made and goes on in them.
fn stage(
    vault: &Vault,
    executor: &dyn Executor,
    id: JobId,
) -> crate::error::CommandResult<Option<Staged>> {
    let mut job = vault.job(id)?;
    if job.status != JobStatus::Running {
        return Ok(None);
    }
    let Prepared { prompt, changes } = executor.prepare(vault, &mut job)?;

    let existing = job.conversation_id.and_then(|c| vault.conversation(c).ok());
    let transcript = match existing {
        Some(conversation) => conversation.id,
        None => {
            // Named after what it is about, so a list of transcripts reads as
            // a list of questions, and carrying the job so the rail's history
            // keeps it out.
            let conversation =
                Conversation::for_job(job.id, Conversation::title_from(&job.title()));
            vault.save_conversation(&conversation)?;
            conversation.id
        }
    };
    job.conversation_id = Some(transcript);
    vault.save_job(&job)?;
    Ok(Some(Staged { job, transcript, prompt, changes }))
}

/// A timer that first fires one `every` from now, not at once.
///
/// A tick that was missed because a save was slow is not owed again: the next
/// one is soon enough.
fn ticker(every: Duration) -> tokio::time::Interval {
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick
}

/// Resolve once the worker is asked to stop, and never otherwise. A caller
/// with no signal to give, a test driving [`pass`], never stops it; and a
/// sender that has gone away without saying so is "never", not "now", as it
/// is for a turn's own stop switch.
async fn closing_down(stop: Option<watch::Receiver<bool>>) {
    let Some(mut stop) = stop else { return std::future::pending::<()>().await };
    if stop.wait_for(|stop| *stop).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Whether somebody has cancelled this job since it started.
async fn is_cancelled(vault: &Arc<Vault>, id: JobId) -> bool {
    let vault = vault.clone();
    blocking(move || Ok(vault.job(id)?.status == JobStatus::Cancelled)).await.unwrap_or(false)
}

/// What the sink has heard: the count and the line the card should show.
#[derive(Default)]
struct Live {
    steps: u32,
    progress: String,
    /// Whether either has changed since it was last written out.
    dirty: bool,
}

impl Live {
    fn started(&mut self, name: &str, arguments: &serde_json::Value) {
        self.steps += 1;
        self.progress = progress_line(name, arguments);
        self.dirty = true;
    }

    /// The count and the line, if they are news.
    fn take(&mut self) -> Option<(u32, String)> {
        if !self.dirty {
            return None;
        }
        self.dirty = false;
        Some((self.steps, self.progress.clone()))
    }
}

/// Write out what the job has been doing, and tell open cards.
async fn save_progress(
    service: &Arc<Service>,
    vault: &Arc<Vault>,
    id: JobId,
    live: &Arc<Mutex<Live>>,
) {
    let news = live.lock().unwrap().take();
    let Some((steps, progress)) = news else { return };
    let saved = blocking({
        let vault = vault.clone();
        move || {
            Ok(amend(&vault, id, |job| {
                job.steps = steps;
                job.progress = progress;
            })?)
        }
    })
    .await;
    if matches!(saved, Ok(true)) {
        service.events().changed(job_change(id));
    }
}

/// Change a job that is still running, starting from the record as it is
/// now rather than from the copy this worker began with. A cancellation that
/// landed in between would otherwise be written over by a progress line.
/// False if it is not running any more, in which case nothing was written.
///
/// There is still the moment between the read and the write; the store has
/// no conditional update to close it. It is milliseconds, and the end of the
/// job re-reads the record and keeps a cancellation it finds.
fn amend(vault: &Vault, id: JobId, change: impl FnOnce(&mut Job)) -> everyday_core::Result<bool> {
    let mut job = vault.job(id)?;
    if job.status != JobStatus::Running {
        return Ok(false);
    }
    change(&mut job);
    vault.save_job(&job)?;
    Ok(true)
}

/// What came of settling a job.
struct Settled {
    job: Job,
    changes: Vec<Change>,
}

/// Write down how a job ended, tell the windows and the person, and come
/// back to the conversation that asked. Whether the job is over, as opposed
/// to back in the queue.
async fn settle(
    service: &Arc<Service>,
    vault: &Arc<Vault>,
    executor: &'static dyn Executor,
    id: JobId,
    outcome: Outcome,
) -> bool {
    let settled = blocking({
        let vault = vault.clone();
        move || Ok(conclude(&vault, executor, id, outcome))
    })
    .await;
    let Ok(Some(Settled { job, changes })) = settled else {
        // The vault locked, or the record is gone. Left as it is, the next
        // pass either sweeps it or finds nothing to sweep.
        tracing::warn!(job = %id, "could not write down how a job ended");
        return false;
    };

    tracing::info!(job = %id, status = job.status.as_str(), steps = job.steps, "a job ended");
    // What the job wrote, then the job: a window with the Notes app open
    // learns about the note before it is told the job is done.
    for change in changes {
        service.events().changed(change);
    }
    service.events().changed(job_change(id));
    announce(service, &job);
    spawn_come_back(service, vault, &job);
    job.status.is_finished()
}

/// How a job ended, in the record. The record is read again first, so that
/// whatever happened to it while the turn ran -- above all a cancellation --
/// is not undone by the worker's older picture of it.
fn conclude(
    vault: &Vault,
    executor: &dyn Executor,
    id: JobId,
    outcome: Outcome,
) -> Option<Settled> {
    let Outcome { ended, steps, closing } = outcome;
    let mut job = vault.job(id).ok()?;
    let cancelled = job.status == JobStatus::Cancelled;
    let mut changes = Vec::new();
    job.steps = job.steps.max(steps);
    // The line is for the card while it runs.
    job.progress = String::new();

    match ended {
        Ended::Turned(turned) => {
            let Turned { text, prose, steps, wrote, stopped } = turned;
            changes.extend(wrote.into_iter().map(written_change));
            job.steps = steps;
            if cancelled {
                // Whoever cancelled it has said why. Keep that, and what the
                // model had to say for itself by then.
                job.summary = text;
            } else if stopped && closing {
                job.interrupted("the app was closing");
            } else if stopped {
                // Stopped from its transcript, which is a way of cancelling it.
                let _ = job.cancel("it was stopped");
            } else {
                // All it said, not only its last word: an executor salvaging
                // what the model wrote but put nowhere -- research, into its
                // note -- needs the findings it wrote on the way.
                match executor.conclude(vault, &job, &prose) {
                    Ok(more) => {
                        changes.extend(more);
                        job.finish(text);
                    }
                    Err(e) => job.fail(e.to_string()),
                }
            }
        }
        Ended::Failed(_) if cancelled => {}
        Ended::Failed(_) if closing => {
            job.interrupted("the app was closing");
        }
        // A lock is not the job's fault. It goes back to the queue and the
        // next unlock finds it.
        Ended::Failed(_) if !vault.is_unlocked() => {
            job.interrupted("the vault was locked while it ran");
        }
        Ended::Failed(why) => job.fail(why),
        Ended::TimedOut if cancelled => {}
        Ended::TimedOut => job.fail(format!(
            "it was still going after {} minutes and was stopped",
            executor.timeout().as_secs() / 60
        )),
    }
    // News again, now it is over, even if somebody looked at the card while
    // it ran: what they saw then was a progress line, not the answer.
    if matches!(job.status, JobStatus::Done | JobStatus::Failed) {
        job.seen = false;
    }
    vault.save_job(&job).ok()?;
    if job.status.is_finished() {
        changes.extend(executor.ended(vault, &job));
    }
    Some(Settled { job, changes })
}

/// A write a turn made, as the change that tells windows about it. One or the
/// other of `id` and `ids`, never both -- see [`Change`]'s own contract.
fn written_change((kind, ids): (Kind, Vec<String>)) -> Change {
    let (id, ids) = match ids.len() {
        1 => (ids.into_iter().next(), Vec::new()),
        _ => (None, ids),
    };
    Change { kind, op: Op::Updated, id, ids, origin: Some("assistant".to_string()) }
}

fn job_change(id: JobId) -> Change {
    Change {
        kind: Kind::Job,
        op: Op::Updated,
        id: Some(id.to_string()),
        ids: Vec::new(),
        origin: Some("assistant".to_string()),
    }
}

/// Say something about a job that ended, once.
///
/// Keyed on the job, so a restart that says it again replaces rather than
/// stacks. A job that was cancelled says nothing: whoever cancelled it knows,
/// and a job put back in the queue has not ended.
///
/// The question is the body, as the plan has it. Unlike a routine's, which
/// keeps what it found out of a notification because a lock screen may show
/// it, this carries what was *asked* -- worth knowing when two are in flight,
/// and not something that was found out.
///
/// One of the assistant's own tasks says nothing here: it says what it came
/// to where the person chose instead, as what the job owes -- see
/// [`task::deliver`].
fn announce(service: &Arc<Service>, job: &Job) {
    if task::is_task(job) {
        return;
    }
    let key = format!("job:{}", job.id);
    match job.status {
        // No body: the question is what the person asked about, and this is a
        // notification that will be drawn over a lock screen -- the rule
        // `scheduler::announce` keeps for a routine's findings. The thread it
        // came from says what it was.
        JobStatus::Done => service.events().notify(
            Notification::new(Level::Info, format!("{} is ready", job.kind.label()))
                .for_user()
                .key(key),
        ),
        JobStatus::Failed => service.events().notify(
            Notification::warning(format!("{} could not finish", job.kind.label()))
                .body(job.reason.clone())
                .for_user()
                .key(key),
        ),
        JobStatus::Queued | JobStatus::Running | JobStatus::Cancelled => {}
    }
}

/// One line for a card saying what the job has just started doing.
///
/// From the tool's name and the arguments it was given, because that is all
/// the sink hears when a call starts, and written for a person: "Reading
/// example.com", not `read_web_page`. A tool this does not know says its name
/// with the underscores taken out, which is plain enough to be true.
fn progress_line(name: &str, arguments: &serde_json::Value) -> String {
    let text = |key: &str| {
        arguments
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    match name {
        "web_search" => match text("query") {
            Some(query) => format!("Searching for {}", short(&query, 60)),
            None => "Searching the web".to_string(),
        },
        "read_web_page" => {
            let site = text("url").and_then(|url| crate::webpage::parse(&url).ok());
            match site.as_ref().and_then(|url| url.host_str()) {
                Some(host) => format!("Reading {}", host.trim_start_matches("www.")),
                None => "Reading a web page".to_string(),
            }
        }
        "update_plan" => "Planning the work".to_string(),
        "create_note" => "Starting a note".to_string(),
        "update_note" => "Writing up the note".to_string(),
        "get_note" | "list_notes" => "Reading notes".to_string(),
        "get_weather" => "Checking the weather".to_string(),
        "look_up_item" => "Looking something up".to_string(),
        "search_images" => match text("query") {
            Some(query) => format!("Looking for pictures of {}", short(&query, 50)),
            None => "Looking for pictures".to_string(),
        },
        "search_places" => match text("query") {
            Some(query) => format!("Looking for {} on the map", short(&query, 50)),
            None => "Looking at the map".to_string(),
        },
        "get_directions" => match text("to") {
            Some(to) => format!("Finding the way to {}", short(&to, 50)),
            None => "Finding the way".to_string(),
        },
        "list_events" | "list_calendars" | "list_time_blocks" => {
            "Checking the calendar".to_string()
        }
        "search_mail" | "list_threads" | "read_thread" | "find_address" | "list_accounts" => {
            "Looking through mail".to_string()
        }
        "search" | "overview" => "Searching the vault".to_string(),
        "list_tasks" | "get_task" | "list_projects" => "Checking tasks".to_string(),
        "list_stories" | "read_story" | "list_news_sources" => "Reading the Feed".to_string(),
        "list_meeting_notes" | "get_transcript" => "Looking through meeting notes".to_string(),
        other => format!("Using {}", other.replace('_', " ")),
    }
}

/// `text` on one line and at most `max` characters, cut with an ellipsis.
fn short(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max).collect();
    format!("{}\u{2026}", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_search_says_what_it_is_looking_for() {
        assert_eq!(
            progress_line("web_search", &json!({ "query": "  best e-bike   for hills " })),
            "Searching for best e-bike for hills"
        );
        assert_eq!(progress_line("web_search", &json!({})), "Searching the web");
        let long = "x ".repeat(100);
        let line = progress_line("web_search", &json!({ "query": long }));
        assert!(line.ends_with('\u{2026}') && line.chars().count() < 80, "{line}");
    }

    #[test]
    fn reading_a_page_names_the_site_and_not_the_address() {
        assert_eq!(
            progress_line(
                "read_web_page",
                &json!({ "url": "https://www.example.com/reviews/2026?page=2#top" })
            ),
            "Reading example.com"
        );
        // Not an address at all: still a line, and not a lie about a site.
        assert_eq!(
            progress_line("read_web_page", &json!({ "url": "not a url" })),
            "Reading a web page"
        );
        assert_eq!(progress_line("read_web_page", &json!(null)), "Reading a web page");
    }

    #[test]
    fn the_other_tools_have_a_line_and_an_unknown_one_a_plain_one() {
        assert_eq!(progress_line("update_note", &json!({})), "Writing up the note");
        assert_eq!(progress_line("update_plan", &json!({})), "Planning the work");
        assert_eq!(progress_line("list_events", &json!({})), "Checking the calendar");
        assert_eq!(progress_line("get_weather", &json!({})), "Checking the weather");
        assert_eq!(progress_line("archive_thread", &json!({})), "Using archive thread");
    }

    #[test]
    fn a_tool_started_counts_a_step_and_is_news_once() {
        let mut live = Live::default();
        assert_eq!(live.take(), None, "nothing has happened");

        live.started("web_search", &json!({ "query": "tide times" }));
        live.started("read_web_page", &json!({ "url": "https://example.com/tides" }));
        assert_eq!(live.take(), Some((2, "Reading example.com".to_string())));
        assert_eq!(live.take(), None, "written out, so not news again");

        live.started("update_note", &json!({}));
        assert_eq!(live.take(), Some((3, "Writing up the note".to_string())));
    }

    #[test]
    fn a_follow_up_is_never_started_twice_at_once() {
        let runtime = JobRuntime::default();
        let job = JobId::new();
        assert!(runtime.begin_answer(job));
        assert!(!runtime.begin_answer(job), "already under way");
        runtime.end_answer(job);
        assert!(runtime.begin_answer(job), "free again once it ended");

        assert!(runtime.begin_answer(JobId::new()), "and another job is another matter");
    }

    #[test]
    fn a_job_owes_its_conversation_by_how_it_ended() {
        let asked = Some(ConversationId::new());
        let mut job = Job::research("Which e-bike?", "", asked);
        assert!(!owes_the_conversation(&job), "queued: nothing yet");

        job.begin();
        assert!(!owes_the_conversation(&job), "running: nothing yet");

        let mut done = job.clone();
        done.finish("Three.");
        assert!(owes_the_conversation(&done), "an answer");
        done.answered = true;
        assert!(!owes_the_conversation(&done), "had it");

        let mut failed = job.clone();
        failed.fail("the endpoint refused");
        assert!(owes_the_conversation(&failed), "the news");
        failed.answered = true;
        assert!(!owes_the_conversation(&failed), "told");

        let mut cancelled = job.clone();
        cancelled.cancel("they changed their mind").unwrap();
        assert!(!owes_the_conversation(&cancelled), "whoever cancelled it knows");

        let mut nobody = Job::research("Which e-bike?", "", None);
        nobody.begin();
        nobody.fail("the endpoint refused");
        assert!(!owes_the_conversation(&nobody), "started from the app: no thread to tell");
    }
}
