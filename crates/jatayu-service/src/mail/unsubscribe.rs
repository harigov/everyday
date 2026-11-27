//! Unsubscribing: finding which message of a sender's says how to leave
//! their list, and leaving it.
//!
//! What a header offers, and which way out comes first, is
//! [`jatayu_core::mail::unsubscribe`]'s business. This module does the parts
//! that need a vault, a socket or the outbox:
//!
//! - **Finding the header.** New mail keeps it on the message record
//!   ([`Message::unsubscribe`]); mail synced before that has it read back
//!   from the message's own bytes in the pack store ([`header_of`]) -- only
//!   the header block, and at most [`MAX_READS`] messages for one sender
//!   and [`MAX_READS_PER_CALL`] for one request, so a bulk unsubscribe over
//!   years of old mail is a bounded amount of decrypting.
//! - **One click**: an RFC 8058 POST through [`crate::http::unsubscribe_client`],
//!   which stays on the open internet and follows no redirect
//!   ([`post_one_click`]).
//! - **An email**: a draft from the address the list writes to, queued
//!   through the outbox like any other send, with no undo delay -- nobody
//!   wants forty "Undo" cards for one press.
//! - **A page**: handed back to the interface to open in the browser -- this
//!   module never fetches it.
//!
//! When one click is refused the email is tried, then the page, in the order
//! [`ListUnsubscribe::methods`] gives them.
//!
//! A sender asked is remembered on the message used
//! ([`jatayu_core::mail::ListUnsubscribe::asked_at`]), so Mail can say "you
//! asked on 26 Nov" rather than offer it again as new. Stamping it also keeps
//! the header of a message synced before headers were kept, so it is not read
//! from the message's bytes again.
//!
//! Who counts as a sender: never the person themselves (any of the account's
//! addresses, as the provider delivers them -- see
//! [`crate::mail::address::is_own`]) and never a draft. A list that hands a
//! person's own reply back with its header on is still not them to leave.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet, btree_map, hash_map};
use std::sync::{Arc, LazyLock, Mutex};

use futures::StreamExt;
use jatayu_core::Vault;
use jatayu_core::account::Account;
use jatayu_core::id::{AccountId, MailMessageId, ThreadId};
use jatayu_core::mail::unsubscribe::{Mailto, Method};
use jatayu_core::mail::{Address, Draft, ListUnsubscribe, Message, Origin, UnsubscribeVia};
use jatayu_core::markup::escape_html;
use jatayu_core::packstore::PackStore;
use jiff::Timestamp;
use serde::Serialize;

use crate::error::CommandResult;
use crate::mail::address::{is_own, own_mailboxes};
use crate::mail::sync::ingest::is_pending;
use crate::service::{Service, blocking};

/// What unsubscribing from one sender would do -- what the interface says
/// before asking: who, which way, and where to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsubscribeOffer {
    /// The message whose header would be used.
    pub message: MailMessageId,
    pub account: AccountId,
    /// The sender, lower-cased.
    pub email: String,
    /// Their display name, or the address when they gave none.
    pub name: String,
    pub via: UnsubscribeVia,
    /// Where it goes: the host a POST or a page is on, or the address an
    /// email goes to.
    pub to: String,
    /// When this person last asked them, if they have.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asked_at: Option<Timestamp>,
}

/// How unsubscribing from one sender went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsubscribeOutcome {
    pub account: AccountId,
    /// The sender, lower-cased.
    pub email: String,
    /// Their display name, or the address when they gave none.
    pub name: String,
    /// The way out that was taken, or offered: `web` when a page is left to
    /// open, `None` when the sender's mail named no way out at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<UnsubscribeVia>,
    /// Whether the sender has been asked: a POST they accepted, or an email
    /// queued to them. Never true for a page.
    pub done: bool,
    /// A page for the person to finish on, in the browser: the sender's only
    /// way out, or the fallback when a one-click POST was refused and no
    /// email address was offered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Why it did not work, in words, with no link in it (unsubscribe links
    /// carry a token per subscriber).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One sender to unsubscribe from, and the message whose header says how.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub account: AccountId,
    /// The sender, lower-cased.
    pub email: String,
    /// Their display name, or the address when they gave none.
    pub name: String,
    pub message: Message,
    pub header: ListUnsubscribe,
}

/// How many messages' own bytes one lookup reads at most, looking for a
/// header their records did not keep.
pub(crate) const MAX_READS: usize = 5;

/// How many messages' own bytes one request reads at most, across every
/// sender in it -- [`MAX_READS`] each would otherwise let a bulk unsubscribe
/// over a thousand senders decrypt five thousand messages. A sender it leaves
/// unread is reported as naming no way out, as one whose mail kept no header
/// is; pressing again for them alone reads theirs.
const MAX_READS_PER_CALL: usize = 500;

/// How many senders are asked at once. A one-click POST waits on somebody
/// else's server for up to the client's thirty seconds; one at a time, a
/// single slow sender would hold up everybody behind them, and many at once
/// would look like a flood to whoever runs two of the lists.
const AT_ONCE: usize = 4;

