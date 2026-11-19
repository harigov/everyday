//! Making a picture with whatever Settings → Models has chosen.
//!
//! One door, [`Service::generate_image`]. It reads the image card, and only
//! that card: an OpenAI model is asked over its Images API, and a model on
//! this computer is run from the weights downloaded for it. The caller gets
//! the PNG back. Nothing here stores it.

mod local;
mod openai;

use crate::error::{CommandError, CommandResult, codes};
use crate::local::files;
use crate::service::{Service, blocking};
use karmayoga_core::imagegen::ImageGeneratorConfig;
use std::sync::{Arc, Mutex, TryLockError};

const FIX: &str = "Download it in Settings → Models.";

/// Held while a picture is made on this computer. Each run loads gigabytes
/// of weights -- a 4B text encoder, the diffusion model, the VAE -- and two
/// at once, on the eight-gigabyte machines these quantized models are for,
/// would swap or wake the OOM killer. A second asks again rather than
/// queueing behind minutes of the first.
///
/// Taken on the blocking thread that runs the engine, not in the request: a
/// request that is dropped -- a client gone away -- leaves the engine running
/// to the end, and the lock with it.
static PAINTING: Mutex<()> = Mutex::new(());

impl Service {
    /// The picture the saved image model makes for `prompt`.
    ///
    /// Refuses when the vault is locked, when no image model is chosen, when
    /// a hosted one has no key, and when a local one has not been downloaded.
    pub async fn generate_image(self: &Arc<Self>, prompt: &str) -> CommandResult<Vec<u8>> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err(CommandError::new(codes::INVALID, "say what the picture should be"));
        }
        if prompt.chars().count() > 4_000 {
            return Err(CommandError::new(
                codes::INVALID,
                "that prompt is longer than an image model will take",
            ));
        }
        let prompt = prompt.to_string();
        let vault = self.require_unlocked()?;
        let access = blocking(move || Ok(vault.image_access()?)).await?;
        match access.config.generator.clone() {
            ImageGeneratorConfig::OpenAi { model } => {
                let key = access.key.unwrap_or_default();
                let base = access
                    .config
                    .endpoint()
                    .unwrap_or(karmayoga_core::llm::Provider::OpenAi.default_base_url())
                    .to_string();
                openai::OpenAiPainter::new(base, key, model).paint(&prompt).await
            }
            ImageGeneratorConfig::Local { model } => {
                let Some(weights) = files::image_paths(model) else {
                    return Err(CommandError::new(
                        codes::INVALID,
                        format!(
                            "The image model, {}, has not been downloaded yet. {FIX}",
                            model.name()
                        ),
                    ));
                };
                blocking(move || {
                    let _painting = match PAINTING.try_lock() {
                        Ok(turn) => turn,
                        // A run that panicked has still finished.
                        Err(TryLockError::Poisoned(turn)) => turn.into_inner(),
                        Err(TryLockError::WouldBlock) => {
                            return Err(CommandError::new(
                                codes::BUSY,
                                "A picture is already being made on this computer. Try again \
                                 once it is done.",
                            ));
                        }
                    };
                    let bin = local::engine()?;
                    local::paint(&bin, model, &weights, &prompt)
                })
                .await
            }
        }
    }
}
