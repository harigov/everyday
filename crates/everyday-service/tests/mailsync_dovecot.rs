//! The sync engine against a real IMAP server: `make test-imap` starts a
//! throwaway Dovecot container (see the `Makefile`), this file seeds it with
//! about fifty messages -- an attachment and a three-deep reply chain among
//! them -- syncs them into a real SQLite vault with
//! [`everyday_service::mailsync::passes::sync_once`], and asserts on what
//! the vault, the pack store and the search index say afterwards.
//!
//! Gated exactly the way `everyday-mail`'s own `imap_dovecot.rs` is: without
//! `EVERYDAY_TEST_IMAP` set, this prints why it did nothing and passes.
//! Unlike that file, this crate's `insecure-test-tls` feature on
//! `everyday-mail` is unconditionally on for `everyday-service`'s own tests
//! (see this crate's `Cargo.toml`), so there is no second feature gate here
//! -- only the environment variable decides whether this runs.

#[allow(dead_code)]
mod support;

use everyday_core::MailQuery;
use everyday_core::account::{Account, AccountSecret, AuthMethod, Provider};
use everyday_core::store::mail::ThreadFilter;
use everyday_mail::imap::{self, Security};
use everyday_mail::session::{Credential, Flags, MailSession};
use everyday_service::mailsync::discovery::LabelMailboxes;
use everyday_service::mailsync::ingest::ThreadIndex;
use everyday_service::mailsync::passes::{self, SyncContext};

struct Config {
    host: String,
    tls_port: u16,
    user: String,
    pass: String,
}

fn config() -> Option<Config> {
    if std::env::var("EVERYDAY_TEST_IMAP").ok().filter(|v| !v.is_empty()).is_none() {
        eprintln!(
            "skipping: set EVERYDAY_TEST_IMAP (`make test-imap` sets it, and everything else \
             this needs) to run the mail sync integration suite"
        );
        return None;
    }
    let var = |name: &str, default: &str| {
        std::env::var(name).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
    };
    Some(Config {
        host: var("EVERYDAY_TEST_IMAP_HOST", "127.0.0.1"),
        tls_port: var("EVERYDAY_TEST_IMAP_TLS_PORT", "15993").parse().expect("a port number"),
        user: var("EVERYDAY_TEST_IMAP_USER", "everyday"),
        pass: var("EVERYDAY_TEST_IMAP_PASS", "testpass"),
    })
}

async fn connect(cfg: &Config) -> imap::ImapSession {
    let credential = Credential::Password { user: cfg.user.clone(), pass: cfg.pass.clone().into() };
    imap::connect_insecure_for_tests(&cfg.host, cfg.tls_port, Security::Tls, credential)
        .await
        .expect("connect and log in to the test server")
}

fn plain_message(
    subject: &str,
    message_id: &str,
    in_reply_to: Option<&str>,
    body: &str,
) -> Vec<u8> {
    let mut s = String::new();
    s.push_str("From: sender@example.com\r\n");
    s.push_str("To: everyday@example.com\r\n");
    s.push_str(&format!("Subject: {subject}\r\n"));
    s.push_str("Date: Mon, 14 Sep 2026 09:30:00 +0000\r\n");
    s.push_str(&format!("Message-Id: <{message_id}@everyday-mail.test>\r\n"));
    if let Some(parent) = in_reply_to {
        s.push_str(&format!("In-Reply-To: <{parent}@everyday-mail.test>\r\n"));
    }
    s.push_str("Content-Type: text/plain\r\n\r\n");
    s.push_str(body);
    s.push_str("\r\n");
    s.into_bytes()
}

