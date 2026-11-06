//! This machine's screen time: watching which app has focus, sending it to
//! the vault as heartbeats, and the loopback door the browser and GNOME
//! Shell extensions send theirs through.
//!
//! See `docs/plans/screentime.md`.
//!
//! # Three parts
//!
//! - **The watcher thread**: an [`everyday_focus::Watcher`] asked every few
//!   seconds what has focus, how long since the last input, and whether the
//!   screen is locked. In use means focused, not idle past the vault's
//!   `idle_minutes`, not locked. Runs of the same app are joined here and
//!   flushed every thirty seconds as one `record_screen_time` call through
//!   whichever vault the window is looking at (`AppState::session`), so a
//!   window connected to another machine sends there. A failed flush keeps
//!   its runs for the next one, up to a cap. On GNOME on Wayland it stands
//!   aside: the Shell extension reports instead.
//! - **The loopback listener**: [`everyday_server::screentime::start`] on
//!   `127.0.0.1:7399`, with a [`everyday_server::screentime::Forward`] that
//!   calls the same command through the same session.
//! - **Setup**: the machine's own config (`screentime.json` beside
//!   `mcp.json` in the configuration directory: tracking on or off, this
//!   machine's name, the listener's port), installing the GNOME extension
//!   from the copy compiled into this binary (`extensions/gnome`) with its
//!   own `screentime-gnome.json` holding a heartbeats-only token, and
//!   minting a connection code for a browser extension.
//!
//! Tracking is on by default on a machine whose desktop can be watched;
//! the switch in Settings → Screen time turns it off.
//!
//! # Why the machine's settings are not in the vault
//!
//! Whether *this computer* records, and what it is called, are facts about
//! the computer, not the vault: one vault is looked at from a laptop and a
//! desktop, and a window can be pointed at another machine's vault without
//! the laptop becoming that machine. So they live in the configuration
//! directory beside `mcp.json` and `devices.json`, as sharing's and MCP's
//! do, and the vault keeps only what every collector should agree on (idle
//! minutes, whether titles are kept), handed out by `screen_time_collector`.
//!
//! # Runs, not heartbeats every five seconds
//!
//! Sampling every five seconds and sending each sample would be six rows of
//! traffic a minute for one fact. The thread joins consecutive samples of
//! the same app into a run, `{app, name, title, start, end}`, and sends the
//! runs. A switch to another app ends the old run where the new one starts,
//! so time is not lost between samples; a sample with nothing in use ends
//! the run at the last sample that saw it, so time away is never counted.
//! And two samples more than [`MAX_GAP_MS`] apart are never one run: a
//! machine that slept, suspended or sat at a lock screen shows up as a gap,
//! never as one long stretch of whatever was focused before. The thread's
//! own sleep cannot tell it the machine slept -- it simply wakes late --
//! which is why the gap is measured on the wall clock between samples.
//!
//! The extension in `extensions/gnome` joins its samples by exactly the same
//! rule, in JavaScript, so a run means the same thing whoever made it.
//!
//! # Sending never stalls watching
//!
//! A flush is spawned on Tauri's runtime and the thread goes on sampling; a
//! remote vault that takes twenty seconds to time out would otherwise leave
//! a twenty-second hole in the samples, and the gap rule would split the
//! run. At most one flush is in flight. One that fails hands its beats back
//! over a channel and they go out with the next, oldest first; the backlog is
//! capped at [`MAX_PENDING`] heartbeats and [`MAX_PENDING_AGE_SECS`] of age,
//! because a vault locked all weekend should not mean a weekend of memory.
//!
//! # The listener is always on
//!
//! Even with this machine's own tracking off, because a browser on this
//! machine may still be reporting sites, and that is the browser
//! extension's switch, not this one. Turning tracking off *does* refuse the
//! GNOME extension's heartbeats here (they are answered as rejected, so it
//! drops them rather than queueing): on GNOME that extension *is* this
//! machine's tracking, and the switch should mean the same on every
//! desktop.
//!
//! # Tokens
//!
//! The GNOME extension and each browser get a token of their own from the
//! process's one [`everyday_server::Registry`], scoped to
//! [`Scope::Heartbeats`] alone, so a stolen one can add made-up screen time
//! and nothing else. The config remembers each token's registry device id,
//! and installing again or reconnecting a browser revokes the old one after
//! the new one is in place, so the device list never fills with dead tokens
//! and a token handed out once is never valid twice over.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::net::SocketAddr;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use everyday_core::screentime::{CollectorConfig, Heartbeat, IngestReport, SpanKind};
use everyday_focus::{Backend, Focused, Watcher};
use everyday_server::screentime::{DEFAULT_SCREENTIME_PORT, Forward, HeartbeatBody};
use everyday_service::Scope;
use everyday_service::ctx::Ctx;
use everyday_service::error::{CommandError, CommandResult};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Manager, State};

use crate::listener::Listener;
use crate::state::AppState;

/// This machine's screen-time settings, in the configuration directory.
const CONFIG_FILE: &str = "screentime.json";

/// What the GNOME extension reads: see `docs/plans/screentime.md`'s "The
/// GNOME extension's config". The extension looks for it at
/// `$XDG_CONFIG_HOME/everyday/`, which is where
/// [`everyday_vault::config_dir`] is on Linux.
const GNOME_CONFIG_FILE: &str = "screentime-gnome.json";

/// How often the focused window is looked at.
const SAMPLE_EVERY: Duration = Duration::from_secs(5);

/// How often runs are sent.
const FLUSH_EVERY: Duration = Duration::from_secs(30);

/// How often the vault's idle setting is asked for again once known.
const COLLECTOR_EVERY: Duration = Duration::from_secs(10 * 60);

/// Two samples further apart than this are never one run. See the module
/// doc's "Runs, not heartbeats every five seconds".
const MAX_GAP_MS: i64 = 30_000;

/// The most heartbeats kept while they cannot be delivered.
const MAX_PENDING: usize = 2000;

/// The oldest a kept heartbeat may get before it is dropped.
const MAX_PENDING_AGE_SECS: i64 = 24 * 3600;

/// Until the vault says otherwise.
const DEFAULT_IDLE_MINUTES: u32 = 3;

/// What the status says on GNOME on Wayland, where this process cannot see
/// focus at all.
const GNOME_NOTE: &str = "On GNOME the Shell extension reports which app has focus; this app cannot see it from outside the Shell.";

/// What `screentime_status` answers. Mirrors `ScreenTimeStatus` in
/// `ui/src/lib/types.ts`.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenTimeStatus {
    pub tracking: bool,
    pub device: String,
    pub backend: String,
    pub note: String,
    pub extension: Option<ExtensionStatus>,
    pub listening: Option<String>,
    pub last_sent: Option<String>,
    pub last_error: Option<String>,
    pub clients: Vec<Client>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionStatus {
    pub installed: bool,
    pub enabled: bool,
    pub active: bool,
    pub needs_login: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Client {
    pub collector: String,
    pub last_seen: String,
}

/// What `screentime_connect_browser` answers, once.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserConnection {
    pub code: String,
    pub endpoint: String,
    pub device: String,
}

