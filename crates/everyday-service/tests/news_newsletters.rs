//! Newsletters from Mail, through the Feed's three doors into it -- a turn of
//! a mail-backed source, marking its stories read, and the list of senders to
//! follow -- against a real SQLite vault.
//!
//! What is counted and grouped is pinned where it lives
//! (`everyday_core::mail::cleanup`'s unit tests, the store's conformance
//! suite); what is checked here is what the Feed adds on top: one story per
//! message and no more, the watermark, the outbox ops, and which mail it
//! refuses to make a story of.

#[allow(dead_code)]
mod support;

use std::cell::Cell;
use std::sync::Arc;

use everyday_core::Vault;
use everyday_core::account::{Account, Provider};
use everyday_core::id::{AccountId, MailMessageId, MailboxId, NewsSourceId, PackId, ThreadId};
use everyday_core::mail::{
    Address, Category, CategorySource, Mailbox, MailboxRole, Message, MessageFlags, Op, OpKind,
    OpState, Origin,
};
use everyday_core::news::{FetchState, NewsSource, SourceKind, Story};
use everyday_core::packstore::PackRef;
use everyday_core::store::mail::{IngestMessage, ThreadFilter};
use everyday_core::store::news::StoryQuery;
use everyday_service::Service;
use everyday_service::ctx::Ctx;
use everyday_service::error::codes;
use everyday_service::news::newsletters;
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
        let mut source = NewsSource::new(SourceKind::Mail { sender: sender.into() }, "The Weekly");
        source.keep_out_of_inbox = keep_out_of_inbox;
        self.vault.save_news_source(&source).unwrap();
        source
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

    let mut source = NewsSource::new(SourceKind::Mail { sender: NEWS.into() }, "The Weekly");
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
