//! The Feed half of the conformance suite: sources, fetch state, stories,
//! bodies, expiry, a newsletter story's read state read through mail, a
//! story's files, the Feed's settings, Up Next, what was consumed, and ranks.

use super::*;
use crate::id::{
    AccountId, ConsumptionId, FileId, ItemId, MailMessageId, NewsSourceId, PackId, QueueEntryId,
    StoryId, ThreadId,
};
use crate::mail::{Address, CategorySource, Mailbox, MailboxRole, Message, MessageFlags};
use crate::news::{
    AddedBy, Also, BodyOrigin, ConsumeMode, ConsumedQuery, Consumption, ContentRef, Criterion,
    DownloadPolicy, Enclosure, ExpireReport, ExpiryRule, FeedSettings, FetchState, HnList,
    KeepPolicy, NewsImage, NewsSource, QueueEntry, Rank, RankAnswer, Ranking, Retention,
    SourceKind, Span, Story, StoryBody, StoryFiles, TranscribePolicy,
};
use crate::packstore::PackRef as MailPackRef;
use crate::store::mail::IngestMessage;
use crate::store::news::{NewsStore, StoryQuery};
use jiff::{SignedDuration, Timestamp};

/// The Feed half of the suite. Called by [`run_all`] when the backend has a
/// [`NewsStore`](crate::store::news::NewsStore); public so a backend under
/// construction can run it alone.
///
/// Handed the whole journal store because a mail-backed story's read state
/// is its message's: the suite stores a message to check the join. The
/// garbage-collection check is a question about the journal store too.
///
/// The store must hold no sources on entry; it is left with none on success.
pub fn run_news_suite(store: &dyn JournalStore) {
    eprintln!("--- news conformance suite ---");
    let news = news_store(store);

    news_starts_empty(news);
    missing_news_records_are_not_found(news);
    source_round_trips_every_field(news);
    fetch_state_round_trips_and_says_what_is_due(news);
    a_story_round_trips_every_field(news);
    inserting_is_idempotent_by_source_and_key(news);
    update_story_rewrites_content_but_never_state(news);
    moving_stories_changes_their_source_and_skips_what_cannot_move(news);
    story_by_key_finds_the_first_arrival_across_sources(news);
    listing_filters_and_pages_newest_first(news);
    counts_follow_read_state_and_mark_all_read_moves_it(news);
    a_long_list_of_ids_is_one_call(news);
    bodies_round_trip_and_set_has_body(news);
    expiry_leaves_tombstones_and_spares_saved_stories(news);
    old_tombstones_are_removed_and_their_keys_forgotten(news);
    deleting_a_source_takes_its_stories_bodies_and_fetch_state(news);
    blob_refs_name_icons_and_cached_pictures(store);
    newsletter_stories_follow_their_message(store);
    unicode_survives_a_news_round_trip(news);

    files_are_written_only_by_set_story_files(news);
    a_story_with_files_or_in_up_next_is_never_expired(news);
    feed_settings_round_trip_and_are_replaced_whole(news);
    up_next_is_ordered_and_holds_one_entry_per_thing(news);
    deleting_from_up_next_ignores_what_is_not_there(news);
    consumption_is_one_record_per_thing(news);
    consumption_queries_filter_and_come_newest_first(news);
    ranks_are_replaced_by_thing_and_cleared_by_age(news);
    deleting_a_source_takes_its_queue_entries_and_ranks_but_not_what_was_consumed(news);
    a_list_longer_than_one_chunk_reaches_every_table(news);

    news_cleanup(news);
    eprintln!("--- news suite passed ---");
}

fn news_store(store: &dyn JournalStore) -> &dyn NewsStore {
    store.news().expect("the news suite needs a news store")
}

/// Every timestamp in this suite is whole seconds from this one, so a time
/// that comes back from a clear column (whole microseconds) compares equal
/// to the one that went in, and "thirty days ago" means the same thing on
/// every run.
const NOW: i64 = 1_800_000_000;

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).expect("a representable time")
}

fn now() -> Timestamp {
    at(NOW)
}

fn ago(days: i64) -> Timestamp {
    at(NOW - days * 86_400)
}

fn hours(n: i64) -> SignedDuration {
    SignedDuration::from_hours(n)
}

/// A web source, stored.
fn followed(news: &dyn NewsStore, title: &str) -> NewsSource {
    let source =
        NewsSource::new(SourceKind::Web { url: format!("https://{title}.example/feed") }, title);
    news.put_source(&source).expect("put_source");
    source
}

/// A story first seen when it was published, so the clear `created_us` it is
/// stored under is as predictable as its date.
fn new_story(source: &NewsSource, key: &str, title: &str, published: Timestamp) -> Story {
    let mut story = Story::new(source.id, key, title, published);
    story.fetched_at = published;
    story
}

fn body_of(story: &Story, html: &str) -> StoryBody {
    StoryBody {
        byline: None,
        site_name: None,
        story_id: story.id,
        html: html.into(),
        text: html.into(),
        origin: BodyOrigin::Feed,
        images: Vec::new(),
    }
}

/// Insert stories with no bodies and say which ones landed.
fn insert(news: &dyn NewsStore, stories: &[&Story]) -> Vec<StoryId> {
    let pairs: Vec<(Story, Option<StoryBody>)> =
        stories.iter().map(|s| ((*s).clone(), None)).collect();
    news.insert_stories(&pairs).expect("insert_stories")
}

fn list(news: &dyn NewsStore, query: &StoryQuery) -> Vec<Story> {
    news.list_stories(query).expect("list_stories").stories
}

fn listed_ids(news: &dyn NewsStore, query: &StoryQuery) -> Vec<StoryId> {
    list(news, query).iter().map(|s| s.id).collect()
}

/// Ids in a fixed order, for comparing sets the store answers in no
/// particular order.
fn sorted(mut ids: Vec<StoryId>) -> Vec<StoryId> {
    ids.sort();
    ids
}

/// `(unread, total)` for one source; a source with no stories may be absent
/// from the answer, and is then `(0, 0)`.
fn tally(news: &dyn NewsStore, source: NewsSourceId) -> (u32, u32) {
    news.counts()
        .expect("counts")
        .iter()
        .find(|c| c.source_id == source)
        .map_or((0, 0), |c| (c.unread, c.total))
}

/// Every page of a query, walked by cursor to the end.
fn walk(news: &dyn NewsStore, query: &StoryQuery) -> Vec<Vec<StoryId>> {
    let mut query = query.clone();
    let mut pages = Vec::new();
    loop {
        let page = news.list_stories(&query).expect("list_stories");
        pages.push(page.stories.iter().map(|s| s.id).collect());
        match page.next {
            Some(next) => query.before = Some(next),
            None => return pages,
        }
        assert!(pages.len() < 1000, "a cursor that never reaches the end");
    }
}

fn forget(news: &dyn NewsStore, sources: &[&NewsSource]) {
    for source in sources {
        news.delete_source(source.id).expect("delete_source");
    }
}

fn news_starts_empty(news: &dyn NewsStore) {
    assert!(news.list_sources().unwrap().is_empty(), "a fresh store must follow nothing");
    assert!(list(news, &StoryQuery::default()).is_empty(), "a fresh store must hold no stories");
    assert!(news.counts().unwrap().is_empty());
    assert!(news.due_sources(now()).unwrap().is_empty());
    assert_eq!(news.next_due().unwrap(), None);
    assert!(news.blob_refs().unwrap().is_empty());
    assert!(news.stories_with_files().unwrap().is_empty());
    assert_eq!(news.feed_settings().unwrap(), None, "settings are None until first saved");
    assert!(news.queue().unwrap().is_empty(), "a fresh store has nothing in Up Next");
    assert!(news.consumptions(&everything()).unwrap().is_empty());
    assert_eq!(news.clear_ranks(None).unwrap(), 0, "a fresh store has ranked nothing");
}

fn news_cleanup(news: &dyn NewsStore) {
    for source in news.list_sources().expect("list_sources") {
        news.delete_source(source.id).expect("delete_source");
    }
    assert!(news.list_sources().unwrap().is_empty(), "cleanup left sources behind");
    assert!(list(news, &StoryQuery::default()).is_empty(), "cleanup left stories behind");
    assert!(news.counts().unwrap().is_empty(), "cleanup left counts behind");
    assert_eq!(news.next_due().unwrap(), None, "cleanup left fetch state behind");
    assert!(news.blob_refs().unwrap().is_empty(), "cleanup left blob references behind");
    assert!(news.stories_with_files().unwrap().is_empty(), "cleanup left files behind");
    assert!(news.queue().unwrap().is_empty(), "cleanup left Up Next entries behind");
    assert!(news.consumptions(&everything()).unwrap().is_empty(), "cleanup left history behind");
    assert_eq!(news.clear_ranks(None).unwrap(), 0, "cleanup left ranks behind");
}

fn missing_news_records_are_not_found(news: &dyn NewsStore) {
    super::assert_not_found(news.get_source(NewsSourceId::new()));
    super::assert_not_found(news.get_story(StoryId::new()));
    assert!(news.fetch_state(NewsSourceId::new()).unwrap().is_none());
    assert!(news.get_body(StoryId::new()).unwrap().is_none());
    assert!(news.story_by_key("no such key").unwrap().is_none());
    assert!(news.keys_for_source(NewsSourceId::new()).unwrap().is_empty());
    assert!(news.get_stories(&[StoryId::new()]).unwrap().is_empty(), "missing ids are skipped");
    assert!(news.stories_for_mail(&[MailMessageId::new()]).unwrap().is_empty());

    // Unknown ids are ignored, not failed on: a list of ids that was right a
    // moment ago, and a source deleted since, must not stop the rest.
    news.set_read(&[StoryId::new()], true).expect("set_read ignores unknown ids");
    news.set_saved(&[StoryId::new()], true).expect("set_saved ignores unknown ids");
    assert!(news.counts().unwrap().is_empty());
}

fn source_round_trips_every_field(news: &dyn NewsStore) {
    let mut source = NewsSource::new(
        SourceKind::HackerNews { list: HnList::Top, min_points: 150 },
        "Hacker News, 150 and up",
    );
    source.folder = "Tech".into();
    source.site_url = "https://news.ycombinator.com".into();
    source.icon = Some(BlobId::of(b"a favicon"));
    source.images = false;
    source.full_text = true;
    source.keep_out_of_inbox = true;
    source.mute = vec!["crypto".into(), "NFT".into()];
    source.retention = Some(Retention { read_days: 7, unread_days: 21 });
    source.paused = true;
    source.sort_order = 4;
    news.put_source(&source).expect("put_source");

    let back = news.get_source(source.id).expect("get_source");
    assert_eq!(back, source, "a source must survive the round trip unchanged");
    assert_eq!(news.list_sources().unwrap(), vec![source.clone()]);

    let web = followed(news, "plain");
    let letters = NewsSource::new(
        SourceKind::Mail { sender: "letters@writer.example".into(), name: String::new() },
        "A writer's letters",
    );
    news.put_source(&letters).expect("put_source");
    assert_eq!(news.get_source(web.id).unwrap(), web);
    assert_eq!(news.get_source(letters.id).unwrap(), letters);
    assert_eq!(news.list_sources().unwrap().len(), 3);

    // Idempotent: writing the same id twice updates rather than duplicates.
    source.title = "Hacker News (renamed)".into();
    source.paused = false;
    news.put_source(&source).expect("put_source again");
    assert_eq!(news.list_sources().unwrap().len(), 3, "put must not duplicate");
    assert_eq!(news.get_source(source.id).unwrap(), source);

    forget(news, &[&source, &web, &letters]);
    assert!(news.list_sources().unwrap().is_empty());
}

