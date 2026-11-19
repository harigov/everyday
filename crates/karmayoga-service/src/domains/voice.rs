//! The voice card in Settings → Models: the voices that can be downloaded, and
//! a few seconds of the saved one to hear before using it.
//!
//! The sixth card, beside the speech model's (`domains::speech`), and built the
//! same way: the downloads are files beside the vault and touch no key or
//! setting, so they sit under the `Meetings` scope as their neighbours do, and
//! a download tells other windows through [`Kind::Settings`](crate::events::Kind)
//! rather than a kind of its own. Which voice is chosen, and its key, are the
//! models domain's (`save_model_settings` and friends); nothing here reads
//! them except [`voice_sample`], which has to say something with them.

use crate::command;
use crate::ctx::Ctx;
use crate::domains::speech::{LocalModelInfo, MODEL_REF_SIG, ModelRef, info};
use crate::error::{CommandError, CommandResult, codes};
use crate::local::files::{self as models, ModelKind};
use crate::service::Service;
use crate::speech::wav;
use base64::Engine;
use serde::Serialize;
use std::sync::Arc;

/// What the sample says. Short, because the person is waiting to hear it, and
/// plain, so that it is the voice they are judging and not the sentence.
const SAMPLE_TEXT: &str = "Hello. This is how I sound.";

/// A few seconds of the saved voice. Mirrors `VoiceSample` in
/// `ui/src/lib/types.ts`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceSample {
    /// A 16-bit mono WAV file, base64.
    pub wav: String,
}

/// `id`, when it is one of the voices -- the catalogue holds recognisers and
/// decision models too, which this card has no business downloading.
fn voice_id(id: &str) -> CommandResult<&str> {
    match models::spec(id) {
        Some(spec) if spec.kind == ModelKind::Voice => Ok(id),
        _ => Err(CommandError::new(codes::INVALID, format!("{id} is not a voice model"))),
    }
}

async fn voice_models(
    _svc: Arc<Service>,
    _ctx: Ctx,
    _args: super::Nothing,
) -> CommandResult<Vec<LocalModelInfo>> {
    Ok(models::catalogue_of(ModelKind::Voice).map(info).collect())
}

async fn download_voice_model(_svc: Arc<Service>, _ctx: Ctx, args: ModelRef) -> CommandResult<()> {
    models::start_download(voice_id(&args.id)?).map_err(|e| CommandError::new(codes::INVALID, e))
}

async fn cancel_voice_model_download(
    _svc: Arc<Service>,
    _ctx: Ctx,
    args: ModelRef,
) -> CommandResult<()> {
    // Nothing to stop is not a failure -- and another card's download is not
    // this one's to stop.
    if voice_id(&args.id).is_ok() {
        models::cancel_download(&args.id);
    }
    Ok(())
}

async fn delete_voice_model(_svc: Arc<Service>, _ctx: Ctx, args: ModelRef) -> CommandResult<()> {
    models::delete(voice_id(&args.id)?).map_err(|e| CommandError::new(codes::INVALID, e))
}

/// The saved voice saying [`SAMPLE_TEXT`], in the speech model's language where
/// the voice can speak it.
///
/// Locked vault refused, as every command that puts a request on the network
/// is: a hosted voice is sent the words. So is a voice that is not downloaded
/// yet, in a sentence that says so.
async fn voice_sample(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: super::Nothing,
) -> CommandResult<VoiceSample> {
    let synthesiser = svc.speech_synthesiser().await?;
    let speech = synthesiser.synthesise(SAMPLE_TEXT).await?;
    let bytes = wav::encode(&speech.samples, speech.sample_rate);
    Ok(VoiceSample { wav: base64::engine::general_purpose::STANDARD.encode(bytes) })
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "voice_models", scope: Meetings, effect: Read,
        args: super::Nothing, returns: "LocalModelInfo[]", signature: &[],
        run: voice_models,
    },
    command! {
        name: "download_voice_model", scope: Meetings, effect: Write,
        change: Settings / Updated,
        args: ModelRef, returns: "void", signature: MODEL_REF_SIG,
        run: download_voice_model,
    },
    command! {
        name: "cancel_voice_model_download", scope: Meetings, effect: Write,
        change: Settings / Updated,
        args: ModelRef, returns: "void", signature: MODEL_REF_SIG,
        run: cancel_voice_model_download,
    },
    command! {
        name: "delete_voice_model", scope: Meetings, effect: Destructive,
        change: Settings / Deleted,
        args: ModelRef, returns: "void", signature: MODEL_REF_SIG,
        run: delete_voice_model,
    },
    command! {
        name: "voice_sample", scope: Meetings, effect: Read,
        args: super::Nothing, returns: "VoiceSample", signature: &[],
        run: voice_sample,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_voices_are_this_cards_to_download_or_delete() {
        assert_eq!(voice_id("supertonic3").unwrap(), "supertonic3");
        for id in ["parakeetV3", "whisperTurbo", "no-such-model", ""] {
            let err = voice_id(id).unwrap_err();
            assert_eq!(err.code, codes::INVALID, "{id}");
        }
    }

    #[test]
    fn the_catalogue_holds_the_voices_and_nothing_else() {
        let listed: Vec<String> =
            models::catalogue_of(ModelKind::Voice).map(|s| s.id.to_string()).collect();
        assert!(listed.contains(&"supertonic3".to_string()), "{listed:?}");
        assert!(listed.iter().all(|id| voice_id(id).is_ok()));
    }

    #[test]
    fn a_sample_travels_as_the_interface_spells_it() {
        let json = serde_json::to_value(VoiceSample { wav: "UklGRg==".into() }).unwrap();
        assert_eq!(json, serde_json::json!({ "wav": "UklGRg==" }));
    }
}
