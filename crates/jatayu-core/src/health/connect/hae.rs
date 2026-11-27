//! Health Auto Export's REST API JSON (export version 2).
//!
//! `{"data": {"metrics": [...], "workouts": [...], "medications": [...],
//! ...}}`, one data type per automation. Dates are `yyyy-MM-dd HH:mm:ss Z`.
//! The format is documented at
//! <https://help.healthyapps.dev/en/health-auto-export/export-format>.
//! Metrics must be sent with "Summarize Data" off: summarised buckets
//! would not deduplicate against the export file's raw samples, so
//! [`parse`] refuses a push whose aggregation header says it is summarised.
//!
//! Units are passed through as the push spelled them (`count/min`, `lb`,
//! `mg/dL`); the vault makes them canonical. The workout fields that
//! [`RawWorkout`] fixes in kilocalories and metres are converted here, with
//! `energy_kcal` and `length_m`, which the Apple export reader shares.
//! Nothing in here logs a value: a rejected or skipped entry is counted
//! in the batch and nothing more.

use super::super::{
    Activity, Code, DoseStatus, HealthBatch, Metric, PushHeaders, RawDose, RawSample, RawWorkout,
    RoutePoint, SleepStage,
};
use super::fhir;
use crate::error::{Error, Result};
use jiff::Timestamp;
use jiff::civil::DateTime;
use jiff::tz::Offset;
use serde_json::Value;

/// Parse one push. `headers` are what the route read beside it.
///
/// Refuses the whole push, with a sentence the person can act on, when it
/// is summarised or has no `data`. Inside a push, an entry that cannot be
/// read (no date, no value) is counted in [`HealthBatch::rejected`] and a
/// metric or array the catalogue does not know in [`HealthBatch::skipped`];
/// the rest still arrives.
pub fn parse(body: &Value, headers: &PushHeaders) -> Result<HealthBatch> {
    let aggregation = headers.automation_aggregation.trim().to_ascii_lowercase();
    if matches!(aggregation.as_str(), "hours" | "days" | "weeks" | "months" | "years") {
        return Err(Error::Invalid(format!(
            "this automation sends data summarised by {aggregation}; turn Summarize Data off in \
             Health Auto Export, so that readings arrive one by one and can be told apart from \
             the ones already here"
        )));
    }
    let Some(data) = body.get("data").and_then(Value::as_object) else {
        return Err(Error::Invalid(
            "that is not a Health Auto Export push: the JSON has no \"data\" object".into(),
        ));
    };
    let mut batch = HealthBatch::default();
    for (name, value) in data {
        let Some(items) = value.as_array() else { continue };
        match name.as_str() {
            "metrics" => items.iter().for_each(|m| read_metric(&mut batch, m)),
            "workouts" => items.iter().for_each(|w| read_workout(&mut batch, w)),
            "medications" => items.iter().for_each(|m| read_dose(&mut batch, m)),
            // stateOfMind, symptoms, cycleTracking, ecg, heartRateNotifications, and
            // whatever the app adds next: counted, so nothing disappears silently.
            other => skip_many(&mut batch, other, items.len()),
        }
    }
    Ok(batch)
}

/// `2024-02-06 14:30:00 -0800` → the instant and its offset in minutes.
///
/// Also reads RFC 3339 (`2024-02-06T14:30:00-08:00`, `...Z`, with
/// fractional seconds), which is what a FHIR file or a GPX track writes. A
/// time with no offset is refused rather than guessed at: assuming UTC would
/// put a reading hours away from the day it was taken on.
pub fn parse_date(s: &str) -> Option<(Timestamp, i16)> {
    let s = s.trim();
    let field = |from: usize, to: usize| -> Option<i32> {
        let digits = s.get(from..to)?;
        if digits.bytes().all(|b| b.is_ascii_digit()) { digits.parse().ok() } else { None }
    };
    let byte_is = |i: usize, want: &[u8]| s.as_bytes().get(i).is_some_and(|b| want.contains(b));
    if !(byte_is(4, b"-")
        && byte_is(7, b"-")
        && byte_is(10, b" Tt")
        && byte_is(13, b":")
        && byte_is(16, b":"))
    {
        return None;
    }
    let mut rest = s.get(19..)?;
    let mut nanos = 0;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        // Nanoseconds: the first nine digits, padded on the right.
        let kept = &fraction[..digits.min(9)];
        nanos = kept.parse::<i32>().ok()? * 10i32.pow(9 - kept.len() as u32);
        rest = &fraction[digits..];
    }
    let datetime = DateTime::new(
        i16::try_from(field(0, 4)?).ok()?,
        i8::try_from(field(5, 7)?).ok()?,
        i8::try_from(field(8, 10)?).ok()?,
        i8::try_from(field(11, 13)?).ok()?,
        i8::try_from(field(14, 16)?).ok()?,
        i8::try_from(field(17, 19)?).ok()?,
        nanos,
    )
    .ok()?;
    let seconds = offset_seconds(rest)?;
    let instant = Offset::from_seconds(seconds).ok()?.to_timestamp(datetime).ok()?;
    Some((instant, i16::try_from(seconds / 60).ok()?))
}

