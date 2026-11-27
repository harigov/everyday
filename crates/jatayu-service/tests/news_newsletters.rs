//! Newsletters from Mail, through the Feed's three doors into it -- a turn of
//! a mail-backed source, marking its stories read, and the list of senders to
//! follow -- against a real SQLite vault.
//!
//! What is counted and grouped is pinned where it lives
//! (`jatayu_core::mail::cleanup`'s unit tests, the store's conformance
//! suite); what is checked here is what the Feed adds on top: one story per
//! message and no more, the watermark, the outbox ops, which mail it refuses
//! to make a story of, and -- for the Newsletters source -- which source of
//! its own each newsletter's issues go to.

#[allow(dead_code)]
mod support;

use std::cell::Cell;
use std::sync::Arc;

use jatayu_core::Vault;
use jatayu_core::account::{Account, Provider};
use jatayu_core::id::{AccountId, MailMessageId, MailboxId, NewsSourceId, PackId, ThreadId};
use jatayu_core::mail::{
    Address, Category, CategorySource, Mailbox, MailboxRole, Message, MessageFlags, Op, OpKind,
    OpState, Origin,
};
use jatayu_core::news::{FetchState, NewsSource, NewsletterRef, SourceKind, Story};
use jatayu_core::packstore::PackRef;
use jatayu_core::store::mail::{IngestMessage, ThreadFilter};
use jatayu_core::store::news::StoryQuery;
use jatayu_service::Service;
use jatayu_service::ctx::Ctx;
use jatayu_service::error::codes;
use jatayu_service::news::newsletters;
use jiff::Timestamp;
use serde_json::json;

const NEWS: &str = "news@example.com";
const DIGEST: &str = "digest@example.com";

/// A vault with one mail account and the three folders these tests file
/// into. The account is saved straight into the vault rather than through
/// `save_account`, so no sync task starts for an account with no server
/// behind it.
struct World {
    svc: Arc<Service>,
    vault: Arc<Vault>,
    account: AccountId,
    inbox: MailboxId,
    archive: MailboxId,
    trash: MailboxId,
    next_uid: Cell<u32>,
    _dir: tempfile::TempDir,
}

fn world() -> World {
    let (svc, dir) = support::vault::service(None);
    let vault = svc.get().unwrap();
    let mut account = Account::new(Provider::Custom, "me@example.com");
    account.services.mail = true;
    vault.save_account(&account).unwrap();
    let mailbox = |name: &str, role| {
        let mailbox = Mailbox::new(account.id, name, role);
        vault.save_mailbox(&mailbox).unwrap();
        mailbox.id
    };
    World {
        inbox: mailbox("INBOX", MailboxRole::Inbox),
        archive: mailbox("Archive", MailboxRole::Archive),
        trash: mailbox("Trash", MailboxRole::Trash),
        account: account.id,
        svc,
        vault,
        next_uid: Cell::new(0),
        _dir: dir,
    }
}

/// One message to seed. Most tests want an issue of the newsletter they
/// follow, so [`Mail::news`] is that, and the rest override a field.
#[derive(Clone)]
struct Mail<'a> {
    from: &'a str,
    name: &'a str,
    subject: &'a str,
    /// Hours before now; at least one, so the message is inside any window
    /// that ends now.
    hours_ago: i64,
    category: Option<Category>,
    flags: MessageFlags,
    /// File it in this thread rather than one of its own.
    thread: Option<ThreadId>,
}

impl<'a> Mail<'a> {
    fn news(hours_ago: i64, subject: &'a str) -> Self {
        Self {
            from: NEWS,
            name: "The Weekly",
            subject,
            hours_ago,
            category: Some(Category::Newsletter),
            flags: MessageFlags::default(),
            thread: None,
        }
    }
}

#[derive(Clone, Copy)]
struct Seeded {
    id: MailMessageId,
    thread: ThreadId,
    date: Timestamp,
}

/// The `Message-ID` header every seeded message carries, so a test can work
/// out the key its story was stored under.
fn header(id: MailMessageId) -> String {
    format!("<{id}@news.example>")
}

impl World {
    fn seed(&self, mail: Mail) -> Seeded {
        self.seed_in(self.inbox, mail)
    }

    fn seed_in(&self, mailbox: MailboxId, mail: Mail) -> Seeded {
        let id = MailMessageId::new();
        let thread = mail.thread.unwrap_or_default();
        // Whole seconds, so a date read back out of the vault is the one
        // that went in.
        let date =
            Timestamp::from_second(Timestamp::now().as_second() - mail.hours_ago * 3600).unwrap();
        let message = Message {
            id,
            account_id: self.account,
            thread_id: thread,
            message_id_header: header(id),
            date,
            from: Address::new(mail.name, mail.from),
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: mail.subject.into(),
            snippet: format!("About {}", mail.subject),
            flags: mail.flags,
            labels: Vec::new(),
            has_attachments: false,
            size: 128,
            category: mail.category,
            category_source: CategorySource::Rules,
            pack: PackRef {
                account: self.account.to_string(),
                pack: PackId::new(),
                offset: 0,
                len: 0,
            },
            gmail: None,
            invite: None,
        };
        let uid = self.next_uid.get() + 1;
        self.next_uid.set(uid);
        self.vault
            .ingest_mail(self.account, vec![IngestMessage { message, mailbox, uid }])
            .unwrap();
        Seeded { id, thread, date }
    }

    fn follow(&self, sender: &str, keep_out_of_inbox: bool) -> NewsSource {
        let mut source = NewsSource::new(
            SourceKind::Mail { sender: sender.into(), name: String::new() },
            "The Weekly",
        );
        source.keep_out_of_inbox = keep_out_of_inbox;
        self.vault.save_news_source(&source).unwrap();
        source
    }

