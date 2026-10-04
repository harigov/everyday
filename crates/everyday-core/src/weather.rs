//! The weather, except the socket.
//!
//! The assistant's `get_weather` tool asks [Open-Meteo](https://open-meteo.com)
//! two questions -- where is this place, and what is the sky going to do
//! there -- and this module is everything about those two questions that
//! does not need a network: the addresses they are asked at, what the
//! answers mean, which of several places called "Portland" somebody meant,
//! and what WMO code 63 is in words. `everyday_service::weather` opens the
//! connection and hands the bytes back here. It is the split
//! [`crate::websearch`] makes for the same reason: "the forecast said
//! Portland, Maine to somebody in Oregon" is a test in this file rather than
//! a network capture.
//!
//! # Why Open-Meteo
//!
//! It needs no key and no account, which is the rule every outward lookup in
//! this application keeps -- see [`crate::websearch`]'s own "Which sources,
//! and why these". A weather feature that stops working the day a free tier
//! is withdrawn, or that is off until somebody pastes a key into a settings
//! box, should not have shipped. Open-Meteo also does its own geocoding over
//! the same keyless interface, so one provider answers both questions and
//! only one stranger's server learns which city somebody is asking about.
//!
//! # What leaves the machine
//!
//! A place name, to the geocoder, and then a latitude and longitude rounded
//! to what the geocoder itself answered, to the forecast. Nothing else: no
//! identifier, no history, and the place is the one the person named or the
//! one they typed into Settings → About You as roughly where they live.
//!
//! # Matching a place
//!
//! Open-Meteo's geocoder matches *names*, not addresses: "Seattle" finds
//! Seattle, and "Seattle, WA" finds nothing at all, because no place is
//! called that. People write the second. So [`lookups`] tries the whole
//! string first -- a place whose name really does contain a comma loses
//! nothing -- and then the part before the first comma, with everything
//! after it kept as *qualifiers* that [`choose`] prefers a result by:
//! "Portland, OR" is Portland in Oregon even though Portland, Oregon and
//! Portland, Maine both answer to "Portland". A qualifier that matches
//! nothing is not an error -- "Paris, the one in France" still finds Paris --
//! and the geocoder's own first answer, which it ranks by population, is
//! the fallback, because that is what somebody who says just "Paris" means
//! nine times in ten.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::websearch::encode;

/// The furthest ahead Open-Meteo forecasts.
pub const MAX_DAYS: u8 = 16;

/// How many days a forecast covers when nobody said: today and the next two,
/// which is what "what's the weather doing" means.
pub const DEFAULT_DAYS: u8 = 3;

/// Largest response either endpoint is trusted to send. A sixteen-day
/// forecast is a few kilobytes; a megabyte is somebody else's problem.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Who answers, for the sentences a failure turns into.
pub const PROVIDER: &str = "Open-Meteo";

/// How many places to ask the geocoder for on the first, whole-string try.
///
/// Small, because a whole string that matches anything at all is usually
/// specific enough that the first answer is the one meant.
const FIRST_TRY: u32 = 5;

/// How many to ask for once the string has been cut at its first comma.
///
/// Larger than [`FIRST_TRY`]: now the qualifiers decide, and "Springfield,
/// IL" is no help if Illinois's Springfield is the seventh most populous of
/// them and only five came back.
const QUALIFIED_TRY: u32 = 10;

// ── Units ────────────────────────────────────────────────────────────────

/// Which units to forecast in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Units {
    /// Celsius, kilometres an hour, millimetres.
    Metric,
    /// Fahrenheit, miles an hour, inches.
    Imperial,
}

