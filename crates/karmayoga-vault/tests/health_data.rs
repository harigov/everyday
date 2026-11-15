//! The rules the vault applies to the data half of Health, over a real
//! SQLite backend: samples filed once however often, and from whomever,
//! they arrive; units made canonical on the way in and the impossible
//! refused; workouts upserted by what they are without undoing what a person
//! called them; paused sources; buckets; readings typed in; deletion; the
//! food-lookup key; and health metrics as a tracker's source.
//!
//! Samples are filed on fixed days in October 2026 and read back over the
//! same days in UTC. The tracker tests are the exception: a tracker counts
//! days where this computer is, so they record a few hours before the real
//! clock.

mod support;
use support::vault;

use jiff::civil::{Date, date};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use karmayoga_core::health::{
    Activity, BucketSize, Connector, HealthBatch, HealthIngestReport, HealthKeys, HealthSettings,
    HealthSource, ManualReading, Metric, PushHeaders, RawSample, RawWorkout, RoutePoint,
    SamplePoint, SeriesQuery, SeriesResult, SourceStatus, Stat, Workout,
};
use karmayoga_core::{
    Error, HealthSourceId, ReadingQuery, Tracker, TrackerKind, TrackerSource, Vault, WorkoutId,
};

fn day(d: i8) -> Date {
    Date::constant(2026, 10, d)
}

/// `hour` o'clock UTC on October `d`, 2026.
fn at(d: i8, hour: i8) -> Timestamp {
    TimeZone::UTC.to_timestamp(date(2026, 10, d).at(hour, 0, 0, 0)).unwrap()
}

fn plus(at: Timestamp, secs: i64) -> Timestamp {
    at + SignedDuration::from_secs(secs)
}

/// An instant reading (a heart rate, a weight, a glucose).
fn sample(metric: Metric, at: Timestamp, value: f64, source: &str) -> RawSample {
    RawSample {
        metric,
        start: at,
        end: at,
        value,
        unit: String::new(),
        source: source.into(),
        offset_min: 0,
    }
}

/// A reading over a stretch of time (steps, energy).
fn lasting(metric: Metric, start: Timestamp, secs: i64, value: f64, source: &str) -> RawSample {
    RawSample { end: plus(start, secs), ..sample(metric, start, value, source) }
}

fn a_source(vault: &Vault, connector: Connector, name: &str) -> HealthSource {
    let now = Timestamp::now();
    let source = HealthSource {
        id: HealthSourceId::new(),
        connector,
        name: name.into(),
        paused: false,
        status: SourceStatus::default(),
        token_id: String::new(),
        created_at: now,
        updated_at: now,
    };
    vault.save_health_source(&source).unwrap()
}

fn phone(vault: &Vault) -> HealthSource {
    a_source(vault, Connector::HealthAutoExport, "Phone")
}

fn ingest(vault: &Vault, source: &HealthSource, samples: Vec<RawSample>) -> HealthIngestReport {
    vault.ingest_health(source.id, HealthBatch { samples, ..Default::default() }, None).unwrap()
}

fn values(points: &[SamplePoint]) -> Vec<f64> {
    points.iter().map(|p| p.value).collect()
}

fn heart_rate(vault: &Vault, from_day: i8, to_day: i8) -> Vec<f64> {
    values(&vault.health_samples(Metric::HeartRate, at(from_day, 0), at(to_day, 0)).unwrap())
}

// ---- samples ------------------------------------------------------------------

#[test]
fn a_batch_ingested_twice_is_stored_once() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let phone = phone(&vault);
    let batch = || {
        vec![
            sample(Metric::HeartRate, at(5, 8), 61.0, "Apple Watch"),
            sample(Metric::HeartRate, at(5, 9), 72.0, "Apple Watch"),
            sample(Metric::HeartRate, at(6, 9), 70.0, "Apple Watch"),
        ]
    };

    let once = ingest(&vault, &phone, batch());
    assert_eq!((once.samples_added, once.samples_duplicate, once.samples_rejected), (3, 0, 0));
    assert_eq!((once.first, once.last), (Some(at(5, 8)), Some(at(6, 9))));

    let twice = ingest(&vault, &phone, batch());
    assert_eq!((twice.samples_added, twice.samples_duplicate), (0, 3));
    assert_eq!(heart_rate(&vault, 5, 7), [61.0, 72.0, 70.0]);

    // The source says what it has brought: only what was new counts.
    let status = vault.health_source(phone.id).unwrap().status;
    assert_eq!(status.samples_total, 3);
    assert_eq!(status.devices, ["Apple Watch"]);
    assert!(status.last_received_at.is_some());
    assert_eq!(status.newest_sample_at, Some(at(6, 9)));
}

#[test]
fn the_same_reading_from_two_sources_is_stored_once() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let watch = a_source(&vault, Connector::AppleExport, "Export");
    let phone = phone(&vault);

    ingest(&vault, &watch, vec![sample(Metric::HeartRate, at(5, 8), 61.0, "Apple Watch")]);
    let again = ingest(&vault, &phone, vec![sample(Metric::HeartRate, at(5, 8), 61.0, "iPhone")]);

    assert_eq!((again.samples_added, again.samples_duplicate), (0, 1));
    assert_eq!(heart_rate(&vault, 5, 6), [61.0]);
}