/// `Z`, `-0800`, `+05:30` or `+01` as seconds east of UTC.
fn offset_seconds(s: &str) -> Option<i32> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("z") || s.eq_ignore_ascii_case("utc") {
        return Some(0);
    }
    let sign = match s.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits: String = s[1..].chars().filter(|c| *c != ':').collect();
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (hours, minutes) = match digits.len() {
        2 => (digits.parse::<i32>().ok()?, 0),
        4 => (digits[..2].parse::<i32>().ok()?, digits[2..].parse::<i32>().ok()?),
        _ => return None,
    };
    (minutes < 60).then_some(sign * (hours * 3600 + minutes * 60))
}

/// Energy in kilocalories. Empty means kilocalories already. `None` for a
/// unit this does not know.
pub(crate) fn energy_kcal(value: f64, unit: &str) -> Option<f64> {
    match unit.trim() {
        "" | "kcal" | "Cal" | "kilocalorie" | "kilocalories" => Some(value),
        "cal" => Some(value / 1000.0),
        "kJ" | "kj" => Some(value / 4.184),
        "J" => Some(value / 4184.0),
        _ => None,
    }
}

/// A length in metres. `None` for a unit this does not know, and for no unit
/// at all: a distance of 5 might be kilometres or miles.
pub(crate) fn length_m(value: f64, unit: &str) -> Option<f64> {
    let factor = match unit.trim().to_ascii_lowercase().as_str() {
        "m" | "meter" | "meters" | "metre" | "metres" => 1.0,
        "km" | "kilometer" | "kilometers" | "kilometre" | "kilometres" => 1000.0,
        "mi" | "mile" | "miles" => 1609.344,
        "yd" | "yard" | "yards" => 0.9144,
        "ft" | "foot" | "feet" => 0.3048,
        "cm" => 0.01,
        "in" | "inch" | "inches" => 0.0254,
        _ => return None,
    };
    Some(value * factor)
}

// ---- values -------------------------------------------------------------------

/// A JSON number, or a string holding one. Never NaN or infinite.
fn number(v: &Value) -> Option<f64> {
    let n = match v {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.trim().parse().ok()?,
        _ => return None,
    };
    n.is_finite().then_some(n)
}

fn num(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(number)
}

fn text<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// `{"qty": 5, "units": "km"}`, or a bare number, as (quantity, units).
fn quantity(v: &Value) -> Option<(f64, &str)> {
    match v {
        Value::Object(_) => Some((num(v, "qty")?, text(v, "units").unwrap_or(""))),
        other => number(other).map(|n| (n, "")),
    }
}

fn skip_many(batch: &mut HealthBatch, what: &str, count: usize) {
    if count > 0 {
        *batch.skipped.entry(what.to_string()).or_default() += count as u64;
    }
}

/// A moment as a string (`yyyy-MM-dd HH:mm:ss Z`), or epoch seconds (or
/// milliseconds, past the year 5138 in seconds) as a number.
fn moment(v: &Value) -> Option<(Timestamp, i16)> {
    match v {
        Value::String(s) => parse_date(s),
        Value::Number(n) => {
            let n = n.as_f64()?;
            let secs = if n.abs() > 1e11 { n / 1000.0 } else { n };
            Some((Timestamp::from_second(secs.floor() as i64).ok()?, 0))
        }
        _ => None,
    }
}

fn first_moment(v: &Value, keys: &[&str]) -> Option<(Timestamp, i16)> {
    keys.iter().find_map(|k| v.get(*k).and_then(moment))
}

/// When a data point was measured.
struct When {
    start: Timestamp,
    end: Timestamp,
    offset_min: i16,
}

/// Its own `start` and `end` (or `startDate` and `endDate`) whenever it
/// carries them, so that an interval stays an interval; only an entry with
/// neither is an instant, `start = end = date`. HAE's `date` is the
/// sample's start, so it is the right start for an entry that has no
/// `start` of its own. (An instant and an interval that begin together at
/// the same value are one reading to the series: see `chunk::merge`.)
fn when(point: &Value) -> Option<When> {
    let (start, offset_min) =
        first_moment(point, &["start", "startDate"]).or_else(|| first_moment(point, &["date"]))?;
    let end = first_moment(point, &["end", "endDate"]).map_or(start, |(t, _)| t);
    (end >= start).then_some(When { start, end, offset_min })
}

/// A stretch of time that must have both ends: a sleep segment.
fn span(point: &Value) -> Option<When> {
    let (start, offset_min) = first_moment(point, &["startDate", "start"])?;
    let (end, _) = first_moment(point, &["endDate", "end"])?;
    (end > start).then_some(When { start, end, offset_min })
}

// ---- metrics ------------------------------------------------------------------

/// What one `metrics[]` entry's data points look like.
enum Shape {
    /// `{qty, date, source}`; `heart_rate` carries `Min`/`Avg`/`Max`
    /// instead, and its average is what is kept.
    Plain {
        metric: Metric,
        average: bool,
    },
    BloodPressure,
    Sleep,
    Insulin,
}

