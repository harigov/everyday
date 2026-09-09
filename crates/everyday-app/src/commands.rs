//! The command surface exposed to the interface.
//!
//! Every command that touches storage runs its work on the blocking pool
//! rather than on an async worker. Vault operations touch disk and, when
//! unlocking, deliberately burn ~64 MiB of memory in Argon2; doing that on
//! the runtime's async threads would stall every other task, and doing it on
//! the webview's thread would freeze the window mid-keystroke.
//!
//! # Which means: `async fn` and [`blocking`], not a plain `fn`
//!
//! This is not a stylistic choice, and it is easy to get wrong by omission.
//! A `#[tauri::command]` on a *synchronous* function is dispatched inline on
//! the main thread -- the same thread that runs the platform's UI loop. The
//! command then does not merely fail to reach the blocking pool; it holds
//! the window while it decrypts. Listing a day's entries, filtering two
//! thousand tasks against sealed titles, sealing a save and fsyncing it, or
//! counting tags across the vault are all work of that shape, and all of it
//! was landing on the UI thread.
//!
//! So: anything that reads or writes the vault is `pub async fn`, takes its
//! `Arc<Vault>` out of the state first, and does the rest inside `blocking`.
//! What stays a plain `fn` is only what never touches storage -- reading a
//! status word, stamping activity against an atomic, or minting a record in
//! memory for the interface to fill in -- where a hop to another thread and
//! back would cost more than the work.
//!
//! The one deliberate exception is [`poll_auto_lock`], which the interface
//! asks every few seconds: it is an integer comparison that answers `false`
//! almost every time, and on the rare occasion it answers `true` the vault
//! is dropped, which is the cheap direction.

use everyday_core::agent::{AgentSettings, Conversation, Memory, Message as AgentMessage};
use everyday_core::calendar::{Calendar, CalendarProvider, Event, SyncReport};
use everyday_core::library::{Item, ItemStatus, Kind, LibraryStats, LogEntry, LogEvent, Progress};
use everyday_core::model::{local_date_in, system_tz, today_local};
use everyday_core::purpose::{Goal, GoalActivity, PurposeMinutes, Role, RoleEventMinutes};
use everyday_core::search::SearchHit;
use everyday_core::store::agent::ConversationQuery;
use everyday_core::store::calendars::EventQuery;
use everyday_core::store::library::{ItemQuery, LogQuery};
use everyday_core::store::purpose::{GoalQuery, PurposeWindow};
use everyday_core::store::tasks::{BlockQuery, TaskQuery};
use everyday_core::store::trackers::{ReadingQuery, TrackerDay};
use everyday_core::store::{EntryQuery, StoreStats};
use everyday_core::task::{
    BlockKind, BlockSubject, Project, Task, TaskStats, TaskStatus, TimeBlock,
};
use everyday_core::tracker::{Reading, Tracker, TrackerKind};
use everyday_core::websearch::{SearchRequest, SearchResult, Source};
use everyday_core::{
    BlobId, BlockId, CalendarId, ConversationId, Entry, EntryId, EventId, GoalId, ItemId, Journal,
    JournalId, KindId, LogId, MemoryId, ProjectId, ReadingId, RoleId, TaskId, TrackerId, Vault,
    VaultConfig, VaultStatus,
};
use jiff::civil::Date;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{Manager, State};

use crate::agent::{self, AgentEvent, Pending};
use crate::error::{CommandError, CommandResult};
use crate::feeds;
use crate::notify::{self, Notification};
use crate::state::AppState;
use crate::tray::{Tray, TrayItem};
use crate::websearch;

/// Run blocking vault work off the async runtime.
async fn blocking<T, F>(f: F) -> CommandResult<T>
where
    F: FnOnce() -> CommandResult<T> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| CommandError::new("panic", format!("background task failed: {e}")))?
}

/// What the interface needs before any vault is open.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    pub vault_exists: bool,
    /// The vault location in play: the one last opened if it is still there,
    /// otherwise where a new vault would be created.
    pub default_path: PathBuf,
    /// Every backend this build can open, with the fields each one needs
    /// configuring. The setup screen renders these rather than knowing that
    /// Postgres exists.
    pub backends: Vec<everyday_core::BackendInfo>,
    pub status: Option<VaultStatus>,
}

#[tauri::command]
pub async fn bootstrap(state: State<'_, AppState>) -> CommandResult<Bootstrap> {
    // Prefer the vault this user last had open. Only fall back to the default
    // location when nothing was recorded or what was recorded is gone -- an
    // external disk that is not plugged in should show the setup screen, not
    // an error about a path the user cannot see.
    let path = state
        .last_path()
        .filter(|p| everyday_vault::exists(p))
        .unwrap_or_else(everyday_vault::default_vault_dir);
    let backends = everyday_vault::available_backends();

    // Open eagerly so an unencrypted vault is usable immediately and an
    // encrypted one can name itself on the lock screen.
    if state.get().is_none() && everyday_vault::exists(&path) {
        let opened = {
            let path = path.clone();
            blocking(move || everyday_vault::open(&path).map_err(CommandError::from)).await
        };
        match opened {
            Ok(vault) => {
                state.set(vault);
            }
            // A vault we cannot open is not fatal: the interface should still
            // start and be able to say why.
            Err(e) => tracing::warn!(error = %e, "could not open the vault at startup"),
        }
    }

    Ok(Bootstrap {
        vault_exists: everyday_vault::exists(&path),
        default_path: path,
        backends,
        status: state.get().map(|v| v.status()),
    })
}

#[tauri::command]
pub async fn create_vault(
    state: State<'_, AppState>,
    path: PathBuf,
    name: String,
    backend: String,
    // `settings` is whatever the chosen backend asked for in its spec -- a
    // connection URL, a schema name -- and is absent for a local vault. It is
    // sealed under the vault key once the vault exists; see `VaultHeader`.
    settings: Option<everyday_core::BackendSettings>,
    password: Option<String>,
) -> CommandResult<VaultStatus> {
    if let Some(p) = password.as_deref() {
        everyday_vault::validate_password(p)?;
    }
    let settings = settings.unwrap_or_default();
    // Checked here as well as inside `create`, so a missing connection URL
    // is a message on the setup screen and not a half-made vault directory.
    everyday_vault::validate_settings(&backend, &settings)?;
    let config = VaultConfig {
        name,
        backend,
        settings,
        password,
        kdf: Default::default(),
        auto_lock_seconds: 15 * 60,
    };
    state.close();
    let created = {
        let path = path.clone();
        blocking(move || {
            let vault = everyday_vault::create(&path, config)?;
            // A vault with no journal is a dead end; give it one.
            vault.save_journal(&Journal::new("Journal"))?;
            Ok(vault)
        })
        .await?
    };
    let vault = state.set(created);
    Ok(vault.status())
}

#[tauri::command]
pub async fn open_vault(state: State<'_, AppState>, path: PathBuf) -> CommandResult<VaultStatus> {
    // Release the vault we already hold first. Its write lock is this
    // process's, and opening a second vault -- including the same one again
    // -- while still holding it would come up read-only. See `AppState::close`.
    state.close();
    let opened = {
        let path = path.clone();
        blocking(move || everyday_vault::open(&path).map_err(CommandError::from)).await?
    };
    let vault = state.set(opened);
    Ok(vault.status())
}

#[tauri::command]
pub async fn unlock(state: State<'_, AppState>, password: String) -> CommandResult<VaultStatus> {
    let vault = state.require()?;
    let v = vault.clone();
    blocking(move || v.unlock(Some(&password)).map_err(CommandError::from)).await?;
    Ok(vault.status())
}

#[tauri::command]
pub async fn lock(state: State<'_, AppState>) -> CommandResult<VaultStatus> {
    let vault = state.require()?;
    blocking(move || {
        vault.lock();
        Ok(vault.status())
    })
    .await
}

