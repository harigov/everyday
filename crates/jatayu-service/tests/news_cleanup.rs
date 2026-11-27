//! Cleaning up the Feed after so many days, against a real SQLite vault: what
//! each kind's default is worth, which newsletter issues an expiry pass says
//! have gone with their email to follow, and what moving that email to Trash
//! leaves where it is.
//!
//! The store's own rule for what expires is pinned in its conformance suite;
//! what is checked here is what the vault adds to it -- the defaults handed
//! down, and the list of messages worked out beside it -- and what
//! `news::cleanup` does with that list.

#[allow(dead_code)]
mod support;

use std::cell::Cell;
use std::sync::Arc;

use jatayu_core::account::{Account, Provider};
use jatayu_core::id::{AccountId, FileId, MailMessageId, MailboxId, PackId, ThreadId};
use jatayu_core::mail::{
    Address, Category, CategorySource, Mailbox, MailboxRole, Message, MessageFlags, Op, OpKind,
    Origin,
};
use jatayu_core::news::{
    ContentRef, ExpireReport, FeedSettings, NewsSource, NewsletterRef, QueueEntry, Retention,
    SourceKind, Story,
};
use jatayu_core::packstore::PackRef;
use jatayu_core::store::mail::IngestMessage;
use jatayu_core::{Error, QueuePlace, Vault};
use jatayu_service::Service;
use jatayu_service::news::cleanup;
use jiff::{SignedDuration, Timestamp};

const NEWS: &str = "news@example.com";

/// A vault with one mail account and the folders these tests file into. The
/// account is saved straight into the vault rather than through
/// `save_account`, so no sync task starts for an account with no server
/// behind it.
struct World {
    svc: Arc<Service>,
    vault: Arc<Vault>,
    account: AccountId,
    inbox: MailboxId,
    sent: MailboxId,
    trash: MailboxId,
    next_uid: Cell<u32>,
    now: Timestamp,
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
        sent: mailbox("Sent", MailboxRole::Sent),
        trash: mailbox("Trash", MailboxRole::Trash),
        account: account.id,
        svc,
        vault,
        next_uid: Cell::new(0),
        now: Timestamp::from_second(Timestamp::now().as_second()).unwrap(),
        _dir: dir,
    }
}

#[derive(Clone, Copy)]
struct Seeded {
    id: MailMessageId,
    thread: ThreadId,
}

impl World {
    /// One message in `mailbox`, in `thread` if given, else a thread of its
    /// own. It carries `flags` and is from the newsletter's sender.
    fn message(&self, mailbox: MailboxId, thread: Option<ThreadId>, flags: MessageFlags) -> Seeded {
        let id = MailMessageId::new();
        let thread = thread.unwrap_or_default();
        let message = Message {
            id,
            account_id: self.account,
            thread_id: thread,
            message_id_header: format!("<{id}@news.example>"),
            date: self.now,
            from: Address::new("The Weekly", NEWS),
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: "An issue".into(),
            snippet: "About an issue".into(),
            flags,
            labels: Vec::new(),
            has_attachments: false,
            size: 128,
            category: Some(Category::Newsletter),
            category_source: CategorySource::Rules,
            pack: PackRef {
                account: self.account.to_string(),
                pack: PackId::new(),
                offset: 0,
                len: 0,
            },
            gmail: None,
            invite: None,
            unsubscribe: None,
        };
        let uid = self.next_uid.get() + 1;
        self.next_uid.set(uid);
        self.vault
            .ingest_mail(self.account, vec![IngestMessage { message, mailbox, uid }])
            .unwrap();
        Seeded { id, thread }
    }

    /// An unread issue in the Inbox.
    fn issue(&self) -> Seeded {
        self.message(self.inbox, None, MessageFlags::default())
    }

