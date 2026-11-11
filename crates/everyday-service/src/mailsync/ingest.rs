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
//! [`resolve_header`] checks *that* first, for free. A miss then asks the
//! vault's keyed index ([`everyday_core::Vault::mail_message_by_header`],
//! one indexed lookup, schema v24), which outlives the task: before it, a
//! mailbox read again by a fresh task -- after an error, a restart, or a
//! pass that briefly saw it empty -- stored every message in it a second
//! time, and on Gmail that was each sent message, once from Sent and once
//! from All Mail. The expensive scan is kept for what the store method's own
//! docs describe: `force_db_rematch` is `true` only for the one
//! `sync_headers` call right after a mailbox's `UIDVALIDITY` has changed,
//! for a message stored before keys existed and not yet keyed.
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
//!
//! # A reply with no headers to follow
//!
//! Some replies name nothing: no `In-Reply-To`, no `References`, only "Re:"
//! and the subject. Sales tools and mail-merge services that send through a
//! person's mailbox do this, and so do some phone clients. Neither of the
//! two mechanisms above can place one -- Gmail starts a thread of its own for
//! it, and in Gmail's own window it shows apart too -- and somebody reading
//! the conversation sees the answer to their question somewhere else. So such
//! a message, and only such a message, joins the account's thread with the
//! same subject when somebody other than the account itself is in both, and
//! both are from the last [`SUBJECT_LINK_DAYS`] -- see [`subject_thread`].
//! `threading::Options::subject_fallback` stays off for every other message:
//! a subject is not an identifier, and these three conditions are what keep
//! "Re: Invoice" from two strangers apart.
//!
//! On Gmail the message keeps its own `X-GM-THRID`, and the next message in
//! that Gmail thread -- an answer to it, naming it in `References` -- would
//! hash to Gmail's separate thread again. [`ThreadIndex::resolve`] looks for
//! that: a message whose chain names one already moved out of its Gmail
//! thread follows it to where it went.

use std::collections::HashMap;

