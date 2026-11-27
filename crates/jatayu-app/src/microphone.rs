//! What is taken out of this computer's microphone before anything listens
//! to the person: other people's voices, and noise the ordinary filter
//! leaves behind. Settings → AI features → On this computer.
//!
//! Three switches, each this computer's, beside the wake word's and the talk
//! key's in the config directory -- a room and a microphone belong to a
//! machine, not to a vault:
//!
//! - **Only my voice.** Every utterance is compared with the person's
//!   voiceprint before it is transcribed, and one that is plainly somebody
//!   else's -- the television, a colleague across the room -- is never
//!   heard. The service does it, in [`jatayu_service::speech::owner`]: it
//!   is told the switch here, at start-up and whenever it is changed, and
//!   every stream listening reads it for each utterance.
//! - **Remove other voices.** VoxLock, run over the microphone itself, keeps
//!   the person's voice and takes out everybody else's, even talking over
//!   them. It needs a sample of the person's voice, which recording a
//!   voiceprint keeps, and its model downloaded.
//! - **Stronger noise filter.** DPDFNet in place of the ordinary noise
//!   suppression, for noise that comes and goes: a fan, dishes, a keyboard.
//!
//! The last two run in the audio engine ([`crate::audio::engine`]), on a
//! thread of their own, and only where the microphone listens for the
//! person: voice mode and the talk key, the wake word, voice typing and
//! dictation. A meeting is recorded as it was, everybody in the room
//! included. See `docs/plans/mic-filtering.md`.

use std::sync::{Arc, Mutex};

use jatayu_service::error::{CommandError, CommandResult};
use jatayu_service::local::files;
use jatayu_service::service::Service;
use jatayu_service::speech::cleanup::Cleanup;
use jatayu_service::speech::owner::Strictness;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::state::AppState;

/// The microphone's settings on this machine. All off until chosen.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// Hear only utterances in the person's own voice.
    pub only_my_voice: bool,
    /// How alike to their voice an utterance must be.
    pub strictness: Strictness,
    /// Take other voices out of the microphone (VoxLock).
    pub isolate_voice: bool,
    /// DPDFNet in place of the ordinary noise suppression.
    pub strong_noise_filter: bool,
}

const FILE: &str = "microphone.json";

/// The setting as last read or saved: read once, then kept, since every
/// microphone that opens asks for it.
static CONFIG: Mutex<Option<Config>> = Mutex::new(None);

pub fn load_config() -> Config {
    let mut kept = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
    kept.get_or_insert_with(|| crate::machine_config::load(FILE)).clone()
}

fn save_config(config: &Config) -> Result<(), String> {
    crate::machine_config::save(FILE, config)?;
    *CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = Some(config.clone());
    Ok(())
}

/// Tell the service what was chosen here, at start-up: it checks the voice
/// of what it hears, and has no file of this computer's to read.
pub fn install(app: &AppHandle) {
    tell_service(app, &load_config());
}

fn tell_service(app: &AppHandle, config: &Config) {
    let check = config.only_my_voice.then_some(config.strictness);
    app.state::<AppState>().service().set_voice_check(check);
}

/// What the audio engine runs over a microphone that listens for the person,
/// or `None` for the ordinary filter alone: what is switched on here, as far
/// as its model is downloaded and, for taking out other voices, a sample of
/// the person's voice has been recorded. Asked each time a microphone opens,
/// so a change here is heard by the next one.
pub async fn cleanup(service: &Arc<Service>) -> Option<Arc<Cleanup>> {
    let config = load_config();
    let strong_noise = config.strong_noise_filter && files::noise_filter_installed();
    let voice_sample = if config.isolate_voice && files::voice_isolation_installed() {
        match service.owner_voice().await {
            Ok(owner) => owner.and_then(|o| o.sample),
            Err(e) => {
                tracing::debug!(error = %e, "microphone: no voice sample to isolate");
                None
            }
        }
    } else {
        None
    };
    if !strong_noise && voice_sample.is_none() {
        return None;
    }
    Some(Arc::new(Cleanup { strong_noise, voice_sample }))
}

