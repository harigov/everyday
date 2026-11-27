//! How a picture is made: the image model, the seventh card in Settings →
//! Models.
//!
//! The same shape as the voice. One card, chosen once, and anything that
//! makes a picture asks [`Vault::image_access`](crate::Vault::image_access)
//! for it. OpenAI's Images API is one choice. The other two run on this
//! computer: the quantized FLUX.2 Klein 4B weights, which also edit, and the
//! quantized Z-Image Turbo weights, which are the smaller generator. Both
//! are reached through stable-diffusion.cpp, downloaded beside the vault
//! like a voice is, and neither sends the prompt anywhere.
//!
//! # Its key belongs to an endpoint too
//!
//! [`ImageGeneratorConfig::endpoint`] names where a prompt goes, spelt
//! exactly as a language model's address -- OpenAI's is
//! `https://api.openai.com/v1` -- and its key is the one already filed
//! there. A model on this computer has no address and no key.
//!
//! # Not in a mail acknowledgement
//!
//! A picture is not mail. The endpoint is left out of
//! [`crate::llm::ModelSettings::acknowledgement_name`] and counted only
//! where a key may be kept: [`crate::llm::ModelSettings::all_endpoints`].

use crate::error::{Error, Result};
use crate::llm::Provider;
use serde::{Deserialize, Serialize};

/// The everyday OpenAI image model: fast generation.
pub const OPENAI_IMAGE_FLARE: &str = "gpt-image-2.6-flare";

/// The OpenAI image model for detailed work.
pub const OPENAI_IMAGE_SUNBURST: &str = "gpt-image-2.6-sunburst";

/// The OpenAI image models the card offers, in the order they are listed.
pub const OPENAI_IMAGE_MODELS: &[&str] = &[OPENAI_IMAGE_FLARE, OPENAI_IMAGE_SUNBURST];

/// A picture model that runs on this computer. The files are the quantized
/// ones: see `jatayu_service::local::files`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum LocalImageModel {
    /// FLUX.2 [klein] 4B, Q4_0. Four steps. Generates and edits.
    Flux2KleinQ4,
    /// Z-Image Turbo, Q4_K. Eight steps. Generation only.
    ZImageTurboQ4,
}

impl LocalImageModel {
    /// Every one, in the order the card lists them.
    pub const ALL: [Self; 2] = [Self::Flux2KleinQ4, Self::ZImageTurboQ4];

    /// What it is called in the model catalogue and on the wire.
    pub fn id(self) -> &'static str {
        match self {
            LocalImageModel::Flux2KleinQ4 => "flux2KleinQ4",
            LocalImageModel::ZImageTurboQ4 => "zImageTurboQ4",
        }
    }

    /// The diffusion file inside the model's directory.
    pub fn diffusion_file(self) -> &'static str {
        match self {
            LocalImageModel::Flux2KleinQ4 => "flux-2-klein-4b-q4_0.gguf",
            LocalImageModel::ZImageTurboQ4 => "z_image_turbo-Q4_K.gguf",
        }
    }

    /// The text encoder inside the model's directory.
    pub fn text_encoder_file(self) -> &'static str {
        match self {
            LocalImageModel::Flux2KleinQ4 => "Qwen3-4B-Q4_K_S.gguf",
            LocalImageModel::ZImageTurboQ4 => "Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
        }
    }

    /// The VAE inside the model's directory.
    pub fn vae_file(self) -> &'static str {
        match self {
            LocalImageModel::Flux2KleinQ4 => "flux2-vae.safetensors",
            LocalImageModel::ZImageTurboQ4 => "ae.safetensors",
        }
    }

    /// Denoising steps for the distilled checkpoint.
    pub fn steps(self) -> u32 {
        match self {
            LocalImageModel::Flux2KleinQ4 => 4,
            LocalImageModel::ZImageTurboQ4 => 8,
        }
    }

    /// A name a person can read.
    pub fn name(self) -> &'static str {
        match self {
            LocalImageModel::Flux2KleinQ4 => "FLUX.2 Klein",
            LocalImageModel::ZImageTurboQ4 => "Z-Image Turbo",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "flux2KleinQ4" => Some(Self::Flux2KleinQ4),
            "zImageTurboQ4" => Some(Self::ZImageTurboQ4),
            _ => None,
        }
    }
}

