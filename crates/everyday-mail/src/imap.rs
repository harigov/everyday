//! [`session::MailSession`](crate::session::MailSession), built on
//! [`async-imap`](https://docs.rs/async-imap) over `tokio-rustls`.
//!
//! Three things here are worth explaining before the rest of the file makes
//! sense.
//!
//! # Why `async-imap`, without QRESYNC
//!
//! `async-imap` does not speak RFC 7162's `QRESYNC`, which is the extension
//! that would let [`ImapSession::changes_since`] ask the server for exactly
//! what changed and get `VANISHED` ranges back instead of working them out.
//! It was chosen anyway — see `docs/plans/mail.md`'s "Decisions already
//! made" — because it is mature, it is what Delta Chat ships, and the seam
//! is [`crate::session::MailSession`], not this module: an `io-imap`
//! adapter, when that crate settles, drops in without the sync engine
//! noticing. Until then, [`ImapSession::changes_since`] gets the same
//! answer from cheaper parts: `SELECT (CONDSTORE)` at [`ImapSession::select`]
//! time, `UID FETCH 1:* (FLAGS) (CHANGEDSINCE …)` for what changed, and a
//! `UID SEARCH` diffed against the caller's own known-UID set for what
//! vanished. At the cadence a real mailbox changes — driven by `IDLE`, not
//! polled — the extra round trip this costs over `QRESYNC` is not one this
//! crate's speed budget (`docs/plans/mail.md`, "The speed budget") has ever
//! had to account for.
//!
//! # Getting `X-GM-THRID` out of a library that does not expose it
//!
//! `imap-proto` — the parser `async-imap` is built on — already understands
//! `X-GM-THRID`: it is `AttributeValue::GmailThrId` in
//! `imap_proto::types`. `async-imap`'s typed [`async_imap::types::Fetch`]
//! simply never grew an accessor for it (only `gmail_labels()` and
//! `gmail_msg_id()` did), and `Fetch`'s one constructor is private to that
//! crate, so there is no way to build one from a `X-GM-THRID` line by hand.
//!
//! The way out is that [`async_imap::Session::run_command`] and
//! [`async_imap::Session::read_response`] — the primitives the typed
//! methods are themselves built from — are both public. [`ImapSession::headers`]
//! uses them directly: it sends its own `UID FETCH … (… X-GM-THRID
//! X-GM-MSGID X-GM-LABELS)` and reads each untagged `FETCH` response's
//! `imap_proto::types::Response::Fetch(_, attrs)` itself, pulling
//! `GmailThrId` out of `attrs` along with everything else. [`ImapSession::raw`]
//! and [`ImapSession::append`] use the same pair of primitives for their own
//! reasons — see their docs — so this crate ends up with one small
//! "run a command, read responses until the tagged one" routine
//! ([`run_fetch_command`]) rather than three.
//!
//! # TLS
//!
//! `rustls` 0.23 with the `ring` provider, matching the version this
//! workspace already pins for `everyday-store-postgres` (see that crate's
//! `Cargo.lock` entry) so there is one `ring` in the binary. Trust is the
//! platform's own store via `rustls-platform-verifier`, the same choice
//! `everyday-store-postgres` makes and for the same reason: a corporate
//! root already trusted for a work calendar or a work database is trusted
//! for work mail too, rather than a second, bundled root list
//! (`webpki-roots`) with its own update cadence to keep in step with.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use async_imap::Session as ImapLibSession;
use async_imap::error::Error as ImapLibError;
use futures::TryStreamExt;
use futures::stream;
use imap_proto::{
    AttributeValue, MailboxDatum, MessageSection, NameAttribute, Response as ImapResponse,
    ResponseCode, SectionPath, Status, UidSetMember,
};
use jiff::Timestamp;
use rustls_platform_verifier::BuilderVerifierExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::watch;

use crate::session::{
    Capabilities, Changes, Credential, Flags, GmailMeta, IdleEvent, MailError, MailSession,
    MailboxState, RawStream, RemoteHeader, RemoteMailbox, Result, Role, SyncCursor, Uid, UidSet,
};

/// The stream type every [`ImapSession`] carries, after TLS: a plain
/// `TcpStream` for [`Security::StartTls`] is only ever seen inside
/// [`connect`], upgraded before an [`ImapSession`] is built, so the rest of
/// this module is written against one concrete type rather than generic
/// over the transport.
type TlsStream = tokio_rustls::client::TlsStream<TcpStream>;

/// How to reach the server: already encrypted, or plaintext upgraded with
/// `STARTTLS` before any credential crosses the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// Connect straight into TLS — port 993 for almost every provider.
    Tls,
    /// Connect in the clear and immediately issue `STARTTLS` — port 143.
    /// [`connect`] never sends a credential before this completes.
    StartTls,
}

/// Which certificate this connection is willing to trust — kept separate
/// from [`Security`], which is only about *when* TLS happens (before or
/// after `STARTTLS`), not which certificate is acceptable once it has.
#[derive(Clone, Copy)]
enum Verifier {
    /// The platform's own trust store, via `rustls-platform-verifier`.
    /// What [`connect`] always uses.
    Platform,
    /// Trust whatever certificate the server presents, without checking its
    /// chain or hostname. What [`connect_insecure_for_tests`] always uses,
    /// and the only place this variant is ever constructed.
    #[cfg(feature = "insecure-test-tls")]
    InsecureTestOnly,
}

