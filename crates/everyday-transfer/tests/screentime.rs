//! Screen time, out of a vault and back into another.
//!
//! Its own file for the Feed's reason: what comes back is checked against
//! what was meant to, and two things here are meant to be more than "the
//! same ids" -- a starter category or a labelled key already in the vault is
//! the same thing under another id, and a span already here is left as it
//! is however many times one archive is read.
//!
//! The spans are written straight into the store rather than sent as
//! heartbeats, so these tests are about the part and not about how
//! heartbeats are joined.

use everyday_core::screentime::{
    ScreenApp, ScreenCategory, ScreenSpan, ScreenTimeSettings, SpanKind,
};
use everyday_core::store::screentime::ScreenTimeStore;
use everyday_core::{ScreenSpanId, Vault, VaultConfig};
use everyday_transfer::{Mode, Options, zip};
use jiff::Timestamp;
use std::collections::BTreeMap;

/// The stream an import writes under. The part modules are private, so it is
/// restated here, and the first test fails if the two drift.
const IMPORT_STREAM: &str = "import";

const FORMULA: &str = "=HYPERLINK(\"https://evil.example\",\"click\")";

fn vault(dir: &std::path::Path) -> Vault {
    everyday_vault::create(
        dir,
        VaultConfig {
            name: "Test".into(),
            backend: "sqlite".into(),
            settings: Default::default(),
            password: None,
            kdf: everyday_core::crypto::KdfParams::insecure_fast(),
            auto_lock_seconds: 0,
            forget_key_seconds: 0,
        },
    )
    .unwrap()
}

fn only_screen_time() -> Options {
    Options { parts: vec!["screentime".into()], media: false }
}

fn screen_time() -> Vec<String> {
    vec!["screentime".to_string()]
}

fn text<'a>(archive: &'a zip::Reader, name: &str) -> &'a str {
    std::str::from_utf8(archive.get(name).unwrap_or_else(|| panic!("no {name} in the archive")))
        .unwrap()
}

/// Run `f` against the vault's screen-time store.
fn with<T>(vault: &Vault, f: impl FnOnce(&dyn ScreenTimeStore) -> T) -> T {
    vault.with_store(|store| Ok(f(store.screentime().expect("a screen-time store")))).unwrap()
}

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_second(1_800_000_000 + seconds).unwrap()
}

fn span(collector: &str, kind: SpanKind, app: &str, host: &str, from: i64, to: i64) -> ScreenSpan {
    ScreenSpan {
        id: ScreenSpanId::new(),
        device: "laptop".into(),
        collector: collector.into(),
        kind,
        app: app.into(),
        name: String::new(),
        host: host.into(),
        title: String::new(),
        start: at(from),
        end: at(to),
    }
}

fn every_span(vault: &Vault) -> Vec<ScreenSpan> {
    with(vault, |s| s.spans(Timestamp::MIN, Timestamp::MAX).unwrap())
}

fn by_id<T, K: Ord>(mut list: Vec<T>, key: impl Fn(&T) -> K) -> Vec<T> {
    list.sort_by_key(key);
    list
}

struct Filled {
    work: ScreenCategory,
    github: ScreenApp,
}

