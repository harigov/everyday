//! Apple Health's `export.zip`, read as a stream.
//!
//! The archive holds `apple_health_export/export.xml` (every sample,
//! workout and activity summary -- gigabytes after a few years of a watch),
//! `export_cda.xml` (the same again, ignored), `workout-routes/*.gpx`,
//! `electrocardiograms/*.csv` and `clinical-records/*.json` (FHIR
//! resources from providers connected to Apple Health). The XML is read
//! with a pull parser and handed over in batches of
//! [`IMPORT_BATCH`](super::super::IMPORT_BATCH) samples, so memory stays
//! flat however big the export is.
//!
//! # What is kept in memory
//!
//! Samples are handed over as each batch fills. Workouts are not: a
//! workout's route is a separate file, found only once `export.xml` has been
//! read, so the few thousand workouts of a lifetime wait (a few hundred
//! bytes each) until their routes are joined to them, then go in batches of
//! their own. Clinical records and ECGs follow, a couple of hundred at a time.
//!
//! # What is dropped, and how it is counted
//!
//! A record type that is not in the catalogue is counted by name in
//! [`HealthBatch::skipped`]; a record that is malformed (no date, a value
//! that is not a number) in [`HealthBatch::rejected`]. Two things are dropped
//! without a count because they carry nothing: an `Idle` stand hour (an hour
//! with no stand is the absence of a sample), and the records inside a
//! `<Correlation>`, which Apple writes out a second time at the top level.
//!
//! Nothing here logs a value: not a reading, not a document's text.

use super::super::{
    Activity, DocKind, HealthBatch, IMPORT_BATCH, Metric, RawDocument, RawSample, RawWorkout,
    RoutePoint, SleepStage,
};
use super::{fhir, hae};
use crate::error::{Error, Result};
use jiff::civil::Date;
use jiff::{SignedDuration, Timestamp};
use quick_xml::Reader;
use quick_xml::encoding::Decoder;
use quick_xml::events::attributes::Attribute;
use quick_xml::events::{BytesStart, Event};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek};
use std::sync::atomic::{AtomicBool, Ordering};
use zip::ZipArchive;
use zip::result::ZipError;

/// How often, in bytes of `export.xml`, progress is reported (and the
/// cancel flag looked at) between batches.
const PROGRESS_EVERY: u64 = 1 << 20;
/// How much of the zip entry is read ahead of the parser.
const BUFFER_BYTES: usize = 1 << 20;
/// How many documents (clinical records, ECGs) go to the sink at a time.
const DOCUMENT_BATCH: usize = 200;
/// The largest clinical record or ECG that is read. Real ones are
/// kilobytes to a few hundred; this is what a hostile archive cannot exceed.
const MAX_DOCUMENT_BYTES: u64 = 64 << 20;

/// How far the reader has got through `export.xml`, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Progress {
    pub read: u64,
    pub total: u64,
}

/// Read a whole `export.zip`: every batch of `export.xml` to `sink` as it
/// fills, then one batch of workout routes joined to their workouts' and
/// one of clinical records and ECGs as documents. Stops at the next batch
/// boundary once `cancel` is set, answering `Error::Invalid("cancelled")`.
pub fn read_export<R: Read + Seek>(
    zip: R,
    sink: &mut dyn FnMut(HealthBatch) -> Result<()>,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<()> {
    let mut archive = ZipArchive::new(zip).map_err(zip_error)?;
    let index = export_xml_index(&archive)?;
    // The routes and documents sit beside `export.xml`, in a folder that
    // is named in the phone's language.
    let root = archive
        .name_for_index(index)
        .and_then(|name| name.rfind('/').map(|slash| name[..=slash].to_string()))
        .unwrap_or_default();
    let pending = {
        let entry = archive.by_index(index).map_err(zip_error)?;
        let total = entry.size();
        read_records(BufReader::with_capacity(BUFFER_BYTES, entry), total, sink, progress, cancel)?
    };
    let workouts = join_routes(&mut archive, &root, pending, cancel)?;
    emit_workouts(workouts, sink, cancel)?;
    read_documents(&mut archive, sink, cancel)
}

/// Read `export.xml` alone (what [`read_export`] does inside the zip), for
/// tests and for an export somebody unzipped themselves. `total` is the
/// XML's size for progress, 0 if unknown. Workouts arrive without routes,
/// which are separate files.
pub fn read_export_xml<R: BufRead>(
    xml: R,
    total: u64,
    sink: &mut dyn FnMut(HealthBatch) -> Result<()>,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<()> {
    let pending = read_records(xml, total, sink, progress, cancel)?;
    emit_workouts(pending.into_iter().map(|p| p.workout).collect(), sink, cancel)
}

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) { Err(Error::Invalid("cancelled".into())) } else { Ok(()) }
}

fn zip_error(e: ZipError) -> Error {
    match e {
        ZipError::Io(e) => Error::RawIo(e),
        other => Error::Invalid(format!("that is not a readable export.zip ({other})")),
    }
}

/// The entry that is `export.xml`, whatever folder it is in. Not
/// `export_cda.xml`, and not a macOS resource fork (`._export.xml`).
fn export_xml_index<R: Read + Seek>(archive: &ZipArchive<R>) -> Result<usize> {
    (0..archive.len())
        .find(|&i| {
            archive.name_for_index(i).is_some_and(|name| {
                name.rsplit('/').next().is_some_and(|base| base.eq_ignore_ascii_case("export.xml"))
            })
        })
        .ok_or_else(|| {
            Error::Invalid("that zip has no export.xml, so it is not an Apple Health export".into())
        })
}

// ---- export.xml ---------------------------------------------------------------------

/// A workout read from the XML, with the GPX files its route is in.
struct PendingWorkout {
    workout: RawWorkout,
    routes: Vec<String>,
}

