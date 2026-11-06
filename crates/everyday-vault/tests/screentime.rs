//! The rules the vault applies to screen time, over a real SQLite backend:
//! heartbeats joined into spans per stream, a browser's time never counted
//! twice, the one-label-per-key rule, categories and the labels filed under
//! them, settings, forgetting, retention, and screen time as a tracker's
//! source.
//!
//! Heartbeats are refused when they are more than thirty days old or in the
//! future, so every test here records them a few hours before the real
//! clock and reports over the days around it, rather than on fixed dates.

mod support;
use support::vault;

use everyday_core::screentime::{
    Heartbeat, HeartbeatBatch, IngestReport, ScreenApp, ScreenCategory, ScreenQuery, ScreenReport,
    ScreenTimeSettings, SpanKind,
};
use everyday_core::{
    Error, ReadingQuery, ScreenAppId, ScreenCategoryId, Tracker, TrackerKind, TrackerSource, Vault,
};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

/// Two hours ago, to the second: far enough back that a run of a few
/// minutes never reaches the future, near enough to be inside any backlog.
fn base() -> Timestamp {
    Timestamp::from_second(Timestamp::now().as_second() - 2 * 3600).unwrap()
}

fn plus(at: Timestamp, secs: i64) -> Timestamp {
    at + SignedDuration::from_secs(secs)
}

/// A run of an app, `from..to` seconds after `at`.
fn app(name: &str, at: Timestamp, from: i64, to: i64) -> Heartbeat {
    Heartbeat {
        kind: SpanKind::App,
        app: name.into(),
        name: String::new(),
        host: String::new(),
        title: String::new(),
        start: plus(at, from),
        end: Some(plus(at, to)),
    }
}

/// A run of a site in Firefox, `from..to` seconds after `at`.
fn site(host: &str, at: Timestamp, from: i64, to: i64) -> Heartbeat {
    Heartbeat {
        kind: SpanKind::Site,
        app: "Firefox".into(),
        host: host.into(),
        ..app("", at, from, to)
    }
}

fn batch(device: &str, collector: &str, beats: Vec<Heartbeat>) -> HeartbeatBatch {
    HeartbeatBatch { device: device.into(), collector: collector.into(), beats }
}

fn record(vault: &Vault, collector: &str, beats: Vec<Heartbeat>) -> IngestReport {
    vault.record_heartbeats(&batch("laptop", collector, beats)).unwrap()
}

fn utc_day(at: Timestamp) -> Date {
    at.to_zoned(TimeZone::UTC).date()
}

/// Everything from three days ago to tomorrow, in UTC.
fn report(vault: &Vault) -> ScreenReport {
    let today = utc_day(Timestamp::now());
    vault
        .screen_report(&ScreenQuery {
            from: today.checked_sub(jiff::Span::new().days(3)).unwrap(),
            to: today.tomorrow().unwrap(),
            tz: "UTC".into(),
            devices: Vec::new(),
            category: None,
            item: None,
        })
        .unwrap()
}

fn secs_of(report: &ScreenReport, key: &str) -> u64 {
    report.items.iter().filter(|i| i.key == key).map(|i| i.secs).sum()
}

fn category(vault: &Vault, builtin: &str) -> ScreenCategory {
    vault.screen_categories().unwrap().into_iter().find(|c| c.builtin == builtin).unwrap()
}

fn invalid<T: std::fmt::Debug>(result: everyday_core::Result<T>) -> String {
    match result {
        Err(Error::Invalid(message)) => message,
        other => panic!("expected Invalid, got {other:?}"),
    }
}

// ---- heartbeats ------------------------------------------------------------

