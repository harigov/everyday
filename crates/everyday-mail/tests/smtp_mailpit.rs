//! [`crate::smtp`](everyday_mail::smtp) against a real SMTP server:
//! `make test-smtp` starts a throwaway [Mailpit](https://mailpit.axllent.org)
//! container (see the `Makefile`) with exactly one SMTP credential
//! (`everyday:testpass`), so this suite can prove both that the right
//! password is accepted and that a wrong one comes back as this crate's
//! own [`MailError::Auth`], and reads back what was actually delivered
//! through Mailpit's own HTTP API rather than trusting this crate's own
//! account of what it sent.
//!
//! Two things have to both be true for this file to do anything — the same
//! two-part gate `tests/imap_dovecot.rs` uses, for the same reasons:
//!
//! * **`EVERYDAY_TEST_SMTP` is set.** Without a server there is nothing to
//!   send to, so this prints why it did nothing and passes rather than
//!   failing or silently skipping without a word.
//! * **The `insecure-test-tls` feature is enabled**, so this file can reach
//!   [`smtp::connect_plain_for_tests`]. Without the feature this file still
//!   compiles, it just has nothing to run.
//!
//! `make test-smtp` sets both.
//!
//! # Why this suite does not also exercise TLS
//!
//! [`smtp::connect`]'s `Security::Tls`/`Security::StartTls` paths use the
//! exact same `tokio1-rustls` and `rustls-platform-verifier` configuration
//! `crate::imap::connect` already proves works end-to-end against a real
//! server in `tests/imap_dovecot.rs`. Standing up a second TLS-terminating
//! throwaway container to prove the same crypto and trust-store wiring a
//! second time would cost real complexity (Mailpit's SMTP TLS needs a
//! certificate whose Subject Alternative Names are DNS names, not the IP
//! address a Docker port mapping is reached by, which is a mismatch this
//! crate's real [`connect`] would rightly refuse under real verification)
//! for no additional coverage of `smtp.rs`'s own code, which does not
//! branch on which TLS mode was chosen once a socket is open. This suite
//! instead uses [`smtp::connect_plain_for_tests`] and spends its effort on
//! what is specific to SMTP: the verb sequence, envelope construction, and
//! this module's own [`MailError`] mapping.

#[cfg(not(feature = "insecure-test-tls"))]
#[test]
fn skipped_without_insecure_test_tls_feature() {
    eprintln!(
        "skipping: run `make test-smtp`, or `cargo test -p everyday-mail --features \
         insecure-test-tls --test smtp_mailpit` against a server started by hand, to run the \
         SMTP integration suite"
    );
}

#[cfg(feature = "insecure-test-tls")]
mod live {
    use std::time::Duration;

    use everyday_mail::compose::{self, Address, Outgoing};
    use everyday_mail::session::{Credential, MailError};
    use everyday_mail::smtp;

    struct Config {
        host: String,
        port: u16,
        api: String,
    }

    fn config() -> Option<Config> {
        if std::env::var("EVERYDAY_TEST_SMTP").ok().filter(|v| !v.is_empty()).is_none() {
            eprintln!(
                "skipping: set EVERYDAY_TEST_SMTP (and the EVERYDAY_TEST_SMTP_* variables `make \
                 test-smtp` sets) to run the SMTP integration suite"
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
            host: var("EVERYDAY_TEST_SMTP_HOST", "127.0.0.1"),
            port: var("EVERYDAY_TEST_SMTP_PORT", "11025").parse().expect("a port number"),
            api: var("EVERYDAY_TEST_SMTP_API", "http://127.0.0.1:18025"),
        })
    }

    fn correct_credential() -> Credential {
        Credential::Password { user: "everyday".to_string(), pass: "testpass".to_string().into() }
    }

    /// A subject nothing else running against the same throwaway Mailpit
    /// would produce by accident, so [`find_delivered`] can tell this
    /// test's own message apart from any other test's -- Mailpit is one
    /// shared inbox, and `cargo test` runs these concurrently.
    fn unique_subject(label: &str) -> String {
        format!("everyday-mail smtp test {label} {}", uuid::Uuid::new_v4())
    }

