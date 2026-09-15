//! The sync passes: headers, bodies (with attachments folded in, since both
//! need the same raw bytes), and the discovery-plus-both-passes sweep that
//! serves the first sync and every steady-state wake alike.
//!
//! # Why "three passes" is two functions
//!
//! `docs/plans/mail.md` describes headers, bodies and attachments as three
//! passes. Here, attachments are extracted inside the bodies pass rather
//! than as a separate walk over the mailbox: the plan's own words say the
//! attachments pass "only extracts" from bytes "already inside the raw
//! message", and [`bodies_pass`] already has those bytes in hand, decoded,
//! for exactly as long as one message's processing takes. A third pass
//! would mean fetching -- or re-decoding from the pack store -- the same raw
//! bytes a second time for no reason this crate could find.
//!
//! # Resumability, without a numeric cursor
//!
//! Neither pass trusts a "how far did we get" counter. [`sync_headers`]
//! asks the vault which UIDs it already has (via
//! [`everyday_core::Vault::mail_uid_set`]) and only asks the server for the
//! rest; [`bodies_pass`] asks which of *those* still carry
//! [`crate::mailsync::ingest::pending_pack_ref`]'s sentinel and only fetches
//! raw bytes for them. A process killed mid-batch leaves some messages
//! ingested and some not, some bodies fetched and some not -- and the next
//! attempt's first query already excludes everything that landed, so
//! nothing is re-ingested as a duplicate and nothing already-fetched is
//! fetched twice. This is also what makes the same two functions serve the
//! first sync and a steady-state wake: a wake is simply a sync with very
//! little left to do.

use std::collections::HashMap;
use std::sync::Arc;

use everyday_core::id::{AccountId, MailboxId};
use everyday_core::mail::{Body, MailboxRole, Message, PartRef};
use everyday_core::packstore::PackStore;
use everyday_core::store::mail::IngestMessage;
use everyday_core::{MailDoc, MailSearch, Vault};
use everyday_mail::mime::Disposition;
use everyday_mail::session::{MailSession, Result as SessionResult, SyncCursor, Uid, UidSet};
use futures::StreamExt;

use crate::mailsync::discovery::{self, LabelMailboxes, SyncedMailbox};
use crate::mailsync::ingest::{self, ThreadIndex};
use crate::mailsync::status::{Phase, StatusRegistry};

/// Headers are fetched in batches of about this many UIDs -- "a few hundred"
/// per the plan, matching the size a `UID FETCH` line stays comfortably
/// short at.
const HEADER_BATCH_SIZE: usize = 500;

/// Bodies are fetched in batches of about this many messages at once.
///
/// The plan asks for batches "bounded by bytes" rather than by count; this
/// bounds by count instead, which is simpler and -- since
/// [`everyday_mail::imap::ImapSession::raw`] already sub-batches its own
/// `BODY.PEEK[]` fetches at twenty messages internally -- still keeps any
/// one round trip small. Revisit if a real mailbox's attachment-heavy
/// messages ever make a hundred of them at once too much to hold in memory
/// between the fetch and the blocking-pool pass below.
const BODY_BATCH_SIZE: usize = 100;

/// What every pass needs, gathered once by [`crate::mailsync::task::run_account`]
/// so a test can build the same shape without a live connection.
pub struct SyncContext<'a> {
    pub vault: &'a Arc<Vault>,
    pub account_id: AccountId,
    pub packs: Arc<dyn PackStore>,
    pub index: Arc<dyn MailSearch>,
    pub statuses: &'a StatusRegistry,
    pub attachment_cap_bytes: Option<u64>,
}

/// Discover `ctx.account_id`'s mailboxes and run [`sync_headers`] then
/// [`bodies_pass`] over every one of them, inbox first. What one first sync
/// and one steady-state wake both are, from the caller's side.
pub async fn sync_once<S: MailSession>(
    ctx: &SyncContext<'_>,
    session: &mut S,
    labels: &mut LabelMailboxes,
    threads: &mut ThreadIndex,
) -> SessionResult<Vec<SyncedMailbox>> {
    let mut mailboxes = discovery::discover(ctx.vault, ctx.account_id, session).await?;
    for mailbox in &mut mailboxes {
        sync_headers(ctx, session, mailbox, labels, threads).await?;
    }
    for mailbox in &mailboxes {
        bodies_pass(ctx, session, mailbox).await?;
    }
    let _ = ctx.index.commit();
    Ok(mailboxes)
}