    /// A sender followed, under one display name.
    fn follow_named(&self, sender: &str, name: &str) -> NewsSource {
        let source = NewsSource::new(
            SourceKind::Mail { sender: sender.into(), name: name.into() },
            format!("{name} from {sender}"),
        );
        self.vault.save_news_source(&source).unwrap();
        source
    }

    /// Every newsletter at once.
    fn follow_all(&self) -> NewsSource {
        let all = NewsSource::new(SourceKind::Newsletters, "Newsletters");
        self.vault.save_news_source(&all).unwrap();
        all
    }

    /// The sources the Newsletters source made.
    fn found(&self) -> Vec<NewsSource> {
        self.vault.news_sources().unwrap().into_iter().filter(|s| s.found).collect()
    }

    /// The found source called `title`.
    fn found_named(&self, title: &str) -> NewsSource {
        let found = self.found().into_iter().find(|s| s.title == title);
        found.unwrap_or_else(|| panic!("no found source called {title:?}"))
    }

    /// Any source called `title`; the titles in a test are its own.
    fn source_named(&self, title: &str) -> NewsSource {
        let sources = self.vault.news_sources().unwrap();
        sources.into_iter().find(|s| s.title == title).unwrap_or_else(|| panic!("no {title:?}"))
    }

    fn turned_down(&self) -> Vec<(String, String)> {
        let settings = self.vault.feed_settings().unwrap();
        settings.not_newsletters.into_iter().map(|n| (n.sender, n.name)).collect()
    }

    /// One turn of `source`, as the refresher takes it.
    async fn turn(&self, source: &NewsSource, state: &mut FetchState) -> u32 {
        newsletters::refresh(&self.svc, &self.vault, source, state).await.expect("a turn")
    }

    /// The source's stories, newest first.
    fn stories(&self, source: &NewsSource) -> Vec<Story> {
        let query = StoryQuery { sources: vec![source.id], limit: 100, ..Default::default() };
        self.vault.stories(&query).unwrap().stories
    }

    fn ops(&self, thread: ThreadId) -> Vec<Op> {
        self.vault.ops_for_thread(thread, 20).unwrap()
    }
}

fn mail_ids(stories: &[Story]) -> Vec<Option<MailMessageId>> {
    stories.iter().map(|s| s.mail_message).collect()
}

#[tokio::test]
async fn a_turn_stores_one_story_per_message_and_moves_the_watermark() {
    let w = world();
    let first = w.seed(Mail::news(72, "Issue 1"));
    let second = w.seed(Mail { from: "News@Example.COM", ..Mail::news(48, "Issue 2") });
    let third = w.seed(Mail::news(24, "Issue 3"));
    let other = w.seed(Mail { from: "bob@example.com", name: "Bob", ..Mail::news(30, "Lunch?") });

    let source = w.follow(NEWS, false);
    let mut state = FetchState::new(source.id);
    assert_eq!(w.turn(&source, &mut state).await, 3);
    assert_eq!(state.watermark, Some(third.date), "the newest message of the sender's");

    let stories = w.stories(&source);
    let titles: Vec<&str> = stories.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(titles, ["Issue 3", "Issue 2", "Issue 1"], "newest first");
    assert_eq!(mail_ids(&stories), [Some(third.id), Some(second.id), Some(first.id)]);
    assert!(!mail_ids(&stories).contains(&Some(other.id)), "somebody else's mail is not ours");

    let story = &stories[0];
    assert_eq!(story.author, "The Weekly");
    assert_eq!(story.summary, "About Issue 3", "the snippet");
    assert_eq!(story.published_at, third.date);
    assert!(story.url.is_empty() && !story.has_body, "the message is the body");
    assert!(!story.read);
    assert_eq!(
        story.key,
        w.vault.news_key(&format!("mail:{}", header(third.id))).unwrap(),
        "keyed by the Message-ID, so the same issue is the same story"
    );
    assert!(
        w.vault.ops_by_origin("person", 50).unwrap().is_empty(),
        "nothing reaches the server unless a source asks for it"
    );
}

#[tokio::test]
async fn a_second_turn_stores_nothing_until_a_new_issue_arrives() {
    let w = world();
    let issue = w.seed(Mail::news(48, "Issue 1"));
    let source = w.follow(NEWS, false);
    let mut state = FetchState::new(source.id);
    assert_eq!(w.turn(&source, &mut state).await, 1);

    assert_eq!(w.turn(&source, &mut state).await, 0, "the same message is not a second story");
    assert_eq!(state.watermark, Some(issue.date));
    assert_eq!(w.stories(&source).len(), 1);

    let next = w.seed(Mail::news(1, "Issue 2"));
    assert_eq!(w.turn(&source, &mut state).await, 1);
    assert_eq!(state.watermark, Some(next.date));
    assert_eq!(w.stories(&source)[0].title, "Issue 2");
}

#[tokio::test]
async fn mail_from_somebody_else_is_ignored_and_leaves_the_watermark_alone() {
    let w = world();
    w.seed(Mail { from: "bob@example.com", name: "Bob", ..Mail::news(30, "Lunch?") });
    w.seed(Mail { from: "newsletter@example.com", ..Mail::news(20, "Not the same address") });

    let source = w.follow(NEWS, false);
    let mut state = FetchState::new(source.id);
    assert_eq!(w.turn(&source, &mut state).await, 0);
    assert_eq!(state.watermark, None);
    assert!(w.stories(&source).is_empty());
}

#[tokio::test]
async fn the_first_turn_reaches_thirty_days_back_and_no_further() {
    let w = world();
    w.seed(Mail::news(24 * 31, "Last month's"));
    w.seed(Mail::news(24 * 29, "This month's"));

    let source = w.follow(NEWS, false);
    let mut state = FetchState::new(source.id);
    assert_eq!(w.turn(&source, &mut state).await, 1);
    assert_eq!(w.stories(&source)[0].title, "This month's");
}

