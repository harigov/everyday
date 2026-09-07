//! The command surface exposed to the interface.
//!
//! Every command runs its storage work on the blocking pool rather than on an
//! async worker. Vault operations touch disk and, when unlocking, deliberately
//! burn ~64 MiB of memory in Argon2; doing that on the runtime's async threads
//! would stall every other task, and doing it on the webview's thread would
//! freeze the window mid-keystroke.

use everyday_core::calendar::{Calendar, CalendarProvider, Event, SyncReport};
use everyday_core::model::local_date_in;
use everyday_core::search::SearchHit;
use everyday_core::store::calendars::EventQuery;
use everyday_core::store::tasks::{BlockQuery, TaskQuery};
use everyday_core::store::{EntryQuery, StoreStats};
use everyday_core::task::{
    BlockKind, BlockSubject, Project, Task, TaskStats, TaskStatus, TimeBlock,
};
use everyday_core::{
    BlobId, BlockId, CalendarId, Entry, EntryId, EventId, Journal, JournalId, ProjectId, TaskId,
    Vault, VaultConfig, VaultStatus,
};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::State;

use crate::error::{CommandError, CommandResult};
use crate::feeds;
use crate::state::AppState;

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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendInfo {
    pub id: String,
    pub description: String,
}

/// What the interface needs before any vault is open.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    pub vault_exists: bool,
    /// The vault location in play: the one last opened if it is still there,
    /// otherwise where a new vault would be created.
    pub default_path: PathBuf,
    pub backends: Vec<BackendInfo>,
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
    let backends = everyday_vault::available_backends()
        .into_iter()
        .map(|(id, description)| BackendInfo {
            id: id.to_string(),
            description: description.to_string(),
        })
        .collect();

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
    password: Option<String>,
) -> CommandResult<VaultStatus> {
    if let Some(p) = password.as_deref() {
        everyday_vault::validate_password(p)?;
    }
    let config = VaultConfig {
        name,
        backend,
        password,
        kdf: Default::default(),
        auto_lock_seconds: 15 * 60,
    };
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
pub fn lock(state: State<'_, AppState>) -> CommandResult<VaultStatus> {
    let vault = state.require()?;
    vault.lock();
    Ok(vault.status())
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
    blocking(move || {
        vault.change_password(Some(&current), Some(&next)).map_err(CommandError::from)
    })
    .await
}

#[tauri::command]
pub fn set_auto_lock(state: State<'_, AppState>, seconds: u64) -> CommandResult<()> {
    Ok(state.require()?.set_auto_lock(seconds)?)
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
pub fn list_journals(state: State<'_, AppState>) -> CommandResult<Vec<Journal>> {
    Ok(state.require()?.journals()?)
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
pub fn save_journal(state: State<'_, AppState>, journal: Journal) -> CommandResult<()> {
    Ok(state.require()?.save_journal(&journal)?)
}

#[tauri::command]
pub fn delete_journal(state: State<'_, AppState>, id: JournalId) -> CommandResult<()> {
    Ok(state.require()?.delete_journal(id)?)
}

// ---- entries ------------------------------------------------------------

#[tauri::command]
pub fn list_entries(
    state: State<'_, AppState>,
    query: EntryQuery,
) -> CommandResult<Vec<everyday_core::EntrySummary>> {
    Ok(state.require()?.entries(&query)?)
}

#[tauri::command]
pub fn get_entry(state: State<'_, AppState>, id: EntryId) -> CommandResult<Entry> {
    Ok(state.require()?.entry(id)?)
}

/// A blank entry, filed under today in the machine's own time zone.
#[tauri::command]
pub fn new_entry(state: State<'_, AppState>, journal_id: JournalId) -> CommandResult<Entry> {
    let _ = state.require()?;
    let tz = jiff::tz::TimeZone::system().iana_name().unwrap_or("UTC").to_string();
    let mut entry = Entry::new(journal_id, &tz);
    entry.local_date = local_date_in(entry.created_at, &tz);
    Ok(entry)
}

#[tauri::command]
pub fn save_entry(state: State<'_, AppState>, entry: Entry) -> CommandResult<()> {
    Ok(state.require()?.save_entry(&entry)?)
}

#[tauri::command]
pub fn delete_entry(state: State<'_, AppState>, id: EntryId) -> CommandResult<()> {
    Ok(state.require()?.delete_entry(id)?)
}

// ---- search -------------------------------------------------------------

#[tauri::command]
pub fn search(
    state: State<'_, AppState>,
    query: String,
    journal_id: Option<JournalId>,
    limit: usize,
) -> CommandResult<Vec<SearchHit>> {
    Ok(state.require()?.search(&query, journal_id, limit.min(200))?)
}

#[tauri::command]
pub fn list_tags(state: State<'_, AppState>) -> CommandResult<Vec<String>> {
    let vault = state.require()?;
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for e in vault.entries(&EntryQuery::default())? {
        for t in e.tags {
            *counts.entry(t).or_default() += 1;
        }
    }
    let mut tags: Vec<(String, usize)> = counts.into_iter().collect();
    tags.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(tags.into_iter().map(|(t, _)| t).collect())
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
pub fn list_projects(state: State<'_, AppState>) -> CommandResult<Vec<Project>> {
    Ok(state.require()?.projects()?)
}

/// Mint a project, without saving it.
#[tauri::command]
pub fn new_project(state: State<'_, AppState>, name: String) -> CommandResult<Project> {
    let _ = state.require()?;
    Ok(Project::new(name))
}

#[tauri::command]
pub fn save_project(state: State<'_, AppState>, project: Project) -> CommandResult<()> {
    Ok(state.require()?.save_project(&project)?)
}

/// Delete a project, its tasks and every block of time booked against them.
#[tauri::command]
pub fn delete_project(state: State<'_, AppState>, id: ProjectId) -> CommandResult<()> {
    Ok(state.require()?.delete_project(id)?)
}

#[tauri::command]
pub fn list_tasks(state: State<'_, AppState>, query: TaskQuery) -> CommandResult<Vec<Task>> {
    Ok(state.require()?.tasks(&query)?)
}

#[tauri::command]
pub fn get_task(state: State<'_, AppState>, id: TaskId) -> CommandResult<Task> {
    Ok(state.require()?.task(id)?)
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
pub fn save_task(state: State<'_, AppState>, task: Task) -> CommandResult<()> {
    Ok(state.require()?.save_task(&task)?)
}

/// Write several tasks at once. This is what dragging a card across a board
/// is: two columns renumbered, which must land as one change or not at all.
#[tauri::command]
pub fn save_tasks(state: State<'_, AppState>, tasks: Vec<Task>) -> CommandResult<()> {
    Ok(state.require()?.save_tasks(&tasks)?)
}

/// Delete a task, its subtasks and their time blocks.
#[tauri::command]
pub fn delete_task(state: State<'_, AppState>, id: TaskId) -> CommandResult<()> {
    Ok(state.require()?.delete_task(id)?)
}

#[tauri::command]
pub fn list_blocks(state: State<'_, AppState>, query: BlockQuery) -> CommandResult<Vec<TimeBlock>> {
    Ok(state.require()?.blocks(&query)?)
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
    let tz = jiff::tz::TimeZone::system().iana_name().unwrap_or("UTC").to_string();
    let mut block = TimeBlock::new(subject, start, minutes, &tz);
    if let Some(kind) = kind {
        block.kind = kind;
    }
    Ok(block)
}

#[tauri::command]
pub fn save_block(state: State<'_, AppState>, block: TimeBlock) -> CommandResult<()> {
    Ok(state.require()?.save_block(&block)?)
}

#[tauri::command]
pub fn delete_block(state: State<'_, AppState>, id: BlockId) -> CommandResult<()> {
    Ok(state.require()?.delete_block(id)?)
}

/// Every tag used anywhere in the task domain, most used first.
#[tauri::command]
pub fn task_tags(state: State<'_, AppState>) -> CommandResult<Vec<TagCount>> {
    Ok(state
        .require()?
        .task_tags()?
        .into_iter()
        .map(|(tag, count)| TagCount { tag, count })
        .collect())
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
pub fn task_stats(state: State<'_, AppState>) -> CommandResult<TaskStats> {
    let tz = jiff::tz::TimeZone::system().iana_name().unwrap_or("UTC").to_string();
    let today = local_date_in(jiff::Timestamp::now(), &tz);
    Ok(state.require()?.task_stats(today)?)
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
pub fn list_calendars(state: State<'_, AppState>) -> CommandResult<Vec<CalendarInfo>> {
    let vault = state.require()?;
    let mut out = Vec::new();
    for calendar in vault.calendars()? {
        // The count is a `COUNT(*)` over a clear index column, so listing
        // four calendars decrypts four records and nothing else.
        let events = vault.event_count(calendar.id).unwrap_or(0);
        out.push(CalendarInfo { calendar, events });
    }
    Ok(out)
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
pub fn save_calendar(state: State<'_, AppState>, calendar: Calendar) -> CommandResult<()> {
    Ok(state.require()?.save_calendar(&calendar)?)
}

/// Unsubscribe: the calendar and every event that came from it.
#[tauri::command]
pub fn delete_calendar(state: State<'_, AppState>, id: CalendarId) -> CommandResult<()> {
    Ok(state.require()?.delete_calendar(id)?)
}

#[tauri::command]
pub fn list_events(state: State<'_, AppState>, query: EventQuery) -> CommandResult<Vec<Event>> {
    Ok(state.require()?.events(&query)?)
}

#[tauri::command]
pub fn get_event(state: State<'_, AppState>, id: EventId) -> CommandResult<Event> {
    Ok(state.require()?.event(id)?)
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
    sync_one(&vault, id).await
}

/// Fetch every subscription whose refresh interval has elapsed.
///
/// Polled by the interface rather than driven by a timer in here, so that a
/// locked vault is never fetched into and a window nobody is looking at is
/// never the reason a laptop wakes its radio. Failures are collected, not
/// raised: one calendar being down must not stop the other three.
#[tauri::command]
pub async fn sync_due_calendars(
    state: State<'_, AppState>,
    force: bool,
) -> CommandResult<Vec<SyncReport>> {
    let vault = state.require()?;
    let now = jiff::Timestamp::now();
    let due: Vec<CalendarId> = vault
        .calendars()?
        .into_iter()
        .filter(|c| c.origin.url().is_some() && (force || c.is_due(now)))
        .map(|c| c.id)
        .collect();

    let mut out = Vec::new();
    for id in due {
        match sync_one(&vault, id).await {
            Ok(report) => out.push(report),
            // Already recorded on the subscription by `sync_one`; the
            // interface reads it from there, beside the calendar it belongs
            // to, rather than as an error over the whole operation.
            Err(e) => tracing::info!(%id, error = %e, "a calendar could not be refreshed"),
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
    let window = feeds::sync_window(feeds::today());
    let tz = feeds::local_tz();
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
    let mut calendar = Calendar::subscribed(name.trim(), &url).with_color(color);

    let text = feeds::fetch(&url).await?;
    // Name it after the publisher when the person adding it did not: Google,
    // Outlook and Apple all set `X-WR-CALNAME`, and "Priya — Work" is a
    // better name than anything a text field would have got out of someone
    // in a hurry.
    if calendar.name.is_empty() {
        calendar.name = everyday_core::ics::parse(&text)
            .name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "Calendar".to_string());
    }
    let id = calendar.id;
    {
        let vault = vault.clone();
        let calendar = calendar.clone();
        blocking(move || Ok(vault.save_calendar(&calendar)?)).await?;
    }

    match apply_feed(&vault, id, text).await {
        Ok(report) => Ok(CalendarInfo { calendar: vault.calendar(id)?, events: report.events }),
        Err(e) => {
            // Nothing added: see the doc comment. Undoing the save is safe
            // because nothing else can have pointed at it yet.
            let _ = vault.delete_calendar(id);
            Err(e)
        }
    }
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
    let mut calendar = Calendar::imported(name.trim(), label).with_color(color);
    if calendar.name.is_empty() {
        calendar.name = everyday_core::ics::parse(&ics)
            .name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "Imported calendar".to_string());
    }
    let id = calendar.id;
    {
        let vault = vault.clone();
        let calendar = calendar.clone();
        blocking(move || Ok(vault.save_calendar(&calendar)?)).await?;
    }
    match apply_feed(&vault, id, ics).await {
        Ok(report) => Ok(CalendarInfo { calendar: vault.calendar(id)?, events: report.events }),
        Err(e) => {
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

// ---- media --------------------------------------------------------------

/// Largest file accepted as an attachment.
///
/// Not a storage limit -- the blob store chunks and streams happily past this
/// -- but a bound on the single allocation this command makes, since the
/// bytes arrive as one JSON array from the webview.
const MAX_ATTACHMENT_BYTES: usize = 512 * 1024 * 1024;

#[tauri::command]
pub async fn put_blob(state: State<'_, AppState>, bytes: Vec<u8>) -> CommandResult<String> {
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

// ---- maintenance --------------------------------------------------------

#[tauri::command]
pub async fn collect_garbage(state: State<'_, AppState>) -> CommandResult<u64> {
    let vault = state.require()?;
    blocking(move || Ok(vault.collect_garbage()?)).await
}

#[tauri::command]
pub fn vault_stats(state: State<'_, AppState>) -> CommandResult<StoreStats> {
    Ok(state.require()?.stats()?)
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
