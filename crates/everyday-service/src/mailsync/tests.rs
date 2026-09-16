//! Unit tests for the sync engine, against a fake [`MailSession`] and a
//! real, throwaway vault -- SQLite, the real pack store, the real search
//! index, everything but the socket.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use everyday_core::account::{Account, AccountSecret, AccountStatus, AuthMethod, Provider};
use everyday_core::crypto::NullCipher;
use everyday_core::id::AccountId;
use everyday_core::mail::{Address, Category, Draft, MailboxRole, OpKind, OpState, Origin};
use everyday_core::packstore::{FilePackStore, PackStore};
use everyday_core::store::mail::ThreadFilter;
use everyday_core::{MailQuery, MailSearch, Vault, VaultConfig};
use everyday_mail::compose::Built;
use everyday_mail::session::{
    Capabilities, Changes, Credential, GmailMeta, IdleEvent, MailError, MailSession, MailboxState,
    RawStream, RemoteHeader, RemoteMailbox, Result as SessionResult, Role, SyncCursor, Uid, UidSet,
};
use everyday_mail::smtp::SendReceipt;
use everyday_mailindex::MailIndex;
use futures::StreamExt;
use jiff::Timestamp;

use super::discovery::{self, LabelMailboxes};
use super::ingest::ThreadIndex;
use super::passes::{self, SyncContext};
use super::status::StatusRegistry;

// ---------------------------------------------------------------------
// The fake server and session
// ---------------------------------------------------------------------

#[derive(Clone)]
struct FakeMessage {
    raw: Vec<u8>,
    flags: everyday_mail::session::Flags,
    internal_date: Timestamp,
    gmail: Option<GmailMeta>,
    modseq: u64,
}

struct FakeMailbox {
    special_use: Option<Role>,
    uidvalidity: u32,
    uidnext: Uid,
    messages: BTreeMap<Uid, FakeMessage>,
}

impl FakeMailbox {
    fn new(special_use: Option<Role>) -> Self {
        Self { special_use, uidvalidity: 1, uidnext: 1, messages: BTreeMap::new() }
    }
}

/// The state a `FakeMailSession` reads and writes -- shared behind an `Arc`
/// so a test can mutate "the server" between two calls that simulate two
/// separate connections, the same way a real IMAP server keeps state a
/// client's next connection sees fresh.
struct FakeServer {
    gmail: bool,
    mailboxes: HashMap<String, FakeMailbox>,
    next_modseq: u64,
}

impl FakeServer {
    fn new(gmail: bool) -> Self {
        Self { gmail, mailboxes: HashMap::new(), next_modseq: 1 }
    }

    fn mailbox(&mut self, name: &str, special_use: Option<Role>) -> &mut FakeMailbox {
        self.mailboxes.entry(name.to_string()).or_insert_with(|| FakeMailbox::new(special_use))
    }

