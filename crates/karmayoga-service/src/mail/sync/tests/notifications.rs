// ---------------------------------------------------------------------
// Which new mail raises a notification -- `Account::mail_notify`, decided
// as a pass stores it. See `crate::arrivals`.
// ---------------------------------------------------------------------

use karmayoga_core::account::MailNotify;
use karmayoga_core::mail::Category;
use karmayoga_mail::session::Flags;

use super::fixtures::*;

const DATE: &str = "01 Jan 2024 09:00:00 +0000";

/// An account past its first fill, so a message's age says nothing; set to
/// `rule`, with `ada@example.com` always filed as Important.
async fn synced_with(rule: MailNotify) -> (TestEnv, std::sync::Arc<std::sync::Mutex<FakeServer>>) {
    let env = TestEnv::new();
    let mut account = env.vault.account(env.account_id).unwrap();
    account.mail_notify = rule;
    env.vault.save_account(&account).unwrap();
    let mut rules = env.vault.category_rules(env.account_id).unwrap();
    rules.set_sender("ada@example.com", Category::Important);
    env.vault.save_category_rules(env.account_id, &rules).unwrap();

    let server = plain_server();
    server.lock().unwrap().append(
        "INBOX",
        raw_message("first@example.com", None, "someone@example.com", "First", DATE, "hi"),
        Flags::SEEN,
        None,
    );
    let mut session = FakeMailSession::new(server.clone());
    env.sync(&mut session).await;
    assert_eq!(noted(&env), Vec::<String>::new(), "old read mail is not news");
    (env, server)
}

fn arrive(server: &std::sync::Arc<std::sync::Mutex<FakeServer>>, id: &str, from: &str) {
    server.lock().unwrap().append(
        "INBOX",
        raw_message(id, None, from, id, DATE, "hello"),
        Flags::NONE,
        None,
    );
}

/// Each message noted since last asked, with the first words its
/// notification will show.
fn noted_with_words(env: &TestEnv) -> Vec<(String, String)> {
    let (kept, _) = env.statuses.take_new_mail(env.account_id);
    kept.into_iter().map(|m| (m.subject, m.snippet)).collect()
}

/// The subjects noted since last asked.
fn noted(env: &TestEnv) -> Vec<String> {
    let (kept, total) = env.statuses.take_new_mail(env.account_id);
    assert_eq!(kept.len(), total, "fewer than the pass keeps whole");
    let mut subjects: Vec<String> = kept.into_iter().map(|m| m.subject).collect();
    subjects.sort();
    subjects
}

#[tokio::test]
async fn the_default_tells_you_about_important_mail_and_nothing_else() {
    let (env, server) = synced_with(MailNotify::default()).await;
    arrive(&server, "wanted", "ada@example.com");
    arrive(&server, "stranger", "someone@example.com");

    let mut session = FakeMailSession::new(server);
    env.sync(&mut session).await;

    // Its body is fetched before it is noted, so the notification can say
    // what it is about.
    assert_eq!(noted_with_words(&env), [("wanted".to_string(), "hello".to_string())]);
}

#[tokio::test]
async fn all_tells_you_about_everything_but_what_you_wrote_or_already_read() {
    let (env, server) = synced_with(MailNotify::All).await;
    arrive(&server, "wanted", "ada@example.com");
    arrive(&server, "stranger", "someone@example.com");
    arrive(&server, "own", "me@example.com");
    server.lock().unwrap().append(
        "INBOX",
        raw_message("read", None, "someone@example.com", "read", DATE, "hi"),
        Flags::SEEN,
        None,
    );

    let mut session = FakeMailSession::new(server);
    env.sync(&mut session).await;

    assert_eq!(noted(&env), ["stranger", "wanted"]);
}

#[tokio::test]
async fn off_tells_you_nothing() {
    let (env, server) = synced_with(MailNotify::Off).await;
    arrive(&server, "wanted", "ada@example.com");

    let mut session = FakeMailSession::new(server);
    env.sync(&mut session).await;

    assert_eq!(noted(&env), Vec::<String>::new());
}

#[tokio::test]
async fn categories_tell_you_about_the_tabs_ticked_and_no_others() {
    let (env, server) =
        synced_with(MailNotify::Categories { tabs: vec![Category::Other], sorted: Vec::new() })
            .await;
    arrive(&server, "important", "ada@example.com");
    arrive(&server, "other", "someone@example.com");

    let mut session = FakeMailSession::new(server);
    env.sync(&mut session).await;

    assert_eq!(noted(&env), ["other"]);
}

