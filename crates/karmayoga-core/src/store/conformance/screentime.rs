//! The screen-time half of the conformance suite: spans and the streams they
//! belong to, labels, categories and the settings.
//!
//! One check per rule in [`crate::store::screentime`]'s module doc, at least,
//! and a round trip of every field of every record. Two of those rules
//! cannot be checked from here, for the reason Money's suite gives: that
//! nothing a person would recognise sits in a clear column, and that a
//! payload copied into another row will not open there. The trait offers no
//! way to read or write a raw row, and inventing one for a test would be a
//! door the running app does not need. Both are checked where the rows are:
//! the SQL backend's own tests tie each clear column to the schema, and the
//! SQLite driver's search the database file for every device, app, host and
//! title it wrote, and move a sealed span into another row to watch it fail.
//!
//! One more is only checked as far as the trait lets it be: that a batch of
//! spans is all or nothing. Nothing a caller can hand `put_spans` makes a
//! conforming store fail half way through, so what is checked is that a
//! large batch lands whole in one call.

use super::*;
use crate::id::{ScreenAppId, ScreenCategoryId, ScreenSpanId};
use crate::screentime::{ScreenApp, ScreenCategory, ScreenSpan, ScreenTimeSettings, SpanKind};
use crate::store::screentime::ScreenTimeStore;
use jiff::Timestamp;

/// The screen-time half of the suite. Called by [`run_all`] when the backend
/// has a [`ScreenTimeStore`]; public so a backend under construction can run
/// it alone.
///
/// The store must hold no screen-time records on entry; it is left with none
/// on success, and with the settings at their defaults.
pub fn run_screen_time_suite(store: &dyn JournalStore) {
    eprintln!("--- screen time conformance suite ---");
    let screen = store.screentime().expect("run_screen_time_suite needs a screen-time store");

    screen_time_starts_empty(screen);
    missing_screen_records_are_not_found(screen);
    a_span_round_trips_every_field(screen);
    putting_a_span_again_replaces_it_by_id(screen);
    spans_are_every_one_overlapping_the_range_and_nothing_touching_it(screen);
    spans_come_back_in_start_order_then_id(screen);
    the_last_span_is_the_streams_own_with_the_greatest_end(screen);
    a_large_batch_lands_whole_in_one_call(screen);
    deleting_spans_by_id_answers_how_many_went(screen);
    deleting_spans_before_a_time_keeps_one_ending_exactly_then(screen);
    stripping_titles_touches_only_the_window_and_only_titles(screen);
    a_span_longer_than_the_cap_is_refused_and_the_longest_allowed_is_found(screen);
    the_earliest_span_is_the_one_that_began_first(screen);
    a_label_round_trips_every_field(screen);
    categories_round_trip_and_come_back_in_their_order(screen);
    deleting_a_category_touches_nothing_else(screen);
    settings_default_until_saved_and_round_trip(screen);
    unicode_survives_a_screen_time_round_trip(screen);

    screen_time_cleanup(screen);
    eprintln!("--- screen time suite passed ---");
}

/// Every time in this suite is whole seconds from this one, so "an hour ago"
/// means the same thing on every run, and a time that comes back from a
/// clear column (whole microseconds) compares equal to the one that went in.
const NOW: i64 = 1_800_000_000;

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).expect("a representable time")
}

/// `NOW + seconds`.
fn t(seconds: i64) -> Timestamp {
    at(NOW + seconds)
}

/// Two streams, as the vault would name them: keyed hashes, opaque here.
const LAPTOP_GNOME: &str = "3f9a0c1e5b7d2468ace013579bdf2468";
const LAPTOP_FIREFOX: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90";

/// Everything ever written, and more: a range no span in this suite reaches
/// past.
fn every_span(screen: &dyn ScreenTimeStore) -> Vec<ScreenSpan> {
    screen.spans(at(0), at(4_000_000_000)).expect("spans")
}

/// An app span on the laptop, `from..to` seconds after [`NOW`].
fn app(name: &str, from: i64, to: i64) -> ScreenSpan {
    ScreenSpan {
        id: ScreenSpanId::new(),
        device: "laptop".into(),
        collector: "gnome".into(),
        kind: SpanKind::App,
        app: format!("org.example.{name}"),
        name: name.into(),
        host: String::new(),
        title: String::new(),
        start: t(from),
        end: t(to),
    }
}

