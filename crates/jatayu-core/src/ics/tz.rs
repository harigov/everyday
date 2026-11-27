use jiff::tz::TimeZone;

// ── Time zones ───────────────────────────────────────────────────────────
/// Windows time-zone names, as Outlook and Exchange write them, mapped to
/// the IANA names the system database understands.
///
/// A subset — the CLDR mapping has several hundred entries and most name
/// zones nobody's calendar is quoted in. An unlisted name is not an error:
/// it falls back to the feed's own default zone, and then to the reader's,
/// which is at worst the behaviour of a floating time.
pub const WINDOWS_ZONES: &[(&str, &str)] = &[
    ("AUS Eastern Standard Time", "Australia/Sydney"),
    ("AUS Central Standard Time", "Australia/Darwin"),
    ("Arabian Standard Time", "Asia/Dubai"),
    ("Argentina Standard Time", "America/Argentina/Buenos_Aires"),
    ("Atlantic Standard Time", "America/Halifax"),
    ("Canada Central Standard Time", "America/Regina"),
    ("Cen. Australia Standard Time", "Australia/Adelaide"),
    ("Central America Standard Time", "America/Guatemala"),
    ("Central Brazilian Standard Time", "America/Cuiaba"),
    ("Central Europe Standard Time", "Europe/Budapest"),
    ("Central European Standard Time", "Europe/Warsaw"),
    ("Central Standard Time", "America/Chicago"),
    ("Central Standard Time (Mexico)", "America/Mexico_City"),
    ("China Standard Time", "Asia/Shanghai"),
    ("E. Africa Standard Time", "Africa/Nairobi"),
    ("E. Australia Standard Time", "Australia/Brisbane"),
    ("E. South America Standard Time", "America/Sao_Paulo"),
    ("Eastern Standard Time", "America/New_York"),
    ("Egypt Standard Time", "Africa/Cairo"),
    ("FLE Standard Time", "Europe/Kiev"),
    ("GMT Standard Time", "Europe/London"),
    ("GTB Standard Time", "Europe/Bucharest"),
    ("Greenwich Standard Time", "Atlantic/Reykjavik"),
    ("Hawaiian Standard Time", "Pacific/Honolulu"),
    ("India Standard Time", "Asia/Kolkata"),
    ("Iran Standard Time", "Asia/Tehran"),
    ("Israel Standard Time", "Asia/Jerusalem"),
    ("Korea Standard Time", "Asia/Seoul"),
    ("Mountain Standard Time", "America/Denver"),
    ("Mountain Standard Time (Mexico)", "America/Chihuahua"),
    ("New Zealand Standard Time", "Pacific/Auckland"),
    ("Pacific SA Standard Time", "America/Santiago"),
    ("Pacific Standard Time", "America/Los_Angeles"),
    ("Romance Standard Time", "Europe/Paris"),
    ("Russian Standard Time", "Europe/Moscow"),
    ("SA Pacific Standard Time", "America/Bogota"),
    ("SE Asia Standard Time", "Asia/Bangkok"),
    ("Singapore Standard Time", "Asia/Singapore"),
    ("South Africa Standard Time", "Africa/Johannesburg"),
    ("Tokyo Standard Time", "Asia/Tokyo"),
    ("Turkey Standard Time", "Europe/Istanbul"),
    ("US Eastern Standard Time", "America/Indianapolis"),
    ("US Mountain Standard Time", "America/Phoenix"),
    ("UTC", "UTC"),
    ("W. Australia Standard Time", "Australia/Perth"),
    ("W. Central Africa Standard Time", "Africa/Lagos"),
    ("W. Europe Standard Time", "Europe/Berlin"),
];
/// Turn a feed's `TZID` into an IANA name the tz database will accept.
///
/// Three shapes turn up in real feeds: a plain IANA name, a Windows display
/// name, and Mozilla's `/mozilla.org/20050126_1/Europe/Berlin` — a prefixed
/// IANA name, which is why the last two path segments are tried.
pub fn resolve_tzid(tzid: &str) -> Option<String> {
    let tzid = tzid.trim().trim_matches('"');
    if tzid.is_empty() {
        return None;
    }
    if TimeZone::get(tzid).is_ok() {
        return Some(tzid.to_string());
    }
    if let Some((_, iana)) = WINDOWS_ZONES.iter().find(|(win, _)| win.eq_ignore_ascii_case(tzid)) {
        return Some((*iana).to_string());
    }
    // `/mozilla.org/20050126_1/Europe/Berlin` -> `Europe/Berlin`.
    let segs: Vec<&str> = tzid.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() >= 2 {
        let tail = format!("{}/{}", segs[segs.len() - 2], segs[segs.len() - 1]);
        if TimeZone::get(&tail).is_ok() {
            return Some(tail);
        }
    }
    if let Some(last) = segs.last()
        && TimeZone::get(last).is_ok()
    {
        return Some((*last).to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_windows_zone_name_resolves_to_an_iana_one() {
        assert_eq!(resolve_tzid("W. Europe Standard Time").as_deref(), Some("Europe/Berlin"));
        assert_eq!(resolve_tzid("Europe/Berlin").as_deref(), Some("Europe/Berlin"));
        assert_eq!(
            resolve_tzid("/mozilla.org/20050126_1/Europe/Berlin").as_deref(),
            Some("Europe/Berlin"),
        );
        assert_eq!(resolve_tzid("Middle-earth Standard Time"), None);
    }
}