#[test]
fn heartbeats_join_into_one_span_across_batches() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let at = base();

    let beat = |i: i64| Heartbeat { end: None, ..app("org.gnome.Ptyxis", at, i * 5, i * 5) };
    let first = record(&vault, "gnome", (0..12).map(beat).collect());
    assert_eq!(first, IngestReport { accepted: 12, rejected: 0, spans: 1 });
    // The next flush carries on where the last left off: the same span.
    let second = record(&vault, "gnome", (12..24).map(beat).collect());
    assert_eq!(second, IngestReport { accepted: 12, rejected: 0, spans: 1 });

    let timeline = vault.screen_timeline(utc_day(at), "UTC", &[]).unwrap();
    assert_eq!(timeline.runs.len(), 1, "{:?}", timeline.runs);
    assert_eq!(timeline.runs[0].start, at);
    assert_eq!(timeline.runs[0].end, plus(at, 115));
    assert_eq!(timeline.runs[0].name, "Terminal");
    assert_eq!(report(&vault).total_secs, 115);
}

#[test]
fn a_heartbeat_already_inside_the_span_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let at = base();
    record(&vault, "gnome", vec![app("code", at, 0, 600)]);
    let again = record(&vault, "gnome", vec![app("code", at, 100, 200)]);
    assert_eq!(again, IngestReport { accepted: 1, rejected: 0, spans: 0 });
    assert_eq!(report(&vault).total_secs, 600);
}

#[test]
fn each_collector_is_its_own_stream_and_each_device_is_listed() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let at = base();
    record(&vault, "gnome", vec![app("code", at, 0, 60)]);
    record(&vault, "firefox", vec![site("github.com", at, 100, 160)]);
    vault
        .record_heartbeats(&batch("desktop", "windows", vec![app("Code.exe", at, 0, 30)]))
        .unwrap();

    let devices = vault.screen_devices().unwrap();
    let laptop = devices.iter().find(|d| d.device == "laptop").unwrap();
    assert_eq!(laptop.collectors, ["firefox", "gnome"]);
    assert!(devices.iter().any(|d| d.device == "desktop"));

    let r = report(&vault);
    assert_eq!(secs_of(&r, "app:code"), 60);
    assert_eq!(secs_of(&r, "app:code.exe"), 30);
    assert_eq!(secs_of(&r, "site:github.com"), 60);
}

#[test]
fn bad_beats_are_counted_and_a_bad_batch_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let at = base();
    let long_ago = Timestamp::now() - SignedDuration::from_hours(24 * 40);
    let report = record(
        &vault,
        "gnome",
        vec![
            app("code", at, 0, 60),
            app("", at, 0, 60),
            app("code", at, 60, 30),
            app("code", long_ago, 0, 60),
            app("code", Timestamp::now(), 3600, 3700),
            site("", at, 0, 60),
        ],
    );
    assert_eq!(report, IngestReport { accepted: 1, rejected: 5, spans: 1 });

    invalid(vault.record_heartbeats(&batch("", "gnome", vec![app("code", at, 0, 1)])));
    invalid(vault.record_heartbeats(&batch("laptop", "GNOME!", vec![app("code", at, 0, 1)])));
}

#[test]
fn titles_are_dropped_on_arrival_unless_kept() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let at = base();
    let titled =
        |from, to, title: &str| Heartbeat { title: title.into(), ..app("code", at, from, to) };

    vault
        .save_screen_settings(&ScreenTimeSettings { keep_titles: false, ..Default::default() })
        .unwrap();
    record(&vault, "gnome", vec![titled(0, 60, "dropped.rs")]);
    vault.save_screen_settings(&ScreenTimeSettings::default()).unwrap();
    record(&vault, "gnome", vec![titled(600, 660, "kept.rs")]);

    let timeline = vault.screen_timeline(utc_day(at), "UTC", &[]).unwrap();
    let titles: Vec<&str> = timeline.runs.iter().map(|r| r.title.as_str()).collect();
    assert_eq!(titles, ["", "kept.rs"], "the first was never stored");
    assert_eq!(
        vault.collector_config().unwrap(),
        everyday_core::screentime::CollectorConfig { idle_minutes: 3, keep_titles: true }
    );
}