/// A site span on the laptop's Firefox, `from..to` seconds after [`NOW`].
fn site(host: &str, from: i64, to: i64) -> ScreenSpan {
    ScreenSpan {
        id: ScreenSpanId::new(),
        device: "laptop".into(),
        collector: "firefox".into(),
        kind: SpanKind::Site,
        app: "Firefox".into(),
        name: String::new(),
        host: host.into(),
        title: format!("{host} - the front page"),
        start: t(from),
        end: t(to),
    }
}

fn put(screen: &dyn ScreenTimeStore, stream: &str, spans: &[&ScreenSpan]) {
    let owned: Vec<ScreenSpan> = spans.iter().map(|s| (*s).clone()).collect();
    screen.put_spans(stream, &owned).expect("put_spans");
}

fn ids_of(spans: &[ScreenSpan]) -> Vec<ScreenSpanId> {
    spans.iter().map(|s| s.id).collect()
}

fn sorted<T: Ord>(mut ids: Vec<T>) -> Vec<T> {
    ids.sort();
    ids
}

/// Every span gone, by the widest sweep there is.
fn clear_spans(screen: &dyn ScreenTimeStore) {
    screen.delete_spans_before(at(4_000_000_000)).expect("delete_spans_before");
    assert!(every_span(screen).is_empty(), "a sweep past every end left spans behind");
}

fn label(key: &str, kind: SpanKind, created: Timestamp) -> ScreenApp {
    let mut app = ScreenApp::new(key, kind);
    app.created_at = created;
    app.updated_at = created;
    app
}

fn category(name: &str, order: i64, created: Timestamp) -> ScreenCategory {
    let mut category = ScreenCategory::new(name);
    category.sort_order = order;
    category.created_at = created;
    category.updated_at = created;
    category
}

fn screen_time_starts_empty(screen: &dyn ScreenTimeStore) {
    assert!(every_span(screen).is_empty(), "a fresh store must hold no spans");
    assert_eq!(screen.earliest_span().unwrap(), None, "and so has no earliest one");
    assert!(screen.last_span(LAPTOP_GNOME).unwrap().is_none(), "nor any stream's last");
    assert!(screen.list_apps().unwrap().is_empty(), "a fresh store must hold no labels");
    assert!(screen.list_categories().unwrap().is_empty(), "nor any categories");
    assert_eq!(screen.screen_settings().unwrap(), ScreenTimeSettings::default());
}

fn screen_time_cleanup(screen: &dyn ScreenTimeStore) {
    clear_spans(screen);
    for app in screen.list_apps().expect("list_apps") {
        screen.delete_app(app.id).expect("delete_app");
    }
    for category in screen.list_categories().expect("list_categories") {
        screen.delete_category(category.id).expect("delete_category");
    }
    screen.put_screen_settings(&ScreenTimeSettings::default()).expect("put_screen_settings");
    assert!(screen.list_apps().unwrap().is_empty(), "cleanup left labels behind");
    assert!(screen.list_categories().unwrap().is_empty(), "cleanup left categories behind");
    assert_eq!(screen.earliest_span().unwrap(), None, "cleanup left spans behind");
}

fn missing_screen_records_are_not_found(screen: &dyn ScreenTimeStore) {
    super::assert_not_found(screen.get_app(ScreenAppId::new()));
    super::assert_not_found(screen.get_category(ScreenCategoryId::new()));
    assert!(screen.last_span("a stream nobody wrote").unwrap().is_none());
    assert_eq!(screen.delete_spans(&[ScreenSpanId::new()]).unwrap(), 0, "unknown ids are ignored");
    assert_eq!(screen.delete_spans(&[]).unwrap(), 0);
    assert_eq!(screen.delete_spans_before(t(0)).unwrap(), 0);
    screen.put_spans(LAPTOP_GNOME, &[]).expect("an empty batch is nothing to do");
    assert!(every_span(screen).is_empty());
    // Deleting what is not there is not an error, as it is not for Money.
    screen.delete_app(ScreenAppId::new()).expect("delete_app of nothing");
    screen.delete_category(ScreenCategoryId::new()).expect("delete_category of nothing");
}