/// `SELECT` `mailbox`, diff it against what the vault already has, and
/// ingest headers for everything new -- in batches of
/// [`HEADER_BATCH_SIZE`], newest first, committing each batch as it lands.
/// See the module docs for why "committing" needs no cursor field beyond
/// what [`everyday_core::Vault::ingest`] itself already made durable.
pub async fn sync_headers<S: MailSession>(
    ctx: &SyncContext<'_>,
    session: &mut S,
    mailbox: &mut SyncedMailbox,
    labels: &mut LabelMailboxes,
    threads: &mut ThreadIndex,
) -> SessionResult<()> {
    let state = session.select(&mailbox.remote_name).await?;

    // A `UIDVALIDITY` change: forget this mailbox's membership and start
    // fresh, rematching by `Message-ID` as headers arrive rather than
    // trusting a uid that now means something else -- see
    // `crate::mailsync::ingest`'s module docs.
    let just_reset = mailbox.row.uidvalidity != 0 && mailbox.row.uidvalidity != state.uidvalidity;
    if just_reset {
        let _ = ctx.vault.reset_mailbox(mailbox.row.id);
        mailbox.row.uidvalidity = 0;
        mailbox.row.uidnext = 0;
        mailbox.row.highest_modseq = 0;
    }
    if mailbox.row.uidvalidity == 0 {
        mailbox.row.uidvalidity = state.uidvalidity;
    }

    let cursor = SyncCursor {
        uidvalidity: mailbox.row.uidvalidity,
        // Unused by every adapter today (see `SyncCursor`'s own docs): what
        // actually decides which uids are "new" is the vault's own
        // membership, read fresh below, not this field.
        highest_uid_seen: 0,
        highestmodseq: (mailbox.row.highest_modseq != 0).then_some(mailbox.row.highest_modseq),
    };
    let known: UidSet =
        ctx.vault.mail_uid_set(mailbox.row.id).unwrap_or_default().into_iter().collect();
    let changes = session.changes_since(&cursor, &known).await?;

    if !changes.vanished.is_empty() {
        let uids: Vec<Uid> = changes.vanished.iter().collect();
        let _ = ctx.vault.remove_mail_uids(mailbox.row.id, &uids);
    }
    for &(uid, flags, _modseq) in &changes.flag_changes {
        let _ = ctx.vault.update_message_flags(mailbox.row.id, uid, ingest::mail_flags(flags));
    }

    let new_uids: Vec<Uid> = changes.new_uids.iter().collect();
    let total = new_uids.len() as u64;
    let mut done = 0u64;
    ctx.statuses.set_phase(ctx.account_id, Phase::Headers, done, total);

    for batch in discovery::newest_first_chunks(new_uids, HEADER_BATCH_SIZE) {
        let uid_set: UidSet = batch.iter().copied().collect();
        let mut headers = session.headers(&uid_set).await?;
        // Newest first within the batch too, matching the plan's ordering.
        headers.sort_by(|a, b| b.uid.cmp(&a.uid).then(b.internal_date.cmp(&a.internal_date)));

        let mut ingest_batch = Vec::with_capacity(headers.len());
        for header in &headers {
            let resolved = match ingest::resolve_header(
                ctx.vault,
                ctx.account_id,
                header,
                threads,
                just_reset,
            ) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        uid = header.uid,
                        "could not parse a message header; skipping it"
                    );
                    continue;
                }
            };
            ingest_batch.push(IngestMessage {
                message: resolved.message.clone(),
                mailbox: mailbox.row.id,
                uid: header.uid,
            });
            // The Gmail folder rule: All Mail carries every physical
            // message, and its own `X-GM-LABELS` is where `\Inbox` and
            // every user label come from -- see `crate::mailsync::discovery`.
            if mailbox.row.role == MailboxRole::All
                && let Some(gmail) = &header.gmail
            {
                for label in &gmail.labels {
                    let label_row = labels.resolve(ctx.vault, label);
                    ingest_batch.push(IngestMessage {
                        message: resolved.message.clone(),
                        mailbox: label_row.id,
                        uid: header.uid,
                    });
                }
            }
        }
        if let Err(e) = ctx.vault.ingest_mail(ctx.account_id, ingest_batch) {
            tracing::warn!(error = %e, "could not ingest a batch of headers");
        }

        done += headers.len() as u64;
        ctx.statuses.set_phase(ctx.account_id, Phase::Headers, done, total);
        // Yields between batches, per the plan's throttling rule, so a
        // giant first sync never starves whatever else this runtime is
        // doing between one `UID FETCH` and the next.
        tokio::task::yield_now().await;
    }

    mailbox.row.uidnext = state.uidnext;
    if let Some(modseq) = state.highestmodseq {
        mailbox.row.highest_modseq = modseq;
    }
    let _ = ctx.vault.save_mailbox(&mailbox.row);

    Ok(())
}