/// The batch size for [`ImapSession::raw`]'s `BODY.PEEK[]` fetches.
///
/// Large enough that a hundred-thousand-message backfill is not a hundred
/// thousand round trips; small enough that a batch of full messages —
/// attachments and all — does not hold an unbounded amount of the mailbox
/// in memory at once while the pack-store writer on the other end of the
/// stream catches up.
const RAW_BATCH_SIZE: usize = 20;

/// How long an `IDLE` is allowed to sit before [`ImapSession::idle`] ends it
/// with `DONE` and starts another. RFC 2177 advises re-issuing well inside
/// thirty minutes to avoid a server-side inactivity timeout;
/// `async-imap`'s own advice is twenty-nine, and twenty-five leaves margin
/// for the round trip the re-issue itself takes.
const IDLE_REISSUE: Duration = Duration::from_secs(25 * 60);

/// One live, authenticated IMAP connection: the [`crate::session::MailSession`]
/// implementation over `async-imap`.
///
/// `session` is an [`Option`] rather than a plain field because
/// [`async_imap::Session::idle`] takes the session *by value* and hands it
/// back on [`async_imap::extensions::idle::Handle::done`] — see
/// [`ImapSession::idle`]. Every other method takes it out with
/// [`ImapSession::session_mut`] and never sees it missing, because nothing
/// else in this type gives it up.
pub struct ImapSession {
    session: Option<ImapLibSession<TlsStream>>,
    capabilities: Capabilities,
    /// `SPECIAL-USE` (RFC 6154), checked once at connect time so
    /// [`ImapSession::mailboxes`] only asks `LIST` to `RETURN (SPECIAL-USE)`
    /// on a server that understands the modifier.
    special_use: bool,
    /// What [`ImapSession::select`] last returned, so
    /// [`ImapSession::changes_since`] can compare its cursor's
    /// `UIDVALIDITY` against the mailbox's current one without a further
    /// round trip.
    selected: Option<MailboxState>,
}

impl ImapSession {
    fn session_mut(&mut self) -> Result<&mut ImapLibSession<TlsStream>> {
        self.session.as_mut().ok_or_else(|| {
            MailError::Protocol("the session is mid-IDLE and was not returned".into())
        })
    }

    fn take_session(&mut self) -> Result<ImapLibSession<TlsStream>> {
        self.session.take().ok_or_else(|| {
            MailError::Protocol("the session is mid-IDLE and was not returned".into())
        })
    }

    async fn fetch_raw_batch(&mut self, batch: &UidSet) -> Result<Vec<(Uid, Vec<u8>)>> {
        let command = format!("UID FETCH {} (UID BODY.PEEK[])", batch.to_imap());
        let mut out = Vec::new();
        let session = self.session_mut()?;
        run_fetch_command(session, &command, |_seq, attrs| {
            let mut uid = None;
            let mut body = None;
            for attr in attrs {
                match attr {
                    AttributeValue::Uid(v) => uid = Some(*v),
                    AttributeValue::BodySection { section: None, data: Some(bytes), .. } => {
                        body = Some(bytes.as_ref().to_vec());
                    }
                    _ => {}
                }
            }
            if let (Some(uid), Some(body)) = (uid, body) {
                out.push((uid, body));
            }
        })
        .await?;
        Ok(out)
    }

    /// Create a mailbox, treating "it already exists" as success.
    ///
    /// Not part of [`MailSession`]: the sync engine never creates a
    /// mailbox — every one it touches already exists, named by the
    /// account's own [`MailSession::mailboxes`] — so this stays a small
    /// extra on the adapter itself, for account setup and for this crate's
    /// own integration tests (`tests/imap_dovecot.rs`), rather than a
    /// method every future adapter would have to implement for no caller.
    pub async fn create_mailbox(&mut self, name: &str) -> Result<()> {
        let session = self.session_mut()?;
        match session.create(name).await {
            Ok(()) => Ok(()),
            Err(ImapLibError::No(_)) => Ok(()),
            Err(e) => Err(classify(e)),
        }
    }
}

impl MailSession for ImapSession {
    async fn mailboxes(&mut self) -> Result<Vec<RemoteMailbox>> {
        let gmail = self.capabilities.gmail;
        let command = if self.special_use {
            "LIST \"\" \"*\" RETURN (SPECIAL-USE)"
        } else {
            "LIST \"\" \"*\""
        };
        let session = self.session_mut()?;
        let id = session.run_command(command).await.map_err(classify)?;
        let mut out = Vec::new();
        loop {
            let Some(resp) = session.read_response().await.map_err(classify_io)? else {
                return Err(MailError::Network("connection closed while listing mailboxes".into()));
            };
            match resp.parsed() {
                ImapResponse::Done { tag, status, code, information } if *tag == id => {
                    status_result(status, code.as_ref(), information.as_deref())
                        .map_err(classify)?;
                    break;
                }
                ImapResponse::MailboxData(MailboxDatum::List {
                    name_attributes,
                    delimiter,
                    name,
                }) => {
                    let role = role_from_attributes(name_attributes, name, gmail);
                    out.push(RemoteMailbox {
                        name: name.to_string(),
                        delimiter: delimiter.as_deref().and_then(|d| d.chars().next()),
                        attributes: name_attributes.iter().map(name_attribute_to_string).collect(),
                        special_use: role,
                    });
                }
                _ => {}
            }
        }
        Ok(out)
    }

