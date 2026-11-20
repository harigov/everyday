//! The image card in Settings → Models: the picture models that can be
//! downloaded, and making one picture with whatever is saved -- and drawing
//! one from an SVG or an HTML page, which needs no model at all
//! ([`crate::illustrate`]).
//!
//! Which model is chosen, and its key, are the models domain's. Nothing
//! here reads them except [`generate_image`], which has to use them.

use crate::command;
use crate::ctx::Ctx;
use crate::domains::speech::{LocalModelInfo, MODEL_REF_SIG, ModelRef, info};
use crate::error::{CommandError, CommandResult, codes};
use crate::illustrate::{self, Markup};
use crate::images::Shape;
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
    /// Square when absent.
    #[serde(default)]
    pub shape: Option<Shape>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderIllustration {
    pub markup: Markup,
    pub source: String,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
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
    let png = svc.generate_image(&args.prompt, args.shape.unwrap_or_default()).await?;
    Ok(GeneratedImage { png: base64::engine::general_purpose::STANDARD.encode(png) })
}

/// An SVG or an HTML page drawn into a PNG. Needs no vault: it reads and
/// keeps nothing.
async fn render_illustration(
    _svc: Arc<Service>,
    _ctx: Ctx,
    args: RenderIllustration,
) -> CommandResult<GeneratedImage> {
    let drawn = illustrate::render(args.markup, &args.source, args.width, args.height).await?;
    Ok(GeneratedImage { png: base64::engine::general_purpose::STANDARD.encode(drawn.png) })
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
        signature: &[("prompt", "string", true), ("shape", "ImageShape | null", false)],
        run: generate_image,
    },
    command! {
        name: "render_illustration", scope: Agent, effect: Read,
        args: RenderIllustration, returns: "GeneratedImage",
        signature: &[
            ("markup", "IllustrationMarkup", true),
            ("source", "string", true),
            ("width", "number | null", false),
            ("height", "number | null", false),
        ],
        run: render_illustration,
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
        let err = image_id("phonon").unwrap_err();
        assert_eq!(err.code, codes::INVALID);
    }

    #[test]
    fn the_shape_and_the_markup_arrive_as_the_interface_spells_them() {
        let args: GenerateImage =
            serde_json::from_value(serde_json::json!({ "prompt": "a fox", "shape": "wide" }))
                .unwrap();
        assert_eq!(args.shape, Some(Shape::Wide));
        let args: GenerateImage =
            serde_json::from_value(serde_json::json!({ "prompt": "a fox" })).unwrap();
        assert_eq!(args.shape, None);
        let args: RenderIllustration = serde_json::from_value(
            serde_json::json!({ "markup": "svg", "source": "<svg/>", "width": 300 }),
        )
        .unwrap();
        assert_eq!((args.markup, args.width, args.height), (Markup::Svg, Some(300), None));
    }

    #[test]
    fn a_picture_travels_as_the_interface_spells_it() {
        let json = serde_json::to_value(GeneratedImage { png: "iVBOR".into() }).unwrap();
        assert_eq!(json, serde_json::json!({ "png": "iVBOR" }));
    }
}
