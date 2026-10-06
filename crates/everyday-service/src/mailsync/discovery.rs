//! Deciding which of an account's mailboxes to sync, and keeping the vault's
//! [`Mailbox`] rows in step with what `LIST` says.
//!
//! # The Gmail folder rule
//!
//! `docs/plans/mail.md`'s settled answer to "which folders sync": on a
//! server that advertises `X-GM-EXT-1`, only five real IMAP mailboxes are
//! ever `SELECT`ed -- All Mail, Sent, Drafts, Spam and Trash -- because All
//! Mail alone already holds every message Gmail keeps (Spam and Trash are
//! its two exceptions, which is why they are named individually) and every
//! other folder Gmail shows, `INBOX` included, is a *label*, not a place a
//! message physically lives. Fetching those as separate mailboxes too would
//! mean a message crosses the wire as many times as it has labels.
//!
//! So `\Inbox` and every user label reach the vault a different way: they
//! are read off each message's own `X-GM-LABELS` during the headers pass
//! (see [`crate::mailsync::ingest`]) and turned into [`Mailbox`] rows of
//! their own, with [`MailboxRole::Inbox`] for `\Inbox` and
//! [`MailboxRole::Other`] for everything else -- rows that
//! `message_mailboxes` can then name a membership against exactly the way
//! it names membership in All Mail, Sent, Drafts, Spam or Trash. A message
//! in the inbox with one label therefore ends up in *three*
//! `message_mailboxes` rows: All Mail (where its bytes live), the inbox
//! label, and the user label -- one physical fetch, three memberships. This
//! module only creates the five folder-backed rows; [`LabelMailboxes`] is
//! what mints the label-backed ones as they are discovered.
//!
//! # Everywhere else
//!
//! "Every folder", per the same settled answer -- every `LIST`ed mailbox
//! that is selectable becomes a synced [`Mailbox`], with
//! [`everyday_mail::session::Role`] mapped onto [`MailboxRole`] one for one
//! and `Role::All` (Gmail-only, in practice) falling back to
//! [`MailboxRole::Other`] on a server that is not Gmail but happens to
//! advertise the same `SPECIAL-USE` attribute.

use std::collections::HashMap;
use std::sync::Arc;

use everyday_core::Vault;
use everyday_core::id::{AccountId, MailboxId};
use everyday_core::mail::{Mailbox, MailboxRole};
use everyday_mail::imap::unescape_imap_quoted;
use everyday_mail::session::{MailSession, RemoteMailbox, Role, Uid};

/// One selectable, synced mailbox: the vault's own row, and the name to
/// `SELECT` it by on the server.
#[derive(Debug, Clone)]
pub struct SyncedMailbox {
    pub row: Mailbox,
    pub remote_name: String,
}

fn to_mailbox_role(role: Role) -> MailboxRole {
    match role {
        Role::Inbox => MailboxRole::Inbox,
        Role::Sent => MailboxRole::Sent,
        Role::Drafts => MailboxRole::Drafts,
        Role::Archive => MailboxRole::Archive,
        Role::Trash => MailboxRole::Trash,
        Role::Spam => MailboxRole::Spam,
        Role::All => MailboxRole::All,
        Role::Other => MailboxRole::Other,
    }
}

/// The five roles Gmail's own folders are fetched for -- see the module
/// docs. `\Inbox` is deliberately absent: it is a label, read off messages
/// in All Mail, never a folder this crate selects.
const GMAIL_SYNCED_ROLES: [Role; 5] =
    [Role::All, Role::Sent, Role::Drafts, Role::Spam, Role::Trash];

/// `true` if `attrs` names a mailbox `SELECT` would refuse -- `\Noselect`,
/// a pure hierarchy node such as Gmail's own `[Gmail]`.
fn is_selectable(mailbox: &RemoteMailbox) -> bool {
    !mailbox.attributes.iter().any(|a| a.eq_ignore_ascii_case("\\Noselect"))
}

