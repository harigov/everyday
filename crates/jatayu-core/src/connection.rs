//! Connections: MCP servers the assistant may use, and what is known about
//! each. See `docs/plans/connections.md`.
//!
//! A connection is one signed-in server. Slack, Linear and Chrome differ only
//! in what is written here -- a URL or a command, how it signs in, and the
//! tools it lists -- so nothing in the tree knows any of them by name except
//! the preset catalogue at the bottom, which only fills in a form.
//!
//! Three things are kept, by three different hands:
//!
//! - [`ConnectionSettings`], the person's list. Written only by Settings.
//! - [`ConnectionStates`], what the runtime learned: the server's name, its
//!   tools, the last error. Written only by the runtime, apart from the
//!   settings so a refresh cannot undo a rename.
//! - [`ConnectionSecret`], one sealed secret per connection: a token, header
//!   and environment values, an OAuth grant. Never handed to the interface.
//!
//! Synchronous and offline, like the rest of the core: the client that talks
//! to a server is `jatayu-connect`, and the runtime that holds one open is
//! `jatayu_service::connections`.

use std::collections::BTreeMap;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::id::ConnectionId;

/// The `owner_kind` a connection's secret is sealed under in the
/// [`SecretStore`](crate::store::secrets::SecretStore).
pub const CONNECTION_SECRET_KIND: &str = "connection";

/// The most connections one vault keeps. Generous; it stops a runaway
/// client, not a person.
pub const MAX_CONNECTIONS: usize = 64;

/// How long a cached tool list is trusted before the assistant's search
/// lists the server again.
pub const TOOLS_FRESH_SECS: i64 = 60 * 60;

/// Associated data the settings half of the connections row is sealed
/// under.
pub fn settings_aad() -> Vec<u8> {
    b"karmayoga.connection-settings.v1".to_vec()
}

/// Associated data the state half is sealed under.
pub fn states_aad() -> Vec<u8> {
    b"karmayoga.connection-states.v1".to_vec()
}

// ── The person's list ────────────────────────────────────────────────────

/// Every connection, in the order the panel shows them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionSettings {
    #[serde(default)]
    pub connections: Vec<Connection>,
}

impl ConnectionSettings {
    pub fn get(&self, id: ConnectionId) -> Option<&Connection> {
        self.connections.iter().find(|c| c.id == id)
    }

    /// The connection a model named: its id, or its name ignoring case and
    /// surrounding space. A name two connections share answers neither.
    pub fn find(&self, named: &str) -> Option<&Connection> {
        let named = named.trim();
        if let Ok(id) = named.parse::<ConnectionId>()
            && let Some(c) = self.get(id)
        {
            return Some(c);
        }
        let mut matching =
            self.connections.iter().filter(|c| c.name.trim().eq_ignore_ascii_case(named));
        let first = matching.next()?;
        matching.next().is_none().then_some(first)
    }
}

/// One MCP server, signed in as one account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub id: ConnectionId,
    /// What the person calls it -- "Work Slack" -- and what the model names
    /// it by. Unique within the vault, ignoring case.
    pub name: String,
    /// The [`Preset`] it was made from, if any: `"slack"`, `"linear"`. Only
    /// for the panel's icon and help; nothing behaves differently because
    /// of it.
    #[serde(default)]
    pub preset: Option<String>,
    pub transport: Transport,
    pub auth: AuthMethod,
    /// Switched off: kept, signed in, offered to nobody.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Whether routines and background jobs may use it. Off by default:
    /// with nobody watching, a run reads whatever a stranger wrote and acts
    /// on it as the person.
    #[serde(default)]
    pub unattended: bool,
    /// The person's choice per tool, by the tool's name. A tool not named
    /// here follows [`ToolPolicy::default_for`].
    #[serde(default)]
    pub policies: BTreeMap<String, ToolPolicy>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

fn yes() -> bool {
    true
}

