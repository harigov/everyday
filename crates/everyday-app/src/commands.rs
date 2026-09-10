//! What the webview can call.
//!
//! Eleven entries, where there were ninety. Everything a vault can be asked to
//! do is in [`everyday_service`] now and is reached through [`call`], which
//! takes a name and a bag of JSON and hands back JSON. What is left here is the
//! handful of things that are about *this process* rather than about a vault.
//!
//! # Which things, and why they cannot be commands
//!
//! * **Opening and creating a vault**, and `bootstrap`, which decides what this
//!   session is about. A vault is handed to a service; a service does not go
//!   looking for one, and a remote client must not be able to make the server
//!   open a different vault.
//! * **`ready_to_close`**, which destroys the window.
//! * **The tray**, which is a platform menu this process owns.
//! * **`put_blob`**, which is bytes rather than JSON -- see its own comment.
//! * **`send_message`**, which answers with a stream rather than a value.
//!
//! # `async fn`, always
//!
//! A `#[tauri::command]` on a *synchronous* function is dispatched inline on
//! the thread that runs the platform's UI loop, so it does not merely miss the
//! blocking pool -- it holds the window while it runs. Everything here is
//! `async` and the service does its own hop to the blocking pool inside.

use everyday_core::{BlobId, Journal, Vault, VaultConfig, VaultStatus};
use everyday_service::agent::AgentEvent;
use everyday_service::ctx::Ctx;
use everyday_service::error::{CommandError, CommandResult};
use everyday_service::service::blocking;
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{Manager, State};

use crate::remote::{self, Remote};
use crate::remotes;
use crate::state::AppState;
use crate::tray::{Tray, TrayItem};
use everyday_server::client::{Connection, RemoteClient};

/// What the interface needs before any vault is open.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    pub vault_exists: bool,
    /// The vault location in play: the one last opened if it is still there,
    /// otherwise where a new vault would be created.
    pub default_path: PathBuf,
    /// Every backend this build can open, with the fields each one needs
    /// configuring. The setup screen renders these rather than knowing that
    /// Postgres exists.
    pub backends: Vec<everyday_core::BackendInfo>,
    pub status: Option<VaultStatus>,
    /// The command surface this build speaks. The interface does not use it;
    /// it is here so that a mismatch is visible in a bug report.
    pub protocol: u32,
    /// Vaults on other computers this copy has paired with, most recent last.
    /// The picker offers them beside "open a vault".
    pub remotes: Vec<Connection>,
    /// The connection this window is on, if it is on one.
    pub remote: Option<Connection>,
    /// Whether this machine holds the key to the local vault, so that it
    /// opens without a password when the process starts. Off unless somebody
    /// turned it on; see `autounlock`.
    pub opens_itself: bool,
}

/// Turn "open this vault without a password" on or off, for this machine.
///
/// The password is asked for on the way *on* rather than taken from the open
/// vault, because this is the one switch whose whole effect is that the
/// password stops being needed -- so pressing it should cost the password
/// once, from somebody who knows it, rather than being available to anybody
/// who wandered past an unlocked screen.
#[tauri::command]
pub async fn set_opens_itself(
    state: State<'_, AppState>,
    on: bool,
    password: Option<String>,
) -> CommandResult<bool> {
    let Some(path) = state.service().last_path() else {
        return Err(CommandError::new("no_vault", "no vault is open"));
    };
    // Off the async runtime, all of it. The keychain is a blocking call on
    // every platform and on Linux it is a D-Bus round trip that can sit there
    // for the length of an unlock prompt -- and a Tauri worker held for that
    // long is a window that has stopped answering.
    if !on {
        let at = path.clone();
        return blocking(move || {
            everyday_vault::autounlock::forget(&at)?;
            Ok(false)
        })
        .await;
    }
    let vault = state.service().require()?;
    blocking(move || {
        let key = vault.export_data_key(password.as_deref())?;
        everyday_vault::autounlock::remember(&path, &key)?;
        Ok(true)
    })
    .await
}

