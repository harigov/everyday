//! The assistant's own tasks: the second kind of job.
//!
//! A task whose [`assignee`](everyday_core::Task::assignee) is the assistant
//! is queued as a [`JobKind::Task`] when its reminder comes round (see
//! `reminders`), and done here: one unattended turn, told what the task says,
//! with the tools a job has. What it ends by saying is the result, and goes
//! where [`ReminderSettings::assistants`](everyday_core::ReminderSettings::assistants)
//! says -- the person's chat unless they chose otherwise -- since nobody asked
//! for it in a thread that is waiting for an answer.
//!
//! # The task moves with the job
//!
//! It goes to doing when the job starts -- not when it is queued, so a job
//! cancelled before it begins leaves nothing claiming to be under way -- from
//! whatever open state it was in, blocked included. A job that finishes
//! leaves it done, unless the model said otherwise by moving it itself -- to
//! blocked, say, for what it could not finish. One that fails, or is
//! cancelled, puts it back to do, so the list does not claim work is going on
//! that is not.
//!
//! # The result is owed until it is sent
//!
//! [`Job::answered`] is the mark, as it is for research: set once the result
//! has gone to the chat or the desktop, and offered again on the worker's
//! every pass until then, so a result the app quit in the middle of sending
//! still arrives. A job that failed because the task was closed or deleted
//! while it waited says nothing: whoever closed it knows.

use super::{Executor, Prepared};
use crate::error::CommandResult;
use crate::events::{Change, Kind, Level, Notification, Op};
use crate::service::{Service, blocking};
use everyday_core::{Error, Job, JobId, JobKind, JobStatus, Task, TaskStatus, Vault};
use std::sync::Arc;
use std::time::Duration;

/// The most tool calls one of its tasks may make. Fewer than research's: a
/// task is one piece of work, not a survey.
pub const MAX_STEPS: u32 = 60;

/// The longest one may take.
pub const TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How late a start has to be before the prompt says so.
const LATE: jiff::SignedDuration = jiff::SignedDuration::from_mins(30);

/// What a task's turn is told in place of what is on screen. See
/// [`crate::agent::JobTurn::context`].
pub const CONTEXT: &str = "This is one of your own tasks, which you took on earlier for the \
     person whose assistant you are and are doing now, in the background. Nobody is watching \
     and nobody can answer a question, so do not ask one: take the sensible reading of the \
     task and carry on, and where something cannot be settled, say so in your reply. Anything \
     that deletes or sends something is not done in the background: it is refused. Do not try \
     to get round that; say in your reply what you would have done. Your final reply, what \
     you write after your last tool call, is sent to them exactly as you write it, so write it \
     as that message: what they need to know, short enough to read on a phone, not an account \
     of how you found it out. Nothing you write alongside a tool call on the way is sent.";

/// The kind of job this file is the executor for.
pub(crate) struct TaskJob;

impl Executor for TaskJob {
    fn max_steps(&self) -> u32 {
        MAX_STEPS
    }

    fn timeout(&self) -> Duration {
        TIMEOUT
    }

    fn context(&self) -> &'static str {
        CONTEXT
    }

    fn prepare(&self, vault: &Vault, job: &mut Job) -> everyday_core::Result<Prepared> {
        let mut task = task_of(vault, job)?;
        let zone = vault
            .agent_settings()
            .map(|s| s.now().time_zone().clone())
            .unwrap_or(jiff::tz::TimeZone::UTC);
        let mut changes = Vec::new();
        if task.status != TaskStatus::Doing {
            everyday_core::Completable::set_status(&mut task, TaskStatus::Doing);
            everyday_core::Timestamped::touch(&mut task);
            vault.save_task(&task)?;
            changes.push(task_change(&task));
        }
        Ok(Prepared { prompt: prompt(&task, &zone, jiff::Timestamp::now()), changes })
    }

    fn conclude(
        &self,
        _vault: &Vault,
        _job: &Job,
        _reply: &str,
    ) -> everyday_core::Result<Vec<Change>> {
        // The reply is the result whatever it says, and the task is moved
        // once the job's own end is written down -- see `ended`.
        Ok(Vec::new())
    }

    fn ended(&self, vault: &Vault, job: &Job) -> Vec<Change> {
        let Some(id) = job.kind.task_id() else { return Vec::new() };
        let Ok(mut task) = vault.task(id) else { return Vec::new() };
        // Only from doing: anything else is a move somebody made on purpose
        // while it ran, the model included, and is theirs.
        if task.status != TaskStatus::Doing {
            return Vec::new();
        }
        let to = match job.status {
            JobStatus::Done => TaskStatus::Done,
            JobStatus::Failed | JobStatus::Cancelled => TaskStatus::Todo,
            JobStatus::Queued | JobStatus::Running => return Vec::new(),
        };
        everyday_core::Completable::set_status(&mut task, to);
        everyday_core::Timestamped::touch(&mut task);
        if vault.save_task(&task).is_err() {
            return Vec::new();
        }
        vec![task_change(&task)]
    }
}

