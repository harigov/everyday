//! Health: what a watch and a phone measured, what somebody takes, eats and
//! lives with, and the papers their clinicians gave them.
//!
//! See `docs/plans/health.md` for the reasoning; this is the shape.
//!
//! ```text
//!   connectors ──▶ HealthBatch ──(Vault::ingest_health)──▶ series days, Workout, Dose
//!   (Apple export,                                          │
//!    Health Auto Export)                                    ▼
//!                                   report / sleep / glucose ──▶ what the screens draw
//!
//!   Condition ── CarePlan (targets, labs due, trackers)
//!   Medication ── Dose            LabResult ── HealthDocument (sealed files)
//!   Meal ── MealItem ── Food      Allergy
//! ```
//!
//! # Samples are not records
//!
//! A watch writes thousands of readings a day. They are kept as one sealed
//! [`SeriesDay`] per metric per UTC day (see [`chunk`]) -- the raw samples,
//! plus a small [`SeriesSummary`] of quarter-hour buckets with the source
//! priority already applied, so a year of resting heart rate is 365 small
//! reads and not a million rows. A sample has no id: it is identified by its
//! metric, start, end and value, which is what makes importing the same day
//! twice -- from the export file and from the phone -- harmless.
//!
//! # Units
//!
//! Every value is stored in its metric's canonical unit
//! ([`metric::MetricInfo::unit`]): kilograms, metres, kilocalories, mg/dL,
//! degrees Celsius, milliseconds of HRV. Conversion happens once on the way
//! in ([`units::to_canonical`]) and once on the way out, in the interface,
//! from [`HealthUnits`].
//!
//! # What is sealed
//!
//! Everything. In the clear only what an index needs: a series' keyed hash
//! (never the metric's name -- "this vault measures glucose" is itself a
//! diagnosis), its UTC day, record ids, timestamps, a dose's medication id,
//! a workout's or dose's keyed dedupe key.
//!
//! # Not a medical device
//!
//! Nothing here diagnoses, doses or alarms. Targets and thresholds are the
//! person's own (a template suggests, the person confirms), alerts are
//! notices after the fact, and a CGM's own alarms stay the safety alarm.

pub mod chunk;
pub mod connect;
pub mod glucose;
pub mod metric;
pub mod nutrition;
pub mod report;
pub mod rules;
pub mod schedule;
pub mod sleep;
pub mod templates;
pub mod units;

use crate::id::{
    AllergyId, BlobId, ConditionId, DoseId, FileId, FoodId, HealthDocumentId, HealthSourceId,
    ItemId, LabResultId, MealId, MedicationId, TrackerId, WorkoutId,
};
use jiff::Timestamp;
use jiff::civil::{Date, Time};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub use metric::Metric;

/// The label the vault derives its health subkey under: series hashes,
/// workout and dose dedupe keys. Never changes -- it is a crypto input.
pub const HEALTH_KEY_LABEL: &str = "karmayoga.health.key.v1";

/// `record_secrets.owner_kind` of the vault's food-lookup keys (one row,
/// owner id `"vault"`), as Money's Plaid keys are filed.
pub const HEALTH_KEYS_OWNER_KIND: &str = "health-keys";

/// Most samples one [`HealthBatch`] may carry. Health Auto Export with
/// "Batch Requests" on sends well under this; an export import is cut into
/// batches of [`IMPORT_BATCH`].
pub const MAX_BATCH_SAMPLES: usize = 500_000;
/// How many samples the Apple export reader hands over at a time.
pub const IMPORT_BATCH: usize = 50_000;
/// The longest a sample may last. Longer is refused: a range read looks back
/// no further than this before its start.
pub const MAX_SAMPLE_SECS: i64 = 36 * 3600;
/// The longest range one series query, report or list reads.
pub const MAX_RANGE_DAYS: i64 = 800;

// ---- samples ----------------------------------------------------------------

/// One reading as a connector or a person handed it over, before units are
/// made canonical and before it is filed into a [`SeriesDay`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RawSample {
    pub metric: Metric,
    pub start: Timestamp,
    /// Equal to `start` for an instant (a weight, a heart rate).
    pub end: Timestamp,
    pub value: f64,
    /// The unit `value` is in, as the source spelled it (`count/min`,
    /// `lb`, `mmol<180.1558800000541>/L`). Empty means canonical.
    #[serde(default)]
    pub unit: String,
    /// The device or app that measured it: "Hari's Apple Watch", "iPhone",
    /// "Dexcom G7", "Karma Yoga" for a reading typed in here.
    #[serde(default)]
    pub source: String,
    /// The UTC offset where it was measured, in minutes -- what decides
    /// which local day it belongs to.
    #[serde(default)]
    pub offset_min: i16,
}

/// One sample inside a [`SeriesDay`]: canonical value, source by index into
/// the day's own source table. Compact on purpose -- a busy day of heart
/// rate is tens of thousands of these.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Sample {
    /// Seconds since the epoch.
    #[serde(rename = "s")]
    pub start: i64,
    /// Seconds after `start`; 0 for an instant.
    #[serde(rename = "d")]
    pub secs: u32,
    #[serde(rename = "v")]
    pub value: f64,
    /// Index into [`SeriesDay::sources`].
    #[serde(rename = "i")]
    pub source: u16,
    #[serde(rename = "o")]
    pub offset_min: i16,
}

/// Every sample of one metric that started on one UTC day.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SeriesDay {
    pub metric: Option<Metric>,
    pub day: Option<Date>,
    /// Source names, indexed by [`Sample::source`].
    #[serde(default)]
    pub sources: Vec<String>,
    /// In start order, then end, then value. No two equal in
    /// (start, secs, value) -- see [`chunk::merge`].
    #[serde(default)]
    pub samples: Vec<Sample>,
    /// Samples a person deleted that a source may send again. A phone's
    /// "Previous 7 Days" automation re-sends the week every day, and
    /// without these a deleted glucose spike would be back by morning.
    /// [`chunk::merge`] refuses anything matching one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tombstones: Vec<Tombstone>,
}

/// The identity of a deleted sample: what [`chunk::merge`] matches on.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Tombstone {
    #[serde(rename = "s")]
    pub start: i64,
    #[serde(rename = "d")]
    pub secs: u32,
    #[serde(rename = "v")]
    pub value: f64,
}

