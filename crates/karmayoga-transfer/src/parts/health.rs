//! Health: `health/` in the export.
//!
//! ```text
//!   health/
//!     samples/<metric>.csv    every sample of each metric: start, end, value, source
//!     workouts.csv            one row per workout
//!     doses.csv               one row per dose taken or skipped
//!     labs.csv                one row per lab result
//!     meals.csv               one row per meal, its items as JSON in the last column
//!     medications.json        the medications, whole
//!     conditions.json         the conditions and their care plans, whole
//!     allergies.json          the allergies, whole
//!     foods.json              the foods, with their nutrients, whole
//!     sources.json            the connections data arrives through: name and kind only
//!     settings.json           the units, goals, targets and the order of trust among devices
//!     documents/<id>/         document.json for each clinical document, and its files
//!     media/                  meal photographs
//! ```
//!
//! # Samples are a spreadsheet's
//!
//! `samples/<metric>.csv` is what somebody would open to ask their own
//! question of a year of heart rate or glucose, so it is a flat table with a
//! file per metric, named by the metric's own key (`heart_rate`,
//! `blood_glucose`). Its columns are `start`, `end`, `value` and `source`;
//! times are RFC 3339 instants in UTC, which every spreadsheet and every
//! language reads, and the unit every value is in is named in the header --
//! `value (mg/dL)` -- because the store keeps canonical units whatever the
//! person displays. An instant has an `end` equal to its `start`. The offset
//! a reading was taken at is not written: the instants are absolute.
//!
//! A metric is read a month at a time and written as it is read, so an export
//! of years of heart rate holds one metric's table in memory and not the
//! vault's.
//!
//! # Documents carry their files
//!
//! A clinical document is its own folder: `document.json` -- everything
//! about it, its extracted text and any lab results waiting to be accepted
//! included -- and, when "include attachments" is ticked, its files beside it
//! under the names the person gave them, prefixed with eight digits of the
//! blob's address to keep two `scan.pdf`s apart. `document.json` lists the
//! files by name, type and size, and says which of them were written; a file
//! the vault has lost is left out and said so, rather than failing the export.
//!
//! # What is not here, ever
//!
//! A token, a food-lookup key, a provider's own id, or a dedupe key: a
//! workout's `key` and its source's `externalId`, a dose's `key`, a
//! document's `externalId`, the id of the push token a source was given.
//! The keys are hashed under the vault's own key and mean nothing outside it,
//! and the external ids are somebody else's identifiers for the person's own
//! papers and workouts. The tokens themselves are not records and nothing
//! here could reach them if it tried: this part reads the store's records and
//! nothing else, and reads them directly, without the vault's key.
//!
//! Nor a workout or a dose the person deleted that a source would bring back
//! (the store keeps it, hidden, so that it does not come back): what was
//! deleted is not handed over, as it is not shown.
//!
//! # A spreadsheet will open this
//!
//! So a cell of somebody's text that starts the way a formula does -- `=`,
//! `+`, `-`, `@`, a tab or a carriage return, after any leading spaces -- is
//! written with an apostrophe in front, which a spreadsheet shows as text
//! rather than running. A device is whatever a phone calls itself, a note is
//! whatever was typed in a hurry, and a lab's name for a test is the lab's.
//! The files are read back too, so the apostrophe is taken off again on the
//! way in, and a cell that really did begin with an apostrophe before a
//! formula character is given a second one on the way out, so that every cell
//! comes back exactly as it went. Numbers, dates and ids are written by this
//! code and are what they look like, so `-3.5` stays a number.
//!
//! The rule itself, and its undoing on the way in, is in `crate::text`
//! (`Csv::row` and `Row::get`), so every kind of export has it and this one
//! need not remember to.
//!
//! # Reading it back
//!
//! Medications, conditions, lab results, allergies, foods, meals and
//! documents merge by id, the way every part's records do: one already here is
//! left alone, or overwritten under [`Mode::Replace`]. Doses do too, but only
//! the ones typed in here or tapped from a reminder -- a dose that was a
//! source's event (its row says `keyed`) is skipped and counted, because the
//! phone brings it again. A row of `doses.csv` names its medication by id, or
//! failing that by name; one that names no medication the vault has is
//! refused. A lab result's document and a medication's conditions are kept as
//! they are written: ids are the same wherever an archive came from, so they
//! line up when the records they name are in the archive too.
//!
//! **Not samples or workouts**, nor the sources. A sample's series and a
//! workout's dedupe key are hashed under the vault's own key, which an import
//! (reaching only the store) does not hold; the phone and the Apple export
//! bring them back whole anyway, and a hand-made sample file from here would
//! only ever duplicate them. `sources.json` and `settings.json` are for
//! reading: a connection is set up where its token is minted, and the
//! person's units and goals are theirs to set again.
//!
//! A file is held to what the app holds a record to: a medication, a food, a
//! condition and an allergy need a name, a lab result a name and the date it
//! was collected, a dose a time and a medication, a meal a time. Rows that
//! cannot be read are named -- the first twenty of a file, then counted --
//! and the rest of the file lands.