impl Connection {
    pub fn new(name: impl Into<String>, transport: Transport, auth: AuthMethod) -> Self {
        let now = Timestamp::now();
        Self {
            id: ConnectionId::new(),
            name: name.into(),
            preset: None,
            transport,
            auth,
            enabled: true,
            unattended: false,
            policies: BTreeMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// What happens when the assistant calls `tool`: the person's choice, or
    /// the tool's own hints.
    pub fn policy_for(&self, tool: &RemoteTool) -> ToolPolicy {
        self.policies.get(&tool.name).copied().unwrap_or_else(|| ToolPolicy::default_for(tool))
    }

    /// Who the connection's credentials and trust belong to: the URL's
    /// origin (scheme, host and port), or the program and its arguments. A
    /// token and a person's choices about tools were given to *that*; moving
    /// the connection anywhere else must not carry them along.
    pub fn owner(&self) -> String {
        match &self.transport {
            Transport::Http { url, .. } => match url::Url::parse(url.trim()) {
                Ok(parsed) => format!("http {}", parsed.origin().ascii_serialization()),
                Err(_) => format!("http {}", url.trim()),
            },
            Transport::Stdio { command, args, .. } => {
                format!("stdio {} {}", command.trim(), args.join(" "))
            }
        }
    }

    /// Tidy what a form sends: names and addresses trimmed, empty entries
    /// dropped, duplicate secret names removed.
    pub fn normalized(mut self) -> Self {
        self.name = self.name.trim().to_string();
        self.preset = self.preset.map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
        self.transport = match self.transport {
            Transport::Http { url, headers } => {
                Transport::Http { url: url.trim().to_string(), headers: tidy_names(headers) }
            }
            Transport::Stdio { command, args, env, cwd } => Transport::Stdio {
                command: command.trim().to_string(),
                args: args.into_iter().filter(|a| !a.is_empty()).collect(),
                env: tidy_names(env),
                cwd: cwd.map(|c| c.trim().to_string()).filter(|c| !c.is_empty()),
            },
        };
        if let AuthMethod::OAuth { client_id, scopes, redirect_port } = self.auth {
            self.auth = AuthMethod::OAuth {
                client_id: client_id.map(|c| c.trim().to_string()).filter(|c| !c.is_empty()),
                scopes: scopes
                    .into_iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
                redirect_port,
            };
        }
        self
    }

    /// Whether this can be saved as it stands.
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(Error::Invalid("a connection needs a name".into()));
        }
        if self.name.chars().count() > 80 {
            return Err(Error::Invalid("a connection's name is at most 80 characters".into()));
        }
        match &self.transport {
            Transport::Http { url, headers } => {
                let parsed = url::Url::parse(url.trim()).map_err(|_| {
                    Error::Invalid(format!("\u{201c}{url}\u{201d} is not a web address"))
                })?;
                if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                    return Err(Error::Invalid(
                        "the address must start with https:// or http://".into(),
                    ));
                }
                // Plain HTTP only to this machine: a token sent in the clear
                // anywhere else is a token given away.
                if parsed.scheme() == "http" && !is_loopback_host(parsed.host_str().unwrap_or("")) {
                    return Err(Error::Invalid(
                        "an http:// address is only allowed for this computer (127.0.0.1 or \
                         localhost); use https://"
                            .into(),
                    ));
                }
                if headers.iter().any(|h| !valid_header_name(h)) {
                    return Err(Error::Invalid(
                        "a header name may use letters, digits and dashes".into(),
                    ));
                }
                if matches!(self.auth, AuthMethod::OAuth { .. } | AuthMethod::Bearer)
                    && headers.iter().any(|h| h.eq_ignore_ascii_case("authorization"))
                {
                    return Err(Error::Invalid(
                        "the Authorization header is set by the sign-in; remove it from the headers"
                            .into(),
                    ));
                }
            }
            Transport::Stdio { command, env, .. } => {
                if command.trim().is_empty() {
                    return Err(Error::Invalid("a command to run is needed".into()));
                }
                if matches!(self.auth, AuthMethod::OAuth { .. } | AuthMethod::Bearer) {
                    return Err(Error::Invalid(
                        "a command signs in through its environment variables, not OAuth or a \
                         bearer token"
                            .into(),
                    ));
                }
                if env.iter().any(|e| !valid_env_name(e)) {
                    return Err(Error::Invalid(
                        "an environment variable's name may use letters, digits and underscores"
                            .into(),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// How the client reaches the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Transport {
    /// Streamable HTTP. `headers` names the headers sent with every
    /// request; their values are sealed in [`ConnectionSecret::headers`].
    #[serde(rename_all = "camelCase")]
    Http {
        url: String,
        #[serde(default)]
        headers: Vec<String>,
    },
    /// A program on this computer, spoken to over its standard streams.
    /// `env` names the variables it is given; their values are sealed in
    /// [`ConnectionSecret::env`].
    #[serde(rename_all = "camelCase")]
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
    },
}

/// How a connection signs in, beyond any headers or environment it names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AuthMethod {
    /// Nothing, or only headers / environment values.
    None,
    /// A token pasted in, sent as `Authorization: Bearer`. Sealed in
    /// [`ConnectionSecret::bearer`].
    Bearer,
    /// The MCP authorization flow. `client_id` is the person's own client
    /// for a service that does not register clients itself (its secret, if
    /// any, is [`ConnectionSecret::client_secret`]); `None` registers one at
    /// every sign-in. `scopes` empty asks for what the server suggests.
    /// `redirect_port` fixes the loopback's port for a provider that matches
    /// redirect addresses exactly.
    #[serde(rename = "oAuth", rename_all = "camelCase")]
    OAuth {
        #[serde(default)]
        client_id: Option<String>,
        #[serde(default)]
        scopes: Vec<String>,
        #[serde(default)]
        redirect_port: Option<u16>,
    },
}

/// What happens when the assistant calls a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolPolicy {
    /// Run it without asking.
    Allow,
    /// Stop and ask the person first, showing the arguments.
    Ask,
    /// Never: refused, and left out of the assistant's search.
    Off,
}

impl ToolPolicy {
    /// What a tool nobody has chosen for gets: run a tool that says it only
    /// reads, ask about everything else. The hints are the server's claim,
    /// which is why the person can change any of them.
    pub fn default_for(tool: &RemoteTool) -> ToolPolicy {
        let reads = tool.hints.read_only == Some(true) && tool.hints.destructive != Some(true);
        if reads { ToolPolicy::Allow } else { ToolPolicy::Ask }
    }
}

// ── What the runtime learned ─────────────────────────────────────────────

/// What the runtime knows about every connection, by id.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionStates {
    #[serde(default)]
    pub states: BTreeMap<ConnectionId, ConnectionState>,
}

impl ConnectionStates {
    /// The state for `id`, or an empty one: never connected.
    pub fn of(&self, id: ConnectionId) -> ConnectionState {
        self.states.get(&id).cloned().unwrap_or_default()
    }
}

/// What the last connection to a server found.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionState {
    #[serde(default)]
    pub server: Option<ServerInfo>,
    /// The tools it listed, in its order.
    #[serde(default)]
    pub tools: Vec<RemoteTool>,
    /// When `tools` was read.
    #[serde(default)]
    pub tools_at: Option<Timestamp>,
    #[serde(default)]
    pub last_ok: Option<Timestamp>,
    /// Why the last attempt failed, in words for a person, with nothing
    /// secret in it. `None` after a success.
    #[serde(default)]
    pub last_error: Option<String>,
    /// The server refused the credentials, or never had any: a sign-in (or a
    /// new token) is needed before anything else.
    #[serde(default)]
    pub needs_sign_in: bool,
}