/// One quarter hour of a [`SeriesDay`], with the source priority applied:
/// for a cumulative metric only the highest-priority source that reported
/// anything in the quarter counts; for any other, every source does.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Bucket {
    /// 0..96, the quarter hour of the UTC day.
    #[serde(rename = "q")]
    pub quarter: u8,
    #[serde(rename = "n")]
    pub count: u32,
    #[serde(rename = "s")]
    pub sum: f64,
    #[serde(rename = "lo")]
    pub min: f64,
    #[serde(rename = "hi")]
    pub max: f64,
    /// The value of the sample that started last in the quarter.
    #[serde(rename = "l")]
    pub last: f64,
    /// Seconds covered by samples (for coverage, e.g. CGM wear).
    #[serde(rename = "c")]
    pub covered: u32,
}

/// The small, separately sealed summary of a [`SeriesDay`] that range
/// queries read instead of the samples.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SeriesSummary {
    pub metric: Option<Metric>,
    pub day: Option<Date>,
    /// Only quarters with something in them, in order.
    #[serde(default)]
    pub buckets: Vec<Bucket>,
    /// The UTC offset most samples of the day carried -- what a local-day
    /// total is cut by.
    #[serde(default)]
    pub offset_min: i16,
}

/// One sample as the interface and the assistant see it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SamplePoint {
    pub start: Timestamp,
    pub end: Timestamp,
    pub value: f64,
    pub source: String,
}

/// A reading typed in by hand: a weight, a fingerstick, a blood pressure.
/// `value` is in `unit` (empty for canonical); the vault converts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ManualReading {
    pub metric: Metric,
    pub at: Timestamp,
    pub value: f64,
    #[serde(default)]
    pub unit: String,
    /// The diastolic half, when `metric` is the systolic one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub second: Option<f64>,
    /// IANA zone the reading was taken in; empty is the system's.
    #[serde(default)]
    pub tz: String,
}

/// How a series query adds samples up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Stat {
    /// The metric's own default ([`metric::MetricInfo::stat`]).
    #[default]
    Auto,
    Sum,
    Mean,
    Min,
    Max,
    Last,
    Count,
    /// Share of covered time (or of readings, for a series without
    /// durations) inside `low..=high`, 0..=1. Whole canonical units, so the
    /// enum stays `Eq` for [`crate::tracker::TrackerSource`].
    InRange {
        low: i32,
        high: i32,
    },
}

/// How wide one point of a series answer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum BucketSize {
    /// Every sample, no adding up. Only for ranges of at most 3 days.
    Raw,
    QuarterHour,
    Hour,
    #[default]
    Day,
    Week,
    Month,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SeriesQuery {
    pub metric: Metric,
    /// Inclusive local dates.
    pub from: Date,
    pub to: Date,
    /// IANA zone; empty is the system's.
    #[serde(default)]
    pub tz: String,
    #[serde(default)]
    pub bucket: BucketSize,
    #[serde(default)]
    pub stat: Stat,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SeriesPoint {
    /// Start of the bucket (or the sample, for `Raw`).
    pub at: Timestamp,
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SeriesResult {
    pub metric: Metric,
    /// Canonical unit of every value.
    pub unit: String,
    pub stat: Stat,
    pub points: Vec<SeriesPoint>,
    /// The latest sample at or before the range's end, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<SamplePoint>,
}

// ---- sources and ingest -----------------------------------------------------

/// What kind of connector a [`HealthSource`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum Connector {
    /// Apple Health's `export.zip`, imported from a file.
    #[default]
    AppleExport,
    /// The Health Auto Export iOS app's REST API automations, pushed to
    /// `/v1/health/hae/<source id>` over web access.
    HealthAutoExport,
    /// What a person types in here.
    Manual,
}

/// One connection health data arrives through. What a person chose (name,
/// paused) and what arrival learned (`status`) live side by side; both are
/// written inside the vault's exclusive write, so neither overwrites the
/// other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct HealthSource {
    pub id: HealthSourceId,
    pub connector: Connector,
    pub name: String,
    /// Pushes to a paused source are refused with `paused`.
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub status: SourceStatus,
    /// The registry id of the token minted for this source's phone push
    /// (`health_push_setup`), empty when none. A push is accepted only
    /// from that token, so one phone's token cannot write into another
    /// source. Written only by `Vault::set_health_source_token`.
    #[serde(default)]
    pub token_id: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SourceStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_received_at: Option<Timestamp>,
    /// The newest sample time anything from this source carried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newest_sample_at: Option<Timestamp>,
    #[serde(default)]
    pub last_error: String,
    #[serde(default)]
    pub samples_total: u64,
    /// Health Auto Export automations heard from, by `automation-id`.
    #[serde(default)]
    pub automations: Vec<AutomationSeen>,
    /// Device and app names this source has brought, for the priority list.
    #[serde(default)]
    pub devices: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AutomationSeen {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// `automation-period` as sent ("Since Last Sync", "Previous 7 Days").
    #[serde(default)]
    pub period: String,
    /// `automation-aggregation` as sent.
    #[serde(default)]
    pub aggregation: String,
    #[serde(default)]
    pub last_at: Option<Timestamp>,
    #[serde(default)]
    pub last_count: u64,
}

/// The headers Health Auto Export sends beside a push, as the server route
/// read them. Every field is optional: a hand-made `curl` sends none.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PushHeaders {
    #[serde(default)]
    pub automation_id: String,
    #[serde(default)]
    pub automation_name: String,
    #[serde(default)]
    pub automation_period: String,
    #[serde(default)]
    pub automation_aggregation: String,
    #[serde(default)]
    pub session_id: String,
}

/// What a connector hands the vault: everything it read, in the source's
/// own units, plus what it could not place.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct HealthBatch {
    #[serde(default)]
    pub samples: Vec<RawSample>,
    #[serde(default)]
    pub workouts: Vec<RawWorkout>,
    #[serde(default)]
    pub doses: Vec<RawDose>,
    #[serde(default)]
    pub documents: Vec<RawDocument>,
    /// External types not in the catalogue, with how many of each were
    /// passed over: nothing disappears without being counted.
    #[serde(default)]
    pub skipped: BTreeMap<String, u64>,
    /// Entries that were malformed (no date, no value).
    #[serde(default)]
    pub rejected: u64,
}