#[tokio::test]
async fn drafts_the_trash_and_muted_words_are_not_stories_but_still_move_the_watermark() {
    let w = world();
    let draft = MessageFlags { draft: true, ..Default::default() };
    w.seed(Mail { flags: draft, ..Mail::news(60, "A draft") });
    w.seed_in(w.trash, Mail::news(50, "Thrown away"));
    w.seed(Mail::news(40, "SPONSORED: buy a thing"));
    let kept = w.seed(Mail::news(30, "Issue 1"));
    let newest_muted = w.seed(Mail::news(20, "Our sponsors this week"));

    let mut source = NewsSource::new(
        SourceKind::Mail { sender: NEWS.into(), name: String::new() },
        "The Weekly",
    );
    source.mute = vec!["sponsor".into()];
    w.vault.save_news_source(&source).unwrap();
    let mut state = FetchState::new(source.id);

    assert_eq!(w.turn(&source, &mut state).await, 1);
    assert_eq!(mail_ids(&w.stories(&source)), [Some(kept.id)]);
    assert_eq!(state.watermark, Some(newest_muted.date), "a muted issue is not read again");
}

#[tokio::test]
async fn a_source_that_is_not_a_sender_is_refused_in_a_sentence() {
    let w = world();
    let source = NewsSource::new(SourceKind::Web { url: "https://x.example/feed".into() }, "X");
    let mut state = FetchState::new(source.id);
    let err = newsletters::refresh(&w.svc, &w.vault, &source, &mut state)
        .await
        .expect_err("a web feed is not a newsletter");
    assert!(err.contains("sender"), "{err}");
}

#[tokio::test]
async fn keeping_issues_out_of_the_inbox_archives_each_new_thread_once() {
    let w = world();
    let alone = w.seed(Mail::news(72, "Issue 1"));
    // Two issues in one thread: one archive between them.
    let pair = w.seed(Mail::news(48, "Issue 2"));
    w.seed(Mail { thread: Some(pair.thread), ..Mail::news(47, "Issue 2, again") });
    // Filed away by hand already: a story, but nothing to archive.
    let filed = w.seed_in(w.archive, Mail::news(30, "Issue 3"));
    // Thrown away: not a story at all.
    let trashed = w.seed_in(w.trash, Mail::news(20, "Issue 4"));

    let source = w.follow(NEWS, true);
    let mut state = FetchState::new(source.id);
    assert_eq!(w.turn(&source, &mut state).await, 4);

    for thread in [alone.thread, pair.thread] {
        let ops = w.ops(thread);
        assert_eq!(ops.len(), 1, "one archive for the thread: {ops:?}");
        assert_eq!(ops[0].kind, OpKind::Archive);
        assert_eq!(ops[0].state, OpState::Pending);
        assert_eq!(ops[0].origin, Origin::Person);
        assert_eq!(ops[0].account_id, w.account);
    }
    assert!(w.ops(filed.thread).is_empty(), "already out of the inbox");
    assert!(w.ops(trashed.thread).is_empty(), "never a story");

    let page = w.vault.list_threads(w.inbox, &ThreadFilter::default(), None, 50).unwrap();
    let still_there: Vec<ThreadId> = page.threads.iter().map(|t| t.id).collect();
    assert!(
        !still_there.contains(&alone.thread) && !still_there.contains(&pair.thread),
        "the inbox no longer lists them: {still_there:?}"
    );

    assert_eq!(w.turn(&source, &mut state).await, 0);
    assert_eq!(w.ops(alone.thread).len(), 1, "an issue is archived when it arrives, not again");
    assert_eq!(w.ops(pair.thread).len(), 1);
}

#[tokio::test]
async fn a_late_issue_is_archived_on_the_turn_that_finds_it() {
    let w = world();
    w.seed(Mail::news(72, "Issue 1"));
    let source = w.follow(NEWS, true);
    let mut state = FetchState::new(source.id);
    assert_eq!(w.turn(&source, &mut state).await, 1);

    let next = w.seed(Mail::news(1, "Issue 2"));
    assert_eq!(w.turn(&source, &mut state).await, 1);
    assert_eq!(w.ops(next.thread).len(), 1);
    assert_eq!(w.ops(next.thread)[0].kind, OpKind::Archive);
}

#[tokio::test]
async fn marking_stories_read_marks_their_messages_and_queues_the_ops() {
    let w = world();
    let older = w.seed(Mail::news(48, "Issue 1"));
    let newer = w.seed(Mail::news(24, "Issue 2"));
    let seen = MessageFlags { seen: true, ..Default::default() };
    let already = w.seed(Mail { flags: seen, ..Mail::news(12, "Issue 3") });

    let source = w.follow(NEWS, false);
    let mut state = FetchState::new(source.id);
    assert_eq!(w.turn(&source, &mut state).await, 3);
    let stories = w.stories(&source);
    let read: Vec<bool> = stories.iter().map(|s| s.read).collect();
    assert_eq!(read, [true, false, false], "a story is as read as its message: {stories:?}");

    // A story that is not mail's -- a web feed's -- has nothing to mark.
    let web = Story::new(source.id, "k", "A web story", Timestamp::now());
    let mut asked = stories.clone();
    asked.push(web);
    newsletters::mark_read(&w.svc, &w.vault, &asked, true).await.unwrap();

    for message in [older, newer, already] {
        assert!(w.vault.mail_message(message.id).unwrap().flags.seen);
    }
    for message in [older, newer] {
        let ops = w.ops(message.thread);
        assert_eq!(ops.len(), 1, "{ops:?}");
        assert_eq!(ops[0].kind, OpKind::MarkRead);
        assert_eq!(ops[0].origin, Origin::Person);
    }
    assert!(w.ops(already.thread).is_empty(), "already read: nothing for the server to learn");
    assert!(w.stories(&source).iter().all(|s| s.read), "the story follows the message");

    newsletters::mark_read(&w.svc, &w.vault, &asked, true).await.unwrap();
    assert_eq!(w.ops(older.thread).len(), 1, "a second press queues nothing");

    let newest = stories.iter().find(|s| s.mail_message == Some(newer.id)).unwrap();
    newsletters::mark_read(&w.svc, &w.vault, std::slice::from_ref(newest), false).await.unwrap();
    assert!(w.vault.mail_message(newer.id).unwrap().flags.unread());
    assert!(w.vault.mail_message(older.id).unwrap().flags.seen, "only the one asked for");
    let ops = w.ops(newer.thread);
    assert_eq!(ops.len(), 2, "{ops:?}");
    assert!(ops.iter().any(|op| op.kind == OpKind::MarkUnread));
}

