//! The worker that runs the assistant's background jobs.
//!
//! See `docs/plans/jobs.md` and `docs/plans/watches.md`. One loop, beside the
//! routine scheduler rather than inside it, so a twenty-minute research
//! cannot make the seven o'clock brief late. Woken by [`Service::wake_jobs`]
//! when a job is queued, and on its own now and then in case a wake was
//! missed. Beside it runs the watch loop ([`watch`]), which carries out the
//! jobs that sleep between looks; this one is for the jobs that are one long
//! turn each -- research, a project, one of the assistant's own tasks.
//!
//! # Rules it keeps
//!
//! **Two at a time.** Up to [`MAX_AT_ONCE`] queued jobs run at once, oldest
//! first, each as a task of its own, so a project that will take an hour does
//! not hold up the research somebody asked for a minute ago. Not more: every
//! turn is a model's attention and a bill, and two is what a person can follow.
//! They run alongside the rail and the scheduler, as a routine already does.
//! [`pass`], which tests drive, still runs the oldest queued job to its end
//! and returns.
//!
//! **Nothing while locked.** A locked vault has no key and a read-only one
//! cannot be written to, and a job writes constantly. Whatever the lock
//! interrupts goes back to the queue (once; see [`karmayoga_core::job::MAX_ATTEMPTS`])
//! and starts over when somebody unlocks.
//!
//! **A row that says `Running` and is not ours is somebody else's corpse.**
//! The app was quit, the machine slept, the vault was switched. The sweep at
//! the top of every pass puts such a job back in the queue, or fails it if it
//! has been started too often already. Our own are told apart by
//! [`Service::is_running_job`], not by how old they look -- and there may be
//! several, so the sweep asks of each. It leaves a job that is
//! [`Waiting`](karmayoga_core::JobStatus::Waiting) alone: that is a watch
//! asleep between looks, which is not running and not in need of anything.
//! A watch somebody queued by hand is put to sleep, due now, for the watch
//! loop to pick up; this worker never runs one.
//!
//! **A turn is never dropped from a `select!`.** The same rule the scheduler
//! keeps: a turn cancelled half way would leave a row saying `Running` with
//! nothing to finish it. Stopping, cancelling and timing out all ask the turn
//! to end and wait for it, except the timeout, which is a deliberate drop and
//! is recorded as one. When the worker is told to stop, every job in hand is
//! asked to end, and the worker waits for all of them.
//!
//! **Coming back is off the worker's path.** A job that ends owes the
//! conversation that asked: an answer if it is done (`answer::answer`), a line
//! saying so if it failed (`answer::report_failure`, no model). Both are
//! spawned rather than awaited, because the first waits for the thread to go
//! quiet, and both are offered again on every pass for any job that still
//! owes, so one a crash skipped is caught. A watch owes nothing: it said what
//! it had to as it went.
//!
//! **What a job says while it works is sent on.** A project's own turn can
//! write a short update onto its record (`report_progress`). The save tick
//! reads the record, sends each update not yet sent through
//! [`Service::deliver_update`] -- off the turn's path, since delivering waits on
//! a chat -- and marks it delivered, once more when the job settles so that
//! nothing said in the last seconds is lost.
//!
//! # What is shared and what is research
//!
//! Everything here is generic: the queue, the sweep, the record's life from
//! `Running` to an end, progress, updates, cancelling, the transcript, the
//! notification. What changes with the kind of job is an [`Executor`]: what to
//! set up before the turn (research and projects make their note), the first
//! message, the budget, who the job may ask, and what to check when the turn
//! is done. [`research`], [`project`] and [`task`] are the ones that run here;
//! the browser the plan describes would be another.

pub mod answer;
pub mod project;
pub mod research;
pub mod task;
pub mod watch;

use crate::agent::{AgentEvent, Asker, JobTurn, Reach, Sink, Turn, Turned};
use crate::ctx::Scope;
use crate::events::{Change, Kind, Level, Notification, Op};
use crate::reach::Update;
use crate::service::{Service, blocking};
use jiff::Timestamp;
use karmayoga_core::{
    Conversation, ConversationId, Job, JobId, JobKind, JobQuery, JobStatus, Vault,
};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch as signal;
use tokio::task::JoinSet;

/// How many queued jobs run at once. Two: a long project does not hold up a
/// question's research, and nobody has more than two model turns of
/// background work going on that they could follow.
pub const MAX_AT_ONCE: usize = 2;

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

