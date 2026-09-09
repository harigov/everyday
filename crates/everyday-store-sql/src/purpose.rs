//! The purpose domain: roles, goals, and the reports every other table feeds.
//!
//! What stays in the clear here is only what an index is built from — which
//! role a goal is under, its status, its horizon, the ordering, and the
//! archived flag. Every word is sealed: the name of the role, the title of
//! the goal, the notes under both. The file can say that goal `7f3a` is
//! active under role `91c0` with a horizon in March and that three hours
//! went to it on Tuesday. It cannot say that `91c0` is "parent", which is
//! the whole of what somebody holding the database would actually want to
//! know.
//!
//! # The one interesting query
//!
//! [`time_by_purpose`](PurposeStore::time_by_purpose) is why the pointer is
//! two clear columns rather than a field inside a sealed payload. A block's
//! purpose is its own, else its task's, else that task's project's, and
//! `COALESCE` over a two-step `LEFT JOIN` expresses exactly that in one
//! grouped scan — a year of blocks costs an index scan and decrypts nothing.
//!
//! Note the pair of `COALESCE`s and *not* a single one over a concatenated
//! key. Coalescing kind and id separately is only correct because the two
//! columns are written and cleared together; a row with one set and not the
//! other could take its kind from the block and its id from the project and
//! name something that does not exist. That invariant is enforced by
//! [`purpose_cols`], which is the only thing in this crate that writes
//! either column, and it always writes both.

use everyday_core::error::{Error, Result};
use everyday_core::id::{GoalId, RoleId};
use everyday_core::purpose::{Goal, GoalActivity, Purpose, PurposeMinutes, Role, RoleEventMinutes};
use everyday_core::store::purpose::{GoalQuery, PurposeStore, PurposeWindow, goal_aad, role_aad};

use crate::conn::{Sql, SqlExt, Value};
use crate::{SqlStore, date_str, from_us, to_us, vals};

/// What kind of record a `purposes` row belongs to.
///
/// A closed set, spelled once. These strings are a schema, not labels: a
/// typo in one of them makes a record's purpose silently unfindable by the
/// reports while still reading back perfectly from its own sealed payload,
/// which is the most annoying class of bug this table could have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordKind {
    Project,
    Task,
    Block,
    Entry,
    Item,
    Calendar,
    Tracker,
}

impl RecordKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            RecordKind::Project => "project",
            RecordKind::Task => "task",
            RecordKind::Block => "block",
            RecordKind::Entry => "entry",
            RecordKind::Item => "item",
            RecordKind::Calendar => "calendar",
            RecordKind::Tracker => "tracker",
        }
    }
}

/// Point one record at a purpose, or clear it.
///
/// Takes the statement runner rather than the store so that it can be part
/// of the same transaction as the record's own write — the sealed payload is
/// the source of truth and this table is an index over it, and an index that
/// can commit without the thing it indexes is an index that lies.
///
/// `None` deletes rather than writing nulls: a record with no purpose has no
/// row, so the reports never have to distinguish "unset" from "set to
/// nothing", and the table stays the size of what is actually attributed.
pub(crate) fn set_purpose(
    tx: &mut dyn Sql,
    kind: RecordKind,
    id: &str,
    purpose: Option<&Purpose>,
) -> Result<()> {
    match purpose {
        Some(p) => tx.execute(
            "INSERT INTO purposes (record_kind, record_id, purpose_kind, purpose_id)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (record_kind, record_id) DO UPDATE SET
                purpose_kind = ?3, purpose_id = ?4",
            &vals![kind.as_str(), id, p.kind_str(), p.id_str()],
        )?,
        None => tx.execute(
            "DELETE FROM purposes WHERE record_kind = ?1 AND record_id = ?2",
            &vals![kind.as_str(), id],
        )?,
    };
    Ok(())
}

