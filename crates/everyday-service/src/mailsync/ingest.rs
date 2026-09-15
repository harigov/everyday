//! Turning one fetched header into a [`Message`] row: which existing
//! message it is (if the account has already seen it under another mailbox
//! this same run, or under a UID the server has since reassigned), and
//! which thread it belongs to.
//!
//! # Rematching by `Message-ID`, and why it is not one call
//!
//! [`everyday_core::Vault::message_by_message_id_header`]'s own docs accept
//! being a full per-account table scan because it expects to be called
//! rarely -- "a `UIDVALIDITY` reset is rare, and this scan is bounded to one
//! account's messages, never the whole vault." Calling it for *every*
//! header, to also catch Gmail's Sent and Drafts folders naming a
//! `Message-ID` All Mail already stored, would turn a first sync into an
//! `O(n^2)` scan-of-a-scan -- measured, on a synthetic two-thousand-message
//! account, as the difference between finishing in low tens of seconds and
//! finishing in a small multiple of that.
//!
//! So [`ThreadIndex`] also carries a cheap, in-memory map of every message
//! this sync attempt has already resolved, keyed by `Message-ID`, and
//! [`resolve_header`] checks *that* first, for free, before ever asking the
//! vault -- which is what makes the Sent/All-Mail overlap cost nothing
//! extra when (the ordinary case) both are synced in the same run. The
//! expensive vault scan is reserved for exactly the case the store method's
//! own docs describe: `force_db_rematch` is `true` only for the one
//! `sync_headers` call right after a mailbox's `UIDVALIDITY` has changed,
//! where every uid in it needs rematching and there is no in-memory answer
//! to fall back on for a message this run has not touched yet.
//!
//! # Threading
//!
//! Two independent mechanisms, matching `everyday-mail::threading`'s own
//! two-check design but adapted to storage this crate already has, rather
//! than re-threading a whole mailbox on every message:
//!
//! - **Gmail's own `X-GM-THRID` wins outright.** It is deterministically
//!   turned into a [`ThreadId`] by hashing the account and the thread id
//!   together (see [`gmail_thread_id`]) -- not looked up -- because Gmail's
//!   grouping is not always a header chain this account has any of, and a
//!   hash is the one mechanism that agrees with itself across a restart
//!   without a database query to ask.
//! - **Everywhere else, JWZ**, done with two lookups rather than the whole
//!   mailbox [`everyday_mail::threading::place`] takes: forward, by asking
//!   the vault (and this run's own memory, for a message still mid-batch)
//!   whether any id in this message's `References`/`In-Reply-To` chain is
//!   already stored; backward, by remembering -- in [`ThreadIndex`], for the
//!   life of one sync attempt -- which ids each processed message's chain
//!   named, so that a parent arriving after its child (the common case,
//!   since sync fetches newest UIDs first) still finds it. A message naming
//!   more than one existing thread is a real `everyday-mail::threading`
//!   `Merge`; this module keeps the lowest-id candidate and migrates every
//!   other candidate's messages into it, through
//!   [`everyday_core::Vault::merge_mail_threads`], and redirects this run's
//!   own memory of the merged-away ids -- see [`ThreadIndex::resolve`] and
//!   [`ThreadIndex::redirect`].

use std::collections::HashMap;

use everyday_core::Vault;
use everyday_core::id::{AccountId, MailMessageId, PackId, ThreadId};
use everyday_core::mail::{Address as MailAddress, GmailMeta, Message, MessageFlags};
use everyday_core::packstore::PackRef;
use everyday_mail::mime::{self, Address as MimeAddress};
use everyday_mail::session::{Flags, RemoteHeader};

