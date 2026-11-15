//! Health on this machine: connecting the phone's Health Auto Export, and
//! picking an Apple Health export to import.
//!
//! The phone pushes to web access (`docs/plans/web-access.md`): the one
//! listener it can reach with a certificate it already trusts. Setting it up
//! mints a write-only token ([`Scope::HealthIngest`]) for one source and
//! hands back the address and the token, once, for the person to paste into
//! the automation's URL and `Authorization` header.
//!
//! # Which token belongs to which source
//!
//! `health.json`, beside `mcp.json` in the configuration directory, remembers
//! each source's token by its registry device id -- never the token, which
//! only the person's pasted copy and the registry's hash of it hold. Setting
//! a source up again mints a new token and revokes the old one, so the device
//! list never fills with tokens nobody has and a token handed out once is
//! never valid twice over. It is written private, as `screentime.json` is: it
//! says which rows of the device list are a phone's.
//!
//! # A token is bound to its source
//!
//! A `healthIngest` token is the one most likely to leak, so it may push to
//! one source only: the vault remembers the registry id of each source's
//! token (`set_health_source_token`) and `record_health_push` refuses a
//! token that is not its source's. Setting a source up binds the new token
//! before anything else is changed, so a failure leaves the old one working;
//! revoking clears the binding as well as the registry row.
//!
//! # Both commands are about this machine
//!
//!
//! The token is a row in this machine's device list and the address is this
//! machine's own tailnet name, so a window looking at another machine's vault
//! has nothing to set up here: the phone would be pushing into the wrong
//! computer's vault. An export is read from a path on the machine that holds
//! the vault, for the same reason. Both are refused, in words, in that case.
//!
//! [`Scope::HealthIngest`]: karmayoga_service::Scope::HealthIngest

use std::collections::BTreeMap;
use std::path::Path;

use karmayoga_core::id::HealthSourceId;
use karmayoga_service::Scope;
use karmayoga_service::ctx::Ctx;
use karmayoga_service::error::{CommandError, CommandResult, codes};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;

use crate::state::AppState;

/// `health.json`, in the configuration directory.
const CONFIG_FILE: &str = "health.json";

/// What the token is called in Settings → Sharing's device list.
const TOKEN_NAME: &str = "Health Auto Export";

/// What Settings says when web access is off.
const WEB_OFF: &str =
    "Turn on web access in Settings → Web access so your phone can reach this computer.";

/// Held while `health.json` is read, changed and written and while the vault
/// is told which token is a source's, so two setups at once cannot each
/// revoke the other's token.
static CONFIG_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// What the setup screen shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushSetup {
    /// `https://<machine>.<tailnet>.ts.net:7400/v1/health/hae/<source id>`,
    /// empty when web access is off or has no name yet.
    pub url: String,
    /// The bearer token, shown once. Empty when `url` is.
    pub token: String,
    /// Web access is on and serving.
    pub web_on: bool,
    /// Why there is no address, in a sentence, when there is none.
    #[serde(default)]
    pub problem: String,
}

/// `health.json`. Every field defaults, so a file from an older build, or
/// none at all, reads as "no source has a token".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct PushConfig {
    /// Health source id → the registry device id of its push token.
    sources: BTreeMap<String, String>,
}

impl PushConfig {
    fn load(dir: &Path) -> Self {
        let path = dir.join(CONFIG_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), error = %e, "health: config unreadable");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    fn save(&self, dir: &Path) -> CommandResult<()> {
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| CommandError::new(codes::INTERNAL, e.to_string()))?;
        karmayoga_core::fsutil::create_private_dir(dir)
            .map_err(|e| CommandError::new(codes::IO, e.to_string()))?;
        karmayoga_core::fsutil::write_atomic_private(
            &dir.join(CONFIG_FILE),
            text.as_bytes(),
            &karmayoga_core::fsutil::unique_tag(),
        )
        .map_err(|e| CommandError::new(codes::IO, e.to_string()))
    }
}

/// `source_id` as the id it names, or a refusal in words.
fn parse_source(source_id: &str) -> CommandResult<HealthSourceId> {
    source_id
        .trim()
        .parse()
        .map_err(|_| CommandError::new(codes::INVALID, "that is not the id of a Health source"))
}