/// Pull-parse `export.xml`, handing samples to `sink` as they fill and
/// answering the workouts, which wait for their routes.
fn read_records<R: BufRead>(
    xml: R,
    total: u64,
    sink: &mut dyn FnMut(HealthBatch) -> Result<()>,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<Vec<PendingWorkout>> {
    let mut reader = Reader::from_reader(xml);
    let decoder = reader.decoder();
    let mut state = State::default();
    let mut buf = Vec::new();
    let mut reported = 0;
    loop {
        buf.clear();
        let event = match reader.read_event_into(&mut buf) {
            Ok(event) => event,
            Err(e) => {
                return Err(Error::Invalid(format!(
                    "export.xml could not be read near byte {}: {e}",
                    reader.buffer_position()
                )));
            }
        };
        match event {
            Event::Start(e) => state.open(&e, decoder, false),
            Event::Empty(e) => state.open(&e, decoder, true),
            Event::End(e) => state.close(e.name().as_ref()),
            Event::Eof => break,
            _ => {}
        }
        let read = reader.buffer_position();
        let full = state.batch.samples.len() >= IMPORT_BATCH;
        if full || read - reported >= PROGRESS_EVERY {
            check_cancel(cancel)?;
            reported = read;
            progress(Progress { read, total });
        }
        if full {
            sink(std::mem::take(&mut state.batch))?;
        }
    }
    progress(Progress { read: reader.buffer_position(), total });
    let State { batch, workouts, .. } = state;
    if !batch.is_empty() || batch.rejected > 0 || !batch.skipped.is_empty() {
        sink(batch)?;
    }
    Ok(workouts)
}

/// Where the parser is in the document, and what it has gathered.
#[derive(Default)]
struct State {
    batch: HealthBatch,
    workouts: Vec<PendingWorkout>,
    /// The metric each record type is, looked up once (`None` for a type
    /// the catalogue does not have).
    metrics: HashMap<String, Option<Metric>>,
    /// A `<Record>` that has children still to come.
    record: Option<Record>,
    /// A `<Workout>` whose children are still to come.
    workout: Option<OpenWorkout>,
    /// Inside a `<Correlation>`, whose records Apple repeats at the top level.
    correlation: bool,
    /// Inside a `<WorkoutActivity>`, whose statistics are its own, not the
    /// workout's.
    activity: u32,
}

impl State {
    fn open(&mut self, e: &BytesStart<'_>, decoder: Decoder, empty: bool) {
        match e.name().as_ref() {
            b"Record" if !self.correlation => {
                let record = Record::read(e, decoder);
                if empty { self.file_record(record) } else { self.record = Some(record) }
            }
            b"Correlation" => {
                if !empty {
                    self.correlation = true;
                }
            }
            b"MetadataEntry" => self.metadata(e, decoder),
            b"Workout" => match read_workout(e, decoder) {
                Some(pending) if empty => self.finish_workout(OpenWorkout::new(pending)),
                Some(pending) => self.workout = Some(OpenWorkout::new(pending)),
                None => self.reject(),
            },
            b"WorkoutActivity" => {
                if !empty {
                    self.activity += 1;
                }
            }
            b"WorkoutStatistics" => {
                if self.activity == 0
                    && let Some(open) = self.workout.as_mut()
                {
                    open.statistics(e, decoder);
                }
            }
            b"FileReference" => {
                if let Some(open) = self.workout.as_mut()
                    && let Some(path) = attr(e, decoder, b"path")
                {
                    open.pending.routes.push(path);
                }
            }
            b"ActivitySummary" => self.batch.skip("ActivitySummary"),
            _ => {}
        }
    }

    fn close(&mut self, name: &[u8]) {
        match name {
            b"Record" => {
                if let Some(record) = self.record.take() {
                    self.file_record(record);
                }
            }
            b"Workout" => {
                if let Some(open) = self.workout.take() {
                    self.finish_workout(open);
                }
            }
            b"Correlation" => self.correlation = false,
            b"WorkoutActivity" => self.activity = self.activity.saturating_sub(1),
            _ => {}
        }
    }

    fn reject(&mut self) {
        self.batch.rejected += 1;
    }

    /// A `<MetadataEntry>` of the record or workout it is inside.
    fn metadata(&mut self, e: &BytesStart<'_>, decoder: Decoder) {
        if self.record.is_none() && self.workout.is_none() {
            return;
        }
        let (Some(key), Some(value)) = (attr(e, decoder, b"key"), attr(e, decoder, b"value"))
        else {
            return;
        };
        if let Some(record) = self.record.as_mut() {
            if key == "HKInsulinDeliveryReason" {
                record.reason = value.trim().parse().ok();
            }
        } else if self.activity == 0
            && let Some(open) = self.workout.as_mut()
        {
            open.metadata(&key, &value);
        }
    }

    fn metric_for(&mut self, kind: &str) -> Option<Metric> {
        if let Some(known) = self.metrics.get(kind) {
            return *known;
        }
        let metric = Metric::from_apple(kind);
        self.metrics.insert(kind.to_string(), metric);
        metric
    }

    fn finish_workout(&mut self, open: OpenWorkout) {
        let OpenWorkout { mut pending, basal_kcal } = open;
        let w = &mut pending.workout;
        if let (Some(active), Some(basal)) = (w.active_energy_kcal, basal_kcal) {
            w.total_energy_kcal = Some(active + basal);
        }
        self.workouts.push(pending);
    }

    /// File one `<Record>` as a sample, count it as skipped, or reject it.
    fn file_record(&mut self, r: Record) {
        let kind = match r.kind.as_str() {
            "HKCategoryTypeIdentifierSleepAnalysis" => Kind::Sleep,
            "HKCategoryTypeIdentifierAppleStandHour" => Kind::StandHour,
            "HKCategoryTypeIdentifierMindfulSession" => Kind::Mindful,
            "HKQuantityTypeIdentifierInsulinDelivery" => Kind::Insulin,
            other => match self.metric_for(other) {
                Some(metric) => Kind::Plain(metric),
                None => return self.batch.skip(other),
            },
        };
        let (Some((start, offset_min)), Some((end, _))) =
            (hae::parse_date(&r.start), hae::parse_date(&r.end))
        else {
            return self.reject();
        };
        if end < start {
            return self.reject();
        }
        let (metric, value, unit) = match kind {
            Kind::Sleep => match sleep_stage(&r.value) {
                Some(stage) => (Metric::SleepStage, stage.code(), String::new()),
                None => return self.reject(),
            },
            Kind::StandHour => match r.value.as_str() {
                "HKCategoryValueAppleStandHourStood" => (Metric::StandHour, 1.0, String::new()),
                "HKCategoryValueAppleStandHourIdle" => return,
                _ => return self.reject(),
            },
            Kind::Mindful => {
                let minutes = (end.as_second() - start.as_second()) as f64 / 60.0;
                (Metric::MindfulMinutes, minutes, "min".to_string())
            }
            Kind::Insulin => {
                // HealthKit's insulin delivery reasons: 1 basal, 2 bolus.
                let metric = match r.reason {
                    Some(1) => Metric::InsulinBasal,
                    Some(2) => Metric::InsulinBolus,
                    _ => return self.reject(),
                };
                match number(&r.value) {
                    Some(value) => (metric, value, r.unit),
                    None => return self.reject(),
                }
            }
            Kind::Plain(metric) => match number(&r.value) {
                Some(value) if r.unit == "%" => (metric, percent(value), r.unit),
                Some(value) => (metric, value, r.unit),
                None => return self.reject(),
            },
        };
        self.batch.samples.push(RawSample {
            metric,
            start,
            end,
            value,
            unit,
            source: r.source,
            offset_min,
        });
    }
}

/// How a `<Record>` becomes a sample: the four types that are not simply a
/// number in a unit, and the ones that are.
enum Kind {
    Sleep,
    StandHour,
    Mindful,
    Insulin,
    Plain(Metric),
}

/// The attributes of a `<Record>` that matter, and the insulin reason read
/// from its `<MetadataEntry>` child once that arrives.
#[derive(Default)]
struct Record {
    kind: String,
    source: String,
    unit: String,
    start: String,
    end: String,
    value: String,
    reason: Option<u8>,
}

impl Record {
    fn read(e: &BytesStart<'_>, decoder: Decoder) -> Record {
        let mut record = Record::default();
        for a in e.attributes().with_checks(false).flatten() {
            let slot = match a.key.as_ref() {
                b"type" => &mut record.kind,
                b"sourceName" => &mut record.source,
                b"unit" => &mut record.unit,
                b"startDate" => &mut record.start,
                b"endDate" => &mut record.end,
                b"value" => &mut record.value,
                _ => continue,
            };
            *slot = text(&a, decoder);
        }
        record
    }
}

/// HealthKit stores a percentage as a fraction (oxygen saturation 0.97) and
/// still calls its unit `%`. Every other source writes 97, and a reading
/// must be the same number whichever way it arrived to be stored once, so it
/// is scaled here and rounded to shed the noise of the multiplication
/// (0.57 * 100 is 56.99999999999999).
fn percent(fraction: f64) -> f64 {
    (fraction * 100.0 * 1e9).round() / 1e9
}

/// A number, finite, or nothing.
fn number(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|n| n.is_finite())
}