#[tauri::command]
pub async fn bootstrap(state: State<'_, AppState>) -> CommandResult<Bootstrap> {
    let service = state.service();
    // A window already looking at another machine's vault is not one that
    // should quietly open a local one underneath it.
    if state.is_remote() {
        let session = state.session().as_session();
        return Ok(Bootstrap {
            vault_exists: true,
            default_path: everyday_vault::default_vault_dir(),
            backends: everyday_vault::available_backends(),
            status: session.status().await,
            protocol: everyday_service::PROTOCOL,
            remotes: remotes::list(),
            remote: session.connection(),
            // The key to a vault on another computer is that computer's
            // business, and this switch is about the one held here.
            opens_itself: false,
        });
    }

    // Prefer the vault this user last had open. Only fall back to the default
    // location when nothing was recorded or what was recorded is gone -- an
    // external disk that is not plugged in should show the setup screen, not an
    // error about a path the user cannot see.
    let path = service
        .last_path()
        .filter(|p| everyday_vault::exists(p))
        .unwrap_or_else(everyday_vault::default_vault_dir);
    let backends = everyday_vault::available_backends();

    // Open eagerly so an unencrypted vault is usable immediately and an
    // encrypted one can name itself on the lock screen.
    if service.get().is_none() && everyday_vault::exists(&path) {
        let opened = {
            let path = path.clone();
            blocking(move || everyday_vault::open(&path).map_err(CommandError::from)).await
        };
        match opened {
            Ok(vault) => {
                let vault = service.set(vault);
                // If this machine has been told to, open it without asking.
                // Best effort in every direction: a keychain that cannot be
                // reached, or a key that no longer fits because the password
                // was changed elsewhere, is a lock screen -- which is what
                // would have happened anyway.
                //
                // Off the runtime, because this is the startup path and the
                // keychain can block: a window that has drawn nothing yet
                // must not be waiting on D-Bus.
                let at = path.clone();
                let unlocked = blocking(move || {
                    let Some(key) = everyday_vault::autounlock::recall(&at) else {
                        return Ok(());
                    };
                    if !vault.is_unlocked() {
                        vault.unlock_with_key(key.as_str())?;
                    }
                    Ok(())
                })
                .await;
                if let Err(e) = unlocked {
                    tracing::warn!(error = %e, "the key in the keychain did not open the vault");
                }
            }
            // A vault we cannot open is not fatal: the interface should still
            // start and be able to say why.
            Err(e) => tracing::warn!(error = %e, "could not open the vault at startup"),
        }
    }

    // A machine that was sharing when it was shut down is sharing when it comes
    // back. Done here rather than in `setup` because there is nothing to serve
    // until a vault is open, and this is the call that opens it.
    let config = crate::sharing::Sharing::config();
    if config.enabled
        && service.get().is_some()
        && !state.sharing().status().sharing
        && let Some(sink) = state.sink()
        && let Err(e) = state.sharing().start(service.clone(), sink, config).await
    {
        // Not fatal, and not a dialog. A port already taken, or a network
        // that is not up yet, should not stop somebody reading their journal;
        // the sharing pane says what happened when they go looking.
        tracing::warn!(error = %e, "could not resume sharing this vault");
    }

    Ok(Bootstrap {
        vault_exists: everyday_vault::exists(&path),
        opens_itself: {
            let at = path.clone();
            blocking(move || Ok(everyday_vault::autounlock::enabled(&at))).await?
        },
        default_path: path,
        backends,
        status: service.get().map(|v| v.status()),
        protocol: everyday_service::PROTOCOL,
        remotes: remotes::list(),
        remote: None,
    })
}

#[tauri::command]
pub async fn create_vault(
    state: State<'_, AppState>,
    path: PathBuf,
    name: String,
    backend: String,
    // `settings` is whatever the chosen backend asked for in its spec -- a
    // connection URL, a schema name -- and is absent for a local vault. It is
    // sealed under the vault key once the vault exists; see `VaultHeader`.
    settings: Option<everyday_core::BackendSettings>,
    password: Option<String>,
) -> CommandResult<VaultStatus> {
    if let Some(p) = password.as_deref() {
        everyday_vault::validate_password(p)?;
    }
    let settings = settings.unwrap_or_default();
    // Checked here as well as inside `create`, so a missing connection URL is a
    // message on the setup screen and not a half-made vault directory.
    everyday_vault::validate_settings(&backend, &settings)?;
    let config = VaultConfig {
        name,
        backend,
        settings,
        password,
        kdf: Default::default(),
        auto_lock_seconds: 15 * 60,
        // Never, by default. The machine holding a vault serves it -- to its
        // own window, to a phone, to the assistant -- and a key that went
        // away because one keyboard was idle would take all of that with it.
        forget_key_seconds: 0,
    };
    let service = state.service();
    state.disconnect();
    service.close();
    let created = {
        let path = path.clone();
        blocking(move || {
            let vault = everyday_vault::create(&path, config)?;
            // A vault with no journal is a dead end; give it one.
            vault.save_journal(&Journal::new("Journal"))?;
            Ok(vault)
        })
        .await?
    };
    let vault = service.set(created);
    Ok(vault.status())
}

