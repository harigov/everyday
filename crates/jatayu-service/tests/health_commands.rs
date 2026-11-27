//! Health's commands, end to end against a real vault.
//!
//! What is here is what only the command layer decides: which scope reaches
//! which command -- a `healthIngest` token pushes and reads nothing, a client
//! allowed to read somebody's health cannot pose as their phone, and the
//! food-lookup key is the whole vault's to set -- what the edge refuses
//! before the vault reads it, that a locked vault says `locked` to a phone,
//! that a push which is not a push is noted on its source, and which changes
//! the writes announce. How a push is read, how samples are filed and what a
//! day's numbers add up to are the core's, and tested there.

use jatayu_core::id::{ConditionId, DoseId, HealthSourceId, MedicationId};
use jatayu_service::ctx::{Caller, Ctx, Scope};
use jatayu_service::events::{Change, EventSink, Kind, Op};
use jatayu_service::{Service, error::CommandError};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[allow(dead_code)]
mod support;

/// Fifty characters or so of what a FoodData Central key looks like, ending
/// in something to look for.
const KEY: &str = "abcd1234efgh5678ijkl9012mnop3456qrst7890wxyz";

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

    /// Every change since the last look, as (kind, op, id).
    fn seen(&self) -> Vec<(Kind, Op, Option<String>)> {
        self.take().into_iter().map(|c| (c.kind, c.op, c.id)).collect()
    }
}

async fn call(svc: &Arc<Service>, name: &str, args: Value) -> Value {
    svc.call(Ctx::local(), name, args).await.unwrap_or_else(|e| panic!("{name}: {e}"))
}

async fn fails(svc: &Arc<Service>, name: &str, args: Value) -> CommandError {
    svc.call(Ctx::local(), name, args).await.expect_err("expected a failure")
}

/// A caller holding exactly `scopes`, as a paired device or the phone's
/// token would.
fn holding(scopes: Vec<Scope>) -> Ctx {
    Ctx { caller: Caller::Device("a-phone".into()), scopes, proved_at: None, request_id: None }
}

/// A source of `connector`, as the interface would save a new one.
fn source(connector: &str) -> (HealthSourceId, Value) {
    let id = HealthSourceId::new();
    let now = Timestamp::now();
    let source = json!({
        "id": id,
        "connector": connector,
        "name": "Hari's iPhone",
        "createdAt": now,
        "updatedAt": now,
    });
    (id, source)
}

/// What Health Auto Export posts for one automation, with `body` as its JSON.
fn push_args(source: HealthSourceId, body: Value) -> Value {
    json!({
        "sourceId": source,
        "headers": {
            "automationId": "hourly-metrics",
            "automationName": "Hourly metrics",
            "automationPeriod": "Since Last Sync",
        },
        "body": body,
    })
}

/// One reading of steps from two hours ago, in Health Auto Export's own
/// shape and date format.
fn steps_push() -> Value {
    let when = (Timestamp::now() - SignedDuration::from_secs(2 * 3600))
        .to_zoned(TimeZone::UTC)
        .strftime("%Y-%m-%d %H:%M:%S %z")
        .to_string();
    json!({ "data": { "metrics": [{
        "name": "step_count",
        "units": "count",
        "data": [{ "date": when, "qty": 120, "source": "Hari's Apple Watch" }],
    }]}})
}

fn today() -> String {
    Timestamp::now().to_zoned(TimeZone::UTC).date().to_string()
}

// ---- scopes ----------------------------------------------------------------

