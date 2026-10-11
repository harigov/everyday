//! Chat links: the commands behind Settings → Chat.
//!
//! A link is one bot account on a chat service, through which its owner talks
//! to the assistant from another device -- see `everyday_core::chat` for what
//! one is and [`crate::chat`] for the task each one runs as. These four
//! commands are the whole of how a person manages them: list, mint a blank,
//! save, remove. They sit in the `Agent` scope beside `save_agent_settings`,
//! because letting somebody reach the assistant from a phone is the same
//! decision as letting it exist, and a client that may not change the
//! assistant's settings has no business adding a way in.
//!
//! # What a client is told, and what it is not
//!
//! Never the sign-in itself. A link's password, its session and its recovery
//! key live in the secret store ([`ChatSecret`]) and stay there; the
//! interface sees only whether each exists ([`ChatLinkView::signed_in`],
//! [`ChatLinkView::has_password`]), the same way it is told whether an
//! assistant key is set without being handed the key. A password goes the
//! other way once, in `save_chat_link`, and is not echoed back.
//!
//! # Every save restarts the link
//!
//! The running task holds the settings it was started with -- and, on Matrix,
//! the open store of the account it signed in as. Rather than teach it to
//! notice a change, a save ends it and starts another, which is also the only
//! way a different account can be safe: its files cannot be deleted from
//! under a task that still has them open, so the task is stopped first and
//! only then is anything wiped.

use super::Nothing;
use crate::chat::{ChatStatus, reconcile, restart_link, state_dir, task_key};
use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::service::{Service, blocking};
use everyday_core::chat::{ChatLink, ChatSecret};
use everyday_core::{ChatLinkId, Error, Vault};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

// ---- wire shapes ----------------------------------------------------------

/// One link as the settings pane draws it: the link itself, where it stands,
/// and what it has to sign in with -- as yes or no.
///
/// Mirrors the TS `ChatLinkView`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatLinkView {
    pub link: ChatLink,
    pub status: ChatStatus,
    /// Whether the service has accepted the bot's sign-in and given it a
    /// session. False for a link that has never connected, and for one whose
    /// account just changed.
    pub signed_in: bool,
    /// Whether a password is stored waiting to be used. It is dropped once a
    /// session exists, so this is true for a new link and false after.
    pub has_password: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveChatLink {
    pub link: ChatLink,
    /// Given when the bot's password is being set or changed, left out
    /// otherwise. A blank one is the same as none, so a form that sends an
    /// empty box does not wipe a stored password.
    #[serde(default)]
    pub password: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatLinkRef {
    pub id: ChatLinkId,
}

// ---- helpers --------------------------------------------------------------

/// Every link with its secret, in the order they were saved.
///
/// Blocking: each secret is a sealed read of its own.
fn read_links(vault: &Vault) -> CommandResult<Vec<(ChatLink, ChatSecret)>> {
    let mut out = Vec::new();
    for link in vault.chat_settings()?.links {
        let secret = vault.chat_secret(link.id)?;
        out.push((link, secret));
    }
    Ok(out)
}

/// `link` as the pane draws it. `writable` is whether this process holds the
/// vault's write claim, which decides what "not running" means -- see
/// [`Service::resolved_chat_status`].
fn view(svc: &Service, link: ChatLink, secret: &ChatSecret, writable: bool) -> ChatLinkView {
    let status = svc.resolved_chat_status(&link, writable);
    ChatLinkView {
        status,
        signed_in: secret.session.is_some(),
        has_password: secret.password.as_deref().is_some_and(|p| !p.is_empty()),
        link,
    }
}

/// Delete a link's local files, and say nothing if there were none -- a link
/// that never connected has not made any.
fn remove_state_dir(dir: &Path) -> CommandResult<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(dir, e).into()),
    }
}

/// The blocking half of `save_chat_link`: write the link, and the part of its
/// secret that changed. Returns both as they now stand.
///
/// `stored` is what was saved under this id before, if anything, and
/// `account_changed` whether the new one is a different account -- both
/// worked out by the caller, which had to stop the old account's task before
/// this could delete anything.
///
/// The order is deliberate. Whatever is dropped goes first and the link last:
/// if something fails halfway, the worst left behind is a link that has to
/// sign in again, never a new account holding the old one's session.
fn save_link(
    vault: &Vault,
    args: SaveChatLink,
    stored: Option<&ChatLink>,
    account_changed: bool,
    now: Timestamp,
) -> CommandResult<(ChatLink, ChatSecret)> {
    let SaveChatLink { mut link, password } = args;
    // Stamped here rather than trusted from the client: when it was made is
    // the vault's to say, and when it last changed is now.
    link.created_at = stored.map_or(now, |old| old.created_at);
    link.updated_at = now;

    let mut settings = vault.chat_settings()?;
    match settings.links.iter_mut().find(|l| l.id == link.id) {
        Some(slot) => *slot = link.clone(),
        None => settings.links.push(link.clone()),
    }
    settings.validate()?;

    let mut secret = vault.chat_secret(link.id)?;
    let mut secret_changed = false;
    if account_changed {
        // The bot is now a different account: its session, and the key that
        // would make it a verified device of the old one, mean nothing, and
        // its local store is full of the old account's encryption keys.
        secret.session = None;
        secret.recovery_key = None;
        secret_changed = true;
        remove_state_dir(&state_dir(vault, link.id))?;
    }
    if let Some(password) = password.filter(|p| !p.trim().is_empty()) {
        secret.password = Some(password);
        secret_changed = true;
    }
    if secret_changed {
        vault.save_chat_secret(link.id, &secret)?;
    }
    vault.save_chat_settings(&settings)?;
    Ok((link, secret))
}