#[tokio::test]
async fn replies_tell_you_only_about_threads_you_have_written_in() {
    let (env, server) = synced_with(MailNotify::Replies).await;
    // Somebody's first message, and the account's own answer in Sent.
    {
        let mut s = server.lock().unwrap();
        s.append(
            "INBOX",
            raw_message("asked@example.com", None, "someone@example.com", "Asked", DATE, "hi"),
            Flags::SEEN,
            None,
        );
        s.append(
            "Sent",
            raw_message(
                "answered@example.com",
                Some("asked@example.com"),
                "me@example.com",
                "Re: Asked",
                DATE,
                "yes",
            ),
            Flags::SEEN,
            None,
        );
    }
    let mut session = FakeMailSession::new(server.clone());
    env.sync(&mut session).await;
    assert_eq!(noted(&env), Vec::<String>::new());

    {
        let mut s = server.lock().unwrap();
        s.append(
            "INBOX",
            raw_message(
                "reply@example.com",
                Some("answered@example.com"),
                "someone@example.com",
                "in a thread I answered",
                DATE,
                "thanks",
            ),
            Flags::NONE,
            None,
        );
        s.append(
            "INBOX",
            raw_message(
                "unrelated@example.com",
                None,
                "ada@example.com",
                "important but never answered",
                DATE,
                "hi",
            ),
            Flags::NONE,
            None,
        );
    }
    env.sync(&mut session).await;

    // Held until the pass had stored Sent, and fetched before that like what
    // is announced at once.
    assert_eq!(
        noted_with_words(&env),
        [("in a thread I answered".to_string(), "thanks".to_string())]
    );
}

#[tokio::test]
async fn a_message_told_as_it_arrived_is_not_told_again_when_a_model_files_it() {
    let env = TestEnv::new();
    let mut account = env.vault.account(env.account_id).unwrap();
    account.mail_notify = MailNotify::All;
    env.vault.save_account(&account).unwrap();
    let server = plain_server();
    let mut session = FakeMailSession::new(server.clone());
    env.sync(&mut session).await;

    arrive(&server, "once", "someone@example.com");
    env.sync(&mut session).await;
    assert_eq!(noted(&env), ["once"]);
    assert!(
        !env.statuses.first_announcement(
            &env.vault
                .message_by_message_id_header(env.account_id, "once")
                .unwrap()
                .unwrap()
                .id
                .to_string()
        ),
        "a model filing it next will find it told already"
    );
}

/// A phone, overnight: the account's answer and the reply to it both reach this
/// app in one pass. Off Gmail the Inbox is synced before Sent, so the reply is
/// stored first, and a message is only new the once.
#[tokio::test]
async fn a_reply_is_told_when_your_own_answer_arrives_in_the_same_pass() {
    let (env, server) = synced_with(MailNotify::Replies).await;
    server.lock().unwrap().append(
        "INBOX",
        raw_message("asked@example.com", None, "someone@example.com", "Asked", DATE, "hi"),
        Flags::SEEN,
        None,
    );
    let mut session = FakeMailSession::new(server.clone());
    env.sync(&mut session).await;
    assert_eq!(noted(&env), Vec::<String>::new());

    {
        let mut s = server.lock().unwrap();
        s.append(
            "INBOX",
            raw_message(
                "reply@example.com",
                Some("asked@example.com"),
                "someone@example.com",
                "after my answer from my phone",
                DATE,
                "thanks",
            ),
            Flags::NONE,
            None,
        );
        s.append(
            "Sent",
            raw_message(
                "answered@example.com",
                Some("asked@example.com"),
                "me@example.com",
                "Re: Asked",
                DATE,
                "yes",
            ),
            Flags::SEEN,
            None,
        );
    }
    env.sync(&mut session).await;

    assert_eq!(noted(&env), ["after my answer from my phone"]);
}

/// An address is the account's own however it was typed, and from the moment it
/// is added, not from the next restart.
#[tokio::test]
async fn your_own_mail_is_yours_whatever_case_you_typed_and_whenever_you_added_it() {
    let (env, server) = synced_with(MailNotify::All).await;
    let mut account = env.vault.account(env.account_id).unwrap();
    account.address = "Me@Example.COM".into();
    env.vault.save_account(&account).unwrap();
    arrive(&server, "own", "me@example.com");
    arrive(&server, "alias", "alias@example.com");
    arrive(&server, "someone", "someone@example.com");
    let mut session = FakeMailSession::new(server.clone());
    env.sync(&mut session).await;
    assert_eq!(noted(&env), ["alias", "someone"]);

    account.identities.push(karmayoga_core::account::Identity {
        name: String::new(),
        address: "Alias@Example.com".into(),
        signature_html: String::new(),
    });
    env.vault.save_account(&account).unwrap();
    arrive(&server, "alias again", "alias@example.com");
    env.sync(&mut session).await;
    assert_eq!(noted(&env), Vec::<String>::new());
}

