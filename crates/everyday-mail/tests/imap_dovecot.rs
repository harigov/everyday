//! The [`crate::imap`](everyday_mail::imap) adapter against a real IMAP
//! server: `make test-imap` starts a throwaway Dovecot container (see the
//! `Makefile`) with CONDSTORE, QRESYNC, IDLE, MOVE, UIDPLUS and SPECIAL-USE
//! all on by default, seeds nothing (this file seeds its own messages, so a
//! run is repeatable without a fixture to keep in step), and removes the
//! container when the tests finish.
//!
//! Two things have to both be true for this file to do anything:
//!
//! * **`EVERYDAY_TEST_IMAP` is set.** Without a server there is nothing to
//!   test against, so — the same rule
//!   `everyday-store-postgres/tests/conformance.rs` follows — this prints
//!   why it did nothing and passes rather than failing or, worse, silently
//!   skipping without a word.
//! * **The `insecure-test-tls` feature is enabled.** Dovecot's container
//!   presents a self-signed certificate nothing on a test machine has been
//!   told to trust, and connecting to it needs the verifier that feature
//!   gates — see `connect_insecure_for_tests` in `src/imap.rs`. Without the
//!   feature this file still compiles (so a plain `cargo test -p
//!   everyday-mail` never fails here), it just has nothing to run.
//!
//! `make test-imap` sets both.

#[cfg(not(feature = "insecure-test-tls"))]
#[test]
fn skipped_without_insecure_test_tls_feature() {
    eprintln!(
        "skipping: run `make test-imap`, or `cargo test -p everyday-mail --features \
         insecure-test-tls --test imap_dovecot` against a server started by hand, to run the \
         IMAP integration suite"
    );
}

#[cfg(feature = "insecure-test-tls")]
mod live {
    use std::time::Duration;

    use everyday_mail::imap::{self, Security};
    use everyday_mail::session::{Credential, Flags, MailSession, SyncCursor, UidSet};
    use tokio::sync::watch;

    /// Where the throwaway server is and who to log in as. `make test-imap`
    /// sets every one of these; a developer pointing this file at a server
    /// they started by hand can set the same variables.
    struct Config {
        host: String,
        tls_port: u16,
        starttls_port: u16,
        user: String,
        pass: String,
    }

