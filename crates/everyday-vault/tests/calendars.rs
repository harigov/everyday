//! The calendar domain wired all the way through: a real vault, a real
//! SQLite file, a real cipher, and a real feed -- the shape Google, Outlook
//! and Apple all publish, folded lines and all.

use everyday_core::VaultConfig;
use everyday_vault::{create, open};

#[test]
fn subscribing_to_a_calendar_works_end_to_end_on_the_default_backend() {
    use everyday_core::calendar::Calendar;
    use everyday_core::store::calendars::EventQuery;

    let dir = tempfile::tempdir().unwrap();
    let cfg = VaultConfig {
        password: Some("correct horse battery".into()),
        kdf: everyday_core::crypto::KdfParams::insecure_fast(),
        ..Default::default()
    };
    let vault = create(dir.path(), cfg).unwrap();
    assert!(vault.supports_calendars(), "the default backend must carry the calendar domain");

    let calendar =
        Calendar::subscribed("Work", "webcal://example.com/work.ics").with_color("#0f766e");
    vault.save_calendar(&calendar).unwrap();
    assert_eq!(
        vault.calendar(calendar.id).unwrap().fetch_url().unwrap(),
        "https://example.com/work.ics",
        "webcal must be normalised on the way to the fetcher",
    );

    let feed = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-WR-CALNAME:Priya — Work\r\n\
         X-WR-TIMEZONE:Europe/Berlin\r\n\
         BEGIN:VEVENT\r\nUID:standup@example.com\r\nSUMMARY:Morning stand-\r\n up\r\n\
         DTSTART;TZID=Europe/Berlin:20260907T093000\r\n\
         DTEND;TZID=Europe/Berlin:20260907T094500\r\n\
         RRULE:FREQ=WEEKLY;BYDAY=MO,WE;COUNT=4\r\nEND:VEVENT\r\n\
         BEGIN:VEVENT\r\nUID:trip@example.com\r\nSUMMARY:Lisbon\r\n\
         DTSTART;VALUE=DATE:20260910\r\nDTEND;VALUE=DATE:20260921\r\nEND:VEVENT\r\n\
         END:VCALENDAR\r\n";

    let window = (jiff::civil::date(2026, 1, 1), jiff::civil::date(2026, 12, 31));
    let report = vault.sync_calendar_from_ics(calendar.id, feed, window, "UTC").unwrap();
    assert_eq!(report.events, 5, "four stand-ups and one trip");
    assert_eq!(report.feed_name.as_deref(), Some("Priya — Work"));

    // The week query is an overlap test, so it finds the trip from the
    // middle of it as well as the stand-ups that start inside it.
    let week = vault
        .events(&EventQuery::between(
            jiff::civil::date(2026, 9, 14),
            jiff::civil::date(2026, 9, 20),
        ))
        .unwrap();
    assert!(week.iter().any(|e| e.title == "Lisbon"), "a trip must show in its own second week",);
    assert!(week.iter().any(|e| e.title == "Morning stand-up"), "folded titles are rejoined",);

    // Syncing again replaces rather than duplicates.
    let report = vault.sync_calendar_from_ics(calendar.id, feed, window, "UTC").unwrap();
    assert_eq!(report.events, 5);
    assert_eq!(vault.event_count(calendar.id).unwrap(), 5, "a sync replaces, it does not append",);
    assert!(vault.calendar(calendar.id).unwrap().last_synced_at.is_some());

    // Lock, reopen, unlock: the events must still decrypt, which is what
    // would break if they were sealed against the wrong associated data.
    drop(vault);
    let vault = open(dir.path()).unwrap();
    assert_eq!(vault.calendars().unwrap_err().code(), "locked");
    vault.unlock(Some("correct horse battery")).unwrap();
    assert_eq!(vault.event_count(calendar.id).unwrap(), 5);

    // A reply that is not a calendar keeps what is already there.
    let err = vault
        .sync_calendar_from_ics(calendar.id, "<html>Sign in to the wifi</html>", window, "UTC")
        .unwrap_err();
    assert_eq!(err.code(), "invalid", "got {err}");
    assert_eq!(vault.event_count(calendar.id).unwrap(), 5, "a bad reply must not empty a feed",);
    vault.mark_calendar_failed(calendar.id, "that address did not return a calendar").unwrap();
    let after = vault.calendar(calendar.id).unwrap();
    assert!(after.last_error.is_some());
    assert!(after.last_synced_at.is_some(), "a failure must not erase when it last worked");

    // And unsubscribing takes the events with it.
    vault.delete_calendar(calendar.id).unwrap();
    assert!(vault.calendars().unwrap().is_empty());
    assert!(vault.events(&EventQuery::default()).unwrap().is_empty());
}
