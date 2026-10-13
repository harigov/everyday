//! Storage for the assistant's background work.
//!
//! One record and no cascade into the rest of the vault: a job that wrote a
//! note and is then deleted leaves the note alone. What it takes with it is
//! its own transcript, which the vault removes either side of the delete
//! because conversations belong to the agent store.
//!
//! In the clear: that a job exists, when it was asked for and whether
//! anybody has looked at it -- which is what the list and the count on the
//! app bar are ordered by. Sealed: its kind, its question and brief, where it
//! stands, and everything it said.

use crate::Result;
use crate::id::{ConversationId, JobId};
use crate::job::{Job, JobStatus};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Filter and pagination for [`JobStore::list_jobs`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct JobQuery {
    /// Keep only jobs in one of these states. Empty means any.
    pub statuses: Vec<JobStatus>,
    /// Keep only jobs nobody has looked at (`Some(true)`), or only those
    /// somebody has (`Some(false)`).
    pub unseen: Option<bool>,
    /// Keep only jobs started from this conversation.
    pub asked_in: Option<ConversationId>,
    /// Jobs asked for at or after this.
    pub since: Option<Timestamp>,
    pub limit: Option<u32>,
}

impl JobQuery {
    /// What the worker asks on every pass: everything not over yet.
    pub fn unfinished() -> Self {
        Self { statuses: vec![JobStatus::Queued, JobStatus::Running], ..Default::default() }
    }

    pub fn unseen(limit: u32) -> Self {
        Self { unseen: Some(true), limit: Some(limit), ..Default::default() }
    }

    pub fn asked_in(conversation: ConversationId) -> Self {
        Self { asked_in: Some(conversation), ..Default::default() }
    }

    /// Does this job pass the filters? The fallback for a backend that
    /// cannot express one natively, so behaviour is identical across them.
    pub fn matches(&self, job: &Job) -> bool {
        if let Some(unseen) = self.unseen
            && job.seen == unseen
        {
            return false;
        }
        if let Some(conversation) = self.asked_in
            && job.asked_in != Some(conversation)
        {
            return false;
        }
        if let Some(since) = self.since
            && job.created_at < since
        {
            return false;
        }
        self.statuses.is_empty() || self.statuses.contains(&job.status)
    }

    /// Filter, sort newest first and cap. Done here rather than in SQL so
    /// that two backends cannot disagree about where a tie goes.
    pub fn apply(&self, mut rows: Vec<Job>) -> Vec<Job> {
        rows.retain(|j| self.matches(j));
        rows.sort_by(|a, b| {
            b.created_at.cmp(&a.created_at).then_with(|| b.id.to_string().cmp(&a.id.to_string()))
        });
        if let Some(limit) = self.limit {
            rows.truncate(limit as usize);
        }
        rows
    }
}

/// What a backend must do to hold jobs.
pub trait JobStore: Send + Sync {
    fn list_jobs(&self, query: &JobQuery) -> Result<Vec<Job>>;
    fn get_job(&self, id: JobId) -> Result<Job>;
    /// Idempotent: saving the same job twice leaves one.
    fn put_job(&self, job: &Job) -> Result<()>;
    /// Deleting one that is not there is not an error. Its transcript is
    /// the vault's to remove -- see [`crate::Vault::delete_job`].
    fn delete_job(&self, id: JobId) -> Result<()>;
    /// How many jobs nobody has looked at. One indexed count.
    fn count_unseen_jobs(&self) -> Result<u64>;
    /// Mark jobs as looked at. Empty means all of them.
    fn mark_jobs_seen(&self, ids: &[JobId]) -> Result<()>;
}

/// Additional authenticated data for a sealed job payload.
pub fn job_aad(id: JobId) -> Vec<u8> {
    format!("everyday.job.v1:{id}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::RoutineRunId;
    use crate::store::routines::run_aad;
    use uuid::Uuid;

    fn at(second: i64) -> Timestamp {
        Timestamp::from_second(1_700_000_000 + second).unwrap()
    }

    /// A job asked for `second` seconds after an arbitrary start.
    fn job(second: i64, status: JobStatus, seen: bool) -> Job {
        let mut job = Job::research(format!("Question {second}"), "", None);
        job.created_at = at(second);
        job.status = status;
        job.seen = seen;
        job
    }

    fn titles(rows: &[Job]) -> Vec<String> {
        rows.iter().map(Job::title).collect()
    }

    #[test]
    fn an_empty_query_keeps_everything_newest_first() {
        let rows = vec![
            job(10, JobStatus::Done, true),
            job(30, JobStatus::Running, false),
            job(20, JobStatus::Failed, true),
        ];
        let got = JobQuery::default().apply(rows);
        assert_eq!(titles(&got), ["Question 30", "Question 20", "Question 10"]);
    }

    #[test]
    fn jobs_asked_for_at_the_same_moment_fall_in_a_fixed_order() {
        // Two backends sorting a tie differently would list the same rows in
        // a different order, which is the one thing `apply` exists to rule out.
        let mut low = job(10, JobStatus::Done, true);
        low.id = JobId::from(Uuid::from_u128(1));
        let mut high = job(10, JobStatus::Done, true);
        high.id = JobId::from(Uuid::from_u128(2));

        let forwards = JobQuery::default().apply(vec![low.clone(), high.clone()]);
        let backwards = JobQuery::default().apply(vec![high.clone(), low.clone()]);
        assert_eq!(forwards, backwards);
        assert_eq!(forwards[0].id, high.id, "the larger id first, as newest-first would have it");
    }

    #[test]
    fn a_status_filter_wants_any_of_them_rather_than_all() {
        let rows = vec![
            job(1, JobStatus::Queued, false),
            job(2, JobStatus::Running, false),
            job(3, JobStatus::Done, false),
            job(4, JobStatus::Cancelled, false),
        ];
        let only_done = JobQuery { statuses: vec![JobStatus::Done], ..Default::default() };
        assert_eq!(titles(&only_done.apply(rows.clone())), ["Question 3"]);

        let two = JobQuery {
            statuses: vec![JobStatus::Done, JobStatus::Cancelled],
            ..Default::default()
        };
        assert_eq!(titles(&two.apply(rows.clone())), ["Question 4", "Question 3"]);

        assert_eq!(titles(&JobQuery::unfinished().apply(rows)), ["Question 2", "Question 1"]);
    }

    #[test]
    fn the_unseen_filter_means_what_it_says() {
        let rows = vec![job(1, JobStatus::Done, false), job(2, JobStatus::Done, true)];
        let unseen = JobQuery { unseen: Some(true), ..Default::default() };
        assert_eq!(titles(&unseen.apply(rows.clone())), ["Question 1"]);
        let seen = JobQuery { unseen: Some(false), ..Default::default() };
        assert_eq!(titles(&seen.apply(rows.clone())), ["Question 2"]);
        assert_eq!(JobQuery::default().apply(rows).len(), 2, "unsaid means both");
    }

    #[test]
    fn asked_in_keeps_only_that_conversations_jobs() {
        let here = ConversationId::new();
        let elsewhere = ConversationId::new();
        let mut mine = job(1, JobStatus::Done, true);
        mine.asked_in = Some(here);
        let mut theirs = job(2, JobStatus::Done, true);
        theirs.asked_in = Some(elsewhere);
        let from_the_app = job(3, JobStatus::Done, true);

        let got = JobQuery::asked_in(here).apply(vec![mine.clone(), theirs, from_the_app]);
        assert_eq!(got, vec![mine], "a job nobody asked for in a thread is in no thread's list");
    }

    #[test]
    fn since_includes_the_moment_it_names() {
        let rows = vec![job(10, JobStatus::Done, true), job(20, JobStatus::Done, true)];
        let q = JobQuery { since: Some(at(20)), ..Default::default() };
        assert_eq!(titles(&q.apply(rows.clone())), ["Question 20"], "at or after, not after");
        let q = JobQuery { since: Some(at(15)), ..Default::default() };
        assert_eq!(titles(&q.apply(rows)), ["Question 20"]);
    }

    #[test]
    fn a_limit_keeps_the_newest_after_the_filters_have_run() {
        let rows = vec![
            job(1, JobStatus::Done, true),
            job(2, JobStatus::Queued, true),
            job(3, JobStatus::Done, true),
            job(4, JobStatus::Queued, true),
        ];
        let q = JobQuery { limit: Some(1), ..Default::default() };
        assert_eq!(titles(&q.apply(rows.clone())), ["Question 4"]);

        // Filter first, then cap: a limit of one on the done jobs is the
        // newest *done* job, not whatever the newest job happened to be.
        let q = JobQuery { statuses: vec![JobStatus::Done], limit: Some(1), ..Default::default() };
        assert_eq!(titles(&q.apply(rows.clone())), ["Question 3"]);

        let q = JobQuery { limit: Some(0), ..Default::default() };
        assert!(q.apply(rows).is_empty(), "zero is zero");
    }

    #[test]
    fn filters_narrow_one_another() {
        let here = ConversationId::new();
        let mut wanted = job(5, JobStatus::Done, false);
        wanted.asked_in = Some(here);
        let mut already_seen = job(6, JobStatus::Done, true);
        already_seen.asked_in = Some(here);
        let mut still_going = job(7, JobStatus::Running, false);
        still_going.asked_in = Some(here);
        let too_old = {
            let mut old = job(1, JobStatus::Done, false);
            old.asked_in = Some(here);
            old
        };

        let q = JobQuery {
            statuses: vec![JobStatus::Done],
            unseen: Some(true),
            asked_in: Some(here),
            since: Some(at(2)),
            limit: Some(10),
        };
        let got = q.apply(vec![wanted.clone(), already_seen, still_going, too_old]);
        assert_eq!(got, vec![wanted]);
    }

    #[test]
    fn the_shorthand_constructors_ask_what_their_names_say() {
        let unfinished = JobQuery::unfinished();
        assert_eq!(unfinished.statuses, [JobStatus::Queued, JobStatus::Running]);
        assert_eq!(unfinished.limit, None);

        let unseen = JobQuery::unseen(7);
        assert_eq!((unseen.unseen, unseen.limit), (Some(true), Some(7)));
        assert!(unseen.statuses.is_empty());

        let conversation = ConversationId::new();
        assert_eq!(JobQuery::asked_in(conversation).asked_in, Some(conversation));
    }

    #[test]
    fn a_query_from_the_interface_may_name_only_what_it_cares_about() {
        let nothing: JobQuery = serde_json::from_str("{}").unwrap();
        assert!(nothing.statuses.is_empty() && nothing.unseen.is_none() && nothing.limit.is_none());

        let some: JobQuery =
            serde_json::from_str(r#"{ "statuses": ["queued", "running"], "unseen": true }"#)
                .unwrap();
        assert_eq!(some.statuses, [JobStatus::Queued, JobStatus::Running]);
        assert_eq!(some.unseen, Some(true));
    }

    #[test]
    fn a_job_is_sealed_under_a_label_of_its_own() {
        let id = JobId::new();
        let label = String::from_utf8(job_aad(id)).unwrap();
        assert_eq!(label, format!("everyday.job.v1:{id}"));

        // The same uuid under a routine run's label is a different string, so
        // a job's ciphertext cannot be opened as a run (or the other way
        // round) by moving it between tables.
        assert_ne!(job_aad(id), run_aad(RoutineRunId::from(id.0)));
        assert_ne!(job_aad(id), job_aad(JobId::new()), "and every job has its own");
    }
}
