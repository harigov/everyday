//! Settings → Connections: the MCP servers the assistant may use. The
//! talking to them is [`crate::connections`]'; these are its doors.
//!
//! Scope `All`. A stdio connection runs a program on this computer, and any
//! connection lets the assistant act as the person somewhere else; nothing
//! narrower than a paired desktop may add, change or sign one in.
//!
//! `All` is not enough for the program, though. A paired phone and a web
//! session over the tailnet both hold `All`, and a stdio connection is a
//! command line this computer will run: whoever may write one may run
//! anything the person's account can. So a connection that runs a program
//! may be made, pointed at another program (or other arguments, variables or
//! directory), given values for the variables it names, or connected to
//! only by the shell on this computer -- see [`Ctx::is_local`]. What a phone may
//! still do with one is what carries no command: switch it on or off, rename
//! it, choose what its tools may do, remove it. An address on the internet
//! is another matter and is open to every caller, as it always was.
//!
//! Every write answers the view of what it touched, so the panel is always
//! drawn from what the vault says.

use std::collections::BTreeMap;
use std::sync::Arc;

use karmayoga_core::connection::{
    AuthMethod, Connection, ConnectionSecret, ConnectionState, ConnectionStates, Preset,
    SecretsSet, ToolPolicy, Transport, presets,
};
use karmayoga_core::{ConnectionId, Vault};
use serde::{Deserialize, Serialize};

use crate::command;
use crate::connections::{HubError, hub};
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};

use super::Nothing;

/// One connection as the panel draws it. Mirrors the TS `ConnectionView`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionView {
    pub connection: Connection,
    pub state: ConnectionState,
    /// Which secrets are set -- names and yes/no only.
    pub secrets: SecretsSet,
    pub status: ConnectionStatus,
    /// A client is open to it in this process right now.
    pub live: bool,
}

/// The one word the list shows beside a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionStatus {
    /// Switched off.
    Off,
    /// Something it needs is missing: a token for `bearer`, a value for a
    /// header or variable it names, a client ID the last sign-in said was
    /// needed.
    NeedsSetup,
    /// OAuth with no grant yet, or the server refused the credentials.
    NeedsSignIn,
    /// The last attempt failed for another reason; `state.lastError` says.
    Error,
    /// The last attempt worked.
    Ready,
    /// Never tried.
    Unknown,
}