/// What a model may announce a moment later is what a pass just stored. Read
/// mail, the account's own, and everything from before are not arrivals.
#[tokio::test]
async fn a_pass_says_what_arrived_for_a_model_to_announce_later() {
    let (env, server) = synced_with(MailNotify::Off).await;
    let id_of = |header: &str| {
        env.vault
            .message_by_message_id_header(env.account_id, header)
            .unwrap()
            .unwrap()
            .id
            .to_string()
    };
    let now = std::time::Instant::now;
    assert!(!env.statuses.arrived_lately(&id_of("first@example.com"), now()), "from before");

    arrive(&server, "just-now", "someone@example.com");
    server.lock().unwrap().append(
        "INBOX",
        raw_message("read", None, "someone@example.com", "read", DATE, "hi"),
        Flags::SEEN,
        None,
    );
    arrive(&server, "mine", "me@example.com");
    let mut session = FakeMailSession::new(server);
    env.sync(&mut session).await;

    assert!(env.statuses.arrived_lately(&id_of("just-now"), now()), "whether wanted or not");
    assert!(!env.statuses.arrived_lately(&id_of("read"), now()));
    assert!(!env.statuses.arrived_lately(&id_of("mine"), now()));
    assert_eq!(noted(&env), Vec::<String>::new(), "and nobody is told yet");
}

/// A noted message wakes the account task beside its pass, which announces it
/// there and then rather than after every other mailbox and every body; and
/// any new inbox mail, wanted or not as the rules filed it, has the model's
/// sorting started as soon as the pass is over.
#[tokio::test]
async fn a_noted_message_wakes_the_task_and_new_mail_starts_the_sorting() {
    use std::time::Duration;
    let (env, server) = synced_with(MailNotify::default()).await;
    let news = env.statuses.news(env.account_id);
    assert!(env.statuses.take_arrived(env.account_id).is_empty(), "read mail from the first fill");

    arrive(&server, "stranger", "someone@example.com");
    let mut session = FakeMailSession::new(server.clone());
    env.sync(&mut session).await;
    assert_eq!(env.statuses.take_arrived(env.account_id).len(), 1, "the model may yet want it");
    assert!(
        tokio::time::timeout(Duration::ZERO, news.notified()).await.is_err(),
        "nothing wanted, nothing to wake for"
    );

    arrive(&server, "wanted", "ada@example.com");
    env.sync(&mut session).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), news.notified()).await.is_ok(),
        "woken as it was noted"
    );
    assert_eq!(noted(&env), ["wanted"]);
}

/// The account task announces a noted message the moment it wakes, mid-pass,
/// and tells an open list to draw it only if the pass has said its list
/// changed. That used to be said once the whole mailbox was done, so the
/// banner came first and the list went on without the message it named.
#[tokio::test]
async fn by_the_time_a_notice_can_go_out_the_list_has_been_told() {
    let (env, server) = synced_with(MailNotify::default()).await;
    let news = env.statuses.news(env.account_id);
    let _ = env.statuses.take_changed(env.account_id);

    arrive(&server, "wanted", "ada@example.com");
    let mut session = FakeMailSession::new(server);
    // Woken as the task is, beside the pass: what it would find then.
    let at_the_notice = async {
        news.notified().await;
        env.statuses.take_changed(env.account_id)
    };
    let (_, changed) = tokio::join!(env.sync(&mut session), at_the_notice);

    assert!(changed, "the list is told with the notice, not after the pass");
    assert!(
        !env.statuses.take_changed(env.account_id),
        "and not told again at the end of the mailbox for the same message"
    );
    assert_eq!(noted(&env), ["wanted"]);
}

// ---------------------------------------------------------------------
// Mail filed where a person writing to you lands -- Priority and Important
// -- is announced as it is stored only when it is from somebody you know.
// See `crate::arrivals`' "people, and strangers who write like people".
// ---------------------------------------------------------------------