/// Two categories (a starter and one of the person's own), two labels, a
/// changed setting, and three spans across two streams -- one whose title
/// is a spreadsheet formula.
fn fill(vault: &Vault) -> Filled {
    let mut work = ScreenCategory::new("Work");
    work.builtin = "work".into();
    work.color = "#2563eb".into();
    let mut reading = ScreenCategory::new("Reading");
    reading.sort_order = 1;

    let mut github = ScreenApp::new("site:github.com", SpanKind::Site);
    github.category = Some(work.id);
    github.name = "GitHub".into();
    let mut terminal = ScreenApp::new("app:org.gnome.ptyxis", SpanKind::App);
    terminal.name = "Terminal".into();
    terminal.category = Some(work.id);

    let mut editing = span("gnome", SpanKind::App, "org.gnome.Ptyxis", "", 0, 600);
    editing.name = "Terminal".into();
    editing.title = FORMULA.into();
    let mut mail = span("firefox", SpanKind::Site, "Firefox", "mail.google.com", 600, 900);
    mail.title = "Inbox (3) - hari@example.com".into();
    let code = span("firefox", SpanKind::Site, "Firefox", "github.com", 900, 1_500);

    with(vault, |s| {
        s.put_category(&work).unwrap();
        s.put_category(&reading).unwrap();
        s.put_app(&github).unwrap();
        s.put_app(&terminal).unwrap();
        s.put_screen_settings(&ScreenTimeSettings {
            idle_minutes: 5,
            browsers: vec!["librewolf".into()],
            ..ScreenTimeSettings::default()
        })
        .unwrap();
        s.put_spans("desktop-stream", &[editing]).unwrap();
        s.put_spans("browser-stream", &[mail, code]).unwrap();
    });
    Filled { work, github }
}

#[test]
fn screen_time_comes_back_whole_and_a_second_import_changes_nothing() {
    let source_dir = tempfile::tempdir().unwrap();
    let source = vault(source_dir.path());
    fill(&source);

    let (bytes, manifest) =
        everyday_transfer::export(&source, &only_screen_time(), Vec::new()).unwrap();
    assert_eq!(manifest.parts.len(), 1);
    assert_eq!(manifest.parts[0].id, "screentime");
    let archive = zip::Reader::open(&bytes).unwrap();
    for name in [
        "screentime/spans.csv",
        "screentime/labels.json",
        "screentime/categories.json",
        "screentime/settings.json",
    ] {
        assert!(archive.contains(name), "{name} is missing");
    }

    // A table anybody can open: a header, a row a span, the host as the
    // browser saw it, and a title that is a formula opening as text.
    let csv = text(&archive, "screentime/spans.csv");
    assert!(csv.starts_with("id,start,end,device,collector,kind,app,name,host,title\r\n"), "{csv}");
    assert_eq!(csv.lines().count(), 4, "{csv}");
    assert!(csv.contains(",site,Firefox,,mail.google.com,"), "{csv}");
    assert!(csv.contains(",2027-01-15T08:00:00Z,2027-01-15T08:10:00Z,"), "UTC instants: {csv}");
    assert!(csv.contains("\"'=HYPERLINK("), "the formula is written as text: {csv}");
    assert!(!csv.contains(",=HYPERLINK"), "{csv}");

    // Into a vault that has never heard of any of it.
    let target_dir = tempfile::tempdir().unwrap();
    let target = vault(target_dir.path());
    let reports = everyday_transfer::import(&target, &archive, &screen_time(), Mode::Skip).unwrap();
    assert_eq!(reports.len(), 1);
    let report = &reports[0];
    assert!(report.problems.is_empty(), "{report:?}");
    // Settings, two categories, two labels, three spans.
    assert_eq!((report.added, report.skipped, report.replaced), (8, 0, 0), "{report:?}");

    assert_eq!(every_span(&target), every_span(&source), "every span, every field, by id");
    assert_eq!(
        every_span(&target).iter().find(|s| s.kind == SpanKind::App).unwrap().title,
        FORMULA,
        "the apostrophe came off on the way back in"
    );
    let apps = |v: &Vault| by_id(with(v, |s| s.list_apps().unwrap()), |a| a.id);
    let categories = |v: &Vault| with(v, |s| s.list_categories().unwrap());
    assert_eq!(apps(&target), apps(&source));
    assert_eq!(categories(&target), categories(&source));
    assert_eq!(with(&target, |s| s.screen_settings().unwrap()).idle_minutes, 5);
    assert_eq!(
        with(&target, |s| s.screen_settings().unwrap()),
        with(&source, |s| s.screen_settings().unwrap())
    );
    // Written under the import's own stream, which no collector extends.
    let last = with(&target, |s| s.last_span(IMPORT_STREAM).unwrap());
    assert_eq!(last.map(|s| s.end), Some(at(1_500)));
    assert!(with(&target, |s| s.last_span("browser-stream").unwrap()).is_none());

    // A second import adds nothing and changes nothing.
    let before = every_span(&target);
    let again = everyday_transfer::import(&target, &archive, &screen_time(), Mode::Skip).unwrap();
    assert_eq!((again[0].added, again[0].skipped, again[0].replaced), (0, 8, 0), "{again:?}");
    assert_eq!(every_span(&target), before);

    // Nor does a third under Replace: a span the same as the file's is left
    // exactly as it is.
    let replaced =
        everyday_transfer::import(&target, &archive, &screen_time(), Mode::Replace).unwrap();
    assert_eq!(replaced[0].added, 0, "{replaced:?}");
    assert_eq!(every_span(&target), before);
    assert_eq!(apps(&target).len(), 2);
    assert_eq!(categories(&target).len(), 2);
}

