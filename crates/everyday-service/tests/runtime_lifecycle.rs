//! Characterizes what `Service::close`, `Service::locked` and
//! `Service::unlocked` do to session state *today*, before
//! docs/plans/architecture-refactor.md's phase 9.5 regroups that state into
//! per-area runtimes (`MailRuntime`, `MeetingRuntime`, `RoutineRuntime`).
//!
//! Every test here must keep passing, unchanged, once that regrouping is
//! done -- that is the whole point of writing them first. Two things are
//! worth knowing before reading them:
//!
//! * `close()` and `locked()` are **not** the same operation wearing two
//!   names. `locked()` (a lock screen; the vault the caller cares about is
//!   still the same file, still remembered, just keyed away) clears far
//!   less than `close()` (a different vault chosen, or going remote; the
//!   vault pointer itself is dropped). Several tests below exist only to
//!   pin that gap so a later change cannot quietly widen or close it.
//! * A handful of fields `close()` never touches at all --
//!   `meeting_offered`, `meeting_last_append`, `meeting_expiry_warned`, the
//!   mail rate-limit budgets -- and that is not an oversight this refactor
//!   gets to fix; see each test's own doc for why.
//!
//! What is deliberately *not* here: the durable, per-op outbox table lives
//! in the vault itself (`everyday-core`'s store, exercised by
//! `tests/mail_outbox.rs`), not in any `Service` field, so it is not this
//! phase's business. What phase 9.5 touches is the in-memory bookkeeping
//! around it -- `mail_notify`'s wake handles and `mail_draft_debounce`'s
//! timers -- which is what "a queued outbox drain" means below.

#[allow(dead_code)]
mod support;

use std::sync::{Arc, Mutex};

use everyday_core::id::{AccountId, CalendarId, DraftId, MailMessageId, RecordingId, ThreadId};
use everyday_core::mail::Origin;
use everyday_service::clock::Clock;
use everyday_service::error::codes;
use everyday_service::supervisor::{Outcome, TaskState};
use everyday_service::{Ctx, Service};
use serde_json::json;

use support::clock::FakeClock;

/// An encrypted vault, so a test can actually lock and unlock it -- unlike
/// `support::vault::service(None)`, which most of this crate's tests use
/// because they are not about the lock at all.
fn encrypted_service() -> (Arc<Service>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = everyday_core::VaultConfig {
        name: "Test".into(),
        backend: "sqlite".into(),
        settings: Default::default(),
        password: Some("correct horse battery staple".into()),
        kdf: everyday_core::crypto::KdfParams::insecure_fast(),
        auto_lock_seconds: 900,
        forget_key_seconds: 0,
    };
    let vault = everyday_vault::create(dir.path(), config).unwrap();
    vault.save_journal(&everyday_core::Journal::new("Test")).unwrap();
    let svc = Arc::new(Service::new());
    svc.set(vault);
    (svc, dir)
}

/// Register a task under `key` that does nothing until it is told to stop,
/// then records -- into `seen` -- whether `svc.packs()` still answered
/// `Some` at the moment it noticed. What "a running mail sync task" means
/// in every test below: not a real IMAP connection, but a supervised task
/// standing in for one, exactly the way `close_stops_every_supervised_task`
/// in `service.rs` already stands one in for "deaf".
fn watch_mail_state_at_stop(svc: &Arc<Service>, key: &str, seen: Arc<Mutex<Option<bool>>>) {
    let svc = svc.clone();
    svc.clone().supervisor().ensure(key, move |mut stop| {
        let svc = svc.clone();
        let seen = seen.clone();
        Box::pin(async move {
            stop.changed().await.ok();
            *seen.lock().unwrap() = Some(svc.packs().is_some());
            Ok(Outcome::Done)
        })
    });
}

