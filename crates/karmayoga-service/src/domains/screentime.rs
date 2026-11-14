//! Screen time: heartbeats in from collectors, and the reports, labels,
//! categories and settings the app is drawn from.
//!
//! Thin, as Money's commands are: the vault checks and merges, this decides
//! only what the edge can decide cheaply -- sizes -- and what a caller may
//! reach. `record_screen_time` and `screen_time_collector` are the two
//! commands under [`Scope::Heartbeats`](crate::ctx::Scope::Heartbeats);
//! everything else wants [`Scope::ScreenTime`](crate::ctx::Scope::ScreenTime).
//!
//! # Two scopes that do not imply each other
//!
//! A heartbeats token is what a browser profile and the GNOME Shell
//! extension keep on disk, so it is the token most likely to leak. It can
//! add usage and ask the two numbers a collector needs (how long counts as
//! idle, whether titles are kept), and nothing else: not one report, not one
//! label, not the list of machines. The other way round, a client allowed to
//! read the reports has no business posing as a collector, so `screentime`
//! does not reach `record_screen_time` either.
//!
//! # A collector is told `locked`, not something vaguer
//!
//! Both heartbeat commands want an unlocked vault and say so with the
//! `locked` code before the vault is asked anything. Heartbeats are sealed
//! like everything else, so a locked vault could not take them -- and a
//! collector that hears `locked` keeps its queue and tries again, where an
//! answer it does not recognise would make it guess whether to drop what it
//! holds. The ingest routes in `karmayoga-server` turn that code into `409`.
//!
//! # What the edge refuses
//!
//! A batch past [`MAX_BEATS`], before a beat of it is looked at; an item key
//! past [`MAX_KEY`] characters, a device list past [`MAX_DEVICES`] or a
//! device name past [`MAX_KEY`], and a time zone past [`MAX_TZ`] -- lengths
//! no screen of this application sends, so anything past them is somebody
//! else's client, and is told so before the vault parses it.

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};
use jiff::Timestamp;
use jiff::civil::Date;
use karmayoga_core::screentime::{
    CollectorConfig, HeartbeatBatch, IngestReport, MAX_BEATS, ScreenApp, ScreenAppRow,
    ScreenCategory, ScreenDevice, ScreenQuery, ScreenReport, ScreenTimeSettings, ScreenTimeline,
};
use karmayoga_core::{ScreenAppId, ScreenCategoryId};
use serde::Deserialize;
use std::sync::Arc;

use super::Nothing;

/// Longest item key (`app:…`, `site:…`) or device name taken. A key is at
/// most a registrable domain or an app id with its prefix -- a few dozen
/// characters, two hundred at the very worst -- so this is room, not a
/// limit anybody meets.
pub const MAX_KEY: usize = 300;

/// Most devices one report or timeline may be filtered to. A person has a
/// handful of machines.
pub const MAX_DEVICES: usize = 50;

