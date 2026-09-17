use everyday_core::meeting::TranscriberConfig;

use crate::error::{CommandError, CommandResult, codes};
use crate::meeting::transcribe::Transcriber;

// ============================================================================
// Building a transcriber from settings
// ============================================================================

/// `https://api.openai.com/v1`, per the plan's own words for it.
pub const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Build the configured transcriber. The one place `TranscriberConfig` turns
/// into a live [`Transcriber`] -- used by the pipeline and by
/// `domains::transcripts::test_transcriber` alike, so the two can never
/// disagree about what "the configured transcriber" means.
pub fn build_transcriber(
    config: &TranscriberConfig,
    key: Option<String>,
) -> CommandResult<Box<dyn Transcriber>> {
    match config {
        TranscriberConfig::OpenAi { model } => {
            Ok(Box::new(crate::meeting::transcribe::openai::OpenAiTranscriber::new(
                OPENAI_BASE_URL,
                key,
                model.clone(),
            )))
        }
        TranscriberConfig::Compatible { base_url, model } => {
            Ok(Box::new(crate::meeting::transcribe::openai::OpenAiTranscriber::new(
                base_url.clone(),
                key,
                model.clone(),
            )))
        }
        TranscriberConfig::Google { model } => {
            let key = key.ok_or_else(|| {
                CommandError::new(codes::UNSUPPORTED, "a Google transcriber needs a key")
            })?;
            Ok(Box::new(crate::meeting::transcribe::gemini::GeminiTranscriber::new(
                key,
                model.clone(),
            )))
        }
        TranscriberConfig::Local { model } => build_local_transcriber(*model),
    }
}

#[cfg(feature = "speech")]
fn build_local_transcriber(
    model: everyday_core::meeting::LocalModel,
) -> CommandResult<Box<dyn Transcriber>> {
    Ok(Box::new(crate::meeting::transcribe::local::LocalTranscriber::new(model)))
}

#[cfg(not(feature = "speech"))]
fn build_local_transcriber(
    _model: everyday_core::meeting::LocalModel,
) -> CommandResult<Box<dyn Transcriber>> {
    Err(CommandError::new(codes::UNSUPPORTED, "this build does not include local speech"))
}
