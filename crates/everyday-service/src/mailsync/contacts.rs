//! [`ContactIndex`]: the in-memory, incrementally-updated wrapper around
//! [`everyday_core::mail::ContactBook`] that answers `suggest_addresses`.
//!
//! # Why this exists rather than reading the sealed book straight off the
//! vault on every call
//!
//! The book itself is already small and cheap to decrypt -- the whole
//! reason it exists is to be, per the plan's own words, "a small sealed
//! contacts record" rather than a scan of every message. But
//! `suggest_addresses` is typed-while-composing, called on every keystroke
//! of a "To" field, and building a [`frizbee::Matcher`] plus a fresh
//! `Vec<String>` of every contact's display text on each of those calls is
//! wasted work a session-lived cache avoids. [`ContactIndex::load`] reads
//! the book once, when mail's storage opens (see `wiring::open`, the same
//! moment the pack store and search index open); every later
//! `record_sent_to`/`record_received_from` updates the in-memory copy
//! immediately and marks it dirty, and [`ContactIndex::persist_if_dirty`]
//! is what the sync engine and the outbox call to flush that back to the
//! sealed row -- see each call site's own docs for exactly when.
//!
//! # The backfill
//!
//! Counting only as messages arrive has a hole: a message is counted once,
//! the first time sync stores it, and the count lives in memory until a
//! save. A first sync that never got as far as saving -- the app quit, or
//! crashed, part way through a hundred-thousand-message mailbox -- left the
//! book empty, and none of those messages is ever new again to be counted
//! a second time. [`ContactIndex::backfill`] closes it: once per vault, it
//! recounts every message already stored and folds the result in.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use everyday_core::Vault;
use everyday_core::mail::{Address, ContactBook, Message};
use frizbee::{Config, Matcher};
use jiff::Timestamp;

/// How often a sync pass in progress writes what it has learned so far --
/// see [`ContactIndex::persist_if_due`].
const PERSIST_EVERY: Duration = Duration::from_secs(30);

/// See the module docs.
pub struct ContactIndex(Mutex<State>);

struct State {
    book: ContactBook,
    /// Set by every `record_*` call, cleared by
    /// [`ContactIndex::persist_if_dirty`] -- what lets that function skip
    /// writing a row nothing has changed since the last time it did.
    dirty: bool,
    /// When the book was last written, for [`ContactIndex::persist_if_due`].
    persisted_at: Option<Instant>,
}

impl ContactIndex {
    /// Read the sealed book once. `vault.mail_contacts()` failing (a fresh
    /// vault with no row yet, or a decrypt problem) starts this session
    /// with an empty index rather than refusing to open mail at all -- a
    /// contact index is derived and disposable, the same tolerance the
    /// search index and the pack store already have.
    pub fn load(vault: &Vault) -> Self {
        let book = vault.mail_contacts().unwrap_or_default();
        Self(Mutex::new(State { book, dirty: false, persisted_at: None }))
    }

    /// Note that the account itself sent a message naming `address` in
    /// `To` or `Cc`. Called from two places: the sync engine's header
    /// ingest, for a message landing in a `Sent` mailbox, and the outbox
    /// executor, right after a `Send` op succeeds -- see each call site.
    pub fn record_sent_to(&self, address: &str, name: &str) {
        let mut state = self.lock();
        state.book.record_sent_to(address, name);
        state.dirty = true;
    }

    /// Note that an incoming message's `From` was `address`.
    pub fn record_received_from(&self, address: &str, name: &str) {
        let mut state = self.lock();
        state.book.record_received_from(address, name);
        state.dirty = true;
    }

    /// Has this vault's owner ever sent a message naming `address`? What
    /// `crate::mailsync::passes::sync_headers` reads for
    /// `everyday_core::mail::categorize`'s "a real correspondent" signal, in
    /// memory rather than through a fresh decrypt of the sealed book on
    /// every message a first sync ingests.
    pub fn has_sent_to(&self, address: &str) -> bool {
        self.lock().book.has_sent_to(address)
    }