/// Every connection, and the presets to add one from. Mirrors the TS
/// `ConnectionsView`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionsView {
    pub connections: Vec<ConnectionView>,
    pub presets: Vec<Preset>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddConnection {
    #[serde(default)]
    pub preset: Option<String>,
    pub name: String,
    pub transport: Transport,
    pub auth: AuthMethod,
    /// Secrets to store with it, as [`SetConnectionSecrets`] would.
    #[serde(default)]
    pub secrets: Option<SecretsPatch>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateConnection {
    /// The whole connection as edited: name, transport, auth, the two
    /// switches, the policies. Its timestamps are the vault's to set.
    pub connection: Connection,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionRef {
    pub id: ConnectionId,
}

/// Secrets to change. A field left out is left alone; an empty string
/// removes that secret (for `headers` and `env`, that one name's value).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretsPatch {
    #[serde(default)]
    pub bearer: Option<String>,
    #[serde(default)]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub client_secret: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetConnectionSecrets {
    pub id: ConnectionId,
    #[serde(flatten)]
    pub patch: SecretsPatch,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetConnectionToolPolicy {
    pub id: ConnectionId,
    pub tool: String,
    /// `None` goes back to what the tool's own hints say.
    #[serde(default)]
    pub policy: Option<ToolPolicy>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BegunConnectionSignIn {
    pub sign_in_id: String,
    pub url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionSignInRef {
    pub sign_in_id: String,
}

/// Why a caller that is not the shell on this machine may not set up, change
/// or start a program.
const PROGRAMS_ARE_LOCAL: &str = "a connection that runs a program can be set up, changed or \
    started only from this computer's own window: from anywhere else it would be a way to run \
    anything on it";

/// Whether `transport` is a program this computer would run.
fn runs_a_program(transport: &Transport) -> bool {
    matches!(transport, Transport::Stdio { .. })
}

/// Refuse what only the desktop itself may do.
fn require_local(ctx: &Ctx) -> CommandResult<()> {
    if ctx.is_local() {
        Ok(())
    } else {
        Err(CommandError::new(codes::FORBIDDEN, PROGRAMS_ARE_LOCAL))
    }
}

/// A [`HubError`] as the error a command answers with. A sign-in that has
/// a name (cancelled, a client ID needed) keeps it; one that only a sign-in
/// can mend is `forbidden`; the rest take `default`.
fn command_error(e: HubError, default: &'static str) -> CommandError {
    let code = e.code.unwrap_or(if e.needs_sign_in { codes::FORBIDDEN } else { default });
    CommandError::new(code, e.message)
}

/// Whether a named header or variable has a value stored.
fn has(values: &BTreeMap<String, String>, name: &str) -> bool {
    values.get(name).is_some_and(|v| !v.is_empty())
}

/// Whether something the connection names is still to be filled in.
fn needs_setup(connection: &Connection, secret: &ConnectionSecret) -> bool {
    let token_missing = matches!(connection.auth, AuthMethod::Bearer)
        && !secret.bearer.as_ref().is_some_and(|b| !b.is_empty());
    let value_missing = match &connection.transport {
        Transport::Http { headers, .. } => headers.iter().any(|h| !has(&secret.headers, h)),
        Transport::Stdio { env, .. } => env.iter().any(|e| !has(&secret.env, e)),
    };
    token_missing || value_missing
}

/// The one word the list shows beside a connection. What is missing from
/// the form outranks what the server said last: a token not yet pasted is
/// setup, whatever an earlier attempt without one reported.
fn status_of(
    connection: &Connection,
    state: &ConnectionState,
    secret: &ConnectionSecret,
) -> ConnectionStatus {
    if !connection.enabled {
        ConnectionStatus::Off
    } else if needs_setup(connection, secret) {
        ConnectionStatus::NeedsSetup
    } else if state.needs_sign_in
        || (matches!(connection.auth, AuthMethod::OAuth { .. }) && secret.grant.is_none())
    {
        ConnectionStatus::NeedsSignIn
    } else if state.last_error.is_some() {
        ConnectionStatus::Error
    } else if state.last_ok.is_some() {
        ConnectionStatus::Ready
    } else {
        ConnectionStatus::Unknown
    }
}

/// One connection as the panel draws it.
fn view_of(
    vault: &Vault,
    connection: Connection,
    states: &ConnectionStates,
) -> karmayoga_core::Result<ConnectionView> {
    let secret = vault.connection_secret(connection.id)?;
    let state = states.of(connection.id);
    Ok(ConnectionView {
        status: status_of(&connection, &state, &secret),
        live: hub().is_live(connection.id),
        secrets: secret.summary(),
        state,
        connection,
    })
}

/// The view of connection `id`, as the vault has it now.
fn one_view(vault: &Vault, id: ConnectionId) -> karmayoga_core::Result<ConnectionView> {
    let connection = vault.connection(id)?;
    view_of(vault, connection, &vault.connection_states()?)
}

/// Every connection, and the presets.
fn all_views(vault: &Vault) -> karmayoga_core::Result<ConnectionsView> {
    let states = vault.connection_states()?;
    let connections = vault
        .connection_settings()?
        .connections
        .into_iter()
        .map(|c| view_of(vault, c, &states))
        .collect::<karmayoga_core::Result<Vec<_>>>()?;
    Ok(ConnectionsView { connections, presets: presets() })
}

/// `new` unless it is empty, which clears the secret.
fn changed(new: Option<String>, old: Option<String>) -> Option<String> {
    match new {
        Some(v) => Some(v.trim().to_string()).filter(|v| !v.is_empty()),
        None => old,
    }
}

/// Apply a patch to a connection's secrets. A map's empty value removes that
/// name, and a name the connection no longer lists is dropped whether or not
/// the patch mentioned it, so a secret does not outlive the setting that
/// asked for it.
fn apply_patch(secret: &mut ConnectionSecret, patch: SecretsPatch, connection: &Connection) {
    secret.bearer = changed(patch.bearer, secret.bearer.take());
    secret.client_secret = changed(patch.client_secret, secret.client_secret.take());
    for (target, values) in [(&mut secret.headers, patch.headers), (&mut secret.env, patch.env)] {
        for (name, value) in values.into_iter().flatten() {
            let value = value.trim().to_string();
            if value.is_empty() {
                target.remove(&name);
            } else {
                target.insert(name, value);
            }
        }
    }
    let (headers, env): (&[String], &[String]) = match &connection.transport {
        Transport::Http { headers, .. } => (headers.as_slice(), &[][..]),
        Transport::Stdio { env, .. } => (&[][..], env.as_slice()),
    };
    secret.headers.retain(|name, _| headers.contains(name));
    secret.env.retain(|name, _| env.contains(name));
}

/// Every connection, and the presets.
async fn connections(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<ConnectionsView> {
    let vault = svc.require_unlocked()?;
    blocking(move || Ok(all_views(&vault)?)).await
}

/// Add a connection, with any secrets it came with. Does not connect.
async fn add_connection(
    svc: Arc<Service>,
    ctx: Ctx,
    args: AddConnection,
) -> CommandResult<ConnectionView> {
    if runs_a_program(&args.transport) {
        require_local(&ctx)?;
    }
    let vault = svc.require_unlocked()?;
    blocking(move || {
        let mut connection = Connection::new(args.name, args.transport, args.auth);
        connection.preset = args.preset;
        let connection = vault.save_connection(connection)?;
        if let Some(patch) = args.secrets {
            let mut secret = vault.connection_secret(connection.id)?;
            apply_patch(&mut secret, patch, &connection);
            vault.set_connection_secret(connection.id, &secret)?;
        }
        Ok(one_view(&vault, connection.id)?)
    })
    .await
}

/// Save an edited connection. Drops its live client, so the next use
/// connects with what was saved.
async fn update_connection(
    svc: Arc<Service>,
    ctx: Ctx,
    args: UpdateConnection,
) -> CommandResult<ConnectionView> {
    let vault = svc.require_unlocked()?;
    let id = args.connection.id;
    let writing = Arc::clone(&vault);
    let local = ctx.is_local();
    blocking(move || {
        // `save_connection` adds what it does not know; an edit of one that
        // has been removed meanwhile is an error, not a resurrection.
        let before = writing.connection(id)?;
        let mut after = args.connection.normalized();
        // Compared as it is stored, after the same tidying: a program
        // already set up may be switched on or off and renamed from
        // anywhere, but not pointed at anything else, and no connection may
        // become one.
        if !local && runs_a_program(&after.transport) && before.transport != after.transport {
            return Err(CommandError::new(codes::FORBIDDEN, PROGRAMS_ARE_LOCAL));
        }
        // Pointed somewhere else -- another host, another program -- or
        // signing in another way: the token was given to the old one, and
        // the tools it listed, their hints and the person's choices about
        // them, were about the old one too. None of it carries over.
        let moved = before.owner() != after.owner();
        let auth_changed =
            std::mem::discriminant(&before.auth) != std::mem::discriminant(&after.auth);
        if moved {
            after.policies.clear();
        }
        writing.save_connection(after.clone())?;
        if moved || auth_changed {
            let mut secret = writing.connection_secret(id)?;
            secret.grant = None;
            if moved || !matches!(after.auth, AuthMethod::Bearer) {
                secret.bearer = None;
            }
            writing.set_connection_secret(id, &secret)?;
        }
        // Another address on the same host may list other tools: what was
        // listed is not trusted until it is listed again.
        if before.transport != after.transport || auth_changed {
            writing.save_connection_state(id, ConnectionState::default())?;
        }
        Ok(())
    })
    .await?;
    hub().forget(id);
    blocking(move || Ok(one_view(&vault, id)?)).await
}

/// Forget a connection, its state and its secret.
async fn remove_connection(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: ConnectionRef,
) -> CommandResult<ConnectionsView> {
    let vault = svc.require_unlocked()?;
    hub().forget(args.id);
    hub().sign_ins().cancel_connection(args.id);
    blocking(move || {
        vault.remove_connection(args.id)?;
        Ok(all_views(&vault)?)
    })
    .await
}

/// Change some of a connection's secrets.
async fn set_connection_secrets(
    svc: Arc<Service>,
    ctx: Ctx,
    args: SetConnectionSecrets,
) -> CommandResult<ConnectionView> {
    let vault = svc.require_unlocked()?;
    let id = args.id;
    let writing = Arc::clone(&vault);
    let local = ctx.is_local();
    blocking(move || {
        let connection = writing.connection(id)?;
        // The values a program is started with are part of what it runs: a
        // library to load, a script to run first. They are named by the
        // transport, which a phone cannot change, but the values it could.
        if !local && args.patch.env.is_some() && runs_a_program(&connection.transport) {
            return Err(CommandError::new(codes::FORBIDDEN, PROGRAMS_ARE_LOCAL));
        }
        let mut secret = writing.connection_secret(id)?;
        apply_patch(&mut secret, args.patch, &connection);
        writing.set_connection_secret(id, &secret)?;
        // What was refused may be mended by what was just given; the next
        // use is the test of it.
        writing.update_connection_state(id, |state| {
            if state.needs_sign_in {
                state.needs_sign_in = false;
                state.last_error = None;
            }
        })?;
        Ok(())
    })
    .await?;
    hub().forget(id);
    blocking(move || Ok(one_view(&vault, id)?)).await
}

/// Connect and list the tools afresh. Answers the view whether or not that
/// worked: the state says how it went.
async fn check_connection(
    svc: Arc<Service>,
    ctx: Ctx,
    args: ConnectionRef,
) -> CommandResult<ConnectionView> {
    let vault = svc.require_unlocked()?;
    let id = args.id;
    let reading = Arc::clone(&vault);
    let connection = blocking(move || Ok(reading.connection(id)?)).await?;
    // Checking starts the program. Refused before anything is forgotten or
    // tried, so a phone cannot even stop the one that is running.
    if runs_a_program(&connection.transport) {
        require_local(&ctx)?;
    }
    // A fresh handshake, not whatever is already open: a check that reused a
    // working client could not say the settings are right.
    hub().forget(id);
    let _ = hub().refresh(&vault, id).await;
    blocking(move || Ok(one_view(&vault, id)?)).await
}

/// Set, or with `None` clear, the person's choice for one tool.
async fn set_connection_tool_policy(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SetConnectionToolPolicy,
) -> CommandResult<ConnectionView> {
    let vault = svc.require_unlocked()?;
    if args.tool.trim().is_empty() {
        return Err(CommandError::new(codes::INVALID, "which tool?"));
    }
    blocking(move || {
        let mut connection = vault.connection(args.id)?;
        match args.policy {
            Some(policy) => {
                connection.policies.insert(args.tool, policy);
            }
            None => {
                connection.policies.remove(&args.tool);
            }
        }
        vault.save_connection(connection)?;
        Ok(one_view(&vault, args.id)?)
    })
    .await
}

/// Start an OAuth sign-in. The interface opens `url` itself (or shows it
/// to copy), then calls `await_connection_sign_in`.
async fn begin_connection_sign_in(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: ConnectionRef,
) -> CommandResult<BegunConnectionSignIn> {
    let vault = svc.require_unlocked()?;
    let begun = hub()
        .sign_ins()
        .begin(&vault, args.id)
        .await
        .map_err(|e| command_error(e, codes::PROVIDER))?;
    Ok(BegunConnectionSignIn { sign_in_id: begun.sign_in_id, url: begun.url })
}

/// Wait for a sign-in to land, then connect and list the tools. Fails with
/// `cancelled`, `timed_out`, `invalid_client` (a client ID of the person's
/// own is needed) or `provider`.
async fn await_connection_sign_in(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: ConnectionSignInRef,
) -> CommandResult<ConnectionView> {
    let vault = svc.require_unlocked()?;
    let id = hub()
        .sign_ins()
        .wait(&vault, &args.sign_in_id)
        .await
        .map_err(|e| command_error(e, codes::PROVIDER))?;
    // The sign-in worked whether or not the first listing does; the state
    // records how that went.
    let _ = hub().refresh(&vault, id).await;
    blocking(move || Ok(one_view(&vault, id)?)).await
}

async fn cancel_connection_sign_in(
    _svc: Arc<Service>,
    _ctx: Ctx,
    args: ConnectionSignInRef,
) -> CommandResult<()> {
    hub().sign_ins().cancel(&args.sign_in_id);
    Ok(())
}

/// Forget an OAuth grant, keeping the connection.
async fn sign_out_connection(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: ConnectionRef,
) -> CommandResult<ConnectionView> {
    let vault = svc.require_unlocked()?;
    let id = args.id;
    let writing = Arc::clone(&vault);
    blocking(move || {
        writing.connection(id)?;
        let mut secret = writing.connection_secret(id)?;
        secret.grant = None;
        writing.set_connection_secret(id, &secret)?;
        writing.update_connection_state(id, |state| {
            state.needs_sign_in = true;
            state.last_error = None;
        })?;
        Ok(())
    })
    .await?;
    hub().forget(id);
    blocking(move || Ok(one_view(&vault, id)?)).await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "connections", scope: All, effect: Read,
        args: Nothing, returns: "ConnectionsView", signature: &[],
        run: connections,
    },
    command! {
        name: "add_connection", scope: All, effect: Write,
        change: Settings / Updated,
        args: AddConnection, returns: "ConnectionView",
        signature: &[
            ("preset", "string", false),
            ("name", "string", true),
            ("transport", "ConnectionTransport", true),
            ("auth", "ConnectionAuth", true),
            ("secrets", "ConnectionSecretsPatch", false),
        ],
        run: add_connection,
    },
    command! {
        name: "update_connection", scope: All, effect: Write,
        change: Settings / Updated,
        args: UpdateConnection, returns: "ConnectionView",
        signature: &[("connection", "McpConnection", true)],
        run: update_connection,
    },
    command! {
        name: "remove_connection", scope: All, effect: Write,
        change: Settings / Updated,
        args: ConnectionRef, returns: "ConnectionsView",
        signature: &[("id", "ConnectionId", true)],
        run: remove_connection,
    },
    command! {
        name: "set_connection_secrets", scope: All, effect: Write,
        change: Settings / Updated,
        args: SetConnectionSecrets, returns: "ConnectionView",
        signature: &[
            ("id", "ConnectionId", true),
            ("bearer", "string", false),
            ("headers", "ConnectionValues", false),
            ("env", "ConnectionValues", false),
            ("clientSecret", "string", false),
        ],
        run: set_connection_secrets,
    },
    command! {
        name: "check_connection", scope: All, effect: Write,
        change: Settings / Updated,
        args: ConnectionRef, returns: "ConnectionView",
        signature: &[("id", "ConnectionId", true)],
        run: check_connection,
    },
    command! {
        name: "set_connection_tool_policy", scope: All, effect: Write,
        change: Settings / Updated,
        args: SetConnectionToolPolicy, returns: "ConnectionView",
        signature: &[
            ("id", "ConnectionId", true),
            ("tool", "string", true),
            ("policy", "ToolPolicy", false),
        ],
        run: set_connection_tool_policy,
    },
    command! {
        name: "begin_connection_sign_in", scope: All, effect: Write,
        args: ConnectionRef, returns: "BegunConnectionSignIn",
        signature: &[("id", "ConnectionId", true)],
        run: begin_connection_sign_in,
    },
    command! {
        name: "await_connection_sign_in", scope: All, effect: Write,
        change: Settings / Updated,
        args: ConnectionSignInRef, returns: "ConnectionView",
        signature: &[("signInId", "string", true)],
        run: await_connection_sign_in,
    },
    command! {
        name: "cancel_connection_sign_in", scope: All, effect: Write,
        args: ConnectionSignInRef, returns: "void",
        signature: &[("signInId", "string", true)],
        run: cancel_connection_sign_in,
    },
    command! {
        name: "sign_out_connection", scope: All, effect: Write,
        change: Settings / Updated,
        args: ConnectionRef, returns: "ConnectionView",
        signature: &[("id", "ConnectionId", true)],
        run: sign_out_connection,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::Caller;
    use karmayoga_core::connection::OAuthGrant;

    fn http(headers: &[&str], auth: AuthMethod) -> Connection {
        Connection::new(
            "Work",
            Transport::Http {
                url: "https://example.com/mcp".into(),
                headers: headers.iter().map(|h| h.to_string()).collect(),
            },
            auth,
        )
    }

    fn oauth() -> AuthMethod {
        AuthMethod::OAuth { client_id: None, scopes: Vec::new(), redirect_port: None }
    }

    fn grant() -> OAuthGrant {
        OAuthGrant {
            client_id: "c".into(),
            client_secret: None,
            token_endpoint: "https://example.com/token".into(),
            token_auth: karmayoga_core::connection::TokenAuth::None,
            resource: None,
            access_token: "a".into(),
            refresh_token: None,
            expires_at: None,
            scope: None,
        }
    }

    #[test]
    fn status_follows_what_is_missing_then_what_was_last_learned() {
        let secret = ConnectionSecret::default();
        let fresh = ConnectionState::default();

        let mut off = http(&[], AuthMethod::None);
        off.enabled = false;
        assert_eq!(status_of(&off, &fresh, &secret), ConnectionStatus::Off);

        let bearer = http(&[], AuthMethod::Bearer);
        assert_eq!(status_of(&bearer, &fresh, &secret), ConnectionStatus::NeedsSetup);
        // An attempt without a token said "sign in"; the missing token is
        // still what the person has to fix.
        let refused = ConnectionState { needs_sign_in: true, ..Default::default() };
        assert_eq!(status_of(&bearer, &refused, &secret), ConnectionStatus::NeedsSetup);
        let with_token = ConnectionSecret { bearer: Some("t".into()), ..Default::default() };
        assert_eq!(status_of(&bearer, &fresh, &with_token), ConnectionStatus::Unknown);
        assert_eq!(status_of(&bearer, &refused, &with_token), ConnectionStatus::NeedsSignIn);

        let signed = http(&[], oauth());
        assert_eq!(status_of(&signed, &fresh, &secret), ConnectionStatus::NeedsSignIn);
        let granted = ConnectionSecret { grant: Some(grant()), ..Default::default() };
        assert_eq!(status_of(&signed, &fresh, &granted), ConnectionStatus::Unknown);

        let headed = http(&["X-Key"], AuthMethod::None);
        assert_eq!(status_of(&headed, &fresh, &secret), ConnectionStatus::NeedsSetup);

        let plain = http(&[], AuthMethod::None);
        let failed = ConnectionState { last_error: Some("down".into()), ..Default::default() };
        assert_eq!(status_of(&plain, &failed, &secret), ConnectionStatus::Error);
        let worked =
            ConnectionState { last_ok: Some(jiff::Timestamp::now()), ..Default::default() };
        assert_eq!(status_of(&plain, &worked, &secret), ConnectionStatus::Ready);
    }

    #[test]
    fn a_patch_sets_removes_and_drops_names_the_connection_no_longer_lists() {
        let connection = http(&["X-Key", "X-Other"], AuthMethod::Bearer);
        let mut secret = ConnectionSecret {
            bearer: Some("old".into()),
            client_secret: Some("cs".into()),
            headers: BTreeMap::from([
                ("X-Key".to_string(), "k".to_string()),
                ("X-Gone".to_string(), "g".to_string()),
            ]),
            env: BTreeMap::from([("LEFTOVER".to_string(), "e".to_string())]),
            grant: None,
        };
        let patch = SecretsPatch {
            bearer: Some("  new \n".into()),
            headers: Some(BTreeMap::from([
                ("X-Other".to_string(), "o".to_string()),
                ("X-Key".to_string(), String::new()),
            ])),
            env: None,
            client_secret: None,
        };
        apply_patch(&mut secret, patch, &connection);
        assert_eq!(secret.bearer.as_deref(), Some("new"));
        assert_eq!(secret.client_secret.as_deref(), Some("cs"));
        assert_eq!(secret.headers, BTreeMap::from([("X-Other".to_string(), "o".to_string())]));
        assert!(secret.env.is_empty());

        let clear = SecretsPatch { bearer: Some(String::new()), ..Default::default() };
        apply_patch(&mut secret, clear, &connection);
        assert_eq!(secret.bearer, None);
    }

    #[test]
    fn hub_errors_become_the_commands_codes() {
        assert_eq!(command_error(HubError::new("x"), codes::PROVIDER).code, codes::PROVIDER);
        assert_eq!(command_error(HubError::sign_in("x"), codes::PROVIDER).code, codes::FORBIDDEN);
        assert_eq!(
            command_error(HubError::coded(codes::CANCELLED, "x"), codes::PROVIDER).code,
            codes::CANCELLED
        );
    }

    #[test]
    fn only_this_computers_own_windows_may_run_programs() {
        for (caller, local) in [
            (Caller::Local, true),
            (Caller::Window("window:app-notes".into()), true),
            (Caller::Socket, false),
            (Caller::Device("phone".into()), false),
            (Caller::Assistant("run-1".into()), false),
        ] {
            let ctx = Ctx { caller: caller.clone(), ..Ctx::local() };
            assert_eq!(ctx.is_local(), local, "{caller:?}");
            assert_eq!(require_local(&ctx).is_ok(), local, "{caller:?}");
        }
        let stdio = Transport::Stdio { command: "x".into(), args: vec![], env: vec![], cwd: None };
        assert!(runs_a_program(&stdio));
        assert!(!runs_a_program(&Transport::Http {
            url: "https://x.test".into(),
            headers: vec![]
        }));
    }
}