#[test]
fn a_browsers_time_is_split_by_its_sites_and_never_counted_twice() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let at = base();
    record(&vault, "gnome", vec![app("firefox", at, 0, 600)]);
    record(
        &vault,
        "firefox",
        vec![site("github.com", at, 120, 240), site("mail.google.com", at, 240, 360)],
    );

    let r = report(&vault);
    assert_eq!(r.total_secs, 600);
    assert_eq!(secs_of(&r, "site:github.com"), 120);
    assert_eq!(secs_of(&r, "site:google.com"), 120, "a host counts towards its domain");
    assert_eq!(secs_of(&r, "app:firefox"), 360);
    let rest = r.items.iter().find(|i| i.key == "app:firefox").unwrap();
    assert!(rest.browser_rest);
}

#[test]
fn a_report_checks_its_range_and_zone() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let day = Date::constant(2026, 11, 5);
    let query = |from: Date, to: Date, tz: &str| ScreenQuery {
        from,
        to,
        tz: tz.into(),
        devices: Vec::new(),
        category: None,
        item: None,
    };
    invalid(vault.screen_report(&query(day, day.yesterday().unwrap(), "UTC")));
    invalid(vault.screen_report(&query(Date::constant(2025, 1, 1), day, "UTC")));
    invalid(vault.screen_report(&query(day, day, "Nowhere/Special")));
    let empty = vault.screen_report(&query(day, day, "Europe/London")).unwrap();
    assert_eq!(empty.total_secs, 0);
    assert_eq!(empty.days.len(), 1);
    assert_eq!(empty.hours.len(), 24);
}

// ---- labels ------------------------------------------------------------------

#[test]
fn a_label_must_name_a_real_app_or_site_and_a_real_category() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.ensure_screen_defaults().unwrap();

    invalid(vault.save_screen_app(&ScreenApp::new("firefox", SpanKind::App)));
    invalid(vault.save_screen_app(&ScreenApp::new("site:github.com", SpanKind::App)));
    let mut host = ScreenApp::new("site:mail.google.com", SpanKind::Site);
    host.split = true;
    invalid(vault.save_screen_app(&host));
    let mut lost = ScreenApp::new("app:code", SpanKind::App);
    lost.category = Some(ScreenCategoryId::new());
    invalid(vault.save_screen_app(&lost));
    let mut wordy = ScreenApp::new("app:code", SpanKind::App);
    wordy.name = "x".repeat(201);
    invalid(vault.save_screen_app(&wordy));

    let mut domain = ScreenApp::new("site:Google.com", SpanKind::Site);
    domain.split = true;
    vault.save_screen_app(&domain).unwrap();
    assert!(vault.screen_app_for("site:google.com").unwrap().split);
}

#[test]
fn there_is_one_label_per_key_and_a_second_save_edits_it() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.ensure_screen_defaults().unwrap();
    let work = category(&vault, "work");

    let mut first = ScreenApp::new("app:Code", SpanKind::App);
    first.name = "  Editor ".into();
    first.uncategorized = true;
    first.category = Some(work.id);
    vault.save_screen_app(&first).unwrap();
    let stored = vault.screen_app_for("app:code").unwrap();
    assert_eq!(stored.id, first.id);
    assert_eq!(stored.key, "app:code");
    assert_eq!(stored.name, "Editor");
    assert!(!stored.uncategorized, "a category chosen beside it wins");

    let mut second = ScreenApp::new("app:code", SpanKind::App);
    second.name = "VS Code".into();
    vault.save_screen_app(&second).unwrap();
    let stored = vault.screen_app_for("app:CODE").unwrap();
    assert_eq!(stored.id, first.id, "the label already there was edited");
    assert_eq!(stored.name, "VS Code");
    let labelled: Vec<ScreenAppId> = vault
        .screen_apps()
        .unwrap()
        .into_iter()
        .filter_map(|row| row.label.map(|l| l.id))
        .collect();
    assert_eq!(labelled, [first.id]);

    // A key with no label answers a fresh one, unsaved.
    let fresh = vault.screen_app_for("site:example.com").unwrap();
    assert_eq!(fresh.kind, SpanKind::Site);
    assert_eq!(vault.screen_apps().unwrap().len(), 1);

    vault.delete_screen_app(first.id).unwrap();
    assert!(vault.screen_apps().unwrap().is_empty());
}