#[tauri::command]
pub fn status(state: State<'_, AppState>) -> CommandResult<VaultStatus> {
    Ok(state.require()?.status())
}

#[tauri::command]
pub async fn change_password(
    state: State<'_, AppState>,
    current: String,
    next: String,
) -> CommandResult<()> {
    everyday_vault::validate_password(&next)?;
    let vault = state.require()?;
    blocking(move || vault.change_password(Some(&current), Some(&next)).map_err(CommandError::from))
        .await
}

#[tauri::command]
pub async fn set_auto_lock(state: State<'_, AppState>, seconds: u64) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.set_auto_lock(seconds)?)).await
}

/// Defer the idle auto-lock. Called on real interaction, so it must be cheap.
#[tauri::command]
pub fn touch(state: State<'_, AppState>) -> CommandResult<()> {
    if let Some(v) = state.get() {
        v.touch();
    }
    Ok(())
}

/// Returns true if the vault just locked itself. Polled by the interface.
#[tauri::command]
pub fn poll_auto_lock(state: State<'_, AppState>) -> CommandResult<bool> {
    Ok(state.get().is_some_and(|v| v.auto_lock_if_idle()))
}

// ---- journals -----------------------------------------------------------

#[tauri::command]
pub async fn list_journals(state: State<'_, AppState>) -> CommandResult<Vec<Journal>> {
    let vault = state.require()?;
    blocking(move || Ok(vault.journals()?)).await
}

/// Mint a journal, without saving it.
///
/// The id is the core's to allocate, not the interface's. Ids here are
/// UUIDv7, which the storage layer relies on to sort chronologically; the
/// interface had been minting v4 with `crypto.randomUUID`, which is both a
/// different ordering and unavailable outside a secure context.
#[tauri::command]
pub fn new_journal(state: State<'_, AppState>, name: String) -> CommandResult<Journal> {
    let _ = state.require()?;
    Ok(Journal::new(name))
}

#[tauri::command]
pub async fn save_journal(state: State<'_, AppState>, journal: Journal) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.save_journal(&journal)?)).await
}

#[tauri::command]
pub async fn delete_journal(state: State<'_, AppState>, id: JournalId) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.delete_journal(id)?)).await
}

// ---- entries ------------------------------------------------------------

#[tauri::command]
pub async fn list_entries(
    state: State<'_, AppState>,
    query: EntryQuery,
) -> CommandResult<Vec<everyday_core::EntrySummary>> {
    let vault = state.require()?;
    blocking(move || Ok(vault.entries(&query)?)).await
}

#[tauri::command]
pub async fn get_entry(state: State<'_, AppState>, id: EntryId) -> CommandResult<Entry> {
    let vault = state.require()?;
    blocking(move || Ok(vault.entry(id)?)).await
}

/// A blank entry, filed under today in the machine's own time zone.
#[tauri::command]
pub fn new_entry(state: State<'_, AppState>, journal_id: JournalId) -> CommandResult<Entry> {
    let _ = state.require()?;
    let tz = system_tz();
    let mut entry = Entry::new(journal_id, &tz);
    entry.local_date = local_date_in(entry.created_at, &tz);
    Ok(entry)
}

/// Save an entry, refusing to overwrite a change made since it was loaded.
///
/// `expect` is the `updatedAt` the interface last read for this entry, or
/// `null` for one it has just created. A mismatch comes back as `conflict`
/// and nothing is written, which is what the editor's conflict banner is
/// driven by. `save_entry_force` is the "keep mine" on that banner.
#[tauri::command]
pub async fn save_entry(
    state: State<'_, AppState>,
    entry: Entry,
    expect: Option<jiff::Timestamp>,
) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.save_entry(&entry, expect)?)).await
}

#[tauri::command]
pub async fn save_entry_force(state: State<'_, AppState>, entry: Entry) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.overwrite_entry(&entry)?)).await
}

#[tauri::command]
pub async fn delete_entry(state: State<'_, AppState>, id: EntryId) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.delete_entry(id)?)).await
}

// ---- search -------------------------------------------------------------

#[tauri::command]
pub async fn search(
    state: State<'_, AppState>,
    query: String,
    journal_id: Option<JournalId>,
    limit: usize,
) -> CommandResult<Vec<SearchHit>> {
    let vault = state.require()?;
    blocking(move || Ok(vault.search(&query, journal_id, limit.min(200))?)).await
}

#[tauri::command]
pub async fn list_tags(state: State<'_, AppState>) -> CommandResult<Vec<String>> {
    let vault = state.require()?;
    blocking(move || Ok(vault.entry_tags()?.into_iter().map(|(tag, _)| tag).collect())).await
}

// ---- projects, tasks and time -------------------------------------------
//
// The todo app. Every command here fails with `unsupported` on a vault whose
// backend stores journals only; the interface reads `capabilities.tasks` from
// the vault status and hides the app rather than letting that happen.
//
// Ids and timestamps are minted by the core, never by the interface, for the
// same reason journals are: they are UUIDv7, which storage relies on to sort
// chronologically, and `crypto.randomUUID` is both a different ordering and
// unavailable outside a secure context.

#[tauri::command]
pub async fn list_projects(state: State<'_, AppState>) -> CommandResult<Vec<Project>> {
    let vault = state.require()?;
    blocking(move || Ok(vault.projects()?)).await
}

/// Mint a project, without saving it.
#[tauri::command]
pub fn new_project(state: State<'_, AppState>, name: String) -> CommandResult<Project> {
    let _ = state.require()?;
    Ok(Project::new(name))
}

#[tauri::command]
pub async fn save_project(state: State<'_, AppState>, project: Project) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.save_project(&project)?)).await
}

/// Delete a project, its tasks and every block of time booked against them.
#[tauri::command]
pub async fn delete_project(state: State<'_, AppState>, id: ProjectId) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.delete_project(id)?)).await
}

#[tauri::command]
pub async fn list_tasks(state: State<'_, AppState>, query: TaskQuery) -> CommandResult<Vec<Task>> {
    let vault = state.require()?;
    blocking(move || Ok(vault.tasks(&query)?)).await
}

#[tauri::command]
pub async fn get_task(state: State<'_, AppState>, id: TaskId) -> CommandResult<Task> {
    let vault = state.require()?;
    blocking(move || Ok(vault.task(id)?)).await
}

/// Mint a task, without saving it.
///
/// The interface fills in the title and whatever the quick-add line parsed
/// out of it, then calls `save_task`. Two round trips rather than one, in
/// exchange for one shape of task travelling in each direction.
#[tauri::command]
pub fn new_task(
    state: State<'_, AppState>,
    project_id: Option<ProjectId>,
    parent_id: Option<TaskId>,
    status: Option<TaskStatus>,
) -> CommandResult<Task> {
    let _ = state.require()?;
    let mut task = Task::new(String::new()).in_project(project_id).under(parent_id);
    if let Some(status) = status {
        task.status = status;
    }
    Ok(task)
}

#[tauri::command]
pub async fn save_task(state: State<'_, AppState>, task: Task) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.save_task(&task)?)).await
}

/// Write several tasks at once. This is what dragging a card across a board
/// is: two columns renumbered, which must land as one change or not at all.
#[tauri::command]
pub async fn save_tasks(state: State<'_, AppState>, tasks: Vec<Task>) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.save_tasks(&tasks)?)).await
}

/// Delete a task, its subtasks and their time blocks.
#[tauri::command]
pub async fn delete_task(state: State<'_, AppState>, id: TaskId) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.delete_task(id)?)).await
}

#[tauri::command]
pub async fn list_blocks(
    state: State<'_, AppState>,
    query: BlockQuery,
) -> CommandResult<Vec<TimeBlock>> {
    let vault = state.require()?;
    blocking(move || Ok(vault.blocks(&query)?)).await
}