    /// A newsletter followed from its sender, which bins its email when it
    /// cleans up if `trash_mail`.
    fn follow(&self, title: &str, trash_mail: bool) -> NewsSource {
        let mut source =
            NewsSource::new(SourceKind::Mail { sender: NEWS.into(), name: String::new() }, title);
        source.trash_mail = trash_mail;
        self.vault.save_news_source(&source).unwrap();
        source
    }

    fn save(&self, source: &NewsSource) {
        self.vault.save_news_source(source).unwrap();
    }

    /// A story of `source` published `days` days ago, pointing at `mail`.
    fn story(&self, source: &NewsSource, mail: Option<Seeded>, days: i64) -> Story {
        let key = format!("k-{}", MailMessageId::new());
        let published = self.now - SignedDuration::from_hours(days * 24);
        let mut story = Story::new(source.id, self.vault.news_key(&key).unwrap(), &key, published);
        story.mail_message = mail.map(|m| m.id);
        self.vault.insert_stories(&[(story.clone(), None)]).unwrap();
        story
    }

    fn expired(&self, story: &Story) -> bool {
        self.vault.story(story.id).unwrap().expired
    }

    fn expire(&self) -> ExpireReport {
        self.vault.expire_stories(self.now).unwrap()
    }

    fn ops(&self, thread: ThreadId) -> Vec<Op> {
        self.vault.ops_for_thread(thread, 20).unwrap()
    }

    /// Run the pass's second half over `messages`, as a report would hand it.
    async fn bin(&self, messages: &[Seeded]) {
        let report =
            ExpireReport { mail: messages.iter().map(|m| m.id).collect(), ..Default::default() };
        cleanup::after_expiry(&self.svc, &self.vault, &report).await;
    }

    /// The moves to Trash queued for `thread`.
    fn trashed(&self, thread: ThreadId) -> usize {
        self.ops(thread).iter().filter(|op| op.kind == OpKind::Trash).count()
    }
}

const SEEN: MessageFlags =
    MessageFlags { seen: true, answered: false, flagged: false, draft: false, deleted: false };
const STARRED: MessageFlags =
    MessageFlags { seen: false, answered: false, flagged: true, draft: false, deleted: false };
const DRAFT: MessageFlags =
    MessageFlags { seen: false, answered: false, flagged: false, draft: true, deleted: false };

// ---- the defaults ---------------------------------------------------------

#[tokio::test]
async fn a_source_with_no_retention_of_its_own_keeps_what_its_kind_says() {
    let w = world();
    let mut settings = FeedSettings::default();
    settings.cleanup.newsletters = Retention { read_days: 7, unread_days: 7 };
    w.vault.save_feed_settings(&settings).unwrap();

    let web = NewsSource::new(SourceKind::Web { url: "https://a.example/feed".into() }, "Web");
    w.save(&web);
    let web_story = w.story(&web, None, 10);
    let letter = w.follow("The Weekly", false);
    let letter_story = w.story(&letter, Some(w.issue()), 10);
    let mut own = NewsSource::new(
        SourceKind::Mail { sender: "other@example.com".into(), name: String::new() },
        "Own days",
    );
    own.retention = Some(Retention { read_days: 30, unread_days: 30 });
    w.save(&own);
    let own_story = w.story(&own, Some(w.issue()), 10);

    let report = w.expire();
    assert_eq!(report.expired, 1);
    assert!(w.expired(&letter_story), "ten days is past the newsletters' week");
    assert!(!w.expired(&web_story), "a web feed's thirty days are the feeds' default");
    assert!(!w.expired(&own_story), "and a source's own days come before either");
    assert!(report.mail.is_empty(), "no source here bins its email");
}

#[tokio::test]
async fn nothing_saved_in_settings_means_thirty_days_read_and_ninety_unread() {
    let w = world();
    let letter = w.follow("The Weekly", false);
    let unread_40 = w.story(&letter, Some(w.issue()), 40);
    let read_40 = w.story(&letter, Some(w.message(w.inbox, None, SEEN)), 40);
    let unread_100 = w.story(&letter, Some(w.issue()), 100);

    assert_eq!(w.expire().expired, 2);
    assert!(!w.expired(&unread_40));
    assert!(w.expired(&read_40));
    assert!(w.expired(&unread_100));
}