/// Fetch raw bytes for every message in `mailbox` still carrying
/// [`ingest::pending_pack_ref`]'s sentinel, and turn each into a sealed pack
/// entry, a sanitised [`Body`] and a searchable [`MailDoc`].
///
/// The CPU-heavy half of each message -- MIME parsing, HTML sanitising, the
/// quoted/signature heuristics, attachment extraction -- runs on the
/// blocking pool, one batch at a time, so a first sync never runs that work
/// on the same task a command handler needs to make progress. Bounded by
/// [`BODY_BATCH_SIZE`] rather than a semaphore of its own: batches are
/// already processed one at a time, so the "bounded parallelism" the plan
/// asks for is, today, bounded at one batch in flight.
pub async fn bodies_pass<S: MailSession>(
    ctx: &SyncContext<'_>,
    session: &mut S,
    mailbox: &SyncedMailbox,
) -> SessionResult<()> {
    let pending = pending_messages(ctx.vault, mailbox.row.id);
    if pending.is_empty() {
        return Ok(());
    }
    // `sync_headers`, run for every mailbox just before this pass starts,
    // leaves the session `SELECT`ed on whichever mailbox it looked at last
    // -- almost never this one. `raw` fetches by uid within the *selected*
    // mailbox, so this has to point the session back here first.
    session.select(&mailbox.remote_name).await?;
    let total = pending.len() as u64;
    let mut done = 0u64;
    ctx.statuses.set_phase(ctx.account_id, Phase::Bodies, done, total);

    for batch in pending.chunks(BODY_BATCH_SIZE) {
        let uid_set: UidSet = batch.iter().map(|(_, uid)| *uid).collect();
        let mut raw_by_uid: HashMap<Uid, Vec<u8>> = HashMap::new();
        {
            let mut stream = session.raw(&uid_set).await?;
            while let Some(item) = stream.next().await {
                let (uid, bytes) = item?;
                raw_by_uid.insert(uid, bytes);
            }
        }

        let vault = ctx.vault.clone();
        let packs = ctx.packs.clone();
        let account_id = ctx.account_id;
        let mailbox_id = mailbox.row.id;
        let attachment_cap = ctx.attachment_cap_bytes;
        let batch_owned: Vec<(Message, Uid)> = batch.to_vec();
        let docs = tokio::task::spawn_blocking(move || {
            let mut processed = Vec::with_capacity(batch_owned.len());
            for (message, uid) in batch_owned {
                let Some(raw) = raw_by_uid.get(&uid) else { continue };
                if let Some(one) = process_body(
                    vault.as_ref(),
                    packs.as_ref(),
                    account_id,
                    uid,
                    message,
                    raw,
                    attachment_cap,
                ) {
                    processed.push(one);
                }
            }
            // One upsert for the whole batch rather than one per message --
            // `everyday-store-sql`'s own `upsert_batched` is what keeps this
            // off the vault's single writer lock for the length of a giant
            // first sync, the same reasoning `MailStore::ingest`'s own docs
            // give for accepting a `Vec` rather than one message at a time.
            let ingests: Vec<IngestMessage> = processed
                .iter()
                .map(|(message, uid, _)| IngestMessage {
                    message: message.clone(),
                    mailbox: mailbox_id,
                    uid: *uid,
                })
                .collect();
            let _ = vault.ingest_mail(account_id, ingests);
            let bodies: Vec<Body> = processed.iter().map(|(_, _, body)| body.clone()).collect();
            let _ = vault.save_bodies(&bodies);
            let docs: Vec<MailDoc> = processed
                .iter()
                .map(|(message, _uid, body)| {
                    mail_doc(account_id, mailbox_id, message, &body.model_text())
                })
                .collect();
            docs
        })
        .await
        .unwrap_or_default();

        if !docs.is_empty() {
            let _ = ctx.index.index(&docs);
            // Committed every batch -- simpler than the plan's "every 2,000
            // docs or every few seconds", and cheap: tantivy's own commit
            // merges rather than rebuilding, so a batch of at most
            // `BODY_BATCH_SIZE` messages costs comfortably less than the
            // budget's 150 ms search itself is measured against.
            let _ = ctx.index.commit();
        }

        done += batch.len() as u64;
        ctx.statuses.set_phase(ctx.account_id, Phase::Bodies, done, total);
        tokio::task::yield_now().await;
    }

    Ok(())
}

/// Every message in `mailbox` whose pack is still
/// [`ingest::pending_pack_ref`]'s sentinel, newest first.
fn pending_messages(vault: &Vault, mailbox_id: MailboxId) -> Vec<(Message, Uid)> {
    let mut out = Vec::new();
    let Ok(uids) = vault.mail_uid_set(mailbox_id) else { return out };
    for uid in uids {
        if let Ok(Some(message)) = vault.message_by_uid(mailbox_id, uid)
            && ingest::is_pending(&message.pack)
        {
            out.push((message, uid));
        }
    }
    out.sort_by(|a, b| b.0.date.cmp(&a.0.date));
    out
}

