//! The health half of the conformance suite: series days, sources,
//! workouts, medications and doses, conditions, lab results, documents,
//! allergies, foods, meals and the settings.
//!
//! One check per rule in [`crate::store::health`]'s module doc, at least,
//! and a round trip of every field of every record. Two of those rules
//! cannot be checked from here, for the reason Money's suite gives: that
//! nothing a person would recognise sits in a clear column, and that a
//! payload copied into another row, series or day will not open there. The
//! trait offers no way to read or write a raw row, and inventing one for a
//! test would be a door the running app does not need. Both are checked where
//! the rows are: the SQL backend's own tests tie each clear column to the
//! schema, and the SQLite driver's search the database file for every metric,
//! value, source, medication and title they wrote, and move a sealed row to
//! another series, day and id to watch it fail.
//!
//! One more is only checked as far as the trait lets it be: that a batch is
//! all or nothing. Nothing a caller can hand [`HealthStore::put_series`] makes
//! a conforming store fail half way through, so what is checked is that a
//! large batch lands whole in one call -- and, for the two batches that can be
//! refused (a key held by another id), that the refusal writes nothing.

use super::*;
use crate::health::{
    Activity, ActivityGoals, Allergy, AllergyKind, AutomationSeen, Bucket, CarePlan, CareTarget,
    Code, Condition, ConditionStatus, Connector, DocFile, DocKind, DocSource, Dose, DoseChange,
    DoseOrigin, DoseStatus, ExtractionState, FhirEntry, Food, FoodSource, HealthDocument,
    HealthSettings, HealthSource, HealthUnits, LabFlag, LabOrigin, LabResult, LabSchedule, Meal,
    MealItem, MealSlot, Measure, MedKind, MedRule, MedSchedule, MedStatus, Medication, Metric,
    Nutrients, NutritionTargets, Sample, SeriesDay, SeriesSummary, Serving, Severity, SourceStatus,
    Stat, Supply, TemplateKey, Tombstone, Workout,
};
use crate::id::{
    AllergyId, BlobId, ConditionId, DoseId, FileId, FoodId, HealthDocumentId, HealthSourceId,
    ItemId, LabResultId, MealId, MedicationId, TrackerId, WorkoutId,
};
use crate::store::health::{HealthStore, SeriesExtent, SeriesRow};
use jiff::Timestamp;
use std::collections::{BTreeMap, BTreeSet};

/// The health half of the suite. Called by [`run_all`] when the backend has
/// a [`HealthStore`](crate::store::health::HealthStore); public so a backend
/// under construction can run it alone.
///
/// The store must hold no health records on entry; it is left with none, and
/// with the settings at their defaults.
pub fn run_health_suite(store: &dyn JournalStore) {
    eprintln!("--- health conformance suite ---");
    let health = store.health().expect("run_health_suite needs a health store");

    health_starts_empty(health);
    missing_health_records_are_not_found(health);
    a_series_day_round_trips_both_payloads(health);
    putting_a_series_day_again_replaces_both_payloads(health);
    series_are_told_apart(health);
    series_days_are_inclusive_and_in_day_order(health);
    the_latest_series_day_is_the_latest_at_or_before(health);
    deleting_series_days_answers_how_many_went(health);
    series_extents_name_every_series_with_data(health);
    a_large_series_batch_lands_whole_in_one_call(health);
    settings_default_until_saved_and_round_trip(health);
    a_source_round_trips_every_field(health);
    a_workout_round_trips_every_field(health);
    workouts_are_every_one_overlapping_the_range_and_nothing_touching_it(health);
    workouts_come_back_in_start_order_then_id(health);
    a_batch_of_workouts_is_refused_whole_when_a_key_is_held_by_another_id(health);
    a_medication_round_trips_every_field(health);
    medications_come_back_in_their_order(health);
    a_dose_round_trips_every_field(health);
    doses_are_those_in_the_range_in_time_order(health);
    a_batch_of_doses_is_refused_whole_when_a_key_is_held_by_another_id(health);
    a_large_batch_of_keyed_doses_is_one_call(health);
    deleting_a_medication_takes_its_doses_and_only_its_doses(health);
    a_condition_round_trips_every_field(health);
    lab_results_round_trip_and_come_back_by_collection_date(health);
    documents_round_trip_and_come_back_newest_first(health);
    an_allergy_round_trips_every_field(health);
    foods_round_trip_and_come_back_most_recently_used_first(health);
    meals_round_trip_and_are_those_in_the_range_in_time_order(health);
    deleting_anything_else_touches_nothing_else(health);
    blob_refs_name_every_blob_a_health_record_holds(health);
    garbage_collection_keeps_what_health_holds(store, health);
    unicode_survives_a_health_round_trip(health);

    health_cleanup(health);
    eprintln!("--- health suite passed ---");
}

/// Every time in this suite is whole seconds from this one, so a time that
/// comes back from a clear column (whole microseconds) compares equal to the
/// one that went in, and "an hour ago" means the same thing on every run.
const NOW: i64 = 1_800_000_000;

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).expect("a representable time")
}

/// `NOW + seconds`.
fn t(seconds: i64) -> Timestamp {
    at(NOW + seconds)
}

/// A day, `n` days from the first of October 2026 -- negative for before it.
fn d(n: i32) -> Date {
    date(2026, 10, 1).checked_add(jiff::Span::new().days(n)).expect("a representable date")
}

/// Two series, as the vault would name them: keyed hashes, opaque here.
const HEART: &str = "3f9a0c1e5b7d2468ace013579bdf2468";
const GLUCOSE: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90";

fn sorted<T: Ord>(mut list: Vec<T>) -> Vec<T> {
    list.sort();
    list
}

// ---- fixtures ---------------------------------------------------------------

/// A series day of heart rate: two samples from two sources and the one
/// bucket they fall in, `bpm` and one more.
fn row(series: &str, on: Date, bpm: f64) -> SeriesRow {
    SeriesRow {
        series: series.into(),
        day: on,
        data: SeriesDay {
            metric: Some(Metric::HeartRate),
            day: Some(on),
            sources: vec!["Apple Watch".into(), "iPhone".into()],
            samples: vec![
                Sample { start: NOW, secs: 0, value: bpm, source: 0, offset_min: -480 },
                Sample { start: NOW + 60, secs: 30, value: bpm + 1.0, source: 1, offset_min: -480 },
            ],
            // A reading the person deleted, which the phone would bring back.
            tombstones: vec![Tombstone { start: NOW + 120, secs: 0, value: bpm + 2.0 }],
        },
        summary: SeriesSummary {
            metric: Some(Metric::HeartRate),
            day: Some(on),
            buckets: vec![Bucket {
                quarter: 33,
                count: 2,
                sum: bpm * 2.0 + 1.0,
                min: bpm,
                max: bpm + 1.0,
                last: bpm + 1.0,
                covered: 30,
            }],
            offset_min: -480,
        },
    }
}

fn put_rows(health: &dyn HealthStore, rows: &[SeriesRow]) {
    health.put_series(rows).expect("put_series");
}

fn days_of(days: &[SeriesDay]) -> Vec<Date> {
    days.iter().map(|d| d.day.expect("a series day names its day")).collect()
}

/// Every series day gone, by the widest sweep there is.
fn clear_series(health: &dyn HealthStore) {
    for extent in health.series_extents().expect("series_extents") {
        health.delete_series(&extent.series, extent.first, extent.last).expect("delete_series");
    }
    assert!(health.series_extents().unwrap().is_empty(), "a sweep left series behind");
}

fn full_source() -> HealthSource {
    HealthSource {
        id: HealthSourceId::new(),
        connector: Connector::HealthAutoExport,
        name: "Hari's iPhone".into(),
        paused: true,
        status: SourceStatus {
            last_received_at: Some(t(10)),
            newest_sample_at: Some(t(5)),
            last_error: "the vault was locked".into(),
            samples_total: 123_456,
            automations: vec![AutomationSeen {
                id: "automation-1".into(),
                name: "Hourly metrics".into(),
                period: "Since Last Sync".into(),
                aggregation: "none".into(),
                last_at: Some(t(10)),
                last_count: 17,
            }],
            devices: vec!["Apple Watch".into(), "iPhone".into()],
        },
        token_id: "token-0001".into(),
        created_at: t(-3_600),
        updated_at: t(10),
    }
}

fn source(name: &str, created: i64) -> HealthSource {
    HealthSource {
        id: HealthSourceId::new(),
        connector: Connector::Manual,
        name: name.into(),
        paused: false,
        status: SourceStatus::default(),
        token_id: String::new(),
        created_at: t(created),
        updated_at: t(created),
    }
}

fn full_workout(key: &str) -> Workout {
    Workout {
        id: WorkoutId::new(),
        activity: Activity::Running,
        name: "Outdoor Run".into(),
        start: t(0),
        end: t(1_800),
        offset_min: -480,
        active_energy_kcal: Some(310.5),
        total_energy_kcal: Some(352.25),
        distance_m: Some(5_012.5),
        elevation_up_m: Some(48.0),
        avg_hr: Some(151.0),
        max_hr: Some(178.0),
        effort: Some(6.5),
        indoor: Some(false),
        source: "Apple Watch".into(),
        source_id: Some(HealthSourceId::new()),
        external_id: "hae-workout-0001".into(),
        route: Some(BlobId::of(b"a route nobody keeps")),
        note: "felt good after the first mile".into(),
        key: key.into(),
        hidden: true,
        created_at: t(-60),
        updated_at: t(5),
    }
}

/// A workout with nothing optional set, `from..to` seconds after [`NOW`].
fn workout(key: &str, from: i64, to: i64) -> Workout {
    Workout {
        id: WorkoutId::new(),
        activity: Activity::Walking,
        name: String::new(),
        start: t(from),
        end: t(to),
        offset_min: 0,
        active_energy_kcal: None,
        total_energy_kcal: None,
        distance_m: None,
        elevation_up_m: None,
        avg_hr: None,
        max_hr: None,
        effort: None,
        indoor: None,
        source: String::new(),
        source_id: None,
        external_id: String::new(),
        route: None,
        note: String::new(),
        key: key.into(),
        hidden: false,
        created_at: t(0),
        updated_at: t(0),
    }
}

fn put_workouts(health: &dyn HealthStore, workouts: &[&Workout]) {
    let owned: Vec<Workout> = workouts.iter().map(|w| (*w).clone()).collect();
    health.put_workouts(&owned).expect("put_workouts");
}

fn every_workout(health: &dyn HealthStore) -> Vec<Workout> {
    health.workouts(at(0), at(4_000_000_000)).expect("workouts")
}

fn clear_workouts(health: &dyn HealthStore) {
    for workout in every_workout(health) {
        health.delete_workout(workout.id).expect("delete_workout");
    }
    assert!(every_workout(health).is_empty(), "a sweep left workouts behind");
}

