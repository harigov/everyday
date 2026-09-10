//! The quick model's commands: one per flow that has a use for it.
//!
//! Every one of these follows the same three steps — read whatever the vault
//! has to say, build a [`Prompt`](everyday_core::quick) in the core, post it
//! through [`crate::quick`] and hand the clamped answer back. None of them
//! *writes* anything. That is the module's whole discipline and the reason it
//! can be this long without being risky: an answer here becomes a chip beside
//! a field, and it is the existing `update_task`, `save_item` or `add_reading`
//! that applies it when somebody taps.
//!
//! # Why they are commands rather than one `quick_run`
//!
//! A single command taking a job name and a blob would put the prompt-building
//! in the interface, which is exactly where it must not be: what the model is
//! told would then be untestable, unversioned and different in the CLI. The
//! interface asks for *a suggestion about this task*; the core decides what
//! that means.
//!
//! # Failure is not an error anybody sees
//!
//! A quick job that times out, comes back malformed, or is switched off gives
//! back an empty answer rather than an error, wherever an empty answer is
//! expressible. The interaction is a chip that appears; the failure of a chip
//! to appear is not a thing to interrupt somebody about. The log line is for
//! whoever is debugging it.

use std::sync::Arc;

use everyday_core::agent::AgentSettings;
use everyday_core::model::system_tz;
use everyday_core::purpose::{Goal, Purpose, Role};
use everyday_core::quick::{self, QuickContext, QuickJob, QuickPolicy};
use everyday_core::store::purpose::GoalQuery;
use everyday_core::store::tasks::{TaskQuery, TaskSort};
use everyday_core::tracker::Tracker;
use everyday_core::{
    EntryId, GoalId, ItemId, JournalId, KindId, NoteId, RoleId, TaskId, TrackerId, Vault,
};
use serde::{Deserialize, Serialize};

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult};
use crate::service::{Service, blocking};

use super::Nothing;

// ── What the settings pane draws ─────────────────────────────────────────

/// One switch in the settings pane.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobRow {
    pub name: &'static str,
    pub label: &'static str,
    /// What this job sends. The sentence somebody reads to decide.
    pub blurb: &'static str,
    pub app: &'static str,
    /// Where the switch is drawn right now, defaults and policy combined.
    pub on: bool,
    /// Whether it is on because nobody has said otherwise.
    pub default_on: bool,
}

fn rows(settings: &AgentSettings) -> Vec<JobRow> {
    quick::JOBS
        .iter()
        .map(|job: &'static QuickJob| JobRow {
            name: job.name,
            label: job.label,
            blurb: job.blurb,
            app: job.app.label(),
            on: settings.quick_jobs.allows(job.name),
            default_on: job.default_on,
        })
        .collect()
}

