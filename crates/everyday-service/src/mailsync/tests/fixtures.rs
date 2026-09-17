use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use everyday_core::account::{Account, AccountSecret, AccountStatus, AuthMethod, Provider};
use everyday_core::crypto::NullCipher;
use everyday_core::id::AccountId;
use everyday_core::packstore::{FilePackStore, PackStore};
use everyday_core::{MailSearch, Vault, VaultConfig};
use everyday_mail::compose::Built;
use everyday_mail::session::{
    Capabilities, Changes, GmailMeta, IdleEvent, MailError, MailSession, MailboxState, RawStream,
    RemoteHeader, RemoteMailbox, Result as SessionResult, Role, SyncCursor, Uid, UidSet,
};
use everyday_mail::smtp::SendReceipt;
use everyday_mailindex::MailIndex;
use futures::StreamExt;
use jiff::Timestamp;

use crate::mailsync::discovery::{self, LabelMailboxes};
use crate::mailsync::ingest::ThreadIndex;
use crate::mailsync::passes::{self, SyncContext};
use crate::mailsync::status::StatusRegistry;

// ---------------------------------------------------------------------
// The fake server and session
// ---------------------------------------------------------------------

#[derive(Clone)]
pub(super) struct FakeMessage {
    raw: Vec<u8>,
    flags: everyday_mail::session::Flags,
    internal_date: Timestamp,
    gmail: Option<GmailMeta>,
    modseq: u64,
}

pub(super) struct FakeMailbox {
    special_use: Option<Role>,
    uidvalidity: u32,
    uidnext: Uid,
    /// Whether [`FakeMailSession::select`] reports `uidnext` at all. `UIDNEXT`
    /// is an *optional* untagged `SELECT` response (RFC 3501 6.3.1 marks
    /// `UIDVALIDITY` "REQUIRED" and `UIDNEXT` only "OPTIONAL"), and real
    /// servers exist that never send it -- see
    /// `crate::mailsync::passes::sync_headers`'s own docs for the bug that
    /// went unnoticed because nothing here exercised that server. `true` by
    /// default: every other test in this file wants the ordinary,
    /// UIDNEXT-reporting server.
    reports_uidnext: bool,
    pub(super) messages: BTreeMap<Uid, FakeMessage>,
}

impl FakeMailbox {
    pub(super) fn new(special_use: Option<Role>) -> Self {
        Self {
            special_use,
            uidvalidity: 1,
            uidnext: 1,
            reports_uidnext: true,
            messages: BTreeMap::new(),
        }
    }

    /// Simulate a server that omits `UIDNEXT` from every `SELECT` response,
    /// for good -- see [`Self::reports_uidnext`]'s own docs.
    pub(super) fn stop_reporting_uidnext(&mut self) {
        self.reports_uidnext = false;
    }
}

/// The state a `FakeMailSession` reads and writes -- shared behind an `Arc`
/// so a test can mutate "the server" between two calls that simulate two
/// separate connections, the same way a real IMAP server keeps state a
/// client's next connection sees fresh.
pub(super) struct FakeServer {
    gmail: bool,
    pub(super) mailboxes: HashMap<String, FakeMailbox>,
    next_modseq: u64,
}

impl FakeServer {
    pub(super) fn new(gmail: bool) -> Self {
        Self { gmail, mailboxes: HashMap::new(), next_modseq: 1 }
    }

    pub(super) fn mailbox(&mut self, name: &str, special_use: Option<Role>) -> &mut FakeMailbox {
        self.mailboxes.entry(name.to_string()).or_insert_with(|| FakeMailbox::new(special_use))
    }

    /// Append a message, returning its uid.
    pub(super) fn append(
        &mut self,
        mailbox: &str,
        raw: Vec<u8>,
        flags: everyday_mail::session::Flags,
        gmail: Option<GmailMeta>,
    ) -> Uid {
        let modseq = self.next_modseq;
        self.next_modseq += 1;
        let mb = self.mailboxes.get_mut(mailbox).expect("mailbox must exist before appending");
        let uid = mb.uidnext;
        mb.uidnext += 1;
        mb.messages.insert(
            uid,
            FakeMessage { raw, flags, internal_date: Timestamp::now(), gmail, modseq },
        );
        uid
    }