/// The whole body of an RFC 8058 POST, byte for byte.
const ONE_CLICK_BODY: &str = "List-Unsubscribe=One-Click";

/// The type RFC 8058 has that body sent as.
const ONE_CLICK_CONTENT_TYPE: &str = "application/x-www-form-urlencoded";

/// What a one-click POST says it is: the same generic agent a message's
/// remote images are fetched under (`mail::view`'s `REMOTE_IMAGE_USER_AGENT`),
/// for the same reason -- the address is a stranger's, and which application
/// and which build just pressed their button is none of their business. Not
/// blank, since some servers refuse a request with no agent at all.
const ONE_CLICK_USER_AGENT: &str = "Mozilla/5.0";

/// The largest message whose own bytes are read for a header its record
/// did not keep. A pack frame is decrypted whole, attachments and all, and a
/// list's mail is small: a message this size is somebody's file, not a
/// newsletter, and reading it on every look would be paying for the file.
const MAX_READ_BYTES: u64 = 1024 * 1024;

/// How many messages' read-back headers [`READ_BACK`] holds before it starts
/// again from empty.
const READ_BACK_MOST: usize = 50_000;

/// What the bytes of messages synced before headers were kept said, for the
/// life of this process: a header never changes, and without this opening
/// the same conversation, or Quick Cleanup, again would decrypt the same
/// messages again. Keyed by message id, which no two messages share, so one
/// map serves every vault this process opens.
static READ_BACK: LazyLock<Mutex<HashMap<MailMessageId, Option<ListUnsubscribe>>>> =
    LazyLock::new(Mutex::default);

/// The header `message` offers: what its record kept, or -- for mail synced
/// before headers were kept -- what its own bytes say, read from `packs`
/// (once a process; see [`READ_BACK`]). `None` when neither names a usable
/// way out, when the record says it was looked at and offered none, or when
/// the bytes are not worth reading or not there: too large, the body not
/// fetched yet, or `packs` is `None`.
pub(crate) fn header_of(
    packs: Option<&dyn PackStore>,
    message: &Message,
) -> Option<ListUnsubscribe> {
    if let Some(header) = &message.unsubscribe {
        return (!header.links.is_empty()).then(|| header.clone());
    }
    if let Some(known) = READ_BACK.lock().unwrap_or_else(|e| e.into_inner()).get(&message.id) {
        return known.clone();
    }
    if !reads_bytes(packs, message) {
        return None;
    }
    // Any failure here is "no header", and is not remembered: a compaction
    // may move the frame between reading the record and reading the pack,
    // and the next look finds it where it went. Nothing about a header is
    // worth an error.
    let raw = packs?.read(&message.pack).ok()?;
    let header = header_in(&raw);
    let mut known = READ_BACK.lock().unwrap_or_else(|e| e.into_inner());
    if known.len() >= READ_BACK_MOST {
        known.clear();
    }
    known.insert(message.id, header.clone());
    header
}

/// Whether [`header_of`] would read `message`'s bytes to answer -- what a
/// read budget counts. A kept header (or a kept "none") costs nothing, and
/// neither does one already read this run, a message too large to be a
/// list's, or one with no bytes to read.
pub(crate) fn reads_bytes(packs: Option<&dyn PackStore>, message: &Message) -> bool {
    message.unsubscribe.is_none()
        && packs.is_some()
        && !is_pending(&message.pack)
        && message.size <= MAX_READ_BYTES
        && !READ_BACK.lock().unwrap_or_else(|e| e.into_inner()).contains_key(&message.id)
}

/// The `List-Unsubscribe` header `raw` carries, read from its header block
/// alone: a newsletter's body is most of its bytes, and a line in the body
/// that looks like a header is not one.
fn header_in(raw: &[u8]) -> Option<ListUnsubscribe> {
    let parsed = jatayu_mail::mime::parse(header_block(raw)).ok()?;
    ListUnsubscribe::parse(
        parsed.list_unsubscribe.as_deref()?,
        parsed.list_unsubscribe_post.as_deref(),
    )
}

/// `raw` up to and including the blank line that ends its headers -- the
/// first `\r\n\r\n`, or `\n\n` for a message stored with bare newlines --
/// or all of it when there is no blank line.
fn header_block(raw: &[u8]) -> &[u8] {
    let crlf = find(raw, b"\r\n\r\n").map(|at| at + 4);
    let lf = find(raw, b"\n\n").map(|at| at + 2);
    let end = match (crlf, lf) {
        (Some(a), Some(b)) => a.min(b),
        (one, other) => one.or(other).unwrap_or(raw.len()),
    };
    &raw[..end]
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// The first of `candidates` -- newest first, already narrowed to one
/// sender -- whose header offers a way out, reading at most `max_reads`
/// messages' bytes along the way.
pub(crate) fn first_target(
    packs: Option<&dyn PackStore>,
    candidates: &[Message],
    max_reads: usize,
) -> Option<Target> {
    first_found(packs, candidates, max_reads).0
}

/// [`first_target`] over any run of messages, also saying how many messages'
/// bytes it read -- so a caller with a budget across many senders can take
/// it out of that. Kept headers are still looked at once the reads run out;
/// only reading stops.
fn first_found<'a>(
    packs: Option<&dyn PackStore>,
    candidates: impl IntoIterator<Item = &'a Message>,
    max_reads: usize,
) -> (Option<Target>, usize) {
    let mut reads = 0;
    for message in candidates {
        if reads_bytes(packs, message) {
            if reads >= max_reads {
                continue;
            }
            reads += 1;
        }
        if let Some(header) = header_of(packs, message)
            && header.method().is_some()
        {
            return (Some(target_of(message, header)), reads);
        }
    }
    (None, reads)
}

