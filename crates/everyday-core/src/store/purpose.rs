//! Storage for the purpose domain: roles, goals, and the reports that only
//! exist because everything else points at them.
//!
//! # Why this is a fifth trait
//!
//! The same reason [`LibraryStore`](super::library::LibraryStore) is a
//! fourth. A backend that keeps journals as a tree of Markdown files has no
//! opinion about what a life is made of, and folding this into
//! [`JournalStore`](super::JournalStore) would oblige it to grow one. It is
//! reached through [`JournalStore::purpose`](super::JournalStore::purpose),
//! which returns `None` by default, and the interface reads
//! [`Capabilities::goals`](super::Capabilities::goals) to know whether to
//! offer the app at all.
//!
//! # Two records, and the cascade that is refused
//!
//! ```text
//!   Role ────── Goal
//! ```
//!
//! Deleting a role with goals under it is **refused**, not cascaded. That is
//! the opposite of every other parent in this vault — a kind takes its
//! items, a project takes its tasks — and the difference is deliberate. An
//! item is *made of* its shelf: without the kind it has no fields, no verbs
//! and nowhere to be drawn. A goal is not made of its role in that way; it
//! is a thing you wanted, with a year of attributed hours behind it, and one
//! click on a sidebar row is the wrong distance from losing all of that. So
//! the store returns [`Error::Invalid`](crate::Error::Invalid) naming the
//! count, and the interface offers archiving instead — which is what
//! somebody reorganising their roles actually meant.
//!
//! Records pointing at a deleted *goal* are a different matter and are left
//! dangling on purpose. [`Purpose::from_columns`](crate::purpose::Purpose)
//! already reads an unresolvable pointer as no purpose at all, so a block
//! whose goal is gone reports as unattributed rather than as an error, and
//! the alternative — rewriting every task, block, entry and item that
//! mentioned it — is a great deal of writing to make a report one row
//! shorter.
//!
//! # The reports are the point
//!
//! [`PurposeStore::time_by_purpose`] is why the pointer is two clear columns
//! instead of a field inside the sealed payload. It resolves the inheritance
//! chain — a block's own purpose, else its task's, else that task's
//! project's — in one `GROUP BY` over indexed columns, so a year of blocks
//! costs an index scan and decrypts nothing. Doing it in Rust would mean
//! decrypting every block, task and project in the vault to draw a bar
//! chart, which is the sort of thing that is fine at ten rows and unusable
//! at ten thousand.

use crate::error::Result;
use crate::id::{GoalId, RoleId};
use crate::purpose::{Goal, GoalActivity, GoalStatus, PurposeMinutes, Role, RoleEventMinutes};
use jiff::civil::Date;
use serde::{Deserialize, Serialize};

/// Filter for [`PurposeStore::list_goals`].
///
/// All filters are ANDed. An empty query matches every goal in the vault,
/// which is what the Overview's own sidebar asks for.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GoalQuery {
    /// Restrict to one role. `None` means every role.
    pub role_id: Option<RoleId>,
    /// Empty means any status.
    pub statuses: Vec<GoalStatus>,
    /// Horizon on or before this day. The "what is due this quarter" filter.
    pub horizon_to: Option<Date>,
    /// `None` means no limit.
    pub limit: Option<u32>,
}

impl GoalQuery {
    /// Everything still being pursued, paused ones included.
    pub fn open() -> Self {
        Self { statuses: vec![GoalStatus::Active, GoalStatus::Paused], ..Default::default() }
    }

    pub fn under(role: RoleId) -> Self {
        Self { role_id: Some(role), ..Default::default() }
    }

    /// Does this goal pass the filters? Backends that cannot express a
    /// filter natively fall back to this, so behaviour stays identical
    /// across backends.
    pub fn matches(&self, goal: &Goal) -> bool {
        if let Some(role) = self.role_id
            && goal.role_id != role
        {
            return false;
        }
        if !self.statuses.is_empty() && !self.statuses.contains(&goal.status) {
            return false;
        }
        if let Some(to) = self.horizon_to
            && goal.horizon.is_none_or(|h| h > to)
        {
            return false;
        }
        true
    }

