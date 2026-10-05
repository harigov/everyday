//! What's in the air, beside what it is doing.
//!
//! [`crate::weather`] answers "what is the sky doing"; this answers the
//! question beside it, over a second, equally keyless Open-Meteo host --
//! `https://air-quality-api.open-meteo.com` -- that knows nothing about
//! place names and answers only to a latitude and longitude, the same two
//! numbers [`crate::weather::forecast_url`] already asked the forecast for.
//! Split into its own module for the reason [`crate::weather`] itself is
//! split from `everyday_service::weather`: what AQI 142 means in words, and
//! which of the two numbers Open-Meteo sends back is the one to show, are
//! questions this file answers without a socket, and a canned response is
//! enough to test either one.
//!
//! # Best-effort
//!
//! The forecast and the air quality are two requests to two hosts, and the
//! second is allowed to fail on its own: `everyday_service::weather::forecast`
//! catches whatever this module's [`parse`] refuses, or whatever the socket
//! never answered at all, logs it, and returns the weather anyway with
//! [`AirQualityReport`] simply absent. A smoggy day is worth knowing about; a
//! forecast withheld because the smaller of two requests timed out is not.
//!
//! # Which scale
//!
//! Open-Meteo reports two AQI numbers for the same air: the US EPA's and the
//! Europe-wide one, which do not agree on a 0-500 or a 0-100-plus range, let
//! alone which number earns which category word. [`AqiScale::customary_in`]
//! picks the one a place would actually use for itself, the same
//! simplification [`crate::weather::Units::customary_in`] already makes for
//! temperature: American for the United States, European everywhere else --
//! wrong for, say, India or China, exactly as "metric everywhere but the US,
//! Liberia and Myanmar" is wrong for a thermometer in Monrovia, and for the
//! same reason: Open-Meteo hands back only these two, and picking between
//! them reads better than refusing to. Both raw numbers stay on [`Current`]
//! regardless, so nothing is thrown away by the guess.
//!
//! # What leaves the machine
//!
//! The same latitude and longitude [`crate::weather`] already sent the
//! forecast host -- nothing that names the place, no account, nothing else.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The air quality API's own forecast limit -- shorter than the sixteen days
/// [`crate::weather::MAX_DAYS`] allows, because Open-Meteo's air quality
/// model simply does not look as far ahead as its weather one does.
pub const MAX_DAYS: u8 = 7;

/// As [`crate::weather::MAX_RESPONSE_BYTES`], but smaller: this response has
/// no sixteen-day column of its own, only a current reading and an hourly
/// one no longer than [`MAX_DAYS`] covers.
pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;

pub const PROVIDER: &str = "Open-Meteo";

/// The air quality host's address for `latitude`/`longitude`, `days` days
/// ahead -- current conditions plus the hourly columns [`parse`] folds into
/// [`AirQualityReport::daily`].
///
/// `timezone=auto`, like [`crate::weather::forecast_url`], so the hourly
/// timestamps [`parse`] groups by date are the place's own local days and
/// not UTC's.
pub fn url(latitude: f64, longitude: f64, days: u8) -> String {
    format!(
        "https://air-quality-api.open-meteo.com/v1/air-quality?latitude={latitude}&longitude={longitude}\
         &current=us_aqi,european_aqi,pm2_5,pm10,ozone,nitrogen_dioxide\
         &hourly=us_aqi,european_aqi&timezone=auto&forecast_days={}",
        days.clamp(1, MAX_DAYS),
    )
}

/// Which of Open-Meteo's two AQI numbers is the one to show, without being
/// asked -- see the module doc's "Which scale".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AqiScale {
    Us,
    European,
}

impl AqiScale {
    /// What somebody in `country_code` would expect to be shown, when nobody
    /// said. See the module doc's "Which scale" for the honest limits of
    /// this guess.
    pub fn customary_in(country_code: &str) -> Self {
        match country_code.trim().to_ascii_uppercase().as_str() {
            "US" => Self::Us,
            _ => Self::European,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Us => "US AQI",
            Self::European => "European AQI",
        }
    }
}

/// What's in the air right now, plus the forecast's daily worst. Attached to
/// [`crate::weather::Report::air_quality`] by `everyday_service::weather`,
/// never by this module -- see that field's own doc.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AirQualityReport {
    pub current: Option<Current>,
    /// Today first, aligned by date with [`crate::weather::Report::days`].
    /// Shorter than that list, or empty, when the hourly columns this is
    /// folded from did not reach as far -- see [`MAX_DAYS`].
    pub daily: Vec<Daily>,
}

