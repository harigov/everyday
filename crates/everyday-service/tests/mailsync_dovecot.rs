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