/// Mint a block of time, without saving it.
///
/// The time zone is the machine's, resolved here rather than in the webview,
/// so that `local_date` -- the column a calendar's week query scans -- is
/// decided by the same code that decides an entry's.
#[tauri::command]
pub fn new_block(
    state: State<'_, AppState>,
    subject: BlockSubject,
    start: jiff::Timestamp,
    minutes: u32,
    kind: Option<BlockKind>,
) -> CommandResult<TimeBlock> {
    let _ = state.require()?;
    let tz = system_tz();
    let mut block = TimeBlock::new(subject, start, minutes, &tz);
    if let Some(kind) = kind {
        block.kind = kind;
    }
    Ok(block)
}

#[tauri::command]
pub async fn save_block(state: State<'_, AppState>, block: TimeBlock) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.save_block(&block)?)).await
}

#[tauri::command]
pub async fn delete_block(state: State<'_, AppState>, id: BlockId) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.delete_block(id)?)).await
}

/// Every tag used anywhere in the task domain, most used first.
#[tauri::command]
pub async fn task_tags(state: State<'_, AppState>) -> CommandResult<Vec<TagCount>> {
    let vault = state.require()?;
    blocking(move || {
        Ok(vault.task_tags()?.into_iter().map(|(tag, count)| TagCount { tag, count }).collect())
    })
    .await
}

/// A tag and how often it is used. A named struct rather than a tuple so the
/// interface reads `t.count` instead of `t[1]`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagCount {
    pub tag: String,
    pub count: u32,
}

/// Counts for the sidebar, as of the machine's own calendar day.
///
/// The day is resolved here rather than in the core, and here rather than in
/// the webview, so that "overdue" is decided by the same code that decides
/// which day an entry is filed under.
#[tauri::command]
pub async fn task_stats(state: State<'_, AppState>) -> CommandResult<TaskStats> {
    let vault = state.require()?;
    blocking(move || Ok(vault.task_stats(today_local())?)).await
}

// ---- calendars ----------------------------------------------------------
//
// The calendar app. Note the division of labour, which is the same one the
// rest of the application makes and is worth spelling out because this is
// the only feature that touches a network:
//
//   this file      what a URL is, when to fetch it, what to do on a 403
//   feeds.rs       turning a URL into bytes -- the only socket in the app
//   everyday-core  everything that happens to those bytes afterwards
//
// The last of those is the part with the difficult logic in it -- RFC 5545,
// recurrence, time zones -- and it is testable offline precisely because it
// never learns that a network exists.

#[tauri::command]
pub async fn list_calendars(state: State<'_, AppState>) -> CommandResult<Vec<CalendarInfo>> {
    let vault = state.require()?;
    blocking(move || {
        let mut out = Vec::new();
        for calendar in vault.calendars()? {
            // The count is a `COUNT(*)` over a clear index column, so listing
            // four calendars decrypts four records and nothing else.
            let events = vault.event_count(calendar.id).unwrap_or(0);
            out.push(CalendarInfo { calendar, events });
        }
        Ok(out)
    })
    .await
}

/// A calendar and how much is in it. A named struct rather than widening
/// `Calendar` itself: the count is a fact about storage at this instant, not
/// a property of the subscription, and putting it on the record would mean
/// every sync had to remember to update it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarInfo {
    #[serde(flatten)]
    pub calendar: Calendar,
    pub events: u64,
}

#[tauri::command]
pub async fn save_calendar(state: State<'_, AppState>, calendar: Calendar) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.save_calendar(&calendar)?)).await
}

/// Unsubscribe: the calendar and every event that came from it.
#[tauri::command]
pub async fn delete_calendar(state: State<'_, AppState>, id: CalendarId) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.delete_calendar(id)?)).await
}

#[tauri::command]
pub async fn list_events(
    state: State<'_, AppState>,
    query: EventQuery,
) -> CommandResult<Vec<Event>> {
    let vault = state.require()?;
    blocking(move || Ok(vault.events(&query)?)).await
}

#[tauri::command]
pub async fn get_event(state: State<'_, AppState>, id: EventId) -> CommandResult<Event> {
    let vault = state.require()?;
    blocking(move || Ok(vault.event(id)?)).await
}

/// Fetch one calendar's feed and replace its events with what came back.
///
/// The failure path is as deliberate as the success one. A feed that cannot
/// be fetched, or that answers with something that is not a calendar, leaves
/// the events already stored exactly where they are and records *why* on the
/// subscription -- because the alternative, a calendar that empties itself
/// when the wifi drops, is the one failure of an automatic sync that people
/// notice and never forgive.
#[tauri::command]
pub async fn sync_calendar(
    state: State<'_, AppState>,
    id: CalendarId,
) -> CommandResult<SyncReport> {
    let vault = state.require()?;
    let report = sync_one(&vault, id).await?;
    // A hand-driven refresh that works ends the outage as much as a
    // background one does, so the next failure is news again. Without this, a
    // feed fixed from the sidebar would never notify a second time.
    state.feed_recovered(id);
    Ok(report)
}

/// Fetch every subscription whose refresh interval has elapsed.
///
/// Polled by the interface rather than driven by a timer in here, so that a
/// locked vault is never fetched into and a window nobody is looking at is
/// never the reason a laptop wakes its radio. Failures are collected, not
/// raised: one calendar being down must not stop the other three.
#[tauri::command]
pub async fn sync_due_calendars(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    force: bool,
) -> CommandResult<Vec<SyncReport>> {
    let vault = state.require()?;
    // A read-only vault cannot store what a sync fetches, and `sync_one`
    // would also try to record the failure on the subscription -- another
    // write. Fetching feeds over the network to throw the bytes away is not
    // a useful thing to do on a timer, so the whole pass is skipped.
    if !vault.is_writable() {
        return Ok(Vec::new());
    }
    let now = jiff::Timestamp::now();
    // The name travels with the id because the notification below needs it,
    // and re-reading the subscription after a failed sync to find out what to
    // call it is a second decrypt for a string we already had.
    let due: Vec<(CalendarId, String)> = vault
        .calendars()?
        .into_iter()
        .filter(|c| c.origin.url().is_some() && (force || c.is_due(now)))
        .map(|c| (c.id, c.name))
        .collect();

    let mut out = Vec::new();
    for (id, name) in due {
        match sync_one(&vault, id).await {
            Ok(report) => {
                state.feed_recovered(id);
                out.push(report);
            }
            // The failure itself is already recorded on the subscription by
            // `sync_one`, and the calendar sidebar shows it there, beside the
            // calendar it belongs to. That is the right place for it and it
            // stays -- but it is only the right place if you are looking at
            // the calendar. This pass runs on a timer whoever is using the
            // app, so the case worth notifying is somebody who has spent the
            // week in the journal while a subscription they rely on has been
            // quietly returning nothing.
            //
            // Once per outage, never on a refresh the user asked for by hand
            // -- they are looking at the calendar, and the sidebar has just
            // told them.
            Err(e) => {
                tracing::info!(%id, error = %e, "a calendar could not be refreshed");
                if !force && state.feed_failed(id) {
                    notify::notify(
                        &app,
                        Notification::warning(format!("{name} is not refreshing"))
                            // Deliberately not `e.message`: a feed URL is a
                            // bearer credential and error text from a fetch
                            // can quote it. The interface shows the recorded
                            // detail beside the calendar, where the person
                            // reading it already has the address.
                            .body("Its events may be out of date. Open the calendar for details.")
                            .for_user()
                            .key(format!("feed:{id}")),
                    );
                }
            }
        }
    }
    Ok(out)
}

async fn sync_one(vault: &Arc<Vault>, id: CalendarId) -> CommandResult<SyncReport> {
    let url = vault.calendar(id)?.fetch_url()?;
    let text = match feeds::fetch(&url).await {
        Ok(text) => text,
        Err(e) => {
            record_failure(vault, id, &e.message);
            return Err(e);
        }
    };
    apply_feed(vault, id, text).await
}