fn full_medication(order: i64) -> Medication {
    Medication {
        id: MedicationId::new(),
        name: "Levothyroxine".into(),
        kind: MedKind::Prescription,
        form: "tablet".into(),
        strength: "50 mcg".into(),
        dose: 1.5,
        dose_unit: "tablet".into(),
        schedule: MedSchedule::Weekly {
            days: vec![1, 3, 5],
            times: vec![time(6, 30, 0, 0), time(21, 0, 0, 0)],
        },
        rules: vec![
            MedRule::EmptyStomach { minutes: 30 },
            MedRule::WithFood { minutes: 15 },
            MedRule::ApartFrom { tags: vec!["calcium".into(), "iron".into()], hours: 4.0 },
        ],
        tags: vec!["thyroid".into()],
        status: MedStatus::Paused,
        started: Some(date(2024, 3, 1)),
        stopped: Some(date(2026, 9, 30)),
        stop_reason: "switched brand".into(),
        history: vec![
            DoseChange {
                on: date(2024, 3, 1),
                dose: 1.0,
                strength: "25 mcg".into(),
                note: "started low".into(),
            },
            DoseChange {
                on: date(2025, 1, 15),
                dose: 1.5,
                strength: "50 mcg".into(),
                note: "after the six-week test".into(),
            },
        ],
        condition_ids: vec![ConditionId::new(), ConditionId::new()],
        prescriber: "Dr. Okafor".into(),
        pharmacy: "Corner Pharmacy".into(),
        supply: Some(Supply {
            remaining: 42.0,
            refill_days: 10,
            last_refill: Some(date(2026, 10, 20)),
        }),
        remind: false,
        codes: vec![Code {
            system: "rxnorm".into(),
            code: "10582".into(),
            display: "levothyroxine".into(),
        }],
        source_names: vec!["Levothyroxine Sodium".into()],
        notes: "empty stomach, with water".into(),
        sort_order: order,
        created_at: t(-3_600),
        updated_at: t(7),
    }
}

/// A medication with nothing optional set.
fn medication(name: &str, order: i64, created: i64) -> Medication {
    Medication {
        id: MedicationId::new(),
        name: name.into(),
        kind: MedKind::Supplement,
        form: String::new(),
        strength: String::new(),
        dose: 1.0,
        dose_unit: String::new(),
        schedule: MedSchedule::AsNeeded,
        rules: Vec::new(),
        tags: Vec::new(),
        status: MedStatus::Active,
        started: None,
        stopped: None,
        stop_reason: String::new(),
        history: Vec::new(),
        condition_ids: Vec::new(),
        prescriber: String::new(),
        pharmacy: String::new(),
        supply: None,
        remind: true,
        codes: Vec::new(),
        source_names: Vec::new(),
        notes: String::new(),
        sort_order: order,
        created_at: t(created),
        updated_at: t(created),
    }
}

fn stored_medication(health: &dyn HealthStore, name: &str) -> Medication {
    let medication = medication(name, 0, 0);
    health.put_medication(&medication).expect("put_medication");
    medication
}

fn full_dose(medication: &Medication, key: &str, from_now: i64) -> Dose {
    Dose {
        id: DoseId::new(),
        medication_id: medication.id,
        scheduled_at: Some(t(from_now - 120)),
        at: t(from_now),
        status: DoseStatus::Skipped,
        amount: Some(0.5),
        origin: DoseOrigin::Source,
        note: "felt sick, skipped".into(),
        key: key.into(),
        hidden: true,
        created_at: t(from_now),
        updated_at: t(from_now + 1),
    }
}

/// A dose with nothing optional set, taken `from_now` seconds after [`NOW`].
fn dose(medication: &Medication, key: &str, from_now: i64) -> Dose {
    Dose {
        id: DoseId::new(),
        medication_id: medication.id,
        scheduled_at: None,
        at: t(from_now),
        status: DoseStatus::Taken,
        amount: None,
        origin: DoseOrigin::Manual,
        note: String::new(),
        key: key.into(),
        hidden: false,
        created_at: t(0),
        updated_at: t(0),
    }
}

fn put_doses(health: &dyn HealthStore, doses: &[&Dose]) {
    let owned: Vec<Dose> = doses.iter().map(|d| (*d).clone()).collect();
    health.put_doses(&owned).expect("put_doses");
}

fn every_dose(health: &dyn HealthStore, medication: Option<MedicationId>) -> Vec<Dose> {
    health.doses(at(0), at(4_000_000_000), medication).expect("doses")
}

fn full_condition() -> Condition {
    let code = |system: &str, code: &str, display: &str| Code {
        system: system.into(),
        code: code.into(),
        display: display.into(),
    };
    Condition {
        id: ConditionId::new(),
        name: "Hypothyroidism".into(),
        template: Some(TemplateKey::Hypothyroidism),
        status: ConditionStatus::Remission,
        diagnosed: Some(date(2023, 5, 4)),
        codes: vec![
            code("icd10", "E03.9", "Hypothyroidism, unspecified"),
            code("snomed", "40930008", ""),
        ],
        plan: CarePlan {
            watch: vec![
                Measure::Metric { metric: Metric::RestingHeartRate, stat: Stat::Mean },
                Measure::Metric {
                    metric: Metric::BloodGlucose,
                    stat: Stat::InRange { low: 70, high: 180 },
                },
                Measure::Lab { code: "3016-3".into() },
            ],
            targets: vec![
                CareTarget {
                    label: "TSH inside the lab's own range".into(),
                    measure: Measure::Lab { code: "3016-3".into() },
                    low: None,
                    high: None,
                    use_reference_range: true,
                    window_days: 0,
                    note: "from the endocrinologist".into(),
                },
                CareTarget {
                    label: "Time in range".into(),
                    measure: Measure::Metric {
                        metric: Metric::BloodGlucose,
                        stat: Stat::InRange { low: 70, high: 180 },
                    },
                    low: Some(0.7),
                    high: Some(1.0),
                    use_reference_range: false,
                    window_days: 14,
                    note: String::new(),
                },
            ],
            labs: vec![LabSchedule {
                code: "3016-3".into(),
                name: "TSH".into(),
                every_days: 365,
                after_change_days: Some(42),
                after_change_tag: "thyroid".into(),
            }],
            med_rules: BTreeMap::from([(
                "thyroid".to_string(),
                vec![
                    MedRule::EmptyStomach { minutes: 30 },
                    MedRule::ApartFrom { tags: vec!["calcium".into()], hours: 4.0 },
                ],
            )]),
            tracker_ids: vec![TrackerId::new(), TrackerId::new()],
            template_version: 1,
        },
        notes: "recheck after any change of dose".into(),
        created_at: t(-3_600),
        updated_at: t(9),
    }
}

fn condition(name: &str, created: i64) -> Condition {
    Condition {
        id: ConditionId::new(),
        name: name.into(),
        template: None,
        status: ConditionStatus::Active,
        diagnosed: None,
        codes: Vec::new(),
        plan: CarePlan::default(),
        notes: String::new(),
        created_at: t(created),
        updated_at: t(created),
    }
}

fn full_lab(on: Date) -> LabResult {
    LabResult {
        id: LabResultId::new(),
        code: "3016-3".into(),
        name: "TSH".into(),
        value: Some(2.4),
        text: "within range".into(),
        unit: "mIU/L".into(),
        ref_low: Some(0.4),
        ref_high: Some(4.0),
        ref_text: "0.4-4.0".into(),
        flag: LabFlag::Normal,
        collected: on,
        collected_at: Some(t(-600)),
        lab: "Quest".into(),
        panel: "Thyroid panel".into(),
        document_id: Some(HealthDocumentId::new()),
        origin: LabOrigin::Extracted,
        note: "fasting".into(),
        created_at: t(-60),
        updated_at: t(3),
    }
}

/// A result with nothing optional set, collected `on`.
fn lab(name: &str, on: Date) -> LabResult {
    LabResult {
        id: LabResultId::new(),
        code: String::new(),
        name: name.into(),
        value: None,
        text: String::new(),
        unit: String::new(),
        ref_low: None,
        ref_high: None,
        ref_text: String::new(),
        flag: LabFlag::None,
        collected: on,
        collected_at: None,
        lab: String::new(),
        panel: String::new(),
        document_id: None,
        origin: LabOrigin::Manual,
        note: String::new(),
        created_at: t(0),
        updated_at: t(0),
    }
}

fn full_document(on: Option<Date>) -> HealthDocument {
    HealthDocument {
        id: HealthDocumentId::new(),
        title: "Thyroid panel, October".into(),
        kind: DocKind::LabReport,
        date: on,
        provider: "Quest Diagnostics".into(),
        files: vec![
            DocFile {
                file: FileId::new(),
                name: "page-1.pdf".into(),
                mime: "application/pdf".into(),
                size: 48_213,
            },
            DocFile {
                file: FileId::new(),
                name: "page-2.png".into(),
                mime: "image/png".into(),
                size: 9_001,
            },
        ],
        source: Some(DocSource {
            blob: BlobId::of(b"the resource as it came"),
            name: "Observation-77.json".into(),
            mime: "application/fhir+json".into(),
            size: 1_204,
        }),
        text: "TSH 2.4 mIU/L (0.4-4.0)".into(),
        fhir: vec![FhirEntry {
            resource_type: "Observation".into(),
            display: "Thyroid stimulating hormone".into(),
            date: Some(date(2026, 10, 2)),
            codes: vec![Code {
                system: "loinc".into(),
                code: "3016-3".into(),
                display: "TSH".into(),
            }],
            value: "2.4 mIU/L".into(),
        }],
        extraction: ExtractionState::Proposed,
        extraction_error: "page two was too dark to read".into(),
        pending_labs: vec![lab("Free T4", date(2026, 10, 2))],
        condition_ids: vec![ConditionId::new()],
        external_id: "fhir-resource-77".into(),
        notes: "bring to the visit".into(),
        created_at: t(-60),
        updated_at: t(4),
    }
}

/// A document with nothing optional set.
fn document(title: &str, on: Option<Date>) -> HealthDocument {
    HealthDocument {
        id: HealthDocumentId::new(),
        title: title.into(),
        kind: DocKind::Other,
        date: on,
        provider: String::new(),
        files: Vec::new(),
        source: None,
        text: String::new(),
        fhir: Vec::new(),
        extraction: ExtractionState::None,
        extraction_error: String::new(),
        pending_labs: Vec::new(),
        condition_ids: Vec::new(),
        external_id: String::new(),
        notes: String::new(),
        created_at: t(0),
        updated_at: t(0),
    }
}

fn full_allergy() -> Allergy {
    Allergy {
        id: AllergyId::new(),
        substance: "Penicillin".into(),
        kind: AllergyKind::Drug,
        reaction: "hives".into(),
        severity: Severity::Severe,
        noted: Some(date(2019, 7, 1)),
        notes: "carry the card".into(),
        created_at: t(-60),
        updated_at: t(2),
    }
}

fn allergy(substance: &str, created: i64) -> Allergy {
    Allergy {
        id: AllergyId::new(),
        substance: substance.into(),
        kind: AllergyKind::Food,
        reaction: String::new(),
        severity: Severity::Mild,
        noted: None,
        notes: String::new(),
        created_at: t(created),
        updated_at: t(created),
    }
}

