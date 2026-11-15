//! Settings, sources, ingest, series, reports, workouts and keys. See the
//! module docs one level up.
//!
//! # One door in
//!
//! [`Vault::ingest_health`] is how everything a watch or a phone measured
//! arrives, whichever connector read it, and [`Vault::add_health_reading`]
//! is a person's own typing -- both file samples through the same two
//! steps: [`Intake`] makes each one canonical and refuses the implausible,
//! and [`file_samples`] merges what is left into its series days and
//! rebuilds their summaries.
//!
//! # Keys
//!
//! A series' name in the clear is a keyed hash of the metric's key, and a
//! workout's dedupe key a keyed hash of what makes it that workout, both
//! under the subkey [`HEALTH_KEY_LABEL`]. [`Vault::derive_subkey`] takes the
//! state lock, so code that already holds it -- every closure handed to
//! [`Vault::read`] or [`Vault::write`] -- derives from the session's own
//! cipher instead, as screen time does.
//!
//! # What is never logged
//!
//! Nothing here writes a value, a medication, a diagnosis or a document's
//! words to a log or into an error: a refusal says what kind of thing was
//! wrong, never what it was.

use super::{file_documents, file_source_doses};
use crate::crypto::SecretKey;
use crate::error::{Error, Result};
use crate::health::chunk::{Incoming, by_day, merge, points, ranker, remove, summarize, utc_day};
use crate::health::report::{self, ACTIVITY_METRICS};
use crate::health::{
    Activity, ActivityDay, ActivityGoals, AutomationSeen, BucketSize, Condition, ConditionStatus,
    Connector, DayTimeline, GlucoseReport, HEALTH_KEY_LABEL, HEALTH_KEYS_OWNER_KIND, HealthBatch,
    HealthDay, HealthIngestReport, HealthKeys, HealthKeysInfo, HealthSettings, HealthSource,
    LabDue, LabResult, Latest, MAX_BATCH_SAMPLES, MAX_RANGE_DAYS, MAX_SAMPLE_SECS, ManualReading,
    Medication, Metric, Nutrients, NutritionDay, PushHeaders, RawSample, RawWorkout, RoutePoint,
    SamplePoint, SeriesPoint, SeriesQuery, SeriesResult, SeriesSummary, SleepNight, SourceStatus,
    Stat, Workout, WorkoutDetail, glucose, sleep, templates, units,
};
use crate::id::{BlobId, HealthSourceId, WorkoutId};
use crate::record::RecordKind;
use crate::store::JournalStore;
use crate::store::health::{HealthStore, SeriesRow};
use crate::store::purpose::PurposeWindow;
use crate::store::secrets::SecretStore;
use crate::store::trackers::TrackerDay;
use crate::tracker::{Tracker, TrackerSource};
use crate::vault::Vault;
use crate::vault::session::{Domain, pick_domain};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Span, Timestamp};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// The longest name a person may give a connection, and the longest a
/// device's or an app's name is kept: in characters, after trimming.
const MAX_NAME: usize = 200;
/// The longest note on a workout.
const MAX_NOTE: usize = 2000;
/// The most names a source priority list may hold.
const MAX_PRIORITY_NAMES: usize = 50;
/// The longest a source may be quiet before Today says so: thirty days.
const MAX_STALE_HOURS: u32 = 720;
/// The most device names a source's status keeps.
const MAX_DEVICES: usize = 50;
/// The most automations a source's status keeps.
const MAX_AUTOMATIONS: usize = 50;
/// The longest a header Health Auto Export sent is kept. Outside text, so
/// cut rather than refused.
const MAX_HEADER: usize = 200;
/// The longest push failure a source's status keeps.
const MAX_ERROR: usize = 500;
/// The longest food-lookup key that is taken as one. api.data.gov's are
/// forty characters.
const MAX_KEY: usize = 200;
/// The longest range of raw samples one read answers.
const RAW_DAYS: i32 = 3;
/// What a reading typed in here is filed under, as the device that took it.
const MANUAL_SOURCE: &str = "Karma Yoga";
/// The owner id of the food-lookup keys in `record_secrets`: the vault's.
const KEYS_OWNER_ID: &str = "vault";
/// How far ahead Today looks for a test coming due.
const LABS_HORIZON_DAYS: i64 = 30;
/// How far back a trend arrow compares a reading with.
const TREND_DAYS: i64 = 30;
/// How many nights before a range are read, so the first night in it has
/// others to be scored for regularity against.
const SCORE_NIGHTS: usize = 7;
/// What Today shows the latest reading of, when there is one.
const TODAY_METRICS: &[Metric] = &[
    Metric::Weight,
    Metric::RestingHeartRate,
    Metric::HeartRateVariability,
    Metric::BloodGlucose,
    Metric::BloodPressureSystolic,
    Metric::BloodPressureDiastolic,
    Metric::OxygenSaturation,
    Metric::RespiratoryRate,
    Metric::Vo2Max,
];

const GLUCOSE_UNITS: &[&str] = &["mg/dL", "mmol/L"];
const WEIGHT_UNITS: &[&str] = &["kg", "lb"];
const DISTANCE_UNITS: &[&str] = &["km", "mi"];
const ENERGY_UNITS: &[&str] = &["kcal", "kJ"];
const TEMPERATURE_UNITS: &[&str] = &["C", "F"];
const LENGTH_UNITS: &[&str] = &["cm", "in"];

impl Vault {
    // ---- settings -------------------------------------------------------

    pub fn health_settings(&self) -> Result<HealthSettings> {
        self.with_health(|h| h.health_settings())
    }

    /// Checked (units are known spellings, goals and targets positive, a
    /// priority list at most 50 names of at most 200 characters, stale
    /// hours at most 720) and saved. Priority names are trimmed and blanks
    /// and repeats dropped, so the list a person sees is the one that
    /// ranks.
    pub fn save_health_settings(&self, settings: &HealthSettings) -> Result<()> {
        self.writable()?;
        let settings = checked_settings(settings)?;
        self.with_health(|h| h.put_health_settings(&settings))
    }

    // ---- sources --------------------------------------------------------

    pub fn health_sources(&self) -> Result<Vec<HealthSource>> {
        self.with_health(|h| h.list_sources())
    }

    pub fn health_source(&self, id: HealthSourceId) -> Result<HealthSource> {
        self.with_health(|h| h.get_source(id))
    }

    /// Create a source, or rename or pause one. `status` and `token_id` are
    /// never taken from the caller: the first is what arrival learned and the
    /// second is the registry token minted for the phone's push (see
    /// [`Vault::set_health_source_token`]), so a stored source keeps its own
    /// and a new one starts with none. A stored source keeps its connector
    /// and its creation time too: what a connection is does not change under
    /// a rename.
    pub fn save_health_source(&self, source: &HealthSource) -> Result<HealthSource> {
        self.writable()?;
        let name = name_text("a connection's name", &source.name)?;
        let now = Timestamp::now();
        let saved = self.write(|u| {
            let health = pick_domain(u.store.as_ref(), Domain::Health, |s| s.health())?;
            let held = health.list_sources()?.into_iter().find(|s| s.id == source.id);
            let saved = match held {
                Some(held) => HealthSource { name, paused: source.paused, updated_at: now, ..held },
                None => HealthSource {
                    id: source.id,
                    connector: source.connector,
                    name,
                    paused: source.paused,
                    status: SourceStatus::default(),
                    token_id: String::new(),
                    created_at: now,
                    updated_at: now,
                },
            };
            health.put_source(&saved)?;
            Ok(saved)
        })?;
        self.wrote(RecordKind::HealthSource, saved.id);
        Ok(saved)
    }

    /// Record which registry token was minted for `id`'s phone push, so it
    /// can be revoked with it; an empty `token_id` clears it. Writes that one
    /// field and nothing else.
    pub fn set_health_source_token(&self, id: HealthSourceId, token_id: &str) -> Result<()> {
        self.writable()?;
        let token_id = capped(token_id, MAX_NAME);
        self.write(|u| {
            let health = pick_domain(u.store.as_ref(), Domain::Health, |s| s.health())?;
            let mut held = health.get_source(id)?;
            held.token_id = token_id;
            health.put_source(&held)
        })?;
        self.wrote(RecordKind::HealthSource, id);
        Ok(())
    }

    /// Delete the connection. What it brought stays.
    pub fn delete_health_source(&self, id: HealthSourceId) -> Result<()> {
        self.writable()?;
        self.with_health(|h| h.delete_source(id))?;
        self.wrote(RecordKind::HealthSource, id);
        Ok(())
    }

    // ---- ingest ---------------------------------------------------------