/// Longest the words saying who an update is from -- the job's title -- may
/// run to. A project's goal can be a paragraph; a chat's lead line and a
/// notification's title cannot.
const FROM_CHARS: usize = 100;

/// Why a job found `Running` with nobody carrying it out went back to the
/// queue.
const INTERRUPTED: &str = "the app stopped while it was running";

/// The worker's session state, held by [`Service`].
#[derive(Default)]
pub struct JobRuntime {
    /// Rung by [`Service::wake_jobs`]; the loop waits on it between passes.
    pub(crate) wake: tokio::sync::Notify,
    /// Rung by [`Service::wake_jobs`] too: the watch loop ([`watch::run`])
    /// waits on it between passes, so a watch just started or told to look
    /// now is not left for the fallback.
    pub(crate) wake_watches: tokio::sync::Notify,
    /// The jobs this process is carrying out right now -- a job's turn, or a
    /// watch's look -- each held by a [`Working`]. What the sweep tells a
    /// job of ours from one a dead process left.
    pub(crate) running: Mutex<HashSet<JobId>>,
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
    /// Tell the worker there may be a job to pick up, and the watch loop
    /// there may be a watch to look at. Cheap, and harmless when there is
    /// none.
    pub fn wake_jobs(&self) {
        self.jobs.wake.notify_one();
        self.jobs.wake_watches.notify_one();
    }

    /// A job this process is carrying out right now, if any. With several
    /// under way -- two of the worker's, or one and a watch's look -- the one
    /// with the lowest id, which is the one asked for first, not necessarily
    /// the first claimed: for a test that runs one at a time, and a log line.
    /// To ask about a particular job, see [`Service::is_running_job`].
    pub fn running_job(&self) -> Option<JobId> {
        self.jobs.running.lock().unwrap().iter().min().copied()
    }

    /// Whether this process is carrying out `job` right now.
    pub fn is_running_job(&self, job: JobId) -> bool {
        self.jobs.running.lock().unwrap().contains(&job)
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
    fn prepare(&self, vault: &Vault, job: &mut Job) -> karmayoga_core::Result<Prepared>;

    /// The turn is over and `reply` is everything the model said in it --
    /// the steps on the way as well as its last word, which is the job's
    /// summary. Check that the job left something worth the name, and mend
    /// it if it can be mended. An error fails the job, with the error as the
    /// reason.
    fn conclude(
        &self,
        vault: &Vault,
        job: &Job,
        reply: &str,
    ) -> karmayoga_core::Result<Vec<Change>>;

    /// The job is over -- done, failed or cancelled -- and written down as
    /// such. Whatever else should move with it, moved; what was written, for
    /// open windows. Nothing, for a kind with nothing to move.
    fn ended(&self, _vault: &Vault, _job: &Job) -> Vec<Change> {
        Vec::new()
    }

    /// Who the job's turn may ask while it works, if anybody: the person, in
    /// their chat. With one, a call that would otherwise be refused is put to
    /// them, and the turn is offered `ask_owner`. Nobody, by default: a job
    /// asks nobody unless its kind says it may -- research and tasks are
    /// written to carry on without, and a question nobody could answer would
    /// only stall them. See [`project::Project`].
    fn asker(&self, _service: &Arc<Service>, _job: &Job) -> Option<Arc<dyn Asker>> {
        None
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
        JobKind::Project { .. } => &project::Project,
        // A watch is never queued for the worker: the watch loop carries it
        // out, a look at a time. This is only reached by a job somebody put
        // in the queue by hand, and the act turn's settings are as fitting as
        // any for it. See `watch`.
        JobKind::Watch { .. } => &watch::WatchAct,
    }
}

/// Run the worker until `stop` says otherwise. Spawned once beside the
/// scheduler, on the runtime the rest of the process uses.
pub async fn run(service: Arc<Service>, stop: signal::Receiver<bool>) {
    // The watch loop beside the worker, on the same stop signal, so neither
    // shell has to know there are two.
    let watches = self::watch::run(service.clone(), stop.clone());
    tokio::join!(work(service, stop), watches);
}

/// The worker's own loop: everything [`run`] did before there were watches.
async fn work(service: Arc<Service>, mut stop: signal::Receiver<bool>) {
    tracing::info!("the assistant's job worker is awake");
    let mut hands = Hands::default();
    'working: loop {
        if *stop.borrow() {
            break;
        }
        hands.reap();
        // Deliberately *not* inside the `select!` below, for the reason the
        // scheduler gives: a turn cancelled half way would be a live model
        // turn dropped. The turns run on tasks of their own, which are only
        // ever waited for. Stopping is asked of each turn instead -- see
        // `execute` -- and this waits for them to end, below.
        take_on(&service, &stop, &mut hands).await;
        // Until there is something to do about: a wake, the fallback, or a
        // job ending -- which frees a place, and may have left another job
        // behind it that has no reason to wait for the fallback. A job that
        // went back to the queue is not an end: looking again at once would
        // only pick it up to be interrupted in the same way.
        loop {
            tokio::select! {
                _ = service.jobs.wake.notified() => break,
                _ = tokio::time::sleep(FALLBACK) => break,
                reached_an_end = hands.ended(), if !hands.is_empty() => {
                    if reached_an_end {
                        break;
                    }
                }
                _ = stop.changed() => break 'working,
            }
        }
    }
    // Every job in hand heard the same signal and is ending its turn. Wait for
    // them: a turn is never left half done, and a row left `Running` would
    // only be a corpse for the next start to sweep.
    hands.finish().await;
    tracing::info!("the assistant's job worker has stopped");
}