// ---- which email is reported ----------------------------------------------

#[tokio::test]
async fn the_report_names_the_email_of_a_binning_sources_expired_issues_only() {
    let w = world();
    let bin = w.follow("Bins its mail", true);

    // Due: old and unread, and old enough once read.
    let unread_100 = w.story(&bin, Some(w.issue()), 100);
    let read_40 = w.story(&bin, Some(w.message(w.inbox, None, SEEN)), 40);
    // Not due.
    let unread_40 = w.story(&bin, Some(w.issue()), 40);
    let saved = w.story(&bin, Some(w.issue()), 100);
    w.vault.set_stories_saved(&[saved.id], true).unwrap();
    let queued = w.story(&bin, Some(w.issue()), 100);
    let entry = QueueEntry::new(ContentRef::Story(queued.id), "Queued", 0);
    w.vault.add_to_queue(entry, QueuePlace::End).unwrap();
    let with_files = w.story(&bin, Some(w.issue()), 100);
    w.vault.update_story_files(with_files.id, |f| f.media = Some(FileId::new())).unwrap();
    // Due too: a summary stays in Files without its story, and does not keep it.
    let summarised = w.story(&bin, Some(w.issue()), 100);
    w.vault.update_story_files(summarised.id, |f| f.summary = Some(FileId::new())).unwrap();

    // Expired, but not this source's to bin: another newsletter, and the
    // Newsletters source, which takes whatever Mail files under Newsletters.
    let keeps = w.follow("Keeps its mail", false);
    let keeps_story = w.story(&keeps, Some(w.issue()), 100);
    let every = NewsSource::new(SourceKind::Newsletters, "Newsletters");
    w.save(&every);
    let every_story = w.story(&every, Some(w.issue()), 100);

    let report = w.expire();
    assert_eq!(report.expired, 5);
    let mail: Vec<MailMessageId> = report.mail.clone();
    let message = |story: &Story| story.mail_message.unwrap();
    assert_eq!(mail.len(), 3, "{mail:?}");
    assert!(mail.contains(&message(&unread_100)) && mail.contains(&message(&read_40)));
    assert!(mail.contains(&message(&summarised)));
    for gone in [&unread_100, &read_40, &summarised, &keeps_story, &every_story] {
        assert!(w.expired(gone), "{}", gone.title);
    }
    for kept in [&unread_40, &saved, &queued, &with_files] {
        assert!(!w.expired(kept), "{}", kept.title);
    }

    assert_eq!(w.expire(), ExpireReport::default(), "and a second pass has nothing to say");
}

#[tokio::test]
async fn a_source_can_bin_its_mail_only_if_it_reads_a_sender() {
    let w = world();
    let mut web = NewsSource::new(SourceKind::Web { url: "https://a.example/feed".into() }, "Web");
    web.trash_mail = true;
    assert!(matches!(w.vault.save_news_source(&web), Err(Error::Invalid(_))));
    let mut every = NewsSource::new(SourceKind::Newsletters, "Newsletters");
    every.trash_mail = true;
    assert!(matches!(w.vault.save_news_source(&every), Err(Error::Invalid(_))));
    assert!(w.vault.news_sources().unwrap().is_empty(), "neither was stored");

    w.follow("The Weekly", true);
    assert!(w.vault.news_sources().unwrap()[0].trash_mail);
}

#[tokio::test]
async fn saved_settings_are_clamped_and_the_not_newsletters_tidied() {
    let w = world();
    let mut settings = FeedSettings::default();
    settings.cleanup.feeds = Retention { read_days: 0, unread_days: 100_000 };
    settings.not_newsletters = vec![
        NewsletterRef { sender: " News@Example.com ".into(), name: " The Download ".into() },
        NewsletterRef { sender: "news@example.com".into(), name: "the download".into() },
        NewsletterRef { sender: "".into(), name: "Nobody".into() },
    ];
    let saved = w.vault.save_feed_settings(&settings).unwrap();
    assert_eq!(saved.cleanup.feeds, Retention { read_days: 1, unread_days: 365 });
    assert_eq!(
        saved.not_newsletters,
        [NewsletterRef { sender: NEWS.into(), name: "The Download".into() }]
    );
    assert_eq!(w.vault.feed_settings().unwrap(), saved, "what is read back is what was tidied");
}