/// Resolved from one [`RemoteHeader`]: the row to write, and whether it is a
/// message this account has never stored before -- which is what tells the
/// bodies pass whether it needs fetching at all.
pub struct HeaderIngest {
    pub message: Message,
    /// `false` for a message this account already had -- under another
    /// mailbox, or under a uid a `UIDVALIDITY` change retired -- whose raw
    /// bytes are already in the pack store and whose body has already been
    /// indexed.
    pub is_new: bool,
    /// The four raw signals `crate::mailsync::passes::sync_headers` hands to
    /// `everyday_core::mail::categorize::categorize` right after this
    /// returns -- read once, here, from the same [`mime::parse`] this
    /// function already ran, rather than asking that pass to parse the
    /// header bytes a second time. `None` for exactly the headers a message
    /// did not carry, which `categorize` already treats as "no opinion" on
    /// that signal.
    pub list_id: Option<String>,
    pub list_unsubscribe: Option<String>,
    pub precedence: Option<String>,
    pub auto_submitted: Option<String>,
}

/// A [`PackRef`] that names nothing yet: what a freshly ingested message's
/// row carries between the headers pass, which does not have raw bytes, and
/// the bodies pass, which re-ingests the same message id with the real one.
/// [`is_pending`] is how a caller tells the two apart -- a real pack's sealed
/// length is never zero, even for an empty message, because the AEAD
/// overhead alone is non-zero. See [`crate::packstore`] -- I mean
/// [`everyday_core::packstore`] -- for the framing this trades on.
pub fn pending_pack_ref(account_id: AccountId) -> PackRef {
    PackRef { account: account_id.to_string(), pack: PackId::new(), offset: 0, len: 0 }
}

pub fn is_pending(pack: &PackRef) -> bool {
    pack.len == 0
}

/// Resolve one header into a [`HeaderIngest`], consulting and updating
/// `threads` for the message's place in its conversation and for its own
/// identity -- see the module docs for `force_db_rematch`, which must be
/// `true` only right after the mailbox this header came from had its
/// `UIDVALIDITY` reset.
pub fn resolve_header(
    vault: &Vault,
    account_id: AccountId,
    header: &RemoteHeader,
    threads: &mut ThreadIndex,
    force_db_rematch: bool,
) -> mime::Result<HeaderIngest> {
    let parsed = mime::parse(&header.header)?;
    let flags = mail_flags(header.flags);
    let labels = header.gmail.as_ref().map(|g| g.labels.clone()).unwrap_or_default();
    let gmail = header.gmail.as_ref().map(|g| GmailMeta {
        thread_id: Some(g.thrid.to_string()),
        message_id: Some(g.msgid.to_string()),
    });

    if let Some(id) = &parsed.message_id {
        let already_seen = threads.seen_by_message_id.get(id).cloned().or_else(|| {
            if force_db_rematch {
                vault.message_by_message_id_header(account_id, id).ok().flatten()
            } else {
                None
            }
        });
        if let Some(mut existing) = already_seen {
            // Already stored, under some (mailbox, uid) -- possibly this
            // very one, after a `UIDVALIDITY` reset, or another mailbox
            // entirely (Gmail's Sent naming a `Message-ID` All Mail already
            // has). Its pack and thread stay; only what a header can tell
            // us fresh is refreshed.
            existing.flags = flags;
            existing.labels = labels;
            existing.gmail = gmail;
            threads.remember(id, existing.thread_id);
            threads.seen_by_message_id.insert(id.clone(), existing.clone());
            return Ok(HeaderIngest {
                message: existing,
                is_new: false,
                list_id: parsed.list_id,
                list_unsubscribe: parsed.list_unsubscribe,
                precedence: parsed.precedence,
                auto_submitted: parsed.auto_submitted,
            });
        }
    }

    let local_key = format!("uid:{}", header.uid);
    let gmail_thrid = header.gmail.as_ref().map(|g| g.thrid);
    let thread_id = threads.resolve(
        vault,
        account_id,
        parsed.message_id.as_deref(),
        parsed.in_reply_to.as_deref(),
        &parsed.references,
        &local_key,
        gmail_thrid,
    );

    let message = Message {
        id: MailMessageId::new(),
        account_id,
        thread_id,
        message_id_header: parsed.message_id.clone().unwrap_or_else(|| local_key.clone()),
        date: parsed.date.unwrap_or(header.internal_date),
        from: parsed.from.first().map(to_address).unwrap_or_else(|| MailAddress::bare("")),
        to: parsed.to.iter().map(to_address).collect(),
        cc: parsed.cc.iter().map(to_address).collect(),
        bcc: parsed.bcc.iter().map(to_address).collect(),
        reply_to: parsed.reply_to.iter().map(to_address).collect(),
        subject: parsed.subject.clone().unwrap_or_default(),
        snippet: String::new(),
        flags,
        labels,
        has_attachments: false,
        size: u64::from(header.size),
        category: None,
        pack: pending_pack_ref(account_id),
        gmail,
    };
    threads.seen_by_message_id.insert(message.message_id_header.clone(), message.clone());
    Ok(HeaderIngest {
        message,
        is_new: true,
        list_id: parsed.list_id,
        list_unsubscribe: parsed.list_unsubscribe,
        precedence: parsed.precedence,
        auto_submitted: parsed.auto_submitted,
    })
}