fn a_span_round_trips_every_field(screen: &dyn ScreenTimeStore) {
    let mut terminal = app("Terminal", 0, 600);
    terminal.app = "org.gnome.Ptyxis".into();
    terminal.title = "hari@laptop: ~/projects".into();
    let github = site("github.com", 600, 900);
    put(screen, LAPTOP_GNOME, &[&terminal]);
    put(screen, LAPTOP_FIREFOX, &[&github]);

    assert_eq!(every_span(screen), vec![terminal.clone(), github.clone()]);
    assert_eq!(screen.last_span(LAPTOP_GNOME).unwrap(), Some(terminal.clone()));
    assert_eq!(screen.last_span(LAPTOP_FIREFOX).unwrap(), Some(github.clone()));

    // A span with nothing optional set comes back with nothing set.
    let bare = ScreenSpan {
        id: ScreenSpanId::new(),
        device: "desktop".into(),
        collector: "windows".into(),
        kind: SpanKind::App,
        app: "Code.exe".into(),
        name: String::new(),
        host: String::new(),
        title: String::new(),
        start: t(1_000),
        end: t(1_000),
    };
    put(screen, "a-third-stream", &[&bare]);
    assert_eq!(screen.last_span("a-third-stream").unwrap(), Some(bare));

    clear_spans(screen);
}

fn putting_a_span_again_replaces_it_by_id(screen: &dyn ScreenTimeStore) {
    // What the vault does when a heartbeat extends the open span: the same
    // id, a later end, the latest title.
    let mut open = app("Editor", 0, 30);
    put(screen, LAPTOP_GNOME, &[&open]);
    open.end = t(75);
    open.title = "main.rs".into();
    put(screen, LAPTOP_GNOME, &[&open]);

    assert_eq!(every_span(screen), vec![open.clone()], "put must not duplicate");
    assert_eq!(screen.last_span(LAPTOP_GNOME).unwrap(), Some(open.clone()));
    // And its end moved in the index too: it is found by a range that
    // begins after its old end.
    assert_eq!(ids_of(&screen.spans(t(40), t(50)).unwrap()), vec![open.id]);

    // Put again under another stream, it belongs to that stream now.
    put(screen, LAPTOP_FIREFOX, &[&open]);
    assert!(screen.last_span(LAPTOP_GNOME).unwrap().is_none(), "it left the first stream");
    assert_eq!(screen.last_span(LAPTOP_FIREFOX).unwrap(), Some(open));

    clear_spans(screen);
}

fn spans_are_every_one_overlapping_the_range_and_nothing_touching_it(screen: &dyn ScreenTimeStore) {
    // The range is 100..200.
    let ends_at_from = app("EndsAtFrom", 50, 100);
    let starts_at_to = app("StartsAtTo", 200, 250);
    let wholly_before = app("Before", 0, 50);
    let wholly_after = app("After", 250, 300);
    let over_from = app("OverFrom", 90, 110);
    let over_to = app("OverTo", 190, 210);
    let inside = app("Inside", 120, 150);
    let around = app("Around", 0, 300);
    let exactly = app("Exactly", 100, 200);
    let just_in = app("JustIn", 99, 101);
    put(
        screen,
        LAPTOP_GNOME,
        &[
            &ends_at_from,
            &starts_at_to,
            &wholly_before,
            &wholly_after,
            &over_from,
            &over_to,
            &inside,
            &around,
            &exactly,
            &just_in,
        ],
    );
    // A second stream's span is answered too: a range is across streams.
    let other = site("example.org", 150, 160);
    put(screen, LAPTOP_FIREFOX, &[&other]);

    let found = screen.spans(t(100), t(200)).unwrap();
    assert_eq!(
        sorted(ids_of(&found)),
        sorted(vec![
            over_from.id,
            over_to.id,
            inside.id,
            around.id,
            exactly.id,
            just_in.id,
            other.id,
        ]),
        "every span sharing time with 100..200, and none that only touches it"
    );
    assert!(!found.iter().any(|s| s.id == ends_at_from.id), "ending at `from` is not inside");
    assert!(!found.iter().any(|s| s.id == starts_at_to.id), "starting at `to` is not inside");

    // Laid end to end, two ranges count a boundary span once each side only
    // where it actually crosses.
    let left = ids_of(&screen.spans(t(50), t(100)).unwrap());
    assert!(left.contains(&ends_at_from.id) && !left.contains(&exactly.id));

    // A range of no length is an instant: what was running then.
    assert_eq!(
        sorted(ids_of(&screen.spans(t(155), t(155)).unwrap())),
        sorted(vec![around.id, exactly.id, other.id])
    );

    clear_spans(screen);
}

