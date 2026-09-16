//! Real end-to-end verification of meeting notes: real `cpal` capture
//! against real PipeWire devices, driven through
//! [`everyday_app_lib::meeting::begin_headless`] -- the same window-free
//! core `meeting_start` and "Always" mode's `start_automatic` call -- into
//! the real spool (`everyday_service::meeting::spool`) and the real
//! pipeline (`everyday_service::meeting::pipeline::run_recording`).
//!
//! `crates/everyday-service/tests/meeting_pipeline_e2e.rs` already proved
//! the pipeline itself (VAD, local Parakeet, echo removal, embedding,
//! clustering) against a file-backed [`pipeline::AudioSource`]. Nothing
//! before this file had exercised real capture -> service commands ->
//! spool -> pipeline -> note together, or the watcher's "Always" mode
//! through the shell's own handler, or auto-stop, or crash recovery.
//!
//! `#[ignore]`, gated a second time by `EVERYDAY_TEST_AUDIO_E2E=1` -- the
//! same two-gate shape `meeting_pipeline_e2e.rs` and `speech_real.rs` use:
//! this drives real hardware, changes (and restores) this machine's default
//! PipeWire sink, and runs minutes of real local inference, and must never
//! run by surprise under a plain `cargo test`.
//!
//! # Audio
//!
//! [`AudioRig`] creates one `module-null-sink` (`pactl load-module
//! module-null-sink ...`), points it at *only* the system/loopback track by
//! setting it as the default sink for the life of the rig, and restores
//! whatever this machine's default sink was before, on drop -- including on
//! a panic, since a `Drop` runs during unwinding. The default *source* (the
//! microphone) is never touched: this machine's real input device (however
//! silent) is used as-is for the mic track, per this task's own allowance
//! to accept a silent mic rather than build a virtual one -- the mic track
//! never enters diarisation or clustering anyway (see
//! `docs/plans/meeting-notes.md`'s "Who said what": "Mic track -> the vault
//! owner. No inference."), so a silent one proves everything the mic track
//! is actually for.
//!
//! Two voices are synthesised with `piper` (`~/.local/bin/piper`) against
//! `en_US-amy-medium` and `en_US-ryan-high`, resampled to 16 kHz mono with
//! `ffmpeg`, and played into the null sink with `paplay --device=...`
//! while `cpal` captures its monitor through the same loopback path
//! `capture.rs` uses in production -- `paplay`, not `pw-play`, and always
//! targeted at the null sink by name rather than left to resolve a
//! (possibly stale) default, per this machine's own quirk noted in
//! `capture.rs`'s `real_loopback_hears_a_played_tone` doc.
//!
//! # Summarising: stubbed
//!
//! There is no local LLM on this machine and the `OPENAI_API_KEY` in the
//! environment is invalid (401), so [`chat_stub`] runs a tiny local
//! OpenAI-compatible HTTP server (axum, already a workspace dev-dependency)
//! that answers `POST /chat/completions` with a fixed, heading-correct
//! note, and the assistant's `provider_config.base_url` is pointed at it --
//! a loopback address needs no key (`Provider::needs_key`). Everything
//! upstream of that one call -- capture, the spool, real VAD, real local
//! Parakeet recognition, echo removal, real speaker embedding and
//! clustering -- runs for real. This is stated here once; nowhere below
//! pretends otherwise.
//!
//! # Running
//!
//! ```text
//! cd crates/everyday-app
//! EVERYDAY_TEST_AUDIO_E2E=1 cargo test --test meeting_e2e -- --ignored --test-threads=1
//! ```
//!
//! `--test-threads=1`: every scenario here that opens real capture takes
//! the same [`AUDIO_LOCK`] first, so they cannot race each other's use of
//! this machine's default sink even under the default multi-threaded
//! runner, but serialising the whole file is simpler to reason about and
//! costs nothing this harness was not already going to spend serially.

// No `#[cfg(feature = "speech")]` gate here, unlike
// `meeting_pipeline_e2e.rs` in `everyday-service`: `everyday-app` always
// depends on `everyday-service` with that feature on (see its own
// `Cargo.toml` comment), so it is never optional in this crate.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use everyday_app_lib::SessionHandle;
use everyday_app_lib::capture::{self, CaptureHandle, RecordingHandle, RecordingSink};
use everyday_app_lib::meeting::begin_headless;

use everyday_core::agent::{AgentSettings, LLMModelConfig, LLMProviderConfig, Provider};
use everyday_core::calendar::{
    AccountCalendarSource, AccountSyncCursor, Calendar, CalendarOrigin, Event, EventStatus,
};
use everyday_core::id::{AccountId, EventId};
use everyday_core::meeting::{
    Attribution, LocalModel, MeetingSettings, Offer, Recording, SAMPLE_RATE, Stage,
    TranscriberConfig,
};
use everyday_core::purpose::{Purpose, Role};
use everyday_core::{CalendarId, RecordingId, Vault, VaultConfig};
use everyday_service::error::CommandResult;
use everyday_service::events::{EventSink, MeetingOffer};
use everyday_service::meeting::{models, watch};
use everyday_service::{Ctx, Service};
use jiff::Timestamp;
use serde_json::json;

