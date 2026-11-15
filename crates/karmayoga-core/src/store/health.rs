//! Storage for Health: series days of samples, sources, workouts,
//! medications and doses, conditions, lab results, documents, allergies,
//! foods, meals and the settings.
//!
//! # Why this is its own trait
//!
//! Money's reason: a backend that keeps journals as Markdown files has no
//! opinion about a heart rate, so the domain is reached through
//! [`JournalStore::health`](super::JournalStore::health), `None` by default,
//! and the interface reads [`Capabilities::health`](super::Capabilities::health)
//! to know whether to offer the app at all.
//!
//! # The rules an implementation must keep
//!
//! Every one of these is in the conformance suite (`conformance/health.rs`):
//!
//! - **Everything but the index is sealed.** Clear columns are ids, a
//!   series' keyed hash and its UTC day, timestamps (a workout's start and
//!   end, a dose's time, a meal's time, a food's last use, created/updated),
//!   a lab result's collection date, a document's date, a dose's
//!   medication id, and the keyed dedupe keys of workouts and doses. Never
//!   a metric's name, a value, a source name, a medication, a condition, a
//!   food or a word of a document.
//! - **A series day is two sealed payloads**, the samples and the summary,
//!   each bound to `(series, day)` under its own label, so one cannot be
//!   passed off as the other or as another day's.
//! - **[`HealthStore::put_series`] writes all of its rows or none.**
//! - **A workout's `key` is unique**, and so is a dose's non-empty `key`.
//!   [`HealthStore::put_workouts`] and [`HealthStore::put_doses`] write all of
//!   a batch or none, and refuse -- with
//!   [`Error::Invalid`](crate::error::Error::Invalid), writing nothing -- a
//!   row whose key is already held by a *different* id.
//! - **Range reads**: workouts overlapping `from..to` (`start < to`, `end >
//!   from`) in start order then id; doses and meals with `from <= at < to`
//!   in time order then id; series days with `from <= day <= to` in day
//!   order.
//! - **Deleting a medication takes its doses.** Deleting anything else
//!   touches nothing else: a lab result still pointing at a deleted
//!   document, a medication still naming a deleted condition, is the vault's
//!   to tidy.
//! - **[`HealthStore::blob_refs`] names every blob a health record holds**
//!   -- a document's files, a workout's route, a meal's photo -- or the
//!   garbage collector takes them.

use crate::error::Result;
use crate::health::{
    Allergy, Condition, Dose, Food, HealthDocument, HealthSettings, HealthSource, LabResult, Meal,
    Medication, SeriesDay, SeriesSummary, Workout,
};
use crate::id::{
    AllergyId, BlobId, ConditionId, DoseId, FoodId, HealthDocumentId, HealthSourceId, LabResultId,
    MealId, MedicationId, WorkoutId,
};
use jiff::Timestamp;
use jiff::civil::Date;

/// One series day to write: the clear index (`series`, `day`) and the two
/// payloads sealed beside it.
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesRow {
    /// The metric's keyed hash, computed by the vault. Opaque here.
    pub series: String,
    pub day: Date,
    pub data: SeriesDay,
    pub summary: SeriesSummary,
}

/// One series' extent, for the export and for "what has data".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesExtent {
    pub series: String,
    pub first: Date,
    pub last: Date,
    pub days: u64,
}

/// The persistence contract for Health.
pub trait HealthStore: Send + Sync {
    // ---- series ---------------------------------------------------------

    /// Insert or replace each row by `(series, day)`, all or nothing.
    fn put_series(&self, rows: &[SeriesRow]) -> Result<()>;
    /// The sample payloads of `series` with `from <= day <= to`, by day.
    fn series_days(&self, series: &str, from: Date, to: Date) -> Result<Vec<SeriesDay>>;
    /// The summaries of `series` with `from <= day <= to`, by day. Never
    /// opens a sample payload.
    fn series_summaries(&self, series: &str, from: Date, to: Date) -> Result<Vec<SeriesSummary>>;
    /// The latest day of `series` at or before `on`, if any.
    fn latest_series_day(&self, series: &str, on: Date) -> Result<Option<SeriesDay>>;
    /// Delete `series` days with `from <= day <= to`. Answers how many went.
    fn delete_series(&self, series: &str, from: Date, to: Date) -> Result<u64>;
    /// Every series with anything in it.
    fn series_extents(&self) -> Result<Vec<SeriesExtent>>;