/// Longest time zone name taken. The longest IANA name is about thirty
/// characters.
pub const MAX_TZ: usize = 64;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub query: ScreenQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Timeline {
    pub date: Date,
    /// An IANA zone; empty is the system's.
    pub tz: String,
    /// Only these devices. Empty is all.
    #[serde(default)]
    pub devices: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveApp {
    pub app: ScreenApp,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppRef {
    pub id: ScreenAppId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveCategory {
    pub category: ScreenCategory,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryRef {
    pub id: ScreenCategoryId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveSettings {
    pub settings: ScreenTimeSettings,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemRef {
    /// An item key, as a report names it.
    pub key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Range {
    pub from: Timestamp,
    pub to: Timestamp,
}

// ---- heartbeats ------------------------------------------------------------

/// Take one batch from one collector. The vault checks every beat and
/// answers how many it took, refused and joined; the edge refuses only a
/// batch too long to be worth reading.
async fn record_screen_time(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: HeartbeatBatch,
) -> CommandResult<IngestReport> {
    if args.beats.len() > MAX_BEATS {
        return Err(CommandError::new(
            codes::TOO_LARGE,
            format!("at most {MAX_BEATS} heartbeats in one batch; send the rest in another"),
        ));
    }
    let vault = svc.require_unlocked()?;
    blocking(move || Ok(vault.record_heartbeats(&args)?)).await
}

/// What a collector needs to know when it connects: how long without input
/// counts as idle, and whether to send titles at all.
async fn screen_time_collector(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<CollectorConfig> {
    let vault = svc.require_unlocked()?;
    blocking(move || Ok(vault.collector_config()?)).await
}

// ---- reports ---------------------------------------------------------------

async fn screen_time_report(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Report,
) -> CommandResult<ScreenReport> {
    let query = args.query;
    time_zone(&query.tz)?;
    devices(&query.devices)?;
    if let Some(item) = &query.item {
        key(item)?;
    }
    svc.on_vault(move |vault| vault.screen_report(&query)).await
}

async fn screen_time_timeline(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Timeline,
) -> CommandResult<ScreenTimeline> {
    let Timeline { date, tz, devices: wanted } = args;
    time_zone(&tz)?;
    devices(&wanted)?;
    svc.on_vault(move |vault| vault.screen_timeline(date, &tz, &wanted)).await
}

async fn list_screen_devices(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<ScreenDevice>> {
    svc.on_vault(|vault| vault.screen_devices()).await
}

// ---- labels ----------------------------------------------------------------

async fn list_screen_apps(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<ScreenAppRow>> {
    svc.on_vault(|vault| vault.screen_apps()).await
}

async fn save_screen_app(svc: Arc<Service>, _ctx: Ctx, args: SaveApp) -> CommandResult<()> {
    key(&args.app.key)?;
    svc.on_vault(move |vault| vault.save_screen_app(&args.app)).await
}

async fn delete_screen_app(svc: Arc<Service>, _ctx: Ctx, args: AppRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_screen_app(args.id)).await
}

// ---- categories ------------------------------------------------------------

/// Every category, in order -- the starter ones written first, the first
/// time anybody asks, as Money's are on its first unlock.
///
/// Written here rather than at unlock because Screen time has no task of
/// its own to start then, and the first screen to want categories is this
/// list. Only where this copy may write: a copy opened beside another that
/// holds the vault lists what is there, and the copy that can write will
/// have written them when it was asked the same.
async fn list_screen_categories(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<ScreenCategory>> {
    let vault = svc.require()?;
    blocking(move || {
        if vault.is_writable() {
            vault.ensure_screen_defaults()?;
        }
        Ok(vault.screen_categories()?)
    })
    .await
}

async fn save_screen_category(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SaveCategory,
) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_screen_category(&args.category)).await
}

/// Delete a category. Labels filed under it lose their category; what they
/// label keeps every second it was counted.
async fn delete_screen_category(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: CategoryRef,
) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_screen_category(args.id)).await
}

// ---- settings --------------------------------------------------------------

async fn screen_time_settings(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<ScreenTimeSettings> {
    svc.on_vault(|vault| vault.screen_settings()).await
}

async fn save_screen_time_settings(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SaveSettings,
) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_screen_settings(&args.settings)).await
}

// ---- forgetting ------------------------------------------------------------

/// Delete every span of one app or site. Answers how many went.
async fn forget_screen_item(svc: Arc<Service>, _ctx: Ctx, args: ItemRef) -> CommandResult<u64> {
    key(&args.key)?;
    svc.on_vault(move |vault| vault.forget_screen_item(&args.key)).await
}

/// Delete every span overlapping a range. Answers how many went.
async fn delete_screen_time(svc: Arc<Service>, _ctx: Ctx, args: Range) -> CommandResult<u64> {
    let Range { from, to } = args;
    if from > to {
        return Err(CommandError::new(codes::INVALID, "the start comes after the end"));
    }
    svc.on_vault(move |vault| vault.delete_screen_time(from, to)).await
}

// ---- checks at the edge ----------------------------------------------------

/// Refuse an item key longer than any report names.
fn key(text: &str) -> CommandResult<()> {
    if text.chars().count() > MAX_KEY {
        return Err(CommandError::new(codes::INVALID, "that is not an app or a site"));
    }
    Ok(())
}

/// Refuse a device list longer than anybody has machines, or a name longer
/// than any collector sends.
fn devices(list: &[String]) -> CommandResult<()> {
    if list.len() > MAX_DEVICES {
        return Err(CommandError::new(
            codes::INVALID,
            format!("at most {MAX_DEVICES} devices at once"),
        ));
    }
    if list.iter().any(|d| d.chars().count() > MAX_KEY) {
        return Err(CommandError::new(codes::INVALID, "that is not a device's name"));
    }
    Ok(())
}

/// Refuse a time zone name longer than any IANA one. Whether it names a
/// zone at all is the vault's to say.
fn time_zone(tz: &str) -> CommandResult<()> {
    if tz.chars().count() > MAX_TZ {
        return Err(CommandError::new(codes::INVALID, "that is not a time zone"));
    }
    Ok(())
}

pub static COMMANDS: &[crate::command::Command] = &[
    // ---- heartbeats ----
    //
    // The only two a heartbeats token reaches. Never with an id: spans are
    // written by the thousand, and what a window does about any of them is
    // reload what it shows.
    command! {
        name: "record_screen_time", scope: Heartbeats, effect: Write,
        change: ScreenSpan / Updated,
        args: HeartbeatBatch, returns: "IngestReport",
        signature: &[
            ("device", "string", true),
            ("collector", "string", true),
            ("beats", "Heartbeat[]", true),
        ],
        run: record_screen_time,
    },
    command! {
        name: "screen_time_collector", scope: Heartbeats, effect: Read,
        args: Nothing, returns: "CollectorConfig", signature: &[],
        run: screen_time_collector,
    },
    // ---- reports ----
    command! {
        name: "screen_time_report", scope: ScreenTime, effect: Read,
        args: Report, returns: "ScreenReport",
        signature: &[("query", "ScreenQuery", true)],
        run: screen_time_report,
    },
    command! {
        name: "screen_time_timeline", scope: ScreenTime, effect: Read,
        args: Timeline, returns: "ScreenTimeline",
        signature: &[
            ("date", "string", true),
            ("tz", "string", true),
            ("devices", "string[]", false),
        ],
        run: screen_time_timeline,
    },
    command! {
        name: "list_screen_devices", scope: ScreenTime, effect: Read,
        args: Nothing, returns: "ScreenDevice[]", signature: &[],
        run: list_screen_devices,
    },
    // ---- labels ----
    command! {
        name: "list_screen_apps", scope: ScreenTime, effect: Read,
        args: Nothing, returns: "ScreenAppRow[]", signature: &[],
        run: list_screen_apps,
    },
    command! {
        name: "save_screen_app", scope: ScreenTime, effect: Write,
        change: ScreenApp / Updated,
        id: |a: &SaveApp| Some(a.app.id.to_string()),
        args: SaveApp, returns: "void",
        signature: &[("app", "ScreenApp", true)],
        run: save_screen_app,
    },
    command! {
        name: "delete_screen_app", scope: ScreenTime, effect: Destructive,
        change: ScreenApp / Deleted,
        id: |a: &AppRef| Some(a.id.to_string()),
        args: AppRef, returns: "void",
        signature: &[("id", "ScreenAppId", true)],
        run: delete_screen_app,
    },
    // ---- categories ----
    command! {
        name: "list_screen_categories", scope: ScreenTime, effect: Read,
        args: Nothing, returns: "ScreenCategory[]", signature: &[],
        run: list_screen_categories,
    },
    command! {
        name: "save_screen_category", scope: ScreenTime, effect: Write,
        change: ScreenCategory / Updated,
        id: |a: &SaveCategory| Some(a.category.id.to_string()),
        args: SaveCategory, returns: "void",
        signature: &[("category", "ScreenCategory", true)],
        run: save_screen_category,
    },
    command! {
        name: "delete_screen_category", scope: ScreenTime, effect: Destructive,
        change: ScreenCategory / Deleted,
        id: |a: &CategoryRef| Some(a.id.to_string()),
        args: CategoryRef, returns: "void",
        signature: &[("id", "ScreenCategoryId", true)],
        run: delete_screen_category,
    },
    // ---- settings ----
    command! {
        name: "screen_time_settings", scope: ScreenTime, effect: Read,
        args: Nothing, returns: "ScreenTimeSettings", signature: &[],
        run: screen_time_settings,
    },
    command! {
        name: "save_screen_time_settings", scope: ScreenTime, effect: Write,
        change: Settings / Updated,
        args: SaveSettings, returns: "void",
        signature: &[("settings", "ScreenTimeSettings", true)],
        run: save_screen_time_settings,
    },
    // ---- forgetting ----
    command! {
        name: "forget_screen_item", scope: ScreenTime, effect: Destructive,
        change: ScreenSpan / Deleted,
        args: ItemRef, returns: "number",
        signature: &[("key", "string", true)],
        run: forget_screen_item,
    },
    command! {
        name: "delete_screen_time", scope: ScreenTime, effect: Destructive,
        change: ScreenSpan / Deleted,
        args: Range, returns: "number",
        signature: &[("from", "string", true), ("to", "string", true)],
        run: delete_screen_time,
    },
];