fn target_of(message: &Message, header: ListUnsubscribe) -> Target {
    let email = message.from.email.trim().to_lowercase();
    Target {
        account: message.account_id,
        name: name_of(&message.from.name, &email),
        email,
        message: message.clone(),
        header,
    }
}

/// A sender's display name, trimmed, or `email` when they gave none.
fn name_of(name: &str, email: &str) -> String {
    let name = name.trim();
    if name.is_empty() { email.to_string() } else { name.to_string() }
}

/// Whether `message` could be a sender's to leave: not a draft (by flag, or
/// by Gmail's `\Draft` label), from somebody, and not from one of `own`
/// ([`own_mailboxes`]).
fn is_candidate(message: &Message, own: &HashSet<String>) -> bool {
    !message.flags.draft
        && !message.labels.iter().any(|label| label == "\\Draft")
        && !message.from.email.trim().is_empty()
        && !is_own(&message.from.email, own)
}

/// Whether `a` is newer than `b`, the id breaking a tie so the answer never
/// depends on the order they were looked at in.
fn newer(a: &Message, b: &Message) -> bool {
    (a.date, a.id) > (b.date, b.id)
}

/// What unsubscribing from `thread` would do: the newest message in it that
/// is not a draft, not from one of the account's own addresses, and offers a
/// way out. `None` when no message does.
///
/// A thread that does not exist is the vault's not-found error, as
/// `get_thread` answers for one.
pub(crate) fn offer(
    vault: &Vault,
    packs: Option<&dyn PackStore>,
    thread: ThreadId,
) -> CommandResult<Option<UnsubscribeOffer>> {
    let (thread, messages) = vault.thread(thread)?;
    let own = own_mailboxes(&vault.account(thread.account_id)?);
    // Oldest first from the store; newest first is what is wanted.
    let candidates: Vec<Message> =
        messages.into_iter().rev().filter(|m| is_candidate(m, &own)).collect();
    Ok(first_target(packs, &candidates, MAX_READS).as_ref().and_then(offer_for))
}

fn offer_for(target: &Target) -> Option<UnsubscribeOffer> {
    let method = target.header.method()?;
    Some(UnsubscribeOffer {
        message: target.message.id,
        account: target.account,
        email: target.email.clone(),
        name: target.name.clone(),
        via: method.via(),
        to: method.destination(),
        asked_at: target.header.asked_at,
    })
}

/// The senders to unsubscribe from in `threads`, one [`Target`] per
/// (account, sender), and an outcome saying so for each sender whose mail
/// named no way out.
///
/// With `senders`, only messages from those addresses count, and each is
/// one sender however many threads it is spread over (Quick Cleanup's rows).
/// Without, each thread names one sender: the newest message in it, not a
/// draft and not the person's own, that offers a way out (a conversation,
/// as Mail lists it).
///
/// A thread that no longer exists is skipped: a bulk press can race an
/// archive or a sync that merged it away. Both lists come back in a stable
/// order, by name and then address.
pub(crate) fn targets_in_threads(
    vault: &Vault,
    packs: Option<&dyn PackStore>,
    threads: &[ThreadId],
    senders: Option<&[String]>,
) -> CommandResult<(Vec<Target>, Vec<UnsubscribeOutcome>)> {
    let wanted: Option<HashSet<String>> = senders.map(|senders| {
        senders.iter().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect()
    });
    if wanted.as_ref().is_some_and(|wanted| wanted.is_empty()) {
        return Ok((Vec::new(), Vec::new()));
    }

    // Each thread's candidates, newest first, with the account's own
    // addresses worked out once per account rather than once per thread.
    let mut own_of: HashMap<AccountId, HashSet<String>> = HashMap::new();
    let mut seen: HashSet<ThreadId> = HashSet::new();
    let mut conversations: Vec<Vec<Message>> = Vec::new();
    for &id in threads {
        if !seen.insert(id) {
            continue;
        }
        let (thread, messages) = match vault.thread(id) {
            Ok(found) => found,
            Err(jatayu_core::Error::NotFound { .. }) => continue,
            Err(e) => return Err(e.into()),
        };
        if let hash_map::Entry::Vacant(slot) = own_of.entry(thread.account_id) {
            slot.insert(own_mailboxes(&vault.account(thread.account_id)?));
        }
        let own = &own_of[&thread.account_id];
        let mut candidates: Vec<Message> =
            messages.into_iter().filter(|m| is_candidate(m, own)).collect();
        candidates.reverse();
        if !candidates.is_empty() {
            conversations.push(candidates);
        }
    }

    let mut budget = MAX_READS_PER_CALL;
    let (mut targets, mut nothing) = match &wanted {
        Some(wanted) => by_sender(packs, &conversations, wanted, &mut budget),
        None => by_conversation(packs, &conversations, &mut budget),
    };
    targets.sort_by_cached_key(|t| (t.name.to_lowercase(), t.email.clone(), t.account));
    nothing.sort_by_cached_key(|o| (o.name.to_lowercase(), o.email.clone(), o.account));
    Ok((targets, nothing))
}