#[test]
fn labels_rename_categorise_split_and_ignore_in_reports() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.ensure_screen_defaults().unwrap();
    let at = base();
    record(&vault, "gnome", vec![app("code", at, 0, 600), app("gnome-shell", at, 600, 700)]);
    record(
        &vault,
        "firefox",
        vec![site("mail.google.com", at, 1000, 1100), site("docs.google.com", at, 1100, 1300)],
    );

    let mut code = vault.screen_app_for("app:code").unwrap();
    code.name = "Editor".into();
    code.category = Some(category(&vault, "learning").id);
    vault.save_screen_app(&code).unwrap();
    let mut shell = vault.screen_app_for("app:gnome-shell").unwrap();
    shell.ignored = true;
    vault.save_screen_app(&shell).unwrap();
    let mut google = vault.screen_app_for("site:google.com").unwrap();
    google.split = true;
    vault.save_screen_app(&google).unwrap();

    let r = report(&vault);
    let code_item = r.items.iter().find(|i| i.key == "app:code").unwrap();
    assert_eq!(code_item.name, "Editor");
    assert_eq!(code_item.category, Some(category(&vault, "learning").id));
    assert!(!code_item.suggested);
    assert_eq!(secs_of(&r, "app:gnome-shell"), 0, "ignored");
    assert_eq!(secs_of(&r, "site:google.com"), 0, "split into its hosts");
    assert_eq!(secs_of(&r, "site:docs.google.com"), 200);
    assert_eq!(secs_of(&r, "site:mail.google.com"), 100);
    assert_eq!(r.total_secs, 900);

    // And the list labels are edited in still shows the ignored one.
    let rows = vault.screen_apps().unwrap();
    assert!(rows.iter().any(|row| row.key == "app:gnome-shell"));
}

// ---- categories ----------------------------------------------------------------

#[test]
fn starter_categories_are_written_once() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    assert!(vault.screen_categories().unwrap().is_empty());
    vault.ensure_screen_defaults().unwrap();
    vault.ensure_screen_defaults().unwrap();
    let all = vault.screen_categories().unwrap();
    assert_eq!(all.len(), 6);
    assert_eq!(all[0].builtin, "work");
    assert!(all.windows(2).all(|w| w[0].sort_order <= w[1].sort_order));
}

#[test]
fn a_category_needs_a_unique_name_and_a_hex_colour() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.ensure_screen_defaults().unwrap();

    invalid(vault.save_screen_category(&ScreenCategory::new("  WORK ")));
    invalid(vault.save_screen_category(&ScreenCategory::new("   ")));
    let mut bad_colour = ScreenCategory::new("Reading");
    bad_colour.color = "red; background: url(x)".into();
    invalid(vault.save_screen_category(&bad_colour));

    let mut reading = ScreenCategory::new(" Reading ");
    reading.color = "#aabbcc".into();
    reading.builtin = "social".into();
    vault.save_screen_category(&reading).unwrap();
    let stored = vault.screen_categories().unwrap().into_iter().find(|c| c.id == reading.id);
    let stored = stored.unwrap();
    assert_eq!(stored.name, "Reading");
    assert_eq!(stored.builtin, "", "builtin is the vault's to set");

    let mut work = category(&vault, "work");
    work.name = "Deep work".into();
    work.builtin = String::new();
    vault.save_screen_category(&work).unwrap();
    let renamed = category(&vault, "work");
    assert_eq!(renamed.name, "Deep work", "a rename keeps the starter it began as");
}

