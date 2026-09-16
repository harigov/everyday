//! Which calendar events are online calls, and where to join them.
//!
//! Pure string work over an [`Event`], tested against real invitation text.

use crate::calendar::Event;

/// Hosts whose links mean "this is a call". Data, not logic: adding one is a
/// line here and a test case.
///
/// Most entries are a bare host, matched as a suffix (`us02web.zoom.us`
/// matches `zoom.us`). One, Slack's huddle link, is a host and a path
/// prefix together (`app.slack.com/huddle`), because `app.slack.com` on its
/// own is Slack itself, not a call.
pub const MEETING_HOSTS: &[&str] = &[
    "meet.google.com",
    "zoom.us",
    "zoomgov.com",
    "teams.microsoft.com",
    "teams.live.com",
    "webex.com",
    "whereby.com",
    "chime.aws",
    "gotomeeting.com",
    "meet.goto.com",
    "around.co",
    "app.slack.com/huddle",
    "discord.gg",
    "meet.jit.si",
];

/// Characters that stop a URL token: whitespace, and the wrappers an
/// invitation is fond of putting a link inside of (`<...>` in a Teams
/// invite, quotes in an HTML one).
const TOKEN_STOP: [char; 4] = ['<', '>', '"', '\''];

/// Punctuation a sentence leaves stuck to the end of a URL: a full stop
/// after "join here: https://zoom.us/j/123.", a closing bracket around it.
const TRAILING_PUNCTUATION: [char; 11] = ['.', ',', ';', ':', ')', ']', '}', '>', '"', '\'', '!'];

/// Is this event an online call worth offering to record?
///
/// Busy, not cancelled, not all day, at least one attendee besides the
/// owner (`owner_addresses`, compared case-insensitively), and a meeting
/// host in its url, location or description.
pub fn is_online_call(event: &Event, owner_addresses: &[String]) -> bool {
    if !event.busy || event.all_day {
        return false;
    }
    if event.status == crate::calendar::EventStatus::Cancelled {
        return false;
    }
    if !has_other_attendee(event, owner_addresses) {
        return false;
    }
    join_link(event).is_some()
}

/// Does this event have at least one attendee who is not the owner?
///
/// An attendee is "Name <addr>", a bare address, or a bare name. Only an
/// address can be compared against `owner_addresses`; a bare name is never
/// known to be the owner, so it always counts as somebody else.
fn has_other_attendee(event: &Event, owner_addresses: &[String]) -> bool {
    event.attendees.iter().any(|raw| {
        let (_, email) = super::identify::parse_attendee(raw);
        match email {
            Some(addr) => !owner_addresses.iter().any(|owner| owner.eq_ignore_ascii_case(&addr)),
            None => true,
        }
    })
}

/// The first meeting-host link in the event's url, location or description.
pub fn join_link(event: &Event) -> Option<String> {
    for field in [&event.url, &event.location, &event.description] {
        if let Some(link) = first_meeting_link(field) {
            return Some(link);
        }
    }
    None
}

/// Scan `text` left to right for the first link -- schemed or, for the
/// likes of Google Meet, bare -- whose host (and, for `app.slack.com/huddle`,
/// path) matches [`MEETING_HOSTS`].
fn first_meeting_link(text: &str) -> Option<String> {
    for (i, _) in text.char_indices() {
        let rest = &text[i..];
        if rest.starts_with("https://") || rest.starts_with("http://") {
            let token = take_token(rest);
            if let Some((_, no_scheme)) = token.split_once("://") {
                let (authority, path) = split_authority(no_scheme);
                let host = host_of(authority);
                if matches_meeting_host(&host, path) {
                    return Some(token.to_string());
                }
            }
            continue;
        }
        // A bare mention (Google Meet's own invitation text never adds a
        // scheme) only counts at a word boundary -- otherwise
        // "xmeet.google.com" or "user@meet.google.com" would match.
        let boundary = match text[..i].chars().last() {
            None => true,
            Some(c) => !(c.is_alphanumeric() || matches!(c, '.' | '-' | '@' | '/')),
        };
        if boundary {
            if let Some(link) = bare_host_token(rest) {
                return Some(link);
            }
        }
    }
    None
}

/// Take characters up to the first stop character, then trim trailing
/// punctuation a sentence or a bracket left stuck to the end.
fn take_token(s: &str) -> &str {
    let end = s.find(|c: char| c.is_whitespace() || TOKEN_STOP.contains(&c)).unwrap_or(s.len());
    s[..end].trim_end_matches(|c: char| TRAILING_PUNCTUATION.contains(&c))
}

/// Split `host[:port]/path?query` (no scheme) into the authority and the
/// path onward (empty when there is none).
fn split_authority(no_scheme: &str) -> (&str, &str) {
    match no_scheme.find('/') {
        Some(idx) => (&no_scheme[..idx], &no_scheme[idx..]),
        None => (no_scheme, ""),
    }
}

/// The host out of an authority, lower-cased, with any `user:pass@` and
/// `:port` stripped.
fn host_of(authority: &str) -> String {
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = host_port.split(':').next().unwrap_or(host_port);
    host.to_ascii_lowercase()
}