    // ---- settings -------------------------------------------------------

    /// [`HealthSettings::default`] when never saved.
    fn health_settings(&self) -> Result<HealthSettings>;
    fn put_health_settings(&self, settings: &HealthSettings) -> Result<()>;

    // ---- sources --------------------------------------------------------

    fn list_sources(&self) -> Result<Vec<HealthSource>>;
    fn get_source(&self, id: HealthSourceId) -> Result<HealthSource>;
    /// Insert or replace by id.
    fn put_source(&self, source: &HealthSource) -> Result<()>;
    fn delete_source(&self, id: HealthSourceId) -> Result<()>;

    // ---- workouts -------------------------------------------------------

    /// Every workout overlapping `from..to`, in start order.
    fn workouts(&self, from: Timestamp, to: Timestamp) -> Result<Vec<Workout>>;
    fn get_workout(&self, id: WorkoutId) -> Result<Workout>;
    fn workout_by_key(&self, key: &str) -> Result<Option<Workout>>;
    /// Insert or replace by id, all or nothing; see the module docs on keys.
    fn put_workouts(&self, workouts: &[Workout]) -> Result<()>;
    fn delete_workout(&self, id: WorkoutId) -> Result<()>;

    // ---- medications and doses -----------------------------------------

    /// By sort order, then id.
    fn list_medications(&self) -> Result<Vec<Medication>>;
    fn get_medication(&self, id: MedicationId) -> Result<Medication>;
    fn put_medication(&self, medication: &Medication) -> Result<()>;
    /// Deletes its doses too.
    fn delete_medication(&self, id: MedicationId) -> Result<()>;

    /// Doses with `from <= at < to`, in time order; only one medication's
    /// when `medication` says so.
    fn doses(
        &self,
        from: Timestamp,
        to: Timestamp,
        medication: Option<MedicationId>,
    ) -> Result<Vec<Dose>>;
    fn get_dose(&self, id: DoseId) -> Result<Dose>;
    fn dose_by_key(&self, key: &str) -> Result<Option<Dose>>;
    /// Insert or replace by id, all or nothing; see the module docs on keys.
    fn put_doses(&self, doses: &[Dose]) -> Result<()>;
    fn delete_dose(&self, id: DoseId) -> Result<()>;

    // ---- conditions -----------------------------------------------------

    fn list_conditions(&self) -> Result<Vec<Condition>>;
    fn get_condition(&self, id: ConditionId) -> Result<Condition>;
    fn put_condition(&self, condition: &Condition) -> Result<()>;
    fn delete_condition(&self, id: ConditionId) -> Result<()>;

    // ---- lab results ----------------------------------------------------

    /// Every result, by collection date then id.
    fn list_labs(&self) -> Result<Vec<LabResult>>;
    fn get_lab(&self, id: LabResultId) -> Result<LabResult>;
    /// Insert or replace by id, all or nothing.
    fn put_labs(&self, labs: &[LabResult]) -> Result<()>;
    fn delete_lab(&self, id: LabResultId) -> Result<()>;

    // ---- documents ------------------------------------------------------

    /// Newest first: by date descending (undated last), then id descending.
    fn list_documents(&self) -> Result<Vec<HealthDocument>>;
    fn get_document(&self, id: HealthDocumentId) -> Result<HealthDocument>;
    fn put_document(&self, document: &HealthDocument) -> Result<()>;
    fn delete_document(&self, id: HealthDocumentId) -> Result<()>;

    // ---- allergies ------------------------------------------------------

    fn list_allergies(&self) -> Result<Vec<Allergy>>;
    fn get_allergy(&self, id: AllergyId) -> Result<Allergy>;
    fn put_allergy(&self, allergy: &Allergy) -> Result<()>;
    fn delete_allergy(&self, id: AllergyId) -> Result<()>;