/// Drop the pointer rows for records that have just been deleted.
///
/// Called with the ids the delete actually removed rather than with a
/// subquery against the table they came from, because by the time this runs
/// that table no longer has them.
pub(crate) fn forget_purposes(tx: &mut dyn Sql, kind: RecordKind, ids: &[String]) -> Result<()> {
    for id in ids {
        tx.execute(
            "DELETE FROM purposes WHERE record_kind = ?1 AND record_id = ?2",
            &vals![kind.as_str(), id.as_str()],
        )?;
    }
    Ok(())
}

impl PurposeStore for SqlStore {
    // ---- roles ----------------------------------------------------------

    fn list_roles(&self) -> Result<Vec<Role>> {
        let rows = self
            .read()
            .records("SELECT id, data FROM roles ORDER BY sort_order, created_us", &[])?;
        self.collect(rows, role_aad)
    }

    fn get_role(&self, id: RoleId) -> Result<Role> {
        let sealed = self
            .read()
            .sealed("SELECT data FROM roles WHERE id = ?1", &vals![id.to_string()])?
            .ok_or_else(|| Error::not_found("role", id))?;
        self.unseal(&role_aad(id), &sealed)
    }

    fn put_role(&self, role: &Role) -> Result<()> {
        // The name and the colour are inside `data`. A database whose roles
        // table said "parent" and "recovering alcoholic" would be telling
        // somebody a great deal more than a list of shelf names would.
        let data = self.seal(&role_aad(role.id), role)?;
        self.write().execute(
            "INSERT INTO roles (id, archived, sort_order, created_us, updated_us, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (id) DO UPDATE SET
                archived = ?2, sort_order = ?3, created_us = ?4, updated_us = ?5, data = ?6",
            &vals![
                role.id.to_string(),
                role.archived,
                role.sort_order,
                to_us(role.created_at),
                to_us(role.updated_at),
                data,
            ],
        )?;
        Ok(())
    }

    fn delete_role(&self, id: RoleId) -> Result<()> {
        // Refused rather than cascaded, and the count is in the message so
        // the interface can say what is in the way rather than "no". See the
        // trait's module docs for why this one parent does not take its
        // children with it.
        let mut conn = self.write();
        let goals = conn
            .scalar_i64("SELECT COUNT(*) FROM goals WHERE role_id = ?1", &vals![id.to_string()])?;
        if goals > 0 {
            return Err(Error::Invalid(format!(
                "this role still has {goals} goal{} under it. Move them to another role, \
                 or archive this one to keep its history.",
                if goals == 1 { "" } else { "s" }
            )));
        }
        conn.execute("DELETE FROM roles WHERE id = ?1", &vals![id.to_string()])?;
        Ok(())
    }

    // ---- goals ----------------------------------------------------------

    fn list_goals(&self, query: &GoalQuery) -> Result<Vec<Goal>> {
        let mut sql = String::from("SELECT id, data FROM goals WHERE 1=1");
        let mut args: Vec<Value> = Vec::new();

        if let Some(role) = query.role_id {
            args.push(Value::Text(role.to_string()));
            sql.push_str(&format!(" AND role_id = ?{}", args.len()));
        }
        if !query.statuses.is_empty() {
            let mut names = Vec::new();
            for status in &query.statuses {
                args.push(Value::Text(status.as_str().to_string()));
                names.push(format!("?{}", args.len()));
            }
            sql.push_str(&format!(" AND status IN ({})", names.join(",")));
        }
        if let Some(to) = query.horizon_to {
            // NULL compares as neither, so an undated goal falls outside the
            // bound rather than sweeping into every quarter's list. Same
            // behaviour as `GoalQuery::matches`, deliberately.
            args.push(Value::Text(to.to_string()));
            sql.push_str(&format!(" AND horizon <= ?{}", args.len()));
        }

        let rows = self.read().records(&sql, &args)?;
        // Sorted and capped in Rust, as the library's items are: the
        // ordering is by title as often as not, which needs the payload
        // decrypted anyway, and one implementation of it cannot disagree
        // with itself across two backends.
        Ok(query.apply(self.collect(rows, goal_aad)?))
    }