/// The blocking half of `remove_chat_link`: the link, its secret and its
/// files, in that order, so a failure halfway leaves garbage nobody can see
/// rather than a link with no way to sign in.
fn remove_link(vault: &Vault, id: ChatLinkId) -> CommandResult<()> {
    let mut settings = vault.chat_settings()?;
    let before = settings.links.len();
    settings.links.retain(|l| l.id != id);
    if settings.links.len() == before {
        return Err(Error::not_found("chat link", id).into());
    }
    vault.save_chat_settings(&settings)?;
    vault.delete_chat_secret(id)?;
    remove_state_dir(&state_dir(vault, id))
}

// ---- commands ---------------------------------------------------------

/// Every link, with where it stands.
async fn chat_links(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<ChatLinkView>> {
    let vault = svc.require()?;
    let writable = vault.is_writable();
    let links = blocking(move || read_links(&vault)).await?;
    Ok(links.into_iter().map(|(link, secret)| view(&svc, link, &secret, writable)).collect())
}

/// A blank Matrix link, switched on, without saving it -- the id is the
/// core's to mint, for the reason `new_routine` and `new_note` already give.
async fn new_chat_link(_svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<ChatLink> {
    Ok(ChatLink::new_matrix())
}

/// Add a link or change one, and have it take effect now.
async fn save_chat_link(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SaveChatLink,
) -> CommandResult<ChatLinkView> {
    let vault = svc.require()?;
    let id = args.link.id;
    // Before anything is stopped or touched: a typo in the form must not
    // take down a bot that was working.
    args.link.validate()?;
    let now = svc.now();

    let stored = {
        let vault = vault.clone();
        blocking(move || Ok(vault.chat_settings()?.link(id).cloned())).await?
    };
    let account_changed = stored.as_ref().is_some_and(|old| !old.same_account(&args.link));
    if account_changed {
        // The old account's task has its store open; it must be gone before
        // `save_link` deletes the files.
        svc.supervisor().stop(&task_key(id)).await;
    }

    let writable = vault.is_writable();
    let (link, secret) = {
        let vault = vault.clone();
        blocking(move || save_link(&vault, args, stored.as_ref(), account_changed, now)).await?
    };
    restart_link(&svc, &vault, id).await;
    Ok(view(&svc, link, &secret, writable))
}

/// Stop a link, and forget it and everything it signed in with.
async fn remove_chat_link(svc: Arc<Service>, _ctx: Ctx, args: ChatLinkRef) -> CommandResult<()> {
    let vault = svc.require()?;
    let id = args.id;
    // First, so nothing is signing in while its credentials are deleted, and
    // nothing has the store open while its directory is.
    svc.supervisor().stop(&task_key(id)).await;
    {
        let vault = vault.clone();
        blocking(move || remove_link(&vault, id)).await?;
    }
    svc.forget_chat_status(id);
    // Not for this link, which is gone, but for the rest: whether any is left
    // on is what decides if the vault still keeps its key past the idle timer.
    reconcile(&svc, &vault);
    Ok(())
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "chat_links", scope: Agent, effect: Read,
        args: Nothing, returns: "ChatLinkView[]", signature: &[],
        run: chat_links,
    },
    command! {
        name: "new_chat_link", scope: Agent, effect: Read,
        args: Nothing, returns: "ChatLink", signature: &[],
        run: new_chat_link,
    },
    command! {
        name: "save_chat_link", scope: Agent, effect: Write,
        change: Settings / Updated,
        args: SaveChatLink, returns: "ChatLinkView",
        // `password` is `Option<String>` and `#[serde(default)]`: leaving it
        // out of the object is the same as `null`, so it is not required.
        signature: &[("link", "ChatLink", true), ("password", "string | null", false)],
        run: save_chat_link,
    },
    command! {
        name: "remove_chat_link", scope: Agent, effect: Destructive,
        change: Settings / Updated,
        args: ChatLinkRef, returns: "void",
        signature: &[("id", "ChatLinkId", true)],
        run: remove_chat_link,
    },
];