#[tokio::test]
async fn a_push_token_pushes_and_reads_nothing() {
    let (svc, _dir) = support::vault::service(None);
    let (phone, record) = source("healthAutoExport");
    call(&svc, "save_health_source", json!({ "source": record })).await;
    // The phone's token is bound to its source, as `health_push_setup` binds it.
    call(&svc, "set_health_source_token", json!({ "sourceId": phone, "tokenId": "a-phone" })).await;
    let pusher = holding(vec![Scope::HealthIngest]);

    // What the push does with its body is the connector's to test. Here it
    // only has to be let in.
    let pushed = svc.call(pusher.clone(), "record_health_push", push_args(phone, steps_push()));
    if let Err(e) = pushed.await {
        assert_ne!(e.code, "forbidden", "{e:?}");
    }

    for (name, args) in [
        ("health_catalog", json!({})),
        ("health_settings", json!({})),
        ("save_health_settings", json!({})),
        ("list_health_sources", json!({})),
        ("health_today", json!({ "date": "2026-11-05", "tz": "UTC" })),
        ("list_medications", json!({})),
        ("list_conditions", json!({})),
        ("list_health_documents", json!({})),
        ("health_keys", json!({})),
        ("import_apple_health", json!({ "path": "/tmp/export.zip" })),
        ("apple_import_status", json!({})),
        ("cancel_apple_import", json!({})),
        ("search_foods", json!({ "query": "oats" })),
    ] {
        let e = svc.call(pusher.clone(), name, args).await.expect_err(name);
        assert_eq!(e.code, "forbidden", "{name}: {e:?}");
    }
}

#[tokio::test]
async fn the_health_scope_cannot_pose_as_the_phone() {
    let (svc, _dir) = support::vault::service(None);
    let reader = holding(vec![Scope::Health]);

    let catalog = svc.call(reader.clone(), "health_catalog", json!({})).await.unwrap();
    assert!(
        catalog.as_array().unwrap().iter().any(|m| m["metric"] == "steps"),
        "the catalogue lists the metrics: {catalog}"
    );

    let (phone, _) = source("healthAutoExport");
    let e = svc
        .call(reader, "record_health_push", push_args(phone, steps_push()))
        .await
        .expect_err("record_health_push");
    assert_eq!(e.code, "forbidden", "{e:?}");
}

#[tokio::test]
async fn the_food_lookup_key_is_the_whole_vaults_to_set_and_never_answered() {
    let (svc, _dir) = support::vault::service(None);
    let reader = holding(vec![Scope::Health]);

    for (name, args) in
        [("save_health_keys", json!({ "fdcKey": KEY })), ("clear_health_keys", json!({}))]
    {
        let e = svc.call(reader.clone(), name, args).await.expect_err(name);
        assert_eq!(e.code, "forbidden", "{name}: {e:?}");
    }
    let none = svc.call(reader.clone(), "health_keys", json!({})).await.unwrap();
    assert_eq!(none["fdcKeySet"], json!(false), "a refused save stores nothing: {none}");

    call(&svc, "save_health_keys", json!({ "fdcKey": KEY })).await;
    let info = svc.call(reader.clone(), "health_keys", json!({})).await.unwrap();
    assert_eq!(info["fdcKeySet"], json!(true), "{info}");
    assert!(info["fdcKeyHint"].as_str().unwrap().ends_with("wxyz"), "{info}");
    assert!(!info.to_string().contains(KEY), "the key itself was answered: {info}");

    call(&svc, "clear_health_keys", json!({})).await;
    let cleared = svc.call(reader, "health_keys", json!({})).await.unwrap();
    assert_eq!(cleared["fdcKeySet"], json!(false), "{cleared}");
}

#[tokio::test]
async fn a_key_that_is_blank_or_longer_than_any_is_refused() {
    let (svc, _dir) = support::vault::service(None);
    for key in ["   ".to_string(), "k".repeat(201)] {
        let e = fails(&svc, "save_health_keys", json!({ "fdcKey": key })).await;
        assert_eq!(e.code, "invalid", "{key:?}");
    }
}

// ---- the push --------------------------------------------------------------

#[tokio::test]
async fn a_locked_vault_tells_the_phone_it_is_locked() {
    let (svc, _dir) = support::vault::service(Some("correct horse"));
    let (phone, _) = source("healthAutoExport");
    svc.get().unwrap().lock();
    let e = svc
        .call(
            holding(vec![Scope::HealthIngest]),
            "record_health_push",
            push_args(phone, steps_push()),
        )
        .await
        .expect_err("a push to a locked vault");
    assert_eq!(e.code, "locked", "{e:?}");
}