#[test]
fn a_starter_or_a_key_already_here_is_the_same_record_under_its_own_id() {
    let source_dir = tempfile::tempdir().unwrap();
    let source = vault(source_dir.path());
    let filled = fill(&source);
    let (bytes, _) = everyday_transfer::export(&source, &only_screen_time(), Vec::new()).unwrap();
    let archive = zip::Reader::open(&bytes).unwrap();

    // This vault made its own Work, and labelled GitHub, before the archive
    // arrived.
    let target_dir = tempfile::tempdir().unwrap();
    let target = vault(target_dir.path());
    let mut mine = ScreenCategory::new("Work");
    mine.builtin = "work".into();
    let mut gh = ScreenApp::new("site:github.com", SpanKind::Site);
    gh.name = "GH".into();
    with(&target, |s| {
        s.put_category(&mine).unwrap();
        s.put_app(&gh).unwrap();
    });

    let reports = everyday_transfer::import(&target, &archive, &screen_time(), Mode::Skip).unwrap();
    assert!(reports[0].problems.is_empty(), "{reports:?}");
    let categories = with(&target, |s| s.list_categories().unwrap());
    assert_eq!(categories.len(), 2, "no second Work: {categories:?}");
    assert!(categories.iter().any(|c| c.id == mine.id));
    assert!(!categories.iter().any(|c| c.id == filled.work.id));
    let github = with(&target, |s| s.get_app(gh.id).unwrap());
    assert_eq!(github.name, "GH", "skip leaves the label here alone");
    assert!(with(&target, |s| s.get_app(filled.github.id)).is_err(), "and adds no second one");
    // The terminal's label came in, filed under this vault's Work.
    let terminal = with(&target, |s| s.list_apps().unwrap())
        .into_iter()
        .find(|a| a.key == "app:org.gnome.ptyxis")
        .unwrap();
    assert_eq!(terminal.category, Some(mine.id));

    // Replace puts the file's word on the label here, under the id it has.
    everyday_transfer::import(&target, &archive, &screen_time(), Mode::Replace).unwrap();
    let github = with(&target, |s| s.get_app(gh.id).unwrap());
    assert_eq!(github.name, "GitHub");
    assert_eq!(github.category, Some(mine.id), "filed under this vault's Work");
    assert_eq!(with(&target, |s| s.list_apps().unwrap()).len(), 2);
}

#[test]
fn titles_stay_behind_when_this_vault_does_not_keep_them() {
    let source_dir = tempfile::tempdir().unwrap();
    let source = vault(source_dir.path());
    fill(&source);
    let (bytes, _) = everyday_transfer::export(&source, &only_screen_time(), Vec::new()).unwrap();
    let archive = zip::Reader::open(&bytes).unwrap();

    let target_dir = tempfile::tempdir().unwrap();
    let target = vault(target_dir.path());
    let private = ScreenTimeSettings { keep_titles: false, ..ScreenTimeSettings::default() };
    with(&target, |s| s.put_screen_settings(&private).unwrap());

    let reports = everyday_transfer::import(&target, &archive, &screen_time(), Mode::Skip).unwrap();
    assert!(reports[0].problems.is_empty(), "{reports:?}");
    assert_eq!(with(&target, |s| s.screen_settings().unwrap()), private, "the setting stood");
    let spans = every_span(&target);
    assert_eq!(spans.len(), 3);
    assert!(spans.iter().all(|s| s.title.is_empty()), "{spans:?}");
}