#[test]
fn a_weight_in_pounds_is_stored_in_kilograms() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let phone = phone(&vault);
    let mut pounds = sample(Metric::Weight, at(5, 7), 180.0, "Scale");
    pounds.unit = "lb".into();

    let report = ingest(&vault, &phone, vec![pounds]);

    assert_eq!(report.samples_added, 1);
    let held = vault.health_samples(Metric::Weight, at(5, 0), at(6, 0)).unwrap();
    assert_eq!(held.len(), 1);
    assert!((held[0].value - 81.6466).abs() < 0.01, "{}", held[0].value);
    assert_eq!(held[0].source, "Scale");
}

#[test]
fn an_implausible_sample_is_refused_and_counted() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let phone = phone(&vault);
    let backwards =
        RawSample { end: at(5, 7), ..sample(Metric::HeartRate, at(5, 8), 60.0, "Watch") };
    let too_long = lasting(Metric::Steps, at(5, 8), 40 * 3600, 100.0, "Watch");

    let report = ingest(
        &vault,
        &phone,
        vec![
            sample(Metric::Weight, at(5, 7), 70.0, "Scale"),
            // A unit mix-up: no one weighs five tonnes.
            sample(Metric::Weight, at(5, 8), 5000.0, "Scale"),
            sample(Metric::HeartRate, at(5, 9), f64::NAN, "Watch"),
            backwards,
            too_long,
        ],
    );

    assert_eq!((report.samples_added, report.samples_rejected), (1, 4));
    let weights = vault.health_samples(Metric::Weight, at(5, 0), at(6, 0)).unwrap();
    assert_eq!(values(&weights), [70.0]);
    assert!(heart_rate(&vault, 5, 6).is_empty());
}

#[test]
fn what_a_batch_could_not_place_is_reported_and_a_nameless_sample_takes_its_connections() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let phone = phone(&vault);
    let mut batch = HealthBatch {
        samples: vec![sample(Metric::HeartRate, at(5, 8), 61.0, "  ")],
        rejected: 2,
        ..Default::default()
    };
    batch.skip("HKQuantityTypeIdentifierMadeUp");
    batch.skip("HKQuantityTypeIdentifierMadeUp");

    let report = vault.ingest_health(phone.id, batch, None).unwrap();

    assert_eq!(report.skipped["HKQuantityTypeIdentifierMadeUp"], 2);
    assert_eq!((report.samples_added, report.samples_rejected), (1, 2));
    let held = vault.health_samples(Metric::HeartRate, at(5, 0), at(6, 0)).unwrap();
    assert_eq!(held[0].source, "Phone");
}

// ---- sources ------------------------------------------------------------------

#[test]
fn a_paused_source_is_refused_and_files_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let phone = phone(&vault);
    let paused = vault.save_health_source(&HealthSource { paused: true, ..phone.clone() }).unwrap();
    let batch = || HealthBatch {
        samples: vec![sample(Metric::HeartRate, at(5, 8), 61.0, "Watch")],
        ..Default::default()
    };

    let refused = vault.ingest_health(paused.id, batch(), None).unwrap_err();
    assert!(matches!(&refused, Error::Invalid(why) if why == "paused"), "{refused}");
    assert!(heart_rate(&vault, 5, 6).is_empty());
    assert_eq!(vault.health_source(paused.id).unwrap().status.last_received_at, None);

    vault.save_health_source(&HealthSource { paused: false, ..paused }).unwrap();
    assert_eq!(vault.ingest_health(phone.id, batch(), None).unwrap().samples_added, 1);
}

#[test]
fn a_push_to_a_source_that_is_not_there_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let unknown = HealthSourceId::new();
    let err = vault.ingest_health(unknown, HealthBatch::default(), None).unwrap_err();
    assert_eq!(err.code(), "not_found");
}

#[test]
fn a_source_keeps_the_status_arrival_gave_it() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let phone = phone(&vault);
    let headers = PushHeaders {
        automation_id: "auto-1".into(),
        automation_name: "Hourly".into(),
        automation_period: "Since Last Sync".into(),
        automation_aggregation: "false".into(),
        session_id: String::new(),
    };
    let batch = HealthBatch {
        samples: vec![sample(Metric::HeartRate, at(5, 8), 61.0, "Watch")],
        ..Default::default()
    };
    vault.ingest_health(phone.id, batch, Some(&headers)).unwrap();
    let stored = vault.health_source(phone.id).unwrap();
    assert_eq!(stored.status.automations.len(), 1);
    assert_eq!(stored.status.automations[0].id, "auto-1");
    assert_eq!(stored.status.automations[0].name, "Hourly");
    assert_eq!(stored.status.automations[0].last_count, 1);

    // A person renames it; whatever status they send is not taken.
    let tampered = HealthSource {
        name: "  Renamed  ".into(),
        status: SourceStatus { samples_total: 999, ..Default::default() },
        ..stored.clone()
    };
    let saved = vault.save_health_source(&tampered).unwrap();
    assert_eq!(saved.name, "Renamed");
    assert_eq!(saved.status, stored.status);
    assert_eq!(vault.health_sources().unwrap().len(), 1);
    let blank = HealthSource { name: "   ".into(), ..stored.clone() };
    assert_eq!(vault.save_health_source(&blank).unwrap_err().code(), "invalid");

    // The registry token minted for the phone is the vault's to record, and
    // the caller of a save cannot change it.
    vault.set_health_source_token(phone.id, "token-1").unwrap();
    let meddled = HealthSource { token_id: "another".into(), ..stored.clone() };
    assert_eq!(vault.save_health_source(&meddled).unwrap().token_id, "token-1");
    assert_eq!(vault.health_source(phone.id).unwrap().token_id, "token-1");
    vault.set_health_source_token(phone.id, "").unwrap();
    assert_eq!(vault.health_source(phone.id).unwrap().token_id, "");
    let unknown = HealthSourceId::new();
    assert_eq!(vault.set_health_source_token(unknown, "x").unwrap_err().code(), "not_found");

    // A push that could not be read is noted, but is not hearing from it.
    vault.record_health_push_failure(phone.id, "  the body was not JSON ").unwrap();
    let failed = vault.health_source(phone.id).unwrap().status;
    assert_eq!(failed.last_error, "the body was not JSON");
    assert_eq!(failed.last_received_at, stored.status.last_received_at);

    // The next good one clears it.
    ingest(&vault, &phone, vec![sample(Metric::HeartRate, at(5, 9), 62.0, "Watch")]);
    assert_eq!(vault.health_source(phone.id).unwrap().status.last_error, "");

    vault.delete_health_source(phone.id).unwrap();
    assert!(vault.health_sources().unwrap().is_empty());
    assert_eq!(heart_rate(&vault, 5, 6), [61.0, 62.0], "what it brought stays");
}