    fn outgoing(subject: &str) -> Outgoing {
        Outgoing {
            from: Address::named("Everyday Test", "sender@example.com"),
            to: vec![Address::named("Bob", "bob@example.com")],
            cc: vec![Address::new("carol-cc@example.com")],
            bcc: vec![Address::new("dave-bcc@example.com")],
            reply_to: Vec::new(),
            subject: subject.to_string(),
            html: "<p>Hello from the everyday-mail SMTP suite.</p>".to_string(),
            text: None,
            attachments: Vec::new(),
            in_reply_to: None,
            references: Vec::new(),
            message_id_domain: "example.com".to_string(),
            message_id: None,
        }
    }

    /// Polls Mailpit's `GET /api/v1/messages` for a message with `subject`,
    /// then fetches its raw source through `GET /api/v1/message/{id}/raw`.
    /// A short poll rather than one shot: delivery and the HTTP API are two
    /// different code paths inside Mailpit, and nothing here promises they
    /// finish in the same instant `send` returns.
    async fn find_delivered(api: &str, subject: &str) -> Vec<u8> {
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
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        panic!("{subject:?} never showed up in Mailpit's message list");
    }

    #[tokio::test]
    async fn a_sent_message_is_delivered_and_parses() {
        let Some(cfg) = config() else { return };
        let transport = smtp::connect_plain_for_tests(&cfg.host, cfg.port, &correct_credential())
            .await
            .expect("connect and authenticate");

        let subject = unique_subject("delivery");
        let built = compose::build(&outgoing(&subject)).expect("build the message");

        let receipt = transport.send(&built).await.expect("send");
        assert_eq!(receipt.accepted, built.envelope_to);

        let raw = find_delivered(&cfg.api, &subject).await;
        let parsed = everyday_mail::mime::parse(&raw).expect("the delivered message parses");
        assert_eq!(parsed.subject.as_deref(), Some(subject.as_str()));
        assert_eq!(parsed.to[0].email.as_deref(), Some("bob@example.com"));
    }

    #[tokio::test]
    async fn bcc_is_delivered_but_never_written_by_this_crate() {
        let Some(cfg) = config() else { return };
        let transport = smtp::connect_plain_for_tests(&cfg.host, cfg.port, &correct_credential())
            .await
            .expect("connect and authenticate");

        let subject = unique_subject("bcc");
        let built = compose::build(&outgoing(&subject)).expect("build the message");
        assert!(built.envelope_to.contains(&"dave-bcc@example.com".to_string()));

        // What this crate actually put on the wire, checked before a
        // single byte of it left the process: no "Bcc:" line at all. This
        // is the assertion `compose::tests::build_omits_bcc_from_the_written_headers`
        // already makes; repeated here against the exact bytes this test
        // then hands to a real SMTP session, so the two are provably the
        // same claim about the same bytes.
        let sent_text = String::from_utf8_lossy(&built.raw);
        assert!(!sent_text.to_ascii_lowercase().contains("bcc:"), "{sent_text}");

        // A successful `send` proves RCPT TO accepted the Bcc recipient --
        // see `SendReceipt::accepted`'s docs -- so "was delivered" is
        // established the moment this does not error.
        transport.send(&built).await.expect("send, including the Bcc recipient");

        // Mailpit is the final destination for every envelope recipient,
        // Bcc included, and -- deliberately, to help *other* people debug
        // senders that get this wrong -- adds its own "Bcc:" header to its
        // stored copy for any RCPT TO address that was not in the
        // message's own To/Cc/Bcc headers (see mailpit's own
        // `internal/smtpd/main.go`, `mailHandler`). That addition is
        // Mailpit's, not this crate's: it is independent proof that the
        // envelope this crate built the Bcc recipient into really did
        // reach Mailpit's SMTP session even though `built.raw`, asserted
        // above, never named it.
        let raw = find_delivered(&cfg.api, &subject).await;
        let raw_text = String::from_utf8_lossy(&raw);
        assert!(raw_text.contains("dave-bcc@example.com"), "{raw_text}");
    }

    #[tokio::test]
    async fn a_wrong_password_is_reported_as_an_auth_error() {
        let Some(cfg) = config() else { return };
        let wrong = Credential::Password {
            user: "everyday".to_string(),
            pass: "not-the-password".to_string().into(),
        };

        let err = smtp::connect_plain_for_tests(&cfg.host, cfg.port, &wrong)
            .await
            .expect_err("a wrong password must not connect successfully");
        assert!(matches!(err, MailError::Auth(_)), "{err:?}");
    }
}