impl Units {
    /// Read a model's spelling of it. `None` for anything else, so the caller
    /// can say which values exist rather than guessing.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "metric" => Some(Self::Metric),
            "imperial" => Some(Self::Imperial),
            _ => None,
        }
    }

    /// What somebody in `country_code` would expect, when nobody said.
    ///
    /// The tool's description asks the model to choose, and a model usually
    /// does. When it does not, the answer should still not be a temperature
    /// in Celsius read aloud to somebody in Ohio: the United States, Liberia
    /// and Myanmar are the places that do not use metric for the weather,
    /// and everywhere else does.
    pub fn customary_in(country_code: &str) -> Self {
        match country_code.trim().to_ascii_uppercase().as_str() {
            "US" | "LR" | "MM" => Self::Imperial,
            _ => Self::Metric,
        }
    }

    /// The labels each number in a [`Report`] is in.
    pub fn labels(self) -> UnitLabels {
        match self {
            Self::Metric => UnitLabels { temperature: "°C", wind: "km/h", precipitation: "mm" },
            Self::Imperial => UnitLabels { temperature: "°F", wind: "mph", precipitation: "in" },
        }
    }

    /// The query-string suffix that asks Open-Meteo for these units. Empty
    /// for metric, which is its default.
    fn query(self) -> &'static str {
        match self {
            Self::Metric => "",
            Self::Imperial => {
                "&temperature_unit=fahrenheit&wind_speed_unit=mph&precipitation_unit=inch"
            }
        }
    }
}

/// The unit beside every number a [`Report`] carries, spelled for a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct UnitLabels {
    pub temperature: &'static str,
    pub wind: &'static str,
    pub precipitation: &'static str,
}

// ── Finding the place ────────────────────────────────────────────────────

/// One question for the geocoder: a name to look up, and what to prefer
/// among the answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lookup {
    /// What is sent.
    pub name: String,
    /// What is *not* sent but decides between results -- the parts after
    /// the first comma, trimmed. See [`choose`].
    pub qualifiers: Vec<String>,
    /// How many results to ask for.
    pub count: u32,
}

impl Lookup {
    /// The geocoder's address for this question.
    pub fn url(&self) -> String {
        format!(
            "https://geocoding-api.open-meteo.com/v1/search?name={}&count={}&language=en&format=json",
            encode(&self.name),
            self.count
        )
    }
}

/// The questions to ask, in order, for what somebody typed. See the module
/// docs for why there are two.
///
/// Empty when there is nothing to ask about at all.
pub fn lookups(place: &str) -> Vec<Lookup> {
    let whole = collapse(place);
    if whole.is_empty() {
        return Vec::new();
    }
    let mut out = vec![Lookup { name: whole.clone(), qualifiers: Vec::new(), count: FIRST_TRY }];
    if let Some((head, rest)) = whole.split_once(',') {
        let head = head.trim();
        let qualifiers: Vec<String> =
            rest.split(',').map(str::trim).filter(|q| !q.is_empty()).map(str::to_string).collect();
        if !head.is_empty() {
            out.push(Lookup { name: head.to_string(), qualifiers, count: QUALIFIED_TRY });
        }
    }
    out
}

/// One place the geocoder knows. Only the fields this module reads; the
/// response carries a dozen more.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Place {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    /// The first-level division: a US state, an English county's country
    /// ("England"), a French région. Often what a qualifier names.
    #[serde(default)]
    pub admin1: String,
    #[serde(default)]
    pub country: String,
    /// ISO 3166-1 alpha-2, upper case.
    #[serde(default)]
    pub country_code: String,
    #[serde(default)]
    pub timezone: String,
}

impl Place {
    /// "Seattle, Washington, United States" -- the name, the region and the
    /// country, leaving out whichever are missing or repeat the one before
    /// ("Singapore, Singapore" is one place said twice).
    pub fn label(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        for part in [self.name.trim(), self.admin1.trim(), self.country.trim()] {
            if !part.is_empty() && parts.last().is_none_or(|last| !last.eq_ignore_ascii_case(part))
            {
                parts.push(part);
            }
        }
        parts.join(", ")
    }

    /// Whether `qualifier` names this place's region or country, by name, by
    /// ISO code, or by a US state's postal abbreviation.
    fn answers_to(&self, qualifier: &str) -> bool {
        let q = qualifier.trim().trim_end_matches('.');
        if q.is_empty() {
            return false;
        }
        let same = |s: &str| !s.trim().is_empty() && s.trim().eq_ignore_ascii_case(q);
        if same(&self.admin1) || same(&self.country) || same(&self.country_code) {
            return true;
        }
        // "WA" is Washington, but only in the United States: a two-letter
        // qualifier is read as a state only where states are what admin1
        // holds, so "Perth, WA" is not taken to mean Washington.
        if self.country_code.eq_ignore_ascii_case("US")
            && let Some(state) = us_state(q)
            && self.admin1.trim().eq_ignore_ascii_case(state)
        {
            return true;
        }
        country_alias(q).is_some_and(|code| self.country_code.eq_ignore_ascii_case(code))
    }
}