// ---- workouts -----------------------------------------------------------------

fn run(name: &str, kcal: f64) -> RawWorkout {
    RawWorkout {
        activity: Activity::Running,
        name: name.into(),
        start: at(5, 7),
        end: at(5, 8),
        offset_min: 0,
        active_energy_kcal: Some(kcal),
        total_energy_kcal: None,
        distance_m: Some(5000.0),
        elevation_up_m: None,
        avg_hr: Some(150.0),
        max_hr: Some(175.0),
        effort: None,
        indoor: Some(false),
        source: "Apple Watch".into(),
        external_id: "w-1".into(),
        route: Vec::new(),
        heart_rate: Vec::new(),
    }
}

fn workouts(vault: &Vault, source: &HealthSource, list: Vec<RawWorkout>) -> HealthIngestReport {
    let batch = HealthBatch { workouts: list, ..Default::default() };
    vault.ingest_health(source.id, batch, None).unwrap()
}

fn listed(vault: &Vault) -> Vec<Workout> {
    vault.list_workouts(at(5, 0), at(7, 0)).unwrap()
}

#[test]
fn a_workout_is_upserted_by_what_it_is_and_keeps_the_name_a_person_gave_it() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let phone = phone(&vault);
    let point = |lat: f64, lon: f64| RoutePoint { at: at(5, 7), lat, lon, alt: None };
    let mut first = run("Outdoor Run", 400.0);
    first.heart_rate = vec![sample(Metric::HeartRate, plus(at(5, 7), 1800), 150.0, "Apple Watch")];
    first.route = vec![point(51.5, -0.12), point(51.6, -0.13), point(200.0, 0.0)];
    let route_again = first.route.clone();

    let report = workouts(&vault, &phone, vec![first]);
    assert_eq!((report.workouts_added, report.workouts_updated), (1, 0));
    assert_eq!(report.samples_added, 0, "the heart rate a workout carries is not filed");
    assert!(heart_rate(&vault, 5, 6).is_empty());
    // Its detail reads the raw samples over its span, whoever sent them.
    let inside = sample(Metric::HeartRate, plus(at(5, 7), 1200), 148.0, "Apple Watch");
    ingest(&vault, &phone, vec![inside]);
    let original = listed(&vault).remove(0);
    assert_eq!(original.activity, Activity::Running);
    assert_eq!((original.source.as_str(), original.source_id), ("Apple Watch", Some(phone.id)));
    assert_eq!(original.key.len(), 32);
    let detail = vault.workout_detail(original.id).unwrap();
    assert_eq!(values(&detail.heart_rate), [148.0]);
    assert_eq!(detail.route.len(), 2, "a point off the globe is dropped");

    // A person names it, and tries to change what the watch measured.
    let mine = Workout {
        name: "Tuesday tempo".into(),
        note: " felt strong ".into(),
        distance_m: Some(1.0),
        start: plus(original.start, 60),
        ..original.clone()
    };
    let saved = vault.save_workout(&mine).unwrap();
    assert_eq!((saved.name.as_str(), saved.note.as_str()), ("Tuesday tempo", "felt strong"));
    assert_eq!((saved.distance_m, saved.start), (Some(5000.0), original.start));

    // The watch revises its energy, and sends no route this time.
    let again = workouts(&vault, &phone, vec![run("Outdoor Run", 420.0)]);
    assert_eq!((again.workouts_added, again.workouts_updated), (0, 1));
    let after = listed(&vault);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, original.id);
    assert_eq!(after[0].name, "Tuesday tempo");
    assert_eq!(after[0].note, "felt strong");
    assert_eq!(after[0].active_energy_kcal, Some(420.0));
    assert_eq!(after[0].route, original.route, "a push without a route does not unmake it");

    // The same again changes nothing, and neither does a route that has
    // changed: a workout's route is sealed once.
    let third = workouts(&vault, &phone, vec![run("Outdoor Run", 420.0)]);
    assert_eq!((third.workouts_added, third.workouts_updated), (0, 0));
    let rerouted = RawWorkout { route: route_again[..1].to_vec(), ..run("Outdoor Run", 420.0) };
    let fourth = workouts(&vault, &phone, vec![rerouted]);
    assert_eq!((fourth.workouts_added, fourth.workouts_updated), (0, 0));
    assert_eq!(listed(&vault)[0].route, original.route);
}

