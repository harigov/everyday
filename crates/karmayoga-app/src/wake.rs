//! The wake word: calling the assistant by name instead of pressing the talk
//! key.
//!
//! Switched on in Settings → AI features, the microphone is listened to
//! whenever nothing else needs it, and "Siri, type I won't be available
//! tomorrow" -- the assistant's name, or whatever name was chosen here,
//! followed by a request -- begins a conversation exactly as the talk key
//! does ([`crate::talk`]): the card in the corner, the answer spoken, typing
//! where the person was typing. What came after the name is its first turn;
//! the name alone opens it and listens. It ends after a short quiet, and the
//! listening for the name starts again.
//!
//! The listening is the service's ([`karmayoga_service::speech::wake`]): the
//! detector and the recogniser voice mode uses, with nothing kept and nothing
//! said without the name ever leaving it. This module decides when to listen
//! -- [`hold_back`] -- and holds the microphone while it does. It never claims
//! the microphone from anything: voice mode, voice typing, a meeting being
//! recorded, write-by-voice, something being read aloud and the talk key's
//! conversation all come first, and it stops for them within half a second
//! of their starting and listens again when they are done; so does another
//! application's call, unless that is switched off. See
//! `docs/plans/wake-word.md`.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use karmayoga_service::error::{CommandError, CommandResult};
use karmayoga_service::speech::wake::{WakeLink, Woken};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::audio::engine::{AudioEngine, EngineEvent, EngineOptions, EngineSink};
use crate::state::AppState;

// ---- the per-machine setting -------------------------------------------------

/// The wake word's setting on this machine, beside the talk key's: the
/// microphone it listens to is this computer's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub enabled: bool,
    /// The names it answers to, separated by commas -- "Siri", or "Jarvis,
    /// Javis" for a name the recogniser spells more than one way. Empty: the
    /// assistant's own name, from Settings → Assistant.
    pub names: String,
    /// Stop listening while another application has the microphone: on a
    /// call, what is said is for the call.
    pub pause_in_calls: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { enabled: false, names: String::new(), pause_in_calls: true }
    }
}

const FILE: &str = "wake.json";

/// The most names that may be listened for at once, and the longest one.
const MAX_NAMES: usize = 5;
const MAX_NAME_CHARS: usize = 40;

/// The setting as last read or saved: read from its file once, then kept
/// here, since [`look`] wants it twice a second and [`set_wake_settings`] is
/// the one thing that changes it.
static CONFIG: Mutex<Option<Config>> = Mutex::new(None);

fn save_config(config: &Config) -> Result<(), String> {
    crate::machine_config::save(FILE, config)?;
    *CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = Some(config.clone());
    Ok(())
}

fn load_config() -> Config {
    let mut kept = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
    kept.get_or_insert_with(|| crate::machine_config::load(FILE)).clone()
}

/// The names in `names`, as listened for: split at commas, trimmed, empty
/// ones left out.
fn names_in(names: &str) -> Vec<String> {
    names.split(',').map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).collect()
}

// ---- deciding when to listen -------------------------------------------------

/// How often whether to listen is looked at: how long something that wants
/// the microphone shares it with this before this lets go.
const LOOK_EVERY: Duration = Duration::from_millis(500);

/// How often, while it listens for the assistant's own name, that name is
/// looked at again: renamed in Settings → Assistant, it answers to the new one
/// within this.
const NAME_CHECK: Duration = Duration::from_secs(15);

/// How long after the listening could not be started -- no speech model, the
/// microphone refused -- before it is tried again.
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// Why it is not listening when it is switched on. Each is a moment, not a
/// fault: it listens again when the reason has gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    Off,
    /// The window is looking at a vault on another computer, whose
    /// microphone this is not.
    Remote,
    Locked,
    /// Something of this application's has the microphone, or the speakers
    /// are reading something aloud that the microphone would hear.
    Busy,
    /// A conversation is going on -- one the name began, or the key.
    Talking,
    /// Another application has the microphone.
    Call,
}

/// What is true now that decides whether to listen, gathered so that the
/// rule itself ([`reason`]) is a function of nothing else.
#[derive(Debug, Clone, Copy, Default)]
struct Now {
    enabled: bool,
    remote: bool,
    unlocked: bool,
    busy: bool,
    talking: bool,
    pause_in_calls: bool,
    others_have_microphone: bool,
}

/// Why not to listen now, or `None` to listen.
fn reason(now: Now) -> Option<Reason> {
    if !now.enabled {
        Some(Reason::Off)
    } else if now.remote {
        Some(Reason::Remote)
    } else if !now.unlocked {
        Some(Reason::Locked)
    } else if now.busy {
        Some(Reason::Busy)
    } else if now.talking {
        Some(Reason::Talking)
    } else if now.pause_in_calls && now.others_have_microphone {
        Some(Reason::Call)
    } else {
        None
    }
}