    fn get_goal(&self, id: GoalId) -> Result<Goal> {
        let sealed = self
            .read()
            .sealed("SELECT data FROM goals WHERE id = ?1", &vals![id.to_string()])?
            .ok_or_else(|| Error::not_found("goal", id))?;
        self.unseal(&goal_aad(id), &sealed)
    }

    fn put_goal(&self, goal: &Goal) -> Result<()> {
        let data = self.seal(&goal_aad(goal.id), goal)?;
        self.write().execute(GOAL_UPSERT, &goal_row(goal, data))?;
        Ok(())
    }

    fn put_goals(&self, goals: &[Goal]) -> Result<()> {
        let sealed: Vec<(&Goal, Vec<u8>)> =
            goals.iter().map(|g| Ok((g, self.seal(&goal_aad(g.id), g)?))).collect::<Result<_>>()?;
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        for (goal, data) in sealed {
            tx.execute(GOAL_UPSERT, &goal_row(goal, data))?;
        }
        tx.commit()
    }

    fn delete_goal(&self, id: GoalId) -> Result<()> {
        // Records pointing here are left dangling on purpose: an
        // unresolvable pointer already reads as no purpose at all, and
        // rewriting every task, block, entry and item that mentioned this
        // goal is a great deal of writing to make one report row shorter.
        self.write().execute("DELETE FROM goals WHERE id = ?1", &vals![id.to_string()])?;
        Ok(())
    }

    fn count_goals(&self, role: RoleId) -> Result<(u64, u64)> {
        let mut conn = self.read();
        let id = vals![role.to_string()];
        let all = conn.scalar_i64("SELECT COUNT(*) FROM goals WHERE role_id = ?1", &id)?;
        let open = conn.scalar_i64(
            "SELECT COUNT(*) FROM goals WHERE role_id = ?1 AND status IN ('active', 'paused')",
            &id,
        )?;
        Ok((all.max(0) as u64, open.max(0) as u64))
    }

    // ---- the reports ----------------------------------------------------

    fn time_by_purpose(&self, window: PurposeWindow) -> Result<Vec<PurposeMinutes>> {
        // The inheritance chain, in one grouped scan: the block's own
        // pointer, else its task's, else that task's project's. Every column
        // touched is clear, so a year of blocks costs an index scan and no
        // ciphertext is opened to draw the chart.
        //
        // The two COALESCEs are over kind and id separately, which is only
        // sound because `purposes` can never hold half a pointer — the
        // column pair is `NOT NULL` and a record with no purpose has no row
        // at all. Otherwise this could take its kind from the block and its
        // id from the project and name something that does not exist.
        //
        // Summed in microseconds and divided in Rust for the reason
        // `task_stats` gives: `SUM` over an integer is exact in SQLite and a
        // `numeric` in Postgres, and only one of those truncates on divide.
        let sql = format!(
            "SELECT COALESCE(pb.purpose_kind, pt.purpose_kind, pp.purpose_kind),
                    COALESCE(pb.purpose_id,   pt.purpose_id,   pp.purpose_id),
                    b.kind,
                    CAST(COALESCE(SUM({greatest}(b.end_us - b.start_us, 0)), 0) AS BIGINT),
                    COUNT(*)
             FROM time_blocks b
             LEFT JOIN tasks t ON t.id = b.task_id
             LEFT JOIN purposes pb ON pb.record_kind = 'block' AND pb.record_id = b.id
             LEFT JOIN purposes pt ON pt.record_kind = 'task'  AND pt.record_id = b.task_id
             LEFT JOIN purposes pp ON pp.record_kind = 'project'
                                  AND pp.record_id = COALESCE(b.project_id, t.project_id)
             WHERE b.local_date >= ?1 AND b.local_date <= ?2
             GROUP BY COALESCE(pb.purpose_kind, pt.purpose_kind, pp.purpose_kind),
                      COALESCE(pb.purpose_id,   pt.purpose_id,   pp.purpose_id),
                      b.kind",
            greatest = self.dialect.greatest(),
        );
        let rows =
            self.read().query(&sql, &vals![window.from.to_string(), window.to.to_string()])?;

        // One row per purpose, folded from the two the group-by yields — a
        // purpose with planned time and no actual is one row with a zero in
        // it, because the chart wants the pair side by side.
        let mut out: Vec<PurposeMinutes> = Vec::new();
        for row in rows {
            let purpose =
                Purpose::from_columns(row.opt_text(0)?.as_deref(), row.opt_text(1)?.as_deref());
            let kind = row.text(2)?;
            let minutes = (row.i64(3)?.max(0) / 1_000_000 / 60) as u64;
            let blocks = row.u64(4)?;

            let slot = match out.iter().position(|m| m.purpose == purpose) {
                Some(i) => &mut out[i],
                None => {
                    out.push(PurposeMinutes::empty(purpose));
                    out.last_mut().expect("just pushed")
                }
            };
            if kind == "actual" {
                slot.actual_minutes += minutes;
            } else {
                slot.planned_minutes += minutes;
            }
            slot.blocks += blocks;
        }
        Ok(out)
    }

