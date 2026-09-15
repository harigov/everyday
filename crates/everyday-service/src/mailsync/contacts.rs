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

use std::sync::Mutex;

use everyday_core::Vault;
use everyday_core::mail::{Address, ContactBook};
use frizbee::{Config, Matcher};

/// See the module docs.
pub struct ContactIndex(Mutex<State>);

struct State {
    book: ContactBook,
    /// Set by every `record_*` call, cleared by
    /// [`ContactIndex::persist_if_dirty`] -- what lets that function skip
    /// writing a row nothing has changed since the last time it did.
    dirty: bool,
}

impl ContactIndex {
    /// Read the sealed book once. `vault.mail_contacts()` failing (a fresh
    /// vault with no row yet, or a decrypt problem) starts this session
    /// with an empty index rather than refusing to open mail at all -- a
    /// contact index is derived and disposable, the same tolerance the
    /// search index and the pack store already have.
    pub fn load(vault: &Vault) -> Self {
        let book = vault.mail_contacts().unwrap_or_default();
        Self(Mutex::new(State { book, dirty: false }))
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
        }
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