fn spans_come_back_in_start_order_then_id(screen: &dyn ScreenTimeStore) {
    let late = app("Late", 300, 400);
    let early = app("Early", 0, 100);
    let mut tied_a = app("TiedA", 200, 250);
    let mut tied_b = site("tied.example", 200, 260);
    // Ids in a known order, whatever order they were minted in.
    if tied_a.id > tied_b.id {
        std::mem::swap(&mut tied_a.id, &mut tied_b.id);
    }
    // Written out of order, across two streams.
    put(screen, LAPTOP_GNOME, &[&late, &tied_a]);
    put(screen, LAPTOP_FIREFOX, &[&tied_b, &early]);

    assert_eq!(
        ids_of(&every_span(screen)),
        vec![early.id, tied_a.id, tied_b.id, late.id],
        "by start, then by id"
    );
    clear_spans(screen);
}

fn the_last_span_is_the_streams_own_with_the_greatest_end(screen: &dyn ScreenTimeStore) {
    // On the desktop collector's stream: three spans, the latest-ending
    // not the latest-starting.
    let first = app("First", 0, 100);
    let long = app("Long", 50, 500);
    let short = app("Short", 400, 450);
    put(screen, LAPTOP_GNOME, &[&first, &long, &short]);
    // On the browser's: one that ends later than all of them, which must not
    // be the desktop stream's answer.
    let later = site("later.example", 0, 900);
    put(screen, LAPTOP_FIREFOX, &[&later]);

    assert_eq!(screen.last_span(LAPTOP_GNOME).unwrap().map(|s| s.id), Some(long.id));
    assert_eq!(screen.last_span(LAPTOP_FIREFOX).unwrap().map(|s| s.id), Some(later.id));

    // Two ending at the same instant: the greater id.
    let mut tie_a = site("a.example", 900, 1_000);
    let mut tie_b = site("b.example", 950, 1_000);
    if tie_a.id > tie_b.id {
        std::mem::swap(&mut tie_a.id, &mut tie_b.id);
    }
    put(screen, LAPTOP_FIREFOX, &[&tie_b, &tie_a]);
    assert_eq!(
        screen.last_span(LAPTOP_FIREFOX).unwrap().map(|s| s.id),
        Some(tie_b.id),
        "a tie at the end goes to the greatest id"
    );

    clear_spans(screen);
    assert!(screen.last_span(LAPTOP_GNOME).unwrap().is_none());
}

fn a_large_batch_lands_whole_in_one_call(screen: &dyn ScreenTimeStore) {
    // A collector back from a week offline: more than any `IN (...)` chunk,
    // in one batch, every one of them there afterwards.
    let batch: Vec<ScreenSpan> =
        (0..1_203).map(|n| app(&format!("App{n}"), n * 10, n * 10 + 5)).collect();
    screen.put_spans(LAPTOP_GNOME, &batch).expect("put_spans");
    let found = every_span(screen);
    assert_eq!(found.len(), batch.len());
    assert_eq!(ids_of(&found), ids_of(&batch), "every one, in start order");
    assert_eq!(screen.last_span(LAPTOP_GNOME).unwrap().map(|s| s.id), batch.last().map(|s| s.id));

    // And deleted as one call, too.
    assert_eq!(screen.delete_spans(&ids_of(&batch)).unwrap(), batch.len() as u64);
    assert!(every_span(screen).is_empty());
}

fn deleting_spans_by_id_answers_how_many_went(screen: &dyn ScreenTimeStore) {
    let a = app("A", 0, 10);
    let b = app("B", 10, 20);
    let c = site("c.example", 20, 30);
    put(screen, LAPTOP_GNOME, &[&a, &b]);
    put(screen, LAPTOP_FIREFOX, &[&c]);

    let gone = screen.delete_spans(&[a.id, ScreenSpanId::new(), c.id]).unwrap();
    assert_eq!(gone, 2, "two were there; the unknown id is ignored");
    assert_eq!(ids_of(&every_span(screen)), vec![b.id]);
    assert_eq!(screen.delete_spans(&[a.id]).unwrap(), 0, "already gone");
    assert!(screen.last_span(LAPTOP_FIREFOX).unwrap().is_none(), "its stream is empty now");

    clear_spans(screen);
}