#[tauri::command]
pub async fn open_vault(state: State<'_, AppState>, path: PathBuf) -> CommandResult<VaultStatus> {
    let service = state.service();
    // Back to a vault in this process, if this window was looking elsewhere.
    state.disconnect();
    // Release the vault we already hold first. Its write lock is this process's,
    // and opening a second vault -- including the same one again -- while still
    // holding it would come up read-only. See `Service::close`.
    service.close();
    let opened = {
        let path = path.clone();
        blocking(move || everyday_vault::open(&path).map_err(CommandError::from)).await?
    };
    let vault = service.set(opened);
    Ok(vault.status())
}

/// Run any command in the service by name.
///
/// The one entry point for everything the interface does to a vault. `args` is
/// the same bag of named arguments each command took as separate parameters
/// before, so nothing about the wire changed when they moved.
///
/// `request_id` is minted by the interface for a write. It does nothing on a
/// local vault, where a command cannot half-happen, and everything on a remote
/// one -- see `everyday_service::idempotency`. Carrying it on both paths is
/// what keeps the two identical.
#[tauri::command]
pub async fn call(
    state: State<'_, AppState>,
    name: String,
    args: Option<Value>,
    request_id: Option<String>,
) -> CommandResult<Value> {
    let session = state.session().as_session();
    let ctx = Ctx::local().with_request_id(request_id);
    session.call(ctx, &name, args.unwrap_or(Value::Null)).await
}

// ---- looking at another computer's vault --------------------------------

/// Every server this copy has paired with.
#[tauri::command]
pub async fn list_remotes() -> CommandResult<Vec<Connection>> {
    Ok(remotes::list())
}

/// What connecting answers with.
///
/// Both halves, because the caller needs both and cannot derive either from
/// the other. The interface used to match the new connection out of the list
/// by comparing the vault's *name*, which is wrong the moment somebody has two
/// machines each holding a vault called "Journal" -- and that is not an exotic
/// case, it is the default name.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Connected {
    pub status: VaultStatus,
    pub connection: Connection,
}

/// Pair with a server from the link somebody copied, and connect to it.
///
/// One command rather than pair-then-connect, for the reason `subscribe_calendar`
/// is one command: the two are not independent. A pairing that succeeded and a
/// connection that then failed would leave a row in the picker whose token the
/// user has no way to know is good.
#[tauri::command]
pub async fn connect_remote(state: State<'_, AppState>, link: String) -> CommandResult<Connected> {
    let invite = everyday_server::pairing::parse(&link)
        .map_err(|message| CommandError::new("invalid", message))?;
    let device_name = device_name();
    let (client, token) = RemoteClient::pair(&invite, &device_name).await?;
    remotes::remember(client.connection(), &token)?;
    attach(&state, client).await
}

/// Reconnect to a server this copy already paired with.
#[tauri::command]
pub async fn reconnect_remote(state: State<'_, AppState>, id: String) -> CommandResult<Connected> {
    let client = remote::resume(&id).await?;
    attach(&state, client).await
}

/// Stop looking at another computer's vault. The pairing survives.
#[tauri::command]
pub async fn disconnect_remote(state: State<'_, AppState>) -> CommandResult<()> {
    state.disconnect();
    Ok(())
}

/// Forget a pairing: the connection and its token.
#[tauri::command]
pub async fn forget_remote(state: State<'_, AppState>, id: String) -> CommandResult<()> {
    if state.session().as_session().connection().is_some_and(|c| c.id == id) {
        state.disconnect();
    }
    remotes::forget(&id)
}

async fn attach(state: &State<'_, AppState>, client: RemoteClient) -> CommandResult<Connected> {
    let sink =
        state.sink().ok_or_else(|| CommandError::new("internal", "the window is not ready yet"))?;
    let connection = client.connection().clone();
    let remote = Remote::start(client, sink);
    state.connect(remote);
    let status =
        state.session().as_session().status().await.ok_or_else(|| {
            CommandError::new("network", "that computer did not say what it holds")
        })?;
    Ok(Connected { status, connection })
}

/// What to call this machine in the other one's device list.
///
/// The host name, because that is what somebody looking at a list of paired
/// devices at two in the morning will recognise. Not an identifier of any kind:
/// it is shown to the person who owns both machines and to nobody else.
fn device_name() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty())
        })
        .unwrap_or_else(|| "A computer".to_string())
}

