//! The local speech models: what can be downloaded, downloading it, and a
//! quick benchmark. See `meeting::models` and, for the model-dependent
//! commands, `meeting::speech` (feature `speech`).
//!
//! `speech_models` works, and answers truthfully, in a build without the
//! `speech` feature -- see `meeting::models`' own doc on why it is not
//! feature-gated. Only [`benchmark_speech_model`], which has to actually run
//! a recogniser, is feature-gated; the other writes here move files around
//! and need nothing sherpa-onnx provides.
//!
//! # Live refresh
//!
//! A model's directory lives beside vaults, not inside one, so none of
//! `events::Kind`'s existing variants is really "a model changed" --
//! [`crate::events::Kind::Settings`] is the closest fit, and what the
//! interface actually reloads off it (`meetingSettings`'s own `localSpeech`
//! flag, indirectly) is settings-shaped. The moment-to-moment progress bar
//! does not lean on this at all: the interface polls `speech_models` once a
//! second while a download is running, the same way `poll_auto_lock` is
//! polled rather than pushed. The `Settings` change exists so a *second*
//! window notices a download finished, or was deleted, without polling
//! forever.

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::meeting::models;
use crate::service::Service;
use everyday_core::meeting::LocalModel;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// `(name, TypeScript type, required)` args this domain reuses. See
/// `ui/src/lib/types.ts`'s `SpeechModelInfo`, `ModelBenchmark`.
const MODEL_REF_SIG: &[(&str, &str, bool)] = &[("id", "string", true)];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub done: u64,
    pub total: u64,
}

/// Mirrors `SpeechModelInfo` in `ui/src/lib/types.ts`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechModelInfo {
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

fn info(spec: &models::ModelSpec) -> SpeechModelInfo {
    let status = models::status(spec.id);
    SpeechModelInfo {
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
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef {
    pub id: String,
}

fn parse_local_model(id: &str) -> CommandResult<LocalModel> {
    match id {
        "parakeetV3" => Ok(LocalModel::ParakeetV3),
        "whisperTurbo" => Ok(LocalModel::WhisperTurbo),
        _ => Err(CommandError::new(codes::INVALID, format!("{id} is not a local recogniser"))),
    }
}

async fn speech_models(
    _svc: Arc<Service>,
    _ctx: Ctx,
    _args: super::Nothing,
) -> CommandResult<Vec<SpeechModelInfo>> {
    Ok(models::catalogue().iter().map(info).collect())
}

async fn download_speech_model(_svc: Arc<Service>, _ctx: Ctx, args: ModelRef) -> CommandResult<()> {
    models::start_download(&args.id).map_err(|e| CommandError::new(codes::INVALID, e))
}

async fn cancel_speech_model_download(
    _svc: Arc<Service>,
    _ctx: Ctx,
    args: ModelRef,
) -> CommandResult<()> {
    models::cancel_download(&args.id);
    Ok(())
}

async fn delete_speech_model(_svc: Arc<Service>, _ctx: Ctx, args: ModelRef) -> CommandResult<()> {
    models::delete(&args.id).map_err(|e| CommandError::new(codes::INVALID, e))
}

#[cfg(feature = "speech")]
async fn benchmark_speech_model(
    _svc: Arc<Service>,
    _ctx: Ctx,
    args: ModelRef,
) -> CommandResult<ModelBenchmark> {
    use crate::meeting::speech;
    use everyday_core::meeting::SAMPLE_RATE;

    let model = parse_local_model(&args.id)?;
    let recogniser = speech::recogniser(model)?;

    // Ten seconds of deterministic pseudo-noise. A recogniser's own forward
    // pass costs the same whether the audio is speech or not -- it is a
    // fixed amount of arithmetic over a fixed number of frames -- so this
    // measures real inference throughput without needing a bundled sample
    // clip, and the same seed gives the same number run to run.
    const SECONDS: u64 = 10;
    let n = (SECONDS * u64::from(SAMPLE_RATE)) as usize;
    let samples: Vec<i16> =
        (0..n).map(|i| ((i.wrapping_mul(2_654_435_761) % 2000) as i16) - 1000).collect();

    let elapsed = crate::service::blocking(move || {
        let started = std::time::Instant::now();
        let _ = recogniser.transcribe(&samples, None);
        Ok(started.elapsed())
    })
    .await?;

    let realtime_factor =
        if elapsed.as_secs_f64() > 0.0 { SECONDS as f64 / elapsed.as_secs_f64() } else { 0.0 };
    Ok(ModelBenchmark { realtime_factor })
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
        "this copy of Every Day was built without local speech",
    ))
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "speech_models", scope: Meetings, effect: Read,
        args: super::Nothing, returns: "SpeechModelInfo[]", signature: &[],
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