/// Serialises every scenario in this file that touches real audio hardware
/// or this machine's default PipeWire sink -- see this module's own doc on
/// why one lock across the whole file, rather than one per scenario, is the
/// simpler and equally correct choice here.
static AUDIO_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const PASSWORD: &str = "correct horse battery staple";

fn enabled() -> bool {
    std::env::var("EVERYDAY_TEST_AUDIO_E2E").as_deref() == Ok("1")
}

/// Skip with a plain reason -- the two-gate shape every real-hardware/real-
/// model test in this workspace uses (see `speech_real.rs`,
/// `meeting_pipeline_e2e.rs`).
macro_rules! require_e2e {
    () => {
        if !enabled() {
            eprintln!("skipped: set EVERYDAY_TEST_AUDIO_E2E=1 to run this against real hardware");
            return;
        }
    };
}

fn scratch_root() -> PathBuf {
    match std::env::var("TMPDIR") {
        Ok(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var("HOME").expect("HOME should be set"))
            .join(".cache/everyday-test-tmp/e2e"),
    }
}

// ============================================================================
// PipeWire: a throwaway null sink, and restoring exactly what was there
// ============================================================================

fn pactl(args: &[&str]) -> String {
    let out = Command::new("pactl").args(args).output().expect("pactl should be on PATH");
    assert!(
        out.status.success(),
        "pactl {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A `module-null-sink`, made the default sink for its own lifetime, and
/// unmade on drop -- including a panicking drop, since scenario code between
/// construction and drop runs entirely under `?`/`assert!`. Only the
/// *sink* is touched; see this module's doc on why the microphone is left
/// exactly as this machine has it.
struct AudioRig {
    sink_name: String,
    module_id: String,
    orig_sink: String,
    orig_source: String,
}

impl AudioRig {
    fn start(tag: &str) -> Self {
        let orig_sink = pactl(&["get-default-sink"]);
        let orig_source = pactl(&["get-default-source"]);
        let sink_name = format!("everyday_e2e_{tag}_{}", std::process::id());
        let module_id = pactl(&[
            "load-module",
            "module-null-sink",
            &format!("sink_name={sink_name}"),
            "sink_properties=device.description=EverydayE2E",
        ]);
        pactl(&["set-default-sink", &sink_name]);
        eprintln!(
            "[audio] default sink was {orig_sink:?}, source {orig_source:?}; using null sink \
             {sink_name:?} (module {module_id}) for the system track"
        );
        Self { sink_name, module_id, orig_sink, orig_source }
    }

    /// Play a WAV file into this rig's sink, targeted by name -- not left to
    /// resolve a default -- and return the child so the caller can wait for
    /// it (a `paplay` that outlives the scenario would otherwise keep
    /// writing to a sink this rig is about to unload).
    fn play(&self, wav: &Path) -> Child {
        Command::new("paplay")
            .arg(format!("--device={}", self.sink_name))
            .arg(wav)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("paplay should be on PATH")
    }
}

impl Drop for AudioRig {
    /// Unload the null sink *before* trying to set anything back, in that
    /// order and not the reverse -- found the hard way, on this machine:
    /// its real default (`auto_null`, a `module-always-sink` dummy, per
    /// this task's own environment note) is itself destroyed the moment a
    /// person-set default points somewhere else, and recreated only once
    /// nothing else claims to be the default. Calling `set-default-sink
    /// auto_null` while this rig's own sink was still the default failed
    /// outright ("No such entity") for exactly that reason. Unloading
    /// first gives PipeWire's own module a chance to bring `auto_null`
    /// back on its own, which it does; the explicit `set-default-sink`
    /// below is then only needed for a machine whose original default was
    /// a real, still-present device, where it is a no-op if PipeWire
    /// already restored it and a fix if it did not.
    fn drop(&mut self) {
        let _ = Command::new("pactl").args(["unload-module", self.module_id.trim()]).status();
        // PipeWire's own reconciliation (recreating `auto_null`, in
        // particular) is not synchronous with `unload-module` returning.
        std::thread::sleep(Duration::from_millis(500));

        for attempt in 0..5 {
            let sink = pactl(&["get-default-sink"]);
            let source = pactl(&["get-default-source"]);
            if sink == self.orig_sink && source == self.orig_source {
                eprintln!("[audio] restored default sink={sink:?} source={source:?}");
                return;
            }
            if attempt == 0 {
                let _ = Command::new("pactl").args(["set-default-sink", &self.orig_sink]).status();
                let _ =
                    Command::new("pactl").args(["set-default-source", &self.orig_source]).status();
            }
            std::thread::sleep(Duration::from_millis(300));
        }

        let sink = pactl(&["get-default-sink"]);
        let source = pactl(&["get-default-source"]);
        eprintln!("[audio] final state: sink={sink:?} source={source:?}");
        assert_eq!(sink, self.orig_sink, "default sink was not restored");
        assert_eq!(source, self.orig_source, "default source was not restored");
    }
}

// ============================================================================
// Piper: synthesising the call
// ============================================================================

fn piper_bin() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME should be set")).join(".local/bin/piper")
}

fn voices_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME should be set"))
        .join(".cache/everyday-test-tmp/speech/piper-voices")
}

