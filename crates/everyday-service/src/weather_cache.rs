//! A short memory for one place's forecast.
//!
//! The Overview's Weather widget (`domains::weather::weather`) redraws every
//! fifteen minutes for as long as it is on screen, and the forecast behind it
//! changes slower than that -- Open-Meteo's own model does not run any more
//! often. Without this, every redraw would be a fresh round trip to two
//! hosts for a number that had not moved. [`WeatherCache`] is what makes a
//! redraw free most of the time: one [`Service`](crate::service::Service)
//! holds one, exactly as it holds [`crate::token_cache::TokenCache`] for
//! access tokens, and for the same reason this is session state rather than
//! a vault record -- a forecast is not something anybody would want to find
//! again after quitting the app, and keeping it in memory means a locked or
//! closed vault starts the next session with nothing stale to show.

use std::collections::HashMap;
use std::sync::Mutex;

use everyday_core::weather::Report;
use jiff::{SignedDuration, Timestamp};

/// How long a cached forecast is served before a fresh one replaces it.
///
/// A little under the widget's own fifteen-minute refresh: a redraw that
/// lands almost exactly on the boundary still finds a warm cache more often
/// than not, while nothing shown is ever more than a quarter of an hour
/// stale even in the worst case.
pub const TTL: SignedDuration = SignedDuration::from_secs(12 * 60);

struct Entry {
    at: Timestamp,
    report: Report,
}

/// Cached forecasts, keyed by the place they were asked for.
#[derive(Default)]
pub struct WeatherCache {
    entries: Mutex<HashMap<String, Entry>>,
}

impl WeatherCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Trimmed and lower-cased, so "Seattle", " seattle " and "SEATTLE"
    /// share one entry rather than each paying for their own round trip.
    fn key(place: &str) -> String {
        place.trim().to_ascii_lowercase()
    }

    /// A still-fresh forecast for `place`, cached at `now` or more recently
    /// than [`TTL`] ago. `None` for a place never asked about, or one whose
    /// entry has aged out.
    pub fn get(&self, place: &str, now: Timestamp) -> Option<Report> {
        let entries = self.entries.lock().unwrap();
        let entry = entries.get(&Self::key(place))?;
        (now.duration_since(entry.at) < TTL).then(|| entry.report.clone())
    }

    /// Remember `report` as `place`'s forecast as of `now`, replacing
    /// whatever was cached for it before.
    pub fn put(&self, place: &str, now: Timestamp, report: Report) {
        self.entries.lock().unwrap().insert(Self::key(place), Entry { at: now, report });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::weather::UnitLabels;

    fn report(place: &str) -> Report {
        Report {
            place: place.to_string(),
            latitude: 0.0,
            longitude: 0.0,
            timezone: "UTC".into(),
            units: UnitLabels { temperature: "°C", wind: "km/h", precipitation: "mm" },
            current: None,
            days: Vec::new(),
            air_quality: None,
        }
    }

    #[test]
    fn a_fresh_entry_is_served_and_a_stale_one_is_not() {
        let cache = WeatherCache::new();
        let now = Timestamp::now();
        cache.put("Seattle", now, report("Seattle"));

        assert_eq!(cache.get("Seattle", now).unwrap().place, "Seattle");
        let just_inside = now + (TTL - SignedDuration::from_secs(1));
        assert!(cache.get("Seattle", just_inside).is_some());
        let just_outside = now + (TTL + SignedDuration::from_secs(1));
        assert!(cache.get("Seattle", just_outside).is_none(), "aged out past the TTL");
    }

    #[test]
    fn the_key_ignores_case_and_surrounding_space() {
        let cache = WeatherCache::new();
        let now = Timestamp::now();
        cache.put(" Seattle ", now, report("Seattle, Washington, United States"));
        assert!(cache.get("seattle", now).is_some());
        assert!(cache.get("SEATTLE", now).is_some());
    }

    #[test]
    fn different_places_do_not_share_an_entry() {
        let cache = WeatherCache::new();
        let now = Timestamp::now();
        cache.put("Seattle", now, report("Seattle"));
        assert!(cache.get("Paris", now).is_none());
    }

    #[test]
    fn a_later_put_replaces_the_cached_entry() {
        let cache = WeatherCache::new();
        let now = Timestamp::now();
        cache.put("Seattle", now, report("Seattle (first)"));
        cache.put("Seattle", now, report("Seattle (second)"));
        assert_eq!(cache.get("Seattle", now).unwrap().place, "Seattle (second)");
    }
}
