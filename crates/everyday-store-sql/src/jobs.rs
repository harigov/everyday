//! The assistant's background work.
//!
//! One straightforward table. Everything a job is -- its kind, its question,
//! where it stands, what it said -- is sealed; the clear columns are when it
//! was asked for and whether anybody has looked at it, which is what the list
//! and the count on the app bar are ordered by. A database that said
//! "researching the oncology appointment" would be telling somebody a great
//! deal, and so would one that said which job is running.

use everyday_core::error::Result;
use everyday_core::id::JobId;
use everyday_core::job::Job;
use everyday_core::store::jobs::{JobQuery, JobStore, job_aad};

use crate::conn::{SqlExt, ToValue, Value, Where};
use crate::record::Record;
use crate::{SqlStore, to_us, vals};

impl Record for Job {
    const TABLE: &'static str = "jobs";

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![("created_us", to_us(self.created_at).to_value()), ("seen", self.seen.to_value())]
    }
}

impl JobStore for SqlStore {
    fn list_jobs(&self, query: &JobQuery) -> Result<Vec<Job>> {
        // What an index can answer is pushed down; the rest -- the status and
        // the asking conversation, both sealed -- is finished in Rust by
        // `JobQuery::apply`, the same code path every backend uses.
        let mut w = Where::new();
        if let Some(unseen) = query.unseen {
            w = w.eq("seen", !unseen);
        }
        if let Some(since) = query.since {
            w = w.gte("created_us", to_us(since));
        }
        let (where_sql, args) = w.finish();
        let sql = format!("SELECT id, data FROM jobs WHERE {where_sql} ORDER BY created_us DESC");
        let rows = self.read().records(&sql, &args)?;
        Ok(query.apply(self.collect(rows, job_aad)?))
    }

    fn get_job(&self, id: JobId) -> Result<Job> {
        self.get(id)
    }

    fn put_job(&self, job: &Job) -> Result<()> {
        self.upsert(job)
    }

    fn delete_job(&self, id: JobId) -> Result<()> {
        self.delete_by_id::<Job>(id)?;
        Ok(())
    }

    fn count_unseen_jobs(&self) -> Result<u64> {
        let n = self.read().scalar_i64("SELECT COUNT(*) FROM jobs WHERE NOT seen", &[])?;
        Ok(n.max(0) as u64)
    }

    /// Mark jobs as looked at -- read-modify-write, for the reason
    /// `mark_runs_seen` gives: `seen` is in the clear column and inside the
    /// sealed payload, and a statement that moved only the column would
    /// leave a record that put the number straight back on the app bar.
    fn mark_jobs_seen(&self, ids: &[JobId]) -> Result<()> {
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        if ids.is_empty() {
            self.rewrite_each::<Job>(
                tx.as_mut(),
                "SELECT id, data FROM jobs WHERE NOT seen",
                &[],
                |job| job.seen = true,
            )?;
        } else {
            for id in ids {
                self.rewrite_each::<Job>(
                    tx.as_mut(),
                    "SELECT id, data FROM jobs WHERE id = ?1 AND NOT seen",
                    &vals![id.to_string()],
                    |job| job.seen = true,
                )?;
            }
        }
        tx.commit()
    }
}
