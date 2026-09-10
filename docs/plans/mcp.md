# An MCP server: the assistant's own verbs, for somebody else's model

A plan for letting Claude Code, Claude Desktop, an OpenAI agent or anything
else that speaks the Model Context Protocol reach this vault's tools —
over stdio for a program on this machine, and over HTTP for one on the home
network. Written 10 September 2026 against the tree at `ecf778c`, on
`tedious-dragon`.

Nothing here is a new capability. Every verb an MCP client will be able to
call is one `everyday do` can already run and one the rail's assistant
already has. What is being written is a translation layer and a switch.

## What this is

Four things, in dependency order, and each is small because the layer under
it already exists.

1. **A protocol crate.** `everyday-mcp`: JSON-RPC 2.0, `initialize`,
   `tools/list`, `tools/call`, `ping`, and the notifications that go with
   them. Synchronous where it can be, no socket anywhere, tested offline —
   the same argument `everyday-core::agent::tools` makes about itself.
2. **One transport, and a pipe to it.** Streamable HTTP — `POST /mcp` on a
   listener of its own in `everyday-server` — is the transport. A client
   that speaks only stdio gets `everyday mcp`, which is a hundred lines of
   pipe between stdin/stdout and that endpoint, not a second server.
3. **A switch and a port**, in the Vault tab beside sharing, built out of
   the same `Config` / `Running` / status-panel parts `Sharing` already
   uses.
4. **A token that is a device**, so "what can reach this vault" keeps
   having one answer and one list.

## What is deliberately not here

- **Hosted connectors.** ChatGPT's and claude.ai's web connectors want a
  publicly reachable HTTPS endpoint, a certificate no client will pin, and
  OAuth 2.1 with dynamic client registration. That is a bigger project than
  everything below put together and it is not what was asked for. Local and
  local-network agents only. A desktop ChatGPT or Claude app configured
  against `http://127.0.0.1:<port>/mcp` or against the stdio command is in
  scope; the cloud reaching in is not.
- **`resources/` and `prompts/`.** Notes and entries would make plausible
  MCP resources and it would be a day's work, but a tools-only server is
  the whole of what a calling agent needs, and every method not implemented
  is one that cannot be got wrong.
- **`sampling/` and `elicitation/`.** These let a *server* ask the client's
  model for a completion. This vault has its own model configuration and no
  reason to borrow one.
- **New tools.** In particular `web_search` stays out. It lives in
  `everyday-service` rather than the core catalogue, it is absent from
  `list_tools` for that reason, and an MCP client has its own way to search
  the web. Adding it here would mean this vault's key was paying for
  somebody else's search.

## The decisions

### One shim, generated from the catalogue

The tool list is not written down anywhere in this plan and must never be.
`tools::available(&vault)` already yields, per tool, everything MCP asks
for:

| MCP field       | Source                                          |
|-----------------|-------------------------------------------------|
| `name`          | `Tool::name`                                     |
| `title`         | `domains::meta::title_of(name)`                  |
| `description`   | `Tool::description` — already written for a model to read |
| `inputSchema`   | `Tool::parameters()` — already JSON Schema       |
| `annotations.readOnlyHint`    | `effect == Effect::Read`           |
| `annotations.destructiveHint` | `effect == Effect::Destructive`    |

So `tools/list` is one `map` over one slice, and `tools/call` is one
function. There are thirty-four tools and there will be one shim. A tool
added to `ALL` in `tools.rs` appears over MCP with no other edit, which is
the property to protect: the day somebody adds a tool and has to remember
to register it in a second place is the day the two lists start disagreeing.

The same instinct suggests generating tools from
`everyday_service::command::catalog()` instead, since that table is
introspectable too — and it should not be done. A command's signature is
`("query", "EntryQuery", false)`: Rust type *names*, enough to generate a
TypeScript client against types the UI also has, and not enough to make a
JSON Schema a model can fill in. The tool catalogue carries real schemas
and descriptions written for exactly this reader. Tools are the surface;
commands are the transport under it.