use crate::text::{Csv, Row, Table, safe_name};
use crate::{Files, Mode, Options, Part, Portable, Report, Spec, land};
use jiff::Timestamp;
use jiff::civil::Date;
use karmayoga_core::files::{FileOrigin, RefKind, RefOwner};
use karmayoga_core::health::{
    Allergy, Condition, DocFile, DocSource, Dose, ExtractionState, Food, HealthDocument,
    HealthSettings, LabResult, Meal, MealItem, Medication, Metric, Nutrients, Workout,
};
use karmayoga_core::store::JournalStore;
use karmayoga_core::{
    Error, FileId, HealthDocumentId, HealthStore, LabResultId, MealId, MedicationId, Result,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub struct HealthPart;

pub static HEALTH: HealthPart = HealthPart;

static SPEC: Spec = Spec {
    id: "health",
    label: "Health",
    summary: "Everything your watch and phone measured, workouts, medications and the doses you \
              logged, conditions and their care plans, lab results, clinical documents, \
              allergies, foods and meals.",
    format: "CSV for samples and logs, JSON for everything else, documents as their own files",
    media: true,
    imports: true,
};

const WORKOUTS: &str = "workouts.csv";
const DOSES: &str = "doses.csv";
const LABS: &str = "labs.csv";
const MEALS: &str = "meals.csv";
const MEDICATIONS: &str = "medications.json";
const CONDITIONS: &str = "conditions.json";
const ALLERGIES: &str = "allergies.json";
const FOODS: &str = "foods.json";
const SOURCES: &str = "sources.json";
const SETTINGS: &str = "settings.json";
const DOCUMENT: &str = "document.json";

const WORKOUT_COLUMNS: &[&str] = &[
    "id",
    "activity",
    "name",
    "start",
    "end",
    "offset_min",
    "active_energy_kcal",
    "total_energy_kcal",
    "distance_m",
    "elevation_up_m",
    "avg_hr",
    "max_hr",
    "effort",
    "indoor",
    "source",
    "note",
];
const DOSE_COLUMNS: &[&str] = &[
    "at",
    "medication",
    "status",
    "amount",
    "scheduled_at",
    "origin",
    "note",
    "keyed",
    "medication_id",
    "created_at",
    "updated_at",
    "id",
];
const LAB_COLUMNS: &[&str] = &[
    "collected",
    "name",
    "value",
    "unit",
    "ref_low",
    "ref_high",
    "ref_text",
    "flag",
    "text",
    "code",
    "panel",
    "lab",
    "origin",
    "document_id",
    "collected_at",
    "note",
    "created_at",
    "updated_at",
    "id",
];
const MEAL_COLUMNS: &[&str] = &[
    "at",
    "offset_min",
    "slot",
    "name",
    "kcal",
    "protein_g",
    "carbs_g",
    "fat_g",
    "note",
    "photo",
    "items",
    "created_at",
    "updated_at",
    "id",
];

/// How many days of one metric are read at a time. See the module doc.
const SAMPLE_DAYS: i64 = 31;

/// How many doses go to the store in one call. Each call is all or nothing;
/// holding the vault's one writer through years of doses would stall every
/// reminder that arrived meanwhile, and a second import completes a failure
/// part way without duplicating, because doses merge by id.
const DOSE_BATCH: usize = 1_000;

/// After this many rows of a file that could not be read, the rest are
/// counted rather than named: a file that is wrong on every line should say
/// so in a sentence, not in ten thousand.
const ROW_PROBLEMS: usize = 20;

impl Portable for HealthPart {
    fn spec(&self) -> &'static Spec {
        &SPEC
    }

    /// Every record: workouts, doses, medications, conditions, lab results,
    /// documents, allergies, foods, meals and sources, but not a workout or a
    /// dose that was deleted and is only kept so that it is not brought back.
    /// Not samples, which are not records -- a year of heart rate would make
    /// the app look a million times fuller than anything somebody wrote.
    ///
    /// Counting workouts, doses and meals opens every one: the store has no
    /// count of its own.
    fn tally(&self, store: &dyn JournalStore) -> Result<Option<u64>> {
        let Some(health) = store.health() else { return Ok(None) };
        let (from, to) = (Timestamp::MIN, Timestamp::MAX);
        let count = health.workouts(from, to)?.iter().filter(|w| !w.hidden).count()
            + health.doses(from, to, None)?.iter().filter(|d| !d.hidden).count()
            + health.meals(from, to)?.len()
            + health.list_sources()?.len()
            + health.list_medications()?.len()
            + health.list_conditions()?.len()
            + health.list_labs()?.len()
            + health.list_documents()?.len()
            + health.list_allergies()?.len()
            + health.list_foods()?.len();
        Ok(Some(count as u64))
    }

    fn export(&self, store: &dyn JournalStore, out: &mut Files<'_>, opts: &Options) -> Result<()> {
        let Some(health) = store.health() else { return Ok(()) };
        export_samples(health, out)?;
        export_workouts(health, out)?;
        export_doses(health, out)?;
        export_labs(health, out)?;
        export_meals(store, health, out, opts)?;

        let medications = health.list_medications()?;
        if !medications.is_empty() {
            let json = sidecar("medications", &medications)?;
            out.records(MEDICATIONS, json, medications.len() as u64)?;
        }
        let conditions = health.list_conditions()?;
        if !conditions.is_empty() {
            out.records(CONDITIONS, sidecar("conditions", &conditions)?, conditions.len() as u64)?;
        }
        let allergies = health.list_allergies()?;
        if !allergies.is_empty() {
            out.records(ALLERGIES, sidecar("allergies", &allergies)?, allergies.len() as u64)?;
        }
        let foods = health.list_foods()?;
        if !foods.is_empty() {
            out.records(FOODS, sidecar("foods", &foods)?, foods.len() as u64)?;
        }
        // Names and kinds only: what a source has heard from its device, and
        // the devices it has seen, are the source's own business.
        let sources: Vec<serde_json::Value> = health
            .list_sources()?
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "connector": s.connector,
                    "name": s.name,
                    "paused": s.paused,
                })
            })
            .collect();
        if !sources.is_empty() {
            out.records(SOURCES, sidecar("sources", &sources)?, sources.len() as u64)?;
        }
        // The default settings alone are not something anybody chose.
        let settings = health.health_settings()?;
        if settings != HealthSettings::default() {
            out.text(SETTINGS, format!("{}\n", serde_json::to_string_pretty(&settings)?))?;
        }
        export_documents(store, health, out, opts)
    }

    fn import(&self, store: &dyn JournalStore, src: &Part<'_>, mode: Mode) -> Result<Report> {
        let mut report = Report::new(SPEC.id);
        let Some(health) = store.health() else {
            report.problem(SPEC.label, "this vault's backend does not store health data");
            return Ok(report);
        };
        // In this order, because each reads what the one before it wrote: a
        // dose names a medication, a lab result may name a document.
        read_medications(health, src, mode, &mut report)?;
        read_conditions(health, src, mode, &mut report)?;
        read_allergies(health, src, mode, &mut report)?;
        read_foods(health, src, mode, &mut report)?;
        read_documents(store, health, src, mode, &mut report)?;
        read_labs(health, src, mode, &mut report)?;
        read_doses(health, src, mode, &mut report)?;
        read_meals(store, health, src, mode, &mut report)?;
        Ok(report)
    }
}

// ---- writing --------------------------------------------------------------

/// `{"<key>": [...]}`, pretty-printed. An object rather than a bare list so
/// that a file can grow a second field without being a different shape.
fn sidecar<T: serde::Serialize>(key: &str, list: &[T]) -> Result<String> {
    let mut object = serde_json::Map::new();
    object.insert(key.to_string(), serde_json::to_value(list)?);
    let json = serde_json::to_string_pretty(&serde_json::Value::Object(object))?;
    Ok(format!("{json}\n"))
}