fn fetch_state_round_trips_and_says_what_is_due(news: &dyn NewsStore) {
    let never = followed(news, "never-fetched");
    let overdue = followed(news, "overdue");
    let very_overdue = followed(news, "very-overdue");
    let later = followed(news, "not-yet");

    assert!(news.fetch_state(never.id).unwrap().is_none(), "nobody has fetched it yet");
    assert_eq!(news.next_due().unwrap(), None, "no state anywhere means nothing to sleep until");

    let state = |source: &NewsSource, next: Timestamp| {
        let mut s = FetchState::new(source.id);
        s.next_at = next;
        s
    };
    let mut full = state(&overdue, now() - hours(1));
    full.etag = Some("\"33a64df5\"".into());
    full.last_modified = Some("Sat, 10 Oct 2026 08:00:00 GMT".into());
    full.last_attempt = Some(now() - hours(1));
    full.last_ok = Some(now() - hours(9));
    full.last_error = Some("the server took too long to answer".into());
    full.failing_since = Some(now() - hours(5));
    full.failures = 3;
    full.interval_minutes = 90;
    full.recent = vec![now() - hours(7), now() - hours(30)];
    full.watermark = Some(now() - hours(12));
    news.put_fetch_state(&full).expect("put_fetch_state");
    assert_eq!(news.fetch_state(overdue.id).unwrap(), Some(full.clone()));

    news.put_fetch_state(&state(&very_overdue, now() - hours(2))).unwrap();
    news.put_fetch_state(&state(&later, now() + hours(1))).unwrap();

    // The source nobody has fetched comes first -- it is wanted at once --
    // and then the rest, soonest first. One not due yet is left out.
    assert_eq!(
        news.due_sources(now()).unwrap(),
        vec![never.id, very_overdue.id, overdue.id],
        "never fetched, then soonest first"
    );
    assert_eq!(news.next_due().unwrap(), Some(now() - hours(2)));

    // Exactly at its time counts as due.
    assert_eq!(
        news.due_sources(now() + hours(1)).unwrap(),
        vec![never.id, very_overdue.id, overdue.id, later.id]
    );

    // Replacing state moves a source: pushed out, it stops being due.
    full.next_at = now() + hours(3);
    news.put_fetch_state(&full).unwrap();
    assert_eq!(news.fetch_state(overdue.id).unwrap(), Some(full));
    assert_eq!(news.due_sources(now()).unwrap(), vec![never.id, very_overdue.id]);

    // Fetching the one nobody had fetched gives it a time, and it too waits.
    news.put_fetch_state(&state(&never, now() + hours(2))).unwrap();
    assert_eq!(news.due_sources(now()).unwrap(), vec![very_overdue.id]);
    assert_eq!(news.next_due().unwrap(), Some(now() - hours(2)));
    news.put_fetch_state(&state(&very_overdue, now() + hours(6))).unwrap();
    assert!(news.due_sources(now()).unwrap().is_empty());
    assert_eq!(news.next_due().unwrap(), Some(now() + hours(1)), "the soonest of what is left");

    forget(news, &[&never, &overdue, &very_overdue, &later]);
    assert_eq!(news.next_due().unwrap(), None, "deleting the sources took their state with them");
}

fn a_story_round_trips_every_field(news: &dyn NewsStore) {
    let source = followed(news, "full");
    let elsewhere = followed(news, "elsewhere");

    let mut story = new_story(&source, "k-full", "Show HN: a thing", at(NOW - 5_000));
    story.url = "https://thing.example/".into();
    story.author = "pg".into();
    story.fetched_at = at(NOW - 4_000);
    story.summary = "It does a thing.".into();
    story.image_url = "https://thing.example/lead.png".into();
    story.score = Some(412);
    story.comments = Some(97);
    story.discussion_url = "https://news.example/item?id=1".into();
    story.also = vec![Also {
        source_id: elsewhere.id,
        score: Some(5),
        comments: None,
        discussion_url: "https://other.example/thread/1".into(),
    }];
    story.read = true;
    story.saved = true;
    story.media = Some(Enclosure {
        url: "https://cdn.example/episode-12.mp3".into(),
        mime: "audio/mpeg".into(),
        length: Some(48_000_000),
        duration_ms: Some(3_005_000),
        transcript_url: "https://cdn.example/episode-12.vtt".into(),
        transcript_mime: "text/vtt".into(),
        episode: Some(12),
        season: Some(2),
    });
    story.files = StoryFiles {
        media: Some(FileId::new()),
        transcript: Some(FileId::new()),
        text: Some(FileId::new()),
        audio: Some(FileId::new()),
        summary: None,
    };
    assert_eq!(insert(news, &[&story]), vec![story.id]);

    assert_eq!(news.get_story(story.id).unwrap(), story, "a story must survive unchanged");
    assert_eq!(
        news.stories_with_files().unwrap(),
        vec![story.clone()],
        "a story inserted with files says so in its clear column"
    );
    assert_eq!(news.get_stories(&[story.id]).unwrap(), vec![story.clone()]);
    assert_eq!(news.story_by_key("k-full").unwrap(), Some(story.clone()));
    assert_eq!(list(news, &StoryQuery::default()), vec![story.clone()]);
    assert_eq!(news.keys_for_source(source.id).unwrap(), vec!["k-full".to_string()]);

    forget(news, &[&source, &elsewhere]);
}

fn inserting_is_idempotent_by_source_and_key(news: &dyn NewsStore) {
    let blog = followed(news, "blog");
    let other = followed(news, "other");

    let first = new_story(&blog, "k-first", "First", ago(2));
    let second = new_story(&blog, "k-second", "Second", ago(1));
    let first_body = body_of(&first, "<p>The first body.</p>");
    let inserted = news
        .insert_stories(&[(first.clone(), Some(first_body.clone())), (second.clone(), None)])
        .unwrap();
    assert_eq!(inserted, vec![first.id, second.id], "the ids inserted, in input order");
    assert!(news.get_story(first.id).unwrap().has_body, "a story with a body says so");
    assert!(!news.get_story(second.id).unwrap().has_body);
    assert_eq!(news.get_body(first.id).unwrap(), Some(first_body.clone()));
    assert!(news.insert_stories(&[]).unwrap().is_empty());

    // The same feed again, as the next refresh sees it: both old items, with
    // fresh ids and a retitled first one, a body for one that will not land,
    // and one new item. Only the new one lands.
    let again_first = new_story(&blog, "k-first", "First, retitled", ago(2));
    let again_second = new_story(&blog, "k-second", "Second", ago(1));
    let third = new_story(&blog, "k-third", "Third", ago(0));
    let rejected_body = body_of(&again_second, "<p>This must not be stored.</p>");
    let inserted = news
        .insert_stories(&[
            (again_first.clone(), None),
            (again_second.clone(), Some(rejected_body)),
            (third.clone(), None),
        ])
        .unwrap();
    assert_eq!(inserted, vec![third.id], "what is already stored is not in the answer");
    assert_eq!(news.get_story(first.id).unwrap().title, "First", "a stored story is left alone");
    super::assert_not_found(news.get_story(again_first.id));
    super::assert_not_found(news.get_story(again_second.id));
    assert!(
        news.get_body(again_second.id).unwrap().is_none(),
        "the body of a story that did not land is not stored"
    );
    assert_eq!(news.get_body(first.id).unwrap(), Some(first_body), "nor does it replace one");
    let mut keys = news.keys_for_source(blog.id).unwrap();
    keys.sort();
    assert_eq!(keys, ["k-first", "k-second", "k-third"]);

    // Unique per source: the same key in another source is another story.
    let elsewhere = new_story(&other, "k-first", "Also first", ago(2));
    assert_eq!(insert(news, &[&elsewhere]), vec![elsewhere.id]);

    // Two of a kind in one batch: the first wins.
    let one = new_story(&blog, "k-twin", "One", ago(0));
    let two = new_story(&blog, "k-twin", "Two", ago(0));
    assert_eq!(insert(news, &[&one, &two]), vec![one.id]);

    // What a person did survives the refresh that follows.
    news.set_read(&[first.id], true).unwrap();
    news.set_saved(&[first.id], true).unwrap();
    assert!(insert(news, &[&again_first]).is_empty());
    let kept = news.get_story(first.id).unwrap();
    assert!(kept.read && kept.saved, "a refresh must not undo a tap");

    // Read and saved go into their columns as the story carries them.
    let mut tapped = new_story(&blog, "k-tapped", "Already read", ago(3));
    tapped.read = true;
    tapped.saved = true;
    assert_eq!(insert(news, &[&tapped]), vec![tapped.id]);
    let back = news.get_story(tapped.id).unwrap();
    assert!(back.read && back.saved && !back.expired);
    let unread = listed_ids(news, &StoryQuery { unread_only: true, ..Default::default() });
    assert!(!unread.contains(&tapped.id), "the column decides, so it is not unread");
    assert!(unread.contains(&second.id));

    forget(news, &[&blog, &other]);
}

fn update_story_rewrites_content_but_never_state(news: &dyn NewsStore) {
    let source = followed(news, "hn");
    let other = followed(news, "hn-too");
    let mut story = new_story(&source, "k-update", "Show HN: a thing", ago(1));
    story.score = Some(10);
    story.comments = Some(2);
    assert_eq!(insert(news, &[&story]), vec![story.id]);
    news.set_read(&[story.id], true).unwrap();
    news.set_saved(&[story.id], true).unwrap();

    // A refresh that read the story before the tap and writes it back after,
    // carrying whatever it had: the sealed content moves and the state does
    // not.
    let mut changed = story.clone();
    changed.score = Some(412);
    changed.comments = Some(97);
    changed.also = vec![Also {
        source_id: other.id,
        score: Some(3),
        comments: Some(1),
        discussion_url: "https://other.example/item/1".into(),
    }];
    changed.read = false;
    changed.saved = false;
    changed.expired = true;
    news.update_story(&changed).expect("update_story");

    let back = news.get_story(story.id).unwrap();
    assert_eq!(back.score, Some(412));
    assert_eq!(back.comments, Some(97));
    assert_eq!(back.also, changed.also);
    assert!(back.read, "update_story never touches read");
    assert!(back.saved, "update_story never touches saved");
    assert!(!back.expired, "update_story never touches expired");

    let listed = list(news, &StoryQuery::default());
    assert_eq!(listed.len(), 1, "a story that was not expired is still listed");
    assert!(listed[0].read && listed[0].saved);
    assert_eq!(listed[0].score, Some(412));
    assert!(
        listed_ids(news, &StoryQuery { unread_only: true, ..Default::default() }).is_empty(),
        "it is still read, whatever the record passed in said"
    );

    forget(news, &[&source, &other]);
}