/// Store an attachment and return its content address.
///
/// The bytes arrive as the request's *raw body* rather than as a named
/// argument, and that is the whole point of the odd signature. Tauri serialises
/// command arguments as JSON, and a `Uint8Array` inside a JSON object becomes an
/// array of numbers -- so a 100 MB video was turned into a hundred million
/// JavaScript numbers, stringified into roughly 350 MB of text, and parsed back
/// a byte at a time on this side. It froze the window for as long as it took
/// and peaked at something over a gigabyte of memory for a file the disk had
/// already handed us as a buffer.
///
/// A payload that *is* an `ArrayBuffer` at the top level is sent as
/// `application/octet-stream` instead and lands here as bytes.
#[tauri::command]
pub async fn put_blob(
    state: State<'_, AppState>,
    request: tauri::ipc::Request<'_>,
) -> CommandResult<String> {
    let bytes = match request.body() {
        tauri::ipc::InvokeBody::Raw(bytes) => bytes.clone(),
        // The slow path, kept working rather than refused. Tauri falls back to
        // `postMessage` when a webview blocks its custom protocol, and there a
        // buffer really does arrive as an array of numbers. Losing attachments
        // entirely in that configuration would be a worse outcome than the
        // allocation this exists to avoid.
        tauri::ipc::InvokeBody::Json(value) => serde_json::from_value::<Vec<u8>>(value.clone())
            .map_err(|_| CommandError::new("invalid", "attachment payload was not a buffer"))?,
    };
    state.session().as_session().put_blob(&Ctx::local(), bytes).await
}

/// Say something to the assistant, and stream what it says back.
///
/// The reply arrives on `channel` rather than as this command's return value: a
/// turn takes seconds and calls tools while it runs, and a panel that could draw
/// none of that until the end would read as a hang. What this returns is only
/// whether the turn finished.
#[tauri::command]
pub async fn send_message(
    state: State<'_, AppState>,
    conversation_id: everyday_core::ConversationId,
    prompt: String,
    context: Option<String>,
    channel: tauri::ipc::Channel<AgentEvent>,
) -> CommandResult<()> {
    let session = state.session().as_session();
    let sink: everyday_service::agent::Sink = Arc::new(move |event| {
        let _ = channel.send(event);
    });
    session
        .send_message(
            Ctx::local(),
            serde_json::json!({
                "conversationId": conversation_id,
                "prompt": prompt,
                "context": context,
            }),
            sink,
        )
        .await
}

/// The interface reporting that its pending writes have landed.
///
/// Second half of the close handshake begun in `run`'s `CloseRequested`
/// handler: that one cancelled the close and asked for a flush, this one
/// completes it. Locking here rather than in the window handler is what makes
/// the ordering right -- the key is dropped after the last write, not before it.
#[tauri::command]
pub async fn ready_to_close(
    window: tauri::Window,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    if let Some(vault) = state.service().get() {
        blocking(move || {
            // Best-effort: a checkpoint failing is not a reason to refuse to
            // quit, and the data is committed either way.
            let _ = vault.with_store(|s| s.flush());
            vault.lock();
            Ok(())
        })
        .await?;
    }
    window.destroy().map_err(|e| CommandError::new("close_failed", e.to_string()))
}

// ---- sharing this vault -------------------------------------------------

/// What the sharing pane draws.
#[tauri::command]
pub async fn share_status(
    state: State<'_, AppState>,
) -> CommandResult<crate::sharing::ShareStatus> {
    Ok(state.sharing().status())
}

/// Change whether a connected computer may unlock this vault.
///
/// Its own command rather than an argument to `share_start`, because it is not
/// a reason to restart a server: the interface used to call `share_start`
/// again to flip it, which rebound the port and could fail with "address
/// already in use" -- leaving sharing off because somebody moved a switch.
#[tauri::command]
pub async fn set_remote_unlock(
    state: State<'_, AppState>,
    allow: bool,
) -> CommandResult<crate::sharing::ShareStatus> {
    state.sharing().set_remote_unlock(allow)
}