/// A message with one small "attachment" -- plain text standing in for a
/// binary file, base64-encoded the way a real client would send one, with a
/// filename and an explicit `Content-Disposition: attachment`.
fn message_with_attachment(
    subject: &str,
    message_id: &str,
    filename: &str,
    contents: &str,
) -> Vec<u8> {
    let boundary = "everyday-test-boundary";
    let encoded = base64_encode(contents.as_bytes());
    let mut s = String::new();
    s.push_str("From: sender@example.com\r\n");
    s.push_str("To: everyday@example.com\r\n");
    s.push_str(&format!("Subject: {subject}\r\n"));
    s.push_str("Date: Mon, 14 Sep 2026 09:30:00 +0000\r\n");
    s.push_str(&format!("Message-Id: <{message_id}@everyday-mail.test>\r\n"));
    s.push_str("MIME-Version: 1.0\r\n");
    s.push_str(&format!("Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n"));
    s.push_str(&format!("--{boundary}\r\n"));
    s.push_str("Content-Type: text/plain\r\n\r\n");
    s.push_str("See the attached file.\r\n\r\n");
    s.push_str(&format!("--{boundary}\r\n"));
    s.push_str("Content-Type: application/octet-stream\r\n");
    s.push_str(&format!("Content-Disposition: attachment; filename=\"{filename}\"\r\n"));
    s.push_str("Content-Transfer-Encoding: base64\r\n\r\n");
    s.push_str(&encoded);
    s.push_str("\r\n");
    s.push_str(&format!("--{boundary}--\r\n"));
    s.into_bytes()
}

/// An invitation: a `multipart/mixed` message carrying a plain-text part and
/// a `text/calendar; method=REQUEST` part naming `attendee_email` -- the
/// shape phase 6's `process_body` reads with `everyday_mail::invite::parse_invite`.
fn invite_message(
    subject: &str,
    message_id: &str,
    uid: &str,
    organizer_email: &str,
    attendee_email: &str,
) -> Vec<u8> {
    let boundary = "everyday-invite-boundary";
    let ics = format!(
        "BEGIN:VCALENDAR\r\n\
         PRODID:-//Every Day//Test//EN\r\n\
         VERSION:2.0\r\n\
         CALSCALE:GREGORIAN\r\n\
         METHOD:REQUEST\r\n\
         BEGIN:VEVENT\r\n\
         DTSTART:20260901T100000Z\r\n\
         DTEND:20260901T103000Z\r\n\
         DTSTAMP:20260820T090000Z\r\n\
         ORGANIZER;CN=Priya Patel:mailto:{organizer_email}\r\n\
         UID:{uid}\r\n\
         ATTENDEE;CUTYPE=INDIVIDUAL;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE;\
         CN=Everyday Tester:mailto:{attendee_email}\r\n\
         SEQUENCE:0\r\n\
         STATUS:CONFIRMED\r\n\
         SUMMARY:Standup\r\n\
         END:VEVENT\r\n\
         END:VCALENDAR\r\n"
    );
    let mut s = String::new();
    s.push_str(&format!("From: {organizer_email}\r\n"));
    s.push_str(&format!("To: {attendee_email}\r\n"));
    s.push_str(&format!("Subject: {subject}\r\n"));
    s.push_str("Date: Mon, 14 Sep 2026 09:30:00 +0000\r\n");
    s.push_str(&format!("Message-Id: <{message_id}@everyday-mail.test>\r\n"));
    s.push_str("MIME-Version: 1.0\r\n");
    s.push_str(&format!("Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n"));
    s.push_str(&format!("--{boundary}\r\n"));
    s.push_str("Content-Type: text/plain\r\n\r\n");
    s.push_str("You are invited to Standup.\r\n\r\n");
    s.push_str(&format!("--{boundary}\r\n"));
    s.push_str("Content-Type: text/calendar; method=REQUEST; charset=utf-8\r\n\r\n");
    s.push_str(&ics);
    s.push_str(&format!("--{boundary}--\r\n"));
    s.into_bytes()
}

/// A minimal base64 encoder -- this crate has no dependency that offers one
/// outside `dev-dependencies`, and pulling one in for a single test fixture
/// would be a heavier fix than writing the twelve lines RFC 4648 needs.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18 & 0x3f) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6 & 0x3f) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[(n & 0x3f) as usize] as char } else { '=' });
    }
    out
}