fn full_nutrients() -> Nutrients {
    Nutrients {
        kcal: 205.0,
        protein_g: 4.3,
        carbs_g: 45.0,
        fat_g: 0.4,
        fiber_g: Some(3.5),
        sugar_g: Some(0.1),
        sat_fat_g: Some(0.1),
        sodium_mg: Some(2.0),
        extra: BTreeMap::from([("calcium_mg".to_string(), 16.0), ("caffeine_mg".to_string(), 0.0)]),
    }
}

fn bare_nutrients() -> Nutrients {
    Nutrients { kcal: 50.0, protein_g: 1.0, carbs_g: 10.0, fat_g: 0.5, ..Nutrients::default() }
}

fn full_food(source: FoodSource, last_used: Option<i64>) -> Food {
    Food {
        id: FoodId::new(),
        name: "Brown rice, cooked".into(),
        brand: "Uncle Ben's".into(),
        barcode: "0123456789012".into(),
        source,
        per_100g: full_nutrients(),
        servings: vec![
            Serving { name: "1 cup".into(), grams: 195.0 },
            Serving { name: "100 g".into(), grams: 100.0 },
        ],
        favorite: true,
        last_used: last_used.map(t),
        use_count: 3,
        created_at: t(-3_600),
        updated_at: t(6),
    }
}

/// A food with nothing optional set, last eaten `last_used` seconds after
/// [`NOW`], or never.
fn food(name: &str, last_used: Option<i64>) -> Food {
    Food {
        id: FoodId::new(),
        name: name.into(),
        brand: String::new(),
        barcode: String::new(),
        source: FoodSource::Own,
        per_100g: bare_nutrients(),
        servings: Vec::new(),
        favorite: false,
        last_used: last_used.map(t),
        use_count: 0,
        created_at: t(0),
        updated_at: t(0),
    }
}

fn full_meal(from_now: i64) -> Meal {
    Meal {
        id: MealId::new(),
        at: t(from_now),
        offset_min: -480,
        slot: MealSlot::Dinner,
        name: "Chicken and rice".into(),
        items: vec![
            MealItem {
                food_id: Some(FoodId::new()),
                name: "Brown rice, cooked".into(),
                amount: 1.5,
                unit: "cup".into(),
                grams: Some(292.5),
                nutrients: full_nutrients(),
                estimated: false,
                doubt: String::new(),
            },
            MealItem {
                food_id: None,
                name: "Chicken, roasted".into(),
                amount: 120.0,
                unit: "g".into(),
                grams: None,
                nutrients: bare_nutrients(),
                estimated: true,
                doubt: "150-250 g; the bowl's depth is hidden".into(),
            },
        ],
        photo: Some(FileId::new()),
        note: "second helping".into(),
        created_at: t(from_now),
        updated_at: t(from_now + 1),
    }
}

/// A meal with nothing optional set, eaten `from_now` seconds after [`NOW`].
fn meal(name: &str, from_now: i64) -> Meal {
    Meal {
        id: MealId::new(),
        at: t(from_now),
        offset_min: 0,
        slot: MealSlot::Snack,
        name: name.into(),
        items: Vec::new(),
        photo: None,
        note: String::new(),
        created_at: t(0),
        updated_at: t(0),
    }
}

fn every_meal(health: &dyn HealthStore) -> Vec<Meal> {
    health.meals(at(0), at(4_000_000_000)).expect("meals")
}

fn full_settings() -> HealthSettings {
    HealthSettings {
        units: HealthUnits {
            glucose: "mmol/L".into(),
            weight: "lb".into(),
            distance: "mi".into(),
            energy: "kJ".into(),
            temperature: "F".into(),
            length: "in".into(),
        },
        goals: ActivityGoals {
            move_kcal: 650.0,
            exercise_min: 45.0,
            stand_hours: 10.0,
            steps: 10_000.0,
            sleep_hours: 7.5,
        },
        nutrition: NutritionTargets {
            kcal: Some(2_100.0),
            protein_g: Some(110.0),
            carbs_g: Some(220.0),
            fat_g: Some(70.0),
            fiber_g: Some(30.0),
            sodium_mg: Some(2_300.0),
            carbs_per_meal_g: Some(45.0),
        },
        priority: vec!["Apple Watch".into(), "iPhone".into(), "Karma Yoga".into()],
        priority_by_metric: BTreeMap::from([
            (Metric::Steps, vec!["iPhone".to_string(), "Apple Watch".to_string()]),
            (Metric::Weight, vec!["Withings Scale".to_string()]),
        ]),
        glucose_low: Some(72.0),
        glucose_high: Some(190.0),
        // Whichever way the default goes, this is the other.
        dose_reminders: !HealthSettings::default().dose_reminders,
        stale_hours: HealthSettings::default().stale_hours + 12,
    }
}

// ---- the checks -------------------------------------------------------------

fn health_starts_empty(health: &dyn HealthStore) {
    assert!(health.series_extents().unwrap().is_empty(), "a fresh store must hold no series");
    assert!(health.series_days(HEART, d(-3_000), d(3_000)).unwrap().is_empty());
    assert!(health.series_summaries(HEART, d(-3_000), d(3_000)).unwrap().is_empty());
    assert!(health.latest_series_day(HEART, d(3_000)).unwrap().is_none());
    assert_eq!(health.health_settings().unwrap(), HealthSettings::default());
    assert!(health.list_sources().unwrap().is_empty(), "a fresh store must hold no sources");
    assert!(every_workout(health).is_empty(), "nor workouts");
    assert!(health.list_medications().unwrap().is_empty(), "nor medications");
    assert!(every_dose(health, None).is_empty(), "nor doses");
    assert!(health.list_conditions().unwrap().is_empty(), "nor conditions");
    assert!(health.list_labs().unwrap().is_empty(), "nor lab results");
    assert!(health.list_documents().unwrap().is_empty(), "nor documents");
    assert!(health.list_allergies().unwrap().is_empty(), "nor allergies");
    assert!(health.list_foods().unwrap().is_empty(), "nor foods");
    assert!(every_meal(health).is_empty(), "nor meals");
    assert!(health.blob_refs().unwrap().is_empty(), "so it holds no blobs");
}

fn health_cleanup(health: &dyn HealthStore) {
    clear_series(health);
    clear_workouts(health);
    for medication in health.list_medications().expect("list_medications") {
        health.delete_medication(medication.id).expect("delete_medication");
    }
    for dose in every_dose(health, None) {
        health.delete_dose(dose.id).expect("delete_dose");
    }
    for source in health.list_sources().expect("list_sources") {
        health.delete_source(source.id).expect("delete_source");
    }
    for condition in health.list_conditions().expect("list_conditions") {
        health.delete_condition(condition.id).expect("delete_condition");
    }
    for lab in health.list_labs().expect("list_labs") {
        health.delete_lab(lab.id).expect("delete_lab");
    }
    for document in health.list_documents().expect("list_documents") {
        health.delete_document(document.id).expect("delete_document");
    }
    for allergy in health.list_allergies().expect("list_allergies") {
        health.delete_allergy(allergy.id).expect("delete_allergy");
    }
    for food in health.list_foods().expect("list_foods") {
        health.delete_food(food.id).expect("delete_food");
    }
    for meal in every_meal(health) {
        health.delete_meal(meal.id).expect("delete_meal");
    }
    health.put_health_settings(&HealthSettings::default()).expect("put_health_settings");

    assert!(health.list_medications().unwrap().is_empty(), "cleanup left medications behind");
    assert!(every_dose(health, None).is_empty(), "cleanup left doses behind");
    assert!(health.list_sources().unwrap().is_empty(), "cleanup left sources behind");
    assert!(health.list_conditions().unwrap().is_empty(), "cleanup left conditions behind");
    assert!(health.list_labs().unwrap().is_empty(), "cleanup left lab results behind");
    assert!(health.list_documents().unwrap().is_empty(), "cleanup left documents behind");
    assert!(health.list_allergies().unwrap().is_empty(), "cleanup left allergies behind");
    assert!(health.list_foods().unwrap().is_empty(), "cleanup left foods behind");
    assert!(every_meal(health).is_empty(), "cleanup left meals behind");
    assert!(health.blob_refs().unwrap().is_empty(), "cleanup left blob references behind");
}

fn missing_health_records_are_not_found(health: &dyn HealthStore) {
    super::assert_not_found(health.get_source(HealthSourceId::new()));
    super::assert_not_found(health.get_workout(WorkoutId::new()));
    super::assert_not_found(health.get_medication(MedicationId::new()));
    super::assert_not_found(health.get_dose(DoseId::new()));
    super::assert_not_found(health.get_condition(ConditionId::new()));
    super::assert_not_found(health.get_lab(LabResultId::new()));
    super::assert_not_found(health.get_document(HealthDocumentId::new()));
    super::assert_not_found(health.get_allergy(AllergyId::new()));
    super::assert_not_found(health.get_food(FoodId::new()));
    super::assert_not_found(health.get_meal(MealId::new()));
    assert!(health.workout_by_key("a key nobody wrote").unwrap().is_none());
    assert!(health.dose_by_key("a key nobody wrote").unwrap().is_none());
    assert!(health.dose_by_key("").unwrap().is_none(), "an empty key is nobody's");

    // An empty batch is nothing to do, and so is deleting what is not there,
    // as it is for Money and Screen time.
    health.put_series(&[]).expect("an empty batch is nothing to do");
    health.put_workouts(&[]).expect("an empty batch is nothing to do");
    health.put_doses(&[]).expect("an empty batch is nothing to do");
    health.put_labs(&[]).expect("an empty batch is nothing to do");
    assert_eq!(health.delete_series(HEART, d(0), d(10)).unwrap(), 0, "no days, none gone");
    health.delete_source(HealthSourceId::new()).expect("delete_source of nothing");
    health.delete_workout(WorkoutId::new()).expect("delete_workout of nothing");
    health.delete_medication(MedicationId::new()).expect("delete_medication of nothing");
    health.delete_dose(DoseId::new()).expect("delete_dose of nothing");
    health.delete_condition(ConditionId::new()).expect("delete_condition of nothing");
    health.delete_lab(LabResultId::new()).expect("delete_lab of nothing");
    health.delete_document(HealthDocumentId::new()).expect("delete_document of nothing");
    health.delete_allergy(AllergyId::new()).expect("delete_allergy of nothing");
    health.delete_food(FoodId::new()).expect("delete_food of nothing");
    health.delete_meal(MealId::new()).expect("delete_meal of nothing");
    assert!(health.series_extents().unwrap().is_empty());
}