#[tokio::test]
async fn marking_a_story_whose_message_has_gone_is_not_an_error() {
    let w = world();
    let mut story = Story::new(NewsSourceId::new(), "k", "Gone", Timestamp::now());
    story.mail_message = Some(MailMessageId::new());
    newsletters::mark_read(&w.svc, &w.vault, &[story], true).await.unwrap();
    assert!(w.vault.ops_by_origin("person", 50).unwrap().is_empty());
}

#[tokio::test]
async fn senders_lists_the_newsletters_in_the_inbox_and_flags_the_followed() {
    let w = world();
    for hours in [24, 48, 72] {
        w.seed(Mail::news(hours, "Issue"));
    }
    for hours in [30, 60] {
        w.seed(Mail { from: DIGEST, name: "The Digest", ..Mail::news(hours, "Digest") });
    }
    // What is left out: not a newsletter, from you, outside the window, and
    // already out of the inbox.
    w.seed(Mail {
        from: "bob@example.com",
        category: Some(Category::Other),
        ..Mail::news(10, "?")
    });
    w.seed(Mail { from: "me@example.com", ..Mail::news(10, "Note to self") });
    w.seed(Mail::news(24 * 40, "Old issue"));
    w.seed_in(w.archive, Mail::news(5, "Filed away"));

    let summary = |found: &[newsletters::NewsletterSender]| -> Vec<(String, u32, bool)> {
        found.iter().map(|s| (s.email.clone(), s.messages, s.followed)).collect()
    };
    let found = newsletters::senders(&w.svc, &w.vault, 30).unwrap();
    assert_eq!(
        summary(&found),
        [(NEWS.to_string(), 3, false), (DIGEST.to_string(), 2, false)],
        "most messages first"
    );
    assert_eq!(found[0].name, "The Weekly");
    assert_eq!(found[1].name, "The Digest");

    // Followed once a source exists -- however its address was cased.
    w.follow("Digest@Example.com", false);
    let found = newsletters::senders(&w.svc, &w.vault, 30).unwrap();
    assert_eq!(summary(&found), [(NEWS.to_string(), 3, false), (DIGEST.to_string(), 2, true)]);

    // A wider window takes in the old issue.
    let wider = newsletters::senders(&w.svc, &w.vault, 90).unwrap();
    assert_eq!(wider[0].messages, 4);
}

/// Counted as Quick Cleanup counts: what is in the Inbox, not whatever else the
/// threads it is in hold. The rest could be archived, or in Trash.
#[tokio::test]
async fn senders_counts_what_is_in_the_inbox_not_the_rest_of_its_threads() {
    let w = world();
    let filed =
        w.seed_in(w.archive, Mail { from: DIGEST, name: "The Digest", ..Mail::news(30, "Filed") });
    let also_filed = w.seed_in(w.archive, Mail::news(20, "Also filed"));
    w.seed(Mail { thread: Some(filed.thread), ..Mail::news(10, "Reply") });
    w.seed(Mail { thread: Some(also_filed.thread), ..Mail::news(5, "Reply") });

    let found = newsletters::senders(&w.svc, &w.vault, 30).unwrap();
    assert_eq!(found.len(), 1, "the sender with nothing left in the Inbox has no row");
    assert_eq!((found[0].email.as_str(), found[0].messages), (NEWS, 2));
}

#[tokio::test]
async fn senders_is_empty_when_nothing_is_in_the_inbox() {
    let w = world();
    w.seed_in(w.archive, Mail::news(5, "Filed away"));
    assert!(newsletters::senders(&w.svc, &w.vault, 90).unwrap().is_empty());
}

#[tokio::test]
async fn the_newsletter_senders_command_defaults_to_ninety_days_and_refuses_nonsense() {
    let w = world();
    w.seed(Mail::news(24 * 40, "Old issue"));
    w.seed(Mail::news(24, "New issue"));

    let all = w.svc.call(Ctx::local(), "newsletter_senders", json!({})).await.unwrap();
    assert_eq!(
        all,
        json!([{ "email": NEWS, "name": "The Weekly", "messages": 2, "followed": false }])
    );
    let month = w.svc.call(Ctx::local(), "newsletter_senders", json!({ "days": 30 })).await;
    assert_eq!(month.unwrap()[0]["messages"], 1);
    let null = w.svc.call(Ctx::local(), "newsletter_senders", json!({ "days": null })).await;
    assert_eq!(null.unwrap()[0]["messages"], 2, "null is the default, not zero");

    for days in [0, 366] {
        let err = w
            .svc
            .call(Ctx::local(), "newsletter_senders", json!({ "days": days }))
            .await
            .expect_err("outside one day to a year");
        assert_eq!(err.code, codes::INVALID, "{days}: {err:?}");
    }
}