fn moving_stories_changes_their_source_and_skips_what_cannot_move(news: &dyn NewsStore) {
    let all = followed(news, "all-letters");
    let weekly = followed(news, "weekly");
    let digest = followed(news, "digest");

    let mut mine = new_story(&all, "k-mine", "Issue 1", ago(5));
    mine.score = Some(3);
    let theirs = new_story(&all, "k-theirs", "Issue 2", ago(4));
    let twin = new_story(&all, "k-twin", "Issue 3", ago(3));
    let old = new_story(&all, "k-old", "Issue 0", ago(200));
    let stay = new_story(&weekly, "k-stay", "Already here", ago(2));
    let already = new_story(&weekly, "k-twin", "Already had it", ago(3));
    assert_eq!(insert(news, &[&mine, &theirs, &twin, &old, &stay, &already]).len(), 6);

    // What a story carries is not the source's: a state, a body, a file.
    news.set_read(&[mine.id], true).unwrap();
    news.set_saved(&[mine.id], true).unwrap();
    news.put_body(&body_of(&mine, "<p>Issue one.</p>")).unwrap();
    let files = files_of(true, true);
    news.set_story_files(mine.id, &files).unwrap();
    let rules =
        [ExpiryRule { source_id: all.id, retention: Retention { read_days: 30, unread_days: 30 } }];
    assert_eq!(news.expire(&rules, now()).unwrap().expired, 1);
    assert!(news.get_story(old.id).unwrap().expired, "a tombstone, to be left where it is");

    let moved = news
        .move_stories(&[
            (mine.id, weekly.id),
            (theirs.id, digest.id),
            // The other source has this key: stays, and costs nothing else.
            (twin.id, weekly.id),
            (old.id, weekly.id),
            (StoryId::new(), weekly.id),
            // Where it is already.
            (stay.id, weekly.id),
        ])
        .expect("move_stories");
    assert_eq!(moved, 2);

    // The column and the sealed copy agree: it is listed under the new
    // source, and says so.
    let weekly_query = StoryQuery { sources: vec![weekly.id], ..Default::default() };
    assert_eq!(sorted(listed_ids(news, &weekly_query)), sorted(vec![mine.id, stay.id, already.id]));
    let digest_query = StoryQuery { sources: vec![digest.id], ..Default::default() };
    assert_eq!(listed_ids(news, &digest_query), vec![theirs.id]);
    assert_eq!(news.get_story(mine.id).unwrap().source_id, weekly.id);
    assert_eq!(news.get_story(theirs.id).unwrap().source_id, digest.id);
    assert_eq!(news.get_story(twin.id).unwrap().source_id, all.id, "the key was taken");
    assert_eq!(news.get_story(old.id).unwrap().source_id, all.id, "a tombstone stays");
    assert_eq!(news.get_story(stay.id).unwrap().source_id, weekly.id);

    let back = news.get_story(mine.id).unwrap();
    assert!(back.read && back.saved && back.has_body, "the rest of the story goes with it");
    assert_eq!((back.title.as_str(), back.score, &back.files), ("Issue 1", Some(3), &files));
    assert!(news.get_body(mine.id).unwrap().is_some());
    assert_eq!(news.stories_with_files().unwrap(), vec![back]);

    assert_eq!(tally(news, all.id), (1, 1), "the one that stayed");
    assert_eq!(tally(news, weekly.id), (2, 3), "the moved one is read, the others not");
    assert_eq!(tally(news, digest.id), (1, 1));
    let mut left = news.keys_for_source(all.id).unwrap();
    left.sort();
    assert_eq!(left, ["k-old", "k-twin"], "a moved story's key goes with it");
    let mut there = news.keys_for_source(weekly.id).unwrap();
    there.sort();
    assert_eq!(there, ["k-mine", "k-stay", "k-twin"]);

    // A second pass finds nothing left to do.
    assert_eq!(news.move_stories(&[(mine.id, weekly.id), (theirs.id, digest.id)]).unwrap(), 0);
    assert_eq!(news.move_stories(&[]).unwrap(), 0);

    // It is the new source's now, as far as unfollowing goes.
    forget(news, &[&all]);
    assert!(news.get_story(mine.id).is_ok() && news.get_story(theirs.id).is_ok());
    super::assert_not_found(news.get_story(twin.id));
    forget(news, &[&weekly]);
    super::assert_not_found(news.get_story(mine.id));
    assert!(news.get_body(mine.id).unwrap().is_none());
    assert!(news.stories_with_files().unwrap().is_empty());
    forget(news, &[&digest]);
}

fn story_by_key_finds_the_first_arrival_across_sources(news: &dyn NewsStore) {
    let first = followed(news, "first-seen");
    let second = followed(news, "second-seen");
    let early = new_story(&first, "k-shared", "From the blog", ago(50));
    let late = new_story(&second, "k-shared", "From the aggregator", ago(2));

    // Stored the other way round, so that the answer has to come from when
    // each was first seen and not from which row was written first.
    assert_eq!(insert(news, &[&late]), vec![late.id]);
    assert_eq!(insert(news, &[&early]), vec![early.id]);
    assert!(news.story_by_key("k-unrelated").unwrap().is_none());
    let found = news.story_by_key("k-shared").unwrap().expect("a story under that key");
    assert_eq!(found.id, early.id, "the earliest arrival is the one later ones fold into");
    assert_eq!(found.title, "From the blog");

    // Once that one has expired it is a tombstone and no longer an answer.
    let rules = [ExpiryRule {
        source_id: first.id,
        retention: Retention { read_days: 30, unread_days: 30 },
    }];
    let report = news.expire(&rules, now()).unwrap();
    assert_eq!(report, ExpireReport { expired: 1, removed: 0, mail: Vec::new() });
    let found = news.story_by_key("k-shared").unwrap().expect("the other is still live");
    assert_eq!(found.id, late.id);

    let rules = [ExpiryRule {
        source_id: second.id,
        retention: Retention { read_days: 1, unread_days: 1 },
    }];
    assert_eq!(news.expire(&rules, now()).unwrap().expired, 1);
    assert!(news.story_by_key("k-shared").unwrap().is_none(), "only tombstones are left");

    forget(news, &[&first, &second]);
}

fn listing_filters_and_pages_newest_first(news: &dyn NewsStore) {
    let a = followed(news, "alpha");
    let b = followed(news, "beta");

    // Eight stories at six distinct minutes, with two more tied to the
    // middle one, so that a page boundary can fall inside a tie.
    let base = ago(1);
    let minute = |n: i64| base + SignedDuration::from_secs(60 * n);
    let mut all: Vec<Story> = (0..6)
        .map(|i| {
            let source = if i % 2 == 0 { &a } else { &b };
            new_story(source, &format!("k{i}"), &format!("Story {i}"), minute(i))
        })
        .collect();
    all.push(new_story(&a, "tie-a", "Tie A", minute(3)));
    all.push(new_story(&b, "tie-b", "Tie B", minute(3)));
    assert_eq!(insert(news, &all.iter().collect::<Vec<_>>()).len(), 8);

    // Newest first; the tied three in descending id order.
    let everything = list(news, &StoryQuery::default());
    let ids: Vec<StoryId> = everything.iter().map(|s| s.id).collect();
    assert_eq!(ids.len(), 8);
    assert_eq!(ids[0], all[5].id);
    assert_eq!(ids[1], all[4].id);
    assert_eq!(ids[5], all[2].id);
    assert_eq!(ids[6], all[1].id);
    assert_eq!(ids[7], all[0].id);
    assert_eq!(
        sorted(ids[2..5].to_vec()),
        sorted(vec![all[3].id, all[6].id, all[7].id]),
        "the three tied stories sit in the middle"
    );
    assert!(ids[2..5].windows(2).all(|w| w[0] > w[1]), "ties break by id, descending");
    assert!(everything.windows(2).all(|w| w[0].published_at >= w[1].published_at));
    assert!(news.list_stories(&StoryQuery::default()).unwrap().next.is_none());

    // Paging by cursor, with pages that end before, inside and after the tie.
    let page = news.list_stories(&StoryQuery { limit: 3, ..Default::default() }).unwrap();
    assert_eq!(page.stories.len(), 3);
    assert!(page.next.is_some(), "more to come");
    for limit in [1, 2, 3, 4, 5, 7, 8, 9] {
        let pages = walk(news, &StoryQuery { limit, ..Default::default() });
        let expected = 8usize.div_ceil(limit as usize).max(1);
        assert_eq!(pages.len(), expected, "limit {limit}");
        assert!(pages.iter().all(|p| p.len() <= limit as usize));
        assert_eq!(pages.concat(), ids, "limit {limit}: no story twice and none missed");
    }
    assert_eq!(
        walk(news, &StoryQuery { limit: 3, ..Default::default() })
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        [3, 3, 2]
    );

    // One source, then two, then none meaning all, then one that is not there.
    let of = |source: &NewsSource| -> Vec<StoryId> {
        ids.iter()
            .copied()
            .filter(|id| everything.iter().any(|s| s.id == *id && s.source_id == source.id))
            .collect()
    };
    let only_b = StoryQuery { sources: vec![b.id], ..Default::default() };
    assert_eq!(listed_ids(news, &only_b), of(&b));
    assert_eq!(of(&b).len(), 4);
    let both = StoryQuery { sources: vec![a.id, b.id], ..Default::default() };
    assert_eq!(listed_ids(news, &both), ids);
    let nobody = StoryQuery { sources: vec![NewsSourceId::new()], ..Default::default() };
    assert!(listed_ids(news, &nobody).is_empty());
    let paged_b = walk(news, &StoryQuery { limit: 3, ..only_b.clone() });
    assert_eq!(paged_b.concat(), of(&b), "a filter and a cursor together");

    // Unread, saved, and both, read from the columns.
    news.set_read(&[all[0].id, all[5].id], true).unwrap();
    let unread = StoryQuery { unread_only: true, ..Default::default() };
    let expect: Vec<StoryId> =
        ids.iter().copied().filter(|id| *id != all[0].id && *id != all[5].id).collect();
    assert_eq!(listed_ids(news, &unread), expect);
    assert_eq!(walk(news, &StoryQuery { limit: 2, ..unread.clone() }).concat(), expect);

    news.set_saved(&[all[2].id, all[6].id], true).unwrap();
    let saved = StoryQuery { saved_only: true, ..Default::default() };
    let saved_ids: Vec<StoryId> =
        ids.iter().copied().filter(|id| *id == all[2].id || *id == all[6].id).collect();
    assert_eq!(listed_ids(news, &saved), saved_ids);
    let saved_in_a = StoryQuery { sources: vec![a.id], ..saved.clone() };
    assert_eq!(listed_ids(news, &saved_in_a), saved_ids, "both are in alpha");
    let saved_in_b = StoryQuery { sources: vec![b.id], ..saved.clone() };
    assert!(listed_ids(news, &saved_in_b).is_empty(), "and neither is in beta");

    news.set_read(&[all[6].id], true).unwrap();
    let both_filters = StoryQuery { saved_only: true, unread_only: true, ..Default::default() };
    assert_eq!(listed_ids(news, &both_filters), vec![all[2].id], "saved and still unread");

    // Published at or after a time.
    let since = StoryQuery { since: Some(minute(3)), ..Default::default() };
    assert_eq!(listed_ids(news, &since), ids[..5].to_vec(), "inclusive of the minute itself");
    let since = StoryQuery { since: Some(minute(4)), ..Default::default() };
    assert_eq!(listed_ids(news, &since), ids[..2].to_vec());
    let since = StoryQuery { since: Some(minute(6)), ..Default::default() };
    assert!(listed_ids(news, &since).is_empty());

    forget(news, &[&a, &b]);
}