impl ConnectionState {
    /// Whether `tools` is recent enough to search without listing again.
    pub fn tools_fresh(&self, now: Timestamp) -> bool {
        self.tools_at.is_some_and(|at| now.as_second() - at.as_second() < TOOLS_FRESH_SECS)
    }
}

/// Who answered, as the server named itself.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub name: String,
    #[serde(default)]
    pub version: String,
    /// The protocol revision agreed on.
    #[serde(default)]
    pub protocol_version: String,
    /// The server's own guidance for a model, if it gave any.
    #[serde(default)]
    pub instructions: Option<String>,
}

/// One tool a server lists.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTool {
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: String,
    /// JSON Schema for its arguments, as the server sent it.
    #[serde(default = "empty_object_schema")]
    pub input_schema: Value,
    #[serde(default)]
    pub hints: ToolHints,
}

fn empty_object_schema() -> Value {
    serde_json::json!({ "type": "object" })
}

/// The server's own annotations on a tool. Claims, not guarantees.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolHints {
    #[serde(default)]
    pub read_only: Option<bool>,
    #[serde(default)]
    pub destructive: Option<bool>,
    #[serde(default)]
    pub idempotent: Option<bool>,
    #[serde(default)]
    pub open_world: Option<bool>,
}

// ── The sealed half ──────────────────────────────────────────────────────