/// Hand fetched text to the core, on the blocking pool.
///
/// Parsing a year of a busy calendar, expanding its recurrences and sealing
/// a few thousand events is real CPU work, and doing it on an async worker
/// would stall every other command for the duration of a sync that is meant
/// to be invisible.
async fn apply_feed(vault: &Arc<Vault>, id: CalendarId, text: String) -> CommandResult<SyncReport> {
    let window = feeds::sync_window(today_local());
    let tz = system_tz();
    let v = vault.clone();
    let outcome = blocking(move || Ok(v.sync_calendar_from_ics(id, &text, window, &tz))).await?;
    match outcome {
        Ok(report) => Ok(report),
        Err(e) => {
            let message = e.to_string();
            record_failure(vault, id, &message);
            Err(CommandError::from(e))
        }
    }
}

/// Note on the subscription why the last attempt did not work.
fn record_failure(vault: &Arc<Vault>, id: CalendarId, why: &str) {
    if let Err(e) = vault.mark_calendar_failed(id, why) {
        tracing::warn!(%id, error = %e, "could not record the calendar sync failure");
    }
}

/// Subscribe to a feed and fetch it once, so the calendar appears with its
/// events already in it rather than empty and pending.
///
/// One command rather than three round trips, because the three are not
/// independent: a subscription that saved and then failed to fetch would
/// leave a calendar in the sidebar that the user has to work out how to
/// remove, and the honest answer to "this address is not a calendar" is to
/// have added nothing at all.
#[tauri::command]
pub async fn subscribe_calendar(
    state: State<'_, AppState>,
    name: String,
    url: String,
    color: String,
) -> CommandResult<CalendarInfo> {
    let vault = state.require()?;
    let url = everyday_core::calendar::normalize_feed_url(&url)?;
    let calendar = Calendar::subscribed(name.trim(), &url).with_color(color);
    let text = feeds::fetch(&url).await?;
    add_calendar(&vault, calendar, text, "Calendar").await
}

/// Add a calendar from a `.ics` file the user chose.
///
/// The text arrives from the interface, which read it with the browser's own
/// file API. No path crosses the bridge and nothing on disk is opened by
/// this process, so "import a calendar" cannot be talked into reading a file
/// the user did not pick.
#[tauri::command]
pub async fn import_calendar(
    state: State<'_, AppState>,
    name: String,
    label: String,
    color: String,
    ics: String,
) -> CommandResult<CalendarInfo> {
    let vault = state.require()?;
    let calendar = Calendar::imported(name.trim(), label).with_color(color);
    add_calendar(&vault, calendar, ics, "Imported calendar").await
}

/// Save a new calendar, fill it from `text`, and add nothing at all if that
/// does not work.
///
/// Both ways in -- a subscription and an imported file -- do exactly this
/// once they have the iCalendar text in hand, and they differ only in where
/// the text came from and what to call the result when the document does not
/// name itself. Keeping the shared half here is what makes the all-or-nothing
/// guarantee in `subscribe_calendar`'s doc comment one piece of code rather
/// than two that have to be kept in agreement.
async fn add_calendar(
    vault: &Arc<Vault>,
    mut calendar: Calendar,
    text: String,
    fallback_name: &str,
) -> CommandResult<CalendarInfo> {
    // Name it after the publisher when the person adding it did not: Google,
    // Outlook and Apple all set `X-WR-CALNAME`, and "Priya — Work" is a
    // better name than anything a text field would have got out of someone
    // in a hurry.
    if calendar.name.is_empty() {
        calendar.name = everyday_core::ics::parse(&text)
            .name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| fallback_name.to_string());
    }
    let id = calendar.id;
    {
        let vault = vault.clone();
        let calendar = calendar.clone();
        blocking(move || Ok(vault.save_calendar(&calendar)?)).await?;
    }

    match apply_feed(vault, id, text).await {
        Ok(report) => Ok(CalendarInfo { calendar: vault.calendar(id)?, events: report.events }),
        Err(e) => {
            // Nothing added: see `subscribe_calendar`. Undoing the save is
            // safe because nothing else can have pointed at it yet.
            let _ = vault.delete_calendar(id);
            Err(e)
        }
    }
}

/// The providers the "add a calendar" sheet offers, with where to find the
/// address for each.
///
/// Held in Rust rather than hard-coded in the interface because the guess
/// that picks a provider from a pasted URL lives here too, and the two have
/// to agree about what the set is.
#[tauri::command]
pub fn calendar_providers(state: State<'_, AppState>) -> CommandResult<Vec<ProviderInfo>> {
    let _ = state.require()?;
    Ok(CalendarProvider::ALL.iter().map(|p| ProviderInfo::of(*p)).collect())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub id: String,
    pub label: String,
    /// Where, in that product, the secret address is found.
    pub hint: String,
}

impl ProviderInfo {
    fn of(p: CalendarProvider) -> Self {
        let (label, hint) = match p {
            CalendarProvider::Google => (
                "Google Calendar",
                "Settings \u{2192} your calendar \u{2192} Integrate calendar \u{2192} \
                 Secret address in iCal format.",
            ),
            CalendarProvider::Outlook => (
                "Outlook",
                "Settings \u{2192} Calendar \u{2192} Shared calendars \u{2192} \
                 Publish a calendar, then copy the ICS link.",
            ),
            CalendarProvider::Apple => (
                "Apple Calendar",
                "iCloud.com \u{2192} Calendar \u{2192} the share icon beside a calendar \
                 \u{2192} Public Calendar, then copy the link.",
            ),
            CalendarProvider::Other => (
                "Another calendar",
                "Any address publishing an iCalendar (.ics) feed \u{2014} a team calendar, \
                 a fixture list, your country\u{2019}s public holidays.",
            ),
        };
        Self { id: p.as_str().to_string(), label: label.into(), hint: hint.into() }
    }
}

// ---- the library --------------------------------------------------------
//
// Shelves, the things on them, and the log of what you did with them. The
// same division of labour the calendar makes, and for the same reason -- this
// is the second feature that touches a network:
//
//   this file        what to look up, when, and what to do with the answer
//   websearch.rs     turning a request into bytes -- one of two sockets
//   everyday-core    building every URL, parsing every reply, and deciding
//                    what a result may change about an item
//
// The last of those is the part with the fiddly logic in it -- five reply
// formats, five rating scales, and the rule that metadata fills gaps and
// never argues -- and it is testable offline precisely because it never
// learns that a network exists.

/// A shelf, and how much is on it.
///
/// A named struct rather than widening `Kind` itself: the counts are facts
/// about storage at this instant, not properties of the shelf, and they must
/// not be written back when the interface saves one.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KindInfo {
    #[serde(flatten)]
    pub kind: Kind,
    pub items: u64,
    /// Wishlist, active and paused together: everything still ahead of you.
    pub open: u64,
}

/// Every shelf, with its counts, seeding the built-in set into an empty
/// library on the way.
///
/// The seeding is here, in the first call the library app makes, rather than
/// in `create_vault`. That is what makes it work for a vault somebody
/// already had before this app existed: the library appears on their next
/// launch with shelves in it rather than as an empty screen holding a "make
/// a category" button. It runs at most once per vault -- see
/// `Vault::seed_library`, which does nothing whenever there is any shelf at
/// all, so a deleted shelf stays deleted.
#[tauri::command]
pub async fn list_kinds(state: State<'_, AppState>) -> CommandResult<Vec<KindInfo>> {
    let vault = state.require()?;
    blocking(move || {
        if let Err(e) = vault.seed_library() {
            // Not fatal. An unwritable vault cannot be seeded and can still
            // be read, and a library with no shelves is a screen that says
            // so rather than an error over the whole window.
            tracing::warn!(error = %e, "could not seed the library");
        }
        let mut out = Vec::new();
        for kind in vault.kinds()? {
            // Two `COUNT(*)`s over clear index columns, so listing ten
            // shelves decrypts ten records and nothing else.
            let (items, open) = vault
                .with_store(|s| {
                    s.library().map(|l| l.count_items(kind.id)).unwrap_or_else(|| Ok((0, 0)))
                })
                .unwrap_or((0, 0));
            out.push(KindInfo { kind, items, open });
        }
        Ok(out)
    })
    .await
}