fn counts_follow_read_state_and_mark_all_read_moves_it(news: &dyn NewsStore) {
    let a = followed(news, "counted");
    let b = followed(news, "also-counted");
    let idle = followed(news, "idle");
    let a_old = new_story(&a, "a-old", "Old", ago(10));
    let a_edge = new_story(&a, "a-edge", "Edge", ago(5));
    let a_new = new_story(&a, "a-new", "New", ago(1));
    let b_one = new_story(&b, "b-one", "One", ago(3));
    let b_two = new_story(&b, "b-two", "Two", ago(2));
    assert_eq!(insert(news, &[&a_old, &a_edge, &a_new, &b_one, &b_two]).len(), 5);

    assert_eq!(tally(news, a.id), (3, 3));
    assert_eq!(tally(news, b.id), (2, 2));
    assert_eq!(tally(news, idle.id), (0, 0), "a source with nothing in it may be absent");

    news.set_read(&[a_old.id, a_new.id], true).unwrap();
    assert_eq!(tally(news, a.id), (1, 3), "read stories are still counted, not as unread");
    news.set_read(&[a_new.id], false).unwrap();
    assert_eq!(tally(news, a.id), (2, 3));
    news.set_read(&[a_old.id], false).unwrap();

    // Everything up to a day, every source: the edge itself is included, the
    // newer ones are not.
    let changed = news.mark_all_read(&[], ago(5)).unwrap();
    assert_eq!(sorted(changed), sorted(vec![a_old.id, a_edge.id]));
    assert_eq!(tally(news, a.id), (1, 3));
    assert_eq!(tally(news, b.id), (2, 2), "published after the cut-off");
    assert!(news.get_story(a_old.id).unwrap().read);
    assert!(news.mark_all_read(&[], ago(5)).unwrap().is_empty(), "only what changed is returned");

    // One source at a time.
    let changed = news.mark_all_read(&[b.id], now()).unwrap();
    assert_eq!(sorted(changed), sorted(vec![b_one.id, b_two.id]));
    assert_eq!(tally(news, b.id), (0, 2));
    assert_eq!(tally(news, a.id), (1, 3), "the other source is not touched");
    assert!(news.mark_all_read(&[idle.id], now()).unwrap().is_empty());

    let changed = news.mark_all_read(&[], now()).unwrap();
    assert_eq!(changed, vec![a_new.id]);
    assert_eq!(tally(news, a.id), (0, 3));
    assert!(listed_ids(news, &StoryQuery { unread_only: true, ..Default::default() }).is_empty());

    let every = [a_old.id, a_edge.id, a_new.id, b_one.id, b_two.id];
    news.set_read(&every, false).unwrap();
    assert_eq!((tally(news, a.id), tally(news, b.id)), ((3, 3), (2, 2)));
    assert_eq!(list(news, &StoryQuery { unread_only: true, ..Default::default() }).len(), 5);

    forget(news, &[&a, &b, &idle]);
    assert!(news.counts().unwrap().is_empty());
}

/// Marking a backlog read is one call however long the list: more ids than a
/// single `IN (...)` should carry.
fn a_long_list_of_ids_is_one_call(news: &dyn NewsStore) {
    const N: i64 = 1_200;
    let source = followed(news, "backlog");
    let stories: Vec<Story> = (0..N)
        .map(|i| {
            let published = ago(1) + SignedDuration::from_secs(i);
            new_story(&source, &format!("bulk-{i}"), &format!("Story {i}"), published)
        })
        .collect();
    let inserted = insert(news, &stories.iter().collect::<Vec<_>>());
    assert_eq!(inserted, stories.iter().map(|s| s.id).collect::<Vec<_>>());
    let ids = inserted;
    assert_eq!(tally(news, source.id), (N as u32, N as u32));

    news.set_read(&ids, true).unwrap();
    assert_eq!(tally(news, source.id), (0, N as u32));
    assert!(listed_ids(news, &StoryQuery { unread_only: true, ..Default::default() }).is_empty());

    news.set_read(&ids[..700], false).unwrap();
    assert_eq!(tally(news, source.id), (700, N as u32));

    news.set_saved(&ids, true).unwrap();
    let saved = walk(news, &StoryQuery { saved_only: true, limit: 500, ..Default::default() });
    assert_eq!(saved.iter().map(Vec::len).collect::<Vec<_>>(), [500, 500, 200]);
    assert_eq!(sorted(saved.concat()), sorted(ids.clone()));

    let fetched = news.get_stories(&ids).unwrap();
    assert_eq!(fetched.len(), ids.len(), "every story, from a list longer than one chunk");
    assert!(fetched.iter().all(|s| s.saved));

    let changed = news.mark_all_read(&[source.id], now()).unwrap();
    assert_eq!(sorted(changed), sorted(ids[..700].to_vec()));
    assert_eq!(tally(news, source.id), (0, N as u32));

    forget(news, &[&source]);
}

fn bodies_round_trip_and_set_has_body(news: &dyn NewsStore) {
    let source = followed(news, "bodies");
    let story = new_story(&source, "k-body", "Has no body yet", ago(1));
    assert_eq!(insert(news, &[&story]), vec![story.id]);
    assert!(news.get_body(story.id).unwrap().is_none());
    assert!(!news.get_story(story.id).unwrap().has_body);

    news.set_read(&[story.id], true).unwrap();
    news.set_saved(&[story.id], true).unwrap();

    let mut body = StoryBody {
        byline: None,
        site_name: None,
        story_id: story.id,
        html: "<p>Hello, <b>world</b>.</p>".into(),
        text: "Hello, world.".into(),
        origin: BodyOrigin::Extracted,
        images: vec![
            NewsImage {
                token: "ab12".into(),
                url: "https://img.example/a.png".into(),
                cached_blob: Some(BlobId::of(b"a picture")),
            },
            NewsImage {
                token: "cd34".into(),
                url: "https://img.example/b.jpg".into(),
                cached_blob: None,
            },
        ],
    };
    news.put_body(&body).expect("put_body");
    assert_eq!(news.get_body(story.id).unwrap(), Some(body.clone()));

    let stored = news.get_story(story.id).unwrap();
    assert!(stored.has_body, "putting a body sets has_body");
    assert!(stored.read && stored.saved, "and leaves what a person did alone");
    assert_eq!(Story { has_body: false, read: false, saved: false, ..stored }, story);
    assert!(list(news, &StoryQuery::default())[0].has_body);

    // Insert or replace.
    body.html = "<p>The author's own text.</p>".into();
    body.text = "The author's own text.".into();
    body.origin = BodyOrigin::HackerNews;
    body.images.clear();
    news.put_body(&body).expect("put_body again");
    assert_eq!(news.get_body(story.id).unwrap(), Some(body));
    assert!(news.get_story(story.id).unwrap().has_body);

    // A body needs a story to belong to, and does not leave one behind.
    let ghost = body_of(&new_story(&source, "k-ghost", "Never stored", ago(1)), "<p>Nobody's</p>");
    assert!(news.put_body(&ghost).is_err(), "no story, no body");
    assert!(news.get_body(ghost.story_id).unwrap().is_none());

    forget(news, &[&source]);
}

fn expiry_leaves_tombstones_and_spares_saved_stories(news: &dyn NewsStore) {
    let tight = followed(news, "tight");
    let loose = followed(news, "default-retention");
    let rules = [ExpiryRule {
        source_id: tight.id,
        retention: Retention { read_days: 10, unread_days: 30 },
    }];

    let aged = |source: &NewsSource, key: &str, days: i64, read: bool, saved: bool| {
        let mut story = new_story(source, key, &format!("{key} (title)"), ago(days));
        story.url = format!("https://{key}.example/");
        story.author = "somebody".into();
        story.summary = format!("About {key}.");
        story.score = Some(12);
        story.read = read;
        story.saved = saved;
        story
    };
    // `tight` keeps what is read for ten days and what is not for thirty;
    // `loose` has no rule, so it gets thirty and ninety.
    let doomed = [
        aged(&tight, "t-old-unread", 31, false, false),
        aged(&tight, "t-old-read", 11, true, false),
        aged(&loose, "l-old-read", 31, true, false),
        aged(&loose, "l-old-unread", 91, false, false),
    ];
    let spared = [
        aged(&tight, "t-mid-unread", 29, false, false),
        aged(&tight, "t-new-read", 9, true, false),
        aged(&tight, "t-saved", 200, false, true),
        aged(&loose, "l-new-read", 29, true, false),
        aged(&loose, "l-mid-unread", 89, false, false),
    ];
    let pairs: Vec<(Story, Option<StoryBody>)> = doomed
        .iter()
        .chain(&spared)
        .map(|s| (s.clone(), Some(body_of(s, &format!("<p>{}</p>", s.key)))))
        .collect();
    assert_eq!(news.insert_stories(&pairs).unwrap().len(), 9);
    assert_eq!((tally(news, tight.id), tally(news, loose.id)), ((3, 5), (2, 4)));

    let report = news.expire(&rules, now()).unwrap();
    assert_eq!(report, ExpireReport { expired: 4, removed: 0, mail: Vec::new() });

    // Never listed or counted again; what is kept is untouched.
    let listed = listed_ids(news, &StoryQuery::default());
    assert_eq!(sorted(listed), sorted(spared.iter().map(|s| s.id).collect()));
    assert_eq!((tally(news, tight.id), tally(news, loose.id)), ((2, 3), (1, 2)));
    let saved = news.get_story(spared[2].id).unwrap();
    assert!(saved.saved && !saved.expired && saved.title == "t-saved (title)");
    assert!(news.get_body(saved.id).unwrap().is_some(), "a saved story keeps its body too");
    assert!(news.get_body(spared[0].id).unwrap().is_some());

    // The tombstone keeps the row -- id, source, key, date -- and nothing
    // else, and its body is gone.
    for story in &doomed {
        let tombstone = news.get_story(story.id).unwrap();
        assert!(tombstone.expired, "{}", story.key);
        assert_eq!(tombstone.source_id, story.source_id);
        assert_eq!(tombstone.key, story.key);
        assert_eq!(tombstone.published_at, story.published_at);
        assert!(tombstone.title.is_empty() && tombstone.url.is_empty());
        assert!(tombstone.author.is_empty() && tombstone.summary.is_empty());
        assert_eq!(tombstone.score, None);
        assert!(!tombstone.has_body);
        assert!(news.get_body(story.id).unwrap().is_none(), "{}: body deleted", story.key);
        assert!(news.story_by_key(&story.key).unwrap().is_none(), "{}: not live", story.key);
    }
    let mut keys = news.keys_for_source(tight.id).unwrap();
    keys.sort();
    assert_eq!(
        keys,
        ["t-mid-unread", "t-new-read", "t-old-read", "t-old-unread", "t-saved"],
        "tombstones are listed among the keys, so a refresh knows them"
    );

    // And a feed that still lists an old item does not bring it back.
    let revived = new_story(&tight, "t-old-unread", "Back again", ago(31));
    assert!(insert(news, &[&revived]).is_empty(), "a tombstone blocks re-insertion");
    super::assert_not_found(news.get_story(revived.id));
    assert_eq!(listed_ids(news, &StoryQuery::default()).len(), 5);

    // Nothing more to do the second time.
    assert_eq!(news.expire(&rules, now()).unwrap(), ExpireReport::default());

    forget(news, &[&tight, &loose]);
}

