//! Which model each size of work runs on: Settings → Models.
//!
//! Application settings rather than the assistant's -- see
//! [`everyday_core::llm`] for the three tiers and why a key belongs to an
//! endpoint. Four commands: the settings, a save, and the two halves of a
//! key. There is no command that reads a key back.

use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::events::{Kind, Op};
use crate::service::{Service, blocking};
use everyday_core::Vault;
use everyday_core::llm::{ModelSettings, ModelTier};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::Nothing;

/// What every command here answers: the settings, and what the pane needs to
/// know about them that the record itself does not say. Mirrors the TS
/// `ModelSettingsView`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSettingsView {
    pub settings: ModelSettings,
    /// The endpoints, of those the settings name, that have a key stored.
    /// A tier's key is the one for its endpoint, so this is what draws "a
    /// key is stored" beside each of them -- and the key itself never leaves
    /// the vault.
    pub keyed: Vec<String>,
    /// [`ModelSettings::acknowledgement_name`]: what an account's mail
    /// acknowledgement must say for its mail to reach these models. Sent
    /// rather than worked out again in the interface, so the string a tick
    /// stores and the string the gate compares are made in one place.
    pub acknowledgement: String,
}

fn view(vault: &Vault) -> everyday_core::Result<ModelSettingsView> {
    let (settings, keyed) = vault.model_settings_and_keyed()?;
    let acknowledgement = settings.acknowledgement_name();
    Ok(ModelSettingsView { settings, keyed, acknowledgement })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveModels {
    pub settings: ModelSettings,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetModelKey {
    pub tier: ModelTier,
    pub key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TierRef {
    pub tier: ModelTier,
}

async fn model_settings(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<ModelSettingsView> {
    svc.on_vault(view).await
}

async fn save_model_settings(
    svc: Arc<Service>,
    ctx: Ctx,
    args: SaveModels,
) -> CommandResult<ModelSettingsView> {
    let vault = svc.require()?;
    let saved = blocking(move || {
        // A change of endpoint *visibly* clears mail access, not merely
        // disables it: `Vault::save_model_settings` clears every account's
        // acknowledgement in the same save, so the ticked checkbox in
        // Settings → Accounts never goes on naming endpoints that are no
        // longer the ones configured.
        vault.save_model_settings(&args.settings)?;
        Ok(view(&vault)?)
    })
    .await?;

    // One batch `Change` for every account that lost its acknowledgement --
    // the collector saw the vault's own writes, in order.
    let origin = ctx.caller.origin().map(str::to_string);
    let touched = crate::touched::current();
    crate::events::emit_touched(
        svc.events().as_ref(),
        origin,
        &touched,
        Kind::Account,
        Op::Updated,
    );
    Ok(saved)
}

/// Store the key for the endpoint `tier` points at -- by tier, so where a
/// key goes is decided by the saved settings rather than by the caller.
async fn set_model_key(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SetModelKey,
) -> CommandResult<ModelSettingsView> {
    svc.on_vault(move |vault| {
        vault.set_model_key(args.tier, &args.key)?;
        view(vault)
    })
    .await
}

/// Forget the key for the endpoint `tier` points at, which every tier at
/// that endpoint shares.
async fn clear_model_key(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: TierRef,
) -> CommandResult<ModelSettingsView> {
    svc.on_vault(move |vault| {
        vault.clear_model_key(args.tier)?;
        view(vault)
    })
    .await
}

// `Agent` rather than a scope of their own: configuring where a model is and
// storing the key that pays for it is what `set_agent_key` was, and a client
// granted the assistant could already do both.
pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "model_settings", scope: Agent, effect: Read,
        args: Nothing, returns: "ModelSettingsView", signature: &[],
        run: model_settings,
    },
    command! {
        name: "save_model_settings", scope: Agent, effect: Write,
        change: Settings / Updated,
        args: SaveModels, returns: "ModelSettingsView",
        signature: &[("settings", "ModelSettings", true)],
        run: save_model_settings,
    },
    command! {
        name: "set_model_key", scope: Agent, effect: Write,
        change: Settings / Updated,
        args: SetModelKey, returns: "ModelSettingsView",
        signature: &[("tier", "ModelTier", true), ("key", "string", true)],
        run: set_model_key,
    },
    command! {
        name: "clear_model_key", scope: Agent, effect: Write,
        change: Settings / Updated,
        args: TierRef, returns: "ModelSettingsView",
        signature: &[("tier", "ModelTier", true)],
        run: clear_model_key,
    },
];