/// Air quality right now. As with [`crate::weather::Current`], every number
/// is optional because a station can stop reporting one pollutant and keep
/// reporting the rest.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Current {
    /// Local time, matching [`crate::weather::Current::time`].
    pub time: String,
    pub us_aqi: Option<i64>,
    pub european_aqi: Option<i64>,
    /// Which of the two numbers above [`Self::aqi`] repeats, and
    /// [`Self::category`] is in words for.
    pub scale: AqiScale,
    /// `us_aqi` or `european_aqi`, whichever [`scale`](Self::scale) names --
    /// here too, so a caller that does not care which scale is in play can
    /// read one field rather than branch on `scale` itself.
    pub aqi: Option<i64>,
    /// [`aqi`](Self::aqi), in words: "Good", "Moderate", "Unhealthy for
    /// sensitive groups" and so on -- see [`us_category`]/[`european_category`].
    pub category: Option<String>,
    /// Micrograms per cubic metre.
    ///
    /// `rename`d by hand rather than left to `rename_all`'s own camelCase
    /// rule: fed "pm2_5", that rule's digit-then-underscore case produces
    /// "pm25" -- correct, but not something a reader should have to work
    /// out from the rule rather than see written down.
    #[serde(rename = "pm25")]
    pub pm2_5: Option<f64>,
    pub pm10: Option<f64>,
    pub ozone: Option<f64>,
    pub nitrogen_dioxide: Option<f64>,
}

/// One day's worst hourly reading, in [`Current::scale`]'s own units.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Daily {
    /// `2026-10-04`.
    pub date: String,
    pub max_aqi: Option<i64>,
}

/// The EPA's six US AQI bands (0-500). Pure and tested on their own, per the
/// house rule that a number's meaning in words should not depend on a
/// network to check.
pub fn us_category(aqi: i64) -> &'static str {
    match aqi {
        ..=50 => "Good",
        51..=100 => "Moderate",
        101..=150 => "Unhealthy for sensitive groups",
        151..=200 => "Unhealthy",
        201..=300 => "Very unhealthy",
        _ => "Hazardous",
    }
}

/// The CAMS European Air Quality Index's six bands (0-100-plus).
pub fn european_category(aqi: i64) -> &'static str {
    match aqi {
        ..=19 => "Good",
        20..=39 => "Fair",
        40..=59 => "Moderate",
        60..=79 => "Poor",
        80..=99 => "Very poor",
        _ => "Extremely poor",
    }
}

/// `aqi`, in words, on whichever of the two scales above `scale` names.
pub fn category(scale: AqiScale, aqi: i64) -> &'static str {
    match scale {
        AqiScale::Us => us_category(aqi),
        AqiScale::European => european_category(aqi),
    }
}