    /// Write the book back if anything has changed since the last time
    /// this was called. Called once per sync pass (`passes::sync_once`)
    /// and once per successful send, rather than on every single
    /// `record_*` call -- a first sync ingesting thousands of headers
    /// would otherwise cost thousands of sealed writes for a record that
    /// only ever needs to be as fresh as the next time somebody opens a
    /// compose window.
    pub fn persist_if_dirty(&self, vault: &Vault) {
        let mut state = self.lock();
        if state.dirty {
            let _ = vault.save_mail_contacts(&state.book);
            state.dirty = false;
            state.persisted_at = Some(Instant::now());
        }
    }

    /// [`ContactIndex::persist_if_dirty`], at most every [`PERSIST_EVERY`]
    /// -- what the header pass calls after every batch, so a first sync
    /// stopped half way keeps what it learned up to the last half minute
    /// rather than nothing at all, without sealing the whole book again for
    /// every batch of a fast one.
    pub fn persist_if_due(&self, vault: &Vault) {
        let due = {
            let state = self.lock();
            state.dirty && state.persisted_at.is_none_or(|at| at.elapsed() >= PERSIST_EVERY)
        };
        if due {
            self.persist_if_dirty(vault);
        }
    }

    /// Count every message already stored into the book, once per vault --
    /// see the module docs for the hole this closes. Returns whether it ran.
    ///
    /// The recount is built apart from the live book and folded in with
    /// [`ContactBook::merge_max`], so sync can keep recording new arrivals
    /// in the meantime: a message counted both ways is counted once, and
    /// one only the live book saw is kept.
    ///
    /// Whose message it was is read from the address, not the folder: one
    /// from any of the account's own addresses is mail the person sent, and
    /// names who they write to; anything else names who writes to them.
    /// That holds on Gmail, where nothing is filed in a folder called Sent,
    /// and for mail sent from another client. Drafts are skipped, as the
    /// header pass skips them, and spam never arrives here at all
    /// ([`Vault::mail_between`] leaves it out).
    pub fn backfill(&self, vault: &Vault) -> everyday_core::Result<bool> {
        if self.lock().book.backfilled {
            return Ok(false);
        }
        let mut recount = ContactBook::default();
        for account in vault.accounts()? {
            if !account.services.mail {
                continue;
            }
            let own: Vec<String> = std::iter::once(&account.address)
                .chain(account.identities.iter().map(|i| &i.address))
                .map(|a| a.trim().to_ascii_lowercase())
                .collect();
            // A year at a time, so a large vault is never decrypted into
            // memory all at once.
            let windows = year_windows(Timestamp::now());
            for pair in windows.windows(2) {
                for message in vault.mail_between(account.id, pair[0], pair[1])? {
                    count_message(&mut recount, &message, &own);
                }
            }
        }
        let mut state = self.lock();
        state.book.merge_max(recount);
        state.book.backfilled = true;
        state.dirty = true;
        drop(state);
        self.persist_if_dirty(vault);
        Ok(true)
    }