/// Does `host` (and, where the entry asks for one, `path`) match one of
/// [`MEETING_HOSTS`]? A host matches as a suffix on a `.` boundary, so
/// `us02web.zoom.us` matches `zoom.us` but `notzoom.us` does not.
fn matches_meeting_host(host: &str, path: &str) -> bool {
    MEETING_HOSTS.iter().any(|entry| match entry.split_once('/') {
        Some((entry_host, entry_path)) => {
            host_matches(host, entry_host)
                && path
                    .to_ascii_lowercase()
                    .starts_with(&format!("/{}", entry_path.to_ascii_lowercase()))
        }
        None => host_matches(host, entry),
    })
}

fn host_matches(host: &str, entry_host: &str) -> bool {
    let entry_host = entry_host.to_ascii_lowercase();
    host == entry_host || host.ends_with(&format!(".{entry_host}"))
}

/// A bare host mention -- `meet.google.com/abc-defg-hij` with no scheme --
/// wrapped in `https://` so callers always get a fetchable link.
fn bare_host_token(rest: &str) -> Option<String> {
    let lower = rest.to_ascii_lowercase();
    for host in MEETING_HOSTS.iter().filter(|h| !h.contains('/')) {
        if !lower.starts_with(host) {
            continue;
        }
        let after = rest[host.len()..].chars().next();
        let boundary_after = match after {
            None => true,
            Some(c) => c.is_whitespace() || matches!(c, '/' | '?' | '#') || TOKEN_STOP.contains(&c),
        };
        if boundary_after {
            return Some(format!("https://{}", take_token(rest)));
        }
    }
    None
}

