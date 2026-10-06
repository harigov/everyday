//! The small, sealed record `suggest_addresses` is built from.
//!
//! `docs/plans/mail.md`'s phase 4 asks for address autocomplete "ranked by
//! how often you write to them" without ever decrypting every message on
//! unlock to find out. [`ContactBook`] is the answer: one sealed row,
//! updated incrementally as mail is ingested and sent (see
//! `everyday-service::mailsync::contacts`, which is the only thing that
//! ever mutates one), rather than a full scan of `bodies` and
//! `mail_messages` every time somebody opens the "To" field. It is a
//! derived index, on the same terms the search index and the pack store's
//! own aggregates are: losing it costs a cold contact list until the next
//! few messages rebuild it, never data nothing else remembers.

use serde::{Deserialize, Serialize};

/// One address this vault's owner has written to or received from, and how
/// often each direction happened.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailContact {
    /// Lower-cased, for the same reason [`crate::mail::RemoteImageSettings`]
    /// compares senders case-insensitively -- a header's casing is not
    /// something to rely on -- while `name`, the display form, is not.
    pub address: String,
    #[serde(default)]
    pub name: String,
    /// Times a message the account itself sent named this address in `To`
    /// or `Cc`. What "how often you write to them" ranks by first.
    #[serde(default)]
    pub sent_to: u32,
    /// Times an incoming message's `From` was this address. The
    /// second-place ranking, per the plan: someone who writes to you a lot
    /// but you rarely reply to is still worth suggesting, just after
    /// everyone you actually write to.
    #[serde(default)]
    pub received_from: u32,
}

/// The whole contact index, as it is sealed: one row, vault-wide rather
/// than per-account, because a person addressing a message does not care
/// which of their accounts they have exchanged mail with someone on
/// before.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactBook {
    #[serde(default)]
    pub contacts: Vec<MailContact>,
    /// Set once every message already stored has been counted in, by
    /// `everyday-service::mailsync::contacts`' backfill. Until then the book
    /// only knows what arrived while a sync pass happened to persist it --
    /// and a first sync interrupted before its pass finished (a crash, a
    /// quit, a laptop lid) used to leave it empty for good, since the
    /// messages it missed are never "new" again.
    #[serde(default)]
    pub backfilled: bool,
}

impl ContactBook {
    /// Note that the account itself sent a message naming `address` (in
    /// `To` or `Cc`). A blank address is ignored -- header parsing hands
    /// one back for a group syntax or a malformed line often enough that
    /// this is not worth failing over, only skipping.
    pub fn record_sent_to(&mut self, address: &str, name: &str) {
        self.bump(address, name, true);
    }

    /// Note that an incoming message's `From` was `address`.
    pub fn record_received_from(&mut self, address: &str, name: &str) {
        self.bump(address, name, false);
    }

    /// Has this vault's owner ever sent a message naming `address` in `To`
    /// or `Cc`? What `crate::mail::categorize` reads as its "a real
    /// correspondent" signal — received-from alone does not count, on the
    /// same reasoning [`MailContact::sent_to`]'s own docs give: someone who
    /// writes to you a lot but you never reply to is not yet a person this
    /// vault's owner has decided matters.
    pub fn has_sent_to(&self, address: &str) -> bool {
        let key = address.trim().to_ascii_lowercase();
        self.contacts.iter().any(|c| c.address == key && c.sent_to > 0)
    }

    /// Fold `other` in, keeping the larger count on each side of every
    /// contact the two share rather than adding them: `other` is a recount
    /// of messages this book may already have counted as they arrived, and
    /// summing the two would count those twice. A name this book lacks is
    /// taken from `other`.
    pub fn merge_max(&mut self, other: ContactBook) {
        for theirs in other.contacts {
            match self.contacts.iter_mut().find(|c| c.address == theirs.address) {
                Some(ours) => {
                    ours.sent_to = ours.sent_to.max(theirs.sent_to);
                    ours.received_from = ours.received_from.max(theirs.received_from);
                    if ours.name.is_empty() {
                        ours.name = theirs.name;
                    }
                }
                None => self.contacts.push(theirs),
            }
        }
    }

    fn bump(&mut self, address: &str, name: &str, sent: bool) {
        let address = address.trim();
        if address.is_empty() {
            return;
        }
        let key = address.to_ascii_lowercase();
        let contact = match self.contacts.iter_mut().find(|c| c.address == key) {
            Some(c) => c,
            None => {
                self.contacts.push(MailContact { address: key, ..Default::default() });
                self.contacts.last_mut().expect("just pushed")
            }
        };
        if contact.name.is_empty() && !name.trim().is_empty() {
            contact.name = name.trim().to_string();
        }
        if sent {
            contact.sent_to += 1;
        } else {
            contact.received_from += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bumping_the_same_address_twice_is_one_contact_with_both_counts() {
        let mut book = ContactBook::default();
        book.record_sent_to("Alice@Example.com", "Alice");
        book.record_received_from("alice@example.com", "");
        assert_eq!(book.contacts.len(), 1, "case must not split one address into two contacts");
        assert_eq!(book.contacts[0].sent_to, 1);
        assert_eq!(book.contacts[0].received_from, 1);
        assert_eq!(book.contacts[0].name, "Alice", "the first name seen is kept");
    }

    #[test]
    fn merge_max_keeps_the_larger_count_and_adds_new_contacts() {
        let mut live = ContactBook::default();
        live.record_sent_to("alice@example.com", "");
        live.record_received_from("alice@example.com", "");
        live.record_received_from("alice@example.com", "");
        let mut recount = ContactBook::default();
        recount.record_sent_to("alice@example.com", "Alice");
        recount.record_sent_to("alice@example.com", "Alice");
        recount.record_received_from("alice@example.com", "Alice");
        recount.record_received_from("bob@example.com", "Bob");

        live.merge_max(recount);
        assert_eq!(live.contacts.len(), 2);
        let alice = live.contacts.iter().find(|c| c.address == "alice@example.com").unwrap();
        assert_eq!((alice.sent_to, alice.received_from), (2, 2), "the larger count, not the sum");
        assert_eq!(alice.name, "Alice", "a missing name is filled in");
        assert!(live.contacts.iter().any(|c| c.address == "bob@example.com"));
    }

    #[test]
    fn a_blank_address_is_ignored() {
        let mut book = ContactBook::default();
        book.record_sent_to("  ", "Nobody");
        assert!(book.contacts.is_empty());
    }
}