// ---- moving it to Trash ---------------------------------------------------

#[tokio::test]
async fn an_unstarred_issue_goes_to_trash_as_a_person_would_send_it() {
    let w = world();
    let issue = w.issue();
    w.bin(&[issue]).await;

    let ops = w.ops(issue.thread);
    assert_eq!(ops.len(), 1, "{ops:?}");
    assert_eq!(ops[0].kind, OpKind::Trash);
    assert_eq!(ops[0].origin, Origin::Person);
    assert_eq!(ops[0].account_id, w.account);
}

#[tokio::test]
async fn mail_that_is_starred_a_draft_in_trash_gone_or_not_the_newsletters_is_left() {
    let w = world();
    let starred = w.message(w.inbox, None, STARRED);
    let draft = w.message(w.inbox, None, DRAFT);
    let in_trash = w.message(w.trash, None, MessageFlags::default());
    let gone = Seeded { id: MailMessageId::new(), thread: ThreadId::new() };
    let plain = w.issue();

    // One issue is cleaned up, another in its thread still is in the Feed:
    // the thread goes when the last of them does, not before.
    let source = w.follow("The Weekly", true);
    let first = w.message(w.inbox, None, MessageFlags::default());
    let second = w.message(w.inbox, Some(first.thread), MessageFlags::default());
    w.story(&source, Some(second), 1);
    // A reply the person wrote is in the thread: not the newsletter's to bin.
    let issue = w.issue();
    w.message(w.sent, Some(issue.thread), SEEN);
    // One of a thread's messages starred is the whole thread starred.
    let thread_with_star = w.issue();
    w.message(w.inbox, Some(thread_with_star.thread), STARRED);

    w.bin(&[starred, draft, in_trash, gone, first, issue, thread_with_star, plain]).await;

    for (what, seeded) in [
        ("starred", starred),
        ("a draft", draft),
        ("already in Trash", in_trash),
        ("gone", gone),
        ("with a newer issue still kept", first),
        ("with a reply of the person's", issue),
        ("with a starred message in it", thread_with_star),
    ] {
        assert_eq!(w.trashed(seeded.thread), 0, "{what}");
    }
    assert_eq!(w.trashed(plain.thread), 1, "the one that is none of those");
}

#[tokio::test]
async fn two_issues_in_one_thread_are_one_move() {
    let w = world();
    let first = w.issue();
    let second = w.message(w.inbox, Some(first.thread), MessageFlags::default());
    w.bin(&[first, second]).await;
    assert_eq!(w.trashed(first.thread), 1);
}

#[tokio::test]
async fn expiring_a_binning_source_trashes_what_is_not_starred_and_only_that() {
    let w = world();
    let source = w.follow("The Weekly", true);
    let plain = w.issue();
    let starred = w.message(w.inbox, None, STARRED);
    let plain_story = w.story(&source, Some(plain), 100);
    let starred_story = w.story(&source, Some(starred), 100);
    let young = w.issue();
    let young_story = w.story(&source, Some(young), 3);

    let report = w.expire();
    assert!(w.expired(&plain_story) && w.expired(&starred_story) && !w.expired(&young_story));
    cleanup::after_expiry(&w.svc, &w.vault, &report).await;

    assert_eq!(w.trashed(plain.thread), 1, "cleaned up, so its email is too");
    assert_eq!(w.trashed(starred.thread), 0, "cleaned up, but starred");
    assert_eq!(w.trashed(young.thread), 0, "still in the Feed");
}