#[tokio::test]
async fn following_a_sender_files_their_mail_as_newsletters_unless_they_were_pinned() {
    let w = world();
    let filed = w.seed(Mail { category: Some(Category::Other), ..Mail::news(2, "An issue") });

    w.svc
        .call(
            Ctx::local(),
            "add_news_source",
            json!({ "source": { "type": "mail", "sender": NEWS } }),
        )
        .await
        .unwrap();
    let rules = w.vault.category_rules(w.account).unwrap();
    assert_eq!(rules.for_sender(NEWS), Some(Category::Newsletter));
    let (thread, _) = w.vault.thread(filed.thread).unwrap();
    assert_eq!(thread.category, Some(Category::Newsletter), "the mail already there moves too");

    // A sender somebody pinned to Priority stays there: following them in the
    // Feed is not a reason to argue with a choice they made in Mail.
    w.vault.correct_mail_category(w.account, DIGEST, Category::Priority).unwrap();
    w.svc
        .call(
            Ctx::local(),
            "add_news_source",
            json!({ "source": { "type": "mail", "sender": DIGEST } }),
        )
        .await
        .unwrap();
    let rules = w.vault.category_rules(w.account).unwrap();
    assert_eq!(rules.for_sender(DIGEST), Some(Category::Priority));
}

/// Following every newsletter at once: whatever Mail files under Newsletters,
/// from anybody, but not a sender followed as a source of their own, nor mail
/// that is not a newsletter, nor what sits in Trash. Each lands under a source
/// of its own, found for the purpose.
#[tokio::test]
async fn following_all_newsletters_takes_every_newsletter_but_a_followed_senders() {
    let w = world();
    let weekly = w.seed(Mail::news(48, "Issue 1"));
    let digest = w.seed(Mail { from: DIGEST, name: "The Digest", ..Mail::news(30, "Digest 9") });
    let followed = w.seed(Mail { from: "own@example.com", ..Mail::news(20, "Theirs") });
    let lunch = w.seed(Mail {
        from: "bob@example.com",
        category: Some(Category::Important),
        ..Mail::news(10, "Lunch?")
    });
    let thrown = w.seed_in(w.trash, Mail::news(5, "Binned"));

    let own = w.follow("own@example.com", false);
    let all = w.follow_all();
    let mut state = FetchState::new(all.id);
    assert_eq!(w.turn(&all, &mut state).await, 2);

    assert!(w.stories(&all).is_empty(), "nothing is kept under it");
    let found = w.found();
    assert_eq!(found.len(), 2, "{found:?}");
    let weekly_source = w.found_named("The Weekly");
    let digest_source = w.found_named("The Digest");
    assert_eq!(mail_ids(&w.stories(&weekly_source)), [Some(weekly.id)]);
    assert_eq!(mail_ids(&w.stories(&digest_source)), [Some(digest.id)]);
    assert_eq!(w.stories(&digest_source)[0].author, "The Digest", "each issue says who it is from");
    assert!(w.stories(&own).is_empty(), "a followed sender's mail waits for its own turn");
    for left in [followed.id, lunch.id, thrown.id] {
        assert!(w.vault.stories_for_mail(&[left]).unwrap().is_empty());
    }
}

#[tokio::test]
async fn all_newsletters_is_followed_once() {
    let w = world();
    let add = json!({ "source": { "type": "newsletters" } });
    let added = w.svc.call(Ctx::local(), "add_news_source", add.clone()).await.unwrap();
    assert_eq!(added["kind"]["type"], "newsletters");
    assert_eq!(added["title"], "Newsletters");
    let again = w.svc.call(Ctx::local(), "add_news_source", add).await.unwrap_err();
    assert_eq!(again.code, codes::INVALID, "{again:?}");
}

/// A message filed under Newsletters after a newer one was already turned
/// into a story -- sorted late, or moved there by hand -- is still found.
#[tokio::test]
async fn a_newsletter_sorted_late_is_still_found() {
    let w = world();
    let late = w.seed(Mail { category: None, ..Mail::news(10, "Sorted late") });
    let early =
        w.seed(Mail { from: DIGEST, name: "The Digest", ..Mail::news(5, "Sorted at once") });
    let all = w.follow_all();
    let mut state = FetchState::new(all.id);
    assert_eq!(w.turn(&all, &mut state).await, 1);
    assert_eq!(state.watermark, Some(early.date), "past the one not yet sorted");

    w.vault.set_mail_message_category(late.id, Category::Newsletter).unwrap();
    assert_eq!(w.turn(&all, &mut state).await, 1, "found behind the watermark");
    assert_eq!(w.turn(&all, &mut state).await, 0, "and only once");
    assert_eq!(mail_ids(&w.stories(&w.found_named("The Digest"))), [Some(early.id)]);
    assert_eq!(mail_ids(&w.stories(&w.found_named("The Weekly"))), [Some(late.id)]);
}

fn kind(sender: &str, name: &str) -> SourceKind {
    SourceKind::Mail { sender: sender.into(), name: name.into() }
}