impl HealthBatch {
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
            && self.workouts.is_empty()
            && self.doses.is_empty()
            && self.documents.is_empty()
    }

    pub fn skip(&mut self, what: &str) {
        *self.skipped.entry(what.to_string()).or_default() += 1;
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RawWorkout {
    pub activity: Activity,
    /// What the source called it ("Outdoor Run"). Empty is the activity's
    /// own name.
    #[serde(default)]
    pub name: String,
    pub start: Timestamp,
    pub end: Timestamp,
    #[serde(default)]
    pub offset_min: i16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_energy_kcal: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_energy_kcal: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elevation_up_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_hr: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_hr: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indoor: Option<bool>,
    #[serde(default)]
    pub source: String,
    /// The source's own id for it (Health Auto Export's `id`). Sealed.
    #[serde(default)]
    pub external_id: String,
    /// GPS points, when the source sent them.
    #[serde(default)]
    pub route: Vec<RoutePoint>,
    /// Heart rate during the workout, if sent with it rather than as
    /// ordinary samples. Filed into the heart-rate series on arrival.
    #[serde(default)]
    pub heart_rate: Vec<RawSample>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RoutePoint {
    pub at: Timestamp,
    pub lat: f64,
    pub lon: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt: Option<f64>,
}

/// A dose event a source recorded (Apple Health medications, iOS 26).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RawDose {
    /// The medication's name as the source shows it.
    pub name: String,
    #[serde(default)]
    pub codes: Vec<Code>,
    pub at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<Timestamp>,
    pub status: DoseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<f64>,
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub source: String,
}

/// A clinical record a connector found -- a FHIR resource file from the
/// Apple export's `clinical-records/`, an ECG.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RawDocument {
    pub title: String,
    pub kind: DocKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<Date>,
    #[serde(default)]
    pub provider: String,
    pub file_name: String,
    pub mime: String,
    /// The file itself. Not serialised to the interface.
    #[serde(skip)]
    pub bytes: Vec<u8>,
    #[serde(default)]
    pub fhir: Vec<FhirEntry>,
    /// Lab results read out of it, for the person to accept.
    #[serde(default)]
    pub labs: Vec<LabResult>,
    /// A stable identity (the FHIR resource id), so importing the same
    /// export twice does not add the document twice.
    #[serde(default)]
    pub external_id: String,
}

/// What [`crate::vault::Vault::ingest_health`] did with a batch.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct HealthIngestReport {
    pub samples_added: u64,
    pub samples_duplicate: u64,
    pub samples_rejected: u64,
    pub workouts_added: u64,
    pub workouts_updated: u64,
    pub doses_added: u64,
    /// Dose events whose medication matched nothing on the list.
    pub doses_unmatched: u64,
    pub documents_added: u64,
    #[serde(default)]
    pub skipped: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<Timestamp>,
}

impl HealthIngestReport {
    /// Fold another report into this one (an import is many batches).
    pub fn absorb(&mut self, other: HealthIngestReport) {
        self.samples_added += other.samples_added;
        self.samples_duplicate += other.samples_duplicate;
        self.samples_rejected += other.samples_rejected;
        self.workouts_added += other.workouts_added;
        self.workouts_updated += other.workouts_updated;
        self.doses_added += other.doses_added;
        self.doses_unmatched += other.doses_unmatched;
        self.documents_added += other.documents_added;
        for (k, v) in other.skipped {
            *self.skipped.entry(k).or_default() += v;
        }
        self.first = match (self.first, other.first) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        self.last = match (self.last, other.last) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
    }
}

/// Where an Apple export import has got to.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ImportStatus {
    /// `idle`, `running`, `done`, `failed`, `cancelled`.
    pub state: String,
    #[serde(default)]
    pub file_name: String,
    /// Bytes of `export.xml` read so far, and its size.
    #[serde(default)]
    pub read: u64,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub report: HealthIngestReport,
    #[serde(default)]
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Timestamp>,
}

// ---- settings ---------------------------------------------------------------

/// How values are shown. Storage is always canonical.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct HealthUnits {
    /// `mg/dL` or `mmol/L`.
    pub glucose: String,
    /// `kg` or `lb`.
    pub weight: String,
    /// `km` or `mi`.
    pub distance: String,
    /// `kcal` or `kJ`.
    pub energy: String,
    /// `C` or `F`.
    pub temperature: String,
    /// `cm` or `in`.
    pub length: String,
}

impl Default for HealthUnits {
    fn default() -> Self {
        Self {
            glucose: "mg/dL".into(),
            weight: "kg".into(),
            distance: "km".into(),
            energy: "kcal".into(),
            temperature: "C".into(),
            length: "cm".into(),
        }
    }
}

/// Daily activity goals, Apple's three rings plus steps and sleep.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ActivityGoals {
    pub move_kcal: f64,
    pub exercise_min: f64,
    pub stand_hours: f64,
    pub steps: f64,
    pub sleep_hours: f64,
}

impl Default for ActivityGoals {
    fn default() -> Self {
        Self {
            move_kcal: 500.0,
            exercise_min: 30.0,
            stand_hours: 12.0,
            steps: 8000.0,
            sleep_hours: 8.0,
        }
    }
}

/// Daily nutrition targets. `None` is "no target".
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct NutritionTargets {
    #[serde(default)]
    pub kcal: Option<f64>,
    #[serde(default)]
    pub protein_g: Option<f64>,
    #[serde(default)]
    pub carbs_g: Option<f64>,
    #[serde(default)]
    pub fat_g: Option<f64>,
    #[serde(default)]
    pub fiber_g: Option<f64>,
    #[serde(default)]
    pub sodium_mg: Option<f64>,
    /// Per-meal carbohydrate limit, for diabetes.
    #[serde(default)]
    pub carbs_per_meal_g: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct HealthSettings {
    #[serde(default)]
    pub units: HealthUnits,
    #[serde(default)]
    pub goals: ActivityGoals,
    #[serde(default)]
    pub nutrition: NutritionTargets,
    /// Source (device or app) names, most trusted first. A cumulative
    /// metric's quarter hour counts only the first of these that reported
    /// in it; names not listed come after, in [`chunk::default_rank`]'s
    /// order. Empty means that order for everything.
    #[serde(default)]
    pub priority: Vec<String>,
    /// Per-metric overrides of `priority`.
    #[serde(default)]
    pub priority_by_metric: BTreeMap<Metric, Vec<String>>,
    /// Glucose alert thresholds the person set, canonical mg/dL. Notices
    /// after the fact, never alarms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glucose_low: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glucose_high: Option<f64>,
    /// Remind at a dose's time; off silences every medication.
    #[serde(default = "yes")]
    pub dose_reminders: bool,
    /// Warn when no push has arrived from a Health Auto Export source for
    /// this many hours. 0 never warns.
    #[serde(default = "stale_hours")]
    pub stale_hours: u32,
}

fn yes() -> bool {
    true
}

fn stale_hours() -> u32 {
    24
}

/// Written out rather than derived: a derived default would give
/// `dose_reminders: false` and `stale_hours: 0`, and a vault that never
/// saved its settings would then never remind anybody of anything.
impl Default for HealthSettings {
    fn default() -> Self {
        Self {
            units: HealthUnits::default(),
            goals: ActivityGoals::default(),
            nutrition: NutritionTargets::default(),
            priority: Vec::new(),
            priority_by_metric: BTreeMap::new(),
            glucose_low: None,
            glucose_high: None,
            dose_reminders: yes(),
            stale_hours: stale_hours(),
        }
    }
}

/// What the interface may know about the food-lookup keys: whether one is
/// set, and its last four characters. Never the key.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct HealthKeysInfo {
    pub fdc_key_set: bool,
    #[serde(default)]
    pub fdc_key_hint: String,
}