    /// Sort and cap. Ordered by the role's own ordering first so a list
    /// reads as sections, then by `sort_order`, then by title — the same
    /// three-step the todo app's projects use.
    pub fn apply(&self, mut goals: Vec<Goal>) -> Vec<Goal> {
        goals.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
                .then_with(|| a.id.cmp(&b.id))
        });
        if let Some(limit) = self.limit {
            goals.truncate(limit as usize);
        }
        goals
    }
}

/// The window a time report covers, in local days.
///
/// Inclusive at both ends, because both ends are days somebody named: "this
/// week" means Monday *and* Sunday. Dates rather than instants for the
/// reason every other range query in this vault takes dates — the clear
/// `local_date` column is what the index is on, and it is already filed in
/// the author's zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PurposeWindow {
    pub from: Date,
    pub to: Date,
}

impl PurposeWindow {
    pub fn new(from: Date, to: Date) -> Self {
        // A window quoted backwards is a caller's slip, not a state worth
        // modelling: swapping is what they meant and returning nothing is
        // an empty chart nobody can debug.
        if to < from { Self { from: to, to: from } } else { Self { from, to } }
    }

    pub fn contains(&self, day: Date) -> bool {
        day >= self.from && day <= self.to
    }
}

/// The persistence contract for the purpose domain.
pub trait PurposeStore: Send + Sync {
    // ---- roles ----------------------------------------------------------

    /// Every role, archived ones included. There are a handful of these, so
    /// filtering is the caller's business.
    fn list_roles(&self) -> Result<Vec<Role>>;

    fn get_role(&self, id: RoleId) -> Result<Role>;

    /// Insert or replace. Implementations must be idempotent.
    fn put_role(&self, role: &Role) -> Result<()>;

    /// Delete the role, refusing while any goal still points at it.
    ///
    /// See the module docs for why this is a refusal rather than a cascade.
    /// Implementations must count first and fail with
    /// [`Error::Invalid`](crate::Error::Invalid) naming the number of goals,
    /// so the interface can say what is in the way.
    fn delete_role(&self, id: RoleId) -> Result<()>;

    // ---- goals ----------------------------------------------------------

    fn list_goals(&self, query: &GoalQuery) -> Result<Vec<Goal>>;

    fn get_goal(&self, id: GoalId) -> Result<Goal>;

    fn put_goal(&self, goal: &Goal) -> Result<()>;

    /// One write for many goals: what a re-ordered list is, and what moving
    /// three goals to another role is. Atomic where the backend can be.
    fn put_goals(&self, goals: &[Goal]) -> Result<()> {
        for goal in goals {
            self.put_goal(goal)?;
        }
        Ok(())
    }

    /// Delete the goal. Records pointing at it are left dangling and read as
    /// unattributed; see the module docs.
    fn delete_goal(&self, id: GoalId) -> Result<()>;

    /// How many goals point at one role, and how many of those are open.
    ///
    /// For the line under a role's name in the sidebar, and for the refusal
    /// message [`delete_role`](PurposeStore::delete_role) raises. Counted by
    /// the backend over the whole vault rather than derived in the
    /// interface, which only ever holds the page it is showing.
    fn count_goals(&self, role: RoleId) -> Result<(u64, u64)>;

    // ---- the reports ----------------------------------------------------

    /// Minutes per resolved purpose over a window, planned and actual.
    ///
    /// Resolution is a block's own purpose, else its task's, else that
    /// task's project's — and a block whose chain yields nothing lands in
    /// the row with `purpose: None`, which is always present.
    ///
    /// One row per distinct purpose. Ordering is the implementation's
    /// business and callers must not depend on it; the interface groups by
    /// role and sorts for itself.
    fn time_by_purpose(&self, window: PurposeWindow) -> Result<Vec<PurposeMinutes>>;

    /// Minutes of subscribed-calendar events per role over a window.
    ///
    /// Attributed by the calendar's own `role_id`, because a feed serves a
    /// role and its individual events are not yours to file. Separate from
    /// [`time_by_purpose`](PurposeStore::time_by_purpose) rather than summed
    /// into it — see [`RoleEventMinutes`] for why.
    fn events_by_role(&self, window: PurposeWindow) -> Result<Vec<RoleEventMinutes>>;

    /// Everything recorded against one goal, over all time.
    fn goal_activity(&self, id: GoalId) -> Result<GoalActivity>;
}