    async fn select(&mut self, mailbox: &str) -> Result<MailboxState> {
        let condstore = self.capabilities.condstore;
        let session = self.session_mut()?;
        let mbox = if condstore {
            session.select_condstore(mailbox).await
        } else {
            session.select(mailbox).await
        }
        .map_err(classify)?;
        let state = MailboxState {
            uidvalidity: mbox.uid_validity.unwrap_or(0),
            uidnext: mbox.uid_next.unwrap_or(0),
            highestmodseq: mbox.highest_modseq,
            exists: mbox.exists,
        };
        self.selected = Some(state);
        Ok(state)
    }

    async fn changes_since(&mut self, cursor: &SyncCursor, known_uids: &UidSet) -> Result<Changes> {
        let state = self
            .selected
            .ok_or_else(|| MailError::Protocol("changes_since called before select".into()))?;
        if state.uidvalidity != cursor.uidvalidity {
            return Ok(Changes { uidvalidity_reset: true, ..Changes::default() });
        }

        let condstore = self.capabilities.condstore;
        let session = self.session_mut()?;

        // What is on the server right now, diffed against what the caller
        // already has: this is the "UID SEARCH diff" the module docs
        // promise, standing in for QRESYNC's `VANISHED`.
        let present: std::collections::HashSet<Uid> =
            session.uid_search("ALL").await.map_err(classify)?;
        let present: UidSet = present.into_iter().collect();
        let mut new_uids = UidSet::new();
        for uid in present.iter() {
            if !known_uids.contains(uid) {
                new_uids.insert(uid);
            }
        }
        let mut vanished = UidSet::new();
        for uid in known_uids.iter() {
            if !present.contains(uid) {
                vanished.insert(uid);
            }
        }

        let mut flag_changes = Vec::new();
        if condstore {
            // `CHANGEDSINCE` only has something to compare against once a
            // modseq has been seen; on a mailbox's first sync there is
            // nothing to report here yet -- every message is `new_uids`,
            // and its flags arrive with its header.
            if let Some(modseq) = cursor.highestmodseq {
                let command = format!("UID FETCH 1:* (FLAGS) (CHANGEDSINCE {modseq})");
                run_fetch_command(session, &command, |_seq, attrs| {
                    if let Some((uid, flags, modseq)) = flags_from_attrs(attrs) {
                        flag_changes.push((uid, flags, modseq));
                    }
                })
                .await?;
            }
        } else if !known_uids.is_empty() {
            // No CONDSTORE: there is no cheap "what changed" question to
            // ask, so this asks the expensive one instead -- current flags
            // for every UID the caller already knows -- and the caller
            // reconciles. `modseq` is `None` throughout, as documented on
            // `Changes::flag_changes`.
            let command = format!("UID FETCH {} (FLAGS)", known_uids.to_imap());
            run_fetch_command(session, &command, |_seq, attrs| {
                if let Some((uid, flags, _)) = flags_from_attrs(attrs) {
                    flag_changes.push((uid, flags, None));
                }
            })
            .await?;
        }

        Ok(Changes { new_uids, flag_changes, vanished, uidvalidity_reset: false })
    }