    fn config() -> Option<Config> {
        if std::env::var("EVERYDAY_TEST_IMAP").ok().filter(|v| !v.is_empty()).is_none() {
            eprintln!(
                "skipping: set EVERYDAY_TEST_IMAP (and the EVERYDAY_TEST_IMAP_* variables `make \
                 test-imap` sets) to run the IMAP integration suite"
            );
            return None;
        }
        let var = |name: &str, default: &str| {
            std::env::var(name)
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| default.to_string())
        };
        Some(Config {
            host: var("EVERYDAY_TEST_IMAP_HOST", "127.0.0.1"),
            tls_port: var("EVERYDAY_TEST_IMAP_TLS_PORT", "15993").parse().expect("a port number"),
            starttls_port: var("EVERYDAY_TEST_IMAP_STARTTLS_PORT", "15143")
                .parse()
                .expect("a port number"),
            user: var("EVERYDAY_TEST_IMAP_USER", "everyday"),
            pass: var("EVERYDAY_TEST_IMAP_PASS", "testpass"),
        })
    }

    fn credential(cfg: &Config) -> Credential {
        Credential::Password { user: cfg.user.clone(), pass: cfg.pass.clone().into() }
    }

    /// A message with a subject nothing in this file would produce by
    /// accident, so a test can tell its own seeded messages apart from
    /// whatever a previous, differently-configured run of the container
    /// left behind.
    fn message(subject_suffix: &str) -> Vec<u8> {
        format!(
            "From: sender@example.com\r\n\
             To: {}@example.com\r\n\
             Subject: everyday-mail test {subject_suffix}\r\n\
             Date: Mon, 14 Sep 2026 09:30:00 +0000\r\n\
             Message-Id: <{subject_suffix}@everyday-mail.test>\r\n\
             \r\n\
             Body for {subject_suffix}.\r\n",
            "everyday"
        )
        .into_bytes()
    }

    /// `CREATE`, ignoring "already exists" — every test wants a mailbox
    /// that is there, not proof that it just made one.
    async fn ensure_mailbox(session: &mut imap::ImapSession, name: &str) {
        session.create_mailbox(name).await.expect("CREATE (or it already existing)");
    }

    async fn connect(cfg: &Config, security: Security, port: u16) -> imap::ImapSession {
        imap::connect_insecure_for_tests(&cfg.host, port, security, credential(cfg))
            .await
            .expect("connect and log in to the test server")
    }

    #[tokio::test]
    async fn login_list_select_and_capabilities() {
        let Some(cfg) = config() else { return };
        let mut session = connect(&cfg, Security::Tls, cfg.tls_port).await;
        assert!(session.capabilities().condstore, "Dovecot advertises CONDSTORE");
        assert!(session.capabilities().idle, "Dovecot advertises IDLE");
        assert!(session.capabilities().move_, "Dovecot advertises MOVE");
        assert!(session.capabilities().uidplus, "Dovecot advertises UIDPLUS");

        ensure_mailbox(&mut session, "INBOX").await;
        let mailboxes = session.mailboxes().await.expect("LIST");
        assert!(mailboxes.iter().any(|m| m.name.eq_ignore_ascii_case("INBOX")), "{mailboxes:?}");

        let state = session.select("INBOX").await.expect("SELECT");
        assert!(state.uidvalidity > 0);
    }

    #[tokio::test]
    async fn starttls_also_logs_in() {
        let Some(cfg) = config() else { return };
        let mut session = connect(&cfg, Security::StartTls, cfg.starttls_port).await;
        session.select("INBOX").await.expect("SELECT over a STARTTLS connection");
    }

    #[tokio::test]
    async fn append_headers_and_raw_round_trip() {
        let Some(cfg) = config() else { return };
        let mut session = connect(&cfg, Security::Tls, cfg.tls_port).await;
        ensure_mailbox(&mut session, "INBOX").await;
        session.select("INBOX").await.expect("SELECT");

        let raw = message("round-trip");
        let uid = session
            .append("INBOX", &raw, Flags::SEEN)
            .await
            .expect("APPEND")
            .expect("Dovecot supports UIDPLUS, so APPENDUID names the uid");

        let uids = UidSet::single(uid);
        let headers = session.headers(&uids).await.expect("FETCH headers");
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].uid, uid);
        assert!(headers[0].flags.contains(Flags::SEEN));
        assert!(
            String::from_utf8_lossy(&headers[0].header).contains("everyday-mail test round-trip")
        );

        let mut stream = session.raw(&uids).await.expect("start the raw fetch");
        let mut collected = Vec::new();
        while let Some(item) = futures::StreamExt::next(&mut stream).await {
            collected.push(item.expect("a raw message"));
        }
        drop(stream);
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0].0, uid);
        // Dovecot is allowed to add its own trailing CRLF bookkeeping; what
        // matters is that the message this test wrote is in there intact.
        assert!(String::from_utf8_lossy(&collected[0].1).contains("Body for round-trip."));
    }

    #[tokio::test]
    async fn flag_change_is_seen_through_changedsince() {
        let Some(cfg) = config() else { return };
        let mut session = connect(&cfg, Security::Tls, cfg.tls_port).await;
        ensure_mailbox(&mut session, "INBOX").await;
        session.select("INBOX").await.expect("SELECT");

        let uid = session
            .append("INBOX", &message("flag-change"), Flags::NONE)
            .await
            .expect("APPEND")
            .expect("APPENDUID");

        let state = session.select("INBOX").await.expect("re-SELECT for a fresh modseq");
        let cursor = SyncCursor {
            uidvalidity: state.uidvalidity,
            highest_uid_seen: uid,
            highestmodseq: state.highestmodseq,
        };
        let known = UidSet::single(uid);

        session.store_flags(&known, Flags::FLAGGED, Flags::NONE).await.expect("STORE");

        let changes = session.changes_since(&cursor, &known).await.expect("changes_since");
        assert!(!changes.uidvalidity_reset);
        let (_, flags, modseq) = changes
            .flag_changes
            .iter()
            .find(|(u, _, _)| *u == uid)
            .expect("the flag change is reported");
        assert!(flags.contains(Flags::FLAGGED));
        assert!(modseq.is_some(), "Dovecot supports CONDSTORE, so a modseq comes back");
    }

    #[tokio::test]
    async fn move_is_seen_as_a_vanished_uid() {
        let Some(cfg) = config() else { return };
        let mut session = connect(&cfg, Security::Tls, cfg.tls_port).await;
        ensure_mailbox(&mut session, "INBOX").await;
        ensure_mailbox(&mut session, "Archive").await;
        let state = session.select("INBOX").await.expect("SELECT");

        let uid = session
            .append("INBOX", &message("move-me"), Flags::NONE)
            .await
            .expect("APPEND")
            .expect("APPENDUID");
        // Re-select: APPEND on the selected mailbox changes EXISTS, and this
        // test wants a cursor from before the move, not before the append.
        let state = session.select("INBOX").await.unwrap_or(state);
        let cursor = SyncCursor {
            uidvalidity: state.uidvalidity,
            highest_uid_seen: uid,
            highestmodseq: state.highestmodseq,
        };
        let known = UidSet::single(uid);

        session.move_to(&known, "Archive").await.expect("MOVE");

        let changes = session.changes_since(&cursor, &known).await.expect("changes_since");
        assert!(changes.vanished.contains(uid), "{changes:?}");
    }

    #[tokio::test]
    async fn idle_wakes_on_an_append_from_a_second_connection() {
        let Some(cfg) = config() else { return };
        let mut idler = connect(&cfg, Security::Tls, cfg.tls_port).await;
        ensure_mailbox(&mut idler, "INBOX").await;
        idler.select("INBOX").await.expect("SELECT");

        let (_stop_tx, stop_rx) = watch::channel(());
        let idle_task = tokio::spawn(async move {
            let event = idler.idle(stop_rx).await;
            (idler, event)
        });

        // Give the IDLE command time to reach the server before the second
        // connection's APPEND is meant to wake it.
        tokio::time::sleep(Duration::from_millis(500)).await;

        let mut writer = connect(&cfg, Security::Tls, cfg.tls_port).await;
        writer.select("INBOX").await.expect("SELECT on the second connection");
        writer
            .append("INBOX", &message("wake-idle"), Flags::NONE)
            .await
            .expect("APPEND")
            .expect("APPENDUID");

        let (_idler, event) = tokio::time::timeout(Duration::from_secs(10), idle_task)
            .await
            .expect("IDLE woke within ten seconds")
            .expect("the IDLE task did not panic");
        assert!(matches!(event, Ok(everyday_mail::session::IdleEvent::Activity)), "{event:?}");
    }

    #[tokio::test]
    async fn idle_stops_cleanly_on_the_stop_signal() {
        let Some(cfg) = config() else { return };
        let mut session = connect(&cfg, Security::Tls, cfg.tls_port).await;
        ensure_mailbox(&mut session, "INBOX").await;
        session.select("INBOX").await.expect("SELECT");

        let (stop_tx, stop_rx) = watch::channel(());
        stop_tx.send(()).expect("the watch channel is open");

        let event = tokio::time::timeout(Duration::from_secs(10), session.idle(stop_rx))
            .await
            .expect("idle() returns promptly when already told to stop")
            .expect("idle() does not error on a clean stop");
        assert_eq!(event, everyday_mail::session::IdleEvent::Stopped);
    }
}
