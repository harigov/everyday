//! Leaving newsletters from the Feed, many at once -- `unsubscribe_news_sources`
//! and `mark_not_newsletters` -- against a real SQLite vault.
//!
//! How a header is read and which way out is taken are pinned where they
//! live (`jatayu_core::mail::unsubscribe`, and the service's own
//! `mail::unsubscribe`). What is checked here is what the Feed adds on top:
//! which mail a newsletter's sender is asked from -- its stories', or recent
//! mail's once those are cleaned up -- that a newsletter left is turned down
//! and not found again, that a web feed goes with it and the Newsletters
//! source never does, which mail moves in Mail when somebody says a
//! newsletter is not one, and who may do either.
//!
//! The headers are set on the seeded messages directly, and every sender here
//! offers an email or a page. One click is never used: its POST goes to the
//! open internet only, which a test on loopback cannot reach.

#[allow(dead_code)]
mod support;

use std::cell::Cell;
use std::sync::Arc;

use jatayu_core::Vault;
use jatayu_core::account::{Account, Provider};
use jatayu_core::id::{AccountId, MailMessageId, MailboxId, NewsSourceId, PackId, ThreadId};
use jatayu_core::mail::{
    Address, Category, CategorySource, ListUnsubscribe, Mailbox, MailboxRole, Message, MessageFlags,
};
use jatayu_core::news::{FetchState, NewsSource, Retention, SourceKind, Story};
use jatayu_core::packstore::PackRef;
use jatayu_core::store::mail::IngestMessage;
use jatayu_core::store::news::StoryQuery;
use jatayu_service::Service;
use jatayu_service::ctx::{Ctx, Scope};
use jatayu_service::error::{CommandError, codes};
use jatayu_service::news::newsletters;
use jiff::Timestamp;
use serde_json::{Value, json};

/// The one mail account's own address, which the newsletters write to.
const ME: &str = "me@example.com";
const NEWS: &str = "news@example.com";
const DIGEST: &str = "digest@example.com";
/// Where the newsletters' unsubscribe emails go.
const LEAVE: &str = "leave@news.example";

/// A header offering an email, and nothing else.
fn by_email() -> ListUnsubscribe {
    ListUnsubscribe::parse(&format!("<mailto:{LEAVE}?subject=unsubscribe>"), None).unwrap()
}

/// A header offering a page, and nothing else.
fn by_page() -> ListUnsubscribe {
    ListUnsubscribe::parse("<https://news.example/leave?u=1>", None).unwrap()
}

/// A vault with one mail account and an Inbox. The account is saved straight
/// into the vault rather than through `save_account`, so no sync task starts
/// for an account with no server behind it -- and nothing drains the outbox,
/// so an email queued stays queued for a test to find.
struct World {
    svc: Arc<Service>,
    vault: Arc<Vault>,
    account: AccountId,
    inbox: MailboxId,
    next_uid: Cell<u32>,
    _dir: tempfile::TempDir,
}

fn world() -> World {
    let (svc, dir) = support::vault::service(None);
    let vault = svc.get().unwrap();
    let mut account = Account::new(Provider::Custom, ME);
    account.services.mail = true;
    vault.save_account(&account).unwrap();
    let inbox = Mailbox::new(account.id, "INBOX", MailboxRole::Inbox);
    vault.save_mailbox(&inbox).unwrap();
    World { svc, vault, account: account.id, inbox: inbox.id, next_uid: Cell::new(0), _dir: dir }
}

/// One message to seed: an issue of a newsletter from [`NEWS`], filed under
/// Newsletters, with a header offering an email unless a test says otherwise.
#[derive(Clone)]
struct Mail<'a> {
    from: &'a str,
    name: &'a str,
    subject: &'a str,
    /// Hours before now; at least one, so the message is inside any window
    /// that ends now.
    hours_ago: i64,
    unsubscribe: Option<ListUnsubscribe>,
}

impl<'a> Mail<'a> {
    fn news(name: &'a str, hours_ago: i64, subject: &'a str) -> Self {
        Self { from: NEWS, name, subject, hours_ago, unsubscribe: Some(by_email()) }
    }
}

impl World {
    fn seed(&self, mail: Mail) -> MailMessageId {
        let id = MailMessageId::new();
        // Whole seconds, so a date read back out of the vault is the one that
        // went in.
        let date =
            Timestamp::from_second(Timestamp::now().as_second() - mail.hours_ago * 3600).unwrap();
        let message = Message {
            id,
            account_id: self.account,
            thread_id: ThreadId::new(),
            message_id_header: format!("<{id}@news.example>"),
            date,
            from: Address::new(mail.name, mail.from),
            to: vec![Address::bare(ME)],
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: mail.subject.into(),
            snippet: format!("About {}", mail.subject),
            flags: MessageFlags::default(),
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
            unsubscribe: mail.unsubscribe,
        };
        let uid = self.next_uid.get() + 1;
        self.next_uid.set(uid);
        self.vault
            .ingest_mail(self.account, vec![IngestMessage { message, mailbox: self.inbox, uid }])
            .unwrap();
        id
    }