/// Mint a shelf. Unsaved: fill it in and pass it to `save_kind`.
///
/// The slug is derived from the name here rather than in the interface,
/// because it is the key metadata lookups and quick capture match on and it
/// has to be stable, lower case and free of spaces whatever somebody typed.
#[tauri::command]
pub fn new_kind(state: State<'_, AppState>, name: String, singular: String) -> CommandResult<Kind> {
    let _ = state.require()?;
    let name = name.trim();
    let singular = if singular.trim().is_empty() { name } else { singular.trim() };
    Ok(Kind::new(&slugify(singular), name, singular))
}

/// A stable, lower-case, hyphenless key for a name somebody typed.
///
/// Empty in, `custom` out: a shelf with no slug could never be looked up or
/// captured into, and refusing to create it over a punctuation-only name is
/// a worse answer than giving it a dull one.
fn slugify(name: &str) -> String {
    let out: String =
        name.trim().to_lowercase().chars().filter(|c| c.is_alphanumeric()).take(24).collect();
    if out.is_empty() { "custom".to_string() } else { out }
}

#[tauri::command]
pub async fn save_kind(state: State<'_, AppState>, kind: Kind) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.save_kind(&kind)?)).await
}

/// Delete the shelf, everything on it, and every log row those items had.
#[tauri::command]
pub async fn delete_kind(state: State<'_, AppState>, id: KindId) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.delete_kind(id)?)).await
}

#[tauri::command]
pub async fn list_items(state: State<'_, AppState>, query: ItemQuery) -> CommandResult<Vec<Item>> {
    let vault = state.require()?;
    blocking(move || Ok(vault.items(&query)?)).await
}

#[tauri::command]
pub async fn get_item(state: State<'_, AppState>, id: ItemId) -> CommandResult<Item> {
    let vault = state.require()?;
    blocking(move || Ok(vault.item(id)?)).await
}

#[tauri::command]
pub async fn save_item(state: State<'_, AppState>, item: Item) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.save_item(&item)?)).await
}

/// One write for many items: what a re-ordered shelf is.
#[tauri::command]
pub async fn save_items(state: State<'_, AppState>, items: Vec<Item>) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.save_items(&items)?)).await
}

/// Delete the item and its whole log.
#[tauri::command]
pub async fn delete_item(state: State<'_, AppState>, id: ItemId) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.delete_item(id)?)).await
}

/// Add something to a shelf, and — if asked — go and find out what it is.
///
/// One command rather than create-then-enrich, because the two are not
/// independent from the interface's point of view: what it wants back is the
/// finished card, and a two-step version would either draw a blank card that
/// changes under the cursor a second later or make the caller sequence two
/// awaits and handle a failure between them.
///
/// The lookup is best-effort by design, and it happens *after* the item is
/// on disk. A network that is off, a source that is down, a title nothing has
/// heard of — none of those should stop something being added to a list,
/// which is the entire job of this app. `looked_up` says whether anything was
/// found, so the interface can offer to search again rather than silently
/// implying it tried.
#[tauri::command]
pub async fn add_item(
    state: State<'_, AppState>,
    kind_id: KindId,
    title: String,
    lookup: bool,
) -> CommandResult<AddedItem> {
    let vault = state.require()?;
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err(CommandError::new("invalid", "give it a name and it will be added"));
    }
    let kind = {
        let vault = vault.clone();
        blocking(move || Ok(vault.kind(kind_id)?)).await?
    };
    let mut item = Item::new(kind_id, &title);

    // Written *before* the lookup, not after.
    //
    // This is the ordering the whole feature turns on. A lookup can take up
    // to the fetch timeout, and if the item were only minted in memory until
    // it came back, an app that was quit -- or a machine that ran out of
    // battery -- during those seconds would lose the thing somebody was
    // trying not to forget. Enriching costs a second write; getting this
    // backwards costs the note.
    {
        let (vault, saved) = (vault.clone(), item.clone());
        blocking(move || Ok(vault.save_item(&saved)?)).await?;
    }

    if !lookup {
        return Ok(AddedItem { item, looked_up: false });
    }

    let hit = match websearch::lookup(&title, &kind, 1).await {
        Ok(hits) => hits.into_iter().next(),
        // Worth a line in the log and nothing more: the item is already on
        // disk, and a network that is off is not a reason to refuse to keep
        // a list. See the doc comment.
        Err(e) => {
            tracing::info!(error = %e, "could not look up a new item");
            None
        }
    };
    let Some(hit) = hit else { return Ok(AddedItem { item, looked_up: false }) };

    websearch::apply_and_cover(&vault, &hit, &kind, &mut item, false).await;
    let (vault, saved) = (vault.clone(), item.clone());
    // The second write is the enrichment. If *it* fails, the item is still
    // there with the title that was typed, which is the outcome to prefer.
    blocking(move || Ok(vault.save_item(&saved)?)).await?;
    Ok(AddedItem { item, looked_up: true })
}

/// What [`add_item`] hands back: the item, and whether the web knew it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddedItem {
    pub item: Item,
    pub looked_up: bool,
}

/// Move an item to `status`, dating it and — optionally — logging it.
///
/// The dates and the log row are coupled here rather than left to the
/// interface because they are the same act. Marking a book read is the
/// moment "finished on" is known and the moment the log gains the row that
/// makes "what did I read this year" answerable, and an interface that had
/// to remember to do all three would eventually do two.
///
/// `Vault::save_item` still does the writing, so nothing here can produce an
/// item the ordinary save path would refuse.
#[tauri::command]
pub async fn set_item_status(
    state: State<'_, AppState>,
    id: ItemId,
    status: ItemStatus,
    log: bool,
) -> CommandResult<Item> {
    let vault = state.require()?;
    let today = today_local();
    let tz = system_tz();
    blocking(move || {
        let mut item = vault.item(id)?;
        if item.status == status {
            return Ok(item);
        }
        item.set_status(status, today);
        vault.save_item(&item)?;

        // Only the transitions that mean something happened on a day. Moving
        // something back to the wishlist is a correction, not an event, and
        // logging it would put a line in the history saying nothing.
        let event = match status {
            ItemStatus::Active => Some(LogEvent::Started),
            ItemStatus::Done => Some(LogEvent::Finished),
            ItemStatus::Paused | ItemStatus::Abandoned => Some(LogEvent::Stopped),
            ItemStatus::Wishlist => None,
        };
        if log && let Some(event) = event {
            vault.save_log(&LogEntry::new(item.id, event, today, &tz))?;
        }
        Ok(item)
    })
    .await
}

/// Record where you have got to, and log it.
///
/// The log row is what makes a reading pace visible later; the field on the
/// item is what the card draws now. Both, from one action, for the reason
/// given on [`set_item_status`].
#[tauri::command]
pub async fn set_item_progress(
    state: State<'_, AppState>,
    id: ItemId,
    position: u32,
    total: Option<u32>,
    log: bool,
) -> CommandResult<Item> {
    let vault = state.require()?;
    let today = today_local();
    let tz = system_tz();
    blocking(move || {
        let mut item = vault.item(id)?;
        // The unit comes from the shelf, and is copied onto the item rather
        // than read live -- see `library::Progress`, which explains why
        // changing a shelf from pages to minutes must not relabel four
        // hundred books.
        let unit = match &item.progress {
            Some(p) if !p.unit.is_empty() => p.unit.clone(),
            _ => vault.kind(item.kind_id).map(|k| k.progress_unit).unwrap_or_default(),
        };
        let total = total.or_else(|| item.progress.as_ref().and_then(|p| p.total));
        item.progress = Some(Progress::new(position, total, unit));
        // Recording progress on something you had only wished for is you
        // telling us you have started it.
        if item.status == ItemStatus::Wishlist {
            item.set_status(ItemStatus::Active, today);
        }
        item.updated_at = jiff::Timestamp::now();
        vault.save_item(&item)?;

        if log {
            let mut entry = LogEntry::new(item.id, LogEvent::Progress, today, &tz);
            entry.position = Some(position);
            vault.save_log(&entry)?;
        }
        Ok(item)
    })
    .await
}