fn stripping_titles_touches_only_the_window_and_only_titles(screen: &dyn ScreenTimeStore) {
    let titled = |name: &str, from: i64, to: i64| ScreenSpan {
        title: format!("{name}: a private subject"),
        ..app(name, from, to)
    };
    let before = titled("Before", 0, 39);
    let inside = titled("Inside", 20, 40);
    let bare = app("Bare", 50, 60);
    let after = titled("After", 90, 200);
    put(screen, LAPTOP_GNOME, &[&before, &inside, &bare, &after]);

    // `from` is inclusive and `before` exclusive, by end; a span with no
    // title is not counted as changed.
    assert_eq!(screen.strip_titles(t(40), t(100)).unwrap(), 1);
    let spans = every_span(screen);
    let title_of = |id| spans.iter().find(|s| s.id == id).unwrap().title.clone();
    assert_eq!(title_of(before.id), before.title, "ended before the window");
    assert_eq!(title_of(inside.id), "");
    assert_eq!(title_of(after.id), after.title, "ended after the window");
    let stripped = spans.iter().find(|s| s.id == inside.id).unwrap();
    assert_eq!(*stripped, ScreenSpan { title: String::new(), ..inside.clone() });
    assert_eq!(screen.strip_titles(t(40), t(100)).unwrap(), 0, "and again is nothing");
    assert_eq!(
        screen.last_span(LAPTOP_GNOME).unwrap().map(|s| s.id),
        Some(after.id),
        "the stream is untouched"
    );
    screen.delete_spans_before(t(1_000)).unwrap();
}

fn a_span_longer_than_the_cap_is_refused_and_the_longest_allowed_is_found(
    screen: &dyn ScreenTimeStore,
) {
    use crate::screentime::MAX_SPAN_SECS;
    let too_long = app("TooLong", 0, MAX_SPAN_SECS + 1);
    let fine = app("Fine", 0, 10);
    let err = screen.put_spans(LAPTOP_GNOME, &[fine.clone(), too_long.clone()]).unwrap_err();
    assert!(matches!(err, crate::error::Error::Invalid(_)), "{err:?}");
    assert!(every_span(screen).is_empty(), "the batch is refused whole");
    let backwards = app("Backwards", 10, 0);
    assert!(screen.put_spans(LAPTOP_GNOME, &[backwards]).is_err());

    // The longest a span may be, read from a window that starts a second
    // before it ends: the look-back reaches exactly far enough.
    let longest = app("Longest", 0, MAX_SPAN_SECS);
    put(screen, LAPTOP_GNOME, &[&longest]);
    let found = screen.spans(t(MAX_SPAN_SECS - 1), t(MAX_SPAN_SECS + 100)).unwrap();
    assert_eq!(ids_of(&found), vec![longest.id]);
    screen.delete_spans_before(t(MAX_SPAN_SECS * 4)).unwrap();
}

fn deleting_spans_before_a_time_keeps_one_ending_exactly_then(screen: &dyn ScreenTimeStore) {
    let old = app("Old", 0, 50);
    let ends_at_cut = app("EndsAtCut", 50, 100);
    let running = app("Running", 90, 200);
    let new = site("new.example", 150, 200);
    put(screen, LAPTOP_GNOME, &[&old, &ends_at_cut, &running]);
    put(screen, LAPTOP_FIREFOX, &[&new]);

    assert_eq!(screen.delete_spans_before(t(100)).unwrap(), 1, "only the one that ended before");
    assert_eq!(
        ids_of(&every_span(screen)),
        vec![ends_at_cut.id, running.id, new.id],
        "a span ending exactly at the cut, or running across it, is kept whole"
    );
    assert_eq!(screen.delete_spans_before(t(100)).unwrap(), 0, "and again is nothing");
    assert_eq!(screen.delete_spans_before(t(201)).unwrap(), 3);
    assert!(every_span(screen).is_empty());
}