    pub(super) fn set_flags(
        &mut self,
        mailbox: &str,
        uid: Uid,
        flags: everyday_mail::session::Flags,
    ) {
        let modseq = self.next_modseq;
        self.next_modseq += 1;
        if let Some(msg) = self.mailboxes.get_mut(mailbox).and_then(|m| m.messages.get_mut(&uid)) {
            msg.flags = flags;
            msg.modseq = modseq;
        }
    }

    /// Simulates a label changed in another client -- Gmail's own MODSEQ
    /// bumps on a label change too, which is what lets `changes_since`'s
    /// CONDSTORE diff notice it at all. See `passes::refresh_gmail_labels`.
    pub(super) fn set_gmail_labels(&mut self, mailbox: &str, uid: Uid, labels: Vec<String>) {
        let modseq = self.next_modseq;
        self.next_modseq += 1;
        self.set_gmail_labels_at(mailbox, uid, labels, modseq);
    }

    /// As [`FakeServer::set_gmail_labels`], but the message's `MODSEQ` is
    /// left exactly where it was -- simulating the case the plan's risk
    /// table asks for a fallback against: a server that, for whatever
    /// reason, does not report a label-only change through CONDSTORE at
    /// all. Only `refresh_gmail_labels`'s unconditional newest-N re-check
    /// can ever notice a change made this way.
    pub(super) fn set_gmail_labels_without_a_modseq_bump(
        &mut self,
        mailbox: &str,
        uid: Uid,
        labels: Vec<String>,
    ) {
        let modseq = self
            .mailboxes
            .get(mailbox)
            .and_then(|m| m.messages.get(&uid))
            .map(|m| m.modseq)
            .unwrap_or(0);
        self.set_gmail_labels_at(mailbox, uid, labels, modseq);
    }

    pub(super) fn set_gmail_labels_at(
        &mut self,
        mailbox: &str,
        uid: Uid,
        labels: Vec<String>,
        modseq: u64,
    ) {
        if let Some(msg) = self.mailboxes.get_mut(mailbox).and_then(|m| m.messages.get_mut(&uid)) {
            let gmail = msg.gmail.get_or_insert_with(|| GmailMeta {
                thrid: u64::from(uid),
                msgid: u64::from(uid),
                labels: Vec::new(),
            });
            gmail.labels = labels;
            msg.modseq = modseq;
        }
    }

    pub(super) fn remove(&mut self, mailbox: &str, uid: Uid) {
        if let Some(mb) = self.mailboxes.get_mut(mailbox) {
            mb.messages.remove(&uid);
        }
    }

    /// Simulate a `UIDVALIDITY` bump: every existing uid becomes
    /// meaningless, and every message is re-appended at a fresh uid.
    pub(super) fn bump_uidvalidity(&mut self, mailbox: &str) {
        let mb = self.mailboxes.get_mut(mailbox).expect("mailbox must exist");
        mb.uidvalidity += 1;
        mb.uidnext = 1;
        let old = std::mem::take(&mut mb.messages);
        for (_, msg) in old {
            let uid = mb.uidnext;
            mb.uidnext += 1;
            mb.messages.insert(uid, msg);
        }
    }
}

#[derive(Clone)]
pub(super) struct FakeMailSession {
    server: Arc<Mutex<FakeServer>>,
    selected: Option<String>,
    /// When set, [`FakeMailSession::idle`] never returns on its own --
    /// simulating a live `IDLE` connection that only ends when its own
    /// `stop` signal fires. Off by default: every sync-pass test wants
    /// `idle` to answer immediately, and only the task-level tests that
    /// actually exercise `crate::mailsync::task`'s own wake channel (see
    /// `a_notify_wakes_the_idle_loop_promptly_rather_than_waiting_for_the_poll`)
    /// need the alternative.
    block_idle: bool,
    /// Every mailbox name that was selected -- see [`Self::selected`] --
    /// at the moment [`FakeMailSession::idle`] was called, in order. A
    /// real `ImapSession::idle` reports activity for whatever is
    /// *currently* selected and never selects anything itself; this is
    /// what lets a test tell whether `crate::mailsync::task::run_account_with`
    /// pointed the session at the inbox first, the bug
    /// `a_wake_selects_the_inbox_before_idling` is a regression for.
    /// Shared behind an `Arc` (unlike `selected` above, which is per-clone
    /// state a real session's own connection would not share either) so a
    /// test can keep reading it after handing its own clone of this
    /// session to a spawned task.
    idle_selections: Arc<Mutex<Vec<Option<String>>>>,
}