fn a_series_day_round_trips_both_payloads(health: &dyn HealthStore) {
    let full = row(HEART, d(1), 61.0);
    put_rows(health, std::slice::from_ref(&full));

    assert_eq!(health.series_days(HEART, d(1), d(1)).unwrap(), vec![full.data.clone()]);
    assert_eq!(health.series_summaries(HEART, d(1), d(1)).unwrap(), vec![full.summary.clone()]);
    assert_eq!(health.latest_series_day(HEART, d(1)).unwrap(), Some(full.data.clone()));

    // A day with nothing in it comes back with nothing in it.
    let bare = SeriesRow {
        series: GLUCOSE.into(),
        day: d(1),
        data: SeriesDay::default(),
        summary: SeriesSummary::default(),
    };
    put_rows(health, std::slice::from_ref(&bare));
    assert_eq!(health.series_days(GLUCOSE, d(1), d(1)).unwrap(), vec![SeriesDay::default()]);
    assert_eq!(
        health.series_summaries(GLUCOSE, d(1), d(1)).unwrap(),
        vec![SeriesSummary::default()]
    );

    clear_series(health);
}

fn putting_a_series_day_again_replaces_both_payloads(health: &dyn HealthStore) {
    let first = row(HEART, d(2), 60.0);
    put_rows(health, std::slice::from_ref(&first));
    let second = row(HEART, d(2), 90.0);
    put_rows(health, std::slice::from_ref(&second));

    assert_eq!(health.series_days(HEART, d(0), d(5)).unwrap(), vec![second.data.clone()]);
    assert_eq!(
        health.series_summaries(HEART, d(0), d(5)).unwrap(),
        vec![second.summary.clone()],
        "the summary moves with the samples"
    );
    assert_eq!(health.series_extents().unwrap().iter().map(|e| e.days).sum::<u64>(), 1);

    // Two rows for one day in one batch: the later wins, as it would in the
    // table.
    let (early, late) = (row(HEART, d(3), 50.0), row(HEART, d(3), 55.0));
    put_rows(health, &[early, late.clone()]);
    assert_eq!(health.series_days(HEART, d(3), d(3)).unwrap(), vec![late.data]);

    clear_series(health);
}

fn series_are_told_apart(health: &dyn HealthStore) {
    // The same days under two series: each is read on its own and deleted on
    // its own.
    let heart: Vec<SeriesRow> = (0..3).map(|n| row(HEART, d(n), 60.0 + n as f64)).collect();
    let glucose: Vec<SeriesRow> = (0..3).map(|n| row(GLUCOSE, d(n), 100.0 + n as f64)).collect();
    put_rows(health, &heart);
    put_rows(health, &glucose);

    let found = health.series_days(HEART, d(0), d(2)).unwrap();
    assert_eq!(found, heart.iter().map(|r| r.data.clone()).collect::<Vec<_>>());
    let found = health.series_summaries(GLUCOSE, d(0), d(2)).unwrap();
    assert_eq!(found, glucose.iter().map(|r| r.summary.clone()).collect::<Vec<_>>());
    assert!(health.series_days("a series nobody wrote", d(0), d(2)).unwrap().is_empty());
    assert!(health.series_summaries("a series nobody wrote", d(0), d(2)).unwrap().is_empty());

    assert_eq!(health.delete_series(HEART, d(0), d(2)).unwrap(), 3);
    assert!(health.series_days(HEART, d(0), d(2)).unwrap().is_empty());
    assert_eq!(health.series_days(GLUCOSE, d(0), d(2)).unwrap().len(), 3, "the other is untouched");

    clear_series(health);
}

fn series_days_are_inclusive_and_in_day_order(health: &dyn HealthStore) {
    // Written out of order, as an import that walks the file would.
    let rows: Vec<SeriesRow> =
        [5, 1, 3, 2, 4].iter().map(|n| row(HEART, d(*n), *n as f64)).collect();
    put_rows(health, &rows);

    let days = health.series_days(HEART, d(2), d(4)).unwrap();
    assert_eq!(days_of(&days), vec![d(2), d(3), d(4)], "both ends inclusive, by day");
    let summaries = health.series_summaries(HEART, d(2), d(4)).unwrap();
    assert_eq!(
        summaries.iter().map(|s| s.day.unwrap()).collect::<Vec<_>>(),
        vec![d(2), d(3), d(4)]
    );
    assert_eq!(days_of(&health.series_days(HEART, d(0), d(100)).unwrap()).len(), 5);
    assert_eq!(days_of(&health.series_days(HEART, d(3), d(3)).unwrap()), vec![d(3)]);
    assert!(health.series_days(HEART, d(6), d(100)).unwrap().is_empty());
    assert!(health.series_days(HEART, d(4), d(2)).unwrap().is_empty(), "a backwards range");

    clear_series(health);
}

fn the_latest_series_day_is_the_latest_at_or_before(health: &dyn HealthStore) {
    let rows: Vec<SeriesRow> = [10, 30, 20].iter().map(|n| row(HEART, d(*n), *n as f64)).collect();
    put_rows(health, &rows);
    put_rows(health, &[row(GLUCOSE, d(40), 99.0)]);

    let latest = |on: i32| health.latest_series_day(HEART, d(on)).unwrap().map(|s| s.day.unwrap());
    assert_eq!(latest(25), Some(d(20)), "the nearest before");
    assert_eq!(latest(30), Some(d(30)), "on the day itself counts");
    assert_eq!(latest(100), Some(d(30)), "long after is the last there is");
    assert_eq!(latest(10), Some(d(10)));
    assert_eq!(latest(9), None, "nothing at or before the first day");
    assert!(health.latest_series_day("a series nobody wrote", d(100)).unwrap().is_none());

    clear_series(health);
}

fn deleting_series_days_answers_how_many_went(health: &dyn HealthStore) {
    let rows: Vec<SeriesRow> = (1..=5).map(|n| row(HEART, d(n), n as f64)).collect();
    put_rows(health, &rows);
    put_rows(health, &[row(GLUCOSE, d(2), 100.0)]);

    assert_eq!(health.delete_series(HEART, d(2), d(3)).unwrap(), 2, "both ends inclusive");
    assert_eq!(days_of(&health.series_days(HEART, d(0), d(100)).unwrap()), vec![d(1), d(4), d(5)]);
    assert_eq!(
        health.series_summaries(HEART, d(0), d(100)).unwrap().len(),
        3,
        "the summary goes with the day"
    );
    assert_eq!(health.delete_series(HEART, d(2), d(3)).unwrap(), 0, "already gone");
    assert_eq!(health.series_days(GLUCOSE, d(0), d(100)).unwrap().len(), 1, "another series");
    assert_eq!(health.delete_series(HEART, d(-100), d(100)).unwrap(), 3);

    clear_series(health);
}

fn series_extents_name_every_series_with_data(health: &dyn HealthStore) {
    let heart: Vec<SeriesRow> = [3, 9, 4].iter().map(|n| row(HEART, d(*n), *n as f64)).collect();
    put_rows(health, &heart);
    put_rows(health, &[row(GLUCOSE, d(7), 100.0)]);

    let extents = sorted_by_series(health.series_extents().unwrap());
    let want = sorted_by_series(vec![
        SeriesExtent { series: HEART.into(), first: d(3), last: d(9), days: 3 },
        SeriesExtent { series: GLUCOSE.into(), first: d(7), last: d(7), days: 1 },
    ]);
    assert_eq!(extents, want);

    // A series with nothing left is not listed.
    health.delete_series(GLUCOSE, d(7), d(7)).unwrap();
    assert_eq!(health.series_extents().unwrap().len(), 1);
    // And one that lost its edges shrinks to what is left.
    health.delete_series(HEART, d(3), d(3)).unwrap();
    health.delete_series(HEART, d(9), d(9)).unwrap();
    assert_eq!(
        health.series_extents().unwrap(),
        vec![SeriesExtent { series: HEART.into(), first: d(4), last: d(4), days: 1 }]
    );

    clear_series(health);
}

fn sorted_by_series(mut extents: Vec<SeriesExtent>) -> Vec<SeriesExtent> {
    extents.sort_by(|a, b| a.series.cmp(&b.series));
    extents
}

fn a_large_series_batch_lands_whole_in_one_call(health: &dyn HealthStore) {
    // More than a year of one metric, in one batch, every day of it there
    // afterwards.
    let batch: Vec<SeriesRow> =
        (0..400).map(|n| row(HEART, d(n), 40.0 + (n % 60) as f64)).collect();
    health.put_series(&batch).expect("put_series");

    let days = health.series_days(HEART, d(0), d(399)).unwrap();
    assert_eq!(days.len(), batch.len());
    assert_eq!(days, batch.iter().map(|r| r.data.clone()).collect::<Vec<_>>(), "in day order");
    let summaries = health.series_summaries(HEART, d(0), d(399)).unwrap();
    assert_eq!(summaries, batch.iter().map(|r| r.summary.clone()).collect::<Vec<_>>());
    assert_eq!(
        health.series_extents().unwrap(),
        vec![SeriesExtent { series: HEART.into(), first: d(0), last: d(399), days: 400 }]
    );
    assert_eq!(health.delete_series(HEART, d(0), d(399)).unwrap(), 400);
    assert!(health.series_extents().unwrap().is_empty());
}

fn settings_default_until_saved_and_round_trip(health: &dyn HealthStore) {
    assert_eq!(health.health_settings().unwrap(), HealthSettings::default(), "never saved");

    let changed = full_settings();
    assert_ne!(changed, HealthSettings::default());
    health.put_health_settings(&changed).expect("put_health_settings");
    assert_eq!(health.health_settings().unwrap(), changed, "every field comes back");

    // A second save replaces the first; it is one row, not two.
    let again = HealthSettings { stale_hours: 6, glucose_low: None, ..changed };
    health.put_health_settings(&again).unwrap();
    assert_eq!(health.health_settings().unwrap(), again);

    health.put_health_settings(&HealthSettings::default()).unwrap();
    assert_eq!(health.health_settings().unwrap(), HealthSettings::default());
}

fn a_source_round_trips_every_field(health: &dyn HealthStore) {
    let mut full = full_source();
    let bare = source("typed in", -10);
    health.put_source(&full).expect("put_source");
    health.put_source(&bare).expect("put_source");

    assert_eq!(health.get_source(full.id).unwrap(), full, "unchanged by a round trip");
    assert_eq!(health.get_source(bare.id).unwrap(), bare, "and with nothing optional set");
    assert_eq!(
        sorted_by_id(health.list_sources().unwrap(), |s| s.id),
        sorted_by_id(vec![full.clone(), bare.clone()], |s| s.id)
    );

    // Replaced by id: a paused source resumed, what it heard forgotten.
    full.paused = false;
    full.status = SourceStatus::default();
    full.name = "Hari's other iPhone".into();
    full.updated_at = t(20);
    health.put_source(&full).unwrap();
    assert_eq!(health.list_sources().unwrap().len(), 2, "put must not duplicate");
    assert_eq!(health.get_source(full.id).unwrap(), full);

    health.delete_source(bare.id).expect("delete_source");
    super::assert_not_found(health.get_source(bare.id));
    assert_eq!(health.list_sources().unwrap(), vec![full.clone()]);
    health.delete_source(full.id).unwrap();
    assert!(health.list_sources().unwrap().is_empty());
}

/// `list` ordered by id, for the lists whose order the trait leaves open.
fn sorted_by_id<T, I: Ord>(mut list: Vec<T>, id: impl Fn(&T) -> I) -> Vec<T> {
    list.sort_by_key(|item| id(item));
    list
}