use everyday_core::Vault;
use everyday_core::id::{AccountId, MailMessageId, PackId, ThreadId};
use everyday_core::mail::{
    Address as MailAddress, CategorySource, GmailMeta, Message, MessageFlags,
};
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
        // Which existing row this `Message-ID` names, from whichever of the
        // two lookups the module docs describe answers first -- this run's
        // own cheap memory, or (only right after a `UIDVALIDITY` reset) the
        // vault's full scan. Either way this is *only* an id: the row
        // itself is always read fresh, next, straight from the vault, never
        // reused from whatever this run happened to cache earlier -- see
        // [`ThreadIndex::seen_by_message_id`]'s own docs for why a cached
        // copy would go stale exactly here.
        // Then the vault's key for it, which outlives this run: a mailbox read
        // again by a fresh task (after an error, a restart, or a mailbox that
        // briefly looked empty) found nothing in this run's memory and stored
        // every message in it a second time. Only the same message, though --
        // same date and size, as `Vault::key_existing_mail` asks: a client
        // that keeps one `Message-ID` across a draft's saves, or for the sent
        // message, must not have the later one folded into the first.
        let date = parsed.date.unwrap_or(header.internal_date);
        let existing = match threads.seen_by_message_id.get(id).copied() {
            Some(mid) => vault.mail_message(mid).ok(),
            None => {
                let keyed = vault
                    .mail_message_by_header(account_id, id)
                    .ok()
                    .flatten()
                    .and_then(|mid| vault.mail_message(mid).ok())
                    .filter(|m| m.date == date && m.size == u64::from(header.size));
                match keyed {
                    Some(found) => Some(found),
                    None if force_db_rematch => {
                        vault.message_by_message_id_header(account_id, id).ok().flatten()
                    }
                    None => None,
                }
            }
        };
        if let Some(mut existing) = existing {
            // Already stored, under some (mailbox, uid) -- possibly this
            // very one, after a `UIDVALIDITY` reset, or another mailbox
            // entirely (Gmail's Sent naming a `Message-ID` All Mail already
            // has). Its pack, snippet and thread -- whatever the *current*
            // row says, not what an earlier header this same run resolved
            // it to, which the bodies pass or a `Merge`'s `redirect` may
            // since have moved on from -- stay; only what a header can tell
            // us fresh is refreshed. The caller's `ingest` is what actually
            // adds the new `(mailbox, uid)` membership this call is for.
            existing.flags = flags;
            existing.labels = labels;
            existing.gmail = gmail;
            threads.remember(id, existing.thread_id);
            threads.seen_by_message_id.insert(id.clone(), existing.id);
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
    let date = parsed.date.unwrap_or(header.internal_date);
    let linked = if parsed.in_reply_to.is_none() && parsed.references.is_empty() {
        subject_thread(vault, account_id, &parsed, date, gmail_thrid, threads)
    } else {
        None
    };
    let thread_id = match linked {
        Some(thread_id) => threads.adopt(
            parsed.message_id.as_deref().unwrap_or(&local_key),
            gmail_thrid,
            thread_id,
        ),
        None => threads.resolve(
            vault,
            account_id,
            parsed.message_id.as_deref(),
            parsed.in_reply_to.as_deref(),
            &parsed.references,
            &local_key,
            gmail_thrid,
        ),
    };

    let message = Message {
        id: MailMessageId::new(),
        account_id,
        thread_id,
        message_id_header: parsed.message_id.clone().unwrap_or_else(|| local_key.clone()),
        date,
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
        // Not yet categorised -- `passes::categorize_new_message` fills
        // `category` in from the rules engine right after this returns, and
        // writes `Rules` there too; `Rules` is the correct placeholder even
        // for the moment in between, since a `None` category is already
        // rules-eligible for a future backfill regardless of what this field
        // says.
        category_source: CategorySource::Rules,
        pack: pending_pack_ref(account_id),
        gmail,
        // Filled in by `passes::process_body` once the bodies pass has the
        // raw bytes in hand to look for a `text/calendar` part at all --
        // the headers pass, here, never sees more than the envelope.
        invite: None,
    };
    threads.seen_by_message_id.insert(message.message_id_header.clone(), message.id);
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

/// How recent a reply with no headers must be, and how recently the thread
/// it names by subject must have had a message, for [`subject_thread`] to put
/// them together. A month: long enough for a slow answer, short enough that
/// next quarter's "Re: Invoice" is not this quarter's.
const SUBJECT_LINK_DAYS: i64 = 30;

/// How many of the account's newest threads [`subject_thread`] looks through.
const SUBJECT_LINK_LOOK: u32 = 200;

/// The thread a reply that names no other message belongs in, by subject --
/// see the module docs, "A reply with no headers to follow". `None` unless
/// the subject says it is a reply or a forward, the message is from the last
/// [`SUBJECT_LINK_DAYS`], and one of the account's threads from that long
/// before it has the same subject and somebody besides the account itself in
/// common with it; the newest such thread when there are several.
///
/// Not on Gmail when Gmail already put other messages in this one's thread:
/// Gmail had a reason, and those messages are where it belongs.
fn subject_thread(
    vault: &Vault,
    account_id: AccountId,
    parsed: &mime::ParsedMessage,
    date: jiff::Timestamp,
    gmail_thrid: Option<u64>,
    threads: &ThreadIndex,
) -> Option<ThreadId> {
    use everyday_mail::threading::{is_reply_or_forward, normalize_subject};

    let subject = parsed.subject.as_deref().unwrap_or_default();
    if !is_reply_or_forward(subject) {
        return None;
    }
    let window = SUBJECT_LINK_DAYS * 24 * 60 * 60;
    if jiff::Timestamp::now().as_second() - date.as_second() > window {
        return None;
    }
    if let Some(thrid) = gmail_thrid
        && (threads.by_gmail_thrid.contains_key(&thrid)
            || vault.thread(gmail_thread_id(account_id, thrid)).is_ok())
    {
        return None;
    }

    let account = vault.account(account_id).ok()?;
    let own = crate::mailai::own_addresses(&account);
    let people: std::collections::HashSet<String> = parsed
        .from
        .iter()
        .chain(&parsed.to)
        .chain(&parsed.cc)
        .filter_map(|a| a.email.as_deref())
        .map(str::to_lowercase)
        .filter(|email| !own.contains(email))
        .collect();
    if people.is_empty() {
        return None;
    }

    let mailboxes: Vec<_> = vault
        .mailboxes(account_id)
        .ok()?
        .into_iter()
        .filter(|m| {
            !matches!(
                m.role,
                everyday_core::mail::MailboxRole::Trash | everyday_core::mail::MailboxRole::Spam
            )
        })
        .map(|m| m.id)
        .collect();
    let page = vault
        .list_threads_across(
            &mailboxes,
            &everyday_core::store::mail::ThreadFilter::default(),
            None,
            SUBJECT_LINK_LOOK,
        )
        .ok()?;
    let wanted = normalize_subject(subject);
    page.threads
        .into_iter()
        .filter(|t| {
            normalize_subject(&t.subject) == wanted
                && date.as_second() - t.last_date.as_second() <= window
        })
        // Only the threads with the same subject are opened, to see everybody
        // who wrote and was written to in them -- `participants` names only
        // who wrote.
        .filter(|t| {
            vault.thread(t.id).is_ok_and(|(_, messages)| {
                messages.iter().any(|m| {
                    std::iter::once(&m.from)
                        .chain(&m.to)
                        .chain(&m.cc)
                        .any(|a| people.contains(&a.email.to_lowercase()))
                })
            })
        })
        .max_by_key(|t| t.last_date)
        .map(|t| t.id)
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

/// Where `thrid`'s messages went, when one this message names was moved out
/// of its Gmail thread by [`subject_thread`]: a message in the same Gmail
/// thread living somewhere other than that thread's own id. `None` -- Gmail's
/// own grouping -- every other time. Asks the vault's keyed index, one lookup
/// per id the chain names.
fn moved_gmail_thread(
    vault: &Vault,
    account_id: AccountId,
    thrid: u64,
    chain: &[String],
) -> Option<ThreadId> {
    let own = gmail_thread_id(account_id, thrid);
    let thrid = thrid.to_string();
    chain.iter().find_map(|named| {
        let id = vault.mail_message_by_header(account_id, named).ok().flatten()?;
        let message = vault.mail_message(id).ok()?;
        let same_gmail_thread =
            message.gmail.as_ref().and_then(|g| g.thread_id.as_deref()) == Some(thrid.as_str());
        (same_gmail_thread && message.thread_id != own).then_some(message.thread_id)
    })
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
    ///
    /// An id, not a [`Message`] -- earlier this held a whole cloned copy,
    /// reused as-is the next time the same `Message-ID` turned up, which
    /// went stale the moment anything else touched that row in between:
    /// the bodies pass filling in a pending pack and a real snippet, or a
    /// `Merge` in [`ThreadIndex::resolve`] retargeting its thread via
    /// [`ThreadIndex::redirect`] -- `redirect` only ever rewrites *this*
    /// map's cousins (`by_message_id`, `by_referenced_id`,
    /// `by_gmail_thrid`), never a cached `Message`, since a `Message` in
    /// hand has nowhere to route a rewrite *to*. [`resolve_header`] re-reads
    /// the row fresh from the vault by this id every time, so this only
    /// ever needs to answer "have I seen this id before, and what row does
    /// it name" -- exactly what threading needs, and nothing a message's
    /// own fields could go stale by holding.
    seen_by_message_id: HashMap<String, MailMessageId>,
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

    /// Place a message in `thread_id`, decided elsewhere -- by
    /// [`subject_thread`] -- and remember it as [`ThreadIndex::resolve`]
    /// would, its Gmail thread included, so the rest of this run follows it.
    pub fn adopt(&mut self, key: &str, gmail_thrid: Option<u64>, thread_id: ThreadId) -> ThreadId {
        self.by_message_id.insert(key.to_string(), thread_id);
        if let Some(thrid) = gmail_thrid {
            self.by_gmail_thrid.insert(thrid, thread_id);
        }
        thread_id
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
            match self.by_gmail_thrid.get(&thrid) {
                Some(&tid) => tid,
                None => {
                    let tid = moved_gmail_thread(vault, account_id, thrid, &chain)
                        .unwrap_or_else(|| gmail_thread_id(account_id, thrid));
                    self.by_gmail_thrid.insert(thrid, tid);
                    tid
                }
            }
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
