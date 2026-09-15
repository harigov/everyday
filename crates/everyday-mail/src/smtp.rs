//! Sending, on [`lettre`](https://docs.rs/lettre)'s async, `tokio1-rustls`
//! transport — the SMTP half of the pair `docs/plans/mail.md`'s libraries
//! table names; [`crate::imap`] is the other.
//!
//! # TLS: the same rustls, the same trust store, as [`crate::imap`]
//!
//! `lettre`'s `rustls` feature pulls in its own copy of `rustls` unless the
//! version already in the workspace satisfies its requirement — it does
//! (`Cargo.lock` already carries `rustls` 0.23 for `everyday-store-postgres`
//! and [`crate::imap`], and `lettre` 0.11's `rustls` dependency is `^0.23.18`)
//! — so `cargo tree -d` shows one `rustls`, not two. What still has to be
//! chosen deliberately is *which* crypto provider and *which* trust store
//! `lettre` reaches for, because both are feature-gated and both have a
//! default that would otherwise disagree with [`crate::imap`]'s:
//!
//! * **Crypto provider: `ring`, via the `ring` feature.** Without it,
//!   `lettre` (built with `rustls-no-provider`-style caution even though
//!   that feature is not named here) would need a process-wide default
//!   [`rustls::crypto::CryptoProvider`] installed before it ever opens a
//!   connection — see `lettre`'s `rustls_crypto::crypto_provider()`. With
//!   `ring` enabled, `lettre` asks `CryptoProvider::get_default()` first
//!   and only builds its own `rustls::crypto::ring::default_provider()` if
//!   nothing is installed, which mirrors [`crate::imap::tls_config`]'s own
//!   choice not to call `install_default` — see that function's docs for
//!   why a shared process default is avoided. The two crates end up using
//!   independently constructed `ring` providers rather than one installed
//!   global, which is deliberate: neither module has to know the other
//!   exists, and "two `ring`s, built the same way, never installed
//!   globally" is one fewer thing than "one global default both modules
//!   must agree to leave alone".
//! * **Trust store: the platform's own, via the `rustls-platform-verifier`
//!   feature.** `lettre`'s `CertificateStore::Default` — what every
//!   [`connect`] here uses — resolves to the platform verifier automatically
//!   once that feature is on, in preference to `rustls-native-certs` or a
//!   bundled `webpki-roots` list; neither of the latter two crates is a
//!   dependency of this crate at all (see `Cargo.toml`), so there is no
//!   second, differently-updated root list for the two protocols this crate
//!   speaks to disagree about, matching [`crate::imap`]'s own reasoning
//!   exactly.
//! * **What is *not* enabled**: `native-tls` (a second TLS stack
//!   entirely), `webpki-roots` (a bundled root list this crate does not
//!   want), and `builder` (`lettre`'s own RFC 5322 writer — [`crate::compose`]
//!   is this crate's, built on `mail-builder` instead, so `lettre` is asked
//!   here only to open a socket and speak the SMTP verb sequence over it;
//!   [`SmtpTransport::send`] hands it already-built bytes via
//!   [`lettre::AsyncTransport::send_raw`]).
//!
//! # Pooling
//!
//! [`connect`] builds one [`lettre::AsyncSmtpTransport`] per account, with
//! the `pool` feature's small internal connection pool —
//! [`lettre::transport::smtp::PoolConfig`] — rather than opening a fresh
//! connection (and a fresh `AUTH`) for every message. The numbers chosen in
//! [`pool_config`] are deliberately modest: an account sends rarely enough
//! (a person composing, an undo-send timer firing, an auto-draft going out)
//! that `min_idle: 0` — no connection is kept open between sends — costs
//! nothing an ordinary account would notice, while `max_size: 4` is enough
//! headroom for a handful of sends in flight at once (a send and, say, the
//! outbox retrying an earlier one) without a misbehaving loop opening
//! dozens of authenticated connections against someone's mail provider.
//! `idle_timeout` matches `lettre`'s own default of sixty seconds, named
//! here rather than left implicit so a reader does not have to go looking
//! in `lettre`'s source to find it.
//!
//! # Errors
//!
//! Everything [`connect`] and [`SmtpTransport::send`] can fail with is
//! folded onto [`MailError`], the same four (well, five) shapes
//! [`crate::session`] already defines for IMAP, on the same reasoning —
//! see [`classify`] for exactly how an SMTP reply code and a `lettre`
//! failure kind land on one of them.

use std::time::Duration;

use lettre::address::Envelope as LettreEnvelope;
use lettre::transport::smtp::PoolConfig;
use lettre::transport::smtp::authentication::{Credentials as LettreCredentials, Mechanism};
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

use crate::compose::Built;
use crate::imap::Security;
use crate::session::{Credential, MailError, Result};

