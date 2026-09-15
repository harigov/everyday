//! `accountcal::caldav` against a real CalDAV server.
//!
//! Everything else this crate tests about `accountcal::caldav` -- the etag
//! diff, the sync-collection XML, the DST/EXDATE recurrence -- is a unit
//! test of a pure function or a hand-built response, in
//! `accountcal::caldav`'s own `#[cfg(test)] mod tests`. What only a real
//! server can prove is the *conversation*: that this crate's requests are
//! shaped the way an actual CalDAV implementation expects, and that its
//! `getetag`/`sync-token` responses parse the way this crate assumes they
//! will. `make test-caldav` starts a throwaway Radicale (Kozea's own image;
//! GPL-3.0, and fine here because nothing links it -- see the Makefile's own
//! comment on the target) and points this test at it with `EVERYDAY_TEST_
//! CALDAV=1`; skipped otherwise, the same convention `imap_dovecot.rs` and
//! `smtp_mailpit.rs` use, so a plain `cargo test` never needs Docker.
//!
//! # Seeding over plain HTTP, not through this crate
//!
//! `accountcal` is read-only by design -- see `docs/plans/mail.md`'s "read
//! only, like every calendar here" -- so it has no PUT, MKCALENDAR or DELETE
//! of its own to seed a test fixture with. This file uses a bare `reqwest`
//! client with HTTP Basic to create the collection and its events directly,
//! exactly as a person's own CalDAV client would have, before ever calling
//! into `accountcal::caldav`. That is also what makes the incremental half
//! of this test meaningful: the *changes* between the first and second sync
//! are made the same way, outside this crate, and `accountcal::caldav::sync`
//! is asked to notice them on its own.

#[allow(dead_code)]
mod support;

use everyday_core::account::{Account, AccountSecret, AuthMethod, Provider};
use everyday_core::calendar::{AccountCalendarSource, Calendar};
use everyday_core::store::calendars::EventQuery;
use everyday_service::accountcal::caldav;