/// [`reason`], as things stand.
fn hold_back(app: &AppHandle, config: &Config) -> Option<Reason> {
    let state = app.state::<AppState>();
    let busy = state.voice().lock().unwrap().is_some()
        || state.capture().lock().unwrap().is_some()
        || state.dictation().lock().unwrap().is_some()
        || state.listen().lock().unwrap().is_some();
    reason(Now {
        enabled: config.enabled,
        remote: state.is_remote(),
        unlocked: state.service().get().is_some_and(|v| v.is_unlocked()),
        busy,
        talking: crate::talk::is_talking(),
        pause_in_calls: config.pause_in_calls,
        others_have_microphone: app.state::<crate::micwatch::MicWatch>().others_using(),
    })
}

/// Listening, and why not when it is not.
#[derive(Default)]
struct Listener {
    /// The task that listens, from the moment it is started until it is
    /// stopped or has stopped by itself.
    task: Option<tauri::async_runtime::JoinHandle<()>>,
    /// The microphone is open and the names are being listened for.
    hearing: bool,
    /// Why it could not listen when it last tried, in words for Settings.
    problem: Option<String>,
    /// Not tried again before this, after it could not listen.
    retry_at: Option<Instant>,
}

static LISTENER: Mutex<Listener> =
    Mutex::new(Listener { task: None, hearing: false, problem: None, retry_at: None });

fn listener() -> MutexGuard<'static, Listener> {
    LISTENER.lock().unwrap_or_else(|e| e.into_inner())
}

/// Look after the listening for the life of the process: start it when it
/// may run and stop it when it may not.
pub fn install(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(LOOK_EVERY).await;
            look(&app);
        }
    });
}

/// Start or stop the listening, as [`hold_back`] says now.
fn look(app: &AppHandle) {
    let config = load_config();
    // Off, as it is unless somebody turned it on: nothing else to look at.
    let held_back = if config.enabled { hold_back(app, &config) } else { Some(Reason::Off) };
    let mut listener = listener();
    if listener.task.as_ref().is_some_and(|task| task.inner().is_finished()) {
        listener.task = None;
        listener.hearing = false;
    }
    match (held_back, listener.task.is_some()) {
        (Some(_), true) => stop(&mut listener),
        (None, false) => {
            if listener.retry_at.is_some_and(|at| Instant::now() < at) {
                return;
            }
            listener.retry_at = None;
            listener.task = Some(tauri::async_runtime::spawn(listen(app.clone(), config)));
        }
        _ => {}
    }
}

fn stop(listener: &mut Listener) {
    if let Some(task) = listener.task.take() {
        task.abort();
    }
    listener.hearing = false;
}

/// Could not listen: say why, and try again later.
fn refused(why: String) {
    tracing::info!(problem = %why, "wake word: not listening");
    let mut listener = listener();
    listener.problem = Some(why);
    listener.hearing = false;
    listener.retry_at = Some(Instant::now() + RETRY_AFTER);
}

/// Listen until the name is heard, then hand over to the talk key's
/// conversation. Aborted when listening must stop; the microphone and the
/// listener go with it.
async fn listen(app: AppHandle, config: Config) {
    let service = app.state::<AppState>().service();
    let mut names = names_in(&config.names);
    // No names chosen: the assistant's own, which may change while it listens.
    let own_name = names.is_empty();
    if own_name {
        names = names_in(&assistant_name(&service).await);
    }
    if names.is_empty() {
        refused(
            "There is no name to listen for: give the assistant one, or choose one here.".into(),
        );
        return;
    }
    let WakeLink { input, mut woken } = match service.wake_open(names.clone()).await {
        Ok(link) => link,
        Err(e) => return refused(e.message),
    };
    let sink: EngineSink = Arc::new(move |event| match event {
        EngineEvent::Microphone(block) => {
            let _ = input.send(block);
        }
        EngineEvent::Failed(message) => {
            tracing::info!(error = %message, "wake word: the microphone failed");
        }
        EngineEvent::Played(_) => {}
    });
    let options = EngineOptions { microphone: true, speaker: false, echo_cancellation: false };
    let engine = match AudioEngine::start(options, sink).await {
        Ok(engine) => engine,
        Err(e) => return refused(e.to_string()),
    };
    {
        let mut listener = listener();
        listener.hearing = true;
        listener.problem = None;
    }
    let mut name_check = tokio::time::interval(NAME_CHECK);
    name_check.tick().await;
    loop {
        tokio::select! {
            heard = woken.recv() => {
                let Some(Woken { rest }) = heard else { break };
                // Something may have taken the microphone in the moment it
                // took to hear the name: what was said was for that.
                if hold_back(&app, &load_config()).is_some() {
                    continue;
                }
                drop(engine);
                listener().hearing = false;
                crate::talk::wake(&app, rest);
                return;
            }
            _ = name_check.tick(), if own_name => {
                // Renamed: [`look`] starts it again, listening for the new one.
                if names_in(&assistant_name(&service).await) != names {
                    drop(engine);
                    listener().hearing = false;
                    return;
                }
            }
        }
    }
    // The recogniser went away; tried again in a while.
    drop(engine);
    refused("Listening for the name stopped unexpectedly.".into());
}

// ---- commands ----------------------------------------------------------------