### The protocol lives apart from the transport

`everyday-mcp` holds a `Session` — a small state machine over JSON-RPC
messages — and a `Host` trait with two methods, roughly `list_tools()` and
`call_tool(name, args)`. The crate depends on `serde_json` and nothing
else; it does not know what a vault is, and its tests are tables of request
values and expected response values that run in microseconds.

`everyday-server` implements `Host` over `Arc<Service>` and is the only
implementation — the stdio pipe in phase 3 does not parse the protocol at
all. So a bug in framing or in the state machine is a unit test in a crate
with no I/O, rather than a thing found by attaching Claude Desktop to it.

### Streamable HTTP is the transport; stdio is a pipe to it

The instinct is that stdio is the simpler of the two — no port, no token,
no listener, and the transport every MCP client supports. In this codebase
it is the more complicated one, for a reason that is written down as a test.

**The second process to open a vault opens it read-only**
(`everyday-vault/src/lib.rs:1775`). A stdio server spawned by an agent,
opening the vault beside a running Every Day, can list tasks and cannot
create one: every write tool in the catalogue fails with "read-only", which
is the worst possible shape of failure to hand a model. It would work only
when the app is closed — and when the app is closed the vault is sealed and
the spawned process has no way to ask anybody for a password, because its
stdin is the protocol.

So a stdio MCP server has to forward to the running app rather than open
anything. That is the arrangement `serve_socket` was built for — but the
client half of it does not exist: `everyday-server/src/client.rs` speaks
TLS over TCP, and the CLI only ever *serves* a socket (`app.rs:1092`),
never connects to one. "Just do stdio" therefore means writing an HTTP
client over a Unix socket, wiring it through a spawned process, and getting
a transport that is strictly HTTP with an extra hop and an extra lifetime.

HTTP has none of that. The app is already running, already owns the single
writable vault handle, already has a router and a token registry, and is
already where a password can be asked for. And the switch-and-port this
feature is *for* is an HTTP object: a stdio transport has no port, and a
toggle in the settings panel would govern nothing.

Hence: HTTP is the transport. `everyday mcp` still exists, because a client
that cannot be given a URL is a real thing and the fallback is cheap — but
it is rescoped to a dumb pipe. It reads JSON-RPC lines from stdin, posts
them to `http://127.0.0.1:<port>/mcp` with the token from `mcp.json`,
writes responses to stdout, and holds no vault handle at all. Confirm which
of the clients you actually use will take an HTTP URL before deciding how
much that fallback matters; support has been moving, and it is the one
claim here worth re-checking rather than inheriting.

### Calls go through `Service::call`, not `tools::dispatch`

`domains::meta::run_tool` already exists, already filters against
`tools::available`, and already refuses a destructive tool without
`confirmDestructive`. Going through `Service::call(ctx, "run_tool", …)`
(`service.rs:293`) rather than reaching past it into `tools::dispatch` buys
three things for free: the change event fan-out, so a task an external
agent creates appears in the open window immediately; idempotency, so a
retried write is not a second write; and the scope check on the `Ctx`.

### Destructive tools are absent, not refused

The flag is `allow_destructive` on the MCP config, default off. When it is
off, destructive tools are filtered out of `tools/list` entirely rather
than listed and then refused.

This is the important half. `run_tool` answers a destructive call with
`confirm_required` and a message saying to call again with
`confirmDestructive` — which is the right answer to a script and the wrong
one to a model, because a model reads that message and calls again with the
flag set. A gate whose refusal explains how to get past it is not a gate.
So the shim never sets `confirmDestructive` from anything the client sent;
it sets it from the server's own configuration, and when the configuration
says no, the tool is not offered at all.

With the flag on, `confirmDestructive: true` is passed and the client's own
tool-approval prompt is what stands in front of a delete. That is a real
answer — Claude Code and Claude Desktop both ask — but it is somebody
else's interface enforcing this vault's rule, so it is opt-in and the panel
should say what it means.

### A locked vault offers nothing