/// Read the geocoder's answer. A name it has never heard of is an empty list
/// -- Open-Meteo leaves `results` out altogether -- not an error.
pub fn parse_places(body: &str) -> Result<Vec<Place>> {
    #[derive(Deserialize)]
    struct Response {
        #[serde(default)]
        results: Vec<Place>,
    }
    let parsed: Response = serde_json::from_str(body)
        .map_err(|e| Error::Invalid(format!("the geocoder's answer could not be read: {e}")))?;
    Ok(parsed.results)
}

/// Which of `places` was meant, given the qualifiers that came with the name.
///
/// The first one that answers to *every* qualifier, else the first that
/// answers to any, else the first -- the geocoder ranks by population, which
/// is the right guess for a bare name. `None` only when there was nothing to
/// choose from.
pub fn choose(places: Vec<Place>, qualifiers: &[String]) -> Option<Place> {
    if qualifiers.is_empty() {
        return places.into_iter().next();
    }
    let all = places.iter().position(|p| qualifiers.iter().all(|q| p.answers_to(q)));
    let any = || places.iter().position(|p| qualifiers.iter().any(|q| p.answers_to(q)));
    let index = all.or_else(any).unwrap_or(0);
    places.into_iter().nth(index)
}

// ── The forecast ─────────────────────────────────────────────────────────

/// The forecast's address for `place`, `days` days ahead, in `units`.
///
/// `timezone=auto` so every time in the answer is the place's own local
/// time, which is what "sunset is at 18:42" has to mean.
pub fn forecast_url(place: &Place, days: u8, units: Units) -> String {
    format!(
        "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}\
         &current=temperature_2m,apparent_temperature,relative_humidity_2m,precipitation,weather_code,wind_speed_10m\
         &daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max,precipitation_sum,sunrise,sunset\
         &timezone=auto&forecast_days={}{}",
        place.latitude,
        place.longitude,
        days.clamp(1, MAX_DAYS),
        units.query(),
    )
}

/// What the tool hands the model. Field names are the wire: see the
/// assistant tool's own description in `everyday_service::agent`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    /// "Seattle, Washington, United States". See [`Place::label`].
    pub place: String,
    pub latitude: f64,
    pub longitude: f64,
    /// The place's own zone, which every time below is in.
    pub timezone: String,
    pub units: UnitLabels,
    /// Right now. `None` only if the forecast came back without it.
    pub current: Option<Current>,
    /// Today first.
    pub days: Vec<Day>,
}

/// Conditions now. Every number is optional because Open-Meteo sends `null`
/// for a variable a station has stopped reporting, and a missing humidity is
/// no reason to throw the temperature away.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Current {
    /// Local time, `2026-10-04T10:15`.
    pub time: String,
    pub temperature: Option<f64>,
    pub feels_like: Option<f64>,
    /// Percent.
    pub humidity: Option<i64>,
    pub precipitation: Option<f64>,
    pub wind_speed: Option<f64>,
    /// See [`describe`].
    pub condition: String,
}

/// One day of the forecast.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Day {
    /// `2026-10-04`.
    pub date: String,
    pub condition: String,
    pub high: Option<f64>,
    pub low: Option<f64>,
    /// Percent: the day's highest hourly chance.
    pub precipitation_chance: Option<i64>,
    /// The day's total.
    pub precipitation: Option<f64>,
    /// Local time.
    pub sunrise: Option<String>,
    pub sunset: Option<String>,
}