fn the_earliest_span_is_the_one_that_began_first(screen: &dyn ScreenTimeStore) {
    let later = app("Later", 500, 600);
    let first = site("first.example", -3_600, 10);
    put(screen, LAPTOP_GNOME, &[&later]);
    assert_eq!(screen.earliest_span().unwrap(), Some(t(500)));
    put(screen, LAPTOP_FIREFOX, &[&first]);
    assert_eq!(screen.earliest_span().unwrap(), Some(t(-3_600)), "across streams");
    screen.delete_spans(&[first.id]).unwrap();
    assert_eq!(screen.earliest_span().unwrap(), Some(t(500)));
    clear_spans(screen);
    assert_eq!(screen.earliest_span().unwrap(), None);
}

fn a_label_round_trips_every_field(screen: &dyn ScreenTimeStore) {
    let work = category("Work", 0, t(0));
    screen.put_category(&work).unwrap();

    let mut google = label("site:google.com", SpanKind::Site, t(-60));
    google.name = "Google".into();
    google.category = Some(work.id);
    google.split = true;
    google.updated_at = t(0);
    let mut lock = label("app:gnome-shell", SpanKind::App, t(-30));
    lock.ignored = true;
    lock.uncategorized = true;
    let plain = label("app:org.gnome.ptyxis", SpanKind::App, t(-90));
    for app in [&google, &lock, &plain] {
        screen.put_app(app).expect("put_app");
    }

    assert_eq!(screen.get_app(google.id).unwrap(), google, "unchanged by a round trip");
    assert_eq!(screen.get_app(lock.id).unwrap(), lock);
    assert_eq!(screen.get_app(plain.id).unwrap(), plain, "and with nothing optional set");
    assert_eq!(
        screen.list_apps().unwrap(),
        vec![plain.clone(), google.clone(), lock.clone()],
        "in the order they were made"
    );

    // Replaced by id: an emptied category comes back empty.
    google.category = None;
    google.split = false;
    google.name.clear();
    google.updated_at = t(10);
    screen.put_app(&google).unwrap();
    assert_eq!(screen.list_apps().unwrap().len(), 3, "put must not duplicate");
    assert_eq!(screen.get_app(google.id).unwrap(), google);

    screen.delete_app(lock.id).expect("delete_app");
    super::assert_not_found(screen.get_app(lock.id));
    assert_eq!(screen.list_apps().unwrap(), vec![plain.clone(), google.clone()]);
    assert_eq!(screen.get_category(work.id).unwrap(), work, "a label's delete is only the label");

    screen.delete_app(google.id).unwrap();
    screen.delete_app(plain.id).unwrap();
    screen.delete_category(work.id).unwrap();
    assert!(screen.list_apps().unwrap().is_empty());
}

fn categories_round_trip_and_come_back_in_their_order(screen: &dyn ScreenTimeStore) {
    let mut social = category("Social", 2, t(0));
    social.color = "#db2777".into();
    social.builtin = "social".into();
    let work = category("Work", 1, t(10));
    // The same order as Work, made earlier: first of the two.
    let study = category("Study", 1, t(5));
    // The same order and the same moment: by id.
    let mut tie_a = category("TieA", 3, t(0));
    let mut tie_b = category("TieB", 3, t(0));
    if tie_a.id > tie_b.id {
        std::mem::swap(&mut tie_a.id, &mut tie_b.id);
    }
    for c in [&tie_b, &social, &work, &tie_a, &study] {
        screen.put_category(c).expect("put_category");
    }

    assert_eq!(screen.get_category(social.id).unwrap(), social, "unchanged by a round trip");
    assert_eq!(screen.get_category(work.id).unwrap(), work);
    assert_eq!(
        screen.list_categories().unwrap().iter().map(|c| c.id).collect::<Vec<_>>(),
        vec![study.id, work.id, social.id, tie_a.id, tie_b.id],
        "by order, then by when each was made, then by id"
    );

    // Replaced by id, and moved to the front by its order.
    social.sort_order = 0;
    social.name = "Friends".into();
    social.color.clear();
    screen.put_category(&social).unwrap();
    let listed = screen.list_categories().unwrap();
    assert_eq!(listed.len(), 5, "put must not duplicate");
    assert_eq!(listed[0], social);

    for c in [&social, &work, &study, &tie_a, &tie_b] {
        screen.delete_category(c.id).unwrap();
    }
    assert!(screen.list_categories().unwrap().is_empty());
}