/// The sealed secret behind [`HealthKeysInfo`]. `Debug` is written by
/// hand so a key never reaches a log, a panic message or a crash report.
#[derive(Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthKeys {
    /// A USDA FoodData Central (api.data.gov) key. Empty uses `DEMO_KEY`,
    /// which is rate-limited hard.
    #[serde(default)]
    pub fdc_key: String,
}

impl std::fmt::Debug for HealthKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let set = if self.fdc_key.is_empty() { "unset" } else { "set" };
        f.debug_struct("HealthKeys").field("fdc_key", &set).finish()
    }
}

// ---- workouts ---------------------------------------------------------------

/// The kinds of workout told apart. Apple has some eighty; the rest are
/// [`Activity::Other`] with the source's name kept on the workout.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    Walking,
    Running,
    Cycling,
    Swimming,
    Hiking,
    Rowing,
    Elliptical,
    StairClimbing,
    StrengthTraining,
    FunctionalTraining,
    CoreTraining,
    Hiit,
    Yoga,
    Pilates,
    Dance,
    MartialArts,
    Tennis,
    Soccer,
    Basketball,
    Golf,
    Cooldown,
    Mindfulness,
    #[default]
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Workout {
    pub id: WorkoutId,
    pub activity: Activity,
    #[serde(default)]
    pub name: String,
    pub start: Timestamp,
    pub end: Timestamp,
    #[serde(default)]
    pub offset_min: i16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_energy_kcal: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_energy_kcal: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elevation_up_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_hr: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_hr: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indoor: Option<bool>,
    #[serde(default)]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<HealthSourceId>,
    /// The source's own id. Sealed; blanked in what commands answer.
    #[serde(default)]
    pub external_id: String,
    /// Sealed blob of the route's [`RoutePoint`]s as JSON, if it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<BlobId>,
    #[serde(default)]
    pub note: String,
    /// Keyed hash of activity, start and end (see [`HEALTH_KEY_LABEL`]),
    /// set by the vault. The clear dedupe column.
    #[serde(default)]
    pub key: String,
    /// Deleted by the person, kept so the key still matches when the phone
    /// sends it again. Left out of every list and report.
    #[serde(default)]
    pub hidden: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// A workout with what its screen draws.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct WorkoutDetail {
    pub workout: Workout,
    pub heart_rate: Vec<SamplePoint>,
    pub route: Vec<RoutePoint>,
}

// ---- medications ------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum MedKind {
    #[default]
    Prescription,
    OverTheCounter,
    Supplement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum MedStatus {
    #[default]
    Active,
    Paused,
    Stopped,
}

/// When a medication is taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum MedSchedule {
    /// Every day at these local times.
    Daily { times: Vec<Time> },
    /// On these weekdays (1 = Monday .. 7 = Sunday) at these times.
    Weekly { days: Vec<u8>, times: Vec<Time> },
    /// Every `n` days counted from `from`, at these times.
    EveryNDays { n: u32, from: Date, times: Vec<Time> },
    /// Only when needed; nothing is ever due.
    AsNeeded,
}

impl Default for MedSchedule {
    fn default() -> Self {
        MedSchedule::Daily { times: vec![Time::constant(8, 0, 0, 0)] }
    }
}