#[derive(Clone, Copy)]
enum Voice {
    Amy,
    Ryan,
}

impl Voice {
    fn model_path(self) -> PathBuf {
        let name = match self {
            Voice::Amy => "en_US-amy-medium.onnx",
            Voice::Ryan => "en_US-ryan-high.onnx",
        };
        voices_dir().join(name)
    }
}

/// Synthesise `text` with `voice`, resampled to 16 kHz mono -- the same
/// subprocess pipeline `meeting_pipeline_e2e.rs`'s own `synth` uses, copied
/// rather than shared: that file lives in `everyday-service` and this one
/// in `everyday-app`, and duplicating twenty lines of test-only tooling
/// across the crate boundary is cheaper than inventing a place for both to
/// depend on.
fn synth(voice: Voice, text: &str, scratch: &Path, tag: &str) -> Vec<i16> {
    let raw = scratch.join(format!("{tag}.raw.wav"));
    let resampled = scratch.join(format!("{tag}.16k.wav"));

    let mut child = Command::new(piper_bin())
        .arg("-m")
        .arg(voice.model_path())
        .arg("-c")
        .arg(voice.model_path().with_extension("onnx.json"))
        .arg("-f")
        .arg(&raw)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("piper should be runnable at ~/.local/bin/piper");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(text.as_bytes())
        .expect("writing text to piper's stdin");
    let status = child.wait().expect("waiting for piper");
    assert!(status.success(), "piper failed on {text:?}");

    let status = Command::new("ffmpeg")
        .args(["-y", "-i"])
        .arg(&raw)
        .args(["-ar", "16000", "-ac", "1", "-sample_fmt", "s16"])
        .arg(&resampled)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("ffmpeg should be on PATH");
    assert!(status.success(), "ffmpeg resample failed on {text:?}");

    let mut reader = hound::WavReader::open(&resampled).expect("reading resampled wav");
    let spec = reader.spec();
    assert_eq!(spec.sample_rate, SAMPLE_RATE);
    assert_eq!(spec.channels, 1);
    reader.samples::<i16>().map(|s| s.expect("sample")).collect()
}

fn write_wav(path: &Path, samples: &[i16]) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).expect("writing wav");
    for &s in samples {
        writer.write_sample(s).expect("writing sample");
    }
    writer.finalize().expect("finalizing wav");
}

/// A design-sync call: the owner (mic track, silent on this machine -- see
/// this module's doc) and two attendees, Priya and Sam, on the system
/// track. The exact conversation `meeting_pipeline_e2e.rs` already proved
/// clusters as two distinct voices against real local Parakeet and real
/// embeddings; reused verbatim here rather than invented afresh, so a
/// mismatch between the two E2E harnesses is not the reason this one
/// behaves differently.
const SCRIPT: &[(&str, Voice, &str)] = &[
    (
        "owner",
        Voice::Amy,
        "Thanks for joining, let's talk through the launch plan for the new pricing page.",
    ),
    (
        "Priya",
        Voice::Ryan,
        "Design is done on my side. The only open question is whether we ship the annual discount at launch, or hold it back a week.",
    ),
    (
        "Sam",
        Voice::Amy,
        "I can ship either way, but if we hold the discount back a week, I would rather use that week for load testing checkout.",
    ),
    (
        "owner",
        Voice::Amy,
        "Let's hold the discount back then. Sam, can you own the load test and report back by Thursday?",
    ),
    ("Sam", Voice::Amy, "Yes, I will have numbers by Thursday morning."),
    (
        "Priya",
        Voice::Ryan,
        "I will get the launch email drafted this week, so it is ready whichever day we pick.",
    ),
    ("owner", Voice::Amy, "Sounds good. What is still open after that?"),
    ("Priya", Voice::Ryan, "We have not decided who announces it in the public changelog."),
    ("Sam", Voice::Amy, "I can write the changelog entry once the email copy is finished."),
    (
        "owner",
        Voice::Amy,
        "Great, let's call that settled for today. Thanks both, talk on Thursday.",
    ),
];

const GAP_MS: u64 = 700;

struct Conversation {
    /// Priya + Sam, played into the system/loopback sink.
    system: PathBuf,
    attendees: Vec<String>,
    total_secs: f64,
}