#[test]
fn a_workout_deleted_here_is_not_brought_back_by_its_source() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let phone = phone(&vault);
    workouts(&vault, &phone, vec![run("Outdoor Run", 400.0)]);
    let held = listed(&vault).remove(0);

    vault.delete_workout(held.id).unwrap();
    assert!(listed(&vault).is_empty());
    assert!(vault.health_day_timeline(day(5), "UTC").unwrap().workouts.is_empty());
    assert!(vault.health_today(day(5), "UTC").unwrap().workouts.is_empty());
    let activity = vault.health_activity(day(5), day(5), "UTC").unwrap();
    assert!(activity.iter().all(|d| d.workouts == 0), "{activity:?}");

    // The next push of the week brings it again: it stays deleted, and the
    // row is not rewritten.
    let again = workouts(&vault, &phone, vec![run("Outdoor Run", 450.0)]);
    assert_eq!((again.workouts_added, again.workouts_updated), (0, 0));
    assert!(listed(&vault).is_empty());
}

#[test]
fn a_workout_sent_twice_in_one_batch_is_one_workout() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let phone = phone(&vault);

    let twice = vec![run("Outdoor Run", 400.0), run("Outdoor Run", 410.0)];
    let report = workouts(&vault, &phone, twice);

    assert_eq!(report.workouts_added, 1);
    let held = listed(&vault);
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].active_energy_kcal, Some(410.0), "the later word");
}

#[test]
fn a_workout_that_ends_before_it_starts_is_counted_and_not_kept() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let phone = phone(&vault);
    let backwards = RawWorkout { start: at(5, 8), end: at(5, 7), ..run("Backwards", 1.0) };

    let report = workouts(&vault, &phone, vec![backwards]);

    assert_eq!(report.workouts_added, 0);
    assert_eq!(report.skipped.values().sum::<u64>(), 1);
    assert!(listed(&vault).is_empty());
}

fn typed(activity: Activity, start: Timestamp, end: Timestamp) -> Workout {
    let now = Timestamp::now();
    Workout {
        id: WorkoutId::new(),
        activity,
        name: "Evening stretch".into(),
        start,
        end,
        offset_min: 0,
        active_energy_kcal: Some(80.0),
        total_energy_kcal: None,
        distance_m: None,
        elevation_up_m: None,
        avg_hr: None,
        max_hr: None,
        effort: None,
        indoor: Some(true),
        source: String::new(),
        source_id: None,
        external_id: String::new(),
        route: None,
        note: String::new(),
        key: String::new(),
        hidden: false,
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn a_workout_typed_in_can_be_edited_but_not_duplicated() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let yoga = typed(Activity::Yoga, at(6, 18), at(6, 19));

    let saved = vault.save_workout(&yoga).unwrap();
    assert_eq!(saved.source, "Karma Yoga");
    assert_eq!(saved.key.len(), 32);

    let twin = Workout { id: WorkoutId::new(), ..yoga.clone() };
    assert_eq!(vault.save_workout(&twin).unwrap_err().code(), "invalid");

    // Unlike one a watch brought, its numbers are its own to change.
    let longer = Workout { end: at(6, 20), active_energy_kcal: Some(160.0), ..saved.clone() };
    let after = vault.save_workout(&longer).unwrap();
    assert_eq!((after.id, after.end, after.active_energy_kcal), (saved.id, at(6, 20), Some(160.0)));
    assert_eq!(vault.list_workouts(at(6, 0), at(7, 0)).unwrap().len(), 1);

    let negative = Workout { distance_m: Some(-1.0), ..saved.clone() };
    assert_eq!(vault.save_workout(&negative).unwrap_err().code(), "invalid");
    let backwards = Workout { end: at(6, 17), ..saved.clone() };
    assert_eq!(vault.save_workout(&backwards).unwrap_err().code(), "invalid");

    // One typed in goes for real, and its times are free again.
    vault.delete_workout(saved.id).unwrap();
    assert!(vault.list_workouts(at(6, 0), at(7, 0)).unwrap().is_empty());
    let retyped = typed(Activity::Yoga, at(6, 18), at(6, 20));
    assert_eq!(vault.save_workout(&retyped).unwrap().end, at(6, 20));
}

// ---- series -------------------------------------------------------------------

#[test]
fn a_day_and_a_week_add_up_the_same_steps() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let phone = phone(&vault);
    let steps: Vec<RawSample> = (1..=14)
        .map(|d| {
            lasting(Metric::Steps, at(d, 12), 600, 1000.0 + f64::from(d) * 10.0, "Apple Watch")
        })
        .collect();
    let expected: Vec<f64> = (1..=14).map(|d| 1000.0 + f64::from(d) * 10.0).collect();
    ingest(&vault, &phone, steps);
    let query = |bucket, stat| SeriesQuery {
        metric: Metric::Steps,
        from: day(1),
        to: day(14),
        tz: "UTC".into(),
        bucket,
        stat,
    };

    let by_day = vault.health_series(&query(BucketSize::Day, Stat::Sum)).unwrap();
    assert_eq!(by_day.unit, "count");
    assert_eq!(by_day.stat, Stat::Sum);
    assert_eq!(values_of(&by_day), expected);
    let latest = by_day.latest.as_ref().unwrap();
    assert_eq!((latest.value, latest.source.as_str()), (1140.0, "Apple Watch"));

    let by_week = vault.health_series(&query(BucketSize::Week, Stat::Sum)).unwrap();
    assert!(!by_week.points.is_empty() && by_week.points.len() < 14, "{:?}", by_week.points);
    assert_eq!(values_of(&by_week).iter().sum::<f64>(), expected.iter().sum::<f64>());

    // Left to the metric, steps are a sum.
    let auto = vault.health_series(&query(BucketSize::Day, Stat::Auto)).unwrap();
    assert_eq!((auto.stat, values_of(&auto)), (Stat::Sum, expected.clone()));

    let daily = vault.health_daily(Metric::Steps, Stat::Sum, day(1), day(14), "UTC").unwrap();
    assert_eq!(daily.len(), 14);
    assert_eq!(daily[&day(3)], 1030.0);
}

