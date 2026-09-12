//! Roles and goals: what everything else in the vault answers "what was
//! this for" to.
//!
//! The purpose domain, on exactly the terms of every domain before it. What
//! is different is that almost nothing here is *about* roles and goals: the
//! two records are small and dull, and the interesting half is the pair
//! of reports, which read every other domain's tables and are the only
//! reason the pointer exists.

use super::Vault;
use super::session::Domain;
use crate::error::{Error, Result};
use crate::id::{GoalId, RoleId};
use crate::purpose::{Goal, GoalActivity, PurposeMinutes, Role, RoleEventMinutes, suggested_roles};
use crate::store::purpose::{GoalQuery, PurposeStore, PurposeWindow};

impl Vault {
    /// Does this vault's backend store roles and goals?
    pub fn supports_purpose(&self) -> bool {
        self.with_purpose(|_| Ok(())).is_ok()
    }

    fn with_purpose<T>(&self, f: impl FnOnce(&dyn PurposeStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::Purpose, |s| s.purpose().map(f))
    }

    pub fn roles(&self) -> Result<Vec<Role>> {
        self.with_purpose(|p| {
            let mut roles = p.list_roles()?;
            roles.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then_with(|| a.name.cmp(&b.name)));
            Ok(roles)
        })
    }

    pub fn role(&self, id: RoleId) -> Result<Role> {
        self.with_purpose(|p| p.get_role(id))
    }

    pub fn save_role(&self, role: &Role) -> Result<()> {
        self.writable()?;
        if role.name.trim().is_empty() {
            return Err(Error::Invalid("a role needs a name".into()));
        }
        self.with_purpose(|p| p.put_role(role))
    }

    /// Delete a role, which the backend refuses while goals point at it.
    pub fn delete_role(&self, id: RoleId) -> Result<()> {
        self.writable()?;
        self.with_purpose(|p| p.delete_role(id))
    }

    /// How many goals sit under a role, and how many are still open.
    pub fn count_goals(&self, role: RoleId) -> Result<(u64, u64)> {
        self.with_purpose(|p| p.count_goals(role))
    }

    /// Put a starting set of roles in a vault that has none, and do nothing
    /// at all otherwise. Returns how many were added.
    ///
    /// Unlike [`seed_library`](Vault::seed_library) this is **not** called on
    /// unlock. A list of shelves is a guess about what people read; a list
    /// of roles is a claim about what somebody's life is made of, and an
    /// application that wrote one unasked would be telling them who they
    /// are. The Overview offers it behind a button on an empty screen, and
    /// somebody who deletes the lot never sees it again.
    pub fn seed_roles(&self) -> Result<usize> {
        self.writable()?;
        self.with_purpose(|p| {
            if !p.list_roles()?.is_empty() {
                return Ok(0);
            }
            let seeds = suggested_roles();
            for role in &seeds {
                p.put_role(role)?;
            }
            Ok(seeds.len())
        })
    }

    pub fn goals(&self, query: &GoalQuery) -> Result<Vec<Goal>> {
        self.with_purpose(|p| p.list_goals(query))
    }

    pub fn goal(&self, id: GoalId) -> Result<Goal> {
        self.with_purpose(|p| p.get_goal(id))
    }

    pub fn save_goal(&self, goal: &Goal) -> Result<()> {
        self.writable()?;
        if goal.title.trim().is_empty() {
            return Err(Error::Invalid("a goal needs a title".into()));
        }
        // A goal must name a role that exists. Checked here rather than by a
        // foreign key because the backend deliberately has none — see
        // `PurposeStore::delete_role` for why — and a goal under a role that
        // was never written would be invisible in every view the Overview
        // has, all of which are grouped by role.
        self.with_purpose(|p| {
            p.get_role(goal.role_id)?;
            p.put_goal(goal)
        })
    }

    pub fn save_goals(&self, goals: &[Goal]) -> Result<()> {
        self.writable()?;
        self.with_purpose(|p| p.put_goals(goals))
    }

    pub fn delete_goal(&self, id: GoalId) -> Result<()> {
        self.writable()?;
        self.with_purpose(|p| p.delete_goal(id))
    }

    /// Minutes per resolved purpose over a window, planned and actual.
    pub fn time_by_purpose(&self, window: PurposeWindow) -> Result<Vec<PurposeMinutes>> {
        self.with_purpose(|p| p.time_by_purpose(window))
    }

    /// Minutes of subscribed-calendar events per role over a window.
    pub fn events_by_role(&self, window: PurposeWindow) -> Result<Vec<RoleEventMinutes>> {
        self.with_purpose(|p| p.events_by_role(window))
    }

    pub fn goal_activity(&self, id: GoalId) -> Result<GoalActivity> {
        self.with_purpose(|p| p.goal_activity(id))
    }
}