#[tauri::command]
pub async fn list_logs(
    state: State<'_, AppState>,
    query: LogQuery,
) -> CommandResult<Vec<LogEntry>> {
    let vault = state.require()?;
    blocking(move || Ok(vault.logs(&query)?)).await
}

/// Mint a log row dated today on the machine's own calendar. Unsaved.
///
/// The date and the time zone are resolved here rather than in the webview so
/// that "what did I finish today" is decided by the same code that decides
/// which day a journal entry is filed under.
#[tauri::command]
pub fn new_log(
    state: State<'_, AppState>,
    item_id: ItemId,
    event: LogEvent,
) -> CommandResult<LogEntry> {
    let _ = state.require()?;
    Ok(LogEntry::new(item_id, event, today_local(), system_tz()))
}

#[tauri::command]
pub async fn save_log(state: State<'_, AppState>, log: LogEntry) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.save_log(&log)?)).await
}

#[tauri::command]
pub async fn delete_log(state: State<'_, AppState>, id: LogId) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.delete_log(id)?)).await
}

/// Counts for the library sidebar, as of the machine's own calendar year.
#[tauri::command]
pub async fn library_stats(state: State<'_, AppState>) -> CommandResult<LibraryStats> {
    let vault = state.require()?;
    let year = today_local().year();
    blocking(move || Ok(vault.library_stats(year)?)).await
}

// ---- roles and goals ----------------------------------------------------
//
// The Overview's own records. Everything below is the plain shape: require
// the vault, do the work on the blocking pool. The two reports at the foot
// are the only interesting ones, and their interest is entirely in the SQL
// they delegate to.

/// A role with the two counts the sidebar draws under its name.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleInfo {
    #[serde(flatten)]
    pub role: Role,
    /// Goals under it, in any state.
    pub goals: u64,
    /// Of those, the ones still being pursued.
    pub open: u64,
}

#[tauri::command]
pub async fn list_roles(state: State<'_, AppState>) -> CommandResult<Vec<RoleInfo>> {
    let vault = state.require()?;
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
/// Minted in the shell rather than in the interface so the id, the colour
/// and the two timestamps come from one place, exactly as `new_kind` does.
#[tauri::command]
pub fn new_role(state: State<'_, AppState>, name: String) -> CommandResult<Role> {
    let _ = state.require()?;
    Ok(Role::new(name.trim()))
}

#[tauri::command]
pub async fn save_role(state: State<'_, AppState>, role: Role) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.save_role(&role)?)).await
}

/// Delete a role. Refused, with a message naming the count, while goals
/// still point at it.
#[tauri::command]
pub async fn delete_role(state: State<'_, AppState>, id: RoleId) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.delete_role(id)?)).await
}

/// Offer a starting set of roles, and answer zero if there are any already.
#[tauri::command]
pub async fn seed_roles(state: State<'_, AppState>) -> CommandResult<usize> {
    let vault = state.require()?;
    blocking(move || Ok(vault.seed_roles()?)).await
}

#[tauri::command]
pub async fn list_goals(state: State<'_, AppState>, query: GoalQuery) -> CommandResult<Vec<Goal>> {
    let vault = state.require()?;
    blocking(move || Ok(vault.goals(&query)?)).await
}

#[tauri::command]
pub async fn get_goal(state: State<'_, AppState>, id: GoalId) -> CommandResult<Goal> {
    let vault = state.require()?;
    blocking(move || Ok(vault.goal(id)?)).await
}

/// Mint a goal under a role. Unsaved.
#[tauri::command]
pub fn new_goal(state: State<'_, AppState>, role_id: RoleId, title: String) -> CommandResult<Goal> {
    let _ = state.require()?;
    Ok(Goal::new(role_id, title.trim()))
}

#[tauri::command]
pub async fn save_goal(state: State<'_, AppState>, goal: Goal) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.save_goal(&goal)?)).await
}

#[tauri::command]
pub async fn save_goals(state: State<'_, AppState>, goals: Vec<Goal>) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.save_goals(&goals)?)).await
}

#[tauri::command]
pub async fn delete_goal(state: State<'_, AppState>, id: GoalId) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.delete_goal(id)?)).await
}

/// What the balance report is made of: minutes per purpose, and the events
/// somebody else booked, over one window.
///
/// Both halves in one call because the Overview always draws them together
/// and two round trips would let one arrive without the other, which shows
/// up as a chart that changes shape twice.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceReport {
    pub purposes: Vec<PurposeMinutes>,
    pub events: Vec<RoleEventMinutes>,
}

#[tauri::command]
pub async fn time_by_purpose(
    state: State<'_, AppState>,
    from: Date,
    to: Date,
) -> CommandResult<BalanceReport> {
    let vault = state.require()?;
    blocking(move || {
        let window = PurposeWindow::new(from, to);
        Ok(BalanceReport {
            purposes: vault.time_by_purpose(window)?,
            events: vault.events_by_role(window).unwrap_or_default(),
        })
    })
    .await
}

#[tauri::command]
pub async fn goal_activity(state: State<'_, AppState>, id: GoalId) -> CommandResult<GoalActivity> {
    let vault = state.require()?;
    blocking(move || Ok(vault.goal_activity(id)?)).await
}

// ---- web search ---------------------------------------------------------
//
// Exposed as its own pair of commands rather than hidden inside the library,
// because it is a facility and not a feature of one app. Anything in the
// interface can call `web_search` -- see `ui/src/lib/websearch.ts`, which is
// the class components actually use -- and the two library-specific commands
// below are built on the same core module rather than on a second one.

/// Search the web. The general entry point; any part of the app may call it.
///
/// Takes a whole [`SearchRequest`] rather than a bare string so that the
/// choice of source, the kind hint and the limit are the caller's, and so
/// that adding a source later is not a new command.
#[tauri::command]
pub async fn web_search(
    state: State<'_, AppState>,
    request: SearchRequest,
) -> CommandResult<Vec<SearchResult>> {
    // A locked vault is not a technical obstacle to a search -- nothing here
    // touches storage -- but it is the wrong moment for one. The lock screen
    // must not be a place from which requests leave the machine.
    let vault = state.require()?;
    if !vault.is_unlocked() {
        return Err(CommandError::from(everyday_core::Error::Locked));
    }
    websearch::search(&request).await
}

/// The sources a search can be run against, for the picker.
#[tauri::command]
pub fn search_sources() -> Vec<SourceInfo> {
    Source::ALL.iter().map(|s| SourceInfo::of(*s)).collect()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    pub id: String,
    pub label: String,
    pub has_images: bool,
}

impl SourceInfo {
    fn of(source: Source) -> Self {
        Self {
            id: source.slug().to_string(),
            label: source.label().to_string(),
            has_images: source.has_images(),
        }
    }
}

/// Look a title up using whatever source a shelf prefers, falling back to a
/// plain web search when that source draws a blank.
#[tauri::command]
pub async fn lookup_metadata(
    state: State<'_, AppState>,
    kind_id: KindId,
    query: String,
    limit: Option<u32>,
) -> CommandResult<Vec<SearchResult>> {
    let vault = state.require()?;
    let kind = {
        let vault = vault.clone();
        blocking(move || Ok(vault.kind(kind_id)?)).await?
    };
    websearch::lookup(query.trim(), &kind, limit.unwrap_or(websearch::DEFAULT_LIMIT)).await
}

