//! Time zones and days: reading a stored zone name, today in a zone, and the
//! instants a local day starts and ends.
//!
//! A zone name is stored as text on a record and arrives as text from the
//! person's settings and from a model, so every reader has to decide what an
//! unknown name means. Here it means UTC, once, rather than in thirty places.

use jiff::{Timestamp, civil::Date, tz::TimeZone};

/// The zone a stored name refers to, or UTC if the platform's tz database
/// does not know it.
pub fn zone_of(tz: &str) -> TimeZone {
    TimeZone::get(tz).unwrap_or(TimeZone::UTC)
}

/// The first instant of `date` in the zone named `tz` (UTC if unknown), or
/// `None` where that midnight does not exist in the zone.
pub fn start_of_day(date: Date, tz: &str) -> Option<Timestamp> {
    date.at(0, 0, 0, 0).to_zoned(zone_of(tz)).ok().map(|z| z.timestamp())
}

/// Resolve the local calendar date of an instant in a named time zone,
/// falling back to UTC if the zone is unknown to the platform tz database.
pub fn local_date_in(ts: Timestamp, tz: &str) -> Date {
    match TimeZone::get(tz) {
        Ok(zone) => ts.to_zoned(zone).date(),
        Err(_) => ts.to_zoned(TimeZone::UTC).date(),
    }
}

/// The machine's own time zone, as an IANA name, falling back to UTC.
///
/// Records store this so their local date survives the author moving
/// countries: an entry written in Berlin is still filed under the day it was
/// Berlin, read back in Chennai.
///
/// It lives here, in the core, rather than in each front end. Both the shell
/// and the CLI need it, and between them they had written this one line out
/// six times -- three of those inline in a single file. The zone a record is
/// filed under, and the day "overdue" is measured from, have to be decided by
/// the same code or they disagree at midnight.
pub fn system_tz() -> String {
    TimeZone::system().iana_name().unwrap_or("UTC").to_string()
}

/// Today, on the machine's own calendar. Kept beside [`system_tz`] so the two
/// can never disagree about which day it is.
pub fn today_local() -> Date {
    local_date_in(Timestamp::now(), &system_tz())
}