#[test]
fn deleting_a_category_takes_it_off_every_label() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.ensure_screen_defaults().unwrap();
    let social = category(&vault, "social");
    let mut label = ScreenApp::new("site:reddit.com", SpanKind::Site);
    label.category = Some(social.id);
    vault.save_screen_app(&label).unwrap();

    vault.delete_screen_category(social.id).unwrap();
    assert_eq!(vault.screen_categories().unwrap().len(), 5);
    let label = vault.screen_app_for("site:reddit.com").unwrap();
    assert_eq!(label.category, None);
}

// ---- settings ---------------------------------------------------------------------

#[test]
fn settings_are_checked_and_browsers_tidied() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ok = ScreenTimeSettings::default();
    invalid(vault.save_screen_settings(&ScreenTimeSettings { idle_minutes: 0, ..ok.clone() }));
    invalid(vault.save_screen_settings(&ScreenTimeSettings { idle_minutes: 61, ..ok.clone() }));
    invalid(vault.save_screen_settings(&ScreenTimeSettings { title_days: 3651, ..ok.clone() }));
    let many: Vec<String> = (0..51).map(|i| format!("browser-{i}")).collect();
    invalid(vault.save_screen_settings(&ScreenTimeSettings { browsers: many, ..ok.clone() }));
    invalid(vault.save_screen_settings(&ScreenTimeSettings {
        browsers: vec!["b".repeat(201)],
        ..ok.clone()
    }));

    vault
        .save_screen_settings(&ScreenTimeSettings {
            idle_minutes: 5,
            title_days: 90,
            browsers: vec![" Nyxt ".into(), "nyxt".into(), "".into(), "Ladybird".into()],
            ..ok
        })
        .unwrap();
    let stored = vault.screen_settings().unwrap();
    assert_eq!(stored.idle_minutes, 5);
    assert_eq!(stored.title_days, 90);
    assert_eq!(stored.browsers, ["Nyxt", "Ladybird"]);
}

// ---- forgetting -------------------------------------------------------------------

#[test]
fn forgetting_an_item_deletes_its_spans_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let at = base();
    record(
        &vault,
        "firefox",
        vec![
            site("github.com", at, 0, 60),
            site("www.youtube.com", at, 100, 200),
            site("gist.github.com", at, 300, 360),
        ],
    );
    assert_eq!(vault.forget_screen_item("site:github.com").unwrap(), 2, "every host of it");
    let r = report(&vault);
    assert_eq!(secs_of(&r, "site:github.com"), 0);
    assert_eq!(secs_of(&r, "site:youtube.com"), 100);
    assert_eq!(vault.forget_screen_item("app:nothing").unwrap(), 0);
    invalid(vault.forget_screen_item("github.com"));
}

#[test]
fn a_range_of_screen_time_can_be_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let at = base();
    record(&vault, "gnome", vec![app("code", at, 0, 60), app("slack", at, 1000, 1060)]);
    assert_eq!(vault.delete_screen_time(plus(at, 30), plus(at, 40)).unwrap(), 1);
    let r = report(&vault);
    assert_eq!(secs_of(&r, "app:code"), 0, "an overlapping span goes whole");
    assert_eq!(secs_of(&r, "app:slack"), 60);
    invalid(vault.delete_screen_time(plus(at, 40), plus(at, 30)));
}

