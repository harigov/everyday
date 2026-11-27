//! Making a picture with whatever Settings → Models has chosen, and keeping
//! a picture that was made.
//!
//! One door to make one, [`Service::generate_image`]. It reads the image
//! card, and only that card: an OpenAI model is asked over its Images API,
//! and a model on this computer is run from the weights downloaded for it.
//! The caller gets the PNG back, and nothing is stored.
//!
//! One door to keep one, [`keep`]: a generated picture, or a drawing from
//! [`crate::illustrate`], put in Files under the name and caption it was
//! given. [`tools`] is both, as the assistant's tools.

mod local;
mod openai;
pub mod tools;

use crate::error::{CommandError, CommandResult, codes};
use crate::local::files;
use crate::service::{Service, blocking};
use jatayu_core::Vault;
use jatayu_core::files::{FileEntry, FileOrigin, FileUpdate, ImportOptions};
use jatayu_core::imagegen::ImageGeneratorConfig;
use serde::{Deserialize, Serialize};
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

/// The shape of a picture to make. Each model is asked for the size it makes
/// that shape at; see [`Shape::openai_size`] and [`Shape::local_size`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Shape {
    #[default]
    Square,
    /// Landscape, three by two.
    Wide,
    /// Portrait, two by three.
    Tall,
}

impl Shape {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "square" => Some(Self::Square),
            "wide" | "landscape" => Some(Self::Wide),
            "tall" | "portrait" => Some(Self::Tall),
            _ => None,
        }
    }

    /// The sizes OpenAI's image models take without further negotiation.
    fn openai_size(self) -> &'static str {
        match self {
            Shape::Square => "1024x1024",
            Shape::Wide => "1536x1024",
            Shape::Tall => "1024x1536",
        }
    }

    /// Width and height for a model on this computer: 512 on the short side,
    /// the size these quantized builds are documented to finish at on a
    /// small machine, and a multiple of 64 on the long one.
    fn local_size(self) -> (u32, u32) {
        match self {
            Shape::Square => (512, 512),
            Shape::Wide => (768, 512),
            Shape::Tall => (512, 768),
        }
    }
}

/// Whether a picture can be made with what Settings → Models has chosen:
/// `Some(true)` for a model on this computer whose weights and engine are
/// both downloaded,
/// `Some(false)` for a hosted one with its key, `None` when nothing could
/// make one yet. What decides whether the assistant is offered
/// `generate_image` at all.
pub fn painter(vault: &Vault) -> Option<bool> {
    let access = vault.image_access().ok()?;
    match access.config.generator {
        ImageGeneratorConfig::OpenAi { .. } => Some(false),
        ImageGeneratorConfig::Local { model } => {
            (files::image_paths(model).is_some() && local::engine_present()).then_some(true)
        }
    }
}

impl Service {
    /// The picture the saved image model makes for `prompt`, in `shape`.
    ///
    /// Refuses when the vault is locked, when no image model is chosen, when
    /// a hosted one has no key, and when a local one has not been downloaded.
    pub async fn generate_image(
        self: &Arc<Self>,
        prompt: &str,
        shape: Shape,
    ) -> CommandResult<Vec<u8>> {
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
                    .unwrap_or(jatayu_core::llm::Provider::OpenAi.default_base_url())
                    .to_string();
                openai::OpenAiPainter::new(base, key, model)
                    .paint(&prompt, shape.openai_size())
                    .await
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
                    local::paint(&bin, model, &weights, &prompt, shape.local_size())
                })
                .await
            }
        }
    }
}

/// How [`keep`] stores what it is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// Byte for byte: a drawing, whose PNG this application made itself and
    /// whose flat colour and lettering a JPEG would only blur.
    AsMade,
    /// Encoded again as whichever of PNG and JPEG is smaller -- a generated
    /// picture is a photograph in all but name, and many times smaller as a
    /// JPEG -- and with nothing but its pixels. See [`crate::pictures::prepare`].
    Smallest,
}

/// Put a picture that was made into Files: in `origin`'s own folder, named
/// `title` (made safe to be a file's name), with `caption` as its caption.
/// Answers the file as the catalog has it, size and thumbnail included: one
/// picture is read in milliseconds, and whoever made it -- a chat sending it,
/// a model told how big it came out -- wants them now, not after a
/// background pass.
///
/// Refuses when the vault has no files catalog.
pub async fn keep(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    picture: Vec<u8>,
    encoding: Encoding,
    title: &str,
    caption: &str,
    origin: FileOrigin,
) -> CommandResult<FileEntry> {
    if !vault.supports_files() {
        return Err(CommandError::new(
            codes::UNSUPPORTED,
            "this vault has no Files to keep a picture in",
        ));
    }
    let (title, caption) = (title.to_string(), caption.trim().to_string());
    let keeping = vault.clone();
    let entry = blocking(move || {
        let (bytes, extension) = match encoding {
            Encoding::AsMade => {
                let extension = match jatayu_core::media::sniff_mime(&picture) {
                    "image/jpeg" => "jpg",
                    "image/webp" => "webp",
                    _ => "png",
                };
                (picture, extension)
            }
            Encoding::Smallest => {
                let prepared = crate::pictures::prepare(&picture)?;
                let extension = if prepared.mime == "image/png" { "png" } else { "jpg" };
                (prepared.bytes, extension)
            }
        };
        let name = format!("{}.{extension}", file_stem(&title));
        let entry = keeping.import_bytes(&bytes, &name, &ImportOptions::new(origin))?;
        if !caption.is_empty() {
            let update = FileUpdate { caption: Some(caption), ..FileUpdate::default() };
            keeping.update_files(&[entry.id], &update)?;
        }
        if let Err(e) = crate::files::probe_and_store(&keeping, entry.id) {
            tracing::debug!(id = %entry.id, error = %e, "could not read a picture that was made");
        }
        Ok(keeping.file(entry.id)?)
    })
    .await?;
    crate::files::announce(svc, vec![entry.id]);
    Ok(entry)
}

/// `title` as the part of a file's name before its extension: one line,
/// none of the characters a file system refuses, at most 80 characters, and
/// "Picture" when nothing is left.
fn file_stem(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| if c.is_control() || r#"/\:*?"<>|"#.contains(c) { ' ' } else { c })
        .collect();
    let words = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let stem: String = words.trim_matches('.').chars().take(80).collect();
    let stem = stem.trim().to_string();
    if stem.is_empty() { "Picture".to_string() } else { stem }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_are_named_as_a_model_would_name_them() {
        assert_eq!(Shape::parse("Wide"), Some(Shape::Wide));
        assert_eq!(Shape::parse("portrait"), Some(Shape::Tall));
        assert_eq!(Shape::parse("round"), None);
        assert_eq!(Shape::default(), Shape::Square);
    }

    #[test]
    fn each_shape_is_a_size_its_model_takes() {
        assert_eq!(Shape::Wide.openai_size(), "1536x1024");
        assert_eq!(Shape::Tall.local_size(), (512, 768));
        for shape in [Shape::Square, Shape::Wide, Shape::Tall] {
            let (w, h) = shape.local_size();
            assert_eq!((w % 64, h % 64), (0, 0));
        }
    }

    #[test]
    fn a_title_becomes_a_name_a_file_can_have() {
        assert_eq!(file_stem("Sunset over Kyoto"), "Sunset over Kyoto");
        assert_eq!(file_stem("a/b: c?\nd"), "a b c d");
        assert_eq!(file_stem("  ...  "), "Picture");
        assert_eq!(file_stem(&"x".repeat(200)).chars().count(), 80);
    }
}