/// Apply a chosen result to an item, downloading its cover on the way.
///
/// `overwrite` is the "yes, replace what is there" the interface offers on an
/// explicit re-fetch. Even then it leaves notes, your rating and the status
/// alone -- see `everyday_core::websearch::apply`, which is where that rule
/// lives and is tested.
#[tauri::command]
pub async fn apply_metadata(
    state: State<'_, AppState>,
    id: ItemId,
    result: SearchResult,
    overwrite: bool,
) -> CommandResult<Item> {
    let vault = state.require()?;
    let (mut item, kind) = {
        let vault = vault.clone();
        blocking(move || {
            let item = vault.item(id)?;
            let kind = vault.kind(item.kind_id)?;
            Ok((item, kind))
        })
        .await?
    };
    websearch::apply_and_cover(&vault, &result, &kind, &mut item, overwrite).await;

    let saved = item.clone();
    let vault = vault.clone();
    blocking(move || Ok(vault.save_item(&saved)?)).await?;
    Ok(item)
}

/// Download a picture into the vault and return its content address.
///
/// The general entry point, beside `web_search`: anything that has found an
/// image address can put it in the blob store with this, and it comes back as
/// a blob id the `everyday://` protocol will serve. Nothing in the interface
/// ever loads a remote image directly -- see `websearch.rs` for why, and the
/// content security policy for the check that outlives the reason.
#[tauri::command]
pub async fn fetch_image(state: State<'_, AppState>, url: String) -> CommandResult<String> {
    let vault = state.require()?;
    // Checked *before* the fetch, like `web_search`, and not left to
    // `put_blob` to refuse afterwards. `state.require` only says a vault is
    // open, so without this a locked vault still put a request on the wire
    // and only failed once the answer came back -- which is precisely the
    // thing the rule exists to prevent. The lock screen must not be a place
    // from which requests leave the machine.
    if !vault.is_unlocked() {
        return Err(CommandError::from(everyday_core::Error::Locked));
    }
    let bytes = websearch::fetch_image(&url).await?;
    blocking(move || Ok(vault.put_blob(&bytes)?.to_hex())).await
}

// ---- media --------------------------------------------------------------

/// Largest file accepted as an attachment.
///
/// Not a storage limit -- the blob store chunks and streams happily past this
/// -- but a bound on the single allocation this command makes, since the
/// payload arrives as one buffer from the webview.
const MAX_ATTACHMENT_BYTES: usize = 512 * 1024 * 1024;

/// Store an attachment and return its content address.
///
/// The bytes arrive as the request's *raw body* rather than as a named
/// argument, and that is the whole point of the odd signature. Tauri
/// serialises command arguments as JSON, and a `Uint8Array` inside a JSON
/// object becomes an array of numbers -- so a 100 MB video was turned into a
/// hundred million JavaScript numbers, stringified into roughly 350 MB of
/// text, and parsed back a byte at a time on this side. It froze the window
/// for as long as it took and peaked at something over a gigabyte of memory
/// for a file the disk had already handed us as a buffer.
///
/// A payload that *is* an `ArrayBuffer` at the top level is sent as
/// `application/octet-stream` instead and lands here as bytes.
#[tauri::command]
pub async fn put_blob(
    state: State<'_, AppState>,
    request: tauri::ipc::Request<'_>,
) -> CommandResult<String> {
    let bytes = match request.body() {
        tauri::ipc::InvokeBody::Raw(bytes) => bytes.clone(),
        // The slow path, kept working rather than refused. Tauri falls back
        // to `postMessage` when a webview blocks its custom protocol, and
        // there a buffer really does arrive as an array of numbers. Losing
        // attachments entirely in that configuration would be a worse
        // outcome than the allocation this exists to avoid.
        tauri::ipc::InvokeBody::Json(value) => serde_json::from_value::<Vec<u8>>(value.clone())
            .map_err(|_| CommandError::new("invalid", "attachment payload was not a buffer"))?,
    };
    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err(CommandError::new(
            "too_large",
            format!(
                "attachment is {} MB; the limit is {} MB",
                bytes.len() / 1_048_576,
                MAX_ATTACHMENT_BYTES / 1_048_576
            ),
        ));
    }
    let vault = state.require()?;
    blocking(move || Ok(vault.put_blob(&bytes)?.to_hex())).await
}

// ---- trackers and readings ----------------------------------------------
//
// The fourth domain. Definitions travel inside a `Journal` and are saved by
// `save_journal`, so what is here is minting one -- which needs an id and a
// clock the webview cannot be trusted with -- and everything to do with the
// readings themselves.

/// Mint a tracker, without saving it.
///
/// The id, the timestamps and the defaults come from here for the same
/// reason `new_journal` mints a journal: `crypto.randomUUID` needs a secure
/// context the packaged webview does not always provide, and a tracker that
/// silently fails to get an id is a tracker whose readings all pile up under
/// the same one.
#[tauri::command]
pub fn new_tracker(
    state: State<'_, AppState>,
    name: String,
    kind: TrackerKind,
) -> CommandResult<Tracker> {
    let _ = state.require()?;
    let mut tracker = Tracker::new(name, kind);
    tracker.normalize();
    Ok(tracker)
}

#[tauri::command]
pub async fn list_readings(
    state: State<'_, AppState>,
    query: ReadingQuery,
) -> CommandResult<Vec<Reading>> {
    let vault = state.require()?;
    blocking(move || Ok(vault.readings(&query)?)).await
}

/// One row per tracker per day: the aggregate every chart is built from.
#[tauri::command]
pub async fn tracker_days(
    state: State<'_, AppState>,
    query: ReadingQuery,
) -> CommandResult<Vec<TrackerDay>> {
    let vault = state.require()?;
    blocking(move || Ok(vault.tracker_days(&query)?)).await
}

/// Record one value, and decide what "when" means.
///
/// The whole of that decision lives here, in one place, because it is the
/// question this domain is easiest to get quietly wrong:
///
/// * an explicit `at` is always believed — the person corrected the time;
/// * ticking something on **today's** page records the minute, because that
///   minute is real: you are logging it as it happens;
/// * ticking something on a **past** page records the day and no minute at
///   all. Writing up Tuesday on Thursday says something true about Tuesday
///   and nothing whatever about 23:04, and a defaulted timestamp there would
///   put a mark on the calendar at an hour nothing happened.
#[tauri::command]
pub async fn log_reading(
    state: State<'_, AppState>,
    journal_id: JournalId,
    tracker_id: TrackerId,
    value: f64,
    date: jiff::civil::Date,
    at: Option<jiff::Timestamp>,
    entry_id: Option<EntryId>,
) -> CommandResult<Reading> {
    let vault = state.require()?;
    blocking(move || {
        let tz = system_tz();
        let now = jiff::Timestamp::now();
        let at = match at {
            Some(at) => Some(at),
            None if date == local_date_in(now, &tz) => Some(now),
            None => None,
        };

        let mut reading = match at {
            Some(at) => Reading::at(journal_id, tracker_id, at, &tz, value),
            None => Reading::on(journal_id, tracker_id, date, value),
        };
        reading.tz = tz;
        reading.entry_id = entry_id;
        vault.save_reading(&reading)?;
        // Read back rather than returned as written: the vault clamps the value
        // against the tracker's definition, and the interface should draw what
        // was stored rather than what it asked for.
        Ok(vault.reading(reading.id)?)
    })
    .await
}

/// Update a reading that already exists: a corrected dose, a note, a time.
#[tauri::command]
pub async fn save_reading(state: State<'_, AppState>, reading: Reading) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.save_reading(&reading)?)).await
}

#[tauri::command]
pub async fn delete_reading(state: State<'_, AppState>, id: ReadingId) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.delete_reading(id)?)).await
}

/// Remove a tracker from its journal along with every reading it made,
/// returning how many went. Archiving is the non-destructive half and is an
/// ordinary `save_journal`.
#[tauri::command]
pub async fn delete_tracker(
    state: State<'_, AppState>,
    journal_id: JournalId,
    tracker_id: TrackerId,
) -> CommandResult<u64> {
    let vault = state.require()?;
    blocking(move || Ok(vault.delete_tracker(journal_id, tracker_id)?)).await
}