    /// The one way health data comes in, whatever connector read it.
    /// Converts units, refuses implausible values, files samples into
    /// series days (deduplicated, summaries rebuilt), upserts workouts by
    /// key, matches dose events to medications, files documents, and
    /// updates the source's status -- one exclusive write per batch.
    /// Refused with `Invalid("paused")` for a paused source.
    ///
    /// A sample is identified by its metric, start, duration and value, not
    /// by who sent it, so the same batch twice, or the same reading from the
    /// phone and from the export, is stored once and counted as a
    /// duplicate. A workout is identified by its activity, start and end: a
    /// second arrival rewrites only what the source measured and leaves the
    /// name, note and activity a person gave it.
    ///
    /// The heart rate a workout carries (`RawWorkout::heart_rate`) is
    /// ignored: Health Auto Export sends it as per-minute aggregates, which
    /// would be counted beside the watch's own raw samples. The workout's
    /// detail reads the raw samples over its span instead.
    ///
    /// What was malformed, or measured something impossible, is counted in
    /// `samples_rejected`; what was not in the catalogue is in `skipped`.
    /// Nothing disappears without being counted.
    pub fn ingest_health(
        &self,
        source: HealthSourceId,
        batch: HealthBatch,
        headers: Option<&PushHeaders>,
    ) -> Result<HealthIngestReport> {
        self.writable()?;
        if batch.samples.len() > MAX_BATCH_SAMPLES {
            return Err(Error::Invalid(format!(
                "a batch can hold at most {MAX_BATCH_SAMPLES} samples; send less at a time"
            )));
        }
        let now = Timestamp::now();
        let (tally, workouts) = self.write(|u| {
            let key = u.cipher.derive_subkey(HEALTH_KEY_LABEL);
            let health = pick_domain(u.store.as_ref(), Domain::Health, |s| s.health())?;
            let mut connection = health.get_source(source)?;
            if connection.paused {
                return Err(Error::Invalid("paused".into()));
            }
            let settings = health.health_settings()?;
            let HealthBatch { samples, workouts, doses, documents, skipped, rejected } = batch;
            let carried = (samples.len() + workouts.len() + doses.len() + documents.len()) as u64;
            let mut tally =
                HealthIngestReport { skipped, samples_rejected: rejected, ..Default::default() };

            let mut intake = Intake::new(&connection.name);
            for raw in &samples {
                intake.take(raw);
            }
            let filing = Filing { store: u.store.as_ref(), health, key: &key, source, now };
            let touched = filing.workouts(workouts, &mut intake, &mut tally)?;
            for dose in &doses {
                intake.note(dose.at);
                intake.device(&dose.source);
            }

            let Intake { by_metric, devices, rejected: refused, first, last, .. } = intake;
            let (added, duplicate) = file_samples(health, &key, &settings, by_metric)?;
            tally.samples_added = added;
            tally.samples_duplicate = duplicate;
            tally.samples_rejected += refused;
            tally.first = first;
            tally.last = last;
            if !doses.is_empty() {
                let (added, unmatched) = file_source_doses(health, &key, &doses)?;
                tally.doses_added = added;
                tally.doses_unmatched = unmatched;
            }
            if !documents.is_empty() {
                tally.documents_added = file_documents(u.store.as_ref(), documents)?;
            }

            let status = &mut connection.status;
            status.last_received_at = Some(now);
            status.newest_sample_at = match (status.newest_sample_at, last) {
                (Some(held), Some(new)) => Some(held.max(new)),
                (held, new) => held.or(new),
            };
            status.samples_total = status.samples_total.saturating_add(added);
            status.last_error.clear();
            for name in devices {
                if status.devices.len() >= MAX_DEVICES {
                    break;
                }
                if !status.devices.contains(&name) {
                    status.devices.push(name);
                }
            }
            if let Some(headers) = headers {
                note_automation(&mut status.automations, headers, carried, now);
            }
            health.put_source(&connection)?;
            Ok((tally, touched))
        })?;
        for id in &workouts {
            self.wrote(RecordKind::Workout, id);
        }
        self.wrote(RecordKind::HealthSource, source);
        Ok(tally)
    }

    /// Note on the source that a push could not be read. The source is not
    /// counted as heard from: nothing arrived.
    pub fn record_health_push_failure(&self, source: HealthSourceId, error: &str) -> Result<()> {
        self.writable()?;
        let error = capped(error, MAX_ERROR);
        self.write(|u| {
            let health = pick_domain(u.store.as_ref(), Domain::Health, |s| s.health())?;
            let mut held = health.get_source(source)?;
            held.status.last_error = error;
            health.put_source(&held)
        })?;
        self.wrote(RecordKind::HealthSource, source);
        Ok(())
    }

    // ---- series ---------------------------------------------------------

    /// A metric over local dates `from..=to` in `tz`, added up into
    /// `bucket`-wide points under `stat` (the metric's own when `Auto`) --
    /// or, for [`BucketSize::Raw`], every sample, which is only for ranges
    /// of at most three days. Ranges are capped at 800 days. `latest` is the
    /// last sample at or before the range's end.
    pub fn health_series(&self, query: &SeriesQuery) -> Result<SeriesResult> {
        let days = range_days(query.from, query.to)?;
        let tz = zone_of(&query.tz)?;
        let metric = query.metric;
        let stat = report::resolve_stat(metric, query.stat);
        let raw = query.bucket == BucketSize::Raw;
        if raw && days > RAW_DAYS {
            return Err(Error::Invalid(format!(
                "every sample can be read for at most {RAW_DAYS} days; ask for a wider bucket"
            )));
        }
        let (start, end) = day_bounds(query.from, query.to, &tz);
        in_series(self, |health, key| {
            let series = series_hash(key, metric);
            let points: Vec<SeriesPoint> = if raw {
                samples_between(health, &series, start, end)?
                    .into_iter()
                    .map(|p| SeriesPoint {
                        at: p.start,
                        value: p.value,
                        min: None,
                        max: None,
                        count: 1,
                    })
                    .collect()
            } else {
                let (first, last) = report::utc_span(query.from, query.to);
                let summaries = health.series_summaries(&series, first, last)?;
                let (from, to) = (query.from, query.to);
                report::bucket_series(metric, &summaries, from, to, &tz, query.bucket, stat)
            };
            let latest = latest_sample(health, &series, end)?;
            Ok(SeriesResult { metric, unit: metric.info().unit.to_string(), stat, points, latest })
        })
    }