/// Every sample of every metric, a file a metric.
fn export_samples(health: &dyn HealthStore, out: &mut Files<'_>) -> Result<()> {
    for extent in health.series_extents()? {
        // One file a metric, built up as the series is read. A series is one
        // metric's hash, so there is one; a day that does not say which it is
        // cannot be filed, and is passed over.
        let mut tables: BTreeMap<Metric, Csv> = BTreeMap::new();
        let mut from = extent.first;
        while from <= extent.last {
            let to = from
                .checked_add(jiff::Span::new().days(SAMPLE_DAYS - 1))
                .map_or(extent.last, |d| d.min(extent.last));
            for day in health.series_days(&extent.series, from, to)? {
                let Some(metric) = day.metric else { continue };
                let table = tables.entry(metric).or_insert_with(|| sample_table(metric));
                for sample in &day.samples {
                    let Ok(start) = Timestamp::from_second(sample.start) else { continue };
                    let Ok(end) = Timestamp::from_second(sample.start + i64::from(sample.secs))
                    else {
                        continue;
                    };
                    let source =
                        day.sources.get(usize::from(sample.source)).map_or("", String::as_str);
                    table.row(&[
                        start.to_string(),
                        end.to_string(),
                        sample.value.to_string(),
                        text(source),
                    ]);
                }
            }
            match to.tomorrow() {
                Ok(next) => from = next,
                Err(_) => break,
            }
        }
        for (metric, table) in tables {
            if table.rows() > 0 {
                out.text(&format!("samples/{}.csv", metric.key()), table.finish())?;
            }
        }
    }
    Ok(())
}

/// An empty samples table for `metric`, its header naming the unit.
fn sample_table(metric: Metric) -> Csv {
    let unit = metric.info().unit;
    let value = if unit.is_empty() { "value".to_string() } else { format!("value ({unit})") };
    Csv::new(&["start", "end", value.as_str(), "source"])
}

fn export_workouts(health: &dyn HealthStore, out: &mut Files<'_>) -> Result<()> {
    let mut workouts = health.workouts(Timestamp::MIN, Timestamp::MAX)?;
    workouts.retain(|w| !w.hidden);
    if workouts.is_empty() {
        return Ok(());
    }
    let mut csv = Csv::new(WORKOUT_COLUMNS);
    for workout in &workouts {
        csv.row(&workout_row(workout));
    }
    out.records(WORKOUTS, csv.finish(), workouts.len() as u64)
}

fn workout_row(workout: &Workout) -> Vec<String> {
    vec![
        workout.id.to_string(),
        word(&workout.activity),
        text(&workout.name),
        workout.start.to_string(),
        workout.end.to_string(),
        workout.offset_min.to_string(),
        number(workout.active_energy_kcal),
        number(workout.total_energy_kcal),
        number(workout.distance_m),
        number(workout.elevation_up_m),
        number(workout.avg_hr),
        number(workout.max_hr),
        number(workout.effort),
        workout.indoor.map(|b| b.to_string()).unwrap_or_default(),
        text(&workout.source),
        text(&workout.note),
    ]
}

fn export_doses(health: &dyn HealthStore, out: &mut Files<'_>) -> Result<()> {
    let mut doses = health.doses(Timestamp::MIN, Timestamp::MAX, None)?;
    doses.retain(|d| !d.hidden);
    if doses.is_empty() {
        return Ok(());
    }
    let names: HashMap<MedicationId, String> =
        health.list_medications()?.into_iter().map(|m| (m.id, m.name)).collect();
    let mut csv = Csv::new(DOSE_COLUMNS);
    for dose in &doses {
        let name = names.get(&dose.medication_id).map_or("", String::as_str);
        csv.row(&dose_row(dose, name));
    }
    out.records(DOSES, csv.finish(), doses.len() as u64)
}

fn dose_row(dose: &Dose, medication: &str) -> Vec<String> {
    vec![
        dose.at.to_string(),
        text(medication),
        word(&dose.status),
        number(dose.amount),
        dose.scheduled_at.map(|t| t.to_string()).unwrap_or_default(),
        word(&dose.origin),
        text(&dose.note),
        // Whether it was a source's event: its key is not written, but a
        // reader of this file -- this app included -- is told there was one.
        (!dose.key.is_empty()).to_string(),
        dose.medication_id.to_string(),
        dose.created_at.to_string(),
        dose.updated_at.to_string(),
        dose.id.to_string(),
    ]
}

fn export_labs(health: &dyn HealthStore, out: &mut Files<'_>) -> Result<()> {
    let labs = health.list_labs()?;
    if labs.is_empty() {
        return Ok(());
    }
    let mut csv = Csv::new(LAB_COLUMNS);
    for lab in &labs {
        csv.row(&lab_row(lab));
    }
    out.records(LABS, csv.finish(), labs.len() as u64)
}

fn lab_row(lab: &LabResult) -> Vec<String> {
    vec![
        lab.collected.to_string(),
        text(&lab.name),
        number(lab.value),
        text(&lab.unit),
        number(lab.ref_low),
        number(lab.ref_high),
        text(&lab.ref_text),
        word(&lab.flag),
        text(&lab.text),
        text(&lab.code),
        text(&lab.panel),
        text(&lab.lab),
        word(&lab.origin),
        lab.document_id.map(|d| d.to_string()).unwrap_or_default(),
        lab.collected_at.map(|t| t.to_string()).unwrap_or_default(),
        text(&lab.note),
        lab.created_at.to_string(),
        lab.updated_at.to_string(),
        lab.id.to_string(),
    ]
}

fn export_meals(
    store: &dyn JournalStore,
    health: &dyn HealthStore,
    out: &mut Files<'_>,
    opts: &Options,
) -> Result<()> {
    let meals = health.meals(Timestamp::MIN, Timestamp::MAX)?;
    if meals.is_empty() {
        return Ok(());
    }
    let mut csv = Csv::new(MEAL_COLUMNS);
    for meal in &meals {
        // The photograph is a file in the catalog, written beside the other
        // apps' files in `media/`, and only when attachments were asked for.
        let photo = match meal.photo {
            Some(file) => out.catalogued(store, file, opts)?.unwrap_or_default(),
            None => String::new(),
        };
        csv.row(&meal_row(meal, &photo)?);
    }
    out.records(MEALS, csv.finish(), meals.len() as u64)
}

