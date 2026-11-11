//! The calendars' background refresher, end to end, against a feed served from
//! loopback and a vault in a temporary directory. What is checked is what only
//! the loop decides: that a due calendar is fetched with no window asking, that
//! one not yet due is left alone, what is said when one fails, and that it stops.
//!
//! How a feed's bytes become events is `everyday-core`'s business and has tests
//! of its own there.

#[allow(dead_code)]
mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use everyday_core::Vault;
use everyday_core::account::{Account, AccountStatus, Provider};
use everyday_core::calendar::{AccountCalendarSource, Calendar};
use everyday_service::Service;
use everyday_service::calendar_refresh::{TASK_KEY, run};
use everyday_service::events::{Change, EventSink, Kind, Notification, Op};
use everyday_service::supervisor::{Outcome, TaskState};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::watch;

const EMPTY_ICS: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//t//EN\r\nEND:VCALENDAR\r\n";

const ICS: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//t//EN\r\n\
BEGIN:VEVENT\r\nUID:a@t\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:20260102T100000Z\r\n\
DTEND:20260102T110000Z\r\nSUMMARY:Standup\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

/// A feed on loopback that answers everything with `status`, and counts.
struct Feed {
    url: String,
    asked: Arc<AtomicUsize>,
}

impl Feed {
    async fn start(status: u16) -> Self {
        Self::serving(status, ICS).await
    }

    async fn serving(status: u16, body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/cal.ics", listener.local_addr().unwrap());
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = asked.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { return };
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = socket.read(&mut buf).await;
                    let body = if status == 200 { body } else { "" };
                    let reply = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: text/calendar\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                });
            }
        });
        Self { url, asked }
    }

    fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }
}

#[derive(Default)]
struct Collector {
    changes: Mutex<Vec<Change>>,
    notices: Mutex<Vec<Notification>>,
}

impl EventSink for Collector {
    fn changed(&self, change: Change) {
        self.changes.lock().unwrap().push(change);
    }
    fn notify(&self, notification: Notification) {
        self.notices.lock().unwrap().push(notification);
    }
}

impl Collector {
    fn event_changes(&self) -> usize {
        self.changes
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.kind == Kind::Event && c.op == Op::Updated)
            .count()
    }

    fn notices(&self) -> usize {
        self.notices.lock().unwrap().len()
    }
}

fn fixture() -> (Arc<Service>, Arc<Vault>, Arc<Collector>, tempfile::TempDir) {
    let (svc, dir) = support::vault::service(None);
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());
    let vault = svc.require().unwrap();
    (svc, vault, events, dir)
}

/// Poll `check` until it is true, or fail after five seconds.
async fn eventually(what: &str, check: impl Fn() -> bool) {
    for _ in 0..200 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("gave up waiting for {what}");
}

/// Run the loop until `until` holds, then stop it and insist that it stopped.
async fn one_pass(svc: &Arc<Service>, vault: &Arc<Vault>, what: &str, until: impl Fn() -> bool) {
    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(run(svc.clone(), vault.clone(), stopped));
    eventually(what, until).await;
    // A pass goes on a little past what a test can see of it, and this ends it.
    tokio::time::sleep(Duration::from_millis(150)).await;
    stop.send(true).unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the loop stops promptly")
        .unwrap();
    assert_eq!(outcome.unwrap(), Outcome::Done);
}

#[tokio::test]
async fn a_due_calendar_is_refreshed_with_nobody_asking() {
    let (svc, vault, events, _dir) = fixture();
    let feed = Feed::start(200).await;
    let calendar = Calendar::subscribed("Work", feed.url.clone());
    vault.save_calendar(&calendar).unwrap();

    one_pass(&svc, &vault, "the refresh to be announced", || events.event_changes() == 1).await;

    let stored = vault.calendar(calendar.id).unwrap();
    assert!(stored.last_synced_at.is_some(), "the pass stored what it fetched");
    assert_eq!(stored.last_error, None);
    assert_eq!(feed.asked(), 1, "it was due once");
}

