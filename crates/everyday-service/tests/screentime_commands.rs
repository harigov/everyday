//! Screen time's commands, end to end against a real vault.
//!
//! What is here is what only the command layer decides: which scope reaches
//! which command -- a heartbeats token adds usage and reads nothing, and a
//! client allowed the reports cannot pose as a collector -- what the edge
//! refuses before the vault reads it, that a locked vault says `locked` to a
//! collector, and which changes the writes announce. How heartbeats are
//! joined into spans and spans added up is the core's, and tested there.

use everyday_core::screentime::{
    MAX_BEATS, ScreenApp, ScreenCategory, ScreenTimeSettings, SpanKind,
};
use everyday_service::ctx::{Caller, Ctx, Scope};
use everyday_service::events::{Change, EventSink, Kind, Op};
use everyday_service::{Service, error::CommandError};
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[allow(dead_code)]
mod support;

#[derive(Default)]
struct Collector {
    changes: Mutex<Vec<Change>>,
}

impl EventSink for Collector {
    fn changed(&self, change: Change) {
        self.changes.lock().unwrap().push(change);
    }
}

impl Collector {
    fn take(&self) -> Vec<Change> {
        std::mem::take(&mut *self.changes.lock().unwrap())
    }
}

async fn call(svc: &Arc<Service>, name: &str, args: Value) -> Value {
    svc.call(Ctx::local(), name, args).await.unwrap_or_else(|e| panic!("{name}: {e}"))
}

async fn fails(svc: &Arc<Service>, name: &str, args: Value) -> CommandError {
    svc.call(Ctx::local(), name, args).await.expect_err("expected a failure")
}

/// A caller holding exactly `scopes`, as a paired device or a collector's
/// token would.
fn holding(scopes: Vec<Scope>) -> Ctx {
    Ctx { caller: Caller::Device("a-collector".into()), scopes, proved_at: None, request_id: None }
}

/// One batch with one run of a terminal, a couple of minutes ago -- well
/// inside every clock check the vault makes.
fn one_batch() -> Value {
    let start = Timestamp::now() - SignedDuration::from_secs(120);
    let end = start + SignedDuration::from_secs(30);
    json!({
        "device": "laptop",
        "collector": "gnome",
        "beats": [{
            "kind": "app",
            "app": "org.gnome.Ptyxis",
            "start": start.to_string(),
            "end": end.to_string(),
        }],
    })
}

// ---- scopes ----------------------------------------------------------------

#[tokio::test]
async fn a_heartbeats_token_adds_usage_and_reads_nothing() {
    let (svc, _dir) = support::vault::service(None);
    let collector = holding(vec![Scope::Heartbeats]);

    svc.call(collector.clone(), "record_screen_time", one_batch()).await.unwrap();
    let config = svc.call(collector.clone(), "screen_time_collector", json!({})).await.unwrap();
    assert!(config.get("idleMinutes").is_some() && config.get("keepTitles").is_some(), "{config}");

    for (name, args) in [
        ("screen_time_report", json!({ "query": { "from": "2026-11-01", "to": "2026-11-07" } })),
        ("screen_time_timeline", json!({ "date": "2026-11-05", "tz": "UTC" })),
        ("list_screen_apps", json!({})),
        ("list_screen_categories", json!({})),
        ("list_screen_devices", json!({})),
        ("screen_time_settings", json!({})),
        ("forget_screen_item", json!({ "key": "app:org.gnome.ptyxis" })),
        (
            "delete_screen_time",
            json!({ "from": "2026-11-01T00:00:00Z", "to": "2026-11-02T00:00:00Z" }),
        ),
    ] {
        let e = svc.call(collector.clone(), name, args).await.expect_err(name);
        assert_eq!(e.code, "forbidden", "{name}: {e:?}");
    }
}

#[tokio::test]
async fn the_screen_time_scope_cannot_pose_as_a_collector() {
    let (svc, _dir) = support::vault::service(None);
    let reader = holding(vec![Scope::ScreenTime]);
    svc.call(reader.clone(), "list_screen_apps", json!({})).await.unwrap();
    for (name, args) in [("record_screen_time", one_batch()), ("screen_time_collector", json!({}))]
    {
        let e = svc.call(reader.clone(), name, args).await.expect_err(name);
        assert_eq!(e.code, "forbidden", "{name}: {e:?}");
    }
}

// ---- heartbeats ------------------------------------------------------------

#[tokio::test]
async fn heartbeats_are_taken_and_announced_without_an_id() {
    let (svc, _dir) = support::vault::service(None);
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());

    let report = call(&svc, "record_screen_time", one_batch()).await;
    assert_eq!(report, json!({ "accepted": 1, "rejected": 0, "spans": 1 }));
    let changes = events.take();
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!((changes[0].kind, changes[0].op), (Kind::ScreenSpan, Op::Updated));
    assert_eq!(changes[0].id, None, "spans are never announced one by one");
    assert!(changes[0].ids.is_empty());
}

#[tokio::test]
async fn a_batch_too_long_is_refused_before_the_vault_reads_a_beat_of_it() {
    let (svc, _dir) = support::vault::service(None);
    let beat = one_batch()["beats"][0].clone();
    let beats: Vec<Value> = std::iter::repeat_n(beat, MAX_BEATS + 1).collect();
    let e = fails(
        &svc,
        "record_screen_time",
        json!({ "device": "laptop", "collector": "gnome", "beats": beats }),
    )
    .await;
    assert_eq!(e.code, "too_large", "{e:?}");
}