/// The `lettre` type this module wraps, named once so the rest of the file
/// reads as this crate's own vocabulary rather than `lettre`'s.
type LettreTransport = AsyncSmtpTransport<Tokio1Executor>;

/// How long a pooled connection may sit idle before `lettre` closes it —
/// see the module docs on pooling.
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// How many connections one account's pool may hold at once — see the
/// module docs on pooling.
const POOL_MAX_SIZE: u32 = 4;

/// One account's SMTP sender: a host, a security setting and a credential,
/// turned into a pooled `lettre` transport by [`connect`]. Cheap to clone —
/// `lettre`'s pool lives behind an `Arc` — so the account task that owns
/// one can hand copies to whatever drains the outbox concurrently.
#[derive(Clone, Debug)]
pub struct SmtpTransport {
    inner: LettreTransport,
}

/// What a successful send returns: which envelope recipients were accepted,
/// and the server's own last response line, kept for a log a person might
/// want to read later ("250 2.0.0 Ok: queued as …").
///
/// `accepted` is [`Built::envelope_to`] verbatim rather than something
/// [`SmtpTransport::send`] recomputes from the server's replies, because
/// `lettre`'s own `AsyncSmtpConnection::send` (see its docs) issues one
/// `RCPT TO` per envelope recipient in a loop and aborts the whole attempt
/// — no partial send — the moment any one of them is refused. A `send` that
/// returns `Ok` therefore means every recipient in `envelope_to`, Bcc
/// included, was accepted; there is no server-reported subset to reflect
/// back that would ever differ from it.
#[derive(Debug, Clone)]
pub struct SendReceipt {
    pub accepted: Vec<String>,
    pub server_response: String,
}

/// Connects to `host:port`, authenticates with `credential`, and proves the
/// connection actually works — not just that a socket opened — before
/// handing back a [`SmtpTransport`] ready for [`SmtpTransport::send`].
///
/// "Proves it works" is [`lettre::AsyncSmtpTransport::test_connection`]:
/// it opens (or reuses) a pooled connection, which is also the one place
/// `lettre` runs the `AUTH` exchange, and issues a `NOOP`. That is why a
/// bad password surfaces from *this* function as [`MailError::Auth`] rather
/// than silently succeeding here and only failing on the first real send —
/// an account added with the wrong password should say so immediately, the
/// same way [`crate::imap::connect`] does.
pub async fn connect(
    host: &str,
    port: u16,
    security: Security,
    credential: &Credential,
) -> Result<SmtpTransport> {
    let tls = TlsParameters::new(host.to_string()).map_err(classify)?;
    let (credentials, mechanisms) = lettre_credentials(credential);

    let transport: LettreTransport = LettreTransport::builder_dangerous(host)
        .port(port)
        .tls(match security {
            Security::Tls => Tls::Wrapper(tls),
            Security::StartTls => Tls::Required(tls),
        })
        .credentials(credentials)
        .authentication(mechanisms)
        .pool_config(pool_config())
        .build();

    match transport.test_connection().await {
        Ok(true) => Ok(SmtpTransport { inner: transport }),
        Ok(false) => Err(MailError::Protocol(
            "the server accepted authentication but refused a NOOP check".into(),
        )),
        Err(err) => Err(classify(err)),
    }
}

/// As [`connect`], but the connection is never upgraded to TLS at all —
/// plaintext throughout. Exists for exactly one caller, `make test-smtp`'s
/// throwaway Mailpit container (`crates/everyday-mail/tests/smtp_mailpit.rs`),
/// which this crate's tests reach over `127.0.0.1` with nothing in between
/// worth encrypting against; a production [`connect`] call never has a
/// `Security::None` to ask for, which is why this is a separate function
/// rather than a third [`Security`] variant nothing else would ever use.
/// Behind the same `insecure-test-tls` feature [`crate::imap`] uses for its
/// own test-only relaxation, so a normal build never contains it.
#[cfg(feature = "insecure-test-tls")]
pub async fn connect_plain_for_tests(
    host: &str,
    port: u16,
    credential: &Credential,
) -> Result<SmtpTransport> {
    let (credentials, mechanisms) = lettre_credentials(credential);

    let transport: LettreTransport = LettreTransport::builder_dangerous(host)
        .port(port)
        .tls(Tls::None)
        .credentials(credentials)
        .authentication(mechanisms)
        .pool_config(pool_config())
        .build();

    match transport.test_connection().await {
        Ok(true) => Ok(SmtpTransport { inner: transport }),
        Ok(false) => Err(MailError::Protocol(
            "the server accepted authentication but refused a NOOP check".into(),
        )),
        Err(err) => Err(classify(err)),
    }
}