/// [`targets_in_threads`] with `senders`: every message from one of `wanted`
/// across all of `conversations`, grouped by account and sender, and each
/// group's newest message that offers a way out.
fn by_sender(
    packs: Option<&dyn PackStore>,
    conversations: &[Vec<Message>],
    wanted: &HashSet<String>,
    budget: &mut usize,
) -> (Vec<Target>, Vec<UnsubscribeOutcome>) {
    // Ordered, so which senders a spent budget leaves unread is the same
    // every time.
    let mut groups: BTreeMap<(String, AccountId), Vec<&Message>> = BTreeMap::new();
    for message in conversations.iter().flatten() {
        let email = message.from.email.trim().to_lowercase();
        if wanted.contains(&email) {
            groups.entry((email, message.account_id)).or_default().push(message);
        }
    }
    let mut targets = Vec::new();
    let mut nothing = Vec::new();
    for ((email, account), mut messages) in groups {
        messages.sort_by_key(|m| Reverse((m.date, m.id)));
        let (found, reads) = first_found(packs, messages.iter().copied(), MAX_READS.min(*budget));
        *budget -= reads;
        match found {
            Some(target) => targets.push(target),
            None => {
                let name = messages
                    .first()
                    .map_or_else(|| email.clone(), |m| name_of(&m.from.name, &email));
                nothing.push(nothing_offered(account, &email, &name));
            }
        }
    }
    (targets, nothing)
}

/// [`targets_in_threads`] without `senders`: each conversation's newest
/// message that offers a way out names its sender, a sender named by two
/// conversations is asked once (from the newer message), and a conversation
/// with no way out at all is reported under its newest sender -- unless
/// another conversation found that sender a way out.
fn by_conversation(
    packs: Option<&dyn PackStore>,
    conversations: &[Vec<Message>],
    budget: &mut usize,
) -> (Vec<Target>, Vec<UnsubscribeOutcome>) {
    let mut found_for: BTreeMap<(String, AccountId), Target> = BTreeMap::new();
    let mut none_for: BTreeMap<(String, AccountId), String> = BTreeMap::new();
    for candidates in conversations {
        let Some(newest) = candidates.first() else { continue };
        let (found, reads) = first_found(packs, candidates, MAX_READS.min(*budget));
        *budget -= reads;
        match found {
            Some(target) => match found_for.entry((target.email.clone(), target.account)) {
                btree_map::Entry::Occupied(mut kept) => {
                    if newer(&target.message, &kept.get().message) {
                        *kept.get_mut() = target;
                    }
                }
                btree_map::Entry::Vacant(slot) => {
                    slot.insert(target);
                }
            },
            None => {
                let email = newest.from.email.trim().to_lowercase();
                let name = name_of(&newest.from.name, &email);
                none_for.entry((email, newest.account_id)).or_insert(name);
            }
        }
    }
    let nothing = none_for
        .into_iter()
        .filter(|(key, _)| !found_for.contains_key(key))
        .map(|((email, account), name)| nothing_offered(account, &email, &name))
        .collect();
    (found_for.into_values().collect(), nothing)
}

/// The outcome for a sender whose mail named no way out.
pub(crate) fn nothing_offered(account: AccountId, email: &str, name: &str) -> UnsubscribeOutcome {
    UnsubscribeOutcome {
        account,
        email: email.to_string(),
        name: name.to_string(),
        via: None,
        done: false,
        url: None,
        error: Some("their mail names no way to unsubscribe".to_string()),
    }
}

/// Ask every target's sender to stop: one click when offered, then an
/// email, then a page left for the person to open. A sender asked has
/// `asked_at` stamped on the message used. Never fails as a whole: each
/// sender's outcome says how theirs went.
///
/// A few at a time ([`AT_ONCE`]), answered in the order `targets` came in.
pub(crate) async fn unsubscribe_all(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    targets: Vec<Target>,
) -> Vec<UnsubscribeOutcome> {
    // Built once for the whole request. A client that could not be built is
    // each one-click sender's error, and the email and the page are still
    // tried for them.
    let client = crate::http::unsubscribe_client().map_err(|e| e.message);
    let asks: Vec<_> = targets
        .into_iter()
        .map(|target| ask(svc.clone(), vault.clone(), client.clone(), target))
        .collect();
    futures::stream::iter(asks).buffered(AT_ONCE).collect().await
}

