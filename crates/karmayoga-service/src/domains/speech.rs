//! The local speech models: what can be downloaded, downloading it, and a
//! quick benchmark -- the speech card's half of Settings → Models, now that
//! how speech becomes text is the platform's choice rather than meeting
//! notes' (see `karmayoga_core::speech`). See `local::files` and, for the
//! model-dependent commands, `speech::local` (feature `speech`).
//!
//! Still under the `Meetings` scope they were born in, so a client allowed
//! to set meeting notes up keeps the downloads it always had. Nothing here
//! touches a key or a setting, only files beside the vault.
//!
//! `speech_models` works, and answers truthfully, in a build without the
//! `speech` feature -- see `local::files`' own doc on why it is not
//! feature-gated. Only [`benchmark_speech_model`], which has to actually run
//! a recogniser, is feature-gated; the other writes here move files around
//! and need nothing sherpa-onnx provides.
//!
//! # Live refresh
//!
//! A model's directory lives beside vaults, not inside one, so none of
//! `events::Kind`'s existing variants is really "a model changed" --
//! [`crate::events::Kind::Settings`] is the closest fit, and what the
//! interface actually reloads off it (whether meeting notes are usable,
//! indirectly) is settings-shaped. The moment-to-moment progress bar
//! does not lean on this at all: the interface polls `speech_models` once a
//! second while a download is running, the same way `poll_auto_lock` is
//! polled rather than pushed. The `Settings` change exists so a *second*
//! window notices a download finished, or was deleted, without polling
//! forever.

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::local::files::{self as models, ModelKind};
use crate::service::Service;
use karmayoga_core::meeting::LocalModel;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// `(name, TypeScript type, required)` args this domain reuses. See
/// `ui/src/lib/types.ts`'s `LocalModelInfo`, `ModelBenchmark`.
pub(crate) const MODEL_REF_SIG: &[(&str, &str, bool)] = &[("id", "string", true)];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub done: u64,
    pub total: u64,
}

/// One downloadable model on this computer, for the card that lists it --
/// the speech card's recognisers here, the decision card's models in
/// `domains::models`. Mirrors `LocalModelInfo` in `ui/src/lib/types.ts`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalModelInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub languages: String,
    pub bytes: u64,
    pub installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<DownloadProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub(crate) fn info(spec: &models::ModelSpec) -> LocalModelInfo {
    let status = models::status(spec.id);
    LocalModelInfo {
        id: spec.id.to_string(),
        name: spec.name.to_string(),
        description: spec.description.to_string(),
        languages: spec.languages.to_string(),
        bytes: spec.bytes(),
        installed: status.as_ref().is_some_and(|s| s.installed),
        progress: status
            .as_ref()
            .and_then(|s| s.progress)
            .map(|(done, total)| DownloadProgress { done, total }),
        error: status.and_then(|s| s.error),
    }
}

/// Mirrors `ModelBenchmark` in `ui/src/lib/types.ts`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelBenchmark {
    pub realtime_factor: f64,
    /// The graphics cards it ran on, by name; absent on the processor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef {
    pub id: String,
}

/// `id`, when it is one of the speech models -- the catalogue holds
/// decision models too, which this pane has no business downloading.
fn speech_id(id: &str) -> CommandResult<&str> {
    match models::spec(id) {
        Some(spec) if spec.kind == ModelKind::Speech => Ok(id),
        _ => Err(CommandError::new(codes::INVALID, format!("{id} is not a speech model"))),
    }
}

fn parse_local_model(id: &str) -> CommandResult<LocalModel> {
    LocalModel::ALL
        .into_iter()
        .find(|m| models::local_model_id(*m) == id)
        .ok_or_else(|| CommandError::new(codes::INVALID, format!("{id} is not a local recogniser")))
}

/// Whether this copy could run the speech model `id`: every one but a
/// recogniser on llama.cpp, which needs the `decisions` feature too -- the
/// desktop app has it, the server does not. The speech kit and the
/// sherpa-onnx recognisers stay listed in a copy without `speech`, which
/// still answers what one would need.
fn runs_here(id: &str) -> bool {
    cfg!(feature = "decisions") || parse_local_model(id).map_or(true, |m| !m.on_llama())
}

async fn speech_models(
    _svc: Arc<Service>,
    _ctx: Ctx,
    _args: super::Nothing,
) -> CommandResult<Vec<LocalModelInfo>> {
    Ok(models::catalogue_of(ModelKind::Speech).filter(|s| runs_here(s.id)).map(info).collect())
}

async fn download_speech_model(_svc: Arc<Service>, _ctx: Ctx, args: ModelRef) -> CommandResult<()> {
    let id = speech_id(&args.id)?;
    if !runs_here(id) {
        return Err(CommandError::new(
            codes::UNSUPPORTED,
            format!("this copy of Karma Yoga cannot run {id}"),
        ));
    }
    models::start_download(id).map_err(|e| CommandError::new(codes::INVALID, e))
}

async fn cancel_speech_model_download(
    _svc: Arc<Service>,
    _ctx: Ctx,
    args: ModelRef,
) -> CommandResult<()> {
    // Nothing to stop is not a failure -- and a decision model's download is
    // not this pane's to stop.
    if speech_id(&args.id).is_ok() {
        models::cancel_download(&args.id);
    }
    Ok(())
}