#[tokio::test]
async fn a_push_that_is_not_a_push_is_refused_and_noted_on_its_source() {
    let (svc, _dir) = support::vault::service(None);
    let (phone, record) = source("healthAutoExport");
    call(&svc, "save_health_source", json!({ "source": record })).await;
    // The phone's token is bound to its source, as `health_push_setup` binds it.
    call(&svc, "set_health_source_token", json!({ "sourceId": phone, "tokenId": "a-phone" })).await;
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());

    let e = svc
        .call(
            holding(vec![Scope::HealthIngest]),
            "record_health_push",
            push_args(phone, json!("this is not what Health Auto Export sends")),
        )
        .await
        .expect_err("a body that is not a push");
    assert_eq!(e.code, "invalid", "{e:?}");
    assert!(events.take().is_empty(), "a refused push announces nothing");

    let sources = call(&svc, "list_health_sources", json!({})).await;
    let noted = sources
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == json!(phone))
        .unwrap_or_else(|| panic!("the source is gone: {sources}"));
    assert!(
        !noted["status"]["lastError"].as_str().unwrap_or_default().is_empty(),
        "the source says nothing about the push it could not read: {noted}"
    );
}

#[tokio::test]
async fn a_push_lands_and_is_announced_without_an_id() {
    let (svc, _dir) = support::vault::service(None);
    let (phone, record) = source("healthAutoExport");
    call(&svc, "save_health_source", json!({ "source": record })).await;
    // The phone's token is bound to its source, as `health_push_setup` binds it.
    call(&svc, "set_health_source_token", json!({ "sourceId": phone, "tokenId": "a-phone" })).await;
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());

    let report = svc
        .call(
            holding(vec![Scope::HealthIngest]),
            "record_health_push",
            push_args(phone, steps_push()),
        )
        .await
        .unwrap();
    assert_eq!(report["samplesAdded"], json!(1), "{report}");

    let changes = events.take();
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!((changes[0].kind, changes[0].op), (Kind::HealthSample, Op::Updated));
    assert_eq!(changes[0].id, None, "samples are never announced one by one");
    assert!(changes[0].ids.is_empty());

    let now = Timestamp::now();
    let points = call(
        &svc,
        "health_samples",
        json!({
            "metric": "steps",
            "from": now - SignedDuration::from_hours(24),
            "to": now + SignedDuration::from_hours(1),
        }),
    )
    .await;
    let points = points.as_array().unwrap();
    assert_eq!(points.len(), 1, "{points:?}");
    assert_eq!(points[0]["value"].as_f64(), Some(120.0));

    let sources = call(&svc, "list_health_sources", json!({})).await;
    let heard = sources.as_array().unwrap().iter().find(|s| s["id"] == json!(phone)).unwrap();
    assert!(heard["status"]["lastReceivedAt"].is_string(), "the source never heard: {heard}");
}

// ---- what the edge refuses -------------------------------------------------