/// Ask one sender, trying each way out their header offers in turn.
async fn ask(
    svc: Arc<Service>,
    vault: Arc<Vault>,
    client: Result<&'static reqwest::Client, String>,
    target: Target,
) -> UnsubscribeOutcome {
    let methods = target.header.methods();
    if methods.is_empty() {
        return nothing_offered(target.account, &target.email, &target.name);
    }
    let mut outcome = UnsubscribeOutcome {
        account: target.account,
        email: target.email.clone(),
        name: target.name.clone(),
        via: None,
        done: false,
        url: None,
        error: None,
    };
    let mut error: Option<String> = None;
    for method in methods {
        outcome.via = Some(method.via());
        let tried = match method {
            Method::OneClick { url } => match &client {
                Ok(client) => post_one_click(client, &url, false).await,
                Err(e) => Err(format!("one click could not be tried: {e}")),
            },
            Method::Mail(mailto) => ask_by_mail(&svc, &vault, &target, mailto).await,
            Method::Web { url } => {
                outcome.url = Some(url);
                break;
            }
        };
        match tried {
            Ok(()) => {
                outcome.done = true;
                break;
            }
            Err(e) => error = Some(e),
        }
    }
    if !outcome.done {
        // A page left to open still says why one click or an email did not
        // do it, so the person knows why they are being sent to a page.
        outcome.error = error;
        return outcome;
    }
    // They were asked, whatever happens to the note of it: a failure to
    // stamp is only a row that will offer it again.
    if let Err(e) = stamp(&svc, &vault, &target).await {
        tracing::warn!(
            error = %e,
            "an unsubscribe went through but could not be noted on its message"
        );
    }
    outcome
}

/// Note on `target`'s message that its sender has been asked, now -- and
/// with it the header itself, for a message whose record did not keep one.
async fn stamp(svc: &Service, vault: &Arc<Vault>, target: &Target) -> CommandResult<()> {
    let vault = vault.clone();
    let id = target.message.id;
    let header = ListUnsubscribe { asked_at: Some(svc.now()), ..target.header.clone() };
    blocking(move || Ok(vault.set_message_unsubscribe(id, Some(header))?)).await
}

/// Queue the email `mailto` describes, from the address `target`'s message
/// was sent to, to go now rather than after the undo window.
async fn ask_by_mail(
    svc: &Service,
    vault: &Arc<Vault>,
    target: &Target,
    mailto: Mailto,
) -> Result<(), String> {
    let origin = Origin::Person;
    // A visible no-op for a person's own press, made anyway -- see
    // `domains::mail`'s module docs on why every enqueue path makes it.
    svc.check_mail_rate_limit(&origin, "person").map_err(|e| e.message)?;
    let vault = vault.clone();
    let account = target.account;
    let message = target.message.id;
    let recipients: Vec<String> =
        target.message.to.iter().chain(&target.message.cc).map(|a| a.email.clone()).collect();
    let now = svc.now();
    blocking(move || {
        let identity = identity_for(&vault.account(account)?, &recipients);
        let mut draft = Draft::new(account, identity, origin.clone());
        // Only the address and the words the link gave: see
        // `jatayu_core::mail::unsubscribe`'s module docs on why nothing else
        // in a stranger's link is taken.
        draft.to = vec![Address::bare(mailto.to)];
        draft.subject = mailto.subject;
        draft.body_html = body_html(&mailto.body);
        // So a send that fails for good takes back the "asked" stamped on
        // the message below: see `Draft::unsubscribes`.
        draft.unsubscribes = Some(message);
        vault.save_draft(&draft)?;
        vault.queue_draft_send(draft.id, now, origin)?;
        Ok(())
    })
    .await
    .map_err(|e| format!("the email to them could not be queued: {}", e.message))?;
    svc.notify_mail_write(account);
    Ok(())
}

/// Which of `account`'s addresses an unsubscribe email goes from: the one
/// among `recipients` (the message's `To` and `Cc`) that the list wrote to,
/// spelled as the account has it set up -- a list keys its subscribers by
/// address, and an email from another of the person's addresses would not
/// match anybody on it. An address that is one mailbox with a set-up one, as
/// the provider delivers it (`me+news@gmail.com` is `me@gmail.com`), goes from
/// that set-up one: the outbox sends as whatever this says, and a server only
/// lets an account send as the addresses it knows. Otherwise the account's
/// own address.
fn identity_for(account: &Account, recipients: &[String]) -> String {
    let set_up: Vec<&str> = std::iter::once(account.address.as_str())
        .chain(account.identities.iter().map(|i| i.address.as_str()))
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .collect();
    for recipient in recipients {
        let recipient = recipient.trim();
        if let Some(exact) = set_up.iter().find(|a| a.eq_ignore_ascii_case(recipient)) {
            return exact.to_string();
        }
        let mailbox = crate::arrivals::mailbox_key(recipient);
        if let Some(same) = set_up.iter().find(|a| crate::arrivals::mailbox_key(a) == mailbox) {
            return same.to_string();
        }
    }
    account.address.clone()
}

/// A `mailto:` body as the compose editor's lines: each line of it escaped
/// and in a `<p>` of its own, a blank line an empty one.
fn body_html(body: &str) -> String {
    body.lines().map(|line| format!("<p>{}</p>", escape_html(line))).collect()
}