    /// Every sample of `metric` in `from..to`, at most 3 days of it.
    pub fn health_samples(
        &self,
        metric: Metric,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<SamplePoint>> {
        check_instants(from, to, i64::from(RAW_DAYS))?;
        in_series(self, |health, key| samples_between(health, &series_hash(key, metric), from, to))
    }

    /// One value per local day under `stat` (`tz` empty is the system's),
    /// only for days with anything in them. A share (`InRange`) is 0 to 1
    /// here; trackers show it as a percentage.
    pub fn health_daily(
        &self,
        metric: Metric,
        stat: Stat,
        from: Date,
        to: Date,
        tz: &str,
    ) -> Result<BTreeMap<Date, f64>> {
        range_days(from, to)?;
        let tz = zone_of(tz)?;
        let (first, last) = report::utc_span(from, to);
        in_series(self, |health, key| {
            let summaries = health.series_summaries(&series_hash(key, metric), first, last)?;
            let stat = report::resolve_stat(metric, stat);
            Ok(report::daily_values(metric, &summaries, from, to, &tz, stat))
        })
    }

    /// The latest sample of each metric at or before the end of `on` (in
    /// this computer's time zone), with the value a month before it for a
    /// trend arrow. Metrics with nothing stored are left out.
    pub fn health_latest(&self, metrics: &[Metric], on: Date) -> Result<Vec<Latest>> {
        let (_, end) = day_bounds(on, on, &TimeZone::system());
        latest_before(self, metrics, end)
    }

    /// A reading typed in here: filed under the source "Karma Yoga". A
    /// blood pressure is typed as one reading and makes two samples, the
    /// diastolic from `second`. A value no such reading could have is
    /// refused rather than filed, since a person is there to correct it.
    pub fn add_health_reading(&self, reading: &ManualReading) -> Result<()> {
        self.writable()?;
        let tz = zone_of(&reading.tz)?;
        let offset_min = i16::try_from(tz.to_offset(reading.at).seconds() / 60).unwrap_or(0);
        let taken = |metric: Metric, value: f64| RawSample {
            metric,
            start: reading.at,
            end: reading.at,
            value,
            unit: reading.unit.clone(),
            source: MANUAL_SOURCE.to_string(),
            offset_min,
        };
        let mut samples = vec![taken(reading.metric, reading.value)];
        match (reading.metric, reading.second) {
            (Metric::BloodPressureSystolic, Some(second)) => {
                samples.push(taken(Metric::BloodPressureDiastolic, second));
            }
            (_, Some(_)) => {
                return Err(Error::Invalid(
                    "a second value belongs only to a blood pressure".into(),
                ));
            }
            (_, None) => {}
        }
        let mut intake = Intake::new(MANUAL_SOURCE);
        if !samples.iter().all(|s| intake.take(s)) {
            return Err(Error::Invalid(
                "that reading is outside what can be measured; check its unit".into(),
            ));
        }
        self.write(|u| {
            let key = u.cipher.derive_subkey(HEALTH_KEY_LABEL);
            let health = pick_domain(u.store.as_ref(), Domain::Health, |s| s.health())?;
            let settings = health.health_settings()?;
            file_samples(health, &key, &settings, intake.by_metric)?;
            Ok(())
        })
    }

    /// Delete `metric`'s samples starting in `from..to` (only `source`'s
    /// when given). Answers how many went. [`remove`] leaves a tombstone for
    /// each sample a device or app brought, so that the same reading arriving
    /// again (a "Previous 7 Days" push overlaps) is not filed again; a day is
    /// therefore kept while it has any, and is deleted only when it has
    /// neither samples nor tombstones. Every day kept has its summary
    /// rebuilt.
    pub fn delete_health_samples(
        &self,
        metric: Metric,
        from: Timestamp,
        to: Timestamp,
        source: Option<&str>,
    ) -> Result<u64> {
        self.writable()?;
        check_instants(from, to, MAX_RANGE_DAYS)?;
        if to == from {
            return Ok(0);
        }
        let source = source.map(str::trim);
        self.write(|u| {
            let key = u.cipher.derive_subkey(HEALTH_KEY_LABEL);
            let health = pick_domain(u.store.as_ref(), Domain::Health, |s| s.health())?;
            let settings = health.health_settings()?;
            let rank = ranker(&settings, metric);
            let series = series_hash(&key, metric);
            let first = utc_day(from.as_second());
            let last = utc_day(to.as_second() - 1);
            let mut rows = Vec::new();
            let mut emptied = Vec::new();
            let mut removed = 0u64;
            for mut held in health.series_days(&series, first, last)? {
                let Some(day) = held.day else { continue };
                let gone = remove(&mut held, from.as_second(), to.as_second(), source);
                if gone == 0 {
                    continue;
                }
                removed += gone;
                // A day with no samples left but tombstones keeps its row: the
                // tombstones are what stop the next push bringing them back.
                if held.samples.is_empty() && held.tombstones.is_empty() {
                    emptied.push(day);
                } else {
                    let summary = summarize(&held, metric, &*rank);
                    rows.push(SeriesRow { series: series.clone(), day, data: held, summary });
                }
            }
            if !rows.is_empty() {
                health.put_series(&rows)?;
            }
            for day in emptied {
                health.delete_series(&series, day, day)?;
            }
            Ok(removed)
        })
    }

    /// Every metric with anything stored: its first and last day and how
    /// many days, in catalogue order. For the export and the Vitals list.
    pub fn health_extents(&self) -> Result<Vec<(Metric, Date, Date, u64)>> {
        in_series(self, |health, key| {
            let held = health.series_extents()?;
            // The store knows series by hash only; hashing every metric is
            // how a hash is turned back into one.
            Ok(Metric::ALL
                .iter()
                .filter_map(|&metric| {
                    let series = series_hash(key, metric);
                    held.iter()
                        .find(|e| e.series == series)
                        .map(|e| (metric, e.first, e.last, e.days))
                })
                .collect())
        })
    }

    // ---- reports --------------------------------------------------------

    pub fn health_activity(&self, from: Date, to: Date, tz: &str) -> Result<Vec<ActivityDay>> {
        range_days(from, to)?;
        activity_between(self, from, to, &zone_of(tz)?)
    }

    /// Nights ending on local dates `from..=to`, each with its score.
    pub fn health_sleep(&self, from: Date, to: Date, tz: &str) -> Result<Vec<SleepNight>> {
        range_days(from, to)?;
        let settings = self.health_settings()?;
        sleep_between(self, from, to, &zone_of(tz)?, &settings)
    }

    pub fn health_glucose(&self, from: Date, to: Date, tz: &str) -> Result<GlucoseReport> {
        range_days(from, to)?;
        glucose_between(self, from, to, &zone_of(tz)?)
    }

    /// Everything one day's overlay draws: glucose against meals, doses,
    /// insulin, workouts and sleep. Insulin is basal and bolus together, in
    /// time order.
    pub fn health_day_timeline(&self, date: Date, tz: &str) -> Result<DayTimeline> {
        let tz = zone_of(tz)?;
        let settings = self.health_settings()?;
        let (start, end) = day_bounds(date, date, &tz);
        let (first, last) = report::utc_span(date, date);
        let rank = ranker(&settings, Metric::SleepStage);
        in_series(self, |health, key| {
            let between =
                |metric: Metric| samples_between(health, &series_hash(key, metric), start, end);
            let glucose = between(Metric::BloodGlucose)?;
            let mut insulin = between(Metric::InsulinBasal)?;
            insulin.extend(between(Metric::InsulinBolus)?);
            insulin.sort_by_key(|p| p.start);
            let heart = series_hash(key, Metric::HeartRate);
            let summaries = health.series_summaries(&heart, first, last)?;
            let heart_rate = report::bucket_series(
                Metric::HeartRate,
                &summaries,
                date,
                date,
                &tz,
                BucketSize::QuarterHour,
                Stat::Mean,
            );
            // Last night's stages begin the evening before.
            let stages = health.series_days(
                &series_hash(key, Metric::SleepStage),
                report::utc_span(minus_days(date, 1), date).0,
                last,
            )?;
            let sleep = sleep::segments(&stages, &*rank)
                .into_iter()
                .filter(|s| s.end > start && s.start < end)
                .collect();
            Ok(DayTimeline {
                date,
                glucose,
                heart_rate,
                meals: health.meals(start, end)?,
                doses: super::care::shown(health.doses(start, end, None)?),
                insulin,
                workouts: shown(health.workouts(start, end)?),
                sleep,
            })
        })
    }

    /// The Today screen. A care method that answers `Unsupported` (a
    /// backend without medications, say) leaves its part empty rather than
    /// failing the whole screen.
    ///
    /// A source is stale when it is a Health Auto Export connection that is
    /// not paused, has been heard from, and has not been for
    /// `stale_hours`; 0 never warns. A connection never heard from is not
    /// stale, only not set up yet. Tests coming due are those of conditions
    /// not resolved, overdue or due within 30 days, each test once.
    pub fn health_today(&self, date: Date, tz: &str) -> Result<HealthDay> {
        let zone = zone_of(tz)?;
        let settings = self.health_settings()?;
        let (start, end) = day_bounds(date, date, &zone);

        let activity = activity_between(self, date, date, &zone)?
            .into_iter()
            .find(|d| d.date == date)
            .unwrap_or_else(|| empty_activity(date));
        let night =
            sleep_between(self, date, date, &zone, &settings)?.into_iter().find(|n| n.date == date);
        let latest = latest_before(self, TODAY_METRICS, end)?;
        let nutrition = or_nothing(self.health_nutrition(date, date, tz))?
            .into_iter()
            .find(|n| n.date == date)
            .unwrap_or_else(|| empty_nutrition(date));
        let workouts = self.with_health(|h| h.workouts(start, end).map(shown))?;
        let glucose = Some(glucose_between(self, date, date, &zone)?).filter(|g| g.readings > 0);

        let conditions = or_nothing(self.list_conditions())?;
        let labs_due = if conditions.is_empty() {
            Vec::new()
        } else {
            let labs = or_nothing(self.list_labs(None))?;
            let medications = or_nothing(self.list_medications())?;
            due_soon(&conditions, &labs, &medications, date)
        };

        let stale_sources: Vec<HealthSource> = if settings.stale_hours > 0 {
            let cutoff = Timestamp::now()
                .checked_sub(SignedDuration::from_hours(i64::from(settings.stale_hours)))
                .unwrap_or(Timestamp::MIN);
            self.health_sources()?
                .into_iter()
                .filter(|s| s.connector == Connector::HealthAutoExport && !s.paused)
                .filter(|s| s.status.last_received_at.is_some_and(|heard| heard < cutoff))
                .collect()
        } else {
            Vec::new()
        };

        Ok(HealthDay {
            date,
            activity,
            goals: settings.goals,
            sleep: night,
            latest,
            doses_due: or_nothing(self.doses_due(date, tz))?,
            nutrition,
            workouts,
            glucose,
            alerts: or_nothing(self.medication_alerts(date, date, tz))?,
            labs_due,
            stale_sources,
        })
    }

    // ---- workouts -------------------------------------------------------

    /// The workouts overlapping `from..to`, in start order, without any a
    /// person deleted.
    pub fn list_workouts(&self, from: Timestamp, to: Timestamp) -> Result<Vec<Workout>> {
        check_instants(from, to, MAX_RANGE_DAYS)?;
        self.with_health(|h| h.workouts(from, to).map(shown))
    }

    /// A workout with the heart rate sampled while it lasted and its route.
    /// A route that cannot be read -- its blob swept, or damaged -- leaves
    /// the map empty rather than failing the screen.
    pub fn workout_detail(&self, id: WorkoutId) -> Result<WorkoutDetail> {
        self.read(|u| {
            let key = u.cipher.derive_subkey(HEALTH_KEY_LABEL);
            let health = pick_domain(u.store.as_ref(), Domain::Health, |s| s.health())?;
            let workout = health.get_workout(id)?;
            let series = series_hash(&key, Metric::HeartRate);
            let heart_rate = samples_between(health, &series, workout.start, workout.end)?;
            let route = workout
                .route
                .and_then(|blob| u.store.get_blob(blob).ok())
                .and_then(|bytes| serde_json::from_slice::<Vec<RoutePoint>>(&bytes).ok())
                .unwrap_or_default();
            Ok(WorkoutDetail { workout, heart_rate, route })
        })
    }

    /// A workout typed in, or an edit (name, note, activity). One a source
    /// brought keeps everything the source measured -- its times, numbers,
    /// route and key -- so the next arrival of it still finds it, and takes
    /// only the name, note and activity. One typed in here may change any of
    /// its numbers; two of those cannot share an activity, start and end.
    pub fn save_workout(&self, workout: &Workout) -> Result<Workout> {
        self.writable()?;
        let name = person_text("a workout's name", &workout.name, MAX_NAME)?;
        let note = person_text("a note", &workout.note, MAX_NOTE)?;
        if workout.end < workout.start {
            return Err(Error::Invalid("a workout cannot end before it starts".into()));
        }
        let measures = [
            workout.active_energy_kcal,
            workout.total_energy_kcal,
            workout.distance_m,
            workout.elevation_up_m,
            workout.avg_hr,
            workout.max_hr,
            workout.effort,
        ];
        if measures.into_iter().flatten().any(|v| !(v.is_finite() && v >= 0.0)) {
            return Err(Error::Invalid("a workout's numbers cannot be negative".into()));
        }
        let now = Timestamp::now();
        let saved = self.write(|u| {
            let key = u.cipher.derive_subkey(HEALTH_KEY_LABEL);
            let health = pick_domain(u.store.as_ref(), Domain::Health, |s| s.health())?;
            let held = match health.get_workout(workout.id) {
                Ok(held) => Some(held),
                Err(Error::NotFound { .. }) => None,
                Err(e) => return Err(e),
            };
            let typed = |created_at: Timestamp, route: Option<BlobId>| Workout {
                id: workout.id,
                activity: workout.activity,
                name: name.clone(),
                start: workout.start,
                end: workout.end,
                offset_min: workout.offset_min,
                active_energy_kcal: workout.active_energy_kcal,
                total_energy_kcal: workout.total_energy_kcal,
                distance_m: workout.distance_m,
                elevation_up_m: workout.elevation_up_m,
                avg_hr: workout.avg_hr,
                max_hr: workout.max_hr,
                effort: workout.effort,
                indoor: workout.indoor,
                source: MANUAL_SOURCE.to_string(),
                source_id: None,
                external_id: String::new(),
                route,
                note: note.clone(),
                key: workout_key(&key, workout.activity, workout.start, workout.end),
                hidden: false,
                created_at,
                updated_at: now,
            };
            let saved = match held {
                Some(held) if held.source_id.is_some() => Workout {
                    activity: workout.activity,
                    name: name.clone(),
                    note: note.clone(),
                    updated_at: now,
                    ..held
                },
                Some(held) => typed(held.created_at, held.route),
                None => typed(now, None),
            };
            if saved.source_id.is_none()
                && let Some(other) = health.workout_by_key(&saved.key)?
                && other.id != saved.id
            {
                return Err(Error::Invalid("a workout at those times is already recorded".into()));
            }
            health.put_workouts(std::slice::from_ref(&saved))?;
            Ok(saved)
        })?;
        self.wrote(RecordKind::Workout, saved.id);
        Ok(saved)
    }

    /// Delete a workout. One a source brought is only hidden: its row stays,
    /// so that the same workout arriving again still matches by key and is not
    /// brought back (the watch never says it was deleted). One typed in here
    /// goes for real.
    pub fn delete_workout(&self, id: WorkoutId) -> Result<()> {
        self.writable()?;
        let now = Timestamp::now();
        self.write(|u| {
            let health = pick_domain(u.store.as_ref(), Domain::Health, |s| s.health())?;
            let held = health.get_workout(id)?;
            if held.source_id.is_none() && held.external_id.is_empty() {
                return health.delete_workout(id);
            }
            if held.hidden {
                return Ok(());
            }
            health.put_workouts(&[Workout { hidden: true, updated_at: now, ..held }])
        })?;
        self.wrote(RecordKind::Workout, id);
        Ok(())
    }

    // ---- keys -----------------------------------------------------------

    /// Whether a food-lookup key is set, and its last four characters when
    /// it is long enough to show them safely. Never the key.
    pub fn health_keys_info(&self) -> Result<HealthKeysInfo> {
        Ok(match self.read(|u| read_keys(u.store.as_ref()))? {
            Some(keys) if !keys.fdc_key.is_empty() => {
                HealthKeysInfo { fdc_key_set: true, fdc_key_hint: hint(&keys.fdc_key) }
            }
            _ => HealthKeysInfo::default(),
        })
    }

    /// Store the vault's food-lookup key, replacing any before. A blank one
    /// is the same as none, since an empty key means the shared `DEMO_KEY`.
    pub fn save_health_keys(&self, keys: &HealthKeys) -> Result<()> {
        self.writable()?;
        let fdc_key = keys.fdc_key.trim();
        if fdc_key.is_empty() {
            return self.clear_health_keys();
        }
        if fdc_key.len() > MAX_KEY || !fdc_key.chars().all(|c| c.is_ascii_graphic()) {
            return Err(Error::Invalid("that does not look like a FoodData Central key".into()));
        }
        let bytes = serde_json::to_vec(&HealthKeys { fdc_key: fdc_key.to_string() })
            .map_err(|_| Error::Invalid("the key could not be sealed".into()))?;
        secrets_for_health(self, |s| s.put_secret(HEALTH_KEYS_OWNER_KIND, KEYS_OWNER_ID, &bytes))
    }

    pub fn clear_health_keys(&self) -> Result<()> {
        self.writable()?;
        secrets_for_health(self, |s| s.delete_secret(HEALTH_KEYS_OWNER_KIND, KEYS_OWNER_ID))
    }

    /// The keys themselves -- only for code about to call a food lookup.
    /// Never fails for want of one: with none set the key is empty, which a
    /// lookup reads as `DEMO_KEY`.
    pub fn health_keys_access(&self) -> Result<HealthKeys> {
        Ok(self.read(|u| read_keys(u.store.as_ref()))?.unwrap_or_default())
    }

    // ---- trackers -------------------------------------------------------

    /// Readings for trackers with a [`TrackerSource::Health`] source: each
    /// metric's day under the tracker's stat, an `InRange` share as a
    /// percentage.
    ///
    /// Counted in this computer's time zone, as every derived reading is,
    /// and only over the last 800 days up to today: a tracker may ask about
    /// a window open at both ends, and there is nothing before that to read.
    pub(crate) fn health_tracker_days(
        &self,
        trackers: &[&Tracker],
        window: PurposeWindow,
    ) -> Result<Vec<TrackerDay>> {
        let tz = TimeZone::system();
        let today = Timestamp::now().to_zoned(tz.clone()).date();
        let to = window.to.min(today);
        let from = window.from.max(minus_days(to, MAX_RANGE_DAYS - 1));
        if from > to {
            return Ok(Vec::new());
        }
        let (first, last) = report::utc_span(from, to);
        in_series(self, |health, key| {
            let mut held: HashMap<Metric, Vec<SeriesSummary>> = HashMap::new();
            let mut out = Vec::new();
            for tracker in trackers {
                let TrackerSource::Health { metric, stat } = &tracker.source else { continue };
                let stat = report::resolve_stat(*metric, *stat);
                if !held.contains_key(metric) {
                    let series = series_hash(key, *metric);
                    held.insert(*metric, health.series_summaries(&series, first, last)?);
                }
                let share = matches!(stat, Stat::InRange { .. });
                let daily = report::daily_values(*metric, &held[metric], from, to, &tz, stat);
                for (date, value) in daily {
                    let value = if share { value * 100.0 } else { value };
                    if value.is_finite() {
                        out.push(TrackerDay {
                            tracker_id: tracker.id,
                            date,
                            count: 1,
                            sum: value,
                            max: value,
                            first_at: None,
                            last_at: None,
                        });
                    }
                }
            }
            Ok(out)
        })
    }
}

// ---- reading ---------------------------------------------------------------

/// Run `f` against the health store and this vault's series key under one
/// read.
fn in_series<T>(
    vault: &Vault,
    f: impl FnOnce(&dyn HealthStore, &SecretKey) -> Result<T>,
) -> Result<T> {
    vault.read(|u| {
        let key = u.cipher.derive_subkey(HEALTH_KEY_LABEL);
        let health = pick_domain(u.store.as_ref(), Domain::Health, |s| s.health())?;
        f(health, &key)
    })
}

fn secrets_for_health<T>(
    vault: &Vault,
    f: impl FnOnce(&dyn SecretStore) -> Result<T>,
) -> Result<T> {
    vault.with_domain(Domain::Secrets, |s| s.secrets().map(f))
}

/// Activity days `from..=to` from each activity metric's day and the
/// workouts in the range.
fn activity_between(
    vault: &Vault,
    from: Date,
    to: Date,
    tz: &TimeZone,
) -> Result<Vec<ActivityDay>> {
    let (start, end) = day_bounds(from, to, tz);
    let (first, last) = report::utc_span(from, to);
    in_series(vault, |health, key| {
        let mut daily = BTreeMap::new();
        for &metric in ACTIVITY_METRICS {
            let summaries = health.series_summaries(&series_hash(key, metric), first, last)?;
            let stat = report::resolve_stat(metric, Stat::Auto);
            daily.insert(metric, report::daily_values(metric, &summaries, from, to, tz, stat));
        }
        let workouts = shown(health.workouts(start, end)?);
        Ok(report::activity_days(&daily, &workouts, from, to, tz))
    })
}

/// Nights ending on `from..=to`. Seven nights before `from` are read too,
/// so every night returned has the nights before it to be scored for
/// regularity against; the score's `recent` is those nights, not including
/// the one being scored. Only the nights returned have their heart rate,
/// HRV and breathing read, which are the large reads.
fn sleep_between(
    vault: &Vault,
    from: Date,
    to: Date,
    tz: &TimeZone,
    settings: &HealthSettings,
) -> Result<Vec<SleepNight>> {
    let early = minus_days(from, SCORE_NIGHTS as i64);
    let rank = ranker(settings, Metric::SleepStage);
    in_series(vault, |health, key| {
        // A night ending on `early` began the evening before it.
        let (first, last) = report::utc_span(minus_days(early, 1), to);
        let stages = health.series_days(&series_hash(key, Metric::SleepStage), first, last)?;
        let segments = sleep::segments(&stages, &*rank);
        let mut nights = sleep::nights(&segments, early, to, tz);
        for i in 0..nights.len() {
            let recent = &nights[i.saturating_sub(SCORE_NIGHTS)..i];
            let (value, parts) = sleep::score(&nights[i], settings.goals.sleep_hours, recent);
            if !parts.is_empty() {
                nights[i].score = Some(value);
                nights[i].score_parts = parts;
            }
        }
        nights.retain(|n| n.date >= from);
        for night in &mut nights {
            let (Some(start), Some(end)) = (night.sleep_start, night.sleep_end) else { continue };
            let read =
                |metric: Metric| samples_between(health, &series_hash(key, metric), start, end);
            let heart_rate = read(Metric::HeartRate)?;
            let hrv = read(Metric::HeartRateVariability)?;
            let breathing = read(Metric::RespiratoryRate)?;
            sleep::enrich(night, &heart_rate, &hrv, &breathing);
        }
        Ok(nights)
    })
}

fn glucose_between(vault: &Vault, from: Date, to: Date, tz: &TimeZone) -> Result<GlucoseReport> {
    let (start, end) = day_bounds(from, to, tz);
    let readings = in_series(vault, |health, key| {
        samples_between(health, &series_hash(key, Metric::BloodGlucose), start, end)
    })?;
    Ok(glucose::report(&readings, from, to, tz))
}

/// The latest sample of each metric starting before `end`, with the value a
/// month before it.
fn latest_before(vault: &Vault, metrics: &[Metric], end: Timestamp) -> Result<Vec<Latest>> {
    in_series(vault, |health, key| {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for &metric in metrics {
            if !seen.insert(metric) {
                continue;
            }
            let series = series_hash(key, metric);
            let Some(point) = latest_sample(health, &series, end)? else { continue };
            let back = point
                .start
                .checked_sub(SignedDuration::from_hours(TREND_DAYS * 24))
                .unwrap_or(Timestamp::MIN);
            let previous = latest_sample(health, &series, back)?.map(|p| p.value);
            out.push(Latest {
                metric,
                value: point.value,
                at: point.start,
                source: point.source,
                previous,
            });
        }
        Ok(out)
    })
}

/// Every sample of a series in `from..to`, looking back as far as a sample
/// may last before `from` for one that began earlier and was still going.
fn samples_between(
    health: &dyn HealthStore,
    series: &str,
    from: Timestamp,
    to: Timestamp,
) -> Result<Vec<SamplePoint>> {
    if to <= from {
        return Ok(Vec::new());
    }
    let first = utc_day(from.as_second().saturating_sub(MAX_SAMPLE_SECS));
    let last = utc_day(to.as_second().saturating_sub(1));
    let held = health.series_days(series, first, last)?;
    Ok(report::raw_points(&held, from, to))
}

/// The last sample of a series that starts before `end`.
fn latest_sample(
    health: &dyn HealthStore,
    series: &str,
    end: Timestamp,
) -> Result<Option<SamplePoint>> {
    let mut on = utc_day(end.as_second().saturating_sub(1));
    // The day `end` falls in can hold samples that start after it, and a day
    // can be left with only tombstones, so the answer may be earlier. Each
    // step goes back at least a day, so this ends.
    loop {
        let Some(held) = health.latest_series_day(series, on)? else { return Ok(None) };
        if let Some(found) = points(&held).into_iter().rev().find(|p| p.start < end) {
            return Ok(Some(found));
        }
        let Some(before) = held.day.map(|d| minus_days(d, 1)) else { return Ok(None) };
        on = before;
    }
}

/// A care method's answer, or nothing if this backend does not keep care
/// records: Today draws without them.
fn or_nothing<T: Default>(answer: Result<T>) -> Result<T> {
    match answer {
        Err(Error::Unsupported(_)) => Ok(T::default()),
        other => other,
    }
}

/// The workouts a person has not deleted.
fn shown(mut workouts: Vec<Workout>) -> Vec<Workout> {
    workouts.retain(|w| !w.hidden);
    workouts
}

fn empty_activity(date: Date) -> ActivityDay {
    ActivityDay {
        date,
        active_kcal: 0.0,
        exercise_min: 0.0,
        stand_hours: 0.0,
        steps: 0.0,
        distance_m: 0.0,
        flights: 0.0,
        resting_hr: None,
        hrv_ms: None,
        workouts: 0,
    }
}

fn empty_nutrition(date: Date) -> NutritionDay {
    NutritionDay { date, logged: Nutrients::default(), imported: Nutrients::default(), meals: 0 }
}

/// The tests of `conditions` (those not resolved) that are overdue or due
/// by `today` plus 30 days, soonest first, each test once.
fn due_soon(
    conditions: &[Condition],
    labs: &[LabResult],
    medications: &[Medication],
    today: Date,
) -> Vec<LabDue> {
    let horizon = plus_days(today, LABS_HORIZON_DAYS);
    let mut due: Vec<LabDue> = conditions
        .iter()
        .filter(|c| c.status != ConditionStatus::Resolved)
        .flat_map(|c| templates::labs_due(&c.plan, labs, medications, today))
        .filter(|l| l.overdue || l.due.is_some_and(|d| d <= horizon))
        .collect();
    due.sort_by_key(|l| (l.due.is_none(), l.due));
    let mut seen = HashSet::new();
    due.retain(|l| seen.insert(l.schedule.code.clone()));
    due
}

// ---- dates and zones --------------------------------------------------------

/// A time zone by IANA name; empty is the system's. `Error::Invalid` for a
/// name that is not one.
fn zone_of(tz: &str) -> Result<TimeZone> {
    let tz = tz.trim();
    if tz.is_empty() {
        return Ok(TimeZone::system());
    }
    TimeZone::get(tz).map_err(|_| {
        Error::Invalid(format!("{tz:?} is not a time zone; name one like Europe/London"))
    })
}

/// How many days `from..=to` is, refusing a range that runs backwards or is
/// longer than [`MAX_RANGE_DAYS`].
fn range_days(from: Date, to: Date) -> Result<i32> {
    if from > to {
        return Err(Error::Invalid("a range's last day cannot be before its first".into()));
    }
    let days = from.until(to).map(|s| s.get_days()).unwrap_or(i32::MAX).saturating_add(1);
    if i64::from(days) > MAX_RANGE_DAYS {
        return Err(Error::Invalid(format!("a range can cover at most {MAX_RANGE_DAYS} days")));
    }
    Ok(days)
}

/// Refuse instants that run backwards or are more than `max_days` apart.
fn check_instants(from: Timestamp, to: Timestamp, max_days: i64) -> Result<()> {
    if to < from {
        return Err(Error::Invalid("a range's end cannot be before its start".into()));
    }
    if to.as_second().saturating_sub(from.as_second()) > max_days * 86_400 {
        return Err(Error::Invalid(format!("a range can cover at most {max_days} days")));
    }
    Ok(())
}

/// The instants `from` begins and the day after `to` begins, in `tz`.
///
/// A day begins at its first instant, which is midnight except in the few
/// zones whose clocks spring forward at midnight, where it is one o'clock.
fn day_bounds(from: Date, to: Date, tz: &TimeZone) -> (Timestamp, Timestamp) {
    let start = day_start(from, tz).unwrap_or(Timestamp::MIN);
    let end = to.tomorrow().ok().and_then(|d| day_start(d, tz)).unwrap_or(Timestamp::MAX);
    (start, end)
}

fn day_start(date: Date, tz: &TimeZone) -> Option<Timestamp> {
    tz.to_timestamp(date.at(0, 0, 0, 0)).ok()
}

fn minus_days(date: Date, days: i64) -> Date {
    date.checked_sub(Span::new().days(days)).unwrap_or(date)
}

fn plus_days(date: Date, days: i64) -> Date {
    date.checked_add(Span::new().days(days)).unwrap_or(date)
}

// ---- keyed hashes -----------------------------------------------------------

/// The keyed hash of `identity` under an already-derived health key: 32
/// lowercase hex characters, the same shape as Money's dedupe keys.
fn keyed(key: &SecretKey, identity: &str) -> String {
    blake3::keyed_hash(key.expose(), identity.as_bytes()).to_hex()[..32].to_string()
}

/// What a series is called in the clear. Never the metric's name: "this
/// vault measures glucose" is itself a diagnosis.
fn series_hash(key: &SecretKey, metric: Metric) -> String {
    keyed(key, metric.key())
}

/// A workout's dedupe key: what makes it that workout whoever reports it.
fn workout_key(key: &SecretKey, activity: Activity, start: Timestamp, end: Timestamp) -> String {
    // The snake_case word serde writes for the activity, as a metric's key
    // is: it is a crypto input, so it must never change for a variant.
    let word = serde_json::to_value(activity)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "other".to_string());
    keyed(key, &format!("{word}|{}|{}", start.as_second(), end.as_second()))
}