// ---- maintenance --------------------------------------------------------

#[tauri::command]
pub async fn collect_garbage(state: State<'_, AppState>) -> CommandResult<u64> {
    let vault = state.require()?;
    blocking(move || Ok(vault.collect_garbage(everyday_core::store::GC_GRACE)?)).await
}

/// The interface reporting that its pending writes have landed.
///
/// Second half of the close handshake begun in `run`'s `CloseRequested`
/// handler: that one cancelled the close and asked for a flush, this one
/// completes it. Locking here rather than in the window handler is what
/// makes the ordering right -- the key is dropped after the last write, not
/// before it.
#[tauri::command]
pub async fn ready_to_close(
    window: tauri::Window,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    if let Some(vault) = state.get() {
        blocking(move || {
            // Best-effort: a checkpoint failing is not a reason to refuse to
            // quit, and the data is committed either way.
            let _ = vault.with_store(|s| s.flush());
            vault.lock();
            Ok(())
        })
        .await?;
    }
    window.destroy().map_err(|e| CommandError::new("close_failed", e.to_string()))
}

#[tauri::command]
pub async fn vault_stats(state: State<'_, AppState>) -> CommandResult<StoreStats> {
    let vault = state.require()?;
    blocking(move || Ok(vault.stats()?)).await
}

// ── the tray ───────────────────────────────────────────────────────────

/// Put `items` in the tray menu, showing the icon if it is not up yet.
///
/// The whole menu is sent every time rather than patched. A quick action's
/// label, its enabled state and whether it is offered at all are derived
/// from vault state that moves, so a change is as likely to be "this item is
/// gone" as "this item's label differs" -- and there is no patch protocol
/// for that which is simpler than resending six items. The interface only
/// calls this when the description has actually changed, so "every time" is
/// a handful of calls per session.
///
/// False means the desktop has no tray to put an icon in.
#[tauri::command]
pub async fn set_tray_menu(app: tauri::AppHandle, items: Vec<TrayItem>) -> CommandResult<bool> {
    // Off the async runtime like everything else here, though for a
    // different reason: building a menu is a series of hops to the main
    // thread, each of which blocks the caller until the event loop answers.
    blocking(move || app.state::<Tray>().show(&app, &items)).await
}

#[tauri::command]
pub async fn hide_tray(app: tauri::AppHandle) -> CommandResult<()> {
    blocking(move || {
        app.state::<Tray>().hide();
        Ok(())
    })
    .await
}

/// Read a blob for the media protocol handler.
pub fn read_blob_range(
    vault: &Arc<Vault>,
    id: BlobId,
    offset: u64,
    len: u64,
) -> everyday_core::Result<Vec<u8>> {
    vault.with_store(|s| s.get_blob_range(id, offset, len))
}

pub fn blob_len(vault: &Arc<Vault>, id: BlobId) -> everyday_core::Result<u64> {
    vault.with_store(|s| s.blob_len(id))
}

// ---- the assistant ------------------------------------------------------
//
// The sixth domain's command surface, and the smallest of the six: almost
// everything the assistant can do it does through its *tools*, which are in
// the core and reached from `crate::agent` rather than from here. What is
// left is configuring it, reading its threads back, and the two halves of one
// conversation -- `send_message` and the confirmation that answers it.

/// How the assistant is configured. Never carries the API key; see
/// [`everyday_core::agent`] for why that is structural rather than a habit.
#[tauri::command]
pub async fn agent_settings(state: State<'_, AppState>) -> CommandResult<AgentSettings> {
    let vault = state.require()?;
    blocking(move || Ok(vault.agent_settings()?)).await
}

#[tauri::command]
pub async fn save_agent_settings(
    state: State<'_, AppState>,
    settings: AgentSettings,
) -> CommandResult<AgentSettings> {
    let vault = state.require()?;
    blocking(move || {
        vault.save_agent_settings(&settings)?;
        // Read back rather than echoing what was sent: `has_key` is derived
        // from the secret table, so the pane must be told what is true rather
        // than what it asked for.
        Ok(vault.agent_settings()?)
    })
    .await
}

/// Store the API key. There is no command that reads one back.
#[tauri::command]
pub async fn set_agent_key(state: State<'_, AppState>, key: String) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.set_agent_key(&key)?)).await
}

#[tauri::command]
pub async fn clear_agent_key(state: State<'_, AppState>) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.clear_agent_key()?)).await
}

/// One row per thread for the history list, with how long each one is.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSummary {
    #[serde(flatten)]
    pub conversation: Conversation,
    pub messages: u64,
}

#[tauri::command]
pub async fn list_conversations(
    state: State<'_, AppState>,
    limit: Option<u32>,
) -> CommandResult<Vec<ConversationSummary>> {
    let vault = state.require()?;
    blocking(move || {
        let query = ConversationQuery { limit, offset: 0 };
        vault
            .conversations(&query)?
            .into_iter()
            .map(|c| {
                let messages = vault.message_count(c.id)?;
                Ok(ConversationSummary { conversation: c, messages })
            })
            .collect()
    })
    .await
}

/// Mint a thread, without saving it. The id is the core's to allocate, for
/// the reason given on [`new_journal`].
#[tauri::command]
pub fn new_conversation(state: State<'_, AppState>) -> CommandResult<Conversation> {
    let _ = state.require()?;
    Ok(Conversation::new())
}

#[tauri::command]
pub async fn conversation_messages(
    state: State<'_, AppState>,
    id: ConversationId,
) -> CommandResult<Vec<AgentMessage>> {
    let vault = state.require()?;
    blocking(move || Ok(vault.messages(id)?)).await
}

#[tauri::command]
pub async fn delete_conversation(
    state: State<'_, AppState>,
    id: ConversationId,
) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.delete_conversation(id)?)).await
}

/// Say something to the assistant, and stream what it says back.
///
/// The reply arrives on `channel` rather than as this command's return value:
/// a turn takes seconds and calls tools while it runs, and a panel that could
/// draw none of that until the end would read as a hang. What this returns is
/// only whether the turn finished.
#[tauri::command]
pub async fn send_message(
    state: State<'_, AppState>,
    pending: State<'_, Arc<Pending>>,
    conversation_id: ConversationId,
    prompt: String,
    context: Option<String>,
    channel: tauri::ipc::Channel<AgentEvent>,
) -> CommandResult<()> {
    let vault = state.require()?;
    agent::run_turn(agent::Turn {
        vault,
        pending: pending.inner().clone(),
        conversation: conversation_id,
        prompt,
        context,
        channel,
    })
    .await
}

/// Answer a confirmation the assistant is waiting on.
///
/// Returns whether anything was still waiting: a turn that was cancelled
/// between the question and the click leaves a card on screen with nothing
/// behind it, and the panel dismisses it rather than showing an error.
#[tauri::command]
pub fn confirm_tool_call(
    pending: State<'_, Arc<Pending>>,
    call_id: String,
    approved: bool,
) -> CommandResult<bool> {
    Ok(pending.answer(&call_id, approved))
}

#[tauri::command]
pub async fn list_memories(state: State<'_, AppState>) -> CommandResult<Vec<Memory>> {
    let vault = state.require()?;
    blocking(move || Ok(vault.memories()?)).await
}

/// Write a memory by hand, which also pins it: a fact somebody typed is not
/// one the assistant's own housekeeping may drop.
#[tauri::command]
pub async fn save_memory(state: State<'_, AppState>, memory: Memory) -> CommandResult<Vec<Memory>> {
    let vault = state.require()?;
    blocking(move || Ok(vault.save_memory(&Memory { pinned: true, ..memory })?)).await
}

#[tauri::command]
pub async fn delete_memory(state: State<'_, AppState>, id: MemoryId) -> CommandResult<()> {
    let vault = state.require()?;
    blocking(move || Ok(vault.delete_memory(id)?)).await
}
