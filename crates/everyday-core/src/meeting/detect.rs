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

/// Is this event a meeting worth offering to take notes for?
///
/// Busy, not cancelled, not all day, and at least one attendee besides the
/// owner (`owner_addresses`, compared case-insensitively). A join link is
/// not required: an in-room meeting is recorded from the microphone alone.
pub fn is_meeting(event: &Event, owner_addresses: &[String]) -> bool {
    if !event.busy || event.all_day {
        return false;
    }
    if event.status == crate::calendar::EventStatus::Cancelled {
        return false;
    }
    has_other_attendee(event, owner_addresses)
}

/// Is this event an online call: [`is_meeting`], with a meeting host in its
/// url, location or description?
pub fn is_online_call(event: &Event, owner_addresses: &[String]) -> bool {
    is_meeting(event, owner_addresses) && join_link(event).is_some()
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
            if let Some((_, no_scheme)) = token.split_once("://")
                && let Some((host, path)) = host_and_path(no_scheme)
                && matches_meeting_host(&host, path)
            {
                return Some(token.to_string());
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
        if boundary && let Some(link) = bare_host_token(rest) {
            return Some(link);
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

/// The host a browser would visit for a link (scheme already removed),
/// lower-cased, and the rest of the link after it -- or `None` for a link
/// whose host this will not vouch for.
///
/// This has to read the link the way a browser does, not merely plausibly,
/// because the link it approves is opened as written by the tray's "Join"
/// item and the notification's "Join & take notes" button. A browser ends
/// the host at the first `/`, `?`, `#` or `\`. Splitting at `/` alone read
/// `https://evil.example?@meet.google.com/abc` as Google Meet, and the app's
/// own button sent somebody to evil.example.
///
/// A `user@` in front of the host is refused rather than stripped: no
/// meeting service puts one in a join link, and it is how a link dresses up
/// as one. A host that is not plain letters, digits, dots and hyphens is
/// refused too: a browser percent-decodes and IDNA-maps a host before using
/// it, and turning such a host away is cheaper than repeating that and
/// getting it wrong.
fn host_and_path(no_scheme: &str) -> Option<(String, &str)> {
    let end = no_scheme.find(['/', '?', '#', '\\']).unwrap_or(no_scheme.len());
    let (authority, rest) = no_scheme.split_at(end);
    if authority.contains('@') {
        return None;
    }
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    let plain_host = !host.is_empty()
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    let plain_port = port.is_none_or(|p| p.bytes().all(|b| b.is_ascii_digit()));
    (plain_host && plain_port).then(|| (host.to_ascii_lowercase(), rest))
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
/// ends `#<start>` (`accountcal::caldav`, a `jiff::Timestamp`). Only a suffix
/// that actually parses as the datetime it claims to be is stripped, so a
/// UID that merely contains an `@` or a `#` of its own is left alone.
///
/// This is a **fallback**, not the whole answer: Google (`singleEvents=true`)
/// and Microsoft Graph (`calendarView`) both hand back a `uid` that is
/// already unique *per occurrence*, with no shared suffix to strip, so this
/// function alone would only ever skip the one occurrence "never for this
/// meeting" was pressed on. [`series_key_of`] is what callers with an
/// `Event` or an `EventRef` in hand should use instead -- it prefers the
/// durable series id those two sources populate on [`Event::series`], and
/// falls back to this function exactly when there isn't one, which is
/// always, for CalDAV and a subscribed feed, because their own `uid`
/// already carries a stable base this strips down to.
pub fn series_key(uid: &str) -> String {
    if let Some(idx) = uid.rfind('@')
        && uid[idx + 1..].parse::<jiff::civil::DateTime>().is_ok()
    {
        return uid[..idx].to_string();
    }
    if let Some(idx) = uid.rfind('#')
        && uid[idx + 1..].parse::<jiff::Timestamp>().is_ok()
    {
        return uid[..idx].to_string();
    }
    uid.to_string()
}

/// The series key for a `uid`/`series` pair, wherever they came from: an
/// `Event` (`series_key_of(&event.uid, event.series.as_deref())`), a wire
/// payload that carries the two loose (an offer, a dismissal), or a test
/// fixture. `series`, when non-empty, always wins over deriving one from
/// `uid` -- see [`series_key`]'s own doc for why a derived key is not
/// enough for every source.
pub fn series_key_of(uid: &str, series: Option<&str>) -> String {
    match series {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => series_key(uid),
    }
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
            series: None,
            own: None,
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
    fn a_meeting_host_after_the_real_host_is_not_matched() {
        // Each of these opens evil.example in a browser, which ends the host
        // at the first `?`, `#` or `\`: what follows is query, fragment or
        // path, however much it looks like Google Meet.
        for link in [
            "https://evil.example?@meet.google.com/abc-defg-hij",
            "https://evil.example#@meet.google.com/abc-defg-hij",
            "https://evil.example\\@meet.google.com/abc-defg-hij",
        ] {
            let mut event = base_event();
            event.description = format!("Join here: {link}");
            assert_eq!(join_link(&event), None, "{link} was taken for a Meet link");
            assert!(!is_online_call(&event, &["alice@example.com".into()]), "{link}");
        }
    }

    #[test]
    fn a_link_with_a_user_in_front_of_its_host_is_refused() {
        // Refused, not stripped: a browser would go to the host after the
        // `@`, but no join link has one, and it is how a link disguises
        // where it goes.
        for link in [
            "https://user@meet.google.com/abc-defg-hij",
            "https://user:pass@us02web.zoom.us/j/85551112222",
            "https://meet.google.com@evil.example/abc-defg-hij",
        ] {
            let mut event = base_event();
            event.url = link.into();
            assert_eq!(join_link(&event), None, "{link} was accepted");
        }
    }

    #[test]
    fn an_ordinary_link_still_matches_whatever_ends_its_host() {
        for link in [
            "https://meet.google.com/abc-defg-hij",
            "https://Meet.Google.COM/abc-defg-hij",
            "https://us02web.zoom.us:443/j/85551112222?pwd=abc",
            "https://meet.google.com?authuser=1",
            "https://meet.google.com#abc",
        ] {
            let mut event = base_event();
            event.url = link.into();
            assert_eq!(join_link(&event).as_deref(), Some(link));
        }

        // And a bare one with no scheme, as before.
        let mut event = base_event();
        event.description = "Dial in: zoom.us/j/1".into();
        assert_eq!(join_link(&event).as_deref(), Some("https://zoom.us/j/1"));
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
    fn a_meeting_needs_no_join_link() {
        // An in-room meeting with a colleague: nothing to join, but still one
        // worth recording from the microphone alone.
        let mut event = base_event();
        event.location = "Conference Room 4B".into();
        assert!(is_meeting(&event, &["alice@example.com".into()]));
        assert!(
            !is_online_call(&event, &["alice@example.com".into()]),
            "an online call is a meeting that also has a link"
        );

        event.url = "https://meet.google.com/abc-defg-hij".into();
        assert!(is_online_call(&event, &["alice@example.com".into()]));
    }

    #[test]
    fn a_meeting_with_only_the_owner_invited_is_not_a_meeting() {
        let mut event = base_event();
        event.attendees = vec!["Alice <alice@example.com>".into()];
        assert!(!is_meeting(&event, &["alice@example.com".into()]));

        event.attendees.clear();
        assert!(!is_meeting(&event, &["alice@example.com".into()]), "nobody invited at all");
    }

    #[test]
    fn a_free_all_day_or_cancelled_event_is_never_a_meeting() {
        let event = base_event();
        assert!(is_meeting(&event, &["alice@example.com".into()]));

        let mut free = event.clone();
        free.busy = false;
        assert!(!is_meeting(&free, &[]));

        let mut all_day = event.clone();
        all_day.all_day = true;
        assert!(!is_meeting(&all_day, &[]));

        let mut cancelled = event;
        cancelled.status = EventStatus::Cancelled;
        assert!(!is_meeting(&cancelled, &[]));
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

    #[test]
    fn series_key_of_prefers_a_carried_series_id_over_deriving_one() {
        // The whole point: two Google occurrences with different, per-
        // instance uids but the same `recurringEventId` must resolve to the
        // same key, so "never for this meeting" on one covers the other.
        assert_eq!(
            series_key_of("evt-1_20260916T090000Z", Some("evt-1")),
            series_key_of("evt-2_20260923T090000Z", Some("evt-1")),
            "two occurrences of the same series must share a key"
        );
    }

    #[test]
    fn series_key_of_falls_back_to_the_uid_when_there_is_no_series_id() {
        assert_eq!(
            series_key_of("abc123@example.com@2026-09-16T09:00:00", None),
            "abc123@example.com"
        );
        assert_eq!(
            series_key_of("abc123@example.com@2026-09-16T09:00:00", Some("")),
            "abc123@example.com",
            "an empty series id is treated as absent, not as the key"
        );
    }
}