/// Read a forecast for `place` and shape it as a [`Report`].
///
/// An answer Open-Meteo itself marks as an error -- `{"error": true,
/// "reason": "..."}`, which it sends with a 400 -- becomes
/// [`Error::Invalid`] carrying its reason, so a caller that read the body of
/// a failed request has a sentence to show rather than a status code.
pub fn parse_forecast(body: &str, place: &Place, units: Units) -> Result<Report> {
    #[derive(Deserialize)]
    struct Response {
        #[serde(default)]
        timezone: String,
        current: Option<RawCurrent>,
        daily: Option<RawDaily>,
        #[serde(default)]
        error: bool,
        #[serde(default)]
        reason: String,
    }
    #[derive(Deserialize)]
    struct RawCurrent {
        #[serde(default)]
        time: String,
        temperature_2m: Option<f64>,
        apparent_temperature: Option<f64>,
        relative_humidity_2m: Option<f64>,
        precipitation: Option<f64>,
        weather_code: Option<f64>,
        wind_speed_10m: Option<f64>,
    }
    #[derive(Deserialize)]
    struct RawDaily {
        #[serde(default)]
        time: Vec<String>,
        #[serde(default)]
        weather_code: Vec<Option<f64>>,
        #[serde(default)]
        temperature_2m_max: Vec<Option<f64>>,
        #[serde(default)]
        temperature_2m_min: Vec<Option<f64>>,
        #[serde(default)]
        precipitation_probability_max: Vec<Option<f64>>,
        #[serde(default)]
        precipitation_sum: Vec<Option<f64>>,
        #[serde(default)]
        sunrise: Vec<Option<String>>,
        #[serde(default)]
        sunset: Vec<Option<String>>,
    }

    let parsed: Response = serde_json::from_str(body)
        .map_err(|e| Error::Invalid(format!("the forecast could not be read: {e}")))?;
    if parsed.error {
        let reason = parsed.reason.trim();
        return Err(Error::Invalid(if reason.is_empty() {
            "the forecast was refused".into()
        } else {
            format!("the forecast was refused: {reason}")
        }));
    }

    let current = parsed.current.map(|c| Current {
        time: c.time,
        temperature: c.temperature_2m,
        feels_like: c.apparent_temperature,
        humidity: c.relative_humidity_2m.map(|h| h.round() as i64),
        precipitation: c.precipitation,
        wind_speed: c.wind_speed_10m,
        condition: describe_code(c.weather_code),
    });

    // Columns rather than rows: `daily` is one array per variable, all the
    // same length when Open-Meteo is well, and read defensively when it is
    // not -- a short column is a missing value on the days it does not
    // reach, not a reason to drop the day.
    let days = parsed
        .daily
        .map(|d| {
            let at = |column: &[Option<f64>], i: usize| column.get(i).copied().flatten();
            let text = |column: &[Option<String>], i: usize| column.get(i).cloned().flatten();
            d.time
                .iter()
                .enumerate()
                .map(|(i, date)| Day {
                    date: date.clone(),
                    condition: describe_code(at(&d.weather_code, i)),
                    high: at(&d.temperature_2m_max, i),
                    low: at(&d.temperature_2m_min, i),
                    precipitation_chance: at(&d.precipitation_probability_max, i)
                        .map(|p| p.round() as i64),
                    precipitation: at(&d.precipitation_sum, i),
                    sunrise: text(&d.sunrise, i),
                    sunset: text(&d.sunset, i),
                })
                .collect()
        })
        .unwrap_or_default();

    let timezone =
        if parsed.timezone.trim().is_empty() { place.timezone.clone() } else { parsed.timezone };
    Ok(Report {
        place: place.label(),
        latitude: place.latitude,
        longitude: place.longitude,
        timezone,
        units: units.labels(),
        current,
        days,
    })
}

/// The reason Open-Meteo gave for refusing a request, if the body is its own
/// error shape. For a caller holding a 400 that wants a sentence.
pub fn error_reason(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let reason = value.get("reason")?.as_str()?.trim();
    (!reason.is_empty()).then(|| reason.to_string())
}

fn describe_code(code: Option<f64>) -> String {
    match code {
        Some(c) if c.is_finite() && c >= 0.0 => describe(c.round() as u16),
        _ => "unknown".into(),
    }
}