impl FakeMailSession {
    pub(super) fn new(server: Arc<Mutex<FakeServer>>) -> Self {
        Self {
            server,
            selected: None,
            block_idle: false,
            idle_selections: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(super) fn blocking_idle(mut self) -> Self {
        self.block_idle = true;
        self
    }

    /// A clone of the shared record [`FakeMailSession::idle`] appends to,
    /// readable after handing this session's own clone off to a spawned
    /// task.
    pub(super) fn idle_selections(&self) -> Arc<Mutex<Vec<Option<String>>>> {
        self.idle_selections.clone()
    }

    pub(super) fn with_selected<T>(&self, f: impl FnOnce(&mut FakeMailbox) -> T) -> T {
        let name = self.selected.clone().expect("select must be called first");
        let mut server = self.server.lock().unwrap();
        f(server.mailboxes.get_mut(&name).expect("selected mailbox must exist"))
    }
}

/// Splits a raw RFC 822 message into its header block (including the blank
/// line separating it from the body) and its body, the same shape
/// `BODY.PEEK[HEADER]` returns.
pub(super) fn split_header(raw: &[u8]) -> Vec<u8> {
    let marker = b"\r\n\r\n";
    match raw.windows(marker.len()).position(|w| w == marker) {
        Some(i) => raw[..i + marker.len()].to_vec(),
        None => raw.to_vec(),
    }
}

/// The bare `Message-ID` header value (angle brackets stripped, the same
/// form [`everyday_mail::compose::build`] hands back as `Built::message_id`)
/// of a raw RFC 822 message, for [`FakeMailSession::search_message_id`]'s
/// own linear scan.
pub(super) fn message_id_header(raw: &[u8]) -> Option<String> {
    let header = split_header(raw);
    let text = String::from_utf8_lossy(&header);
    for line in text.split("\r\n") {
        if let Some(value) =
            line.strip_prefix("Message-ID:").or_else(|| line.strip_prefix("Message-Id:"))
        {
            return Some(value.trim().trim_start_matches('<').trim_end_matches('>').to_string());
        }
    }
    None
}

impl MailSession for FakeMailSession {
    async fn mailboxes(&mut self) -> SessionResult<Vec<RemoteMailbox>> {
        let server = self.server.lock().unwrap();
        Ok(server
            .mailboxes
            .iter()
            .map(|(name, mb)| RemoteMailbox {
                name: name.clone(),
                display_name: name.clone(),
                delimiter: Some('/'),
                attributes: Vec::new(),
                special_use: mb.special_use,
            })
            .collect())
    }

    async fn select(&mut self, mailbox: &str) -> SessionResult<MailboxState> {
        self.selected = Some(mailbox.to_string());
        let server = self.server.lock().unwrap();
        let mb = server
            .mailboxes
            .get(mailbox)
            .ok_or_else(|| MailError::Protocol(format!("no such mailbox: {mailbox}")))?;
        Ok(MailboxState {
            uidvalidity: mb.uidvalidity,
            uidnext: if mb.reports_uidnext { mb.uidnext } else { 0 },
            // The highest mod-sequence actually *assigned* to a message
            // here -- not the server's next-to-assign counter, which is
            // always one ahead and would make a change land on exactly the
            // cursor's own floor rather than above it.
            highestmodseq: Some(mb.messages.values().map(|m| m.modseq).max().unwrap_or(0)),
            exists: mb.messages.len() as u32,
        })
    }

    async fn changes_since(
        &mut self,
        cursor: &SyncCursor,
        known_uids: &UidSet,
    ) -> SessionResult<Changes> {
        Ok(self.with_selected(|mb| {
            if mb.uidvalidity != cursor.uidvalidity {
                return Changes { uidvalidity_reset: true, ..Changes::default() };
            }
            let present: UidSet = mb.messages.keys().copied().collect();
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
            let floor = cursor.highestmodseq.unwrap_or(0);
            let mut flag_changes = Vec::new();
            for (&uid, msg) in &mb.messages {
                if known_uids.contains(uid) && msg.modseq > floor {
                    flag_changes.push((uid, msg.flags, Some(msg.modseq)));
                }
            }
            Changes { new_uids, flag_changes, vanished, uidvalidity_reset: false }
        }))
    }

    async fn headers(&mut self, uids: &UidSet) -> SessionResult<Vec<RemoteHeader>> {
        Ok(self.with_selected(|mb| {
            uids.iter()
                .filter_map(|uid| {
                    mb.messages.get(&uid).map(|m| RemoteHeader {
                        uid,
                        flags: m.flags,
                        internal_date: m.internal_date,
                        size: m.raw.len() as u32,
                        gmail: m.gmail.clone(),
                        header: split_header(&m.raw),
                    })
                })
                .collect()
        }))
    }

    async fn raw(&mut self, uids: &UidSet) -> SessionResult<RawStream<'_>> {
        // A real fetch takes a moment; this fake never otherwise suspends
        // at all, which would make it impossible for a test to land a
        // concurrent local write inside the window `bodies_pass` actually
        // has open between taking its snapshot and using it -- see
        // `a_flag_change_during_the_bodies_pass_survives_it`, the one test
        // that needs this yield to be real rather than instant.
        tokio::task::yield_now().await;
        let items: Vec<(Uid, Vec<u8>)> = self.with_selected(|mb| {
            uids.iter()
                .filter_map(|uid| mb.messages.get(&uid).map(|m| (uid, m.raw.clone())))
                .collect()
        });
        Ok(futures::stream::iter(items.into_iter().map(Ok)).boxed())
    }

    async fn store_flags(
        &mut self,
        uids: &UidSet,
        add: everyday_mail::session::Flags,
        remove: everyday_mail::session::Flags,
    ) -> SessionResult<()> {
        let name = self.selected.clone().expect("select must be called first");
        let mut server = self.server.lock().unwrap();
        let modseq = server.next_modseq;
        server.next_modseq += 1;
        if let Some(mb) = server.mailboxes.get_mut(&name) {
            for uid in uids.iter() {
                if let Some(msg) = mb.messages.get_mut(&uid) {
                    msg.flags.insert(add);
                    msg.flags.remove(remove);
                    msg.modseq = modseq;
                }
            }
        }
        Ok(())
    }

    /// Moves every named uid out of the selected mailbox and re-appends it
    /// to `mailbox` under a fresh uid -- exactly what a real `MOVE` does to
    /// a client watching from outside, which is all the outbox executor's
    /// `archive`/`trash`/`move_to_mailbox` tests here need to see.
    async fn move_to(&mut self, uids: &UidSet, mailbox: &str) -> SessionResult<()> {
        let name = self.selected.clone().expect("select must be called first");
        let mut server = self.server.lock().unwrap();
        let mut moved = Vec::new();
        if let Some(mb) = server.mailboxes.get_mut(&name) {
            for uid in uids.iter() {
                if let Some(msg) = mb.messages.remove(&uid) {
                    moved.push(msg);
                }
            }
        }
        for msg in moved {
            server.append(mailbox, msg.raw, msg.flags, msg.gmail);
        }
        Ok(())
    }

    async fn append(
        &mut self,
        mailbox: &str,
        raw: &[u8],
        flags: everyday_mail::session::Flags,
    ) -> SessionResult<Option<Uid>> {
        let mut server = self.server.lock().unwrap();
        Ok(Some(server.append(mailbox, raw.to_vec(), flags, None)))
    }

    /// `\Deleted` then an unconditional expunge -- this fake always reports
    /// `UIDPLUS` (see [`FakeMailSession::capabilities`]), so a real
    /// `ImapSession` would always take the `UID EXPUNGE` branch too; there
    /// is no non-UIDPLUS fallback path for this fake to model.
    async fn delete(&mut self, uids: &UidSet) -> SessionResult<()> {
        let name = self.selected.clone().expect("select must be called first");
        let mut server = self.server.lock().unwrap();
        if let Some(mb) = server.mailboxes.get_mut(&name) {
            for uid in uids.iter() {
                mb.messages.remove(&uid);
            }
        }
        Ok(())
    }

    /// `UID SEARCH HEADER Message-ID` stood in for by a linear scan of
    /// `mailbox`'s own messages, comparing each one's parsed `Message-ID`
    /// header -- exactly what the recovery path this fake exists for
    /// actually needs, without a real IMAP `SEARCH` grammar to stand in
    /// for.
    async fn search_message_id(
        &mut self,
        mailbox: &str,
        message_id: &str,
    ) -> SessionResult<Option<Uid>> {
        let server = self.server.lock().unwrap();
        let mb = server
            .mailboxes
            .get(mailbox)
            .ok_or_else(|| MailError::Protocol(format!("no such mailbox: {mailbox}")))?;
        Ok(mb
            .messages
            .iter()
            .find(|(_, msg)| message_id_header(&msg.raw).as_deref() == Some(message_id))
            .map(|(&uid, _)| uid))
    }

    async fn idle(
        &mut self,
        mut stop: tokio::sync::watch::Receiver<()>,
    ) -> SessionResult<IdleEvent> {
        self.idle_selections.lock().unwrap().push(self.selected.clone());
        if self.block_idle {
            // Blocks until the caller's own wake signal fires -- see
            // `block_idle`'s own docs -- then ends cleanly, exactly the
            // guarantee a real `ImapSession::idle` gives: `stop` is fed
            // *into* this call by `crate::mailsync::task` rather than
            // raced against it, so the connection (nothing to lose here,
            // but the session in a real adapter) survives every wake.
            let _ = stop.changed().await;
        }
        Ok(IdleEvent::Stopped)
    }

    fn capabilities(&self) -> &Capabilities {
        // Leaked once per process is fine for a fake used only in tests: a
        // `'static` reference is what the trait asks for, and there is no
        // per-instance state in it worth avoiding a leak over.
        static CAPS_GMAIL: Capabilities = Capabilities {
            condstore: true,
            qresync: false,
            idle: true,
            move_: true,
            uidplus: true,
            gmail: true,
            compress: false,
        };
        static CAPS_PLAIN: Capabilities = Capabilities {
            condstore: true,
            qresync: false,
            idle: true,
            move_: true,
            uidplus: true,
            gmail: false,
            compress: false,
        };
        if self.server.lock().unwrap().gmail { &CAPS_GMAIL } else { &CAPS_PLAIN }
    }
}

// ---------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------

pub(super) fn raw_message(
    message_id: &str,
    in_reply_to: Option<&str>,
    from: &str,
    subject: &str,
    date: &str,
    body: &str,
) -> Vec<u8> {
    let mut s = format!("Message-ID: <{message_id}>\r\n");
    if let Some(parent) = in_reply_to {
        s.push_str(&format!("In-Reply-To: <{parent}>\r\n"));
    }
    s.push_str(&format!("From: {from}\r\n"));
    s.push_str("To: me@example.com\r\n");
    s.push_str(&format!("Subject: {subject}\r\n"));
    s.push_str(&format!("Date: {date}\r\n"));
    s.push_str("Content-Type: text/plain\r\n\r\n");
    s.push_str(body);
    s.push_str("\r\n");
    s.into_bytes()
}

/// A throwaway vault, its pack store and its search index -- real, on disk,
/// unencrypted for speed, the same combination `crate::mailsync::wiring`
/// opens against a real one.
pub(super) struct TestEnv {
    pub(super) vault: Arc<Vault>,
    pub(super) packs: Arc<dyn PackStore>,
    pub(super) index: Arc<dyn MailSearch>,
    statuses: StatusRegistry,
    pub(super) account_id: AccountId,
    /// Last, because fields drop in order: the directory must go after the
    /// vault and index that have files open in it, or the index can still be
    /// writing while it is removed and the directory is left behind.
    _dir: tempfile::TempDir,
}

impl TestEnv {
    pub(super) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let vault = Arc::new(
            everyday_vault::create(
                dir.path(),
                VaultConfig { password: None, ..Default::default() },
            )
            .unwrap(),
        );
        let packs: Arc<dyn PackStore> =
            Arc::new(FilePackStore::open(dir.path().join("packs"), Arc::new(NullCipher)).unwrap());
        let index: Arc<dyn MailSearch> = Arc::new(
            MailIndex::open(dir.path().join("index"), Arc::new(NullCipher), 16 * 1024 * 1024)
                .unwrap(),
        );