fn sleep_stage(value: &str) -> Option<SleepStage> {
    Some(match value.strip_prefix("HKCategoryValueSleepAnalysis")? {
        "InBed" => SleepStage::InBed,
        "Awake" => SleepStage::Awake,
        "Asleep" | "AsleepUnspecified" => SleepStage::Asleep,
        "AsleepCore" => SleepStage::Core,
        "AsleepDeep" => SleepStage::Deep,
        "AsleepREM" => SleepStage::Rem,
        _ => return None,
    })
}

/// An attribute's value, unescaped (`Hari&apos;s` is `Hari's`). One that
/// uses an entity the file never declared is kept as written.
fn text(a: &Attribute<'_>, decoder: Decoder) -> String {
    match a.decode_and_unescape_value(decoder) {
        Ok(value) => value.into_owned(),
        Err(_) => String::from_utf8_lossy(&a.value).into_owned(),
    }
}

fn attr(e: &BytesStart<'_>, decoder: Decoder, name: &[u8]) -> Option<String> {
    e.attributes()
        .with_checks(false)
        .flatten()
        .find(|a| a.key.as_ref() == name)
        .map(|a| text(&a, decoder))
}

// ---- workouts -----------------------------------------------------------------------

/// A workout with its children still arriving.
struct OpenWorkout {
    pending: PendingWorkout,
    /// Basal energy over the workout, which with the active energy is the
    /// total.
    basal_kcal: Option<f64>,
}

impl OpenWorkout {
    fn new(pending: PendingWorkout) -> OpenWorkout {
        OpenWorkout { pending, basal_kcal: None }
    }

    /// A `<WorkoutStatistics type sum average maximum unit>`.
    fn statistics(&mut self, e: &BytesStart<'_>, decoder: Decoder) {
        let Some(kind) = attr(e, decoder, b"type") else { return };
        let unit = attr(e, decoder, b"unit").unwrap_or_default();
        let figure = |name: &[u8]| attr(e, decoder, name).as_deref().and_then(number);
        let w = &mut self.pending.workout;
        match kind.strip_prefix("HKQuantityTypeIdentifier").unwrap_or(&kind) {
            "ActiveEnergyBurned" => {
                if let Some(kcal) = figure(b"sum").and_then(|v| hae::energy_kcal(v, &unit)) {
                    w.active_energy_kcal = Some(kcal);
                }
            }
            "BasalEnergyBurned" => {
                self.basal_kcal = figure(b"sum").and_then(|v| hae::energy_kcal(v, &unit));
            }
            "DistanceWalkingRunning"
            | "DistanceCycling"
            | "DistanceSwimming"
            | "DistanceWheelchair" => {
                if let Some(m) = figure(b"sum").and_then(|v| hae::length_m(v, &unit)) {
                    w.distance_m = Some(m);
                }
            }
            "HeartRate" => {
                w.avg_hr = figure(b"average");
                w.max_hr = figure(b"maximum");
            }
            _ => {}
        }
    }

    /// A `<MetadataEntry>` of the workout: whether it was indoors, and the
    /// climb (`1234 cm`).
    fn metadata(&mut self, key: &str, value: &str) {
        let w = &mut self.pending.workout;
        match key {
            "HKIndoorWorkout" => w.indoor = Some(value.trim() == "1"),
            "HKElevationAscended" => {
                let value = value.trim();
                let (amount, unit) = value.split_once(' ').unwrap_or((value, ""));
                w.elevation_up_m = number(amount).and_then(|n| hae::length_m(n, unit.trim()));
            }
            _ => {}
        }
    }
}

/// A `<Workout>`'s own attributes. `None` for one with no usable start and
/// end. Apple's `totalEnergyBurned` is the active energy; the basal energy
/// arrives in a statistic.
fn read_workout(e: &BytesStart<'_>, decoder: Decoder) -> Option<PendingWorkout> {
    let (mut kind, mut source) = (String::new(), String::new());
    let (mut duration, mut distance, mut energy) = (None, None, None);
    let (mut duration_unit, mut distance_unit, mut energy_unit) =
        (String::new(), String::new(), String::new());
    let (mut start, mut end) = (None, None);
    for a in e.attributes().with_checks(false).flatten() {
        let value = text(&a, decoder);
        match a.key.as_ref() {
            b"workoutActivityType" => kind = value,
            b"duration" => duration = number(&value),
            b"durationUnit" => duration_unit = value,
            b"totalDistance" => distance = number(&value),
            b"totalDistanceUnit" => distance_unit = value,
            b"totalEnergyBurned" => energy = number(&value),
            b"totalEnergyBurnedUnit" => energy_unit = value,
            b"sourceName" => source = value,
            b"startDate" => start = hae::parse_date(&value),
            b"endDate" => end = hae::parse_date(&value),
            _ => {}
        }
    }
    let (start, offset_min) = start?;
    let end = match end {
        Some((end, _)) => end,
        None => {
            // No end date: the start and the duration give it.
            let per_unit = match duration_unit.as_str() {
                "min" => 60.0,
                "hr" => 3600.0,
                "s" => 1.0,
                _ => return None,
            };
            let secs = duration? * per_unit;
            if !(0.0..=1e7).contains(&secs) {
                return None;
            }
            start.checked_add(SignedDuration::from_secs(secs as i64)).ok()?
        }
    };
    if end < start {
        return None;
    }
    let (activity, name) = activity_from_apple(&kind);
    Some(PendingWorkout {
        workout: RawWorkout {
            activity,
            name,
            start,
            end,
            offset_min,
            active_energy_kcal: energy.and_then(|v| hae::energy_kcal(v, &energy_unit)),
            total_energy_kcal: None,
            distance_m: distance.and_then(|v| hae::length_m(v, &distance_unit)),
            elevation_up_m: None,
            avg_hr: None,
            max_hr: None,
            effort: None,
            indoor: None,
            source,
            external_id: String::new(),
            route: Vec::new(),
            heart_rate: Vec::new(),
        },
        routes: Vec::new(),
    })
}