    async fn headers(&mut self, uids: &UidSet) -> Result<Vec<RemoteHeader>> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        let items = if self.capabilities.gmail {
            "UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[HEADER] X-GM-THRID X-GM-MSGID X-GM-LABELS"
        } else {
            "UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[HEADER]"
        };
        let command = format!("UID FETCH {} ({items})", uids.to_imap());
        let mut out = Vec::new();
        let session = self.session_mut()?;
        run_fetch_command(session, &command, |_seq, attrs| {
            if let Some(header) = header_from_attrs(attrs) {
                out.push(header);
            }
        })
        .await?;
        Ok(out)
    }

    async fn raw(&mut self, uids: &UidSet) -> Result<RawStream<'_>> {
        let batches: VecDeque<UidSet> = uids.chunks(RAW_BATCH_SIZE).into_iter().collect();
        let state = (self, batches, VecDeque::<(Uid, Vec<u8>)>::new());
        let stream = stream::unfold(state, |(session, mut batches, mut buffer)| async move {
            loop {
                if let Some(item) = buffer.pop_front() {
                    return Some((Ok(item), (session, batches, buffer)));
                }
                let batch = batches.pop_front()?;
                match session.fetch_raw_batch(&batch).await {
                    Ok(items) => buffer.extend(items),
                    Err(e) => return Some((Err(e), (session, batches, buffer))),
                }
            }
        });
        Ok(Box::pin(stream))
    }

    async fn store_flags(&mut self, uids: &UidSet, add: Flags, remove: Flags) -> Result<()> {
        if uids.is_empty() {
            return Ok(());
        }
        let set = uids.to_imap();
        let session = self.session_mut()?;
        if let Some(list) = add.to_imap_list() {
            let command = format!("UID STORE {set} +FLAGS.SILENT {list}");
            run_fetch_command(session, &command, |_, _| {}).await?;
        }
        if let Some(list) = remove.to_imap_list() {
            let command = format!("UID STORE {set} -FLAGS.SILENT {list}");
            run_fetch_command(session, &command, |_, _| {}).await?;
        }
        Ok(())
    }

    async fn store_gmail_labels(
        &mut self,
        uids: &UidSet,
        add: &[String],
        remove: &[String],
    ) -> Result<()> {
        if uids.is_empty() {
            return Ok(());
        }
        let set = uids.to_imap();
        let session = self.session_mut()?;
        if !add.is_empty() {
            let list = gmail_label_list(add);
            let command = format!("UID STORE {set} +X-GM-LABELS.SILENT {list}");
            run_fetch_command(session, &command, |_, _| {}).await?;
        }
        if !remove.is_empty() {
            let list = gmail_label_list(remove);
            let command = format!("UID STORE {set} -X-GM-LABELS.SILENT {list}");
            run_fetch_command(session, &command, |_, _| {}).await?;
        }
        Ok(())
    }

    async fn move_to(&mut self, uids: &UidSet, mailbox: &str) -> Result<()> {
        if uids.is_empty() {
            return Ok(());
        }
        let set = uids.to_imap();
        if self.capabilities.move_ {
            let session = self.session_mut()?;
            session.uid_mv(&set, mailbox).await.map_err(classify)?;
            return Ok(());
        }

        // No MOVE: copy, mark the originals deleted, then expunge exactly
        // this set. `UID EXPUNGE` (RFC 4315, UIDPLUS) only removes the UIDs
        // named. Without UIDPLUS the only expunge there is removes *every*
        // `\Deleted` message in the mailbox -- including ones another client
        // marked and has not yet expunged, which would be deleting mail
        // nobody asked this app to delete. So on such a server the originals
        // are left marked `\Deleted` and not expunged: every client hides
        // them, the next expunge by whoever owns that decision removes them,
        // and nothing is lost that someone did not choose to lose.
        let uidplus = self.capabilities.uidplus;
        let session = self.session_mut()?;
        session.uid_copy(&set, mailbox).await.map_err(classify)?;
        run_fetch_command(
            session,
            &format!("UID STORE {set} +FLAGS.SILENT (\\Deleted)"),
            |_, _| {},
        )
        .await?;
        if uidplus {
            session
                .uid_expunge(&set)
                .await
                .map_err(classify)?
                .try_collect::<Vec<_>>()
                .await
                .map_err(classify)?;
        }
        Ok(())
    }

    async fn append(&mut self, mailbox: &str, raw: &[u8], flags: Flags) -> Result<Option<Uid>> {
        let flags_part = flags.to_imap_list().map(|f| format!(" {f}")).unwrap_or_default();
        let command = format!("APPEND {}{flags_part} {{{}}}", quote_mailbox(mailbox), raw.len());
        let session = self.session_mut()?;

        let id = session.run_command(&command).await.map_err(classify)?;

        // RFC 3501 section 7.5: the server must ask for the literal with a
        // `+` continuation before this crate may write the message bytes.
        // async-imap's own typed `append` does the same wait but is not
        // used here — it does not read the tagged response's `code`, so it
        // cannot hand back the `APPENDUID` this method exists to return.
        match session.read_response().await.map_err(classify_io)? {
            Some(resp) => match resp.parsed() {
                ImapResponse::Continue { .. } => {}
                ImapResponse::Done { status, code, information, .. } => {
                    status_result(status, code.as_ref(), information.as_deref())
                        .map_err(classify)?;
                    return Err(MailError::Protocol(
                        "the server accepted APPEND without asking for the message".into(),
                    ));
                }
                other => {
                    return Err(MailError::Protocol(format!(
                        "unexpected response to APPEND: {other:?}"
                    )));
                }
            },
            None => return Err(MailError::Network("connection closed during APPEND".into())),
        }

        {
            let stream = session.get_mut();
            stream.write_all(raw).await.map_err(classify_io)?;
            stream.write_all(b"\r\n").await.map_err(classify_io)?;
            stream.flush().await.map_err(classify_io)?;
        }

        loop {
            let Some(resp) = session.read_response().await.map_err(classify_io)? else {
                return Err(MailError::Network("connection closed during APPEND".into()));
            };
            if let ImapResponse::Done { tag, status, code, information } = resp.parsed() {
                if *tag == id {
                    status_result(status, code.as_ref(), information.as_deref())
                        .map_err(classify)?;
                    return Ok(append_uid(code.as_ref()));
                }
            }
        }
    }

    async fn idle(&mut self, mut stop: watch::Receiver<()>) -> Result<IdleEvent> {
        if !self.capabilities.idle {
            return Err(MailError::Unsupported("IDLE"));
        }
        loop {
            let session = self.take_session()?;
            let mut handle = session.idle();
            handle.init().await.map_err(classify)?;
            // `_interrupt` (async-imap's own `stop_token::StopSource`) is
            // kept alive, unused otherwise, for exactly as long as `wait`
            // is polled below; this method's own stop signal is the `stop`
            // channel, raced against `wait` with `select!`, not this token.
            let (wait, _interrupt) = handle.wait_with_timeout(IDLE_REISSUE);
            tokio::select! {
                res = wait => {
                    let outcome = res.map_err(classify)?;
                    self.session = Some(handle.done().await.map_err(classify)?);
                    use async_imap::extensions::idle::IdleResponse;
                    match outcome {
                        IdleResponse::Timeout | IdleResponse::ManualInterrupt => continue,
                        IdleResponse::NewData(_) => return Ok(IdleEvent::Activity),
                    }
                }
                _ = stop.changed() => {
                    self.session = Some(handle.done().await.map_err(classify)?);
                    return Ok(IdleEvent::Stopped);
                }
            }
        }
    }

    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }
}

/// Open a connection, authenticate, and read `CAPABILITY`.
///
/// Never sends `credential` before the connection is encrypted: for
/// [`Security::StartTls`] the `STARTTLS` command completes and the stream
/// is rewrapped before `LOGIN` or `AUTHENTICATE` is written.
///
/// Trusts the platform's own certificate store — see the module docs. For
/// a server whose certificate nothing has been told to trust, such as
/// `make test-imap`'s throwaway Dovecot, see [`connect_insecure_for_tests`].
pub async fn connect(
    host: &str,
    port: u16,
    security: Security,
    credential: Credential,
) -> Result<ImapSession> {
    connect_with(host, port, security, credential, Verifier::Platform).await
}