#[tokio::test]
async fn a_calendar_not_yet_due_is_left_alone() {
    let (svc, vault, events, _dir) = fixture();
    let due = Feed::start(200).await;
    let fresh = Feed::start(200).await;
    vault.save_calendar(&Calendar::subscribed("Due", due.url.clone())).unwrap();
    let mut recent = Calendar::subscribed("Fresh", fresh.url.clone());
    recent.mark_synced();
    vault.save_calendar(&recent).unwrap();

    // The due one being fetched is what says the pass has been and gone.
    one_pass(&svc, &vault, "the due calendar to refresh", || events.event_changes() == 1).await;

    assert_eq!(due.asked(), 1);
    assert_eq!(fresh.asked(), 0, "refreshed moments ago, so not asked");
}

/// A calendar that is not due is still fetched when one of its meetings is
/// about to be reminded: moved on somebody's phone since the last refresh, it
/// would otherwise be reminded at the time it used to be.
#[tokio::test]
async fn a_calendar_with_a_reminder_about_to_go_out_is_checked_first() {
    let (svc, vault, events, _dir) = fixture();
    let now = jiff::Timestamp::now();
    let start = now + jiff::SignedDuration::from_mins(15);
    let stamp = |t: jiff::Timestamp| t.strftime("%Y%m%dT%H%M%SZ").to_string();
    let ics = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//t//EN\r\nBEGIN:VEVENT\r\n\
         UID:soon@t\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:{}\r\nDTEND:{}\r\n\
         SUMMARY:Design sync\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        stamp(start),
        stamp(start + jiff::SignedDuration::from_mins(30)),
    );
    let feed = Feed::serving(200, Box::leak(ics.into_boxed_str())).await;
    vault.save_calendar(&Calendar::subscribed("Work", feed.url.clone())).unwrap();
    one_pass(&svc, &vault, "the first fetch", || events.event_changes() == 1).await;
    assert_eq!(feed.asked(), 1);

    // Three minutes on, its hourly refresh is nowhere near due -- but the
    // default ten-minute reminder goes out in two.
    svc.set_clock(Arc::new(support::clock::FakeClock::at(
        now + jiff::SignedDuration::from_mins(3),
    )));
    one_pass(&svc, &vault, "the check before the reminder", || feed.asked() == 2).await;
}

#[tokio::test]
async fn a_pass_with_nothing_due_says_nothing() {
    let (svc, vault, events, _dir) = fixture();
    let feed = Feed::start(200).await;
    let mut recent = Calendar::subscribed("Fresh", feed.url.clone());
    recent.mark_synced();
    vault.save_calendar(&recent).unwrap();

    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(run(svc.clone(), vault.clone(), stopped));
    tokio::time::sleep(Duration::from_millis(200)).await;
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap().unwrap();

    assert_eq!(feed.asked(), 0);
    assert_eq!(events.event_changes(), 0, "nothing changed, so no window should redraw");
}

#[tokio::test]
async fn a_sync_that_brought_nothing_new_says_nothing_but_still_counts() {
    let (svc, vault, events, _dir) = fixture();
    let feed = Feed::serving(200, EMPTY_ICS).await;
    let calendar = Calendar::subscribed("Quiet", feed.url.clone());
    vault.save_calendar(&calendar).unwrap();

    one_pass(&svc, &vault, "the feed to be asked", || feed.asked() == 1).await;

    assert!(vault.calendar(calendar.id).unwrap().last_synced_at.is_some());
    // A window reloading for this would count as somebody being here, and every
    // few minutes of that would keep the key in memory for as long as it was open.
    assert_eq!(events.event_changes(), 0);
}

#[tokio::test]
async fn a_failure_shows_at_once_beside_the_calendar_and_is_notified_only_once_it_has_lasted() {
    let (svc, vault, events, _dir) = fixture();
    let started: jiff::Timestamp = "2026-06-01T09:00:00Z".parse().unwrap();
    let clock = Arc::new(support::clock::FakeClock::at(started));
    svc.set_clock(clock.clone());
    let feed = Feed::start(500).await;
    let calendar = Calendar::subscribed("Work", feed.url.clone());
    vault.save_calendar(&calendar).unwrap();

    // The first failure: what a laptop finds on waking with no wifi yet.
    one_pass(&svc, &vault, "the failure to be announced", || events.event_changes() == 1).await;
    assert!(
        vault.calendar(calendar.id).unwrap().last_error.is_some(),
        "the sidebar has it straight away"
    );
    assert_eq!(events.notices(), 0, "too soon to interrupt anybody");

    // Still failing, and nothing new for a window to draw each time it does.
    clock.set(started + jiff::SignedDuration::from_mins(5));
    one_pass(&svc, &vault, "the second failure", || feed.asked() == 2).await;
    clock.set(started + jiff::SignedDuration::from_mins(10));
    one_pass(&svc, &vault, "the third failure", || feed.asked() == 3).await;
    assert_eq!(events.notices(), 0);
    assert_eq!(events.event_changes(), 1, "the same failure again changes nothing on screen");

    // A quarter of an hour of it: now it is worth saying, and saying once.
    clock.set(started + jiff::SignedDuration::from_mins(15));
    one_pass(&svc, &vault, "the notice", || events.notices() == 1).await;
    clock.set(started + jiff::SignedDuration::from_mins(20));
    one_pass(&svc, &vault, "the next failure", || feed.asked() == 5).await;
    assert_eq!(events.notices(), 1, "once per outage");
}