fn old_tombstones_are_removed_and_their_keys_forgotten(news: &dyn NewsStore) {
    let source = followed(news, "purge");
    let rules = [ExpiryRule {
        source_id: source.id,
        retention: Retention { read_days: 30, unread_days: 30 },
    }];
    let doomed = new_story(&source, "doomed", "Doomed", ago(100));
    let mut saved = new_story(&source, "kept", "Kept", ago(500));
    saved.saved = true;
    let fresh = new_story(&source, "fresh", "Fresh", ago(1));
    let kept_body = body_of(&saved, "<p>Kept.</p>");
    let pairs = [
        (doomed.clone(), Some(body_of(&doomed, "<p>Doomed.</p>"))),
        (saved.clone(), Some(kept_body.clone())),
        (fresh.clone(), None),
    ];
    assert_eq!(news.insert_stories(&pairs).unwrap().len(), 3);

    assert_eq!(
        news.expire(&rules, now()).unwrap(),
        ExpireReport { expired: 1, removed: 0, mail: Vec::new() }
    );
    let mut keys = news.keys_for_source(source.id).unwrap();
    keys.sort();
    assert_eq!(keys, ["doomed", "fresh", "kept"], "a young tombstone is kept");

    // A year and more on: the tombstone is four hundred days old and goes,
    // and the unread story that has meanwhile aged out becomes a young one.
    let later = now() + SignedDuration::from_hours(24 * 300);
    let report = news.expire(&rules, later).unwrap();
    assert_eq!(report, ExpireReport { expired: 1, removed: 1, mail: Vec::new() });
    let mut keys = news.keys_for_source(source.id).unwrap();
    keys.sort();
    assert_eq!(keys, ["fresh", "kept"], "the old tombstone's key is forgotten");
    super::assert_not_found(news.get_story(doomed.id));
    assert!(news.get_story(fresh.id).unwrap().expired);

    // Forgotten means a feed that still lists it brings it back as new.
    let back = new_story(&source, "doomed", "Doomed, again", ago(100));
    assert_eq!(insert(news, &[&back]), vec![back.id]);

    // A saved story is never expired, and never purged: it is not a tombstone.
    let kept = news.get_story(saved.id).unwrap();
    assert!(kept.saved && !kept.expired);
    assert_eq!(news.get_body(saved.id).unwrap(), Some(kept_body));

    forget(news, &[&source]);
}

fn deleting_a_source_takes_its_stories_bodies_and_fetch_state(news: &dyn NewsStore) {
    let mut doomed =
        NewsSource::new(SourceKind::Web { url: "https://doomed.example/".into() }, "doomed");
    doomed.icon = Some(BlobId::of(b"doomed icon"));
    news.put_source(&doomed).unwrap();
    let spared = followed(news, "spared");

    let picture = |bytes: &[u8]| NewsImage {
        token: "t".into(),
        url: "https://img.example/x.png".into(),
        cached_blob: Some(BlobId::of(bytes)),
    };
    let plain = new_story(&doomed, "d-plain", "Plain", ago(2));
    let mut starred = new_story(&doomed, "d-saved", "Saved, and still goes", ago(3));
    starred.saved = true;
    let survivor = new_story(&spared, "s-one", "Survivor", ago(1));
    let mut doomed_body = body_of(&plain, "<p>Doomed.</p>");
    doomed_body.images = vec![picture(b"doomed picture")];
    let mut starred_body = body_of(&starred, "<p>Also doomed.</p>");
    starred_body.images = vec![picture(b"also doomed")];
    let mut survivor_body = body_of(&survivor, "<p>Fine.</p>");
    survivor_body.images = vec![picture(b"survivor picture")];
    news.insert_stories(&[
        (plain.clone(), Some(doomed_body)),
        (starred.clone(), Some(starred_body)),
        (survivor.clone(), Some(survivor_body.clone())),
    ])
    .unwrap();
    // The doomed source's fetch state is due sooner than the spared one's, so
    // if it outlived the delete it would be what `next_due` said.
    let mut doomed_state = FetchState::new(doomed.id);
    doomed_state.next_at = now() - hours(5);
    let mut spared_state = FetchState::new(spared.id);
    spared_state.next_at = now() + hours(5);
    news.put_fetch_state(&doomed_state).unwrap();
    news.put_fetch_state(&spared_state).unwrap();
    assert_eq!(news.blob_refs().unwrap().len(), 4);

    news.delete_source(doomed.id).expect("delete_source");

    super::assert_not_found(news.get_source(doomed.id));
    assert!(news.fetch_state(doomed.id).unwrap().is_none(), "its fetch state goes");
    super::assert_not_found(news.get_story(plain.id));
    super::assert_not_found(news.get_story(starred.id));
    assert!(news.get_body(plain.id).unwrap().is_none(), "its bodies go");
    assert!(news.get_body(starred.id).unwrap().is_none(), "saved ones too");
    assert!(news.keys_for_source(doomed.id).unwrap().is_empty());
    assert!(news.get_stories(&[plain.id, starred.id]).unwrap().is_empty());
    assert_eq!(listed_ids(news, &StoryQuery::default()), vec![survivor.id]);
    assert!(listed_ids(news, &StoryQuery { saved_only: true, ..Default::default() }).is_empty());
    assert_eq!(tally(news, doomed.id), (0, 0));
    assert_eq!(tally(news, spared.id), (1, 1));
    assert_eq!(news.next_due().unwrap(), Some(now() + hours(5)));
    assert_eq!(news.blob_refs().unwrap(), vec![BlobId::of(b"survivor picture")]);

    // The neighbour is exactly as it was.
    assert_eq!(news.get_source(spared.id).unwrap(), spared);
    assert_eq!(news.get_body(survivor.id).unwrap(), Some(survivor_body));
    assert_eq!(news.fetch_state(spared.id).unwrap(), Some(spared_state));

    forget(news, &[&spared]);
}

/// A feed's favicon and the pictures cached for a story must survive the
/// garbage collector, and stop surviving it when the source is unfollowed.
///
/// Handed the journal store because collecting is a question about it: it
/// walks every record that can hold a blob, and the Feed holds two kinds.
fn blob_refs_name_icons_and_cached_pictures(store: &dyn JournalStore) {
    let news = news_store(store);
    let icon = store.put_blob(b"a feed's favicon").unwrap();
    let cached = store.put_blob(b"a picture shown in a story").unwrap();
    let orphan = store.put_blob(b"nobody's picture").unwrap();

    let mut source =
        NewsSource::new(SourceKind::Web { url: "https://pics.example/feed".into() }, "pics");
    source.icon = Some(icon);
    news.put_source(&source).unwrap();
    let bare = followed(news, "no-icon");
    let story = new_story(&source, "k-pics", "With pictures", ago(1));
    let mut body = body_of(&story, "<p>Look.</p>");
    body.images = vec![
        NewsImage {
            token: "a".into(),
            url: "https://i.example/a.png".into(),
            cached_blob: Some(cached),
        },
        NewsImage { token: "b".into(), url: "https://i.example/b.png".into(), cached_blob: None },
    ];
    news.insert_stories(&[(story.clone(), Some(body.clone()))]).unwrap();

    let mut refs = news.blob_refs().unwrap();
    refs.sort();
    let mut expect = vec![icon, cached];
    expect.sort();
    assert_eq!(refs, expect, "the icon and the cached picture, and not the one never fetched");

    let removed = store.collect_garbage(std::time::Duration::ZERO).unwrap();
    assert!(removed >= 1, "the unreferenced blob should be collected");
    assert!(store.has_blob(icon).unwrap(), "a source's icon is a live reference");
    assert!(store.has_blob(cached).unwrap(), "a picture a body names is a live reference");
    assert!(!store.has_blob(orphan).unwrap());

    // A body that no longer shows the picture lets it go.
    body.images.clear();
    news.put_body(&body).unwrap();
    assert_eq!(news.blob_refs().unwrap(), vec![icon]);

    // ...and unfollowing lets the rest go.
    body.images = vec![NewsImage {
        token: "a".into(),
        url: "https://i.example/a.png".into(),
        cached_blob: Some(cached),
    }];
    news.put_body(&body).unwrap();
    assert_eq!(news.blob_refs().unwrap().len(), 2);
    forget(news, &[&source, &bare]);
    assert!(news.blob_refs().unwrap().is_empty());
    store.collect_garbage(std::time::Duration::ZERO).unwrap();
    assert!(!store.has_blob(icon).unwrap(), "an icon nothing points at is collectable");
    assert!(!store.has_blob(cached).unwrap(), "so is a picture no body names");
}

/// A mail message, as the mail suite builds one: only what the join reads
/// matters here -- that it exists, and its flags.
fn issue(account: AccountId, subject: &str, from: &str, date: Timestamp) -> Message {
    let id = MailMessageId::new();
    Message {
        id,
        account_id: account,
        thread_id: ThreadId::new(),
        message_id_header: format!("<{id}@conformance.example>"),
        date,
        from: Address::bare(from),
        to: Vec::new(),
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: subject.into(),
        snippet: String::new(),
        flags: MessageFlags::default(),
        labels: Vec::new(),
        has_attachments: false,
        size: 128,
        category: None,
        category_source: CategorySource::Rules,
        pack: MailPackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
        gmail: None,
        invite: None,
    }
}

