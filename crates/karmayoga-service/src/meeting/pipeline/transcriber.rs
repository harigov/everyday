use karmayoga_core::meeting::TranscriberConfig;

use crate::error::{CommandError, CommandResult, codes};
use crate::meeting::transcribe::Transcriber;

// ============================================================================
// Building a transcriber from settings
// ============================================================================

/// `https://api.openai.com/v1`, per the plan's own words for it -- and the
/// speech model's endpoint when it is OpenAI, the address its key is filed
/// under (`TranscriberConfig::endpoint`). A test below holds the two to one
/// spelling.
pub const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Build the configured transcriber. The one place `TranscriberConfig` turns
/// into a live [`Transcriber`] -- used by the pipeline and by
/// `domains::transcripts::test_transcriber` alike, so the two can never
/// disagree about what "the configured transcriber" means.
///
/// `config` and `key` come from `Vault::speech_access` together, the
/// platform's speech model and the key for where it points: every caller
/// takes them from there rather than assembling them itself.
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
    model: karmayoga_core::meeting::LocalModel,
) -> CommandResult<Box<dyn Transcriber>> {
    Ok(Box::new(crate::meeting::transcribe::local::LocalTranscriber::new(model)))
}

#[cfg(not(feature = "speech"))]
fn build_local_transcriber(
    _model: karmayoga_core::meeting::LocalModel,
) -> CommandResult<Box<dyn Transcriber>> {
    Err(CommandError::new(codes::UNSUPPORTED, "this build does not include local speech"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key is filed under the speech model's endpoint and sent to the
    /// address the transcriber is built with. If the two were spelt apart, a
    /// key could be stored for one host and sent to another -- or, as
    /// likely, stored where the request never looks.
    #[test]
    fn audio_goes_where_the_speech_models_key_is_filed() {
        let openai = TranscriberConfig::OpenAi { model: "gpt-4o-transcribe".into() };
        assert_eq!(openai.endpoint(), Some(OPENAI_BASE_URL));
        let google = TranscriberConfig::Google { model: "gemini-2.5-flash".into() };
        assert_eq!(google.endpoint(), Some(crate::meeting::transcribe::gemini::DEFAULT_BASE_URL));
    }
}