    /// Fuzzy-match `prefix` against every contact's name and address,
    /// newest-typo-tolerant first (see `frizbee`'s own docs), then ranked
    /// by how often the person has written to them, then by how often
    /// they have heard from them -- the plan's own ordering. An empty
    /// `prefix` skips the fuzzy match entirely and answers with the
    /// most-written-to contacts, which is what an empty "To" field
    /// focused for the first time should suggest.
    pub fn suggest(&self, prefix: &str, limit: usize) -> Vec<Address> {
        let state = self.lock();
        let prefix = prefix.trim();
        if prefix.is_empty() {
            let mut contacts = state.book.contacts.clone();
            contacts.sort_by(|a, b| {
                b.sent_to.cmp(&a.sent_to).then(b.received_from.cmp(&a.received_from))
            });
            return contacts.into_iter().take(limit).map(address_of).collect();
        }

        let haystacks: Vec<String> =
            state.book.contacts.iter().map(|c| format!("{} {}", c.name, c.address)).collect();
        let haystack_refs: Vec<&str> = haystacks.iter().map(String::as_str).collect();
        let mut matcher = Matcher::new(prefix, &Config::default());
        let mut matches = matcher.match_list(&haystack_refs);
        matches.sort_by(|a, b| {
            let ca = &state.book.contacts[a.index as usize];
            let cb = &state.book.contacts[b.index as usize];
            b.score
                .cmp(&a.score)
                .then_with(|| cb.sent_to.cmp(&ca.sent_to))
                .then_with(|| cb.received_from.cmp(&ca.received_from))
        });
        matches
            .into_iter()
            .take(limit)
            .map(|m| address_of(state.book.contacts[m.index as usize].clone()))
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// One message's contribution to a recount -- see [`ContactIndex::backfill`].
fn count_message(book: &mut ContactBook, message: &Message, own: &[String]) {
    let is_draft = message.flags.draft || message.labels.iter().any(|l| l == "\\Draft");
    if is_draft {
        return;
    }
    let is_own = |email: &str| own.iter().any(|o| o.eq_ignore_ascii_case(email.trim()));
    if is_own(&message.from.email) {
        for addr in message.to.iter().chain(message.cc.iter()) {
            if !is_own(&addr.email) {
                book.record_sent_to(&addr.email, &addr.name);
            }
        }
    } else {
        book.record_received_from(&message.from.email, &message.from.name);
    }
}

/// `[MIN, 1995)`, a year at a time up to the year after `now`, then the
/// rest of time -- every message lands in exactly one window, whatever its
/// `Date` header claimed.
fn year_windows(now: Timestamp) -> Vec<Timestamp> {
    let utc = jiff::tz::TimeZone::UTC;
    let last = now.to_zoned(utc.clone()).year() + 1;
    let mut edges = vec![Timestamp::MIN];
    for year in 1995..=last {
        if let Ok(at) = jiff::civil::date(year, 1, 1).to_zoned(utc.clone()) {
            edges.push(at.timestamp());
        }
    }
    edges.push(Timestamp::MAX);
    edges
}

fn address_of(c: everyday_core::mail::MailContact) -> Address {
    Address { name: c.name, email: c.address }
}

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::VaultConfig;

    fn env() -> (std::sync::Arc<Vault>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let vault = std::sync::Arc::new(
            everyday_vault::create(
                dir.path(),
                VaultConfig { password: None, ..Default::default() },
            )
            .unwrap(),
        );
        (vault, dir)
    }

    #[test]
    fn suggest_ranks_by_score_then_by_how_often_you_write_to_them() {
        let (vault, _dir) = env();
        let index = ContactIndex::load(&vault);
        index.record_sent_to("alice@example.com", "Alice Anderson");
        index.record_sent_to("alice@example.com", "Alice Anderson");
        index.record_sent_to("alan@example.com", "Alan Smith");
        index.record_received_from("alina@example.com", "Alina Petrova");

        let suggestions = index.suggest("al", 10);
        let emails: Vec<&str> = suggestions.iter().map(|a| a.email.as_str()).collect();
        assert!(emails.contains(&"alice@example.com"));
        assert!(emails.contains(&"alan@example.com"));
        assert!(emails.contains(&"alina@example.com"));
        // Alice, written to twice, must outrank Alan, written to once, once
        // the fuzzy scores are close enough to tie-break on frequency --
        // both start with "al" so their scores should be identical.
        let alice_pos = emails.iter().position(|e| *e == "alice@example.com").unwrap();
        let alan_pos = emails.iter().position(|e| *e == "alan@example.com").unwrap();
        assert!(alice_pos < alan_pos, "{emails:?}");
    }

    /// A message for the backfill to count: `from` to `to`, filed in
    /// `mailbox` at `uid`.
    fn stored(
        vault: &Vault,
        account: everyday_core::id::AccountId,
        mailbox: everyday_core::id::MailboxId,
        uid: u32,
        from: Address,
        to: Vec<Address>,
    ) {
        use everyday_core::mail::{CategorySource, MessageFlags};
        use everyday_core::store::mail::IngestMessage;
        let id = everyday_core::id::MailMessageId::new();
        let message = Message {
            id,
            account_id: account,
            thread_id: everyday_core::id::ThreadId::new(),
            message_id_header: format!("<{id}@contacts.example>"),
            date: Timestamp::now(),
            from,
            to,
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: "hi".into(),
            snippet: String::new(),
            flags: MessageFlags::default(),
            labels: Vec::new(),
            has_attachments: false,
            size: 0,
            category: None,
            category_source: CategorySource::Rules,
            pack: everyday_core::packstore::PackRef {
                account: account.to_string(),
                pack: everyday_core::id::PackId::new(),
                offset: 0,
                len: 0,
            },
            gmail: None,
            invite: None,
        };
        vault.ingest_mail(account, vec![IngestMessage { message, mailbox, uid }]).unwrap();
    }

    /// The regression this exists for: mail synced while the book was never
    /// saved -- a first sync stopped half way -- left nothing to suggest,
    /// and none of it would ever be counted again.
    #[test]
    fn backfill_counts_mail_already_stored_once() {
        use everyday_core::account::{Account, Provider};
        use everyday_core::mail::{Mailbox, MailboxRole};

        let (vault, _dir) = env();
        let account = Account::new(Provider::Custom, "me@example.com");
        vault.save_account(&account).unwrap();
        let inbox = Mailbox::new(account.id, "INBOX", MailboxRole::Inbox);
        vault.save_mailbox(&inbox).unwrap();
        let me = Address { name: "Me".into(), email: "ME@example.com".into() };
        let bob = Address { name: "Bob Stone".into(), email: "bob@example.com".into() };
        let carol = Address { name: "Carol Diaz".into(), email: "carol@example.com".into() };
        // From the account's own address, filed nowhere called Sent -- the
        // Gmail case -- still names someone written to.
        stored(&vault, account.id, inbox.id, 1, me.clone(), vec![bob.clone(), me.clone()]);
        stored(&vault, account.id, inbox.id, 2, carol.clone(), vec![me.clone()]);

        let index = ContactIndex::load(&vault);
        assert!(index.suggest("bo", 10).is_empty(), "nothing counted yet");
        assert!(index.backfill(&vault).unwrap(), "the first unlock runs it");

        let bobs = index.suggest("bob", 10);
        assert_eq!(bobs.first().map(|a| a.email.as_str()), Some("bob@example.com"));
        assert_eq!(bobs[0].name, "Bob Stone");
        assert!(
            index.has_sent_to("bob@example.com"),
            "mail from the account names who it wrote to"
        );
        assert!(!index.has_sent_to("carol@example.com"), "mail to it names who wrote");
        assert!(index.suggest("carol", 10).iter().any(|a| a.email == "carol@example.com"));
        assert!(
            index.suggest("me@", 10).iter().all(|a| a.email != "me@example.com"),
            "never the account's own address"
        );

        // Saved, and remembered: the next unlock loads it and does not
        // count everything a second time.
        let reloaded = ContactIndex::load(&vault);
        assert!(reloaded.suggest("bob", 10).iter().any(|a| a.email == "bob@example.com"));
        assert!(!reloaded.backfill(&vault).unwrap(), "once per vault");
    }

    #[test]
    fn year_windows_cover_all_of_time_in_order() {
        let edges = year_windows(Timestamp::now());
        assert_eq!(edges.first(), Some(&Timestamp::MIN));
        assert_eq!(edges.last(), Some(&Timestamp::MAX));
        assert!(edges.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn an_empty_prefix_answers_with_the_most_written_to_contacts() {
        let (vault, _dir) = env();
        let index = ContactIndex::load(&vault);
        index.record_received_from("quiet@example.com", "Quiet");
        index.record_sent_to("frequent@example.com", "Frequent");
        index.record_sent_to("frequent@example.com", "Frequent");

        let suggestions = index.suggest("", 10);
        assert_eq!(suggestions[0].email, "frequent@example.com");
    }

    #[test]
    fn persist_if_dirty_only_writes_when_something_changed() {
        let (vault, _dir) = env();
        let index = ContactIndex::load(&vault);
        index.persist_if_dirty(&vault); // nothing recorded yet: must not error
        assert!(vault.mail_contacts().unwrap().contacts.is_empty());

        index.record_sent_to("bob@example.com", "Bob");
        index.persist_if_dirty(&vault);
        let saved = vault.mail_contacts().unwrap();
        assert_eq!(saved.contacts.len(), 1);

        // Loading fresh from the vault sees the persisted contact.
        let reloaded = ContactIndex::load(&vault);
        let suggestions = reloaded.suggest("bob", 10);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].email, "bob@example.com");
    }
}