    /// A sender followed by hand, as a whole.
    fn follow(&self, sender: &str) -> NewsSource {
        let source = NewsSource::new(
            SourceKind::Mail { sender: sender.into(), name: String::new() },
            "The Weekly",
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

    /// A web feed, which no test here ever refreshes.
    fn web_feed(&self) -> NewsSource {
        let feed =
            NewsSource::new(SourceKind::Web { url: "https://feeds.example/rss".into() }, "A feed");
        self.vault.save_news_source(&feed).unwrap();
        feed
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

    /// Whether the source is still in the Feed.
    fn has(&self, id: NewsSourceId) -> bool {
        self.vault.news_sources().unwrap().iter().any(|s| s.id == id)
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

    /// Where Mail files the message.
    fn category(&self, id: MailMessageId) -> Option<Category> {
        self.vault.mail_message(id).unwrap().category
    }

    /// The correction Mail keeps for the sender, if there is one.
    fn rule(&self, sender: &str) -> Option<Category> {
        self.vault.category_rules(self.account).unwrap().for_sender(sender)
    }

    /// `unsubscribe_news_sources`, answering its outcomes.
    async fn unsubscribe(
        &self,
        ctx: Ctx,
        ids: &[NewsSourceId],
    ) -> Result<Vec<Value>, CommandError> {
        let ids: Vec<String> = ids.iter().map(ToString::to_string).collect();
        let answer = self.svc.call(ctx, "unsubscribe_news_sources", json!({ "ids": ids })).await?;
        Ok(answer.as_array().expect("a list of outcomes").clone())
    }

    /// `mark_not_newsletters`, moving their mail to `category` when given.
    async fn not_newsletters(
        &self,
        ctx: Ctx,
        ids: &[NewsSourceId],
        category: Option<&str>,
    ) -> Result<Value, CommandError> {
        let ids: Vec<String> = ids.iter().map(ToString::to_string).collect();
        let args = json!({ "ids": ids, "category": category });
        self.svc.call(ctx, "mark_not_newsletters", args).await
    }
}

/// A connection that may reach the Feed and nothing else.
fn feed_only() -> Ctx {
    Ctx { scopes: vec![Scope::News], ..Ctx::local() }
}

/// A newsletter the Newsletters source found is asked by the email its header
/// names, and leaves the Feed turned down, so its next issue is not found
/// again. A web feed among the ids is unfollowed; the Newsletters source
/// stays whatever is asked, and so does a newsletter nobody named.
#[tokio::test]
async fn unsubscribing_asks_by_email_and_turns_the_newsletter_down() {
    let w = world();
    let issue = w.seed(Mail::news("The Download", 50, "Download 1"));
    w.seed(Mail { unsubscribe: Some(by_page()), ..Mail::news("The Algorithm", 40, "Algorithm 1") });
    let all = w.follow_all();
    let mut state = FetchState::new(all.id);
    assert_eq!(w.turn(&all, &mut state).await, 2);
    let download = w.found_named("The Download");
    let algorithm = w.found_named("The Algorithm");
    let feed = w.web_feed();

    let outcomes = w.unsubscribe(Ctx::local(), &[download.id, feed.id, all.id]).await.unwrap();
    assert_eq!(outcomes.len(), 1, "one sender asked, and a web feed has nobody: {outcomes:?}");
    let outcome = &outcomes[0];
    assert_eq!(outcome["email"], json!(NEWS));
    assert_eq!(outcome["name"], json!("The Download"), "named as the Feed names it");
    assert_eq!(outcome["account"], json!(w.account.to_string()));
    assert_eq!(outcome["via"], json!("mail"), "{outcome}");
    assert_eq!(outcome["done"], json!(true), "{outcome}");

    let drafts = w.vault.drafts(w.account).unwrap();
    assert!(drafts.iter().any(|d| d.to.iter().any(|a| a.email == LEAVE)), "the email is queued");
    let asked = w.vault.mail_message(issue).unwrap().unsubscribe.and_then(|u| u.asked_at);
    assert!(asked.is_some(), "the message it was asked from remembers it");

    assert!(!w.has(download.id));
    assert!(!w.has(feed.id), "the web feed is unfollowed");
    assert!(w.has(all.id), "the Newsletters source is never one to leave");
    assert!(w.has(algorithm.id), "nor is a newsletter nobody named");
    assert_eq!(w.turned_down(), [(NEWS.to_string(), "The Download".to_string())]);

    // Its next issue, already on its way, is not found again -- and nor is
    // the one its story pointed at, now that the story has gone with it.
    w.seed(Mail::news("The Download", 1, "Download 2"));
    assert_eq!(w.turn(&all, &mut state).await, 0);
    let found: Vec<String> = w.found().into_iter().map(|s| s.title).collect();
    assert_eq!(found, ["The Algorithm"]);
}

/// A newsletter whose issues have all been cleaned up has no story left to
/// say which mail it brought; its sender is asked from its recent mail
/// instead.
#[tokio::test]
async fn a_newsletter_whose_stories_were_cleaned_up_is_asked_from_its_mail() {
    let w = world();
    w.seed(Mail { unsubscribe: Some(by_page()), ..Mail::news("The Download", 50, "Download 1") });
    let all = w.follow_all();
    let mut state = FetchState::new(all.id);
    assert_eq!(w.turn(&all, &mut state).await, 1);
    let mut download = w.found_named("The Download");
    download.retention = Some(Retention { read_days: 1, unread_days: 1 });
    w.vault.save_news_source(&download).unwrap();
    w.vault.expire_stories(Timestamp::now()).unwrap();
    assert!(w.stories(&download).is_empty(), "cleaned up");

    let outcomes = w.unsubscribe(Ctx::local(), &[download.id]).await.unwrap();
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    assert_eq!(outcomes[0]["via"], json!("web"), "{}", outcomes[0]);
    assert_eq!(outcomes[0]["done"], json!(false), "a page is the person's to finish");
    let url = outcomes[0]["url"].as_str().unwrap_or_default();
    assert!(url.starts_with("https://news.example/leave"), "{url:?}");
    assert!(!w.has(download.id));
    assert_eq!(w.turned_down(), [(NEWS.to_string(), "The Download".to_string())]);
}

/// A newsletter with no mail left anywhere is still answered for -- nothing
/// could be asked -- under the one mail account, and still leaves the Feed.
#[tokio::test]
async fn a_newsletter_with_no_mail_left_says_nothing_could_be_asked() {
    let w = world();
    let mut gone =
        NewsSource::new(SourceKind::Mail { sender: DIGEST.into(), name: String::new() }, DIGEST);
    gone.found = true;
    w.vault.save_news_source(&gone).unwrap();

    let outcomes = w.unsubscribe(Ctx::local(), &[gone.id]).await.unwrap();
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    let outcome = &outcomes[0];
    assert_eq!(outcome["email"], json!(DIGEST));
    assert_eq!(outcome["account"], json!(w.account.to_string()));
    assert_eq!(outcome["done"], json!(false));
    assert!(outcome.get("via").is_none(), "{outcome}");
    assert!(outcome["error"].is_string(), "{outcome}");
    assert!(!w.has(gone.id));
    assert_eq!(w.turned_down(), [(DIGEST.to_string(), String::new())]);
}

/// A sender followed by hand as a whole is turned down under every display
/// name its mail came under, as well as under none: once its source has
/// gone, the Newsletters source would otherwise find each of those again.
#[tokio::test]
async fn unsubscribing_a_sender_followed_whole_turns_down_each_name_it_sent_under() {
    let w = world();
    w.seed(Mail { unsubscribe: Some(by_page()), ..Mail::news("The Weekly", 30, "Weekly 1") });
    let followed = w.follow(NEWS);
    let mut own = FetchState::new(followed.id);
    assert_eq!(w.turn(&followed, &mut own).await, 1);
    let all = w.follow_all();

    let outcomes = w.unsubscribe(Ctx::local(), &[followed.id]).await.unwrap();
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    assert!(!w.has(followed.id));
    assert_eq!(
        w.turned_down(),
        [(NEWS.to_string(), String::new()), (NEWS.to_string(), "The Weekly".to_string())]
    );

    w.seed(Mail { unsubscribe: Some(by_page()), ..Mail::news("The Weekly", 1, "Weekly 2") });
    let mut state = FetchState::new(all.id);
    assert_eq!(w.turn(&all, &mut state).await, 0);
    assert!(w.found().is_empty(), "{:?}", w.found());
}

/// "Not newsletters" with a category moves a sender's mail there in Mail and
/// keeps it there -- but not while another newsletter from the same address
/// stays in the Feed, since Mail files by address and that one's issues
/// would go with it.
#[tokio::test]
async fn not_newsletters_moves_their_mail_unless_a_sibling_stays() {
    let w = world();
    let download = w.seed(Mail::news("The Download", 50, "Download 1"));
    let algorithm = w.seed(Mail::news("The Algorithm", 40, "Algorithm 1"));
    let digest = w.seed(Mail { from: DIGEST, ..Mail::news("The Digest", 30, "Digest 1") });
    let all = w.follow_all();
    let mut state = FetchState::new(all.id);
    assert_eq!(w.turn(&all, &mut state).await, 3);
    let download_source = w.found_named("The Download");
    let algorithm_source = w.found_named("The Algorithm");
    let digest_source = w.found_named("The Digest");

    let ids = [algorithm_source.id, digest_source.id];
    w.not_newsletters(Ctx::local(), &ids, Some("other")).await.unwrap();
    assert!(!w.has(algorithm_source.id));
    assert!(!w.has(digest_source.id));
    assert!(w.has(download_source.id));
    assert_eq!(
        w.turned_down(),
        [
            (DIGEST.to_string(), "The Digest".to_string()),
            (NEWS.to_string(), "The Algorithm".to_string()),
        ]
    );
    assert_eq!(w.category(digest), Some(Category::Other), "moved in Mail");
    assert_eq!(w.rule(DIGEST), Some(Category::Other), "and kept there");
    assert_eq!(w.category(algorithm), Some(Category::Newsletter), "The Download reads its address");
    assert_eq!(w.category(download), Some(Category::Newsletter));
    assert_eq!(w.rule(NEWS), None);

    // Once the last newsletter from the address goes, its mail goes too. The
    // Newsletters source among the ids is not one to say no to.
    w.not_newsletters(Ctx::local(), &[download_source.id, all.id], Some("other")).await.unwrap();
    assert!(!w.has(download_source.id));
    assert!(w.has(all.id));
    assert_eq!(w.category(download), Some(Category::Other));
    assert_eq!(w.category(algorithm), Some(Category::Other));
    assert_eq!(w.rule(NEWS), Some(Category::Other));
}

/// Without a category, "not newsletters" only takes them out of the Feed:
/// their mail stays where Mail filed it, and they are not found again. A web
/// feed among the ids is not a newsletter, and stays.
#[tokio::test]
async fn not_newsletters_without_a_category_leaves_their_mail_alone() {
    let w = world();
    let issue = w.seed(Mail::news("The Download", 50, "Download 1"));
    let all = w.follow_all();
    let mut state = FetchState::new(all.id);
    assert_eq!(w.turn(&all, &mut state).await, 1);
    let download = w.found_named("The Download");
    let feed = w.web_feed();

    w.not_newsletters(Ctx::local(), &[download.id, feed.id], None).await.unwrap();
    assert!(!w.has(download.id));
    assert!(w.has(feed.id), "a web feed is not a newsletter to say no to");
    assert_eq!(w.turned_down(), [(NEWS.to_string(), "The Download".to_string())]);
    assert_eq!(w.category(issue), Some(Category::Newsletter));
    assert_eq!(w.rule(NEWS), None);

    w.seed(Mail::news("The Download", 1, "Download 2"));
    assert_eq!(w.turn(&all, &mut state).await, 0);
    assert!(w.found().is_empty(), "{:?}", w.found());
}

/// Asking a newsletter's sender sends a request in the person's name, and
/// moving mail is Mail's business: a connection that may reach the Feed and
/// not Mail may do neither, and nothing is removed when it tries. What is the
/// Feed's alone -- unfollowing a web feed, saying no without moving mail --
/// it may.
#[tokio::test]
async fn a_connection_without_mail_may_not_ask_senders_or_move_mail() {
    let w = world();
    w.seed(Mail::news("The Download", 50, "Download 1"));
    let all = w.follow_all();
    let mut state = FetchState::new(all.id);
    assert_eq!(w.turn(&all, &mut state).await, 1);
    let download = w.found_named("The Download");
    let feed = w.web_feed();

    let refused = w.unsubscribe(feed_only(), &[download.id, feed.id]).await.unwrap_err();
    assert_eq!(refused.code, codes::FORBIDDEN);
    assert!(w.has(download.id), "nothing is removed");
    assert!(w.has(feed.id), "not even what it could have");
    let refused = w.not_newsletters(feed_only(), &[download.id], Some("other")).await.unwrap_err();
    assert_eq!(refused.code, codes::FORBIDDEN);
    assert!(w.has(download.id));
    assert!(w.turned_down().is_empty());

    assert!(w.unsubscribe(feed_only(), &[feed.id]).await.unwrap().is_empty());
    assert!(!w.has(feed.id));
    w.not_newsletters(feed_only(), &[download.id], None).await.unwrap();
    assert!(!w.has(download.id));
    assert_eq!(w.turned_down(), [(NEWS.to_string(), "The Download".to_string())]);
}