        let mut account = Account::new(Provider::Custom, "me@example.com");
        account.auth = AuthMethod::Password { username: "me@example.com".into() };
        account.status = AccountStatus::Ok;
        vault.save_account(&account).unwrap();
        vault
            .save_account_secret(
                account.id,
                &everyday_core::account::AccountSecret {
                    password: Some("hunter2".into()),
                    ..Default::default()
                },
            )
            .unwrap();

        Self {
            vault,
            packs,
            index,
            statuses: StatusRegistry::new(),
            account_id: account.id,
            _dir: dir,
        }
    }

    pub(super) fn ctx(&self) -> SyncContext<'_> {
        SyncContext {
            vault: &self.vault,
            account_id: self.account_id,
            packs: self.packs.clone(),
            index: self.index.clone(),
            statuses: &self.statuses,
            attachment_cap_bytes: None,
            index_commit: passes::CommitPacer::new(),
            unread_cache: None,
            contacts: None,
            identities: Vec::new(),
        }
    }

    /// Run one full `discover` + headers + bodies sweep, the shape both a
    /// first sync and a steady-state wake take.
    pub(super) async fn sync(
        &self,
        session: &mut FakeMailSession,
    ) -> Vec<discovery::SyncedMailbox> {
        let mut labels = LabelMailboxes::new(&self.vault, self.account_id);
        let mut threads = ThreadIndex::new();
        passes::sync_once(&self.ctx(), session, &mut labels, &mut threads).await.unwrap()
    }
}