/// A timing rule a template suggests and the person confirms.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum MedRule {
    /// Nothing to eat for `minutes` after the dose (levothyroxine: 30-60).
    EmptyStomach { minutes: u32 },
    /// Take with a meal: a meal within `minutes` either side.
    WithFood { minutes: u32 },
    /// Keep at least `hours` from any medication or supplement carrying one
    /// of `tags` ("calcium", "iron") -- either order.
    ApartFrom { tags: Vec<String>, hours: f64 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct DoseChange {
    pub on: Date,
    pub dose: f64,
    #[serde(default)]
    pub strength: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Supply {
    /// Doses (in `dose_unit`) left.
    pub remaining: f64,
    /// Remind to refill when this many days are left.
    #[serde(default = "seven")]
    pub refill_days: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refill: Option<Date>,
}

fn seven() -> u32 {
    7
}

/// A coded identity from a terminology: RxNorm for a medication, LOINC for
/// a lab, ICD-10 or SNOMED CT for a condition. Optional everywhere.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Code {
    /// `rxnorm`, `loinc`, `icd10`, `snomed`, or a FHIR system URL.
    pub system: String,
    pub code: String,
    #[serde(default)]
    pub display: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Medication {
    pub id: MedicationId,
    pub name: String,
    #[serde(default)]
    pub kind: MedKind,
    /// "tablet", "capsule", "injection", "drops".
    #[serde(default)]
    pub form: String,
    /// "50 mcg", "500 mg".
    #[serde(default)]
    pub strength: String,
    /// How much one take is, in `dose_unit`.
    #[serde(default = "one")]
    pub dose: f64,
    #[serde(default)]
    pub dose_unit: String,
    #[serde(default)]
    pub schedule: MedSchedule,
    #[serde(default)]
    pub rules: Vec<MedRule>,
    /// What other medications' [`MedRule::ApartFrom`] match against:
    /// "calcium", "iron", "thyroid", "insulin".
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub status: MedStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<Date>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<Date>,
    #[serde(default)]
    pub stop_reason: String,
    /// Every change of dose or strength, oldest first -- what a lab recheck
    /// after a dose change is counted from.
    #[serde(default)]
    pub history: Vec<DoseChange>,
    #[serde(default)]
    pub condition_ids: Vec<ConditionId>,
    #[serde(default)]
    pub prescriber: String,
    #[serde(default)]
    pub pharmacy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supply: Option<Supply>,
    /// Remind at each due time (subject to [`HealthSettings::dose_reminders`]).
    #[serde(default = "yes")]
    pub remind: bool,
    #[serde(default)]
    pub codes: Vec<Code>,
    /// Names a source's dose events used for this medication, so the next
    /// one matches without asking.
    #[serde(default)]
    pub source_names: Vec<String>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub sort_order: i64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

fn one() -> f64 {
    1.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum DoseStatus {
    #[default]
    Taken,
    Skipped,
    /// Past its window with nothing logged. Written only by the reminder
    /// sweep, never by a person.
    Missed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum DoseOrigin {
    #[default]
    Manual,
    /// From a notification's "Taken" button.
    Reminder,
    /// A connector's dose event.
    Source,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Dose {
    pub id: DoseId,
    pub medication_id: MedicationId,
    /// The due time this answers, if it answers one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<Timestamp>,
    /// When it was taken (or skipped).
    pub at: Timestamp,
    #[serde(default)]
    pub status: DoseStatus,
    /// In the medication's `dose_unit`; `None` is the medication's dose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<f64>,
    #[serde(default)]
    pub origin: DoseOrigin,
    #[serde(default)]
    pub note: String,
    /// Keyed dedupe key for a source's dose event; empty otherwise.
    #[serde(default)]
    pub key: String,
    /// A source's dose event the person deleted, kept so its key still
    /// matches when the phone sends it again. Left out of every list.
    #[serde(default)]
    pub hidden: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// One due time on one day, and what became of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct DueDose {
    pub medication_id: MedicationId,
    pub name: String,
    pub due_at: Timestamp,
    /// The dose logged against it, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dose: Option<Dose>,
    /// Past the window (two hours) with nothing logged.
    #[serde(default)]
    pub overdue: bool,
}

/// A timing rule that a day's doses and meals broke.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RuleAlert {
    pub medication_id: MedicationId,
    pub at: Timestamp,
    pub rule: MedRule,
    /// "Breakfast 10 minutes after levothyroxine (wait 30)".
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meal_id: Option<MealId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other_medication_id: Option<MedicationId>,
}

/// Taken against due over a range.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Adherence {
    pub due: u32,
    pub taken: u32,
    pub skipped: u32,
    pub missed: u32,
    /// Taken within an hour of the due time.
    pub on_time: u32,
}

// ---- allergies --------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum AllergyKind {
    #[default]
    Drug,
    Food,
    Environment,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    #[default]
    Mild,
    Moderate,
    Severe,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Allergy {
    pub id: AllergyId,
    pub substance: String,
    #[serde(default)]
    pub kind: AllergyKind,
    #[serde(default)]
    pub reaction: String,
    #[serde(default)]
    pub severity: Severity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub noted: Option<Date>,
    #[serde(default)]
    pub notes: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

// ---- conditions -------------------------------------------------------------

/// The built-in condition templates. Data, not code paths: applying one
/// copies its [`CarePlan`] onto a [`Condition`], which is the person's to
/// edit from then on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum TemplateKey {
    Type1Diabetes,
    Type2Diabetes,
    Hypothyroidism,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ConditionStatus {
    #[default]
    Active,
    Remission,
    Resolved,
}

/// What a target or a "watch" entry measures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Measure {
    Metric {
        metric: Metric,
        stat: Stat,
    },
    /// A lab test, by LOINC code.
    Lab {
        code: String,
    },
}

/// "Time in range 70-180 at least 70% of the time", "A1c below 7%", "TSH
/// inside the lab's own range".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct CareTarget {
    pub label: String,
    pub measure: Measure,
    /// Canonical units. For an `InRange` stat these are shares, 0..=1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high: Option<f64>,
    /// For a lab: compare against each result's own reference range rather
    /// than `low`/`high`.
    #[serde(default)]
    pub use_reference_range: bool,
    /// Days the value is averaged over (14 for time in range, 0 = latest).
    #[serde(default)]
    pub window_days: u32,
    /// Why, in a sentence, with where the default came from.
    #[serde(default)]
    pub note: String,
}

/// A test that comes due on a schedule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct LabSchedule {
    /// LOINC, or a word for an exam with none ("eye-exam", "foot-exam").
    pub code: String,
    pub name: String,
    /// Due this many days after the last result. 0 = never by time alone.
    #[serde(default)]
    pub every_days: u32,
    /// Also due this many days after any dose change of a medication
    /// carrying `after_change_tag` (hypothyroidism: 42 after a
    /// levothyroxine change).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_change_days: Option<u32>,
    #[serde(default)]
    pub after_change_tag: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct CarePlan {
    /// What the condition's dashboard shows, in order.
    #[serde(default)]
    pub watch: Vec<Measure>,
    #[serde(default)]
    pub targets: Vec<CareTarget>,
    #[serde(default)]
    pub labs: Vec<LabSchedule>,
    /// Medication timing rules the template suggested, keyed by the tag of
    /// the medications they apply to ("thyroid" → empty stomach, apart from
    /// calcium and iron). Offered when such a medication is added.
    #[serde(default)]
    pub med_rules: BTreeMap<String, Vec<MedRule>>,
    /// Symptom trackers and target trackers this plan created, so they show
    /// on the Overview and can be removed with it.
    #[serde(default)]
    pub tracker_ids: Vec<TrackerId>,
    /// The template version the plan was copied from. A newer template is
    /// offered, never applied.
    #[serde(default)]
    pub template_version: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    pub id: ConditionId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<TemplateKey>,
    #[serde(default)]
    pub status: ConditionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnosed: Option<Date>,
    #[serde(default)]
    pub codes: Vec<Code>,
    #[serde(default)]
    pub plan: CarePlan,
    #[serde(default)]
    pub notes: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// A template as the "Add a condition" screen lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TemplateInfo {
    pub key: TemplateKey,
    pub name: String,
    pub summary: String,
    pub version: u32,
    /// The plan it would copy, for the checklist the person confirms.
    pub plan: CarePlan,
    /// Symptom trackers it would create (names, 0-10 scales).
    pub symptoms: Vec<String>,
    /// Codes the condition would carry.
    pub codes: Vec<Code>,
}

/// What the person ticked on the template checklist.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TemplateChoice {
    /// The plan to save -- the template's, as edited on the checklist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<CarePlan>,
    /// Symptom trackers to create, by name.
    #[serde(default)]
    pub symptoms: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnosed: Option<Date>,
}

/// Where one target stands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TargetStanding {
    pub target: CareTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Timestamp>,
    /// `met`, `notMet`, `unknown` (nothing to judge by).
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct LabDue {
    pub schedule: LabSchedule,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<Date>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<Date>,
    /// "6 weeks after the dose change on 3 Oct".
    #[serde(default)]
    pub reason: String,
    pub overdue: bool,
}

/// A condition's dashboard, worked out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ConditionReport {
    pub condition: Condition,
    pub targets: Vec<TargetStanding>,
    pub labs_due: Vec<LabDue>,
    pub medications: Vec<Medication>,
    /// Over the last 30 days.
    pub adherence: Adherence,
    pub alerts: Vec<RuleAlert>,
}

// ---- labs and documents -----------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum LabFlag {
    #[default]
    None,
    Normal,
    Low,
    High,
    Abnormal,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum LabOrigin {
    #[default]
    Manual,
    /// Read out of a document by the model, and accepted.
    Extracted,
    Fhir,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct LabResult {
    pub id: LabResultId,
    /// LOINC, if known. What trends group by; results without one group by
    /// `name`.
    #[serde(default)]
    pub code: String,
    /// As printed: "TSH", "Hemoglobin A1c".
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    /// A result that is not a number ("Negative", "See note").
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub unit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_low: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_high: Option<f64>,
    #[serde(default)]
    pub ref_text: String,
    #[serde(default)]
    pub flag: LabFlag,
    pub collected: Date,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collected_at: Option<Timestamp>,
    #[serde(default)]
    pub lab: String,
    /// "Lipid panel", "CMP".
    #[serde(default)]
    pub panel: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_id: Option<HealthDocumentId>,
    #[serde(default)]
    pub origin: LabOrigin,
    #[serde(default)]
    pub note: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl LabResult {
    /// Which way this result lies outside its own reference range, or `None`
    /// when it does not (or when nothing can be said): `"below"`, `"above"`,
    /// or `"outside"` for a flag with no direction (abnormal, critical).
    ///
    /// A lab's own flag wins where it gave one, as the screens do; with none,
    /// the numbers decide, and a result that is only text is never outside
    /// anything. The wording is for a clinician and for a model that must not
    /// compare numbers itself.
    pub fn outside_range(&self) -> Option<&'static str> {
        match self.flag {
            LabFlag::Low => return Some("below"),
            LabFlag::High => return Some("above"),
            LabFlag::Abnormal | LabFlag::Critical => return Some("outside"),
            LabFlag::Normal => return None,
            LabFlag::None => {}
        }
        let value = self.value?;
        if self.ref_low.is_some_and(|low| value < low) {
            Some("below")
        } else if self.ref_high.is_some_and(|high| value > high) {
            Some("above")
        } else {
            None
        }
    }
}

/// Every result of one test, oldest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct LabTrend {
    /// The LOINC code, or `name:<lower-cased name>` for results without one.
    pub key: String,
    pub name: String,
    pub unit: String,
    pub results: Vec<LabResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum DocKind {
    LabReport,
    VisitNote,
    Imaging,
    Discharge,
    Prescription,
    Immunization,
    Insurance,
    /// FHIR resources from a portal or the Apple export.
    Fhir,
    Ecg,
    #[default]
    Other,
}

/// One of a document's pages: a file in the files catalog, sealed, with
/// [`crate::files::FileOrigin::Health`]. Somebody adds them through Files'
/// own uploads, so a scan of many megabytes never crosses the webview whole,
/// and the Files app shows them, says which document uses each, and keeps
/// them in its trash a while after the document is deleted.
///
/// The name, type and size are the catalog's at the moment the file was
/// added, so a list of documents is drawn without opening the catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct DocFile {
    pub file: FileId,
    pub name: String,
    pub mime: String,
    pub size: u64,
}

/// What a connector sent for a document -- a FHIR resource's JSON, an ECG's
/// CSV -- sealed in the blob store exactly as it arrived. The document's own
/// data, kept so it can be read again, rather than a file anybody handed in:
/// never in the files catalog, never one of [`HealthDocument::files`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct DocSource {
    pub blob: BlobId,
    pub name: String,
    pub mime: String,
    pub size: u64,
}