/// One pass: sweep what a dead process left, answer what is owed, and run
/// the oldest queued job to its end. Public so a test can drive it. The
/// worker's own loop is [`work`], which takes on up to [`MAX_AT_ONCE`] jobs
/// rather than one.
pub async fn pass(service: &Arc<Service>) {
    let Some((vault, stock)) = survey(service).await else { return };
    let oldest = pick(stock.queued, 1, |id| service.is_running_job(id)).pop();
    if let Some(job) = oldest {
        execute(service, &vault, job, None).await;
    }
}

/// A pass of the worker's loop: look at the vault, and start as many of the
/// queued jobs as there are places for.
async fn take_on(service: &Arc<Service>, stop: &signal::Receiver<bool>, hands: &mut Hands) {
    let Some((vault, stock)) = survey(service).await else { return };
    let free = hands.free();
    let chosen = pick(stock.queued, free, |id| hands.holds(id) || service.is_running_job(id));
    for job in chosen {
        hands.start(service, &vault, job, stop.clone());
    }
}

/// The first half of every pass, whoever runs it: sweep what a dead process
/// left, put right what was queued by mistake, and offer every job that owes
/// its conversation a word. What it found, for the caller to run from. `None`
/// when nothing may run: no vault, a locked or read-only one, or one that
/// keeps no jobs.
async fn survey(service: &Arc<Service>) -> Option<(Arc<Vault>, Stock)> {
    let vault = service.get()?;
    // Nothing while locked, and nothing in a second copy of the application
    // that holds no write claim. Not even the sweep: it writes.
    if !vault.is_unlocked() || !vault.is_writable() || !vault.supports_jobs() {
        return None;
    }

    let now = service.now();
    let stock = blocking({
        let vault = vault.clone();
        let service = service.clone();
        move || Ok(take_stock(&vault, &|id: JobId| service.is_running_job(id), now))
    })
    .await;
    let Ok(mut stock) = stock else { return None };

    for change in std::mem::take(&mut stock.changes) {
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
    if !stock.slept.is_empty() {
        for job in &stock.slept {
            tracing::info!(job = %job.id, "a watch was queued; put to sleep for the watch loop");
            service.events().changed(job_change(job.id));
        }
        service.jobs.wake_watches.notify_one();
    }
    Some((vault, stock))
}

/// Of the queued jobs, the ones to run now: oldest first, none that is
/// `claimed` already (by a task of ours that has not got as far as saying so,
/// or by anything else), no more than `free`. A watch is never one: the
/// watch loop carries those out.
///
/// Oldest first because [`JobQuery::apply`] sorts newest first, which is right
/// for a list and wrong for a queue.
fn pick(mut queued: Vec<Job>, free: usize, claimed: impl Fn(JobId) -> bool) -> Vec<Job> {
    queued.sort_by_key(|job| job.created_at);
    queued.into_iter().filter(|job| !job.kind.is_stepped() && !claimed(job.id)).take(free).collect()
}

/// The jobs the worker's loop has in hand: each on a task of its own, so that
/// a project an hour long does not make the job behind it wait.
#[derive(Default)]
struct Hands {
    /// The tasks, each carrying one job to an end. What is waited on.
    tasks: JoinSet<bool>,
    /// The jobs they are carrying, from the moment one is chosen -- before its
    /// task has run a line, so the next pass cannot choose it again. Each task
    /// holds a [`Place`] that gives its job back, even if it panics.
    jobs: Arc<Mutex<HashSet<JobId>>>,
}

/// One job's place among the [`Hands`], given back when dropped.
struct Place(Arc<Mutex<HashSet<JobId>>>, JobId);

impl Drop for Place {
    fn drop(&mut self) {
        self.0.lock().unwrap().remove(&self.1);
    }
}

impl Hands {
    /// How many more jobs may be started.
    fn free(&self) -> usize {
        MAX_AT_ONCE.saturating_sub(self.jobs.lock().unwrap().len())
    }

    /// Whether `job` is one of them.
    fn holds(&self, job: JobId) -> bool {
        self.jobs.lock().unwrap().contains(&job)
    }

    fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Start carrying `job` to an end, on a task of its own.
    fn start(
        &mut self,
        service: &Arc<Service>,
        vault: &Arc<Vault>,
        job: Job,
        stop: signal::Receiver<bool>,
    ) {
        self.jobs.lock().unwrap().insert(job.id);
        let place = Place(self.jobs.clone(), job.id);
        let (service, vault) = (service.clone(), vault.clone());
        self.tasks.spawn(async move {
            let _place = place;
            execute(&service, &vault, job, Some(stop)).await
        });
    }

    /// Wait for one of them to end: whether it reached an end, as opposed to
    /// going back in the queue. Cancel-safe, so it may be a `select!` arm.
    async fn ended(&mut self) -> bool {
        match self.tasks.join_next().await {
            Some(Ok(reached_an_end)) => reached_an_end,
            Some(Err(e)) => {
                // Its claim and its place are given back as it unwinds, and the
                // row it left `Running` is the next sweep's.
                tracing::error!(error = %e, "a job's task failed");
                false
            }
            None => false,
        }
    }

    /// Collect the tasks that have ended already, so that they do not pile up.
    fn reap(&mut self) {
        while self.tasks.try_join_next().is_some() {}
    }

    /// Wait for every one of them.
    async fn finish(&mut self) {
        while self.tasks.join_next().await.is_some() {}
    }
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
    /// Watches found in the queue and put to sleep, due now: the watch loop's
    /// to carry out.
    slept: Vec<Job>,
    /// What the sweep's failures moved -- a task of the assistant's going
    /// back to do -- for open windows.
    changes: Vec<Change>,
}

/// The sweep, and the two lists a pass works from. One trip to the blocking
/// pool: nothing in it awaits anything.
///
/// `claimed` says whether this process is carrying a job out. The worker has
/// two at a time, and the watch loop looks at others, so "the job being run"
/// is not one id but a set, and the question is asked of each row as it is
/// met: a job is claimed before its row says `Running`, so asking after the
/// row is read cannot take a live job for a dead one.
fn take_stock(vault: &Vault, claimed: &dyn Fn(JobId) -> bool, now: Timestamp) -> Stock {
    let mut stock = Stock::default();
    // Not `Waiting`: a watch asleep between looks is not running, and whatever
    // it is waiting for is not the sweep's to hurry.
    for mut job in vault.jobs(&JobQuery::unfinished()).unwrap_or_default() {
        match job.status {
            JobStatus::Running if !claimed(job.id) => {
                job.interrupted(INTERRUPTED);
                if vault.save_job(&job).is_err() {
                    continue;
                }
                match job.status {
                    JobStatus::Queued => stock.queued.push(job.clone()),
                    // Failed for good: over, like any other ending, so what
                    // moves with its end moves now.
                    status if status.is_finished() => {
                        stock.changes.extend(executor_for(&job.kind).ended(vault, &job));
                    }
                    // A job that runs in steps, put back to sleep.
                    _ => {}
                }
                stock.swept.push(job);
            }
            // Never run by this worker: the watch loop carries a watch out, a
            // look at a time. One that was queued by hand is put to sleep, due
            // now, which is where its own loop will look for it.
            JobStatus::Queued if job.kind.is_stepped() => {
                job.sleep_until(now);
                if vault.save_job(&job).is_ok() {
                    stock.slept.push(job);
                }
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
    // A watch said what it had to as it went, its end included: the watch
    // loop reports its own, and there is no follow-up to have. Research and a
    // project are alike: an answer, or the news.
    if job.kind.is_stepped() {
        return false;
    }
    match job.status {
        JobStatus::Done => job.owes_answer(),
        JobStatus::Failed => job.asked_in.is_some() && !job.answered,
        JobStatus::Queued | JobStatus::Running | JobStatus::Waiting | JobStatus::Cancelled => false,
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
pub(crate) struct Working<'a>(&'a Service, JobId);

impl<'a> Working<'a> {
    /// Claim `id` for this process. `None` when it is claimed already -- a
    /// watch's look still under way when the next pass comes round.
    pub(crate) fn start(service: &'a Service, id: JobId) -> Option<Self> {
        service.jobs.running.lock().unwrap().insert(id).then(|| Self(service, id))
    }
}

impl Drop for Working<'_> {
    fn drop(&mut self) {
        self.0.jobs.running.lock().unwrap().remove(&self.1);
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
    stop: Option<signal::Receiver<bool>>,
) -> bool {
    let id = queued.id;
    let executor = executor_for(&queued.kind);

    // Taken before the row says `Running`, so there is no moment when it does
    // and nothing claims it.
    let Some(_working) = Working::start(service, id) else { return false };

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
                .agent_credentials(karmayoga_core::llm::ModelTier::Large)
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
        files: Vec::new(),
        on_screen: None,
        channel,
        // A job is unattended in every way that matters, and says so with
        // `job` rather than with a routine run's id: it has none.
        unattended: None,
        drafting: None,
        via: None,
        spoken: false,
        voice_approves: false,
        job: Some(JobTurn { id, question: job.title(), context: executor.context() }),
        follow_up: None,
        about_job: None,
        max_steps: Some(executor.max_steps()),
        tier: None,
        read_only: false,
        // Most jobs ask nobody: they are refused what they would need somebody
        // for. A project may ask the person in their chat, when one is
        // running. What a job's final reply is sent to is not said here: its
        // answer comes back through the follow-up, not the reply.
        reach: Reach { asker: executor.asker(service, &job), sent_to: Vec::new(), typist: None },
    };

    let turn = tokio::time::timeout(executor.timeout(), crate::agent::run_turn(turn));
    tokio::pin!(turn);
    let shutdown = closing_down(stop);
    tokio::pin!(shutdown);
    let mut cancel_tick = ticker(WATCH_EVERY);
    let mut save_tick = ticker(SAVE_EVERY);
    let mut closing = false;
    // Updates the job has written for the person, being sent on a task of
    // their own: delivering waits on a chat, and the turn is not polled while
    // this loop waits.
    let mut sending: Option<tokio::task::JoinHandle<()>> = None;

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
                if sending.as_ref().is_none_or(|sent| sent.is_finished()) {
                    sending = Some(tokio::spawn({
                        let (service, vault) = (service.clone(), vault.clone());
                        async move { send_updates(&service, &vault, id).await }
                    }));
                }
            }
            _ = &mut shutdown, if !closing => {
                // The worker is being stopped. Ask the turn to end, and wait
                // for it: it ends at once, and what it had done is kept.
                closing = true;
                pending.cancel(transcript);
            }
        }
    };

    // What it said in its last seconds is sent before the job is settled, so
    // that the thread reads the update and then the answer, not the other way
    // round. Not when the worker is stopping: the job goes back in the queue,
    // and what it has still to say is said when it runs again.
    if let Some(sent) = sending {
        let _ = sent.await;
    }
    if !closing {
        send_updates(service, vault, id).await;
    }

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
async fn closing_down(stop: Option<signal::Receiver<bool>>) {
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
fn amend(vault: &Vault, id: JobId, change: impl FnOnce(&mut Job)) -> karmayoga_core::Result<bool> {
    let mut job = vault.job(id)?;
    if job.status != JobStatus::Running {
        return Ok(false);
    }
    change(&mut job);
    vault.save_job(&job)?;
    Ok(true)
}

/// Something a job wrote for the person that has been sent: what, when it
/// was written, and where it went. What marks a [`JobUpdate`] delivered.
///
/// [`JobUpdate`]: karmayoga_core::job::JobUpdate
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sent {
    at: Timestamp,
    text: String,
    to: Vec<String>,
}

/// What a job has written for the person and not had sent on yet, oldest
/// first: when each was written and what it says.
fn unsent(job: &Job) -> Vec<(Timestamp, String)> {
    job.undelivered().map(|update| (update.at, update.text.clone())).collect()
}

/// Mark as delivered the updates in `sent`, matched by when they were written
/// and what they say -- the job may have been changed since they were read,
/// and an update written meanwhile must stay to be sent. Each one only once:
/// an update already delivered is not delivered again, whatever it says.
/// Whether anything changed.
fn mark_sent(job: &mut Job, sent: &[Sent]) -> bool {
    let mut changed = false;
    for one in sent {
        let found = job
            .updates
            .iter_mut()
            .find(|update| !update.delivered && update.at == one.at && update.text == one.text);
        if let Some(update) = found {
            update.delivered = true;
            update.sent_to = one.to.clone();
            changed = true;
        }
    }
    changed
}

/// Send on what a job has written for the person, and mark it sent.
///
/// A project's own turn writes an update onto its record with
/// `report_progress`; this is what makes it reach anybody. Each goes through
/// [`Service::deliver_update`], which decides where -- the thread, the chat, a
/// voice session -- and is marked delivered only if somewhere took it: one
/// nothing could take is left to be sent at the next tick, or the next time
/// the job runs. The record is read again before it is written, for the
/// reason [`amend`] gives, so a cancellation is not undone; and the record is
/// all that changes, so what is written meanwhile is not lost.
///
/// Not for a job somebody has cancelled: whoever did that has stopped
/// listening. Never inside the blocking pool: delivering waits on a chat.
async fn send_updates(service: &Arc<Service>, vault: &Arc<Vault>, id: JobId) {
    let read = blocking({
        let vault = vault.clone();
        move || {
            Ok(vault.job(id).ok().filter(|job| job.status != JobStatus::Cancelled).map(|job| {
                let from = format!("{}: {}", job.kind.label(), short(&job.title(), FROM_CHARS));
                (unsent(&job), from, job.asked_in)
            }))
        }
    })
    .await;
    let Ok(Some((waiting, from, conversation))) = read else { return };

    let mut sent = Vec::new();
    for (at, text) in waiting {
        // Written after reading strangers' pages, and said aloud: without the
        // characters nobody can see.
        let said = karmayoga_core::hidden::strip_hidden(&text).trim().to_string();
        if said.is_empty() {
            // Nothing to say, so nothing to send; marked so that it is not
            // looked at every three seconds.
            sent.push(Sent { at, text, to: Vec::new() });
            continue;
        }
        let delivered = service
            .deliver_update(Update {
                job: Some(id),
                from: from.clone(),
                text: said,
                conversation,
                call_me: false,
            })
            .await;
        if delivered.reached() {
            sent.push(Sent { at, text, to: delivered.sent_to });
        }
    }
    if sent.is_empty() {
        return;
    }

    let saved = blocking({
        let vault = vault.clone();
        move || {
            let mut job = vault.job(id)?;
            if !mark_sent(&mut job, &sent) {
                return Ok(false);
            }
            vault.save_job(&job)?;
            Ok(true)
        }
    })
    .await;
    if matches!(saved, Ok(true)) {
        service.events().changed(job_change(id));
    }
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
            let Turned { text, prose, steps, wrote, stopped, .. } = turned;
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
/// [`task::deliver`]. A watch says nothing here either: the watch loop tells
/// the person when it ends. A project is announced as research is: "Project is
/// ready".
fn announce(service: &Arc<Service>, job: &Job) {
    // Nor does a watch, which tells the person what it has to as it goes.
    if task::is_task(job) || job.kind.is_stepped() {
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
                .domain(Scope::Agent)
                .for_user()
                .key(key),
        ),
        JobStatus::Failed => service.events().notify(
            Notification::warning(format!("{} could not finish", job.kind.label()))
                .body(job.reason.clone())
                .domain(Scope::Agent)
                .for_user()
                .key(key),
        ),
        JobStatus::Queued | JobStatus::Running | JobStatus::Waiting | JobStatus::Cancelled => {}
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
        "start_watch" => "Starting a watch".to_string(),
        "start_project" => "Starting a project".to_string(),
        "update_job" => "Changing how a job works".to_string(),
        "report_progress" => "Sending an update".to_string(),
        "ask_owner" => "Asking you in your chat".to_string(),
        "find_connection_tools" => "Looking through connected services' tools".to_string(),
        "use_connection_tool" => match (text("tool"), text("connection")) {
            (Some(tool), Some(connection)) => {
                format!("Using {} on {}", short(&tool, 50), short(&connection, 40))
            }
            _ => "Using a connected service".to_string(),
        },
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
    fn the_tools_of_watches_and_projects_have_a_line_too() {
        assert_eq!(progress_line("start_watch", &json!({})), "Starting a watch");
        assert_eq!(progress_line("start_project", &json!({})), "Starting a project");
        assert_eq!(progress_line("update_job", &json!({})), "Changing how a job works");
        assert_eq!(progress_line("report_progress", &json!({ "text": "x" })), "Sending an update");
        assert_eq!(progress_line("ask_owner", &json!({})), "Asking you in your chat");
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

    #[test]
    fn a_project_owes_its_conversation_as_research_does() {
        let asked = Some(ConversationId::new());
        let mut project = Job::project("Plan a week in Lisbon", "", asked);
        project.begin();
        assert!(!owes_the_conversation(&project), "running: nothing yet");

        let mut done = project.clone();
        done.finish("Planned.");
        assert!(owes_the_conversation(&done), "an answer");
        done.answered = true;
        assert!(!owes_the_conversation(&done), "had it");

        let mut failed = project.clone();
        failed.fail("the endpoint refused");
        assert!(owes_the_conversation(&failed), "the news");

        let mut cancelled = project;
        cancelled.cancel("they changed their mind").unwrap();
        assert!(!owes_the_conversation(&cancelled), "whoever cancelled it knows");
    }

    #[test]
    fn a_watch_never_owes_its_conversation_anything() {
        let asked = Some(ConversationId::new());
        let probe = karmayoga_core::Probe::Assistant { how: "Check the inbox".into() };
        let mut watch =
            Job::watch("CI on PR 412", probe, Default::default(), asked, Timestamp::now());
        for status in JobStatus::ALL {
            watch.status = status;
            assert!(!owes_the_conversation(&watch), "a watch in {status:?} owes nothing");
        }
    }

    // ---- which jobs run ---------------------------------------------------------

    fn research_at(minutes_ago: i64, question: &str) -> Job {
        let mut job = Job::research(question, "", None);
        job.created_at = Timestamp::now() - jiff::SignedDuration::from_mins(minutes_ago);
        job
    }

    fn questions(jobs: &[Job]) -> Vec<String> {
        jobs.iter().map(Job::title).collect()
    }

    #[test]
    fn the_oldest_jobs_run_first_and_no_more_than_there_is_room_for() {
        let queued =
            vec![research_at(1, "newest"), research_at(30, "oldest"), research_at(10, "middle")];
        let chosen = pick(queued.clone(), MAX_AT_ONCE, |_| false);
        assert_eq!(questions(&chosen), ["oldest", "middle"]);
        assert_eq!(MAX_AT_ONCE, 2, "two at a time is the rule the module documents");

        assert_eq!(questions(&pick(queued.clone(), 1, |_| false)), ["oldest"]);
        assert!(pick(queued, 0, |_| false).is_empty(), "no room, none chosen");
    }

    #[test]
    fn a_job_somebody_has_claimed_is_not_chosen_again_and_does_not_use_a_place() {
        let (a, b, c) = (research_at(30, "a"), research_at(20, "b"), research_at(10, "c"));
        let held = a.id;
        let chosen = pick(vec![a, b, c], MAX_AT_ONCE, |id| id == held);
        assert_eq!(questions(&chosen), ["b", "c"], "the claimed one makes way for the next");
    }

    #[test]
    fn a_watch_is_never_chosen_to_run_here() {
        let probe = karmayoga_core::Probe::Assistant { how: "Check the inbox".into() };
        let mut watch = Job::watch("a watch", probe, Default::default(), None, Timestamp::now());
        watch.status = JobStatus::Queued;
        watch.created_at = Timestamp::now() - jiff::SignedDuration::from_hours(1);
        let chosen = pick(vec![watch, research_at(1, "research")], MAX_AT_ONCE, |_| false);
        assert_eq!(questions(&chosen), ["research"]);
    }

    #[test]
    fn places_are_given_back_so_a_finished_job_makes_room() {
        let hands = Hands::default();
        assert_eq!(hands.free(), MAX_AT_ONCE);
        let (first, second) = (JobId::new(), JobId::new());
        hands.jobs.lock().unwrap().insert(first);
        let place = Place(hands.jobs.clone(), first);
        hands.jobs.lock().unwrap().insert(second);
        assert_eq!(hands.free(), 0);
        assert!(hands.holds(first) && hands.holds(second));

        drop(place);
        assert_eq!(hands.free(), 1, "a place is given back when its job's task is done");
        assert!(!hands.holds(first) && hands.holds(second));
    }

    // ---- what a job says while it runs -------------------------------------------

    fn project_saying(updates: &[(&str, bool)]) -> (Job, Vec<Timestamp>) {
        let mut job = Job::project("Plan a week in Lisbon", "", Some(ConversationId::new()));
        job.begin();
        let start: Timestamp = "2026-11-20T09:00:00Z".parse().unwrap();
        let mut times = Vec::new();
        for (i, (text, delivered)) in updates.iter().enumerate() {
            let at = start + jiff::SignedDuration::from_mins(i as i64 * 10);
            job.add_update(*text, at, *delivered);
            times.push(at);
        }
        (job, times)
    }

    fn sent_to(at: Timestamp, text: &str, to: &[&str]) -> Sent {
        Sent { at, text: text.into(), to: to.iter().map(|w| w.to_string()).collect() }
    }

    #[test]
    fn only_what_was_not_sent_is_still_to_send_oldest_first() {
        let (job, times) = project_saying(&[
            ("Flights found.", true),
            ("Hotels shortlisted.", false),
            ("Itinerary drafted.", false),
        ]);
        assert_eq!(
            unsent(&job),
            vec![
                (times[1], "Hotels shortlisted.".to_string()),
                (times[2], "Itinerary drafted.".to_string())
            ]
        );
    }

    #[test]
    fn a_sent_update_is_marked_delivered_with_where_it_went() {
        let (mut job, times) =
            project_saying(&[("Hotels shortlisted.", false), ("Itinerary drafted.", false)]);
        let sent = [sent_to(times[0], "Hotels shortlisted.", &["thread", "chat"])];

        assert!(mark_sent(&mut job, &sent));
        assert!(job.updates[0].delivered);
        assert_eq!(job.updates[0].sent_to, ["thread", "chat"]);
        assert!(!job.updates[1].delivered, "the one that was not sent is still to send");
        assert_eq!(unsent(&job).len(), 1);
        assert_eq!(job.last_update().map(|u| u.text.as_str()), Some("Hotels shortlisted."));
    }

    #[test]
    fn an_update_written_while_another_was_being_sent_is_left_to_be_sent() {
        let (mut job, times) = project_saying(&[("Hotels shortlisted.", false)]);
        // What was read, sent, and is now being marked...
        let sent = [sent_to(times[0], "Hotels shortlisted.", &["chat"])];
        // ...while the job, on the record, has since written another.
        let later = times[0] + jiff::SignedDuration::from_mins(5);
        job.add_update("Itinerary drafted.", later, false);

        assert!(mark_sent(&mut job, &sent));
        assert_eq!(unsent(&job).len(), 1);
        assert_eq!(unsent(&job)[0].1, "Itinerary drafted.");
    }

    #[test]
    fn an_update_is_marked_once_and_one_that_is_not_there_changes_nothing() {
        let (mut job, times) = project_saying(&[("Hotels shortlisted.", false)]);
        let sent = [sent_to(times[0], "Hotels shortlisted.", &["chat"])];
        assert!(mark_sent(&mut job, &sent));
        let marked = job.clone();

        assert!(!mark_sent(&mut job, &sent), "already delivered: nothing to do");
        assert_eq!(job, marked);

        let stranger = [sent_to(times[0], "Something else.", &["chat"])];
        assert!(!mark_sent(&mut job, &stranger), "matched by what it says as well as when");
        assert_eq!(job, marked);
    }

    #[test]
    fn two_updates_with_the_same_words_at_different_times_are_two_updates() {
        let (mut job, times) = project_saying(&[("Still going.", false), ("Still going.", false)]);
        let sent = [sent_to(times[1], "Still going.", &["chat"])];
        assert!(mark_sent(&mut job, &sent));
        assert!(!job.updates[0].delivered && job.updates[1].delivered);
    }
}