#[test]
fn a_hand_made_table_lands_what_it_can_and_names_the_rest() {
    let kept = ScreenSpanId::new();
    let csv = format!(
        "id,start,end,device,collector,kind,app,name,host,title\r\n\
         {kept},2026-11-05T09:00:00Z,2026-11-05T09:30:00Z,desk,gnome,app,ptyxis,,,'=SUM(1)\r\n\
         ,2026-11-05T10:00:00Z,2026-11-05T10:05:00Z,desk,ff,SITE,ff,,HTTPS://Docs.Example.com:1/x,\r\n\
         not-an-id,2026-11-05T10:00:00Z,2026-11-05T10:05:00Z,desk,gnome,app,x,,,\r\n\
         ,yesterday,2026-11-05T10:05:00Z,desk,gnome,app,x,,,\r\n\
         ,2026-11-05T10:05:00Z,2026-11-05T10:00:00Z,desk,gnome,app,x,,,\r\n\
         ,2026-11-05T10:00:00Z,2026-11-05T10:05:00Z,desk,firefox,site,Firefox,,,\r\n\
         ,2026-11-05T10:00:00Z,2026-11-05T10:05:00Z,desk,gnome,window,x,,,\r\n\
         ,2026-11-04T00:00:00Z,2026-11-05T00:00:00Z,desk,gnome,app,x,,,\r\n\
         ,2026-11-05T10:00:00Z,2026-11-05T10:05:00Z,,gnome,app,x,,,\r\n\
         {kept},2026-11-05T09:00:00Z,2026-11-05T09:30:00Z,desk,gnome,app,ptyxis,,,\r\n"
    );
    let mut files = BTreeMap::new();
    files.insert("screentime/spans.csv".to_string(), csv.into_bytes());
    let archive = zip::Reader::from_files(files);

    let dir = tempfile::tempdir().unwrap();
    let target = vault(dir.path());
    let reports = everyday_transfer::import(&target, &archive, &screen_time(), Mode::Skip).unwrap();
    let report = &reports[0];
    assert_eq!(report.added, 2, "{report:?}");
    // A bad id, a bad time, backwards, a site with no host, an unknown kind,
    // a whole day long, no device, and the first row's id again.
    assert_eq!(report.problems.len(), 8, "{:#?}", report.problems);
    assert!(report.problems.iter().any(|p| p.contains("row 4")), "{:?}", report.problems);
    assert!(report.problems.iter().any(|p| p.contains("already in the file")));

    let spans = every_span(&target);
    assert_eq!(spans.len(), 2);
    let first = spans.iter().find(|s| s.id == kept).unwrap();
    assert_eq!(first.title, "=SUM(1)", "the apostrophe came off");
    assert_eq!(first.kind, SpanKind::App);
    let second = spans.iter().find(|s| s.id != kept).unwrap();
    assert_eq!(second.kind, SpanKind::Site);
    assert_eq!(second.host, "docs.example.com", "normalised as a collector's would be");
}

#[test]
fn a_vault_that_never_used_screen_time_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    // The starter categories alone are not something anybody made.
    let mut work = ScreenCategory::new("Work");
    work.builtin = "work".into();
    with(&vault, |s| s.put_category(&work).unwrap());

    let (bytes, manifest) =
        everyday_transfer::export(&vault, &only_screen_time(), Vec::new()).unwrap();
    assert!(manifest.parts.is_empty(), "{:?}", manifest.parts);
    let archive = zip::Reader::open(&bytes).unwrap();
    assert!(archive.names().all(|n| !n.starts_with("screentime/")));
}