/// One FHIR resource in a document, summarised for the screen.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct FhirEntry {
    /// `Observation`, `Condition`, `MedicationRequest`, ...
    pub resource_type: String,
    pub display: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<Date>,
    #[serde(default)]
    pub codes: Vec<Code>,
    /// A value, for an observation: "5.4 %".
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ExtractionState {
    /// Nobody asked.
    #[default]
    None,
    Running,
    /// Results are waiting in `pending_labs` for the person.
    Proposed,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct HealthDocument {
    pub id: HealthDocumentId,
    pub title: String,
    #[serde(default)]
    pub kind: DocKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<Date>,
    /// The clinician, clinic or lab.
    #[serde(default)]
    pub provider: String,
    /// What somebody added: its pages, in order.
    #[serde(default)]
    pub files: Vec<DocFile>,
    /// What a connector sent, when one brought it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<DocSource>,
    /// Text read out of it (the PDF's text layer, or the model's reading
    /// of a scan). Sealed; what search and the assistant read.
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub fhir: Vec<FhirEntry>,
    #[serde(default)]
    pub extraction: ExtractionState,
    #[serde(default)]
    pub extraction_error: String,
    /// Results read out of it, waiting to be accepted.
    #[serde(default)]
    pub pending_labs: Vec<LabResult>,
    #[serde(default)]
    pub condition_ids: Vec<ConditionId>,
    /// Where it came from, so a re-import finds it (a FHIR resource id).
    #[serde(default)]
    pub external_id: String,
    #[serde(default)]
    pub notes: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

// ---- nutrition --------------------------------------------------------------

/// Nutrients for some amount of food. Energy, protein, carbohydrate and fat
/// always; the rest when the source knew.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Nutrients {
    pub kcal: f64,
    pub protein_g: f64,
    pub carbs_g: f64,
    pub fat_g: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fiber_g: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sugar_g: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sat_fat_g: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sodium_mg: Option<f64>,
    /// Anything else, by name ("calcium_mg", "caffeine_mg").
    #[serde(default)]
    pub extra: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Serving {
    /// "1 cup", "1 slice", "100 g".
    pub name: String,
    pub grams: f64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum FoodSource {
    /// Typed in here.
    #[default]
    Own,
    Usda {
        fdc_id: u64,
    },
    OpenFoodFacts {
        code: String,
    },
    /// Worked out from a Library recipe's ingredients.
    Recipe {
        item_id: ItemId,
    },
    /// A model's estimate from a photo.
    Estimate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Food {
    pub id: FoodId,
    pub name: String,
    #[serde(default)]
    pub brand: String,
    #[serde(default)]
    pub barcode: String,
    #[serde(default)]
    pub source: FoodSource,
    /// Per 100 g (or 100 ml for a drink).
    pub per_100g: Nutrients,
    #[serde(default)]
    pub servings: Vec<Serving>,
    #[serde(default)]
    pub favorite: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<Timestamp>,
    #[serde(default)]
    pub use_count: u32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// A food search answer: one of this vault's foods, or one from a lookup
/// service not yet saved (`food.id` then fresh, saved on first use).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct FoodHit {
    pub food: Food,
    /// `mine`, `usda`, `openFoodFacts`.
    pub origin: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum MealSlot {
    Breakfast,
    Lunch,
    Dinner,
    #[default]
    Snack,
}

impl MealSlot {
    /// The meal's name, as a person says it: "Breakfast".
    pub fn word(self) -> &'static str {
        match self {
            MealSlot::Breakfast => "Breakfast",
            MealSlot::Lunch => "Lunch",
            MealSlot::Dinner => "Dinner",
            MealSlot::Snack => "Snack",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MealItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub food_id: Option<FoodId>,
    pub name: String,
    /// How many of `unit`.
    pub amount: f64,
    /// A serving's name, or `g` / `ml`.
    pub unit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grams: Option<f64>,
    /// For this amount, worked out when it was logged, so a later edit of
    /// the food does not rewrite what was eaten.
    pub nutrients: Nutrients,
    /// A model's guess from a photo, not weighed or looked up.
    #[serde(default)]
    pub estimated: bool,
    /// Why this item should be looked at before it is believed: what else it
    /// might be, the range its portion could be in, or that its energy and
    /// nutrients disagree. Written when a photo is read; cleared when the
    /// person marks the item as checked. Empty for anything else.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub doubt: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Meal {
    pub id: MealId,
    pub at: Timestamp,
    #[serde(default)]
    pub offset_min: i16,
    #[serde(default)]
    pub slot: MealSlot,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub items: Vec<MealItem>,
    /// A photograph of the plate: a sealed file in the files catalog, like a
    /// document's pages (see [`DocFile`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub photo: Option<FileId>,
    #[serde(default)]
    pub note: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// A day's eating against the targets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct NutritionDay {
    pub date: Date,
    /// Meals logged here.
    pub logged: Nutrients,
    /// Dietary samples other apps wrote to the phone (Lose It!, ...),
    /// already priority-merged against what was logged here.
    pub imported: Nutrients,
    pub meals: u32,
}

/// What glucose did after a meal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MealResponse {
    pub meal_id: MealId,
    /// The reading nearest before the meal (within 30 minutes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_after_min: Option<u32>,
    /// Peak minus baseline, mg/dL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rise: Option<f64>,
    /// Back within 10 mg/dL of baseline, minutes after the meal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovered_after_min: Option<u32>,
    /// Incremental area above baseline over 2 h, mg/dL·min.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iauc: Option<f64>,
    /// The 3 hours from the meal.
    pub points: Vec<SamplePoint>,
    pub carbs_g: f64,
}

// ---- reports ----------------------------------------------------------------

/// One day's activity, against the goals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ActivityDay {
    pub date: Date,
    pub active_kcal: f64,
    pub exercise_min: f64,
    pub stand_hours: f64,
    pub steps: f64,
    pub distance_m: f64,
    pub flights: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resting_hr: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hrv_ms: Option<f64>,
    pub workouts: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum SleepStage {
    InBed,
    Awake,
    /// Asleep, stage unknown (older watches, other apps).
    Asleep,
    Core,
    Deep,
    Rem,
}

impl SleepStage {
    /// The number a sleep-stage sample stores as its value.
    pub fn code(self) -> f64 {
        match self {
            SleepStage::InBed => 0.0,
            SleepStage::Awake => 1.0,
            SleepStage::Asleep => 2.0,
            SleepStage::Core => 3.0,
            SleepStage::Deep => 4.0,
            SleepStage::Rem => 5.0,
        }
    }

    pub fn from_code(code: f64) -> Option<SleepStage> {
        match code.round() as i64 {
            0 => Some(SleepStage::InBed),
            1 => Some(SleepStage::Awake),
            2 => Some(SleepStage::Asleep),
            3 => Some(SleepStage::Core),
            4 => Some(SleepStage::Deep),
            5 => Some(SleepStage::Rem),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SleepSegment {
    pub stage: SleepStage,
    pub start: Timestamp,
    pub end: Timestamp,
}

/// One night, filed under the local date it ended on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SleepNight {
    pub date: Date,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_bed_start: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sleep_start: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sleep_end: Option<Timestamp>,
    pub asleep_min: f64,
    pub in_bed_min: f64,
    pub awake_min: f64,
    pub core_min: f64,
    pub deep_min: f64,
    pub rem_min: f64,
    /// asleep / in bed, 0..=1, when in-bed time is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub efficiency: Option<f64>,
    /// Mean heart rate and HRV while asleep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hr: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hrv_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub respiratory_rate: Option<f64>,
    /// 0..=100, this app's own heuristic -- see [`sleep::score`]. Every
    /// part of it is in `score_parts` so the screen can say why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(default)]
    pub score_parts: BTreeMap<String, f64>,
    pub segments: Vec<SleepSegment>,
}

/// AGP percentiles for one time of day (five-minute slot of 288, or
/// whatever slot width the report used).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AgpSlot {
    /// Minutes after local midnight.
    pub minute: u32,
    pub p5: f64,
    pub p25: f64,
    pub p50: f64,
    pub p75: f64,
    pub p95: f64,
}

/// A glucose episode below or above a threshold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct GlucoseEvent {
    pub start: Timestamp,
    pub end: Timestamp,
    /// The lowest (for a low) or highest reading.
    pub extreme: f64,
    /// `low`, `veryLow`, `high`, `veryHigh`.
    pub kind: String,
}

/// The standard CGM report (international consensus, 2019): time in
/// ranges, mean, GMI, variability, AGP.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct GlucoseReport {
    pub from: Option<Date>,
    pub to: Option<Date>,
    pub readings: u32,
    /// Share of the range's minutes with a reading, 0..=1. Under 0.7 the
    /// numbers below are shown as provisional.
    pub coverage: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean: Option<f64>,
    /// Glucose management indicator, %: 3.31 + 0.02392 × mean mg/dL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gmi: Option<f64>,
    /// Coefficient of variation, %.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cv: Option<f64>,
    /// Shares, 0..=1: <54, 54-69, 70-180, 181-250, >250 mg/dL.
    pub very_low: f64,
    pub low: f64,
    pub in_range: f64,
    pub high: f64,
    pub very_high: f64,
    pub agp: Vec<AgpSlot>,
    pub events: Vec<GlucoseEvent>,
}

/// Everything one day's overlay draws: glucose against meals, doses,
/// workouts and sleep.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct DayTimeline {
    pub date: Date,
    pub glucose: Vec<SamplePoint>,
    pub heart_rate: Vec<SeriesPoint>,
    pub meals: Vec<Meal>,
    pub doses: Vec<Dose>,
    pub insulin: Vec<SamplePoint>,
    pub workouts: Vec<Workout>,
    pub sleep: Vec<SleepSegment>,
}

/// The latest value of one metric, for the Today and Vitals tiles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Latest {
    pub metric: Metric,
    pub value: f64,
    pub at: Timestamp,
    pub source: String,
    /// The value 30 days before, for a trend arrow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<f64>,
}

/// The Today screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct HealthDay {
    pub date: Date,
    pub activity: ActivityDay,
    pub goals: ActivityGoals,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sleep: Option<SleepNight>,
    pub latest: Vec<Latest>,
    pub doses_due: Vec<DueDose>,
    pub nutrition: NutritionDay,
    pub workouts: Vec<Workout>,
    /// Time in range today, when there is glucose data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glucose: Option<GlucoseReport>,
    pub alerts: Vec<RuleAlert>,
    /// Labs coming due in the next 30 days, across conditions.
    pub labs_due: Vec<LabDue>,
    /// Sources that have gone quiet past `stale_hours`.
    pub stale_sources: Vec<HealthSource>,
}

/// A summary for a clinic visit, written by the large model from the
/// vault's own numbers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct VisitBrief {
    pub markdown: String,
    pub since: Date,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition_id: Option<ConditionId>,
}