/// What a newsletter is: the display name on its messages, not their address.
/// The Download and The Algorithm come from one sender and are two sources;
/// a name that differs only in case is the same one; and a sender with no
/// display name is called by its address.
#[tokio::test]
async fn two_display_names_from_one_sender_are_two_found_sources() {
    let w = world();
    let first = w.seed(Mail { name: "The Download", ..Mail::news(72, "Download 1") });
    let second = w.seed(Mail { name: "The Algorithm", ..Mail::news(60, "Algorithm 1") });
    let third = w.seed(Mail { name: " the download ", ..Mail::news(48, "Download 2") });
    let bare = w.seed(Mail { from: DIGEST, name: "", ..Mail::news(30, "Digest 1") });

    let all = w.follow_all();
    let mut state = FetchState::new(all.id);
    assert_eq!(w.turn(&all, &mut state).await, 4);
    assert_eq!(state.watermark, Some(bare.date));
    assert!(w.stories(&all).is_empty(), "the Newsletters source keeps nothing itself");

    let found = w.found();
    assert_eq!(found.len(), 3, "{found:?}");
    let download = w.found_named("The Download");
    assert_eq!(download.kind, kind(NEWS, "The Download"), "named for the first display name");
    assert_eq!(w.found_named("The Algorithm").kind, kind(NEWS, "The Algorithm"));
    assert_eq!(w.found_named(DIGEST).kind, kind(DIGEST, ""), "no name: called by its address");
    assert!(found.iter().all(|s| !s.paused && s.retention.is_none() && !s.keep_out_of_inbox));

    assert_eq!(mail_ids(&w.stories(&download)), [Some(third.id), Some(first.id)]);
    assert!(w.stories(&download).iter().all(|s| s.source_id == download.id));
    assert_eq!(mail_ids(&w.stories(&w.found_named("The Algorithm"))), [Some(second.id)]);
    assert_eq!(mail_ids(&w.stories(&w.found_named(DIGEST))), [Some(bare.id)]);

    // The next issue of either goes where the last did, and nothing new is
    // made for it.
    let next = w.seed(Mail { name: "THE ALGORITHM", ..Mail::news(1, "Algorithm 2") });
    assert_eq!(w.turn(&all, &mut state).await, 1);
    assert_eq!(w.found().len(), 3);
    assert_eq!(w.stories(&w.found_named("The Algorithm"))[0].mail_message, Some(next.id));
    assert_eq!(w.turn(&all, &mut state).await, 0, "and only once");
}

/// A sender's first issue with no display name is found under the address,
/// and that source does not take what the sender later sends under a name:
/// each name is found as a newsletter of its own.
#[tokio::test]
async fn a_nameless_found_newsletter_does_not_take_the_senders_named_ones() {
    let w = world();
    let bare = w.seed(Mail { from: DIGEST, name: "", ..Mail::news(50, "Digest 1") });
    let all = w.follow_all();
    let mut state = FetchState::new(all.id);
    assert_eq!(w.turn(&all, &mut state).await, 1);
    assert_eq!(w.found_named(DIGEST).kind, kind(DIGEST, ""));

    let named = w.seed(Mail { from: DIGEST, name: "The Digest", ..Mail::news(10, "Digest 2") });
    let again = w.seed(Mail { from: DIGEST, name: "", ..Mail::news(5, "Digest 3") });
    assert_eq!(w.turn(&all, &mut state).await, 2);
    assert_eq!(w.found().len(), 2, "{:?}", w.found());
    assert_eq!(mail_ids(&w.stories(&w.found_named("The Digest"))), [Some(named.id)]);
    assert_eq!(mail_ids(&w.stories(&w.found_named(DIGEST))), [Some(again.id), Some(bare.id)]);
}

/// A followed sender's source takes everything from the sender, or, with a
/// name, only that name -- whatever Mail files it under -- on its own turn,
/// and the Newsletters turn does not make a second story of it.
#[tokio::test]
async fn a_followed_senders_source_claims_its_mail_and_the_newsletters_source_skips_it() {
    let w = world();
    let download = w.seed(Mail { name: "The Download", ..Mail::news(50, "Download 1") });
    let algorithm = w.seed(Mail { name: "The Algorithm", ..Mail::news(40, "Algorithm 1") });
    let unfiled =
        w.seed(Mail { name: "The Download", category: None, ..Mail::news(30, "Download 2") });
    let digest = w.seed(Mail { from: DIGEST, name: "The Digest", ..Mail::news(20, "Digest 1") });
    let extra = w.seed(Mail { from: DIGEST, name: "Digest Extra", ..Mail::news(10, "Extra 1") });

    let weekly = w.follow(NEWS, false);
    let digests = w.follow_named(DIGEST, "the digest");
    let all = w.follow_all();
    let mut state = FetchState::new(all.id);
    assert_eq!(w.turn(&all, &mut state).await, 1, "only what nobody followed");
    assert_eq!(state.watermark, Some(extra.date));

    let found = w.found();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, kind(DIGEST, "Digest Extra"));
    assert_eq!(mail_ids(&w.stories(&found[0])), [Some(extra.id)]);
    assert!(w.stories(&weekly).is_empty() && w.stories(&digests).is_empty());

    // Their own turns take it all: every message of the sender's, and for
    // the named one, only that name's.
    let mut weekly_state = FetchState::new(weekly.id);
    assert_eq!(w.turn(&weekly, &mut weekly_state).await, 3);
    assert_eq!(
        mail_ids(&w.stories(&weekly)),
        [Some(unfiled.id), Some(algorithm.id), Some(download.id)]
    );
    let mut digests_state = FetchState::new(digests.id);
    assert_eq!(w.turn(&digests, &mut digests_state).await, 1);
    assert_eq!(mail_ids(&w.stories(&digests)), [Some(digest.id)]);

    // And the Newsletters turn still leaves them alone.
    assert_eq!(w.turn(&all, &mut state).await, 0);
    assert_eq!(w.found().len(), 1);
}