fn task_change(task: &Task) -> Change {
    Change {
        kind: Kind::Task,
        op: Op::Updated,
        id: Some(task.id.to_string()),
        ids: Vec::new(),
        origin: Some("assistant".to_string()),
    }
}

/// The task a job is doing, as it stands now -- refused if it has been
/// deleted, or closed while the job waited its turn: doing it anyway would
/// be doing what somebody has just said not to.
fn task_of(vault: &Vault, job: &Job) -> everyday_core::Result<Task> {
    let JobKind::Task { task_id, .. } = &job.kind else {
        return Err(Error::Invalid("not one of the assistant's tasks".into()));
    };
    let task = vault.task(*task_id).map_err(|e| match e {
        Error::NotFound { .. } => Error::Invalid("the task was deleted before it was done".into()),
        e => e,
    })?;
    if !task.status.is_open() {
        return Err(Error::Invalid(format!(
            "the task was marked {} before it was started",
            task.status.as_str()
        )));
    }
    Ok(task)
}

/// The first message of a task's transcript: what the task says, and what
/// to do about it. `now` is when it starts, which says whether it is late.
pub fn prompt(task: &Task, zone: &jiff::tz::TimeZone, now: jiff::Timestamp) -> String {
    let mut out =
        format!("This is a task of yours, and it is time to do it:\n\n{}\n\n", task.title.trim());
    if !task.notes.trim().is_empty() {
        out.push_str(&format!("Its notes:\n\n{}\n\n", task.notes.trim()));
    }
    if let Some(day) = task.due_date {
        let when = match task.due_time {
            Some(time) => format!("{} at {}", day.strftime("%A %-d %B"), time.strftime("%H:%M")),
            None => day.strftime("%A %-d %B").to_string(),
        };
        out.push_str(&format!("It is due {when}.\n\n"));
    }
    let local = |at: jiff::Timestamp| at.to_zoned(zone.clone()).strftime("%A %-d %B, %H:%M");
    if let Some(meant) = task.remind_at
        && now.duration_since(meant) > LATE
    {
        out.push_str(&format!(
            "It was meant to be done at {}, but the app was not running then, so it is late: \
             if being late changes what is worth doing, do that instead and say so.\n\n",
            local(meant)
        ));
    }
    out.push_str(&format!(
        "It is now {}. Its task_id is {}. Do it now, with the tools you have. When you finish, \
         it is marked done for you. If you could not finish it, set it to blocked with \
         update_task and say in your reply what is left.",
        local(now),
        task.id
    ));
    out
}

/// Send what one of its tasks came to, where the person said, and mark it
/// sent. Nothing when it has been sent already -- the worker offers it on
/// every pass until it has -- and nothing but the mark for a job that failed
/// because the task was closed or deleted under it.
///
/// A chat that is not connected, or does not take it, sends it to the
/// desktop instead.
pub(crate) async fn deliver(
    service: &Arc<Service>,
    vault: &Arc<Vault>,
    id: JobId,
) -> CommandResult<()> {
    let looked = blocking({
        let vault = vault.clone();
        move || {
            let job = vault.job(id)?;
            let route = vault.agent_settings()?.reminders.assistants;
            // Closed or gone: the job could only have failed for that, and
            // whoever did it knows.
            let task_open = job
                .kind
                .task_id()
                .and_then(|task| vault.task(task).ok())
                .is_some_and(|task| task.status.is_open());
            Ok((job, route, task_open))
        }
    })
    .await?;
    let (job, route, task_open) = looked;
    if job.answered || !matches!(job.status, JobStatus::Done | JobStatus::Failed) {
        return Ok(());
    }
    let say = job.status == JobStatus::Done || task_open;
    if say {
        let title = job.title();
        let mut desktop = route.desktop();
        if route.chat() {
            let sent = crate::reminders::to_chat(
                service,
                vault,
                &title,
                &title,
                chat_message(&job),
                job.conversation_id,
            )
            .await;
            if let Err(why) = sent {
                tracing::info!(job = %job.id, %why, "a task's result went to the desktop instead");
                desktop = true;
            }
        }
        if desktop {
            service.events().notify(notification(&job));
        }
    }
    blocking({
        let vault = vault.clone();
        move || {
            let mut job = vault.job(id)?;
            job.answered = true;
            Ok(vault.save_job(&job)?)
        }
    })
    .await?;
    service.events().changed(Change {
        kind: Kind::Job,
        op: Op::Updated,
        id: Some(id.to_string()),
        ids: Vec::new(),
        origin: Some("assistant".to_string()),
    });
    Ok(())
}