`tools/list` returns an empty list when the vault is locked or absent, and
`tools/call` returns an error saying so. Not a filtered list, not an error
on list — an empty list, because that is the accurate statement: right now
this server can do nothing.

The right thing then happens on unlock. `EventSink::lock_state`
(`events.rs:197`) already exists, so the MCP server subscribes as a sink,
and on the transition to unlocked it sends
`notifications/tools/list_changed` to every open session. A client that
connected while the vault was locked picks the catalogue up without being
restarted, which is precisely what that notification is for. `initialize`
itself always succeeds, so a locked vault reads to the client as "connected,
nothing available yet" rather than as a broken server.

### A token is a device

`Registry` (`auth.rs`) already issues bearer tokens, validates them into a
`Ctx` with scopes, lists them for the settings panel, and revokes them. An
MCP client should be a row in that list, not a second parallel notion of
who is allowed in.

What is missing is a way to mint one without the pairing dance, which makes
no sense here — there is no second screen to show a code to. So
`Registry::pair` gets split: an `issue(name, scopes) -> (token, id)` that
does the minting, and `pair` becomes the code-checking wrapper around it.
The MCP panel calls `issue("Claude Code", …)` and shows the token once,
with a copy button, for pasting into the client's configuration.

Scopes: `Scope::All` to begin with, which is what a paired desktop gets
today, and which `run_tool` requires anyway (its command scope is `All`).
Narrowing is phase 6 and is worth doing.

The stdio pipe reads the same token out of `mcp.json` and sends the same
header, because it is talking to the same endpoint as everything else. It
is a file only this user can read, on a loopback port only this machine can
reach; there is no second trust model to design.

### Loopback by default, and `Origin` is checked

The listen address defaults to `127.0.0.1`, not `0.0.0.0`. Sharing defaults
the other way because sharing is *for* other machines; an MCP server is for
an agent on this desk, and the network case is the exception somebody opts
into with the address picker.

Either way the `Origin` header is validated and a request carrying one that
is not a local origin is refused. This is not paperwork: a plain HTTP
server on a loopback port is reachable by any web page the person has open
via DNS rebinding, and the MCP specification requires the check for exactly
this reason. It is the one security control on this route that has no
equivalent in the existing `/v1` surface, because that surface is TLS with
a pinned certificate and this one is deliberately not.

TLS is off by default here. A pinned self-signed certificate is right for
the app talking to itself and wrong for an MCP client, which has no pinning
story and will simply refuse. Loopback plaintext is the honest arrangement;
somebody serving MCP across a LAN should be told to put it on a WireGuard
or Tailscale address, which is what `advertisable_addresses` already sorts
to the top.

## The shape

```text
crates/everyday-mcp/          new. protocol only, no I/O
  src/lib.rs                  Session, Host, the method table
  src/schema.rs               request/response types
  tests/protocol.rs           tables of messages in, messages out

crates/everyday-server/
  src/mcp.rs                  new. Host over Arc<Service>, axum route,
                              Origin check, session ids, the config
  src/auth.rs                 + Registry::issue, pair refactored onto it
  src/lib.rs                  + McpConfig, start_mcp, mcp.json

crates/everyday-cli/
  src/app.rs                  + Command::Mcp { .. }, the stdio<->HTTP pipe

crates/everyday-app/
  src/mcp.rs                  new. lifecycle, modelled on sharing.rs
  src/commands.rs             + mcp_status, mcp_start, mcp_stop,
                              mcp_issue_token, mcp_set_destructive
  src/lib.rs                  + the five in generate_handler!

ui/src/components/McpPanel.svelte   new, modelled on SharePanel.svelte
ui/src/components/SettingsDialog.svelte   + the panel, Vault tab
```

## Phases

### 1. The protocol, offline

Read `docs/plans/mcp-protocol-notes.md` first. The revision current as of
10 September 2026 is `2026-07-28`, and it is not the protocol most SDK
examples describe: it removed the `initialize` handshake, protocol-level
sessions, the standalone `GET` stream and resumable streams. Version,
client identity and capabilities now ride in `_meta` on every request, and
every result carries `resultType`.

