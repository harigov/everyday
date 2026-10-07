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

/// One of an account's calendars, saved, on a fresh account of `provider`
/// with calendar switched on.
fn an_account_calendar(
    vault: &everyday_core::Vault,
    provider: everyday_core::account::Provider,
    name: &str,
) -> everyday_core::calendar::Calendar {
    use everyday_core::account::Account;
    use everyday_core::calendar::{AccountCalendarSource, Calendar};
    let mut account = Account::new(provider, format!("{name}@example.com").to_lowercase());
    account.services.calendar = true;
    vault.save_account(&account).unwrap();
    let calendar = Calendar::from_account(
        account.id,
        provider,
        AccountCalendarSource::CalDav,
        format!("/cal/{name}/"),
        name,
    );
    vault.save_calendar(&calendar).unwrap();
    calendar
}

#[test]
fn there_is_only_ever_one_default_calendar_and_none_means_this_computer() {
    use everyday_core::account::Provider;
    use everyday_core::calendar::Calendar;

    let dir = tempfile::tempdir().unwrap();
    let vault = create(dir.path(), VaultConfig { password: None, ..Default::default() }).unwrap();
    let work = an_account_calendar(&vault, Provider::Fastmail, "Work");
    let home = an_account_calendar(&vault, Provider::Fastmail, "Home");
    let feed = Calendar::subscribed("Holidays", "https://example.com/h.ics");
    vault.save_calendar(&feed).unwrap();

    assert_eq!(vault.default_calendar().unwrap(), None, "nothing chosen: this computer");

    vault.set_default_calendar(Some(work.id)).unwrap();
    assert_eq!(vault.default_calendar().unwrap().map(|c| c.id), Some(work.id));

    // Moving it clears the old mark in the same pass.
    vault.set_default_calendar(Some(home.id)).unwrap();
    let marked: Vec<_> =
        vault.calendars().unwrap().into_iter().filter(|c| c.is_default).map(|c| c.id).collect();
    assert_eq!(marked, vec![home.id]);

    // A copy from before the move, saved back -- a sidebar toggling
    // visibility on a stale list -- cannot make a second default.
    let mut stale = vault.calendar(work.id).unwrap();
    stale.is_default = true;
    stale.visible = false;
    vault.save_calendar(&stale).unwrap();
    let saved = vault.calendar(work.id).unwrap();
    assert!(!saved.is_default, "only set_default_calendar moves the mark");
    assert!(!saved.visible, "everything else in the save still lands");

    // A feed can never be where new events go.
    assert!(vault.set_default_calendar(Some(feed.id)).is_err());

    // A calendar its server says is read-only stops being the default.
    vault.set_calendar_read_only(home.id, true).unwrap();
    assert_eq!(vault.default_calendar().unwrap(), None);
    assert!(!vault.calendar(home.id).unwrap().is_default);
    assert!(vault.set_default_calendar(Some(home.id)).is_err());

    vault.set_default_calendar(None).unwrap();
    assert!(vault.calendars().unwrap().iter().all(|c| !c.is_default));
}

#[test]
fn an_account_signed_in_to_read_calendars_cannot_write_them() {
    use everyday_core::account::{Account, AuthMethod, Provider};

    let mut google = Account::new(Provider::Google, "me@gmail.com");
    assert!(!google.can_write_calendars(), "mail scopes alone write nothing");
    if let AuthMethod::OAuth { scopes, .. } = &mut google.auth {
        scopes.push("https://www.googleapis.com/auth/calendar.readonly".into());
    }
    assert!(!google.can_write_calendars(), "a read-only grant from before writing existed");
    if let AuthMethod::OAuth { scopes, .. } = &mut google.auth {
        scopes.extend(Provider::Google.preset().oauth.unwrap().calendar_scopes);
    }
    assert!(google.can_write_calendars());

    let mut microsoft = Account::new(Provider::Microsoft, "me@outlook.com");
    if let AuthMethod::OAuth { scopes, .. } = &mut microsoft.auth {
        scopes.push("Calendars.Read".into());
    }
    assert!(!microsoft.can_write_calendars());
    if let AuthMethod::OAuth { scopes, .. } = &mut microsoft.auth {
        scopes.extend(Provider::Microsoft.preset().oauth.unwrap().calendar_scopes);
    }
    assert!(microsoft.can_write_calendars());

    assert!(Account::new(Provider::ICloud, "me@icloud.com").can_write_calendars(), "a password");
}