fn a_workout_round_trips_every_field(health: &dyn HealthStore) {
    let mut full = full_workout("key-full");
    let bare = workout("key-bare", 4_000, 4_600);
    put_workouts(health, &[&full, &bare]);

    assert_eq!(health.get_workout(full.id).unwrap(), full, "unchanged by a round trip");
    assert_eq!(health.get_workout(bare.id).unwrap(), bare, "and with nothing optional set");
    assert_eq!(health.workout_by_key("key-full").unwrap(), Some(full.clone()));
    assert_eq!(health.workout_by_key("key-bare").unwrap(), Some(bare.clone()));
    assert_eq!(every_workout(health), vec![full.clone(), bare.clone()]);

    // Every activity survives its own name.
    for activity in [Activity::Cycling, Activity::StrengthTraining, Activity::Hiit, Activity::Other]
    {
        let mut other = workout(&format!("key-{activity:?}"), 5_000, 5_100);
        other.activity = activity;
        put_workouts(health, &[&other]);
        assert_eq!(health.get_workout(other.id).unwrap().activity, activity);
        health.delete_workout(other.id).unwrap();
    }

    // Replaced by id, and a new key is found under the new one only.
    full.key = "key-full-again".into();
    full.distance_m = None;
    full.route = None;
    full.updated_at = t(50);
    put_workouts(health, &[&full]);
    assert_eq!(health.get_workout(full.id).unwrap(), full);
    assert_eq!(every_workout(health).len(), 2, "put must not duplicate");
    assert!(health.workout_by_key("key-full").unwrap().is_none(), "the old key is let go");
    assert_eq!(health.workout_by_key("key-full-again").unwrap(), Some(full.clone()));

    health.delete_workout(bare.id).expect("delete_workout");
    super::assert_not_found(health.get_workout(bare.id));
    assert!(health.workout_by_key("key-bare").unwrap().is_none(), "its key goes with it");
    assert_eq!(every_workout(health), vec![full]);

    clear_workouts(health);
}

fn workouts_are_every_one_overlapping_the_range_and_nothing_touching_it(health: &dyn HealthStore) {
    // The range is 100..200.
    let ends_at_from = workout("a", 50, 100);
    let starts_at_to = workout("b", 200, 250);
    let wholly_before = workout("c", 0, 50);
    let wholly_after = workout("d", 250, 300);
    let over_from = workout("e", 90, 110);
    let over_to = workout("f", 190, 210);
    let inside = workout("g", 120, 150);
    let around = workout("h", 0, 300);
    let exactly = workout("i", 100, 200);
    let just_in = workout("j", 99, 101);
    let instant_inside = workout("k", 150, 150);
    let instant_at_from = workout("l", 100, 100);
    put_workouts(
        health,
        &[
            &ends_at_from,
            &starts_at_to,
            &wholly_before,
            &wholly_after,
            &over_from,
            &over_to,
            &inside,
            &around,
            &exactly,
            &just_in,
            &instant_inside,
            &instant_at_from,
        ],
    );

    let found = health.workouts(t(100), t(200)).unwrap();
    assert_eq!(
        sorted(found.iter().map(|w| w.id).collect()),
        sorted(vec![
            over_from.id,
            over_to.id,
            inside.id,
            around.id,
            exactly.id,
            just_in.id,
            instant_inside.id,
        ]),
        "every workout sharing time with 100..200 (`start < to`, `end > from`), and none that \
         only touches it"
    );
    assert!(!found.iter().any(|w| w.id == ends_at_from.id), "ending at `from` is not inside");
    assert!(!found.iter().any(|w| w.id == starts_at_to.id), "starting at `to` is not inside");

    // Laid end to end, two ranges count a boundary workout once each side
    // only where it actually crosses.
    let left: Vec<WorkoutId> =
        health.workouts(t(50), t(100)).unwrap().iter().map(|w| w.id).collect();
    assert!(left.contains(&ends_at_from.id) && !left.contains(&exactly.id));

    clear_workouts(health);
}

fn workouts_come_back_in_start_order_then_id(health: &dyn HealthStore) {
    let late = workout("late", 300, 400);
    let early = workout("early", 0, 100);
    let mut tied_a = workout("tied-a", 200, 250);
    let mut tied_b = workout("tied-b", 200, 260);
    // Ids in a known order, whatever order they were minted in.
    if tied_a.id > tied_b.id {
        std::mem::swap(&mut tied_a.id, &mut tied_b.id);
    }
    // Written out of order.
    put_workouts(health, &[&late, &tied_b, &early, &tied_a]);

    let ids: Vec<WorkoutId> = every_workout(health).iter().map(|w| w.id).collect();
    assert_eq!(ids, vec![early.id, tied_a.id, tied_b.id, late.id], "by start, then by id");

    clear_workouts(health);
}

fn a_batch_of_workouts_is_refused_whole_when_a_key_is_held_by_another_id(health: &dyn HealthStore) {
    let held = workout("k-held", 0, 100);
    put_workouts(health, &[&held]);

    // A new workout, then one under a different id claiming the held key:
    // refused as invalid, and the new one before it is not written either.
    let fresh = workout("k-fresh", 200, 300);
    let thief = workout("k-held", 400, 500);
    let err = health
        .put_workouts(&[fresh.clone(), thief.clone()])
        .expect_err("a key held by another id must be refused");
    assert_eq!(err.code(), "invalid", "refused as invalid, got {err:?}");
    super::assert_not_found(health.get_workout(fresh.id));
    super::assert_not_found(health.get_workout(thief.id));
    assert_eq!(health.get_workout(held.id).unwrap(), held, "the holder is untouched");

    // Nor does an update earlier in a refused batch land.
    let mut changed = held.clone();
    changed.note = "this must not be stored".into();
    assert_eq!(health.put_workouts(&[changed, thief.clone()]).unwrap_err().code(), "invalid");
    assert_eq!(health.get_workout(held.id).unwrap(), held, "all or nothing");

    // Two different ids wanting one key that nobody holds yet: the batch
    // disagrees with itself, and none of it is written.
    let one = workout("k-twin", 600, 700);
    let two = workout("k-twin", 800, 900);
    assert_eq!(health.put_workouts(&[one.clone(), two.clone()]).unwrap_err().code(), "invalid");
    super::assert_not_found(health.get_workout(one.id));
    super::assert_not_found(health.get_workout(two.id));
    assert_eq!(every_workout(health), vec![held.clone()]);

    // The same id under its own key is an update, not a clash.
    let mut again = held.clone();
    again.avg_hr = Some(120.0);
    put_workouts(health, &[&again]);
    assert_eq!(health.get_workout(held.id).unwrap(), again);
    assert_eq!(every_workout(health).len(), 1);

    // A new key for the same id is an update too, and frees the old one --
    // which another id may then take.
    let mut rekeyed = again.clone();
    rekeyed.key = "k-moved".into();
    put_workouts(health, &[&rekeyed]);
    assert!(health.workout_by_key("k-held").unwrap().is_none());
    put_workouts(health, &[&thief]);
    assert_eq!(health.workout_by_key("k-held").unwrap().map(|w| w.id), Some(thief.id));
    assert_eq!(health.workout_by_key("k-moved").unwrap().map(|w| w.id), Some(held.id));

    // And a deleted workout's key is free.
    health.delete_workout(thief.id).unwrap();
    let heir = workout("k-held", 1_000, 1_100);
    put_workouts(health, &[&heir]);
    assert_eq!(health.workout_by_key("k-held").unwrap().map(|w| w.id), Some(heir.id));

    clear_workouts(health);
}

fn a_medication_round_trips_every_field(health: &dyn HealthStore) {
    let mut full = full_medication(3);
    let bare = medication("Vitamin D", 0, 0);
    health.put_medication(&full).expect("put_medication");
    health.put_medication(&bare).expect("put_medication");

    assert_eq!(health.get_medication(full.id).unwrap(), full, "unchanged by a round trip");
    assert_eq!(health.get_medication(bare.id).unwrap(), bare, "and with nothing optional set");

    // Every shape of schedule, kind and status survives.
    let schedules = [
        MedSchedule::Daily { times: vec![time(8, 0, 0, 0), time(20, 0, 0, 0)] },
        MedSchedule::Weekly { days: vec![6, 7], times: vec![time(9, 15, 0, 0)] },
        MedSchedule::EveryNDays { n: 3, from: date(2026, 10, 1), times: vec![time(7, 0, 0, 0)] },
        MedSchedule::AsNeeded,
    ];
    for (n, schedule) in schedules.into_iter().enumerate() {
        let mut other = medication(&format!("Schedule {n}"), 10 + n as i64, 0);
        other.schedule = schedule;
        other.kind = [MedKind::Prescription, MedKind::OverTheCounter, MedKind::Supplement][n % 3];
        other.status = [MedStatus::Active, MedStatus::Paused, MedStatus::Stopped][n % 3];
        health.put_medication(&other).unwrap();
        assert_eq!(health.get_medication(other.id).unwrap(), other);
        health.delete_medication(other.id).unwrap();
    }

    // Replaced by id.
    full.status = MedStatus::Stopped;
    full.supply = None;
    full.history.pop();
    full.sort_order = 1;
    full.updated_at = t(40);
    health.put_medication(&full).unwrap();
    assert_eq!(health.list_medications().unwrap().len(), 2, "put must not duplicate");
    assert_eq!(health.get_medication(full.id).unwrap(), full);

    health.delete_medication(bare.id).expect("delete_medication");
    super::assert_not_found(health.get_medication(bare.id));
    assert_eq!(health.list_medications().unwrap(), vec![full.clone()]);
    health.delete_medication(full.id).unwrap();
    assert!(health.list_medications().unwrap().is_empty());
}

fn medications_come_back_in_their_order(health: &dyn HealthStore) {
    let second = medication("Second", 2, 0);
    let first = medication("First", 1, 100);
    let negative = medication("Before everything", -5, 200);
    let mut tied_a = medication("Tied A", 3, 0);
    let mut tied_b = medication("Tied B", 3, 50);
    if tied_a.id > tied_b.id {
        std::mem::swap(&mut tied_a.id, &mut tied_b.id);
    }
    for m in [&tied_b, &second, &first, &tied_a, &negative] {
        health.put_medication(m).expect("put_medication");
    }

    let ids: Vec<MedicationId> = health.list_medications().unwrap().iter().map(|m| m.id).collect();
    assert_eq!(
        ids,
        vec![negative.id, first.id, second.id, tied_a.id, tied_b.id],
        "by sort order, then by id"
    );

    // Moved to the front by its order.
    let mut moved = tied_b.clone();
    moved.sort_order = -10;
    health.put_medication(&moved).unwrap();
    assert_eq!(health.list_medications().unwrap()[0].id, moved.id);

    for m in [&moved, &second, &first, &tied_a, &negative] {
        health.delete_medication(m.id).unwrap();
    }
    assert!(health.list_medications().unwrap().is_empty());
}