/// Every quick job and whether it is on, for the Assistant tab in settings.
async fn quick_jobs(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<Vec<JobRow>> {
    let vault = svc.require()?;
    blocking(move || Ok(rows(&vault.agent_settings()?))).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetJob {
    pub name: String,
    pub on: bool,
}

/// Turn one job on or off.
///
/// A command of its own rather than a corner of `save_agent_settings`,
/// because the switches are twenty checkboxes somebody flicks one at a time
/// and round-tripping the whole settings record per flick is how two panes
/// open at once overwrite each other.
async fn set_quick_job(svc: Arc<Service>, _ctx: Ctx, args: SetJob) -> CommandResult<Vec<JobRow>> {
    let vault = svc.require()?;
    blocking(move || {
        let mut settings = vault.agent_settings()?;
        let mut policy: QuickPolicy = settings.quick_jobs.clone();
        policy.set(&args.name, args.on);
        settings.quick_jobs = policy;
        vault.save_agent_settings(&settings)?;
        Ok(rows(&vault.agent_settings()?))
    })
    .await
}

// ── The plumbing every command below shares ──────────────────────────────

/// Today, where the person is. Read from the vault's own zone rather than the
/// host's, for the reason `AgentSettings::timezone` gives: a vault served from
/// a machine under a desk has that machine's clock, and "friday" typed into a
/// capture box has to mean Friday where the *person* is.
fn context(settings: &AgentSettings) -> QuickContext {
    let now = settings.now();
    QuickContext::new(now.date(), settings.timezone.clone().unwrap_or_else(system_tz))
}

/// Read something out of the vault, build a prompt from it, post it, and
/// parse the answer.
///
/// The shape every command here has. `prep` runs on the blocking pool
/// because it touches the store; the request does not.
async fn ask<T, P>(svc: &Arc<Service>, ctx: &Ctx, job: &str, prep: P) -> CommandResult<T>
where
    T: serde::de::DeserializeOwned,
    P: FnOnce(&Vault, &QuickContext) -> CommandResult<quick::Prompt> + Send + 'static,
{
    // Declared by every command as its scope too; checked here as well
    // because `ask` is the only path to the socket and a check that lives
    // beside the thing it protects cannot be left off a new command.
    if !ctx.holds(crate::ctx::Scope::Quick) {
        return Err(CommandError::new("forbidden", "this client may not spend the quick model"));
    }
    let vault = svc.require()?;
    let prompt = {
        let vault = vault.clone();
        blocking(move || {
            let settings = vault.agent_settings()?;
            let ctx = context(&settings);
            prep(&vault, &ctx)
        })
        .await?
    };
    let job = job.to_string();
    let raw = crate::quick::run(vault, prompt).await?;
    quick::parse(&job, raw).map_err(|e| CommandError::new("quick", e.to_string()))
}

/// The roles and goals a purpose suggestion chooses between, in the order the
/// prompt numbers them: goals first, then roles.
///
/// Returned alongside so the answer's *number* can be turned back into a
/// [`Purpose`] here, where the list came from. The model never sees an id and
/// never returns one.
fn purposes(vault: &Vault) -> CommandResult<(Vec<Role>, Vec<Goal>)> {
    let roles = vault.roles()?;
    let goals = vault.goals(&GoalQuery::open())?;
    Ok((roles, goals))
}

/// Turn a one-based position in that list back into a purpose.
fn purpose_at(roles: &[Role], goals: &[Goal], choice: Option<usize>) -> Option<Purpose> {
    let n = choice?;
    if n == 0 {
        return None;
    }
    let index = n - 1;
    if let Some(goal) = goals.get(index) {
        return Some(Purpose::Goal { id: goal.id });
    }
    roles.get(index - goals.len()).map(|role| Purpose::Role { id: role.id })
}

/// Tags already in use, so a suggestion prefers an existing one over
/// inventing a synonym — which is the whole difference between tags that
/// aggregate and tags that do not.
fn known_tags<N: Copy + Into<u64>>(
    counted: everyday_core::Result<Vec<(String, N)>>,
) -> Vec<String> {
    let mut tags = counted.unwrap_or_default();
    // Commonest first, and only a handful: the point is to make an existing
    // tag the near option, not to recite the vocabulary.
    tags.sort_by_key(|(_, n)| std::cmp::Reverse((*n).into()));
    tags.into_iter().take(MAX_KNOWN_TAGS).map(|(t, _)| t).collect()
}

/// How many existing tags a suggestion prompt is shown.
///
/// Enough to recognise the vocabulary, few enough that it does not become the
/// bulk of a prompt advertised as cheap.
const MAX_KNOWN_TAGS: usize = 40;

// ── What the commands answer with ────────────────────────────────────────

/// Tags and a purpose, with the purpose already resolved to an id.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Labels {
    pub tags: Vec<String>,
    pub purpose: Option<Purpose>,
}

/// A reading, with its tracker resolved — or a name to make one under.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reading {
    /// `None` when it is proposing a tracker that does not exist yet.
    pub tracker_id: Option<TrackerId>,
    pub name: String,
    pub value: f64,
    pub at: Option<String>,
    pub label: String,
}

fn readings_from(trackers: &[Tracker], answer: quick::ReadingsAnswer) -> Vec<Reading> {
    answer
        .readings
        .into_iter()
        .map(|r| {
            let tracker = r.tracker.and_then(|n| n.checked_sub(1)).and_then(|i| trackers.get(i));
            Reading {
                tracker_id: tracker.map(|t| t.id),
                name: tracker.map(|t| t.name.clone()).unwrap_or(r.name),
                value: r.value,
                at: r.at,
                label: r.label,
            }
        })
        .collect()
}