/// The address the phone posts to, or why there is none yet.
///
/// `urls` are web access's own addresses, best first, as its status lists
/// them (`https://<name>:<port>/`, with the port left out for 443). Only
/// `running` and an address together make one: web access that is on but has
/// no tailnet name yet gets Web access's own explanation of what is wrong,
/// when it has one.
fn push_address(
    running: bool,
    urls: &[String],
    problem: Option<&str>,
    source: HealthSourceId,
) -> Result<String, String> {
    if !running {
        return Err(WEB_OFF.to_string());
    }
    match urls.first() {
        Some(base) => Ok(format!("{}/v1/health/hae/{source}", base.trim_end_matches('/'))),
        None => Err(problem.map(str::to_string).unwrap_or_else(|| {
            "Web access is on but this computer has no address on your tailnet yet. Check that \
             Tailscale is running."
                .to_string()
        })),
    }
}

/// Refuse what only the machine holding the vault can do.
fn require_local(app: &AppHandle, what: &str) -> CommandResult<()> {
    if app.state::<AppState>().is_remote() {
        return Err(CommandError::new(
            codes::UNSUPPORTED,
            format!("{what} has to be done on the computer that holds this vault."),
        ));
    }
    Ok(())
}

/// Mint (or re-mint, revoking the old) the push token for `source_id`.
#[tauri::command]
pub async fn health_push_setup(app: AppHandle, source_id: String) -> CommandResult<PushSetup> {
    let source = parse_source(&source_id)?;
    require_local(&app, "Connecting the phone")?;

    let state = app.state::<AppState>();
    let web = state.web().status();
    let url = match push_address(web.running, &web.urls, web.problem.as_deref(), source) {
        Ok(url) => url,
        // No token is minted for an address nobody can be shown: it would
        // be a row in the device list that no phone holds.
        Err(problem) => {
            return Ok(PushSetup {
                url: String::new(),
                token: String::new(),
                web_on: web.running,
                problem,
            });
        }
    };

    let registry = state.registry()?;
    let _guard = CONFIG_LOCK.lock().await;
    let dir = karmayoga_vault::config_dir();
    let mut config = PushConfig::load(&dir);
    let (token, id) = registry.issue(TOKEN_NAME, vec![Scope::HealthIngest], false)?;
    // Bound in the vault first, while the old token still works: a token the
    // vault does not know as the source's is refused, so one that could not
    // be bound is of no use to anybody.
    if let Err(e) = bind_token(&app, source, &id).await {
        let _ = registry.revoke(&id);
        return Err(e);
    }
    let old = config.sources.insert(source.to_string(), id.clone());
    if let Err(e) = config.save(&dir) {
        let _ = registry.revoke(&id);
        if let Err(again) = bind_token(&app, source, old.as_deref().unwrap_or("")).await {
            tracing::warn!(code = %again.code, "health: could not put a source's old token back");
        }
        return Err(e);
    }
    if let Some(old) = old.filter(|old| *old != id)
        && let Err(e) = registry.revoke(&old)
    {
        tracing::warn!(error = %e, "health: could not revoke a source's old push token");
    }
    tracing::info!(source = %source, "health: a push token was issued");
    Ok(PushSetup { url, token, web_on: true, problem: String::new() })
}

/// Revoke `source_id`'s push token. Answers whether there was one.
#[tauri::command]
pub async fn health_push_revoke(app: AppHandle, source_id: String) -> CommandResult<bool> {
    let source = parse_source(&source_id)?;
    require_local(&app, "Disconnecting the phone")?;

    let registry = app.state::<AppState>().registry()?;
    let _guard = CONFIG_LOCK.lock().await;
    let dir = karmayoga_vault::config_dir();
    let mut config = PushConfig::load(&dir);
    let Some(id) = config.sources.get(&source.to_string()).cloned() else {
        return Ok(false);
    };
    // Revoked first: a token that is still good must never be left without
    // a record of whose it is. Were the write below to fail, the entry only
    // names a row that is already gone.
    let revoked = registry.revoke(&id)?;
    // Then the vault is told the source has none. Not fatal: the registry no
    // longer knows the token, so nothing can push with it whatever the vault
    // still remembers -- and a source that is already deleted, or a vault
    // that is locked, is no reason to keep the entry here.
    match bind_token(&app, source, "").await {
        Ok(()) => {}
        Err(e) if e.code == codes::NOT_FOUND => {}
        Err(e) => {
            tracing::warn!(code = %e.code, "health: could not clear a source's push token");
        }
    }
    config.sources.remove(&source.to_string());
    config.save(&dir)?;
    Ok(revoked)
}

/// Tell the vault which registry token may push to `source`; an empty
/// `token_id` is none. The command wants `all`, which the shell's own call
/// holds.
async fn bind_token(app: &AppHandle, source: HealthSourceId, token_id: &str) -> CommandResult<()> {
    let session = app.state::<AppState>().session().as_session();
    session.call(Ctx::local(), "set_health_source_token", binding(source, token_id)).await?;
    Ok(())
}