#[tokio::test]
async fn a_real_mailbox_syncs_into_a_real_vault() {
    let Some(cfg) = config() else { return };

    let mailbox_name = format!("EverydayTest{}", std::process::id());
    let mut seed_session = connect(&cfg).await;
    seed_session.create_mailbox(&mailbox_name).await.expect("CREATE the test mailbox");
    seed_session.select(&mailbox_name).await.expect("SELECT it");

    const STANDALONE: usize = 45;
    for i in 0..STANDALONE {
        let raw = plain_message(
            &format!("Everyday test message {i}"),
            &format!("standalone-{i}"),
            None,
            &format!("Body of message {i}, mentioning bramble{}.", i % 7),
        );
        seed_session
            .append(&mailbox_name, &raw, Flags::NONE)
            .await
            .expect("APPEND a standalone message");
    }

    // A three-deep reply chain.
    let root = plain_message("Quarterly numbers", "chain-root", None, "Here they are.");
    seed_session.append(&mailbox_name, &root, Flags::NONE).await.expect("APPEND the root");
    let reply1 = plain_message(
        "Re: Quarterly numbers",
        "chain-reply-1",
        Some("chain-root"),
        "Thanks, looks good.",
    );
    seed_session.append(&mailbox_name, &reply1, Flags::NONE).await.expect("APPEND the first reply");
    let reply2 = plain_message(
        "Re: Quarterly numbers",
        "chain-reply-2",
        Some("chain-reply-1"),
        "Agreed, filing this.",
    );
    seed_session
        .append(&mailbox_name, &reply2, Flags::SEEN)
        .await
        .expect("APPEND the second reply");

    // A message with an attachment.
    let with_attachment = message_with_attachment(
        "Invoice enclosed",
        "with-attachment",
        "invoice.txt",
        "This stands in for a real invoice's bytes.",
    );
    seed_session
        .append(&mailbox_name, &with_attachment, Flags::NONE)
        .await
        .expect("APPEND the message with an attachment");

    let total_messages = STANDALONE + 4;
    drop(seed_session);

    // A real vault, with mail's storage wired exactly the way
    // `Service::set`/`Service::unlocked` wire it for a real unlock.
    let (svc, _dir) = support::vault::service(None);
    let vault = svc.get().expect("the vault `support::vault::service` just set");
    let packs = svc.packs().expect("mail's pack store should have opened for an unlocked vault");
    let index = svc.mail_index().expect("mail's search index should have opened too");
    let statuses = svc.mail_statuses().expect("and the status registry");

    let mut account = Account::new(Provider::Custom, "everyday@example.com");
    account.auth = AuthMethod::Password { username: cfg.user.clone() };
    account.imap.host = cfg.host.clone();
    account.imap.port = cfg.tls_port;
    let account_id = account.id;
    vault.save_account(&account).unwrap();
    vault
        .save_account_secret(
            account_id,
            &AccountSecret { password: Some(cfg.pass.clone()), ..Default::default() },
        )
        .unwrap();

    let ctx = SyncContext {
        vault: &vault,
        account_id,
        packs,
        index: index.clone(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: None,
        contacts: None,
        identities: vec![account.address.clone()],
    };
    let mut labels = LabelMailboxes::new(&vault, account_id);
    let mut threads = ThreadIndex::new();

    let mut sync_session = connect(&cfg).await;
    let started = std::time::Instant::now();
    let mailboxes = passes::sync_once(&ctx, &mut sync_session, &mut labels, &mut threads)
        .await
        .expect("a full sync against the real server");
    eprintln!("Dovecot sync of {total_messages} seeded messages took {:?}", started.elapsed());

    let synced = mailboxes
        .iter()
        .find(|m| m.remote_name == mailbox_name)
        .unwrap_or_else(|| panic!("the seeded mailbox should have been synced: {mailboxes:?}"));

    // ---- list ----
    let page = vault.list_threads(synced.row.id, &ThreadFilter::default(), None, 200).unwrap();
    assert_eq!(
        page.threads.len(),
        STANDALONE + 2, // the reply chain is one thread, the attachment message is another.
        "every seeded message should have landed, with the reply chain merged into one thread"
    );

    // ---- thread ----
    let root_message = vault
        .message_by_message_id_header(account_id, "chain-root@everyday-mail.test")
        .unwrap()
        .expect("the root of the reply chain should be stored");
    let (thread, messages) = vault.thread(root_message.thread_id).unwrap();
    assert_eq!(thread.message_count, 3, "the whole reply chain should be one thread");
    assert_eq!(messages.len(), 3);

    // ---- body ----
    let body = vault.body(root_message.id).unwrap();
    assert!(
        body.text.contains("Here they are."),
        "the root message's body should be readable: {body:?}"
    );

    // ---- search ----
    let hits = index.search(&MailQuery::parse("bramble3"), 10, None).unwrap();
    assert!(!hits.hits.is_empty(), "a word from a seeded body should be searchable");

    // ---- attachment ----
    let attachment_message = vault
        .message_by_message_id_header(account_id, "with-attachment@everyday-mail.test")
        .unwrap()
        .expect("the message with an attachment should be stored");
    assert!(attachment_message.has_attachments, "it should be flagged as carrying an attachment");
    let attachment_body = vault.body(attachment_message.id).unwrap();
    let part = attachment_body
        .parts
        .iter()
        .find(|p| p.filename.as_deref() == Some("invoice.txt"))
        .expect("the attachment part should be recorded");
    let blob_id = part.blob.expect("its bytes should have been stored as a blob");
    let bytes = vault.blob(blob_id).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&bytes),
        "This stands in for a real invoice's bytes.",
        "the attachment's bytes should round-trip exactly"
    );
}

