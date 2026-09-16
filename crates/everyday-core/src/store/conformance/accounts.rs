//! The account half of the conformance suite.

use super::*;
use crate::account::{ACCOUNT_SECRET_OWNER_KIND, Account, AccountSecret};
use crate::calendar::{AccountCalendarSource, Calendar, EventStatus};
use crate::store::accounts::AccountStore;
use crate::store::calendars::EventQuery;

/// Everything a backend must do with accounts, including its two cascades:
/// deleting an account takes its secret, and phase 6's account calendars
/// (and their events), with it.
pub fn run_account_suite(store: &dyn JournalStore) {
    eprintln!("--- account conformance suite ---");

    accounts_start_empty(store);
    account_round_trips_every_field(store);
    account_put_is_idempotent(store);
    missing_account_is_not_found(store);
    deleting_an_account_takes_its_secret_with_it(store);
    deleting_an_account_takes_its_calendars_and_their_events_with_it(store);
    deleting_an_account_leaves_a_url_calendar_untouched(store);
    unicode_survives_an_account_round_trip(store);

    account_cleanup(store);
    eprintln!("--- account suite passed ---");
}

fn account_store(store: &dyn JournalStore) -> &dyn AccountStore {
    store.accounts().expect("the account suite needs an account store")
}

fn account_cleanup(store: &dyn JournalStore) {
    let a = account_store(store);
    for account in a.list_accounts().expect("list_accounts") {
        a.delete_account(account.id).expect("delete_account");
    }
    assert!(a.list_accounts().unwrap().is_empty(), "cleanup left accounts behind");
}

fn accounts_start_empty(store: &dyn JournalStore) {
    assert!(account_store(store).list_accounts().unwrap().is_empty(), "a fresh store has none");
}

fn account_round_trips_every_field(store: &dyn JournalStore) {
    let a = account_store(store);
    let mut account = Account::new(crate::account::Provider::Google, "me@gmail.com");
    account.display_name = "Work Gmail".into();
    account.identities.push(crate::account::Identity {
        name: "Work".into(),
        address: "work@gmail.com".into(),
        signature_html: "<p>Sent from my desk</p>".into(),
    });
    account.services.calendar = true;
    account.mcp_access = crate::account::AgentMailAccess::none();
    account.attachment_cap_bytes = Some(25_000_000);
    account.status = crate::account::AccountStatus::Error { message: "the server refused".into() };
    a.put_account(&account).expect("put_account");

    assert_eq!(a.get_account(account.id).unwrap(), account, "every field must survive");
    assert_eq!(a.list_accounts().unwrap().len(), 1);

    account_cleanup(store);
}

fn account_put_is_idempotent(store: &dyn JournalStore) {
    let a = account_store(store);
    let account = Account::new(crate::account::Provider::Fastmail, "me@fastmail.com");
    a.put_account(&account).expect("put_account");
    a.put_account(&account).expect("second put");
    assert_eq!(a.list_accounts().unwrap().len(), 1, "saving twice leaves one");

    a.delete_account(account.id).expect("delete_account");
    a.delete_account(account.id).expect("deleting a missing account is a no-op");
    account_cleanup(store);
}

fn missing_account_is_not_found(store: &dyn JournalStore) {
    super::assert_not_found(account_store(store).get_account(crate::id::AccountId::new()));
}

fn deleting_an_account_takes_its_secret_with_it(store: &dyn JournalStore) {
    let Some(secrets) = store.secrets() else {
        eprintln!("  (no secret store; skipping the cascade)");
        return;
    };
    let a = account_store(store);
    let account = Account::new(crate::account::Provider::Yahoo, "me@yahoo.com");
    a.put_account(&account).expect("put_account");

    let secret = AccountSecret { password: Some("hunter2".into()), ..Default::default() };
    let bytes = serde_json::to_vec(&secret).expect("a secret serialises");
    secrets
        .put_secret(ACCOUNT_SECRET_OWNER_KIND, &account.id.to_string(), &bytes)
        .expect("put_secret");
    assert!(
        secrets.get_secret(ACCOUNT_SECRET_OWNER_KIND, &account.id.to_string()).unwrap().is_some()
    );

    a.delete_account(account.id).expect("delete_account");
    assert!(
        secrets.get_secret(ACCOUNT_SECRET_OWNER_KIND, &account.id.to_string()).unwrap().is_none(),
        "the secret must not outlive the account it belongs to"
    );

    account_cleanup(store);
}

