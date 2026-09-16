//! Which calendar events are online calls, and where to join them.
//!
//! Pure string work over an [`Event`], tested against real invitation text.

use crate::calendar::Event;

/// Hosts whose links mean "this is a call". Data, not logic: adding one is a
/// line here and a test case.
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

/// Is this event an online call worth offering to record?
///
/// Busy, not cancelled, not all day, at least one attendee besides the
/// owner (`owner_addresses`, compared case-insensitively), and a meeting
/// host in its url, location or description.
pub fn is_online_call(event: &Event, owner_addresses: &[String]) -> bool {
    let _ = (event, owner_addresses);
    todo!("core-logic agent")
}

/// The first meeting-host link in the event's url, location or description.
pub fn join_link(event: &Event) -> Option<String> {
    let _ = event;
    todo!("core-logic agent")
}

/// The UID with a recurrence-instance suffix removed, so "never for this
/// meeting" covers the series. See `ics.rs` for how the suffix is appended.
pub fn series_key(uid: &str) -> String {
    let _ = uid;
    todo!("core-logic agent")
}