impl SmtpTransport {
    /// Sends `built` and reports what the server said. See [`classify`]
    /// for how a rejection — a missing recipient, a message too large, an
    /// expired token on a connection `lettre` had to reopen from the pool —
    /// becomes a [`MailError`] the sync engine knows what to do with.
    pub async fn send(&self, built: &Built) -> Result<SendReceipt> {
        let envelope = envelope(built)?;
        let response = self.inner.send_raw(&envelope, &built.raw).await.map_err(classify)?;
        Ok(SendReceipt {
            accepted: built.envelope_to.clone(),
            server_response: response.message().collect::<Vec<_>>().join(" "),
        })
    }
}

/// A small pool per account — see the module docs.
fn pool_config() -> PoolConfig {
    PoolConfig::new().min_idle(0).max_size(POOL_MAX_SIZE).idle_timeout(POOL_IDLE_TIMEOUT)
}

/// `Credential` to `lettre`'s own credential and mechanism list: XOAUTH2
/// for an OAuth account, PLAIN-then-LOGIN — `lettre`'s
/// `DEFAULT_MECHANISMS` order, kept explicit here rather than relied on so
/// a reader does not have to look it up — for a password, so a server that
/// only understands the older LOGIN mechanism (some still do) still works.
///
/// The secret crosses from [`Credential`]'s [`zeroize::Zeroizing`] wrapper
/// into a plain, unwrapped `String` here, because that is the type
/// `lettre::transport::smtp::authentication::Credentials` accepts — the
/// same handoff [`crate::imap::authenticate`] makes to `async-imap`'s own
/// owned-`String` login and `AUTHENTICATE` calls, for the same reason: a
/// foreign crate's API is not one this crate can make zero on drop, and the
/// alternative (not authenticating at all) is not one either.
fn lettre_credentials(credential: &Credential) -> (LettreCredentials, Vec<Mechanism>) {
    match credential {
        Credential::Password { user, pass } => (
            LettreCredentials::new(user.clone(), pass.to_string()),
            vec![Mechanism::Plain, Mechanism::Login],
        ),
        Credential::XOAuth2 { user, access_token } => (
            LettreCredentials::new(user.clone(), access_token.to_string()),
            vec![Mechanism::Xoauth2],
        ),
    }
}

/// Builds the `lettre` envelope from [`Built::envelope_from`] and
/// [`Built::envelope_to`]. Only [`crate::compose::build`] produces a
/// [`Built`], and it only ever puts well-formed addresses in either field,
/// so a parse failure here means this crate's own compose step let
/// something malformed through — a bug to report as
/// [`MailError::Protocol`], not a server condition to retry or ask the
/// person to sign in again over.
fn envelope(built: &Built) -> Result<LettreEnvelope> {
    let from = address(&built.envelope_from)?;
    let to = built.envelope_to.iter().map(|a| address(a)).collect::<Result<Vec<_>>>()?;
    LettreEnvelope::new(Some(from), to)
        .map_err(|err| MailError::Protocol(format!("invalid envelope: {err}")))
}

fn address(raw: &str) -> Result<lettre::Address> {
    raw.parse().map_err(|err| MailError::Protocol(format!("invalid address {raw:?}: {err}")))
}

/// Where an SMTP failure lands on [`MailError`]:
///
/// * **A reply code is present** ([`lettre::transport::smtp::Error::status`]
///   — meaning the server actually answered, whether to `EHLO`, `AUTH`,
///   `MAIL FROM`, `RCPT TO` or `DATA` — [`classify_code`] reads the three
///   digits itself rather than trusting which SMTP verb was in flight,
///   because RFC 4954's own auth-failure codes (530, 534, 535, 538) are
///   unambiguous on their own and do not need that context. That function's
///   docs cover the rest: transient/permanent, and why a permanent error's
///   text is kept verbatim.
/// * **A timeout** ([`lettre::transport::smtp::Error::is_timeout`]) is a
///   [`MailError::Network`]: nothing said the credential or the message was
///   wrong, the far end just did not answer in time, and a retry with
///   backoff is exactly [`MailError::Network`]'s own documented meaning.
/// * **A TLS failure** ([`lettre::transport::smtp::Error::is_tls`]) becomes
///   [`MailError::Server`] rather than [`MailError::Network`]: a certificate
///   this crate does not trust is not a condition that clears up if the
///   sync engine simply tries again a minute later, and the underlying
///   `rustls` error text names what actually went wrong, which is worth
///   keeping verbatim the way a `NO`/`BAD` response's text is.
/// * **A client or response-parsing failure**
///   ([`lettre::transport::smtp::Error::is_client`] /
///   [`is_response`](lettre::transport::smtp::Error::is_response)) is
///   [`MailError::Protocol`]: this crate (or `lettre` underneath it)
///   disagreed with the server about the protocol itself — a response it
///   could not parse, an envelope this crate built that the server or
///   `lettre` rejected before a single command went out.
/// * **Anything else** — a connection that closed, a DNS lookup that
///   failed, `lettre`'s own transport having been shut down — is
///   [`MailError::Network`], on the same "nothing here says the credential
///   was wrong, try again" reasoning as a timeout.
fn classify(err: lettre::transport::smtp::Error) -> MailError {
    if let Some(code) = err.status() {
        return classify_code(u16::from(code), &err.to_string());
    }
    if err.is_timeout() {
        return MailError::Network(err.to_string());
    }
    if err.is_tls() {
        return MailError::Server(err.to_string());
    }
    if err.is_client() || err.is_response() {
        return MailError::Protocol(err.to_string());
    }
    MailError::Network(err.to_string())
}