/// Everything secret about one connection, sealed as one value under
/// `(CONNECTION_SECRET_KIND, id)`. `Debug` prints names, never values.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionSecret {
    /// [`AuthMethod::Bearer`]'s token.
    #[serde(default)]
    pub bearer: Option<String>,
    /// Header values, by the names in [`Transport::Http::headers`].
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Environment values, by the names in [`Transport::Stdio::env`].
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// The person's own OAuth client's secret, for [`AuthMethod::OAuth`]
    /// with a `client_id`.
    #[serde(default)]
    pub client_secret: Option<String>,
    /// What an OAuth sign-in left: enough to send and refresh a token.
    #[serde(default)]
    pub grant: Option<OAuthGrant>,
}

impl std::fmt::Debug for ConnectionSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionSecret")
            .field("bearer", &self.bearer.as_ref().map(|_| "[redacted]"))
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .field("env", &self.env.keys().collect::<Vec<_>>())
            .field("client_secret", &self.client_secret.as_ref().map(|_| "[redacted]"))
            .field("grant", &self.grant)
            .finish()
    }
}

/// An OAuth sign-in's result, with what refreshing it needs.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthGrant {
    /// The client the token was issued to: the person's own, or the one
    /// registered at sign-in.
    pub client_id: String,
    /// A registered client's secret, if the server issued one.
    #[serde(default)]
    pub client_secret: Option<String>,
    pub token_endpoint: String,
    /// How the client authenticates at `token_endpoint`.
    #[serde(default)]
    pub token_auth: TokenAuth,
    /// The `resource` the token was asked for (RFC 8707), sent again on
    /// refresh.
    #[serde(default)]
    pub resource: Option<String>,
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_at: Option<Timestamp>,
    #[serde(default)]
    pub scope: Option<String>,
}

impl std::fmt::Debug for OAuthGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthGrant")
            .field("client_id", &self.client_id)
            .field("client_secret", &self.client_secret.as_ref().map(|_| "[redacted]"))
            .field("token_endpoint", &self.token_endpoint)
            .field("token_auth", &self.token_auth)
            .field("resource", &self.resource)
            .field("access_token", &"[redacted]")
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "[redacted]"))
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .finish()
    }
}

/// How a client proves itself at a token endpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TokenAuth {
    /// A public client: `client_id` in the body, nothing else.
    #[default]
    None,
    /// `client_id` and `client_secret` in the body.
    ClientSecretPost,
    /// HTTP Basic with the id and secret.
    ClientSecretBasic,
}

/// Which secrets are set, for the panel: names and yes/no, never values.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretsSet {
    pub bearer: bool,
    pub headers: Vec<String>,
    pub env: Vec<String>,
    pub client_secret: bool,
    /// An OAuth grant is stored.
    pub signed_in: bool,
    /// When the stored access token runs out, if it said.
    pub expires_at: Option<Timestamp>,
}