/// The arguments of `set_health_source_token`.
fn binding(source: HealthSourceId, token_id: &str) -> Value {
    json!({ "sourceId": source.to_string(), "tokenId": token_id })
}

/// Ask for an `export.zip` and start importing it (`import_apple_health`).
/// Answers the import's status, or `None` when nothing was picked. The
/// path never touches the webview, as an archive import's does not.
#[tauri::command]
pub async fn health_import_apple(app: AppHandle) -> CommandResult<Option<serde_json::Value>> {
    require_local(&app, "Importing an Apple Health export")?;
    let Some(path) = ask_which(&app).await else { return Ok(None) };
    let path = path.to_str().ok_or_else(|| {
        CommandError::new(codes::INVALID, "that file's name cannot be read by this app")
    })?;
    let session = app.state::<AppState>().session().as_session();
    let status = session.call(Ctx::local(), "import_apple_health", json!({ "path": path })).await?;
    Ok(Some(status))
}

async fn ask_which(app: &AppHandle) -> Option<std::path::PathBuf> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Choose your Apple Health export")
        .add_filter("Apple Health export", &["zip"])
        .pick_file(move |path| {
            let _ = tx.send(path);
        });
    rx.await.ok().flatten().and_then(|p| p.into_path().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> HealthSourceId {
        HealthSourceId::parse("0192f1c0-0000-7000-8000-000000000001").unwrap()
    }

    #[test]
    fn the_address_is_web_accesss_first_name_and_the_sources_own_path() {
        let id = source();
        let named = push_address(
            true,
            &["https://desk.tail1234.ts.net:7400/".to_string(), "https://other/".to_string()],
            None,
            id,
        );
        assert_eq!(
            named.unwrap(),
            "https://desk.tail1234.ts.net:7400/v1/health/hae/0192f1c0-0000-7000-8000-000000000001"
        );
        // Port 443 is left out of web access's addresses, and a name with no
        // trailing slash is joined the same way.
        let bare = push_address(true, &["https://desk.tail1234.ts.net".to_string()], None, id);
        assert_eq!(
            bare.unwrap(),
            "https://desk.tail1234.ts.net/v1/health/hae/0192f1c0-0000-7000-8000-000000000001"
        );
    }

    #[test]
    fn with_web_access_off_the_problem_says_where_to_turn_it_on() {
        let off = push_address(false, &[], None, source()).unwrap_err();
        assert!(off.contains("Settings") && off.contains("Web access"), "{off}");
        // Off wins over whatever stale address was left in the status.
        let stale = push_address(false, &["https://x/".to_string()], None, source());
        assert_eq!(stale.unwrap_err(), off);
    }

    #[test]
    fn on_with_no_name_yet_says_what_web_access_says_or_asks_about_tailscale() {
        let said = push_address(true, &[], Some("Tailscale is not running."), source());
        assert_eq!(said.unwrap_err(), "Tailscale is not running.");
        let unsaid = push_address(true, &[], None, source()).unwrap_err();
        assert!(unsaid.contains("Tailscale"), "{unsaid}");
    }

    #[test]
    fn the_binding_names_the_source_and_the_token_by_the_registrys_id() {
        assert_eq!(
            binding(source(), "device-1"),
            json!({ "sourceId": "0192f1c0-0000-7000-8000-000000000001", "tokenId": "device-1" })
        );
        // Nothing is how a source is told it has no token.
        assert_eq!(binding(source(), "")["tokenId"], "");
    }

    #[test]
    fn a_source_id_is_checked() {
        assert!(parse_source(" 0192f1c0-0000-7000-8000-000000000001 ").is_ok());
        for bad in ["", "not-an-id", "../../etc/passwd", "0192f1c0"] {
            let e = parse_source(bad).unwrap_err();
            assert_eq!(e.code, codes::INVALID, "{bad:?}");
        }
    }

    #[test]
    fn the_config_round_trips_through_its_file_and_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(PushConfig::load(dir.path()), PushConfig::default());

        let mut config = PushConfig::default();
        config.sources.insert(source().to_string(), "device-1".to_string());
        config.save(dir.path()).unwrap();
        assert_eq!(PushConfig::load(dir.path()), config);

        // A file that is not ours reads as nothing, not as a crash.
        std::fs::write(dir.path().join(CONFIG_FILE), b"not json").unwrap();
        assert_eq!(PushConfig::load(dir.path()), PushConfig::default());
    }

    #[cfg(unix)]
    #[test]
    fn the_config_is_written_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        PushConfig::default().save(dir.path()).unwrap();
        let mode = std::fs::metadata(dir.path().join(CONFIG_FILE)).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }
}