fn read_metric(batch: &mut HealthBatch, metric: &Value) {
    let Some(name) = text(metric, "name") else {
        batch.rejected += 1;
        return;
    };
    let points = metric.get("data").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let units = text(metric, "units").unwrap_or("");
    let shape = match name {
        "blood_pressure" => Shape::BloodPressure,
        "sleep_analysis" => Shape::Sleep,
        "insulin_delivery" => Shape::Insulin,
        other => match Metric::from_hae(other) {
            Some(metric) => Shape::Plain { metric, average: other == "heart_rate" },
            None => return skip_many(batch, other, points.len()),
        },
    };
    for point in points {
        if !point.is_object() {
            batch.rejected += 1;
            continue;
        }
        match shape {
            Shape::Plain { metric, average } => plain_point(batch, point, metric, units, average),
            Shape::BloodPressure => pressure_point(batch, point, units),
            Shape::Sleep => sleep_point(batch, point),
            Shape::Insulin => insulin_point(batch, point, units),
        }
    }
}

fn sample(metric: Metric, when: &When, value: f64, unit: &str, point: &Value) -> RawSample {
    RawSample {
        metric,
        start: when.start,
        end: when.end,
        value,
        unit: unit.to_string(),
        source: text(point, "source").unwrap_or("").to_string(),
        offset_min: when.offset_min,
    }
}

fn plain_point(batch: &mut HealthBatch, point: &Value, metric: Metric, units: &str, average: bool) {
    let value =
        if average { num(point, "Avg").or_else(|| num(point, "qty")) } else { num(point, "qty") };
    match (value, when(point)) {
        (Some(value), Some(when)) => batch.samples.push(sample(metric, &when, value, units, point)),
        _ => batch.rejected += 1,
    }
}

/// One reading, two samples: systolic and diastolic.
fn pressure_point(batch: &mut HealthBatch, point: &Value, units: &str) {
    let (systolic, diastolic) = (num(point, "systolic"), num(point, "diastolic"));
    let Some(when) = when(point).filter(|_| systolic.is_some() || diastolic.is_some()) else {
        batch.rejected += 1;
        return;
    };
    for (metric, value) in
        [(Metric::BloodPressureSystolic, systolic), (Metric::BloodPressureDiastolic, diastolic)]
    {
        if let Some(value) = value {
            batch.samples.push(sample(metric, &when, value, units, point));
        }
    }
}

/// One segment of one sleep stage. The summarised form (`totalSleep`, one
/// entry a night) cannot be told apart from the segments already stored, so
/// it is rejected, counted, and the person told to turn summarising off.
fn sleep_point(batch: &mut HealthBatch, point: &Value) {
    let stage = text(point, "value").and_then(sleep_stage);
    match (point.get("totalSleep"), stage, span(point)) {
        (None, Some(stage), Some(when)) => {
            batch.samples.push(sample(Metric::SleepStage, &when, stage.code(), "", point));
        }
        _ => batch.rejected += 1,
    }
}

fn sleep_stage(name: &str) -> Option<SleepStage> {
    match name.trim().to_ascii_lowercase().as_str() {
        "core" => Some(SleepStage::Core),
        "rem" => Some(SleepStage::Rem),
        "deep" => Some(SleepStage::Deep),
        "awake" => Some(SleepStage::Awake),
        "asleep" | "unspecified" | "asleepunspecified" => Some(SleepStage::Asleep),
        "in bed" | "inbed" => Some(SleepStage::InBed),
        _ => None,
    }
}

fn insulin_point(batch: &mut HealthBatch, point: &Value, units: &str) {
    let metric = match text(point, "reason").map(str::to_ascii_lowercase).as_deref() {
        Some("bolus") => Some(Metric::InsulinBolus),
        Some("basal") => Some(Metric::InsulinBasal),
        _ => None,
    };
    match (metric, num(point, "qty"), when(point)) {
        (Some(metric), Some(value), Some(when)) => {
            batch.samples.push(sample(metric, &when, value, units, point));
        }
        _ => batch.rejected += 1,
    }
}

// ---- workouts -----------------------------------------------------------------

/// The activity a Health Auto Export workout name stands for ("Outdoor Run",
/// "Traditional Strength Training", "Pool Swim"). Read by its words, so
/// that "Indoor Run" and "Run" are the same and "Throwing" is not rowing.
fn activity_from_name(name: &str) -> Activity {
    let lower = name.to_ascii_lowercase();
    let words: Vec<&str> =
        lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let has = |w: &str| words.contains(&w);
    if has("functional") && has("strength") {
        Activity::FunctionalTraining
    } else if has("strength") {
        Activity::StrengthTraining
    } else if has("core") {
        Activity::CoreTraining
    } else if has("hiit") || (has("high") && has("intensity")) {
        Activity::Hiit
    } else if has("mind") && has("body") {
        Activity::Mindfulness
    } else if has("run") || has("running") {
        Activity::Running
    } else if has("walk") || has("walking") {
        Activity::Walking
    } else if has("cycling") || has("cycle") || has("bike") || has("biking") {
        Activity::Cycling
    } else if has("swim") || has("swimming") {
        Activity::Swimming
    } else if has("hiking") || has("hike") {
        Activity::Hiking
    } else if has("rowing") || has("row") {
        Activity::Rowing
    } else if has("elliptical") {
        Activity::Elliptical
    } else if has("stair") || has("stairs") || has("stairstepper") {
        Activity::StairClimbing
    } else if has("yoga") {
        Activity::Yoga
    } else if has("pilates") {
        Activity::Pilates
    } else if has("dance") || has("dancing") {
        Activity::Dance
    } else if has("martial") {
        Activity::MartialArts
    } else if has("tennis") {
        Activity::Tennis
    } else if has("soccer") {
        Activity::Soccer
    } else if has("basketball") {
        Activity::Basketball
    } else if has("golf") {
        Activity::Golf
    } else if has("cooldown") || (has("cool") && has("down")) {
        Activity::Cooldown
    } else {
        Activity::Other
    }
}