impl ConnectionSecret {
    pub fn summary(&self) -> SecretsSet {
        SecretsSet {
            bearer: self.bearer.as_ref().is_some_and(|b| !b.is_empty()),
            headers: self.headers.keys().cloned().collect(),
            env: self.env.keys().cloned().collect(),
            client_secret: self.client_secret.as_ref().is_some_and(|s| !s.is_empty()),
            signed_in: self.grant.is_some(),
            expires_at: self.grant.as_ref().and_then(|g| g.expires_at),
        }
    }
}

// ── Tool search ──────────────────────────────────────────────────────────

/// One tool the assistant's search found.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolMatch {
    pub connection: String,
    pub connection_id: ConnectionId,
    pub tool: RemoteTool,
    pub policy: ToolPolicy,
}

/// Rank every tool of `connections` against `query`: each word of the query
/// found in the tool's name scores 3, in its title 2, in its description 1;
/// a word found nowhere scores 0, and a tool scoring 0 overall is left out
/// unless the query is empty, when every tool is listed in order. Tools
/// switched `Off` are never listed. Ties keep the connections' and the
/// server's order. At most `limit`.
pub fn search_tools(
    connections: &[(&Connection, &ConnectionState)],
    query: &str,
    limit: usize,
) -> Vec<ToolMatch> {
    let words: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1)
        .map(str::to_lowercase)
        .collect();
    let mut scored: Vec<(u32, usize, ToolMatch)> = Vec::new();
    let mut order = 0usize;
    for (connection, state) in connections {
        for tool in &state.tools {
            order += 1;
            let policy = connection.policy_for(tool);
            if policy == ToolPolicy::Off {
                continue;
            }
            let name = tool.name.to_lowercase().replace(['_', '-', '.'], " ");
            let title = tool.title.as_deref().unwrap_or("").to_lowercase();
            let description = tool.description.to_lowercase();
            let score: u32 = words
                .iter()
                .map(|w| {
                    let mut s = 0;
                    if name.contains(w.as_str()) {
                        s += 3;
                    }
                    if title.contains(w.as_str()) {
                        s += 2;
                    }
                    if description.contains(w.as_str()) {
                        s += 1;
                    }
                    s
                })
                .sum();
            if score == 0 && !words.is_empty() {
                continue;
            }
            scored.push((
                score,
                order,
                ToolMatch {
                    connection: connection.name.clone(),
                    connection_id: connection.id,
                    tool: tool.clone(),
                    policy,
                },
            ));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().take(limit).map(|(_, _, m)| m).collect()
}

// ── Presets ──────────────────────────────────────────────────────────────

/// A filled-in form for a service people commonly connect. Everything in it
/// stays editable.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preset {
    pub key: String,
    pub name: String,
    pub description: String,
    pub transport: Transport,
    pub auth: AuthMethod,
    /// What the person has to supply before it works.
    pub needs: Vec<PresetNeed>,
    /// A sentence or two on setting it up, shown above the form.
    pub help: String,
    pub help_url: Option<String>,
}

/// One thing a preset needs from the person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetNeed {
    pub kind: NeedKind,
    /// The header or variable name, for `Header` and `Env`.
    pub name: Option<String>,
    pub label: String,
    pub help: String,
    pub optional: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NeedKind {
    /// [`AuthMethod::OAuth::client_id`].
    ClientId,
    /// [`ConnectionSecret::client_secret`].
    ClientSecret,
    /// [`ConnectionSecret::bearer`].
    Token,
    /// A value in [`ConnectionSecret::headers`].
    Header,
    /// A value in [`ConnectionSecret::env`].
    Env,
}

fn need(kind: NeedKind, label: &str, help: &str) -> PresetNeed {
    PresetNeed { kind, name: None, label: label.into(), help: help.into(), optional: false }
}

fn http(url: &str) -> Transport {
    Transport::Http { url: url.into(), headers: Vec::new() }
}