#[tokio::test]
async fn ranges_and_sizes_no_screen_sends_are_refused_before_the_vault_reads_them() {
    let (svc, _dir) = support::vault::service(None);
    let long_tz = "Z".repeat(65);
    let many: Vec<&str> = std::iter::repeat_n("steps", 51).collect();
    let too_long = "a".repeat(201);
    let years = json!({ "from": "2020-01-01", "to": "2026-11-01", "tz": "UTC" });
    let backwards = json!({ "from": "2026-11-02", "to": "2026-11-01", "tz": "UTC" });
    let instants = json!({ "from": "2020-01-01T00:00:00Z", "to": "2026-11-01T00:00:00Z" });
    let now = Timestamp::now();

    for (name, args) in [
        // Local dates: more than 800 days, backwards, and a zone too long.
        (
            "health_series",
            json!({ "query": { "metric": "steps", "from": "2020-01-01", "to": "2026-11-01" } }),
        ),
        (
            "health_series",
            json!({ "query": { "metric": "steps", "from": "2026-11-02", "to": "2026-11-01" } }),
        ),
        (
            "health_series",
            json!({ "query": {
                "metric": "steps", "from": "2026-11-01", "to": "2026-11-02", "tz": long_tz,
            }}),
        ),
        ("health_activity", years.clone()),
        ("health_sleep", years.clone()),
        ("health_glucose", years.clone()),
        ("health_nutrition", years.clone()),
        ("medication_alerts", years.clone()),
        ("health_activity", backwards.clone()),
        ("medication_adherence", json!({ "from": "2020-01-01", "to": "2026-11-01", "tz": "UTC" })),
        ("health_today", json!({ "date": "2026-11-05", "tz": long_tz })),
        ("health_day_timeline", json!({ "date": "2026-11-05", "tz": long_tz })),
        ("doses_due", json!({ "date": "2026-11-05", "tz": long_tz })),
        (
            "condition_report",
            json!({ "id": ConditionId::new(), "date": "2026-11-05", "tz": long_tz }),
        ),
        ("health_visit_brief", json!({ "since": "2026-10-01", "tz": long_tz })),
        // Instants: raw samples past three days, ranges past 800, backwards.
        (
            "health_samples",
            json!({
                "metric": "heart_rate",
                "from": "2026-11-01T00:00:00Z",
                "to": "2026-11-05T00:00:01Z",
            }),
        ),
        (
            "health_samples",
            json!({
                "metric": "heart_rate",
                "from": "2026-11-02T00:00:00Z",
                "to": "2026-11-01T00:00:00Z",
            }),
        ),
        ("list_workouts", instants.clone()),
        ("list_meals", instants.clone()),
        ("list_doses", instants.clone()),
        (
            "delete_health_samples",
            json!({
                "metric": "weight",
                "from": "2026-11-02T00:00:00Z",
                "to": "2026-11-01T00:00:00Z",
            }),
        ),
        // Lists and text.
        ("health_latest", json!({ "metrics": many, "on": "2026-11-05" })),
        (
            "add_health_reading",
            json!({ "reading": { "metric": "weight", "at": now, "value": 80.0, "tz": long_tz } }),
        ),
        ("list_foods", json!({ "query": too_long })),
        ("search_foods", json!({ "query": too_long })),
        ("list_labs", json!({ "key": too_long })),
        // A barcode is eight to fourteen digits.
        ("lookup_barcode", json!({ "code": "12ab5678" })),
        ("lookup_barcode", json!({ "code": "1234567" })),
        ("lookup_barcode", json!({ "code": "123456789012345" })),
    ] {
        let e = fails(&svc, name, args.clone()).await;
        assert_eq!(e.code, "invalid", "{name}: {args}: {e:?}");
    }
}

#[tokio::test]
async fn a_document_of_more_files_or_more_text_than_any_scan_is_refused() {
    let (svc, _dir) = support::vault::service(None);
    // A page is a file in the files catalog, named by its id.
    let page = json!("00000000-0000-0000-0000-000000000000");
    let pages: Vec<Value> = std::iter::repeat_n(page, 101).collect();
    let e = fails(
        &svc,
        "add_health_document",
        json!({ "title": "A book", "kind": "other", "files": pages }),
    )
    .await;
    assert_eq!(e.code, "too_large", "{e:?}");

    let e = fails(
        &svc,
        "extract_health_document",
        json!({
            "id": "00000000-0000-0000-0000-000000000000",
            "text": "x".repeat(2_000_001),
        }),
    )
    .await;
    assert_eq!(e.code, "too_large", "{e:?}");
}

// ---- medications -----------------------------------------------------------

