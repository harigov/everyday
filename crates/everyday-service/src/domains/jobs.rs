//! Background jobs, as commands: list them, start research by hand, cancel
//! one, mark them seen, answer again. See `docs/plans/jobs.md`.
//!
//! Eight commands, and like `run_routine` the one that starts something does
//! not do it. `start_research` writes a queued job and rings the worker's
//! bell; the worker picks it up on its own loop, one at a time, which is what
//! keeps "one job at a time" true however many times somebody presses the
//! button, and what lets a twenty-minute job go on after the command that
//! started it has answered.
//!
//! The assistant has tools of the same names for the same things -- see
//! `everyday_core::agent::tools::jobs`. They are separate tables, and a name
//! being in both is not a clash: `run_routine` already is.

use super::Nothing;
use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::jobs::answer;
use crate::service::{Service, blocking};
use everyday_core::{ConversationId, Job, JobId, JobKind, JobQuery, JobStatus};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobRef {
    pub id: JobId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Jobs {
    #[serde(default)]
    pub query: JobQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeenJobs {
    /// Which jobs to mark. Empty means all of them.
    #[serde(default)]
    pub ids: Vec<JobId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartResearch {
    /// What to find out, in the person's words.
    pub question: String,
    /// What the job should know that the question does not say: who is
    /// asking and what for, what they have ruled out.
    #[serde(default)]
    pub brief: String,
    /// The conversation to come back to with the answer. Absent for a job
    /// started from the Assistant app or the command line, which has no
    /// thread to answer in.
    #[serde(default)]
    pub conversation_id: Option<ConversationId>,
}

async fn list_jobs(svc: Arc<Service>, _ctx: Ctx, args: Jobs) -> CommandResult<Vec<Job>> {
    svc.on_vault(move |vault| vault.jobs(&args.query)).await
}

async fn get_job(svc: Arc<Service>, _ctx: Ctx, args: JobRef) -> CommandResult<Job> {
    svc.on_vault(move |vault| vault.job(args.id)).await
}

async fn unseen_jobs(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<u64> {
    svc.on_vault(move |vault| vault.unseen_jobs()).await
}

/// Queue a research job, and wake the worker.
///
/// Answers with the job as saved, still `Queued`: what the card in the thread
/// is drawn from. A `conversationId` that names no conversation is refused
/// here, in words, rather than discovered an hour later when the answer has
/// nowhere to go.
async fn start_research(svc: Arc<Service>, _ctx: Ctx, args: StartResearch) -> CommandResult<Job> {
    let vault = svc.require()?;
    let job = blocking(move || {
        if let Some(conversation) = args.conversation_id {
            vault.conversation(conversation)?;
        }
        let kind = JobKind::Research {
            question: args.question.trim().to_string(),
            brief: args.brief.trim().to_string(),
        };
        Ok(vault.queue_job(kind, args.conversation_id)?)
    })
    .await?;
    svc.wake_jobs();
    Ok(job)
}

/// Stop a job that has not finished.
///
/// The record is what moves, and the worker watches it; but a running job's
/// turn is also stopped here, at once, through the same switch `cancel_turn`
/// pulls, rather than at the worker's next look. Whatever it had written by
/// then is kept. A job that is already over is refused by the core, in words:
/// cancelling it would rewrite what happened to it.
async fn cancel_job(svc: Arc<Service>, _ctx: Ctx, args: JobRef) -> CommandResult<Job> {
    let vault = svc.require()?;
    let job = blocking(move || Ok(vault.cancel_job(args.id, "cancelled by you")?)).await?;
    // `conversation_id` is the job's own transcript, never the thread that
    // asked, so stopping what runs there cannot stop somebody mid-sentence.
    if let Some(transcript) = job.conversation_id {
        svc.pending().cancel(transcript);
    }
    Ok(job)
}

async fn delete_job(svc: Arc<Service>, _ctx: Ctx, args: JobRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_job(args.id)).await
}

async fn mark_jobs_seen(svc: Arc<Service>, _ctx: Ctx, args: SeenJobs) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.mark_jobs_seen(&args.ids)).await
}

/// Why a job cannot be answered, or `None` if it can.
///
/// Only a finished job that was asked from a conversation and has not been
/// answered is owed one. The last of those matters: the follow-up is a model
/// call, and a second click would pay for a second answer to the same
/// question.
fn unanswerable(job: &Job) -> Option<&'static str> {
    let why = match job.status {
        JobStatus::Queued | JobStatus::Running => {
            "that job has not finished yet, so there is nothing to answer from"
        }
        JobStatus::Failed => "that job failed, so there is nothing to answer from",
        JobStatus::Cancelled => "that job was cancelled, so there is nothing to answer from",
        JobStatus::Done if job.asked_in.is_none() => {
            "that job was not started from a conversation, so there is nobody to answer"
        }
        JobStatus::Done if job.answered => "that job has already been answered",
        JobStatus::Done => return None,
    };
    Some(why)
}