`everyday-mcp` therefore holds no session state machine. It holds a `Host`
trait (`list_tools`, `call_tool`) and a pure function from one JSON-RPC
message to one answer, plus an `Era` telling it which envelope to use.

**Dual-era, and this is not optional.** Modern (`2026-07-28`) because it is
the specification; legacy (`2025-11-25`, `2025-06-18`) because the modern
revision is six weeks old and the clients on this desk today still speak
the old one. The specification blesses exactly this and says how to choose:
a request carrying modern `_meta` is served statelessly, an `initialize`
request selects legacy semantics. The tool layer underneath is identical;
only the envelope differs.

Methods: `server/discover` (mandatory in modern), `tools/list`,
`tools/call`, `subscriptions/listen`, plus `initialize` and
`notifications/initialized` for legacy. Everything else is `-32601`.

Errors follow the notes: `-32602` for an unknown tool, `-32022` for an
unsupported version with `data.supported`, `-32020` for a header/body
mismatch, and — the one that matters most — a tool that *fails* comes back
as a result with `isError: true` and the message as text, never as a
JSON-RPC error. That distinction is what lets a model correct itself, and
it is what the `Args` accessors in `tools.rs:255+` were written to feed.

**Done when** `cargo test -p everyday-mcp` covers, per era: a well-formed
call, a request missing required `_meta`, an unsupported version, an
unknown method, an unknown tool, a failing tool, `server/discover`, and a
malformed body.

### 2. HTTP

`POST /mcp` on its own listener, default `127.0.0.1:7398` —
`DEFAULT_MCP_PORT`, one along from sharing's 7397 and unassigned by IANA.
`McpConfig { listen, enabled, allow_destructive, token }` in `mcp.json` next
to `server.json`, with the same `load`/`save`. The token is minted by
`Registry::issue` (already split out of `pair`) and authenticated through
`Registry::authenticate`, so an MCP client is a row in the same device list
as a paired phone.

These names are settled and documented in `README.md` under *Letting another
agent in*; changing one means changing that section too.

`Origin` is validated and a present-but-invalid one is refused with `403`
— note the exact condition, since a native client sends no `Origin` at all
and must not be refused for it. Header mirroring (`MCP-Protocol-Version`,
`Mcp-Method`, `Mcp-Name`) is validated against the body, `400` and `-32020`
on a mismatch. An unknown method answers `404` with `-32601`, which is a
status this codebase's `Failure` mapping already produces for
`unknown_command`.

Live tool-list refresh has two shapes because the eras differ. Modern: a
`subscriptions/listen` POST whose response is a long-lived SSE stream.
Legacy: `GET /mcp` opens the standalone stream. A modern-only client that
`GET`s must be told `405`. Both carry the same
`notifications/tools/list_changed`, and a client that opens neither loses
nothing but the refresh.

Reuse `everyday_server::start`'s bind-and-shutdown shape, including
`stop_and_wait` — the port-rebinding race that comment describes is
identical here.

**Done when** a `tests/mcp.rs` in `everyday-server` drives, over the real
router and in both eras: discovery, list, call, a locked vault, a bad
`Origin`, a header/body mismatch, and `GET`/`DELETE`. And when a real
client — `claude mcp add --transport http everyday
http://127.0.0.1:<port>/mcp --header ...` — lists tools and runs one.

### 3. The stdio pipe

`everyday mcp` in the CLI, beside `serve` and `do`. It opens no vault and
imports no protocol: it reads a line from stdin, posts it to the configured
endpoint, writes the response line to stdout, and repeats. Notifications
arriving on the `GET` stream are written to stdout as they come.

Two rules worth stating loudly in the code. **Nothing but protocol goes to
stdout, ever** — a stray `println!` in that stream corrupts the session, so
`tracing` and every human-readable message go to stderr. And when the
endpoint is not answering, say so in a JSON-RPC error mentioning the switch
in Settings; a client whose server exits silently reports nothing useful.