/// A WMO weather interpretation code, in words.
///
/// The codes are the WMO's own (table 4677, as Open-Meteo documents the
/// subset it uses). Words rather than the number because the model would
/// otherwise have to remember the table, and would sometimes remember it
/// wrong.
pub fn describe(code: u16) -> String {
    let words = match code {
        0 => "clear sky",
        1 => "mainly clear",
        2 => "partly cloudy",
        3 => "overcast",
        45 | 48 => "fog",
        51 => "light drizzle",
        53 => "drizzle",
        55 => "heavy drizzle",
        56 => "light freezing drizzle",
        57 => "freezing drizzle",
        61 => "light rain",
        63 => "moderate rain",
        65 => "heavy rain",
        66 => "light freezing rain",
        67 => "freezing rain",
        71 => "light snow",
        73 => "moderate snow",
        75 => "heavy snow",
        77 => "snow grains",
        80 => "light rain showers",
        81 => "rain showers",
        82 => "heavy rain showers",
        85 => "light snow showers",
        86 => "heavy snow showers",
        95 => "thunderstorm",
        96 | 99 => "thunderstorm with hail",
        other => return format!("unknown (WMO code {other})"),
    };
    words.to_string()
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Whitespace collapsed to single spaces and trimmed, so "  Seattle ,  WA "
/// is asked about as "Seattle , WA" and cut at its comma cleanly.
fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A US state's (or DC's) full name from its postal abbreviation.
fn us_state(abbreviation: &str) -> Option<&'static str> {
    const STATES: &[(&str, &str)] = &[
        ("AL", "Alabama"),
        ("AK", "Alaska"),
        ("AZ", "Arizona"),
        ("AR", "Arkansas"),
        ("CA", "California"),
        ("CO", "Colorado"),
        ("CT", "Connecticut"),
        ("DE", "Delaware"),
        ("DC", "District of Columbia"),
        ("FL", "Florida"),
        ("GA", "Georgia"),
        ("HI", "Hawaii"),
        ("ID", "Idaho"),
        ("IL", "Illinois"),
        ("IN", "Indiana"),
        ("IA", "Iowa"),
        ("KS", "Kansas"),
        ("KY", "Kentucky"),
        ("LA", "Louisiana"),
        ("ME", "Maine"),
        ("MD", "Maryland"),
        ("MA", "Massachusetts"),
        ("MI", "Michigan"),
        ("MN", "Minnesota"),
        ("MS", "Mississippi"),
        ("MO", "Missouri"),
        ("MT", "Montana"),
        ("NE", "Nebraska"),
        ("NV", "Nevada"),
        ("NH", "New Hampshire"),
        ("NJ", "New Jersey"),
        ("NM", "New Mexico"),
        ("NY", "New York"),
        ("NC", "North Carolina"),
        ("ND", "North Dakota"),
        ("OH", "Ohio"),
        ("OK", "Oklahoma"),
        ("OR", "Oregon"),
        ("PA", "Pennsylvania"),
        ("RI", "Rhode Island"),
        ("SC", "South Carolina"),
        ("SD", "South Dakota"),
        ("TN", "Tennessee"),
        ("TX", "Texas"),
        ("UT", "Utah"),
        ("VT", "Vermont"),
        ("VA", "Virginia"),
        ("WA", "Washington"),
        ("WV", "West Virginia"),
        ("WI", "Wisconsin"),
        ("WY", "Wyoming"),
    ];
    STATES.iter().find(|(abbr, _)| abbr.eq_ignore_ascii_case(abbreviation)).map(|(_, name)| *name)
}