/// The activity an `HKWorkoutActivityType*` is, and a name for the ones that
/// are only `Other`: `CrossTraining` becomes "Cross Training".
fn activity_from_apple(identifier: &str) -> (Activity, String) {
    let name = identifier.strip_prefix("HKWorkoutActivityType").unwrap_or(identifier);
    let activity = match name {
        "Walking" => Activity::Walking,
        "Running" => Activity::Running,
        "Cycling" => Activity::Cycling,
        "Swimming" => Activity::Swimming,
        "Hiking" => Activity::Hiking,
        "Rowing" => Activity::Rowing,
        "Elliptical" => Activity::Elliptical,
        "StairClimbing" | "Stairs" => Activity::StairClimbing,
        "TraditionalStrengthTraining" => Activity::StrengthTraining,
        "FunctionalStrengthTraining" => Activity::FunctionalTraining,
        "CoreTraining" => Activity::CoreTraining,
        "HighIntensityIntervalTraining" => Activity::Hiit,
        "Yoga" => Activity::Yoga,
        "Pilates" => Activity::Pilates,
        "Dance" | "SocialDance" | "CardioDance" => Activity::Dance,
        "MartialArts" => Activity::MartialArts,
        "Tennis" => Activity::Tennis,
        "Soccer" => Activity::Soccer,
        "Basketball" => Activity::Basketball,
        "Golf" => Activity::Golf,
        "Cooldown" => Activity::Cooldown,
        "MindAndBody" => Activity::Mindfulness,
        "" | "Other" => return (Activity::Other, String::new()),
        other => return (Activity::Other, prettify(other)),
    };
    (activity, String::new())
}

/// `TableTennis` as "Table Tennis".
fn prettify(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    let mut previous: Option<char> = None;
    for c in name.chars() {
        if c.is_uppercase() && previous.is_some_and(|p| p.is_lowercase() || p.is_ascii_digit()) {
            out.push(' ');
        }
        out.push(c);
        previous = Some(c);
    }
    out
}

fn emit_workouts(
    mut workouts: Vec<RawWorkout>,
    sink: &mut dyn FnMut(HealthBatch) -> Result<()>,
    cancel: &AtomicBool,
) -> Result<()> {
    while !workouts.is_empty() {
        check_cancel(cancel)?;
        let rest = workouts.split_off(workouts.len().min(IMPORT_BATCH));
        let batch =
            HealthBatch { workouts: std::mem::replace(&mut workouts, rest), ..Default::default() };
        sink(batch)?;
    }
    Ok(())
}

// ---- routes -------------------------------------------------------------------------

/// Join each workout to the GPX files it names. A file that is missing from
/// the zip or cannot be read leaves that workout without a route: a
/// workout is worth having without one.
fn join_routes<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    root: &str,
    pending: Vec<PendingWorkout>,
    cancel: &AtomicBool,
) -> Result<Vec<RawWorkout>> {
    let mut workouts = Vec::with_capacity(pending.len());
    for PendingWorkout { mut workout, routes } in pending {
        if !routes.is_empty() {
            check_cancel(cancel)?;
        }
        for path in routes {
            workout.route.extend(read_route(archive, root, &path));
        }
        workout.route.sort_by_key(|p| p.at);
        workouts.push(workout);
    }
    Ok(workouts)
}

/// The points of one `FileReference path="/workout-routes/route_....gpx"`:
/// the path is from the export's folder, whatever that is called.
fn read_route<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    root: &str,
    path: &str,
) -> Vec<RoutePoint> {
    let relative = path.trim_start_matches('/');
    let Some(index) = archive
        .index_for_name(&format!("{root}{relative}"))
        .or_else(|| archive.index_for_name(relative))
    else {
        return Vec::new();
    };
    match archive.by_index(index) {
        Ok(entry) => read_gpx(BufReader::new(entry)),
        Err(_) => Vec::new(),
    }
}

/// `<trkpt lat lon><ele/><time/></trkpt>`. A point with no time is dropped
/// (it cannot be placed in the workout); a file that breaks off keeps the
/// points before the break.
fn read_gpx<R: BufRead>(xml: R) -> Vec<RoutePoint> {
    enum Field {
        None,
        Ele,
        Time,
    }
    let mut reader = Reader::from_reader(xml);
    let decoder = reader.decoder();
    let mut buf = Vec::new();
    let mut points = Vec::new();
    let mut open: Option<(f64, f64)> = None;
    let (mut ele, mut time): (Option<f64>, Option<Timestamp>) = (None, None);
    let mut field = Field::None;
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"trkpt" => {
                    let coordinate =
                        |name: &[u8]| attr(&e, decoder, name).as_deref().and_then(number);
                    open = coordinate(b"lat").zip(coordinate(b"lon"));
                    (ele, time) = (None, None);
                }
                b"ele" if open.is_some() => field = Field::Ele,
                b"time" if open.is_some() => field = Field::Time,
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if let Ok(t) = t.decode() {
                    match field {
                        Field::Ele => ele = number(&t),
                        Field::Time => time = t.trim().parse().ok(),
                        Field::None => {}
                    }
                }
            }
            Ok(Event::End(e)) => {
                field = Field::None;
                if e.name().as_ref() == b"trkpt"
                    && let Some((lat, lon)) = open.take()
                    && let Some(at) = time.take()
                {
                    points.push(RoutePoint { at, lat, lon, alt: ele.take() });
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    points
}

// ---- clinical records and ECGs --------------------------------------------------------

/// What a zip entry beyond `export.xml` is read as.
enum Extra {
    Clinical,
    Ecg,
}

/// Read each `clinical-records/*.json` through [`fhir::read_file`] and each
/// `electrocardiograms/*.csv` as an ECG document. One that cannot be read
/// is counted as rejected and the rest carry on.
fn read_documents<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    sink: &mut dyn FnMut(HealthBatch) -> Result<()>,
    cancel: &AtomicBool,
) -> Result<()> {
    let wanted: Vec<(usize, String, Extra)> = (0..archive.len())
        .filter_map(|i| {
            let name = archive.name_for_index(i)?;
            let lower = name.to_ascii_lowercase();
            let extra = if lower.contains("clinical-records/") && lower.ends_with(".json") {
                Extra::Clinical
            } else if lower.contains("electrocardiograms/") && lower.ends_with(".csv") {
                Extra::Ecg
            } else {
                return None;
            };
            Some((i, name.rsplit('/').next().unwrap_or(name).to_string(), extra))
        })
        .collect();
    let mut batch = HealthBatch::default();
    for (index, file_name, extra) in wanted {
        check_cancel(cancel)?;
        let Some(bytes) = read_entry(archive, index) else {
            batch.rejected += 1;
            continue;
        };
        match extra {
            Extra::Clinical => match fhir::read_file(&bytes, &file_name) {
                Ok(document) => batch.documents.push(document),
                Err(_) => batch.rejected += 1,
            },
            Extra::Ecg => batch.documents.push(ecg_document(bytes, &file_name)),
        }
        if batch.documents.len() >= DOCUMENT_BATCH {
            sink(std::mem::take(&mut batch))?;
        }
    }
    if !batch.is_empty() || batch.rejected > 0 {
        sink(batch)?;
    }
    Ok(())
}