/// `LIST` `account`'s mailboxes and reconcile the vault's [`Mailbox`] rows
/// against them: a folder discovered for the first time gets a fresh row,
/// naming its cursors from a clean sheet; a folder the vault already knows
/// keeps its id and cursors exactly as they were, so an already-synced
/// mailbox's headers pass resumes rather than starting over because the
/// account happened to reconnect.
///
/// Returns the mailboxes to actually sync, inbox-role first -- see the
/// plan's "the inbox is prioritised first so it's usable within seconds" --
/// then every other mailbox in `LIST` order.
pub async fn discover<S: MailSession>(
    vault: &Arc<Vault>,
    account_id: AccountId,
    session: &mut S,
) -> everyday_mail::session::Result<Vec<SyncedMailbox>> {
    let remote = session.mailboxes().await?;
    let gmail = session.capabilities().gmail;

    let wanted: Vec<&RemoteMailbox> = remote
        .iter()
        .filter(|m| is_selectable(m))
        .filter(|m| match (gmail, m.special_use) {
            // Gmail: only the five folders `X-GM-LABELS` cannot express on
            // its own. A labelled mailbox with no special-use role at all
            // (a folder Gmail did not tag, which should not happen for an
            // account speaking `X-GM-EXT-1`, but a server is a server) is
            // left out rather than guessed at -- its messages still arrive
            // through All Mail.
            (true, Some(role)) => GMAIL_SYNCED_ROLES.contains(&role),
            (true, None) => false,
            // Every other server: every selectable folder.
            (false, _) => true,
        })
        .collect();

    let existing = vault.mailboxes(account_id).unwrap_or_default();
    let mut by_name: HashMap<String, Mailbox> =
        existing.into_iter().map(|m| (m.remote_name.clone(), m)).collect();

    let mut synced = Vec::with_capacity(wanted.len());
    for remote_mailbox in wanted {
        let role = to_mailbox_role(remote_mailbox.special_use.unwrap_or(Role::Other));
        let row = match by_name.remove(&remote_mailbox.name) {
            Some(existing) => existing,
            None => {
                let fresh = Mailbox::new(account_id, remote_mailbox.name.clone(), role);
                let _ = vault.save_mailbox(&fresh);
                fresh
            }
        };
        synced.push(SyncedMailbox { row, remote_name: remote_mailbox.name.clone() });
    }

    // Inbox (or, on Gmail, All Mail, which is what carries the inbox's own
    // messages) first, so the plan's "usable within seconds" promise is
    // about the mailbox a person actually opens first.
    synced.sort_by_key(|m| match m.row.role {
        MailboxRole::Inbox => 0,
        MailboxRole::All => 0,
        _ => 1,
    });

    Ok(synced)
}

/// Resolves a Gmail label to the [`Mailbox`] row that represents it, minting
/// one the first time this sync run sees the label -- or, for a label this
/// crate no longer tracks as a folder at all, resolving to nothing. Kept for
/// the life of one account-sync attempt -- a fresh instance per connection,
/// so a label deleted server-side between two syncs is not remembered as
/// though it still existed.
pub struct LabelMailboxes {
    account_id: AccountId,
    /// Label text (Gmail's own, `\Inbox` and all) to the row representing it.
    /// Never holds a row for a backslash system label other than `\Inbox`
    /// -- see [`LabelMailboxes::resolve`].
    by_label: HashMap<String, Mailbox>,
}

impl LabelMailboxes {
    /// Builds the label index for one sync attempt, first repairing any
    /// mailbox row a previous, buggy sync left in the wrong shape -- see
    /// [`repair_escaped_labels`].
    pub fn new(vault: &Vault, account_id: AccountId) -> Self {
        // Every label this account has ever had a mailbox row for -- built
        // from `remote_name`, which is where a label mailbox's own text
        // lives (see `Mailbox::remote_name`'s docs: "the name the server
        // gave it", and a label *is* the name Gmail gave it). Folder-backed
        // rows (All Mail, Sent, ...) are in this same list but are never
        // looked up by label text, so they cost nothing beyond a few extra
        // map entries that are never read.
        let by_label = repair_escaped_labels(vault, account_id)
            .into_iter()
            .map(|m| (m.remote_name.clone(), m))
            .collect();
        Self { account_id, by_label }
    }