// ---------------------------------------------------------------------
// Write actions, end to end: sync, archive, mark read, reply and search --
// against a real Dovecot for IMAP and a real Mailpit for SMTP.
//
// The scenario the plan asks for names `archive` specifically; this test
// uses `move_to_mailbox` into a folder created for the purpose instead,
// because the `dovecot/dovecot:latest` test image was found, empirically,
// not to persist a `CREATE ... (USE (\Archive))` request's special-use tag
// on a custom folder -- `LIST` answers with no `\Archive` attribute
// afterwards even though the server advertises the `SPECIAL-USE`
// capability and returns `OK` to the `CREATE`. `archive_thread`'s own
// server-side effect (an `OpKind::Archive` op resolving to a `MOVE`) and
// `move_to_mailbox`'s are identical once a destination mailbox is in hand;
// what this substitution loses is coverage of `Lookups::special_use`
// discovering an `Archive`-role mailbox specifically, not of a real write
// reaching a real server.
// ---------------------------------------------------------------------

/// Mailpit's own connection details -- the same environment variables
/// `crates/everyday-mail/tests/smtp_mailpit.rs` reads, and the same
/// defaults `make test-smtp` sets.
struct SmtpConfig {
    host: String,
    port: u16,
    api: String,
}

fn smtp_config() -> Option<SmtpConfig> {
    if std::env::var("EVERYDAY_TEST_SMTP").ok().filter(|v| !v.is_empty()).is_none() {
        eprintln!(
            "skipping the write-actions suite: set EVERYDAY_TEST_SMTP (`make test-smtp` sets it) \
             alongside EVERYDAY_TEST_IMAP"
        );
        return None;
    }
    let var = |name: &str, default: &str| {
        std::env::var(name).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
    };
    Some(SmtpConfig {
        host: var("EVERYDAY_TEST_SMTP_HOST", "127.0.0.1"),
        port: var("EVERYDAY_TEST_SMTP_PORT", "11025").parse().expect("a port number"),
        api: var("EVERYDAY_TEST_SMTP_API", "http://127.0.0.1:18025"),
    })
}

/// A minimal [`everyday_mail::outbox::Sender`] for this test only: connects
/// to Mailpit in the clear, the same way `smtp_mailpit.rs` does, rather
/// than through the production `mailsync::sender::LazySmtpSender`, which
/// only ever speaks TLS -- Mailpit's plaintext port has no certificate to
/// offer it. Lazy and cached on the same terms the production sender is,
/// for the same reason: most of this test's own drains never send anything.
struct TestSmtpSender {
    host: String,
    port: u16,
    cached: tokio::sync::Mutex<Option<everyday_mail::smtp::SmtpTransport>>,
}