fn newsletter_stories_follow_their_message(store: &dyn JournalStore) {
    let news = news_store(store);
    // A feed can be offered without mail (`Capabilities::news` says so
    // separately), and then there are no newsletters to check.
    let (Some(mail), Some(accounts)) = (store.mail(), store.accounts()) else {
        eprintln!("backend {:?} stores no mail; skipping the newsletter check", store.backend());
        return;
    };
    let account = AccountId::new();
    let inbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
    mail.put_mailbox(&inbox).unwrap();

    let sender = "letters@writer.example";
    let letters = NewsSource::new(
        SourceKind::Mail { sender: sender.into(), name: String::new() },
        "A writer's letters",
    );
    news.put_source(&letters).unwrap();
    let plain_source = followed(news, "not-mail");

    // Five issues, none seen. Uids are 1..=5 in order.
    let issues: Vec<Message> =
        (0..5).map(|i| issue(account, &format!("Issue {i}"), sender, ago(10 - i))).collect();
    mail.ingest(
        account,
        issues
            .iter()
            .enumerate()
            .map(|(i, m)| IngestMessage {
                message: m.clone(),
                mailbox: inbox.id,
                uid: i as u32 + 1,
            })
            .collect(),
    )
    .unwrap();
    let seen = |on: bool| MessageFlags { seen: on, ..Default::default() };
    let mark = |uid: u32, on: bool| mail.update_flags(inbox.id, uid, seen(on)).unwrap();

    let backed = |i: usize, published: Timestamp| {
        let mut story = new_story(&letters, &format!("m{i}"), &format!("Issue {i}"), published);
        story.mail_message = Some(issues[i].id);
        story
    };
    // Issue 0's own column claims it is read, which the message contradicts;
    // the message is the truth.
    let mut s0 = backed(0, ago(3));
    s0.read = true;
    let s1 = backed(1, ago(2));
    let s2 = backed(2, ago(1));
    let plain = new_story(&plain_source, "p", "Not from mail", ago(1));
    assert_eq!(insert(news, &[&s0, &s1, &s2, &plain]).len(), 4);

    let unread_ids = |news: &dyn NewsStore| {
        sorted(listed_ids(news, &StoryQuery { unread_only: true, ..Default::default() }))
    };
    assert!(!news.get_story(s0.id).unwrap().read, "the column is ignored for a newsletter");
    assert_eq!(unread_ids(news), sorted(vec![s0.id, s1.id, s2.id, plain.id]));
    assert_eq!(tally(news, letters.id), (3, 3));

    // Read it in Mail, and it is read here, with nothing written here.
    mark(2, true);
    assert!(news.get_story(s1.id).unwrap().read);
    assert!(list(news, &StoryQuery::default()).iter().any(|s| s.id == s1.id && s.read));
    assert_eq!(unread_ids(news), sorted(vec![s0.id, s2.id, plain.id]));
    assert_eq!(tally(news, letters.id), (2, 3));

    // Marking the story does nothing to a message: only mail can change it.
    news.set_read(&[s2.id], true).unwrap();
    news.set_read(&[s1.id], false).unwrap();
    assert!(!news.get_story(s2.id).unwrap().read, "the message is still unseen");
    assert!(news.get_story(s1.id).unwrap().read, "the message is still seen");
    assert_eq!(tally(news, letters.id), (2, 3));

    // Mark everything read: the plain story changes, the newsletters do not.
    let changed = news.mark_all_read(&[], now()).unwrap();
    assert_eq!(changed, vec![plain.id], "mail-backed stories are left to mail");
    assert_eq!(unread_ids(news), sorted(vec![s0.id, s2.id]));
    assert!(news.mark_all_read(&[letters.id], now()).unwrap().is_empty());

    // And it follows the message back again.
    mark(2, false);
    assert!(!news.get_story(s1.id).unwrap().read);
    assert_eq!(tally(news, letters.id), (3, 3));
    mark(2, true);

    // Which stories already stand for these messages.
    let mut pairs = news
        .stories_for_mail(&[issues[0].id, issues[2].id, issues[4].id, MailMessageId::new()])
        .unwrap();
    pairs.sort();
    let mut expect = vec![(issues[0].id, s0.id), (issues[2].id, s2.id)];
    expect.sort();
    assert_eq!(pairs, expect);
    assert!(news.stories_for_mail(&[]).unwrap().is_empty());

    // Expiry reads the message's state as well: an old issue that has been
    // read goes after the read period, an unread one waits for the longer.
    let old_seen = backed(3, ago(40));
    let mut old_unseen = backed(4, ago(40));
    old_unseen.read = true;
    assert_eq!(insert(news, &[&old_seen, &old_unseen]).len(), 2);
    mark(4, true); // uid 4 is issue 3, the one whose story is `old_seen`
    let report = news.expire(&[], now()).unwrap();
    assert_eq!(report, ExpireReport { expired: 1, removed: 0, mail: Vec::new() });
    assert!(news.get_story(old_seen.id).unwrap().expired, "read in Mail, so past thirty days");
    assert!(!news.get_story(old_unseen.id).unwrap().expired, "unread in Mail, so kept");

    // Delete a message and its story is gone from every list and count.
    mail.remove_uids(inbox.id, &[1]).unwrap();
    assert!(!listed_ids(news, &StoryQuery::default()).contains(&s0.id));
    assert!(!unread_ids(news).contains(&s0.id));
    assert_eq!(tally(news, letters.id), (2, 3), "issue 0 no longer counts, read or not");

    // And deleting the account takes every message, so every newsletter
    // story, leaving only what never pointed at mail.
    accounts.delete_account(account).unwrap();
    assert_eq!(listed_ids(news, &StoryQuery::default()), vec![plain.id]);
    assert_eq!(tally(news, letters.id), (0, 0));
    assert!(list(news, &StoryQuery { unread_only: true, ..Default::default() }).is_empty());

    forget(news, &[&letters, &plain_source]);
}

fn unicode_survives_a_news_round_trip(news: &dyn NewsStore) {
    let mut source = NewsSource::new(
        SourceKind::Web {
            url: "https://\u{4f8b}\u{3048}.example/\u{30d5}\u{30a3}\u{30fc}\u{30c9}".into(),
        },
        "\u{1f4f0} Le Monde \u{2014} \u{6f22}\u{5b57}",
    );
    source.folder = "\u{00c9}tudes \u{05e2}\u{05d1}\u{05e8}\u{05d9}\u{05ea}".into();
    source.site_url = "https://\u{4f8b}\u{3048}.example/".into();
    source.mute = vec!["\u{1f4a9}".into(), "caf\u{00e9}".into()];
    news.put_source(&source).unwrap();
    assert_eq!(news.get_source(source.id).unwrap(), source);

    let mut story = new_story(
        &source,
        "k-unicode",
        "\u{1f480} Se\u{00f1}or Babel \u{2014} \u{6f22}\u{5b57}",
        ago(1),
    );
    story.author = "\u{00c1}sd\u{00ed}s \u{00d3}sk".into();
    story.summary =
        "emoji \u{1f4da}\u{1f3c1}, combining a\u{0301}, RTL \u{05e2}\u{05d1}\u{05e8}".into();
    story.url = "https://\u{4f8b}\u{3048}.example/\u{8a18}\u{4e8b}?q=\u{00fc}".into();
    let mut body = body_of(&story, "<p>\u{6751}\u{4e0a}\u{6625}\u{6a39} \u{2014} \u{1f3a7}</p>");
    body.text = "\u{6751}\u{4e0a}\u{6625}\u{6a39} \u{2014} \u{1f3a7}".into();
    news.insert_stories(&[(story.clone(), Some(body.clone()))]).unwrap();

    let back = news.get_story(story.id).unwrap();
    assert_eq!(Story { has_body: false, ..back }, story, "unicode must survive sealing");
    assert_eq!(news.get_body(story.id).unwrap(), Some(body));
    assert_eq!(news.story_by_key("k-unicode").unwrap().unwrap().title, story.title);

    forget(news, &[&source]);
}

// ---- a story's files ------------------------------------------------------

fn files_of(media: bool, transcript: bool) -> StoryFiles {
    StoryFiles {
        media: media.then(FileId::new),
        transcript: transcript.then(FileId::new),
        ..Default::default()
    }
}

fn files_are_written_only_by_set_story_files(news: &dyn NewsStore) {
    let source = followed(news, "show");
    let story = new_story(&source, "k-ep", "Episode 1", ago(1));
    assert_eq!(insert(news, &[&story]), vec![story.id]);
    assert!(news.stories_with_files().unwrap().is_empty(), "no files, no entry");

    // What the story already has -- a state, a body -- is not disturbed by
    // linking a file to it.
    news.set_read(&[story.id], true).unwrap();
    news.set_saved(&[story.id], true).unwrap();
    news.put_body(&body_of(&story, "<p>Show notes.</p>")).unwrap();

    let files = files_of(true, true);
    news.set_story_files(story.id, &files).expect("set_story_files");
    let back = news.get_story(story.id).unwrap();
    assert_eq!(back.files, files);
    assert!(back.read && back.saved && back.has_body, "the rest of the story is as stored");
    assert_eq!(back.title, "Episode 1");
    assert_eq!(news.stories_with_files().unwrap(), vec![back.clone()]);
    assert_eq!(listed_ids(news, &StoryQuery::default()), vec![story.id]);

    // A refresh holding a copy read before the download finished -- no
    // files -- or one carrying files of its own: the stored ones stay.
    let mut stale = story.clone();
    stale.score = Some(77);
    news.update_story(&stale).expect("update_story");
    assert_eq!(news.get_story(story.id).unwrap().files, files, "a refresh must not unlink");
    assert_eq!(news.get_story(story.id).unwrap().score, Some(77), "but it still updates the rest");
    let mut meddling = story.clone();
    meddling.files = files_of(false, true);
    news.update_story(&meddling).unwrap();
    assert_eq!(news.get_story(story.id).unwrap().files, files, "whatever the record says");
    assert_eq!(news.stories_with_files().unwrap().len(), 1);

    // Replaced, not merged.
    let replacement = StoryFiles { text: Some(FileId::new()), ..Default::default() };
    news.set_story_files(story.id, &replacement).unwrap();
    assert_eq!(news.get_story(story.id).unwrap().files, replacement);
    assert_eq!(news.stories_with_files().unwrap().len(), 1);

    // Emptied, the story leaves the list, and a refresh does not bring the
    // files back.
    news.set_story_files(story.id, &StoryFiles::default()).unwrap();
    assert!(news.get_story(story.id).unwrap().files.is_empty());
    assert!(news.stories_with_files().unwrap().is_empty());
    news.update_story(&meddling).unwrap();
    assert!(news.get_story(story.id).unwrap().files.is_empty());
    assert!(news.stories_with_files().unwrap().is_empty());

    // Only a story that exists and is live has files to set.
    super::assert_not_found(news.set_story_files(StoryId::new(), &files));
    let old = new_story(&source, "k-old", "Old", ago(200));
    insert(news, &[&old]);
    assert_eq!(
        news.expire(&[], now()).unwrap(),
        ExpireReport { expired: 1, removed: 0, mail: Vec::new() }
    );
    super::assert_not_found(news.set_story_files(old.id, &files));
    assert!(news.get_story(old.id).unwrap().files.is_empty(), "a tombstone stays one");

    forget(news, &[&source]);
}