/// A minimal, valid event on `calendar`, for the cascade tests below --
/// nothing about its content matters, only that a row exists to be found
/// again, or not.
fn stub_event(calendar: crate::id::CalendarId) -> crate::calendar::Event {
    use jiff::Timestamp;
    use jiff::civil::date;
    crate::calendar::Event {
        id: crate::id::EventId::new(),
        calendar_id: calendar,
        uid: "stub@example.com".into(),
        title: "Stub".into(),
        description: String::new(),
        location: String::new(),
        start: Timestamp::now(),
        end: Timestamp::now(),
        local_date: date(2026, 9, 14),
        end_date: date(2026, 9, 14),
        tz: "UTC".into(),
        all_day: false,
        status: EventStatus::Confirmed,
        organizer: String::new(),
        attendees: Vec::new(),
        url: String::new(),
        busy: true,
        series: None,
        updated_at: Timestamp::now(),
    }
}

/// The cascade this phase adds: an account calendar, and the events synced
/// onto it, do not outlive the account they were read from.
fn deleting_an_account_takes_its_calendars_and_their_events_with_it(store: &dyn JournalStore) {
    let Some(calendars) = store.calendars() else {
        eprintln!("  (no calendar store; skipping the account-calendar cascade)");
        return;
    };
    let a = account_store(store);
    let account = Account::new(crate::account::Provider::ICloud, "me@icloud.com");
    a.put_account(&account).expect("put_account");

    let cal = Calendar::from_account(
        account.id,
        account.provider,
        AccountCalendarSource::CalDav,
        "https://caldav.icloud.com/1234/calendars/home/",
        "Home",
    );
    calendars.put_calendar(&cal).expect("put_calendar");
    calendars.replace_events(cal.id, &[stub_event(cal.id)]).expect("replace_events");
    assert_eq!(calendars.count_events(cal.id).unwrap(), 1);

    a.delete_account(account.id).expect("delete_account");

    super::assert_not_found(calendars.get_calendar(cal.id));
    let left = calendars
        .list_events(&EventQuery { calendar_id: Some(cal.id), ..Default::default() })
        .expect("list_events");
    assert!(left.is_empty(), "an account calendar's events must not outlive the account either");

    account_cleanup(store);
}

/// The cascade is specific to account calendars. A feed subscription that
/// happens to exist alongside a deleted account is somebody else's record
/// and must not be touched by it.
fn deleting_an_account_leaves_a_url_calendar_untouched(store: &dyn JournalStore) {
    let Some(calendars) = store.calendars() else {
        eprintln!("  (no calendar store; skipping)");
        return;
    };
    let a = account_store(store);
    let account = Account::new(crate::account::Provider::Fastmail, "me@fastmail.com");
    a.put_account(&account).expect("put_account");

    let feed = Calendar::subscribed("Team fixtures", "https://example.com/team.ics");
    calendars.put_calendar(&feed).expect("put_calendar");

    a.delete_account(account.id).expect("delete_account");

    assert_eq!(calendars.get_calendar(feed.id).unwrap().id, feed.id, "a feed is not an account's");
    calendars.delete_calendar(feed.id).expect("delete_calendar");
    account_cleanup(store);
}

fn unicode_survives_an_account_round_trip(store: &dyn JournalStore) {
    let a = account_store(store);
    let mut account = Account::new(crate::account::Provider::Custom, "me@example.com");
    account.display_name = "\u{5de5}\u{4f5c}\u{90ae}\u{7bb1} \u{2600}".into();
    a.put_account(&account).expect("put_account");
    let back = a.get_account(account.id).expect("get_account");
    assert_eq!(back.display_name, account.display_name);
    a.delete_account(account.id).expect("delete_account");
}