/// What Settings draws for the wake word.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeSettings {
    pub enabled: bool,
    pub names: String,
    /// The assistant's own name, which is listened for when `names` is
    /// empty. Empty when the vault is locked or the assistant has none.
    pub assistant_name: String,
    pub pause_in_calls: bool,
    /// The microphone is open and the names are being listened for.
    pub listening: bool,
    /// Why it could not listen when it last tried.
    pub problem: Option<String>,
    /// Why it is not listening right now, while switched on: a moment that
    /// will pass, unlike `problem`.
    pub waiting: Option<&'static str>,
}

impl Reason {
    /// Why it is waiting, as Settings says it. `None` for being switched
    /// off, which the switch says.
    fn waiting(self) -> Option<&'static str> {
        match self {
            Reason::Off => None,
            Reason::Remote => {
                Some("Not listening: this window is looking at a vault on another computer.")
            }
            Reason::Locked => Some("Not listening while the vault is locked."),
            Reason::Busy => Some("Waiting while the microphone is in use here."),
            Reason::Talking => Some("Waiting for the conversation to end."),
            Reason::Call => Some("Waiting while another application has the microphone."),
        }
    }
}

/// What the assistant is called in Settings → Assistant, or nothing while the
/// vault is locked.
async fn assistant_name(service: &karmayoga_service::service::Service) -> String {
    service.on_vault(|vault| vault.agent_settings()).await.map(|s| s.name).unwrap_or_default()
}

async fn settings(app: &AppHandle) -> WakeSettings {
    let config = load_config();
    let assistant_name = assistant_name(&app.state::<AppState>().service()).await;
    let waiting = hold_back(app, &config).and_then(Reason::waiting);
    let listener = listener();
    WakeSettings {
        enabled: config.enabled,
        names: config.names,
        assistant_name,
        pause_in_calls: config.pause_in_calls,
        listening: listener.hearing,
        problem: if config.enabled { listener.problem.clone() } else { None },
        waiting,
    }
}

#[tauri::command]
pub async fn wake_settings(app: AppHandle) -> CommandResult<WakeSettings> {
    Ok(settings(&app).await)
}

/// Switch the wake word on or off, or change the names it answers to or
/// whether it stops for calls. Listening starts again at once with what
/// was chosen.
#[tauri::command]
pub async fn set_wake_settings(
    app: AppHandle,
    enabled: bool,
    names: Option<String>,
    pause_in_calls: Option<bool>,
) -> CommandResult<WakeSettings> {
    let mut config = load_config();
    config.enabled = enabled;
    if let Some(names) = names {
        let list = names_in(&names);
        if list.len() > MAX_NAMES {
            return Err(CommandError::new("invalid", format!("at most {MAX_NAMES} names")));
        }
        if list.iter().any(|n| n.chars().count() > MAX_NAME_CHARS) {
            return Err(CommandError::new(
                "invalid",
                format!("a name is at most {MAX_NAME_CHARS} characters"),
            ));
        }
        config.names = list.join(", ");
    }
    config.pause_in_calls = pause_in_calls.unwrap_or(config.pause_in_calls);
    save_config(&config).map_err(|e| CommandError::new("wake", e))?;
    {
        // What was listening listened for the old names, and a refusal is
        // worth trying again now that something has changed.
        let mut listener = listener();
        stop(&mut listener);
        listener.problem = None;
        listener.retry_at = None;
    }
    look(&app);
    Ok(settings(&app).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn free() -> Now {
        Now { enabled: true, unlocked: true, pause_in_calls: true, ..Now::default() }
    }

    #[test]
    fn it_listens_only_when_on_unlocked_here_and_nothing_else_needs_the_microphone() {
        assert_eq!(reason(free()), None);
        assert_eq!(reason(Now { enabled: false, ..free() }), Some(Reason::Off));
        assert_eq!(reason(Now { remote: true, ..free() }), Some(Reason::Remote));
        assert_eq!(reason(Now { unlocked: false, ..free() }), Some(Reason::Locked));
        assert_eq!(reason(Now { busy: true, ..free() }), Some(Reason::Busy));
        assert_eq!(reason(Now { talking: true, ..free() }), Some(Reason::Talking));
    }

    #[test]
    fn a_call_stops_it_unless_that_is_switched_off() {
        let call = Now { others_have_microphone: true, ..free() };
        assert_eq!(reason(call), Some(Reason::Call));
        assert_eq!(reason(Now { pause_in_calls: false, ..call }), None);
    }

    #[test]
    fn names_are_split_at_commas() {
        assert_eq!(names_in("Siri"), ["Siri"]);
        assert_eq!(names_in(" Jarvis ,Javis,, "), ["Jarvis", "Javis"]);
        assert!(names_in("  ").is_empty());
    }

    #[test]
    fn it_is_off_until_switched_on_and_stops_for_calls() {
        let config = Config::default();
        assert!(!config.enabled);
        assert!(config.pause_in_calls);
        assert!(config.names.is_empty());
        let saved: Config = serde_json::from_str(r#"{"enabled": true, "names": "Siri"}"#).unwrap();
        assert!(saved.enabled && saved.pause_in_calls);
        assert_eq!(saved.names, "Siri");
    }
}