fn to_address(a: &MimeAddress) -> MailAddress {
    MailAddress {
        name: a.name.clone().unwrap_or_default(),
        email: a.email.clone().unwrap_or_default(),
    }
}

pub(crate) fn mail_flags(f: Flags) -> MessageFlags {
    MessageFlags {
        seen: f.contains(Flags::SEEN),
        answered: f.contains(Flags::ANSWERED),
        flagged: f.contains(Flags::FLAGGED),
        draft: f.contains(Flags::DRAFT),
        deleted: f.contains(Flags::DELETED),
    }
}

/// A deterministic [`ThreadId`] for Gmail's own `X-GM-THRID`, stable across
/// a restart without a lookup -- see the module docs.
fn gmail_thread_id(account_id: AccountId, thrid: u64) -> ThreadId {
    let label = format!("everyday.mail.gmailthread.v1:{account_id}:{thrid}");
    let hash = blake3::hash(label.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    ThreadId::from(uuid::Uuid::from_bytes(bytes))
}

/// `References`, if present and non-empty, else `In-Reply-To` alone -- RFC
/// 5256's own fallback, and the same one `everyday-mail::threading` applies.
fn reference_chain(in_reply_to: Option<&str>, references: &[String]) -> Vec<String> {
    if !references.is_empty() {
        references.to_vec()
    } else {
        in_reply_to.map(|s| vec![s.to_string()]).unwrap_or_default()
    }
}

/// Where a message's thread has been seen so far in this sync attempt, plus
/// the two lookups that place a new one. See the module docs.
pub struct ThreadIndex {
    /// A normalised `Message-ID` (or this run's synthetic key, for a message
    /// with none) to the thread it was placed in.
    by_message_id: HashMap<String, ThreadId>,
    /// A referenced id to every thread, this run, that some processed
    /// message's chain named it from -- the backward check.
    by_referenced_id: HashMap<String, Vec<ThreadId>>,
    /// Memoised [`gmail_thread_id`] results, so a busy Gmail thread does not
    /// recompute the same hash for every message in it.
    by_gmail_thrid: HashMap<u64, ThreadId>,
    /// Every message [`resolve_header`] has resolved this sync attempt,
    /// keyed by `Message-ID` -- the cheap, in-memory half of rematching; see
    /// the module docs for why this exists and when the vault's own,
    /// expensive scan is still asked instead.
    seen_by_message_id: HashMap<String, Message>,
}

impl ThreadIndex {
    pub fn new() -> Self {
        Self {
            by_message_id: HashMap::new(),
            by_referenced_id: HashMap::new(),
            by_gmail_thrid: HashMap::new(),
            seen_by_message_id: HashMap::new(),
        }
    }

    /// Note that `message_id` is already known to belong to `thread_id` --
    /// called for a message [`resolve_header`] found already stored, so a
    /// later message in this run that references it finds it without a
    /// second trip to the vault.
    pub fn remember(&mut self, message_id: &str, thread_id: ThreadId) {
        self.by_message_id.insert(message_id.to_string(), thread_id);
    }

    /// Place a message with no stored row yet. See the module docs for the
    /// two mechanisms this chooses between.
    ///
    /// Eight parameters rather than a struct: every caller is
    /// [`resolve_header`], right beside this in the same file, already
    /// holding each of these as a separate local -- wrapping them up only to
    /// unwrap them again a line later would be ceremony with no reader it
    /// serves.
    #[allow(clippy::too_many_arguments)]
    pub fn resolve(
        &mut self,
        vault: &Vault,
        account_id: AccountId,
        own_message_id: Option<&str>,
        in_reply_to: Option<&str>,
        references: &[String],
        local_key: &str,
        gmail_thrid: Option<u64>,
    ) -> ThreadId {
        let chain = reference_chain(in_reply_to, references);
        let key = own_message_id.unwrap_or(local_key).to_string();

        let thread_id = if let Some(thrid) = gmail_thrid {
            *self.by_gmail_thrid.entry(thrid).or_insert_with(|| gmail_thread_id(account_id, thrid))
        } else {
            let mut candidates: Vec<ThreadId> = Vec::new();
            for r in &chain {
                if let Some(&tid) = self.by_message_id.get(r) {
                    candidates.push(tid);
                } else if let Ok(Some(existing)) = vault.message_by_message_id_header(account_id, r)
                {
                    candidates.push(existing.thread_id);
                }
            }
            if let Some(tids) = self.by_referenced_id.get(&key) {
                candidates.extend(tids.iter().copied());
            }
            candidates.sort_by_key(|t| t.0);
            candidates.dedup();
            let kept = candidates.first().copied().unwrap_or_else(ThreadId::new);
            // A real `Merge`: this message's own chain names more than one
            // thread this account already has. The lowest id is kept
            // (arbitrary but stable, so two runs merge the same way) and
            // every other candidate's messages move into it -- see
            // `Vault::merge_mail_threads`. Every id this run has already
            // resolved *to* one of the merged-away threads is repointed at
            // `kept` too, so a later message in this same sync that
            // references one of them still lands in the thread its
            // messages actually moved to.
            if candidates.len() > 1 {
                let others: Vec<ThreadId> = candidates.into_iter().filter(|&t| t != kept).collect();
                let _ = vault.merge_mail_threads(kept, &others);
                self.redirect(&others, kept);
            }
            kept
        };

        self.by_message_id.insert(key, thread_id);
        for r in &chain {
            self.by_referenced_id.entry(r.clone()).or_default().push(thread_id);
        }
        thread_id
    }

    /// Rewrite every id in `from` this run has already recorded to `to` --
    /// what [`ThreadIndex::resolve`] calls right after a real `Merge`, so
    /// this run's own memory agrees with what the vault now says. `from`
    /// is short (candidates a single message's chain named), so a linear
    /// scan of the maps costs nothing a merge -- already a rare, multi-row
    /// vault write -- would notice.
    fn redirect(&mut self, from: &[ThreadId], to: ThreadId) {
        for tid in self.by_message_id.values_mut() {
            if from.contains(tid) {
                *tid = to;
            }
        }
        for tids in self.by_referenced_id.values_mut() {
            for tid in tids.iter_mut() {
                if from.contains(tid) {
                    *tid = to;
                }
            }
        }
        for tid in self.by_gmail_thrid.values_mut() {
            if from.contains(tid) {
                *tid = to;
            }
        }
    }
}

impl Default for ThreadIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmail_thread_ids_are_stable_and_distinct() {
        let account = AccountId::new();
        let other_account = AccountId::new();

        assert_eq!(
            gmail_thread_id(account, 111),
            gmail_thread_id(account, 111),
            "the same account and thread id must hash the same way every time"
        );
        assert_ne!(
            gmail_thread_id(account, 111),
            gmail_thread_id(account, 222),
            "different Gmail threads must not collide"
        );
        assert_ne!(
            gmail_thread_id(account, 111),
            gmail_thread_id(other_account, 111),
            "two accounts must not share a thread id even for the same Gmail thread number"
        );
    }
}