/// "Not a newsletter" is remembered: the sender and name on the list are not
/// made a source of again, however the case falls, and what they send still
/// moves the watermark so it is not read again every turn. A sender turned
/// down with no name is only their nameless mail.
#[tokio::test]
async fn a_newsletter_turned_down_is_not_found_again() {
    let w = world();
    let mut settings = w.vault.feed_settings().unwrap();
    settings.not_newsletters = vec![
        NewsletterRef { sender: NEWS.into(), name: "The Algorithm".into() },
        NewsletterRef { sender: DIGEST.into(), name: String::new() },
    ];
    w.vault.save_feed_settings(&settings).unwrap();

    let kept = w.seed(Mail { name: "The Download", ..Mail::news(50, "Download 1") });
    w.seed(Mail { name: "the ALGORITHM", ..Mail::news(40, "Algorithm 1") });
    let named = w.seed(Mail { from: DIGEST, name: "The Digest", ..Mail::news(30, "Digest 1") });
    let newest = w.seed(Mail { from: DIGEST, name: "", ..Mail::news(20, "Digest 2") });

    let all = w.follow_all();
    let mut state = FetchState::new(all.id);
    assert_eq!(w.turn(&all, &mut state).await, 2);
    assert_eq!(state.watermark, Some(newest.date), "what was turned down is still read past");

    let found = w.found();
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(mail_ids(&w.stories(&w.found_named("The Download"))), [Some(kept.id)]);
    assert_eq!(w.found_named("The Digest").kind, kind(DIGEST, "The Digest"));
    assert_eq!(mail_ids(&w.stories(&w.found_named("The Digest"))), [Some(named.id)]);
    assert_eq!(w.turn(&all, &mut state).await, 0);
    assert_eq!(w.found().len(), 2);
}

/// What an earlier version stored under the Newsletters source is handed to
/// the source each message belongs to -- a followed one, when there is one --
/// carrying what was done to it, and not again.
#[tokio::test]
async fn stories_already_under_the_newsletters_source_are_given_to_their_own_sources() {
    let w = world();
    let download = w.seed(Mail { name: "The Download", ..Mail::news(72, "Download 1") });
    let again = w.seed(Mail { name: "The Download", ..Mail::news(48, "Download 2") });
    let algorithm = w.seed(Mail { name: "The Algorithm", ..Mail::news(40, "Algorithm 1") });
    let digest = w.seed(Mail { from: DIGEST, name: "The Digest", ..Mail::news(30, "Digest 1") });

    let all = w.follow_all();
    let digests = w.follow(DIGEST, false);
    let stored: Vec<Story> = [&download, &again, &algorithm, &digest]
        .iter()
        .map(|m| {
            let key = w.vault.news_key(&format!("mail:{}", header(m.id))).unwrap();
            let mut story = Story::new(all.id, key, "An issue", m.date);
            story.mail_message = Some(m.id);
            story
        })
        .collect();
    let batch: Vec<_> = stored.iter().map(|s| (s.clone(), None)).collect();
    assert_eq!(w.vault.insert_stories(&batch).unwrap().len(), 4);
    w.vault.set_stories_saved(&[stored[0].id], true).unwrap();
    assert_eq!(w.stories(&all).len(), 4);

    let mut state = FetchState::new(all.id);
    assert_eq!(w.turn(&all, &mut state).await, 0, "no mail is new: the stories were there");
    assert!(w.stories(&all).is_empty(), "nothing is left under it");

    let download_source = w.found_named("The Download");
    let ids: Vec<_> = w.stories(&download_source).iter().map(|s| s.id).collect();
    assert_eq!(ids, [stored[1].id, stored[0].id]);
    assert!(w.stories(&download_source)[1].saved, "what was done to a story goes with it");
    assert!(w.stories(&download_source).iter().all(|s| s.source_id == download_source.id));
    assert_eq!(mail_ids(&w.stories(&w.found_named("The Algorithm"))), [Some(algorithm.id)]);
    assert_eq!(w.stories(&digests)[0].id, stored[3].id, "a followed sender's source is a home too");
    assert_eq!(w.found().len(), 2, "one found for each that had no source");

    // Idempotent.
    assert_eq!(w.turn(&all, &mut state).await, 0);
    assert_eq!(w.found().len(), 2);
    assert_eq!(w.stories(&download_source).len(), 2);
}

/// Unfollowing a found newsletter says it is not one; following the sender by
/// hand takes that back. A source somebody followed leaves no mark.
#[tokio::test]
async fn deleting_a_found_source_turns_it_down_and_following_by_hand_takes_it_back() {
    let w = world();
    w.seed(Mail { name: "The Download", ..Mail::news(50, "Download 1") });
    w.seed(Mail { name: "The Algorithm", ..Mail::news(40, "Algorithm 1") });
    let all = w.follow_all();
    let mut state = FetchState::new(all.id);
    assert_eq!(w.turn(&all, &mut state).await, 2);
    assert!(w.turned_down().is_empty());

    let algorithm = w.found_named("The Algorithm");
    w.svc
        .call(Ctx::local(), "delete_news_source", json!({ "id": algorithm.id.to_string() }))
        .await
        .unwrap();
    assert_eq!(w.turned_down(), [(NEWS.to_string(), "The Algorithm".to_string())]);
    assert_eq!(w.found().len(), 1, "the other is untouched");

    // Its mail is still there, and the next turn does not find it again.
    w.seed(Mail { name: "The Algorithm", ..Mail::news(1, "Algorithm 2") });
    assert_eq!(w.turn(&all, &mut state).await, 0);
    assert_eq!(w.found().len(), 1);
    assert_eq!(w.turned_down().len(), 1, "listed once");

    // Deleted a second time, or as a source somebody followed: nothing to add.
    w.svc
        .call(Ctx::local(), "delete_news_source", json!({ "id": algorithm.id.to_string() }))
        .await
        .unwrap();
    assert_eq!(w.turned_down().len(), 1);

    let added = w
        .svc
        .call(
            Ctx::local(),
            "add_news_source",
            json!({ "source": { "type": "mail", "sender": NEWS } }),
        )
        .await
        .unwrap();
    assert!(w.turned_down().is_empty(), "followed by hand: it is a newsletter after all");
    assert!(added.get("found").is_none(), "a person's source is not found");

    let followed = w.source_named(NEWS);
    w.svc
        .call(Ctx::local(), "delete_news_source", json!({ "id": followed.id.to_string() }))
        .await
        .unwrap();
    assert!(w.turned_down().is_empty(), "unfollowing what you followed says nothing");
}

