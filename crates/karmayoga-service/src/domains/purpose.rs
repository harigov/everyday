//! Areas, goals, and the reports drawn against them.
//!
//! The Overview's own records: the parts of a life, and what you are pursuing
//! in each. Everything here is the plain shape -- require the vault, do the
//! work on the blocking pool -- and the two reports at the foot are the only
//! interesting ones. Their interest is entirely in the SQL they delegate to,
//! which resolves the whole inheritance chain in one grouped scan over clear
//! columns and opens no ciphertext at all.
//!
//! # Why its own scope
//!
//! A purpose is a pointer on almost every record in the vault, so a client
//! that can read areas and goals can read the *shape* of somebody's life --
//! which areas exist, how much time each takes -- without reading a single
//! entry. That is worth being able to withhold separately, in the way the
//! shape of a vault is worth withholding from whoever runs its database.

use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::service::{Service, blocking};
use jiff::civil::Date;
use karmayoga_core::purpose::{Area, AreaEventMinutes, Goal, GoalActivity, PurposeMinutes};
use karmayoga_core::store::purpose::GoalQuery;
use karmayoga_core::{AreaId, GoalId, PurposeWindow};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::Nothing;
use super::journals::Named;

/// An area with the two counts the sidebar draws under its name.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AreaInfo {
    #[serde(flatten)]
    pub area: Area,
    /// Goals under it, in any state.
    pub goals: u64,
    /// Of those, the ones still being pursued.
    pub open: u64,
}

/// What the balance report is made of: minutes per purpose, and the events
/// somebody else booked, over one window.
///
/// Both halves in one call because the Overview always draws them together and
/// two round trips would let one arrive without the other, which shows up as a
/// chart that changes shape twice.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceReport {
    pub purposes: Vec<PurposeMinutes>,
    pub events: Vec<AreaEventMinutes>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveArea {
    pub area: Area,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AreaRef {
    pub id: AreaId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Goals {
    pub query: GoalQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalRef {
    pub id: GoalId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewGoal {
    pub area_id: AreaId,
    pub title: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveGoal {
    pub goal: Goal,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveGoals {
    pub goals: Vec<Goal>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    pub from: Date,
    pub to: Date,
}

async fn list_areas(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<Vec<AreaInfo>> {
    let vault = svc.require()?;
    blocking(move || {
        let mut out = Vec::new();
        for area in vault.areas()? {
            // Two `COUNT(*)`s over a clear index column, so a sidebar of six
            // areas decrypts six records and nothing else.
            let (goals, open) = vault.count_goals(area.id).unwrap_or((0, 0));
            out.push(AreaInfo { area, goals, open });
        }
        Ok(out)
    })
    .await
}

/// Mint an area. Unsaved: fill it in and pass it to `save_area`.
///
/// Minted here rather than in a client so the id, the colour and the two
/// timestamps come from one place, exactly as `new_kind` does.
async fn new_area(svc: Arc<Service>, _ctx: Ctx, args: Named) -> CommandResult<Area> {
    let _ = svc.require()?;
    Ok(Area::new(args.name.trim()))
}

async fn save_area(svc: Arc<Service>, _ctx: Ctx, args: SaveArea) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_area(&args.area)).await
}

/// Delete an area. Refused, with a message naming the count, while goals still
/// point at it.
async fn delete_area(svc: Arc<Service>, _ctx: Ctx, args: AreaRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_area(args.id)).await
}

/// Offer a starting set of areas, and answer zero if there are any already.
async fn seed_areas(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<usize> {
    svc.on_vault(move |vault| vault.seed_areas()).await
}

async fn list_goals(svc: Arc<Service>, _ctx: Ctx, args: Goals) -> CommandResult<Vec<Goal>> {
    svc.on_vault(move |vault| vault.goals(&args.query)).await
}

async fn get_goal(svc: Arc<Service>, _ctx: Ctx, args: GoalRef) -> CommandResult<Goal> {
    svc.on_vault(move |vault| vault.goal(args.id)).await
}

/// Mint a goal under an area. Unsaved.
async fn new_goal(svc: Arc<Service>, _ctx: Ctx, args: NewGoal) -> CommandResult<Goal> {
    let _ = svc.require()?;
    Ok(Goal::new(args.area_id, args.title.trim()))
}

async fn save_goal(svc: Arc<Service>, _ctx: Ctx, args: SaveGoal) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_goal(&args.goal)).await
}

async fn save_goals(svc: Arc<Service>, _ctx: Ctx, args: SaveGoals) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_goals(&args.goals)).await
}

async fn delete_goal(svc: Arc<Service>, _ctx: Ctx, args: GoalRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_goal(args.id)).await
}