    /// The [`Mailbox`] row for `label`, creating and saving one if this is
    /// the first time it has been seen this sync attempt -- or `None` for a
    /// Gmail system label other than `\Inbox` (`\Sent`, `\Draft`,
    /// `\Starred`, `\Important`, `\Spam`, `\Trash`, `\All`, and any other
    /// label with a leading backslash), which this crate does not mint a
    /// folder row for at all.
    ///
    /// # Why only `\Inbox` gets a row
    ///
    /// A user label is the only reason this type exists: Gmail has no
    /// folder for it, so the label itself is the only record of which
    /// messages carry it. Every *other* backslash label either already has
    /// a folder-backed row of its own (`\Sent`/`\Draft` -- Sent/Drafts;
    /// `\Spam`/`\Trash` -- Spam/Trash; `\All` -- All Mail; see
    /// [`discover`]'s own module docs) or is better represented another way
    /// entirely (`\Starred` is the `\Flagged` IMAP flag; `\Important` is a
    /// categorisation signal `everyday_core::mail::categorize` reads
    /// straight off `Message::labels`, never off a mailbox list). Minting a
    /// second, redundant mailbox for one of these does not just waste a
    /// row: it would show a person a folder that duplicates one they
    /// already have. `\Inbox` is the one exception, because it is the only
    /// label among these with no folder-backed row standing in for it
    /// already -- see [`discover`]'s own "Gmail folder rule".
    ///
    /// The caller still has the full, untouched label text for anything
    /// that needs it regardless of this: [`crate::mailsync::ingest::resolve_header`]
    /// and `crate::mailsync::passes::refresh_gmail_labels` both write every
    /// label straight into `Message::labels`, independently of whether this
    /// method minted a mailbox row for it.
    pub fn resolve(&mut self, vault: &Vault, label: &str) -> Option<Mailbox> {
        if let Some(existing) = self.by_label.get(label) {
            return Some(existing.clone());
        }
        if label != "\\Inbox" && label.starts_with('\\') {
            return None;
        }
        let role = if label == "\\Inbox" { MailboxRole::Inbox } else { MailboxRole::Other };
        let fresh = Mailbox::new(self.account_id, label, role);
        let _ = vault.save_mailbox(&fresh);
        self.by_label.insert(label.to_string(), fresh.clone());
        Some(fresh)
    }
}