fn env_or_skip(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

struct Fixture {
    base_url: String,
    user: String,
    pass: String,
    client: reqwest::Client,
}

impl Fixture {
    async fn put(&self, path: &str, ics: &str) {
        let resp = self
            .client
            .put(format!("{}{path}", self.base_url))
            .basic_auth(&self.user, Some(&self.pass))
            .header("Content-Type", "text/calendar")
            .body(ics.to_string())
            .send()
            .await
            .expect("PUT reaches Radicale");
        assert!(resp.status().is_success(), "PUT {path}: {}", resp.status());
    }

    async fn delete(&self, path: &str) {
        let resp = self
            .client
            .delete(format!("{}{path}", self.base_url))
            .basic_auth(&self.user, Some(&self.pass))
            .send()
            .await
            .expect("DELETE reaches Radicale");
        assert!(resp.status().is_success(), "DELETE {path}: {}", resp.status());
    }

    async fn mkcalendar(&self, path: &str, display_name: &str) {
        let body = format!(
            "<mkcalendar xmlns=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\">\
             <set><prop><displayname>{display_name}</displayname></prop></set></mkcalendar>"
        );
        let resp = self
            .client
            .request(
                reqwest::Method::from_bytes(b"MKCALENDAR").unwrap(),
                format!("{}{path}", self.base_url),
            )
            .basic_auth(&self.user, Some(&self.pass))
            .header("Content-Type", "application/xml")
            .body(body)
            .send()
            .await
            .expect("MKCALENDAR reaches Radicale");
        assert!(resp.status().is_success(), "MKCALENDAR {path}: {}", resp.status());
    }
}

const SINGLE_EVENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\n\
    UID:single@example.com\r\nDTSTART:20260920T090000Z\r\nDTEND:20260920T093000Z\r\n\
    SUMMARY:Single meeting\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

const SINGLE_EVENT_RENAMED: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\n\
    UID:single@example.com\r\nDTSTART:20260920T090000Z\r\nDTEND:20260920T093000Z\r\n\
    SUMMARY:Renamed meeting\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

const DOOMED_EVENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\n\
    UID:doomed@example.com\r\nDTSTART:20260921T140000Z\r\nDTEND:20260921T150000Z\r\n\
    SUMMARY:To be deleted\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

const RECURRING_EVENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\n\
    UID:recurring@example.com\r\nDTSTART:20260901T100000Z\r\nDTEND:20260901T110000Z\r\n\
    RRULE:FREQ=WEEKLY;COUNT=4\r\nSUMMARY:Weekly recurring\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

const NEW_EVENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\n\
    UID:new@example.com\r\nDTSTART:20260922T160000Z\r\nDTEND:20260922T170000Z\r\n\
    SUMMARY:Added later\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

/// A weekly meeting with no `COUNT` and no `UNTIL` -- it never ends, the way
/// a real recurring standup never has a last occurrence typed into it. The
/// point of this fixture is that a client has to decide for itself how far
/// forward to materialise it; the server has no opinion.
const UNENDING_WEEKLY_EVENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\n\
    UID:forever@example.com\r\nDTSTART:20260901T100000Z\r\nDTEND:20260901T110000Z\r\n\
    RRULE:FREQ=WEEKLY\r\nSUMMARY:Standing weekly\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

#[tokio::test]
async fn discovers_and_syncs_a_real_caldav_server_incrementally() {
    let Some(base_url) = env_or_skip("EVERYDAY_TEST_CALDAV_URL") else {
        eprintln!("skipping: set EVERYDAY_TEST_CALDAV=1 and run `make test-caldav`");
        return;
    };
    // The real application gets a process-wide `rustls` `CryptoProvider` for
    // free from `everyday_server::lib`'s startup, which installs `ring`
    // before anything opens a connection -- see that call, and this crate's
    // `Cargo.toml` note on why two providers are linked at all. This test
    // binary never runs that startup path, so it installs the same provider
    // itself, the same way, before either this file's own `reqwest::Client`
    // or `accountcal::caldav`'s `hyper-rustls` connector can build one.
    // Ignored if a provider is somehow already installed; never fails the
    // test either way.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let user = std::env::var("EVERYDAY_TEST_CALDAV_USER").unwrap_or_else(|_| "everyday".into());
    let pass = std::env::var("EVERYDAY_TEST_CALDAV_PASS").unwrap_or_else(|_| "testpass".into());

    let fixture = Fixture {
        base_url: base_url.clone(),
        user: user.clone(),
        pass: pass.clone(),
        client: reqwest::Client::new(),
    };

    // Seed: a calendar collection with a single event, one due to be
    // deleted, and a four-occurrence weekly recurrence -- six events in
    // total once expanded.
    let home = format!("/{user}/");
    let cal_path = format!("{home}testcal/");
    fixture.mkcalendar(&cal_path, "Test Calendar").await;
    fixture.put(&format!("{cal_path}single.ics"), SINGLE_EVENT).await;
    fixture.put(&format!("{cal_path}doomed.ics"), DOOMED_EVENT).await;
    fixture.put(&format!("{cal_path}recurring.ics"), RECURRING_EVENT).await;

    // A vault, an account pointed at the throwaway server, and the calendar
    // `discover` finds on it.
    let (svc, _dir) = support::vault::service(Some("pw"));
    let vault = svc.require().expect("vault is open");

    let mut account = Account::new(Provider::Custom, format!("{user}@example.com"));
    account.caldav = Some(base_url);
    account.auth = AuthMethod::Password { username: user.clone() };
    vault.save_account(&account).expect("save_account");
    vault
        .save_account_secret(
            account.id,
            &AccountSecret { password: Some(pass), ..Default::default() },
        )
        .expect("save_account_secret");

    let remotes = caldav::discover(&svc, &vault, &account).await.expect("discover succeeds");
    let remote = remotes
        .iter()
        .find(|r| r.name == "Test Calendar")
        .unwrap_or_else(|| panic!("did not find the seeded calendar among {remotes:?}"));
    assert_eq!(remote.source, AccountCalendarSource::CalDav);

    let mut calendar = Calendar::from_account(
        account.id,
        account.provider,
        remote.source,
        remote.remote_id.clone(),
        remote.name.clone(),
    );
    vault.save_calendar(&calendar).expect("save_calendar");

    // First sync: everything is new.
    let report = caldav::sync(&svc, &vault, &account, &calendar).await.expect("first sync");
    assert_eq!(report.events, 6, "1 single + 1 doomed + 4 recurring occurrences");
    calendar = vault.calendar(calendar.id).expect("reload after sync");
    assert!(
        calendar.account_sync.token.is_some() || !calendar.account_sync.etags.is_empty(),
        "a sync must remember either a sync-token or an etag map to be incremental next time"
    );

    let after_first: Vec<_> = vault
        .events(&EventQuery { calendar_id: Some(calendar.id), ..Default::default() })
        .expect("list_events");
    assert_eq!(after_first.len(), 6);
    assert!(after_first.iter().any(|e| e.title == "Single meeting"));
    assert!(after_first.iter().any(|e| e.title == "To be deleted"));
    assert_eq!(after_first.iter().filter(|e| e.title == "Weekly recurring").count(), 4);

    // Change the world: delete one event, rename another, add a third.
    fixture.delete(&format!("{cal_path}doomed.ics")).await;
    fixture.put(&format!("{cal_path}single.ics"), SINGLE_EVENT_RENAMED).await;
    fixture.put(&format!("{cal_path}new.ics"), NEW_EVENT).await;

    let report2 = caldav::sync(&svc, &vault, &account, &calendar).await.expect("second sync");
    // Exactly what changed: the renamed event and the new one are upserts;
    // the recurring four and nothing else are untouched.
    assert_eq!(report2.events, 2, "the renamed event and the new one, and nothing untouched");

    let after_second: Vec<_> = vault
        .events(&EventQuery { calendar_id: Some(calendar.id), ..Default::default() })
        .expect("list_events");
    assert_eq!(after_second.len(), 6, "one deleted, one added, net unchanged");
    assert!(
        !after_second.iter().any(|e| e.title == "To be deleted"),
        "the deleted event must not survive an incremental sync"
    );
    assert!(
        after_second.iter().any(|e| e.title == "Renamed meeting"),
        "the renamed event must have been re-fetched"
    );
    assert!(
        !after_second.iter().any(|e| e.title == "Single meeting"),
        "the old title must not linger beside the new one"
    );
    assert!(after_second.iter().any(|e| e.title == "Added later"));
    assert_eq!(
        after_second.iter().filter(|e| e.title == "Weekly recurring").count(),
        4,
        "occurrences nothing touched must not have been refetched or duplicated"
    );

    // A third sync, after deleting the whole recurring resource -- what
    // finding 1's "check whether CalDAV has the same gap" asks for. Neither
    // sync above ever obtained an RFC 6578 sync-token (Radicale's own
    // `<d:sync-token>` is only asked for once one is already on record, and
    // this calendar's cursor never got one from `etag_diff`), so every sync
    // in this file, this one included, already goes through the etag-diff
    // path -- which compares the *full* current listing against the *full*
    // remembered one on every single run, not a delta since some token.
    // That is why CalDAV has no version of findings 1's Google/Graph bug:
    // there is no "full resync that only speaks for what it just listed"
    // here, because every resync already speaks for the whole collection.
    let after_second_calendar = vault.calendar(calendar.id).expect("reload before the third sync");
    fixture.delete(&format!("{cal_path}recurring.ics")).await;
    caldav::sync(&svc, &vault, &account, &after_second_calendar).await.expect("third sync");

    let after_third: Vec<_> = vault
        .events(&EventQuery { calendar_id: Some(calendar.id), ..Default::default() })
        .expect("list_events");
    assert!(
        !after_third.iter().any(|e| e.title == "Weekly recurring"),
        "every occurrence of a deleted resource must be gone, not only the ones an incremental \
         diff would have known to look for: {:?}",
        after_third.iter().map(|e| &e.title).collect::<Vec<_>>()
    );
    assert_eq!(after_third.len(), 2, "the renamed single meeting and the added one remain");
}

/// An unending weekly meeting -- no `COUNT`, no `UNTIL` -- must keep growing
/// new occurrences at the far edge of the sync window as today moves
/// forward, even though nothing about the event itself ever changes and its
/// etag never moves. Finding 3's scenario ("sync on day 0, advance the
/// clock 13 months, sync with unchanged etags") reproduced as directly as a
/// test can without controlling the wall clock: this vault does not have a
/// way to fake `jiff::Timestamp::now()` (nothing in this codebase does --
/// see `everyday_core::model::today_local`), so instead of thirteen months
/// really passing, the calendar's own sealed cursor is rewritten to say
/// they already have, the same state a real thirteen months would have left
/// it in. The second sync then has to notice on its own that its
/// `expanded_through` marker is stale and force a refetch of the recurring
/// resource despite its etag being exactly what it was -- which is the
/// mechanism finding 3 adds, not a fact about what day it is.
#[tokio::test]
async fn an_unending_recurring_event_is_reexpanded_once_its_window_marker_goes_stale() {
    let Some(base_url) = env_or_skip("EVERYDAY_TEST_CALDAV_URL") else {
        eprintln!("skipping: set EVERYDAY_TEST_CALDAV=1 and run `make test-caldav`");
        return;
    };
    let _ = rustls::crypto::ring::default_provider().install_default();
    let user = std::env::var("EVERYDAY_TEST_CALDAV_USER").unwrap_or_else(|_| "everyday".into());
    let pass = std::env::var("EVERYDAY_TEST_CALDAV_PASS").unwrap_or_else(|_| "testpass".into());
    let fixture = Fixture {
        base_url: base_url.clone(),
        user: user.clone(),
        pass: pass.clone(),
        client: reqwest::Client::new(),
    };

    let home = format!("/{user}/");
    let cal_path = format!("{home}unendingcal/");
    fixture.mkcalendar(&cal_path, "Unending Calendar").await;
    fixture.put(&format!("{cal_path}forever.ics"), UNENDING_WEEKLY_EVENT).await;

    let (svc, _dir) = support::vault::service(Some("pw"));
    let vault = svc.require().expect("vault is open");
    let mut account = Account::new(Provider::Custom, format!("{user}@example.com"));
    account.caldav = Some(base_url);
    account.auth = AuthMethod::Password { username: user.clone() };
    vault.save_account(&account).expect("save_account");
    vault
        .save_account_secret(
            account.id,
            &AccountSecret { password: Some(pass), ..Default::default() },
        )
        .expect("save_account_secret");

    let remotes = caldav::discover(&svc, &vault, &account).await.expect("discover succeeds");
    let remote = remotes
        .iter()
        .find(|r| r.name == "Unending Calendar")
        .unwrap_or_else(|| panic!("did not find the seeded calendar among {remotes:?}"));
    let mut calendar = Calendar::from_account(
        account.id,
        account.provider,
        remote.source,
        remote.remote_id.clone(),
        remote.name.clone(),
    );
    vault.save_calendar(&calendar).expect("save_calendar");

    caldav::sync(&svc, &vault, &account, &calendar).await.expect("first sync");
    calendar = vault.calendar(calendar.id).expect("reload after the first sync");
    assert!(
        calendar.account_sync.recurring_hrefs.iter().any(|h| h.ends_with("forever.ics")),
        "the unending event's href must be remembered as recurring: {:?}",
        calendar.account_sync.recurring_hrefs
    );
    let first_expanded_through = calendar
        .account_sync
        .expanded_through
        .expect("a sync that saw a recurring resource must record how far it expanded");

    // Thirteen months earlier than what the first sync just recorded -- the
    // state a real thirteen months of nothing else happening would leave
    // behind, per this test's own doc.
    calendar.account_sync.expanded_through =
        Some(first_expanded_through.checked_sub(jiff::Span::new().days(396)).unwrap());
    vault.save_calendar(&calendar).expect("rewind the expansion marker");

    let report2 = caldav::sync(&svc, &vault, &account, &calendar).await.expect("second sync");
    assert!(
        report2.events > 0,
        "the recurring resource must have been refetched and re-expanded despite an unchanged \
         etag, once its window marker was stale"
    );

    let reexpanded = vault.calendar(calendar.id).expect("reload after the second sync");
    assert_eq!(
        reexpanded.account_sync.expanded_through,
        Some(first_expanded_through),
        "the marker must be caught back up to the current window, not left at the rewound date"
    );
}