/// A workout, version 2 (version 1's `lat`/`lon`, `activeEnergy` and
/// `qty`-only heart rate are read as well).
fn read_workout(batch: &mut HealthBatch, w: &Value) {
    let name = text(w, "name").unwrap_or("");
    let start = w.get("start").and_then(moment);
    let end = w.get("end").and_then(moment).map(|(t, _)| t).or_else(|| {
        // No end: the start and the duration (seconds) give it.
        let secs = num(w, "duration").filter(|s| (0.0..=1e7).contains(s))?;
        start?.0.checked_add(jiff::SignedDuration::from_secs(secs as i64)).ok()
    });
    let (Some((start, offset_min)), Some(end)) = (start, end) else {
        batch.rejected += 1;
        return;
    };
    if end < start {
        batch.rejected += 1;
        return;
    }
    let kcal = |v: Option<&Value>| {
        let (qty, unit) = quantity(v?)?;
        energy_kcal(qty, unit)
    };
    let metres = |v: Option<&Value>| {
        let (qty, unit) = quantity(v?)?;
        length_m(qty, unit)
    };
    // Version 2 nests `heartRate.avg.qty`; version 1 has `avgHeartRate.qty`.
    let heart = |range: &str, flat: &str| {
        let nested = w.get("heartRate").and_then(|h| h.get(range));
        quantity(nested.or_else(|| w.get(flat))?).map(|(qty, _)| qty)
    };
    let source = match w.get("source") {
        Some(Value::Object(_)) => text(&w["source"], "name"),
        Some(Value::String(s)) => Some(s.trim()).filter(|s| !s.is_empty()),
        _ => None,
    };
    let indoor = w.get("isIndoor").and_then(Value::as_bool).or_else(|| {
        match text(w, "location")?.to_ascii_lowercase().as_str() {
            "indoor" => Some(true),
            "outdoor" => Some(false),
            _ => None,
        }
    });
    let route = w
        .get("route")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(route_point)
        .collect();
    batch.workouts.push(RawWorkout {
        activity: activity_from_name(name),
        name: name.to_string(),
        start,
        end,
        offset_min,
        active_energy_kcal: kcal(w.get("activeEnergyBurned").or_else(|| w.get("activeEnergy"))),
        total_energy_kcal: kcal(w.get("totalEnergy")),
        distance_m: metres(w.get("distance")),
        elevation_up_m: metres(w.get("elevationUp")),
        avg_hr: heart("avg", "avgHeartRate"),
        max_hr: heart("max", "maxHeartRate"),
        effort: None,
        indoor,
        source: source.unwrap_or("").to_string(),
        external_id: text(w, "id").unwrap_or("").to_string(),
        route,
        // `heartRateData` is HAE's per-minute Min/Avg/Max aggregate of the
        // workout. Filed as samples it would sit beside the watch's own
        // readings and count the same minutes twice, so it is not kept; the
        // workout's own average and maximum are.
        heart_rate: Vec::new(),
    });
}

/// One GPS fix. Version 2 names the fields in full, version 1 abbreviates.
fn route_point(p: &Value) -> Option<RoutePoint> {
    let lat = num(p, "latitude").or_else(|| num(p, "lat"))?;
    let lon = num(p, "longitude").or_else(|| num(p, "lon")).or_else(|| num(p, "lng"))?;
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return None;
    }
    let (at, _) = first_moment(p, &["timestamp", "date"])?;
    Some(RoutePoint { at, lat, lon, alt: num(p, "altitude") })
}

// ---- medications --------------------------------------------------------------