fn a_dose_round_trips_every_field(health: &dyn HealthStore) {
    let medication = stored_medication(health, "doses");
    let mut full = full_dose(&medication, "dose-key-full", 600);
    let bare = dose(&medication, "", 700);
    put_doses(health, &[&full, &bare]);

    assert_eq!(health.get_dose(full.id).unwrap(), full, "unchanged by a round trip");
    assert_eq!(health.get_dose(bare.id).unwrap(), bare, "and with nothing optional set");
    assert_eq!(health.dose_by_key("dose-key-full").unwrap(), Some(full.clone()));
    assert_eq!(every_dose(health, None), vec![full.clone(), bare.clone()]);

    // Every status and origin survives.
    for (n, (status, origin)) in [
        (DoseStatus::Taken, DoseOrigin::Reminder),
        (DoseStatus::Missed, DoseOrigin::Assistant),
        (DoseStatus::Skipped, DoseOrigin::Manual),
    ]
    .into_iter()
    .enumerate()
    {
        let mut other = dose(&medication, "", 800 + n as i64);
        other.status = status;
        other.origin = origin;
        put_doses(health, &[&other]);
        assert_eq!(health.get_dose(other.id).unwrap(), other);
        health.delete_dose(other.id).unwrap();
    }

    // Replaced by id.
    full.status = DoseStatus::Taken;
    full.amount = None;
    full.updated_at = t(900);
    put_doses(health, &[&full]);
    assert_eq!(health.get_dose(full.id).unwrap(), full);
    assert_eq!(every_dose(health, None).len(), 2, "put must not duplicate");

    health.delete_dose(bare.id).expect("delete_dose");
    super::assert_not_found(health.get_dose(bare.id));
    assert_eq!(every_dose(health, None), vec![full.clone()]);
    health.delete_dose(full.id).unwrap();
    assert!(health.dose_by_key("dose-key-full").unwrap().is_none(), "its key goes with it");

    health.delete_medication(medication.id).unwrap();
}

fn doses_are_those_in_the_range_in_time_order(health: &dyn HealthStore) {
    let thyroid = stored_medication(health, "thyroid");
    let vitamin = stored_medication(health, "vitamin");

    // The range is 100..200: `from <= at < to`.
    let before = dose(&thyroid, "", 99);
    let at_from = dose(&thyroid, "", 100);
    let inside = dose(&vitamin, "", 150);
    let at_to = dose(&thyroid, "", 200);
    let after = dose(&thyroid, "", 300);
    let mut tied_a = dose(&thyroid, "", 120);
    let mut tied_b = dose(&vitamin, "", 120);
    if tied_a.id > tied_b.id {
        std::mem::swap(&mut tied_a.id, &mut tied_b.id);
    }
    // Written out of order.
    put_doses(health, &[&after, &tied_b, &at_to, &inside, &before, &tied_a, &at_from]);

    let ids = |doses: Vec<Dose>| doses.iter().map(|d| d.id).collect::<Vec<DoseId>>();
    assert_eq!(
        ids(health.doses(t(100), t(200), None).unwrap()),
        vec![at_from.id, tied_a.id, tied_b.id, inside.id],
        "`from <= at < to`, by time, then by id"
    );
    assert_eq!(
        ids(health.doses(t(100), t(200), Some(thyroid.id)).unwrap()),
        vec![at_from.id, tied_a.id],
        "only one medication's when asked"
    );
    assert_eq!(
        ids(health.doses(t(100), t(200), Some(vitamin.id)).unwrap()),
        vec![tied_b.id, inside.id]
    );
    assert!(health.doses(t(100), t(100), None).unwrap().is_empty(), "an empty range");
    assert!(health.doses(t(100), t(200), Some(MedicationId::new())).unwrap().is_empty());
    assert_eq!(every_dose(health, None).len(), 7);

    health.delete_medication(thyroid.id).unwrap();
    health.delete_medication(vitamin.id).unwrap();
    assert!(every_dose(health, None).is_empty());
}

fn a_batch_of_doses_is_refused_whole_when_a_key_is_held_by_another_id(health: &dyn HealthStore) {
    let medication = stored_medication(health, "unique");
    let held = dose(&medication, "k-held", 0);
    put_doses(health, &[&held]);

    let fresh = dose(&medication, "k-fresh", 10);
    let thief = dose(&medication, "k-held", 20);
    let err = health
        .put_doses(&[fresh.clone(), thief.clone()])
        .expect_err("a key held by another id must be refused");
    assert_eq!(err.code(), "invalid", "refused as invalid, got {err:?}");
    super::assert_not_found(health.get_dose(fresh.id));
    super::assert_not_found(health.get_dose(thief.id));
    assert_eq!(health.get_dose(held.id).unwrap(), held, "the holder is untouched");

    let mut changed = held.clone();
    changed.note = "this must not be stored".into();
    assert_eq!(health.put_doses(&[changed, thief.clone()]).unwrap_err().code(), "invalid");
    assert_eq!(health.get_dose(held.id).unwrap(), held, "all or nothing");

    let one = dose(&medication, "k-twin", 30);
    let two = dose(&medication, "k-twin", 40);
    assert_eq!(health.put_doses(&[one.clone(), two.clone()]).unwrap_err().code(), "invalid");
    super::assert_not_found(health.get_dose(one.id));
    super::assert_not_found(health.get_dose(two.id));

    // The empty key is nobody's: any number of doses may share it.
    let (typed_a, typed_b, typed_c) =
        (dose(&medication, "", 50), dose(&medication, "", 60), dose(&medication, "", 70));
    put_doses(health, &[&typed_a, &typed_b, &typed_c]);
    assert_eq!(every_dose(health, None).len(), 4);
    assert!(health.dose_by_key("").unwrap().is_none(), "and it finds none of them");

    // The same id under its own key is an update, a new key frees the old
    // one for another id, and a dose that loses its key holds none.
    let mut rekeyed = held.clone();
    rekeyed.key = "k-moved".into();
    put_doses(health, &[&rekeyed]);
    put_doses(health, &[&thief]);
    assert_eq!(health.dose_by_key("k-held").unwrap().map(|d| d.id), Some(thief.id));
    assert_eq!(health.dose_by_key("k-moved").unwrap().map(|d| d.id), Some(held.id));
    let mut keyless = rekeyed.clone();
    keyless.key.clear();
    put_doses(health, &[&keyless]);
    assert!(health.dose_by_key("k-moved").unwrap().is_none());
    let heir = dose(&medication, "k-moved", 80);
    put_doses(health, &[&heir]);
    assert_eq!(health.dose_by_key("k-moved").unwrap().map(|d| d.id), Some(heir.id));

    health.delete_medication(medication.id).unwrap();
}

fn a_large_batch_of_keyed_doses_is_one_call(health: &dyn HealthStore) {
    // More than any `IN (...)` chunk, in one batch, keys and all.
    let medication = stored_medication(health, "many");
    let batch: Vec<Dose> =
        (0..1_203).map(|n| dose(&medication, &format!("bulk-{n:05}"), n)).collect();
    health.put_doses(&batch).expect("put_doses");
    assert_eq!(every_dose(health, Some(medication.id)).len(), batch.len());
    assert_eq!(
        health.dose_by_key("bulk-01000").unwrap().map(|d| d.id),
        Some(batch[1_000].id),
        "every key was written"
    );

    // Written again, the same ids under the same keys: an update, not a
    // clash with themselves.
    health.put_doses(&batch).expect("put_doses again");
    assert_eq!(every_dose(health, Some(medication.id)).len(), batch.len());

    // One of them under another's key, in the middle of a large batch: the
    // whole batch is refused.
    let mut stolen = batch.clone();
    stolen[1_100].key = "bulk-00007".into();
    assert_eq!(health.put_doses(&stolen).unwrap_err().code(), "invalid");
    assert_eq!(health.get_dose(batch[1_100].id).unwrap(), batch[1_100], "nothing was written");

    // And deleted as one call, too: a medication takes all of its doses.
    health.delete_medication(medication.id).unwrap();
    assert!(every_dose(health, None).is_empty());
}

fn deleting_a_medication_takes_its_doses_and_only_its_doses(health: &dyn HealthStore) {
    let doomed = stored_medication(health, "doomed");
    let kept = stored_medication(health, "kept");
    let gone: Vec<Dose> = (0..3).map(|n| dose(&doomed, &format!("gone-{n}"), n * 10)).collect();
    let stays: Vec<Dose> = (0..2).map(|n| dose(&kept, &format!("stays-{n}"), n * 10)).collect();
    health.put_doses(&gone).unwrap();
    health.put_doses(&stays).unwrap();

    health.delete_medication(doomed.id).expect("delete_medication");

    super::assert_not_found(health.get_medication(doomed.id));
    for dose in &gone {
        super::assert_not_found(health.get_dose(dose.id));
        assert!(health.dose_by_key(&dose.key).unwrap().is_none(), "and their keys are free");
    }
    assert!(every_dose(health, Some(doomed.id)).is_empty());
    assert_eq!(every_dose(health, None), stays, "another medication's doses are where they were");
    assert_eq!(health.get_medication(kept.id).unwrap(), kept);

    // A key a deleted dose held is free for another.
    let heir = dose(&kept, "gone-0", 500);
    put_doses(health, &[&heir]);
    assert_eq!(health.dose_by_key("gone-0").unwrap().map(|d| d.id), Some(heir.id));

    health.delete_medication(kept.id).unwrap();
    assert!(every_dose(health, None).is_empty());
}

fn a_condition_round_trips_every_field(health: &dyn HealthStore) {
    let mut full = full_condition();
    let bare = condition("Seasonal allergies", -10);
    health.put_condition(&full).expect("put_condition");
    health.put_condition(&bare).expect("put_condition");

    assert_eq!(health.get_condition(full.id).unwrap(), full, "unchanged by a round trip");
    assert_eq!(health.get_condition(bare.id).unwrap(), bare, "and with nothing optional set");
    assert_eq!(
        sorted_by_id(health.list_conditions().unwrap(), |c| c.id),
        sorted_by_id(vec![full.clone(), bare.clone()], |c| c.id)
    );

    // Every template and status survives.
    for (template, status) in [
        (TemplateKey::Type1Diabetes, ConditionStatus::Active),
        (TemplateKey::Type2Diabetes, ConditionStatus::Resolved),
    ] {
        let mut other = condition("Diabetes", 0);
        other.template = Some(template);
        other.status = status;
        health.put_condition(&other).unwrap();
        assert_eq!(health.get_condition(other.id).unwrap(), other);
        health.delete_condition(other.id).unwrap();
    }

    // Replaced by id: a plan edited down to nothing comes back as nothing.
    full.plan = CarePlan::default();
    full.status = ConditionStatus::Resolved;
    full.template = None;
    full.updated_at = t(30);
    health.put_condition(&full).unwrap();
    assert_eq!(health.list_conditions().unwrap().len(), 2, "put must not duplicate");
    assert_eq!(health.get_condition(full.id).unwrap(), full);

    health.delete_condition(bare.id).expect("delete_condition");
    super::assert_not_found(health.get_condition(bare.id));
    assert_eq!(health.list_conditions().unwrap(), vec![full.clone()]);
    health.delete_condition(full.id).unwrap();
    assert!(health.list_conditions().unwrap().is_empty());
}