/// A message from `from` to `to`, with `extra` header lines.
fn raw_to(id: &str, from: &str, to: &str, extra: &str) -> Vec<u8> {
    format!(
        "Message-ID: <{id}>\r\nFrom: {from}\r\nTo: {to}\r\n{extra}Subject: {id}\r\nDate: {DATE}\r\n\
         Content-Type: text/plain\r\n\r\nhello\r\n"
    )
    .into_bytes()
}

/// An account past its first fill, on the default rule, that has written to
/// `ada@example.com` before.
async fn knowing_ada(gmail: bool) -> (TestEnv, std::sync::Arc<std::sync::Mutex<FakeServer>>) {
    let mut env = TestEnv::new();
    let contacts = crate::mail::sync::contacts::ContactIndex::load(&env.vault);
    contacts.record_sent_to("ada@example.com", "Ada");
    env.contacts = Some(std::sync::Arc::new(contacts));
    let server = if gmail { gmail_server() } else { plain_server() };
    let mut session = FakeMailSession::new(server.clone());
    env.sync(&mut session).await;
    (env, server)
}

fn gmail_labels(labels: &[&str], id: u64) -> Option<karmayoga_mail::session::GmailMeta> {
    Some(karmayoga_mail::session::GmailMeta {
        thrid: id,
        msgid: id,
        labels: labels.iter().map(|l| l.to_string()).collect(),
    })
}

#[tokio::test]
async fn somebody_you_have_written_to_is_announced_as_it_is_stored() {
    let (env, server) = knowing_ada(false).await;
    server.lock().unwrap().append(
        "INBOX",
        raw_to("from-ada", "Ada <ada@example.com>", "me@example.com", ""),
        Flags::NONE,
        None,
    );
    env.sync(&mut FakeMailSession::new(server)).await;
    assert_eq!(noted(&env), ["from-ada"]);
}

/// Gmail's `\Important` label is the rules' strongest signal, and it marks a
/// stranger's pitch as often as a colleague's question. Nobody is told until
/// a model has read it; the model is handed it at once.
#[tokio::test]
async fn a_stranger_gmail_calls_important_waits_for_a_model() {
    let (env, server) = knowing_ada(true).await;
    server.lock().unwrap().append(
        "All Mail",
        raw_to("pitch", "Rep <rep@vendor.example>", "me@example.com", ""),
        Flags::NONE,
        gmail_labels(&["\\Inbox", "\\Important"], 7),
    );
    env.sync(&mut FakeMailSession::new(server)).await;
    assert_eq!(noted(&env), Vec::<String>::new());
    assert_eq!(env.statuses.take_arrived(env.account_id).len(), 1, "the model reads it next");
}

#[tokio::test]
async fn somebody_you_know_writing_to_a_list_you_are_on_waits_for_a_model() {
    let (env, server) = knowing_ada(false).await;
    server.lock().unwrap().append(
        "INBOX",
        raw_to("to-the-team", "Ada <ada@example.com>", "team@example.com", ""),
        Flags::NONE,
        None,
    );
    env.sync(&mut FakeMailSession::new(server)).await;
    assert_eq!(noted(&env), Vec::<String>::new());
}

/// Gmail calls it important, and you have written to the sender -- but it
/// carries a list's unsubscribe line, so it is their newsletter.
#[tokio::test]
async fn a_newsletter_from_somebody_you_know_waits_for_a_model() {
    let (env, server) = knowing_ada(true).await;
    server.lock().unwrap().append(
        "All Mail",
        raw_to(
            "ada-weekly",
            "Ada <ada@example.com>",
            "me@example.com",
            "List-Unsubscribe: <mailto:leave@example.com>\r\n",
        ),
        Flags::NONE,
        gmail_labels(&["\\Inbox", "\\Important"], 8),
    );
    env.sync(&mut FakeMailSession::new(server)).await;
    assert_eq!(noted(&env), Vec::<String>::new());
}

/// Everything means everything: the rule nobody narrows is not narrowed here.
#[tokio::test]
async fn all_still_tells_you_about_a_stranger_gmail_calls_important() {
    let (env, server) = knowing_ada(true).await;
    let mut account = env.vault.account(env.account_id).unwrap();
    account.mail_notify = MailNotify::All;
    env.vault.save_account(&account).unwrap();
    server.lock().unwrap().append(
        "All Mail",
        raw_to("pitch", "Rep <rep@vendor.example>", "me@example.com", ""),
        Flags::NONE,
        gmail_labels(&["\\Inbox", "\\Important"], 9),
    );
    env.sync(&mut FakeMailSession::new(server)).await;
    assert_eq!(noted(&env), ["pitch"]);
}