/// RFC 4954 §6 names four codes for an authentication problem specifically
/// (530 auth required, 534 mechanism too weak, 535 credentials invalid, 538
/// encryption required) — anything else is classified by severity alone,
/// RFC 5321 §4.2.1's own rule: `4yz` is transient (a retry is the right
/// answer, which is exactly what [`MailError::Network`]'s docs already say
/// about it), `5yz` is permanent and kept **verbatim** — `text` is the
/// server's own reply, joined, which is `lettre`'s
/// [`Response::message_joined`](lettre::transport::smtp::response::Response)
/// wrapped into this error's `Display` — because that text is frequently
/// the entire point: "552 message too large" needs no rewriting to be
/// useful, and a rejected recipient's own address is usually right there in
/// the server's sentence about it ("550 5.1.1 <bob@example.com>: Recipient
/// address rejected: User unknown"), which this function has no better way
/// to name than to pass along whole.
fn classify_code(code: u16, text: &str) -> MailError {
    if matches!(code, 530 | 534 | 535 | 538) {
        return MailError::Auth(text.to_string());
    }
    match code / 100 {
        4 => MailError::Network(text.to_string()),
        5 => MailError::Server(text.to_string()),
        _ => MailError::Protocol(text.to_string()),
    }
}

/// Whether the sync engine needs to `APPEND` a sent message to the
/// account's Sent mailbox itself.
///
/// Gmail's own SMTP submission saves a copy to `[Gmail]/Sent Mail`
/// automatically — this crate must not also `APPEND` one, or every sent
/// message would show up twice. Every other provider this crate has met
/// does not, so the default for an account whose IMAP session did not
/// report `X-GM-EXT-1` — [`crate::session::Capabilities::gmail`], read once
/// at connect time — is `true`. Takes the capability rather than a
/// provider enum because the question this function answers is entirely
/// about what the *server* already does, which `X-GM-EXT-1` answers
/// directly; a `Provider::Google` value on the account record could in
/// principle be lying (a Google Workspace domain proxied through something
/// else, say), and the capability cannot be.
pub fn needs_sent_append(capabilities: &crate::session::Capabilities) -> bool {
    !capabilities.gmail
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Capabilities;

    #[test]
    fn gmail_saves_its_own_sent_copy() {
        let caps = Capabilities { gmail: true, ..Capabilities::default() };
        assert!(!needs_sent_append(&caps));
    }

    #[test]
    fn everyone_else_needs_an_append() {
        let caps = Capabilities { gmail: false, ..Capabilities::default() };
        assert!(needs_sent_append(&caps));
    }

    #[test]
    fn rfc_4954_auth_codes_are_distinguished_from_other_five_hundreds() {
        assert!(matches!(classify_code(535, "bad creds"), MailError::Auth(_)));
        assert!(matches!(classify_code(530, "auth required"), MailError::Auth(_)));
        assert!(matches!(classify_code(534, "mechanism too weak"), MailError::Auth(_)));
        assert!(matches!(classify_code(538, "encryption required"), MailError::Auth(_)));
    }

    #[test]
    fn four_hundreds_are_network_errors_the_engine_should_retry() {
        assert!(matches!(classify_code(452, "mailbox full, try later"), MailError::Network(_)));
        assert!(matches!(classify_code(421, "service not available"), MailError::Network(_)));
    }

    #[test]
    fn five_hundreds_are_surfaced_verbatim() {
        match classify_code(552, "552 message too large") {
            MailError::Server(text) => assert!(text.contains("message too large")),
            other => panic!("expected Server, got {other:?}"),
        }
    }

    #[test]
    fn a_rejected_recipient_is_named_in_the_surfaced_text() {
        let text = "550 5.1.1 <bob@example.com>: Recipient address rejected: User unknown";
        match classify_code(550, text) {
            MailError::Server(t) => assert!(t.contains("bob@example.com")),
            other => panic!("expected Server, got {other:?}"),
        }
    }

    #[test]
    fn addresses_parse_and_reject_the_obviously_malformed() {
        assert!(address("bob@example.com").is_ok());
        assert!(address("not-an-address").is_err());
    }
}