#[tokio::test]
async fn a_medication_is_saved_taken_and_deleted_with_its_doses() {
    let (svc, _dir) = support::vault::service(None);
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());
    let now = Timestamp::now();
    let hour = SignedDuration::from_hours(1);

    let medication = MedicationId::new();
    let saved = call(
        &svc,
        "save_medication",
        json!({ "medication": {
            "id": medication,
            "name": "Levothyroxine",
            "strength": "50 mcg",
            "started": "2026-01-01",
            "createdAt": now,
            "updatedAt": now,
        }}),
    )
    .await;
    assert_eq!(saved["name"], "Levothyroxine");
    let listed = call(&svc, "list_medications", json!({})).await;
    assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");

    let dose = DoseId::new();
    call(
        &svc,
        "log_dose",
        json!({ "dose": {
            "id": dose,
            "medicationId": medication,
            "at": now,
            "status": "taken",
            "createdAt": now,
            "updatedAt": now,
        }}),
    )
    .await;
    let window = json!({ "from": now - hour, "to": now + hour });
    let all = call(&svc, "list_doses", window.clone()).await;
    assert_eq!(all.as_array().unwrap().len(), 1, "{all}");
    let mine = call(
        &svc,
        "list_doses",
        json!({ "from": now - hour, "to": now + hour, "medicationId": medication }),
    )
    .await;
    assert_eq!(mine.as_array().unwrap().len(), 1, "{mine}");
    let someone_elses = call(
        &svc,
        "list_doses",
        json!({ "from": now - hour, "to": now + hour, "medicationId": MedicationId::new() }),
    )
    .await;
    assert!(someone_elses.as_array().unwrap().is_empty(), "{someone_elses}");

    let due = call(&svc, "doses_due", json!({ "date": today(), "tz": "UTC" })).await;
    assert!(
        due.as_array().unwrap().iter().any(|d| d["medicationId"] == json!(medication)),
        "a daily medication is due today: {due}"
    );

    call(&svc, "delete_dose", json!({ "id": dose })).await;
    let none = call(&svc, "list_doses", window.clone()).await;
    assert!(none.as_array().unwrap().is_empty(), "{none}");

    // Deleting a medication takes its doses with it.
    let again = DoseId::new();
    call(
        &svc,
        "log_dose",
        json!({ "dose": {
            "id": again,
            "medicationId": medication,
            "at": now,
            "createdAt": now,
            "updatedAt": now,
        }}),
    )
    .await;
    call(&svc, "delete_medication", json!({ "id": medication })).await;
    assert!(call(&svc, "list_medications", json!({})).await.as_array().unwrap().is_empty());
    let gone = call(&svc, "list_doses", window).await;
    assert!(gone.as_array().unwrap().is_empty(), "{gone}");

    let seen = events.seen();
    for expected in [
        (Kind::Medication, Op::Updated, Some(medication.to_string())),
        (Kind::Dose, Op::Updated, Some(dose.to_string())),
        (Kind::Dose, Op::Deleted, Some(dose.to_string())),
        (Kind::Medication, Op::Deleted, Some(medication.to_string())),
    ] {
        assert!(seen.contains(&expected), "{expected:?} was not announced: {seen:?}");
    }
}

// ---- conditions ------------------------------------------------------------

#[tokio::test]
async fn a_condition_template_is_copied_into_a_condition_the_person_owns() {
    let (svc, _dir) = support::vault::service(None);
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());

    let templates = call(&svc, "condition_templates", json!({})).await;
    let keys: Vec<&str> =
        templates.as_array().unwrap().iter().map(|t| t["key"].as_str().unwrap()).collect();
    for key in ["type1Diabetes", "type2Diabetes", "hypothyroidism"] {
        assert!(keys.contains(&key), "{key} is not offered: {keys:?}");
    }
    assert!(events.take().is_empty(), "listing the templates announces nothing");

    let condition = call(
        &svc,
        "apply_condition_template",
        json!({ "template": "hypothyroidism", "choice": {} }),
    )
    .await;
    assert_eq!(condition["template"], json!("hypothyroidism"), "{condition}");
    assert!(!condition["name"].as_str().unwrap().is_empty(), "{condition}");
    assert!(
        condition["plan"]["targets"].is_array() && condition["plan"]["labs"].is_array(),
        "the plan was copied onto the condition: {condition}"
    );

    let listed = call(&svc, "list_conditions", json!({})).await;
    assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
    assert_eq!(listed[0]["id"], condition["id"]);

    let report = call(
        &svc,
        "condition_report",
        json!({ "id": condition["id"], "date": today(), "tz": "UTC" }),
    )
    .await;
    assert_eq!(report["condition"]["id"], condition["id"], "{report}");

    let id = condition["id"].as_str().unwrap().to_string();
    call(&svc, "delete_condition", json!({ "id": id, "removeTrackers": true })).await;
    assert!(call(&svc, "list_conditions", json!({})).await.as_array().unwrap().is_empty());

    let seen = events.seen();
    for expected in
        [(Kind::Condition, Op::Created, None), (Kind::Condition, Op::Deleted, Some(id.clone()))]
    {
        assert!(seen.contains(&expected), "{expected:?} was not announced: {seen:?}");
    }
}