    fn events_by_role(&self, window: PurposeWindow) -> Result<Vec<RoleEventMinutes>> {
        // Events overlap the window rather than start in it, which is the
        // same two-column test the calendar's own week query uses: a holiday
        // that began last Thursday is still this week's.
        let sql = format!(
            "SELECT pc.purpose_id,
                    CAST(COALESCE(SUM({greatest}(e.end_us - e.start_us, 0)), 0) AS BIGINT),
                    COUNT(*)
             FROM events e
             LEFT JOIN purposes pc ON pc.record_kind = 'calendar'
                                  AND pc.record_id = e.calendar_id
             WHERE e.end_date >= ?1 AND e.local_date <= ?2
             GROUP BY pc.purpose_id",
            greatest = self.dialect.greatest(),
        );
        let rows =
            self.read().query(&sql, &vals![window.from.to_string(), window.to.to_string()])?;
        rows.into_iter()
            .map(|row| {
                // A calendar's row is always `role`-kinded, so the id column
                // alone is enough here. An unparseable one reads as no role
                // rather than as an error: the week still has to draw.
                let role_id = row.opt_text(0)?.and_then(|id| RoleId::parse(&id).ok());
                Ok(RoleEventMinutes {
                    role_id,
                    minutes: (row.i64(1)?.max(0) / 1_000_000 / 60) as u64,
                    events: row.u64(2)?,
                })
            })
            .collect()
    }