/// Who makes the picture: this computer, or OpenAI's Images API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ImageGeneratorConfig {
    Local { model: LocalImageModel },
    OpenAi { model: String },
}

impl ImageGeneratorConfig {
    /// Where the prompt goes, and what the key is filed under. `None` on
    /// this computer.
    pub fn endpoint(&self) -> Option<&str> {
        match self {
            ImageGeneratorConfig::Local { .. } => None,
            ImageGeneratorConfig::OpenAi { .. } => Some(Provider::OpenAi.default_base_url()),
        }
    }

    pub fn needs_key(&self) -> bool {
        matches!(self, ImageGeneratorConfig::OpenAi { .. })
    }

    pub fn local_model(&self) -> Option<LocalImageModel> {
        match self {
            ImageGeneratorConfig::Local { model } => Some(*model),
            ImageGeneratorConfig::OpenAi { .. } => None,
        }
    }

    /// A model that has not been downloaded yet is still a valid choice:
    /// it is chosen first and downloaded on the same card.
    pub fn validate(&self) -> Result<()> {
        match self {
            ImageGeneratorConfig::Local { .. } => Ok(()),
            ImageGeneratorConfig::OpenAi { model } => {
                if model.trim().is_empty() {
                    return Err(Error::Invalid("a model name is required".into()));
                }
                Ok(())
            }
        }
    }
}

/// The picture model: the seventh card in Settings → Models.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageConfig {
    pub generator: ImageGeneratorConfig,
}

impl ImageConfig {
    pub fn new(generator: ImageGeneratorConfig) -> Self {
        Self { generator }
    }

    pub fn endpoint(&self) -> Option<&str> {
        self.generator.endpoint()
    }

    pub fn needs_key(&self) -> bool {
        self.generator.needs_key()
    }

    pub fn validate(&self) -> Result<()> {
        self.generator.validate()
    }
}

/// The picture model ready to be used, [`crate::speech::VoiceAccess`]'s
/// counterpart.
#[derive(Clone)]
pub struct ImageAccess {
    pub config: ImageConfig,
    /// `None` only where none is needed.
    pub key: Option<String>,
}

impl std::fmt::Debug for ImageAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageAccess")
            .field("config", &self.config)
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_shares_the_language_models_address() {
        let config = ImageGeneratorConfig::OpenAi { model: OPENAI_IMAGE_FLARE.into() };
        assert_eq!(config.endpoint(), Some(Provider::OpenAi.default_base_url()));
        assert!(config.needs_key());
        assert!(config.local_model().is_none());
    }

    #[test]
    fn a_local_model_has_no_address_and_no_key() {
        let config = ImageGeneratorConfig::Local { model: LocalImageModel::Flux2KleinQ4 };
        assert_eq!(config.endpoint(), None);
        assert!(!config.needs_key());
        assert_eq!(config.local_model(), Some(LocalImageModel::Flux2KleinQ4));
        config.validate().unwrap();
    }

    #[test]
    fn an_openai_model_needs_a_name() {
        let config = ImageGeneratorConfig::OpenAi { model: "  ".into() };
        assert!(config.validate().is_err());
    }

    #[test]
    fn the_wire_name_is_the_catalogue_id() {
        let model = LocalImageModel::ZImageTurboQ4;
        let json = serde_json::to_value(model).unwrap();
        assert_eq!(json, model.id());
        assert_eq!(LocalImageModel::from_id(model.id()), Some(model));
        assert_eq!(
            serde_json::to_value(ImageGeneratorConfig::Local { model }).unwrap(),
            serde_json::json!({ "type": "local", "model": "zImageTurboQ4" })
        );
    }
}