/// Read an air quality response for a place in `country_code` into an
/// [`AirQualityReport`].
///
/// An answer Open-Meteo itself marks as an error -- the same `{"error":
/// true, "reason": "..."}` shape [`crate::weather::parse_forecast`] reads --
/// becomes [`Error::Invalid`] carrying its reason.
pub fn parse(body: &str, country_code: &str) -> Result<AirQualityReport> {
    #[derive(Deserialize)]
    struct Response {
        current: Option<RawCurrent>,
        hourly: Option<RawHourly>,
        #[serde(default)]
        error: bool,
        #[serde(default)]
        reason: String,
    }
    #[derive(Deserialize)]
    struct RawCurrent {
        #[serde(default)]
        time: String,
        us_aqi: Option<f64>,
        european_aqi: Option<f64>,
        pm2_5: Option<f64>,
        pm10: Option<f64>,
        ozone: Option<f64>,
        nitrogen_dioxide: Option<f64>,
    }
    #[derive(Deserialize)]
    struct RawHourly {
        #[serde(default)]
        time: Vec<String>,
        #[serde(default)]
        us_aqi: Vec<Option<f64>>,
        #[serde(default)]
        european_aqi: Vec<Option<f64>>,
    }

    let parsed: Response = serde_json::from_str(body)
        .map_err(|e| Error::Invalid(format!("air quality could not be read: {e}")))?;
    if parsed.error {
        let reason = parsed.reason.trim();
        return Err(Error::Invalid(if reason.is_empty() {
            "air quality was refused".into()
        } else {
            format!("air quality was refused: {reason}")
        }));
    }

    let scale = AqiScale::customary_in(country_code);
    let current = parsed.current.map(|c| {
        let us_aqi = c.us_aqi.map(|v| v.round() as i64);
        let european_aqi = c.european_aqi.map(|v| v.round() as i64);
        let aqi = match scale {
            AqiScale::Us => us_aqi,
            AqiScale::European => european_aqi,
        };
        Current {
            time: c.time,
            us_aqi,
            european_aqi,
            scale,
            aqi,
            category: aqi.map(|v| category(scale, v).to_string()),
            pm2_5: c.pm2_5,
            pm10: c.pm10,
            ozone: c.ozone,
            nitrogen_dioxide: c.nitrogen_dioxide,
        }
    });

    // Columns to days: the day a hint belongs to is the first ten characters
    // of its own local timestamp ("2026-10-04T13:00" -> "2026-10-04"), and
    // the day's figure is the highest hourly reading on whichever scale is
    // in play here -- a null hour is a missing reading, not a zero that
    // would drag a smoggy afternoon's maximum down.
    let daily = parsed
        .hourly
        .map(|h| {
            let column: &[Option<f64>] = match scale {
                AqiScale::Us => &h.us_aqi,
                AqiScale::European => &h.european_aqi,
            };
            let mut order: Vec<String> = Vec::new();
            let mut max: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
            for (time, value) in h.time.iter().zip(column.iter()) {
                let Some(v) = value else { continue };
                let date = time.get(..10).unwrap_or(time).to_string();
                max.entry(date.clone())
                    .and_modify(|m| {
                        if *v > *m {
                            *m = *v;
                        }
                    })
                    .or_insert_with(|| {
                        order.push(date.clone());
                        *v
                    });
            }
            order
                .into_iter()
                .map(|date| Daily { max_aqi: max.get(&date).map(|v| v.round() as i64), date })
                .collect()
        })
        .unwrap_or_default();

    Ok(AirQualityReport { current, daily })
}

#[cfg(test)]
mod tests {
    use super::*;