Because this holds no vault handle, the read-only trap in the transport
decision above cannot be reached from here, and there is no case where the
pipe works but writes fail.

**Done when** a stdio-only client configured with `everyday mcp` lists
tools and runs one against the app's running server.

### 4. The switch

`crates/everyday-app/src/mcp.rs`, a near-copy of `sharing.rs`: a `Mutex<Option<Running>>`,
`start`/`stop`/`stop_and_wait`/`status`, config persisted so a machine that
had it on has it on again after a restart, and resumption at startup
alongside the sharing resume at `commands.rs:178`.

`McpStatus` carries: running, address, the addresses picker, port,
`allowDestructive`, whether a token has been issued, and how many sessions
are open. `McpPanel.svelte` draws it with SharePanel's markup and the same
`run(work)` busy-state helper.

The panel has to say three things plainly, in the way the sharing switch
says what a server sees: that the agent on the other end is somebody else's
program, that the vault's contents go into its context, and — if the
destructive switch is on — that deletes will be approved in that program's
interface rather than in this one.

**Done when** the switch survives a restart and the port field rebinds
without an "address already in use".

### 5. Writing it down

A `mcp.json` snapshot test in `everyday-service`, in the exact shape of
`tests/surface.rs`: the tool catalogue as JSON, committed, compared, updated
with `UPDATE_SURFACE=1`. Adding a tool then shows up as a reviewable diff in
the MCP surface too, and dropping one cannot happen quietly.

`README.md` gets the two configuration snippets — the stdio command and the
HTTP URL with a header — because the failure mode for this feature is
somebody not knowing what to paste where.

### 6. Narrowing what an agent may reach (optional, worth doing)

Today `run_tool` requires `Scope::All`, so an MCP token is all-or-nothing.
`Domain` and `Scope` are nearly the same enum already, so: map
`tools::Domain -> ctx::Scope` in `meta.rs`, have `run_tool` require the
mapped scope for the tool it was asked for, and have `list_tools` filter by
what the caller holds. Then the MCP panel can offer checkboxes — "this agent
can reach Tasks and Notes" — and `Registry::issue` is given that list.

This is the phase that makes the feature safe to leave on. It is separable,
so it is last, but it should not be skipped.

## Risks, and the honest ones

- **Prompt injection reaches further.** The rail's assistant runs on a model
  this vault configured, with a system prompt it wrote. An MCP client does
  not: whatever the agent on the other end has been told, and whatever text
  it has picked up along the way, can call these tools. `Sensitivity::Secret`
  and the destructive flag are the two backstops, and both come free by
  going through `tools::available` and `run_tool`. The rest is the person's
  choice, which is why it is a switch.
- **Session state is memory-only.** A restart drops sessions and clients
  must re-initialize. Every MCP client does this on reconnect. Not worth
  persisting.
- **Blocking-pool pressure.** An agent in a loop can issue calls faster than
  a window ever would. Reuse the `MAX_IN_FLIGHT` semaphore from
  `routes.rs:54`, and consider a lower ceiling for MCP than for a paired
  desktop.
- **The protocol revision will move, and it just did.** `2026-07-28` removed
  the handshake, sessions and the GET stream that every older tutorial
  describes; the next revision may retire the legacy era we are deliberately
  still answering. Everything version-shaped lives in `everyday-mcp` behind
  `SUPPORTED_VERSIONS` and an `Era` enum, so dropping legacy is deleting one
  arm of a match rather than unpicking a transport.
- **Legacy support is a dated asset.** We answer `2025-11-25` and
  `2025-06-18` because that is what today's clients speak. Revisit once the
  clients in use have moved; the tests are per-era, so the day it can go, the
  tests say so.

## Sizing

Phases 1 and 2 are the working feature and are about two days. Phase 3 is
half a day and can be skipped entirely if every client you use takes a URL.
Phase 4 is a day of plumbing against a pattern that already works. Phases 5
and 6 are half a day each. Nothing here requires a decision that has not
been made above.