fn lab_results_round_trip_and_come_back_by_collection_date(health: &dyn HealthStore) {
    let mut full = full_lab(date(2026, 10, 2));
    let bare = lab("Glucose", date(2026, 9, 1));
    let latest = lab("A1c", date(2026, 10, 20));
    // The same day, by id.
    let mut tied_a = lab("Tied A", date(2026, 10, 2));
    let mut tied_b = lab("Tied B", date(2026, 10, 2));
    if tied_a.id > tied_b.id {
        std::mem::swap(&mut tied_a.id, &mut tied_b.id);
    }
    // One call for the lot, written out of order.
    health
        .put_labs(&[latest.clone(), tied_b.clone(), full.clone(), bare.clone(), tied_a.clone()])
        .expect("put_labs");

    assert_eq!(health.get_lab(full.id).unwrap(), full, "unchanged by a round trip");
    assert_eq!(health.get_lab(bare.id).unwrap(), bare, "and with nothing optional set");
    let ids: Vec<LabResultId> = health.list_labs().unwrap().iter().map(|l| l.id).collect();
    let mut same_day = vec![full.id, tied_a.id, tied_b.id];
    same_day.sort();
    let mut want = vec![bare.id];
    want.extend(same_day);
    want.push(latest.id);
    assert_eq!(ids, want, "by collection date, then by id");

    // Every flag and origin survives.
    for (flag, origin) in [
        (LabFlag::Low, LabOrigin::Fhir),
        (LabFlag::High, LabOrigin::Manual),
        (LabFlag::Abnormal, LabOrigin::Extracted),
        (LabFlag::Critical, LabOrigin::Manual),
    ] {
        let mut other = lab("Flagged", date(2026, 11, 1));
        other.flag = flag;
        other.origin = origin;
        health.put_labs(std::slice::from_ref(&other)).unwrap();
        assert_eq!(health.get_lab(other.id).unwrap(), other);
        health.delete_lab(other.id).unwrap();
    }

    // Replaced by id, and moved by its date.
    full.collected = date(2026, 12, 1);
    full.value = Some(9.9);
    full.updated_at = t(60);
    health.put_labs(std::slice::from_ref(&full)).unwrap();
    assert_eq!(health.list_labs().unwrap().len(), 5, "put must not duplicate");
    assert_eq!(health.list_labs().unwrap().last().map(|l| l.id), Some(full.id));
    assert_eq!(health.get_lab(full.id).unwrap(), full);

    health.delete_lab(bare.id).expect("delete_lab");
    super::assert_not_found(health.get_lab(bare.id));
    assert_eq!(health.list_labs().unwrap().len(), 4);
    for l in [&latest, &tied_a, &tied_b, &full] {
        health.delete_lab(l.id).unwrap();
    }
    assert!(health.list_labs().unwrap().is_empty());
}

fn documents_round_trip_and_come_back_newest_first(health: &dyn HealthStore) {
    let mut full = full_document(Some(date(2026, 10, 2)));
    let bare = document("Insurance card", None);
    let newest = document("Discharge summary", Some(date(2026, 11, 3)));
    let oldest = document("Vaccination record", Some(date(2019, 4, 1)));
    let undated_too = document("A letter", None);
    for doc in [&full, &bare, &newest, &oldest, &undated_too] {
        health.put_document(doc).expect("put_document");
    }

    assert_eq!(health.get_document(full.id).unwrap(), full, "unchanged by a round trip");
    assert_eq!(health.get_document(bare.id).unwrap(), bare, "and with nothing optional set");

    let ids: Vec<HealthDocumentId> =
        health.list_documents().unwrap().iter().map(|d| d.id).collect();
    let mut undated = vec![bare.id, undated_too.id];
    undated.sort_by(|a, b| b.cmp(a));
    let mut want = vec![newest.id, full.id, oldest.id];
    want.extend(undated);
    assert_eq!(ids, want, "by date descending, undated last, then by id descending");

    // Every kind and extraction state survives.
    for (kind, extraction) in [
        (DocKind::VisitNote, ExtractionState::Running),
        (DocKind::Imaging, ExtractionState::Done),
        (DocKind::Fhir, ExtractionState::Failed),
        (DocKind::Ecg, ExtractionState::None),
    ] {
        let mut other = document("Kinds", Some(date(2026, 1, 1)));
        other.kind = kind;
        other.extraction = extraction;
        health.put_document(&other).unwrap();
        assert_eq!(health.get_document(other.id).unwrap(), other);
        health.delete_document(other.id).unwrap();
    }

    // Replaced by id, and a date added moves it up the list.
    full.date = None;
    full.files.pop();
    full.extraction = ExtractionState::Done;
    full.pending_labs.clear();
    full.updated_at = t(70);
    health.put_document(&full).unwrap();
    assert_eq!(health.list_documents().unwrap().len(), 5, "put must not duplicate");
    assert_eq!(health.get_document(full.id).unwrap(), full);
    assert_eq!(health.list_documents().unwrap()[0].id, newest.id);
    assert_eq!(
        health.list_documents().unwrap().iter().filter(|d| d.date.is_none()).count(),
        3,
        "a dated document that lost its date joins the undated"
    );
    let mut dated = bare.clone();
    dated.date = Some(date(2027, 1, 1));
    health.put_document(&dated).unwrap();
    assert_eq!(health.list_documents().unwrap()[0].id, dated.id, "and the other way round");

    health.delete_document(newest.id).expect("delete_document");
    super::assert_not_found(health.get_document(newest.id));
    assert_eq!(health.list_documents().unwrap().len(), 4);
    for doc in [&full, &dated, &oldest, &undated_too] {
        health.delete_document(doc.id).unwrap();
    }
    assert!(health.list_documents().unwrap().is_empty());
}

fn an_allergy_round_trips_every_field(health: &dyn HealthStore) {
    let mut full = full_allergy();
    let bare = allergy("Peanuts", -10);
    health.put_allergy(&full).expect("put_allergy");
    health.put_allergy(&bare).expect("put_allergy");

    assert_eq!(health.get_allergy(full.id).unwrap(), full, "unchanged by a round trip");
    assert_eq!(health.get_allergy(bare.id).unwrap(), bare, "and with nothing optional set");
    assert_eq!(
        sorted_by_id(health.list_allergies().unwrap(), |a| a.id),
        sorted_by_id(vec![full.clone(), bare.clone()], |a| a.id)
    );

    // Every kind and severity survives.
    for (kind, severity) in [
        (AllergyKind::Environment, Severity::Moderate),
        (AllergyKind::Other, Severity::Severe),
        (AllergyKind::Food, Severity::Mild),
    ] {
        let mut other = allergy("Pollen", 0);
        other.kind = kind;
        other.severity = severity;
        health.put_allergy(&other).unwrap();
        assert_eq!(health.get_allergy(other.id).unwrap(), other);
        health.delete_allergy(other.id).unwrap();
    }

    // Replaced by id.
    full.severity = Severity::Mild;
    full.noted = None;
    full.updated_at = t(20);
    health.put_allergy(&full).unwrap();
    assert_eq!(health.list_allergies().unwrap().len(), 2, "put must not duplicate");
    assert_eq!(health.get_allergy(full.id).unwrap(), full);

    health.delete_allergy(bare.id).expect("delete_allergy");
    super::assert_not_found(health.get_allergy(bare.id));
    assert_eq!(health.list_allergies().unwrap(), vec![full.clone()]);
    health.delete_allergy(full.id).unwrap();
    assert!(health.list_allergies().unwrap().is_empty());
}

fn foods_round_trip_and_come_back_most_recently_used_first(health: &dyn HealthStore) {
    let mut full = full_food(FoodSource::Usda { fdc_id: 169_704 }, Some(100));
    health.put_food(&full).expect("put_food");
    let sources = [
        FoodSource::Own,
        FoodSource::OpenFoodFacts { code: "0123456789012".into() },
        FoodSource::Recipe { item_id: ItemId::new() },
        FoodSource::Estimate,
    ];
    for source in sources {
        let other = full_food(source, None);
        health.put_food(&other).unwrap();
        assert_eq!(health.get_food(other.id).unwrap(), other, "every source survives");
        health.delete_food(other.id).unwrap();
    }
    assert_eq!(health.get_food(full.id).unwrap(), full, "unchanged by a round trip");

    let recent = food("Recent", Some(500));
    let tied_a = food("Tied A", Some(300));
    let tied_b = food("Tied B", Some(300));
    let never_a = food("Never A", None);
    let never_b = food("Never B", None);
    for f in [&never_b, &tied_b, &never_a, &recent, &tied_a] {
        health.put_food(f).expect("put_food");
    }

    let tied = sorted(vec![tied_a.id, tied_b.id]);
    let never = sorted(vec![never_a.id, never_b.id]);
    let ids: Vec<FoodId> = health.list_foods().unwrap().iter().map(|f| f.id).collect();
    assert_eq!(
        ids,
        vec![recent.id, tied[0], tied[1], full.id, never[0], never[1]],
        "most recently used first, never used last, then by id"
    );

    // Eating it moves it to the front; the list is a view of last use.
    full.last_used = Some(t(900));
    full.use_count += 1;
    health.put_food(&full).unwrap();
    assert_eq!(health.list_foods().unwrap()[0].id, full.id);
    assert_eq!(health.list_foods().unwrap().len(), 6, "put must not duplicate");
    assert_eq!(health.get_food(full.id).unwrap(), full);

    health.delete_food(recent.id).expect("delete_food");
    super::assert_not_found(health.get_food(recent.id));
    for f in [&full, &never_a, &never_b, &tied_a, &tied_b] {
        health.delete_food(f.id).unwrap();
    }
    assert!(health.list_foods().unwrap().is_empty());
}

