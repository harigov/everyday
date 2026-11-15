//! Health: every command the Health app, the phone's push and the import
//! are reached through. See `docs/plans/health.md` and the command table in
//! its "Contract" section.
//!
//! Thin, as Money's and Screen time's are: the vault checks and files; this
//! decides only what the edge can decide cheaply -- sizes, ranges -- and
//! which scope reaches what. `record_health_push` is the one command under
//! [`Scope::HealthIngest`](crate::ctx::Scope::HealthIngest); setting or
//! clearing the food-lookup keys needs `all`; everything else wants
//! [`Scope::Health`](crate::ctx::Scope::Health).
//!
//! # Two scopes that do not imply each other
//!
//! A `healthIngest` token is what Health Auto Export keeps on a phone, so it
//! is the token most likely to leak. It can push readings and nothing else:
//! not one report, not one medication, not the list of sources. The other
//! way round, a client allowed to read somebody's health has no business
//! posing as their phone, so `health` does not reach `record_health_push`.
//!
//! # A phone is told `locked`, not something vaguer
//!
//! The push wants an unlocked vault and says so with the `locked` code before
//! the vault is asked anything: samples are sealed like everything else, so a
//! locked vault could not take them, and a phone that hears `locked` knows
//! its next "Previous 7 Days" run brings the same data again. The route in
//! `karmayoga-server` turns that code into `409`.
//!
//! # What the edge refuses
//!
//! A range longer than [`MAX_RANGE_DAYS`], a few days of raw samples past
//! three, more than [`MAX_METRICS`] metrics at once, a time zone past
//! [`MAX_TZ`] characters, a search past [`MAX_QUERY`], a barcode that is not
//! eight to fourteen digits: sizes no screen of this application sends, so
//! anything past them is somebody else's client, and is told so before the
//! vault reads it.
//!
//! # Nothing answers a credential
//!
//! `health_keys` says whether the food-lookup key is set and its last four
//! characters. No command, under any scope, answers the key; the commands
//! that call a food database read it inside the service.
//!
//! # What a workout answers without
//!
//! A workout's `external_id` -- the source's own id for it -- is sealed and
//! blanked in everything a command answers, as Money blanks a transaction's
//! provider id: nothing on screen draws it and a client has no use for it.
//!
//! # Cascades
//!
//! A command declares the one kind it writes. A few write beside it -- a dose
//! comes off a medication's supply, deleting a medication deletes its doses,
//! a template creates trackers -- and those are announced too, from what the
//! vault says it touched, so a window showing the medication or the Overview
//! does not go on drawing the old one.

use crate::command;
use crate::ctx::{Caller, Ctx, Scope};
use crate::error::{CommandError, CommandResult, codes};
use crate::events::{Kind, Op};
use crate::health::{brief, extract, food, import, photo};
use crate::service::{Service, blocking};
use jiff::civil::Date;
use jiff::{SignedDuration, Timestamp};
use karmayoga_core::Vault;
use karmayoga_core::error::Error;
use karmayoga_core::health::connect::hae;
use karmayoga_core::health::metric::{self, MetricEntry};
use karmayoga_core::health::{
    ActivityDay, Adherence, Allergy, Condition, ConditionReport, DayTimeline, DocKind, Dose,
    DueDose, Food, FoodHit, GlucoseReport, HealthDay, HealthDocument, HealthIngestReport,
    HealthKeys, HealthKeysInfo, HealthSettings, HealthSource, ImportStatus, LabResult, LabTrend,
    Latest, MAX_RANGE_DAYS, ManualReading, Meal, MealItem, MealResponse, Medication, Metric,
    NutritionDay, PushHeaders, RuleAlert, SamplePoint, SeriesQuery, SeriesResult, SleepNight,
    TemplateChoice, TemplateInfo, TemplateKey, VisitBrief, Workout, WorkoutDetail,
};
use karmayoga_core::id::{
    AllergyId, ConditionId, DoseId, FileId, FoodId, HealthDocumentId, HealthSourceId, LabResultId,
    MealId, MedicationId, WorkoutId,
};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

use super::Nothing;

/// Longest time zone name taken. The longest IANA name is about thirty
/// characters.
pub const MAX_TZ: usize = 64;

/// Longest search text, filter or lab key taken. Nobody types more into a
/// food search; a lab key is a LOINC code or a short name.
pub const MAX_QUERY: usize = 200;

/// Most metrics one `health_latest` names. The catalogue is forty-odd, and a
/// screen asks for the dozen on its tiles.
pub const MAX_METRICS: usize = 50;

/// Most days of raw samples one `health_samples` answers. A heart rate day
/// is tens of thousands of points; beyond a few the screen wants buckets.
pub const MAX_SAMPLE_DAYS: i64 = 3;

/// Most pages one document is made of. A scanned file is a handful; a
/// hundred is a book.
pub const MAX_FILES: usize = 100;

/// Most lab results accepted from one document in one go.
pub const MAX_LABS: usize = 500;

/// Longest document title taken.
pub const MAX_TITLE: usize = 300;

/// Longest text read out of a document that is taken to be extracted from.
/// A PDF's text layer is kilobytes; two million characters is a textbook.
pub const MAX_DOCUMENT_TEXT: usize = 2_000_000;

/// Longest key taken for a food database.
pub const MAX_KEY: usize = 200;

/// Longest file path taken. The platforms' own limit is about this.
pub const MAX_PATH: usize = 4096;

// ---- arguments -------------------------------------------------------------