/// Synthesise [`SCRIPT`], write the system track's audio to one WAV file in
/// `scratch`, and return it plus the attendee list. The owner's own lines
/// are synthesised too (`owner.wav` files land in `scratch`, unused) only
/// in the sense that skipping them would leave the timing of the two
/// tracks -- which line follows which -- undocumented; nothing plays them,
/// since the mic track is left to this machine's real, silent microphone.
fn build_conversation(scratch: &Path) -> Conversation {
    let mut system = Vec::new();
    let mut attendees = std::collections::BTreeSet::new();
    let gap = vec![0i16; (GAP_MS * u64::from(SAMPLE_RATE) / 1000) as usize];

    for (i, (who, voice, text)) in SCRIPT.iter().enumerate() {
        let samples = synth(*voice, text, scratch, &format!("{i:02}-{who}"));
        if *who != "owner" {
            attendees.insert(*who);
            system.extend_from_slice(&samples);
            system.extend_from_slice(&gap);
        }
    }

    let total_secs = system.len() as f64 / f64::from(SAMPLE_RATE);
    let path = scratch.join("system.wav");
    write_wav(&path, &system);
    Conversation {
        system: path,
        attendees: attendees
            .into_iter()
            .map(|n| format!("{n} <{}@example.com>", n.to_lowercase()))
            .collect(),
        total_secs,
    }
}

// ============================================================================
// The assistant stub
// ============================================================================

/// A fixed, heading-correct note -- everything upstream of the assistant is
/// real; this one call is not. See this module's doc.
const CANNED_NOTE: &str = "## Summary\n\
The team discussed the pricing page launch, the annual discount, and load testing checkout.\n\n\
## Decisions\n\
- Hold the discount back a week for load testing checkout.\n\n\
## Action items\n\
- [ ] Sam — load test checkout — Thursday\n\n\
## Open questions\n\
- Who announces it in the changelog\n";

/// Run a local OpenAI-compatible `/chat/completions` stub and return its
/// base URL (no `/v1`: `rig-agent`'s OpenAI client appends `/chat/completions`
/// directly to whatever base URL it is given -- see
/// `AssistantSummariser::attempt` and `llm::client`).
async fn chat_stub() -> String {
    async fn completions() -> axum::Json<serde_json::Value> {
        axum::Json(json!({
            "id": "chatcmpl-stub",
            "object": "chat.completion",
            "created": 0,
            "model": "stub-model",
            "system_fingerprint": null,
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": CANNED_NOTE },
                "logprobs": null,
                "finish_reason": "stop",
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 },
        }))
    }
    let app = axum::Router::new().route("/chat/completions", axum::routing::post(completions));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    format!("http://{addr}")
}

// ============================================================================
// Vault, calendar, event, role
// ============================================================================

fn configure_assistant(vault: &Vault, base_url: &str) {
    let settings = AgentSettings {
        enabled: true,
        assistant_model: LLMModelConfig {
            model: "stub-model".to_string(),
            ..LLMModelConfig::assistant()
        },
        provider_config: LLMProviderConfig {
            provider: Provider::OpenAi,
            base_url: Some(base_url.to_string()),
        },
        ..AgentSettings::default()
    };
    // A loopback base URL needs no key -- `Provider::needs_key` -- so
    // nothing is stored here.
    vault.save_agent_settings(&settings).unwrap();
}

/// An account calendar with a role, so the note this call produces has
/// something to be filed under -- `docs/plans/meeting-notes.md`'s "a
/// meeting note is filed under its calendar's role automatically". Mirrors
/// `watch.rs`'s own `account_calendar` test helper: only an account
/// calendar's origin is one `Vault::sync_account_calendar` accepts.
fn calendar_with_role(vault: &Vault) -> (Calendar, Role) {
    let role = Role::new("Work");
    vault.save_role(&role).unwrap();
    let mut calendar = Calendar::subscribed("Work", "https://example.com/work.ics");
    calendar.origin = CalendarOrigin::Account {
        account_id: AccountId::new(),
        remote_id: "primary".into(),
        remote_name: "Work".into(),
        source: AccountCalendarSource::Google,
    };
    calendar.role_id = Some(role.id);
    vault.save_calendar(&calendar).unwrap();
    (calendar, role)
}

/// An online call, starting `offset_secs` from now, with a Zoom join link
/// and the given attendees -- `everyday_core::meeting::detect::is_online_call`
/// needs a meeting host in the description, and `EventRef::from_event`
/// pulls it into `join_url` from there.
fn online_event(calendar_id: CalendarId, offset_secs: i64, attendees: &[String]) -> Event {
    let now = Timestamp::now();
    let start = Timestamp::from_second(now.as_second() + offset_secs).unwrap();
    let end = Timestamp::from_second(start.as_second() + offset_secs.max(0) + 1_800).unwrap();
    let zoned = now.to_zoned(jiff::tz::TimeZone::system());
    Event {
        id: EventId::new(),
        calendar_id,
        uid: format!("e2e-{}", EventId::new()),
        title: "Design sync".into(),
        description: "Join: https://zoom.us/j/1234567890".into(),
        location: String::new(),
        start,
        end,
        local_date: zoned.date(),
        end_date: zoned.date(),
        tz: "UTC".into(),
        all_day: false,
        status: EventStatus::Confirmed,
        organizer: "You <you@example.com>".into(),
        attendees: attendees.to_vec(),
        url: String::new(),
        busy: true,
        updated_at: now,
    }
}

