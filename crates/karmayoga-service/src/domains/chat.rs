//! Chat links: the commands behind Settings → Chat.
//!
//! A link is one bot account on a chat service, through which its owner talks
//! to the assistant from another device -- see `karmayoga_core::chat` for what
//! one is and [`crate::chat`] for the task each one runs as. Four commands
//! are the whole of how a person manages them: list, mint a blank, save,
//! remove. Two more answer for a running bot when its owner verifies it,
//! since the bot has no screen to compare emoji on. Two more are about which
//! of the owner's devices the bot trusts: one lists them as the running bot
//! sees them, one saves the hand-picked list. All eight sit in the
//! `Agent` scope beside `save_agent_settings`, because letting somebody
//! reach the assistant from a phone is the same decision as letting it
//! exist, and a client that may not change the assistant's settings has no
//! business adding a way in.
//!
//! # The list of trusted devices is this computer's alone
//!
//! The bot believes an encrypted message only from a device on that list, so
//! whoever can add a device to it can talk to the assistant and read its
//! replies. `set_chat_trusted_devices` therefore refuses every caller but the
//! desktop's own window ([`require_local`]), and `save_chat_link` -- which any
//! client with the `Agent` scope may call -- never takes the list from the link
//! it is given: it keeps the stored one ([`ChatLink::inherit_trusted_devices`]).
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
use crate::chat::{
    ChatDevice, ChatStatus, ChatUntrustedDevice, ChatVerification, ChatVerifier,
    VerificationAnswer, reconcile, restart_link, state_dir, task_key,
};
use crate::command;
use crate::ctx::{Caller, Ctx};
use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};
use jiff::Timestamp;
use karmayoga_core::chat::{ChatLink, ChatSecret, ChatService, tidy_device_ids};
use karmayoga_core::{ChatLinkId, Error, Vault};
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
    /// Whether the bot has verified its owner's encryption identity. `None`
    /// when it cannot say -- it is not running, or has not seen the owner's
    /// keys yet.
    pub owner_verified: Option<bool>,
    /// The verification the bot is in, or was last in this run. Settings
    /// shows its emoji and takes the answer.
    pub verification: Option<ChatVerification>,
    /// The owner's device that last wrote to the bot without being on the
    /// trusted list, and was ignored. `None` when none has, or since it was
    /// trusted.
    pub untrusted_device: Option<ChatUntrustedDevice>,
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetChatTrustedDevices {
    pub id: ChatLinkId,
    /// Every device id of the owner's the bot should trust, replacing what
    /// was trusted before. Any other device of theirs is not.
    pub devices: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnswerChatVerification {
    pub id: ChatLinkId,
    /// The verification being answered -- `ChatVerification::flow` -- so an
    /// answer meant for one cannot land on a newer one.
    pub flow: String,
    pub answer: VerificationAnswer,
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
    let trust = svc.chat_trust(link.id);
    ChatLinkView {
        status,
        signed_in: secret.session.is_some(),
        has_password: secret.password.as_deref().is_some_and(|p| !p.is_empty()),
        owner_verified: trust.owner_verified,
        verification: trust.verification,
        untrusted_device: svc.chat_untrusted_device(link.id),
        link,
    }
}

/// Why a caller that is not the desktop itself may not choose which devices
/// the bot trusts.
const TRUST_IS_LOCAL: &str = "which devices the assistant trusts can be chosen only from this \
    computer's own window: a device that could add itself to the list would make the list worth \
    nothing";

/// Whether this caller is the desktop's own window.
fn is_local(ctx: &Ctx) -> bool {
    matches!(ctx.caller, Caller::Local | Caller::Window(_))
}

/// Refuse what only the desktop itself may do.
fn require_local(ctx: &Ctx) -> CommandResult<()> {
    if is_local(ctx) { Ok(()) } else { Err(CommandError::new(codes::FORBIDDEN, TRUST_IS_LOCAL)) }
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
    // Nor is the list of trusted devices the client's to say. Any client
    // allowed to save a link could otherwise add its own device to it.
    link.inherit_trusted_devices(stored);

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

/// The blocking half of `set_chat_trusted_devices`: write the list into the
/// link, and return the link as it now stands with its secret.
fn save_trusted_devices(
    vault: &Vault,
    id: ChatLinkId,
    devices: Vec<String>,
    now: Timestamp,
) -> CommandResult<(ChatLink, ChatSecret)> {
    let mut settings = vault.chat_settings()?;
    let link = settings
        .links
        .iter_mut()
        .find(|l| l.id == id)
        .ok_or_else(|| CommandError::from(Error::not_found("chat link", id)))?;
    let ChatService::Matrix(matrix) = &mut link.service;
    matrix.trusted_devices = devices;
    link.updated_at = now;
    let link = link.clone();
    settings.validate()?;
    vault.save_chat_settings(&settings)?;
    Ok((link, vault.chat_secret(id)?))
}

/// The ids of the devices a link trusts, as the saved link names them.
fn trusted_of(link: &ChatLink) -> &[String] {
    let ChatService::Matrix(matrix) = &link.service;
    &matrix.trusted_devices
}

/// The verifier of a link that is running here, or why there is none.
fn verifier(svc: &Service, id: ChatLinkId) -> CommandResult<Arc<dyn ChatVerifier>> {
    svc.chat_verifier(id).ok_or_else(|| {
        CommandError::new(
            codes::CONFLICT,
            "The bot is not running here, so it cannot verify anybody. Switch it on and wait \
             until it is online.",
        )
    })
}

/// Answer the emoji comparison the bot is in: they match, they do not, or
/// stop.
async fn answer_chat_verification(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: AnswerChatVerification,
) -> CommandResult<()> {
    verifier(&svc, args.id)?.answer(args.flow, args.answer).await
}

/// Have the bot ask its owner to verify it.
async fn start_chat_verification(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: ChatLinkRef,
) -> CommandResult<()> {
    verifier(&svc, args.id)?.start().await
}

/// The owner's devices as the running bot sees them, each with whether it is
/// trusted.
async fn chat_devices(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: ChatLinkRef,
) -> CommandResult<Vec<ChatDevice>> {
    let Some(devices) = svc.chat_devices(args.id) else {
        return Err(CommandError::new(
            codes::CONFLICT,
            "The bot is not running here, so it cannot list your devices. Switch it on and wait \
             until it is online.",
        ));
    };
    devices.list().await
}

/// Choose, by hand, which of the owner's devices the bot trusts; the bot
/// changes at once.
///
/// This computer's own window only ([`require_local`]). Saved first, so the
/// choice is kept whether or not the bot is running: one that is not picks it
/// up when it starts, and one that is has the new list applied without
/// restarting. If applying it fails -- the encryption store would not take it
/// -- the bot is restarted, which applies the saved list from scratch.
async fn set_chat_trusted_devices(
    svc: Arc<Service>,
    ctx: Ctx,
    args: SetChatTrustedDevices,
) -> CommandResult<ChatLinkView> {
    require_local(&ctx)?;
    let vault = svc.require()?;
    let id = args.id;
    let devices = tidy_device_ids(args.devices);
    let now = svc.now();
    let writable = vault.is_writable();
    let (link, secret) = {
        let vault = vault.clone();
        let devices = devices.clone();
        blocking(move || save_trusted_devices(&vault, id, devices, now)).await?
    };
    svc.settle_untrusted_chat_device(id, trusted_of(&link));
    if let Some(running) = svc.chat_devices(id)
        && let Err(e) = running.trust(devices).await
    {
        tracing::warn!(
            error = %e.message,
            "the running bot could not take the new device list; restarting it"
        );
        restart_link(&svc, &vault, id).await;
    }
    Ok(view(&svc, link, &secret, writable))
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
    // Neither writes to the vault: what they change is the bot's standing
    // with its owner, on the chat server. Writes all the same, because each
    // vouches for somebody's keys -- not a thing a read-only client may do.
    command! {
        name: "answer_chat_verification", scope: Agent, effect: Write,
        args: AnswerChatVerification, returns: "void",
        signature: &[
            ("id", "ChatLinkId", true),
            ("flow", "string", true),
            ("answer", "VerificationAnswer", true),
        ],
        run: answer_chat_verification,
    },
    command! {
        name: "start_chat_verification", scope: Agent, effect: Write,
        args: ChatLinkRef, returns: "void",
        signature: &[("id", "ChatLinkId", true)],
        run: start_chat_verification,
    },
    command! {
        name: "chat_devices", scope: Agent, effect: Read,
        args: ChatLinkRef, returns: "ChatDevice[]",
        signature: &[("id", "ChatLinkId", true)],
        run: chat_devices,
    },
    // Writes the vault (the link's list), and says so; refuses every caller
    // but the desktop's own window. See [`require_local`].
    command! {
        name: "set_chat_trusted_devices", scope: Agent, effect: Write,
        change: Settings / Updated,
        args: SetChatTrustedDevices, returns: "ChatLinkView",
        signature: &[("id", "ChatLinkId", true), ("devices", "string[]", true)],
        run: set_chat_trusted_devices,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_desktops_own_window_may_choose_the_trusted_devices() {
        for (caller, local) in [
            (Caller::Local, true),
            (Caller::Window("window:app-chat".into()), true),
            (Caller::Socket, false),
            (Caller::Device("phone".into()), false),
            (Caller::Assistant("run-1".into()), false),
        ] {
            let ctx = Ctx { caller: caller.clone(), ..Ctx::local() };
            assert_eq!(is_local(&ctx), local, "{caller:?}");
            let refused = require_local(&ctx);
            assert_eq!(refused.is_ok(), local, "{caller:?}");
            if let Err(e) = refused {
                assert_eq!(e.code, codes::FORBIDDEN);
            }
        }
    }

    /// A service around a vault in a temporary directory, with one link saved
    /// by the desktop for an owner who has not picked a device yet.
    async fn a_service_with_a_link() -> (Arc<Service>, tempfile::TempDir, ChatLinkId) {
        let dir = tempfile::tempdir().unwrap();
        let config = karmayoga_core::VaultConfig {
            name: "Test".into(),
            backend: "sqlite".into(),
            settings: Default::default(),
            password: None,
            kdf: karmayoga_core::crypto::KdfParams::insecure_fast(),
            auto_lock_seconds: 900,
            forget_key_seconds: 0,
        };
        let vault = karmayoga_vault::create(dir.path(), config).unwrap();
        let svc = Arc::new(Service::new());
        svc.set(vault);
        let mut link = ChatLink::new_matrix();
        let ChatService::Matrix(matrix) = &mut link.service;
        matrix.homeserver = "http://127.0.0.1:6167".into();
        matrix.user_id = "@karmayoga:example.org".into();
        matrix.owner = "@owner:example.org".into();
        let id = link.id;
        let saving = serde_json::json!({ "link": link, "password": "x" });
        svc.call(Ctx::local(), "save_chat_link", saving).await.unwrap();
        (svc, dir, id)
    }

    fn stored_devices(svc: &Service, id: ChatLinkId) -> Vec<String> {
        let link = svc.get().unwrap().chat_settings().unwrap().link(id).cloned().unwrap();
        trusted_of(&link).to_vec()
    }

    #[tokio::test]
    async fn the_desktop_picks_the_trusted_devices_and_nobody_else_can() {
        let (svc, _dir, id) = a_service_with_a_link().await;
        let pick = |devices: &[&str]| serde_json::json!({ "id": id, "devices": devices });

        // A paired device, the local socket and the assistant itself are refused,
        // and the list is as it was.
        for caller in
            [Caller::Device("phone".into()), Caller::Socket, Caller::Assistant("run-1".into())]
        {
            let ctx = Ctx { caller: caller.clone(), ..Ctx::local() };
            let e = svc
                .call(ctx, "set_chat_trusted_devices", pick(&["INTRUDER"]))
                .await
                .expect_err("a caller that is not the desktop picked a device");
            assert_eq!(e.code, codes::FORBIDDEN, "{caller:?}");
            assert!(stored_devices(&svc, id).is_empty(), "{caller:?}");
        }

        // The desktop's own window can, from either of its callers. Saved
        // tidy, and with the bot not running that is all it does.
        let untidy = pick(&[" PHONE ", "LAPTOP", "PHONE", ""]);
        svc.call(Ctx::local(), "set_chat_trusted_devices", untidy).await.unwrap();
        assert_eq!(stored_devices(&svc, id), ["LAPTOP", "PHONE"]);
        let view = svc
            .call(Ctx::window("app-chat"), "set_chat_trusted_devices", pick(&["PHONE"]))
            .await
            .unwrap();
        assert_eq!(view["link"]["service"]["trustedDevices"], serde_json::json!(["PHONE"]));
    }

    #[tokio::test]
    async fn saving_a_link_never_takes_the_list_from_the_client() {
        let (svc, _dir, id) = a_service_with_a_link().await;
        svc.call(
            Ctx::local(),
            "set_chat_trusted_devices",
            serde_json::json!({ "id": id, "devices": ["PHONE"] }),
        )
        .await
        .unwrap();

        // A client that may save a link -- but not pick devices -- sends one
        // with a device of its own, as a form might send the whole link.
        let mut link = svc.get().unwrap().chat_settings().unwrap().link(id).cloned().unwrap();
        let ChatService::Matrix(matrix) = &mut link.service;
        matrix.trusted_devices = vec!["PHONE".into(), "INTRUDER".into()];
        let sneaky = Ctx { caller: Caller::Device("phone".into()), ..Ctx::local() };
        svc.call(sneaky.clone(), "save_chat_link", serde_json::json!({ "link": link }))
            .await
            .unwrap();
        assert_eq!(stored_devices(&svc, id), ["PHONE"]);

        // Pointing the link at somebody else's account drops the list: those
        // device ids were the old owner's.
        let ChatService::Matrix(matrix) = &mut link.service;
        matrix.owner = "@thief:example.org".into();
        matrix.trusted_devices = vec!["INTRUDER".into()];
        svc.call(sneaky, "save_chat_link", serde_json::json!({ "link": link })).await.unwrap();
        assert!(stored_devices(&svc, id).is_empty());
    }

    #[tokio::test]
    async fn the_devices_cannot_be_listed_while_the_bot_is_not_running() {
        let (svc, _dir, id) = a_service_with_a_link().await;
        let e = svc
            .call(Ctx::local(), "chat_devices", serde_json::json!({ "id": id }))
            .await
            .expect_err("nothing is running");
        assert_eq!(e.code, codes::CONFLICT);
        assert!(e.message.contains("not running"), "{}", e.message);
    }

    #[test]
    fn a_list_is_read_back_from_the_link_that_holds_it() {
        let mut link = ChatLink::new_matrix();
        assert!(trusted_of(&link).is_empty());
        let ChatService::Matrix(matrix) = &mut link.service;
        matrix.trusted_devices = vec!["PHONE".into()];
        assert_eq!(trusted_of(&link), ["PHONE"]);
    }
}
