//! Unit tests for the sync engine, against a fake [`MailSession`] and a
//! real, throwaway vault -- SQLite, the real pack store, the real search
//! index, everything but the socket.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use everyday_core::account::{Account, AccountSecret, AccountStatus, AuthMethod, Provider};
use everyday_core::crypto::NullCipher;
use everyday_core::id::AccountId;
use everyday_core::mail::{Address, Draft, MailboxRole, OpKind, OpState, Origin};
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
    /// simulating a live `IDLE` connection that only ends when the caller's
    /// own `tokio::select!` drops it. Off by default: every sync-pass test
    /// wants `idle` to answer immediately, and only the task-level tests
    /// that actually exercise `IDLE`'s own `select!` (see
    /// `a_notify_wakes_the_idle_loop_promptly_rather_than_waiting_for_the_poll`)
    /// need the alternative.
    block_idle: bool,
}

impl FakeMailSession {
    fn new(server: Arc<Mutex<FakeServer>>) -> Self {
        Self { server, selected: None, block_idle: false }
    }

    fn blocking_idle(mut self) -> Self {
        self.block_idle = true;
        self
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

impl MailSession for FakeMailSession {
    async fn mailboxes(&mut self) -> SessionResult<Vec<RemoteMailbox>> {
        let server = self.server.lock().unwrap();
        Ok(server
            .mailboxes
            .iter()
            .map(|(name, mb)| RemoteMailbox {
                name: name.clone(),
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

    async fn idle(&mut self, _stop: tokio::sync::watch::Receiver<()>) -> SessionResult<IdleEvent> {
        if self.block_idle {
            // Never resolves on its own -- see `block_idle`'s own docs. The
            // caller's `tokio::select!` is what ends this, by dropping the
            // future, exactly as a real `IDLE` connection is torn down.
            std::future::pending::<()>().await;
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
    // true for a pack holding only this one, now-dead, message.
    let remap = env.packs.compact(&env.account_id.to_string()).unwrap();
    assert!(remap.is_empty(), "nothing live was left to remap");
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