fn seed_event(vault: &Vault, calendar_id: CalendarId, event: &Event) {
    vault
        .sync_account_calendar(
            calendar_id,
            std::slice::from_ref(event),
            &[],
            AccountSyncCursor::default(),
        )
        .unwrap();
}

// ============================================================================
// Models
// ============================================================================

async fn ensure_installed(id: &str) {
    if models::status(id).is_some_and(|s| s.installed) {
        eprintln!("{id}: already installed");
        return;
    }
    eprintln!("{id}: downloading...");
    models::start_download(id).unwrap_or_else(|e| panic!("{id}: {e}"));
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let status = models::status(id).unwrap_or_else(|| panic!("{id} is not in the catalogue"));
        if let Some(error) = &status.error {
            panic!("{id} failed to download: {error}");
        }
        if status.installed {
            eprintln!("{id}: installed");
            break;
        }
    }
}

async fn meeting_settings_local_parakeet() -> MeetingSettings {
    ensure_installed(models::SPEECH_KIT_ID).await;
    ensure_installed("parakeetV3").await;
    MeetingSettings {
        enabled: true,
        transcriber: Some(TranscriberConfig::Local { model: LocalModel::ParakeetV3 }),
        voiceprints: false,
        ..MeetingSettings::default()
    }
}

// ============================================================================
// Driving a recording through the real service commands
// ============================================================================

/// A local session and a capture slot, standing in for `AppState`'s own
/// `session()`/`capture()` -- see `begin_headless`'s doc for why plain
/// arguments rather than a whole `AppState` are all it needs.
struct Harness {
    service: Arc<Service>,
    vault: Arc<Vault>,
    capture: Mutex<Option<CaptureHandle>>,
}

impl Harness {
    fn new(vault_dir: &Path) -> Self {
        let service = Arc::new(Service::new());
        let vault = everyday_vault::create(
            vault_dir,
            VaultConfig {
                password: Some(PASSWORD.into()),
                kdf: everyday_core::crypto::KdfParams::insecure_fast(),
                ..Default::default()
            },
        )
        .unwrap();
        let vault = service.set(vault);
        Self { service, vault, capture: Mutex::new(None) }
    }

    fn session(&self) -> SessionHandle {
        SessionHandle::Local(self.service.clone())
    }

    async fn call(&self, name: &str, args: serde_json::Value) -> CommandResult<serde_json::Value> {
        self.service.call(Ctx::local(), name, args).await
    }
}

/// Begin recording through [`begin_headless`] -- the identical, window-free
/// core `meeting_start` and "Always" mode's `start_automatic` both call --
/// bound to `event_id`.
async fn begin(harness: &Harness, event_id: Option<EventId>, automatic: bool) -> Recording {
    begin_headless(
        harness.session(),
        None,
        &harness.capture,
        event_id,
        None,
        None,
        automatic,
        None, // no Tauri window: the whole point of this harness.
    )
    .await
    .unwrap_or_else(|e| panic!("begin_headless failed: {}", e.message))
}

fn take_capture(harness: &Harness) -> CaptureHandle {
    harness.capture.lock().unwrap().take().expect("capture should have started")
}