async fn delete_speech_model(_svc: Arc<Service>, _ctx: Ctx, args: ModelRef) -> CommandResult<()> {
    models::delete(speech_id(&args.id)?).map_err(|e| CommandError::new(codes::INVALID, e))
}

#[cfg(feature = "speech")]
async fn benchmark_speech_model(
    _svc: Arc<Service>,
    _ctx: Ctx,
    args: ModelRef,
) -> CommandResult<ModelBenchmark> {
    use crate::speech::local as speech;
    use karmayoga_core::meeting::SAMPLE_RATE;

    let model = parse_local_model(&args.id)?;
    let recogniser = speech::recogniser_async(model).await?;

    // Ten seconds of deterministic pseudo-noise. A recogniser's own forward
    // pass costs the same whether the audio is speech or not -- it is a
    // fixed amount of arithmetic over a fixed number of frames -- so this
    // measures real inference throughput without needing a bundled sample
    // clip, and the same seed gives the same number run to run. Qwen's time
    // goes mostly on writing what it heard, so `transcribe_timed` has it
    // write as much as ten seconds of talking would need.
    const SECONDS: u64 = 10;
    let n = (SECONDS * u64::from(SAMPLE_RATE)) as usize;
    let samples: Vec<i16> =
        (0..n).map(|i| ((i.wrapping_mul(2_654_435_761) % 2000) as i16) - 1000).collect();

    let (elapsed, gpu) = crate::service::blocking(move || {
        let started = std::time::Instant::now();
        recogniser.transcribe_timed(&samples)?;
        Ok((started.elapsed(), recogniser.gpu()))
    })
    .await?;

    let realtime_factor =
        if elapsed.as_secs_f64() > 0.0 { SECONDS as f64 / elapsed.as_secs_f64() } else { 0.0 };
    Ok(ModelBenchmark { realtime_factor, gpu })
}

#[cfg(not(feature = "speech"))]
async fn benchmark_speech_model(
    _svc: Arc<Service>,
    _ctx: Ctx,
    args: ModelRef,
) -> CommandResult<ModelBenchmark> {
    // Still validates `id`, so a caller gets `invalid` for a bad id and
    // `unsupported` only for the one reason this build genuinely cannot
    // answer -- the same distinction `download_speech_model` and friends
    // draw for a build without local speech, just here for the one command
    // that cannot even pretend to answer.
    parse_local_model(&args.id)?;
    Err(CommandError::new(
        codes::UNSUPPORTED,
        "this copy of Karma Yoga was built without local speech",
    ))
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "speech_models", scope: Meetings, effect: Read,
        args: super::Nothing, returns: "LocalModelInfo[]", signature: &[],
        run: speech_models,
    },
    command! {
        name: "download_speech_model", scope: Meetings, effect: Write,
        change: Settings / Updated,
        args: ModelRef, returns: "void", signature: MODEL_REF_SIG,
        run: download_speech_model,
    },
    command! {
        name: "cancel_speech_model_download", scope: Meetings, effect: Write,
        change: Settings / Updated,
        args: ModelRef, returns: "void", signature: MODEL_REF_SIG,
        run: cancel_speech_model_download,
    },
    command! {
        name: "delete_speech_model", scope: Meetings, effect: Destructive,
        change: Settings / Deleted,
        args: ModelRef, returns: "void", signature: MODEL_REF_SIG,
        run: delete_speech_model,
    },
    command! {
        name: "benchmark_speech_model", scope: Meetings, effect: Read,
        args: ModelRef, returns: "ModelBenchmark", signature: MODEL_REF_SIG,
        run: benchmark_speech_model,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_copy_with_llama_cpp_offers_the_recognisers_on_it() {
        assert!(runs_here(models::SPEECH_KIT_ID));
        assert!(runs_here("parakeetV3") && runs_here("whisperTurbo"));
        for model in LocalModel::ALL.into_iter().filter(LocalModel::on_llama) {
            assert_eq!(runs_here(models::local_model_id(model)), cfg!(feature = "decisions"));
        }
    }

    /// The interface hides the llama.cpp recognisers in a copy without
    /// llama.cpp by a flag of its own; it must flag the same ones.
    #[test]
    fn the_interface_flags_the_same_recognisers_as_on_llama_cpp() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../ui/src/lib/meetings-format.ts");
        let text = std::fs::read_to_string(&path).expect("meetings-format.ts is in the tree");
        let start = text.find("export const LOCAL_RECOGNISERS").expect("LOCAL_RECOGNISERS");
        let list = &text[start..start + text[start..].find("\n]").expect("the list ends")];
        for model in LocalModel::ALL {
            let id = models::local_model_id(model);
            let at = list.find(&format!("id: '{id}'")).unwrap_or_else(|| panic!("{id} unlisted"));
            let entry = &list[at..list[at..].find('}').map_or(list.len(), |end| at + end)];
            assert_eq!(entry.contains("llama: true"), model.on_llama(), "{id}");
        }
    }
}