fn values_of(result: &SeriesResult) -> Vec<f64> {
    result.points.iter().map(|p| p.value).collect()
}

#[test]
fn raw_samples_are_for_a_few_days_and_ranges_are_capped() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let phone = phone(&vault);
    ingest(
        &vault,
        &phone,
        vec![
            sample(Metric::HeartRate, at(5, 10), 64.0, "Watch"),
            sample(Metric::HeartRate, at(5, 8), 60.0, "Watch"),
            sample(Metric::HeartRate, at(5, 9), 62.0, "Watch"),
        ],
    );
    let query = |from, to, bucket| SeriesQuery {
        metric: Metric::HeartRate,
        from,
        to,
        tz: "UTC".into(),
        bucket,
        stat: Stat::Auto,
    };

    let one_day = vault.health_series(&query(day(5), day(5), BucketSize::Raw)).unwrap();
    assert_eq!(values_of(&one_day), [60.0, 62.0, 64.0], "in time order");
    let at_hours: Vec<Timestamp> = one_day.points.iter().map(|p| p.at).collect();
    assert_eq!(at_hours, [at(5, 8), at(5, 9), at(5, 10)]);

    let too_wide = vault.health_series(&query(day(1), day(14), BucketSize::Raw)).unwrap_err();
    assert_eq!(too_wide.code(), "invalid");
    let years = query(Date::constant(2020, 1, 1), Date::constant(2026, 1, 1), BucketSize::Month);
    assert_eq!(vault.health_series(&years).unwrap_err().code(), "invalid");
    let backwards = query(day(6), day(5), BucketSize::Day);
    assert_eq!(vault.health_series(&backwards).unwrap_err().code(), "invalid");
    let zoned = SeriesQuery { tz: "Not/AZone".into(), ..query(day(5), day(5), BucketSize::Day) };
    assert_eq!(vault.health_series(&zoned).unwrap_err().code(), "invalid");

    // A metric with nothing stored answers an empty series, not an error.
    let empty = SeriesQuery { metric: Metric::Weight, ..query(day(5), day(5), BucketSize::Day) };
    let nothing = vault.health_series(&empty).unwrap();
    assert!(nothing.points.is_empty() && nothing.latest.is_none());
}

#[test]
fn extents_name_each_metric_with_anything_stored_in_catalogue_order() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let phone = phone(&vault);
    assert!(vault.health_extents().unwrap().is_empty());

    ingest(
        &vault,
        &phone,
        vec![
            sample(Metric::Weight, at(5, 7), 70.0, "Scale"),
            sample(Metric::HeartRate, at(5, 8), 60.0, "Watch"),
            sample(Metric::HeartRate, at(5, 9), 62.0, "Watch"),
            sample(Metric::HeartRate, at(6, 9), 66.0, "Watch"),
        ],
    );

    assert_eq!(
        vault.health_extents().unwrap(),
        [(Metric::HeartRate, day(5), day(6), 2), (Metric::Weight, day(5), day(5), 1)]
    );
}

// ---- typed in -------------------------------------------------------------------

#[test]
fn a_blood_pressure_typed_in_makes_two_samples() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    vault
        .add_health_reading(&ManualReading {
            metric: Metric::BloodPressureSystolic,
            at: at(5, 9),
            value: 128.0,
            unit: String::new(),
            second: Some(82.0),
            tz: "UTC".into(),
        })
        .unwrap();

    let read = |metric| vault.health_samples(metric, at(5, 0), at(6, 0)).unwrap();
    let (systolic, diastolic) =
        (read(Metric::BloodPressureSystolic), read(Metric::BloodPressureDiastolic));
    assert_eq!((values(&systolic), values(&diastolic)), ([128.0].to_vec(), [82.0].to_vec()));
    assert_eq!((systolic[0].start, systolic[0].source.as_str()), (at(5, 9), "Karma Yoga"));
    assert_eq!(diastolic[0].start, at(5, 9));

    // Typing the same one twice is the same reading.
    let again = ManualReading {
        metric: Metric::BloodPressureSystolic,
        at: at(5, 9),
        value: 128.0,
        unit: String::new(),
        second: Some(82.0),
        tz: "UTC".into(),
    };
    vault.add_health_reading(&again).unwrap();
    assert_eq!(read(Metric::BloodPressureSystolic).len(), 1);
}