    // ---- foods and meals ------------------------------------------------

    /// Every food, most recently used first (never used last), then id.
    fn list_foods(&self) -> Result<Vec<Food>>;
    fn get_food(&self, id: FoodId) -> Result<Food>;
    fn put_food(&self, food: &Food) -> Result<()>;
    fn delete_food(&self, id: FoodId) -> Result<()>;

    /// Meals with `from <= at < to`, in time order.
    fn meals(&self, from: Timestamp, to: Timestamp) -> Result<Vec<Meal>>;
    fn get_meal(&self, id: MealId) -> Result<Meal>;
    fn put_meal(&self, meal: &Meal) -> Result<()>;
    fn delete_meal(&self, id: MealId) -> Result<()>;

    // ---- blobs ----------------------------------------------------------

    /// Every blob a health record holds. See the module docs.
    fn blob_refs(&self) -> Result<Vec<BlobId>>;
}

/// Associated data bound into a series day's sample payload. Binds both
/// halves of the index, so a payload copied to another series or another
/// day will not open there.
pub fn health_series_aad(series: &str, day: Date) -> Vec<u8> {
    format!("everyday.health_series.v1:{series}:{day}").into_bytes()
}

/// The same for the summary beside it -- a different label, so the
/// summary of a day cannot stand in for its samples or the other way round.
pub fn health_summary_aad(series: &str, day: Date) -> Vec<u8> {
    format!("everyday.health_summary.v1:{series}:{day}").into_bytes()
}

/// The settings singleton.
pub fn health_settings_aad() -> Vec<u8> {
    b"everyday.health-settings.v1".to_vec()
}

/// Associated data bound into a record's ciphertext. See
/// [`entry_aad`](super::entry_aad) for why records are bound to their id.
pub fn health_source_aad(id: HealthSourceId) -> Vec<u8> {
    format!("everyday.health_source.v1:{id}").into_bytes()
}

pub fn workout_aad(id: WorkoutId) -> Vec<u8> {
    format!("everyday.workout.v1:{id}").into_bytes()
}

pub fn medication_aad(id: MedicationId) -> Vec<u8> {
    format!("everyday.medication.v1:{id}").into_bytes()
}

pub fn dose_aad(id: DoseId) -> Vec<u8> {
    format!("everyday.dose.v1:{id}").into_bytes()
}

pub fn condition_aad(id: ConditionId) -> Vec<u8> {
    format!("everyday.condition.v1:{id}").into_bytes()
}

pub fn lab_result_aad(id: LabResultId) -> Vec<u8> {
    format!("everyday.lab_result.v1:{id}").into_bytes()
}

pub fn health_document_aad(id: HealthDocumentId) -> Vec<u8> {
    format!("everyday.health_document.v1:{id}").into_bytes()
}

pub fn allergy_aad(id: AllergyId) -> Vec<u8> {
    format!("everyday.allergy.v1:{id}").into_bytes()
}

pub fn food_aad(id: FoodId) -> Vec<u8> {
    format!("everyday.food.v1:{id}").into_bytes()
}

pub fn meal_aad(id: MealId) -> Vec<u8> {
    format!("everyday.meal.v1:{id}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_label_is_distinct() {
        let id = uuid::Uuid::now_v7();
        let day = Date::constant(2026, 11, 15);
        let labels = [
            health_series_aad("abc", day),
            health_summary_aad("abc", day),
            health_settings_aad(),
            health_source_aad(HealthSourceId(id)),
            workout_aad(WorkoutId(id)),
            medication_aad(MedicationId(id)),
            dose_aad(DoseId(id)),
            condition_aad(ConditionId(id)),
            lab_result_aad(LabResultId(id)),
            health_document_aad(HealthDocumentId(id)),
            allergy_aad(AllergyId(id)),
            food_aad(FoodId(id)),
            meal_aad(MealId(id)),
        ];
        for (i, a) in labels.iter().enumerate() {
            for b in &labels[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