// ---------------------------------------------------------------------------
// This machine's config
// ---------------------------------------------------------------------------

/// `screentime.json`. Every field defaults, so a file from an older build,
/// or none at all, reads as "tracking on, called by its hostname".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct MachineConfig {
    /// Does this computer record its own screen time?
    tracking: bool,
    /// What this machine is called in reports. The same name from the
    /// desktop collector and a browser extension is what lets a report tell
    /// they describe the same seconds, so it is handed to both.
    device: String,
    /// The loopback listener's port.
    port: u16,
    /// The registry device id of the token the GNOME extension was given,
    /// so installing again can revoke it.
    #[serde(skip_serializing_if = "Option::is_none")]
    gnome_token_device: Option<String>,
    /// Browser (`chrome`, `firefox`) → the registry device id of its token,
    /// so connecting it again revokes the old one.
    browsers: BTreeMap<String, String>,
}

impl Default for MachineConfig {
    fn default() -> Self {
        Self {
            tracking: true,
            device: default_device(),
            port: DEFAULT_SCREENTIME_PORT,
            gnome_token_device: None,
            browsers: BTreeMap::new(),
        }
    }
}

impl MachineConfig {
    fn load(dir: &Path) -> Self {
        let path = dir.join(CONFIG_FILE);
        let mut config = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Self>(&text) {
                Ok(config) => config,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "screentime: config unreadable; using defaults");
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        };
        config.normalize();
        config
    }

    /// What a hand-edited file can get wrong and still mean something.
    fn normalize(&mut self) {
        let device = self.device.trim();
        self.device = if device.is_empty() { default_device() } else { device.to_string() };
        if self.port == 0 {
            self.port = DEFAULT_SCREENTIME_PORT;
        }
    }

    /// Written private, as `mcp.json` is: it names which registry rows are
    /// this machine's collectors' tokens.
    fn save(&self, dir: &Path) -> CommandResult<()> {
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| CommandError::new("internal", e.to_string()))?;
        write_private(dir, CONFIG_FILE, text.as_bytes())
    }
}

/// `screentime-gnome.json`: what the GNOME extension needs to post.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct GnomeConfig {
    endpoint: String,
    token: String,
    device: String,
    /// SHA-256 of a sharing server's certificate, hex; empty for loopback.
    #[serde(default)]
    fingerprint: String,
}

impl GnomeConfig {
    fn load(dir: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(dir.join(GNOME_CONFIG_FILE)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// `0600`: it holds a bearer token.
    fn save(&self, dir: &Path) -> CommandResult<()> {
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| CommandError::new("internal", e.to_string()))?;
        write_private(dir, GNOME_CONFIG_FILE, text.as_bytes())
    }
}

fn write_private(dir: &Path, name: &str, bytes: &[u8]) -> CommandResult<()> {
    everyday_core::fsutil::create_private_dir(dir)
        .map_err(|e| CommandError::new("io", e.to_string()))?;
    everyday_core::fsutil::write_atomic_private(
        &dir.join(name),
        bytes,
        &everyday_core::fsutil::unique_tag(),
    )
    .map_err(|e| CommandError::new("io", e.to_string()))
}

/// This machine's name, from its hostname.
fn default_device() -> String {
    friendly_device(&gethostname::gethostname().to_string_lossy())
}

/// A hostname as a person would say it: `hari-laptop.local` is
/// `hari-laptop`, and so is `hari-laptop.example.com`. An address stays
/// whole, since its first label alone means nothing; nothing at all is
/// "computer".
fn friendly_device(raw: &str) -> String {
    let raw = raw.trim().trim_end_matches('.');
    let name = if raw.parse::<std::net::IpAddr>().is_ok() {
        raw
    } else {
        raw.split('.').next().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(raw)
    };
    if name.is_empty() {
        return "computer".to_string();
    }
    name.chars().take(everyday_core::screentime::MAX_DEVICE).collect()
}

/// A name typed in Settings: trimmed, not empty, no control characters,
/// cut to the vault's limit.
fn clean_device(name: &str) -> CommandResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CommandError::new("invalid", "a machine needs a name"));
    }
    if name.chars().any(char::is_control) {
        return Err(CommandError::new(
            "invalid",
            "a machine's name cannot hold control characters",
        ));
    }
    Ok(name.chars().take(everyday_core::screentime::MAX_DEVICE).collect())
}

/// A browser's name as a collector calls itself: short, lower-case letters,
/// digits and `-`, which is also what `record_screen_time` accepts as a
/// collector.
fn clean_browser(browser: &str) -> CommandResult<String> {
    let browser = browser.trim().to_ascii_lowercase();
    let ok = !browser.is_empty()
        && browser.len() <= 32
        && !browser.starts_with('-')
        && browser.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(browser)
    } else {
        Err(CommandError::new("invalid", format!("{browser:?} is not a browser's name")))
    }
}