/// The totals are for a person reading the file; reading it back uses the
/// items, which are in the last column of data as JSON.
fn meal_row(meal: &Meal, photo: &str) -> Result<Vec<String>> {
    let total = |pick: fn(&Nutrients) -> f64| {
        round(meal.items.iter().map(|item| pick(&item.nutrients)).sum::<f64>())
    };
    Ok(vec![
        meal.at.to_string(),
        meal.offset_min.to_string(),
        word(&meal.slot),
        text(&meal.name),
        total(|n| n.kcal),
        total(|n| n.protein_g),
        total(|n| n.carbs_g),
        total(|n| n.fat_g),
        text(&meal.note),
        photo.to_string(),
        if meal.items.is_empty() { String::new() } else { serde_json::to_string(&meal.items)? },
        meal.created_at.to_string(),
        meal.updated_at.to_string(),
        meal.id.to_string(),
    ])
}

/// What `document.json` says about one file: enough to find it and to read it
/// back, and not the blob's address, which is the vault's own.
#[derive(serde::Serialize, serde::Deserialize)]
struct FileOut {
    name: String,
    mime: String,
    size: u64,
    /// The file's name beside `document.json`, when it was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file: Option<String>,
}

fn export_documents(
    store: &dyn JournalStore,
    health: &dyn HealthStore,
    out: &mut Files<'_>,
    opts: &Options,
) -> Result<()> {
    for document in health.list_documents()? {
        let folder = format!("documents/{}", document.id);
        let mut written: BTreeSet<String> = BTreeSet::new();
        let mut files = Vec::with_capacity(document.files.len());
        for file in &document.files {
            let mut path = None;
            if opts.media {
                match crate::catalogued_bytes(store, file.file) {
                    Ok((_, body)) => {
                        let name = format!("{}-{}", file.file.short(), safe_name(&file.name));
                        // The same file twice under one name is one file.
                        if written.insert(name.clone()) {
                            out.bytes_at(&format!("{folder}/{name}"), &body)?;
                        }
                        path = Some(name);
                    }
                    // A file the vault has lost -- or one in a folder that is
                    // locked just now -- is not a reason to lose the rest;
                    // `document.json` says it was not written.
                    Err(e) => {
                        tracing::warn!(file = %file.file, error = %e, "a health file was lost");
                    }
                }
            }
            files.push(FileOut {
                name: file.name.clone(),
                mime: file.mime.clone(),
                size: file.size,
                file: path,
            });
        }
        // What a connector sent, beside the pages, under a name of its own.
        let source = match &document.source {
            Some(source) => {
                let mut path = None;
                if opts.media {
                    match store.get_blob(source.blob) {
                        Ok(body) => {
                            let name = format!("source-{}", safe_name(&source.name));
                            out.bytes_at(&format!("{folder}/{name}"), &body)?;
                            path = Some(name);
                        }
                        Err(e) => {
                            tracing::warn!(blob = %source.blob, error = %e, "a health record was lost");
                        }
                    }
                }
                Some(FileOut {
                    name: source.name.clone(),
                    mime: source.mime.clone(),
                    size: source.size,
                    file: path,
                })
            }
            None => None,
        };
        let mut json = serde_json::to_value(&document)?;
        if let Some(object) = json.as_object_mut() {
            object.insert("files".into(), serde_json::to_value(&files)?);
            match source {
                Some(source) => object.insert("source".into(), serde_json::to_value(&source)?),
                None => object.remove("source"),
            };
            // A provider's own id for the paper.
            object.remove("externalId");
        }
        let body = format!("{}\n", serde_json::to_string_pretty(&json)?);
        out.records(&format!("{folder}/{DOCUMENT}"), body, 1)?;
    }
    Ok(())
}

/// A number as a cell, or nothing for no number.
fn number(value: Option<f64>) -> String {
    value.map(|v| v.to_string()).unwrap_or_default()
}

/// A total, to two places, so that adding up a meal's items does not write
/// `205.00000000000003`.
fn round(value: f64) -> String {
    ((value * 100.0).round() / 100.0).to_string()
}

/// A cell of somebody's text. Written as it is: `Csv::row` puts the
/// apostrophe in front of any cell a spreadsheet would run, for every kind
/// of export alike, and `Row::get` takes it off again.
fn text(value: &str) -> String {
    value.to_string()
}

/// An enum's own word on the wire -- `running`, `taken` -- so the file says
/// what the app and the interface say.
fn word<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// [`word`], read back. `None` for a word the enum does not have.
fn unword<T: serde::de::DeserializeOwned>(word: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(word.trim().to_string())).ok()
}

// ---- reading --------------------------------------------------------------

