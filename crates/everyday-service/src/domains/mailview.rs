//! The command surface over `crate::mailview`'s remote-image permissions.
//!
//! Three commands only. Rendering a body, reading a part and fetching a
//! remote image are *not* here -- they are bytes served over a
//! byte-serving route (`everyday-app/src/protocol.rs`'s `everyday://`
//! scheme, and `everyday-server`'s equivalent), on exactly the terms
//! [`crate::service::Service::blob_len`] and
//! [`crate::service::Service::blob_range`] already are, never a JSON
//! result through [`crate::service::Service::call`]. What belongs in the
//! ordinary command table is the settings surface a person's "show images"
//! click drives: the standing allow-list, and the in-memory one-off grant.

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::mailview;
use crate::service::{Service, blocking};
use everyday_core::id::MailMessageId;
use everyday_core::mail::RemoteImageSettings;
use serde::Deserialize;
use std::sync::Arc;

/// Names exactly one of a sender, a domain, or a message -- the plan's own
/// shape, `allow_remote_images {sender? | domain? | message_id}`. A sender
/// or a domain is added to the standing, sealed allow-list; a message id is
/// a one-off grant kept in memory for this session only -- see
/// [`Service::allow_remote_images_once`].
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AllowRemoteImages {
    #[serde(default)]
    pub sender: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub message_id: Option<MailMessageId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevokeRemoteImageAllowance {
    #[serde(default)]
    pub sender: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
}

async fn allow_remote_images(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: AllowRemoteImages,
) -> CommandResult<()> {
    let named = [args.sender.is_some(), args.domain.is_some(), args.message_id.is_some()]
        .into_iter()
        .filter(|set| *set)
        .count();
    if named != 1 {
        return Err(CommandError::new(
            codes::INVALID,
            "name exactly one of sender, domain or messageId",
        ));
    }
    if let Some(message_id) = args.message_id {
        svc.allow_remote_images_once(message_id);
        return Ok(());
    }
    let vault = svc.require()?;
    blocking(move || {
        mailview::allow_remote_images(&vault, args.sender.as_deref(), args.domain.as_deref())
    })
    .await
}

async fn list_remote_image_allowances(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: super::Nothing,
) -> CommandResult<RemoteImageSettings> {
    let vault = svc.require()?;
    blocking(move || mailview::list_remote_image_allowances(&vault)).await
}

async fn revoke_remote_image_allowance(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: RevokeRemoteImageAllowance,
) -> CommandResult<()> {
    if args.sender.is_none() && args.domain.is_none() {
        return Err(CommandError::new(codes::INVALID, "name a sender or a domain to revoke"));
    }
    let vault = svc.require()?;
    blocking(move || {
        mailview::revoke_remote_image_allowance(
            &vault,
            args.sender.as_deref(),
            args.domain.as_deref(),
        )
    })
    .await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "allow_remote_images", scope: Mail, effect: Write,
        // The vault's own settings, on `Kind::Settings`' own reasoning --
        // the same coarse kind `save_agent_settings` announces under. Fires
        // even for a one-off grant, which touches no vault row at all: a
        // client that reloads settings it happens not to have changed loses
        // nothing, and a `change:` that only sometimes fired would be a
        // second thing about this command to keep straight.
        change: Settings / Updated,
        args: AllowRemoteImages, returns: "void",
        signature: &[
            ("sender", "string | null", false),
            ("domain", "string | null", false),
            ("messageId", "MailMessageId | null", false),
        ],
        run: allow_remote_images,
    },
    command! {
        name: "list_remote_image_allowances", scope: Mail, effect: Read,
        args: super::Nothing, returns: "RemoteImageSettings", signature: &[],
        run: list_remote_image_allowances,
    },
    command! {
        name: "revoke_remote_image_allowance", scope: Mail, effect: Write,
        change: Settings / Updated,
        args: RevokeRemoteImageAllowance, returns: "void",
        signature: &[
            ("sender", "string | null", false),
            ("domain", "string | null", false),
        ],
        run: revoke_remote_image_allowance,
    },
];
