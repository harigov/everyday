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
use everyday_core::id::AccountId;
use everyday_core::mail::{Mailbox, MailboxRole};
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
/// one the first time this sync run sees the label. Kept for the life of one
/// account-sync attempt -- a fresh instance per connection, so a label
/// deleted server-side between two syncs is not remembered as though it
/// still existed.
pub struct LabelMailboxes {
    account_id: AccountId,
    /// Label text (Gmail's own, `\Inbox` and all) to the row representing it.
    by_label: HashMap<String, Mailbox>,
}

impl LabelMailboxes {
    pub fn new(vault: &Vault, account_id: AccountId) -> Self {
        // Every label this account has ever had a mailbox row for -- built
        // from `remote_name`, which is where a label mailbox's own text
        // lives (see `Mailbox::remote_name`'s docs: "the name the server
        // gave it", and a label *is* the name Gmail gave it). Folder-backed
        // rows (All Mail, Sent, ...) are in this same list but are never
        // looked up by label text, so they cost nothing beyond a few extra
        // map entries that are never read.
        let by_label = vault
            .mailboxes(account_id)
            .unwrap_or_default()
            .into_iter()
            .map(|m| (m.remote_name.clone(), m))
            .collect();
        Self { account_id, by_label }
    }

    /// The [`Mailbox`] row for `label`, creating and saving one if this is
    /// the first time it has been seen.
    pub fn resolve(&mut self, vault: &Vault, label: &str) -> Mailbox {
        if let Some(existing) = self.by_label.get(label) {
            return existing.clone();
        }
        let role = if label == "\\Inbox" { MailboxRole::Inbox } else { MailboxRole::Other };
        let fresh = Mailbox::new(self.account_id, label, role);
        let _ = vault.save_mailbox(&fresh);
        self.by_label.insert(label.to_string(), fresh.clone());
        fresh
    }
}

/// A batch of UIDs, chunked for `headers`/`raw`, newest first -- what every
/// pass in [`crate::mailsync::passes`] iterates.
pub fn newest_first_chunks(mut uids: Vec<Uid>, size: usize) -> Vec<Vec<Uid>> {
    uids.sort_unstable_by(|a, b| b.cmp(a));
    uids.chunks(size).map(<[Uid]>::to_vec).collect()
}