// ---- the import ------------------------------------------------------------

#[tokio::test]
async fn an_import_that_cannot_start_says_why_and_one_that_starts_ends_in_the_status() {
    let (svc, dir) = support::vault::service(None);

    // Nothing there, a folder, and nothing at all.
    let missing = dir.path().join("nothing.zip");
    let e = fails(&svc, "import_apple_health", json!({ "path": missing.to_str().unwrap() })).await;
    assert_eq!(e.code, "not_found", "{e:?}");
    let folder = dir.path().to_str().unwrap();
    let e = fails(&svc, "import_apple_health", json!({ "path": folder })).await;
    assert_eq!(e.code, "invalid", "{e:?}");
    let e = fails(&svc, "import_apple_health", json!({ "path": "  " })).await;
    assert_eq!(e.code, "invalid", "{e:?}");

    // A path is read on this computer, so only from it.
    let file = dir.path().join("export.zip");
    std::fs::write(&file, b"this is not a zip").unwrap();
    let path = file.to_str().unwrap();
    let e = svc
        .call(holding(vec![Scope::All]), "import_apple_health", json!({ "path": path }))
        .await
        .expect_err("a paired device naming a path");
    assert_eq!(e.code, "forbidden", "{e:?}");

    // A file that is not an export starts, and ends failed with a reason.
    // This is the only test in this file that starts an import: the status
    // is the process's, shared by every test that runs beside it.
    let started = call(&svc, "import_apple_health", json!({ "path": path })).await;
    assert_eq!(started["state"], "running", "{started}");
    assert_eq!(started["fileName"], "export.zip", "{started}");

    let mut status = call(&svc, "apple_import_status", json!({})).await;
    for _ in 0..400 {
        if status["state"] != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
        status = call(&svc, "apple_import_status", json!({})).await;
    }
    assert_eq!(status["state"], "failed", "{status}");
    assert!(!status["error"].as_str().unwrap().is_empty(), "{status}");
    assert!(status["finishedAt"].is_string(), "{status}");

    // Nothing is running now, so there is nothing to cancel.
    assert_eq!(call(&svc, "cancel_apple_import", json!({})).await, json!(false));

    // From a file in Files: a sealed upload, read from a decrypted copy that
    // is gone once the import ends, and -- uploaded only to be read -- put
    // in the trash however it ended. Here that is failed, as above.
    let vault = svc.require_unlocked().unwrap();
    vault.set_up_files_folder(Some(&dir.path().join("Jatayu"))).unwrap();
    let sealed = jatayu_core::files::ImportOptions {
        encrypted: Some(true),
        ..jatayu_core::files::ImportOptions::new(jatayu_core::files::FileOrigin::Health)
    };
    let upload = vault.import_bytes(b"this is not a zip either", "export.zip", &sealed).unwrap();
    let notes = vault
        .import_bytes(
            b"a list",
            "notes.txt",
            &jatayu_core::files::ImportOptions::new(jatayu_core::files::FileOrigin::Files),
        )
        .unwrap();
    let e = fails(&svc, "import_apple_health_file", json!({ "file": notes.id })).await;
    assert_eq!(e.code, "invalid", "only a .zip is an export: {e:?}");
    let e = svc
        .call(
            holding(vec![Scope::Health]),
            "import_apple_health_file",
            json!({ "file": upload.id }),
        )
        .await
        .expect_err("a connection without Files");
    assert_eq!(e.code, "forbidden", "{e:?}");

    let started =
        call(&svc, "import_apple_health_file", json!({ "file": upload.id, "trashAfter": true }))
            .await;
    assert_eq!(started["state"], "running", "{started}");
    let mut status = call(&svc, "apple_import_status", json!({})).await;
    for _ in 0..400 {
        if status["state"] != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
        status = call(&svc, "apple_import_status", json!({})).await;
    }
    assert_eq!(status["state"], "failed", "{status}");
    assert!(vault.file(upload.id).unwrap().status.is_trashed(), "the upload is in the trash");
    let root = vault.files_folder().unwrap().expect("a files folder").root().to_path_buf();
    let copy = root.join(".jatayu").join("open").join(upload.id.to_string()).join("export.zip");
    assert!(!copy.exists(), "the decrypted copy is gone: {}", copy.display());
}