async fn time_by_purpose(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Window,
) -> CommandResult<BalanceReport> {
    let vault = svc.require()?;
    blocking(move || {
        let window = PurposeWindow::new(args.from, args.to);
        Ok(BalanceReport {
            purposes: vault.time_by_purpose(window)?,
            events: vault.events_by_area(window).unwrap_or_default(),
        })
    })
    .await
}

async fn goal_activity(svc: Arc<Service>, _ctx: Ctx, args: GoalRef) -> CommandResult<GoalActivity> {
    svc.on_vault(move |vault| vault.goal_activity(args.id)).await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "list_areas", scope: Purpose, effect: Read,
        args: Nothing, returns: "AreaInfo[]", signature: &[],
        run: list_areas,
    },
    command! {
        name: "new_area", scope: Purpose, effect: Read,
        args: Named, returns: "Area",
        signature: &[("name", "string", true)],
        run: new_area,
    },
    command! {
        name: "save_area", scope: Purpose, effect: Write,
        change: Area / Updated,
        id: |a: &SaveArea| Some(a.area.id.to_string()),
        args: SaveArea, returns: "void",
        signature: &[("area", "Area", true)],
        run: save_area,
    },
    command! {
        name: "delete_area", scope: Purpose, effect: Destructive,
        change: Area / Deleted,
        id: |a: &AreaRef| Some(a.id.to_string()),
        args: AreaRef, returns: "void",
        signature: &[("id", "AreaId", true)],
        run: delete_area,
    },
    command! {
        name: "seed_areas", scope: Purpose, effect: Write,
        change: Area / Created,
        args: Nothing, returns: "number", signature: &[],
        run: seed_areas,
    },
    command! {
        name: "list_goals", scope: Purpose, effect: Read,
        args: Goals, returns: "Goal[]",
        signature: &[("query", "GoalQuery", true)],
        run: list_goals,
    },
    command! {
        name: "get_goal", scope: Purpose, effect: Read,
        args: GoalRef, returns: "Goal",
        signature: &[("id", "GoalId", true)],
        run: get_goal,
    },
    command! {
        name: "new_goal", scope: Purpose, effect: Read,
        args: NewGoal, returns: "Goal",
        signature: &[("areaId", "AreaId", true), ("title", "string", true)],
        run: new_goal,
    },
    command! {
        name: "save_goal", scope: Purpose, effect: Write,
        change: Goal / Updated,
        id: |a: &SaveGoal| Some(a.goal.id.to_string()),
        args: SaveGoal, returns: "void",
        signature: &[("goal", "Goal", true)],
        run: save_goal,
    },
    command! {
        name: "save_goals", scope: Purpose, effect: Write,
        change: Goal / Updated,
        ids: |a: &SaveGoals| a.goals.iter().map(|g| g.id.to_string()).collect(),
        args: SaveGoals, returns: "void",
        signature: &[("goals", "Goal[]", true)],
        run: save_goals,
    },
    command! {
        name: "delete_goal", scope: Purpose, effect: Destructive,
        change: Goal / Deleted,
        id: |a: &GoalRef| Some(a.id.to_string()),
        args: GoalRef, returns: "void",
        signature: &[("id", "GoalId", true)],
        run: delete_goal,
    },
    command! {
        name: "time_by_purpose", scope: Purpose, effect: Read,
        args: Window, returns: "BalanceReport",
        signature: &[("from", "string", true), ("to", "string", true)],
        run: time_by_purpose,
    },
    command! {
        name: "goal_activity", scope: Purpose, effect: Read,
        args: GoalRef, returns: "GoalActivity",
        signature: &[("id", "GoalId", true)],
        run: goal_activity,
    },
];