// ---- filing samples ---------------------------------------------------------

/// Samples on their way in: made canonical, checked, grouped by metric, with
/// a tally of what was refused, who sent them, and when.
struct Intake {
    /// The name a sample with none is filed under: the connection's own.
    fallback: String,
    by_metric: BTreeMap<Metric, Vec<Incoming>>,
    devices: BTreeSet<String>,
    rejected: u64,
    first: Option<Timestamp>,
    last: Option<Timestamp>,
}

impl Intake {
    fn new(fallback: &str) -> Self {
        Self {
            fallback: capped(fallback, MAX_NAME),
            by_metric: BTreeMap::new(),
            devices: BTreeSet::new(),
            rejected: 0,
            first: None,
            last: None,
        }
    }

    /// Take one sample, or count it rejected. True when taken.
    fn take(&mut self, raw: &RawSample) -> bool {
        let Some((value, secs)) = canonical(raw) else {
            self.rejected += 1;
            return false;
        };
        let source = match capped(&raw.source, MAX_NAME) {
            name if name.is_empty() => self.fallback.clone(),
            name => name,
        };
        self.note(raw.start);
        self.note(raw.end);
        self.devices.insert(source.clone());
        self.by_metric.entry(raw.metric).or_default().push(Incoming {
            start: raw.start.as_second(),
            secs,
            value,
            source,
            offset_min: raw.offset_min,
        });
        true
    }