    /// Append a message, returning its uid.
    fn append(
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

    fn set_flags(&mut self, mailbox: &str, uid: Uid, flags: everyday_mail::session::Flags) {
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
    fn set_gmail_labels(&mut self, mailbox: &str, uid: Uid, labels: Vec<String>) {
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
    fn set_gmail_labels_without_a_modseq_bump(
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

    fn set_gmail_labels_at(&mut self, mailbox: &str, uid: Uid, labels: Vec<String>, modseq: u64) {
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

    fn remove(&mut self, mailbox: &str, uid: Uid) {
        if let Some(mb) = self.mailboxes.get_mut(mailbox) {
            mb.messages.remove(&uid);
        }
    }

    /// Simulate a `UIDVALIDITY` bump: every existing uid becomes
    /// meaningless, and every message is re-appended at a fresh uid.
    fn bump_uidvalidity(&mut self, mailbox: &str) {
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
struct FakeMailSession {
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
    fn new(server: Arc<Mutex<FakeServer>>) -> Self {
        Self {
            server,
            selected: None,
            block_idle: false,
            idle_selections: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn blocking_idle(mut self) -> Self {
        self.block_idle = true;
        self
    }

    /// A clone of the shared record [`FakeMailSession::idle`] appends to,
    /// readable after handing this session's own clone off to a spawned
    /// task.
    fn idle_selections(&self) -> Arc<Mutex<Vec<Option<String>>>> {
        self.idle_selections.clone()
    }

    fn with_selected<T>(&self, f: impl FnOnce(&mut FakeMailbox) -> T) -> T {
        let name = self.selected.clone().expect("select must be called first");
        let mut server = self.server.lock().unwrap();
        f(server.mailboxes.get_mut(&name).expect("selected mailbox must exist"))
    }
}

/// Splits a raw RFC 822 message into its header block (including the blank
/// line separating it from the body) and its body, the same shape
/// `BODY.PEEK[HEADER]` returns.
fn split_header(raw: &[u8]) -> Vec<u8> {
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
fn message_id_header(raw: &[u8]) -> Option<String> {
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
            uidnext: mb.uidnext,
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

fn raw_message(
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
struct TestEnv {
    _dir: tempfile::TempDir,
    vault: Arc<Vault>,
    packs: Arc<dyn PackStore>,
    index: Arc<dyn MailSearch>,
    statuses: StatusRegistry,
    account_id: AccountId,
}

impl TestEnv {
    fn new() -> Self {
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
            _dir: dir,
            vault,
            packs,
            index,
            statuses: StatusRegistry::new(),
            account_id: account.id,
        }
    }

    fn ctx(&self) -> SyncContext<'_> {
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
    async fn sync(&self, session: &mut FakeMailSession) -> Vec<discovery::SyncedMailbox> {
        let mut labels = LabelMailboxes::new(&self.vault, self.account_id);
        let mut threads = ThreadIndex::new();
        passes::sync_once(&self.ctx(), session, &mut labels, &mut threads).await.unwrap()
    }
}

fn gmail_server() -> Arc<Mutex<FakeServer>> {
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

fn plain_server() -> Arc<Mutex<FakeServer>> {
    let mut server = FakeServer::new(false);
    server.mailbox("INBOX", Some(Role::Inbox));
    server.mailbox("Sent", Some(Role::Sent));
    server.mailbox("Archive", Some(Role::Archive));
    Arc::new(Mutex::new(server))
}

fn flags_seen() -> everyday_mail::session::Flags {
    everyday_mail::session::Flags::NONE
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_first_sync_lands_everything_with_correct_threads_and_a_searchable_index() {
    let env = TestEnv::new();
    let server = plain_server();
    const PER_MAILBOX: usize = 666; // ~2,000 messages across INBOX, Sent, Archive.
    {
        let mut s = server.lock().unwrap();
        for mailbox in ["INBOX", "Sent", "Archive"] {
            for i in 0..PER_MAILBOX {
                let date = format!("{:02} Jan 2024 10:00:00 +0000", (i % 27) + 1);
                let raw = raw_message(
                    &format!("{mailbox}-{i}@example.com"),
                    None,
                    "alice@example.com",
                    &format!("Report {i}"),
                    &date,
                    &format!("Body of message {i}, mentioning marmalade{}.", i % 5),
                );
                s.append(mailbox, raw, flags_seen(), None);
            }
        }
        // A reply chain, split across two mailboxes the way Sent/Archive
        // copies of a real conversation would be.
        let root = raw_message(
            "thread-root@example.com",
            None,
            "bob@example.com",
            "Lunch on Friday",
            "01 Jan 2024 09:00:00 +0000",
            "Are you free Friday?",
        );
        s.append("INBOX", root, flags_seen(), None);
        let reply = raw_message(
            "thread-reply@example.com",
            Some("thread-root@example.com"),
            "me@example.com",
            "Re: Lunch on Friday",
            "01 Jan 2024 09:30:00 +0000",
            "Yes, works for me.",
        );
        s.append("Sent", reply, flags_seen(), None);
    }

    let mut session = FakeMailSession::new(server);
    let started = std::time::Instant::now();
    let mailboxes = env.sync(&mut session).await;
    let elapsed = started.elapsed();
    let total_messages = PER_MAILBOX * 3 + 2;
    eprintln!(
        "first sync of {total_messages} messages across 3 mailboxes took {elapsed:?} \
         ({:.0} messages/s)",
        total_messages as f64 / elapsed.as_secs_f64().max(0.001)
    );
    assert_eq!(mailboxes.len(), 3, "INBOX, Sent, Archive");

    let inbox = mailboxes.iter().find(|m| m.row.role == MailboxRole::Inbox).unwrap();
    let page =
        env.vault.list_threads(inbox.row.id, &ThreadFilter::default(), None, 10_000).unwrap();
    // `PER_MAILBOX` standalone messages plus the shared reply thread.
    assert_eq!(
        page.threads.len(),
        PER_MAILBOX + 1,
        "every message landed as its own or a shared thread"
    );

    let root_message = env
        .vault
        .message_by_message_id_header(env.account_id, "thread-root@example.com")
        .unwrap()
        .expect("root message stored");
    let reply_message = env
        .vault
        .message_by_message_id_header(env.account_id, "thread-reply@example.com")
        .unwrap()
        .expect("reply message stored");
    assert_eq!(
        root_message.thread_id, reply_message.thread_id,
        "a reply and its parent must land in the same thread even though they were fetched from different mailboxes"
    );
    let (thread, messages) = env.vault.thread(root_message.thread_id).unwrap();
    assert_eq!(thread.message_count, 2);
    assert_eq!(messages.len(), 2);

    // The index is searchable.
    let hits = env.index.search(&MailQuery::parse("marmalade2"), 20, None).unwrap();
    assert!(!hits.hits.is_empty(), "a word from the body should be findable");
}

#[tokio::test]
async fn a_process_killed_mid_pass_resumes_without_duplicates() {
    let env = TestEnv::new();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        for i in 0..40 {
            let raw = raw_message(
                &format!("m{i}@example.com"),
                None,
                "alice@example.com",
                &format!("Subject {i}"),
                "01 Jan 2024 10:00:00 +0000",
                "hello there",
            );
            s.append("INBOX", raw, flags_seen(), None);
        }
    }

    // First "attempt": headers land, but the process is killed before
    // bodies are fetched.
    let mut session = FakeMailSession::new(server.clone());
    let mut labels = LabelMailboxes::new(&env.vault, env.account_id);
    let mut threads = ThreadIndex::new();
    let mut mailboxes =
        discovery::discover(&env.vault, env.account_id, &mut session).await.unwrap();
    for mailbox in &mut mailboxes {
        passes::sync_headers(&env.ctx(), &mut session, mailbox, &mut labels, &mut threads)
            .await
            .unwrap();
    }
    let inbox = mailboxes.iter().find(|m| m.row.role == MailboxRole::Inbox).unwrap();
    let uids_after_headers = env.vault.mail_uid_set(inbox.row.id).unwrap();
    assert_eq!(uids_after_headers.len(), 40);

    // A fresh "process" resumes: a brand new session and index state, same
    // vault. A full sweep must not create duplicate rows and must finish
    // fetching every body.
    let mut session2 = FakeMailSession::new(server);
    let mailboxes2 = env.sync(&mut session2).await;
    let inbox2 = mailboxes2.iter().find(|m| m.row.role == MailboxRole::Inbox).unwrap();

    let page = env.vault.list_threads(inbox2.row.id, &ThreadFilter::default(), None, 1000).unwrap();
    assert_eq!(page.threads.len(), 40, "no duplicates from resuming");

    for uid in env.vault.mail_uid_set(inbox2.row.id).unwrap() {
        let message = env.vault.message_by_uid(inbox2.row.id, uid).unwrap().unwrap();
        assert!(
            !super::ingest::is_pending(&message.pack),
            "uid {uid} should have a real body by now"
        );
    }
}

/// Regression: `bodies_pass` used to upsert the whole `Message` snapshot
/// `pending_messages` took *before* the network fetch, so a local write
/// landing in the window between the headers pass creating a row and the
/// bodies pass finally reaching it -- a person reading the message, or the
/// model setting its category, during a big first sync -- was silently
/// reverted back to whatever the headers pass originally saw. This drives
/// `sync_headers` and `bodies_pass` separately (rather than through
/// `sync_once`, which runs them back to back with nothing in between) so
/// the write can land in exactly that window.
#[tokio::test]
async fn a_flag_change_during_the_bodies_pass_survives_it() {
    let env = TestEnv::new();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        s.append(
            "INBOX",
            raw_message(
                "body-race@example.com",
                None,
                "a@example.com",
                "Hi",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            None,
        );
    }
    let mut session = FakeMailSession::new(server);
    let mut labels = LabelMailboxes::new(&env.vault, env.account_id);
    let mut threads = ThreadIndex::new();
    let mut mailboxes =
        discovery::discover(&env.vault, env.account_id, &mut session).await.unwrap();
    for mailbox in &mut mailboxes {
        passes::sync_headers(&env.ctx(), &mut session, mailbox, &mut labels, &mut threads)
            .await
            .unwrap();
    }
    let inbox = mailboxes.iter().find(|m| m.row.role == MailboxRole::Inbox).unwrap();
    let uid = env.vault.mail_uid_set(inbox.row.id).unwrap()[0];
    let pending = env.vault.message_by_uid(inbox.row.id, uid).unwrap().unwrap();
    assert!(super::ingest::is_pending(&pending.pack), "the body must not be fetched yet");

    // The write this test's own regression is about has to land genuinely
    // *during* the pass -- after `pending_messages`'s own snapshot at the
    // top of `bodies_pass`, before the batch's ingest uses it -- not
    // merely before this call starts, which the fix's own re-read would
    // already see correctly either way. `tokio::join!` polls its two
    // futures in order on a current-thread runtime: `bodies_pass` runs
    // synchronously (this fake session never otherwise suspends) until
    // `raw`'s own real `yield_now` -- see that method's own docs -- at
    // which point the write closure below, having nothing of its own to
    // await, runs to completion before `bodies_pass` is ever polled again.
    let write_race = async {
        env.vault
            .update_message_flags(
                inbox.row.id,
                uid,
                everyday_core::mail::MessageFlags { seen: true, ..Default::default() },
            )
            .unwrap();
    };
    let ctx = env.ctx();
    let (result, ()) = tokio::join!(passes::bodies_pass(&ctx, &mut session, inbox), write_race);
    result.unwrap();

    let after = env.vault.message_by_uid(inbox.row.id, uid).unwrap().unwrap();
    assert!(after.flags.seen, "a flag change during the bodies pass must survive it");
    assert!(
        !super::ingest::is_pending(&after.pack),
        "the body must still have been fetched despite the concurrent write"
    );
}

#[tokio::test]
async fn a_flag_change_a_deletion_and_a_new_message_apply_incrementally() {
    let env = TestEnv::new();
    let server = plain_server();
    let uid_to_delete;
    let uid_to_flag;
    {
        let mut s = server.lock().unwrap();
        uid_to_delete = s.append(
            "INBOX",
            raw_message(
                "del@example.com",
                None,
                "a@example.com",
                "Bye",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            None,
        );
        uid_to_flag = s.append(
            "INBOX",
            raw_message(
                "flag@example.com",
                None,
                "a@example.com",
                "Flag me",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            None,
        );
    }
    let mut session = FakeMailSession::new(server.clone());
    let mailboxes = env.sync(&mut session).await;
    let inbox = mailboxes.iter().find(|m| m.row.role == MailboxRole::Inbox).unwrap();
    assert_eq!(env.vault.mail_uid_set(inbox.row.id).unwrap().len(), 2);

    {
        let mut s = server.lock().unwrap();
        s.remove("INBOX", uid_to_delete);
        s.set_flags(
            "INBOX",
            uid_to_flag,
            everyday_mail::session::Flags::SEEN | everyday_mail::session::Flags::FLAGGED,
        );
        s.append(
            "INBOX",
            raw_message(
                "new@example.com",
                None,
                "a@example.com",
                "New",
                "02 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            None,
        );
    }
    let mailboxes2 = env.sync(&mut session).await;
    let inbox2 = mailboxes2.iter().find(|m| m.row.role == MailboxRole::Inbox).unwrap();

    let remaining = env.vault.mail_uid_set(inbox2.row.id).unwrap();
    assert_eq!(remaining.len(), 2, "one deleted, one added, net still two");
    assert!(!remaining.contains(&uid_to_delete), "the deleted message should be gone");

    let flagged = env.vault.message_by_uid(inbox2.row.id, uid_to_flag).unwrap().unwrap();
    assert!(flagged.flags.seen, "the flag change should have applied");
    assert!(flagged.flags.flagged);

    let new_message = env
        .vault
        .message_by_message_id_header(env.account_id, "new@example.com")
        .unwrap()
        .expect("the new message should have arrived");
    assert!(!super::ingest::is_pending(&new_message.pack), "its body should have been fetched too");
}

/// Regression: every uid `changes_since` reported in `flag_changes` used
/// to be written back unconditionally, even when the flags it reported
/// already matched what was stored -- a full rewrite (decrypt, re-seal,
/// upsert, thread recompute) for nothing. The fake session, like a real
/// Gmail server on a label-only change, can bump a message's own `MODSEQ`
/// without changing any of the five IMAP flags this crate tracks, so this
/// also exercises the secondary bug: `changed` used to be driven by
/// `flag_changes` being non-empty at all, which invalidated the unread
/// cache on every such pass regardless of whether anything a person would
/// notice actually happened.
#[tokio::test]
async fn an_unrelated_modseq_bump_with_unchanged_flags_skips_the_write_and_the_cache() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    let uid;
    {
        let mut s = server.lock().unwrap();
        uid = s.append(
            "INBOX",
            raw_message(
                "modseq-only@example.com",
                None,
                "a@example.com",
                "Hi",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            None,
        );
    }
    let mut session = FakeMailSession::new(server.clone());
    let statuses = svc.mail_statuses().unwrap();
    let cache = svc.mail_unread_cache().unwrap();
    let ctx = SyncContext {
        vault: &vault,
        account_id,
        packs: svc.packs().unwrap(),
        index: svc.mail_index().unwrap(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: Some(cache.clone()),
        contacts: svc.mail_contacts(),
        identities: Vec::new(),
    };
    let mut labels = LabelMailboxes::new(&vault, account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    let inbox_id = vault
        .mailboxes(account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .unwrap()
        .id;
    let flags_before = vault.message_by_uid(inbox_id, uid).unwrap().unwrap().flags;

    // Prime the cache with a sentinel this test controls -- keyed to
    // `inbox_id` itself, so a later read that still sees the same sentinel
    // proves nothing invalidated it in between.
    let sentinel = vec![(inbox_id, 999)];
    cache.get_or_compute(account_id, || Ok(sentinel.clone())).unwrap();

    // Bump the message's own `MODSEQ` without touching any of its actual
    // flags -- exactly what a Gmail label-only change, or a redundant
    // `STORE`, does.
    {
        let mut s = server.lock().unwrap();
        s.set_flags("INBOX", uid, flags_seen());
    }
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    let flags_after = vault.message_by_uid(inbox_id, uid).unwrap().unwrap().flags;
    assert_eq!(flags_before, flags_after, "flags did not actually change");

    let cached = cache
        .get_or_compute(account_id, || panic!("must not recompute: nothing actually changed"))
        .unwrap();
    assert_eq!(cached, sentinel);
}

/// Regression for "removed messages are never marked dead in the pack
/// store, or removed from search": a message the server no longer has must
/// stop being searchable, and its pack frame must actually be reclaimable
/// -- not merely gone from the vault's own `mail_messages` row, which
/// `remove_uids` already got right before this fix.
#[tokio::test]
async fn a_deleted_messages_pack_frame_is_marked_dead_and_dropped_from_search() {
    let env = TestEnv::new();
    let server = plain_server();
    let uid;
    {
        let mut s = server.lock().unwrap();
        uid = s.append(
            "INBOX",
            raw_message(
                "reap@example.com",
                None,
                "a@example.com",
                "Reap me",
                "01 Jan 2024 10:00:00 +0000",
                "marmaladewords",
            ),
            flags_seen(),
            None,
        );
    }
    let mut session = FakeMailSession::new(server.clone());
    env.sync(&mut session).await;

    let message = env
        .vault
        .message_by_message_id_header(env.account_id, "reap@example.com")
        .unwrap()
        .expect("stored");
    assert!(!super::ingest::is_pending(&message.pack), "its body must have been fetched first");
    let pack_before = message.pack.clone();

    let hits_before = env.index.search(&MailQuery::parse("marmaladewords"), 10, None).unwrap();
    assert!(!hits_before.hits.is_empty(), "indexed before removal");

    {
        let mut s = server.lock().unwrap();
        s.remove("INBOX", uid);
    }
    env.sync(&mut session).await;

    let hits_after = env.index.search(&MailQuery::parse("marmaladewords"), 10, None).unwrap();
    assert!(hits_after.hits.is_empty(), "a removed message must no longer be searchable");

    // `compact` reclaims a pack once a third of it is dead -- trivially
    // true for a pack holding only this one, now-dead, message. It no
    // longer deletes anything itself (see `PackStore::compact`'s own
    // two-step contract), so the test plays the caller's part: a real
    // `ReferencedSnapshot`, built the same way `mailsync::task` builds one,
    // is the honest input -- nothing else references this account's packs
    // any more once the message above was removed, so the snapshot's own
    // `referenced` legitimately comes back empty, and `drop_packs` is what
    // actually reclaims the space.
    let snapshot = env.vault.mail_referenced_snapshot(env.packs.as_ref(), env.account_id).unwrap();
    let result = env.packs.compact(&env.account_id.to_string(), &snapshot, &|| true).unwrap();
    assert!(result.remap.is_empty(), "nothing live was left to remap");
    env.vault.remap_packs(env.account_id, &result.remap).unwrap();
    env.packs.drop_packs(&env.account_id.to_string(), &result.obsolete).unwrap();
    assert!(
        env.packs.read(&pack_before).is_err(),
        "the dead frame's pack must actually have been reclaimed"
    );
}

#[tokio::test]
async fn a_uidvalidity_reset_rematches_without_refetching_raw() {
    let env = TestEnv::new();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        s.append(
            "INBOX",
            raw_message(
                "stable@example.com",
                None,
                "a@example.com",
                "Stable",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            None,
        );
    }
    let mut session = FakeMailSession::new(server.clone());
    let _mailboxes = env.sync(&mut session).await;
    let before = env
        .vault
        .message_by_message_id_header(env.account_id, "stable@example.com")
        .unwrap()
        .expect("stored");
    assert!(!super::ingest::is_pending(&before.pack));
    let pack_before = before.pack.clone();

    {
        let mut s = server.lock().unwrap();
        s.bump_uidvalidity("INBOX");
    }
    let mailboxes2 = env.sync(&mut session).await;
    let inbox2 = mailboxes2.iter().find(|m| m.row.role == MailboxRole::Inbox).unwrap();

    let after = env
        .vault
        .message_by_message_id_header(env.account_id, "stable@example.com")
        .unwrap()
        .expect("still stored after the reset");
    assert_eq!(after.id, before.id, "the same physical message, not a new row");
    assert_eq!(after.pack, pack_before, "raw bytes were not refetched");

    let uids = env.vault.mail_uid_set(inbox2.row.id).unwrap();
    assert_eq!(uids.len(), 1, "membership was rebuilt under the new uid");
}

/// Regression: an interrupted `UIDVALIDITY` reset used to permanently
/// duplicate every message the crash left stranded. `reset_mailbox` durably
/// zeroes the row's own `UIDVALIDITY`, but the *new* one was only ever held
/// in memory until `sync_headers`'s own completion at the very end -- so a
/// crash any time before that (a dropped connection mid-batch, the task
/// aborted) meant the very next attempt saw a mailbox indistinguishable
/// from one that had simply never been synced, `force_db_rematch` came back
/// `false`, and every remaining message was minted fresh under a new id
/// with a fresh body download, while the original rows survived, orphaned,
/// with no mailbox membership at all.
///
/// This drives the resume with a brand new [`ThreadIndex`] -- the one a
/// freshly restarted task actually has -- rather than reusing the one the
/// interrupted attempt would have been building up, since that in-memory
/// map is exactly what a crash loses.
#[tokio::test]
async fn resuming_an_interrupted_uidvalidity_reset_does_not_duplicate_messages() {
    let env = TestEnv::new();
    let server = plain_server();
    let message_ids: Vec<String> = (0..3).map(|i| format!("reset{i}@example.com")).collect();
    {
        let mut s = server.lock().unwrap();
        for id in &message_ids {
            s.append(
                "INBOX",
                raw_message(id, None, "a@example.com", "Hi", "01 Jan 2024 10:00:00 +0000", "x"),
                flags_seen(),
                None,
            );
        }
    }
    let mut session = FakeMailSession::new(server.clone());
    let inbox = env
        .sync(&mut session)
        .await
        .into_iter()
        .find(|m| m.row.role == MailboxRole::Inbox)
        .unwrap();
    let original_ids: std::collections::HashSet<_> = message_ids
        .iter()
        .map(|id| env.vault.message_by_message_id_header(env.account_id, id).unwrap().unwrap().id)
        .collect();
    assert_eq!(original_ids.len(), 3);

    // Bump `UIDVALIDITY` on the server -- every uid this account remembers
    // for `INBOX` is now meaningless.
    {
        let mut s = server.lock().unwrap();
        s.bump_uidvalidity("INBOX");
    }

    // The crash this test stands in for: an earlier attempt got as far as
    // `sync_headers`'s own durable reset write (`reset_mailbox`, plus
    // stamping the new `UIDVALIDITY` with `uidnext` left at its `0`
    // sentinel -- see that function's own docs) but never reached the
    // headers loop at all, let alone its own completion.
    let state = session.select("INBOX").await.unwrap();
    env.vault.reset_mailbox(inbox.row.id).unwrap();
    let mut interrupted_row = inbox.row.clone();
    interrupted_row.uidvalidity = state.uidvalidity;
    interrupted_row.uidnext = 0;
    interrupted_row.highest_modseq = 0;
    env.vault.save_mailbox(&interrupted_row).unwrap();

    // The resume: rediscover the mailbox (picking the row back up exactly
    // as the interrupted attempt left it) and run headers with a fresh
    // `ThreadIndex`.
    let mut labels = LabelMailboxes::new(&env.vault, env.account_id);
    let mut threads = ThreadIndex::new();
    let mut mailboxes2 =
        discovery::discover(&env.vault, env.account_id, &mut session).await.unwrap();
    let inbox2 = mailboxes2.iter_mut().find(|m| m.row.role == MailboxRole::Inbox).unwrap();
    passes::sync_headers(&env.ctx(), &mut session, inbox2, &mut labels, &mut threads)
        .await
        .unwrap();

    let uids_after = env.vault.mail_uid_set(inbox2.row.id).unwrap();
    assert_eq!(uids_after.len(), 3, "still exactly three messages after resuming");
    let resumed_ids: std::collections::HashSet<_> = uids_after
        .iter()
        .map(|&uid| env.vault.message_by_uid(inbox2.row.id, uid).unwrap().unwrap().id)
        .collect();
    assert_eq!(
        resumed_ids, original_ids,
        "each message must have been rematched to its original id, not minted fresh"
    );
}

/// Regression: a message moved between mailboxes server-side (INBOX to
/// Archive, say) used to be deleted and reborn under a fresh id the moment
/// both sides of the move were noticed in the same sync round --
/// `sync_headers`'s own vanished-uid reap ran inline, mailbox by mailbox,
/// so the inbox's copy was already gone by the time Archive's headers pass
/// got a chance to rematch it by `Message-ID`. `sync_once` now defers
/// reaping until every mailbox in the round has ingested its own new
/// headers first -- see that function's own docs.
///
/// Uses `sync_once` directly, twice, with the same `labels`/`threads`
/// kept alive across both calls -- exactly the shape `run_account_with`'s
/// own long-lived locals give a real account task across every wake -- so
/// the in-memory half of rematching (`ThreadIndex::seen_by_message_id`)
/// still has this message's id from the first round when the second round
/// sees it reappear elsewhere.
#[tokio::test]
async fn a_message_moved_between_mailboxes_in_one_round_keeps_its_identity() {
    let env = TestEnv::new();
    let server = plain_server();
    let raw = raw_message(
        "moved@example.com",
        None,
        "a@example.com",
        "Moving day",
        "01 Jan 2024 10:00:00 +0000",
        "x",
    );
    let uid;
    {
        let mut s = server.lock().unwrap();
        uid = s.append("INBOX", raw.clone(), flags_seen(), None);
    }
    let mut session = FakeMailSession::new(server.clone());
    let mut labels = LabelMailboxes::new(&env.vault, env.account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&env.ctx(), &mut session, &mut labels, &mut threads).await.unwrap();

    let original = env
        .vault
        .message_by_message_id_header(env.account_id, "moved@example.com")
        .unwrap()
        .expect("stored after the first round");
    assert!(!super::ingest::is_pending(&original.pack), "its body must already be fetched");

    {
        let mut s = server.lock().unwrap();
        s.remove("INBOX", uid);
        s.append("Archive", raw, flags_seen(), None);
    }
    // Same `labels`/`threads` as the first round -- see this test's own
    // docs.
    passes::sync_once(&env.ctx(), &mut session, &mut labels, &mut threads).await.unwrap();

    let moved = env
        .vault
        .message_by_message_id_header(env.account_id, "moved@example.com")
        .unwrap()
        .expect("still stored after the move");
    assert_eq!(moved.id, original.id, "a message moved between mailboxes must keep its identity");
    assert_eq!(moved.pack, original.pack, "and must not be re-downloaded");

    let inbox = env
        .vault
        .mailboxes(env.account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .unwrap();
    let archive = env
        .vault
        .mailboxes(env.account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Archive)
        .unwrap();
    assert!(env.vault.mail_uid_set(inbox.id).unwrap().is_empty(), "gone from the inbox");
    assert_eq!(env.vault.mail_uid_set(archive.id).unwrap().len(), 1, "and filed under Archive");
}

#[tokio::test]
async fn a_message_referencing_two_existing_threads_merges_them() {
    let env = TestEnv::new();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        s.append(
            "INBOX",
            raw_message(
                "root-a@example.com",
                None,
                "a@example.com",
                "Topic A",
                "01 Jan 2024 09:00:00 +0000",
                "start of A",
            ),
            flags_seen(),
            None,
        );
        s.append(
            "INBOX",
            raw_message(
                "root-b@example.com",
                None,
                "b@example.com",
                "Topic B",
                "01 Jan 2024 09:05:00 +0000",
                "start of B",
            ),
            flags_seen(),
            None,
        );
    }
    let mut session = FakeMailSession::new(server.clone());
    env.sync(&mut session).await;

    let root_a = env
        .vault
        .message_by_message_id_header(env.account_id, "root-a@example.com")
        .unwrap()
        .expect("root a stored");
    let root_b = env
        .vault
        .message_by_message_id_header(env.account_id, "root-b@example.com")
        .unwrap()
        .expect("root b stored");
    assert_ne!(root_a.thread_id, root_b.thread_id, "two unrelated threads to start");

    // A later message whose `References` names both -- a real client that
    // merged the two conversations under one, or an unusual reply chain
    // that the two prior messages themselves never revealed a link between.
    let merging = b"Message-ID: <merges-both@example.com>\r\n\
References: <root-a@example.com> <root-b@example.com>\r\n\
From: c@example.com\r\n\
To: me@example.com\r\n\
Subject: Re: Topic A\r\n\
Date: 01 Jan 2024 09:10:00 +0000\r\n\
Content-Type: text/plain\r\n\r\nties them together\r\n"
        .to_vec();
    {
        let mut s = server.lock().unwrap();
        s.append("INBOX", merging, flags_seen(), None);
    }
    env.sync(&mut session).await;

    let root_a_after = env
        .vault
        .message_by_message_id_header(env.account_id, "root-a@example.com")
        .unwrap()
        .expect("root a still stored");
    let root_b_after = env
        .vault
        .message_by_message_id_header(env.account_id, "root-b@example.com")
        .unwrap()
        .expect("root b still stored");
    let merger = env
        .vault
        .message_by_message_id_header(env.account_id, "merges-both@example.com")
        .unwrap()
        .expect("the merging message stored");
    assert_eq!(
        root_a_after.thread_id, root_b_after.thread_id,
        "the two roots must now share a thread"
    );
    assert_eq!(merger.thread_id, root_a_after.thread_id);

    let (thread, messages) = env.vault.thread(root_a_after.thread_id).unwrap();
    assert_eq!(thread.message_count, 3, "all three messages under the one kept thread");
    assert_eq!(messages.len(), 3);

    let inbox = env
        .vault
        .mailboxes(env.account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .unwrap();
    let page = env.vault.list_threads(inbox.id, &ThreadFilter::default(), None, 10).unwrap();
    assert_eq!(page.threads.len(), 1, "the merged-away thread must no longer appear in the list");
}

/// The regression for `ThreadIndex::seen_by_message_id` caching a whole
/// stale [`everyday_core::mail::Message`] rather than just an id: a real
/// `Message` a `ThreadIndex` (kept alive across a whole account task's
/// life, per `mailsync::task::run_account_with`, never per sync attempt)
/// once cached for a message can go stale two ways before that same
/// `Message-ID` is ever seen again -- its body arrives (turning a `pending`
/// pack and an empty snippet into real ones) and a later message's
/// `References` chain merges its thread into another. Both must survive the
/// message turning up again at a new location, which is exactly what moving
/// mailboxes -- or a new `UID` after a `UIDVALIDITY`-free move a plain
/// `IMAP` `MOVE` produces -- does.
#[tokio::test]
async fn a_merged_and_relocated_message_keeps_its_thread_pack_and_snippet() {
    let env = TestEnv::new();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        s.append(
            "INBOX",
            raw_message(
                "root-a@example.com",
                None,
                "a@example.com",
                "Topic A",
                "01 Jan 2024 09:00:00 +0000",
                "start of A",
            ),
            flags_seen(),
            None,
        );
        s.append(
            "INBOX",
            raw_message(
                "root-b@example.com",
                None,
                "b@example.com",
                "Topic B",
                "01 Jan 2024 09:05:00 +0000",
                "the body of B, which the snippet must still say after this wake",
            ),
            flags_seen(),
            None,
        );
    }

    // One `ThreadIndex`, reused across two wakes -- the shape
    // `run_account_with` actually keeps one in for the account task's whole
    // life, never a fresh one per sync attempt the way `TestEnv::sync`'s
    // convenience wrapper does.
    let mut session = FakeMailSession::new(server.clone());
    let mut labels = LabelMailboxes::new(&env.vault, env.account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&env.ctx(), &mut session, &mut labels, &mut threads).await.unwrap();

    let root_b_before = env
        .vault
        .message_by_message_id_header(env.account_id, "root-b@example.com")
        .unwrap()
        .expect("root b stored");
    assert!(!super::ingest::is_pending(&root_b_before.pack), "the first wake's bodies pass ran");
    assert!(!root_b_before.snippet.is_empty());
    let pack_before = root_b_before.pack.clone();
    let snippet_before = root_b_before.snippet.clone();

    // The second wake: a message merging root a and root b's threads, *and*
    // root b turning up again at a new location -- the two ways this bug
    // needs at once. INBOX is synced before Archive (inbox-first), so the
    // merge below is already applied to the vault by the time root b's
    // `Message-ID` is seen again while syncing Archive, in the same
    // `threads`.
    {
        let mut s = server.lock().unwrap();
        s.append(
            "INBOX",
            b"Message-ID: <merges-both@example.com>\r\n\
References: <root-a@example.com> <root-b@example.com>\r\n\
From: c@example.com\r\n\
To: me@example.com\r\n\
Subject: Re: Topic A\r\n\
Date: 01 Jan 2024 09:10:00 +0000\r\n\
Content-Type: text/plain\r\n\r\nties them together\r\n"
                .to_vec(),
            flags_seen(),
            None,
        );
        s.append(
            "Archive",
            raw_message(
                "root-b@example.com",
                None,
                "b@example.com",
                "Topic B",
                "01 Jan 2024 09:05:00 +0000",
                "the body of B, which the snippet must still say after this wake",
            ),
            flags_seen(),
            None,
        );
    }
    passes::sync_once(&env.ctx(), &mut session, &mut labels, &mut threads).await.unwrap();

    let root_a_after = env
        .vault
        .message_by_message_id_header(env.account_id, "root-a@example.com")
        .unwrap()
        .expect("root a still stored");
    let root_b_after = env
        .vault
        .message_by_message_id_header(env.account_id, "root-b@example.com")
        .unwrap()
        .expect("root b still stored");

    assert_eq!(
        root_b_after.thread_id, root_a_after.thread_id,
        "root b must keep the merged thread, not revert to a stale cached one"
    );
    assert_eq!(root_b_after.pack, pack_before, "root b's pack ref must not be reset to pending");
    assert_eq!(root_b_after.snippet, snippet_before, "root b's real snippet must survive");

    let locations = env.vault.mail_message_locations(root_b_after.id).unwrap();
    assert_eq!(
        locations.len(),
        2,
        "root b must now be filed in both INBOX and Archive: {locations:?}"
    );
}

#[tokio::test]
async fn gmail_labels_put_one_message_in_both_the_inbox_and_a_user_label() {
    let env = TestEnv::new();
    let server = gmail_server();
    {
        let mut s = server.lock().unwrap();
        s.append(
            "All Mail",
            raw_message(
                "labelled@example.com",
                None,
                "a@example.com",
                "Travel",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            Some(GmailMeta { thrid: 1, msgid: 1, labels: vec!["\\Inbox".into(), "Travel".into()] }),
        );
    }
    let mut session = FakeMailSession::new(server);
    let mailboxes = env.sync(&mut session).await;

    // Only the five folder-backed mailboxes are ever synced as such --
    // `INBOX` itself must not appear.
    assert_eq!(mailboxes.len(), 5, "All Mail, Sent, Drafts, Spam, Trash only");
    assert!(mailboxes.iter().all(|m| m.remote_name != "INBOX"));

    let all_accounts_mailboxes = env.vault.mailboxes(env.account_id).unwrap();
    let inbox_label = all_accounts_mailboxes
        .iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .expect("an inbox label mailbox should have been minted");
    let travel_label = all_accounts_mailboxes
        .iter()
        .find(|m| m.remote_name == "Travel")
        .expect("a user label mailbox should have been minted");

    let inbox_page =
        env.vault.list_threads(inbox_label.id, &ThreadFilter::default(), None, 10).unwrap();
    let travel_page =
        env.vault.list_threads(travel_label.id, &ThreadFilter::default(), None, 10).unwrap();
    assert_eq!(inbox_page.threads.len(), 1, "the message appears in the inbox label's list");
    assert_eq!(travel_page.threads.len(), 1, "and in the user label's list");
    assert_eq!(
        inbox_page.threads[0].id, travel_page.threads[0].id,
        "it is the same thread in both, not two copies"
    );
}

/// Steady state: a message archived in another client loses `\Inbox`.
/// `changes_since` reports the modseq bump in `flag_changes`, which is
/// what `refresh_gmail_labels`'s targeted path reads `X-GM-LABELS` for.
#[tokio::test]
async fn archiving_in_another_client_removes_the_message_from_the_inbox_label() {
    let env = TestEnv::new();
    let server = gmail_server();
    let uid;
    {
        let mut s = server.lock().unwrap();
        uid = s.append(
            "All Mail",
            raw_message(
                "archived-elsewhere@example.com",
                None,
                "a@example.com",
                "Will be archived",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            Some(GmailMeta { thrid: 1, msgid: 1, labels: vec!["\\Inbox".into()] }),
        );
    }
    let mut session = FakeMailSession::new(server.clone());
    env.sync(&mut session).await;
    let inbox_label = env
        .vault
        .mailboxes(env.account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .expect("the inbox label mailbox exists after the first sync");
    assert_eq!(
        env.vault
            .list_threads(inbox_label.id, &ThreadFilter::default(), None, 10)
            .unwrap()
            .threads
            .len(),
        1,
        "the message starts in the inbox label's list"
    );

    // Archived elsewhere: `\Inbox` dropped, `MODSEQ` bumped, exactly what a
    // real Gmail label change looks like from this side.
    {
        let mut s = server.lock().unwrap();
        s.set_gmail_labels("All Mail", uid, Vec::new());
    }
    env.sync(&mut session).await;

    let after = env.vault.list_threads(inbox_label.id, &ThreadFilter::default(), None, 10).unwrap();
    assert!(after.threads.is_empty(), "the archived message must leave the inbox label's list");
}

/// The fallback: a label changes with no `MODSEQ` bump at all -- the case
/// `changes_since` cannot diff by construction. Only the newest-N re-check
/// notices it.
#[tokio::test]
async fn a_label_change_with_no_modseq_bump_is_still_caught_by_the_fallback() {
    let env = TestEnv::new();
    let server = gmail_server();
    let uid;
    {
        let mut s = server.lock().unwrap();
        uid = s.append(
            "All Mail",
            raw_message(
                "silently-relabelled@example.com",
                None,
                "a@example.com",
                "Will be relabelled without a modseq bump",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            Some(GmailMeta { thrid: 2, msgid: 2, labels: vec!["\\Inbox".into()] }),
        );
    }
    let mut session = FakeMailSession::new(server.clone());
    env.sync(&mut session).await;
    let inbox_label = env
        .vault
        .mailboxes(env.account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .unwrap();
    assert_eq!(
        env.vault
            .list_threads(inbox_label.id, &ThreadFilter::default(), None, 10)
            .unwrap()
            .threads
            .len(),
        1
    );

    {
        let mut s = server.lock().unwrap();
        s.set_gmail_labels_without_a_modseq_bump("All Mail", uid, Vec::new());
    }
    env.sync(&mut session).await;

    let after = env.vault.list_threads(inbox_label.id, &ThreadFilter::default(), None, 10).unwrap();
    assert!(
        after.threads.is_empty(),
        "the newest-N fallback must catch a label change changes_since never reported"
    );
}

/// Regression: `refresh_gmail_labels`'s added-label branch used to
/// re-ingest the message with its *pre-update* label set, reverting the
/// very label this pass just added and leaving the next pass to "notice"
/// the same difference again, forever. A message un-archived elsewhere
/// (gains `\Inbox`) must land in the inbox label's list, and the row's own
/// `labels` must actually say so -- not just the mailbox membership -- and
/// stay that way after a second, otherwise-quiet sync.
#[tokio::test]
async fn un_archiving_in_another_client_adds_the_message_to_the_inbox_label_and_it_sticks() {
    let env = TestEnv::new();
    let server = gmail_server();
    let uid;
    {
        let mut s = server.lock().unwrap();
        uid = s.append(
            "All Mail",
            raw_message(
                "unarchived-elsewhere@example.com",
                None,
                "a@example.com",
                "Will be un-archived",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            Some(GmailMeta { thrid: 3, msgid: 3, labels: Vec::new() }),
        );
    }
    let mut session = FakeMailSession::new(server.clone());
    env.sync(&mut session).await;
    // No `\Inbox` label mailbox exists at all yet -- it is minted the first
    // time some message actually carries the label (see `LabelMailboxes`'s
    // own docs), which has not happened yet for an account whose one
    // message starts outside the inbox.
    assert!(
        env.vault.mailboxes(env.account_id).unwrap().iter().all(|m| m.role != MailboxRole::Inbox),
        "no inbox label mailbox should exist before anything has ever carried \\Inbox"
    );

    // Un-archived elsewhere: `\Inbox` added, `MODSEQ` bumped -- the mirror
    // image of the removal case above.
    {
        let mut s = server.lock().unwrap();
        s.set_gmail_labels("All Mail", uid, vec!["\\Inbox".into()]);
    }
    env.sync(&mut session).await;

    let inbox_label = env
        .vault
        .mailboxes(env.account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .expect("the addition must mint the inbox label mailbox");
    let after = env.vault.list_threads(inbox_label.id, &ThreadFilter::default(), None, 10).unwrap();
    assert_eq!(
        after.threads.len(),
        1,
        "the un-archived message must land in the inbox label's list"
    );

    let all_mail = env
        .vault
        .mailboxes(env.account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::All)
        .unwrap();
    let stored = env.vault.message_by_uid(all_mail.id, uid).unwrap().unwrap();
    assert_eq!(
        stored.labels,
        vec!["\\Inbox".to_string()],
        "the row's own label set must reflect the addition, not the pre-update snapshot"
    );

    // A further, otherwise-quiet sync must not revert the addition again --
    // exactly what the bug being regressed did, on every single pass.
    env.sync(&mut session).await;
    let stored_again = env.vault.message_by_uid(all_mail.id, uid).unwrap().unwrap();
    assert_eq!(stored_again.labels, vec!["\\Inbox".to_string()]);
}

#[tokio::test]
async fn an_auth_failure_sets_needs_sign_in_and_the_task_stops() {
    let dir = tempfile::tempdir().unwrap();
    let vault =
        everyday_vault::create(dir.path(), VaultConfig { password: None, ..Default::default() })
            .unwrap();

    let mut account = Account::new(Provider::Custom, "me@example.com");
    account.auth = AuthMethod::Password { username: "me@example.com".into() };
    let account_id = account.id;

    // `Service::set` opens mail's storage itself, since this vault is
    // unlocked from the moment it is created -- see `Service::set`'s own
    // docs. That happens before the account below is saved, so it
    // registers no task for it; this test drives `run_account_with`
    // directly instead, which is what lets it assert on the *outcome* of
    // one attempt rather than the supervisor's restart loop around it.
    let svc = Arc::new(crate::service::Service::new());
    let vault_arc = svc.set(vault);
    vault_arc.save_account(&account).unwrap();
    // No secret at all: `credential::resolve` must refuse before a
    // connection is ever attempted -- `never_connects` panics if it is.

    let (_stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let outcome = super::task::run_account_with(
        svc.clone(),
        vault_arc.clone(),
        account_id,
        stop_rx,
        never_connects,
        |_account, _svc, _vault| FakeSender::default(),
    )
    .await
    .expect("an auth failure must not be an error the supervisor retries");

    assert_eq!(outcome, crate::supervisor::Outcome::Done);
    let account = vault_arc.account(account_id).unwrap();
    assert!(matches!(account.status, AccountStatus::NeedsSignIn { .. }), "{:?}", account.status);
}

async fn never_connects(
    _account: Account,
    _credential: Credential,
) -> SessionResult<FakeMailSession> {
    panic!("must not attempt a connection once the credential itself is refused")
}

// ---------------------------------------------------------------------
// Wiring the outbox into the account task
// ---------------------------------------------------------------------

/// A fake `everyday_mail::outbox::Sender`: accepts every message handed to
/// it -- there is no SMTP server here to disagree -- and records what it
/// was asked to send, for a test to inspect.
#[derive(Default, Clone)]
struct FakeSender {
    sent: Arc<Mutex<Vec<Built>>>,
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
fn service_test_env() -> (Arc<crate::service::Service>, Arc<Vault>, AccountId, tempfile::TempDir) {
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

/// The contact index learns from ingest: a `Sent` message's recipients
/// count as "sent to", and everyone else's `From` counts as "received
/// from" -- see `passes::sync_headers`'s own wiring.
#[tokio::test]
async fn a_sync_pass_teaches_the_contact_index_from_sent_and_received_mail() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        s.append(
            "INBOX",
            raw_message(
                "from-alice@example.com",
                None,
                "Alice <alice@example.com>",
                "Hello",
                "01 Jan 2024 10:00:00 +0000",
                "hi",
            ),
            flags_seen(),
            None,
        );
        let sent_to_bob = b"Message-ID: <to-bob@example.com>\r\n\
From: me@example.com\r\n\
To: Bob <bob@example.com>\r\n\
Subject: Re: plans\r\n\
Date: 01 Jan 2024 10:05:00 +0000\r\n\
Content-Type: text/plain\r\n\r\nsounds good\r\n"
            .to_vec();
        s.append("Sent", sent_to_bob, flags_seen(), None);
    }
    let mut session = FakeMailSession::new(server);
    let statuses = svc.mail_statuses().unwrap();
    let ctx = SyncContext {
        vault: &vault,
        account_id,
        packs: svc.packs().unwrap(),
        index: svc.mail_index().unwrap(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: svc.mail_unread_cache(),
        contacts: svc.mail_contacts(),
        identities: Vec::new(),
    };
    let mut labels = LabelMailboxes::new(&vault, account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    let index = svc.mail_contacts().unwrap();
    let suggestions = index.suggest("", 10);
    let by_email: HashMap<&str, &everyday_core::mail::Address> =
        suggestions.iter().map(|a| (a.email.as_str(), a)).collect();
    assert!(
        by_email.contains_key("alice@example.com"),
        "received-from must be learned: {suggestions:?}"
    );
    assert!(by_email.contains_key("bob@example.com"), "sent-to must be learned: {suggestions:?}");
}

/// Regression: the account's own address used to be recorded as a
/// correspondent -- a message from oneself (received) or to oneself
/// (sent, e.g. a BCC-to-self) climbing straight up one's own autocomplete
/// -- and a Drafts folder, full of messages *from* the account itself, was
/// fair game for "received from" too, drafts' own recipients included,
/// since the whole contact-recording block ran regardless of which
/// mailbox a header came from.
#[tokio::test]
async fn the_accounts_own_address_and_its_drafts_are_never_recorded_as_a_contact() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        s.mailbox("Drafts", Some(Role::Drafts));
        // A real correspondent, both ways -- must still be learned.
        s.append(
            "INBOX",
            raw_message(
                "from-alice@example.com",
                None,
                "Alice <alice@example.com>",
                "Hello",
                "01 Jan 2024 10:00:00 +0000",
                "hi",
            ),
            flags_seen(),
            None,
        );
        // A note to self, landing in the inbox: `From` is the account's
        // own address.
        let received_from_self = b"Message-ID: <self-received@example.com>\r\n\
From: me@example.com\r\n\
To: me@example.com\r\n\
Subject: Note to self\r\n\
Date: 01 Jan 2024 10:01:00 +0000\r\n\
Content-Type: text/plain\r\n\r\nremember this\r\n"
            .to_vec();
        s.append("INBOX", received_from_self, flags_seen(), None);
        // A BCC-to-self style send: the account's own address is one of
        // the `To` addresses of its own Sent copy.
        let sent_to_self = b"Message-ID: <self-sent@example.com>\r\n\
From: me@example.com\r\n\
To: me@example.com\r\n\
Subject: Reminder\r\n\
Date: 01 Jan 2024 10:02:00 +0000\r\n\
Content-Type: text/plain\r\n\r\nping\r\n"
            .to_vec();
        s.append("Sent", sent_to_self, flags_seen(), None);
        // A draft: entirely from the account itself, to someone real --
        // neither side should ever be recorded from a Drafts folder.
        let draft = b"Message-ID: <draft-only@example.com>\r\n\
From: me@example.com\r\n\
To: someone-else@example.com\r\n\
Subject: Draft\r\n\
Date: 01 Jan 2024 10:03:00 +0000\r\n\
Content-Type: text/plain\r\n\r\ndraft body\r\n"
            .to_vec();
        s.append("Drafts", draft, flags_seen(), None);
    }
    let mut session = FakeMailSession::new(server);
    let statuses = svc.mail_statuses().unwrap();
    let ctx = SyncContext {
        vault: &vault,
        account_id,
        packs: svc.packs().unwrap(),
        index: svc.mail_index().unwrap(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: svc.mail_unread_cache(),
        contacts: svc.mail_contacts(),
        identities: vec!["me@example.com".to_string()],
    };
    let mut labels = LabelMailboxes::new(&vault, account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    let index = svc.mail_contacts().unwrap();
    let suggestions = index.suggest("", 10);
    let emails: Vec<&str> = suggestions.iter().map(|a| a.email.as_str()).collect();
    assert!(
        emails.contains(&"alice@example.com"),
        "a real correspondent must still be learned: {emails:?}"
    );
    assert!(
        !emails.contains(&"me@example.com"),
        "the account's own address must never be recorded as a contact: {emails:?}"
    );
    assert!(
        !emails.contains(&"someone-else@example.com"),
        "a draft's own recipient must not be recorded: {emails:?}"
    );
}

#[tokio::test]
async fn draining_an_archive_moves_the_message_on_the_server_and_completes_the_op() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        s.append(
            "INBOX",
            raw_message(
                "archive-me@example.com",
                None,
                "alice@example.com",
                "Please file this",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            None,
        );
    }

    let mut session = FakeMailSession::new(server.clone());
    let statuses = svc.mail_statuses().unwrap();
    let ctx = SyncContext {
        vault: &vault,
        account_id,
        packs: svc.packs().unwrap(),
        index: svc.mail_index().unwrap(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: svc.mail_unread_cache(),
        contacts: svc.mail_contacts(),
        identities: Vec::new(),
    };
    let mut labels = LabelMailboxes::new(&vault, account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    let inbox = vault
        .mailboxes(account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .unwrap();
    let page = vault.list_threads(inbox.id, &ThreadFilter::default(), None, 10).unwrap();
    let thread_id = page.threads[0].id;

    let op =
        vault.apply_thread_ops(&[thread_id], OpKind::Archive, Origin::Person).unwrap().remove(0);

    let sender = FakeSender::default();
    let report =
        crate::outbox::drain_outbox(&svc, account_id, &mut session, &sender).await.unwrap();
    assert_eq!(report.done, 1, "{report:?}");
    assert_eq!(report.failed, 0, "{report:?}");
    assert_eq!(vault.op(op.id).unwrap().state, OpState::Done);

    let s = server.lock().unwrap();
    assert!(
        s.mailboxes["INBOX"].messages.is_empty(),
        "the message must have left the inbox on the server"
    );
    assert_eq!(
        s.mailboxes["Archive"].messages.len(),
        1,
        "and landed in the account's Archive mailbox"
    );
}

#[tokio::test]
async fn draining_a_send_appends_the_sent_copy_and_marks_the_draft_sent() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    let mut session = FakeMailSession::new(server.clone());

    // A sync first, empty mailboxes and all: `special_use(Sent)` -- which
    // `everyday_mail::outbox::send` reads to know where to `APPEND` its own
    // copy -- answers from the vault's own `mailboxes` table, which nothing
    // populates before the first sync has discovered them.
    let statuses = svc.mail_statuses().unwrap();
    let ctx = SyncContext {
        vault: &vault,
        account_id,
        packs: svc.packs().unwrap(),
        index: svc.mail_index().unwrap(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: svc.mail_unread_cache(),
        contacts: svc.mail_contacts(),
        identities: Vec::new(),
    };
    let mut labels = LabelMailboxes::new(&vault, account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    let mut draft = Draft::new(account_id, "me@example.com", Origin::Person);
    draft.to = vec![Address::bare("bob@example.com")];
    draft.subject = "Hello".into();
    draft.body_html = "<p>Hi</p>".into();
    vault.save_draft(&draft).unwrap();
    let (draft, op) = vault.queue_draft_send(draft.id, Timestamp::now(), Origin::Person).unwrap();

    let sender = FakeSender::default();
    let report =
        crate::outbox::drain_outbox(&svc, account_id, &mut session, &sender).await.unwrap();
    assert_eq!(report.done, 1, "{report:?}");
    assert_eq!(sender.sent.lock().unwrap().len(), 1, "the sender must have been asked to send it");
    assert_eq!(vault.op(op.id).unwrap().state, OpState::Done);
    assert_eq!(vault.draft(draft.id).unwrap().state, everyday_core::mail::DraftState::Sent);

    let s = server.lock().unwrap();
    assert_eq!(
        s.mailboxes["Sent"].messages.len(),
        1,
        "a non-Gmail server needs its own Sent copy appended"
    );
}

/// The regression for the duplicate-send risk `crate::outbox::already_sent`
/// exists to close: a `Send` op recovered from `InFlight` whose draft's
/// (stable) `Message-ID` is already in Sent must be recognised as already
/// sent, not sent again.
#[tokio::test]
async fn recovering_a_send_already_on_the_server_is_not_resent() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    let mut session = FakeMailSession::new(server.clone());

    let statuses = svc.mail_statuses().unwrap();
    let ctx = SyncContext {
        vault: &vault,
        account_id,
        packs: svc.packs().unwrap(),
        index: svc.mail_index().unwrap(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: svc.mail_unread_cache(),
        contacts: svc.mail_contacts(),
        identities: Vec::new(),
    };
    let mut labels = LabelMailboxes::new(&vault, account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    // A draft whose `Message-ID` was already stamped -- what
    // `everyday_mail::outbox::send` does before it ever calls the sender,
    // so it survives a crash between the two.
    let mut draft = Draft::new(account_id, "me@example.com", Origin::Person);
    draft.to = vec![Address::bare("bob@example.com")];
    draft.subject = "Already sent".into();
    draft.body_html = "<p>Hi</p>".into();
    draft.message_id = Some("stable-id@example.com".into());
    vault.save_draft(&draft).unwrap();
    let (draft, op) = vault.queue_draft_send(draft.id, Timestamp::now(), Origin::Person).unwrap();

    // The crash this test simulates happened *after* SMTP accepted the
    // message: the server genuinely has a Sent copy under that exact
    // `Message-ID`, and the op is stranded `InFlight` because this process
    // never got to say so.
    {
        let mut s = server.lock().unwrap();
        s.append(
            "Sent",
            raw_message(
                "stable-id@example.com",
                None,
                "me@example.com",
                "Already sent",
                "01 Jan 2024 10:00:00 +0000",
                "hi",
            ),
            flags_seen(),
            None,
        );
    }
    let mut stranded = vault.op(op.id).unwrap();
    stranded.transition_to(OpState::InFlight).unwrap();
    vault.update_op(&stranded).unwrap();

    crate::outbox::recover_inflight_ops(&svc, account_id, &mut session).await.unwrap();

    assert_eq!(
        vault.op(op.id).unwrap().state,
        OpState::Done,
        "already on the server -- recovered as done, not requeued to send again"
    );
    assert_eq!(vault.draft(draft.id).unwrap().state, everyday_core::mail::DraftState::Sent);

    // And, proof this genuinely skipped sending rather than merely getting
    // lucky: nothing is left pending to drain, and the sender below is
    // never asked to send anything.
    let sender = FakeSender::default();
    let report =
        crate::outbox::drain_outbox(&svc, account_id, &mut session, &sender).await.unwrap();
    assert_eq!(report.attempted, 0, "{report:?}");
    assert!(sender.sent.lock().unwrap().is_empty(), "must not have been sent a second time");
    assert_eq!(
        s_message_count(&server, "Sent"),
        1,
        "still exactly the one copy the server already had"
    );
}

fn s_message_count(server: &Arc<Mutex<FakeServer>>, mailbox: &str) -> usize {
    server.lock().unwrap().mailboxes[mailbox].messages.len()
}

/// Regression: `run_account_with` used to `IDLE` on whatever mailbox
/// `sync_once`'s own last pass happened to leave selected -- never the
/// inbox, since `discovery::discover` always sorts the inbox (All Mail, on
/// Gmail) first and every other mailbox is therefore visited *after* it.
/// Here, only Trash has a message of its own, so `bodies_pass`'s own
/// `SELECT`, called once per mailbox with anything still pending, is
/// issued for Trash and nothing after it -- exactly the shape that leaves
/// a real IMAP session sitting on the wrong mailbox when `run_account_with`
/// asks for `IDLE` without first pointing the session back at the inbox.
#[tokio::test]
async fn idle_is_issued_on_the_inbox_not_whatever_mailbox_was_selected_last() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = gmail_server();
    {
        let mut s = server.lock().unwrap();
        s.append(
            "All Mail",
            raw_message(
                "idle-target@example.com",
                None,
                "a@example.com",
                "Hi",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            Some(GmailMeta { thrid: 1, msgid: 1, labels: vec!["\\Inbox".into()] }),
        );
        // The only other mailbox with anything in it -- so it is the one
        // `bodies_pass` reselects last, and (without the fix) the one
        // `IDLE` would be issued on.
        s.append(
            "Trash",
            raw_message(
                "trash-target@example.com",
                None,
                "a@example.com",
                "Bye",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            None,
        );
    }
    let session = FakeMailSession::new(server).blocking_idle();
    let idle_selections = session.idle_selections();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);

    let svc_task = svc.clone();
    let vault_task = vault.clone();
    let handle = tokio::spawn(async move {
        super::task::run_account_with(
            svc_task,
            vault_task,
            account_id,
            stop_rx,
            move |_account, _credential| {
                let session = session.clone();
                async move { Ok(session) }
            },
            |_account, _svc, _vault| FakeSender::default(),
        )
        .await
    });

    settle(|| !idle_selections.lock().unwrap().is_empty()).await;
    stop_tx.send(true).unwrap();
    handle.await.unwrap().unwrap();

    let selections = idle_selections.lock().unwrap();
    assert_eq!(
        selections.first().cloned().flatten().as_deref(),
        Some("All Mail"),
        "IDLE must be issued on All Mail, Gmail's own inbox-equivalent -- not Trash, \
         which is where sync_once's own last pass would otherwise have left the session: {selections:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_notify_wakes_the_idle_loop_promptly_rather_than_waiting_for_the_poll() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        s.append(
            "INBOX",
            raw_message(
                "notify-me@example.com",
                None,
                "alice@example.com",
                "Wake up",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            None,
        );
    }
    let session = FakeMailSession::new(server).blocking_idle();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);

    let svc_task = svc.clone();
    let vault_task = vault.clone();
    let handle = tokio::spawn(async move {
        super::task::run_account_with(
            svc_task,
            vault_task,
            account_id,
            stop_rx,
            move |_account, _credential| {
                let session = session.clone();
                async move { Ok(session) }
            },
            |_account, _svc, _vault| FakeSender::default(),
        )
        .await
    });

    // Let the task connect, run its first sync, drain its (empty) outbox,
    // and settle into `IDLE` -- `block_idle` means it parks there rather
    // than returning, so only `stop` or a notification can move it on.
    settle(|| {
        matches!(
            svc.mail_statuses()
                .unwrap()
                .all()
                .iter()
                .find(|p| p.account_id == account_id)
                .map(|p| p.phase),
            Some(super::status::Phase::Idling)
        )
    })
    .await;

    let inbox = vault
        .mailboxes(account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .unwrap();
    let page = vault.list_threads(inbox.id, &ThreadFilter::default(), None, 10).unwrap();
    let thread_id = page.threads[0].id;
    let op =
        vault.apply_thread_ops(&[thread_id], OpKind::Archive, Origin::Person).unwrap().remove(0);

    svc.notify_outbox(account_id);
    // Not a single virtual millisecond is advanced from here on: the clock
    // stays exactly where `start_paused = true` left it. `POLL_INTERVAL`
    // is five minutes and `OUTBOX_RETRY_INTERVAL` three seconds, so the
    // only way this op can reach `Done` without either timer ever firing
    // is the notify itself having woken the `IDLE` `select!` -- which is
    // exactly the latency this test exists to prove.
    settle(|| vault.op(op.id).map(|o| o.state == OpState::Done).unwrap_or(false)).await;

    stop_tx.send(true).unwrap();
    handle.await.unwrap().unwrap();
}

/// The regression for the bug [`super::task::sleep_until_due`] fixes: a
/// send-at op queued ten seconds out must drain at about ten seconds, not
/// at `POLL_INTERVAL` (five minutes).
///
/// No paused clock here, unlike this module's other timing tests: `not_before`
/// due-ness is decided by [`jiff::Timestamp::now`] (the real wall clock),
/// never by `tokio::time`'s virtual one -- `next_pending_wake` converts a
/// real duration into a virtual sleep exactly once, the same trade
/// `crate::token_cache::deadline_from` makes, but the *due* check itself in
/// `crate::outbox::drain_outbox` reads the wall clock fresh on every drain.
/// So this test spends ten real seconds proving it, bounded well short of
/// `POLL_INTERVAL` by [`settle_up_to`]'s own timeout, which is the one
/// thing a wrong fix (falling back to `POLL_INTERVAL`) cannot pass short of
/// genuinely waiting five minutes.
#[tokio::test]
async fn a_send_at_op_drains_at_its_own_time_not_the_poll_interval() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    let session = FakeMailSession::new(server).blocking_idle();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);

    let svc_task = svc.clone();
    let vault_task = vault.clone();
    let handle = tokio::spawn(async move {
        super::task::run_account_with(
            svc_task,
            vault_task,
            account_id,
            stop_rx,
            move |_account, _credential| {
                let session = session.clone();
                async move { Ok(session) }
            },
            |_account, _svc, _vault| FakeSender::default(),
        )
        .await
    });

    settle(|| {
        matches!(
            svc.mail_statuses()
                .unwrap()
                .all()
                .iter()
                .find(|p| p.account_id == account_id)
                .map(|p| p.phase),
            Some(super::status::Phase::Idling)
        )
    })
    .await;

    let start = std::time::Instant::now();
    let mut draft = Draft::new(account_id, "me@example.com", Origin::Person);
    draft.to = vec![Address::bare("bob@example.com")];
    draft.subject = "Later".into();
    draft.body_html = "<p>Later</p>".into();
    vault.save_draft(&draft).unwrap();
    let not_before = Timestamp::now() + jiff::SignedDuration::from_secs(10);
    let (_, op) = vault.queue_draft_send(draft.id, not_before, Origin::Person).unwrap();
    // What the real `send_draft` command does right after `queue_draft_send`
    // -- see `domains::mail`'s own wiring -- so this test enqueues the op
    // exactly the way a person actually would, rather than relying on the
    // supervisor loop stumbling onto it by some other path. It also proves
    // the fix is doing the work, not this call: the notify only tells the
    // loop to notice a not-yet-due op and compute when it will be, per
    // `crate::mailsync::task`'s own module docs on its fourth `select!` arm.
    svc.notify_outbox(account_id);

    settle_up_to(std::time::Duration::from_secs(60), || {
        vault.op(op.id).map(|o| o.state == OpState::Done).unwrap_or(false)
    })
    .await;
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "took {elapsed:?} to drain a ten-second delay -- far too close to the five-minute poll"
    );

    stop_tx.send(true).unwrap();
    handle.await.unwrap().unwrap();
}

/// Poll the runtime until `f` is true or `tries` yields have gone by,
/// without needing virtual time to move -- the same tolerance
/// `crate::supervisor`'s own tests give a task that only makes progress
/// between this test's `.await` points on a current-thread runtime.
///
/// A real, wall-clock sleep between yields as well as the yield itself: the
/// spawned account task's own `bodies_pass` finishes its work on
/// `tokio::task::spawn_blocking`'s real OS thread pool, doing genuine
/// SQLite writes -- work that takes actual wall-clock time regardless of
/// `start_paused`, which freezes only `tokio::time`'s virtual clock. A pure
/// `yield_now` spin can complete its whole budget of iterations in
/// microseconds, far faster than that thread pool can finish a single
/// `fsync`, and would never see it -- `std::thread::sleep`, not
/// `tokio::time::sleep`, is what actually waits here.
async fn settle(f: impl Fn() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::task::yield_now().await;
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(f(), "did not settle");
}

// ---------------------------------------------------------------------
// Categorisation at ingest -- docs/plans/mail.md's phase 7, rules half
// ---------------------------------------------------------------------

fn raw_newsletter(message_id: &str, from: &str, subject: &str, date: &str) -> Vec<u8> {
    format!(
        "Message-ID: <{message_id}>\r\n\
         From: {from}\r\n\
         To: me@example.com\r\n\
         Subject: {subject}\r\n\
         Date: {date}\r\n\
         List-Id: Example Weekly <weekly.example.com>\r\n\
         List-Unsubscribe: <https://example.com/unsub>\r\n\
         Content-Type: text/plain\r\n\r\n\
         Hello.\r\n"
    )
    .into_bytes()
}

/// Categorisation is always on: a fresh message lands with a category the
/// moment it is ingested, with no `mail_ai` switch anywhere near it -- the
/// rules-based half sends nothing anywhere and needs no consent.
#[tokio::test]
async fn categorisation_runs_at_ingest_with_no_switch_to_turn_on() {
    let env = TestEnv::new();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        let news = raw_newsletter(
            "news-1@example.com",
            "weekly@example.com",
            "This week",
            "01 Jan 2024 09:00:00 +0000",
        );
        s.append("INBOX", news, flags_seen(), None);
        let plain = raw_message(
            "plain-1@example.com",
            None,
            "dana@example.com",
            "Hi",
            "01 Jan 2024 09:00:00 +0000",
            "hello",
        );
        s.append("INBOX", plain, flags_seen(), None);
    }
    let mut session = FakeMailSession::new(server);
    env.sync(&mut session).await;

    let news = env
        .vault
        .message_by_message_id_header(env.account_id, "news-1@example.com")
        .unwrap()
        .expect("newsletter stored");
    assert_eq!(news.category, Some(Category::Newsletter));
    let (news_thread, _) = env.vault.thread(news.thread_id).unwrap();
    assert_eq!(
        news_thread.category,
        Some(Category::Newsletter),
        "the thread mirrors its newest message's category"
    );

    let plain = env
        .vault
        .message_by_message_id_header(env.account_id, "plain-1@example.com")
        .unwrap()
        .expect("plain message stored");
    assert_eq!(
        plain.category,
        Some(Category::Other),
        "a first message from a stranger with no signals at all is Other"
    );
}

/// `set_thread_category`'s underlying mechanism, `Vault::correct_mail_category`:
/// records a correction, applies it to that sender's existing mail, and a
/// later message from the same sender picks it up at ingest with no second
/// correction needed.
#[tokio::test]
async fn a_correction_outranks_the_rules_and_reaches_existing_and_future_mail() {
    let env = TestEnv::new();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        let raw = raw_message(
            "news-2@example.com",
            None,
            "weekly@example.com",
            "This week",
            "01 Jan 2024 09:00:00 +0000",
            "hi",
        );
        s.append("INBOX", raw, flags_seen(), None);
    }
    let mut session = FakeMailSession::new(server.clone());
    env.sync(&mut session).await;
    let before = env
        .vault
        .message_by_message_id_header(env.account_id, "news-2@example.com")
        .unwrap()
        .unwrap();
    assert_eq!(before.category, Some(Category::Other));

    let changed = env
        .vault
        .correct_mail_category(env.account_id, "weekly@example.com", Category::Important)
        .unwrap();
    assert_eq!(changed, 1, "the one existing message from this sender");
    let after = env
        .vault
        .message_by_message_id_header(env.account_id, "news-2@example.com")
        .unwrap()
        .unwrap();
    assert_eq!(after.category, Some(Category::Important));

    {
        let mut s = server.lock().unwrap();
        let raw = raw_message(
            "news-3@example.com",
            None,
            "weekly@example.com",
            "Next week",
            "02 Jan 2024 09:00:00 +0000",
            "hi again",
        );
        s.append("INBOX", raw, flags_seen(), None);
    }
    env.sync(&mut session).await;
    let fresh = env
        .vault
        .message_by_message_id_header(env.account_id, "news-3@example.com")
        .unwrap()
        .unwrap();
    assert_eq!(
        fresh.category,
        Some(Category::Important),
        "a fresh message from a corrected sender is categorised by the correction at ingest"
    );
}

/// `recategorize_mail`'s one-off backfill: a message stuck with a stale
/// category (as it would be, ingested before this build knew a rule that
/// now applies) is put right by asking for it again, with no correction
/// involved at all.
///
/// The rule is saved directly, through `save_category_rules` alone rather
/// than `correct_mail_category`'s own sweep, so the message's category is
/// genuinely stale -- still whatever the rules said at ingest -- until the
/// backfill below asks again. `set_mail_message_category` is deliberately
/// not how this test creates staleness any more: that call writes through
/// as a model's own answer (see `everyday_core::mail::CategorySource`), and
/// a model's answer is exactly what `recategorize_mail_never_overrides_a_models_own_answer`,
/// just below, checks the backfill must never touch.
#[tokio::test]
async fn recategorize_mail_backfills_a_stale_category() {
    let env = TestEnv::new();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        let raw = raw_message(
            "old-1@example.com",
            None,
            "noreply@service.example.com",
            "Receipt",
            "01 Jan 2024 09:00:00 +0000",
            "hi",
        );
        s.append("INBOX", raw, flags_seen(), None);
    }
    let mut session = FakeMailSession::new(server);
    env.sync(&mut session).await;

    let msg = env
        .vault
        .message_by_message_id_header(env.account_id, "old-1@example.com")
        .unwrap()
        .unwrap();
    assert_eq!(msg.category, Some(Category::Notification), "the rules already got this right");

    let mut rules = env.vault.category_rules(env.account_id).unwrap();
    rules.set_sender("noreply@service.example.com", Category::Important);
    env.vault.save_category_rules(env.account_id, &rules).unwrap();

    let changed = env.vault.recategorize_mail(env.account_id).unwrap();
    assert_eq!(changed, 1);
    let fixed = env.vault.mail_message(msg.id).unwrap();
    assert_eq!(fixed.category, Some(Category::Important));
}

/// Regression: `recategorize_mail`'s backfill must never override a
/// model's own answer -- see `everyday_core::mail::CategorySource`'s own
/// docs on the ranking that makes this true. Before the fix, a category
/// set through `set_mail_message_category` left no trace of where it came
/// from, so the very next backfill blindly reasserted whatever the rules
/// engine said instead, discarding the model's answer.
#[tokio::test]
async fn recategorize_mail_never_overrides_a_models_own_answer() {
    let env = TestEnv::new();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        let raw = raw_message(
            "old-2@example.com",
            None,
            "noreply@service.example.com",
            "Receipt",
            "01 Jan 2024 09:00:00 +0000",
            "hi",
        );
        s.append("INBOX", raw, flags_seen(), None);
    }
    let mut session = FakeMailSession::new(server);
    env.sync(&mut session).await;

    let msg = env
        .vault
        .message_by_message_id_header(env.account_id, "old-2@example.com")
        .unwrap()
        .unwrap();
    env.vault.set_mail_message_category(msg.id, Category::Other).unwrap();

    let changed = env.vault.recategorize_mail(env.account_id).unwrap();
    assert_eq!(changed, 0, "a model's own answer is never the backfill's to touch");
    let after = env.vault.mail_message(msg.id).unwrap();
    assert_eq!(after.category, Some(Category::Other));
}

/// As [`settle`], but for a wait measured in real seconds rather than a
/// handful of milliseconds -- what a test that genuinely needs wall-clock
/// time to pass (a `not_before` some real duration out, checked against
/// `jiff::Timestamp::now`, which no paused `tokio::time` clock touches)
/// polls with instead.
async fn settle_up_to(timeout: std::time::Duration, f: impl Fn() -> bool) {
    let start = std::time::Instant::now();
    loop {
        if f() {
            return;
        }
        assert!(start.elapsed() < timeout, "did not settle within {timeout:?}");
        tokio::task::yield_now().await;
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

// ---------------------------------------------------------------------
// Compaction, wired through the account task
// ---------------------------------------------------------------------

/// A `stop` channel that never fires, for a `compact_account` call a test
/// does not mean to interrupt.
fn never_stops() -> tokio::sync::watch::Receiver<bool> {
    tokio::sync::watch::channel(false).1
}

/// End to end, through exactly the sequence `mailsync::task::compact_account`
/// runs: sync ten messages, delete half of them on the server and sync
/// again (marking their pack frames dead without reclaiming anything), then
/// compact. The pack store actually shrinks, every survivor's raw bytes
/// read back identical via its (remapped) ref, and search -- untouched by
/// any of this, since compaction changes no message id -- still finds
/// exactly the survivors.
#[tokio::test]
async fn compacting_through_the_account_task_shrinks_packs_and_keeps_survivors_searchable() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    let mut uids = Vec::new();
    let raws: Vec<Vec<u8>> = (0..10)
        .map(|i| {
            raw_message(
                &format!("msg{i}@example.com"),
                None,
                "a@example.com",
                &format!("Subject {i}"),
                "01 Jan 2024 10:00:00 +0000",
                "findableword",
            )
        })
        .collect();
    {
        let mut s = server.lock().unwrap();
        for raw in &raws {
            uids.push(s.append("INBOX", raw.clone(), flags_seen(), None));
        }
    }
    let mut session = FakeMailSession::new(server.clone());
    let statuses = svc.mail_statuses().unwrap();
    let ctx = SyncContext {
        vault: &vault,
        account_id,
        packs: svc.packs().unwrap(),
        index: svc.mail_index().unwrap(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: svc.mail_unread_cache(),
        contacts: svc.mail_contacts(),
        identities: Vec::new(),
    };
    let mut labels = LabelMailboxes::new(&vault, account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    // Delete half the messages on the server, then sync again -- marks
    // their pack frames dead; nothing is reclaimed yet.
    {
        let mut s = server.lock().unwrap();
        for uid in &uids[..5] {
            s.remove("INBOX", *uid);
        }
    }
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    let stop = never_stops();
    let summary = super::task::compact_account(&vault, &ctx.packs, account_id, &stop)
        .await
        .unwrap()
        .expect("half the account's one pack being dead must make compaction worthwhile");
    assert!(summary.bytes_reclaimed > 0, "the pack files must shrink");
    assert_eq!(summary.packs_rewritten, 1);

    for (i, raw) in raws.iter().enumerate().skip(5) {
        let msg = vault
            .message_by_message_id_header(account_id, &format!("msg{i}@example.com"))
            .unwrap()
            .expect("a survivor must still be a row");
        assert_eq!(
            &svc.packs().unwrap().read(&msg.pack).unwrap(),
            raw,
            "a survivor's raw bytes must read back identical through its remapped ref"
        );
    }
    let hits =
        svc.mail_index().unwrap().search(&MailQuery::parse("findableword"), 10, None).unwrap();
    assert_eq!(hits.hits.len(), 5, "search must still find exactly the survivors");
}

/// A crash between `compact` returning and the caller committing its remap
/// -- simulated by calling `PackStore::compact` directly and simply never
/// calling `remap_packs` or `drop_packs` on the result, the way a killed
/// process would leave things. Nothing is lost: every message is still
/// readable at its original, untouched address. Running the *whole*
/// production sequence again picks the same still-referenced pack back up
/// and finishes compacting it properly; the pack this first, abandoned
/// attempt wrote is itself only reclaimed by a *third* run, once something
/// newer than it exists to prove it is no longer the account's current
/// pack -- see `packstore`'s own module docs on why the account's newest
/// pack is never swept on absence from `referenced` alone.
#[tokio::test]
async fn a_crash_between_compact_and_remap_packs_loses_nothing_and_eventually_reclaims_the_orphan()
{
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    let mut uids = Vec::new();
    {
        let mut s = server.lock().unwrap();
        for i in 0..3 {
            uids.push(s.append(
                "INBOX",
                raw_message(
                    &format!("m{i}@example.com"),
                    None,
                    "a@example.com",
                    "Subject",
                    "01 Jan 2024 10:00:00 +0000",
                    "x",
                ),
                flags_seen(),
                None,
            ));
        }
    }
    let mut session = FakeMailSession::new(server.clone());
    let statuses = svc.mail_statuses().unwrap();
    let ctx = SyncContext {
        vault: &vault,
        account_id,
        packs: svc.packs().unwrap(),
        index: svc.mail_index().unwrap(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: svc.mail_unread_cache(),
        contacts: svc.mail_contacts(),
        identities: Vec::new(),
    };
    let mut labels = LabelMailboxes::new(&vault, account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();
    {
        let mut s = server.lock().unwrap();
        s.remove("INBOX", uids[0]);
    }
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    let survivors: Vec<_> = (1..3)
        .map(|i| {
            let id = vault
                .message_by_message_id_header(account_id, &format!("m{i}@example.com"))
                .unwrap()
                .unwrap()
                .id;
            let raw = raw_message(
                &format!("m{i}@example.com"),
                None,
                "a@example.com",
                "Subject",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            );
            (id, raw)
        })
        .collect();
    let original_refs: Vec<_> =
        survivors.iter().map(|(id, _)| vault.mail_message(*id).unwrap().pack).collect();

    // The crash: `compact` runs and writes its replacement pack durably,
    // but neither `remap_packs` nor `drop_packs` is ever called.
    let snapshot = vault.mail_referenced_snapshot(ctx.packs.as_ref(), account_id).unwrap();
    let crashed = ctx.packs.compact(&account_id.to_string(), &snapshot, &|| true).unwrap();
    assert!(!crashed.remap.is_empty(), "the test needs compaction to have actually run");
    let abandoned_ref = crashed.remap[0].1.clone();

    // Nothing lost: every survivor still reads at its original address --
    // the vault's own rows were never touched.
    for ((id, raw), original) in survivors.iter().zip(&original_refs) {
        assert_eq!(vault.mail_message(*id).unwrap().pack, *original);
        assert_eq!(&ctx.packs.read(original).unwrap(), raw);
    }

    // Run again, the whole production sequence this time: the still-
    // referenced pack compacts cleanly, and every survivor is readable at
    // its (now genuinely committed) new address.
    let stop = never_stops();
    let second = super::task::compact_account(&vault, &ctx.packs, account_id, &stop)
        .await
        .unwrap()
        .expect("still over the dead threshold");
    assert!(second.packs_rewritten >= 1);
    for (id, raw) in &survivors {
        let refreshed = vault.mail_message(*id).unwrap();
        assert_eq!(&ctx.packs.read(&refreshed.pack).unwrap(), raw);
    }
    // The first, abandoned attempt's pack is not reclaimed by this run --
    // it was still the account's newest pack when this run's own snapshot
    // was taken, so the guard that protects a pack still possibly open for
    // appends protected it too.
    assert!(ctx.packs.read(&abandoned_ref).is_ok(), "not yet reclaimed after only one more run");

    // A third run: now that the second run's own replacement pack is
    // newer, the first attempt's leftover is finally just an ordinary
    // orphan. Nothing was left worth compacting again by this point, so
    // this is purely an orphan sweep.
    super::task::compact_account(&vault, &ctx.packs, account_id, &stop).await.unwrap();
    assert!(
        ctx.packs.read(&abandoned_ref).is_err(),
        "the doubly-orphaned pack from the crashed first attempt must be reclaimed by now"
    );
    for (id, raw) in &survivors {
        let refreshed = vault.mail_message(*id).unwrap();
        assert_eq!(&ctx.packs.read(&refreshed.pack).unwrap(), raw);
    }
}

/// A crash between `remap_packs` committing and `drop_packs` running --
/// simulated by calling both by hand and simply skipping the second.
/// Unlike the compact-then-crash case above, the leftover pack here is
/// reclaimed by the very next run: `referenced` already names the new
/// pack, so the leftover is no longer the account's newest and is an
/// ordinary orphan from the start.
#[tokio::test]
async fn a_crash_between_remap_packs_and_drop_packs_reclaims_the_leftover_on_the_next_run() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    let mut uids = Vec::new();
    {
        let mut s = server.lock().unwrap();
        for i in 0..3 {
            uids.push(s.append(
                "INBOX",
                raw_message(
                    &format!("m{i}@example.com"),
                    None,
                    "a@example.com",
                    "Subject",
                    "01 Jan 2024 10:00:00 +0000",
                    "y",
                ),
                flags_seen(),
                None,
            ));
        }
    }
    let mut session = FakeMailSession::new(server.clone());
    let statuses = svc.mail_statuses().unwrap();
    let ctx = SyncContext {
        vault: &vault,
        account_id,
        packs: svc.packs().unwrap(),
        index: svc.mail_index().unwrap(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: svc.mail_unread_cache(),
        contacts: svc.mail_contacts(),
        identities: Vec::new(),
    };
    let mut labels = LabelMailboxes::new(&vault, account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();
    {
        let mut s = server.lock().unwrap();
        s.remove("INBOX", uids[0]);
    }
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    let survivors: Vec<_> = (1..3)
        .map(|i| {
            let id = vault
                .message_by_message_id_header(account_id, &format!("m{i}@example.com"))
                .unwrap()
                .unwrap()
                .id;
            let raw = raw_message(
                &format!("m{i}@example.com"),
                None,
                "a@example.com",
                "Subject",
                "01 Jan 2024 10:00:00 +0000",
                "y",
            );
            (id, raw)
        })
        .collect();

    let snapshot = vault.mail_referenced_snapshot(ctx.packs.as_ref(), account_id).unwrap();
    let result = ctx.packs.compact(&account_id.to_string(), &snapshot, &|| true).unwrap();
    assert!(!result.remap.is_empty());
    vault.remap_packs(account_id, &result.remap).unwrap();
    // The crash: `drop_packs` never runs.

    for (id, raw) in &survivors {
        let refreshed = vault.mail_message(*id).unwrap();
        assert_eq!(&ctx.packs.read(&refreshed.pack).unwrap(), raw, "the committed remap must read");
    }

    let stop = never_stops();
    super::task::compact_account(&vault, &ctx.packs, account_id, &stop).await.unwrap();

    // The leftover pack (`result.remap`'s old addresses all shared one) is
    // gone: `referenced` already named the new pack at the moment the
    // remap was committed, so it was never the account's newest and needed
    // no second run to be treated as an ordinary orphan.
    assert!(
        ctx.packs.read(&result.remap[0].0).is_err(),
        "the leftover pack must be reclaimed on the very next run"
    );
    for (id, raw) in &survivors {
        let refreshed = vault.mail_message(*id).unwrap();
        assert_eq!(&ctx.packs.read(&refreshed.pack).unwrap(), raw);
    }
}

/// The rate limit: two attempts inside the same hour must compact only
/// once. Tested against [`super::task::compaction_due`] directly, with
/// synthetic [`std::time::Instant`]s rather than a real hour of wall-clock
/// time.
#[test]
fn the_rate_limit_allows_one_attempt_an_hour() {
    use std::time::{Duration, Instant};
    let now = Instant::now();

    // `None` -- a freshly started task, or the first idle moment after an
    // unlock -- is always due.
    assert!(super::task::compaction_due(None, now));

    // An attempt a moment ago is not due again yet.
    let just_now = now;
    assert!(!super::task::compaction_due(Some(just_now), now + Duration::from_secs(1)));
    assert!(!super::task::compaction_due(Some(just_now), now + Duration::from_secs(60 * 60 - 1)));

    // An attempt an hour (or more) ago is due again.
    assert!(super::task::compaction_due(Some(just_now), now + Duration::from_secs(60 * 60)));
    assert!(super::task::compaction_due(Some(just_now), now + Duration::from_secs(2 * 60 * 60)));
}