impl TestSmtpSender {
    fn new(host: String, port: u16) -> Self {
        Self { host, port, cached: tokio::sync::Mutex::new(None) }
    }
}

#[allow(async_fn_in_trait)]
impl everyday_mail::outbox::Sender for TestSmtpSender {
    async fn send(
        &self,
        built: &everyday_mail::compose::Built,
    ) -> everyday_mail::session::Result<everyday_mail::smtp::SendReceipt> {
        let mut guard = self.cached.lock().await;
        if guard.is_none() {
            let credential = Credential::Password {
                user: "everyday".to_string(),
                pass: "testpass".to_string().into(),
            };
            let transport =
                everyday_mail::smtp::connect_plain_for_tests(&self.host, self.port, &credential)
                    .await?;
            *guard = Some(transport);
        }
        guard.as_ref().expect("just connected").send(built).await
    }
}

/// A subject nothing else running against the same throwaway Mailpit would
/// produce by accident.
fn unique_subject(label: &str) -> String {
    format!("everyday write-actions test {label} {}", uuid::Uuid::new_v4())
}

/// Polls Mailpit's `GET /api/v1/messages` for a message with `subject` --
/// see `smtp_mailpit.rs`'s own `find_delivered` for why this polls rather
/// than trusting the send call's own return to mean "visible over HTTP
/// already".
async fn wait_for_delivery(api: &str, subject: &str) {
    let client = reqwest::Client::new();
    for _ in 0..40 {
        let body: serde_json::Value = client
            .get(format!("{api}/api/v1/messages?limit=50"))
            .send()
            .await
            .expect("Mailpit's HTTP API answers")
            .json()
            .await
            .expect("a JSON message list");
        let found = body["messages"].as_array().is_some_and(|messages| {
            messages.iter().any(|m| m["Subject"].as_str() == Some(subject))
        });
        if found {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    panic!("{subject:?} never showed up in Mailpit's message list");
}

/// As [`wait_for_delivery`], but returns the message's own raw RFC 5322
/// bytes through `GET /api/v1/message/{id}/raw` once found -- the same two
/// calls `smtp_mailpit.rs`'s own `find_delivered` makes, kept as a separate
/// copy here on the same reasoning this file's `plain_message` is its own
/// copy of a shape `mailsync_dovecot.rs` and `smtp_mailpit.rs` both need: two
/// different crates' test suites, neither depending on the other's.
async fn fetch_delivered_raw(api: &str, subject: &str) -> Vec<u8> {
    let client = reqwest::Client::new();
    for _ in 0..40 {
        let body: serde_json::Value = client
            .get(format!("{api}/api/v1/messages?limit=50"))
            .send()
            .await
            .expect("Mailpit's HTTP API answers")
            .json()
            .await
            .expect("a JSON message list");
        if let Some(id) = body["messages"].as_array().and_then(|messages| {
            messages
                .iter()
                .find(|m| m["Subject"].as_str() == Some(subject))
                .and_then(|m| m["ID"].as_str())
        }) {
            return client
                .get(format!("{api}/api/v1/message/{id}/raw"))
                .send()
                .await
                .expect("Mailpit serves the raw message")
                .bytes()
                .await
                .expect("a body")
                .to_vec();
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    panic!("{subject:?} never showed up in Mailpit's message list");
}

async fn call(
    svc: &std::sync::Arc<everyday_service::Service>,
    name: &str,
    args: serde_json::Value,
) -> serde_json::Value {
    svc.call(everyday_service::ctx::Ctx::local(), name, args)
        .await
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// The `INBOX` [`everyday_core::id::MailboxId`] for `account`.
fn inbox_mailbox_id(
    vault: &everyday_core::Vault,
    account: everyday_core::id::AccountId,
) -> everyday_core::id::MailboxId {
    vault
        .mailboxes(account)
        .unwrap()
        .into_iter()
        .find(|m| m.role == everyday_core::mail::MailboxRole::Inbox)
        .expect("INBOX should have synced")
        .id
}

async fn sync_once_against(
    svc: &std::sync::Arc<everyday_service::Service>,
    vault: &std::sync::Arc<everyday_core::Vault>,
    account_id: everyday_core::id::AccountId,
    session: &mut imap::ImapSession,
) {
    let statuses = svc.mail_statuses().unwrap();
    let identities = vault
        .account(account_id)
        .map(|a| {
            std::iter::once(a.address.clone())
                .chain(a.identities.iter().map(|i| i.address.clone()))
                .collect()
        })
        .unwrap_or_default();
    let ctx = SyncContext {
        vault,
        account_id,
        packs: svc.packs().unwrap(),
        index: svc.mail_index().unwrap(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: svc.mail_unread_cache(),
        contacts: svc.mail_contacts(),
        identities,
    };
    let mut labels = LabelMailboxes::new(vault, account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&ctx, session, &mut labels, &mut threads)
        .await
        .expect("a full sync against the real server");
}

#[tokio::test]
async fn write_actions_sync_through_a_real_server() {
    let Some(cfg) = config() else { return };
    let Some(smtp) = smtp_config() else { return };

    let inbox_message_id = format!("write-actions-inbox-{}", std::process::id());
    let archive_folder = format!("EverydayArchive{}", std::process::id());

    let mut seed_session = connect(&cfg).await;
    seed_session.create_mailbox(&archive_folder).await.expect("CREATE the archive folder");
    let raw = plain_message(
        "A message to archive and read",
        &inbox_message_id,
        None,
        "please archive and read me",
    );
    seed_session.append("INBOX", &raw, Flags::NONE).await.expect("APPEND to INBOX");
    drop(seed_session);

    let (svc, _dir) = support::vault::service(None);
    let vault = svc.get().unwrap();

    let mut account = Account::new(Provider::Custom, "everyday@example.com");
    account.auth = AuthMethod::Password { username: cfg.user.clone() };
    account.imap.host = cfg.host.clone();
    account.imap.port = cfg.tls_port;
    account.smtp.host = smtp.host.clone();
    account.smtp.port = smtp.port;
    let account_id = account.id;
    vault.save_account(&account).unwrap();
    vault
        .save_account_secret(
            account_id,
            &AccountSecret { password: Some(cfg.pass.clone()), ..Default::default() },
        )
        .unwrap();

    let mut session = connect(&cfg).await;
    sync_once_against(&svc, &vault, account_id, &mut session).await;

    let seeded = vault
        .message_by_message_id_header(account_id, &format!("{inbox_message_id}@everyday-mail.test"))
        .unwrap()
        .expect("the seeded message should have synced");
    let thread_id = seeded.thread_id;

    // ---- mark read, then verify the flag on the server ----
    call(&svc, "mark_read", serde_json::json!({ "threads": [thread_id] })).await;
    let sender = TestSmtpSender::new(smtp.host.clone(), smtp.port);
    let report = everyday_service::outbox::drain_outbox(&svc, account_id, &mut session, &sender)
        .await
        .expect("draining the mark-read op");
    assert_eq!(report.failed, 0, "{report:?}");

    let mut verify_session = connect(&cfg).await;
    verify_session.select("INBOX").await.expect("SELECT INBOX to verify");
    let uid_set: everyday_mail::session::UidSet =
        vault.mail_uid_set(inbox_mailbox_id(&vault, account_id)).unwrap().into_iter().collect();
    let headers = verify_session.headers(&uid_set).await.expect("FETCH to verify flags");
    assert!(
        headers.iter().any(|h| h.flags.contains(Flags::SEEN)),
        "the server should show the message as \\Seen after the drain"
    );

    // ---- "archive" (see the module docs above this test) ----
    let archive_mailbox = vault
        .mailboxes(account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.remote_name == archive_folder)
        .expect("the archive folder should have synced as a mailbox row");
    call(
        &svc,
        "move_to_mailbox",
        serde_json::json!({ "threads": [thread_id], "to": archive_mailbox.id }),
    )
    .await;
    let report = everyday_service::outbox::drain_outbox(&svc, account_id, &mut session, &sender)
        .await
        .expect("draining the move op");
    assert_eq!(report.failed, 0, "{report:?}");

    let mut verify_session = connect(&cfg).await;
    let inbox_state = verify_session.select("INBOX").await.expect("SELECT INBOX");
    assert_eq!(inbox_state.exists, 0, "the message should have left INBOX on the server");
    let archive_state =
        verify_session.select(&archive_folder).await.expect("SELECT the archive folder");
    assert_eq!(archive_state.exists, 1, "and landed in the archive folder on the server");

    // ---- reply, send, and verify delivery and the Sent copy ----
    let reply_subject = unique_subject("reply");
    let draft = call(
        &svc,
        "new_draft",
        serde_json::json!({ "account": account_id, "inReplyTo": seeded.id }),
    )
    .await;
    let mut draft: everyday_core::mail::Draft = serde_json::from_value(draft).unwrap();
    draft.subject = reply_subject.clone();
    draft.to = vec![everyday_core::mail::Address::bare("bob@example.com")];
    draft.body_html = "<p>Reply body for the write-actions test.</p>".to_string();
    call(&svc, "save_draft", serde_json::json!({ "draft": draft.clone() })).await;
    call(&svc, "send_draft", serde_json::json!({ "id": draft.id, "delaySeconds": 0 })).await;

    // `delaySeconds: 0` is clamped up to `undo_send_delay`'s own minimum --
    // five seconds, the shortest the undo-send window is ever allowed to
    // be -- so the op is not actually due the instant `send_draft`
    // returns. Poll rather than sleep a fixed amount: the account task's
    // own `drain_until_caught_up` does the same thing in production, on
    // its own `OUTBOX_RETRY_INTERVAL`.
    // `save_draft` (above) also enqueued its own `AppendDraft` op, due
    // immediately, well before the `Send` op's undo-send delay -- so the
    // first due op this drains is that one, not `Send`. Poll until the
    // draft's own state says `Sent`, not merely until one drain call
    // attempted something.
    let mut sent = false;
    for _ in 0..20 {
        let report =
            everyday_service::outbox::drain_outbox(&svc, account_id, &mut session, &sender)
                .await
                .expect("draining the outbox");
        assert_eq!(report.failed, 0, "{report:?}");
        if matches!(vault.draft(draft.id).unwrap().state, everyday_core::mail::DraftState::Sent) {
            sent = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert!(sent, "the draft never reached DraftState::Sent");

    wait_for_delivery(&smtp.api, &reply_subject).await;

    let mut verify_session = connect(&cfg).await;
    let sent_state = verify_session.select("Sent").await.expect("SELECT Sent");
    assert!(sent_state.exists >= 1, "a copy of the sent reply should be in Sent on the server");

    // ---- search for the reply after the next sync ----
    sync_once_against(&svc, &vault, account_id, &mut session).await;
    let found = call(&svc, "search_mail", serde_json::json!({ "query": reply_subject })).await;
    let threads = found["threads"].as_array().expect("a threads array");
    assert!(
        !threads.is_empty(),
        "the reply should be searchable once the next sync has indexed the Sent copy: {found}"
    );
}

// ---------------------------------------------------------------------
// Phase 6: an invitation in mail -- parsed at sync, answered through
// `respond_to_invite`, and the reply verified against a real Mailpit.
// ---------------------------------------------------------------------

#[tokio::test]
async fn an_invitation_is_parsed_and_a_reply_is_delivered() {
    let Some(cfg) = config() else { return };
    let Some(smtp) = smtp_config() else { return };

    let invite_message_id = format!("write-actions-invite-{}", std::process::id());
    let uid = format!("everyday-test-event-{}@example.com", std::process::id());
    // Its own mailbox, not INBOX: `write_actions_sync_through_a_real_server`
    // seeds and archives exactly one message in INBOX on this same throwaway
    // Dovecot user, and cargo runs the tests in one binary concurrently by
    // default, so sharing INBOX would race the two against each other.
    let invite_folder = format!("EverydayInvite{}", std::process::id());

    let mut seed_session = connect(&cfg).await;
    seed_session.create_mailbox(&invite_folder).await.expect("CREATE the invite folder");
    let raw = invite_message(
        "You're invited: Standup",
        &invite_message_id,
        &uid,
        "priya@example.com",
        "everyday@example.com",
    );
    seed_session.append(&invite_folder, &raw, Flags::NONE).await.expect("APPEND the invitation");
    drop(seed_session);

    let (svc, _dir) = support::vault::service(None);
    let vault = svc.get().unwrap();

    let mut account = Account::new(Provider::Custom, "everyday@example.com");
    account.auth = AuthMethod::Password { username: cfg.user.clone() };
    account.imap.host = cfg.host.clone();
    account.imap.port = cfg.tls_port;
    account.smtp.host = smtp.host.clone();
    account.smtp.port = smtp.port;
    let account_id = account.id;
    vault.save_account(&account).unwrap();
    vault
        .save_account_secret(
            account_id,
            &AccountSecret { password: Some(cfg.pass.clone()), ..Default::default() },
        )
        .unwrap();

    let mut session = connect(&cfg).await;
    sync_once_against(&svc, &vault, account_id, &mut session).await;

    // ---- the invitation is parsed at sync ----
    let seeded = vault
        .message_by_message_id_header(
            account_id,
            &format!("{invite_message_id}@everyday-mail.test"),
        )
        .unwrap()
        .expect("the seeded invitation should have synced");
    let invite = seeded.invite.clone().expect("the invitation should have been parsed at sync");
    assert_eq!(invite.method, everyday_core::mail::InviteMethod::Request);
    assert_eq!(invite.uid, uid);
    assert_eq!(invite.summary, "Standup");
    assert_eq!(invite.organizer.email, "priya@example.com");
    assert_eq!(
        invite.my_response,
        Some(everyday_core::mail::AttendeeResponse::NeedsAction),
        "the seeded account's own address is one of the attendees"
    );

    // ---- accept it ----
    call(
        &svc,
        "respond_to_invite",
        serde_json::json!({ "messageId": seeded.id, "response": "accepted" }),
    )
    .await;

    let updated = vault.mail_message(seeded.id).unwrap();
    assert_eq!(
        updated.invite.and_then(|i| i.my_response),
        Some(everyday_core::mail::AttendeeResponse::Accepted),
        "the local invite should reflect the response immediately, before the reply has gone anywhere"
    );

    // ---- drain the outbox, and check Mailpit received the REPLY ----
    //
    // `respond_to_invite` queues its `Send` with the undo-send window's own
    // default -- `UNDO_SEND_DEFAULT_SECONDS`, ten seconds, per the plan's
    // "no undo delay beyond the default" -- so this polls comfortably past
    // that, not the five-second minimum `write_actions_sync_through_a_real_server`'s
    // own poll above gets away with by asking for `delaySeconds: 0`.
    let sender = TestSmtpSender::new(smtp.host.clone(), smtp.port);
    let mut delivered = false;
    for _ in 0..40 {
        let report =
            everyday_service::outbox::drain_outbox(&svc, account_id, &mut session, &sender)
                .await
                .expect("draining the outbox");
        assert_eq!(report.failed, 0, "{report:?}");
        if report.done > 0 {
            delivered = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert!(delivered, "the reply was never drained to Mailpit");

    let reply_subject = "Accepted: Standup";
    let raw_reply = fetch_delivered_raw(&smtp.api, reply_subject).await;
    let parsed = everyday_mail::mime::parse(&raw_reply).expect("the reply parses as RFC 5322");
    let calendar = parsed.calendar.expect("the reply should carry a text/calendar part");
    let calendar_text = String::from_utf8_lossy(&calendar);
    assert!(calendar_text.contains("METHOD:REPLY"), "{calendar_text}");
    assert!(calendar_text.contains(&format!("UID:{uid}")), "{calendar_text}");
    assert!(calendar_text.contains("PARTSTAT=ACCEPTED"), "{calendar_text}");
    assert!(calendar_text.contains("mailto:everyday@example.com"), "{calendar_text}");

    let raw_text = String::from_utf8_lossy(&raw_reply);
    assert!(
        raw_text.contains("method=REPLY") || raw_text.contains("method=\"REPLY\""),
        "the Content-Type parameter should name the REPLY method: {raw_text}"
    );
}