    /// Something arrived that happened at `at`.
    fn note(&mut self, at: Timestamp) {
        self.first = Some(self.first.map_or(at, |first| first.min(at)));
        self.last = Some(self.last.map_or(at, |last| last.max(at)));
    }

    /// A device or app named something it sent besides samples.
    fn device(&mut self, name: &str) {
        let name = capped(name, MAX_NAME);
        if !name.is_empty() {
            self.devices.insert(name);
        }
    }
}

/// A sample's value in its metric's canonical unit and its length in
/// seconds, or `None` if it is not one the vault will keep: a unit the
/// metric cannot be in, a value outside what the metric can plausibly be
/// (a sensor glitch or a unit mix-up), an end before its start, or a
/// duration past [`MAX_SAMPLE_SECS`].
fn canonical(raw: &RawSample) -> Option<(f64, u32)> {
    let info = raw.metric.info();
    let value = units::to_canonical(raw.metric, raw.value, &raw.unit)?;
    if !value.is_finite() || value < info.min || value > info.max || raw.end < raw.start {
        return None;
    }
    let secs = raw.end.as_second().checked_sub(raw.start.as_second())?;
    if secs > MAX_SAMPLE_SECS {
        return None;
    }
    Some((value, u32::try_from(secs).ok()?))
}

