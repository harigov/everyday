//! Opening this window's vault in a browser on the tailnet.
//!
//! The desktop app's half of web access, as `sharing.rs` is sharing's:
//! everything about *how* -- the network check, the certificate, sessions,
//! routes -- is in [`everyday_server::web`]; what is here is the switch in
//! Settings, remembered in `web.json`, and the interface's files, which this
//! process already carries because Tauri embedded them.
//!
//! Composes with sharing and MCP the way they compose with each other: this
//! module starts and stops its listener and nothing else, and
//! [`crate::state::AppState::recompose_events`] folds its sink into the
//! fan-out after any of them changes.

use everyday_server::Registry;
use everyday_server::web::{Asset, Assets, Config, Web, WebStatus};
use everyday_service::error::CommandResult;
use everyday_service::events::EventSink;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct WebAccess {
    running: Mutex<Option<Arc<Web>>>,
}

impl WebAccess {
    fn dir() -> std::path::PathBuf {
        everyday_vault::config_dir()
    }

    pub fn config() -> Config {
        Config::load(&Self::dir())
    }

    pub fn is_running(&self) -> bool {
        self.running.lock().unwrap().is_some()
    }

    /// This switch's part of the event fan-out, if it is on.
    pub fn sink(&self) -> Option<Arc<dyn EventSink>> {
        self.running.lock().unwrap().as_ref().map(|web| web.sink())
    }

    pub fn status(&self) -> WebStatus {
        match self.running.lock().unwrap().as_ref() {
            Some(web) => web.status(),
            None => WebStatus::off(&Self::config()),
        }
    }

    /// Switch it on. The caller recomposes the event fan-out once this
    /// returns, as it does for sharing and MCP.
    ///
    /// `wait` is for somebody who just threw the switch and is looking at the
    /// pane: it waits for the first look at Tailscale (up to fifteen seconds)
    /// so the status says whether it worked. Starting up passes `false`, so
    /// a slow tailscaled or a first certificate never holds the window.
    pub async fn start(
        &self,
        service: Arc<everyday_service::Service>,
        registry: Arc<Registry>,
        assets: Assets,
        mut config: Config,
        wait: bool,
    ) -> CommandResult<WebStatus> {
        // Awaited, not merely signalled: what follows binds the same port.
        self.stop_and_wait().await;
        let name = service.get().map(|v| v.status().name).unwrap_or_else(|| "Every Day".into());
        config.enabled = true;
        config.save(&Self::dir())?;
        let web = Arc::new(Web::start(service, registry, assets, config, name));
        if wait {
            web.first_look(std::time::Duration::from_secs(15)).await;
        }
        let status = web.status();
        *self.running.lock().unwrap() = Some(web);
        Ok(status)
    }

    pub async fn stop_and_wait(&self) {
        let running = self.running.lock().unwrap().take();
        if let Some(web) = running {
            web.stop_and_wait().await;
        }
    }

    /// Switch it off, and remember not to switch it on next time.
    pub async fn stop_and_remember(&self) -> CommandResult<WebStatus> {
        self.stop_and_wait().await;
        let dir = Self::dir();
        let mut config = Config::load(&dir);
        config.enabled = false;
        config.save(&dir)?;
        Ok(WebStatus::off(&config))
    }
}

/// The interface's files, out of this binary: Tauri embedded `ui/dist` at
/// build time (and in development reads it from disk instead -- see
/// `AssetResolver::get`).
///
/// Asked only for files the build actually has. Tauri's own lookup answers
/// `index.html` for anything it cannot find, which is right for its window
/// and wrong here: a stylesheet that is not there would come back as a page
/// of HTML. The web routes decide for themselves which addresses are places
/// in the interface. (In development nothing is embedded, the list is empty,
/// and Tauri reads the folder instead -- which answers a missing file with
/// nothing, as wanted.)
pub fn assets<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Assets {
    let resolver = app.asset_resolver();
    let embedded: std::collections::HashSet<String> =
        resolver.iter().map(|(key, _)| key.trim_start_matches('/').to_string()).collect();
    Assets::new(move |path| {
        if !embedded.is_empty() && !embedded.contains(path) {
            return None;
        }
        resolver.get(path.to_string()).map(|asset| Asset {
            bytes: std::borrow::Cow::Owned(asset.bytes),
            mime: asset.mime_type,
        })
    })
}