pub(super) fn gmail_server() -> Arc<Mutex<FakeServer>> {
    let mut server = FakeServer::new(true);
    server.mailbox("All Mail", Some(Role::All));
    server.mailbox("Sent", Some(Role::Sent));
    server.mailbox("Drafts", Some(Role::Drafts));
    server.mailbox("Spam", Some(Role::Spam));
    server.mailbox("Trash", Some(Role::Trash));
    // `INBOX` exists on the server, per real Gmail, but must never be
    // synced as its own mailbox -- see `discovery`'s module docs.
    server.mailbox("INBOX", Some(Role::Inbox));
    Arc::new(Mutex::new(server))
}

pub(super) fn plain_server() -> Arc<Mutex<FakeServer>> {
    let mut server = FakeServer::new(false);
    server.mailbox("INBOX", Some(Role::Inbox));
    server.mailbox("Sent", Some(Role::Sent));
    server.mailbox("Archive", Some(Role::Archive));
    Arc::new(Mutex::new(server))
}

pub(super) fn flags_seen() -> everyday_mail::session::Flags {
    everyday_mail::session::Flags::NONE
}

// ---------------------------------------------------------------------
// Wiring the outbox into the account task
// ---------------------------------------------------------------------

/// A fake `everyday_mail::outbox::Sender`: accepts every message handed to
/// it -- there is no SMTP server here to disagree -- and records what it
/// was asked to send, for a test to inspect.
#[derive(Default, Clone)]
pub(super) struct FakeSender {
    pub(super) sent: Arc<Mutex<Vec<Built>>>,
}