#[test]
fn a_repeating_block_is_written_ahead_and_can_be_changed_or_cut_short_from_any_point() {
    use everyday_core::recurrence::{Frequency, Recurrence, Weekday};
    use everyday_core::store::tasks::BlockQuery;
    use everyday_core::task::{BlockSubject, SeriesScope, TimeBlock};

    let dir = tempfile::tempdir().unwrap();
    let vault = create(dir.path(), VaultConfig { password: None, ..Default::default() }).unwrap();

    // Monday 5 October 2026, 07:00 in Berlin, an hour long.
    let start: jiff::Timestamp = "2026-10-05T05:00:00Z".parse().unwrap();
    let mut gym = TimeBlock::new(BlockSubject::Adhoc, start, 60, "Europe/Berlin");
    gym.title = "Gym".into();
    vault.save_block(&gym).unwrap();

    let rule = Recurrence {
        weekdays: vec![Weekday::Monday, Weekday::Thursday],
        count: Some(10),
        ..Recurrence::every(Frequency::Weekly)
    };
    let written = vault.save_block_series(&gym, Some(&rule)).unwrap();
    assert_eq!(written.len(), 10, "the count includes the first");
    assert_eq!(written[0].id, gym.id, "the block it was made from leads the series");
    assert!(written.iter().all(|b| b.series.as_ref().is_some_and(|s| s.id == gym.id)));

    // The wall clock holds across the end of summer time (25 October):
    // seven in the morning in Berlin before and after.
    let zone = jiff::tz::TimeZone::get("Europe/Berlin").unwrap();
    for b in &written {
        let local = b.start.to_zoned(zone.clone());
        assert_eq!((local.hour(), local.minute()), (7, 0), "{}", b.local_date);
        assert_eq!(b.minutes(), 60);
    }

    let all = |vault: &everyday_core::Vault| {
        let mut blocks = vault
            .blocks(&BlockQuery::between(
                jiff::civil::date(2026, 10, 1),
                jiff::civil::date(2027, 1, 31),
            ))
            .unwrap();
        blocks.sort_by_key(|b| b.start);
        blocks
    };
    assert_eq!(all(&vault).len(), 10);

    // Rename from the fourth on, and carry it forward: the earlier three
    // keep their name, the later ones are rewritten from the fourth.
    let mut fourth = all(&vault)[3].clone();
    fourth.title = "Gym, legs".into();
    let rewritten = vault.save_block_series(&fourth, Some(&rule)).unwrap();
    let now = all(&vault);
    assert_eq!(now.iter().filter(|b| b.title == "Gym").count(), 3);
    assert!(now[3..].iter().all(|b| b.title == "Gym, legs"));
    assert_eq!(rewritten.len(), now.len() - 3, "a count starts again from where it was changed");

    // Cut it short from the sixth: that and every later one go.
    let sixth = now[5].id;
    let gone = vault.delete_block_series(sixth, SeriesScope::Following).unwrap();
    assert_eq!(gone.len(), now.len() - 5);
    assert_eq!(all(&vault).len(), 5);

    // Stop the second series repeating altogether, from its first block.
    let head = all(&vault)[3].clone();
    let alone = vault.save_block_series(&head, None).unwrap();
    assert_eq!(alone.len(), 1);
    assert!(vault.block(head.id).unwrap().series.is_none());
    assert_eq!(all(&vault).len(), 4, "the fifth went with the rest of its series");

    // And the original series can still be deleted whole from any member.
    let first_series_member = all(&vault)[1].id;
    vault.delete_block_series(first_series_member, SeriesScope::All).unwrap();
    let left = all(&vault);
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, head.id);

    // Only a plan repeats.
    let mut done = TimeBlock::new(BlockSubject::Adhoc, start, 30, "UTC");
    done.kind = everyday_core::task::BlockKind::Actual;
    assert!(vault.save_block_series(&done, Some(&rule)).is_err());
}