/// Associated data bound into a record's ciphertext. See
/// [`entry_aad`](super::entry_aad) for why records are bound to their id.
pub fn role_aad(id: RoleId) -> Vec<u8> {
    format!("everyday.role.v1:{id}").into_bytes()
}

pub fn goal_aad(id: GoalId) -> Vec<u8> {
    format!("everyday.goal.v1:{id}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn goal(role: RoleId, title: &str) -> Goal {
        Goal::new(role, title)
    }

    #[test]
    fn an_empty_query_matches_everything() {
        assert!(GoalQuery::default().matches(&goal(RoleId::new(), "anything")));
    }

    #[test]
    fn the_open_query_keeps_paused_goals_and_drops_finished_ones() {
        let role = RoleId::new();
        let q = GoalQuery::open();

        let mut paused = goal(role, "later");
        paused.set_status(GoalStatus::Paused);
        assert!(q.matches(&paused), "paused is on the books, just not this month");

        let mut done = goal(role, "finished");
        done.set_status(GoalStatus::Done);
        assert!(!q.matches(&done));

        let mut dropped = goal(role, "gave up");
        dropped.set_status(GoalStatus::Dropped);
        assert!(!q.matches(&dropped));
    }

    #[test]
    fn a_horizon_filter_excludes_goals_that_have_no_horizon() {
        // A goal with no date is not "due by any date": it is undated, and
        // sweeping it into every quarter's list would drown the ones that
        // actually have a deadline.
        let role = RoleId::new();
        let q = GoalQuery { horizon_to: Some(date(2026, 12, 31)), ..Default::default() };
        assert!(!q.matches(&goal(role, "someday")));
        assert!(q.matches(&goal(role, "soon").with_horizon(date(2026, 6, 1))));
        assert!(!q.matches(&goal(role, "later").with_horizon(date(2027, 6, 1))));
        assert!(q.matches(&goal(role, "on the day").with_horizon(date(2026, 12, 31))));
    }

    #[test]
    fn a_role_filter_only_keeps_its_own() {
        let mine = RoleId::new();
        let theirs = RoleId::new();
        let q = GoalQuery::under(mine);
        assert!(q.matches(&goal(mine, "x")));
        assert!(!q.matches(&goal(theirs, "y")));
    }

    #[test]
    fn goals_sort_by_order_then_title_and_the_limit_is_applied_last() {
        let role = RoleId::new();
        let mut a = goal(role, "Banana");
        a.sort_order = 1;
        let mut b = goal(role, "apple");
        b.sort_order = 0;
        let mut c = goal(role, "Cherry");
        c.sort_order = 0;

        let sorted = GoalQuery::default().apply(vec![a.clone(), b.clone(), c.clone()]);
        let titles: Vec<&str> = sorted.iter().map(|g| g.title.as_str()).collect();
        // Case-insensitively, so "apple" leads "Cherry" rather than trailing
        // it on a byte comparison.
        assert_eq!(titles, ["apple", "Cherry", "Banana"]);

        let capped = GoalQuery { limit: Some(2), ..Default::default() }.apply(vec![a, b, c]);
        assert_eq!(capped.len(), 2);
        assert_eq!(capped[0].title, "apple");
    }

    #[test]
    fn a_window_quoted_backwards_is_read_the_way_round_it_was_meant() {
        let w = PurposeWindow::new(date(2026, 9, 30), date(2026, 9, 1));
        assert_eq!(w.from, date(2026, 9, 1));
        assert_eq!(w.to, date(2026, 9, 30));
        assert!(w.contains(date(2026, 9, 1)), "both ends are days somebody named");
        assert!(w.contains(date(2026, 9, 30)));
        assert!(!w.contains(date(2026, 8, 31)));
        assert!(!w.contains(date(2026, 10, 1)));
    }

    #[test]
    fn the_two_kinds_of_record_are_sealed_under_different_labels() {
        // The same argument the library's three make: a role's ciphertext
        // must not be substitutable for a goal's by anyone who can write to
        // the database.
        let uuid = uuid::Uuid::now_v7();
        assert_ne!(role_aad(RoleId::from(uuid)), goal_aad(GoalId::from(uuid)));
        assert!(role_aad(RoleId::from(uuid)).starts_with(b"everyday.role."));
        assert!(goal_aad(GoalId::from(uuid)).starts_with(b"everyday.goal."));
    }
}