/// Merge `incoming` into its series days -- each loaded, or new -- rebuild
/// the summary of every day that gained a sample, and write those days in
/// one batch or not at all. Answers (added, duplicate). A day that gained
/// nothing is not rewritten, so a repeated import costs reads only.
fn file_samples(
    health: &dyn HealthStore,
    key: &SecretKey,
    settings: &HealthSettings,
    incoming: BTreeMap<Metric, Vec<Incoming>>,
) -> Result<(u64, u64)> {
    let mut rows = Vec::new();
    let (mut added, mut duplicate) = (0u64, 0u64);
    for (metric, samples) in incoming {
        let series = series_hash(key, metric);
        let rank = ranker(settings, metric);
        for (day, samples) in by_day(samples) {
            let mut held =
                health.series_days(&series, day, day)?.into_iter().next().unwrap_or_default();
            held.metric = Some(metric);
            held.day = Some(day);
            let outcome = merge(&mut held, &samples);
            added += outcome.added;
            duplicate += outcome.duplicate;
            // Written when anything was added, and when nothing was but a
            // held instant was given its interval: the summary's quarter
            // hours are spread by it.
            if outcome.added > 0 || outcome.lengthened {
                let summary = summarize(&held, metric, &*rank);
                rows.push(SeriesRow { series: series.clone(), day, data: held, summary });
            }
        }
    }
    if !rows.is_empty() {
        health.put_series(&rows)?;
    }
    Ok((added, duplicate))
}

// ---- filing workouts --------------------------------------------------------

/// What an ingest holds while it files workouts: the stores, the key, who
/// sent them and when.
struct Filing<'a> {
    store: &'a dyn JournalStore,
    health: &'a dyn HealthStore,
    key: &'a SecretKey,
    source: HealthSourceId,
    now: Timestamp,
}

impl Filing<'_> {
    /// Upsert workouts by key. A workout already held has the source's
    /// fields rewritten (and is counted updated only if one changed); a new
    /// one gets a fresh id; one a person deleted (`hidden`) is left as it is.
    /// The heart rate a workout carries is ignored -- see
    /// [`Vault::ingest_health`] -- and its times and source join the
    /// intake's tally. A workout whose end is before its start is counted in
    /// `skipped`. A route is sealed into the blob store only for a workout
    /// that has none yet, so a re-push never rewrites a blob. Answers the ids
    /// written.
    fn workouts(
        &self,
        workouts: Vec<RawWorkout>,
        intake: &mut Intake,
        tally: &mut HealthIngestReport,
    ) -> Result<Vec<WorkoutId>> {
        let mut rows: Vec<Workout> = Vec::new();
        // Where in `rows` each key is, for a workout sent twice in one batch.
        let mut at: HashMap<String, usize> = HashMap::new();
        for raw in workouts {
            if raw.end < raw.start {
                let what = "a workout that ends before it starts".to_string();
                *tally.skipped.entry(what).or_default() += 1;
                continue;
            }
            intake.note(raw.start);
            intake.note(raw.end);
            intake.device(&raw.source);
            let dedupe = workout_key(self.key, raw.activity, raw.start, raw.end);
            if let Some(&i) = at.get(&dedupe) {
                let route = self.route_if(rows[i].route.is_none(), &raw.route)?;
                source_fields(&mut rows[i], &raw, route, self.source);
                continue;
            }
            match self.health.workout_by_key(&dedupe)? {
                // Deleted by a person: the row stays so that this key still
                // matches, and the source does not bring it back.
                Some(held) if held.hidden => {}
                Some(held) => {
                    let route = self.route_if(held.route.is_none(), &raw.route)?;
                    let mut changed = held.clone();
                    source_fields(&mut changed, &raw, route, self.source);
                    if changed != held {
                        changed.updated_at = self.now;
                        at.insert(dedupe, rows.len());
                        rows.push(changed);
                        tally.workouts_updated += 1;
                    }
                }
                None => {
                    let route = self.route(&raw.route)?;
                    let mut fresh = Workout {
                        id: WorkoutId::new(),
                        activity: raw.activity,
                        name: capped(&raw.name, MAX_NAME),
                        start: raw.start,
                        end: raw.end,
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
                        source_id: Some(self.source),
                        external_id: String::new(),
                        route: None,
                        note: String::new(),
                        key: dedupe.clone(),
                        hidden: false,
                        created_at: self.now,
                        updated_at: self.now,
                    };
                    source_fields(&mut fresh, &raw, route, self.source);
                    at.insert(dedupe, rows.len());
                    rows.push(fresh);
                    tally.workouts_added += 1;
                }
            }
        }
        if !rows.is_empty() {
            self.health.put_workouts(&rows)?;
        }
        Ok(rows.into_iter().map(|w| w.id).collect())
    }

    /// [`Filing::route`], but only when `wanted`: a workout that already has
    /// its route is not sealed again.
    fn route_if(&self, wanted: bool, route: &[RoutePoint]) -> Result<Option<BlobId>> {
        if wanted { self.route(route) } else { Ok(None) }
    }

    /// A route's points sealed as one blob, as JSON; `None` for a route
    /// with no usable point. A point off the globe is dropped, since the
    /// route is read back as numbers and a non-finite one would not be.
    fn route(&self, route: &[RoutePoint]) -> Result<Option<BlobId>> {
        let points: Vec<RoutePoint> = route
            .iter()
            .filter(|p| {
                p.lat.is_finite()
                    && p.lon.is_finite()
                    && (-90.0..=90.0).contains(&p.lat)
                    && (-180.0..=180.0).contains(&p.lon)
            })
            .map(|p| RoutePoint { alt: p.alt.filter(|a| a.is_finite()), ..*p })
            .collect();
        if points.is_empty() {
            return Ok(None);
        }
        self.store.put_blob(&serde_json::to_vec(&points)?).map(Some)
    }
}