/// Repair, once per sync attempt, every [`Mailbox`] row a previous sync left
/// in the wrong shape -- then return the account's mailboxes with every
/// repair already applied, for [`LabelMailboxes::new`] to index.
///
/// Two things can be wrong with an existing row, both traced to the same
/// root cause (see `everyday_mail::imap`'s module docs, "Gmail's
/// doubly-escaped system labels"): its `remote_name` can be double-escaped
/// (`\\Inbox` rather than `\Inbox`), and — independently, for any row whose
/// *corrected* name is a backslash system label — this crate may simply no
/// longer want a folder row for it at all (see
/// [`LabelMailboxes::resolve`]'s own docs for which, and why). Both are
/// fixed here, in place:
///
/// - **`\Inbox`**, once unescaped, is promoted to [`MailboxRole::Inbox`] and
///   renamed in place, *keeping its id* — exactly like a plain rename, so
///   every `message_mailboxes` row naming it stays valid and a person's
///   inbox does not appear to empty out while history catches up.
/// - **Any other backslash label** (`\Sent`, `\Draft`, `\Starred`, ...) is
///   deleted outright, via [`Vault::delete_mailbox`]. This is safe without
///   this function marking anything dead in the pack store or search index
///   itself: every message such a row ever had a membership for is, by
///   construction (see `crate::mailsync::passes::sync_headers`'s own
///   per-label ingest loop), *also* a member of the account's All Mail
///   mailbox, so `delete_mailbox`'s "a message with no mailbox left at all
///   is deleted" clause never actually fires here — nothing but the one
///   redundant membership is lost.
/// - **A user label** whose own text needed unescaping (a literal quote or
///   backslash in its name) is renamed in place, on the same terms as
///   `\Inbox`.
///
/// If a row already carries the correct, final text for its own kind, it
/// is left completely untouched -- including not re-saved -- which is what
/// makes a second call on an already-repaired account free beyond the one
/// `mailboxes` read and a per-row string scan that finds nothing to fix.
///
/// # Only Gmail's label rows
///
/// This runs before the account's sync has connected, so it cannot ask the
/// server whether it speaks `X-GM-EXT-1`. It asks the vault instead: only a
/// Gmail account ever has a [`MailboxRole::All`] row (All Mail is one of
/// the five folders [`discover`] keeps there, and `SPECIAL-USE \All`
/// anywhere else is mapped the same way but never fetched alongside
/// labels), so an account without one is returned untouched. And within a
/// Gmail account only label-backed rows -- role `Other`, or `Inbox` -- are
/// looked at: All Mail, Sent, Drafts, Spam and Trash are real folders whose
/// `remote_name` is what `SELECT` sends, and renaming or deleting one of
/// those would break the sync rather than repair it.
///
/// # Two rows that correct to the same name
///
/// Rare, and only possible for a handful of system-label rows per account:
/// an already-correctly-named row and an escaped duplicate of it both
/// exist (perhaps because a label's flag-form and quoted-form spellings
/// were both seen, on different syncs, before this fix). The
/// already-correct row always wins and is left alone; the duplicate is
/// deleted, on the same safe terms as the system-label case above, rather
/// than merged into it -- this crate has no "move these memberships to
/// another mailbox id" primitive to merge with.
fn repair_escaped_labels(vault: &Vault, account_id: AccountId) -> Vec<Mailbox> {
    let existing = vault.mailboxes(account_id).unwrap_or_default();
    if !existing.iter().any(|m| m.role == MailboxRole::All) {
        return existing;
    }
    let mut kept: HashMap<String, Mailbox> = HashMap::with_capacity(existing.len());
    let mut needs_repair: Vec<(Mailbox, String)> = Vec::new();
    let mut to_delete: Vec<MailboxId> = Vec::new();

    // First pass: every row already in its final shape is authoritative,
    // regardless of what else this account's history also left behind.
    for row in existing {
        if !matches!(row.role, MailboxRole::Other | MailboxRole::Inbox) {
            kept.insert(row.remote_name.clone(), row);
            continue;
        }
        let corrected = unescape_imap_quoted(&row.remote_name);
        let keep_as_folder = corrected == "\\Inbox" || !corrected.starts_with('\\');
        if !keep_as_folder {
            to_delete.push(row.id);
        } else if corrected == row.remote_name {
            kept.entry(corrected).or_insert(row);
        } else {
            needs_repair.push((row, corrected));
        }
    }

    // Second pass: repair what is left, unless an authoritative row for the
    // same corrected name already won the first pass, in which case this
    // one is a stale duplicate of it.
    for (mut row, corrected) in needs_repair {
        if kept.contains_key(&corrected) {
            to_delete.push(row.id);
            continue;
        }
        row.remote_name = corrected.clone();
        if corrected == "\\Inbox" {
            row.role = MailboxRole::Inbox;
        }
        let _ = vault.save_mailbox(&row);
        kept.insert(corrected, row);
    }

    for id in to_delete {
        let _ = vault.delete_mailbox(id);
    }

    kept.into_values().collect()
}

/// A batch of UIDs, chunked for `headers`/`raw`, newest first -- what every
/// pass in [`crate::mailsync::passes`] iterates.
pub fn newest_first_chunks(mut uids: Vec<Uid>, size: usize) -> Vec<Vec<Uid>> {
    uids.sort_unstable_by(|a, b| b.cmp(a));
    uids.chunks(size).map(<[Uid]>::to_vec).collect()
}