// ---- Health's files and the Files commands ---------------------------------

/// A connection allowed Files and not Health reads the folder, not somebody's
/// lab reports: it is listed none of Health's files -- a listing would hand
/// it their blob addresses -- and refused anything done to one by id.
#[tokio::test]
async fn a_connection_without_health_is_shown_and_allowed_none_of_healths_files() {
    use jatayu_core::files::{FileOrigin, ImportOptions};
    let (svc, dir) = support::vault::service(None);
    let vault = svc.require_unlocked().unwrap();
    // A folder of its own: the default one is shared by every test here.
    vault.set_up_files_folder(Some(&dir.path().join("Jatayu"))).unwrap();
    let sealed = ImportOptions { encrypted: Some(true), ..ImportOptions::new(FileOrigin::Health) };
    let page = vault.import_bytes(b"TSH 5.6 mIU/L", "labs.pdf", &sealed).unwrap();
    vault.import_bytes(b"a list", "list.txt", &ImportOptions::new(FileOrigin::Files)).unwrap();
    let names = |page: &Value| -> Vec<String> {
        let mut names: Vec<String> = page["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap().to_string())
            .collect();
        names.sort();
        names
    };

    let files_only = || holding(vec![Scope::Files]);
    let listed = svc.call(files_only(), "list_files", json!({ "query": {} })).await.unwrap();
    assert_eq!(names(&listed), ["list.txt"], "{listed}");
    assert_eq!(listed["total"], 1);
    for (name, args) in [
        ("get_file", json!({ "id": page.id })),
        ("trash_files", json!({ "ids": [page.id] })),
        ("delete_files_forever", json!({ "ids": [page.id] })),
        ("set_file_encrypted", json!({ "id": page.id, "encrypted": false })),
        ("rename_file", json!({ "id": page.id, "name": "mine.pdf" })),
        ("update_files", json!({ "ids": [page.id], "update": { "caption": "x" } })),
    ] {
        let e = svc.call(files_only(), name, args).await.expect_err(name);
        assert_eq!(e.code, "forbidden", "{name}: {e:?}");
    }
    let untouched = vault.file(page.id).unwrap();
    assert!(
        untouched.is_sealed() && !untouched.status.is_trashed() && untouched.caption.is_empty()
    );

    // Holding Health as well, they are there like any other.
    let both = || holding(vec![Scope::Files, Scope::Health]);
    let listed = svc.call(both(), "list_files", json!({ "query": {} })).await.unwrap();
    assert_eq!(names(&listed), ["labs.pdf", "list.txt"], "{listed}");
    let detail = svc.call(both(), "get_file", json!({ "id": page.id })).await.unwrap();
    assert_eq!(detail["file"]["origin"], "health", "{detail}");
}