/// Following by hand a sender the Newsletters source found under its address
/// makes that source the person's -- the same source, no longer found -- not
/// a refusal as followed already.
#[tokio::test]
async fn following_a_found_senders_address_by_hand_makes_it_followed() {
    let w = world();
    w.seed(Mail { from: DIGEST, name: "", ..Mail::news(30, "Digest 1") });
    let all = w.follow_all();
    let mut state = FetchState::new(all.id);
    assert_eq!(w.turn(&all, &mut state).await, 1);
    let found = w.found_named(DIGEST);

    let added = w
        .svc
        .call(
            Ctx::local(),
            "add_news_source",
            json!({ "source": { "type": "mail", "sender": DIGEST, "title": "The Digest" } }),
        )
        .await
        .unwrap();
    assert_eq!(added["id"], json!(found.id.to_string()), "the same source");
    assert!(added.get("found").is_none(), "and the person's now");
    assert_eq!(added["title"], json!("The Digest"));
    assert!(w.found().is_empty());
}

/// Refreshing a found newsletter is refreshing where its issues come from:
/// the Newsletters source, or -- with none -- the found source itself, over
/// its own mail and no other's. The sidebar's summary of it takes the
/// Newsletters source's health.
#[tokio::test]
async fn a_found_source_is_refreshed_through_the_newsletters_source() {
    let w = world();
    w.seed(Mail { name: "The Download", ..Mail::news(50, "Download 1") });
    w.seed(Mail { name: "The Algorithm", ..Mail::news(40, "Algorithm 1") });
    let all = w.follow_all();
    let report = |reports: &serde_json::Value| {
        (reports[0]["sourceId"].clone(), reports[0]["inserted"].clone())
    };

    let reports = w
        .svc
        .call(Ctx::local(), "refresh_news", json!({ "source": all.id.to_string() }))
        .await
        .unwrap();
    assert_eq!(report(&reports), (json!(all.id.to_string()), json!(2)));
    let download = w.found_named("The Download");
    let algorithm = w.found_named("The Algorithm");

    let fresh = w.seed(Mail { name: "The Algorithm", ..Mail::news(1, "Algorithm 2") });
    let reports = w
        .svc
        .call(Ctx::local(), "refresh_news", json!({ "source": download.id.to_string() }))
        .await
        .unwrap();
    assert_eq!(reports.as_array().unwrap().len(), 1);
    assert_eq!(report(&reports), (json!(download.id.to_string()), json!(1)), "answers the ask");
    assert_eq!(w.stories(&algorithm)[0].mail_message, Some(fresh.id), "the whole turn was taken");

    // Every refresh at once is one turn of the Newsletters source, not one
    // for each newsletter it found.
    w.seed(Mail { name: "The Algorithm", ..Mail::news(1, "Algorithm 3") });
    let reports = w.svc.call(Ctx::local(), "refresh_news", json!({})).await.unwrap();
    assert_eq!(reports.as_array().unwrap().len(), 1, "{reports}");

    let listed = w.svc.call(Ctx::local(), "list_news_sources", json!({})).await.unwrap();
    for summary in listed.as_array().unwrap() {
        assert!(summary["lastOk"].is_string(), "{summary}");
        assert!(summary["nextAt"].is_string(), "{summary}");
    }

    // With no Newsletters source to ask, a found one takes its own turn, over
    // what Mail files as newsletters from its sender and name.
    w.svc
        .call(Ctx::local(), "delete_news_source", json!({ "id": all.id.to_string() }))
        .await
        .unwrap();
    let mine = w.seed(Mail { name: "The Download", ..Mail::news(1, "Download 2") });
    w.seed(Mail { name: "The Algorithm", ..Mail::news(1, "Algorithm 4") });
    w.seed(Mail { name: "The Download", category: None, ..Mail::news(1, "Not a newsletter") });
    let reports = w
        .svc
        .call(Ctx::local(), "refresh_news", json!({ "source": download.id.to_string() }))
        .await
        .unwrap();
    assert_eq!(report(&reports), (json!(download.id.to_string()), json!(1)));
    assert_eq!(w.stories(&download)[0].mail_message, Some(mine.id));
    assert_eq!(w.stories(&algorithm).len(), 3, "somebody else's issue waits for its own turn");
}

/// Whether a source was found is the stored copy's to say: a person cannot
/// make one found, and a found one stays so.
#[tokio::test]
async fn saving_a_source_never_changes_whether_it_was_found() {
    let w = world();
    w.seed(Mail { name: "The Download", ..Mail::news(50, "Download 1") });
    let all = w.follow_all();
    let mut state = FetchState::new(all.id);
    assert_eq!(w.turn(&all, &mut state).await, 1);

    let mut download = w.found_named("The Download");
    download.found = false;
    download.title = "Download, renamed".into();
    w.svc.call(Ctx::local(), "save_news_source", json!({ "source": download })).await.unwrap();
    let stored = w.vault.news_source(download.id).unwrap();
    assert_eq!(stored.title, "Download, renamed", "the rest is theirs to change");
    assert!(stored.found, "found until it is deleted");

    let mut followed = w.follow(NEWS, false);
    followed.found = true;
    w.svc.call(Ctx::local(), "save_news_source", json!({ "source": followed })).await.unwrap();
    assert!(!w.vault.news_source(followed.id).unwrap().found, "nobody can say it is");
}