/// Start answering other machines.
#[tauri::command]
pub async fn share_start(
    state: State<'_, AppState>,
    port: Option<u16>,
    address: Option<String>,
    allow_remote_unlock: Option<bool>,
) -> CommandResult<crate::sharing::ShareStatus> {
    // Sharing a vault this window does not hold would be forwarding, which is
    // a thing nobody has asked for and a good way to build a loop.
    if state.is_remote() {
        return Err(CommandError::new(
            "unsupported",
            "this window is looking at a vault on another computer; share it from there",
        ));
    }
    let sink =
        state.sink().ok_or_else(|| CommandError::new("internal", "the window is not ready yet"))?;

    let mut config = crate::sharing::Sharing::config();
    if let Some(unlock) = allow_remote_unlock {
        config.allow_remote_unlock = unlock;
    }
    let port = port.unwrap_or_else(|| config.listen.port());
    let ip: std::net::IpAddr = match address.as_deref() {
        // Every interface. What somebody on a private network wants, and what
        // makes the certificate cover more than one address.
        None | Some("") | Some("0.0.0.0") => std::net::IpAddr::from([0, 0, 0, 0]),
        Some(other) => other
            .parse()
            .map_err(|_| CommandError::new("invalid", format!("{other} is not an address")))?,
    };
    config.listen = std::net::SocketAddr::new(ip, port);

    state.sharing().start(state.service(), sink, config).await
}

/// Stop answering, and remember not to start next time.
#[tauri::command]
pub async fn share_stop(state: State<'_, AppState>) -> CommandResult<crate::sharing::ShareStatus> {
    let sink =
        state.sink().ok_or_else(|| CommandError::new("internal", "the window is not ready yet"))?;
    state.sharing().stop_and_remember(sink, &state.service())
}

/// Offer to pair, for the next five minutes.
#[tauri::command]
pub async fn new_pairing_code(
    state: State<'_, AppState>,
) -> CommandResult<crate::sharing::ShareStatus> {
    state.sharing().invite()
}

/// Withdraw the offer.
#[tauri::command]
pub async fn cancel_pairing(
    state: State<'_, AppState>,
) -> CommandResult<crate::sharing::ShareStatus> {
    Ok(state.sharing().cancel_invite())
}

/// Take a device's access away. It has to pair again to get it back.
#[tauri::command]
pub async fn revoke_device(
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<crate::sharing::ShareStatus> {
    state.sharing().revoke(&id)
}

// ---- the OS-wide hotkey -------------------------------------------------

/// Whether the desktop granted the key that raises the palette.
///
/// Answers false on a Wayland session with no portal, which is a fact about
/// the desktop rather than a failure -- the settings pane says so and offers
/// the tray instead.
#[tauri::command]
pub async fn hotkey_status(app: tauri::AppHandle) -> CommandResult<HotkeyStatus> {
    let mac = cfg!(target_os = "macos");
    Ok(HotkeyStatus {
        registered: crate::hotkey::is_registered(&app),
        shortcut: crate::hotkey::describe(mac),
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyStatus {
    pub registered: bool,
    /// How to write it on screen, from the same value that was claimed.
    pub shortcut: String,
}

/// Claim the key, or give it back.
#[tauri::command]
pub async fn set_hotkey(app: tauri::AppHandle, on: bool) -> CommandResult<HotkeyStatus> {
    if on {
        crate::hotkey::install(&app);
    } else {
        crate::hotkey::remove(&app)?;
    }
    hotkey_status(app).await
}

// ---- the tray -----------------------------------------------------------

/// Put `items` in the tray menu, showing the icon if it is not up yet.
///
/// The whole menu is sent every time rather than patched. A quick action's
/// label, its enabled state and whether it is offered at all are derived from
/// vault state that moves, so a change is as likely to be "this item is gone" as
/// "this item's label differs" -- and there is no patch protocol for that which
/// is simpler than resending six items. The interface only calls this when the
/// description has actually changed, so "every time" is a handful of calls per
/// session.
///
/// False means the desktop has no tray to put an icon in.
#[tauri::command]
pub async fn set_tray_menu(app: tauri::AppHandle, items: Vec<TrayItem>) -> CommandResult<bool> {
    // Off the async runtime like everything else here, though for a different
    // reason: building a menu is a series of hops to the main thread, each of
    // which blocks the caller until the event loop answers.
    blocking(move || app.state::<Tray>().show(&app, &items)).await
}

#[tauri::command]
pub async fn hide_tray(app: tauri::AppHandle) -> CommandResult<()> {
    blocking(move || {
        app.state::<Tray>().hide();
        Ok(())
    })
    .await
}

// ---- media --------------------------------------------------------------

/// Read a blob for the media protocol handler.
pub fn read_blob_range(
    vault: &Arc<Vault>,
    id: BlobId,
    offset: u64,
    len: u64,
) -> everyday_core::Result<Vec<u8>> {
    vault.with_store(|s| s.get_blob_range(id, offset, len))
}

pub fn blob_len(vault: &Arc<Vault>, id: BlobId) -> everyday_core::Result<u64> {
    vault.with_store(|s| s.blob_len(id))
}