#[allow(async_fn_in_trait)]
impl everyday_mail::outbox::Sender for FakeSender {
    async fn send(&self, built: &Built) -> SessionResult<SendReceipt> {
        self.sent.lock().unwrap().push(Built {
            raw: built.raw.clone(),
            message_id: built.message_id.clone(),
            envelope_from: built.envelope_from.clone(),
            envelope_to: built.envelope_to.clone(),
        });
        Ok(SendReceipt { accepted: built.envelope_to.clone(), server_response: "250 Ok".into() })
    }
}

/// A `Service` with a real, unlocked, throwaway vault -- the same shape
/// [`an_auth_failure_sets_needs_sign_in_and_the_task_stops`] builds, plus a
/// working password secret, for the drain- and task-level tests below that
/// need `Service::packs`/`Service::mail_index`/`Service::outbox_notify` to
/// answer, not only a bare [`Vault`]. The returned `TempDir` must be kept
/// alive by the caller for as long as the vault is used, on the same terms
/// [`TestEnv::_dir`] is.
pub(super) fn service_test_env()
-> (Arc<crate::service::Service>, Arc<Vault>, AccountId, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let vault =
        everyday_vault::create(dir.path(), VaultConfig { password: None, ..Default::default() })
            .unwrap();

    let mut account = Account::new(Provider::Custom, "me@example.com");
    account.auth = AuthMethod::Password { username: "me@example.com".into() };
    account.status = AccountStatus::Ok;
    let account_id = account.id;

    let svc = Arc::new(crate::service::Service::new());
    let vault_arc = svc.set(vault);
    vault_arc.save_account(&account).unwrap();
    vault_arc
        .save_account_secret(
            account_id,
            &AccountSecret { password: Some("hunter2".into()), ..Default::default() },
        )
        .unwrap();
    (svc, vault_arc, account_id, dir)
}
