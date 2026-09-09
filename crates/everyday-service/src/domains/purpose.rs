//! Roles, goals, and the reports drawn against them.
//!
//! The Overview's own records: who you are being, and what you are pursuing
//! under each. Everything here is the plain shape -- require the vault, do the
//! work on the blocking pool -- and the two reports at the foot are the only
//! interesting ones. Their interest is entirely in the SQL they delegate to,
//! which resolves the whole inheritance chain in one grouped scan over clear
//! columns and opens no ciphertext at all.
//!
//! # Why its own scope
//!
//! A purpose is a pointer on almost every record in the vault, so a client
//! that can read roles and goals can read the *shape* of somebody's life --
//! which roles exist, how much time each takes -- without reading a single
//! entry. That is worth being able to withhold separately, in the way the
//! shape of a vault is worth withholding from whoever runs its database.

use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::service::{Service, blocking};
use everyday_core::purpose::{Goal, GoalActivity, PurposeMinutes, Role, RoleEventMinutes};
use everyday_core::store::purpose::GoalQuery;
use everyday_core::{GoalId, PurposeWindow, RoleId};
use jiff::civil::Date;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::journals::Named;
use super::vault::Nothing;

/// A role with the two counts the sidebar draws under its name.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleInfo {
    #[serde(flatten)]
    pub role: Role,
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
    pub events: Vec<RoleEventMinutes>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveRole {
    pub role: Role,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleRef {
    pub id: RoleId,
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
    pub role_id: RoleId,
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

async fn list_roles(svc: Arc<Service>, _c: Ctx, _a: Nothing) -> CommandResult<Vec<RoleInfo>> {
    let vault = svc.require()?;
    blocking(move || {
        let mut out = Vec::new();
        for role in vault.roles()? {
            // Two `COUNT(*)`s over a clear index column, so a sidebar of six
            // roles decrypts six records and nothing else.
            let (goals, open) = vault.count_goals(role.id).unwrap_or((0, 0));
            out.push(RoleInfo { role, goals, open });
        }
        Ok(out)
    })
    .await
}

/// Mint a role. Unsaved: fill it in and pass it to `save_role`.
///
/// Minted here rather than in a client so the id, the colour and the two
/// timestamps come from one place, exactly as `new_kind` does.
async fn new_role(svc: Arc<Service>, _c: Ctx, args: Named) -> CommandResult<Role> {
    let _ = svc.require()?;
    Ok(Role::new(args.name.trim()))
}

async fn save_role(svc: Arc<Service>, _c: Ctx, args: SaveRole) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.save_role(&args.role)?)).await
}

/// Delete a role. Refused, with a message naming the count, while goals still
/// point at it.
async fn delete_role(svc: Arc<Service>, _c: Ctx, args: RoleRef) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.delete_role(args.id)?)).await
}

/// Offer a starting set of roles, and answer zero if there are any already.
async fn seed_roles(svc: Arc<Service>, _c: Ctx, _a: Nothing) -> CommandResult<usize> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.seed_roles()?)).await
}

async fn list_goals(svc: Arc<Service>, _c: Ctx, args: Goals) -> CommandResult<Vec<Goal>> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.goals(&args.query)?)).await
}

async fn get_goal(svc: Arc<Service>, _c: Ctx, args: GoalRef) -> CommandResult<Goal> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.goal(args.id)?)).await
}

/// Mint a goal under a role. Unsaved.
async fn new_goal(svc: Arc<Service>, _c: Ctx, args: NewGoal) -> CommandResult<Goal> {
    let _ = svc.require()?;
    Ok(Goal::new(args.role_id, args.title.trim()))
}

async fn save_goal(svc: Arc<Service>, _c: Ctx, args: SaveGoal) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.save_goal(&args.goal)?)).await
}

async fn save_goals(svc: Arc<Service>, _c: Ctx, args: SaveGoals) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.save_goals(&args.goals)?)).await
}

async fn delete_goal(svc: Arc<Service>, _c: Ctx, args: GoalRef) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.delete_goal(args.id)?)).await
}

async fn time_by_purpose(svc: Arc<Service>, _c: Ctx, args: Window) -> CommandResult<BalanceReport> {
    let vault = svc.require()?;
    blocking(move || {
        let window = PurposeWindow::new(args.from, args.to);
        Ok(BalanceReport {
            purposes: vault.time_by_purpose(window)?,
            events: vault.events_by_role(window).unwrap_or_default(),
        })
    })
    .await
}

async fn goal_activity(svc: Arc<Service>, _c: Ctx, args: GoalRef) -> CommandResult<GoalActivity> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.goal_activity(args.id)?)).await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "list_roles", scope: Purpose, effect: Read,
        args: Nothing, returns: "RoleInfo[]", signature: &[],
        run: list_roles,
    },
    command! {
        name: "new_role", scope: Purpose, effect: Read,
        args: Named, returns: "Role",
        signature: &[("name", "string", true)],
        run: new_role,
    },
    command! {
        name: "save_role", scope: Purpose, effect: Write,
        change: Role / Updated,
        args: SaveRole, returns: "void",
        signature: &[("role", "Role", true)],
        run: save_role,
    },
    command! {
        name: "delete_role", scope: Purpose, effect: Destructive,
        change: Role / Deleted,
        args: RoleRef, returns: "void",
        signature: &[("id", "RoleId", true)],
        run: delete_role,
    },
    command! {
        name: "seed_roles", scope: Purpose, effect: Write,
        change: Role / Created,
        args: Nothing, returns: "number", signature: &[],
        run: seed_roles,
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
        signature: &[("roleId", "RoleId", true), ("title", "string", true)],
        run: new_goal,
    },
    command! {
        name: "save_goal", scope: Purpose, effect: Write,
        change: Goal / Updated,
        args: SaveGoal, returns: "void",
        signature: &[("goal", "Goal", true)],
        run: save_goal,
    },
    command! {
        name: "save_goals", scope: Purpose, effect: Write,
        change: Goal / Updated,
        args: SaveGoals, returns: "void",
        signature: &[("goals", "Goal[]", true)],
        run: save_goals,
    },
    command! {
        name: "delete_goal", scope: Purpose, effect: Destructive,
        change: Goal / Deleted,
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