/// One message's raw bytes turned into a sealed pack entry and a sanitised
/// body -- run on the blocking pool by [`bodies_pass`], which collects the
/// results of a whole batch before writing any of it, so a giant first sync
/// costs one upsert per batch rather than one per message. `None` only when
/// the pack store itself refuses the write; a message that fails to *parse*
/// still gets an (empty) body rather than being skipped, since its raw bytes
/// are sealed either way and a blank preview is a smaller failure than
/// refetching for ever.
fn process_body(
    vault: &Vault,
    packs: &dyn PackStore,
    account_id: AccountId,
    uid: Uid,
    mut message: Message,
    raw: &[u8],
    attachment_cap_bytes: Option<u64>,
) -> Option<(Message, Uid, Body)> {
    let sealed = packs.append_batch(&account_id.to_string(), &[raw]).ok()?;
    message.pack = sealed.into_iter().next()?;

    let mut remote_images = Vec::new();
    let (html_sanitised, plain, parts, has_attachments) = match everyday_mail::mime::parse(raw) {
        Ok(parsed) => {
            let html_sanitised = parsed.html.as_deref().map_or_else(String::new, |html| {
                let rewrite = everyday_mail::sanitize::Rewrite::new(message.id.to_string());
                let sanitised = everyday_mail::sanitize::sanitize(html, &rewrite);
                // What `mailview::remote_image` looks a token up in: without
                // it, every image a message names would answer "no such
                // remote image" even once its sender is trusted.
                remote_images = sanitised
                    .remote_images
                    .into_iter()
                    .map(|r| everyday_core::mail::RemoteImage {
                        original_url: r.original_url,
                        token: r.token,
                        cached_blob: None,
                    })
                    .collect();
                sanitised.html
            });
            let plain = parsed.text.clone().unwrap_or_else(|| {
                parsed.html.as_deref().map(everyday_mail::text::html_to_text).unwrap_or_default()
            });

            let mut parts = Vec::with_capacity(parsed.parts.len());
            let mut has_attachments = false;
            for part in &parsed.parts {
                if part.disposition == Disposition::Attachment {
                    has_attachments = true;
                }
                let within_cap = attachment_cap_bytes.is_none_or(|cap| (part.size as u64) <= cap);
                let blob = if within_cap {
                    everyday_mail::mime::part_bytes(raw, &part.part_id)
                        .ok()
                        .and_then(|bytes| vault.put_blob(&bytes).ok())
                } else {
                    None
                };
                parts.push(PartRef {
                    cid: part.content_id.clone(),
                    filename: part.filename.clone(),
                    mime_type: part.content_type.clone(),
                    size: part.size as u64,
                    blob,
                });
            }
            (html_sanitised, plain, parts, has_attachments)
        }
        Err(e) => {
            tracing::warn!(error = %e, message = %message.id, "could not parse a message body");
            (String::new(), String::new(), Vec::new(), false)
        }
    };

    let quoted_ranges: Vec<(u32, u32)> = everyday_mail::text::quoted_ranges(&plain)
        .into_iter()
        .map(|r| (r.start as u32, r.end as u32))
        .collect();
    let signature_range =
        everyday_mail::text::signature_range(&plain).map(|r| (r.start as u32, r.end as u32));

    message.snippet = everyday_mail::text::snippet(&plain);
    message.has_attachments = has_attachments;

    let body = Body {
        message_id: message.id,
        html_sanitised,
        text: plain,
        quoted_ranges,
        signature_range,
        parts,
        remote_images,
    };

    Some((message, uid, body))
}

/// A [`MailDoc`] for `message`, filed under `mailbox_id`. Shared by
/// [`process_body`] (which has just computed `body_text` itself) and
/// `crate::domains::mailsync::rebuild_mail_index` (which reads it back out
/// of an already-sealed [`Body`] via [`Body::model_text`]).
pub(crate) fn mail_doc(
    account_id: AccountId,
    mailbox_id: MailboxId,
    message: &Message,
    body_text: &str,
) -> MailDoc {
    MailDoc {
        message_key: message.id.to_string(),
        thread_key: message.thread_id.to_string(),
        account: account_id.to_string(),
        mailboxes: vec![mailbox_id.to_string()],
        from: display_address(&message.from),
        to: message.to.iter().map(display_address).collect::<Vec<_>>().join(", "),
        cc: message.cc.iter().map(display_address).collect::<Vec<_>>().join(", "),
        subject: message.subject.clone(),
        body_text: body_text.to_string(),
        labels: message.labels.clone(),
        date: message.date,
        has_attachment: message.has_attachments,
        unread: message.flags.unread(),
        starred: message.flags.flagged,
    }
}

fn display_address(a: &everyday_core::mail::Address) -> String {
    if a.name.is_empty() { a.email.clone() } else { format!("{} <{}>", a.name, a.email) }
}