/// Have the follow-up turn now: what "Answer now" on a finished job's card
/// does, for the case where the automatic one failed.
///
/// Returns at once. The turn is a model call that can take minutes, and it
/// waits for any turn already running in the conversation first, so it runs in
/// the background and announces itself -- the job and the conversation each
/// raise a change when it ends, as they do for the worker's own attempts.
async fn answer_job(svc: Arc<Service>, _ctx: Ctx, args: JobRef) -> CommandResult<()> {
    let vault = svc.require()?;
    let job = blocking({
        let vault = vault.clone();
        move || Ok(vault.job(args.id)?)
    })
    .await?;
    if let Some(why) = unanswerable(&job) {
        return Err(CommandError::new(codes::INVALID, why));
    }
    let id = job.id;
    tokio::spawn(async move {
        // The failure is logged where it happens; the job stays unanswered,
        // which is the state the card offers this button from.
        let _ = answer::answer_now(&svc, &vault, id).await;
    });
    Ok(())
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "list_jobs", scope: Agent, effect: Read,
        args: Jobs, returns: "Job[]",
        signature: &[("query", "JobQuery", false)],
        run: list_jobs,
    },
    command! {
        name: "get_job", scope: Agent, effect: Read,
        args: JobRef, returns: "Job",
        signature: &[("id", "JobId", true)],
        run: get_job,
    },
    command! {
        name: "unseen_jobs", scope: Agent, effect: Read,
        args: Nothing, returns: "number", signature: &[],
        run: unseen_jobs,
    },
    command! {
        name: "start_research", scope: Agent, effect: Write,
        change: Job / Created,
        // No `id:` here, for `run_routine`'s reason: the job's id is minted
        // by the body, after the arguments were read, and the arguments name
        // a *conversation*. Putting that on a `Job` change would send a
        // listener looking for the wrong record.
        args: StartResearch, returns: "Job",
        signature: &[
            ("question", "string", true),
            ("brief", "string", false),
            ("conversationId", "ConversationId | null", false),
        ],
        run: start_research,
    },
    command! {
        name: "cancel_job", scope: Agent, effect: Write,
        change: Job / Updated,
        id: |a: &JobRef| Some(a.id.to_string()),
        args: JobRef, returns: "Job",
        signature: &[("id", "JobId", true)],
        run: cancel_job,
    },
    // Destructive like every other `delete_*` in this table, though it takes
    // less with it than most: the note a job wrote is left alone.
    command! {
        name: "delete_job", scope: Agent, effect: Destructive,
        change: Job / Deleted,
        id: |a: &JobRef| Some(a.id.to_string()),
        args: JobRef, returns: "void",
        signature: &[("id", "JobId", true)],
        run: delete_job,
    },
    command! {
        name: "mark_jobs_seen", scope: Agent, effect: Write,
        change: Job / Updated,
        // Empty means "all of them", as for `mark_runs_seen`: an empty `ids`
        // is then a listener's cue to reload the list, not to look for a
        // particular row.
        ids: |a: &SeenJobs| a.ids.iter().map(|id| id.to_string()).collect(),
        args: SeenJobs, returns: "void",
        signature: &[("ids", "JobId[]", false)],
        run: mark_jobs_seen,
    },
    command! {
        name: "answer_job", scope: Agent, effect: Write,
        change: Job / Updated,
        // The change this raises on return is the *request*: nothing has
        // been written yet. The turn raises its own, for the job and for the
        // conversation, when it ends -- see `jobs::answer`.
        id: |a: &JobRef| Some(a.id.to_string()),
        args: JobRef, returns: "void",
        signature: &[("id", "JobId", true)],
        run: answer_job,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn done() -> Job {
        let mut job = Job::research("q", "", Some(ConversationId::new()));
        job.begin();
        job.finish("It is a draw.");
        job
    }

    #[test]
    fn a_finished_unanswered_job_asked_from_a_conversation_can_be_answered() {
        assert_eq!(unanswerable(&done()), None);
    }

    #[test]
    fn a_job_that_is_not_done_has_nothing_to_answer_from() {
        let queued = Job::research("q", "", Some(ConversationId::new()));
        assert!(unanswerable(&queued).unwrap().contains("not finished"));

        let mut running = queued.clone();
        running.begin();
        assert!(unanswerable(&running).unwrap().contains("not finished"));

        let mut failed = queued.clone();
        failed.fail("the model refused");
        assert!(unanswerable(&failed).unwrap().contains("failed"));

        let mut cancelled = queued;
        cancelled.cancel("cancelled by you").unwrap();
        assert!(unanswerable(&cancelled).unwrap().contains("cancelled"));
    }

    #[test]
    fn a_job_with_nobody_to_answer_or_already_answered_is_refused() {
        let mut nobody = done();
        nobody.asked_in = None;
        assert!(unanswerable(&nobody).unwrap().contains("nobody to answer"));

        let mut answered = done();
        answered.answered = true;
        assert!(unanswerable(&answered).unwrap().contains("already been answered"));
    }
}
