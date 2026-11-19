//! The image card in Settings → Models: the picture models that can be
//! downloaded, and making one picture with whatever is saved.
//!
//! Which model is chosen, and its key, are the models domain's. Nothing
//! here reads them except [`generate_image`], which has to use them.

use crate::command;
use crate::ctx::Ctx;
use crate::domains::speech::{LocalModelInfo, MODEL_REF_SIG, ModelRef, info};
use crate::error::{CommandError, CommandResult, codes};
use crate::local::files::{self as models, ModelKind};
use crate::service::Service;
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// One picture, PNG, base64. Mirrors `GeneratedImage` in `ui/src/lib/types.ts`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedImage {
    pub png: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateImage {
    pub prompt: String,
}

fn image_id(id: &str) -> CommandResult<&str> {
    match models::spec(id) {
        Some(spec) if spec.kind == ModelKind::Image => Ok(id),
        _ => Err(CommandError::new(codes::INVALID, format!("{id} is not an image model"))),
    }
}

async fn image_models(
    _svc: Arc<Service>,
    _ctx: Ctx,
    _args: super::Nothing,
) -> CommandResult<Vec<LocalModelInfo>> {
    Ok(models::catalogue_of(ModelKind::Image).map(info).collect())
}

async fn download_image_model(_svc: Arc<Service>, _ctx: Ctx, args: ModelRef) -> CommandResult<()> {
    models::start_download(image_id(&args.id)?).map_err(|e| CommandError::new(codes::INVALID, e))
}

async fn cancel_image_model_download(
    _svc: Arc<Service>,
    _ctx: Ctx,
    args: ModelRef,
) -> CommandResult<()> {
    if image_id(&args.id).is_ok() {
        models::cancel_download(&args.id);
    }
    Ok(())
}

async fn delete_image_model(_svc: Arc<Service>, _ctx: Ctx, args: ModelRef) -> CommandResult<()> {
    models::delete(image_id(&args.id)?).map_err(|e| CommandError::new(codes::INVALID, e))
}

async fn generate_image(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: GenerateImage,
) -> CommandResult<GeneratedImage> {
    let png = svc.generate_image(&args.prompt).await?;
    Ok(GeneratedImage { png: base64::engine::general_purpose::STANDARD.encode(png) })
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "image_models", scope: Agent, effect: Read,
        args: super::Nothing, returns: "LocalModelInfo[]", signature: &[],
        run: image_models,
    },
    command! {
        name: "download_image_model", scope: Agent, effect: Write,
        change: Settings / Updated,
        args: ModelRef, returns: "void", signature: MODEL_REF_SIG,
        run: download_image_model,
    },
    command! {
        name: "cancel_image_model_download", scope: Agent, effect: Write,
        change: Settings / Updated,
        args: ModelRef, returns: "void", signature: MODEL_REF_SIG,
        run: cancel_image_model_download,
    },
    command! {
        name: "delete_image_model", scope: Agent, effect: Destructive,
        change: Settings / Deleted,
        args: ModelRef, returns: "void", signature: MODEL_REF_SIG,
        run: delete_image_model,
    },
    command! {
        name: "generate_image", scope: Agent, effect: Read,
        args: GenerateImage, returns: "GeneratedImage",
        signature: &[("prompt", "string", true)],
        run: generate_image,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_image_models_are_this_cards_to_download() {
        assert_eq!(image_id("flux2KleinQ4").unwrap(), "flux2KleinQ4");
        assert_eq!(image_id("zImageTurboQ4").unwrap(), "zImageTurboQ4");
        assert_eq!(image_id("sdCli").unwrap(), "sdCli");
        let err = image_id("supertonic3").unwrap_err();
        assert_eq!(err.code, codes::INVALID);
    }

    #[test]
    fn a_picture_travels_as_the_interface_spells_it() {
        let json = serde_json::to_value(GeneratedImage { png: "iVBOR".into() }).unwrap();
        assert_eq!(json, serde_json::json!({ "png": "iVBOR" }));
    }
}