#[test]
fn a_reading_no_one_could_have_is_refused_when_typed_in() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let weight = |value: f64, unit: &str| ManualReading {
        metric: Metric::Weight,
        at: at(5, 7),
        value,
        unit: unit.into(),
        second: None,
        tz: "UTC".into(),
    };

    assert_eq!(vault.add_health_reading(&weight(9000.0, "")).unwrap_err().code(), "invalid");
    assert_eq!(vault.add_health_reading(&weight(70.0, "furlongs")).unwrap_err().code(), "invalid");
    let two = ManualReading { second: Some(80.0), ..weight(70.0, "") };
    assert_eq!(vault.add_health_reading(&two).unwrap_err().code(), "invalid");
    let zoned = ManualReading { tz: "Not/AZone".into(), ..weight(70.0, "") };
    assert_eq!(vault.add_health_reading(&zoned).unwrap_err().code(), "invalid");
    assert!(vault.health_samples(Metric::Weight, at(5, 0), at(6, 0)).unwrap().is_empty());

    vault.add_health_reading(&weight(154.0, "lb")).unwrap();
    let held = vault.health_samples(Metric::Weight, at(5, 0), at(6, 0)).unwrap();
    assert!((held[0].value - 69.85).abs() < 0.01, "{}", held[0].value);
}

#[test]
fn a_reading_typed_in_belongs_to_the_day_where_it_was_taken() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    // 23:30 on the 5th in Los Angeles (seven hours behind UTC in October) is
    // already the 6th in UTC; a person typing it in means the 5th.
    let late = plus(at(6, 6), 1800);
    vault
        .add_health_reading(&ManualReading {
            metric: Metric::Weight,
            at: late,
            value: 70.0,
            unit: String::new(),
            second: None,
            tz: "America/Los_Angeles".into(),
        })
        .unwrap();

    let daily = vault
        .health_daily(Metric::Weight, Stat::Last, day(5), day(6), "America/Los_Angeles")
        .unwrap();
    assert_eq!(daily.keys().copied().collect::<Vec<_>>(), [day(5)]);
}

// ---- deleting ---------------------------------------------------------------------

#[test]
fn samples_are_deleted_by_range_and_source_and_the_summary_follows() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let phone = phone(&vault);
    ingest(
        &vault,
        &phone,
        vec![
            sample(Metric::HeartRate, at(5, 8), 60.0, "Apple Watch"),
            sample(Metric::HeartRate, at(5, 9), 62.0, "iPhone"),
            sample(Metric::HeartRate, at(5, 10), 64.0, "Apple Watch"),
            sample(Metric::HeartRate, at(6, 9), 66.0, "Apple Watch"),
        ],
    );
    let highest = |d: i8| {
        let daily = vault.health_daily(Metric::HeartRate, Stat::Max, day(5), day(6), "UTC");
        daily.unwrap()[&day(d)]
    };
    assert_eq!(highest(5), 64.0);

    // Only the watch's, and only in the range: the 10:00 reading.
    let gone = vault
        .delete_health_samples(Metric::HeartRate, at(5, 9), at(5, 11), Some("Apple Watch"))
        .unwrap();
    assert_eq!(gone, 1);
    assert_eq!(heart_rate(&vault, 5, 7), [60.0, 62.0, 66.0]);
    assert_eq!(highest(5), 62.0, "the day's summary was rebuilt");

    // Anyone's, over both days.
    let gone = vault.delete_health_samples(Metric::HeartRate, at(5, 0), at(7, 0), None).unwrap();
    assert_eq!(gone, 3);
    assert!(heart_rate(&vault, 5, 7).is_empty());

    // Nothing there is not an error; a backwards range is.
    let none = vault.delete_health_samples(Metric::HeartRate, at(5, 0), at(7, 0), None).unwrap();
    assert_eq!(none, 0);
    let backwards = vault.delete_health_samples(Metric::HeartRate, at(7, 0), at(5, 0), None);
    assert_eq!(backwards.unwrap_err().code(), "invalid");
}

#[test]
fn a_deleted_reading_is_not_brought_back_by_a_push_that_overlaps() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let phone = phone(&vault);
    let week = || {
        vec![
            sample(Metric::HeartRate, at(5, 8), 60.0, "Apple Watch"),
            sample(Metric::HeartRate, at(5, 9), 62.0, "Apple Watch"),
        ]
    };
    ingest(&vault, &phone, week());

    let gone = vault.delete_health_samples(Metric::HeartRate, at(5, 0), at(6, 0), None).unwrap();
    assert_eq!(gone, 2);

    // "Previous 7 Days" sends them again.
    let again = ingest(&vault, &phone, week());
    assert_eq!(again.samples_added, 0);
    assert!(heart_rate(&vault, 5, 6).is_empty());

    // A reading that was never deleted still arrives.
    let later = sample(Metric::HeartRate, at(5, 10), 64.0, "Apple Watch");
    let fresh = ingest(&vault, &phone, vec![later]);
    assert_eq!(fresh.samples_added, 1);
    assert_eq!(heart_rate(&vault, 5, 6), [64.0]);
    let latest = vault.health_latest(&[Metric::HeartRate], day(5)).unwrap();
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].value, 64.0);
}