/// One zip entry's bytes, or `None` if it is too big or will not read.
fn read_entry<R: Read + Seek>(archive: &mut ZipArchive<R>, index: usize) -> Option<Vec<u8>> {
    let entry = archive.by_index(index).ok()?;
    if entry.size() > MAX_DOCUMENT_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry.take(MAX_DOCUMENT_BYTES).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// An ECG's CSV as a document. Its header lines (`Recorded Date,2023-11-01
/// 08:00:00 -0700`, `Classification,...`) are read only for the date; the
/// file itself, the voltages, is kept as it is.
fn ecg_document(bytes: Vec<u8>, file_name: &str) -> RawDocument {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).into_owned();
    let date = head
        .lines()
        .find_map(|line| line.trim_start_matches('\u{feff}').strip_prefix("Recorded Date,"))
        .and_then(|value| value.trim().trim_matches('"').get(..10)?.parse::<Date>().ok());
    let stem = file_name.rsplit_once('.').map_or(file_name, |(stem, _)| stem);
    RawDocument {
        title: match date {
            Some(date) => format!("ECG {date}"),
            None => format!("ECG {stem}"),
        },
        kind: DocKind::Ecg,
        date,
        provider: String::new(),
        file_name: file_name.to_string(),
        mime: "text/csv".to_string(),
        bytes,
        fhir: Vec::new(),
        labs: Vec::new(),
        external_id: format!("apple-ecg:{file_name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    /// Fold `b` into `all`, as the vault does batch by batch.
    fn absorb(all: &mut HealthBatch, b: HealthBatch) {
        all.samples.extend(b.samples);
        all.workouts.extend(b.workouts);
        all.doses.extend(b.doses);
        all.documents.extend(b.documents);
        for (what, n) in b.skipped {
            *all.skipped.entry(what).or_default() += n;
        }
        all.rejected += b.rejected;
    }

    fn read(xml: &str) -> HealthBatch {
        let mut all = HealthBatch::default();
        read_export_xml(
            xml.as_bytes(),
            xml.len() as u64,
            &mut |b| {
                absorb(&mut all, b);
                Ok(())
            },
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        all
    }

    const DTD: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE HealthData [
<!-- HealthKit Export Version: 14 -->
<!ELEMENT HealthData (ExportDate,Me,(Record|Correlation|Workout|ActivitySummary)*)>
<!ATTLIST HealthData
  locale CDATA #REQUIRED
>
<!ELEMENT Record ((MetadataEntry|HeartRateVariabilityMetadataList)*)>
<!ATTLIST Record
  type CDATA #REQUIRED
  unit CDATA #IMPLIED
  value CDATA #IMPLIED
>
]>
"#;

    /// A step count record, for tests that need many.
    const STEPS: &str = r#"<Record type="HKQuantityTypeIdentifierStepCount" sourceName="Watch" unit="count" creationDate="2023-11-01 08:06:00 -0700" startDate="2023-11-01 08:00:00 -0700" endDate="2023-11-01 08:05:00 -0700" value="1"/>
"#;

    fn export() -> String {
        format!(
            r#"{DTD}<HealthData locale="en_US">
 <ExportDate value="2023-11-02 09:00:00 -0700"/>
 <Me HKCharacteristicTypeIdentifierBiologicalSex="HKBiologicalSexNotSet"/>
 <Record type="HKQuantityTypeIdentifierStepCount" sourceName="Hari&apos;s Apple Watch" sourceVersion="10.1" unit="count" creationDate="2023-11-01 08:06:00 -0700" startDate="2023-11-01 08:00:00 -0700" endDate="2023-11-01 08:05:00 -0700" value="312"/>
 <Record type="HKQuantityTypeIdentifierHeartRate" sourceName="Apple Watch" unit="count/min" creationDate="2023-11-01 08:11:00 -0700" startDate="2023-11-01 08:10:00 -0700" endDate="2023-11-01 08:10:00 -0700" value="72">
  <MetadataEntry key="HKMetadataKeyHeartRateMotionContext" value="0"/>
 </Record>
 <Record type="HKCategoryTypeIdentifierSleepAnalysis" sourceName="Apple Watch" creationDate="2023-11-01 07:00:00 -0700" startDate="2023-10-31 23:00:00 -0700" endDate="2023-10-31 23:30:00 -0700" value="HKCategoryValueSleepAnalysisAsleepCore"/>
 <Record type="HKCategoryTypeIdentifierSleepAnalysis" sourceName="Apple Watch" creationDate="2023-11-01 07:00:00 -0700" startDate="2023-10-31 23:30:00 -0700" endDate="2023-10-31 23:40:00 -0700" value="HKCategoryValueSleepAnalysisAwake"/>
 <Record type="HKCategoryTypeIdentifierSleepAnalysis" sourceName="iPhone" creationDate="2023-11-01 07:00:00 -0700" startDate="2023-10-31 22:45:00 -0700" endDate="2023-11-01 06:30:00 -0700" value="HKCategoryValueSleepAnalysisInBed"/>
 <Record type="HKCategoryTypeIdentifierSleepAnalysis" sourceName="Apple Watch" creationDate="2023-11-01 07:00:00 -0700" startDate="2023-10-31 23:40:00 -0700" endDate="2023-11-01 00:10:00 -0700" value="HKCategoryValueSleepAnalysisAsleepREM"/>
 <Record type="HKCategoryTypeIdentifierSleepAnalysis" sourceName="Apple Watch" creationDate="2023-11-01 07:00:00 -0700" startDate="2023-11-01 00:10:00 -0700" endDate="2023-11-01 00:20:00 -0700" value="HKCategoryValueSleepAnalysisSomethingNew"/>
 <Record type="HKCategoryTypeIdentifierAppleStandHour" sourceName="Apple Watch" creationDate="2023-11-01 09:00:00 -0700" startDate="2023-11-01 08:00:00 -0700" endDate="2023-11-01 09:00:00 -0700" value="HKCategoryValueAppleStandHourStood"/>
 <Record type="HKCategoryTypeIdentifierAppleStandHour" sourceName="Apple Watch" creationDate="2023-11-01 10:00:00 -0700" startDate="2023-11-01 09:00:00 -0700" endDate="2023-11-01 10:00:00 -0700" value="HKCategoryValueAppleStandHourIdle"/>
 <Record type="HKCategoryTypeIdentifierMindfulSession" sourceName="Mindfulness" creationDate="2023-11-01 09:11:00 -0700" startDate="2023-11-01 09:00:00 -0700" endDate="2023-11-01 09:10:00 -0700" value="HKCategoryValueNotApplicable"/>
 <Record type="HKQuantityTypeIdentifierInsulinDelivery" sourceName="Pump" unit="IU" creationDate="2023-11-01 12:00:00 -0700" startDate="2023-11-01 12:00:00 -0700" endDate="2023-11-01 12:00:00 -0700" value="0.8">
  <MetadataEntry key="HKInsulinDeliveryReason" value="1"/>
 </Record>
 <Record type="HKQuantityTypeIdentifierInsulinDelivery" sourceName="Pump" unit="IU" creationDate="2023-11-01 12:30:00 -0700" startDate="2023-11-01 12:30:00 -0700" endDate="2023-11-01 12:30:00 -0700" value="5">
  <MetadataEntry key="HKInsulinDeliveryReason" value="2"/>
 </Record>
 <Record type="HKQuantityTypeIdentifierInsulinDelivery" sourceName="Pump" unit="IU" creationDate="2023-11-01 13:00:00 -0700" startDate="2023-11-01 13:00:00 -0700" endDate="2023-11-01 13:00:00 -0700" value="1"/>
 <Record type="HKQuantityTypeIdentifierMadeUpForTheTest" sourceName="Watch" unit="x" creationDate="2023-11-01 08:06:00 -0700" startDate="2023-11-01 08:00:00 -0700" endDate="2023-11-01 08:05:00 -0700" value="1"/>
 <Record type="HKQuantityTypeIdentifierMadeUpForTheTest" sourceName="Watch" unit="x" creationDate="2023-11-01 08:06:00 -0700" startDate="2023-11-01 08:10:00 -0700" endDate="2023-11-01 08:15:00 -0700" value="2"/>
 <Record type="HKQuantityTypeIdentifierStepCount" sourceName="Watch" unit="count" startDate="2023-11-01 08:00:00 -0700" endDate="2023-11-01 08:05:00 -0700" value="lots"/>
 <Record type="HKQuantityTypeIdentifierStepCount" sourceName="Watch" unit="count" startDate="2023-11-01 08:00:00 -0700" value="5"/>
 <Correlation type="HKCorrelationTypeIdentifierBloodPressure" sourceName="Health" creationDate="2023-11-01 08:00:00 -0700" startDate="2023-11-01 08:00:00 -0700" endDate="2023-11-01 08:00:00 -0700">
  <Record type="HKQuantityTypeIdentifierStepCount" sourceName="Health" unit="count" startDate="2023-11-01 20:00:00 -0700" endDate="2023-11-01 20:05:00 -0700" value="999"/>
 </Correlation>
 <Workout workoutActivityType="HKWorkoutActivityTypeRunning" duration="45" durationUnit="min" totalDistance="5.2" totalDistanceUnit="km" totalEnergyBurned="410" totalEnergyBurnedUnit="kcal" sourceName="Apple Watch" sourceVersion="10.1" creationDate="2023-11-01 07:50:00 -0700" startDate="2023-11-01 07:00:00 -0700" endDate="2023-11-01 07:45:00 -0700">
  <MetadataEntry key="HKIndoorWorkout" value="0"/>
  <MetadataEntry key="HKElevationAscended" value="1234 cm"/>
  <WorkoutEvent type="HKWorkoutEventTypePause" date="2023-11-01 07:20:00 -0700"/>
  <WorkoutStatistics type="HKQuantityTypeIdentifierActiveEnergyBurned" startDate="2023-11-01 07:00:00 -0700" endDate="2023-11-01 07:45:00 -0700" sum="420.5" unit="kcal"/>
  <WorkoutStatistics type="HKQuantityTypeIdentifierBasalEnergyBurned" startDate="2023-11-01 07:00:00 -0700" endDate="2023-11-01 07:45:00 -0700" sum="60" unit="kcal"/>
  <WorkoutStatistics type="HKQuantityTypeIdentifierDistanceWalkingRunning" startDate="2023-11-01 07:00:00 -0700" endDate="2023-11-01 07:45:00 -0700" sum="5.25" unit="km"/>
  <WorkoutStatistics type="HKQuantityTypeIdentifierHeartRate" startDate="2023-11-01 07:00:00 -0700" endDate="2023-11-01 07:45:00 -0700" average="152.5" minimum="95" maximum="178" unit="count/min"/>
  <WorkoutRoute sourceName="Apple Watch" startDate="2023-11-01 07:00:00 -0700" endDate="2023-11-01 07:45:00 -0700">
   <FileReference path="/workout-routes/route_1.gpx"/>
  </WorkoutRoute>
 </Workout>
 <Workout workoutActivityType="HKWorkoutActivityTypeCrossTraining" duration="20" durationUnit="min" sourceName="Apple Watch" startDate="2023-11-02 07:00:00 -0700" endDate="2023-11-02 07:20:00 -0700"/>
 <ActivitySummary dateComponents="2023-11-01" activeEnergyBurned="420" activeEnergyBurnedGoal="500" appleExerciseTime="40" appleStandHours="10"/>
</HealthData>
"#
        )
    }

    #[test]
    fn records_become_samples_in_the_units_they_came_in() {
        let batch = read(&export());
        let steps = &batch.samples[0];
        assert_eq!(steps.metric, Metric::Steps);
        assert_eq!(steps.value, 312.0);
        assert_eq!(steps.unit, "count");
        assert_eq!(steps.source, "Hari's Apple Watch", "entities are unescaped");
        assert_eq!(steps.start, at("2023-11-01T15:00:00Z"));
        assert_eq!(steps.end, at("2023-11-01T15:05:00Z"));
        assert_eq!(steps.offset_min, -420);
        let heart = &batch.samples[1];
        assert_eq!(
            (heart.metric, heart.value, heart.unit.as_str()),
            (Metric::HeartRate, 72.0, "count/min")
        );
        assert_eq!(heart.start, heart.end);
    }

    #[test]
    fn sleep_stages_stand_hours_and_mindful_minutes_are_samples() {
        let batch = read(&export());
        let of = |metric: Metric| -> Vec<&RawSample> {
            batch.samples.iter().filter(|s| s.metric == metric).collect()
        };
        let stages: Vec<f64> = of(Metric::SleepStage).iter().map(|s| s.value).collect();
        assert_eq!(
            stages,
            vec![
                SleepStage::Core.code(),
                SleepStage::Awake.code(),
                SleepStage::InBed.code(),
                SleepStage::Rem.code()
            ],
            "an unknown stage is rejected, not guessed"
        );
        let stood = of(Metric::StandHour);
        assert_eq!(stood.len(), 1, "an idle hour is not a sample");
        assert_eq!(stood[0].value, 1.0);
        let mindful = of(Metric::MindfulMinutes);
        assert_eq!(mindful.len(), 1);
        assert_eq!((mindful[0].value, mindful[0].unit.as_str()), (10.0, "min"));
    }

    #[test]
    fn percentages_are_scaled_from_the_fractions_healthkit_stores() {
        let xml = r#"<HealthData>
 <Record type="HKQuantityTypeIdentifierOxygenSaturation" sourceName="Apple Watch" unit="%" startDate="2023-11-01 08:00:00 -0700" endDate="2023-11-01 08:00:00 -0700" value="0.57"/>
</HealthData>"#;
        let batch = read(xml);
        assert_eq!(batch.samples.len(), 1);
        assert_eq!((batch.samples[0].value, batch.samples[0].unit.as_str()), (57.0, "%"));
        assert_eq!((percent(0.97), percent(0.185), percent(1.0)), (97.0, 18.5, 100.0));
    }

    #[test]
    fn insulin_follows_its_reason_metadata() {
        let batch = read(&export());
        let insulin: Vec<_> = batch
            .samples
            .iter()
            .filter(|s| matches!(s.metric, Metric::InsulinBasal | Metric::InsulinBolus))
            .map(|s| (s.metric, s.value, s.unit.as_str()))
            .collect();
        assert_eq!(
            insulin,
            vec![(Metric::InsulinBasal, 0.8, "IU"), (Metric::InsulinBolus, 5.0, "IU")]
        );
    }

    #[test]
    fn what_cannot_be_filed_is_counted() {
        let batch = read(&export());
        assert_eq!(batch.skipped.get("HKQuantityTypeIdentifierMadeUpForTheTest"), Some(&2));
        assert_eq!(batch.skipped.get("ActivitySummary"), Some(&1));
        // An unknown sleep stage, an insulin dose with no reason, a step
        // count that is not a number, and one with no end date.
        assert_eq!(batch.rejected, 4);
    }

    #[test]
    fn a_correlations_records_are_not_read_twice() {
        let batch = read(&export());
        let steps: Vec<f64> =
            batch.samples.iter().filter(|s| s.metric == Metric::Steps).map(|s| s.value).collect();
        assert_eq!(
            steps,
            vec![312.0],
            "the 999 inside the correlation duplicates a top-level record"
        );
    }

    #[test]
    fn a_workout_carries_its_statistics_in_kilocalories_and_metres() {
        let batch = read(&export());
        assert_eq!(batch.workouts.len(), 2);
        let w = &batch.workouts[0];
        assert_eq!(w.activity, Activity::Running);
        assert_eq!(w.name, "");
        assert_eq!(w.start, at("2023-11-01T14:00:00Z"));
        assert_eq!(w.end, at("2023-11-01T14:45:00Z"));
        assert_eq!(w.offset_min, -420);
        assert_eq!(w.active_energy_kcal, Some(420.5), "the statistic wins over totalEnergyBurned");
        assert_eq!(w.total_energy_kcal, Some(480.5), "active plus basal");
        assert_eq!(w.distance_m, Some(5250.0));
        assert!((w.elevation_up_m.unwrap() - 12.34).abs() < 1e-9);
        assert_eq!((w.avg_hr, w.max_hr), (Some(152.5), Some(178.0)));
        assert_eq!(w.indoor, Some(false));
        assert_eq!(w.source, "Apple Watch");
        assert!(w.route.is_empty(), "routes are separate files, not in the bare XML");

        let other = &batch.workouts[1];
        assert_eq!((other.activity, other.name.as_str()), (Activity::Other, "Cross Training"));
        assert_eq!(other.distance_m, None);
    }

    #[test]
    fn workout_activity_types_map_to_activities() {
        for (identifier, activity) in [
            ("HKWorkoutActivityTypeWalking", Activity::Walking),
            ("HKWorkoutActivityTypeCycling", Activity::Cycling),
            ("HKWorkoutActivityTypeTraditionalStrengthTraining", Activity::StrengthTraining),
            ("HKWorkoutActivityTypeFunctionalStrengthTraining", Activity::FunctionalTraining),
            ("HKWorkoutActivityTypeHighIntensityIntervalTraining", Activity::Hiit),
            ("HKWorkoutActivityTypeSocialDance", Activity::Dance),
            ("HKWorkoutActivityTypeMindAndBody", Activity::Mindfulness),
            ("HKWorkoutActivityTypeStairClimbing", Activity::StairClimbing),
        ] {
            assert_eq!(activity_from_apple(identifier), (activity, String::new()), "{identifier}");
        }
        assert_eq!(
            activity_from_apple("HKWorkoutActivityTypeTableTennis"),
            (Activity::Other, "Table Tennis".into())
        );
        assert_eq!(
            activity_from_apple("HKWorkoutActivityTypeOther"),
            (Activity::Other, String::new())
        );
    }

    #[test]
    fn samples_arrive_in_batches_and_progress_is_reported() {
        let xml = format!("<HealthData>\n{}</HealthData>", STEPS.repeat(IMPORT_BATCH + 10));
        let mut batches = Vec::new();
        let mut reports = Vec::new();
        read_export_xml(
            xml.as_bytes(),
            xml.len() as u64,
            &mut |b| {
                batches.push(b);
                Ok(())
            },
            &mut |p| reports.push(p),
            &AtomicBool::new(false),
        )
        .unwrap();
        let sizes: Vec<usize> = batches.iter().map(|b| b.samples.len()).collect();
        assert_eq!(sizes, vec![IMPORT_BATCH, 10]);
        assert!(!reports.is_empty());
        assert!(reports.iter().all(|p| p.total == xml.len() as u64 && p.read <= p.total));
        assert!(reports.windows(2).all(|w| w[0].read <= w[1].read));
    }

    #[test]
    fn cancel_stops_at_the_next_batch_with_a_cancelled_error() {
        let xml = format!("<HealthData>\n{}</HealthData>", STEPS.repeat(IMPORT_BATCH + 10));
        let mut sent = 0;
        let err = read_export_xml(
            xml.as_bytes(),
            0,
            &mut |_| {
                sent += 1;
                Ok(())
            },
            &mut |_| {},
            &AtomicBool::new(true),
        )
        .unwrap_err();
        assert!(matches!(&err, Error::Invalid(m) if m == "cancelled"), "{err}");
        assert_eq!(sent, 0);
    }

    #[test]
    fn xml_that_breaks_is_an_error_not_a_panic() {
        let broken = "<HealthData><Record type=\"x\"></Workout></HealthData>";
        let result = read_export_xml(
            broken.as_bytes(),
            0,
            &mut |_| Ok(()),
            &mut |_| {},
            &AtomicBool::new(false),
        );
        assert!(matches!(result, Err(Error::Invalid(_))));
    }

    // ---- the zip ----------------------------------------------------------------------

    fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, body) in files {
            writer.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(body.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    const GPX: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<gpx version="1.1" creator="Apple Health Export">
 <metadata><time>2023-11-01T14:00:00Z</time></metadata>
 <trk><name>Route 1</name><trkseg>
  <trkpt lat="37.3300" lon="-122.0300"><ele>12.5</ele><time>2023-11-01T14:00:02Z</time><extensions><speed>2.5</speed></extensions></trkpt>
  <trkpt lat="37.3310" lon="-122.0310"><ele>13.0</ele><time>2023-11-01T14:00:01Z</time></trkpt>
  <trkpt lat="37.3320" lon="-122.0320"><ele>13.5</ele></trkpt>
 </trkseg></trk>
</gpx>
"#;

    const ECG: &str = "Name,Test Person\nDate of Birth,1980-01-01\nRecorded Date,2023-11-01 08:00:00 -0700\nClassification,Sinus Rhythm\nSample Rate,512 hertz\nLead,Lead I\nUnit,uV\n\n1.5\n2.5\n";

    const OBSERVATION: &str = r#"{"resourceType": "Observation", "id": "obs-1", "status": "final",
        "category": [{"coding": [{"code": "laboratory"}]}],
        "code": {"coding": [{"system": "http://loinc.org", "code": "3016-3"}], "text": "TSH"},
        "effectiveDateTime": "2023-10-12T08:30:00-07:00", "valueQuantity": {"value": 2.1, "unit": "mIU/L"}}"#;

    fn read_zip(bytes: Vec<u8>) -> Result<(Vec<HealthBatch>, HealthBatch)> {
        let mut batches = Vec::new();
        let mut all = HealthBatch::default();
        read_export(
            Cursor::new(bytes),
            &mut |b| {
                batches.push(b.clone());
                absorb(&mut all, b);
                Ok(())
            },
            &mut |_| {},
            &AtomicBool::new(false),
        )?;
        Ok((batches, all))
    }

    #[test]
    fn a_zip_is_read_whole_routes_joined_documents_after() {
        // The folder is named in the phone's language, and `export_cda.xml`
        // beside it must not be taken for the export.
        let xml = export();
        let zip = zip_of(&[
            ("Sante Export/export_cda.xml", "<ClinicalDocument/>"),
            ("Sante Export/export.xml", xml.as_str()),
            ("Sante Export/workout-routes/route_1.gpx", GPX),
            ("Sante Export/clinical-records/Observation-1.json", OBSERVATION),
            ("Sante Export/clinical-records/notes.txt", "not a record"),
            ("Sante Export/electrocardiograms/ecg_2023-11-01.csv", ECG),
        ]);
        let (batches, all) = read_zip(zip).unwrap();

        assert_eq!(all.samples.iter().filter(|s| s.metric == Metric::Steps).count(), 1);
        assert_eq!(all.workouts.len(), 2);
        let route = &all.workouts[0].route;
        assert_eq!(route.len(), 2, "the point with no time is dropped");
        assert_eq!(route[0].at, at("2023-11-01T14:00:01Z"), "in time order");
        assert_eq!((route[1].lat, route[1].lon, route[1].alt), (37.33, -122.03, Some(12.5)));
        assert!(all.workouts[1].route.is_empty());

        // Samples first, then workouts, then documents.
        let last = batches.last().unwrap();
        assert_eq!(last.documents.len(), 2);
        assert!(last.samples.is_empty() && last.workouts.is_empty());
        assert!(batches.iter().rev().skip(1).any(|b| !b.workouts.is_empty()));

        let fhir = last.documents.iter().find(|d| d.kind == DocKind::Fhir).unwrap();
        assert_eq!(fhir.title, "TSH 2.1 mIU/L");
        assert_eq!(fhir.labs.len(), 1);
        assert_eq!(fhir.file_name, "Observation-1.json");
        let ecg = last.documents.iter().find(|d| d.kind == DocKind::Ecg).unwrap();
        assert_eq!(ecg.title, "ECG 2023-11-01");
        assert_eq!(ecg.date, Some("2023-11-01".parse().unwrap()));
        assert_eq!(ecg.mime, "text/csv");
        assert_eq!(ecg.bytes, ECG.as_bytes());
        assert_eq!(ecg.external_id, "apple-ecg:ecg_2023-11-01.csv");
    }

    #[test]
    fn a_zip_without_its_routes_or_with_a_bad_record_still_reads() {
        let xml = export();
        let zip = zip_of(&[
            ("apple_health_export/export.xml", xml.as_str()),
            ("apple_health_export/clinical-records/broken.json", "{ not json"),
        ]);
        let (_, all) = read_zip(zip).unwrap();
        assert_eq!(all.workouts.len(), 2);
        assert!(all.workouts[0].route.is_empty(), "the GPX is not in the zip");
        assert!(all.documents.is_empty());
        assert_eq!(all.rejected, 5, "the four bad records and the broken clinical record");
    }

    #[test]
    fn what_is_not_an_export_is_refused() {
        let no_export = zip_of(&[("apple_health_export/export_cda.xml", "<ClinicalDocument/>")]);
        assert!(matches!(read_zip(no_export), Err(Error::Invalid(_))));
        assert!(matches!(read_zip(b"this is not a zip".to_vec()), Err(Error::Invalid(_))));
    }

    #[test]
    fn an_ecg_with_no_date_is_named_by_its_file() {
        let doc = ecg_document(b"Name,x\n\n1\n".to_vec(), "ecg_odd.csv");
        assert_eq!(doc.title, "ECG ecg_odd");
        assert_eq!(doc.date, None);
    }
}