/// A dose event from Apple Health's Medications (iOS 26). Only `Taken` and
/// `Skipped` are doses; every other status (`Not Logged`, `Snoozed`, ...)
/// is counted under `medication:<status>`.
fn read_dose(batch: &mut HealthBatch, m: &Value) {
    if !m.is_object() {
        batch.rejected += 1;
        return;
    }
    let status = match text(m, "status").unwrap_or("unspecified") {
        s if s.eq_ignore_ascii_case("taken") => DoseStatus::Taken,
        s if s.eq_ignore_ascii_case("skipped") => DoseStatus::Skipped,
        other => return skip_many(batch, &format!("medication:{other}"), 1),
    };
    let name = text(m, "displayText").or_else(|| text(m, "nickname"));
    let at = first_moment(m, &["start", "date"]);
    let (Some(name), Some((at, _))) = (name, at) else {
        batch.rejected += 1;
        return;
    };
    let codes = m
        .get("codings")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(|c| {
            Some(Code {
                system: fhir::short_system(text(c, "system").unwrap_or("")),
                code: text(c, "code")?.to_string(),
                display: name.to_string(),
            })
        })
        .collect();
    batch.doses.push(RawDose {
        name: name.to_string(),
        codes,
        at,
        scheduled_at: m.get("scheduledDate").and_then(moment).map(|(t, _)| t),
        status,
        amount: num(m, "dosage"),
        unit: text(m, "units").unwrap_or("").to_string(),
        source: String::new(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn headers() -> PushHeaders {
        PushHeaders::default()
    }

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    /// Unit conversions multiply floats; compare them to a billionth.
    fn near(got: Option<f64>, want: f64) -> bool {
        got.is_some_and(|g| (g - want).abs() < 1e-9)
    }

    #[test]
    fn dates_in_the_apps_format_carry_their_offset() {
        let (t, offset) = parse_date("2024-02-06 14:30:00 -0800").unwrap();
        assert_eq!(t, at("2024-02-06T22:30:00Z"));
        assert_eq!(offset, -480);
        let (t, offset) = parse_date("2024-02-06 08:00:00 +0530").unwrap();
        assert_eq!(t, at("2024-02-06T02:30:00Z"));
        assert_eq!(offset, 330);
    }

    #[test]
    fn rfc_3339_dates_are_read_too() {
        let (t, offset) = parse_date("2024-02-06T14:30:00-08:00").unwrap();
        assert_eq!(t, at("2024-02-06T22:30:00Z"));
        assert_eq!(offset, -480);
        assert_eq!(parse_date("2024-02-06T14:30:00Z"), Some((at("2024-02-06T14:30:00Z"), 0)));
        let (t, _) = parse_date("2024-02-06T14:30:00.250+01:00").unwrap();
        assert_eq!(t, at("2024-02-06T13:30:00.25Z"));
    }

    #[test]
    fn a_date_that_is_not_one_is_none() {
        for bad in [
            "",
            "yesterday",
            "2024-02-06",
            "2024-02-06 14:30:00",
            "2024-13-06 14:30:00 -0800",
            "2024-02-06 14:30:00 PST",
            "2024-02-06 14:3é:00 -0800",
        ] {
            assert_eq!(parse_date(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn units_become_kilocalories_and_metres() {
        assert_eq!(energy_kcal(100.0, "kcal"), Some(100.0));
        assert!(near(energy_kcal(418.4, "kJ"), 100.0));
        assert_eq!(energy_kcal(5.0, "furlongs"), None);
        assert!(near(length_m(5.0, "km"), 5000.0));
        assert!(near(length_m(1.0, "mi"), 1609.344));
        assert!(near(length_m(1234.0, "cm"), 12.34));
        assert_eq!(length_m(5.0, ""), None);
    }

    #[test]
    fn plain_metrics_become_samples_in_the_units_they_came_in() {
        let body = json!({"data": {"metrics": [
            {"name": "step_count", "units": "count", "data": [
                {"qty": 8500, "date": "2024-02-06 14:30:00 -0800", "source": "Apple Watch"}
            ]},
            {"name": "blood_glucose", "units": "mg/dL", "data": [
                {"date": "2024-02-06 08:00:00 -0800", "qty": 95, "mealTime": "Before Meal", "source": "Health"}
            ]}
        ]}});
        let batch = parse(&body, &headers()).unwrap();
        assert_eq!(batch.rejected, 0);
        assert_eq!(batch.samples.len(), 2);
        let steps = &batch.samples[0];
        assert_eq!(steps.metric, Metric::Steps);
        assert_eq!(steps.value, 8500.0);
        assert_eq!(steps.unit, "count");
        assert_eq!(steps.source, "Apple Watch");
        assert_eq!(steps.start, at("2024-02-06T22:30:00Z"));
        assert_eq!(steps.end, steps.start);
        assert_eq!(steps.offset_min, -480);
        let glucose = &batch.samples[1];
        assert_eq!(
            (glucose.metric, glucose.value, glucose.unit.as_str()),
            (Metric::BloodGlucose, 95.0, "mg/dL")
        );
    }

    #[test]
    fn a_data_point_with_start_and_end_keeps_both() {
        let body = json!({"data": {"metrics": [{"name": "step_count", "units": "count", "data": [
            {"qty": 120, "start": "2024-02-06 14:00:00 -0800", "end": "2024-02-06 14:05:00 -0800", "date": "2024-02-06 14:05:00 -0800"}
        ]}]}});
        let s = &parse(&body, &headers()).unwrap().samples[0];
        assert_eq!(s.end.as_second() - s.start.as_second(), 300);
    }

    #[test]
    fn an_interval_stays_an_interval_and_only_a_bare_date_is_an_instant() {
        let body = json!({"data": {"metrics": [{"name": "step_count", "units": "count", "data": [
            {"qty": 1, "startDate": "2024-02-06 14:00:00 -0800", "endDate": "2024-02-06 14:05:00 -0800", "date": "2024-02-06 14:05:00 -0800"},
            {"qty": 2, "date": "2024-02-06 15:00:00 -0800"}
        ]}]}});
        let samples = parse(&body, &headers()).unwrap().samples;
        assert_eq!(samples[0].end.as_second() - samples[0].start.as_second(), 300);
        assert_eq!(samples[0].start, at("2024-02-06T22:00:00Z"));
        assert_eq!(samples[1].start, samples[1].end);
        assert_eq!(samples[1].start, at("2024-02-06T23:00:00Z"));
    }

    #[test]
    fn heart_rate_keeps_the_average() {
        let body = json!({"data": {"metrics": [{"name": "heart_rate", "units": "count/min", "data": [
            {"date": "2024-02-06 14:30:00 -0800", "Min": 65, "Avg": 72, "Max": 85, "source": "Apple Watch"}
        ]}]}});
        let s = &parse(&body, &headers()).unwrap().samples[0];
        assert_eq!((s.metric, s.value, s.unit.as_str()), (Metric::HeartRate, 72.0, "count/min"));
    }

    #[test]
    fn blood_pressure_is_two_samples() {
        let body = json!({"data": {"metrics": [{"name": "blood_pressure", "units": "mmHg", "data": [
            {"date": "2024-02-06 14:30:00 -0800", "systolic": 120, "diastolic": 80, "source": "Health"}
        ]}]}});
        let batch = parse(&body, &headers()).unwrap();
        let got: Vec<_> = batch.samples.iter().map(|s| (s.metric, s.value, s.start)).collect();
        let when = at("2024-02-06T22:30:00Z");
        assert_eq!(
            got,
            vec![
                (Metric::BloodPressureSystolic, 120.0, when),
                (Metric::BloodPressureDiastolic, 80.0, when)
            ]
        );
    }

    #[test]
    fn sleep_segments_are_stage_samples() {
        let body = json!({"data": {"metrics": [{"name": "sleep_analysis", "units": "hr", "data": [
            {"date": "2024-02-05 23:00:00 -0800", "startDate": "2024-02-05 23:00:00 -0800",
             "endDate": "2024-02-05 23:30:00 -0800", "qty": 0.5, "value": "Core", "source": "Apple Watch"},
            {"date": "2024-02-05 23:30:00 -0800", "startDate": "2024-02-05 23:30:00 -0800",
             "endDate": "2024-02-06 00:00:00 -0800", "qty": 0.5, "value": "REM", "source": "Apple Watch"},
            {"date": "2024-02-06 00:00:00 -0800", "startDate": "2024-02-06 00:00:00 -0800",
             "endDate": "2024-02-06 00:10:00 -0800", "qty": 0.17, "value": "In Bed", "source": "Apple Watch"}
        ]}]}});
        let batch = parse(&body, &headers()).unwrap();
        let stages: Vec<_> = batch.samples.iter().map(|s| (s.metric, s.value)).collect();
        assert_eq!(
            stages,
            vec![
                (Metric::SleepStage, SleepStage::Core.code()),
                (Metric::SleepStage, SleepStage::Rem.code()),
                (Metric::SleepStage, SleepStage::InBed.code()),
            ]
        );
        assert_eq!(batch.samples[0].end.as_second() - batch.samples[0].start.as_second(), 1800);
    }

    #[test]
    fn summarised_sleep_is_rejected_and_counted() {
        let body = json!({"data": {"metrics": [{"name": "sleep_analysis", "units": "hr", "data": [
            {"date": "2024-02-06 00:00:00 -0800", "totalSleep": 7.5, "asleep": 0.5, "core": 3.5, "deep": 1.5,
             "rem": 2.0, "sleepStart": "2024-02-05 23:00:00 -0800", "sleepEnd": "2024-02-06 06:30:00 -0800",
             "inBed": 8.0, "awake": 0.0, "source": "Apple Watch"}
        ]}]}});
        let batch = parse(&body, &headers()).unwrap();
        assert!(batch.samples.is_empty());
        assert_eq!(batch.rejected, 1);
    }

    #[test]
    fn insulin_follows_its_reason() {
        let body = json!({"data": {"metrics": [{"name": "insulin_delivery", "units": "IU", "data": [
            {"date": "2024-02-06 08:00:00 -0800", "qty": 5, "reason": "Bolus", "source": "Health"},
            {"date": "2024-02-06 09:00:00 -0800", "qty": 0.8, "reason": "Basal", "source": "Health"},
            {"date": "2024-02-06 10:00:00 -0800", "qty": 1, "source": "Health"}
        ]}]}});
        let batch = parse(&body, &headers()).unwrap();
        let got: Vec<_> =
            batch.samples.iter().map(|s| (s.metric, s.value, s.unit.as_str())).collect();
        assert_eq!(got, vec![(Metric::InsulinBolus, 5.0, "IU"), (Metric::InsulinBasal, 0.8, "IU")]);
        assert_eq!(batch.rejected, 1, "a dose with no reason cannot be filed as either kind");
    }

    #[test]
    fn what_the_catalogue_does_not_know_is_counted_by_name() {
        let body = json!({"data": {"metrics": [
            {"name": "made_up_for_the_test", "units": "x", "data": [
                {"qty": 1, "date": "2024-02-06 08:00:00 -0800"}, {"qty": 2, "date": "2024-02-06 09:00:00 -0800"}]},
            {"name": "step_count", "units": "count", "data": [
                {"date": "2024-02-06 08:00:00 -0800"}, {"qty": 4, "date": "not a date"}]}
        ]}});
        let batch = parse(&body, &headers()).unwrap();
        assert_eq!(batch.skipped.get("made_up_for_the_test"), Some(&2));
        assert_eq!(batch.rejected, 2, "no value, and no date");
        assert!(batch.samples.is_empty());
    }

    #[test]
    fn a_version_2_workout_becomes_a_raw_workout() {
        let body = json!({"data": {"workouts": [{
            "id": "A1B2C3D4",
            "name": "Outdoor Run",
            "start": "2024-02-06 07:00:00 -0800",
            "end": "2024-02-06 07:45:00 -0800",
            "duration": 2700,
            "location": "Outdoor",
            "source": {"name": "Apple Watch", "identifier": "com.apple.health.1234"},
            "activeEnergyBurned": {"qty": 418.4, "units": "kJ"},
            "totalEnergy": {"qty": 500, "units": "kcal"},
            "distance": {"qty": 5.2, "units": "km"},
            "elevationUp": {"qty": 100, "units": "ft"},
            "heartRate": {"min": {"qty": 95, "units": "count/min"}, "avg": {"qty": 152, "units": "count/min"},
                          "max": {"qty": 178, "units": "count/min"}},
            "heartRateData": [
                {"date": "2024-02-06 07:01:00 -0800", "Min": 120, "Avg": 130, "Max": 140, "units": "count/min", "source": "Apple Watch"},
                {"date": "2024-02-06 07:02:00 -0800", "Min": 125, "Avg": 135, "Max": 145, "units": "count/min", "source": "Apple Watch"}
            ],
            "route": [
                {"latitude": 37.33, "longitude": -122.03, "altitude": 12.5, "timestamp": "2024-02-06 07:00:01 -0800"},
                {"latitude": 37.331, "longitude": -122.031, "altitude": 13.0, "timestamp": "2024-02-06 07:00:02 -0800"},
                {"latitude": 137.0, "longitude": 0.0, "timestamp": "2024-02-06 07:00:03 -0800"}
            ]
        }]}});
        let batch = parse(&body, &headers()).unwrap();
        assert_eq!(batch.workouts.len(), 1);
        let w = &batch.workouts[0];
        assert_eq!(w.activity, Activity::Running);
        assert_eq!(w.name, "Outdoor Run");
        assert_eq!(w.start, at("2024-02-06T15:00:00Z"));
        assert_eq!(w.end, at("2024-02-06T15:45:00Z"));
        assert_eq!(w.offset_min, -480);
        assert!(near(w.active_energy_kcal, 100.0));
        assert_eq!(w.total_energy_kcal, Some(500.0));
        assert!(near(w.distance_m, 5200.0));
        assert!(near(w.elevation_up_m, 30.48));
        assert_eq!((w.avg_hr, w.max_hr), (Some(152.0), Some(178.0)));
        assert_eq!(w.indoor, Some(false));
        assert_eq!(w.source, "Apple Watch");
        assert_eq!(w.external_id, "A1B2C3D4");
        assert!(w.heart_rate.is_empty(), "heartRateData is aggregates, not raw samples");
        assert_eq!(w.route.len(), 2, "the fix at latitude 137 is not on Earth");
        assert_eq!(w.route[0].alt, Some(12.5));
    }

    #[test]
    fn a_version_1_workout_is_read_too() {
        let body = json!({"data": {"workouts": [{
            "name": "Walk",
            "start": "2024-02-06 07:00:00 -0800",
            "end": "2024-02-06 07:30:00 -0800",
            "activeEnergy": {"qty": 120, "units": "kcal"},
            "avgHeartRate": {"qty": 101, "units": "bpm"},
            "maxHeartRate": {"qty": 118, "units": "bpm"},
            "route": [{"lat": 37.33, "lon": -122.03, "altitude": 3, "timestamp": "2024-02-06 07:00:01 -0800"}]
        }]}});
        let w = &parse(&body, &headers()).unwrap().workouts[0];
        assert_eq!(w.activity, Activity::Walking);
        assert_eq!(w.active_energy_kcal, Some(120.0));
        assert_eq!((w.avg_hr, w.max_hr), (Some(101.0), Some(118.0)));
        assert_eq!(w.route.len(), 1);
        assert_eq!(w.indoor, None);
    }

    #[test]
    fn workout_names_map_to_activities() {
        for (name, activity) in [
            ("Outdoor Run", Activity::Running),
            ("Indoor Run", Activity::Running),
            ("Walk", Activity::Walking),
            ("Cycling", Activity::Cycling),
            ("Pool Swim", Activity::Swimming),
            ("Open Water Swim", Activity::Swimming),
            ("Hiking", Activity::Hiking),
            ("Traditional Strength Training", Activity::StrengthTraining),
            ("Functional Strength Training", Activity::FunctionalTraining),
            ("Core Training", Activity::CoreTraining),
            ("High Intensity Interval Training", Activity::Hiit),
            ("Yoga", Activity::Yoga),
            ("Pilates", Activity::Pilates),
            ("Elliptical", Activity::Elliptical),
            ("Rowing", Activity::Rowing),
            ("Stair Stepper", Activity::StairClimbing),
            ("Stairs", Activity::StairClimbing),
            ("Dance", Activity::Dance),
            ("Cooldown", Activity::Cooldown),
            ("Mind and Body", Activity::Mindfulness),
            ("Throwing Sports", Activity::Other),
            ("Kickboxing", Activity::Other),
        ] {
            assert_eq!(activity_from_name(name), activity, "{name}");
        }
    }

    #[test]
    fn a_workout_with_no_end_uses_its_duration() {
        let body = json!({"data": {"workouts": [
            {"name": "Yoga", "start": "2024-02-06 07:00:00 -0800", "duration": 1800},
            {"name": "Yoga", "duration": 1800}
        ]}});
        let batch = parse(&body, &headers()).unwrap();
        assert_eq!(batch.workouts.len(), 1);
        assert_eq!(batch.workouts[0].end, at("2024-02-06T15:30:00Z"));
        assert_eq!(batch.rejected, 1);
    }

    #[test]
    fn taken_and_skipped_doses_come_in_and_the_rest_are_counted() {
        let body = json!({"data": {"medications": [
            {"displayText": "Levothyroxine", "nickname": "Thyroid pill", "start": "2024-02-06 08:05:00 -0800",
             "end": "2024-02-06 08:05:00 -0800", "scheduledDate": "2024-02-06 08:00:00 -0800", "status": "Taken",
             "isArchived": false, "dosage": 1, "scheduledDosage": 1, "units": "count",
             "codings": [{"code": "966247", "system": "http://www.nlm.nih.gov/research/umls/rxnorm", "version": ""}]},
            {"displayText": "Metformin", "start": "2024-02-06 20:00:00 -0800", "status": "Skipped", "codings": []},
            {"displayText": "Metformin", "start": "2024-02-07 20:00:00 -0800", "status": "Not Logged", "codings": []},
            {"displayText": "Metformin", "start": "2024-02-08 20:00:00 -0800", "status": "Snoozed", "codings": []},
            {"status": "Taken", "start": "2024-02-08 20:00:00 -0800"}
        ]}});
        let batch = parse(&body, &headers()).unwrap();
        assert_eq!(batch.doses.len(), 2);
        let taken = &batch.doses[0];
        assert_eq!((taken.name.as_str(), taken.status), ("Levothyroxine", DoseStatus::Taken));
        assert_eq!(taken.at, at("2024-02-06T16:05:00Z"));
        assert_eq!(taken.scheduled_at, Some(at("2024-02-06T16:00:00Z")));
        assert_eq!((taken.amount, taken.unit.as_str()), (Some(1.0), "count"));
        assert_eq!(taken.codes.len(), 1);
        let code = &taken.codes[0];
        assert_eq!((code.system.as_str(), code.code.as_str()), ("rxnorm", "966247"));
        assert_eq!(batch.doses[1].status, DoseStatus::Skipped);
        assert_eq!(batch.skipped.get("medication:Not Logged"), Some(&1));
        assert_eq!(batch.skipped.get("medication:Snoozed"), Some(&1));
        assert_eq!(batch.rejected, 1, "a dose with no name");
    }

    #[test]
    fn the_other_arrays_are_counted_by_name() {
        let body = json!({"data": {
            "stateOfMind": [{"id": 1}, {"id": 2}],
            "symptoms": [{"name": "Headache"}],
            "ecg": [],
            "heartRateNotifications": [{"x": 1}]
        }});
        let batch = parse(&body, &headers()).unwrap();
        assert!(batch.is_empty());
        assert_eq!(batch.skipped.get("stateOfMind"), Some(&2));
        assert_eq!(batch.skipped.get("symptoms"), Some(&1));
        assert_eq!(batch.skipped.get("heartRateNotifications"), Some(&1));
        assert_eq!(batch.skipped.get("ecg"), None);
    }

    #[test]
    fn a_summarised_push_is_refused_whatever_the_case() {
        let body = json!({"data": {"metrics": []}});
        for aggregation in ["hours", "Days", " WEEKS ", "months", "years"] {
            let h = PushHeaders { automation_aggregation: aggregation.into(), ..headers() };
            assert!(matches!(parse(&body, &h), Err(Error::Invalid(_))), "{aggregation:?}");
        }
        for aggregation in ["", "default", "seconds", "minutes"] {
            let h = PushHeaders { automation_aggregation: aggregation.into(), ..headers() };
            assert!(parse(&body, &h).is_ok(), "{aggregation:?}");
        }
    }

    #[test]
    fn a_body_without_data_is_invalid() {
        for body in [json!({}), json!({"metrics": []}), json!({"data": []}), json!([1, 2])] {
            assert!(matches!(parse(&body, &headers()), Err(Error::Invalid(_))), "{body}");
        }
    }
}
