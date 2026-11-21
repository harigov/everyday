//! Areas and goals: what everything else in the vault answers "what was
//! this for" to.
//!
//! The purpose domain, on exactly the terms of every domain before it. What
//! is different is that almost nothing here is *about* areas and goals: the
//! two records are small and dull, and the interesting half is the pair
//! of reports, which read every other domain's tables and are the only
//! reason the pointer exists.

use super::Vault;
use super::session::Domain;
use crate::error::{Error, Result};
use crate::id::{AreaId, GoalId};
use crate::purpose::{
    Area, AreaEventMinutes, Goal, GoalActivity, PurposeDayMinutes, PurposeMinutes, suggested_areas,
};
use crate::record::RecordKind;
use crate::store::purpose::{GoalQuery, PurposeStore, PurposeWindow};

impl Vault {
    /// Does this vault's backend store areas and goals?
    pub fn supports_purpose(&self) -> bool {
        self.with_purpose(|_| Ok(())).is_ok()
    }

    fn with_purpose<T>(&self, f: impl FnOnce(&dyn PurposeStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::Purpose, |s| s.purpose().map(f))
    }

    pub fn areas(&self) -> Result<Vec<Area>> {
        self.with_purpose(|p| {
            let mut areas = p.list_areas()?;
            areas.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then_with(|| a.name.cmp(&b.name)));
            Ok(areas)
        })
    }

    pub fn area(&self, id: AreaId) -> Result<Area> {
        self.with_purpose(|p| p.get_area(id))
    }

    pub fn save_area(&self, area: &Area) -> Result<()> {
        self.writable()?;
        if area.name.trim().is_empty() {
            return Err(Error::Invalid("an area needs a name".into()));
        }
        self.with_purpose(|p| p.put_area(area))?;
        self.wrote(RecordKind::Area, area.id);
        Ok(())
    }

    /// Delete an area, which the backend refuses while goals point at it.
    pub fn delete_area(&self, id: AreaId) -> Result<()> {
        self.writable()?;
        self.with_purpose(|p| p.delete_area(id))?;
        self.wrote(RecordKind::Area, id);
        Ok(())
    }

    /// How many goals sit under an area, and how many are still open.
    pub fn count_goals(&self, area: AreaId) -> Result<(u64, u64)> {
        self.with_purpose(|p| p.count_goals(area))
    }

    /// Put a starting set of areas in a vault that has none, and do nothing
    /// at all otherwise. Returns how many were added.
    ///
    /// Unlike [`seed_library`](Vault::seed_library) this is **not** called on
    /// unlock. A list of shelves is a guess about what people read; a list
    /// of areas is a claim about what somebody's life is made of, and an
    /// application that wrote one unasked would be telling them who they
    /// are. The Overview offers it behind a button on an empty screen, and
    /// somebody who deletes the lot never sees it again.
    pub fn seed_areas(&self) -> Result<usize> {
        self.writable()?;
        let added = self.with_purpose(|p| {
            if !p.list_areas()?.is_empty() {
                return Ok(Vec::new());
            }
            let seeds = suggested_areas();
            for area in &seeds {
                p.put_area(area)?;
            }
            Ok(seeds)
        })?;
        for area in &added {
            self.wrote(RecordKind::Area, area.id);
        }
        Ok(added.len())
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
        // A goal must name an area that exists. Checked here rather than by a
        // foreign key because the backend deliberately has none — see
        // `PurposeStore::delete_area` for why — and a goal under an area that
        // was never written would be invisible in every view the Overview
        // has, all of which are grouped by area.
        self.with_purpose(|p| {
            p.get_area(goal.area_id)?;
            p.put_goal(goal)
        })?;
        self.wrote(RecordKind::Goal, goal.id);
        Ok(())
    }

    pub fn save_goals(&self, goals: &[Goal]) -> Result<()> {
        self.writable()?;
        self.with_purpose(|p| p.put_goals(goals))?;
        for goal in goals {
            self.wrote(RecordKind::Goal, goal.id);
        }
        Ok(())
    }

    pub fn delete_goal(&self, id: GoalId) -> Result<()> {
        self.writable()?;
        self.with_purpose(|p| p.delete_goal(id))?;
        self.wrote(RecordKind::Goal, id);
        Ok(())
    }

    /// Minutes per resolved purpose over a window, planned and actual.
    pub fn time_by_purpose(&self, window: PurposeWindow) -> Result<Vec<PurposeMinutes>> {
        self.with_purpose(|p| p.time_by_purpose(window))
    }

    /// Actual minutes per resolved purpose per day over a window.
    pub fn actual_minutes_by_day(&self, window: PurposeWindow) -> Result<Vec<PurposeDayMinutes>> {
        self.with_purpose(|p| p.actual_minutes_by_day(window))
    }

    /// Minutes of subscribed-calendar events per area over a window.
    pub fn events_by_area(&self, window: PurposeWindow) -> Result<Vec<AreaEventMinutes>> {
        self.with_purpose(|p| p.events_by_area(window))
    }

    pub fn goal_activity(&self, id: GoalId) -> Result<GoalActivity> {
        self.with_purpose(|p| p.goal_activity(id))
    }
}