fn a_story_with_files_or_in_up_next_is_never_expired(news: &dyn NewsStore) {
    let source = followed(news, "ephemeral");
    let rules = [ExpiryRule {
        source_id: source.id,
        retention: Retention { read_days: 10, unread_days: 10 },
    }];
    let old = |key: &str| new_story(&source, key, key, ago(100));
    let (with_files, queued, plain) = (old("with-files"), old("queued"), old("plain"));
    let written = old("transcript-only");
    let mut saved = old("saved");
    saved.saved = true;
    insert(news, &[&with_files, &queued, &saved, &plain, &written]);
    news.set_story_files(with_files.id, &files_of(true, false)).unwrap();
    news.set_story_files(written.id, &files_of(false, true)).unwrap();

    // Nothing in `keep` is expired, nor is a story with its episode
    // downloaded; saved ones never are. The plain one goes, and so does one
    // whose only file is a transcript: that stays in Files without it.
    let report = news.expire_except(&rules, &[queued.id], now()).unwrap();
    assert_eq!(report, ExpireReport { expired: 2, removed: 0, mail: Vec::new() });
    assert!(news.get_story(plain.id).unwrap().expired);
    assert!(news.get_story(written.id).unwrap().expired);
    for kept in [&with_files, &queued, &saved] {
        assert!(!news.get_story(kept.id).unwrap().expired, "{}", kept.key);
    }

    // `expire` is `expire_except` with nothing to keep: the story that was
    // in Up Next is now just an old one, and the one with files still is not.
    let report = news.expire(&rules, now()).unwrap();
    assert_eq!(report, ExpireReport { expired: 1, removed: 0, mail: Vec::new() });
    assert!(news.get_story(queued.id).unwrap().expired);
    assert!(!news.get_story(with_files.id).unwrap().expired, "expire spares files too");
    assert_eq!(news.expire(&rules, now()).unwrap(), ExpireReport::default());

    // With its files gone, it ages out like any other.
    news.set_story_files(with_files.id, &StoryFiles::default()).unwrap();
    assert_eq!(
        news.expire(&rules, now()).unwrap(),
        ExpireReport { expired: 1, removed: 0, mail: Vec::new() }
    );
    assert!(news.get_story(with_files.id).unwrap().expired);
    assert!(!news.get_story(saved.id).unwrap().expired);

    forget(news, &[&source]);
}

// ---- the Feed's settings ----------------------------------------------------

fn feed_settings_round_trip_and_are_replaced_whole(news: &dyn NewsStore) {
    assert_eq!(news.feed_settings().unwrap(), None);

    let mut settings = FeedSettings::default();
    settings.podcasts.download = DownloadPolicy::All;
    settings.podcasts.keep = KeepPolicy::Newest { count: 5 };
    settings.podcasts.queue_new = true;
    settings.podcasts.transcribe = TranscribePolicy::WhenDownloaded;
    settings.ranking.criteria = vec![
        Criterion { id: "ai".into(), text: "about building software with AI".into(), weight: 3 },
        Criterion { id: "ads".into(), text: "mostly an advertisement".into(), weight: -2 },
    ];
    settings.ranking.goals = true;
    settings.ranking.window_days = 14;
    settings.keep_read = false;
    settings.autofill = Some(8);
    settings.autoplay = false;
    settings.skip_back_s = 10;
    settings.skip_forward_s = 45;
    news.put_feed_settings(&settings).expect("put_feed_settings");
    assert_eq!(news.feed_settings().unwrap(), Some(settings.clone()), "every field survives");

    // Saved whole: what is not in the new copy is not kept from the old.
    let plain = FeedSettings::default();
    news.put_feed_settings(&plain).unwrap();
    assert_eq!(news.feed_settings().unwrap(), Some(plain), "replaced, not merged");

    // There is no way to unset them, so the suite leaves the defaults, which
    // is what a vault that never saved any behaves as.
}

// ---- Up Next ------------------------------------------------------------------

/// An entry added `added` seconds past [`NOW`], so ties in `sort_order` are
/// broken by something the test chose.
fn entry(what: ContentRef, title: &str, order: i64, added: i64) -> QueueEntry {
    let mut entry = QueueEntry::new(what, title, order);
    entry.added_at = at(NOW + added);
    entry
}

fn queued(news: &dyn NewsStore) -> Vec<QueueEntryId> {
    news.queue().expect("queue").iter().map(|e| e.id).collect()
}

fn up_next_is_ordered_and_holds_one_entry_per_thing(news: &dyn NewsStore) {
    let (a, b, c, d) = (
        ContentRef::Story(StoryId::new()),
        ContentRef::File(FileId::new()),
        ContentRef::Item(ItemId::new()),
        ContentRef::Story(StoryId::new()),
    );

    // Every field survives, and the order is `sort_order` then `added_at`,
    // whatever order they went in.
    let mut first = entry(a, "The first", 10, 0);
    first.source = "A Show".into();
    first.mode = ConsumeMode::Listen;
    first.duration_ms = Some(1_800_000);
    first.added_by = AddedBy::Recommended;
    let tie_early = entry(b, "Tie, added early", 20, 5);
    let tie_late = entry(c, "Tie, added late", 20, 9);
    let before_all = entry(d, "Before all", -5, 99);
    for e in [&tie_late, &before_all, &first, &tie_early] {
        news.put_queue_entry(e).expect("put_queue_entry");
    }
    assert_eq!(queued(news), [before_all.id, first.id, tie_early.id, tie_late.id]);
    assert_eq!(news.queue().unwrap()[1], first, "an entry survives unchanged");

    // The same id is replaced, and can move.
    let mut moved = first.clone();
    moved.sort_order = 30;
    moved.title = "The first, retitled".into();
    news.put_queue_entry(&moved).unwrap();
    assert_eq!(queued(news), [before_all.id, tie_early.id, tie_late.id, first.id]);
    assert_eq!(news.queue().unwrap()[3].title, "The first, retitled");

    // The same thing under another id replaces the entry that held it: one
    // entry per thing.
    let again = entry(b, "The file, queued again", 0, 50);
    news.put_queue_entry(&again).unwrap();
    assert_eq!(queued(news), [before_all.id, again.id, tie_late.id, first.id]);
    assert!(!queued(news).contains(&tie_early.id), "the older entry for it is gone");
    assert_eq!(news.queue().unwrap().len(), 4);

    // An entry may be re-pointed at something else; the thing it left is
    // free, and the thing it took loses its old entry.
    let mut repointed = again.clone();
    repointed.what = d;
    news.put_queue_entry(&repointed).unwrap();
    let after = news.queue().unwrap();
    assert_eq!(after.len(), 3, "taking d's place removed d's entry");
    assert!(after.iter().all(|e| e.id != before_all.id));
    assert!(after.iter().any(|e| e.id == again.id && e.what == d));
    let reuse = entry(b, "The file, once more", 1, 60);
    news.put_queue_entry(&reuse).unwrap();
    assert_eq!(news.queue().unwrap().len(), 4);

    news.delete_queue_entries(&queued(news)).unwrap();
    assert!(news.queue().unwrap().is_empty());
}

fn deleting_from_up_next_ignores_what_is_not_there(news: &dyn NewsStore) {
    let things: Vec<ContentRef> = (0..4).map(|_| ContentRef::Story(StoryId::new())).collect();
    let entries: Vec<QueueEntry> = things
        .iter()
        .enumerate()
        .map(|(i, what)| entry(*what, &format!("Entry {i}"), i as i64, 0))
        .collect();
    for e in &entries {
        news.put_queue_entry(e).unwrap();
    }

    // By entry id: the unknown one, and an empty list, are no trouble.
    news.delete_queue_entries(&[]).unwrap();
    news.delete_queue_entries(&[QueueEntryId::new(), entries[0].id]).unwrap();
    assert_eq!(queued(news), [entries[1].id, entries[2].id, entries[3].id]);

    // By thing, whatever the entry's id; things that are not queued are
    // ignored.
    news.delete_queue_for(&[]).unwrap();
    news.delete_queue_for(&[
        things[2],
        things[0],
        ContentRef::File(FileId::new()),
        ContentRef::Item(ItemId::new()),
    ])
    .unwrap();
    assert_eq!(queued(news), [entries[1].id, entries[3].id]);

    news.delete_queue_for(&[things[1], things[3]]).unwrap();
    assert!(news.queue().unwrap().is_empty());
}

// ---- what was read, heard and watched -----------------------------------------

fn everything() -> ConsumedQuery {
    ConsumedQuery { limit: 500, ..Default::default() }
}

/// Something consumed, last touched `last` seconds past [`NOW`].
fn consumed(what: ContentRef, title: &str, last: i64) -> Consumption {
    let mut consumption = Consumption::new(what, title, ConsumeMode::Listen);
    consumption.first_at = at(NOW + last - 600);
    consumption.last_at = at(NOW + last);
    consumption
}

fn consumed_ids(found: &[Consumption]) -> Vec<ConsumptionId> {
    found.iter().map(|c| c.id).collect()
}

fn forget_consumed(news: &dyn NewsStore, all: &[&Consumption]) {
    for c in all {
        news.delete_consumption(c.id).expect("delete_consumption");
    }
}

fn consumption_is_one_record_per_thing(news: &dyn NewsStore) {
    let episode = ContentRef::Story(StoryId::new());
    let film = ContentRef::File(FileId::new());
    assert_eq!(news.consumption(&episode).unwrap(), None);
    assert!(news.consumptions_for(&[episode, film]).unwrap().is_empty());

    // Every field survives.
    let mut heard = consumed(episode, "Episode 12", 100);
    heard.source = "A Show".into();
    heard.url = "https://show.example/12".into();
    heard.progress = 0.75;
    heard.finished = true;
    heard.finished_at = Some(at(NOW + 90));
    heard.position_ms = Some(2_250_000);
    heard.duration_ms = Some(3_000_000);
    heard.heard = vec![
        Span { start_ms: 0, end_ms: 1_000_000 },
        Span { start_ms: 1_500_000, end_ms: 2_250_000 },
    ];
    heard.text_file = Some(FileId::new());
    heard.media_file = Some(FileId::new());
    news.put_consumption(&heard).expect("put_consumption");
    assert_eq!(news.consumption(&episode).unwrap(), Some(heard.clone()));
    assert_eq!(news.consumptions_for(&[episode, film]).unwrap(), vec![heard.clone()]);
    assert_eq!(news.consumptions(&everything()).unwrap(), vec![heard.clone()]);

    // The same id is replaced, and `last_at` -- a clear column -- moves with
    // it; coming back to something widens it rather than adding a row.
    let mut again = heard.clone();
    again.progress = 1.0;
    again.last_at = at(NOW + 200);
    again.finished = false;
    news.put_consumption(&again).unwrap();
    assert_eq!(news.consumption(&episode).unwrap(), Some(again.clone()));
    assert_eq!(news.consumptions(&everything()).unwrap().len(), 1);
    let unfinished = ConsumedQuery { finished_only: true, ..everything() };
    assert!(news.consumptions(&unfinished).unwrap().is_empty(), "the column follows the record");

    // Another id for the same thing replaces the one that held it.
    let mut other = consumed(episode, "Episode 12, from elsewhere", 300);
    other.mode = ConsumeMode::Watch;
    news.put_consumption(&other).unwrap();
    assert_eq!(news.consumptions(&everything()).unwrap(), vec![other.clone()]);
    assert_eq!(news.consumption(&episode).unwrap(), Some(other.clone()));

    // A neighbour is not disturbed, and many can be asked for at once.
    let watched = consumed(film, "A film", 50);
    news.put_consumption(&watched).unwrap();
    let mut pair =
        news.consumptions_for(&[film, episode, ContentRef::Item(ItemId::new())]).unwrap();
    pair.sort_by_key(|c| c.id);
    let mut expected = vec![other.clone(), watched.clone()];
    expected.sort_by_key(|c| c.id);
    assert_eq!(pair, expected, "what has none is skipped");

    // Forgetting one is one record; an unknown id is ignored.
    news.delete_consumption(ConsumptionId::new()).unwrap();
    news.delete_consumption(other.id).unwrap();
    assert_eq!(news.consumption(&episode).unwrap(), None);
    assert_eq!(news.consumption(&film).unwrap(), Some(watched.clone()));
    forget_consumed(news, &[&watched]);
    assert!(news.consumptions(&everything()).unwrap().is_empty());
}