    const AIR_QUALITY: &str = r#"{
        "latitude": 47.6, "longitude": -122.3, "generationtime_ms": 0.9,
        "utc_offset_seconds": -25200, "timezone": "America/Los_Angeles",
        "timezone_abbreviation": "PDT", "elevation": 17.0,
        "current_units": {"time": "iso8601", "us_aqi": "USAQI"},
        "current": {"time": "2026-10-05T07:00", "interval": 3600, "us_aqi": 56,
                    "european_aqi": 44, "pm2_5": 10.6, "pm10": 10.7, "ozone": 12.0,
                    "nitrogen_dioxide": 31.3},
        "hourly_units": {"time": "iso8601"},
        "hourly": {
            "time": ["2026-10-05T00:00", "2026-10-05T01:00", "2026-10-06T00:00", "2026-10-06T01:00"],
            "us_aqi": [54, 60, null, 70],
            "european_aqi": [40, 44, 38, 39]
        }
    }"#;

    #[test]
    fn the_url_asks_both_scales_and_both_hourly_columns() {
        let asked = url(47.60621, -122.33207, 20);
        assert!(asked.starts_with(
            "https://air-quality-api.open-meteo.com/v1/air-quality?latitude=47.60621&longitude=-122.33207"
        ));
        assert!(asked.contains("&current=us_aqi,european_aqi,pm2_5,pm10,ozone,nitrogen_dioxide&"));
        assert!(asked.contains("&hourly=us_aqi,european_aqi&"));
        assert!(asked.contains("&forecast_days=7"), "clamped to what this API serves");
        assert!(url(1.0, 2.0, 0).contains("&forecast_days=1"));
    }

    #[test]
    fn a_scale_is_customary_the_same_way_units_are() {
        assert_eq!(AqiScale::customary_in("us"), AqiScale::Us);
        assert_eq!(AqiScale::customary_in("US"), AqiScale::Us);
        assert_eq!(AqiScale::customary_in("GB"), AqiScale::European);
        assert_eq!(AqiScale::customary_in(""), AqiScale::European);
    }

    #[test]
    fn the_us_bands_cover_every_epa_category() {
        assert_eq!(us_category(0), "Good");
        assert_eq!(us_category(50), "Good");
        assert_eq!(us_category(51), "Moderate");
        assert_eq!(us_category(100), "Moderate");
        assert_eq!(us_category(101), "Unhealthy for sensitive groups");
        assert_eq!(us_category(150), "Unhealthy for sensitive groups");
        assert_eq!(us_category(151), "Unhealthy");
        assert_eq!(us_category(200), "Unhealthy");
        assert_eq!(us_category(201), "Very unhealthy");
        assert_eq!(us_category(300), "Very unhealthy");
        assert_eq!(us_category(301), "Hazardous");
        assert_eq!(us_category(500), "Hazardous");
    }

    #[test]
    fn the_european_bands_cover_every_eaqi_category() {
        assert_eq!(european_category(0), "Good");
        assert_eq!(european_category(19), "Good");
        assert_eq!(european_category(20), "Fair");
        assert_eq!(european_category(39), "Fair");
        assert_eq!(european_category(40), "Moderate");
        assert_eq!(european_category(59), "Moderate");
        assert_eq!(european_category(60), "Poor");
        assert_eq!(european_category(79), "Poor");
        assert_eq!(european_category(80), "Very poor");
        assert_eq!(european_category(99), "Very poor");
        assert_eq!(european_category(100), "Extremely poor");
    }

    #[test]
    fn a_response_reads_into_the_scale_the_country_picks() {
        let us = parse(AIR_QUALITY, "US").unwrap();
        let now = us.current.as_ref().unwrap();
        assert_eq!(now.time, "2026-10-05T07:00");
        assert_eq!(now.us_aqi, Some(56));
        assert_eq!(now.european_aqi, Some(44));
        assert_eq!(now.scale, AqiScale::Us);
        assert_eq!(now.aqi, Some(56), "the US number, since the place is in the US");
        assert_eq!(now.category.as_deref(), Some("Moderate"));
        assert_eq!(now.pm2_5, Some(10.6));

        let fr = parse(AIR_QUALITY, "FR").unwrap();
        let now_fr = fr.current.as_ref().unwrap();
        assert_eq!(now_fr.aqi, Some(44), "the European number outside the US");
        assert_eq!(now_fr.category.as_deref(), Some("Moderate"), "44 is in the 40-59 band");
    }

    #[test]
    fn the_daily_maximum_is_folded_from_the_hourly_column_by_local_date() {
        let us = parse(AIR_QUALITY, "US").unwrap();
        assert_eq!(us.daily.len(), 2, "two local dates in the fixture");
        assert_eq!(us.daily[0].date, "2026-10-05");
        assert_eq!(us.daily[0].max_aqi, Some(60), "the higher of the two hours, not the first");
        assert_eq!(us.daily[1].date, "2026-10-06");
        assert_eq!(us.daily[1].max_aqi, Some(70), "a null hour is skipped, not read as zero");
    }

    #[test]
    fn the_report_serialises_in_camel_case() {
        let report = parse(AIR_QUALITY, "US").unwrap();
        let json = serde_json::to_value(&report).unwrap();
        for key in [
            "time",
            "usAqi",
            "europeanAqi",
            "scale",
            "aqi",
            "category",
            "pm25",
            "pm10",
            "ozone",
            "nitrogenDioxide",
        ] {
            assert!(json["current"].get(key).is_some(), "current is missing {key}");
        }
        assert!(json["current"].get("pm2_5").is_none(), "the digit case is spelled out by hand");
        assert_eq!(json["current"]["scale"], serde_json::json!("us"));
        for key in ["date", "maxAqi"] {
            assert!(json["daily"][0].get(key).is_some(), "a day is missing {key}");
        }
    }

    #[test]
    fn an_error_body_becomes_a_sentence() {
        let body = r#"{"error": true, "reason": "Latitude must be in range of -90 to 90°."}"#;
        let err = parse(body, "US").unwrap_err();
        assert!(err.to_string().contains("Latitude must be in range"), "got {err}");
    }

    #[test]
    fn a_response_with_neither_block_is_an_empty_report_not_an_error() {
        let report = parse(r#"{"latitude": 1.0, "longitude": 2.0}"#, "US").unwrap();
        assert!(report.current.is_none());
        assert!(report.daily.is_empty());
    }
}