/// Whether `job` is doing one of the assistant's own tasks.
pub(crate) fn is_task(job: &Job) -> bool {
    job.kind.task_id().is_some()
}

/// What a task's result says in the chat: its title, then what the model
/// ended with, or why it could not finish.
fn chat_message(job: &Job) -> String {
    let title = crate::chat::escape_markdown(&job.title());
    match job.status {
        JobStatus::Failed => format!("**{title}** could not be finished: {}", job.reason.trim()),
        _ if job.summary.trim().is_empty() => format!("**{title}**\n\nDone."),
        _ => format!("**{title}**\n\n{}", job.summary.trim()),
    }
}

/// The desktop's word on it. What it found stays out, for the reason
/// `scheduler::announce` keeps a routine's findings out: a notification may
/// be drawn over a lock screen. The title is the person's or the assistant's
/// own words about the task, and is what says which one it was.
fn notification(job: &Job) -> Notification {
    let key = format!("job:{}", job.id);
    match job.status {
        JobStatus::Failed => Notification::warning(format!("Could not finish: {}", job.title()))
            .body(job.reason.clone())
            .for_user()
            .key(key),
        _ => Notification::new(Level::Success, format!("Done: {}", job.title()))
            .body("The result is in the Assistant app.")
            .for_user()
            .key(key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::TaskId;

    fn job_for(title: &str) -> Job {
        Job::new(JobKind::Task { task_id: TaskId::new(), title: title.into() }, None)
    }

    #[test]
    fn the_chat_is_sent_the_title_and_what_it_ended_with() {
        let mut job = job_for("Find a shuttle from MCO");
        job.finish("Two run at 6am; I'd book the Go Port one.");
        assert_eq!(
            chat_message(&job),
            "**Find a shuttle from MCO**\n\nTwo run at 6am; I'd book the Go Port one."
        );
        job.summary.clear();
        assert_eq!(chat_message(&job), "**Find a shuttle from MCO**\n\nDone.");
        job.fail("it was still going after 30 minutes and was stopped");
        assert_eq!(
            chat_message(&job),
            "**Find a shuttle from MCO** could not be finished: it was still going after 30 \
             minutes and was stopped"
        );
    }

    #[test]
    fn the_prompt_carries_the_task_and_how_to_leave_it() {
        let mut task = Task::new("Look into shuttles from MCO");
        task.notes = "Three of us, one car seat.".into();
        task.due_date = Some(jiff::civil::date(2026, 10, 28));
        task.due_time = Some(jiff::civil::time(17, 0, 0, 0));
        let now: jiff::Timestamp = "2026-10-27T20:00:00Z".parse().unwrap();
        task.remind_at = Some(now);
        let said = prompt(&task, &jiff::tz::TimeZone::UTC, now);
        assert!(said.contains("Look into shuttles from MCO"));
        assert!(said.contains("Three of us, one car seat."));
        assert!(said.contains("due Wednesday 28 October at 17:00"));
        assert!(said.contains(&task.id.to_string()));
        assert!(said.contains("set it to blocked"));
        assert!(!said.contains("is late"), "on time");
    }

    #[test]
    fn a_late_start_is_told_when_it_was_meant_for() {
        let mut task = Task::new("Check the flight");
        task.remind_at = Some("2026-10-30T16:00:00Z".parse().unwrap());
        let now: jiff::Timestamp = "2026-10-31T17:00:00Z".parse().unwrap();
        let said = prompt(&task, &jiff::tz::TimeZone::UTC, now);
        assert!(said.contains("meant to be done at Friday 30 October, 16:00"), "{said}");
        assert!(said.contains("so it is late"));
    }
}