#[test]
fn old_titles_are_cleared_and_the_time_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let old = Timestamp::now() - SignedDuration::from_hours(24 * 3);
    let titled = |h: Heartbeat| Heartbeat { title: "Re: the private thing".into(), ..h };
    record(
        &vault,
        "gnome",
        vec![titled(app("code", old, 0, 60)), titled(app("slack", base(), 0, 60))],
    );

    assert_eq!(vault.prune_screen_time(Timestamp::now()).unwrap(), 0, "kept by default");
    vault
        .save_screen_settings(&ScreenTimeSettings { title_days: 1, ..Default::default() })
        .unwrap();
    assert_eq!(vault.prune_screen_time(Timestamp::now()).unwrap(), 1);
    assert_eq!(vault.prune_screen_time(Timestamp::now()).unwrap(), 0, "each span once");

    // The time stays, so reports and the goals drawn from them keep their
    // history; only the old title went.
    let r = report(&vault);
    assert_eq!(secs_of(&r, "app:code"), 60);
    assert_eq!(secs_of(&r, "app:slack"), 60);
    let old_day = old.to_zoned(jiff::tz::TimeZone::system()).date();
    let timeline = vault.screen_timeline(old_day, "", &[]).unwrap();
    assert!(timeline.runs.iter().all(|r| r.title.is_empty()), "{timeline:?}");

    // The sweep's bookmark survives a settings save that never saw it.
    vault
        .save_screen_settings(&ScreenTimeSettings { title_days: 1, ..Default::default() })
        .unwrap();
    assert!(vault.screen_settings().unwrap().titles_cleared_to.is_some());
}

// ---- trackers ---------------------------------------------------------------------

fn tracker_minutes(vault: &Vault, tracker: &Tracker) -> f64 {
    let today = jiff::Zoned::now().date();
    let query = ReadingQuery {
        tracker_ids: vec![tracker.id],
        ..ReadingQuery::between(today.yesterday().unwrap(), today)
    };
    vault.tracker_days(&query).unwrap().iter().map(|d| d.sum).sum()
}

fn screen_tracker(vault: &Vault, name: &str, source: TrackerSource) -> Tracker {
    let mut tracker = Tracker::new(name, TrackerKind::Amount);
    tracker.source = source;
    vault.save_tracker(&tracker).unwrap();
    vault.tracker(tracker.id).unwrap()
}

#[test]
fn screen_time_is_a_trackers_source() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.ensure_screen_defaults().unwrap();
    let at = base();
    record(
        &vault,
        "firefox",
        vec![site("reddit.com", at, 0, 600), site("github.com", at, 600, 900)],
    );
    let social = category(&vault, "social");
    let reddit = vault.screen_app_for("site:reddit.com").unwrap();
    vault.save_screen_app(&reddit).unwrap();

    let all =
        screen_tracker(&vault, "Screen", TrackerSource::ScreenTime { category: None, item: None });
    assert_eq!(all.unit, "min");
    assert!((tracker_minutes(&vault, &all) - 15.0).abs() < 0.2);

    let by_category = screen_tracker(
        &vault,
        "Social",
        TrackerSource::ScreenTime { category: Some(social.id), item: None },
    );
    assert!((tracker_minutes(&vault, &by_category) - 10.0).abs() < 0.2);
    // A category a goal measures cannot quietly vanish under it.
    let refused = vault.delete_screen_category(social.id).unwrap_err().to_string();
    assert!(refused.contains("Social"), "{refused}");

    let by_app = screen_tracker(
        &vault,
        "Reddit",
        TrackerSource::ScreenTime { category: None, item: Some("site:reddit.com".into()) },
    );
    assert!((tracker_minutes(&vault, &by_app) - 10.0).abs() < 0.2);

    // Deleting the label does not cost the goal what it counts: it is
    // pointed at by key.
    vault.delete_screen_app(reddit.id).unwrap();
    assert!((tracker_minutes(&vault, &by_app) - 10.0).abs() < 0.2);

    let unknown = screen_tracker(
        &vault,
        "Nothing",
        TrackerSource::ScreenTime { category: None, item: Some("app:never-seen".into()) },
    );
    assert_eq!(tracker_minutes(&vault, &unknown), 0.0, "an unseen item counts nothing");

    // Readings are made up, and cannot be written by hand.
    let reading = everyday_core::Reading::on(all.id, jiff::Zoned::now().date(), 5.0);
    assert!(vault.save_reading(&reading).is_err());
}

#[test]
fn minutes_by_day_answers_nothing_for_an_empty_vault() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let minutes = vault
        .screen_minutes_by_day(Date::constant(1900, 1, 1), Date::constant(2200, 12, 31), None, None)
        .unwrap();
    assert!(minutes.is_empty());
}