/// The numbers a [`VisitBrief`] is written from -- also what the assistant
/// tool answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct VisitFacts {
    pub since: Date,
    pub until: Date,
    pub conditions: Vec<ConditionReport>,
    pub medications: Vec<Medication>,
    pub medication_changes: Vec<(String, DoseChange)>,
    pub labs: Vec<LabResult>,
    pub allergies: Vec<Allergy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glucose: Option<GlucoseReport>,
    pub latest: Vec<Latest>,
    pub weight_change_kg: Option<f64>,
    pub mean_sleep_min: Option<f64>,
    pub mean_steps: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lab(value: Option<f64>, low: Option<f64>, high: Option<f64>, flag: &str) -> LabResult {
        let mut json = serde_json::json!({
            "id": LabResultId::new(),
            "name": "TSH",
            "flag": flag,
            "collected": "2026-11-10",
            "createdAt": "2026-11-10T12:00:00Z",
            "updatedAt": "2026-11-10T12:00:00Z",
        });
        if let Some(v) = value {
            json["value"] = v.into();
        } else {
            json["text"] = "Negative".into();
        }
        if let Some(l) = low {
            json["refLow"] = l.into();
        }
        if let Some(h) = high {
            json["refHigh"] = h.into();
        }
        serde_json::from_value(json).expect("a lab result")
    }

    #[test]
    fn a_result_is_outside_its_range_by_its_flag_first_and_its_numbers_after() {
        // The numbers decide when the report printed no flag.
        assert_eq!(lab(Some(4.8), Some(0.4), Some(4.0), "none").outside_range(), Some("above"));
        assert_eq!(lab(Some(0.1), Some(0.4), Some(4.0), "none").outside_range(), Some("below"));
        assert_eq!(lab(Some(2.0), Some(0.4), Some(4.0), "none").outside_range(), None);
        // A limit with one end only.
        assert_eq!(lab(Some(250.0), None, Some(200.0), "none").outside_range(), Some("above"));
        assert_eq!(lab(Some(10.0), None, Some(200.0), "none").outside_range(), None);
        // The lab's own flag wins, either way.
        assert_eq!(lab(Some(2.0), Some(0.4), Some(4.0), "high").outside_range(), Some("above"));
        assert_eq!(lab(Some(2.0), Some(0.4), Some(4.0), "low").outside_range(), Some("below"));
        assert_eq!(
            lab(Some(2.0), Some(0.4), Some(4.0), "critical").outside_range(),
            Some("outside")
        );
        assert_eq!(lab(Some(5.0), Some(0.4), Some(4.0), "normal").outside_range(), None);
        // A result that is only words is outside nothing.
        assert_eq!(lab(None, Some(0.4), Some(4.0), "none").outside_range(), None);
    }

    #[test]
    fn a_meal_item_carries_a_doubt_only_when_it_has_one() {
        let json = serde_json::json!({
            "name": "Dal", "amount": 200.0, "unit": "g",
            "nutrients": { "kcal": 220.0, "proteinG": 11.0, "carbsG": 30.0, "fatG": 6.0 },
        });
        let item: MealItem = serde_json::from_value(json).expect("an item stored before doubts");
        assert_eq!(item.doubt, "");
        assert!(serde_json::to_value(&item).unwrap().get("doubt").is_none());
        let doubtful = MealItem { doubt: "paneer or tofu".into(), ..item };
        assert_eq!(serde_json::to_value(&doubtful).unwrap()["doubt"], "paneer or tofu");
    }
}
