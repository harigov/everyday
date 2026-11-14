//! Background work: the jobs the assistant takes on, and what became of
//! them. See [`crate::job`] and `docs/plans/jobs.md`.

use super::Vault;
use super::session::Domain;
use crate::error::Result;
use crate::id::{ConversationId, JobId};
use crate::job::{Job, JobKind, JobStatus};
use crate::record::RecordKind;
use crate::store::jobs::{JobQuery, JobStore};

impl Vault {
    /// Does this vault's backend hold jobs at all?
    pub fn supports_jobs(&self) -> bool {
        self.with_jobs(|_| Ok(())).is_ok()
    }

    fn with_jobs<T>(&self, f: impl FnOnce(&dyn JobStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::Jobs, |s| s.jobs().map(f))
    }

    pub fn jobs(&self, query: &JobQuery) -> Result<Vec<Job>> {
        self.with_jobs(|j| j.list_jobs(query))
    }

    pub fn job(&self, id: JobId) -> Result<Job> {
        self.with_jobs(|j| j.get_job(id))
    }

    /// Save a job, validated.
    pub fn save_job(&self, job: &Job) -> Result<()> {
        self.writable()?;
        job.validate()?;
        self.with_jobs(|j| j.put_job(job))?;
        self.wrote(RecordKind::Job, job.id);
        Ok(())
    }

    /// Queue a new job and hand it back as saved. Waking the worker is the
    /// service's business: this only writes the record it will find.
    pub fn queue_job(&self, kind: JobKind, asked_in: Option<ConversationId>) -> Result<Job> {
        let job = Job::new(kind, asked_in);
        self.save_job(&job)?;
        Ok(job)
    }

    /// Cancel a job that is queued or running, saying who did, and hand it
    /// back as saved.
    ///
    /// Only the record moves here. A running job's turn is stopped by the
    /// worker, which watches its own job's status while it runs -- so the
    /// same call works from a tool, a command, the CLI or another device,
    /// none of which can reach the worker directly.
    pub fn cancel_job(&self, id: JobId, reason: &str) -> Result<Job> {
        self.writable()?;
        let mut job = self.job(id)?;
        job.cancel(reason)?;
        self.save_job(&job)?;
        Ok(job)
    }

    /// Delete a job and its transcript. The note it wrote stays: it is a
    /// note now, and somebody may have edited it.
    ///
    /// A job still running is cancelled rather than deleted out from under
    /// the worker; delete it once it has stopped.
    pub fn delete_job(&self, id: JobId) -> Result<()> {
        self.writable()?;
        let job = self.job(id)?;
        if job.status == JobStatus::Running {
            return Err(crate::error::Error::Invalid(
                "that job is still running; cancel it first".into(),
            ));
        }
        if let Some(conversation) = job.conversation_id {
            // Best effort: a transcript already gone is not a reason to
            // refuse to delete the job.
            let _ = self.delete_conversation(conversation);
        }
        self.with_jobs(|j| j.delete_job(id))?;
        self.wrote(RecordKind::Job, id);
        Ok(())
    }

    /// How many jobs nobody has looked at. Part of the number on the app bar.
    pub fn unseen_jobs(&self) -> Result<u64> {
        self.with_jobs(|j| j.count_unseen_jobs())
    }

    /// Mark jobs as looked at. An empty list means all of them.
    pub fn mark_jobs_seen(&self, ids: &[JobId]) -> Result<()> {
        self.writable()?;
        self.with_jobs(|j| j.mark_jobs_seen(ids))?;
        for id in ids {
            self.wrote(RecordKind::Job, *id);
        }
        Ok(())
    }
}