fn deleting_a_category_touches_nothing_else(screen: &dyn ScreenTimeStore) {
    let social = category("Social", 0, t(0));
    let work = category("Work", 1, t(0));
    screen.put_category(&social).unwrap();
    screen.put_category(&work).unwrap();
    let mut reddit = label("site:reddit.com", SpanKind::Site, t(0));
    reddit.category = Some(social.id);
    screen.put_app(&reddit).unwrap();
    let browsing = site("www.reddit.com", 0, 60);
    put(screen, LAPTOP_FIREFOX, &[&browsing]);

    screen.delete_category(social.id).expect("delete_category");

    super::assert_not_found(screen.get_category(social.id));
    assert_eq!(
        screen.get_app(reddit.id).unwrap(),
        reddit,
        "a label still filed under it is the vault's to tidy, not the store's"
    );
    assert_eq!(every_span(screen), vec![browsing], "and the time spent is untouched");
    assert_eq!(screen.list_categories().unwrap(), vec![work.clone()]);

    screen.delete_app(reddit.id).unwrap();
    screen.delete_category(work.id).unwrap();
    clear_spans(screen);
}

fn settings_default_until_saved_and_round_trip(screen: &dyn ScreenTimeStore) {
    let default = screen.screen_settings().unwrap();
    assert_eq!(default, ScreenTimeSettings::default(), "never saved is the defaults");
    assert_eq!(default.idle_minutes, 3);
    assert!(default.keep_titles);
    assert_eq!(default.title_days, 0);

    let changed = ScreenTimeSettings {
        idle_minutes: 10,
        keep_titles: false,
        title_days: 90,
        titles_cleared_to: Some(t(5)),
        browsers: vec!["librewolf".into(), "org.qutebrowser.qutebrowser".into()],
    };
    screen.put_screen_settings(&changed).expect("put_screen_settings");
    assert_eq!(screen.screen_settings().unwrap(), changed);

    // A second save replaces the first; it is one row, not two.
    let again = ScreenTimeSettings { idle_minutes: 5, ..changed };
    screen.put_screen_settings(&again).unwrap();
    assert_eq!(screen.screen_settings().unwrap(), again);

    screen.put_screen_settings(&ScreenTimeSettings::default()).unwrap();
    assert_eq!(screen.screen_settings().unwrap(), ScreenTimeSettings::default());
}

fn unicode_survives_a_screen_time_round_trip(screen: &dyn ScreenTimeStore) {
    let mut span = site("xn--bcher-kva.example", 0, 120);
    span.device = "Hari\u{2019}s \u{7b14}\u{8bb0}\u{672c}".into();
    span.name = "Firefox \u{1f98a}".into();
    span.title = "B\u{00fc}cher \u{2014} \u{65e5}\u{672c}\u{8a9e} \u{1f4da}".into();
    put(screen, "\u{6d41}", &[&span]);
    assert_eq!(every_span(screen), vec![span.clone()]);
    assert_eq!(screen.last_span("\u{6d41}").unwrap(), Some(span));

    let mut label = label("site:b\u{00fc}cher.example", SpanKind::Site, t(0));
    label.name = "B\u{00fc}cher \u{1f4d6}".into();
    screen.put_app(&label).unwrap();
    assert_eq!(screen.get_app(label.id).unwrap(), label);

    let mut category = category("\u{5a2f}\u{697d} \u{1f3ae}", 0, t(0));
    category.builtin = "entertainment".into();
    screen.put_category(&category).unwrap();
    assert_eq!(screen.get_category(category.id).unwrap(), category);

    let settings = ScreenTimeSettings {
        browsers: vec!["\u{6d4f}\u{89c8}\u{5668}".into()],
        ..ScreenTimeSettings::default()
    };
    screen.put_screen_settings(&settings).unwrap();
    assert_eq!(screen.screen_settings().unwrap(), settings);

    screen.put_screen_settings(&ScreenTimeSettings::default()).unwrap();
    screen.delete_app(label.id).unwrap();
    screen.delete_category(category.id).unwrap();
    clear_spans(screen);
}
