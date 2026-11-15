# Connections: the assistant reaching other services through MCP

The goal is to let the assistant do things this application does not do
itself: search Slack, file a Linear issue, read a GitHub pull request,
look through Google Drive, drive a Chrome window, query Datadog, hand a
task to a Paseo agent. None of that is built here. Each of those services
already publishes a Model Context Protocol server, so this application
becomes an MCP **client**. A **connection** is one signed-in MCP server.
There can be as many connections as there are accounts, and two
connections can point at the same server: "Work Slack" and "Club Slack"
are two connections to `https://mcp.slack.com/mcp`, each with its own
token.

There is no Slack code, no Linear code and no GitHub code anywhere in
the tree. The services differ in three ways only, and all three are data:

- **Transport.** Either a URL (Streamable HTTP) or a command to run
  (stdio).
- **Sign-in.** One of:
  - nothing at all;
  - a token or headers pasted in;
  - OAuth, with the client either registered on the spot or brought by
    the person.
- **Tools.** Whatever the server lists, which is read at run time.

## Pieces

| Piece | Where | What |
| --- | --- | --- |
| Records and presets | `karmayoga-core/src/connection.rs` | `Connection`, `ConnectionState`, `ConnectionSecret`, `RemoteTool`, `ToolPolicy`, the preset catalogue, tool search. Synchronous, no sockets. |
| Storage | `karmayoga-store-sql/src/connections.rs`, schema v31 | One sealed singleton row: `settings` (the person's list) and `state` (what the runtime learned), kept apart as backups' two halves are. Credentials are `record_secrets` rows, owner kind `connection`, one per connection. |
| Vault facade | `karmayoga-core/src/vault/connections.rs` | Read, save and validate connections; seal and open each one's secret. |
| MCP client | new crate `karmayoga-connect` | JSON-RPC over Streamable HTTP and stdio, both protocol eras, `tools/list`, `tools/call`, and OAuth (discovery, dynamic registration, PKCE, refresh). No vault, no core. |
| Runtime | `karmayoga-service/src/connections/` | `Hub`: one live client per connection, opened lazily and kept warm, with tokens refreshed, tool lists cached, and status written back. `signin.rs` runs the OAuth sign-in. `tools.rs` holds the assistant's two tools and the gate's judgement. |
| Commands | `karmayoga-service/src/domains/connections.rs` | Settings → Connections. Scope `All`: a stdio connection runs a program, and nothing narrower may add one. |
| Interface | `ui/src/components/ConnectionsPanel.svelte`, `ui/src/lib/connections.svelte.ts` | The list, adding from a preset, signing in, the tools with a policy each, and removing. |

## Protocol

The client follows `docs/plans/mcp-protocol-notes.md`, from the other
side of the wire.

- **HTTP.** POST JSON-RPC with `Accept: application/json,
  text/event-stream`. A reply comes back either as JSON or as an SSE
  stream; the client reads events until the response with its `id`
  arrives. It keeps the `Mcp-Session-Id` the server mints and sends
  `MCP-Protocol-Version` once that version is negotiated.
  - A 401 is `Unauthorized`, carrying the `resource_metadata` from
    `WWW-Authenticate`.
  - A 404 on a request that carried a session id means the session
    expired: initialize once more and retry.
  - The older HTTP+SSE transport (a GET `/sse` endpoint) is not
    supported. Every preset speaks Streamable HTTP.
- **stdio.** Spawn the command with newline-delimited JSON-RPC on
  stdin/stdout.
  - Keep the last 4 KB of stderr, to explain a failure.
  - A command given without a path is looked up on `PATH`, and then in
    the places a desktop session's `PATH` often lacks:
    `~/.nvm/versions/node/*/bin` (newest first), `~/.local/bin`,
    `~/.cargo/bin`, `~/.bun/bin`, `~/.deno/bin`, `/usr/local/bin`,
    `/opt/homebrew/bin`. The found directory is put at the front of the
    child's `PATH` (npx needs node beside it).
  - The process is killed when the client is dropped.
  - Requests the server makes:
    - `ping` is answered `{}`;
    - `roots/list` is answered `{"roots": []}`;
    - anything else is answered `-32601`.
- **Eras.** The client opens with legacy `initialize`, at
  `protocolVersion` `2025-11-25`, and then sends
  `notifications/initialized`. Every server in the catalogue speaks the
  legacy era today. If the server refuses with `-32022` and lists
  `2026-07-28` or later among its supported versions, the client switches
  to the modern era. It sends `server/discover`, then stateless requests,
  each carrying `_meta` with the protocol version, client info and client
  capabilities, plus the `Mcp-Method` header and, for `tools/call`, the
  `Mcp-Name` header.
- **Tools.** `tools/list` is followed through every `nextCursor`. A
  `notifications/tools/list_changed` marks the cache stale.
- **Results.** The content blocks are rendered for the model:
  - text as it is;
  - an embedded text resource as its text;
  - a resource link as its title and URI;
  - an image or audio block as `[image: image/png, 34 KB]`.

  `structuredContent` is used only when there is no text. The result is
  cut at 40,000 characters, with a note saying so. `isError: true` is a
  failed call whose message goes back to the model.
- **Retries.** Listing tools reconnects and tries once more after a
  dropped connection. A `tools/call` is never sent twice: a drop or a 502
  may come after the server acted. Only a refused token is mended (with a
  refresh) and retried, and an expired session is renewed inside the client
  before the call goes out.
- **Moving a connection.** Pointing a connection at another origin, or
  another program, drops its OAuth grant, its bearer token and the person's
  per-tool choices. Changing how it signs in drops the grant. Any change to
  the transport empties the cached tool list until it is listed again.
- **Timeouts.**
  - Connecting: 30 s over HTTP and 90 s over stdio, since npx may
    download the server on first run.
  - A call: 120 s.
  - An idle stdio process is stopped after 10 minutes.
  - An idle HTTP client is dropped after 30 minutes.

## Sign-in

`AuthMethod` on the connection says which kind of sign-in it uses:

- **`none`.** Paseo and Chrome.
- **`bearer`.** A token is sealed and sent as `Authorization: Bearer`.
  This is GitHub with a personal access token.
- **`headers` / `env`.** Any named header (HTTP) or environment variable
  (stdio) may be given a sealed value. The connection records only the
  names.
- **`oAuth { clientId?, scopes, redirectPort? }`.** The MCP authorization
  flow:
  1. The server's 401 names its protected-resource metadata (RFC 9728).
     Without it, the client tries `/.well-known/oauth-protected-resource`
     with the server's path inserted, then at the root.
  2. Its first authorization server's metadata comes from RFC 8414 with
     the path inserted, then from OIDC `openid-configuration`.
  3. With no `clientId` on the connection, the client registers itself
     dynamically (RFC 7591) at the loopback redirect, once per sign-in.
     A server that does not offer registration (Slack, GitHub, Google)
     fails with "this service needs a client ID of your own", and the
     panel asks for one.
  4. PKCE S256, a random `state`, `resource=` set to the server's
     canonical URL (RFC 8707), and the scopes:
     - the connection's own, if it has any;
     - otherwise the `scope` from the challenge;
     - otherwise the resource metadata's `scopes_supported`.

     Google gets `access_type=offline&prompt=consent`, so that it sends a
     refresh token. An authorization server whose endpoints are plain HTTP
     off this machine, or that lists challenge methods without S256, is
     refused.
  5. The browser comes back to `karmayoga_mail::oauth::Loopback`. The
     port is fixed by `redirectPort` for providers that match redirect
     URLs exactly (Slack), and ephemeral otherwise.
  6. The code is exchanged. The token endpoint's reply may be JSON or,
     from GitHub, form-encoded. Slack nests the token under `authed_user`.
     Everything needed to refresh is sealed into the connection's
     secret: the client id and secret, the token endpoint, the auth
     method, the resource, the access and refresh tokens, and the expiry.
  7. Before each connect, an access token due to expire within 60 s is
     refreshed, and any rotated refresh token is saved. If the refresh is
     refused, the connection is marked `needsSignIn`.

Two connections to the same server register two clients and keep two
grants, so multiple accounts need nothing special.

## The assistant

Listing every remote tool directly would put hundreds of tools in front
of the model; Datadog's server alone has more than a hundred. The prompt
cache would also break every time a server changed its list. So the
assistant gets two tools, declared in the service the way the browser's
are:

- **`find_connection_tools { query, connection? }`** searches the cached
  tool lists by name, title and description, and returns the best
  matches, ten by default. Each match comes with its connection, its
  input schema and whether it will ask first. A connection whose list is
  missing, or older than an hour, is listed first. A connection that
  cannot be reached shows up in `unavailable`, with the reason, rather
  than failing the search.
- **`use_connection_tool { connection, tool, arguments }`** calls the
  tool. `connection` is a connection's name, case-insensitive, or its id.

The guidance names the connections that are switched on, with their
presets, so the model knows what exists without spending a search on it.
Example: "Connected services: Work Slack (Slack), Linear (Linear).
Find their tools with find_connection_tools, then call one with
use_connection_tool."

**Policy.** Each tool has a `ToolPolicy`:

- `allow` runs without asking;
- `ask` stops for a confirmation card of kind `connection`, showing the
  connection, the tool and the arguments;
- `off` refuses, and the tool is left out of search results.

The default comes from the tool's own hints: `readOnlyHint: true`
without `destructiveHint` is `allow`, and everything else is `ask`. The
hints are the server's claim and nothing more, so a person can change
any tool in the panel.

Once a stranger's writing has reached the turn, only an `allow` the person
chose runs without asking; a tool allowed only by its own hint asks. A
mail, a page, a connection's result or a server's own instructions all
count as a stranger's writing. A read still sends its arguments to the
server, and an injected "search for <their notes>" is a read.

The gate fails closed: if the settings cannot be read, the call is refused.

**Where it is offered.**

- Never to a dream.
- To a routine or a background job only through connections switched to
  `unattended`. With nobody to ask, `ask` is refused there, as a send is.
- From a Matrix chat, the question goes through the asker like any other
  confirmation.

**Taint.** Whatever a connection returns is a stranger's writing: a
Slack message or an issue body can carry instructions. A successful
`use_connection_tool` therefore sets the same flag that a mail read sets,
so a later fetch of an address the model composed asks first. Arguments
are written by the model, which is why a write tool's card shows them in
full.

## Presets

Presets are templates, and every field stays editable after a preset is
chosen.

| Key | Transport | Sign-in |
| --- | --- | --- |
| `slack` | `https://mcp.slack.com/mcp` | OAuth, your own Slack app's client ID and secret, `redirectPort` 33418 |
| `google-drive` | `https://drivemcp.googleapis.com/mcp/v1` | OAuth, your own Google OAuth client (Desktop), scope `drive` |
| `linear` | `https://mcp.linear.app/mcp` | OAuth, registers itself |
| `github` | `https://api.githubcopilot.com/mcp/` | Personal access token |
| `paseo` | `http://127.0.0.1:6767/mcp/agents` | None |
| `chrome` | `npx -y chrome-devtools-mcp@latest` | None |
| `datadog` | `https://mcp.datadoghq.com/v1/mcp` | OAuth, registers itself |
| `custom-http` | a URL | Any |
| `custom-stdio` | a command | Any |

## Not done

- Resources, prompts, sampling and elicitation.
- The deprecated HTTP+SSE transport.
- Exposing connections to outside MCP clients through this
  application's own server.
- Syncing anything from a connection into the vault.
- Proposals ("later") for connection calls.