fn meals_round_trip_and_are_those_in_the_range_in_time_order(health: &dyn HealthStore) {
    let mut full = full_meal(150);
    let bare = meal("Toast", 120);
    put_meals(health, &[&full, &bare]);

    assert_eq!(health.get_meal(full.id).unwrap(), full, "unchanged by a round trip");
    assert_eq!(health.get_meal(bare.id).unwrap(), bare, "and with nothing optional set");

    // Every slot survives.
    for slot in [MealSlot::Breakfast, MealSlot::Lunch, MealSlot::Dinner, MealSlot::Snack] {
        let mut other = meal("Slots", 5_000);
        other.slot = slot;
        health.put_meal(&other).unwrap();
        assert_eq!(health.get_meal(other.id).unwrap(), other);
        health.delete_meal(other.id).unwrap();
    }
    health.delete_meal(full.id).unwrap();
    health.delete_meal(bare.id).unwrap();

    // The range is 100..200: `from <= at < to`.
    let before = meal("before", 99);
    let at_from = meal("at from", 100);
    let inside = meal("inside", 150);
    let at_to = meal("at to", 200);
    let after = meal("after", 300);
    let mut tied_a = meal("tied a", 120);
    let mut tied_b = meal("tied b", 120);
    if tied_a.id > tied_b.id {
        std::mem::swap(&mut tied_a.id, &mut tied_b.id);
    }
    put_meals(health, &[&after, &tied_b, &at_to, &inside, &before, &tied_a, &at_from]);

    let ids = |meals: Vec<Meal>| meals.iter().map(|m| m.id).collect::<Vec<MealId>>();
    assert_eq!(
        ids(health.meals(t(100), t(200)).unwrap()),
        vec![at_from.id, tied_a.id, tied_b.id, inside.id],
        "`from <= at < to`, by time, then by id"
    );
    assert!(health.meals(t(100), t(100)).unwrap().is_empty(), "an empty range");
    assert_eq!(every_meal(health).len(), 7);

    // Replaced by id, and moved by its time.
    let mut moved = inside.clone();
    moved.at = t(400);
    moved.photo = Some(FileId::new());
    health.put_meal(&moved).unwrap();
    assert_eq!(every_meal(health).len(), 7, "put must not duplicate");
    assert_eq!(health.get_meal(moved.id).unwrap(), moved);
    assert_eq!(
        ids(health.meals(t(100), t(200)).unwrap()),
        vec![at_from.id, tied_a.id, tied_b.id],
        "its time in the index moved with it"
    );

    full.at = t(10);
    health.put_meal(&full).unwrap();
    assert_eq!(health.meals(t(0), t(20)).unwrap(), vec![full.clone()]);
    for m in every_meal(health) {
        health.delete_meal(m.id).unwrap();
    }
    assert!(every_meal(health).is_empty());
}

fn put_meals(health: &dyn HealthStore, meals: &[&Meal]) {
    for meal in meals {
        health.put_meal(meal).expect("put_meal");
    }
}

fn deleting_anything_else_touches_nothing_else(health: &dyn HealthStore) {
    // A condition a medication names, a document a lab result and a meal
    // point at, a food a meal was made of, and a source a workout came from:
    // deleting the one leaves the other as it was, for the vault to tidy.
    let condition = condition("Diabetes", 0);
    health.put_condition(&condition).unwrap();
    let mut medication = medication("Insulin", 0, 0);
    medication.condition_ids = vec![condition.id];
    health.put_medication(&medication).unwrap();

    let document = document("Lab report", Some(date(2026, 10, 2)));
    health.put_document(&document).unwrap();
    let mut result = lab("A1c", date(2026, 10, 2));
    result.document_id = Some(document.id);
    health.put_labs(std::slice::from_ref(&result)).unwrap();

    let food = food("Rice", Some(10));
    health.put_food(&food).unwrap();
    let mut dinner = meal("Dinner", 10);
    dinner.items = vec![MealItem {
        food_id: Some(food.id),
        name: "Rice".into(),
        amount: 1.0,
        unit: "cup".into(),
        grams: Some(195.0),
        nutrients: bare_nutrients(),
        estimated: false,
        doubt: String::new(),
    }];
    health.put_meal(&dinner).unwrap();

    let source = source("A watch", 0);
    health.put_source(&source).unwrap();
    let mut run = workout("k-run", 0, 600);
    run.source_id = Some(source.id);
    put_workouts(health, &[&run]);
    let logged = dose(&medication, "", 5);
    put_doses(health, &[&logged]);

    health.delete_condition(condition.id).unwrap();
    assert_eq!(health.get_medication(medication.id).unwrap(), medication, "still naming it");
    health.delete_document(document.id).unwrap();
    assert_eq!(health.get_lab(result.id).unwrap(), result, "still pointing at it");
    health.delete_food(food.id).unwrap();
    assert_eq!(health.get_meal(dinner.id).unwrap(), dinner, "still made of it");
    health.delete_source(source.id).unwrap();
    assert_eq!(health.get_workout(run.id).unwrap(), run, "still from it");
    health.delete_workout(run.id).unwrap();
    health.delete_meal(dinner.id).unwrap();
    health.delete_lab(result.id).unwrap();
    assert_eq!(health.get_dose(logged.id).unwrap(), logged, "doses answer to their medication");
    health.delete_dose(logged.id).unwrap();
    assert_eq!(health.get_medication(medication.id).unwrap(), medication);

    health.delete_medication(medication.id).unwrap();
}

fn blob_refs_name_every_blob_a_health_record_holds(health: &dyn HealthStore) {
    let (sent, route) =
        (BlobId::of(b"the resource as it came"), BlobId::of(b"a route nobody keeps"));

    // The fixtures hold these blobs: what a connector sent for a document,
    // and a route. A document's pages and a meal's photograph are files in
    // the catalog, which keeps their blobs itself, so they are not here.
    let report = full_document(Some(date(2026, 10, 2)));
    assert_eq!(report.source.as_ref().map(|s| s.blob), Some(sent));
    let paperless = document("Nothing attached", None);
    let run = full_workout("k-blobs");
    let dinner = full_meal(10);
    let bare_run = workout("k-no-route", 1_000, 1_100);
    let bare_meal = meal("No photo", 20);
    health.put_document(&report).unwrap();
    health.put_document(&paperless).unwrap();
    put_workouts(health, &[&run, &bare_run]);
    health.put_meal(&dinner).unwrap();
    health.put_meal(&bare_meal).unwrap();

    // A connector's record and a workout's route; a record that holds none
    // contributes none. Duplicates are not the store's business, so what is
    // compared is the set.
    let refs = health.blob_refs().unwrap();
    assert_eq!(
        refs.iter().copied().collect::<BTreeSet<BlobId>>(),
        [sent, route].into_iter().collect::<BTreeSet<BlobId>>(),
        "{refs:?}"
    );

    // And a blob stops being referenced when what held it goes.
    health.delete_document(report.id).unwrap();
    assert_eq!(health.blob_refs().unwrap(), vec![route]);
    health.delete_workout(run.id).unwrap();
    assert!(health.blob_refs().unwrap().is_empty());

    health.delete_meal(dinner.id).unwrap();
    health.delete_document(paperless.id).unwrap();
    health.delete_workout(bare_run.id).unwrap();
    health.delete_meal(bare_meal.id).unwrap();
}

/// What `blob_refs` is for: garbage collection walks every record that can
/// hold a blob, and what a connector sent for a clinical document and a
/// workout's route are two of them. Missing this walk would make the first
/// sweep after a day's grace delete every record an Apple export brought,
/// with no error anywhere and each document still naming a record that is
/// not there.
fn garbage_collection_keeps_what_health_holds(store: &dyn JournalStore, health: &dyn HealthStore) {
    let sent = store.put_blob(b"{\"resourceType\": \"Observation\"}").unwrap();
    let route = store.put_blob(b"[a route of points]").unwrap();
    let orphan = store.put_blob(b"nobody's scan").unwrap();

    let mut report = document("Imported record", Some(date(2026, 10, 2)));
    report.source = Some(DocSource {
        blob: sent,
        name: "Observation.json".into(),
        mime: "application/fhir+json".into(),
        size: 32,
    });
    let mut run = workout("k-gc", 0, 600);
    run.route = Some(route);
    health.put_document(&report).unwrap();
    put_workouts(health, &[&run]);

    store.collect_garbage(std::time::Duration::ZERO).unwrap();
    assert!(store.has_blob(sent).unwrap(), "what a connector sent is a live reference");
    assert!(store.has_blob(route).unwrap(), "a workout's route is");
    assert!(!store.has_blob(orphan).unwrap(), "and what nothing holds is collected");

    // ...and each stops being one when what held it goes.
    health.delete_document(report.id).unwrap();
    health.delete_workout(run.id).unwrap();
    store.collect_garbage(std::time::Duration::ZERO).unwrap();
    assert!(!store.has_blob(sent).unwrap(), "a record nothing points at is collectable");
    assert!(!store.has_blob(route).unwrap());
}

fn unicode_survives_a_health_round_trip(health: &dyn HealthStore) {
    let mut source = source("Hari\u{2019}s \u{7b14}\u{8bb0}\u{672c} \u{231a}", 0);
    source.status.devices = vec!["\u{1f4f1} iPhone".into()];
    health.put_source(&source).unwrap();
    assert_eq!(health.get_source(source.id).unwrap(), source);

    let mut medication = medication("L-Thyroxin \u{2014} 50 \u{b5}g", 0, 0);
    medication.notes = "n\u{fc}chtern einnehmen \u{1f48a}".into();
    medication.prescriber = "Dr. M\u{fc}ller-Schmidt".into();
    health.put_medication(&medication).unwrap();
    assert_eq!(health.get_medication(medication.id).unwrap(), medication);

    let mut result = lab("H\u{e4}moglobin A1c", date(2026, 10, 2));
    result.unit = "mmol/mol \u{2265} 48".into();
    result.note = "\u{65e5}\u{672c}\u{8a9e}\u{306e}\u{30e1}\u{30e2}".into();
    health.put_labs(std::slice::from_ref(&result)).unwrap();
    assert_eq!(health.get_lab(result.id).unwrap(), result);

    let mut paper = document("Befund \u{2014} Schilddr\u{fc}se \u{1f4c4}", None);
    paper.text = "TSH 2,4 mU/l \u{2014} \u{6b63}\u{5e38}".into();
    health.put_document(&paper).unwrap();
    assert_eq!(health.get_document(paper.id).unwrap(), paper);

    let mut dish = food("Cr\u{e8}me br\u{fb}l\u{e9}e \u{1f36e}", None);
    dish.brand = "Maison \u{e9}t\u{e9}".into();
    health.put_food(&dish).unwrap();
    assert_eq!(health.get_food(dish.id).unwrap(), dish);

    let supper = meal("\u{591c}\u{3054}\u{98ef} \u{1f35c}", 10);
    health.put_meal(&supper).unwrap();
    assert_eq!(health.get_meal(supper.id).unwrap(), supper);

    let allergic = allergy("Erdn\u{fc}sse \u{1f95c}", 0);
    health.put_allergy(&allergic).unwrap();
    assert_eq!(health.get_allergy(allergic.id).unwrap(), allergic);

    let ill = condition("Hashimoto-Thyreoiditis \u{1f98b}", 0);
    health.put_condition(&ill).unwrap();
    assert_eq!(health.get_condition(ill.id).unwrap(), ill);

    let run = workout("k-\u{6d41}", 0, 60);
    put_workouts(health, &[&run]);
    assert_eq!(health.workout_by_key("k-\u{6d41}").unwrap(), Some(run.clone()));

    let mut day = row("\u{6d41}", d(1), 61.0);
    day.data.sources = vec!["Hari\u{2019}s \u{231a}".into()];
    put_rows(health, std::slice::from_ref(&day));
    assert_eq!(health.series_days("\u{6d41}", d(1), d(1)).unwrap(), vec![day.data.clone()]);

    let settings = HealthSettings {
        priority: vec!["Hari\u{2019}s \u{231a}".into(), "\u{1f4f1}".into()],
        ..HealthSettings::default()
    };
    health.put_health_settings(&settings).unwrap();
    assert_eq!(health.health_settings().unwrap(), settings);
}