#[tokio::test]
async fn a_locked_vault_tells_a_collector_it_is_locked() {
    let (svc, _dir) = support::vault::service(Some("correct horse"));
    svc.get().unwrap().lock();
    let collector = holding(vec![Scope::Heartbeats]);
    for (name, args) in [("record_screen_time", one_batch()), ("screen_time_collector", json!({}))]
    {
        let e = svc.call(collector.clone(), name, args).await.expect_err(name);
        assert_eq!(e.code, "locked", "{name}: {e:?}");
    }
}

#[tokio::test]
async fn a_collector_is_told_what_the_settings_say() {
    let (svc, _dir) = support::vault::service(None);
    let settings = ScreenTimeSettings { idle_minutes: 7, keep_titles: false, ..Default::default() };
    call(&svc, "save_screen_time_settings", json!({ "settings": settings })).await;
    let config = svc
        .call(holding(vec![Scope::Heartbeats]), "screen_time_collector", json!({}))
        .await
        .unwrap();
    assert_eq!(config, json!({ "idleMinutes": 7, "keepTitles": false }));
}

// ---- what the edge refuses -------------------------------------------------

#[tokio::test]
async fn keys_device_lists_and_time_zones_longer_than_any_screen_sends_are_refused() {
    let (svc, _dir) = support::vault::service(None);
    let long_key = format!("site:{}", "a".repeat(300));
    let many: Vec<String> = (0..51).map(|n| format!("machine-{n}")).collect();
    let long_tz = "Z".repeat(65);

    let e = fails(&svc, "forget_screen_item", json!({ "key": long_key })).await;
    assert_eq!(e.code, "invalid");
    let label = ScreenApp::new(long_key.clone(), SpanKind::Site);
    let e = fails(&svc, "save_screen_app", json!({ "app": label })).await;
    assert_eq!(e.code, "invalid");

    let e = fails(
        &svc,
        "screen_time_timeline",
        json!({ "date": "2026-11-05", "tz": "UTC", "devices": many }),
    )
    .await;
    assert_eq!(e.code, "invalid");
    let e =
        fails(&svc, "screen_time_timeline", json!({ "date": "2026-11-05", "tz": long_tz })).await;
    assert_eq!(e.code, "invalid");

    for query in [
        json!({ "from": "2026-11-01", "to": "2026-11-07", "devices": many }),
        json!({ "from": "2026-11-01", "to": "2026-11-07", "tz": long_tz }),
        json!({ "from": "2026-11-01", "to": "2026-11-07", "item": long_key }),
    ] {
        let e = fails(&svc, "screen_time_report", json!({ "query": query })).await;
        assert_eq!(e.code, "invalid", "{query}");
    }
}

#[tokio::test]
async fn clearing_a_range_that_ends_before_it_starts_is_refused() {
    let (svc, _dir) = support::vault::service(None);
    let e = fails(
        &svc,
        "delete_screen_time",
        json!({ "from": "2026-11-05T12:00:00Z", "to": "2026-11-05T11:00:00Z" }),
    )
    .await;
    assert_eq!(e.code, "invalid");
}

// ---- labels and categories -------------------------------------------------

#[tokio::test]
async fn the_starter_categories_are_written_by_the_first_listing_and_only_once() {
    let (svc, _dir) = support::vault::service(None);
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());
    let first = call(&svc, "list_screen_categories", json!({})).await;
    let again = call(&svc, "list_screen_categories", json!({})).await;
    assert!(!first.as_array().unwrap().is_empty(), "a vault starts with some categories");
    assert_eq!(first, again, "listing twice writes them once");
    assert!(events.take().is_empty(), "a listing announces nothing");
}

#[tokio::test]
async fn labels_and_categories_announce_the_record_they_wrote() {
    let (svc, _dir) = support::vault::service(None);
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());

    let category = ScreenCategory::new("Side projects");
    call(&svc, "save_screen_category", json!({ "category": category })).await;
    let mut label = ScreenApp::new("app:org.gnome.ptyxis", SpanKind::App);
    label.name = "Terminal".into();
    label.category = Some(category.id);
    call(&svc, "save_screen_app", json!({ "app": label })).await;
    call(&svc, "delete_screen_app", json!({ "id": label.id })).await;
    call(&svc, "delete_screen_category", json!({ "id": category.id })).await;

    let seen: Vec<(Kind, Op, Option<String>)> =
        events.take().into_iter().map(|c| (c.kind, c.op, c.id)).collect();
    assert_eq!(
        seen,
        vec![
            (Kind::ScreenCategory, Op::Updated, Some(category.id.to_string())),
            (Kind::ScreenApp, Op::Updated, Some(label.id.to_string())),
            (Kind::ScreenApp, Op::Deleted, Some(label.id.to_string())),
            (Kind::ScreenCategory, Op::Deleted, Some(category.id.to_string())),
        ]
    );
}

#[tokio::test]
async fn forgetting_announces_spans_without_an_id_and_answers_how_many_went() {
    let (svc, _dir) = support::vault::service(None);
    call(&svc, "record_screen_time", one_batch()).await;
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());

    let gone = call(&svc, "forget_screen_item", json!({ "key": "app:org.gnome.ptyxis" })).await;
    assert_eq!(gone, json!(1));
    let from = (Timestamp::now() - SignedDuration::from_secs(3600)).to_string();
    let none = call(
        &svc,
        "delete_screen_time",
        json!({ "from": from, "to": Timestamp::now().to_string() }),
    )
    .await;
    assert_eq!(none, json!(0), "the one span was already forgotten");

    let seen: Vec<(Kind, Op, Option<String>)> =
        events.take().into_iter().map(|c| (c.kind, c.op, c.id)).collect();
    assert_eq!(
        seen,
        vec![(Kind::ScreenSpan, Op::Deleted, None), (Kind::ScreenSpan, Op::Deleted, None)]
    );
}