#[test]
fn a_reading_typed_in_and_deleted_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let typed_in = ManualReading {
        metric: Metric::Weight,
        at: at(5, 7),
        value: 70.0,
        unit: String::new(),
        second: None,
        tz: "UTC".into(),
    };
    vault.add_health_reading(&typed_in).unwrap();
    assert_eq!(vault.health_extents().unwrap().len(), 1);

    let gone = vault.delete_health_samples(Metric::Weight, at(5, 0), at(6, 0), None).unwrap();
    assert_eq!(gone, 1);
    assert!(vault.health_extents().unwrap().is_empty(), "no device brought it, so no tombstone");

    // So typing it again is a new reading.
    vault.add_health_reading(&typed_in).unwrap();
    assert_eq!(vault.health_samples(Metric::Weight, at(5, 0), at(6, 0)).unwrap().len(), 1);
}

// ---- settings and keys -------------------------------------------------------------

#[test]
fn settings_are_checked_before_they_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mut stone = HealthSettings::default();
    stone.units.weight = "stone".into();
    assert_eq!(vault.save_health_settings(&stone).unwrap_err().code(), "invalid");
    let mut idle = HealthSettings::default();
    idle.goals.steps = 0.0;
    assert_eq!(vault.save_health_settings(&idle).unwrap_err().code(), "invalid");
    let long_wait = HealthSettings { stale_hours: 721, ..Default::default() };
    assert_eq!(vault.save_health_settings(&long_wait).unwrap_err().code(), "invalid");
    let crowded = HealthSettings {
        priority: (0..51).map(|i| format!("device {i}")).collect(),
        ..Default::default()
    };
    assert_eq!(vault.save_health_settings(&crowded).unwrap_err().code(), "invalid");
    let wordy = HealthSettings { priority: vec!["x".repeat(201)], ..Default::default() };
    assert_eq!(vault.save_health_settings(&wordy).unwrap_err().code(), "invalid");

    let mut kept = HealthSettings {
        priority: vec!["  Apple Watch ".into(), "".into(), "iPhone".into(), "Apple Watch".into()],
        stale_hours: 12,
        ..Default::default()
    };
    kept.units.glucose = "mmol/L".into();
    vault.save_health_settings(&kept).unwrap();

    let stored = vault.health_settings().unwrap();
    assert_eq!(stored.units.glucose, "mmol/L");
    assert_eq!(stored.priority, ["Apple Watch", "iPhone"]);
    assert_eq!(stored.stale_hours, 12);
}

#[test]
fn the_food_lookup_key_is_sealed_and_only_its_tail_is_shown() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    assert!(!vault.health_keys_info().unwrap().fdc_key_set);
    assert_eq!(vault.health_keys_access().unwrap().fdc_key, "", "none set is DEMO_KEY's cue");

    let key = "abcdefghijklmnopqrstuvwxyz0123456789ABCD";
    vault.save_health_keys(&HealthKeys { fdc_key: format!("  {key} ") }).unwrap();
    let info = vault.health_keys_info().unwrap();
    assert!(info.fdc_key_set);
    assert_eq!(info.fdc_key_hint, "ABCD");
    assert_eq!(vault.health_keys_access().unwrap().fdc_key, key);

    let spaced = HealthKeys { fdc_key: "two words".into() };
    assert_eq!(vault.save_health_keys(&spaced).unwrap_err().code(), "invalid");
    assert_eq!(vault.health_keys_access().unwrap().fdc_key, key, "a refused key changes nothing");

    vault.clear_health_keys().unwrap();
    assert!(!vault.health_keys_info().unwrap().fdc_key_set);

    // A blank key is the same as none.
    vault.save_health_keys(&HealthKeys { fdc_key: key.into() }).unwrap();
    vault.save_health_keys(&HealthKeys::default()).unwrap();
    assert!(!vault.health_keys_info().unwrap().fdc_key_set);
}

// ---- the day -------------------------------------------------------------------------

#[test]
fn today_draws_from_what_the_vault_holds() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let phone = phone(&vault);
    let mut pounds = sample(Metric::Weight, at(5, 7), 180.0, "Scale");
    pounds.unit = "lb".into();
    let mut batch = vec![
        pounds,
        lasting(Metric::Steps, at(5, 12), 600, 5000.0, "Apple Watch"),
        sample(Metric::HeartRate, at(5, 9), 70.0, "Apple Watch"),
    ];
    batch.extend(
        (0..12).map(|i| sample(Metric::BloodGlucose, plus(at(5, 13), i * 300), 110.0, "Dexcom")),
    );
    ingest(&vault, &phone, batch);

    let today = vault.health_today(day(5), "UTC").unwrap();

    assert_eq!(today.date, day(5));
    assert_eq!(today.activity.steps, 5000.0);
    assert_eq!(today.goals, HealthSettings::default().goals);
    let weight = today.latest.iter().find(|l| l.metric == Metric::Weight).unwrap();
    assert!((weight.value - 81.6466).abs() < 0.01, "{}", weight.value);
    assert_eq!(weight.previous, None, "nothing a month earlier");
    assert!(today.latest.iter().any(|l| l.metric == Metric::BloodGlucose));
    assert!(
        today.latest.iter().all(|l| l.metric != Metric::HeartRate && l.metric != Metric::Steps),
        "only the metrics Today shows, and only those with something"
    );
    assert_eq!(today.glucose.as_ref().map(|g| g.readings), Some(12));
    assert!(today.sleep.is_none());
    assert!(today.stale_sources.is_empty());

    // A day with nothing is still a day.
    let quiet = vault.health_today(day(20), "UTC").unwrap();
    assert_eq!(quiet.activity.steps, 0.0);
    let weights = quiet.latest.iter().filter(|l| l.metric == Metric::Weight);
    assert_eq!(weights.count(), 1, "the latest is as of the day, not on it");
    assert!(quiet.glucose.is_none());
    assert!(quiet.workouts.is_empty());
}