/// Block until `key` shows as [`TaskState::Running`], the same settling
/// loop `service.rs`'s own supervisor test uses: `tokio::time::sleep`
/// inside the task needs a moment to actually be polled.
async fn wait_running(svc: &Service, key: &str) {
    for _ in 0..200 {
        if matches!(svc.supervisor().state(key), Some(TaskState::Running)) {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("task {key} never reached Running");
}

/// `close()`'s own doc says the ordering plainly: every supervised task is
/// stopped "before anything else -- ... a mail account's sync task writes
/// through the pack store and search index `close_mail` is about to drop,
/// and letting one keep running against storage that has just gone out
/// from under it is a bug this ordering exists to make impossible". This
/// proves it from the task's own point of view rather than trusting the
/// comment: a task that is actually watching the stop signal must still
/// see mail state open at the instant it notices being asked to stop.
#[tokio::test]
async fn close_stops_a_mail_sync_task_before_it_clears_mail_state() {
    let (svc, _dir) = encrypted_service();
    assert!(svc.packs().is_some(), "a freshly-set, unlocked vault must open mail eagerly");

    let seen = Arc::new(Mutex::new(None));
    watch_mail_state_at_stop(&svc, "account:test", seen.clone());
    wait_running(&svc, "account:test").await;

    svc.close().await;

    assert_eq!(
        seen.lock().unwrap().as_ref(),
        Some(&true),
        "a supervised task must still see mail state open when it is told to stop"
    );
    assert!(svc.packs().is_none(), "close() must have cleared mail state by the time it returns");
}

/// The same ordering, through `locked()` instead of `close()` -- the path a
/// lock screen takes rather than switching vaults. `locked()`'s own doc
/// gives the identical reason.
#[tokio::test]
async fn locked_stops_a_mail_sync_task_before_it_clears_mail_state() {
    let (svc, _dir) = encrypted_service();
    let vault = svc.get().unwrap();
    assert!(svc.packs().is_some());

    let seen = Arc::new(Mutex::new(None));
    watch_mail_state_at_stop(&svc, "account:test", seen.clone());
    wait_running(&svc, "account:test").await;

    vault.lock();
    svc.locked().await;

    assert_eq!(
        seen.lock().unwrap().as_ref(),
        Some(&true),
        "locked() must also stop tasks before it clears mail state"
    );
    assert!(svc.packs().is_none());
    // Unlike close(), locked() does not let go of the vault itself -- it is
    // still the vault to reopen, just keyed away.
    assert!(svc.get().is_some(), "locked() must not drop the vault pointer -- only close() does");
}

/// Everything `close()` clears by hand around mail, in one place: the
/// per-account wake handle, the draft debounce timer, the per-caller rate
/// limit, the summarise cache and both scheduler cursors. And the one thing
/// it deliberately does *not* touch: the categorisation/auto-draft token
/// buckets, which are meant to survive exactly as a per-minute budget would
/// -- see `mail_categorize_budget`'s own field doc ("a restart simply
/// starts a fresh minute's budget" describes a *process* restart, not a
/// vault close). A [`FakeClock`] pins the budget math so no real time can
/// pass between the two takes and blur the assertion.
#[tokio::test]
async fn close_clears_mail_session_bookkeeping_but_leaves_the_rate_budgets_alone() {
    let (svc, _dir) = encrypted_service();
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::at(jiff::Timestamp::now()));
    svc.set_clock(clock);

    let account = AccountId::new();
    let draft = DraftId::new();
    let thread = ThreadId::new();

    // Outbox wake handle: "a queued outbox drain" waiting on this handle
    // would never be woken by a notify that arrives after a reopen if the
    // handle it is holding has gone stale -- which is exactly what close()
    // does to it.
    let notify_before = svc.outbox_notify(account);

    // Draft debounce: due once, then not again inside the window.
    assert!(svc.draft_append_due(draft, svc.now()));
    assert!(!svc.draft_append_due(draft, svc.now()));

    // Per-turn mail rate limit: exhaust it under one client/turn pair.
    let origin = Origin::Mcp { client: "test-client".into() };
    let mut allowed_before_limit = 0;
    loop {
        match svc.check_mail_rate_limit(&origin, "turn-1") {
            Ok(()) => allowed_before_limit += 1,
            Err(e) => {
                assert_eq!(e.code, codes::RATE_LIMITED);
                break;
            }
        }
        assert!(allowed_before_limit < 100, "the per-turn limit must refuse eventually");
    }

    svc.set_mail_categorize_cursor(account, Some("cursor-a".into()));
    svc.set_mail_autodraft_cursor(account, Some("cursor-b".into()));
    svc.mail_summary_cache_put(thread, 3, "a summary".into());

    let spent = svc.mail_categorize_take(5);
    assert_eq!(spent, 5, "a fresh budget must have room for 5");

    svc.close().await;

    let notify_after = svc.outbox_notify(account);
    assert!(
        !Arc::ptr_eq(&notify_before, &notify_after),
        "close() must drop mail_notify's handles, not reuse them"
    );

    assert!(svc.draft_append_due(draft, svc.now()), "the debounce timer must be forgotten");

    assert!(
        svc.check_mail_rate_limit(&origin, "turn-1").is_ok(),
        "close() must clear the rate limiter -- the same turn id must be allowed again"
    );

    assert_eq!(
        svc.mail_categorize_cursor(account),
        None,
        "the categorise cursor must be forgotten"
    );
    assert_eq!(svc.mail_autodraft_cursor(account), None, "the auto-draft cursor must be forgotten");
    assert_eq!(svc.mail_summary_cached(thread, 3), None, "the summary cache must be forgotten");

    // The budget is the one piece of mail session state close() leaves
    // alone: only 15 of the 20-per-minute categorise budget remain,
    // because the 5 spent before close() were never refunded.
    let remaining = svc.mail_categorize_take(Service::MAIL_CATEGORIZE_PER_MINUTE);
    assert_eq!(
        remaining,
        Service::MAIL_CATEGORIZE_PER_MINUTE - spent,
        "close() must not reset the categorisation budget"
    );
}

/// `locked()` is a lighter touch than `close()`: it stops tasks and closes
/// mail's pack store and index, but -- unlike `close()` -- it never clears
/// the routine, feed or mail-cursor bookkeeping at all. That gap is
/// deliberate (a lock screen is not "start this session over") and is
/// exactly the inconsistency phase 9.5 is told to keep, not fix.
#[tokio::test]
async fn locked_leaves_routine_and_mail_cursor_bookkeeping_alone() {
    let (svc, _dir) = encrypted_service();
    let vault = svc.get().unwrap();
    let account = AccountId::new();

    svc.set_running_routine(Some("morning".into()));
    assert!(svc.routine_failed("r1".to_string()), "first report is news");
    let claim = svc.claim_run("run-1".into());
    svc.set_mail_categorize_cursor(account, Some("cursor-a".into()));

    vault.lock();
    svc.locked().await;

    assert_eq!(
        svc.running_routine(),
        Some("morning".into()),
        "locked() must not clear the running routine -- only close() does"
    );
    assert!(
        !svc.routine_failed("r1".to_string()),
        "already reported must still read as already reported"
    );
    assert!(svc.claims_run("run-1"), "locked() must not release a claimed run");
    assert_eq!(
        svc.mail_categorize_cursor(account),
        Some("cursor-a".into()),
        "locked() must not forget the categorise cursor"
    );

    drop(claim);
}

/// `close()`'s hand-written clears cover routine bookkeeping
/// (`reported_routines`, `claimed_runs`, `running_routine`), the feed
/// outage flag, and mail's "show images once" allowance -- all of it, even
/// while a [`RunClaim`](everyday_service::service) guard for a run it just
/// forgot is still alive. The guard's own `Drop` must still be harmless
/// once the set it would have removed from has already been cleared and
/// reused for other runs.
#[tokio::test]
async fn close_clears_routine_feed_and_remote_image_bookkeeping_even_while_a_claim_is_held() {
    let (svc, _dir) = encrypted_service();
    let calendar = CalendarId::new();
    let message = MailMessageId::new();

    svc.set_running_routine(Some("morning".into()));
    assert!(svc.routine_failed("r1".to_string()));
    let claim = svc.claim_run("run-1".into());
    assert!(svc.claims_run("run-1"));
    assert!(svc.feed_failed(calendar));
    svc.allow_remote_images_once(message);
    assert!(svc.remote_images_allowed_once(message));

    svc.close().await;

    assert_eq!(svc.running_routine(), None);
    assert!(svc.routine_failed("r1".to_string()), "cleared, so reporting it again is news again");
    assert!(!svc.claims_run("run-1"), "close() clears claimed runs even with the guard still held");
    assert!(svc.feed_failed(calendar), "cleared, so the outage is news again");
    assert!(!svc.remote_images_allowed_once(message));

    // The guard for the run close() already forgot must still drop
    // harmlessly -- `RunClaim::drop` removes an id from a set that no
    // longer has it, which a plain `HashSet::remove` treats as a no-op
    // rather than an error.
    drop(claim);
    assert!(!svc.claims_run("run-1"));
}

/// The one session state `close()` and `locked()` **never** touch, on
/// either path: the meeting watcher's offered/dismissed set, the spool's
/// last-append clock, and the expiry warning set. There is no bug tracked
/// against this -- a relock does not need "not now" to be asked again, and
/// recovery reading "no entry" as "stale" already covers a process that
/// actually restarted (see `meeting_last_append`'s own field doc). Phase
/// 9.5 must keep this exactly as surprising as it is today.
#[tokio::test]
async fn close_and_locked_never_clear_meeting_session_state() {
    let (svc, _dir) = encrypted_service();
    let vault = svc.get().unwrap();
    let recording = RecordingId::new();
    let calendar = CalendarId::new();

    svc.meeting_touch_append(recording);
    assert!(svc.meeting_since_append(recording).is_some());
    // `meeting_offer_seen` and `meeting_expiry_warn_once` both answer `true`
    // only the first time a given key is seen -- see their own docs.
    assert!(svc.meeting_offer_seen(calendar, "uid-1"), "first ask is new");
    assert!(svc.meeting_expiry_warn_once(recording), "first warning is new");

    vault.lock();
    svc.locked().await;

    assert!(
        svc.meeting_since_append(recording).is_some(),
        "locked() must not forget the append time"
    );
    assert!(
        !svc.meeting_offer_seen(calendar, "uid-1"),
        "locked() must not forget the offer was seen"
    );
    assert!(
        !svc.meeting_expiry_warn_once(recording),
        "locked() must not forget the warning was sent"
    );

    svc.close().await;

    assert!(
        svc.meeting_since_append(recording).is_some(),
        "close() must not forget the append time"
    );
    assert!(
        !svc.meeting_offer_seen(calendar, "uid-1"),
        "close() must not forget the offer was seen"
    );
    assert!(
        !svc.meeting_expiry_warn_once(recording),
        "close() must not forget the warning was sent"
    );
}

/// A retry that arrives after the vault it wrote through has since closed
/// must still be answered from the record, not re-run against a vault that
/// is no longer open -- idempotency is deliberately outside every runtime
/// this refactor introduces. Proven through the public surface: `run_tool`
/// mints a fresh task id on every real run, so a second call with the same
/// request id answering with the *same* id -- after `close()` has dropped
/// the vault a real second run would need -- can only mean the cache
/// answered it.
#[tokio::test]
async fn close_does_not_clear_a_pending_idempotent_write() {
    let (svc, _dir) = encrypted_service();
    let ctx = Ctx::local().with_request_id(Some("idem-1".into()));

    let first = svc
        .call(
            ctx.clone(),
            "run_tool",
            json!({ "name": "create_task", "arguments": { "title": "Buy milk" } }),
        )
        .await
        .expect("the first call must succeed");
    let first_id = first["id"].as_str().expect("a created task must have an id").to_string();

    svc.close().await;

    let second = svc
        .call(
            ctx,
            "run_tool",
            json!({ "name": "create_task", "arguments": { "title": "Buy milk" } }),
        )
        .await
        .expect("a retried request id must be answered from the record, not re-run");
    let second_id =
        second["id"].as_str().expect("the cached answer must still carry the id").to_string();

    assert_eq!(second_id, first_id, "close() must not clear a retry's cached answer");
}