/// POST RFC 8058's one-click body to `url`, and say whether the sender took
/// it: any 2xx answer is yes, and so is a 301, 302 or 303 (not followed);
/// anything else is no, in words that never carry the link (it holds a
/// token per subscriber).
///
/// Refused before anything is sent: an address that is not https, and one
/// whose host is a numeric address on this computer or a private network --
/// the one case [`crate::http::PublicOnly`] never sees, since nothing is
/// resolved. `allow_loopback` is the test seam, like `mail::view`'s
/// `remote_image_allowing_loopback_for_test`: it lets a plain http server on
/// `127.0.0.1` stand in for a sender. Every production caller passes `false`.
pub(crate) async fn post_one_click(
    client: &reqwest::Client,
    url: &str,
    allow_loopback: bool,
) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|_| "their one-click link is not a web address".to_string())?;
    let secure = parsed.scheme() == "https" || (allow_loopback && parsed.scheme() == "http");
    if !secure {
        return Err("their one-click link is not a secure one, so it was not used".to_string());
    }
    let private = parsed
        .host_str()
        .and_then(crate::http::literal_ip)
        .is_some_and(|ip| crate::http::is_forbidden_address(ip, allow_loopback));
    if private {
        return Err("their one-click link points at a private network".to_string());
    }
    let response = client
        .post(parsed)
        .header(reqwest::header::CONTENT_TYPE, ONE_CLICK_CONTENT_TYPE)
        .header(reqwest::header::USER_AGENT, ONE_CLICK_USER_AGENT)
        .body(ONE_CLICK_BODY)
        .send()
        .await
        .map_err(|e| not_sent(&e))?;
    let status = response.status();
    // RFC 8058 has the sender answer the POST itself, but plenty take it and
    // then point at a "you're unsubscribed" page with a 301, 302 or 303.
    // Those are a yes, and the page is not followed: that would be the
    // background fetch of a page this must never make. A 307 or 308 asks for
    // the POST again somewhere else, so it was not taken.
    let taken = status.is_success() || matches!(status.as_u16(), 301..=303);
    if taken {
        Ok(())
    } else if status.is_redirection() {
        Err(format!(
            "their server sent it somewhere else rather than taking it (HTTP {})",
            status.as_u16()
        ))
    } else {
        Err(format!("their server refused it (HTTP {})", status.as_u16()))
    }
}