    fn goal_activity(&self, id: GoalId) -> Result<GoalActivity> {
        let mut conn = self.read();
        let goal = vals![id.to_string()];
        // Projects filed directly against the goal.
        let projects = conn
            .scalar_i64(
                "SELECT COUNT(*) FROM purposes
                 WHERE purpose_kind = 'goal' AND purpose_id = ?1 AND record_kind = 'project'",
                &goal,
            )?
            .max(0) as u64;
        let mut out = GoalActivity { projects, ..Default::default() };

        // Tasks, by their own pointer or their project's.
        let task_counts = conn.query(
            "SELECT t.status, COUNT(*), MAX(t.updated_us)
             FROM tasks t
             LEFT JOIN purposes pt ON pt.record_kind = 'task'    AND pt.record_id = t.id
             LEFT JOIN purposes pp ON pp.record_kind = 'project' AND pp.record_id = t.project_id
             WHERE COALESCE(pt.purpose_kind, pp.purpose_kind) = 'goal'
               AND COALESCE(pt.purpose_id,   pp.purpose_id)   = ?1
             GROUP BY t.status",
            &goal,
        )?;
        for row in task_counts {
            let status = row.text(0)?;
            let n = row.u64(1)?;
            if status == "done" || status == "cancelled" {
                out.done_tasks += n;
            } else {
                out.open_tasks += n;
            }
            out.touch(row.opt_i64(2)?.map(from_us));
        }

        // Hours, over all time and only what actually happened. Planned time
        // is an intention, and "last touched" must not be moved by an hour
        // you set aside and did not use.
        let time = conn.query_opt(
            &format!(
                "SELECT CAST(COALESCE(SUM({greatest}(b.end_us - b.start_us, 0)), 0) AS BIGINT),
                        MAX(b.start_us)
                 FROM time_blocks b
                 LEFT JOIN tasks t ON t.id = b.task_id
                 LEFT JOIN purposes pb ON pb.record_kind = 'block' AND pb.record_id = b.id
                 LEFT JOIN purposes pt ON pt.record_kind = 'task'  AND pt.record_id = b.task_id
                 LEFT JOIN purposes pp ON pp.record_kind = 'project'
                                      AND pp.record_id = COALESCE(b.project_id, t.project_id)
                 WHERE b.kind = 'actual'
                   AND COALESCE(pb.purpose_kind, pt.purpose_kind, pp.purpose_kind) = 'goal'
                   AND COALESCE(pb.purpose_id,   pt.purpose_id,   pp.purpose_id)   = ?1",
                greatest = self.dialect.greatest(),
            ),
            &goal,
        )?;
        if let Some(row) = time {
            out.actual_minutes = (row.i64(0)?.max(0) / 1_000_000 / 60) as u64;
            out.touch(row.opt_i64(1)?.map(from_us));
        }

        // Entries and shelf items, each by its own pointer. Joined back to
        // the record for the timestamp, because `purposes` holds no dates —
        // it is an index, and an index with its own clock would drift.
        for (table, entries) in [("entries", true), ("items", false)] {
            let row = conn.query_opt(
                &format!(
                    "SELECT COUNT(*), MAX(r.updated_us) FROM purposes p
                     JOIN {table} r ON r.id = p.record_id
                     WHERE p.purpose_kind = 'goal' AND p.purpose_id = ?1
                       AND p.record_kind = ?2"
                ),
                &vals![id.to_string(), if entries { "entry" } else { "item" }],
            )?;
            if let Some(row) = row {
                let n = row.u64(0)?;
                if entries {
                    out.entries = n
                } else {
                    out.items = n
                }
                out.touch(row.opt_i64(1)?.map(from_us));
            }
        }

        // Readings from the trackers that measure this goal. Two steps
        // rather than one join, because `trackers` is a handful of rows and
        // an `IN` over them beats a join against every reading in the vault.
        let trackers: Vec<String> = conn
            .query(
                "SELECT record_id FROM purposes
                 WHERE purpose_kind = 'goal' AND purpose_id = ?1 AND record_kind = 'tracker'",
                &goal,
            )?
            .into_iter()
            .map(|r| r.text(0))
            .collect::<Result<_>>()?;
        if !trackers.is_empty() {
            let names: Vec<String> = (1..=trackers.len()).map(|i| format!("?{i}")).collect();
            let args: Vec<Value> = trackers.into_iter().map(Value::Text).collect();
            let row = conn.query_opt(
                &format!(
                    "SELECT COUNT(*), MAX(created_us) FROM readings
                     WHERE tracker_id IN ({})",
                    names.join(",")
                ),
                &args,
            )?;
            if let Some(row) = row {
                out.readings = row.u64(0)?;
                out.touch(row.opt_i64(1)?.map(from_us));
            }
        }

        Ok(out)
    }
}

const GOAL_UPSERT: &str = "INSERT INTO goals
        (id, role_id, status, horizon, sort_order, created_us, updated_us, completed_us, data)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
     ON CONFLICT (id) DO UPDATE SET
        role_id = ?2, status = ?3, horizon = ?4, sort_order = ?5,
        created_us = ?6, updated_us = ?7, completed_us = ?8, data = ?9";

fn goal_row(goal: &Goal, data: Vec<u8>) -> Vec<Value> {
    vals![
        goal.id.to_string(),
        goal.role_id.to_string(),
        goal.status.as_str(),
        date_str(goal.horizon),
        goal.sort_order,
        to_us(goal.created_at),
        to_us(goal.updated_at),
        goal.completed_at.map(to_us),
        data,
    ]
}