#[tokio::test]
async fn an_account_waiting_for_sign_in_is_left_alone_until_somebody_signs_in() {
    let (svc, vault, events, _dir) = fixture();
    let mut account = Account::new(Provider::Custom, "me@example.com");
    account.status = AccountStatus::NeedsSignIn { reason: "the first reason".into() };
    vault.save_account(&account).unwrap();
    let calendar = Calendar::from_account(
        account.id,
        Provider::Custom,
        AccountCalendarSource::CalDav,
        "/cal/",
        "Theirs",
    );
    vault.save_calendar(&calendar).unwrap();
    // The feed being fetched is what says the pass has been and gone.
    let feed = Feed::start(200).await;
    vault.save_calendar(&Calendar::subscribed("Feed", feed.url.clone())).unwrap();

    one_pass(&svc, &vault, "the pass", || feed.asked() == 1).await;

    let stored = vault.calendar(calendar.id).unwrap();
    assert_eq!(stored.last_synced_at, None);
    assert_eq!(stored.last_error, None, "it was not tried, so it did not fail");
    assert_eq!(
        vault.account(account.id).unwrap().status,
        AccountStatus::NeedsSignIn { reason: "the first reason".into() },
        "and nothing refused a token and wrote a new reason"
    );
    assert_eq!(events.notices(), 0);
}

#[tokio::test]
async fn it_starts_on_a_vault_that_is_never_unlocked_and_comes_back_on_one_that_is() {
    let (svc, _vault, _events, _dir) = fixture();
    assert_eq!(svc.supervisor().state(TASK_KEY), None, "nothing registered yet");

    // No password, or just created: what a shell does after setting such a vault.
    svc.start_refreshers();
    assert!(svc.supervisor().state(TASK_KEY).is_some());
    assert!(svc.supervisor().state(everyday_service::news::TASK_KEY).is_some());
    svc.supervisor().stop_all().await;
    assert_eq!(svc.supervisor().state(TASK_KEY), Some(TaskState::Stopped));

    svc.unlocked();
    assert!(
        matches!(svc.supervisor().state(TASK_KEY), Some(TaskState::Starting | TaskState::Running)),
        "the unlock is what brings it back"
    );
    svc.supervisor().stop_all().await;
}

#[tokio::test]
async fn a_locked_vault_starts_nothing_until_it_is_unlocked() {
    let (svc, _dir) = support::vault::service(Some("pw"));
    let vault = svc.require().unwrap();
    vault.lock();
    svc.start_refreshers();
    assert_eq!(svc.supervisor().state(TASK_KEY), None);
}

/// The supervisor keeps a task's factory for good. One holding the vault would keep
/// its write lock after a close, and opening it again would come up read-only.
#[tokio::test]
async fn closing_lets_go_of_the_vault_so_it_opens_writable_again() {
    let (svc, dir) = support::vault::service(None);
    svc.start_refreshers();
    svc.close().await;

    let reopened = everyday_vault::open(dir.path()).unwrap();
    assert!(reopened.is_writable(), "something is still holding the old vault's lock");
    let vault = svc.set(reopened);

    // And what comes back is about this vault, not the one that was closed.
    let feed = Feed::start(200).await;
    let calendar = Calendar::subscribed("Work", feed.url.clone());
    vault.save_calendar(&calendar).unwrap();
    svc.start_refreshers();
    eventually("the refresher to reach the new vault", || {
        vault.calendar(calendar.id).unwrap().last_synced_at.is_some()
    })
    .await;
    svc.supervisor().stop_all().await;
}