/// Take from `raw` what a source measured. A figure the source did not send
/// this time is kept as it was, so a push without a route does not unmake
/// the route an export brought; a negative or non-finite one is not a
/// measurement. The name, note and activity are the person's and untouched.
fn source_fields(
    workout: &mut Workout,
    raw: &RawWorkout,
    route: Option<BlobId>,
    source: HealthSourceId,
) {
    let measured =
        |new: Option<f64>, held: Option<f64>| new.filter(|v| v.is_finite() && *v >= 0.0).or(held);
    workout.active_energy_kcal = measured(raw.active_energy_kcal, workout.active_energy_kcal);
    workout.total_energy_kcal = measured(raw.total_energy_kcal, workout.total_energy_kcal);
    workout.distance_m = measured(raw.distance_m, workout.distance_m);
    workout.elevation_up_m = measured(raw.elevation_up_m, workout.elevation_up_m);
    workout.avg_hr = measured(raw.avg_hr, workout.avg_hr);
    workout.max_hr = measured(raw.max_hr, workout.max_hr);
    workout.effort = measured(raw.effort, workout.effort);
    workout.indoor = raw.indoor.or(workout.indoor);
    if raw.offset_min != 0 {
        workout.offset_min = raw.offset_min;
    }
    let name = capped(&raw.source, MAX_NAME);
    if !name.is_empty() {
        workout.source = name;
    }
    let external = capped(&raw.external_id, MAX_NAME);
    if !external.is_empty() {
        workout.external_id = external;
    }
    workout.source_id = Some(source);
    workout.route = route.or(workout.route);
}

/// Record that Health Auto Export's automation `headers` named just pushed
/// `carried` items. Nothing is recorded for a push that named none. The
/// list is bounded: a new automation past [`MAX_AUTOMATIONS`] replaces the
/// one heard from longest ago.
fn note_automation(
    seen: &mut Vec<AutomationSeen>,
    headers: &PushHeaders,
    carried: u64,
    now: Timestamp,
) {
    let id = capped(&headers.automation_id, MAX_HEADER);
    if id.is_empty() {
        return;
    }
    let known = seen.iter().position(|a| a.id == id);
    let at = match known {
        Some(at) => at,
        None => {
            if seen.len() >= MAX_AUTOMATIONS
                && let Some(oldest) = (0..seen.len()).min_by_key(|&i| seen[i].last_at)
            {
                seen.remove(oldest);
            }
            seen.push(AutomationSeen { id, ..Default::default() });
            seen.len() - 1
        }
    };
    let entry = &mut seen[at];
    keep(&mut entry.name, &headers.automation_name);
    keep(&mut entry.period, &headers.automation_period);
    keep(&mut entry.aggregation, &headers.automation_aggregation);
    entry.last_at = Some(now);
    entry.last_count = carried;
}

/// Replace `field` with `value`, trimmed and cut, unless it is blank: a
/// later push that omits a header does not erase what an earlier one said.
fn keep(field: &mut String, value: &str) {
    let value = capped(value, MAX_HEADER);
    if !value.is_empty() {
        *field = value;
    }
}

// ---- validation -------------------------------------------------------------

/// The settings as they will be stored: every part checked, priority names
/// trimmed and tidied.
fn checked_settings(settings: &HealthSettings) -> Result<HealthSettings> {
    let units = &settings.units;
    known_unit("glucose", &units.glucose, GLUCOSE_UNITS)?;
    known_unit("weight", &units.weight, WEIGHT_UNITS)?;
    known_unit("distance", &units.distance, DISTANCE_UNITS)?;
    known_unit("energy", &units.energy, ENERGY_UNITS)?;
    known_unit("temperature", &units.temperature, TEMPERATURE_UNITS)?;
    known_unit("length", &units.length, LENGTH_UNITS)?;

    let ActivityGoals { move_kcal, exercise_min, stand_hours, steps, sleep_hours } = settings.goals;
    positive("the move goal", move_kcal)?;
    positive("the exercise goal", exercise_min)?;
    positive("the stand goal", stand_hours)?;
    positive("the step goal", steps)?;
    positive("the sleep goal", sleep_hours)?;
    if stand_hours > 24.0 || sleep_hours > 24.0 {
        return Err(Error::Invalid("a day has only 24 hours".into()));
    }

    let n = &settings.nutrition;
    let targets = [
        ("the calorie target", n.kcal),
        ("the protein target", n.protein_g),
        ("the carbohydrate target", n.carbs_g),
        ("the fat target", n.fat_g),
        ("the fibre target", n.fiber_g),
        ("the sodium target", n.sodium_mg),
        ("the carbohydrate target per meal", n.carbs_per_meal_g),
        ("the low glucose threshold", settings.glucose_low),
        ("the high glucose threshold", settings.glucose_high),
    ];
    for (what, target) in targets {
        if let Some(value) = target {
            positive(what, value)?;
        }
    }
    if let (Some(low), Some(high)) = (settings.glucose_low, settings.glucose_high)
        && low >= high
    {
        return Err(Error::Invalid("the low glucose threshold must be below the high one".into()));
    }

    if settings.stale_hours > MAX_STALE_HOURS {
        return Err(Error::Invalid(format!(
            "a quiet source can be waited for at most {MAX_STALE_HOURS} hours, or 0 never to warn"
        )));
    }

    let mut checked = settings.clone();
    checked.priority = priority_names(&settings.priority)?;
    checked.priority_by_metric.clear();
    for (metric, names) in &settings.priority_by_metric {
        let names = priority_names(names)?;
        if !names.is_empty() {
            checked.priority_by_metric.insert(*metric, names);
        }
    }
    Ok(checked)
}

fn known_unit(what: &str, value: &str, known: &[&str]) -> Result<()> {
    if known.contains(&value) {
        return Ok(());
    }
    Err(Error::Invalid(format!("{what} can be shown in {}", known.join(" or "))))
}

fn positive(what: &str, value: f64) -> Result<()> {
    if value.is_finite() && value > 0.0 {
        return Ok(());
    }
    Err(Error::Invalid(format!("{what} must be a number above zero")))
}

/// A source priority list: names trimmed, blanks and repeats dropped, at
/// most [`MAX_PRIORITY_NAMES`] of at most [`MAX_NAME`] characters.
fn priority_names(names: &[String]) -> Result<Vec<String>> {
    let mut kept: Vec<String> = Vec::new();
    for name in names {
        let name = name.trim();
        if name.is_empty() || kept.iter().any(|k| k == name) {
            continue;
        }
        if name.chars().count() > MAX_NAME {
            return Err(Error::Invalid(format!(
                "a source's name can be at most {MAX_NAME} characters"
            )));
        }
        kept.push(name.to_string());
    }
    if kept.len() > MAX_PRIORITY_NAMES {
        return Err(Error::Invalid(format!("at most {MAX_PRIORITY_NAMES} sources can be ranked")));
    }
    Ok(kept)
}

/// A string a person typed, trimmed and no longer than `max` characters.
fn person_text(what: &str, text: &str, max: usize) -> Result<String> {
    let trimmed = text.trim();
    if trimmed.chars().count() > max {
        return Err(Error::Invalid(format!("{what} can be at most {max} characters")));
    }
    Ok(trimmed.to_string())
}

/// As [`person_text`] for a name: required, and at most [`MAX_NAME`].
fn name_text(what: &str, text: &str) -> Result<String> {
    let name = person_text(what, text, MAX_NAME)?;
    if name.is_empty() {
        return Err(Error::Invalid(format!("{what} cannot be blank")));
    }
    Ok(name)
}

/// Outside text, trimmed and cut to at most `max` characters rather than
/// refused: a device with a long name should not fail a push.
fn capped(text: &str, max: usize) -> String {
    text.trim().chars().take(max).collect()
}

// ---- the food-lookup keys ----------------------------------------------------