/// As [`connect`], but trusts whatever certificate the server presents,
/// without checking its chain or hostname.
///
/// Exists for exactly one caller: `make test-imap`'s throwaway Dovecot
/// container (see `crates/everyday-mail/tests/imap_dovecot.rs`), which
/// presents a self-signed certificate nothing on a test machine has been
/// told to trust. Behind a Cargo feature that defaults off and that no
/// normal build enables, so this function does not exist in a binary that
/// was not deliberately built for it.
#[cfg(feature = "insecure-test-tls")]
pub async fn connect_insecure_for_tests(
    host: &str,
    port: u16,
    security: Security,
    credential: Credential,
) -> Result<ImapSession> {
    connect_with(host, port, security, credential, Verifier::InsecureTestOnly).await
}

async fn connect_with(
    host: &str,
    port: u16,
    security: Security,
    credential: Credential,
    verifier: Verifier,
) -> Result<ImapSession> {
    let tcp = TcpStream::connect((host, port)).await.map_err(classify_io)?;

    let mut session = match security {
        Security::Tls => {
            let tls = tls_connect(host, tcp, verifier).await?;
            let client = async_imap::Client::new(tls);
            authenticate(client, credential).await?
        }
        Security::StartTls => {
            let mut client = async_imap::Client::new(tcp);
            client.run_command_and_check_ok("STARTTLS", None).await.map_err(classify)?;
            let tcp = client.into_inner();
            let tls = tls_connect(host, tcp, verifier).await?;
            let client = async_imap::Client::new(tls);
            authenticate(client, credential).await?
        }
    };

    let raw_caps = session.capabilities().await.map_err(classify)?;
    let capabilities = capabilities_from(&raw_caps);
    let special_use = raw_caps.has_str("SPECIAL-USE");
    drop(raw_caps);

    if capabilities.condstore {
        session.run_command_and_check_ok("ENABLE CONDSTORE").await.map_err(classify)?;
    }

    Ok(ImapSession { session: Some(session), capabilities, special_use, selected: None })
}

async fn tls_connect(host: &str, tcp: TcpStream, verifier: Verifier) -> Result<TlsStream> {
    let config = tls_config(verifier);
    let connector = tokio_rustls::TlsConnector::from(config);
    let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| MailError::Protocol(format!("{host:?} is not a valid TLS server name")))?;
    connector.connect(server_name, tcp).await.map_err(classify_io)
}

/// Build the `rustls::ClientConfig` for `verifier`.
///
/// An explicit `ring` crypto provider, not the process default -- this
/// process may already carry another rustls user, and two providers in one
/// binary makes "the default" ambiguous enough to panic at connect time.
/// Naming one here cannot be ambiguous. Mirrors
/// `everyday_store_postgres::tls`, which makes the same choice for the same
/// reason.
fn tls_config(verifier: Verifier) -> Arc<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("the ring provider supports rustls's default protocol versions");

    match verifier {
        Verifier::Platform => Arc::new(
            builder
                .with_platform_verifier()
                .expect("the platform verifier accepts the ring provider")
                .with_no_client_auth(),
        ),
        #[cfg(feature = "insecure-test-tls")]
        Verifier::InsecureTestOnly => Arc::new(
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(
                    insecure_test_tls::NoServerVerification(provider),
                ))
                .with_no_client_auth(),
        ),
    }
}

/// A certificate verifier that accepts anything, for `Security::InsecureTestTls`.
///
/// Kept in its own module, gated by the same `insecure-test-tls` feature as
/// the variant that uses it, so a `grep` for the feature name finds the
/// whole blast radius in one place.
#[cfg(feature = "insecure-test-tls")]
mod insecure_test_tls {
    use std::sync::Arc;

    use rustls::DigitallySignedStruct;
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::crypto::CryptoProvider;
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

    #[derive(Debug)]
    pub(super) struct NoServerVerification(pub(super) Arc<CryptoProvider>);

    impl ServerCertVerifier for NoServerVerification {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> std::result::Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }
}

/// Log in with `credential`, consuming `client` either way — on failure the
/// `Client` `async-imap` hands back for a retry is dropped, because nothing
/// in this crate retries a login with the same credential.
async fn authenticate(
    client: async_imap::Client<TlsStream>,
    credential: Credential,
) -> Result<ImapLibSession<TlsStream>> {
    match credential {
        Credential::Password { user, pass } => {
            client.login(&user, pass.as_str()).await.map_err(|(e, _)| classify_auth(e))
        }
        Credential::XOAuth2 { user, access_token } => {
            let authenticator =
                XOAuth2Authenticator { user, access_token: access_token.to_string(), sent: false };
            client.authenticate("XOAUTH2", authenticator).await.map_err(|(e, _)| classify_auth(e))
        }
    }
}

/// SASL `XOAUTH2` (Google's and Microsoft's OAuth bridge for IMAP): one
/// challenge-response round trip, base64-encoded by `async-imap` itself —
/// see [`async_imap::Authenticator`].
///
/// `sent` matters: on a rejected token the server does not just say `NO`,
/// it sends a *second* challenge (a JSON error payload) and RFC 7628
/// requires the client to answer it with an empty response before the
/// exchange can end in failure. Answering it with the credential again, as
/// a naïve `process` would, leaves the exchange stuck.
struct XOAuth2Authenticator {
    user: String,
    access_token: String,
    sent: bool,
}