/// A one-click POST that never got an answer, in words.
fn not_sent(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "their server did not answer in time".to_string()
    } else if e.is_connect() {
        "it could not be reached".to_string()
    } else {
        format!("it did not go through: {}", crate::http::strip_url(&e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jatayu_core::account::{Identity, Provider};
    use jatayu_core::id::PackId;
    use jatayu_core::mail::{CategorySource, MessageFlags};
    use jatayu_core::packstore::PackRef;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A server on this machine that answers every request with `reply`, and
    /// keeps each request it was sent, whole. Answers the address of a
    /// one-click link on it, token and all.
    async fn sender(reply: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let kept = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let request = read_request(&mut socket).await;
                kept.lock().unwrap().push(request);
                let _ = socket.write_all(reply.as_bytes()).await;
            }
        });
        (format!("http://127.0.0.1:{port}/u/secret-token"), seen)
    }

    /// One whole request: its head, and as much body as it says it has.
    async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = socket.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf);
            if let Some(end) = text.find("\r\n\r\n") {
                let length = text[..end]
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buf.len() >= end + 4 + length {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// A client shaped like [`crate::http::unsubscribe_client`] where it
    /// matters here -- no proxy, no redirect -- but able to reach a plain
    /// http server on this machine.
    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn a_one_click_post_sends_exactly_what_rfc_8058_asks_and_nothing_more() {
        let (url, seen) = sender("HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n").await;
        post_one_click(&client(), &url, true).await.unwrap();

        let requests = seen.lock().unwrap().clone();
        assert_eq!(requests.len(), 1, "{requests:?}");
        let (head, body) = requests[0].split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("POST /u/secret-token HTTP/1.1\r\n"), "{head}");
        assert_eq!(body, "List-Unsubscribe=One-Click");
        let has = |line: &str| head.lines().any(|h| h.eq_ignore_ascii_case(line));
        assert!(has("content-type: application/x-www-form-urlencoded"), "{head}");
        assert!(has("user-agent: Mozilla/5.0"), "{head}");
        for leak in ["cookie:", "authorization:", "referer:"] {
            let sent = head.lines().any(|h| h.to_ascii_lowercase().starts_with(leak));
            assert!(!sent, "{leak} in {head}");
        }
    }

    #[tokio::test]
    async fn a_pointer_to_a_confirmation_page_is_a_yes_and_is_not_followed() {
        for status in ["301 Moved Permanently", "302 Found", "303 See Other"] {
            let reply: &'static str = Box::leak(
                format!(
                    "HTTP/1.1 {status}\r\nlocation: http://127.0.0.1:9/confirm\r\ncontent-length: 0\r\n\r\n"
                )
                .into_boxed_str(),
            );
            let (url, seen) = sender(reply).await;
            post_one_click(&client(), &url, true).await.unwrap();
            assert_eq!(seen.lock().unwrap().len(), 1, "{status}: one POST, nothing followed");
        }
    }

    #[tokio::test]
    async fn anything_but_a_success_is_a_failure_that_names_no_address() {
        for (reply, code) in [
            ("HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n", "404"),
            (
                concat!(
                    "HTTP/1.1 307 Temporary Redirect\r\n",
                    "location: https://elsewhere.example/confirm?t=1\r\n",
                    "content-length: 0\r\n\r\n",
                ),
                "307",
            ),
            ("HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\n\r\n", "500"),
        ] {
            let (url, seen) = sender(reply).await;
            let error = post_one_click(&client(), &url, true).await.unwrap_err();
            assert!(error.contains(code), "{error}");
            for leak in ["127.0.0.1", "secret-token", "http:", "elsewhere"] {
                assert!(!error.contains(leak), "{leak} in {error:?}");
            }
            assert_eq!(seen.lock().unwrap().len(), 1, "one POST, and nothing fetched after it");
        }
    }

    #[tokio::test]
    async fn a_link_that_is_not_https_or_names_a_private_address_is_never_posted_to() {
        let client = client();
        for url in [
            "http://list.example/u",
            "https://127.0.0.1/u",
            "https://10.0.0.8/u",
            "https://192.168.1.1/u",
            "https://[::1]/u",
            "not a link at all",
        ] {
            let error = post_one_click(&client, url, false).await.unwrap_err();
            assert!(!error.contains(url), "{error}");
        }
    }

    #[test]
    fn the_header_is_read_from_the_header_block_alone() {
        let crlf = b"From: news@list.example\r\n\
List-Unsubscribe: <mailto:leave@list.example?subject=stop>,\r\n <https://list.example/u/abc>\r\n\
List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n\
Content-Type: text/plain\r\n\
\r\n\
List-Unsubscribe: <https://evil.example/u>\r\n";
        let header = header_in(crlf).unwrap();
        assert_eq!(
            header.links,
            vec!["mailto:leave@list.example?subject=stop", "https://list.example/u/abc"]
        );
        assert!(header.one_click);
        assert_eq!(header.via(), Some(UnsubscribeVia::OneClick));

        let bare_newlines =
            b"From: news@list.example\nList-Unsubscribe: <https://list.example/u/abc>\n\nHello.\n";
        assert_eq!(header_in(bare_newlines).unwrap().via(), Some(UnsubscribeVia::Web));

        let in_the_body_only =
            b"From: a@b.example\r\n\r\nList-Unsubscribe: <https://evil.example/u>\r\n";
        assert_eq!(header_in(in_the_body_only), None, "a line in the body is not a header");
        assert_eq!(header_in(b""), None);
    }

    #[test]
    fn the_header_block_ends_at_the_first_blank_line_of_either_kind() {
        assert_eq!(header_block(b"A: 1\r\n\r\nbody\n\nmore"), b"A: 1\r\n\r\n");
        assert_eq!(header_block(b"A: 1\n\nbody\r\n\r\nmore"), b"A: 1\n\n");
        assert_eq!(header_block(b"A: 1\r\nB: 2"), b"A: 1\r\nB: 2");
        assert_eq!(header_block(b""), b"");
    }

    fn message(from: &str, minutes: i64, header: Option<ListUnsubscribe>) -> Message {
        let id = MailMessageId::new();
        let account_id = AccountId::new();
        Message {
            id,
            account_id,
            thread_id: ThreadId::new(),
            message_id_header: format!("<{id}@example.com>"),
            date: Timestamp::from_second(1_790_000_000).unwrap()
                + jiff::SignedDuration::from_mins(minutes),
            from: Address::new("  The Weekly ", from),
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: "Issue".into(),
            snippet: String::new(),
            flags: MessageFlags::default(),
            labels: Vec::new(),
            has_attachments: false,
            size: 128,
            category: None,
            category_source: CategorySource::Rules,
            pack: PackRef {
                account: account_id.to_string(),
                pack: PackId::new(),
                offset: 0,
                len: 0,
            },
            gmail: None,
            invite: None,
            unsubscribe: header,
        }
    }

    #[test]
    fn the_first_message_with_a_usable_way_out_is_the_target() {
        let unusable =
            ListUnsubscribe { links: vec!["mailto:not-an-address".into()], ..Default::default() };
        let usable = ListUnsubscribe::parse("<mailto:leave@list.example>", None);
        let older = ListUnsubscribe::parse("<https://list.example/old>", None);
        let candidates = vec![
            message("News@List.example ", 30, None),
            message("news@list.example", 20, Some(unusable)),
            message("news@list.example", 10, usable.clone()),
            message("news@list.example", 0, older),
        ];
        let target = first_target(None, &candidates, MAX_READS).unwrap();
        assert_eq!(target.message.id, candidates[2].id);
        assert_eq!(target.header, usable.unwrap());
        assert_eq!(target.email, "news@list.example");
        assert_eq!(target.name, "The Weekly", "trimmed");
        assert_eq!(target.account, candidates[2].account_id);

        let none = vec![message("news@list.example", 0, None)];
        assert!(first_target(None, &none, MAX_READS).is_none(), "no bytes to read, no header");
        assert!(!reads_bytes(None, &none[0]), "and nothing counted as read");
    }

    /// A pack store holding one newsletter's bytes for every message, that
    /// counts how often it is asked.
    #[derive(Default)]
    struct CountingPacks {
        reads: std::sync::atomic::AtomicUsize,
    }

    impl PackStore for CountingPacks {
        fn append_batch(&self, _: &str, _: &[&[u8]]) -> jatayu_core::Result<Vec<PackRef>> {
            Ok(Vec::new())
        }
        fn read(&self, _: &PackRef) -> jatayu_core::Result<Vec<u8>> {
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(b"From: news@list.example\r\n\
List-Unsubscribe: <mailto:leave@list.example>\r\n\r\nHello.\r\n"
                .to_vec())
        }
        fn mark_dead(&self, _: &[PackRef]) -> jatayu_core::Result<()> {
            Ok(())
        }
        fn delete_account(&self, _: &str) -> jatayu_core::Result<()> {
            Ok(())
        }
        fn high_water_mark(&self, _: &str) -> jatayu_core::Result<Option<PackId>> {
            Ok(None)
        }
        fn compaction_worthwhile(
            &self,
            _: &str,
            _: &jatayu_core::packstore::ReferencedSnapshot,
        ) -> jatayu_core::Result<bool> {
            Ok(false)
        }
        fn compact(
            &self,
            _: &str,
            _: &jatayu_core::packstore::ReferencedSnapshot,
            _: &dyn Fn() -> bool,
        ) -> jatayu_core::Result<jatayu_core::packstore::CompactionResult> {
            unreachable!("never compacted here")
        }
        fn drop_packs(&self, _: &str, _: &[PackId]) -> jatayu_core::Result<()> {
            Ok(())
        }
    }

    /// A message synced before headers were kept, with bytes to read.
    fn old_message(size: u64) -> Message {
        let mut m = message("news@list.example", 0, None);
        m.pack.len = 64;
        m.size = size;
        m
    }

    #[test]
    fn an_old_messages_bytes_are_read_once_and_never_when_they_are_a_file() {
        let packs = CountingPacks::default();
        let count = || packs.reads.load(std::sync::atomic::Ordering::SeqCst);

        let old = old_message(2_000);
        assert!(reads_bytes(Some(&packs), &old));
        let header = header_of(Some(&packs), &old).expect("read from its bytes");
        assert_eq!(header.via(), Some(UnsubscribeVia::Mail));
        assert_eq!(header_of(Some(&packs), &old), Some(header), "the same answer again");
        assert!(!reads_bytes(Some(&packs), &old), "and no read is counted for it any more");
        assert_eq!(count(), 1, "its bytes read once");

        let file = old_message(MAX_READ_BYTES + 1);
        assert!(!reads_bytes(Some(&packs), &file));
        assert_eq!(header_of(Some(&packs), &file), None, "too large to be a list's");
        assert_eq!(count(), 1, "never read");

        let mut looked = old_message(2_000);
        looked.unsubscribe = Some(ListUnsubscribe::default());
        assert!(!reads_bytes(Some(&packs), &looked));
        assert_eq!(header_of(Some(&packs), &looked), None, "looked at, nothing offered");
        assert_eq!(count(), 1, "a kept none is never read again");
    }

    #[test]
    fn the_email_goes_from_the_address_the_list_wrote_to() {
        let mut account = Account::new(Provider::Custom, "me@gmail.com");
        account.identities.push(Identity {
            name: "Me".into(),
            address: "Lists@Example.com".into(),
            signature_html: String::new(),
        });
        let to = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            identity_for(&account, &to(&["friend@example.com", "lists@example.com"])),
            "Lists@Example.com",
            "the alias, spelled as it is set up"
        );
        assert_eq!(
            identity_for(&account, &to(&["M.e+news@googlemail.com"])),
            "me@gmail.com",
            "one mailbox with the account's, as Gmail delivers it"
        );
        assert_eq!(identity_for(&account, &to(&["someone@else.example"])), "me@gmail.com");
        assert_eq!(identity_for(&account, &[]), "me@gmail.com");
    }

    #[test]
    fn the_body_is_escaped_line_by_line() {
        assert_eq!(body_html("Unsubscribe"), "<p>Unsubscribe</p>");
        assert_eq!(
            body_html("remove <me>\n\nnow & then"),
            "<p>remove &lt;me&gt;</p><p></p><p>now &amp; then</p>"
        );
    }

    #[test]
    fn a_sender_with_no_way_out_says_so() {
        let account = AccountId::new();
        let outcome = nothing_offered(account, "bob@example.com", "Bob");
        assert!(!outcome.done && outcome.via.is_none() && outcome.url.is_none());
        assert!(outcome.error.is_some());
    }
}