#[test]
fn a_days_timeline_gathers_glucose_and_the_workouts_of_that_day() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let phone = phone(&vault);
    let samples: Vec<RawSample> = (0..6)
        .map(|i| sample(Metric::BloodGlucose, plus(at(5, 13), i * 300), 100.0 + i as f64, "Dexcom"))
        .chain([
            sample(Metric::InsulinBolus, at(5, 13), 4.0, "Pen"),
            sample(Metric::InsulinBasal, at(5, 22), 20.0, "Pen"),
            sample(Metric::BloodGlucose, at(6, 13), 140.0, "Dexcom"),
        ])
        .collect();
    ingest(&vault, &phone, samples);
    workouts(&vault, &phone, vec![run("Outdoor Run", 400.0)]);

    let timeline = vault.health_day_timeline(day(5), "UTC").unwrap();

    assert_eq!(timeline.date, day(5));
    assert_eq!(timeline.glucose.len(), 6, "the next day's reading is not in it");
    assert_eq!(values(&timeline.insulin), [4.0, 20.0], "basal and bolus, in time order");
    assert_eq!(timeline.workouts.len(), 1);
    assert!(timeline.meals.is_empty() && timeline.doses.is_empty());
}

// ---- trackers -------------------------------------------------------------------------

/// The top of the hour two hours ago: far enough back that a run of a few
/// minutes never reaches the future, and on the hour so that the minutes after
/// it cannot cross a local midnight.
fn base() -> Timestamp {
    let two_hours_ago = Timestamp::now().as_second() - 2 * 3600;
    Timestamp::from_second(two_hours_ago - two_hours_ago.rem_euclid(3600)).unwrap()
}

fn health_tracker(vault: &Vault, name: &str, metric: Metric, stat: Stat) -> Tracker {
    let mut tracker = Tracker::new(name, TrackerKind::Amount);
    tracker.source = TrackerSource::Health { metric, stat };
    vault.save_tracker(&tracker).unwrap();
    vault.tracker(tracker.id).unwrap()
}

fn tracker_values(vault: &Vault, tracker: &Tracker) -> Vec<f64> {
    let today = jiff::Zoned::now().date();
    let query = ReadingQuery {
        tracker_ids: vec![tracker.id],
        ..ReadingQuery::between(today.yesterday().unwrap().yesterday().unwrap(), today)
    };
    vault.tracker_days(&query).unwrap().iter().map(|d| d.sum).collect()
}

#[test]
fn health_metrics_are_a_trackers_source() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let phone = phone(&vault);
    let now = base();
    ingest(
        &vault,
        &phone,
        vec![
            lasting(Metric::Steps, now, 600, 3000.0, "Apple Watch"),
            lasting(Metric::Steps, plus(now, -86_400), 600, 2000.0, "Apple Watch"),
            sample(Metric::BloodGlucose, now, 100.0, "Dexcom"),
            sample(Metric::BloodGlucose, plus(now, 60), 120.0, "Dexcom"),
            sample(Metric::BloodGlucose, plus(now, 120), 90.0, "Dexcom"),
            sample(Metric::HeartRate, now, 60.0, "Apple Watch"),
            sample(Metric::HeartRate, plus(now, 60), 100.0, "Apple Watch"),
        ],
    );

    // Steps are summed per day, in the metric's own unit.
    let steps = health_tracker(&vault, "Steps", Metric::Steps, Stat::Sum);
    assert_eq!(steps.unit, "steps");
    assert_eq!(tracker_values(&vault, &steps), [2000.0, 3000.0]);

    // Under another stat, the same metric answers differently.
    let peak = health_tracker(&vault, "Peak heart rate", Metric::HeartRate, Stat::Max);
    assert_eq!(tracker_values(&vault, &peak), [100.0]);

    // A share in range is shown as a percentage.
    let share = Stat::InRange { low: 70, high: 180 };
    let in_range = health_tracker(&vault, "Time in range", Metric::BloodGlucose, share);
    assert_eq!(in_range.unit, "%");
    let held = tracker_values(&vault, &in_range);
    assert_eq!(held.len(), 1);
    assert!((held[0] - 100.0).abs() < 1e-6, "every reading was in range: {held:?}");

    // A metric with nothing stored has no days.
    let weight = health_tracker(&vault, "Weight", Metric::Weight, Stat::Auto);
    assert!(tracker_values(&vault, &weight).is_empty());
}