/// Which filters a microphone opened now would run, as far as this machine's
/// setting and downloads go: `(voice isolation, noise filter)`. Something
/// that keeps a microphone open looks at it now and then, to open it again
/// once a model has arrived.
pub fn filters() -> (bool, bool) {
    let config = load_config();
    (
        config.isolate_voice && files::voice_isolation_installed(),
        config.strong_noise_filter && files::noise_filter_installed(),
    )
}

// ---- commands ----------------------------------------------------------------

/// What Settings draws for the microphone.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MicrophoneSettings {
    pub only_my_voice: bool,
    pub strictness: Strictness,
    pub isolate_voice: bool,
    pub strong_noise_filter: bool,
    /// A voiceprint of the person has been recorded, to compare with.
    pub voice_recorded: bool,
    /// And a sample of their voice kept with it, which taking out other
    /// voices needs: a voiceprint recorded before that was kept has none.
    pub voice_sample: bool,
    /// Why the filters were last turned off part-way, if they were: this
    /// computer could not keep up with them.
    pub problem: Option<String>,
}

async fn settings(app: &AppHandle) -> MicrophoneSettings {
    let config = load_config();
    let owner = app.state::<AppState>().service().owner_voice().await.ok().flatten();
    MicrophoneSettings {
        only_my_voice: config.only_my_voice,
        strictness: config.strictness,
        isolate_voice: config.isolate_voice,
        strong_noise_filter: config.strong_noise_filter,
        voice_recorded: owner.is_some(),
        voice_sample: owner.is_some_and(|o| o.sample.is_some()),
        problem: crate::audio::engine::cleanup_trouble(),
    }
}

#[tauri::command]
pub async fn microphone_settings(app: AppHandle) -> CommandResult<MicrophoneSettings> {
    Ok(settings(&app).await)
}

/// Change any of the three switches, or how strict the voice check is;
/// `None` leaves one as it is. The voice check applies to the next utterance
/// heard; the filters to the next microphone that opens, and the wake word's
/// opens again at once when they changed.
#[tauri::command]
pub async fn set_microphone_settings(
    app: AppHandle,
    only_my_voice: Option<bool>,
    strictness: Option<Strictness>,
    isolate_voice: Option<bool>,
    strong_noise_filter: Option<bool>,
) -> CommandResult<MicrophoneSettings> {
    let was = load_config();
    let mut config = was.clone();
    config.only_my_voice = only_my_voice.unwrap_or(config.only_my_voice);
    config.strictness = strictness.unwrap_or(config.strictness);
    config.isolate_voice = isolate_voice.unwrap_or(config.isolate_voice);
    config.strong_noise_filter = strong_noise_filter.unwrap_or(config.strong_noise_filter);
    save_config(&config).map_err(|e| CommandError::new("microphone", e))?;
    tell_service(&app, &config);
    if (config.isolate_voice, config.strong_noise_filter)
        != (was.isolate_voice, was.strong_noise_filter)
    {
        crate::audio::engine::clear_cleanup_trouble();
        crate::wake::relisten(&app);
    }
    Ok(settings(&app).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_is_off_until_chosen() {
        let config: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(config, Config::default());
        assert!(!config.only_my_voice && !config.isolate_voice && !config.strong_noise_filter);
        assert_eq!(config.strictness, Strictness::Balanced);
    }

    #[test]
    fn the_file_reads_back_what_was_saved() {
        let config = Config {
            only_my_voice: true,
            strictness: Strictness::Strict,
            isolate_voice: true,
            strong_noise_filter: false,
        };
        let text = serde_json::to_string(&config).unwrap();
        assert!(text.contains("\"onlyMyVoice\":true"), "{text}");
        assert!(text.contains("\"strictness\":\"strict\""), "{text}");
        assert_eq!(serde_json::from_str::<Config>(&text).unwrap(), config);
    }
}
