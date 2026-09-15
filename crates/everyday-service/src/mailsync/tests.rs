//! Unit tests for the sync engine, against a fake [`MailSession`] and a
//! real, throwaway vault -- SQLite, the real pack store, the real search
//! index, everything but the socket.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use everyday_core::account::{Account, AccountStatus, AuthMethod, Provider};
use everyday_core::crypto::NullCipher;
use everyday_core::id::AccountId;
use everyday_core::mail::MailboxRole;
use everyday_core::packstore::{FilePackStore, PackStore};
use everyday_core::store::mail::ThreadFilter;
use everyday_core::{MailQuery, MailSearch, Vault, VaultConfig};
use everyday_mail::session::{
    Capabilities, Changes, Credential, GmailMeta, IdleEvent, MailError, MailSession, MailboxState,
    RawStream, RemoteHeader, RemoteMailbox, Result as SessionResult, Role, SyncCursor, Uid, UidSet,
};
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
}

impl FakeMailSession {
    fn new(server: Arc<Mutex<FakeServer>>) -> Self {
        Self { server, selected: None }
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

    async fn move_to(&mut self, _uids: &UidSet, _mailbox: &str) -> SessionResult<()> {
        Err(MailError::Unsupported("MOVE"))
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