fn oauth(scopes: &[&str], redirect_port: Option<u16>) -> AuthMethod {
    AuthMethod::OAuth {
        client_id: None,
        scopes: scopes.iter().map(|s| s.to_string()).collect(),
        redirect_port,
    }
}

/// The loopback port the Slack preset fixes, so the redirect address can be
/// registered on the person's Slack app exactly.
pub const SLACK_REDIRECT_PORT: u16 = 33418;

/// Every preset, in the order the panel offers them.
pub fn presets() -> Vec<Preset> {
    vec![
        Preset {
            key: "slack".into(),
            name: "Slack".into(),
            description: "Search and read channels, threads and DMs; send messages.".into(),
            transport: http("https://mcp.slack.com/mcp"),
            auth: oauth(&[], Some(SLACK_REDIRECT_PORT)),
            needs: vec![
                need(
                    NeedKind::ClientId,
                    "Client ID",
                    "From your Slack app's Basic Information page.",
                ),
                need(NeedKind::ClientSecret, "Client secret", "From the same page."),
            ],
            help: format!(
                "Slack's server only signs in through an app of your own. Create one at \
                 api.slack.com/apps, add http://127.0.0.1:{SLACK_REDIRECT_PORT}/callback as a \
                 redirect URL under OAuth & Permissions, and paste its client ID and secret here."
            ),
            help_url: Some("https://docs.slack.dev/ai/slack-mcp-server/".into()),
        },
        Preset {
            key: "google-drive".into(),
            name: "Google Drive".into(),
            description: "Find, read and organise files in Drive.".into(),
            transport: http("https://drivemcp.googleapis.com/mcp/v1"),
            auth: oauth(&["https://www.googleapis.com/auth/drive"], None),
            needs: vec![
                need(NeedKind::ClientId, "Client ID", "A Google OAuth client of type Desktop app."),
                need(NeedKind::ClientSecret, "Client secret", "From the same client."),
            ],
            help: "Google's Drive server needs an OAuth client of your own. In a Google Cloud \
                   project, enable the Drive API and the Drive MCP API \
                   (drivemcp.googleapis.com), create an OAuth client of type Desktop app, and \
                   paste its ID and secret here."
                .into(),
            help_url: Some(
                "https://developers.google.com/workspace/drive/api/guides/configure-mcp-server"
                    .into(),
            ),
        },
        Preset {
            key: "linear".into(),
            name: "Linear".into(),
            description: "Find, create and update issues, projects and comments.".into(),
            transport: http("https://mcp.linear.app/mcp"),
            auth: oauth(&[], None),
            needs: Vec::new(),
            help: "Signs in with your Linear account in the browser. Add it once per workspace."
                .into(),
            help_url: Some("https://linear.app/docs/mcp".into()),
        },
        Preset {
            key: "github".into(),
            name: "GitHub".into(),
            description: "Repositories, issues, pull requests, code search and Actions.".into(),
            transport: http("https://api.githubcopilot.com/mcp/"),
            auth: AuthMethod::Bearer,
            needs: vec![need(
                NeedKind::Token,
                "Personal access token",
                "A fine-grained token with access to the repositories you want it to see.",
            )],
            help: "GitHub's server takes a personal access token. Create one under Settings → \
                   Developer settings → Personal access tokens, with the repositories and \
                   permissions you want the assistant to have."
                .into(),
            help_url: Some("https://github.com/settings/personal-access-tokens/new".into()),
        },
        Preset {
            key: "paseo".into(),
            name: "Paseo".into(),
            description: "Start, follow and message your coding agents.".into(),
            transport: http("http://127.0.0.1:6767/mcp/agents"),
            auth: AuthMethod::None,
            needs: Vec::new(),
            help: "Talks to the Paseo daemon on this computer. Paseo has to be running.".into(),
            help_url: Some("https://paseo.sh".into()),
        },
        Preset {
            key: "chrome".into(),
            name: "Google Chrome".into(),
            description: "Open pages, click, fill in forms and read the console in Chrome.".into(),
            transport: Transport::Stdio {
                command: "npx".into(),
                args: vec!["-y".into(), "chrome-devtools-mcp@latest".into()],
                env: Vec::new(),
                cwd: None,
            },
            auth: AuthMethod::None,
            needs: Vec::new(),
            help: "Runs Chrome DevTools' MCP server with Node.js, which has to be installed. It \
                   opens a Chrome window of its own; add --isolated to the arguments for a \
                   throwaway profile, or --headless for no window."
                .into(),
            help_url: Some("https://github.com/ChromeDevTools/chrome-devtools-mcp".into()),
        },
        Preset {
            key: "datadog".into(),
            name: "Datadog".into(),
            description: "Logs, metrics, traces, monitors, incidents and dashboards.".into(),
            transport: http("https://mcp.datadoghq.com/v1/mcp"),
            auth: oauth(&[], None),
            needs: Vec::new(),
            help: "Signs in with your Datadog account in the browser. For a site other than US1, \
                   change the address's host (mcp.datadoghq.eu, mcp.us3.datadoghq.com, …); add \
                   ?toolsets=… to choose which tools it offers."
                .into(),
            help_url: Some("https://docs.datadoghq.com/bits_ai/mcp_server/setup/".into()),
        },
        Preset {
            key: "custom-http".into(),
            name: "Other server (web address)".into(),
            description: "Any MCP server reached over HTTPS.".into(),
            transport: http(""),
            auth: oauth(&[], None),
            needs: Vec::new(),
            help:
                "Paste the server's MCP address. Most sign in with OAuth in the browser; for one \
                   that takes a token or an API key, choose Token or Headers instead."
                    .into(),
            help_url: None,
        },
        Preset {
            key: "custom-stdio".into(),
            name: "Other server (command)".into(),
            description: "Any MCP server run as a program on this computer.".into(),
            transport: Transport::Stdio {
                command: String::new(),
                args: Vec::new(),
                env: Vec::new(),
                cwd: None,
            },
            auth: AuthMethod::None,
            needs: Vec::new(),
            help: "The command and its arguments, exactly as the server's instructions give them. \
                   API keys go in environment variables, which are kept sealed."
                .into(),
            help_url: None,
        },
    ]
}