/// The handful of ways people name a country that are neither its English
/// name nor its ISO code: "UK" is `GB`, and "USA" is `US`.
fn country_alias(q: &str) -> Option<&'static str> {
    match q.to_ascii_lowercase().replace('.', "").as_str() {
        "uk" | "united kingdom" | "great britain" | "britain" => Some("GB"),
        "usa" | "us" | "america" | "united states of america" => Some("US"),
        "uae" => Some("AE"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape Open-Meteo's geocoder answers with, trimmed to the fields
    /// that matter and the order it ranks in: population first.
    const PORTLANDS: &str = r#"{
        "results": [
            {"id": 5746545, "name": "Portland", "latitude": 45.52345, "longitude": -122.67621,
             "elevation": 15.0, "feature_code": "PPLA2", "country_code": "US",
             "timezone": "America/Los_Angeles", "population": 652503,
             "country": "United States", "admin1": "Oregon", "admin2": "Multnomah"},
            {"id": 4975802, "name": "Portland", "latitude": 43.65737, "longitude": -70.2589,
             "country_code": "US", "timezone": "America/New_York", "population": 66881,
             "country": "United States", "admin1": "Maine", "admin2": "Cumberland"},
            {"id": 2152668, "name": "Portland", "latitude": -38.34623, "longitude": 141.60383,
             "country_code": "AU", "timezone": "Australia/Melbourne", "population": 9712,
             "country": "Australia", "admin1": "Victoria"}
        ],
        "generationtime_ms": 0.81
    }"#;

    const FORECAST: &str = r#"{
        "latitude": 47.6, "longitude": -122.33, "generationtime_ms": 0.1,
        "utc_offset_seconds": -25200, "timezone": "America/Los_Angeles",
        "timezone_abbreviation": "PDT", "elevation": 56.0,
        "current_units": {"time": "iso8601", "temperature_2m": "°F"},
        "current": {"time": "2026-10-04T10:15", "interval": 900, "temperature_2m": 57.6,
                    "apparent_temperature": 55.2, "relative_humidity_2m": 71,
                    "precipitation": 0.0, "weather_code": 3, "wind_speed_10m": 5.8},
        "daily_units": {"time": "iso8601"},
        "daily": {
            "time": ["2026-10-04", "2026-10-05", "2026-10-06"],
            "weather_code": [3, 61, null],
            "temperature_2m_max": [61.2, 58.0, 60.1],
            "temperature_2m_min": [49.8, 50.3, 48.0],
            "precipitation_probability_max": [10, 80, 35],
            "precipitation_sum": [0.0, 0.31, 0.02],
            "sunrise": ["2026-10-04T07:12", "2026-10-05T07:13", "2026-10-06T07:15"],
            "sunset": ["2026-10-04T18:42", "2026-10-05T18:40"]
        }
    }"#;

    fn seattle() -> Place {
        Place {
            name: "Seattle".into(),
            latitude: 47.60621,
            longitude: -122.33207,
            admin1: "Washington".into(),
            country: "United States".into(),
            country_code: "US".into(),
            timezone: "America/Los_Angeles".into(),
        }
    }

    #[test]
    fn a_bare_name_is_asked_about_once_and_a_qualified_one_twice() {
        let bare = lookups("  Paris ");
        assert_eq!(bare.len(), 1);
        assert_eq!(bare[0].name, "Paris");
        assert!(bare[0].qualifiers.is_empty());

        let qualified = lookups("Seattle,  WA");
        assert_eq!(qualified.len(), 2, "the whole string first, then the name before the comma");
        assert_eq!(qualified[0].name, "Seattle, WA");
        assert_eq!(qualified[1].name, "Seattle");
        assert_eq!(qualified[1].qualifiers, vec!["WA".to_string()]);

        let several = lookups("Springfield, Illinois, USA");
        assert_eq!(several[1].qualifiers, vec!["Illinois".to_string(), "USA".to_string()]);

        assert!(lookups("   ").is_empty(), "nothing to ask about");
        assert_eq!(lookups(", France").len(), 1, "no name before the comma to fall back to");
    }

    #[test]
    fn the_geocoder_url_encodes_the_name_and_asks_in_english() {
        let url = lookups("São Paulo")[0].url();
        assert_eq!(
            url,
            "https://geocoding-api.open-meteo.com/v1/search?name=S%C3%A3o%20Paulo&count=5\
             &language=en&format=json"
        );
    }

    #[test]
    fn an_unknown_name_is_an_empty_list_not_an_error() {
        assert!(parse_places(r#"{"generationtime_ms": 0.3}"#).unwrap().is_empty());
        assert!(parse_places("<html>").is_err(), "but nonsense is");
    }

    #[test]
    fn a_state_abbreviation_picks_the_right_portland() {
        let places = parse_places(PORTLANDS).unwrap();
        let me = choose(places.clone(), &["ME".into()]).unwrap();
        assert_eq!(me.admin1, "Maine");
        let or = choose(places.clone(), &["or".into()]).unwrap();
        assert_eq!(or.admin1, "Oregon");
        let spelled = choose(places.clone(), &["Maine".into()]).unwrap();
        assert_eq!(spelled.admin1, "Maine");
    }

    #[test]
    fn a_country_qualifier_matches_by_name_code_or_common_alias() {
        let places = parse_places(PORTLANDS).unwrap();
        assert_eq!(choose(places.clone(), &["Australia".into()]).unwrap().country_code, "AU");
        assert_eq!(choose(places.clone(), &["au".into()]).unwrap().country_code, "AU");
        // Every Portland here but one is in the US; "USA" keeps the first.
        assert_eq!(choose(places.clone(), &["USA".into()]).unwrap().admin1, "Oregon");
    }

    #[test]
    fn every_qualifier_matching_beats_one_matching() {
        let places = parse_places(PORTLANDS).unwrap();
        let chosen = choose(places, &["United States".into(), "Maine".into()]).unwrap();
        assert_eq!(chosen.admin1, "Maine");
    }

    #[test]
    fn a_qualifier_that_matches_nothing_falls_back_to_the_most_populous() {
        let places = parse_places(PORTLANDS).unwrap();
        let chosen = choose(places, &["the one with the roses".into()]).unwrap();
        assert_eq!(chosen.admin1, "Oregon");
        assert!(choose(Vec::new(), &[]).is_none());
    }

    #[test]
    fn a_two_letter_state_is_only_a_state_in_the_united_states() {
        // Western Australia is also "WA". The qualifier is not read as
        // Washington for a place outside the US, so it matches nothing here
        // and the first result stands rather than being skipped for one
        // in the wrong country.
        let perth = Place {
            name: "Perth".into(),
            admin1: "Western Australia".into(),
            country: "Australia".into(),
            country_code: "AU".into(),
            ..seattle()
        };
        assert!(!perth.answers_to("WA"));
        assert!(seattle().answers_to("WA"));
        assert!(seattle().answers_to("wa."));
    }

    #[test]
    fn a_label_skips_what_is_missing_and_what_repeats() {
        assert_eq!(seattle().label(), "Seattle, Washington, United States");
        let singapore = Place {
            name: "Singapore".into(),
            admin1: String::new(),
            country: "Singapore".into(),
            ..seattle()
        };
        assert_eq!(singapore.label(), "Singapore");
    }

    #[test]
    fn the_forecast_url_asks_for_every_variable_in_the_right_units() {
        let metric = forecast_url(&seattle(), 3, Units::Metric);
        assert!(metric.starts_with(
            "https://api.open-meteo.com/v1/forecast?latitude=47.60621&longitude=-122.33207&current="
        ));
        assert!(metric.contains(
            "&current=temperature_2m,apparent_temperature,relative_humidity_2m,precipitation,\
             weather_code,wind_speed_10m&"
        ));
        assert!(metric.contains(
            "&daily=weather_code,temperature_2m_max,temperature_2m_min,\
             precipitation_probability_max,precipitation_sum,sunrise,sunset&"
        ));
        assert!(metric.ends_with("&timezone=auto&forecast_days=3"));
        assert!(!metric.contains("fahrenheit"));

        let imperial = forecast_url(&seattle(), 40, Units::Imperial);
        assert!(imperial.contains("&forecast_days=16&"), "clamped to what the API serves");
        assert!(
            imperial.ends_with(
                "&temperature_unit=fahrenheit&wind_speed_unit=mph&precipitation_unit=inch"
            )
        );
        assert!(forecast_url(&seattle(), 0, Units::Metric).contains("&forecast_days=1"));
    }

    #[test]
    fn a_forecast_reads_into_a_report_with_words_for_codes() {
        let report = parse_forecast(FORECAST, &seattle(), Units::Imperial).unwrap();
        assert_eq!(report.place, "Seattle, Washington, United States");
        assert_eq!(report.timezone, "America/Los_Angeles");
        assert_eq!(report.units.temperature, "°F");
        assert_eq!(report.units.wind, "mph");
        assert_eq!(report.units.precipitation, "in");

        let now = report.current.as_ref().unwrap();
        assert_eq!(now.time, "2026-10-04T10:15");
        assert_eq!(now.temperature, Some(57.6));
        assert_eq!(now.feels_like, Some(55.2));
        assert_eq!(now.humidity, Some(71));
        assert_eq!(now.condition, "overcast");

        assert_eq!(report.days.len(), 3);
        assert_eq!(report.days[1].date, "2026-10-05");
        assert_eq!(report.days[1].condition, "light rain");
        assert_eq!(report.days[1].precipitation_chance, Some(80));
        assert_eq!(report.days[1].precipitation, Some(0.31));
        assert_eq!(report.days[0].high, Some(61.2));
        assert_eq!(report.days[0].low, Some(49.8));
        assert_eq!(report.days[0].sunset.as_deref(), Some("2026-10-04T18:42"));
        // A null code and a short column are missing values, not lost days.
        assert_eq!(report.days[2].condition, "unknown");
        assert_eq!(report.days[2].sunset, None);
        assert_eq!(report.days[2].sunrise.as_deref(), Some("2026-10-06T07:15"));
    }

    #[test]
    fn the_report_serialises_in_the_shape_the_tool_promises() {
        let report = parse_forecast(FORECAST, &seattle(), Units::Metric).unwrap();
        let json = serde_json::to_value(&report).unwrap();
        for key in ["place", "latitude", "longitude", "timezone", "units", "current", "days"] {
            assert!(json.get(key).is_some(), "missing {key}");
        }
        assert_eq!(
            json["units"],
            serde_json::json!({ "temperature": "°C", "wind": "km/h", "precipitation": "mm" })
        );
        for key in [
            "time",
            "temperature",
            "feels_like",
            "humidity",
            "precipitation",
            "wind_speed",
            "condition",
        ] {
            assert!(json["current"].get(key).is_some(), "current is missing {key}");
        }
        for key in [
            "date",
            "condition",
            "high",
            "low",
            "precipitation_chance",
            "precipitation",
            "sunrise",
            "sunset",
        ] {
            assert!(json["days"][0].get(key).is_some(), "a day is missing {key}");
        }
    }

    #[test]
    fn an_error_body_becomes_a_sentence() {
        let body =
            r#"{"error": true, "reason": "Forecast days is invalid. Allowed range 0 to 16."}"#;
        let err = parse_forecast(body, &seattle(), Units::Metric).unwrap_err();
        assert!(err.to_string().contains("Allowed range"), "got {err}");
        assert_eq!(
            error_reason(body).as_deref(),
            Some("Forecast days is invalid. Allowed range 0 to 16.")
        );
        assert_eq!(error_reason("not json"), None);
    }

    #[test]
    fn every_documented_code_has_words() {
        for code in [
            0, 1, 2, 3, 45, 48, 51, 53, 55, 56, 57, 61, 63, 65, 66, 67, 71, 73, 75, 77, 80, 81, 82,
            85, 86, 95, 96, 99,
        ] {
            assert!(!describe(code).starts_with("unknown"), "{code} has no description");
        }
        assert_eq!(describe(63), "moderate rain");
        assert_eq!(describe(99), "thunderstorm with hail");
        assert_eq!(describe(4), "unknown (WMO code 4)");
    }

    #[test]
    fn units_parse_and_default_to_what_is_customary() {
        assert_eq!(Units::parse(" Imperial "), Some(Units::Imperial));
        assert_eq!(Units::parse("metric"), Some(Units::Metric));
        assert_eq!(Units::parse("kelvin"), None);
        assert_eq!(Units::customary_in("us"), Units::Imperial);
        assert_eq!(Units::customary_in("GB"), Units::Metric);
        assert_eq!(Units::customary_in(""), Units::Metric);
    }
}