/// Is the vault's own copy of this record there? `None` when it is not.
fn found<T>(result: Result<T>) -> Result<Option<T>> {
    match result {
        Ok(t) => Ok(Some(t)),
        Err(Error::NotFound { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// A cell of text, its apostrophe taken off and trimmed.
fn cell(row: &Row<'_>, column: &str) -> String {
    row.get(column).trim().to_string()
}

/// The records of a JSON sidecar, each read on its own: one that cannot be
/// understood costs that record and not the file, as the Feed's sidecar does.
fn records<T: serde::de::DeserializeOwned>(
    src: &Part<'_>,
    file: &str,
    key: &str,
    report: &mut Report,
) -> Vec<T> {
    let Some(source) = src.text(file) else { return Vec::new() };
    let mut value: serde_json::Value = match serde_json::from_str(source) {
        Ok(value) => value,
        Err(e) => {
            report.problem(file, e);
            return Vec::new();
        }
    };
    let Some(serde_json::Value::Array(list)) = value.get_mut(key).map(serde_json::Value::take)
    else {
        report.problem(file, format!("this file has no {key} list"));
        return Vec::new();
    };
    let mut out = Vec::with_capacity(list.len());
    for (n, item) in list.into_iter().enumerate() {
        match serde_json::from_value::<T>(item) {
            Ok(record) => out.push(record),
            Err(e) => report.problem(file, format!("{key} {} could not be read: {e}", n + 1)),
        }
    }
    out
}

/// Land each of `records` by id: one already here is left alone or written
/// over as `mode` says, one that fails `check` is named and left out.
fn land_all<T: Clone>(
    records: Vec<T>,
    file: &str,
    mode: Mode,
    report: &mut Report,
    check: impl Fn(&T) -> Option<&'static str>,
    existing: impl Fn(&T) -> Result<Option<T>>,
    put: impl Fn(&T) -> Result<()>,
) -> Result<()> {
    for (n, record) in records.into_iter().enumerate() {
        if let Some(why) = check(&record) {
            report.problem(file, format!("entry {}: {why}", n + 1));
            continue;
        }
        let mut landing = land(existing(&record)?, || record.clone(), mode);
        let Some(target) = landing.as_mut() else {
            report.landed(&landing);
            continue;
        };
        *target = record;
        put(target)?;
        report.landed(&landing);
    }
    Ok(())
}

fn read_medications(
    health: &dyn HealthStore,
    src: &Part<'_>,
    mode: Mode,
    report: &mut Report,
) -> Result<()> {
    let medications: Vec<Medication> = records(src, MEDICATIONS, "medications", report);
    land_all(
        medications,
        MEDICATIONS,
        mode,
        report,
        |m| m.name.trim().is_empty().then_some("a medication needs a name"),
        |m| found(health.get_medication(m.id)),
        |m| health.put_medication(m),
    )
}

fn read_conditions(
    health: &dyn HealthStore,
    src: &Part<'_>,
    mode: Mode,
    report: &mut Report,
) -> Result<()> {
    let conditions: Vec<Condition> = records(src, CONDITIONS, "conditions", report);
    land_all(
        conditions,
        CONDITIONS,
        mode,
        report,
        |c| c.name.trim().is_empty().then_some("a condition needs a name"),
        |c| found(health.get_condition(c.id)),
        |c| health.put_condition(c),
    )
}

fn read_allergies(
    health: &dyn HealthStore,
    src: &Part<'_>,
    mode: Mode,
    report: &mut Report,
) -> Result<()> {
    let allergies: Vec<Allergy> = records(src, ALLERGIES, "allergies", report);
    land_all(
        allergies,
        ALLERGIES,
        mode,
        report,
        |a| a.substance.trim().is_empty().then_some("an allergy needs a substance"),
        |a| found(health.get_allergy(a.id)),
        |a| health.put_allergy(a),
    )
}

fn read_foods(
    health: &dyn HealthStore,
    src: &Part<'_>,
    mode: Mode,
    report: &mut Report,
) -> Result<()> {
    let foods: Vec<Food> = records(src, FOODS, "foods", report);
    land_all(
        foods,
        FOODS,
        mode,
        report,
        |f| f.name.trim().is_empty().then_some("a food needs a name"),
        |f| found(health.get_food(f.id)),
        |f| health.put_food(f),
    )
}

/// Every `documents/<id>/document.json`, each with its files.
fn read_documents(
    store: &dyn JournalStore,
    health: &dyn HealthStore,
    src: &Part<'_>,
    mode: Mode,
    report: &mut Report,
) -> Result<()> {
    let names: Vec<&str> = src
        .files()
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("documents/") && name.ends_with(&format!("/{DOCUMENT}")))
        .collect();
    for name in names {
        let Some(source) = src.text(name) else { continue };
        let folder = name.strip_suffix(DOCUMENT).unwrap_or(name);
        let mut value: serde_json::Value = match serde_json::from_str(source) {
            Ok(value) => value,
            Err(e) => {
                report.problem(name, e);
                continue;
            }
        };
        // The files are described, not embedded: take them out before the
        // rest is read as the record it is.
        let described: Vec<FileOut> = value
            .as_object_mut()
            .and_then(|object| object.remove("files"))
            .and_then(|files| serde_json::from_value(files).ok())
            .unwrap_or_default();
        let sent: Option<FileOut> = value
            .as_object_mut()
            .and_then(|object| object.remove("source"))
            .and_then(|source| serde_json::from_value(source).ok());
        let mut document: HealthDocument = match serde_json::from_value(value) {
            Ok(document) => document,
            Err(e) => {
                report.problem(name, format!("this document could not be read: {e}"));
                continue;
            }
        };
        if document.title.trim().is_empty() {
            report.problem(name, "a document needs a title");
            continue;
        }
        let stored = found(health.get_document(document.id))?;
        let held_pages: Vec<DocFile> = stored.iter().flat_map(|d| d.files.clone()).collect();
        let held: Vec<FileId> = held_pages.iter().map(|f| f.file).collect();
        let mut landing = land(stored, || document.clone(), mode);
        let Some(target) = landing.as_mut() else {
            report.landed(&landing);
            continue;
        };
        // Whatever was being read out of it when it was exported is not
        // being read now.
        if document.extraction == ExtractionState::Running {
            document.extraction = ExtractionState::None;
        }
        document.files = Vec::with_capacity(described.len());
        for file in described {
            // A page the archive has no bytes for -- not written, when
            // attachments were not asked for -- is the page the replaced
            // document already had under that name and size, if it had one;
            // otherwise the document lands without it.
            let same = || held_pages.iter().find(|p| p.name == file.name && p.size == file.size);
            let Some(path) = file.file.as_deref().filter(|p| !p.contains('/')) else {
                document.files.extend(same().cloned());
                continue;
            };
            let Some(body) = src.get(&format!("{folder}{path}")) else {
                report.problem(name, format!("{path} is listed but is not in the archive"));
                document.files.extend(same().cloned());
                continue;
            };
            // Back into the files catalog, sealed, as Health keeps every
            // page.
            let Some(id) = crate::seal_into_catalog(
                store,
                body,
                &file.name,
                &file.mime,
                FileOrigin::Health,
                &held,
            )?
            else {
                report.problem(
                    name,
                    "this vault keeps no files, so the document's pages were left out",
                );
                break;
            };
            document.files.push(DocFile {
                file: id,
                name: file.name,
                mime: file.mime,
                size: body.len() as u64,
            });
        }
        document.source = None;
        if let Some(sent) = sent
            && let Some(path) = sent.file.as_deref().filter(|p| !p.contains('/'))
        {
            match src.get(&format!("{folder}{path}")) {
                Some(body) => {
                    document.source = Some(DocSource {
                        blob: store.put_blob(body)?,
                        name: sent.name,
                        mime: sent.mime,
                        size: body.len() as u64,
                    });
                }
                None => report.problem(name, format!("{path} is listed but is not in the archive")),
            }
        }
        *target = document;
        health.put_document(target)?;
        if let Some(catalog) = store.files() {
            let pages: Vec<FileId> = target.files.iter().map(|f| f.file).collect();
            catalog.set_refs(&RefOwner::new(RefKind::HealthDocument, target.id), &pages)?;
            crate::release_into_trash(store, FileOrigin::Health, &held, &pages)?;
        }
        report.landed(&landing);
    }
    Ok(())
}

/// A file's table, if it is there and has the column that says what it is.
fn table(src: &Part<'_>, file: &str, column: &str, report: &mut Report) -> Option<Table> {
    let table = Table::parse(src.text(file)?);
    if !table.has(column) {
        report.problem(file, format!("this file has no {column} column"));
        return None;
    }
    Some(table)
}

/// Names the first few rows of a file that could not be read and counts the
/// rest. See [`ROW_PROBLEMS`].
struct Unread {
    file: &'static str,
    count: usize,
}

impl Unread {
    fn new(file: &'static str) -> Self {
        Self { file, count: 0 }
    }

    /// `line` is the row's place in the table, the header being line one.
    fn row(&mut self, report: &mut Report, line: usize, why: impl std::fmt::Display) {
        self.count += 1;
        if self.count <= ROW_PROBLEMS {
            report.problem(self.file, format!("row {line}: {why}"));
        }
    }

    fn finish(self, report: &mut Report) {
        if self.count > ROW_PROBLEMS {
            report.problem(
                self.file,
                format!("{} more rows could not be read either", self.count - ROW_PROBLEMS),
            );
        }
    }
}

/// A row's id, or a new one for a row somebody typed in by hand -- new, and
/// new again each time the file is read, since there is nothing to know it by.
fn id_of<T: std::str::FromStr + Default>(row: &Row<'_>) -> std::result::Result<T, String>
where
    T::Err: std::fmt::Display,
{
    match row.get("id").trim() {
        "" => Ok(T::default()),
        id => id.parse::<T>().map_err(|e| format!("{id:?} is not an id: {e}")),
    }
}

/// One row of `labs.csv` as a lab result, or why it is not one.
fn read_lab(row: &Row<'_>, now: Timestamp) -> std::result::Result<LabResult, String> {
    let name = cell(row, "name");
    if name.is_empty() {
        return Err("a result needs a name".into());
    }
    let collected = row
        .parse::<Date>("collected")
        .ok_or_else(|| "a result needs the date it was collected".to_string())?;
    Ok(LabResult {
        id: id_of::<LabResultId>(row)?,
        code: cell(row, "code"),
        name,
        value: row.parse("value"),
        text: cell(row, "text"),
        unit: cell(row, "unit"),
        ref_low: row.parse("ref_low"),
        ref_high: row.parse("ref_high"),
        ref_text: cell(row, "ref_text"),
        flag: unword(row.get("flag")).unwrap_or_default(),
        collected,
        collected_at: row.parse("collected_at"),
        lab: cell(row, "lab"),
        panel: cell(row, "panel"),
        document_id: HealthDocumentId::parse(row.get("document_id").trim()).ok(),
        origin: unword(row.get("origin")).unwrap_or_default(),
        note: cell(row, "note"),
        created_at: row.parse("created_at").unwrap_or(now),
        updated_at: row.parse("updated_at").unwrap_or(now),
    })
}

fn read_labs(
    health: &dyn HealthStore,
    src: &Part<'_>,
    mode: Mode,
    report: &mut Report,
) -> Result<()> {
    let Some(table) = table(src, LABS, "collected", report) else { return Ok(()) };
    // A result's document is kept when this vault has it, now that the
    // documents have landed, and let go when it does not: an id that names
    // nothing is nothing to follow.
    let documents: BTreeSet<HealthDocumentId> =
        health.list_documents()?.into_iter().map(|d| d.id).collect();
    let now = Timestamp::now();
    let mut unread = Unread::new(LABS);
    let mut seen = BTreeSet::new();
    for (n, row) in table.rows().enumerate() {
        let mut lab = match read_lab(&row, now) {
            Ok(lab) => lab,
            Err(why) => {
                unread.row(report, n + 2, why);
                continue;
            }
        };
        if !seen.insert(lab.id) {
            report.problem(LABS, format!("row {}: {} is already in the file", n + 2, lab.id));
            continue;
        }
        lab.document_id = lab.document_id.filter(|id| documents.contains(id));
        let mut landing = land(found(health.get_lab(lab.id))?, || lab.clone(), mode);
        let Some(target) = landing.as_mut() else {
            report.landed(&landing);
            continue;
        };
        *target = lab;
        health.put_labs(std::slice::from_ref(target))?;
        report.landed(&landing);
    }
    unread.finish(report);
    Ok(())
}

/// The medications a dose may name, by id and by name.
struct Medications {
    ids: BTreeSet<MedicationId>,
    by_name: HashMap<String, MedicationId>,
}

impl Medications {
    fn of(health: &dyn HealthStore) -> Result<Self> {
        let list = health.list_medications()?;
        Ok(Self {
            ids: list.iter().map(|m| m.id).collect(),
            by_name: list.iter().map(|m| (m.name.trim().to_lowercase(), m.id)).collect(),
        })
    }

    /// The one a row means: its id when this vault has it, else its name.
    fn find(&self, row: &Row<'_>) -> Option<MedicationId> {
        MedicationId::parse(row.get("medication_id").trim())
            .ok()
            .filter(|id| self.ids.contains(id))
            .or_else(|| self.by_name.get(&cell(row, "medication").to_lowercase()).copied())
    }
}

/// One row of `doses.csv` as a dose, or why it is not one.
fn read_dose(
    row: &Row<'_>,
    medications: &Medications,
    now: Timestamp,
) -> std::result::Result<Dose, String> {
    let at = row
        .parse::<Timestamp>("at")
        .ok_or_else(|| "a dose needs the time it was taken".to_string())?;
    let medication_id =
        medications.find(row).ok_or_else(|| "it names no medication this vault has".to_string())?;
    Ok(Dose {
        id: id_of(row)?,
        medication_id,
        scheduled_at: row.parse("scheduled_at"),
        at,
        status: unword(row.get("status")).unwrap_or_default(),
        amount: row.parse("amount"),
        origin: unword(row.get("origin")).unwrap_or_default(),
        note: cell(row, "note"),
        key: String::new(),
        hidden: false,
        created_at: row.parse("created_at").unwrap_or(now),
        updated_at: row.parse("updated_at").unwrap_or(now),
    })
}

fn read_doses(
    health: &dyn HealthStore,
    src: &Part<'_>,
    mode: Mode,
    report: &mut Report,
) -> Result<()> {
    let Some(table) = table(src, DOSES, "at", report) else { return Ok(()) };
    let medications = Medications::of(health)?;
    let now = Timestamp::now();
    let mut unread = Unread::new(DOSES);
    let mut doses = Vec::new();
    let mut seen = BTreeSet::new();
    for (n, row) in table.rows().enumerate() {
        // A source's event: the phone brings it again, under a key this
        // import cannot make.
        if row.flag("keyed") {
            report.skipped += 1;
            continue;
        }
        match read_dose(&row, &medications, now) {
            Ok(dose) if !seen.insert(dose.id) => {
                report.problem(DOSES, format!("row {}: {} is already in the file", n + 2, dose.id));
            }
            Ok(dose) => doses.push(dose),
            Err(why) => unread.row(report, n + 2, why),
        }
    }
    unread.finish(report);
    let (Some(first), Some(last)) =
        (doses.iter().map(|d| d.at).min(), doses.iter().map(|d| d.at).max())
    else {
        return Ok(());
    };

    // What is here already, over the file's whole reach -- to just past the
    // last, since the range is open at its end.
    let to = last.checked_add(jiff::SignedDuration::from_secs(1)).unwrap_or(last);
    let here: HashMap<_, _> =
        health.doses(first, to, None)?.into_iter().map(|d| (d.id, d)).collect();

    let mut write = Vec::new();
    let mut counts = (0u64, 0u64, 0u64);
    for mut dose in doses {
        match here.get(&dose.id) {
            Some(existing) => {
                // A key this vault gave it is not the file's to take away.
                dose.key = existing.key.clone();
                if *existing == dose || mode == Mode::Skip {
                    counts.2 += 1;
                } else {
                    counts.1 += 1;
                    write.push(dose);
                }
            }
            None => {
                counts.0 += 1;
                write.push(dose);
            }
        }
    }
    for batch in write.chunks(DOSE_BATCH) {
        health.put_doses(batch)?;
    }
    // Counted once everything has landed: a batch that failed has returned
    // above, and the report it would have given is replaced by the error.
    report.added += counts.0;
    report.replaced += counts.1;
    report.skipped += counts.2;
    Ok(())
}

/// One row of `meals.csv` as a meal -- without its photograph -- or why it is
/// not one.
fn read_meal(row: &Row<'_>, now: Timestamp) -> std::result::Result<Meal, String> {
    let at = row
        .parse::<Timestamp>("at")
        .ok_or_else(|| "a meal needs the time it was eaten".to_string())?;
    let items = match row.get("items").trim() {
        "" => {
            // A table somebody typed: the totals are the meal.
            let totals = Nutrients {
                kcal: row.parse("kcal").unwrap_or_default(),
                protein_g: row.parse("protein_g").unwrap_or_default(),
                carbs_g: row.parse("carbs_g").unwrap_or_default(),
                fat_g: row.parse("fat_g").unwrap_or_default(),
                ..Nutrients::default()
            };
            let typed = ["kcal", "protein_g", "carbs_g", "fat_g"];
            if typed.iter().any(|c| row.parse::<f64>(c).is_some()) {
                vec![MealItem {
                    food_id: None,
                    name: cell(row, "name"),
                    amount: 1.0,
                    unit: "serving".into(),
                    grams: None,
                    nutrients: totals,
                    estimated: false,
                    doubt: String::new(),
                }]
            } else {
                Vec::new()
            }
        }
        json => serde_json::from_str::<Vec<MealItem>>(json)
            .map_err(|e| format!("its items could not be read: {e}"))?,
    };
    Ok(Meal {
        id: id_of::<MealId>(row)?,
        at,
        offset_min: row.parse("offset_min").unwrap_or(0),
        slot: unword(row.get("slot")).unwrap_or_default(),
        name: cell(row, "name"),
        items,
        photo: None,
        note: cell(row, "note"),
        created_at: row.parse("created_at").unwrap_or(now),
        updated_at: row.parse("updated_at").unwrap_or(now),
    })
}

fn read_meals(
    store: &dyn JournalStore,
    health: &dyn HealthStore,
    src: &Part<'_>,
    mode: Mode,
    report: &mut Report,
) -> Result<()> {
    let Some(table) = table(src, MEALS, "at", report) else { return Ok(()) };
    let now = Timestamp::now();
    let mut unread = Unread::new(MEALS);
    let mut seen = BTreeSet::new();
    for (n, row) in table.rows().enumerate() {
        let mut meal = match read_meal(&row, now) {
            Ok(meal) => meal,
            Err(why) => {
                unread.row(report, n + 2, why);
                continue;
            }
        };
        if !seen.insert(meal.id) {
            report.problem(MEALS, format!("row {}: {} is already in the file", n + 2, meal.id));
            continue;
        }
        let stored = found(health.get_meal(meal.id))?;
        let held: Vec<FileId> = stored.iter().filter_map(|m| m.photo).collect();
        let mut landing = land(stored, || meal.clone(), mode);
        let Some(target) = landing.as_mut() else {
            report.landed(&landing);
            continue;
        };
        // The photograph, when the archive has it: into the files catalog,
        // sealed, only now that the meal is going to be written.
        // An archive written without attachments has no photograph to give
        // back: the replaced meal's own is kept.
        let photo = Some(row.get("photo").trim()).filter(|p| !p.is_empty());
        meal.photo = match photo.and_then(|path| Some((path, src.get(path)?))) {
            Some((path, body)) => {
                let name = path.rsplit('/').next().unwrap_or(path);
                crate::seal_into_catalog(store, body, name, "", FileOrigin::Health, &held)?
            }
            None => held.first().copied(),
        };
        *target = meal;
        health.put_meal(target)?;
        if let Some(catalog) = store.files() {
            let photo: Vec<FileId> = target.photo.into_iter().collect();
            catalog.set_refs(&RefOwner::new(RefKind::Meal, target.id), &photo)?;
            crate::release_into_trash(store, FileOrigin::Health, &held, &photo)?;
        }
        report.landed(&landing);
    }
    unread.finish(report);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use karmayoga_core::health::{DoseStatus, LabFlag, LabOrigin, MealSlot};

    fn first_row(table: &Table) -> Row<'_> {
        table.rows().next().expect("a row")
    }

    #[test]
    fn enums_are_written_in_the_words_the_app_uses_and_read_back() {
        assert_eq!(word(&karmayoga_core::health::Activity::StairClimbing), "stair_climbing");
        assert_eq!(word(&DoseStatus::Skipped), "skipped");
        assert_eq!(word(&LabFlag::Abnormal), "abnormal");
        assert_eq!(word(&MealSlot::Breakfast), "breakfast");
        assert_eq!(unword::<DoseStatus>(" Skipped "), None, "the app's words, case and all");
        assert_eq!(unword::<DoseStatus>(" skipped "), Some(DoseStatus::Skipped));
        assert_eq!(unword::<LabOrigin>("fhir"), Some(LabOrigin::Fhir));
        assert_eq!(unword::<LabFlag>("nonsense"), None);
    }

    #[test]
    fn a_samples_header_names_the_unit_when_the_catalogue_has_one() {
        let table = sample_table(Metric::BloodGlucose).finish();
        let header = table.lines().next().unwrap();
        let unit = Metric::BloodGlucose.info().unit;
        if unit.is_empty() {
            assert_eq!(header, "start,end,value,source");
        } else {
            assert_eq!(header, format!("start,end,value ({unit}),source"));
        }
    }

    #[test]
    fn a_meal_is_summed_to_two_places_and_its_items_are_one_json_cell() {
        let now = Timestamp::from_second(1_800_000_000).unwrap();
        let item = |kcal: f64| MealItem {
            food_id: None,
            name: "Rice".into(),
            amount: 1.0,
            unit: "cup".into(),
            grams: None,
            nutrients: Nutrients { kcal, ..Nutrients::default() },
            estimated: false,
            doubt: String::new(),
        };
        let meal = Meal {
            id: MealId::new(),
            at: now,
            offset_min: 0,
            slot: MealSlot::Lunch,
            name: "Lunch".into(),
            items: vec![item(100.1), item(105.2)],
            photo: None,
            note: String::new(),
            created_at: now,
            updated_at: now,
        };
        let row = meal_row(&meal, "").unwrap();
        assert_eq!(row.len(), MEAL_COLUMNS.len());
        assert_eq!(row[4], "205.3", "no 205.30000000000001");
        let items: Vec<MealItem> = serde_json::from_str(&row[10]).unwrap();
        assert_eq!(items, meal.items);
        assert_eq!(round(0.1 + 0.2), "0.3");
    }

    #[test]
    fn a_hand_made_lab_row_lands_what_it_can() {
        let now = Timestamp::from_second(1_800_000_000).unwrap();
        let table = Table::parse("name,collected,value,flag\r\nTSH,2026-10-02,2.4,HIGH\r\n");
        let lab = read_lab(&first_row(&table), now).unwrap();
        assert_eq!((lab.name.as_str(), lab.value, lab.flag), ("TSH", Some(2.4), LabFlag::None));
        assert_eq!(lab.collected, jiff::civil::date(2026, 10, 2));
        assert_eq!((lab.created_at, lab.updated_at), (now, now));

        for bad in [
            "name,collected\r\n,2026-10-02\r\n",
            "name,collected\r\nTSH,\r\n",
            "name,collected\r\nTSH,yesterday\r\n",
            "name,collected,id\r\nTSH,2026-10-02,not-an-id\r\n",
        ] {
            let table = Table::parse(bad);
            assert!(read_lab(&first_row(&table), now).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_dose_names_its_medication_by_id_or_failing_that_by_name() {
        let now = Timestamp::from_second(1_800_000_000).unwrap();
        let known = MedicationId::new();
        let medications = Medications {
            ids: BTreeSet::from([known]),
            by_name: HashMap::from([("levothyroxine".to_string(), known)]),
        };
        let by = |csv: String| {
            let table = Table::parse(&csv);
            read_dose(&first_row(&table), &medications, now).map(|d| d.medication_id)
        };
        let at = "2026-11-05T14:00:00Z";
        assert_eq!(by(format!("at,medication_id\r\n{at},{known}\r\n")), Ok(known));
        assert_eq!(by(format!("at,medication\r\n{at}, Levothyroxine \r\n")), Ok(known));
        // An id this vault has not heard of falls back to the name beside it.
        let stranger = MedicationId::new();
        assert_eq!(
            by(format!("at,medication_id,medication\r\n{at},{stranger},LEVOTHYROXINE\r\n")),
            Ok(known)
        );
        assert!(by(format!("at,medication\r\n{at},Aspirin\r\n")).is_err());
        assert!(by("at,medication\r\nlater,Levothyroxine\r\n".to_string()).is_err());
    }

    #[test]
    fn a_dose_read_from_a_file_never_has_a_key() {
        let now = Timestamp::from_second(1_800_000_000).unwrap();
        let known = MedicationId::new();
        let medications = Medications { ids: BTreeSet::from([known]), by_name: HashMap::new() };
        let table = Table::parse(&format!(
            "at,medication_id,status,key\r\n2026-11-05T14:00:00Z,{known},skipped,a-key\r\n"
        ));
        let dose = read_dose(&first_row(&table), &medications, now).unwrap();
        assert!(dose.key.is_empty());
        assert_eq!(dose.status, DoseStatus::Skipped);
    }

    #[test]
    fn a_typed_meal_with_only_totals_becomes_one_item() {
        let now = Timestamp::from_second(1_800_000_000).unwrap();
        let table = Table::parse("at,name,kcal,carbs_g\r\n2026-11-05T12:00:00Z,Toast,180,30\r\n");
        let meal = read_meal(&first_row(&table), now).unwrap();
        assert_eq!(meal.items.len(), 1);
        assert_eq!((meal.items[0].nutrients.kcal, meal.items[0].nutrients.carbs_g), (180.0, 30.0));
        assert_eq!(meal.items[0].name, "Toast");

        let table = Table::parse("at,name\r\n2026-11-05T12:00:00Z,Water\r\n");
        assert!(read_meal(&first_row(&table), now).unwrap().items.is_empty());
        let table = Table::parse("at,items\r\n2026-11-05T12:00:00Z,not json\r\n");
        assert!(read_meal(&first_row(&table), now).is_err());
    }

    #[test]
    fn unread_rows_are_named_and_then_counted() {
        let mut report = Report::new("health");
        let mut unread = Unread::new(LABS);
        for line in 2..2 + ROW_PROBLEMS + 5 {
            unread.row(&mut report, line, "bad");
        }
        unread.finish(&mut report);
        assert_eq!(report.problems.len(), ROW_PROBLEMS + 1);
        assert!(report.problems.last().unwrap().contains("5 more rows"));
    }
}