/// `chrome` → `Chrome`, for the device list.
fn display_browser(browser: &str) -> String {
    let mut chars = browser.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

fn endpoint(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// What a browser extension is pasted: `everyday-screentime:` and the
/// base64url, unpadded, of `{"endpoint", "token", "device"}`. See
/// `docs/plans/screentime.md`'s "Connection code".
fn connection_code(endpoint: &str, token: &str, device: &str) -> String {
    use base64::Engine as _;
    let body = json!({ "endpoint": endpoint, "token": token, "device": device }).to_string();
    format!(
        "everyday-screentime:{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(body.as_bytes())
    )
}

// ---------------------------------------------------------------------------
// Joining samples into runs
// ---------------------------------------------------------------------------

/// One app in use, unbroken, from `start` to `end`.
#[derive(Debug, Clone, PartialEq)]
struct Run {
    app: String,
    name: String,
    title: String,
    start: Timestamp,
    end: Timestamp,
}

impl Run {
    fn beat(self) -> Heartbeat {
        Heartbeat {
            kind: SpanKind::App,
            app: self.app,
            name: self.name,
            host: String::new(),
            title: self.title,
            start: self.start,
            end: Some(self.end),
        }
    }
}

/// Samples folded into runs. See the module doc's "Runs, not heartbeats
/// every five seconds". Takes the time as an argument, so a test can run a
/// day in no time.
#[derive(Debug, Default)]
struct Runs {
    open: Option<Run>,
    done: Vec<Run>,
    last: Option<Timestamp>,
}

impl Runs {
    /// What was in use at `now`, or `None` for nothing.
    fn observe(&mut self, now: Timestamp, seen: Option<&Focused>) {
        let continuous = self.last.is_some_and(|last| {
            let gap = now.as_millisecond() - last.as_millisecond();
            (0..=MAX_GAP_MS).contains(&gap)
        });
        self.last = Some(now);
        if !continuous {
            self.close();
        }
        let Some(seen) = seen else {
            self.close();
            return;
        };
        if let Some(run) = self.open.as_mut().filter(|run| run.app == seen.app_id) {
            run.end = now;
            if !seen.title.is_empty() {
                run.title.clone_from(&seen.title);
            }
            if !seen.name.is_empty() {
                run.name.clone_from(&seen.name);
            }
            return;
        }
        // A switch: still continuous, or the run would have been closed
        // above, so the old one ends where the new one begins.
        if let Some(mut run) = self.open.take() {
            run.end = now;
            self.finish(run);
        }
        self.open = Some(Run {
            app: seen.app_id.clone(),
            name: seen.name.clone(),
            title: seen.title.clone(),
            start: now,
            end: now,
        });
    }

    /// End the open run at its last sample.
    fn close(&mut self) {
        if let Some(run) = self.open.take() {
            self.finish(run);
        }
    }

    /// A run of one sample has no length, and is dropped.
    fn finish(&mut self, run: Run) {
        if run.end > run.start {
            self.done.push(run);
        }
    }

    /// Everything finished, and what the open run has added since the last
    /// take; the open run carries on from where this leaves it, so the next
    /// take sends only what came after.
    fn take(&mut self) -> Vec<Run> {
        let mut out = std::mem::take(&mut self.done);
        if let Some(run) = self.open.as_mut().filter(|run| run.end > run.start) {
            out.push(run.clone());
            run.start = run.end;
        }
        out
    }
}

/// Put beats a failed flush handed back in front of what came since: they
/// are older.
fn requeue(pending: &mut VecDeque<Heartbeat>, older: Vec<Heartbeat>) {
    for beat in older.into_iter().rev() {
        pending.push_front(beat);
    }
}

/// Drop what is too old to be worth sending, then the oldest past the cap.
fn trim_pending(pending: &mut VecDeque<Heartbeat>, now: Timestamp) {
    let oldest = now.as_second() - MAX_PENDING_AGE_SECS;
    pending.retain(|beat| beat.end.unwrap_or(beat.start).as_second() >= oldest);
    while pending.len() > MAX_PENDING {
        pending.pop_front();
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// How focus is watched here, and the sentence the status shows about it.
#[derive(Debug, Clone)]
struct BackendInfo {
    backend: Backend,
    note: String,
}

/// What the watcher thread, the listener's [`Forward`] and the commands all
/// look at.
struct Shared {
    config: Mutex<MachineConfig>,
    /// Learnt once, by the watcher thread or by the first status.
    backend: Mutex<Option<BackendInfo>>,
    /// The watcher could not read focus, latest attempt.
    sample_error: Mutex<Option<String>>,
    last_sent: Mutex<Option<Timestamp>>,
    last_error: Mutex<Option<String>>,
    /// Collector → when the listener last delivered for it.
    heard: Mutex<BTreeMap<String, Timestamp>>,
    idle_minutes: AtomicU32,
    /// Whether `idle_minutes` came from the vault (else it is the default,
    /// and is asked for again at the next flush).
    idle_known: AtomicBool,
}

impl Shared {
    fn config(&self) -> MachineConfig {
        self.config.lock().unwrap().clone()
    }

    fn device(&self) -> String {
        self.config.lock().unwrap().device.clone()
    }

    fn sent(&self, at: Timestamp) {
        *self.last_sent.lock().unwrap() = Some(at);
        *self.last_error.lock().unwrap() = None;
    }

    fn failed(&self, why: String) {
        *self.last_error.lock().unwrap() = Some(why);
    }

    fn set_sample_error(&self, error: Option<String>) {
        *self.sample_error.lock().unwrap() = error;
    }

    /// Remember a delivery. Bounded, though only a misbehaving extension
    /// would ever name more than a handful of collectors.
    fn heard(&self, collector: &str) {
        let collector: String =
            collector.chars().take(everyday_core::screentime::MAX_COLLECTOR).collect();
        let mut heard = self.heard.lock().unwrap();
        if !heard.contains_key(&collector) && heard.len() >= 32 {
            let oldest = heard.iter().min_by_key(|(_, at)| **at).map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                heard.remove(&oldest);
            }
        }
        heard.insert(collector, Timestamp::now());
    }
}

/// The shell's screen-time state, managed by Tauri.
pub struct ScreenTime {
    shared: Arc<Shared>,
    /// The loopback listener, once bound.
    listener: Listener<Arc<everyday_server::screentime::Listener>>,
    /// Why it is not, when binding failed.
    listen_error: Mutex<Option<String>>,
    /// The running watcher thread's stop flag.
    watcher: Mutex<Option<Arc<AtomicBool>>>,
    /// Serialises the commands that mint and revoke tokens, so two clicks on
    /// "Connect" cannot each revoke the other's token.
    setup: tokio::sync::Mutex<()>,
}

impl Default for ScreenTime {
    fn default() -> Self {
        Self {
            shared: Arc::new(Shared {
                config: Mutex::new(MachineConfig::load(&everyday_vault::config_dir())),
                backend: Mutex::new(None),
                sample_error: Mutex::new(None),
                last_sent: Mutex::new(None),
                last_error: Mutex::new(None),
                heard: Mutex::new(BTreeMap::new()),
                idle_minutes: AtomicU32::new(DEFAULT_IDLE_MINUTES),
                idle_known: AtomicBool::new(false),
            }),
            listener: Listener::default(),
            listen_error: Mutex::new(None),
            watcher: Mutex::new(None),
            setup: tokio::sync::Mutex::new(()),
        }
    }
}

impl ScreenTime {
    /// Change the config: on disk first, in memory only once that worked.
    fn update_config(
        &self,
        change: impl FnOnce(&mut MachineConfig),
    ) -> CommandResult<MachineConfig> {
        let mut current = self.shared.config.lock().unwrap();
        let mut next = current.clone();
        change(&mut next);
        next.save(&everyday_vault::config_dir())?;
        *current = next.clone();
        Ok(next)
    }

    /// Start the watcher thread, unless one is running.
    fn start_watcher(&self, app: &AppHandle) {
        let mut slot = self.watcher.lock().unwrap();
        if slot.is_some() {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_stop, app, shared) = (stop.clone(), app.clone(), self.shared.clone());
        let spawned = std::thread::Builder::new()
            .name("screentime-watch".into())
            .spawn(move || watch(app, shared, thread_stop));
        match spawned {
            Ok(_) => *slot = Some(stop),
            Err(e) => {
                tracing::warn!(error = %e, "screentime: could not start the watcher thread");
                self.shared.set_sample_error(Some(format!("could not start watching: {e}")));
            }
        }
    }

    /// Tell the watcher thread to stop. It sends what it has on its way out.
    fn stop_watcher(&self) {
        if let Some(stop) = self.watcher.lock().unwrap().take() {
            stop.store(true, Ordering::Release);
        }
    }

    /// How focus is watched here. Learnt by the watcher thread when it
    /// starts; with tracking off, by asking once here. A watcher made only
    /// to be asked is dropped at once.
    async fn backend_info(&self) -> BackendInfo {
        let cached = self.shared.backend.lock().unwrap().clone();
        if let Some(info) = cached {
            return info;
        }
        let info = tauri::async_runtime::spawn_blocking(|| {
            let mut watcher = everyday_focus::detect();
            describe(&mut *watcher)
        })
        .await
        .unwrap_or_else(|e| BackendInfo { backend: Backend::Unsupported, note: e.to_string() });
        *self.shared.backend.lock().unwrap() = Some(info.clone());
        info
    }

    async fn status(&self) -> ScreenTimeStatus {
        let info = self.backend_info().await;
        let config = self.shared.config();
        let heard = self.shared.heard.lock().unwrap().clone();

        let mut clients: Vec<(String, Timestamp)> =
            heard.iter().map(|(k, v)| (k.clone(), *v)).collect();
        clients.sort_by_key(|c| std::cmp::Reverse(c.1));
        let clients = clients
            .into_iter()
            .map(|(collector, at)| Client { collector, last_seen: at.to_string() })
            .collect();

        let (extension, extension_note) = extension_status(info.backend, &heard).await;
        let listening =
            self.listener.with(|running| running.map(|r| format!("http://{}", r.address)));

        let mut notes = Vec::new();
        if !info.note.is_empty() {
            notes.push(info.note.clone());
        }
        let watching = !matches!(info.backend, Backend::Gnome | Backend::Unsupported);
        if config.tracking
            && watching
            && let Some(e) = self.shared.sample_error.lock().unwrap().clone()
        {
            notes.push(format!("Could not read which app has focus: {e}."));
        }
        notes.extend(extension_note);
        if listening.is_none() {
            notes.extend(self.listen_error.lock().unwrap().clone());
        }

        ScreenTimeStatus {
            tracking: config.tracking,
            device: config.device,
            backend: info.backend.as_str().to_string(),
            note: notes.join(" "),
            extension,
            listening,
            last_sent: (*self.shared.last_sent.lock().unwrap()).map(|t| t.to_string()),
            last_error: self.shared.last_error.lock().unwrap().clone(),
            clients,
        }
    }

    /// Mint a token for a browser and hand back its connection code.
    async fn connect_browser(
        &self,
        app: &AppHandle,
        browser: &str,
    ) -> CommandResult<BrowserConnection> {
        let browser = clean_browser(browser)?;
        let _guard = self.setup.lock().await;
        let registry = app.state::<AppState>().registry()?;
        let config = self.shared.config();
        let (token, id) = registry.issue(
            &format!("{} extension", display_browser(&browser)),
            vec![Scope::Heartbeats],
            false,
        )?;
        let old = config.browsers.get(&browser).cloned();
        if let Err(e) = self.update_config(|c| {
            c.browsers.insert(browser.clone(), id.clone());
        }) {
            let _ = registry.revoke(&id);
            return Err(e);
        }
        if let Some(old) = old.filter(|old| *old != id)
            && let Err(e) = registry.revoke(&old)
        {
            tracing::warn!(error = %e, "screentime: could not revoke a browser's old token");
        }
        let endpoint = endpoint(config.port);
        Ok(BrowserConnection {
            code: connection_code(&endpoint, &token, &config.device),
            endpoint,
            device: config.device,
        })
    }

    /// Write the extension, give it a token, and switch it on.
    #[cfg(target_os = "linux")]
    async fn install_gnome(&self, app: &AppHandle) -> CommandResult<()> {
        let _guard = self.setup.lock().await;
        gnome::write_files()?;

        let registry = app.state::<AppState>().registry()?;
        let config = self.shared.config();
        let dir = everyday_vault::config_dir();
        let (token, id) = registry.issue("GNOME Shell", vec![Scope::Heartbeats], false)?;
        let file = GnomeConfig {
            endpoint: endpoint(config.port),
            token,
            device: config.device.clone(),
            fingerprint: String::new(),
        };
        // The old token stays good until the new one is where the extension
        // will read it, so a failure here leaves it working as it was.
        if let Err(e) = file.save(&dir) {
            let _ = registry.revoke(&id);
            return Err(e);
        }
        if let Err(e) = self.update_config(|c| c.gnome_token_device = Some(id.clone())) {
            tracing::warn!(error = %e, "screentime: the GNOME extension's token works but is not remembered");
        }
        if let Some(old) = config.gnome_token_device.filter(|old| *old != id)
            && let Err(e) = registry.revoke(&old)
        {
            tracing::warn!(error = %e, "screentime: could not revoke the GNOME extension's old token");
        }

        // The running Shell enables an extension it knows at once. One it
        // has not seen -- the first install, on Wayland -- it will only
        // discover at the next login, and refuses; listing it in the
        // settings is what makes that login turn it on.
        if gnome::enable_over_dbus().await != Some(true) {
            gnome::enable_in_gsettings().await.map_err(|e| {
                CommandError::new("gnome", format!("could not enable the extension: {e}"))
            })?;
        }
        Ok(())
    }

    /// Rename this machine. The GNOME extension's file is rewritten to
    /// match, so it sends the new name too (it also asks the listener,
    /// which answers with the current one).
    fn set_device(&self, name: &str) -> CommandResult<()> {
        let device = clean_device(name)?;
        self.update_config(|c| c.device = device.clone())?;
        let dir = everyday_vault::config_dir();
        if let Some(mut file) = GnomeConfig::load(&dir)
            && file.device != device
        {
            file.device = device;
            file.save(&dir)?;
        }
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
impl ScreenTime {
    async fn install_gnome(&self, _app: &AppHandle) -> CommandResult<()> {
        Err(CommandError::new("unsupported", "the GNOME Shell extension is for GNOME on Linux"))
    }
}

fn describe(watcher: &mut dyn Watcher) -> BackendInfo {
    let backend = watcher.backend();
    let note = match backend {
        Backend::Gnome => GNOME_NOTE.to_string(),
        Backend::Unsupported => match watcher.focused() {
            Err(e) => format!("Screen time cannot watch this desktop: {e}."),
            Ok(_) => String::new(),
        },
        _ => String::new(),
    };
    BackendInfo { backend, note }
}

// ---------------------------------------------------------------------------
// The watcher thread
// ---------------------------------------------------------------------------

/// Sleep for `total` in short steps; true if told to stop meanwhile.
fn sleep_unless_stopped(stop: &AtomicBool, total: Duration) -> bool {
    let step = Duration::from_millis(500);
    let until = Instant::now() + total;
    loop {
        if stop.load(Ordering::Acquire) {
            return true;
        }
        let now = Instant::now();
        if now >= until {
            return false;
        }
        std::thread::sleep(step.min(until - now));
    }
}

/// The thread's whole life. Returns at once where this process watches
/// nothing (GNOME, an unsupported desktop), having said why.
fn watch(app: AppHandle, shared: Arc<Shared>, stop: Arc<AtomicBool>) {
    let mut watcher = everyday_focus::detect();
    let info = describe(&mut *watcher);
    let backend = info.backend;
    *shared.backend.lock().unwrap() = Some(info);
    if matches!(backend, Backend::Gnome | Backend::Unsupported) {
        tracing::info!(
            backend = backend.as_str(),
            "screentime: not watching focus from the app here"
        );
        return;
    }
    tracing::info!(backend = backend.as_str(), "screentime: watching focus");
    let collector = backend.as_str();

    let (back_tx, back_rx) = mpsc::channel::<Vec<Heartbeat>>();
    let in_flight = Arc::new(AtomicBool::new(false));
    let mut runs = Runs::default();
    let mut pending: VecDeque<Heartbeat> = VecDeque::new();
    let mut last_flush = Instant::now();
    let mut last_asked: Option<Instant> = None;

    loop {
        if sleep_unless_stopped(&stop, SAMPLE_EVERY) {
            break;
        }

        let ask_every =
            if shared.idle_known.load(Ordering::Acquire) { COLLECTOR_EVERY } else { FLUSH_EVERY };
        if last_asked.is_none_or(|at| at.elapsed() >= ask_every) {
            last_asked = Some(Instant::now());
            ask_collector(&app, &shared);
        }

        let idle_limit =
            Duration::from_secs(u64::from(shared.idle_minutes.load(Ordering::Acquire)) * 60);
        let now = Timestamp::now();
        let seen = sample(&mut *watcher, idle_limit, &shared);
        runs.observe(now, seen.as_ref());

        // The flag is cleared only after a failed flush's beats are in the
        // channel, so once it reads false they can all be taken back here,
        // before anything newer is sent.
        if last_flush.elapsed() >= FLUSH_EVERY && !in_flight.load(Ordering::Acquire) {
            last_flush = Instant::now();
            while let Ok(older) = back_rx.try_recv() {
                requeue(&mut pending, older);
            }
            pending.extend(runs.take().into_iter().map(Run::beat));
            trim_pending(&mut pending, now);
            if !pending.is_empty() {
                in_flight.store(true, Ordering::Release);
                let beats = pending.drain(..).collect();
                send(&app, &shared, collector, beats, Some((back_tx.clone(), in_flight.clone())));
            }
        }
    }

    // Tracking was switched off: what was seen while it was on is still
    // the person's, so it goes out, once, without waiting for an answer.
    runs.close();
    while let Ok(older) = back_rx.try_recv() {
        requeue(&mut pending, older);
    }
    pending.extend(runs.take().into_iter().map(Run::beat));
    trim_pending(&mut pending, Timestamp::now());
    if !pending.is_empty() {
        send(&app, &shared, collector, pending.into_iter().collect(), None);
    }
    tracing::info!("screentime: stopped watching focus");
}

/// What is in use now, if anything: focused, not locked, not idle.
///
/// Focus that cannot be read counts as nothing in use. Idle time or a lock
/// state that cannot be read does not stop counting -- a desktop whose
/// logind will not answer `LockedHint` should not record nothing at all --
/// and a machine that really is locked or asleep is a gap in samples anyway.
fn sample(watcher: &mut dyn Watcher, idle_limit: Duration, shared: &Shared) -> Option<Focused> {
    let focused = match watcher.focused() {
        Ok(focused) => {
            shared.set_sample_error(None);
            focused?
        }
        Err(e) => {
            shared.set_sample_error(Some(e.to_string()));
            return None;
        }
    };
    if watcher.locked().unwrap_or(false) {
        return None;
    }
    if watcher.idle().is_ok_and(|idle| idle >= idle_limit) {
        return None;
    }
    Some(focused)
}

/// Send `beats` as one `record_screen_time` call through whichever vault the
/// window is looking at, on Tauri's runtime. A failure hands them back over
/// `back`, then clears its in-flight flag.
fn send(
    app: &AppHandle,
    shared: &Arc<Shared>,
    collector: &'static str,
    beats: Vec<Heartbeat>,
    back: Option<(mpsc::Sender<Vec<Heartbeat>>, Arc<AtomicBool>)>,
) {
    let session = app.state::<AppState>().session();
    let device = shared.device();
    let shared = shared.clone();
    tauri::async_runtime::spawn(async move {
        let args = json!({ "device": device, "collector": collector, "beats": &beats });
        match session.as_session().call(Ctx::local(), "record_screen_time", args).await {
            Ok(_) => shared.sent(Timestamp::now()),
            Err(e) => {
                tracing::debug!(error = %e, beats = beats.len(), "screentime: could not send; keeping them");
                shared.failed(e.message.clone());
                if let Some((tx, _)) = &back {
                    let _ = tx.send(beats);
                }
            }
        }
        if let Some((_, in_flight)) = back {
            in_flight.store(false, Ordering::Release);
        }
    });
}

/// Ask the vault how long without input counts as idle. In the background;
/// the answer lands in `shared` for the next sample.
fn ask_collector(app: &AppHandle, shared: &Arc<Shared>) {
    let session = app.state::<AppState>().session();
    let shared = shared.clone();
    tauri::async_runtime::spawn(async move {
        match collector_config(&session).await {
            Ok(config) => {
                shared.idle_minutes.store(config.idle_minutes.clamp(1, 60), Ordering::Release);
                shared.idle_known.store(true, Ordering::Release);
            }
            // Locked, or nothing open: keep what was known, ask again soon.
            Err(_) => shared.idle_known.store(false, Ordering::Release),
        }
    });
}

async fn collector_config(session: &crate::state::SessionHandle) -> CommandResult<CollectorConfig> {
    let value = session.as_session().call(Ctx::local(), "screen_time_collector", json!({})).await?;
    serde_json::from_value(value).map_err(|e| {
        CommandError::new("internal", format!("screen_time_collector answered oddly: {e}"))
    })
}

// ---------------------------------------------------------------------------
// The loopback listener
// ---------------------------------------------------------------------------

type Boxed<T> = Pin<Box<dyn Future<Output = CommandResult<T>> + Send + 'static>>;

/// Hands what the listener was sent to whichever vault the window is
/// looking at -- looked up per request, so connecting the window to another
/// machine redirects the extensions too.
struct ShellForward {
    app: AppHandle,
    shared: Arc<Shared>,
}

impl Forward for ShellForward {
    fn device(&self) -> String {
        self.shared.device()
    }

    fn record(&self, body: HeartbeatBody) -> Boxed<IngestReport> {
        let HeartbeatBody { device, collector, beats } = body;
        // See the module doc's "The listener is always on".
        if collector == "gnome" && !self.shared.config.lock().unwrap().tracking {
            let rejected = u32::try_from(beats.len()).unwrap_or(u32::MAX);
            return Box::pin(async move { Ok(IngestReport { accepted: 0, rejected, spans: 0 }) });
        }
        let device =
            device.filter(|d| !d.trim().is_empty()).unwrap_or_else(|| self.shared.device());
        let session = self.app.state::<AppState>().session();
        Box::pin(async move {
            let args = json!({ "device": device, "collector": collector, "beats": beats });
            let value = session.as_session().call(Ctx::local(), "record_screen_time", args).await?;
            serde_json::from_value(value).map_err(|e| {
                CommandError::new("internal", format!("record_screen_time answered oddly: {e}"))
            })
        })
    }

    fn config(&self) -> Boxed<CollectorConfig> {
        let session = self.app.state::<AppState>().session();
        Box::pin(async move { collector_config(&session).await })
    }

    fn heard(&self, collector: &str) {
        self.shared.heard(collector);
    }
}

async fn start_listener(app: AppHandle) {
    let state = app.state::<ScreenTime>();
    let port = state.shared.config().port;
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let failed = |why: String| {
        tracing::warn!(%address, error = %why, "screentime: the loopback listener is not running");
        *state.listen_error.lock().unwrap() = Some(format!(
            "Browser extensions cannot reach this app: nothing is listening on {address} ({why})."
        ));
    };
    let registry = match app.state::<AppState>().registry() {
        Ok(registry) => registry,
        Err(e) => return failed(e.message),
    };
    let forward: Arc<dyn Forward> =
        Arc::new(ShellForward { app: app.clone(), shared: state.shared.clone() });
    match everyday_server::screentime::start(forward, registry, address).await {
        Ok(running) => {
            tracing::info!(address = %running.address, "screentime: listening for extensions");
            state.listener.set(running);
            *state.listen_error.lock().unwrap() = None;
        }
        Err(e) => failed(e.message),
    }
}

// ---------------------------------------------------------------------------
// The GNOME Shell extension
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
async fn extension_status(
    backend: Backend,
    heard: &BTreeMap<String, Timestamp>,
) -> (Option<ExtensionStatus>, Option<String>) {
    let installed = gnome::installed();
    if backend != Backend::Gnome && !installed {
        return (None, None);
    }
    let active = heard.get("gnome").is_some_and(|at| {
        Timestamp::now().as_second() - at.as_second() <= gnome::ACTIVE_WITHIN_SECS
    });
    let shell = gnome::shell_info().await;
    let known = shell.as_ref().map(|s| s.known);
    let enabled = match &shell {
        Some(s) if s.known => s.enabled,
        _ => gnome::listed_in_gsettings().await.unwrap_or(false),
    };
    let mut note = shell.filter(|s| s.known && !s.error.is_empty()).map(|s| {
        format!("The GNOME Shell extension did not load: {}.", s.error.trim_end_matches('.'))
    });
    if installed && gnome::user_extensions_off().await {
        note = Some(
            "GNOME has every extension switched off (Extensions → Use Extensions), so the              Screen time one cannot run until that is on again."
                .into(),
        );
    }
    let status = ExtensionStatus {
        installed,
        enabled,
        active,
        needs_login: installed && enabled && known == Some(false),
    };
    (Some(status), note)
}

#[cfg(not(target_os = "linux"))]
async fn extension_status(
    _backend: Backend,
    _heard: &BTreeMap<String, Timestamp>,
) -> (Option<ExtensionStatus>, Option<String>) {
    (None, None)
}

/// Installing the extension and asking GNOME Shell about it.
///
/// Over D-Bus first (`org.gnome.Shell.Extensions` on the session bus, the
/// interface the Extensions app uses), because the running Shell is the only
/// one that knows whether the extension is actually loaded. The settings
/// (`org.gnome.shell enabled-extensions`, through the `gsettings` command so
/// no GSettings binding is linked for two keys) are the fallback: for an
/// extension the Shell has not discovered yet -- a fresh install on Wayland,
/// until the next login -- and for a session bus with no Shell on it.
#[cfg(target_os = "linux")]
mod gnome {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::Duration;

    use everyday_service::error::{CommandError, CommandResult};
    use zbus::zvariant::OwnedValue;

    /// The app id is `app.everyday.journal`; the browser extension's Gecko id
    /// is the same string as this.
    pub const UUID: &str = "screentime@everyday.journal";

    /// Heard from within this long is "active".
    pub const ACTIVE_WITHIN_SECS: i64 = 120;

    const BUS: &str = "org.gnome.Shell.Extensions";
    const PATH: &str = "/org/gnome/Shell/Extensions";
    const INTERFACE: &str = "org.gnome.Shell.Extensions";
    const SCHEMA: &str = "org.gnome.shell";
    /// A Shell that does not answer in this long is treated as absent.
    const DBUS_TIMEOUT: Duration = Duration::from_secs(3);
    /// `ExtensionState.ACTIVE` (`ENABLED` before GNOME 45; the same number).
    const STATE_ACTIVE: f64 = 1.0;

    const METADATA: &str =
        include_str!("../../../extensions/gnome/screentime@everyday.journal/metadata.json");
    const EXTENSION_JS: &str =
        include_str!("../../../extensions/gnome/screentime@everyday.journal/extension.js");

    /// `$XDG_DATA_HOME/gnome-shell/extensions`, where the Shell looks for a
    /// user's own extensions.
    pub fn extensions_dir() -> Option<PathBuf> {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".local").join("share"))
            })?;
        Some(data.join("gnome-shell").join("extensions"))
    }

    pub fn installed() -> bool {
        extensions_dir().is_some_and(|dir| dir.join(UUID).is_dir())
    }

    /// Copy the extension compiled into this binary over whatever is there:
    /// installing again is also how it is updated.
    pub fn write_files() -> CommandResult<PathBuf> {
        let dir = extensions_dir()
            .ok_or_else(|| {
                CommandError::new("io", "no home directory to install the extension into")
            })?
            .join(UUID);
        std::fs::create_dir_all(&dir)
            .map_err(|e| CommandError::new("io", format!("{}: {e}", dir.display())))?;
        for (name, text) in [("metadata.json", METADATA), ("extension.js", EXTENSION_JS)] {
            everyday_core::fsutil::write_atomic(
                &dir.join(name),
                text.as_bytes(),
                &everyday_core::fsutil::unique_tag(),
            )
            .map_err(|e| CommandError::new("io", e.to_string()))?;
        }
        Ok(dir)
    }

    /// What the running Shell says about the extension.
    pub struct ShellInfo {
        /// The Shell has discovered it (it has been there since login).
        pub known: bool,
        pub enabled: bool,
        /// Why it failed to load, if it did.
        pub error: String,
    }

    async fn proxy() -> zbus::Result<zbus::Proxy<'static>> {
        let connection = zbus::Connection::session().await?;
        zbus::Proxy::new(&connection, BUS, PATH, INTERFACE).await
    }

    /// `None` when there is no Shell to ask.
    pub async fn shell_info() -> Option<ShellInfo> {
        let ask = async {
            let proxy = proxy().await?;
            let info: HashMap<String, OwnedValue> =
                proxy.call("GetExtensionInfo", &(UUID,)).await?;
            Ok::<_, zbus::Error>(info)
        };
        let info = match tokio::time::timeout(DBUS_TIMEOUT, ask).await {
            Ok(Ok(info)) => info,
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "screentime: GNOME Shell did not answer GetExtensionInfo");
                return None;
            }
            Err(_) => return None,
        };
        // An extension the Shell has not discovered serializes to nothing.
        let known = info.contains_key("uuid") || info.contains_key("state");
        let state = info.get("state").and_then(|v| f64::try_from(v).ok());
        let enabled = info
            .get("enabled")
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(state == Some(STATE_ACTIVE));
        let error =
            info.get("error").and_then(|v| <&str>::try_from(v).ok()).unwrap_or("").to_string();
        Some(ShellInfo { known, enabled, error })
    }

    /// `Some(true)` when the running Shell enabled it; `Some(false)` when it
    /// refused (it has not discovered the extension yet); `None` when there
    /// is no Shell to ask.
    pub async fn enable_over_dbus() -> Option<bool> {
        let ask = async {
            let proxy = proxy().await?;
            let enabled: bool = proxy.call("EnableExtension", &(UUID,)).await?;
            Ok::<_, zbus::Error>(enabled)
        };
        match tokio::time::timeout(DBUS_TIMEOUT, ask).await {
            Ok(Ok(enabled)) => Some(enabled),
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "screentime: GNOME Shell did not answer EnableExtension");
                None
            }
            Err(_) => None,
        }
    }

    /// Has the person switched every user extension off? Ours too, then;
    /// it is theirs to switch back on, not this application's.
    pub(super) async fn user_extensions_off() -> bool {
        blocking(|| gsettings(&["get", SCHEMA, "disable-user-extensions"]))
            .await
            .is_ok_and(|v| v == "true")
    }

    /// Is it in `enabled-extensions`? `None` when that cannot be read.
    pub async fn listed_in_gsettings() -> Option<bool> {
        blocking(|| gsettings(&["get", SCHEMA, "enabled-extensions"]))
            .await
            .ok()
            .map(|text| parse_strv(&text).iter().any(|uuid| uuid == UUID))
    }

    /// List it in `enabled-extensions` (and out of `disabled-extensions`,
    /// which wins over it), so the next login discovers it switched on.
    pub async fn enable_in_gsettings() -> Result<(), String> {
        blocking(|| {
            let mut enabled = parse_strv(&gsettings(&["get", SCHEMA, "enabled-extensions"])?);
            if !enabled.iter().any(|uuid| uuid == UUID) {
                enabled.push(UUID.to_string());
                let value = format_strv(&enabled);
                gsettings(&["set", SCHEMA, "enabled-extensions", value.as_str()])?;
            }
            if let Ok(text) = gsettings(&["get", SCHEMA, "disabled-extensions"]) {
                let disabled = parse_strv(&text);
                if disabled.iter().any(|uuid| uuid == UUID) {
                    let kept: Vec<String> =
                        disabled.into_iter().filter(|uuid| uuid != UUID).collect();
                    let value = format_strv(&kept);
                    gsettings(&["set", SCHEMA, "disabled-extensions", value.as_str()])?;
                }
            }
            // `disable-user-extensions` is left alone even when it is on:
            // it switches off every extension the person installed, and
            // turning it back off for ours would turn all of theirs back on
            // too. Status says so instead (see `user_extensions_off`).
            Ok(())
        })
        .await
    }

    async fn blocking<T: Send + 'static>(
        f: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())?
    }

    fn gsettings(args: &[&str]) -> Result<String, String> {
        let output = std::process::Command::new("gsettings")
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("gsettings: {e}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            Err(format!(
                "gsettings {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }

    /// A GVariant string array as `gsettings get` prints it: `['a', 'b']`,
    /// or `@as []` when empty. Extension uuids hold no quotes or commas.
    pub fn parse_strv(text: &str) -> Vec<String> {
        let text = text.trim();
        let text = text.strip_prefix("@as").unwrap_or(text).trim();
        let inner = text.strip_prefix('[').and_then(|t| t.strip_suffix(']')).unwrap_or("");
        inner
            .split(',')
            .map(|item| item.trim().trim_matches(|c| c == '\'' || c == '"').to_string())
            .filter(|item| !item.is_empty())
            .collect()
    }

    pub fn format_strv(items: &[String]) -> String {
        let quoted: Vec<String> = items.iter().map(|item| format!("'{item}'")).collect();
        format!("[{}]", quoted.join(", "))
    }
}

// ---------------------------------------------------------------------------
// Startup and commands
// ---------------------------------------------------------------------------

/// Start the watcher thread and the loopback listener, as the saved config
/// says. Returns at once.
///
/// `EVERYDAY_NO_SCREENTIME` set skips both, for a test harness or a
/// screenshot run that should neither record nor hold the port.
pub fn spawn(app: &AppHandle) {
    if std::env::var_os("EVERYDAY_NO_SCREENTIME").is_some() {
        tracing::debug!("screentime: switched off by EVERYDAY_NO_SCREENTIME");
        return;
    }
    let state = app.state::<ScreenTime>();
    if state.shared.config().tracking {
        state.start_watcher(app);
    }
    tauri::async_runtime::spawn(start_listener(app.clone()));
}

#[tauri::command]
pub async fn screentime_status(state: State<'_, ScreenTime>) -> CommandResult<ScreenTimeStatus> {
    Ok(state.status().await)
}

#[tauri::command]
pub async fn screentime_set_tracking(
    app: AppHandle,
    state: State<'_, ScreenTime>,
    on: bool,
) -> CommandResult<ScreenTimeStatus> {
    state.update_config(|c| c.tracking = on)?;
    if on {
        state.start_watcher(&app);
    } else {
        state.stop_watcher();
    }
    Ok(state.status().await)
}

#[tauri::command]
pub async fn screentime_set_device(
    state: State<'_, ScreenTime>,
    name: String,
) -> CommandResult<ScreenTimeStatus> {
    state.set_device(&name)?;
    Ok(state.status().await)
}

#[tauri::command]
pub async fn screentime_install_gnome_extension(
    app: AppHandle,
    state: State<'_, ScreenTime>,
) -> CommandResult<ScreenTimeStatus> {
    state.install_gnome(&app).await?;
    Ok(state.status().await)
}

#[tauri::command]
pub async fn screentime_connect_browser(
    app: AppHandle,
    state: State<'_, ScreenTime>,
    browser: String,
) -> CommandResult<BrowserConnection> {
    state.connect_browser(&app, &browser).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> Timestamp {
        Timestamp::from_second(1_800_000_000 + secs).unwrap()
    }

    fn app(id: &str) -> Focused {
        Focused { app_id: id.to_string(), name: format!("{id} name"), title: format!("{id} title") }
    }

    fn spans(runs: &[Run]) -> Vec<(&str, i64, i64)> {
        let base = at(0).as_second();
        runs.iter()
            .map(|r| (r.app.as_str(), r.start.as_second() - base, r.end.as_second() - base))
            .collect()
    }

    #[test]
    fn samples_of_one_app_are_one_run() {
        let mut runs = Runs::default();
        for t in [0, 5, 10, 15] {
            runs.observe(at(t), Some(&app("a")));
        }
        assert_eq!(spans(&runs.take()), vec![("a", 0, 15)]);
    }

    #[test]
    fn a_switch_ends_the_old_run_where_the_new_one_starts() {
        let mut runs = Runs::default();
        runs.observe(at(0), Some(&app("a")));
        runs.observe(at(5), Some(&app("a")));
        runs.observe(at(10), Some(&app("b")));
        runs.observe(at(15), Some(&app("b")));
        assert_eq!(spans(&runs.take()), vec![("a", 0, 10), ("b", 10, 15)]);
    }

    #[test]
    fn a_gap_is_never_counted() {
        // Asleep from 5 to 300: the run ends at the last sample before it.
        let mut runs = Runs::default();
        runs.observe(at(0), Some(&app("a")));
        runs.observe(at(5), Some(&app("a")));
        runs.observe(at(300), Some(&app("a")));
        runs.observe(at(305), Some(&app("a")));
        assert_eq!(spans(&runs.take()), vec![("a", 0, 5), ("a", 300, 305)]);
    }

    #[test]
    fn a_clock_that_goes_back_is_a_gap() {
        let mut runs = Runs::default();
        runs.observe(at(100), Some(&app("a")));
        runs.observe(at(105), Some(&app("a")));
        runs.observe(at(50), Some(&app("a")));
        runs.observe(at(55), Some(&app("a")));
        assert_eq!(spans(&runs.take()), vec![("a", 100, 105), ("a", 50, 55)]);
    }

    #[test]
    fn nothing_in_use_ends_the_run_at_its_last_sample() {
        let mut runs = Runs::default();
        runs.observe(at(0), Some(&app("a")));
        runs.observe(at(5), Some(&app("a")));
        runs.observe(at(10), None);
        runs.observe(at(15), Some(&app("a")));
        // A lone sample has no length and is not sent.
        assert_eq!(spans(&runs.take()), vec![("a", 0, 5)]);
    }

    #[test]
    fn take_sends_only_what_came_since_the_last_take() {
        let mut runs = Runs::default();
        runs.observe(at(0), Some(&app("a")));
        runs.observe(at(5), Some(&app("a")));
        assert_eq!(spans(&runs.take()), vec![("a", 0, 5)]);
        assert!(runs.take().is_empty());
        runs.observe(at(10), Some(&app("a")));
        assert_eq!(spans(&runs.take()), vec![("a", 5, 10)]);
    }

    #[test]
    fn a_run_keeps_the_latest_title() {
        let mut runs = Runs::default();
        runs.observe(at(0), Some(&app("a")));
        let mut later = app("a");
        later.title = "another page".into();
        runs.observe(at(5), Some(&later));
        let beat = runs.take().remove(0).beat();
        assert_eq!(beat.title, "another page");
        assert_eq!(beat.kind, SpanKind::App);
        assert_eq!(beat.end, Some(at(5)));
    }

    fn beat(end: i64) -> Heartbeat {
        Run {
            app: "a".into(),
            name: String::new(),
            title: String::new(),
            start: at(end - 5),
            end: at(end),
        }
        .beat()
    }

    #[test]
    fn the_backlog_is_capped_by_count_and_age() {
        let mut pending: VecDeque<Heartbeat> =
            (0..(MAX_PENDING as i64 + 10)).map(|i| beat(i * 10)).collect();
        let now = at(MAX_PENDING as i64 * 10);
        trim_pending(&mut pending, now);
        assert_eq!(pending.len(), MAX_PENDING);
        // The oldest went first.
        assert_eq!(pending.front().unwrap().end, Some(at(100)));

        let mut pending: VecDeque<Heartbeat> = [beat(0), beat(MAX_PENDING_AGE_SECS + 100)].into();
        trim_pending(&mut pending, at(MAX_PENDING_AGE_SECS + 200));
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn failed_beats_go_back_in_front() {
        let mut pending: VecDeque<Heartbeat> = [beat(30)].into();
        requeue(&mut pending, vec![beat(10), beat(20)]);
        let ends: Vec<_> = pending.iter().map(|b| b.end.unwrap()).collect();
        assert_eq!(ends, vec![at(10), at(20), at(30)]);
    }

    #[test]
    fn the_connection_code_round_trips() {
        use base64::Engine as _;
        let code = connection_code("http://127.0.0.1:7399", "tok/en+=", "laptop");
        let encoded = code.strip_prefix("everyday-screentime:").expect("prefix");
        assert!(!encoded.contains('=') && !encoded.contains('+') && !encoded.contains('/'));
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            value,
            json!({ "endpoint": "http://127.0.0.1:7399", "token": "tok/en+=", "device": "laptop" })
        );
    }

    #[test]
    fn the_config_defaults_to_tracking_on_the_standard_port() {
        let config = MachineConfig::default();
        assert!(config.tracking);
        assert_eq!(config.port, 7399);
        assert!(!config.device.is_empty());
        assert!(config.gnome_token_device.is_none());

        // A file from an older build, or a hand-edited one, fills the rest.
        let mut partial: MachineConfig =
            serde_json::from_str(r#"{"tracking": false, "device": "  "}"#).unwrap();
        partial.normalize();
        assert!(!partial.tracking);
        assert_eq!(partial.port, 7399);
        assert_eq!(partial.device, default_device());
    }

    #[test]
    fn the_config_round_trips_through_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = MachineConfig { device: "desk".into(), ..MachineConfig::default() };
        config.browsers.insert("firefox".into(), "dev-1".into());
        config.save(dir.path()).unwrap();
        assert_eq!(MachineConfig::load(dir.path()), config);
    }

    #[test]
    fn hostnames_become_friendly_names() {
        assert_eq!(friendly_device("hari-laptop.local"), "hari-laptop");
        assert_eq!(friendly_device(" box.example.com.\n"), "box");
        assert_eq!(friendly_device("DESKTOP-AB12"), "DESKTOP-AB12");
        assert_eq!(friendly_device("192.168.1.5"), "192.168.1.5");
        assert_eq!(friendly_device(""), "computer");
    }

    #[test]
    fn names_are_checked() {
        assert_eq!(clean_device("  desk  ").unwrap(), "desk");
        assert!(clean_device("   ").is_err());
        assert!(clean_device("a\nb").is_err());

        assert_eq!(clean_browser("Firefox").unwrap(), "firefox");
        assert_eq!(clean_browser("chrome-beta").unwrap(), "chrome-beta");
        assert!(clean_browser("").is_err());
        assert!(clean_browser("-x").is_err());
        assert!(clean_browser("fire fox").is_err());
        assert!(clean_browser(&"a".repeat(33)).is_err());
        assert_eq!(display_browser("chrome"), "Chrome");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn gsettings_string_arrays_parse_and_print() {
        assert_eq!(gnome::parse_strv("@as []"), Vec::<String>::new());
        assert_eq!(
            gnome::parse_strv("['a@b', 'screentime@everyday.journal']\n"),
            vec!["a@b".to_string(), "screentime@everyday.journal".to_string()]
        );
        assert_eq!(gnome::format_strv(&["a@b".into(), "c@d".into()]), "['a@b', 'c@d']");
        assert_eq!(gnome::format_strv(&[]), "[]");
    }
}