fn consumption_queries_filter_and_come_newest_first(news: &dyn NewsStore) {
    let day = 86_400;
    let mut all: Vec<Consumption> = (1..=5)
        .map(|n| {
            consumed(ContentRef::Story(StoryId::new()), &format!("Touched {n} days ago"), -n * day)
        })
        .collect();
    // The second and fourth newest were read to the end.
    for i in [1, 3] {
        all[i].finished = true;
        all[i].finished_at = Some(all[i].last_at);
    }
    // Put in a scrambled order, so the order asked for is the store's doing.
    for i in [2, 0, 4, 1, 3] {
        news.put_consumption(&all[i]).unwrap();
    }
    let ids = |range: std::ops::Range<usize>| consumed_ids(&all[range]);
    let found = |query: &ConsumedQuery| consumed_ids(&news.consumptions(query).unwrap());

    assert_eq!(found(&everything()), ids(0..5), "newest last-touched first");
    assert_eq!(found(&ConsumedQuery::default()), ids(0..5), "no limit means the default one");

    // `since` is inclusive and `until` is not; both together are a window.
    let since = ConsumedQuery { since: Some(all[2].last_at), ..everything() };
    assert_eq!(found(&since), ids(0..3), "touched at or after");
    let until = ConsumedQuery { until: Some(all[2].last_at), ..everything() };
    assert_eq!(found(&until), ids(3..5), "touched before");
    let window =
        ConsumedQuery { since: Some(all[3].last_at), until: Some(all[1].last_at), ..everything() };
    assert_eq!(found(&window), ids(2..4));

    let finished = ConsumedQuery { finished_only: true, ..everything() };
    assert_eq!(found(&finished), vec![all[1].id, all[3].id]);
    let both = ConsumedQuery { finished_only: true, since: Some(all[2].last_at), ..everything() };
    assert_eq!(found(&both), vec![all[1].id], "the filters are ANDed");

    // The limit keeps the newest.
    assert_eq!(found(&ConsumedQuery { limit: 2, ..Default::default() }), ids(0..2));
    assert_eq!(
        found(&ConsumedQuery { limit: 1, finished_only: true, ..Default::default() }),
        [all[1].id]
    );
    assert!(found(&ConsumedQuery { since: Some(at(NOW + 1)), ..everything() }).is_empty());

    forget_consumed(news, &all.iter().collect::<Vec<_>>());
    assert!(news.consumptions(&everything()).unwrap().is_empty());
}

// ---- ranks ----------------------------------------------------------------------

fn rank_of(what: ContentRef, score: f32, seconds: i64) -> Rank {
    Rank {
        what,
        score,
        answers: vec![
            RankAnswer { criterion: "ai".into(), yes: 0.75 },
            RankAnswer { criterion: Ranking::GOALS.into(), yes: 0.25 },
        ],
        fingerprint: "0123456789abcdef".into(),
        at: at(NOW + seconds),
    }
}

fn ranked(news: &dyn NewsStore, what: &[ContentRef]) -> Vec<Rank> {
    let mut found = news.ranks(what).expect("ranks");
    found.sort_by_key(|r| r.what);
    found
}

fn sorted_ranks(mut ranks: Vec<Rank>) -> Vec<Rank> {
    ranks.sort_by_key(|r| r.what);
    ranks
}

fn ranks_are_replaced_by_thing_and_cleared_by_age(news: &dyn NewsStore) {
    let (a, b, c) = (
        ContentRef::Story(StoryId::new()),
        ContentRef::Story(StoryId::new()),
        ContentRef::Item(ItemId::new()),
    );
    assert!(news.ranks(&[]).unwrap().is_empty());
    assert!(news.ranks(&[a]).unwrap().is_empty());
    news.put_ranks(&[]).expect("an empty list is nothing to do");

    // Every field survives, and what has no rank is skipped.
    let (ra, rb, rc) = (rank_of(a, 0.5, -300), rank_of(b, -0.25, -200), rank_of(c, 1.0, -100));
    news.put_ranks(&[ra.clone(), rb.clone(), rc.clone()]).expect("put_ranks");
    assert_eq!(ranked(news, &[a, b, c]), sorted_ranks(vec![ra.clone(), rb.clone(), rc.clone()]));
    assert_eq!(ranked(news, &[b, ContentRef::Story(StoryId::new())]), vec![rb.clone()]);

    // Replaced by thing, not added: one rank per thing, and the age -- a
    // clear column -- is the new one's.
    let ra_again = rank_of(a, -1.0, -50);
    news.put_ranks(std::slice::from_ref(&ra_again)).unwrap();
    assert_eq!(ranked(news, &[a]), vec![ra_again.clone()]);
    assert_eq!(ranked(news, &[a, b, c]).len(), 3);
    // Twice in one call: the later one stands.
    let (first, second) = (rank_of(b, 0.1, -10), rank_of(b, 0.9, -10));
    news.put_ranks(&[first, second.clone()]).unwrap();
    assert_eq!(ranked(news, &[b]), vec![second.clone()]);

    // Cleared by age, strictly before; the count is what went.
    assert_eq!(news.clear_ranks(Some(at(NOW - 100))).unwrap(), 0, "nothing is older than that");
    assert_eq!(news.clear_ranks(Some(at(NOW - 99))).unwrap(), 1, "only c, at -100");
    assert!(ranked(news, &[c]).is_empty());
    assert_eq!(ranked(news, &[a, b]), sorted_ranks(vec![ra_again, second]));
    assert_eq!(news.clear_ranks(None).unwrap(), 2, "every one, when no time is given");
    assert!(news.ranks(&[a, b, c]).unwrap().is_empty());
    assert_eq!(news.clear_ranks(None).unwrap(), 0);
}

fn deleting_a_source_takes_its_queue_entries_and_ranks_but_not_what_was_consumed(
    news: &dyn NewsStore,
) {
    let doomed = followed(news, "unfollowed");
    let spared = followed(news, "kept");
    let gone = new_story(&doomed, "d-1", "Going", ago(1));
    let gone_too = new_story(&doomed, "d-2", "Going too", ago(2));
    let stays = new_story(&spared, "s-1", "Staying", ago(1));
    insert(news, &[&gone, &gone_too, &stays]);
    let (gone, gone_too, stays) =
        (ContentRef::Story(gone.id), ContentRef::Story(gone_too.id), ContentRef::Story(stays.id));
    let file = ContentRef::File(FileId::new());

    let queue =
        [entry(gone, "Going", 1, 0), entry(file, "A file", 2, 0), entry(stays, "Staying", 3, 0)];
    for e in &queue {
        news.put_queue_entry(e).unwrap();
    }
    news.put_ranks(&[rank_of(gone, 0.1, 0), rank_of(gone_too, 0.2, 0), rank_of(stays, 0.3, 0)])
        .unwrap();
    let history = [consumed(gone, "Read before it went", -10), consumed(stays, "Read, kept", -20)];
    for c in &history {
        news.put_consumption(c).unwrap();
    }

    news.delete_source(doomed.id).expect("delete_source");

    assert_eq!(queued(news), [queue[1].id, queue[2].id], "its stories' entries go, nobody else's");
    assert_eq!(
        ranked(news, &[gone, gone_too, stays]).iter().map(|r| r.what).collect::<Vec<_>>(),
        vec![stays]
    );
    let found = news.consumptions(&everything()).unwrap();
    assert_eq!(found.len(), 2, "what was read is remembered after the source is gone");
    assert_eq!(news.consumption(&gone).unwrap().as_ref(), Some(&history[0]));
    assert_eq!(news.consumption(&stays).unwrap().as_ref(), Some(&history[1]));

    forget(news, &[&spared]);
    assert_eq!(queued(news), [queue[1].id], "and so the other source's, when it goes");
    assert_eq!(news.consumptions(&everything()).unwrap().len(), 2, "still never a consumption");

    news.delete_queue_entries(&[queue[1].id]).unwrap();
    news.clear_ranks(None).unwrap();
    forget_consumed(news, &history.iter().collect::<Vec<_>>());
}

fn a_list_longer_than_one_chunk_reaches_every_table(news: &dyn NewsStore) {
    // Far more things than any statement may name at once, of which a few
    // are real: the lookups and deletes must split the list without losing
    // what is in a later piece of it.
    const N: usize = 1_200;
    let things: Vec<ContentRef> = (0..N).map(|_| ContentRef::Item(ItemId::new())).collect();
    let real = [things[3], things[600], things[N - 1]];

    for (i, what) in real.iter().enumerate() {
        news.put_queue_entry(&entry(*what, "queued", i as i64, 0)).unwrap();
        news.put_consumption(&consumed(*what, "consumed", i as i64)).unwrap();
        news.put_ranks(&[rank_of(*what, 0.5, i as i64)]).unwrap();
    }
    assert_eq!(news.consumptions_for(&things).unwrap().len(), 3);
    assert_eq!(news.ranks(&things).unwrap().len(), 3);

    // Taken out by thing, from a list longer than a chunk.
    news.delete_queue_for(&things).unwrap();
    assert!(news.queue().unwrap().is_empty());

    // By entry id, too.
    for (i, what) in real.iter().enumerate() {
        news.put_queue_entry(&entry(*what, "queued", i as i64, 0)).unwrap();
    }
    let mut ids = queued(news);
    ids.extend((0..N).map(|_| QueueEntryId::new()));
    news.delete_queue_entries(&ids).unwrap();
    assert!(news.queue().unwrap().is_empty());

    let all = news.consumptions(&everything()).unwrap();
    forget_consumed(news, &all.iter().collect::<Vec<_>>());
    assert_eq!(news.clear_ranks(None).unwrap(), 3);
}