impl async_imap::Authenticator for XOAuth2Authenticator {
    type Response = String;

    fn process(&mut self, _challenge: &[u8]) -> String {
        if self.sent {
            String::new()
        } else {
            self.sent = true;
            format!("user={}\x01auth=Bearer {}\x01\x01", self.user, self.access_token)
        }
    }
}

/// Send `command`, call `on_fetch` for every untagged `FETCH` response, and
/// return once the tagged completion arrives. See the module docs for why
/// this exists instead of [`async_imap::Session::uid_fetch`].
async fn run_fetch_command(
    session: &mut ImapLibSession<TlsStream>,
    command: &str,
    mut on_fetch: impl FnMut(u32, &[AttributeValue<'_>]),
) -> Result<()> {
    let id = session.run_command(command).await.map_err(classify)?;
    loop {
        let Some(resp) = session.read_response().await.map_err(classify_io)? else {
            return Err(MailError::Network("connection closed mid-command".into()));
        };
        match resp.parsed() {
            ImapResponse::Done { tag, status, code, information } if *tag == id => {
                status_result(status, code.as_ref(), information.as_deref()).map_err(classify)?;
                return Ok(());
            }
            ImapResponse::Fetch(seq, attrs) => on_fetch(*seq, attrs),
            // Anything else -- EXISTS, EXPUNGE, an unrelated unsolicited
            // line -- is safe to ignore here: the next `changes_since`
            // picks up whatever it implied.
            _ => {}
        }
    }
}

fn header_from_attrs(attrs: &[AttributeValue<'_>]) -> Option<RemoteHeader> {
    let mut uid = None;
    let mut flags = Flags::NONE;
    let mut internal_date = None;
    let mut size = None;
    let mut header = None;
    let mut thrid = None;
    let mut msgid = None;
    let mut labels = None;

    for attr in attrs {
        match attr {
            AttributeValue::Uid(v) => uid = Some(*v),
            AttributeValue::Flags(v) => flags = Flags::from_imap(v.iter().map(|s| s.as_ref())),
            AttributeValue::InternalDate(v) => internal_date = parse_internal_date(v),
            AttributeValue::Rfc822Size(v) => size = Some(*v),
            AttributeValue::BodySection {
                section: Some(SectionPath::Full(MessageSection::Header)),
                data: Some(bytes),
                ..
            } => header = Some(bytes.as_ref().to_vec()),
            AttributeValue::GmailThrId(v) => thrid = Some(*v),
            AttributeValue::GmailMsgId(v) => msgid = Some(*v),
            AttributeValue::GmailLabels(v) => {
                labels = Some(v.iter().map(|s| s.to_string()).collect())
            }
            _ => {}
        }
    }

    let gmail = match (thrid, msgid) {
        (Some(thrid), Some(msgid)) => {
            Some(GmailMeta { thrid, msgid, labels: labels.unwrap_or_default() })
        }
        _ => None,
    };

    Some(RemoteHeader {
        uid: uid?,
        flags,
        internal_date: internal_date?,
        size: size.unwrap_or(0),
        gmail,
        header: header.unwrap_or_default(),
    })
}

fn flags_from_attrs(attrs: &[AttributeValue<'_>]) -> Option<(Uid, Flags, Option<u64>)> {
    let mut uid = None;
    let mut flags = Flags::NONE;
    let mut modseq = None;
    for attr in attrs {
        match attr {
            AttributeValue::Uid(v) => uid = Some(*v),
            AttributeValue::Flags(v) => flags = Flags::from_imap(v.iter().map(|s| s.as_ref())),
            AttributeValue::ModSeq(v) => modseq = Some(*v),
            _ => {}
        }
    }
    uid.map(|uid| (uid, flags, modseq))
}

/// `INTERNALDATE`'s wire format, RFC 3501 section 9: `"14-Sep-2026
/// 09:30:00 +0000"`.
fn parse_internal_date(raw: &str) -> Option<Timestamp> {
    jiff::fmt::strtime::parse("%d-%b-%Y %H:%M:%S %z", raw).ok()?.to_timestamp().ok()
}

fn append_uid(code: Option<&ResponseCode<'_>>) -> Option<Uid> {
    match code {
        Some(ResponseCode::AppendUid(_uidvalidity, members)) => members.first().map(|m| match m {
            UidSetMember::Uid(u) => *u,
            UidSetMember::UidRange(r) => *r.start(),
        }),
        _ => None,
    }
}

fn capabilities_from(raw: &async_imap::types::Capabilities) -> Capabilities {
    Capabilities {
        condstore: raw.has_str("CONDSTORE"),
        qresync: raw.has_str("QRESYNC"),
        idle: raw.has_str("IDLE"),
        move_: raw.has_str("MOVE"),
        uidplus: raw.has_str("UIDPLUS"),
        gmail: raw.has_str("X-GM-EXT-1"),
        compress: raw.has_str("COMPRESS=DEFLATE"),
    }
}

/// What a mailbox is for: `SPECIAL-USE` first, then — only for a Gmail
/// account, and only when the attribute was somehow missing — the English
/// names Gmail itself gives its folders.
///
/// Gmail folder rule (`docs/plans/mail.md`, "What the open questions were
/// settled as"): the sync engine that will be built on this trait fetches
/// `\All` instead of every labelled mailbox, so `Role::All` is the one this
/// function most needs to get right.
fn role_from_attributes(attrs: &[NameAttribute<'_>], name: &str, gmail: bool) -> Option<Role> {
    if name.eq_ignore_ascii_case("INBOX") {
        return Some(Role::Inbox);
    }
    for attr in attrs {
        let role = match attr {
            NameAttribute::Sent => Some(Role::Sent),
            NameAttribute::Drafts => Some(Role::Drafts),
            NameAttribute::Archive => Some(Role::Archive),
            NameAttribute::Trash => Some(Role::Trash),
            NameAttribute::Junk => Some(Role::Spam),
            NameAttribute::All => Some(Role::All),
            _ => None,
        };
        if role.is_some() {
            return role;
        }
    }
    if gmail { gmail_role_from_name(name) } else { None }
}

fn gmail_role_from_name(name: &str) -> Option<Role> {
    match name {
        "[Gmail]/All Mail" => Some(Role::All),
        "[Gmail]/Sent Mail" => Some(Role::Sent),
        "[Gmail]/Drafts" => Some(Role::Drafts),
        "[Gmail]/Spam" => Some(Role::Spam),
        "[Gmail]/Trash" => Some(Role::Trash),
        _ => None,
    }
}

fn name_attribute_to_string(attr: &NameAttribute<'_>) -> String {
    match attr {
        NameAttribute::NoInferiors => "\\Noinferiors".to_string(),
        NameAttribute::NoSelect => "\\Noselect".to_string(),
        NameAttribute::Marked => "\\Marked".to_string(),
        NameAttribute::Unmarked => "\\Unmarked".to_string(),
        NameAttribute::All => "\\All".to_string(),
        NameAttribute::Archive => "\\Archive".to_string(),
        NameAttribute::Drafts => "\\Drafts".to_string(),
        NameAttribute::Flagged => "\\Flagged".to_string(),
        NameAttribute::Junk => "\\Junk".to_string(),
        NameAttribute::Sent => "\\Sent".to_string(),
        NameAttribute::Trash => "\\Trash".to_string(),
        NameAttribute::Extension(s) => format!("\\{s}"),
        // `NameAttribute` is `#[non_exhaustive]`: imap-proto may add a name
        // attribute IANA registers after this was written. Render it rather
        // than fail to compile against a later 0.16.x.
        other => format!("{other:?}"),
    }
}

/// IMAP string-literal quoting, matching `async-imap`'s own private `quote!`
/// macro exactly (backslash and double-quote escaped, wrapped in quotes),
/// needed here because [`ImapSession::append`] builds its `APPEND` command
/// by hand rather than through a typed method.
fn quote_mailbox(name: &str) -> String {
    format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The parenthesised, space-separated label list an `X-GM-LABELS` `STORE`
/// sends, e.g. `("\Inbox" "Work")` — Gmail's own system labels (`\Inbox`,
/// `\Important`, `\Starred`) are atoms with a leading backslash and a
/// person's own label can contain spaces, so every entry is quoted the same
/// way [`quote_mailbox`] quotes a mailbox name; a system label's leading
/// backslash survives that quoting untouched, which is exactly what Gmail's
/// own `STORE` syntax expects.
fn gmail_label_list(labels: &[String]) -> String {
    format!("({})", labels.iter().map(|l| quote_mailbox(l)).collect::<Vec<_>>().join(" "))
}

/// Mirrors `async_imap::client::Connection::check_status_ok`, which is
/// private to that crate: the same three-way match on a tagged response's
/// `Status`, producing the same [`ImapLibError`] shape so [`classify`] and
/// [`classify_auth`] handle a hand-rolled command exactly like a typed one.
fn status_result(
    status: &Status,
    code: Option<&ResponseCode<'_>>,
    information: Option<&str>,
) -> std::result::Result<(), ImapLibError> {
    match status {
        Status::Ok => Ok(()),
        Status::Bad => Err(ImapLibError::Bad(format!("code: {code:?}, info: {information:?}"))),
        Status::No => Err(ImapLibError::No(format!("code: {code:?}, info: {information:?}"))),
        other => Err(ImapLibError::Io(std::io::Error::other(format!(
            "unexpected status: {other:?}, code: {code:?}, information: {information:?}"
        )))),
    }
}

fn classify_io(e: std::io::Error) -> MailError {
    MailError::Network(e.to_string())
}

/// [`ImapLibError`] to [`MailError`], for every command except the one that
/// authenticates — see [`classify_auth`].
fn classify(err: ImapLibError) -> MailError {
    match err {
        ImapLibError::Io(e) => MailError::Network(e.to_string()),
        ImapLibError::ConnectionLost => MailError::Network("the connection was closed".into()),
        ImapLibError::Bad(msg) | ImapLibError::No(msg) => MailError::Server(msg),
        ImapLibError::Parse(e) => MailError::Protocol(e.to_string()),
        ImapLibError::Validate(e) => MailError::Protocol(e.to_string()),
        ImapLibError::Append => MailError::Protocol("the server rejected the message".into()),
        other => MailError::Protocol(other.to_string()),
    }
}

/// As [`classify`], except a `NO`/`BAD` response becomes [`MailError::Auth`]
/// — used only for the `LOGIN`/`AUTHENTICATE` round trip in [`authenticate`],
/// because that is the one command whose rejection means "sign in again"
/// rather than "something else is wrong".
fn classify_auth(err: ImapLibError) -> MailError {
    match err {
        ImapLibError::Bad(msg) | ImapLibError::No(msg) => MailError::Auth(msg),
        other => classify(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_set_encodes_ranges() {
        let set: UidSet = [1, 2, 3, 5, 7, 8, 9].into_iter().collect();
        assert_eq!(set.to_imap(), "1:3,5,7:9");
        assert_eq!(set.len(), 7);
        assert!(set.contains(8));
        assert!(!set.contains(4));
    }

    #[test]
    fn uid_set_insert_merges_neighbours() {
        let mut set = UidSet::new();
        for uid in [5, 1, 3, 2, 9, 4] {
            set.insert(uid);
        }
        assert_eq!(set.to_imap(), "1:5,9");
    }

    #[test]
    fn uid_set_chunks_stay_within_size() {
        let set: UidSet = (1..=45).collect();
        let chunks = set.chunks(20);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), 20);
        assert_eq!(chunks[2].len(), 5);
        let rejoined: UidSet = chunks.iter().flat_map(|c| c.iter()).collect();
        assert_eq!(rejoined, set);
    }

    #[test]
    fn uid_set_empty_range_is_empty() {
        assert!(UidSet::range(5, 1).is_empty());
    }

    #[test]
    fn flags_round_trip_through_imap_strings() {
        let flags = Flags::from_imap(["\\Seen", "\\Flagged", "\\Unknown"]);
        assert!(flags.contains(Flags::SEEN));
        assert!(flags.contains(Flags::FLAGGED));
        assert!(!flags.contains(Flags::DELETED));
        assert_eq!(flags.to_imap_list().as_deref(), Some("(\\Seen \\Flagged)"));
    }

    #[test]
    fn empty_flags_have_no_store_list() {
        assert_eq!(Flags::NONE.to_imap_list(), None);
    }

    #[test]
    fn parses_internal_date() {
        let ts = parse_internal_date("14-Sep-2026 09:30:00 +0000").expect("parses");
        assert_eq!(ts.to_string(), "2026-09-14T09:30:00Z");
    }

    #[test]
    fn rejects_malformed_internal_date() {
        assert!(parse_internal_date("not a date").is_none());
    }

    #[test]
    fn header_from_attrs_reads_gmail_thrid() {
        let attrs = vec![
            AttributeValue::Uid(42),
            AttributeValue::Flags(vec!["\\Seen".into()]),
            AttributeValue::InternalDate("14-Sep-2026 09:30:00 +0000".into()),
            AttributeValue::Rfc822Size(1234),
            AttributeValue::BodySection {
                section: Some(SectionPath::Full(MessageSection::Header)),
                index: None,
                data: Some(b"Subject: hi\r\n\r\n".as_slice().into()),
            },
            AttributeValue::GmailThrId(9_999_999_999),
            AttributeValue::GmailMsgId(1_111_111_111),
            AttributeValue::GmailLabels(vec!["\\Important".into(), "Work".into()]),
        ];
        let header = header_from_attrs(&attrs).expect("a complete header");
        assert_eq!(header.uid, 42);
        assert!(header.flags.contains(Flags::SEEN));
        assert_eq!(header.size, 1234);
        assert_eq!(header.header, b"Subject: hi\r\n\r\n");
        let gmail = header.gmail.expect("gmail metadata");
        assert_eq!(gmail.thrid, 9_999_999_999);
        assert_eq!(gmail.msgid, 1_111_111_111);
        assert_eq!(gmail.labels, vec!["\\Important".to_string(), "Work".to_string()]);
    }

    #[test]
    fn header_from_attrs_without_gmail_capability_has_no_meta() {
        let attrs = vec![
            AttributeValue::Uid(1),
            AttributeValue::InternalDate("14-Sep-2026 09:30:00 +0000".into()),
        ];
        let header = header_from_attrs(&attrs).expect("a complete header");
        assert!(header.gmail.is_none());
    }

    #[test]
    fn header_from_attrs_needs_uid_and_date() {
        assert!(header_from_attrs(&[AttributeValue::Rfc822Size(1)]).is_none());
    }

    #[test]
    fn role_from_special_use_attribute() {
        assert_eq!(
            role_from_attributes(&[NameAttribute::Sent], "Sent Items", false),
            Some(Role::Sent)
        );
        assert_eq!(role_from_attributes(&[], "INBOX", false), Some(Role::Inbox));
        assert_eq!(role_from_attributes(&[], "Archives/2024", false), None);
    }

    #[test]
    fn role_falls_back_to_gmail_folder_names() {
        assert_eq!(role_from_attributes(&[], "[Gmail]/All Mail", true), Some(Role::All));
        assert_eq!(role_from_attributes(&[], "[Gmail]/All Mail", false), None);
    }

    #[test]
    fn append_uid_reads_the_first_member() {
        let code = ResponseCode::AppendUid(12345, vec![UidSetMember::Uid(7)]);
        assert_eq!(append_uid(Some(&code)), Some(7));
        assert_eq!(append_uid(None), None);
    }

    #[test]
    fn classify_marks_login_failures_as_auth() {
        let err = classify_auth(ImapLibError::No("nope".into()));
        assert!(matches!(err, MailError::Auth(_)));
        let err = classify(ImapLibError::No("nope".into()));
        assert!(matches!(err, MailError::Server(_)));
    }

    #[test]
    fn quote_mailbox_escapes_quotes_and_backslashes() {
        assert_eq!(quote_mailbox("Sent"), "\"Sent\"");
        assert_eq!(quote_mailbox("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }
}