/// The sealed food-lookup keys, or `None`, including on a backend with no
/// secret store.
fn read_keys(store: &dyn JournalStore) -> Result<Option<HealthKeys>> {
    let Some(secrets) = store.secrets() else { return Ok(None) };
    match secrets.get_secret(HEALTH_KEYS_OWNER_KIND, KEYS_OWNER_ID)? {
        Some(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            // A fixed message: serde's own would quote the decrypted bytes.
            .map_err(|_| Error::Invalid("the stored food-lookup key could not be read".into())),
        None => Ok(None),
    }
}

/// The last four characters of a key, for telling keys apart -- or nothing
/// for one so short that four characters would be much of it.
fn hint(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 8 {
        return String::new();
    }
    chars[chars.len() - 4..].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SecretKey {
        SecretKey::from_bytes([9u8; 32])
    }

    #[test]
    fn a_series_hash_is_thirty_two_hex_characters_and_one_per_metric() {
        let key = key();
        let hashes: BTreeSet<String> = Metric::ALL.iter().map(|m| series_hash(&key, *m)).collect();
        assert_eq!(hashes.len(), Metric::ALL.len());
        for hash in &hashes {
            assert_eq!(hash.len(), 32);
            assert!(hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
        }
        // And it is the key's, not the metric's: another vault's differs.
        let other = SecretKey::from_bytes([1u8; 32]);
        assert_ne!(series_hash(&key, Metric::Steps), series_hash(&other, Metric::Steps));
    }

    #[test]
    fn a_workout_key_is_the_activity_and_the_times_and_nothing_else() {
        let key = key();
        let start = Timestamp::from_second(1_700_000_000).unwrap();
        let end = Timestamp::from_second(1_700_003_600).unwrap();
        let run = workout_key(&key, Activity::Running, start, end);
        assert_eq!(run, workout_key(&key, Activity::Running, start, end));
        assert_ne!(run, workout_key(&key, Activity::Walking, start, end));
        let longer = end + SignedDuration::from_secs(60);
        assert_ne!(run, workout_key(&key, Activity::Running, start, longer));
        // Sub-second noise in a source's clock is not a different workout.
        let jitter = Timestamp::from_millisecond(1_700_000_000_400).unwrap();
        assert_eq!(run, workout_key(&key, Activity::Running, jitter, end));
    }

    #[test]
    fn a_sample_with_an_impossible_time_is_not_kept() {
        let at = Timestamp::from_second(1_700_000_000).unwrap();
        let sample = |start: Timestamp, end: Timestamp, value: f64| RawSample {
            metric: Metric::HeartRate,
            start,
            end,
            value,
            unit: String::new(),
            source: "Watch".into(),
            offset_min: 0,
        };
        let later = at + SignedDuration::from_secs(60);
        assert!(canonical(&sample(at, later, 60.0)).is_some());
        assert!(canonical(&sample(later, at, 60.0)).is_none(), "ends before it starts");
        assert!(canonical(&sample(at, at, f64::NAN)).is_none());
        assert!(canonical(&sample(at, at, f64::INFINITY)).is_none());
        let long = at + SignedDuration::from_secs(MAX_SAMPLE_SECS + 1);
        assert!(canonical(&sample(at, long, 60.0)).is_none(), "longer than a sample may last");
    }

    #[test]
    fn an_intake_counts_what_it_refuses_and_names_a_nameless_sample_for_its_connection() {
        let at = Timestamp::from_second(1_700_000_000).unwrap();
        let mut intake = Intake::new("Phone");
        let mut raw = RawSample {
            metric: Metric::HeartRate,
            start: at,
            end: at,
            value: 61.0,
            unit: String::new(),
            source: "   ".into(),
            offset_min: -420,
        };
        assert!(intake.take(&raw));
        raw.value = f64::NAN;
        assert!(!intake.take(&raw));
        assert_eq!(intake.rejected, 1);
        assert_eq!(intake.devices.iter().collect::<Vec<_>>(), ["Phone"]);
        let held = &intake.by_metric[&Metric::HeartRate];
        assert_eq!(held.len(), 1);
        assert_eq!((held[0].source.as_str(), held[0].offset_min, held[0].secs), ("Phone", -420, 0));
        assert_eq!((intake.first, intake.last), (Some(at), Some(at)));
    }

    #[test]
    fn settings_with_a_wrong_unit_or_a_goal_of_nothing_are_refused() {
        assert!(checked_settings(&HealthSettings::default()).is_ok());

        let mut bad = HealthSettings::default();
        bad.units.weight = "stone".into();
        assert_eq!(checked_settings(&bad).unwrap_err().code(), "invalid");

        let mut bad = HealthSettings::default();
        bad.goals.steps = 0.0;
        assert!(checked_settings(&bad).is_err());
        bad.goals.steps = f64::NAN;
        assert!(checked_settings(&bad).is_err());

        let bad = HealthSettings { stale_hours: MAX_STALE_HOURS + 1, ..Default::default() };
        assert!(checked_settings(&bad).is_err());

        let bad = HealthSettings {
            glucose_low: Some(180.0),
            glucose_high: Some(70.0),
            ..Default::default()
        };
        assert!(checked_settings(&bad).is_err(), "a low above the high");
    }

    #[test]
    fn a_priority_list_is_trimmed_deduplicated_and_bounded() {
        let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            priority_names(&names(&["  Apple Watch ", "", "iPhone", "Apple Watch"])).unwrap(),
            ["Apple Watch", "iPhone"]
        );
        let many: Vec<String> = (0..=MAX_PRIORITY_NAMES).map(|i| format!("device {i}")).collect();
        assert!(priority_names(&many).is_err());
        assert!(priority_names(&["x".repeat(MAX_NAME + 1)]).is_err());
    }

    #[test]
    fn only_a_long_enough_key_shows_its_last_four_characters() {
        assert_eq!(hint("abcd1234"), "");
        assert_eq!(hint("abcdefgh12345678"), "5678");
    }

    #[test]
    fn a_range_must_run_forward_and_stay_within_the_cap() {
        let day = Date::constant(2026, 11, 15);
        assert_eq!(range_days(day, day).unwrap(), 1);
        assert!(range_days(day, day.yesterday().unwrap()).is_err());
        let far = plus_days(day, MAX_RANGE_DAYS);
        assert!(range_days(day, far).is_err());
        let last = plus_days(day, MAX_RANGE_DAYS - 1);
        assert_eq!(range_days(day, last).unwrap(), MAX_RANGE_DAYS as i32);
    }

    #[test]
    fn a_zone_is_the_systems_when_empty_and_refused_when_not_a_name() {
        assert!(zone_of("").is_ok());
        assert!(zone_of("UTC").is_ok());
        assert_eq!(zone_of("Not/AZone").unwrap_err().code(), "invalid");
    }

    #[test]
    fn a_day_begins_at_its_first_instant_in_the_zone() {
        let day = Date::constant(2026, 11, 15);
        let (start, end) = day_bounds(day, day, &TimeZone::UTC);
        assert_eq!(end.as_second() - start.as_second(), 86_400);
        assert_eq!(utc_day(start.as_second()), day);
    }

    #[test]
    fn a_care_method_that_is_unsupported_leaves_nothing_where_any_other_error_stays() {
        let none: Result<Vec<u8>> = Err(Error::Unsupported("health"));
        assert!(or_nothing(none).unwrap().is_empty());
        let broken: Result<Vec<u8>> = Err(Error::Invalid("no".into()));
        assert_eq!(or_nothing(broken).unwrap_err().code(), "invalid");
    }

    #[test]
    fn an_automation_is_recorded_by_id_and_a_missing_header_keeps_what_was_known() {
        let now = Timestamp::from_second(1_700_000_000).unwrap();
        let mut seen = Vec::new();
        let mut headers = PushHeaders {
            automation_id: " abc ".into(),
            automation_name: "Hourly".into(),
            automation_period: "Since Last Sync".into(),
            automation_aggregation: String::new(),
            session_id: String::new(),
        };
        note_automation(&mut seen, &headers, 12, now);
        headers.automation_name = String::new();
        note_automation(&mut seen, &headers, 3, now);
        assert_eq!(seen.len(), 1);
        let only = &seen[0];
        assert_eq!(
            (only.id.as_str(), only.name.as_str(), only.period.as_str(), only.last_count),
            ("abc", "Hourly", "Since Last Sync", 3)
        );
        headers.automation_id = String::new();
        note_automation(&mut seen, &headers, 1, now);
        assert_eq!(seen.len(), 1, "a push that names no automation records none");
    }

    #[test]
    fn an_automation_list_never_grows_past_its_bound() {
        let mut seen = Vec::new();
        for i in 0..(MAX_AUTOMATIONS + 5) {
            let headers = PushHeaders { automation_id: format!("a{i}"), ..Default::default() };
            let at = Timestamp::from_second(1_700_000_000 + i as i64).unwrap();
            note_automation(&mut seen, &headers, 1, at);
        }
        assert_eq!(seen.len(), MAX_AUTOMATIONS);
        let newest = format!("a{}", MAX_AUTOMATIONS + 4);
        assert!(seen.iter().any(|a| a.id == newest), "the newest is kept");
        assert!(!seen.iter().any(|a| a.id == "a0"), "the longest quiet went");
    }
}