/// A command whose only argument is the id of what it acts on.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ref<T> {
    pub id: T,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveSettings {
    pub settings: HealthSettings,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveSource {
    pub source: HealthSource,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Push {
    pub source_id: HealthSourceId,
    pub headers: PushHeaders,
    /// What the phone posted, already parsed as JSON.
    pub body: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceToken {
    pub source_id: HealthSourceId,
    /// The registry id of the token minted for the source; empty clears it.
    pub token_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportApple {
    pub path: String,
    #[serde(default)]
    pub source_id: Option<HealthSourceId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportAppleFile {
    /// An `export.zip` in the files catalog.
    pub file: FileId,
    #[serde(default)]
    pub source_id: Option<HealthSourceId>,
    /// Put the file in the Files trash once the import has ended, however it
    /// ended: for an export uploaded only to be read.
    #[serde(default)]
    pub trash_after: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FdcKeys {
    pub fdc_key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Day {
    pub date: Date,
    /// An IANA zone; empty is the system's.
    pub tz: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Between {
    pub from: Date,
    pub to: Date,
    /// An IANA zone; empty is the system's.
    pub tz: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Instants {
    pub from: Timestamp,
    pub to: Timestamp,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Series {
    pub query: SeriesQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Samples {
    pub metric: Metric,
    pub from: Timestamp,
    pub to: Timestamp,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatestOf {
    pub metrics: Vec<Metric>,
    pub on: Date,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reading {
    pub reading: ManualReading,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteSamples {
    pub metric: Metric,
    pub from: Timestamp,
    pub to: Timestamp,
    /// Only this device's or app's samples. `None` is every source's.
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveWorkout {
    pub workout: Workout,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveMedication {
    pub medication: Medication,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoseRange {
    pub from: Timestamp,
    pub to: Timestamp,
    #[serde(default)]
    pub medication_id: Option<MedicationId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveDose {
    pub dose: Dose,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdherenceOf {
    #[serde(default)]
    pub medication_id: Option<MedicationId>,
    pub from: Date,
    pub to: Date,
    pub tz: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveAllergy {
    pub allergy: Allergy,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveCondition {
    pub condition: Condition,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteCondition {
    pub id: ConditionId,
    /// Also delete the trackers its care plan created.
    pub remove_trackers: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyTemplate {
    pub template: TemplateKey,
    pub choice: TemplateChoice,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConditionAt {
    pub id: ConditionId,
    pub date: Date,
    pub tz: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Labs {
    /// One test's results, by LOINC code or `name:<name>`. `None` is all.
    #[serde(default)]
    pub key: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveLab {
    pub lab: LabResult,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddDocument {
    pub title: String,
    pub kind: DocKind,
    #[serde(default)]
    pub date: Option<Date>,
    /// Files already in the files catalog; the interface adds them through
    /// Files first, sealed, as Health's.
    pub files: Vec<FileId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveDocument {
    pub document: HealthDocument,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteDocument {
    pub id: HealthDocumentId,
    /// Also delete the results accepted from it.
    pub delete_labs: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractDocument {
    pub id: HealthDocumentId,
    /// The PDF's text layer, when the interface could read one.
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptLabs {
    pub id: HealthDocumentId,
    pub labs: Vec<LabResult>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Query {
    pub query: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Barcode {
    pub code: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveFood {
    pub food: Food,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveMeal {
    pub meal: Meal,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Photo {
    /// A picture in the files catalog.
    pub photo: FileId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Brief {
    #[serde(default)]
    pub condition_id: Option<ConditionId>,
    pub since: Date,
    pub tz: String,
}

// ---- the catalogue and the settings ----------------------------------------

/// Every metric Health keeps, with its unit and what kind it is. Needs no
/// vault: it is the code's own catalogue.
async fn health_catalog(
    _svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<MetricEntry>> {
    Ok(metric::catalog())
}

async fn health_settings(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<HealthSettings> {
    svc.on_vault(|vault| vault.health_settings()).await
}

async fn save_health_settings(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SaveSettings,
) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_health_settings(&args.settings)).await
}

// ---- sources ---------------------------------------------------------------

async fn list_health_sources(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<HealthSource>> {
    svc.on_vault(|vault| vault.health_sources()).await
}

async fn save_health_source(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SaveSource,
) -> CommandResult<HealthSource> {
    svc.on_vault(move |vault| vault.save_health_source(&args.source)).await
}

/// Delete the connection. What it brought stays.
async fn delete_health_source(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Ref<HealthSourceId>,
) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_health_source(args.id)).await
}

// ---- the phone's push ------------------------------------------------------

/// Take one push from Health Auto Export: parsed, then filed.
///
/// A body that is not a push is noted on its source -- so Settings can say
/// the phone is sending something this cannot read, rather than that it has
/// gone quiet -- and answered `invalid`. A source that does not exist is
/// `not_found` either way.
async fn record_health_push(
    svc: Arc<Service>,
    ctx: Ctx,
    args: Push,
) -> CommandResult<HealthIngestReport> {
    let vault = svc.require_unlocked()?;
    let Push { source_id, headers, body } = args;
    blocking(move || {
        bound(&vault, source_id, &ctx.caller)?;
        push(&vault, source_id, &headers, &body)
    })
    .await
}

/// Is `caller` the token this source was set up with? A phone's token is
/// minted for one source (`health_push_setup` in the shell) and is good for
/// that one only: a leaked token cannot write into another phone's source,
/// and one whose source was deleted or set up again opens nothing. The
/// owner's own process -- this window, the CLI socket -- is not a token and
/// may push anywhere, as it may do anything else.
fn bound(vault: &Vault, source: HealthSourceId, caller: &Caller) -> CommandResult<()> {
    let Caller::Device(token) = caller else { return Ok(()) };
    let held = vault.health_source(source)?;
    if held.token_id.is_empty() || held.token_id != *token {
        return Err(CommandError::new(
            codes::FORBIDDEN,
            "this token was not issued for this source; set the phone up again in Settings",
        ));
    }
    Ok(())
}

/// Bind a source to the token minted for its phone, or clear the binding.
/// Only from this computer, where the shell mints the token: a paired
/// device binding a token of its choosing would defeat the binding.
async fn set_health_source_token(
    svc: Arc<Service>,
    ctx: Ctx,
    args: SourceToken,
) -> CommandResult<()> {
    if !matches!(ctx.caller, Caller::Local | Caller::Window(_) | Caller::Socket) {
        return Err(CommandError::new(
            codes::FORBIDDEN,
            "a source's phone token is set up on the computer that holds the vault",
        ));
    }
    if args.token_id.chars().count() > 200 {
        return Err(CommandError::new(codes::INVALID, "that is not a token id"));
    }
    svc.on_vault(move |vault| vault.set_health_source_token(args.source_id, &args.token_id)).await
}

fn push(
    vault: &Vault,
    source: HealthSourceId,
    headers: &PushHeaders,
    body: &serde_json::Value,
) -> CommandResult<HealthIngestReport> {
    match hae::parse(body, headers) {
        Ok(batch) => Ok(vault.ingest_health(source, batch, Some(headers))?),
        Err(e) => {
            let message = match e {
                Error::Invalid(why) => why,
                other => other.to_string(),
            };
            match vault.record_health_push_failure(source, &message) {
                Ok(()) => {}
                Err(e @ Error::NotFound { .. }) => return Err(e.into()),
                // Noting it is a courtesy: the phone is told what was wrong
                // with its push whether or not it could be written down.
                Err(e) => tracing::warn!(code = e.code(), "could not note a refused health push"),
            }
            Err(CommandError::new(codes::INVALID, message))
        }
    }
}

// ---- the export import -----------------------------------------------------

/// Start importing an Apple Health `export.zip`. Answers at once, with the
/// import running; `apple_import_status` is how it is followed.
///
/// Only from this computer: the path is read here, so a paired device's
/// would name a file on some other machine, or -- worse -- on this one.
async fn import_apple_health(
    svc: Arc<Service>,
    ctx: Ctx,
    args: ImportApple,
) -> CommandResult<ImportStatus> {
    if !matches!(ctx.caller, Caller::Local | Caller::Window(_) | Caller::Socket) {
        return Err(CommandError::new(
            codes::FORBIDDEN,
            "an Apple Health export is read from a file on this computer, so it can only be \
             started from here",
        ));
    }
    let ImportApple { path, source_id } = args;
    let path = path.trim();
    if path.is_empty() || path.len() > MAX_PATH {
        return Err(CommandError::new(codes::INVALID, "that is not a file"));
    }
    let path = PathBuf::from(path);
    let vault = svc.require_unlocked()?;
    blocking(move || import::start(&svc, vault, path, source_id, None)).await
}

/// Start importing an Apple Health `export.zip` that is in the files
/// catalog: one a sync tool brought into the folder from the phone, or one
/// uploaded through Files from a window on another machine -- in pieces,
/// so an export of a gigabyte crosses no webview whole. Answers at once, as
/// `import_apple_health` does.
///
/// Any caller holding Health and Files may: the file is the vault's, read
/// where it is, and nothing of it is answered. A sealed one is read from a
/// decrypted copy in the folder's private place, deleted the moment the
/// import ends.
async fn import_apple_health_file(
    svc: Arc<Service>,
    ctx: Ctx,
    args: ImportAppleFile,
) -> CommandResult<ImportStatus> {
    ctx.require(Scope::Files)?;
    let ImportAppleFile { file, source_id, trash_after } = args;
    let vault = svc.require_unlocked()?;
    blocking(move || {
        let entry = vault.file(file)?;
        if entry.status.is_trashed() {
            return Err(CommandError::new(
                codes::INVALID,
                "that file is in the trash; restore it in Files first",
            ));
        }
        if !entry.name.to_ascii_lowercase().ends_with(".zip") {
            return Err(CommandError::new(
                codes::INVALID,
                "an Apple Health export is a .zip file (export.zip)",
            ));
        }
        // Asked before a sealed export is decrypted, which can be gigabytes
        // of somebody's health written out in the clear only to be refused.
        if import::status().state == "running" {
            return Err(CommandError::new(
                codes::BUSY,
                "an Apple Health import is already running",
            ));
        }
        let path = vault.open_file_copy(file)?;
        let decrypted = entry.is_sealed().then(|| path.clone());
        let forget = |copy: &Option<PathBuf>| {
            if let Some(copy) = copy
                && let Err(e) = std::fs::remove_file(copy)
            {
                tracing::warn!(error = %e, "could not delete an export's decrypted copy");
            }
        };
        let finished: import::Finished = {
            let decrypted = decrypted.clone();
            Box::new(move |vault: &Vault, _done: bool| {
                forget(&decrypted);
                // However it ended: an export uploaded only to be read is
                // done with once it has been, and the trash still gives it
                // back to try again.
                if trash_after && let Err(e) = vault.trash_files(&[file]) {
                    tracing::warn!(
                        code = e.code(),
                        "could not move an imported export to the trash"
                    );
                }
            })
        };
        // Refused after all -- a source that takes no export, a thread that
        // would not start -- the decrypted copy goes at once.
        import::start(&svc, vault, path, source_id, Some(finished))
            .inspect_err(|_| forget(&decrypted))
    })
    .await
}

async fn apple_import_status(
    _svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<ImportStatus> {
    Ok(import::status())
}

/// Stop the running import at its next batch. Answers whether one was
/// running.
async fn cancel_apple_import(_svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<bool> {
    Ok(import::cancel())
}

// ---- the food-lookup key ---------------------------------------------------

async fn health_keys(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<HealthKeysInfo> {
    svc.on_vault(|vault| vault.health_keys_info()).await
}

/// Store the FoodData Central key. Never answers it.
async fn save_health_keys(svc: Arc<Service>, _ctx: Ctx, args: FdcKeys) -> CommandResult<()> {
    let key = args.fdc_key.trim().to_string();
    if key.is_empty() || key.chars().count() > MAX_KEY {
        return Err(CommandError::new(codes::INVALID, "that is not a FoodData Central key"));
    }
    svc.on_vault(move |vault| vault.save_health_keys(&HealthKeys { fdc_key: key })).await
}

async fn clear_health_keys(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<()> {
    svc.on_vault(|vault| vault.clear_health_keys()).await
}

// ---- samples and reports ---------------------------------------------------

async fn health_today(svc: Arc<Service>, _ctx: Ctx, args: Day) -> CommandResult<HealthDay> {
    time_zone(&args.tz)?;
    svc.on_vault(move |vault| vault.health_today(args.date, &args.tz).map(scrub_day)).await
}

async fn health_series(svc: Arc<Service>, _ctx: Ctx, args: Series) -> CommandResult<SeriesResult> {
    let query = args.query;
    time_zone(&query.tz)?;
    dates(query.from, query.to)?;
    svc.on_vault(move |vault| vault.health_series(&query)).await
}

async fn health_samples(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Samples,
) -> CommandResult<Vec<SamplePoint>> {
    let Samples { metric, from, to } = args;
    instants(from, to, MAX_SAMPLE_DAYS)?;
    svc.on_vault(move |vault| vault.health_samples(metric, from, to)).await
}

async fn health_latest(svc: Arc<Service>, _ctx: Ctx, args: LatestOf) -> CommandResult<Vec<Latest>> {
    if args.metrics.len() > MAX_METRICS {
        return Err(CommandError::new(
            codes::INVALID,
            format!("at most {MAX_METRICS} metrics at once"),
        ));
    }
    svc.on_vault(move |vault| vault.health_latest(&args.metrics, args.on)).await
}

/// A reading typed in here -- a weight, a fingerstick, a blood pressure.
async fn add_health_reading(svc: Arc<Service>, _ctx: Ctx, args: Reading) -> CommandResult<()> {
    time_zone(&args.reading.tz)?;
    svc.on_vault(move |vault| vault.add_health_reading(&args.reading)).await
}

/// Delete one metric's samples in a range, and only one source's if asked.
/// Answers how many went.
async fn delete_health_samples(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: DeleteSamples,
) -> CommandResult<u64> {
    let DeleteSamples { metric, from, to, source } = args;
    if from > to {
        return Err(CommandError::new(codes::INVALID, "the start comes after the end"));
    }
    if let Some(source) = &source {
        query_text(source)?;
    }
    svc.on_vault(move |vault| vault.delete_health_samples(metric, from, to, source.as_deref()))
        .await
}

async fn health_activity(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Between,
) -> CommandResult<Vec<ActivityDay>> {
    between(&args)?;
    svc.on_vault(move |vault| vault.health_activity(args.from, args.to, &args.tz)).await
}

async fn health_sleep(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Between,
) -> CommandResult<Vec<SleepNight>> {
    between(&args)?;
    svc.on_vault(move |vault| vault.health_sleep(args.from, args.to, &args.tz)).await
}

async fn health_glucose(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Between,
) -> CommandResult<GlucoseReport> {
    between(&args)?;
    svc.on_vault(move |vault| vault.health_glucose(args.from, args.to, &args.tz)).await
}

async fn health_nutrition(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Between,
) -> CommandResult<Vec<NutritionDay>> {
    between(&args)?;
    svc.on_vault(move |vault| vault.health_nutrition(args.from, args.to, &args.tz)).await
}

async fn health_day_timeline(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Day,
) -> CommandResult<DayTimeline> {
    time_zone(&args.tz)?;
    svc.on_vault(move |vault| vault.health_day_timeline(args.date, &args.tz).map(scrub_timeline))
        .await
}

// ---- workouts --------------------------------------------------------------

async fn list_workouts(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Instants,
) -> CommandResult<Vec<Workout>> {
    instants(args.from, args.to, MAX_RANGE_DAYS)?;
    svc.on_vault(move |vault| {
        Ok(vault.list_workouts(args.from, args.to)?.into_iter().map(scrub_workout).collect())
    })
    .await
}

async fn get_workout(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Ref<WorkoutId>,
) -> CommandResult<WorkoutDetail> {
    svc.on_vault(move |vault| {
        let mut detail = vault.workout_detail(args.id)?;
        detail.workout = scrub_workout(detail.workout);
        Ok(detail)
    })
    .await
}

async fn save_workout(svc: Arc<Service>, _ctx: Ctx, args: SaveWorkout) -> CommandResult<Workout> {
    svc.on_vault(move |vault| vault.save_workout(&args.workout).map(scrub_workout)).await
}

async fn delete_workout(svc: Arc<Service>, _ctx: Ctx, args: Ref<WorkoutId>) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_workout(args.id)).await
}

// ---- medications and doses -------------------------------------------------

async fn list_medications(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<Medication>> {
    svc.on_vault(|vault| vault.list_medications()).await
}

async fn save_medication(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SaveMedication,
) -> CommandResult<Medication> {
    svc.on_vault(move |vault| vault.save_medication(&args.medication)).await
}

/// Delete a medication and every dose logged against it.
async fn delete_medication(
    svc: Arc<Service>,
    ctx: Ctx,
    args: Ref<MedicationId>,
) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_medication(args.id)).await?;
    also(&svc, &ctx, Kind::Dose, Op::Deleted);
    Ok(())
}

async fn list_doses(svc: Arc<Service>, _ctx: Ctx, args: DoseRange) -> CommandResult<Vec<Dose>> {
    instants(args.from, args.to, MAX_RANGE_DAYS)?;
    svc.on_vault(move |vault| vault.list_doses(args.from, args.to, args.medication_id)).await
}

/// Log a dose taken or skipped. A taken one comes off the medication's
/// supply, which is announced as the medication changing.
async fn log_dose(svc: Arc<Service>, ctx: Ctx, args: SaveDose) -> CommandResult<Dose> {
    let dose = svc.on_vault(move |vault| vault.log_dose(&args.dose)).await?;
    also(&svc, &ctx, Kind::Medication, Op::Updated);
    Ok(dose)
}

async fn delete_dose(svc: Arc<Service>, _ctx: Ctx, args: Ref<DoseId>) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_dose(args.id)).await
}

async fn doses_due(svc: Arc<Service>, _ctx: Ctx, args: Day) -> CommandResult<Vec<DueDose>> {
    time_zone(&args.tz)?;
    svc.on_vault(move |vault| vault.doses_due(args.date, &args.tz)).await
}

async fn medication_alerts(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Between,
) -> CommandResult<Vec<RuleAlert>> {
    between(&args)?;
    svc.on_vault(move |vault| vault.medication_alerts(args.from, args.to, &args.tz)).await
}

async fn medication_adherence(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: AdherenceOf,
) -> CommandResult<Adherence> {
    time_zone(&args.tz)?;
    dates(args.from, args.to)?;
    svc.on_vault(move |vault| {
        vault.medication_adherence(args.medication_id, args.from, args.to, &args.tz)
    })
    .await
}

// ---- allergies -------------------------------------------------------------

async fn list_allergies(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<Allergy>> {
    svc.on_vault(|vault| vault.list_allergies()).await
}

async fn save_allergy(svc: Arc<Service>, _ctx: Ctx, args: SaveAllergy) -> CommandResult<Allergy> {
    svc.on_vault(move |vault| vault.save_allergy(&args.allergy)).await
}

async fn delete_allergy(svc: Arc<Service>, _ctx: Ctx, args: Ref<AllergyId>) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_allergy(args.id)).await
}

// ---- conditions ------------------------------------------------------------

async fn list_conditions(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<Condition>> {
    svc.on_vault(|vault| vault.list_conditions()).await
}

async fn save_condition(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SaveCondition,
) -> CommandResult<Condition> {
    svc.on_vault(move |vault| vault.save_condition(&args.condition)).await
}

/// Delete a condition. Medications and documents that named it lose the
/// reference, and with `removeTrackers` the trackers its plan made go too.
async fn delete_condition(svc: Arc<Service>, ctx: Ctx, args: DeleteCondition) -> CommandResult<()> {
    let DeleteCondition { id, remove_trackers } = args;
    svc.on_vault(move |vault| vault.delete_condition(id, remove_trackers)).await?;
    also(&svc, &ctx, Kind::Medication, Op::Updated);
    also(&svc, &ctx, Kind::HealthDocument, Op::Updated);
    if remove_trackers {
        also(&svc, &ctx, Kind::Tracker, Op::Deleted);
    }
    Ok(())
}

/// The templates the "Add a condition" screen offers. A copy of the code's
/// own list, so it needs an open vault only because the facade lives on one.
async fn condition_templates(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<TemplateInfo>> {
    Ok(svc.require()?.condition_templates())
}

/// Create a condition from a template as the person ticked it. The symptom
/// trackers it creates are announced beside the condition.
async fn apply_condition_template(
    svc: Arc<Service>,
    ctx: Ctx,
    args: ApplyTemplate,
) -> CommandResult<Condition> {
    let ApplyTemplate { template, choice } = args;
    let condition =
        svc.on_vault(move |vault| vault.apply_condition_template(template, &choice)).await?;
    also(&svc, &ctx, Kind::Tracker, Op::Created);
    Ok(condition)
}

async fn condition_report(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: ConditionAt,
) -> CommandResult<ConditionReport> {
    time_zone(&args.tz)?;
    svc.on_vault(move |vault| vault.condition_report(args.id, args.date, &args.tz)).await
}

// ---- labs ------------------------------------------------------------------

async fn list_labs(svc: Arc<Service>, _ctx: Ctx, args: Labs) -> CommandResult<Vec<LabResult>> {
    if let Some(key) = &args.key {
        query_text(key)?;
    }
    svc.on_vault(move |vault| vault.list_labs(args.key.as_deref())).await
}

async fn save_lab(svc: Arc<Service>, _ctx: Ctx, args: SaveLab) -> CommandResult<LabResult> {
    svc.on_vault(move |vault| vault.save_lab(&args.lab)).await
}

async fn delete_lab(svc: Arc<Service>, _ctx: Ctx, args: Ref<LabResultId>) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_lab(args.id)).await
}

async fn lab_trends(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<Vec<LabTrend>> {
    svc.on_vault(|vault| vault.lab_trends()).await
}

// ---- documents -------------------------------------------------------------

async fn list_health_documents(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<HealthDocument>> {
    svc.on_vault(|vault| vault.list_health_documents()).await
}

async fn get_health_document(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Ref<HealthDocumentId>,
) -> CommandResult<HealthDocument> {
    svc.on_vault(move |vault| vault.get_health_document(args.id)).await
}

/// A new document from files the interface has already uploaded.
async fn add_health_document(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: AddDocument,
) -> CommandResult<HealthDocument> {
    let AddDocument { title, kind, date, files } = args;
    if title.chars().count() > MAX_TITLE {
        return Err(CommandError::new(codes::INVALID, "that title is too long"));
    }
    if files.len() > MAX_FILES {
        return Err(CommandError::new(
            codes::TOO_LARGE,
            format!("at most {MAX_FILES} files in one document"),
        ));
    }
    svc.on_vault(move |vault| vault.add_health_document(&title, kind, date, &files)).await
}

async fn save_health_document(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SaveDocument,
) -> CommandResult<HealthDocument> {
    svc.on_vault(move |vault| vault.save_health_document(&args.document)).await
}

/// Delete a document. Its results go with it, or lose the reference.
async fn delete_health_document(
    svc: Arc<Service>,
    ctx: Ctx,
    args: DeleteDocument,
) -> CommandResult<()> {
    let DeleteDocument { id, delete_labs } = args;
    svc.on_vault(move |vault| vault.delete_health_document(id, delete_labs)).await?;
    also(&svc, &ctx, Kind::LabResult, if delete_labs { Op::Deleted } else { Op::Updated });
    Ok(())
}

/// Have the large model read lab results out of a document. They wait on
/// the document for the person to accept; nothing is saved as a result here.
async fn extract_health_document(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: ExtractDocument,
) -> CommandResult<HealthDocument> {
    let ExtractDocument { id, text } = args;
    if text.as_ref().is_some_and(|t| t.len() > MAX_DOCUMENT_TEXT) {
        return Err(CommandError::new(
            codes::TOO_LARGE,
            "that document's text is too long to read in one go",
        ));
    }
    let vault = svc.require_unlocked()?;
    extract::extract(&svc, vault, id, text).await
}

/// Save the results the person reviewed, as results pointing at the
/// document, and clear them from its waiting list.
async fn accept_document_labs(
    svc: Arc<Service>,
    ctx: Ctx,
    args: AcceptLabs,
) -> CommandResult<Vec<LabResult>> {
    let AcceptLabs { id, labs } = args;
    if labs.len() > MAX_LABS {
        return Err(CommandError::new(
            codes::TOO_LARGE,
            format!("at most {MAX_LABS} results at once"),
        ));
    }
    let saved = svc.on_vault(move |vault| vault.accept_document_labs(id, &labs)).await?;
    also(&svc, &ctx, Kind::HealthDocument, Op::Updated);
    Ok(saved)
}

// ---- foods and meals -------------------------------------------------------

async fn list_foods(svc: Arc<Service>, _ctx: Ctx, args: Query) -> CommandResult<Vec<Food>> {
    let query = query_text(&args.query)?;
    svc.on_vault(move |vault| vault.list_foods(&query)).await
}

async fn save_food(svc: Arc<Service>, _ctx: Ctx, args: SaveFood) -> CommandResult<Food> {
    svc.on_vault(move |vault| vault.save_food(&args.food)).await
}

async fn delete_food(svc: Arc<Service>, _ctx: Ctx, args: Ref<FoodId>) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_food(args.id)).await
}

/// Search this vault's foods and then FoodData Central. Only the text leaves
/// the machine, and not from a locked vault.
async fn search_foods(svc: Arc<Service>, _ctx: Ctx, args: Query) -> CommandResult<Vec<FoodHit>> {
    let query = query_text(&args.query)?;
    let vault = svc.require_unlocked()?;
    if query.is_empty() {
        return Ok(Vec::new());
    }
    food::search(&svc, vault, &query).await
}

/// Look a barcode up: this vault's foods, then Open Food Facts, then
/// FoodData Central's branded ones. `None` when nobody knows it.
async fn lookup_barcode(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Barcode,
) -> CommandResult<Option<FoodHit>> {
    let code = args.code.trim();
    if !(8..=14).contains(&code.len()) || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(CommandError::new(codes::INVALID, "that is not a barcode"));
    }
    let vault = svc.require_unlocked()?;
    food::barcode(&svc, vault, code).await
}

async fn list_meals(svc: Arc<Service>, _ctx: Ctx, args: Instants) -> CommandResult<Vec<Meal>> {
    instants(args.from, args.to, MAX_RANGE_DAYS)?;
    svc.on_vault(move |vault| vault.list_meals(args.from, args.to)).await
}

/// Save a meal. Each food it names has its last use moved on, which is
/// announced as the food changing.
async fn save_meal(svc: Arc<Service>, ctx: Ctx, args: SaveMeal) -> CommandResult<Meal> {
    let meal = svc.on_vault(move |vault| vault.save_meal(&args.meal)).await?;
    also(&svc, &ctx, Kind::Food, Op::Updated);
    Ok(meal)
}

async fn delete_meal(svc: Arc<Service>, _ctx: Ctx, args: Ref<MealId>) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_meal(args.id)).await
}

/// The foods and portions a picture of a plate shows, as the medium model
/// guesses them -- every item marked `estimated`.
async fn estimate_meal_photo(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Photo,
) -> CommandResult<Vec<MealItem>> {
    let vault = svc.require_unlocked()?;
    photo::estimate(&svc, vault, args.photo).await
}

async fn meal_response(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Ref<MealId>,
) -> CommandResult<MealResponse> {
    svc.on_vault(move |vault| vault.meal_response(args.id)).await
}

// ---- a visit ---------------------------------------------------------------

/// A page for a clinic visit, written by the large model from the vault's
/// own numbers.
async fn health_visit_brief(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Brief,
) -> CommandResult<VisitBrief> {
    time_zone(&args.tz)?;
    let vault = svc.require_unlocked()?;
    brief::write(&svc, vault, args.condition_id, args.since, args.tz).await
}

// ---- what a command answers without ----------------------------------------

fn scrub_workout(mut workout: Workout) -> Workout {
    workout.external_id.clear();
    workout
}

fn scrub_day(mut day: HealthDay) -> HealthDay {
    day.workouts = day.workouts.into_iter().map(scrub_workout).collect();
    day
}

fn scrub_timeline(mut timeline: DayTimeline) -> DayTimeline {
    timeline.workouts = timeline.workouts.into_iter().map(scrub_workout).collect();
    timeline
}

// ---- cascades --------------------------------------------------------------

/// Announce the records a command's vault call touched of `kind`, beside the
/// one its own `change:` names. Nothing is raised when it touched none.
fn also(svc: &Arc<Service>, ctx: &Ctx, kind: Kind, op: Op) {
    crate::events::emit_touched(
        svc.events().as_ref(),
        ctx.caller.origin().map(str::to_string),
        &crate::touched::current(),
        kind,
        op,
    );
}

// ---- checks at the edge ----------------------------------------------------

/// Refuse a time zone name longer than any IANA one. Whether it names a
/// zone at all is the vault's to say.
fn time_zone(tz: &str) -> CommandResult<()> {
    if tz.chars().count() > MAX_TZ {
        return Err(CommandError::new(codes::INVALID, "that is not a time zone"));
    }
    Ok(())
}

/// Refuse a search, a filter or a source's name longer than anything a
/// screen sends, and give it back trimmed.
fn query_text(text: &str) -> CommandResult<String> {
    if text.chars().count() > MAX_QUERY {
        return Err(CommandError::new(codes::INVALID, format!("at most {MAX_QUERY} characters")));
    }
    Ok(text.trim().to_string())
}

/// Refuse local dates that run backwards or cover more than
/// [`MAX_RANGE_DAYS`].
fn dates(from: Date, to: Date) -> CommandResult<()> {
    if from > to {
        return Err(CommandError::new(codes::INVALID, "the start comes after the end"));
    }
    if i64::from((to - from).get_days()) + 1 > MAX_RANGE_DAYS {
        return Err(CommandError::new(
            codes::INVALID,
            format!("at most {MAX_RANGE_DAYS} days at once"),
        ));
    }
    Ok(())
}

/// [`dates`] and [`time_zone`] for the commands that take both.
fn between(args: &Between) -> CommandResult<()> {
    time_zone(&args.tz)?;
    dates(args.from, args.to)
}

/// Refuse instants that run backwards or are more than `days` apart.
fn instants(from: Timestamp, to: Timestamp, days: i64) -> CommandResult<()> {
    if from > to {
        return Err(CommandError::new(codes::INVALID, "the start comes after the end"));
    }
    if to.duration_since(from) > SignedDuration::from_hours(24 * days) {
        return Err(CommandError::new(codes::INVALID, format!("at most {days} days at once")));
    }
    Ok(())
}

pub static COMMANDS: &[crate::command::Command] = &[
    // ---- the catalogue and the settings ----
    command! {
        name: "health_catalog", scope: Health, effect: Read,
        args: Nothing, returns: "MetricEntry[]", signature: &[],
        run: health_catalog,
    },
    command! {
        name: "health_settings", scope: Health, effect: Read,
        args: Nothing, returns: "HealthSettings", signature: &[],
        run: health_settings,
    },
    command! {
        name: "save_health_settings", scope: Health, effect: Write,
        change: Settings / Updated,
        args: SaveSettings, returns: "void",
        signature: &[("settings", "HealthSettings", true)],
        run: save_health_settings,
    },
    // ---- sources ----
    command! {
        name: "list_health_sources", scope: Health, effect: Read,
        args: Nothing, returns: "HealthSource[]", signature: &[],
        run: list_health_sources,
    },
    command! {
        name: "save_health_source", scope: Health, effect: Write,
        change: HealthSource / Updated,
        id: |a: &SaveSource| Some(a.source.id.to_string()),
        args: SaveSource, returns: "HealthSource",
        signature: &[("source", "HealthSource", true)],
        run: save_health_source,
    },
    command! {
        name: "delete_health_source", scope: Health, effect: Destructive,
        change: HealthSource / Deleted,
        id: |a: &Ref<HealthSourceId>| Some(a.id.to_string()),
        args: Ref<HealthSourceId>, returns: "void",
        signature: &[("id", "HealthSourceId", true)],
        run: delete_health_source,
    },
    // ---- the phone's push ----
    //
    // The only command a healthIngest token reaches. Never with an id:
    // samples are written by the thousand, and what a window does about any
    // of them is reload what it shows.
    command! {
        name: "record_health_push", scope: HealthIngest, effect: Write,
        change: HealthSample / Updated,
        args: Push, returns: "HealthIngestReport",
        signature: &[
            ("sourceId", "HealthSourceId", true),
            ("headers", "PushHeaders", true),
            ("body", "unknown", true),
        ],
        run: record_health_push,
    },
    command! {
        name: "set_health_source_token", scope: All, effect: Write,
        change: HealthSource / Updated,
        id: |a: &SourceToken| Some(a.source_id.to_string()),
        args: SourceToken, returns: "void",
        signature: &[("sourceId", "HealthSourceId", true), ("tokenId", "string", true)],
        run: set_health_source_token,
    },
    // ---- the export import ----
    command! {
        name: "import_apple_health", scope: Health, effect: Write,
        change: HealthSample / Updated,
        args: ImportApple, returns: "ImportStatus",
        signature: &[
            ("path", "string", true),
            ("sourceId", "HealthSourceId | null", false),
        ],
        run: import_apple_health,
    },
    command! {
        name: "import_apple_health_file", scope: Health, effect: Write,
        change: HealthSample / Updated,
        args: ImportAppleFile, returns: "ImportStatus",
        signature: &[
            ("file", "FileId", true),
            ("sourceId", "HealthSourceId | null", false),
            ("trashAfter", "boolean", false),
        ],
        run: import_apple_health_file,
    },
    command! {
        name: "apple_import_status", scope: Health, effect: Read,
        args: Nothing, returns: "ImportStatus", signature: &[],
        run: apple_import_status,
    },
    // Writes nothing itself: the import it stops raises its own changes as
    // it ends. Announced here too so that a screen showing the import looks
    // again at once, and because a write that announces nothing has to be
    // listed by hand in `command.rs`.
    command! {
        name: "cancel_apple_import", scope: Health, effect: Write,
        change: HealthSample / Updated,
        args: Nothing, returns: "boolean", signature: &[],
        run: cancel_apple_import,
    },
    // ---- the food-lookup key ----
    //
    // Setting or removing it needs the whole vault, not `health` alone: a
    // client narrowed to Health could otherwise put its own key in, and the
    // next food somebody looked up would be looked up under that client.
    command! {
        name: "health_keys", scope: Health, effect: Read,
        args: Nothing, returns: "HealthKeysInfo", signature: &[],
        run: health_keys,
    },
    command! {
        name: "save_health_keys", scope: All, effect: Write,
        change: Settings / Updated,
        sensitive: true,
        args: FdcKeys, returns: "void",
        signature: &[("fdcKey", "string", true)],
        run: save_health_keys,
    },
    command! {
        name: "clear_health_keys", scope: All, effect: Destructive,
        change: Settings / Updated,
        sensitive: true,
        args: Nothing, returns: "void", signature: &[],
        run: clear_health_keys,
    },
    // ---- samples and reports ----
    command! {
        name: "health_today", scope: Health, effect: Read,
        args: Day, returns: "HealthDay",
        signature: &[("date", "string", true), ("tz", "string", true)],
        run: health_today,
    },
    command! {
        name: "health_series", scope: Health, effect: Read,
        args: Series, returns: "SeriesResult",
        signature: &[("query", "SeriesQuery", true)],
        run: health_series,
    },
    command! {
        name: "health_samples", scope: Health, effect: Read,
        args: Samples, returns: "SamplePoint[]",
        signature: &[
            ("metric", "Metric", true),
            ("from", "string", true),
            ("to", "string", true),
        ],
        run: health_samples,
    },
    command! {
        name: "health_latest", scope: Health, effect: Read,
        args: LatestOf, returns: "Latest[]",
        signature: &[("metrics", "Metric[]", true), ("on", "string", true)],
        run: health_latest,
    },
    command! {
        name: "add_health_reading", scope: Health, effect: Write,
        change: HealthSample / Updated,
        args: Reading, returns: "void",
        signature: &[("reading", "ManualReading", true)],
        run: add_health_reading,
    },
    command! {
        name: "delete_health_samples", scope: Health, effect: Destructive,
        change: HealthSample / Deleted,
        args: DeleteSamples, returns: "number",
        signature: &[
            ("metric", "Metric", true),
            ("from", "string", true),
            ("to", "string", true),
            ("source", "string | null", false),
        ],
        run: delete_health_samples,
    },
    command! {
        name: "health_activity", scope: Health, effect: Read,
        args: Between, returns: "ActivityDay[]",
        signature: &[
            ("from", "string", true),
            ("to", "string", true),
            ("tz", "string", true),
        ],
        run: health_activity,
    },
    command! {
        name: "health_sleep", scope: Health, effect: Read,
        args: Between, returns: "SleepNight[]",
        signature: &[
            ("from", "string", true),
            ("to", "string", true),
            ("tz", "string", true),
        ],
        run: health_sleep,
    },
    command! {
        name: "health_glucose", scope: Health, effect: Read,
        args: Between, returns: "GlucoseReport",
        signature: &[
            ("from", "string", true),
            ("to", "string", true),
            ("tz", "string", true),
        ],
        run: health_glucose,
    },
    command! {
        name: "health_nutrition", scope: Health, effect: Read,
        args: Between, returns: "NutritionDay[]",
        signature: &[
            ("from", "string", true),
            ("to", "string", true),
            ("tz", "string", true),
        ],
        run: health_nutrition,
    },
    command! {
        name: "health_day_timeline", scope: Health, effect: Read,
        args: Day, returns: "DayTimeline",
        signature: &[("date", "string", true), ("tz", "string", true)],
        run: health_day_timeline,
    },
    // ---- workouts ----
    command! {
        name: "list_workouts", scope: Health, effect: Read,
        args: Instants, returns: "Workout[]",
        signature: &[("from", "string", true), ("to", "string", true)],
        run: list_workouts,
    },
    command! {
        name: "get_workout", scope: Health, effect: Read,
        args: Ref<WorkoutId>, returns: "WorkoutDetail",
        signature: &[("id", "WorkoutId", true)],
        run: get_workout,
    },
    command! {
        name: "save_workout", scope: Health, effect: Write,
        change: Workout / Updated,
        id: |a: &SaveWorkout| Some(a.workout.id.to_string()),
        args: SaveWorkout, returns: "Workout",
        signature: &[("workout", "Workout", true)],
        run: save_workout,
    },
    command! {
        name: "delete_workout", scope: Health, effect: Destructive,
        change: Workout / Deleted,
        id: |a: &Ref<WorkoutId>| Some(a.id.to_string()),
        args: Ref<WorkoutId>, returns: "void",
        signature: &[("id", "WorkoutId", true)],
        run: delete_workout,
    },
    // ---- medications and doses ----
    command! {
        name: "list_medications", scope: Health, effect: Read,
        args: Nothing, returns: "Medication[]", signature: &[],
        run: list_medications,
    },
    command! {
        name: "save_medication", scope: Health, effect: Write,
        change: Medication / Updated,
        id: |a: &SaveMedication| Some(a.medication.id.to_string()),
        args: SaveMedication, returns: "Medication",
        signature: &[("medication", "Medication", true)],
        run: save_medication,
    },
    command! {
        name: "delete_medication", scope: Health, effect: Destructive,
        change: Medication / Deleted,
        id: |a: &Ref<MedicationId>| Some(a.id.to_string()),
        args: Ref<MedicationId>, returns: "void",
        signature: &[("id", "MedicationId", true)],
        run: delete_medication,
    },
    command! {
        name: "list_doses", scope: Health, effect: Read,
        args: DoseRange, returns: "Dose[]",
        signature: &[
            ("from", "string", true),
            ("to", "string", true),
            ("medicationId", "MedicationId | null", false),
        ],
        run: list_doses,
    },
    command! {
        name: "log_dose", scope: Health, effect: Write,
        change: Dose / Updated,
        id: |a: &SaveDose| Some(a.dose.id.to_string()),
        args: SaveDose, returns: "Dose",
        signature: &[("dose", "Dose", true)],
        run: log_dose,
    },
    command! {
        name: "delete_dose", scope: Health, effect: Destructive,
        change: Dose / Deleted,
        id: |a: &Ref<DoseId>| Some(a.id.to_string()),
        args: Ref<DoseId>, returns: "void",
        signature: &[("id", "DoseId", true)],
        run: delete_dose,
    },
    command! {
        name: "doses_due", scope: Health, effect: Read,
        args: Day, returns: "DueDose[]",
        signature: &[("date", "string", true), ("tz", "string", true)],
        run: doses_due,
    },
    command! {
        name: "medication_alerts", scope: Health, effect: Read,
        args: Between, returns: "RuleAlert[]",
        signature: &[
            ("from", "string", true),
            ("to", "string", true),
            ("tz", "string", true),
        ],
        run: medication_alerts,
    },
    command! {
        name: "medication_adherence", scope: Health, effect: Read,
        args: AdherenceOf, returns: "Adherence",
        signature: &[
            ("medicationId", "MedicationId | null", false),
            ("from", "string", true),
            ("to", "string", true),
            ("tz", "string", true),
        ],
        run: medication_adherence,
    },
    // ---- allergies ----
    command! {
        name: "list_allergies", scope: Health, effect: Read,
        args: Nothing, returns: "Allergy[]", signature: &[],
        run: list_allergies,
    },
    command! {
        name: "save_allergy", scope: Health, effect: Write,
        change: Allergy / Updated,
        id: |a: &SaveAllergy| Some(a.allergy.id.to_string()),
        args: SaveAllergy, returns: "Allergy",
        signature: &[("allergy", "Allergy", true)],
        run: save_allergy,
    },
    command! {
        name: "delete_allergy", scope: Health, effect: Destructive,
        change: Allergy / Deleted,
        id: |a: &Ref<AllergyId>| Some(a.id.to_string()),
        args: Ref<AllergyId>, returns: "void",
        signature: &[("id", "AllergyId", true)],
        run: delete_allergy,
    },
    // ---- conditions ----
    command! {
        name: "list_conditions", scope: Health, effect: Read,
        args: Nothing, returns: "Condition[]", signature: &[],
        run: list_conditions,
    },
    command! {
        name: "save_condition", scope: Health, effect: Write,
        change: Condition / Updated,
        id: |a: &SaveCondition| Some(a.condition.id.to_string()),
        args: SaveCondition, returns: "Condition",
        signature: &[("condition", "Condition", true)],
        run: save_condition,
    },
    command! {
        name: "delete_condition", scope: Health, effect: Destructive,
        change: Condition / Deleted,
        id: |a: &DeleteCondition| Some(a.id.to_string()),
        args: DeleteCondition, returns: "void",
        signature: &[("id", "ConditionId", true), ("removeTrackers", "boolean", true)],
        run: delete_condition,
    },
    command! {
        name: "condition_templates", scope: Health, effect: Read,
        args: Nothing, returns: "TemplateInfo[]", signature: &[],
        run: condition_templates,
    },
    // No id: the condition is minted by the vault, so it did not exist when
    // the call began.
    command! {
        name: "apply_condition_template", scope: Health, effect: Write,
        change: Condition / Created,
        args: ApplyTemplate, returns: "Condition",
        signature: &[
            ("template", "TemplateKey", true),
            ("choice", "TemplateChoice", true),
        ],
        run: apply_condition_template,
    },
    command! {
        name: "condition_report", scope: Health, effect: Read,
        args: ConditionAt, returns: "ConditionReport",
        signature: &[
            ("id", "ConditionId", true),
            ("date", "string", true),
            ("tz", "string", true),
        ],
        run: condition_report,
    },
    // ---- labs ----
    command! {
        name: "list_labs", scope: Health, effect: Read,
        args: Labs, returns: "LabResult[]",
        signature: &[("key", "string | null", false)],
        run: list_labs,
    },
    command! {
        name: "save_lab", scope: Health, effect: Write,
        change: LabResult / Updated,
        id: |a: &SaveLab| Some(a.lab.id.to_string()),
        args: SaveLab, returns: "LabResult",
        signature: &[("lab", "LabResult", true)],
        run: save_lab,
    },
    command! {
        name: "delete_lab", scope: Health, effect: Destructive,
        change: LabResult / Deleted,
        id: |a: &Ref<LabResultId>| Some(a.id.to_string()),
        args: Ref<LabResultId>, returns: "void",
        signature: &[("id", "LabResultId", true)],
        run: delete_lab,
    },
    command! {
        name: "lab_trends", scope: Health, effect: Read,
        args: Nothing, returns: "LabTrend[]", signature: &[],
        run: lab_trends,
    },
    // ---- documents ----
    command! {
        name: "list_health_documents", scope: Health, effect: Read,
        args: Nothing, returns: "HealthDocument[]", signature: &[],
        run: list_health_documents,
    },
    command! {
        name: "get_health_document", scope: Health, effect: Read,
        args: Ref<HealthDocumentId>, returns: "HealthDocument",
        signature: &[("id", "HealthDocumentId", true)],
        run: get_health_document,
    },
    // No id: the document is minted by the vault.
    command! {
        name: "add_health_document", scope: Health, effect: Write,
        change: HealthDocument / Created,
        args: AddDocument, returns: "HealthDocument",
        signature: &[
            ("title", "string", true),
            ("kind", "DocKind", true),
            ("date", "string | null", false),
            ("files", "FileId[]", true),
        ],
        run: add_health_document,
    },
    command! {
        name: "save_health_document", scope: Health, effect: Write,
        change: HealthDocument / Updated,
        id: |a: &SaveDocument| Some(a.document.id.to_string()),
        args: SaveDocument, returns: "HealthDocument",
        signature: &[("document", "HealthDocument", true)],
        run: save_health_document,
    },
    command! {
        name: "delete_health_document", scope: Health, effect: Destructive,
        change: HealthDocument / Deleted,
        id: |a: &DeleteDocument| Some(a.id.to_string()),
        args: DeleteDocument, returns: "void",
        signature: &[("id", "HealthDocumentId", true), ("deleteLabs", "boolean", true)],
        run: delete_health_document,
    },
    command! {
        name: "extract_health_document", scope: Health, effect: Write,
        change: HealthDocument / Updated,
        id: |a: &ExtractDocument| Some(a.id.to_string()),
        args: ExtractDocument, returns: "HealthDocument",
        signature: &[
            ("id", "HealthDocumentId", true),
            ("text", "string | null", false),
        ],
        run: extract_health_document,
    },
    // No ids: the results are minted by the vault from what was reviewed.
    command! {
        name: "accept_document_labs", scope: Health, effect: Write,
        change: LabResult / Created,
        args: AcceptLabs, returns: "LabResult[]",
        signature: &[("id", "HealthDocumentId", true), ("labs", "LabResult[]", true)],
        run: accept_document_labs,
    },
    // ---- foods and meals ----
    command! {
        name: "list_foods", scope: Health, effect: Read,
        args: Query, returns: "Food[]",
        signature: &[("query", "string", true)],
        run: list_foods,
    },
    command! {
        name: "save_food", scope: Health, effect: Write,
        change: Food / Updated,
        id: |a: &SaveFood| Some(a.food.id.to_string()),
        args: SaveFood, returns: "Food",
        signature: &[("food", "Food", true)],
        run: save_food,
    },
    command! {
        name: "delete_food", scope: Health, effect: Destructive,
        change: Food / Deleted,
        id: |a: &Ref<FoodId>| Some(a.id.to_string()),
        args: Ref<FoodId>, returns: "void",
        signature: &[("id", "FoodId", true)],
        run: delete_food,
    },
    command! {
        name: "search_foods", scope: Health, effect: Read,
        args: Query, returns: "FoodHit[]",
        signature: &[("query", "string", true)],
        run: search_foods,
    },
    command! {
        name: "lookup_barcode", scope: Health, effect: Read,
        args: Barcode, returns: "FoodHit | null",
        signature: &[("code", "string", true)],
        run: lookup_barcode,
    },
    command! {
        name: "list_meals", scope: Health, effect: Read,
        args: Instants, returns: "Meal[]",
        signature: &[("from", "string", true), ("to", "string", true)],
        run: list_meals,
    },
    command! {
        name: "save_meal", scope: Health, effect: Write,
        change: Meal / Updated,
        id: |a: &SaveMeal| Some(a.meal.id.to_string()),
        args: SaveMeal, returns: "Meal",
        signature: &[("meal", "Meal", true)],
        run: save_meal,
    },
    command! {
        name: "delete_meal", scope: Health, effect: Destructive,
        change: Meal / Deleted,
        id: |a: &Ref<MealId>| Some(a.id.to_string()),
        args: Ref<MealId>, returns: "void",
        signature: &[("id", "MealId", true)],
        run: delete_meal,
    },
    command! {
        name: "estimate_meal_photo", scope: Health, effect: Read,
        args: Photo, returns: "MealItem[]",
        signature: &[("photo", "FileId", true)],
        run: estimate_meal_photo,
    },
    command! {
        name: "meal_response", scope: Health, effect: Read,
        args: Ref<MealId>, returns: "MealResponse",
        signature: &[("id", "MealId", true)],
        run: meal_response,
    },
    // ---- a visit ----
    command! {
        name: "health_visit_brief", scope: Health, effect: Read,
        args: Brief, returns: "VisitBrief",
        signature: &[
            ("conditionId", "ConditionId | null", false),
            ("since", "string", true),
            ("tz", "string", true),
        ],
        run: health_visit_brief,
    },
];