/// The UID with a recurrence-instance suffix removed, so "never for this
/// meeting" covers the series. See `ics.rs` for how the suffix is appended.
///
/// Two shapes exist: a feed occurrence's UID ends `@<local start>`
/// (`ics::materialise`, a `jiff::civil::DateTime`), and a CalDAV occurrence's
/// ends `#<start>` (`accountcal::caldav`, a `jiff::Timestamp`). Google and
/// Graph hand back an id that is already unique per occurrence, so there is
/// no suffix to strip there and this is a no-op for them. Only a suffix that
/// actually parses as the datetime it claims to be is stripped, so a UID
/// that merely contains an `@` or a `#` of its own is left alone.
pub fn series_key(uid: &str) -> String {
    if let Some(idx) = uid.rfind('@') {
        if uid[idx + 1..].parse::<jiff::civil::DateTime>().is_ok() {
            return uid[..idx].to_string();
        }
    }
    if let Some(idx) = uid.rfind('#') {
        if uid[idx + 1..].parse::<jiff::Timestamp>().is_ok() {
            return uid[..idx].to_string();
        }
    }
    uid.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::EventStatus;
    use crate::id::{CalendarId, EventId};
    use jiff::Timestamp;
    use jiff::civil::date;

    fn base_event() -> Event {
        Event {
            id: EventId::new(),
            calendar_id: CalendarId::new(),
            uid: "abc123@example.com".into(),
            title: "Design sync".into(),
            description: String::new(),
            location: String::new(),
            start: Timestamp::now(),
            end: Timestamp::now(),
            local_date: date(2026, 9, 16),
            end_date: date(2026, 9, 16),
            tz: "UTC".into(),
            all_day: false,
            status: EventStatus::Confirmed,
            organizer: "Alice <alice@example.com>".into(),
            attendees: vec!["Bob <bob@example.com>".into()],
            url: String::new(),
            busy: true,
            updated_at: Timestamp::now(),
        }
    }

    #[test]
    fn a_zoom_invite_is_recognised_and_its_join_link_found() {
        let mut event = base_event();
        event.description = "Alice is inviting you to a scheduled Zoom meeting.\n\n\
            Join Zoom Meeting\nhttps://us02web.zoom.us/j/85551112222?pwd=abcDEF123\n\n\
            Meeting ID: 855 5111 2222\nPasscode: 123456"
            .into();
        assert!(is_online_call(&event, &["alice@example.com".into()]));
        assert_eq!(
            join_link(&event).as_deref(),
            Some("https://us02web.zoom.us/j/85551112222?pwd=abcDEF123")
        );
    }

    #[test]
    fn a_google_meet_link_is_found_bare_or_schemed() {
        let mut event = base_event();
        event.location =
            "Google Meet joining info\nVideo call link: meet.google.com/abc-defg-hij".into();
        assert_eq!(join_link(&event).as_deref(), Some("https://meet.google.com/abc-defg-hij"));

        event.location.clear();
        event.description = "Join here: https://meet.google.com/abc-defg-hij".into();
        assert_eq!(join_link(&event).as_deref(), Some("https://meet.google.com/abc-defg-hij"));
    }

    #[test]
    fn a_teams_link_wrapped_in_angle_brackets_is_trimmed_of_them() {
        let mut event = base_event();
        event.description = "________________________________________________________________\n\
            Microsoft Teams meeting\nJoin on your computer\n\
            <https://teams.microsoft.com/l/meetup-join/19%3ameeting_abc%40thread.v2/0?context=%7b%22Tid%22%3a%221%22%7d>\n\
            ________________________________________________________________"
            .into();
        assert_eq!(
            join_link(&event).as_deref(),
            Some(
                "https://teams.microsoft.com/l/meetup-join/19%3ameeting_abc%40thread.v2/0?context=%7b%22Tid%22%3a%221%22%7d"
            )
        );
    }

    #[test]
    fn a_webex_link_trailing_a_sentence_is_trimmed_of_its_full_stop() {
        let mut event = base_event();
        event.description =
            "Join from the meeting link (https://example.webex.com/meet/alice).".into();
        assert_eq!(join_link(&event).as_deref(), Some("https://example.webex.com/meet/alice"));
    }

    #[test]
    fn a_slack_huddle_needs_both_the_host_and_the_huddle_path() {
        let mut event = base_event();
        event.url = "https://app.slack.com/huddle/T0123/C0456".into();
        assert_eq!(join_link(&event).as_deref(), Some("https://app.slack.com/huddle/T0123/C0456"));

        // app.slack.com on its own is just Slack, not a call.
        let mut not_a_call = base_event();
        not_a_call.url = "https://app.slack.com/client/T0123/C0456".into();
        assert_eq!(join_link(&not_a_call), None);
    }

    #[test]
    fn a_lookalike_host_is_not_matched() {
        let mut event = base_event();
        event.description = "Dial in at https://notzoom.us/j/123".into();
        assert_eq!(join_link(&event), None);
        assert!(!is_online_call(&event, &["alice@example.com".into()]));
    }

    #[test]
    fn url_is_preferred_over_location_over_description() {
        let mut event = base_event();
        event.description = "https://meet.jit.si/desc-room".into();
        event.location = "https://meet.jit.si/loc-room".into();
        event.url = "https://meet.jit.si/url-room".into();
        assert_eq!(join_link(&event).as_deref(), Some("https://meet.jit.si/url-room"));

        event.url.clear();
        assert_eq!(join_link(&event).as_deref(), Some("https://meet.jit.si/loc-room"));
    }

    #[test]
    fn a_call_needs_an_attendee_besides_the_owner() {
        let mut event = base_event();
        event.url = "https://meet.google.com/abc-defg-hij".into();
        event.attendees = vec!["Alice <alice@example.com>".into()];
        assert!(
            !is_online_call(&event, &["alice@example.com".into()]),
            "only the owner is invited"
        );

        event.attendees.push("Bob <bob@example.com>".into());
        assert!(is_online_call(&event, &["alice@example.com".into()]));
    }

    #[test]
    fn a_bare_name_attendee_still_counts_as_somebody_else() {
        let mut event = base_event();
        event.url = "https://meet.google.com/abc-defg-hij".into();
        event.attendees = vec!["Just A Name".into()];
        assert!(is_online_call(&event, &["alice@example.com".into()]));
    }

    #[test]
    fn owner_comparison_is_case_insensitive() {
        let mut event = base_event();
        event.url = "https://meet.google.com/abc-defg-hij".into();
        event.attendees = vec!["Alice <ALICE@Example.com>".into()];
        assert!(!is_online_call(&event, &["alice@example.com".into()]));
    }

    #[test]
    fn a_free_all_day_or_cancelled_event_is_never_a_call() {
        let mut event = base_event();
        event.url = "https://meet.google.com/abc-defg-hij".into();

        let mut free = event.clone();
        free.busy = false;
        assert!(!is_online_call(&free, &[]));

        let mut all_day = event.clone();
        all_day.all_day = true;
        assert!(!is_online_call(&all_day, &[]));

        event.status = EventStatus::Cancelled;
        assert!(!is_online_call(&event, &[]));
    }

    #[test]
    fn an_event_with_no_meeting_host_anywhere_is_not_a_call() {
        let mut event = base_event();
        event.location = "Conference Room 4B".into();
        assert!(!is_online_call(&event, &["alice@example.com".into()]));
        assert_eq!(join_link(&event), None);
    }

    #[test]
    fn series_key_strips_a_feed_occurrences_at_suffix() {
        assert_eq!(series_key("weekly-standup@2026-09-16T09:00:00"), "weekly-standup");
        // The base UID's own '@' must survive; only the trailing datetime goes.
        assert_eq!(series_key("abc123@example.com@2026-09-16T09:00:00"), "abc123@example.com");
    }

    #[test]
    fn series_key_strips_a_caldav_occurrences_hash_suffix() {
        assert_eq!(series_key("/cal/standup.ics#2026-09-16T09:00:00Z"), "/cal/standup.ics");
    }

    #[test]
    fn series_key_leaves_a_plain_uid_alone() {
        // Google and Graph hand back an id that is already unique per
        // occurrence, and a one-off feed event's UID has no suffix worth
        // stripping if it does not parse as a datetime.
        assert_eq!(series_key("google-event-id-xyz"), "google-event-id-xyz");
        assert_eq!(series_key("mailto:someone@example.com"), "mailto:someone@example.com");
    }
}