async fn wait_for_terminal_stage(vault: &Vault, id: RecordingId, timeout: Duration) -> Recording {
    let deadline = Instant::now() + timeout;
    loop {
        let recording = vault.recording(id).unwrap();
        if !matches!(recording.stage, Stage::Recording) && !recording.stage.is_pending() {
            return recording;
        }
        if Instant::now() > deadline {
            panic!(
                "recording {id} did not reach a terminal stage within {timeout:?}; stage={:?}",
                recording.stage
            );
        }
        eprintln!("  ...stage={:?}", recording.stage);
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

// ============================================================================
// Scenario 1: real capture -> real spool -> real pipeline -> a note
// ============================================================================

/// The full path: a calendar event with attendees and a Zoom link, a
/// two-voice call played into a real PipeWire null sink while `cpal`
/// captures its monitor through `capture::start`, stopped and handed to
/// the real pipeline, transcribed by real local Parakeet, summarised by
/// the stub (see this module's doc).
#[tokio::test]
#[ignore]
async fn local_parakeet_hears_a_real_call_through_real_capture_and_writes_a_note() {
    require_e2e!();
    assert!(piper_bin().is_file(), "expected piper at {}", piper_bin().display());
    let _audio = AUDIO_LOCK.lock().await;

    let root = scratch_root().join(format!("full-pipeline-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let scratch = root.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();

    eprintln!("[full] synthesising the call with piper...");
    let call = build_conversation(&scratch);
    eprintln!("[full] system track: {:.1}s", call.total_secs);
    assert!(call.total_secs >= 20.0, "expected a substantial call, not a snippet");

    let stub_url = chat_stub().await;
    eprintln!("[full] assistant stub listening at {stub_url}");

    let harness = Harness::new(&root.join("vault"));
    configure_assistant(&harness.vault, &stub_url);
    let settings = meeting_settings_local_parakeet().await;
    harness.vault.save_meeting_settings(&settings).unwrap();

    let (calendar, role) = calendar_with_role(&harness.vault);
    let event = online_event(calendar.id, 0, &call.attendees);
    seed_event(&harness.vault, calendar.id, &event);

    // The system-track sink is set up only now, right before capture
    // starts, and torn down (restoring this machine's own default) the
    // moment this scenario's scope ends.
    let rig = AudioRig::start("full");

    eprintln!("[full] starting capture through begin_headless (meeting_start's own path)...");
    let recording = begin(&harness, Some(event.id), false).await;
    assert_eq!(recording.event.as_ref().unwrap().uid, event.uid);

    eprintln!("[full] playing the call into the system track...");
    let mut player = rig.play(&call.system);
    let status = player.wait().expect("waiting for paplay");
    assert!(status.success(), "paplay failed");
    // A little more capture after the last word, the way a real call's
    // trailing silence would look, before this scenario stops it by hand
    // (auto-stop is not what this scenario is proving).
    tokio::time::sleep(Duration::from_secs(2)).await;

    eprintln!("[full] stopping capture through meeting_stop's own path...");
    let handle = take_capture(&harness);
    tokio::task::spawn_blocking(move || handle.stop(false)).await.unwrap();
    drop(rig); // restore the default sink before the (possibly slow) pipeline runs.

    eprintln!("[full] waiting for the pipeline...");
    let done =
        wait_for_terminal_stage(&harness.vault, recording.id, Duration::from_secs(300)).await;
    assert_eq!(done.stage, Stage::Done, "expected Done, got {:?}", done.stage);
    assert!(done.chunks.is_empty(), "chunks should be cleared at Done");

    let spool_dir =
        root.join("vault").join("spool").join("recordings").join(recording.id.to_string());
    assert!(
        !spool_dir.exists(),
        "the spool directory should have been removed: {}",
        spool_dir.display()
    );

    let note_id = done.note_id.expect("a note was written");
    let note = harness.vault.note(note_id).unwrap();
    let body = note.body.to_markdown();
    println!("\n===== note =====\n{body}\n");
    assert!(body.contains("**When:**"), "missing the details block: {body}");
    assert!(body.contains(&event.title), "missing the event's own title: {body}");
    assert!(body.contains("**Invited:**"), "missing the attendees line: {body}");
    assert!(body.contains("zoom.us"), "missing the join link: {body}");
    assert!(body.contains("## Summary"), "missing the template's own heading: {body}");
    assert!(body.contains("## Decisions"), "missing the template's own heading: {body}");
    assert_eq!(
        note.purpose,
        Some(Purpose::Role { id: role.id }),
        "should be filed under the calendar's role"
    );

    let transcript =
        harness.vault.transcript_for_note(note_id).unwrap().expect("a transcript was saved");
    println!("===== transcript ({} segments) =====", transcript.segments.len());
    for seg in &transcript.segments {
        println!("{}", transcript.line(seg));
    }
    let system_speakers: Vec<_> =
        transcript.speakers.iter().filter(|s| !matches!(s.how, Attribution::Owner)).collect();
    eprintln!(
        "system-track speakers: {:?}",
        system_speakers.iter().map(|s| &s.label).collect::<Vec<_>>()
    );
    assert!(
        system_speakers.len() >= 2,
        "expected at least two system-track speakers (Priya, Sam), got {}",
        system_speakers.len()
    );

    // Only words from the *system*-track lines (Priya's and Sam's) --
    // "pricing" is said only in the owner's opening line, which lives on
    // the mic track this scenario leaves genuinely silent (see this
    // module's doc), so it is never expected to show up here.
    let text = transcript.searchable_text().to_lowercase();
    for word in ["discount", "load test", "thursday"] {
        assert!(text.contains(word), "transcript should mention {word:?}: {text}");
    }

    let _ = std::fs::remove_dir_all(&root);
}

// ============================================================================
// Scenario 2: the watcher's Always/Ask modes, and the shell's window-free
// handler
// ============================================================================

#[derive(Clone, Default)]
struct OfferSink {
    offers: Arc<Mutex<Vec<MeetingOffer>>>,
}

impl EventSink for OfferSink {
    fn meeting_offer(&self, offer: MeetingOffer) {
        self.offers.lock().unwrap().push(offer);
    }
}

/// "Always" makes `watch::tick` raise an automatic offer, and
/// [`begin_headless`] -- the same call `everyday-app`'s `on_meeting_offer`
/// makes through `start_automatic` when it sees `automatic: true` -- turns
/// that straight into a running capture, with no press. "Ask" raises an
/// offer with `automatic: false`, which `on_meeting_offer` never passes to
/// `start_automatic` at all (see `meeting::starts_automatically`, unit
/// tested in `everyday-app`'s own `meeting.rs` for the plain decision); this
/// scenario proves the *consequence* of that decision by simply not calling
/// `begin_headless` for it either, and checking nothing started.
#[tokio::test]
#[ignore]
async fn the_watcher_offers_always_mode_starts_capture_and_ask_mode_does_not() {
    require_e2e!();
    let _audio = AUDIO_LOCK.lock().await;

    let root = scratch_root().join(format!("watch-always-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let harness = Harness::new(&root.join("vault"));
    let mut settings = meeting_settings_local_parakeet().await;
    settings.offer = Offer::Always;
    harness.vault.save_meeting_settings(&settings).unwrap();

    let sink = OfferSink::default();
    harness.service.set_events(Arc::new(sink.clone()));

    let (calendar, _role) = calendar_with_role(&harness.vault);
    let always_attendee = vec!["Priya <priya@example.com>".to_string()];
    let always_event = online_event(calendar.id, 0, &always_attendee);
    seed_event(&harness.vault, calendar.id, &always_event);

    eprintln!("[watch] Always: ticking the watcher...");
    watch::tick(&harness.service, &harness.vault);
    let offers = sink.offers.lock().unwrap().clone();
    assert_eq!(offers.len(), 1, "expected exactly one offer");
    assert!(offers[0].automatic, "an Always calendar should offer automatic: true");

    eprintln!(
        "[watch] Always: starting capture through begin_headless, exactly as \
               `on_meeting_offer`/`start_automatic` would..."
    );
    let recording = begin(&harness, Some(offers[0].event_id), true).await;
    assert!(recording.automatic);
    let handle = take_capture(&harness);
    // This scenario only needs to prove capture *started* -- the full
    // capture -> pipeline -> note path is scenario 1's job. Discard rather
    // than finish, so nothing here waits on a transcriber.
    tokio::task::spawn_blocking(move || handle.stop(true)).await.unwrap();

    // ---- Ask mode: a second event, a fresh offer, never automatic ----
    settings.offer = Offer::Ask;
    harness.vault.save_meeting_settings(&settings).unwrap();
    let ask_event = online_event(calendar.id, 60, &always_attendee);
    seed_event(&harness.vault, calendar.id, &ask_event);

    eprintln!("[watch] Ask: ticking the watcher...");
    watch::tick(&harness.service, &harness.vault);
    let offers = sink.offers.lock().unwrap().clone();
    assert_eq!(offers.len(), 2, "the ask-mode event should also be offered");
    let ask_offer = offers.iter().find(|o| o.event_id == ask_event.id).unwrap();
    assert!(!ask_offer.automatic, "an Ask calendar should offer automatic: false");
    assert!(
        harness.capture.lock().unwrap().is_none(),
        "an Ask offer must never start capture on its own"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ============================================================================
// Scenario 3: auto-stop
// ============================================================================

/// A short event, and a shortened auto-stop quiet window -- the "test hook"
/// this scenario needs, threaded through explicitly as
/// [`capture::start`]'s own `auto_stop_quiet` parameter rather than any
/// change to [`capture::AUTO_STOP_QUIET`] itself, which every production
/// caller (`begin_headless`) still uses unchanged.
///
/// Bypasses `begin_headless` on purpose, for the one reason it would
/// otherwise always pass the *production* constant: this is the one
/// scenario in this file that needs a different one. It still goes through
/// the real `begin_recording`/`append`/`finish` commands underneath --
/// only the capture-thread wiring above them is assembled by hand, the
/// same handful of lines `begin_headless` itself runs.
#[tokio::test]
#[ignore]
async fn auto_stop_arms_and_stops_once_both_tracks_are_quiet_past_the_events_end() {
    require_e2e!();
    let _audio = AUDIO_LOCK.lock().await;

    let root = scratch_root().join(format!("auto-stop-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let harness = Harness::new(&root.join("vault"));
    let stub_url = chat_stub().await;
    configure_assistant(&harness.vault, &stub_url);
    let settings = meeting_settings_local_parakeet().await;
    harness.vault.save_meeting_settings(&settings).unwrap();

    let session = harness.session().as_session();
    let value =
        session.call(Ctx::local(), "begin_recording", json!({ "automatic": false })).await.unwrap();
    let recording: Recording = serde_json::from_value(value).unwrap();

    let call_session = harness.session().as_session();
    let caller: Box<capture::CommandCaller> = Box::new(move |name, args| {
        tauri::async_runtime::block_on(call_session.call(Ctx::local(), name, args))
    });
    let sink = Box::new(RecordingSink::new(recording.id, caller));

    let event_end = Timestamp::now().checked_add(Duration::from_secs(3)).unwrap();
    let handle = RecordingHandle {
        id: recording.id,
        title: recording.title.clone(),
        automatic: false,
        template_id: Some(recording.template_id),
        event_end: Some(event_end),
    };
    // Both tracks are silent from the first sample (nothing is played into
    // either device this scenario's real default sink/source happen to be
    // pointed at right now), so the 4s quiet window starts counting
    // essentially from the moment capture opens.
    let quiet = Duration::from_secs(4);
    let capture_handle =
        capture::start(handle, sink, None, None, Some(quiet)).expect("capture should start");

    eprintln!("[auto-stop] waiting for auto_stop_at to arm...");
    let armed_by = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = capture_handle.status()
            && status.auto_stop_at.is_some()
        {
            eprintln!("[auto-stop] armed: {:?}", status.auto_stop_at);
            break;
        }
        assert!(Instant::now() < armed_by, "auto-stop never armed");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    eprintln!("[auto-stop] waiting for the recording to leave Stage::Recording on its own...");
    let vault = harness.vault.clone();
    let id = recording.id;
    let stopped_by = Instant::now() + Duration::from_secs(20);
    loop {
        let current = vault.recording(id).unwrap();
        if !matches!(current.stage, Stage::Recording) {
            eprintln!("[auto-stop] stopped itself: stage={:?}", current.stage);
            break;
        }
        assert!(Instant::now() < stopped_by, "auto-stop never actually stopped the recording");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    // The capture thread has already exited on its own; nothing left to
    // join for this scenario's purposes. Let the pipeline run to Done so
    // the vault directory can be cleaned up without a dangling spool.
    let done = wait_for_terminal_stage(&harness.vault, id, Duration::from_secs(120)).await;
    assert_eq!(done.stage, Stage::Done);
    drop(capture_handle);

    let _ = std::fs::remove_dir_all(&root);
}

// ============================================================================
// Scenario 4: crash recovery
// ============================================================================

fn silence_chunk(n: usize) -> String {
    use base64::Engine;
    let bytes = vec![0u8; n * 2];
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Begin a recording, append two silent chunks, and drop the service
/// without ever calling `finish_recording` -- a kill-style interruption.
/// Reopening the same vault directory under a fresh [`Service`] and calling
/// `unlocked()` (what `everyday-app`'s bootstrap does on every unlock) must
/// find it, move it on, and let the pipeline finish it, per
/// `everyday_service::meeting::spool`'s own "Recovery" doc: a recording
/// still `Stage::Recording` with no chunk appended *in this process* is
/// abandoned, and a fresh process has appended none at all.
#[tokio::test]
#[ignore]
async fn a_crash_mid_recording_is_recovered_on_reopen_and_the_pipeline_finishes_it() {
    require_e2e!();

    let root = scratch_root().join(format!("crash-recovery-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let vault_dir = root.join("vault");

    let stub_url = chat_stub().await;
    let recording_id;
    {
        let harness = Harness::new(&vault_dir);
        configure_assistant(&harness.vault, &stub_url);
        let settings = meeting_settings_local_parakeet().await;
        harness.vault.save_meeting_settings(&settings).unwrap();

        let value = harness.call("begin_recording", json!({ "automatic": false })).await.unwrap();
        let recording: Recording = serde_json::from_value(value).unwrap();
        recording_id = recording.id;

        for seq in 0..2u32 {
            harness
                .call(
                    "append_recording_chunk",
                    json!({
                        "id": recording.id,
                        "track": "mic",
                        "seq": seq,
                        "startMs": u64::from(seq) * 1_000,
                        "pcm": silence_chunk(SAMPLE_RATE as usize),
                    }),
                )
                .await
                .unwrap();
        }

        let mid = harness.vault.recording(recording_id).unwrap();
        assert_eq!(mid.stage, Stage::Recording, "still recording -- never finished");
        assert_eq!(mid.chunks.len(), 2);
        eprintln!(
            "[crash] begun and appended to; dropping the service without finishing (a crash)"
        );
        // `harness` (and its `Arc<Service>`) is dropped at the end of this
        // block -- no `finish_recording`, no graceful shutdown.
    }

    eprintln!("[crash] reopening the same vault under a fresh Service...");
    let vault = everyday_vault::open(&vault_dir).unwrap();
    vault.unlock(Some(PASSWORD)).unwrap();
    let service = Arc::new(Service::new());
    let vault = service.set(vault);
    service.unlocked(); // triggers `meeting::spool::recover`

    let recovering = vault.recording(recording_id).unwrap();
    assert!(
        !matches!(recovering.stage, Stage::Recording),
        "recovery should have moved it out of Stage::Recording, got {:?}",
        recovering.stage
    );
    eprintln!("[crash] recovered: stage={:?}", recovering.stage);

    let done = wait_for_terminal_stage(&vault, recording_id, Duration::from_secs(120)).await;
    assert_eq!(done.stage, Stage::Done, "the recovered pipeline should still reach Done");
    assert!(done.note_id.is_some());

    let _ = std::fs::remove_dir_all(&root);
}