/// The preset called `key`.
pub fn preset(key: &str) -> Option<Preset> {
    presets().into_iter().find(|p| p.key == key)
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn tidy_names(names: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in names {
        let name = name.trim().to_string();
        if !name.is_empty() && !out.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
            out.push(name);
        }
    }
    out
}

fn valid_header_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `localhost`, `127.x.x.x` or `::1`.
pub fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str, description: &str, read_only: Option<bool>) -> RemoteTool {
        RemoteTool {
            name: name.into(),
            title: None,
            description: description.into(),
            input_schema: json!({ "type": "object" }),
            hints: ToolHints { read_only, ..Default::default() },
        }
    }

    #[test]
    fn a_tool_that_says_it_only_reads_runs_and_everything_else_asks() {
        assert_eq!(ToolPolicy::default_for(&tool("search", "", Some(true))), ToolPolicy::Allow);
        assert_eq!(ToolPolicy::default_for(&tool("post", "", Some(false))), ToolPolicy::Ask);
        assert_eq!(ToolPolicy::default_for(&tool("post", "", None)), ToolPolicy::Ask);
        let mut both = tool("wipe", "", Some(true));
        both.hints.destructive = Some(true);
        assert_eq!(ToolPolicy::default_for(&both), ToolPolicy::Ask);
    }

    #[test]
    fn a_connection_is_found_by_name_or_id_but_never_by_a_shared_name() {
        let a = Connection::new("Work Slack", http("https://mcp.slack.com/mcp"), AuthMethod::None);
        let b = Connection::new("Linear", http("https://mcp.linear.app/mcp"), AuthMethod::None);
        let settings = ConnectionSettings { connections: vec![a.clone(), b.clone()] };
        assert_eq!(settings.find(" work slack ").map(|c| c.id), Some(a.id));
        assert_eq!(settings.find(&b.id.to_string()).map(|c| c.id), Some(b.id));
        let twin = Connection::new("linear", http("https://mcp.linear.app/mcp"), AuthMethod::None);
        let settings = ConnectionSettings { connections: vec![b, twin] };
        assert!(settings.find("Linear").is_none());
    }

    #[test]
    fn search_ranks_names_over_descriptions_and_leaves_out_what_is_off() {
        let mut c = Connection::new("Linear", http("https://mcp.linear.app/mcp"), AuthMethod::None);
        let state = ConnectionState {
            tools: vec![
                tool("list_projects", "Lists projects; issues not included", Some(true)),
                tool("create_issue", "Create a new issue", None),
                tool("delete_issue", "Delete an issue", None),
            ],
            ..Default::default()
        };
        c.policies.insert("delete_issue".into(), ToolPolicy::Off);
        let found = search_tools(&[(&c, &state)], "create issue", 10);
        let names: Vec<_> = found.iter().map(|m| m.tool.name.as_str()).collect();
        assert_eq!(names, ["create_issue", "list_projects"]);
        assert_eq!(found[0].policy, ToolPolicy::Ask);
        assert_eq!(search_tools(&[(&c, &state)], "", 10).len(), 2);
    }

    #[test]
    fn plain_http_is_allowed_only_to_this_computer() {
        let ok =
            Connection::new("Paseo", http("http://127.0.0.1:6767/mcp/agents"), AuthMethod::None);
        assert!(ok.validate().is_ok());
        let bad = Connection::new("X", http("http://example.com/mcp"), AuthMethod::None);
        assert!(bad.validate().is_err());
    }

    #[test]
    fn a_command_cannot_sign_in_with_oauth() {
        let c = Connection::new(
            "Chrome",
            Transport::Stdio { command: "npx".into(), args: vec![], env: vec![], cwd: None },
            oauth(&[], None),
        );
        assert!(c.validate().is_err());
    }

    #[test]
    fn every_preset_but_the_blank_ones_is_valid_as_it_stands() {
        for preset in presets() {
            let c = Connection::new(&preset.name, preset.transport.clone(), preset.auth.clone());
            let blank = preset.key.starts_with("custom-");
            assert_eq!(c.validate().is_ok(), !blank, "{}", preset.key);
        }
    }

    #[test]
    fn a_secret_never_prints_its_values() {
        let secret = ConnectionSecret {
            bearer: Some("ghp_supersecret".into()),
            headers: [("DD-API-KEY".to_string(), "abc123".to_string())].into(),
            grant: Some(OAuthGrant {
                client_id: "client".into(),
                client_secret: Some("shh".into()),
                token_endpoint: "https://t".into(),
                token_auth: TokenAuth::None,
                resource: None,
                access_token: "tok".into(),
                refresh_token: Some("ref".into()),
                expires_at: None,
                scope: None,
            }),
            ..Default::default()
        };
        let printed = format!("{secret:?}");
        for value in ["ghp_supersecret", "abc123", "shh", "\"tok\"", "\"ref\""] {
            assert!(!printed.contains(value), "{printed}");
        }
        assert!(printed.contains("DD-API-KEY"));
    }

    #[test]
    fn wire_names_are_what_the_interface_reads() {
        let auth = serde_json::to_value(oauth(&["read"], Some(1))).unwrap();
        assert_eq!(
            auth,
            json!({ "type": "oAuth", "clientId": null, "scopes": ["read"], "redirectPort": 1 })
        );
        let t = serde_json::to_value(http("https://x")).unwrap();
        assert_eq!(t, json!({ "type": "http", "url": "https://x", "headers": [] }));
        assert_eq!(serde_json::to_value(ToolPolicy::Allow).unwrap(), json!("allow"));
    }
}