// ── Library ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemRef {
    pub item_id: ItemId,
}

/// L1/L4 — the fields a shelf wants, out of what the search found.
///
/// Takes the item rather than a query so that the *shelf's* declared fields
/// decide what is asked for. A fact with no field to live in is invisible in
/// the interface and undeletable from it, so it is never asked for in the
/// first place and dropped again by `FieldsAnswer::clamp` if it arrives.
async fn quick_item_fields(
    svc: Arc<Service>,
    ctx: Ctx,
    args: ItemRef,
) -> CommandResult<quick::FieldsAnswer> {
    let (item, kind) = {
        let vault = svc.require()?;
        blocking(move || {
            let item = vault.item(args.item_id)?;
            let kind = vault.kind(item.kind_id)?;
            Ok((item, kind))
        })
        .await?
    };
    // The search itself is `crate::websearch`, which owns its own socket and
    // its own scope. Doing it here rather than making the interface pass the
    // results in keeps the two requests one command from the interface's
    // point of view, which is what lets it draw one spinner.
    let results = crate::websearch::lookup(&item.title, &kind, 3).await.unwrap_or_default();
    if results.is_empty() {
        return Ok(quick::FieldsAnswer::default());
    }
    let keys: Vec<String> = kind.fields.iter().map(|f| f.key.clone()).collect();
    let mut answer: quick::FieldsAnswer = ask(&svc, &ctx, "library.fields", move |_, qctx| {
        Ok(quick::library_fields(qctx, &kind, &item, &results))
    })
    .await?;
    answer.clamp(&keys);
    Ok(answer)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickArgs {
    pub kind_id: KindId,
    pub query: String,
    /// The results as the interface already has them, so the choice is made
    /// over exactly the list somebody is looking at.
    pub results: Vec<everyday_core::websearch::SearchResult>,
}

/// L2 — which of these is the thing, if any of them is.
async fn quick_pick_result(
    svc: Arc<Service>,
    ctx: Ctx,
    args: PickArgs,
) -> CommandResult<Option<usize>> {
    let len = args.results.len();
    if len == 0 {
        return Ok(None);
    }
    let PickArgs { kind_id, query, results } = args;
    let answer: quick::PickAnswer = ask(&svc, &ctx, "library.pick", move |vault, qctx| {
        let kind = vault.kind(kind_id)?;
        Ok(quick::library_pick(qctx, &kind, &query, &results))
    })
    .await?;
    Ok(answer.index(len))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KindName {
    pub name: String,
}

/// L3 — design a shelf from its name.
async fn quick_kind_draft(
    svc: Arc<Service>,
    ctx: Ctx,
    args: KindName,
) -> CommandResult<quick::KindDraft> {
    let name = args.name.clone();
    let mut draft: quick::KindDraft =
        ask(&svc, &ctx, "library.kind", move |_, qctx| Ok(quick::library_kind(qctx, &name)))
            .await?;
    draft.clamp();
    Ok(draft)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportColumns {
    pub kind_id: KindId,
    pub columns: Vec<String>,
    #[serde(default)]
    pub sample: Vec<String>,
}

/// L5 — map somebody's exported list onto a shelf.
async fn quick_import_columns(
    svc: Arc<Service>,
    ctx: Ctx,
    args: ImportColumns,
) -> CommandResult<quick::MappingAnswer> {
    let ImportColumns { kind_id, columns, sample } = args;
    let (their_columns, kind_id2) = (columns.clone(), kind_id);
    let mut answer: quick::MappingAnswer =
        ask(&svc, &ctx, "library.import_map", move |vault, qctx| {
            let kind = vault.kind(kind_id2)?;
            Ok(quick::library_import_map(qctx, &kind, &columns, &sample))
        })
        .await?;
    let vault = svc.require()?;
    let ours: Vec<String> = blocking(move || {
        let kind = vault.kind(kind_id)?;
        Ok(["title", "subtitle", "creator", "year"]
            .iter()
            .map(|s| s.to_string())
            .chain(kind.fields.iter().map(|f| f.key.clone()))
            .collect())
    })
    .await?;
    answer.clamp(&ours, &their_columns);
    Ok(answer)
}

// ── Todo ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    pub line: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRef {
    pub task_id: TaskId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Title {
    pub title: String,
}

/// T1 — which role or goal a task serves, and what to tag it.
async fn quick_task_labels(svc: Arc<Service>, ctx: Ctx, args: Title) -> CommandResult<Labels> {
    let vault = svc.require()?;
    let (roles, goals) = {
        let vault = vault.clone();
        blocking(move || purposes(&vault)).await?
    };
    if roles.is_empty() && goals.is_empty() {
        // Nothing to choose between. Asking anyway would spend a request to
        // be told null.
        return Ok(Labels::default());
    }
    let (r, g) = (roles.clone(), goals.clone());
    let title = args.title;
    let mut answer: quick::LabelsAnswer = ask(&svc, &ctx, "todo.purpose", move |vault, qctx| {
        let tags = known_tags(vault.task_tags());
        Ok(quick::todo_purpose(qctx, &title, &r, &g, &tags))
    })
    .await?;
    answer.clamp(&[]);
    Ok(Labels { purpose: purpose_at(&roles, &goals, answer.purpose), tags: answer.tags })
}

/// T2 — the line the sigil grammar could not read.
async fn quick_task_from_line(
    svc: Arc<Service>,
    ctx: Ctx,
    args: Line,
) -> CommandResult<Option<quick::TaskDraft>> {
    let line = args.line;
    let mut draft: quick::TaskDraft =
        ask(&svc, &ctx, "todo.parse", move |_, qctx| Ok(quick::todo_parse(qctx, &line))).await?;
    Ok(draft.clamp().then_some(draft))
}

/// T3 — the steps a task is made of.
async fn quick_subtasks(
    svc: Arc<Service>,
    ctx: Ctx,
    args: TaskRef,
) -> CommandResult<Vec<quick::TaskDraft>> {
    let id = args.task_id;
    let mut answer: quick::TasksAnswer = ask(&svc, &ctx, "todo.subtasks", move |vault, qctx| {
        let task = vault.task(id)?;
        Ok(quick::todo_subtasks(qctx, &task.title, &task.notes))
    })
    .await?;
    answer.clamp();
    Ok(answer.tasks)
}

/// T4 — how long, grounded in what this person's own work actually took.
async fn quick_estimate(svc: Arc<Service>, ctx: Ctx, args: TaskRef) -> CommandResult<Option<u32>> {
    let id = args.task_id;
    let mut answer: quick::EstimateAnswer = ask(&svc, &ctx, "todo.estimate", move |vault, qctx| {
        let task = vault.task(id)?;
        Ok(quick::todo_estimate(qctx, &task.title, &comparable(vault, id)?))
    })
    .await?;
    answer.clamp();
    Ok(answer.minutes)
}

/// Finished tasks with a recorded estimate, newest first.
///
/// What the model is given instead of its own sense of how long work takes.
/// Their *estimates* rather than their time blocks, because an estimate is
/// what this job is being asked to produce and comparing like with like is
/// the point — a task that overran is still evidence about how this person
/// sizes things.
fn comparable(vault: &Vault, exclude: TaskId) -> CommandResult<Vec<(String, u32)>> {
    let tasks = vault.tasks(&TaskQuery {
        sort: TaskSort::CreatedDesc,
        limit: Some(200),
        ..Default::default()
    })?;
    Ok(tasks
        .into_iter()
        .filter(|t| t.id != exclude)
        .filter_map(|t| t.estimate_minutes.map(|m| (t.title, m)))
        .take(12)
        .collect())
}

// ── Calendar ─────────────────────────────────────────────────────────────

/// C1 — an appointment from a sentence.
async fn quick_event_from_line(
    svc: Arc<Service>,
    ctx: Ctx,
    args: Line,
) -> CommandResult<Option<quick::EventDraft>> {
    let line = args.line;
    let mut draft: quick::EventDraft =
        ask(&svc, &ctx, "calendar.parse", move |_, qctx| Ok(quick::calendar_parse(qctx, &line)))
            .await?;
    Ok(draft.clamp().then_some(draft))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawTitle {
    pub title: String,
}

/// C2 — a readable title for a subscribed event.
///
/// Display only: nothing here writes to the event, because the feed will be
/// re-fetched and the record is not ours.
async fn quick_event_title(svc: Arc<Service>, ctx: Ctx, args: RawTitle) -> CommandResult<String> {
    let raw = args.title;
    let answer: quick::TextAnswer =
        ask(&svc, &ctx, "calendar.title", move |_, qctx| Ok(quick::calendar_title(qctx, &raw)))
            .await?;
    Ok(answer.text.trim().to_string())
}

// ── Journal ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryRef {
    pub entry_id: EntryId,
    #[serde(default)]
    pub journal_id: Option<JournalId>,
}

/// J1 — the numbers a day's writing states.
async fn quick_entry_readings(
    svc: Arc<Service>,
    ctx: Ctx,
    args: EntryRef,
) -> CommandResult<Vec<Reading>> {
    let vault = svc.require()?;
    let trackers = {
        let vault = vault.clone();
        blocking(move || Ok(vault.trackers()?)).await?
    };
    let id = args.entry_id;
    let list = trackers.clone();
    let mut answer: quick::ReadingsAnswer =
        ask(&svc, &ctx, "journal.readings", move |vault, qctx| {
            let entry = vault.entry(id)?;
            Ok(quick::journal_readings(qctx, &entry.body.plain_text(), &list))
        })
        .await?;
    answer.clamp();
    Ok(readings_from(&trackers, answer))
}

/// J2 — a title for a day.
async fn quick_entry_title(svc: Arc<Service>, ctx: Ctx, args: EntryRef) -> CommandResult<String> {
    let id = args.entry_id;
    let answer: quick::TextAnswer = ask(&svc, &ctx, "journal.title", move |vault, qctx| {
        let entry = vault.entry(id)?;
        Ok(quick::journal_title(qctx, &entry.body.plain_text()))
    })
    .await?;
    Ok(answer.text.trim().to_string())
}

/// J3 — tags and a purpose for a day.
async fn quick_entry_labels(svc: Arc<Service>, ctx: Ctx, args: EntryRef) -> CommandResult<Labels> {
    let vault = svc.require()?;
    let (roles, goals) = {
        let vault = vault.clone();
        blocking(move || purposes(&vault)).await?
    };
    let (r, g, id) = (roles.clone(), goals.clone(), args.entry_id);
    let mut answer: quick::LabelsAnswer = ask(&svc, &ctx, "journal.labels", move |vault, qctx| {
        let entry = vault.entry(id)?;
        let tags = known_tags(vault.entry_tags());
        Ok(quick::journal_labels(qctx, &entry.body.plain_text(), &r, &g, &tags))
    })
    .await?;
    let existing = {
        let vault = svc.require()?;
        blocking(move || Ok(vault.entry(id)?.tags)).await?
    };
    answer.clamp(&existing);
    Ok(Labels { purpose: purpose_at(&roles, &goals, answer.purpose), tags: answer.tags })
}

// ── Notes ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteRef {
    pub note_id: NoteId,
}

/// N1 — a title for a note.
async fn quick_note_title(svc: Arc<Service>, ctx: Ctx, args: NoteRef) -> CommandResult<String> {
    let id = args.note_id;
    let answer: quick::TextAnswer = ask(&svc, &ctx, "notes.title", move |vault, qctx| {
        let note = vault.note(id)?;
        Ok(quick::notes_title(qctx, &note.body.plain_text()))
    })
    .await?;
    Ok(answer.text.trim().to_string())
}

/// N2 — the tasks buried in a page of notes.
async fn quick_note_tasks(
    svc: Arc<Service>,
    ctx: Ctx,
    args: NoteRef,
) -> CommandResult<Vec<quick::TaskDraft>> {
    let id = args.note_id;
    let mut answer: quick::TasksAnswer = ask(&svc, &ctx, "notes.tasks", move |vault, qctx| {
        let note = vault.note(id)?;
        Ok(quick::notes_tasks(qctx, &note.title, &note.body.plain_text()))
    })
    .await?;
    answer.clamp();
    Ok(answer.tasks)
}

/// N3 — tags and a purpose for a note.
async fn quick_note_labels(svc: Arc<Service>, ctx: Ctx, args: NoteRef) -> CommandResult<Labels> {
    let vault = svc.require()?;
    let (roles, goals) = {
        let vault = vault.clone();
        blocking(move || purposes(&vault)).await?
    };
    let (r, g, id) = (roles.clone(), goals.clone(), args.note_id);
    let mut answer: quick::LabelsAnswer = ask(&svc, &ctx, "notes.labels", move |vault, qctx| {
        let note = vault.note(id)?;
        let tags = known_tags(vault.note_tags());
        Ok(quick::notes_labels(qctx, &note.body.plain_text(), &r, &g, &tags))
    })
    .await?;
    let existing = {
        let vault = svc.require()?;
        blocking(move || Ok(vault.note(id)?.tags)).await?
    };
    answer.clamp(&existing);
    Ok(Labels { purpose: purpose_at(&roles, &goals, answer.purpose), tags: answer.tags })
}

// ── Tracking ─────────────────────────────────────────────────────────────

/// K1 — a reading from a sentence.
async fn quick_reading_from_line(
    svc: Arc<Service>,
    ctx: Ctx,
    args: Line,
) -> CommandResult<Option<Reading>> {
    let vault = svc.require()?;
    let trackers = {
        let vault = vault.clone();
        blocking(move || Ok(vault.trackers()?)).await?
    };
    let (list, line) = (trackers.clone(), args.line);
    let mut draft: quick::ReadingDraft = ask(&svc, &ctx, "tracker.parse", move |_, qctx| {
        Ok(quick::tracker_parse(qctx, &line, &list))
    })
    .await?;
    if !draft.clamp() {
        return Ok(None);
    }
    let answer = quick::ReadingsAnswer { readings: vec![draft] };
    Ok(readings_from(&trackers, answer).into_iter().next())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackerFrom {
    pub name: String,
    #[serde(default)]
    pub line: String,
}

/// K2 — how a tracker made by the act of recording should be set up.
async fn quick_tracker_draft(
    svc: Arc<Service>,
    ctx: Ctx,
    args: TrackerFrom,
) -> CommandResult<Option<quick::TrackerDraft>> {
    let TrackerFrom { name, line } = args;
    let mut draft: quick::TrackerDraft = ask(&svc, &ctx, "tracker.draft", move |_, qctx| {
        Ok(quick::tracker_draft(qctx, &name, &line))
    })
    .await?;
    Ok(draft.clamp().then_some(draft))
}

// ── Roles and goals ──────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalWording {
    pub title: String,
    pub role_id: RoleId,
}

/// P1 — a goal phrased as an outcome.
async fn quick_goal_wording(
    svc: Arc<Service>,
    ctx: Ctx,
    args: GoalWording,
) -> CommandResult<String> {
    let GoalWording { title, role_id } = args;
    let original = title.clone();
    let answer: quick::TextAnswer = ask(&svc, &ctx, "purpose.goal", move |vault, qctx| {
        let role = vault.roles()?.into_iter().find(|r| r.id == role_id);
        let name = role.map(|r| r.name).unwrap_or_default();
        Ok(quick::purpose_goal(qctx, &title, &name))
    })
    .await?;
    // An answer identical to what was typed is not a suggestion.
    let text = answer.text.trim().to_string();
    Ok(if text.eq_ignore_ascii_case(original.trim()) { String::new() } else { text })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalRef {
    pub goal_id: GoalId,
}

/// One record a new goal might cover.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackfillPick {
    pub task_id: TaskId,
    pub title: String,
}

/// P2 — which existing tasks serve a goal that has just been written.
///
/// Tasks only, and on purpose: they are the records with a title short enough
/// to classify a hundred of at once, and the ones whose missing purpose most
/// distorts the balance report. Nothing here writes — the answer is a list
/// with checkboxes.
async fn quick_goal_backfill(
    svc: Arc<Service>,
    ctx: Ctx,
    args: GoalRef,
) -> CommandResult<Vec<BackfillPick>> {
    let vault = svc.require()?;
    let id = args.goal_id;
    let (goal_title, candidates) = {
        let vault = vault.clone();
        blocking(move || {
            let goal = vault
                .goals(&GoalQuery::default())?
                .into_iter()
                .find(|g| g.id == id)
                .ok_or_else(|| CommandError::new("not_found", "no such goal"))?;
            let tasks = vault.tasks(&TaskQuery { limit: Some(120), ..Default::default() })?;
            let open: Vec<(TaskId, String)> = tasks
                .into_iter()
                .filter(|t| t.purpose.is_none())
                .map(|t| (t.id, t.title))
                .collect();
            Ok((goal.title, open))
        })
        .await?
    };
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let titles: Vec<String> = candidates.iter().map(|(_, t)| t.clone()).collect();
    let list = titles.clone();
    let answer: quick::LabelsAnswer = ask(&svc, &ctx, "purpose.backfill", move |_, qctx| {
        Ok(quick::purpose_backfill(qctx, &goal_title, &list))
    })
    .await?;
    Ok(quick::backfill_picks(&answer, candidates.len())
        .into_iter()
        .filter_map(|i| candidates.get(i))
        .map(|(id, title)| BackfillPick { task_id: *id, title: title.clone() })
        .collect())
}

// ── Overview ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeekTotals {
    /// Rendered by the interface, which is the side that already has the
    /// week's figures on screen. Sending them back as prose rather than
    /// re-deriving them here keeps the card and the sentence describing it
    /// from ever disagreeing.
    pub this_week: String,
    #[serde(default)]
    pub last_week: String,
}

/// O1 — the week, in a sentence or two.
async fn quick_week_note(svc: Arc<Service>, ctx: Ctx, args: WeekTotals) -> CommandResult<String> {
    let WeekTotals { this_week, last_week } = args;
    let answer: quick::TextAnswer = ask(&svc, &ctx, "overview.week", move |_, qctx| {
        Ok(quick::overview_week(qctx, &this_week, &last_week))
    })
    .await?;
    Ok(answer.text.trim().to_string())
}

// ── Data ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrontMatter {
    pub ours: Vec<String>,
    pub theirs: Vec<String>,
}

/// X1 — map somebody else's front matter onto ours.
async fn quick_front_matter(
    svc: Arc<Service>,
    ctx: Ctx,
    args: FrontMatter,
) -> CommandResult<quick::MappingAnswer> {
    let FrontMatter { ours, theirs } = args;
    let (o, t) = (ours.clone(), theirs.clone());
    let mut answer: quick::MappingAnswer =
        ask(&svc, &ctx, "data.import_map", move |_, qctx| Ok(quick::data_import_map(qctx, &o, &t)))
            .await?;
    answer.clamp(&ours, &theirs);
    Ok(answer)
}

/// Every quick command.
///
/// All of them `Read`: nothing in this module writes a record. What they
/// spend is money rather than data, which is what [`Scope::Quick`] is for and
/// why it is not [`Scope::Agent`].
pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "quick_jobs", scope: Agent, effect: Read,
        args: Nothing, returns: "QuickJobRow[]", signature: &[],
        run: quick_jobs,
    },
    command! {
        name: "set_quick_job", scope: Agent, effect: Write,
        change: Settings / Updated,
        args: SetJob, returns: "QuickJobRow[]",
        signature: &[("name", "string", true), ("on", "boolean", true)],
        run: set_quick_job,
    },
    command! {
        name: "quick_item_fields", scope: Quick, effect: Read,
        args: ItemRef, returns: "QuickFields",
        signature: &[("itemId", "ItemId", true)],
        run: quick_item_fields,
    },
    command! {
        name: "quick_pick_result", scope: Quick, effect: Read,
        args: PickArgs, returns: "number | null",
        signature: &[
            ("kindId", "KindId", true),
            ("query", "string", true),
            ("results", "SearchResult[]", true),
        ],
        run: quick_pick_result,
    },
    command! {
        name: "quick_kind_draft", scope: Quick, effect: Read,
        args: KindName, returns: "QuickKindDraft",
        signature: &[("name", "string", true)],
        run: quick_kind_draft,
    },
    command! {
        name: "quick_import_columns", scope: Quick, effect: Read,
        args: ImportColumns, returns: "QuickMapping",
        signature: &[
            ("kindId", "KindId", true),
            ("columns", "string[]", true),
            ("sample", "string[]", false),
        ],
        run: quick_import_columns,
    },
    command! {
        name: "quick_task_labels", scope: Quick, effect: Read,
        args: Title, returns: "QuickLabels",
        signature: &[("title", "string", true)],
        run: quick_task_labels,
    },
    command! {
        name: "quick_task_from_line", scope: Quick, effect: Read,
        args: Line, returns: "QuickTaskDraft | null",
        signature: &[("line", "string", true)],
        run: quick_task_from_line,
    },
    command! {
        name: "quick_subtasks", scope: Quick, effect: Read,
        args: TaskRef, returns: "QuickTaskDraft[]",
        signature: &[("taskId", "TaskId", true)],
        run: quick_subtasks,
    },
    command! {
        name: "quick_estimate", scope: Quick, effect: Read,
        args: TaskRef, returns: "number | null",
        signature: &[("taskId", "TaskId", true)],
        run: quick_estimate,
    },
    command! {
        name: "quick_event_from_line", scope: Quick, effect: Read,
        args: Line, returns: "QuickEventDraft | null",
        signature: &[("line", "string", true)],
        run: quick_event_from_line,
    },
    command! {
        name: "quick_event_title", scope: Quick, effect: Read,
        args: RawTitle, returns: "string",
        signature: &[("title", "string", true)],
        run: quick_event_title,
    },
    command! {
        name: "quick_entry_readings", scope: Quick, effect: Read,
        args: EntryRef, returns: "QuickReading[]",
        signature: &[("entryId", "EntryId", true), ("journalId", "JournalId | null", false)],
        run: quick_entry_readings,
    },
    command! {
        name: "quick_entry_title", scope: Quick, effect: Read,
        args: EntryRef, returns: "string",
        signature: &[("entryId", "EntryId", true), ("journalId", "JournalId | null", false)],
        run: quick_entry_title,
    },
    command! {
        name: "quick_entry_labels", scope: Quick, effect: Read,
        args: EntryRef, returns: "QuickLabels",
        signature: &[("entryId", "EntryId", true), ("journalId", "JournalId | null", false)],
        run: quick_entry_labels,
    },
    command! {
        name: "quick_note_title", scope: Quick, effect: Read,
        args: NoteRef, returns: "string",
        signature: &[("noteId", "NoteId", true)],
        run: quick_note_title,
    },
    command! {
        name: "quick_note_tasks", scope: Quick, effect: Read,
        args: NoteRef, returns: "QuickTaskDraft[]",
        signature: &[("noteId", "NoteId", true)],
        run: quick_note_tasks,
    },
    command! {
        name: "quick_note_labels", scope: Quick, effect: Read,
        args: NoteRef, returns: "QuickLabels",
        signature: &[("noteId", "NoteId", true)],
        run: quick_note_labels,
    },
    command! {
        name: "quick_reading_from_line", scope: Quick, effect: Read,
        args: Line, returns: "QuickReading | null",
        signature: &[("line", "string", true)],
        run: quick_reading_from_line,
    },
    command! {
        name: "quick_tracker_draft", scope: Quick, effect: Read,
        args: TrackerFrom, returns: "QuickTrackerDraft | null",
        signature: &[("name", "string", true), ("line", "string", false)],
        run: quick_tracker_draft,
    },
    command! {
        name: "quick_goal_wording", scope: Quick, effect: Read,
        args: GoalWording, returns: "string",
        signature: &[("title", "string", true), ("roleId", "RoleId", true)],
        run: quick_goal_wording,
    },
    command! {
        name: "quick_goal_backfill", scope: Quick, effect: Read,
        args: GoalRef, returns: "QuickBackfillPick[]",
        signature: &[("goalId", "GoalId", true)],
        run: quick_goal_backfill,
    },
    command! {
        name: "quick_week_note", scope: Quick, effect: Read,
        args: WeekTotals, returns: "string",
        signature: &[("thisWeek", "string", true), ("lastWeek", "string", false)],
        run: quick_week_note,
    },
    command! {
        name: "quick_front_matter", scope: Quick, effect: Read,
        args: FrontMatter, returns: "QuickMapping",
        signature: &[("ours", "string[]", true), ("theirs", "string[]", true)],
        run: quick_front_matter,
    },
];
