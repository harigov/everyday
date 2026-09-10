# Plan: one vault, many windows; and a palette for every verb

Two features, planned together because they share a spine, and planned with
the next apps and clients in view: Contacts, Passwords and Notes as apps; a
browser extension, a phone shell and the CLI as clients.

1. **Server mode.** The app can serve its open vault to other copies of the
   same app on other machines. Those copies hold no key, no ciphertext and no
   index; every command they run travels to the server. Pairing is a URL, and
   the QR code is a picture of it. For people who self-host and want the data
   in one place.
2. **Quick actions.** A command palette in the window (`mod+k`) and an OS-wide
   hotkey that raises it, drawing on one registry of everything the app can
   do, which also feeds the tray and the shortcut help sheet.

Branch: `add-server-mode-remote-clients`, rebased on `main` at 371783e.

**Status: built.** Every phase below is implemented, with four deliberate
deviations recorded at the bottom under *What was done differently*. Read that
section before this one if you are picking the work up.

## The shape

```
everyday-core            gains: a sensitivity class per domain; the
                         read-pool/single-writer store split
everyday-vault           unchanged
everyday-service   NEW   command bodies, feeds, websearch, agent harness, media
                         planning. One entry point: call(ctx, name, json) -> json.
                         Assembled from per-domain modules. Events out through a
                         sink trait. Owns the protocol version and the schema
                         export the interface's types are generated from.
everyday-server    NEW   one router over the service, served on TLS/TCP for
                         remote clients and on a local socket for the CLI and the
                         browser-extension host. Pairing, scoped device tokens,
                         self-signed cert, listen address, audit.
everyday-app             window, tray, everyday:// adapter, and ONE generic Tauri
                         command that either runs call() locally or forwards it.
                         Embeds everyday-server behind a "Share" toggle and always
                         serves the local socket.
everyday-cli             gains `everyday serve`, `everyday do <tool>`, and the
                         ability to write through a running app via the socket.
ui                       api.ts routes through the generic command; generated
                         types and client; the palette; the connect screen; the
                         share settings.
```

### Four decisions the rest depends on

**The unit that crosses the wire is the command, not the store method.** A
command may fetch a feed, run a web lookup, or run the assistant; none of that
can happen on a client with no network policy and no key. So the service
crate holds what the Tauri commands hold today, minus Tauri.

**Commands are the one public surface.** The interface, the CLI, the browser
extension and the assistant all call commands. Assistant tools become thin
wrappers over commands that add a description written for a model. There is
no third API. The surface is versioned, its argument and result types are
plain serde structs, and the TypeScript for the interface is generated from
them rather than maintained by hand.

**A domain is a module that registers itself.** Each domain (journals, tasks,
calendars, library, trackers, agent, and later contacts, passwords, notes)
contributes one slice of command entries. An entry declares its name, its
argument type, its handler, the *scope* it requires, the *change kind* it
emits, and whether it is *sensitive*. The dispatch table is the concatenation
of those slices. Nothing central has to be told about a new app.

**Every request carries an auth context.** Who is calling (local window,
device id, local socket peer), what scopes the token holds, and when this
caller last proved the vault password. The first release issues one scope,
`all`, to every paired device and never asks for step-up. The fields exist so
that the passwords app and the extension do not need a new protocol.

Protocol version: a `PROTOCOL: u32` constant in the service crate. Client and
server are the same binary, so skew is a matter of time; a mismatch is refused
with a message naming both versions, the way a newer schema is refused today.

## Phase 0: store concurrency

**Goal.** Several callers can read the vault at once. Today one mutex guards
one connection for both SQLite and Postgres, so every read from every client
would queue behind every other, and one slow query would stall them all.

- In `everyday-store-sql`, split the single `Mutex<Box<dyn Connection>>` into a writer and a pool of readers behind the existing `Connection` trait. Writes take the writer; reads take any reader. The SQLite driver opens the pool in WAL mode, which allows many readers beside one writer. The Postgres driver hands out pooled connections.
- The vault's own "one write lands at a time" rule is unchanged; it moves onto the writer.
- Nothing holds a connection across a network await. Verify by reading every command that fetches: today the feed and the lookup both fetch first and save after, which is the pattern to keep.

**Tests.** The conformance suite passes on both drivers. A new test runs a
long read on one thread while a write lands on another and checks neither
waited for the other.

**Done when** a read never waits for an unrelated read.

## Phase 1: extract `everyday-service` (no behaviour change)

**Goal.** The command bodies compile without Tauri, and the desktop shell calls
them through one generic command. This is the refactor the README promised
when it said a second shell would not be a rewrite.

**Move out of `crates/everyday-app/src/`** into `crates/everyday-service/src/`:

| From | To | Notes |
|---|---|---|
| `commands.rs` | `domains/<name>.rs`, one per domain | bodies unchanged; signatures lose `State<'_, AppState>` and take `&Service` and the auth context |
| `state.rs` | `service.rs` as `Service` | vault handle, `reported_feeds`, `Pending`, an `Arc<dyn EventSink>`, the idempotency cache |
| `error.rs` | `error.rs` | `CommandError` and `CommandResult` as they are |
| `feeds.rs`, `http.rs`, `websearch.rs` | same names | `http.rs` becomes the service's one outbound client |
| `agent.rs` | `agent.rs` | `tauri::ipc::Channel<AgentEvent>` becomes `Arc<dyn Fn(AgentEvent) + Send + Sync>`; rig deps move with it |
| `notify.rs` types | `events.rs` | `Notification`, `Level`, `Reach` move; the `AppHandle` emit stays in the shell |

Stays in the shell: `bootstrap`, `create_vault`, `open_vault` (they decide what
the session *is*), `ready_to_close`, `set_tray_menu`, `hide_tray`, the tray, the
protocol adapter, the window event handler.

**The command entry.**

```rust
pub struct Command {
    pub name: &'static str,
    pub scope: Scope,          // Journals | Tasks | ... | Passwords | Admin
    pub effect: Effect,        // Read | Write | Destructive  (reused from tools)
    pub change: Option<Kind>,  // what `changed` reports after a successful write
    pub sensitive: bool,       // requires step-up once step-up exists
    pub streams: bool,         // answers with a stream of events, not one value
    schema: fn() -> Schema,    // argument and result schema, for codegen
    run: fn(&Service, &Ctx, Value) -> BoxFuture<CommandResult<Value>>,
}
```

Each domain module exports `pub static COMMANDS: &[Command]`. A macro keeps
the entries one line each. `Service::call` finds the entry, checks the scope
against the context, deserialises the arguments into the entry's typed struct,
runs the handler on the blocking pool, emits the change, and serialises the
result.

**Idempotency.** Every write may carry a request id. The service keeps the
last few minutes of `(caller, request id) -> result` and answers a repeat
from the cache. This is what stops a retried autosave over a dropped
connection being refused as a conflict against its own earlier write.

**Streams.** `send_message` is the first command with `streams: true`. A
streaming command's handler is given a sink and returns when the stream is
over. The shell pumps the sink into a Tauri channel; the server writes NDJSON.
Any later command (an export, an import with progress) uses the same
mechanism.

**Blobs.** `put_blob(&self, ctx, bytes) -> BlobId` and
`blob_range(&self, ctx, id, start, len)` stay as dedicated methods, since
they are bytes rather than JSON.

**Events.** `pub trait EventSink: Send + Sync { fn notify(&self, n: Notification); fn changed(&self, c: Change); fn lock_state(&self, locked: bool); }`.
`Change { kind, id, op, origin }` carries the caller so a client is not told
about its own writes.

**Schema export.** A `cargo run -p everyday-service --bin schema` (or a test
that writes to a fixture) emits JSON schema for every command's arguments
and result. `ui/scripts/gen-api.mjs` turns it into `ui/src/lib/generated/`
containing the types that `types.ts` holds by hand today and a typed `api`
object. Generation runs in `make lint` and the fixture is committed, so a
Rust change that alters the wire shows up as a diff.

**Shell after the move.** `commands.rs` shrinks to: `call(name, args, requestId)`,
`put_blob(bytes)`, `send_message(..., channel)`, and the shell-local ones above.
`AppState` holds a `Service`. `generate_handler!` lists about ten entries.

**Interface.** In `ui/src/lib/api.ts` the Tauri `invoke` becomes: if the
command is one of the shell-local set, call it by name; otherwise call
`call` with `{ name, args, requestId }`. Writes mint a request id. The
hand-written `types.ts` is replaced by the generated module in the same
change or the one after, once the generated output is reviewed. The mock
backend is untouched.

**Tests.**

- `everyday-service/tests/call.rs`: open a temp SQLite vault, `call("list_journals", {})`, `call("save_entry", …)` then `call("get_entry", …)`, an unknown name, a wrong-typed argument, a scope the context lacks, and the same write twice with one request id.
- `make lint` and `make test` green. Run the app and walk each app once.

**Done when** the desktop app behaves identically, the service crate has no
`tauri` in its dependency tree, and the interface types are generated.

## Phase 2: `everyday-server`

**Goal.** One router serves an open vault to paired clients over TLS and to
local processes over a socket.

**Transports.** The router is built once and served twice.

- **TCP with TLS**, for remote clients. Certificate and key live in the app config directory, not beside the vault header, so `backup` never copies a private key. Generated with `rcgen` on first start, SAN carrying every address the server listens on. Fingerprint is sha256 of the DER and is what the pairing URL carries. `--no-tls` for a reverse proxy holding a real certificate; the client then pins nothing and requires `https` from the proxy.
- **A Unix socket or Windows named pipe**, always on while a vault is open, in the desktop app as well as in `serve`. No TLS and no token: the OS's peer credentials are the auth, and only the same user may connect. This is how the CLI writes through a running app and how the browser-extension host reaches it.

**Endpoints, all under `/v1`.**

| Route | Auth | Body |
|---|---|---|
| `GET /hello` | none | `{ name, protocol, fingerprint, locked }` |
| `POST /pair` | pairing code | in: `{ code, deviceName, scopes }`; out: `{ token, deviceId, scopes }` |
| `POST /call/:name` | bearer or socket peer | JSON args in, JSON result out, `{ code, message }` on error; a `streams` command answers NDJSON |
| `POST /blob` | same | raw body in, blob id out; sized so a chunked, resumable form can be added beside it |
| `GET /blob/:id` | same | honours `Range`; uses `everyday_vault::media::plan` exactly as `protocol.rs` does |
| `GET /events` | same | SSE: `notify`, `changed`, `lockState` |

Every request carries `X-Everyday-Protocol: <n>`, `X-Everyday-Client: <deviceId>`
and, on writes, `X-Everyday-Request: <id>`. A protocol mismatch is `426`
naming both versions.

**Auth.**

- `devices.json` in the app config directory: `[{ id, name, tokenHash, scopes, created, lastSeen, expires }]`. Written temp-fsync-rename like the header.
- Token: 32 random bytes, base64url on the wire, blake3 at rest, constant-time compare. Tokens expire after thirty days of no use and the device must pair again.
- Pairing code: 8 characters from an unambiguous alphabet, one at a time, 5 minute expiry, single use, in memory. Five failed attempts in a minute locks pairing for ten.
- Scopes are checked per command from the entry's `scope`. The first release issues `all`.
- Unlock attempts run one at a time, server-wide, with per-device backoff, because each one burns 64 MiB of Argon2 by design and a stolen token must not be able to make the server do that in parallel.
- Inbound body limits on `/call` and `/blob`, and a per-device concurrency cap.

**Audit.** The service records `(device, command, record id, time)` for every
sensitive command, in a table the server's settings screen can show. Empty
until the passwords app exists, present so it is not designed then.

**Lock.** The server's vault lock governs every client. A background task
runs `auto_lock_if_idle` and emits `lockState`. `touch` from any client
counts. `unlock` over the wire is allowed by default and is a setting,
`allowRemoteUnlock`, in `server.json` with the listen address.

**CLI.** `everyday serve --vault DIR --listen ADDR [--pair]`. `--pair` prints
the pairing URL and a QR code in the terminal. Starts locked unless
`EVERYDAY_PASSWORD` is set. Every existing CLI command tries the local socket
first and falls back to opening the vault itself, so `everyday new` while the
app is open writes through the app instead of coming up read-only.

**Pairing URL.**

```
everyday://pair?host=100.64.0.12:7397&fp=<hex>&code=<code>&name=<vault name>
```

**Tests.** `everyday-server/tests/`: start on `127.0.0.1:0` and on a temp
socket with a temp vault; `hello`; pair then call `list_journals`; a wrong
code; a revoked token; an expired token; a scope the token lacks; a protocol
mismatch; upload a blob and read the middle of it with `Range`; SSE receives
`changed` after a save from a second client and not from the first; two
concurrent unlocks run one after the other; a socket peer of another user is
refused.

**Done when** the CLI can serve a vault, write through a running app, and the
test client can drive both transports.

## Phase 3: client mode in the desktop shell

**Goal.** The desktop app connects to a server instead of opening a vault, and
the interface does not know the difference beyond a status field.

- `state.rs`: `enum Session { Local(Service), Remote(RemoteClient) }`. `AppState` holds one.
- `remote.rs`: `RemoteClient` on `reqwest` with HTTP/2, one pooled connection, the pinned certificate and the bearer token. Methods mirror the service: `call`, `put_blob`, `blob_range`, `send_message` (reads NDJSON and pumps the Tauri channel), and an `events` task that re-emits SSE as window events: `everyday://notify`, `everyday://changed`, `everyday://lock-state`. Reconnects with backoff; after three failures emits a notice the interface draws as a condition.
- Certificate pinning: at pairing time fetch the certificate, check its fingerprint against the URL, store the PEM, and build the client with that certificate as its only root. Spike first: `rcgen` must put the IP in the SAN or `reqwest` will reject the name. Fallback is a custom rustls verifier comparing the fingerprint.
- `remotes.json` in the app config dir holds `{ id, name, host, fingerprint, certPem }`. The token goes in the OS keychain via the `keyring` crate, never in the file.
- New shell commands: `list_remotes`, `connect_remote(url | id)`, `forget_remote(id)`. `bootstrap` returns known remotes beside the local vault path.
- `protocol.rs`: when the session is remote, `everyday://localhost/<id>` proxies to `GET /v1/blob/<id>` with the `Range` header passed through and the response headers passed back. The content security policy is unchanged.
- **In-memory cache.** Blobs are content-addressed and immutable, so a cache keyed by blob id is always correct. The remote client keeps a bounded LRU of fetched ranges in memory (a few hundred megabytes at most, configurable), which makes scrubbing a video and scrolling a shelf of covers local after the first fetch. Nothing is written to disk; a disk cache is a later opt-in. The same LRU shape can hold the last result of read commands keyed by `(name, args)` and be invalidated by `changed` events; that is the second step and only if the call audit below shows it is needed.
- `VaultStatus` gains `remote: Option<{ name, host }>`; the service returns `None`.
- Interface: the picker gets "Connect to a computer…" with a paste field for the URL, and a list of known connections. Settings hides "change backend" and "open another vault" while remote. The connect screen states there is no offline mode.
- Version skew: `connect_remote` calls `hello` first and refuses with the two versions named.

**The call audit.** Before this phase ships, instrument the mock invoke to
count calls in a typical minute of use. Expected fixes: throttle `touch` to
once per thirty seconds; drop `poll_auto_lock` in favour of the `lockState`
event; run the two refreshes after an autosave in parallel; stop calling
`status` where the answer is already held. Each was free over IPC and is a
round trip now.

**Tests.** `RemoteClient` against the phase 2 test server: call, blob round
trip with a range and a cache hit on the second read, event delivery,
reconnect after the server restarts, the idempotent retry.

**Done when** two copies of the app on two machines edit one vault and the
save-conflict dialog appears when they edit the same entry, and never when a
retried save reaches a server that already has it.

## Phase 4: serve from the desktop app

**Goal.** A person who runs the desktop app on the machine that holds the
vault can share it without a terminal.

- The server runs inside the Tauri tokio runtime (`tauri::async_runtime::spawn`, never a second runtime) over the same `Service` as the local window, so both see one vault and both receive events.
- Settings, Vault tab, "Share on the network": a switch; a listen address picker listing interfaces, preferring a Tailscale address when one exists; the pairing URL with a copy button and a QR code (rendered as SVG by the server, drawn as `data:` which the CSP already allows); the device list with scopes, last seen and "forget"; the remote-unlock switch; the audit log once anything writes to it.
- Shell commands: `share_start`, `share_stop`, `share_status`, `new_pairing_code`, `list_devices`, `revoke_device`.
- Sharing persists in `server.json` and restarts with the app.

**Done when** a second laptop can pair by pasting the URL from the first
laptop's settings.

## Phase 5: live refresh

**Goal.** A change made from one window appears in the others.

- The interface listens for `everyday://changed` and routes by kind to the refresh methods the stores already have: `app.requestRefresh`, and the reload paths in `todo`, `calendar`, `library`, `tracking`. Coalesced the way autosave refreshes already are.
- Changes from this window are excluded server-side by origin, so nothing flickers.
- A `lockState` event shows the lock screen or re-runs bootstrap.

**Done when** a task ticked on one machine disappears from the other's list
within a second.

## Phase 6: actions and the palette

Independent of phases 0 to 5 and small enough to land first.

**6a. One registry.** `main` now has `ui/src/lib/shortcuts.svelte.ts` whose
`Binding` rows carry a label, a group, a `when` and a `run`. That is the
registry; it grows rather than being replaced.

- `keys.ts`: `Binding` gains `id: string`, `keywords?: string[]`, `icon?: string`, `tray?: boolean`, `raise?: boolean`, `checked?: () => boolean`, and `keys` becomes optional. Rows without keys are palette-only.
- `shortcuts.svelte.ts` keeps dispatching the rows that have keys. Rename `BINDINGS` to `ACTIONS` with a re-export for the help sheet.
- `tray.svelte.ts`: the four `tray.register` calls become rows with `tray: true` in the same table; the reactive re-run and the description-only crossing to Rust stay as they are. `TRAY_ORDER` becomes group order.
- `Palette.svelte`, opened with `mod+k` and from the bar: lists applicable actions by group, fuzzy matched on label and keywords, shows the shortcut beside each. When the query matches nothing it offers "Add as task", "Add to library", "New entry with this" and "Search", running the quick-add grammar in `quickadd.ts`. Opened over the lock screen it offers only unlock.
- Tests: `keys.test.mjs` for the wider `Binding`; a palette test on the mock backend for matching and the capture fallbacks.

**6b. Tools as wrappers.** In the core, each `Tool` gains a human `title` and
a sensitivity check: `tools::available` never returns a tool from a domain
whose class is `Secret`, whatever the settings say. Tool bodies that duplicate
a command body are rewritten to call the command, so there is one
implementation of "mark this book read". The service exposes `list_tools` and
`run_tool(name, args)` with the same confirmation rule the assistant applies.
CLI: `everyday do <tool> [json]`. The in-window palette calls the stores,
because the stores are what update the screen; once phase 5 exists the two
paths can converge.

**6c. The OS hotkey.** `tauri-plugin-global-shortcut`, default
`mod+shift+space`, changeable and switchable off in Settings. It raises the
window and emits `everyday://palette`. Settings notes that Wayland needs a
desktop portal and offers the tray as the fallback. A dedicated palette window
that appears without raising the main one is a follow-up, once the in-app
palette has settled what belongs in it.

## What the next apps and clients need, and where it is provided for

| Need | Provided by |
|---|---|
| **A new app** (Contacts, Notes) | one domain module with a `COMMANDS` slice, a store trait, a capability flag, a schema module, an interface store, tool wrappers. Nothing central changes. Types are generated. |
| **Passwords: matching a site without a clear column** | the server's in-memory search index, built at unlock, is the first answer. A keyed hash of the normalised site as a column is the second if a headless lookup is ever needed. Decide before the schema exists; the clear-column rule in the README does not apply to this domain. |
| **Passwords: a list that never carries a secret** | list commands return metadata; a separate `reveal` command, marked `sensitive`, returns one secret and writes an audit row. Design the domain's command set this way from its first draft. |
| **Passwords: the assistant** | the domain's class is `Secret`; `tools::available` never offers it. Not a setting. |
| **Passwords: step-up** | the auth context carries when the caller last proved the password; `sensitive` commands can require it to be recent. The protocol carries it from phase 1; the check is switched on with the app. |
| **Browser extension** | a native-messaging host that connects to the desktop app's local socket. The extension never holds a token, never sees the server, and works with a purely local vault. Its scope, when tokens are ever issued to it, is `passwords:read` and `library:write` and nothing else. Quick-save-an-article is `add_item` on the Articles shelf, the same command the capture line calls. |
| **Phone shell** | a Tauri mobile shell that is always a remote client. Argon2, the index and the feeds stay on the server. The pairing QR is for this. |
| **CLI against a running app** | the local socket, phase 2. |
| **Third-party clients** | the generated schema is the contract; the protocol version is the promise. |

## Order and parallelism

```
6a ─────────────────────────────────────────────────┐
0 ──▶ 1 ──▶ 2 ──▶ 3 ──▶ 4 ──▶ 5                     ├──▶ 6b ──▶ 6c
```

Phase 0 is a store-layer change with the conformance suite as its proof.
Phase 1 is one pull request with no behaviour change and is reviewed on that
basis; the generated types may be a second PR. Phases 2 and 3 are best
reviewed together, since 3 is what tests 2 end to end. 6a can go first on its
own.

## What happened when a fifth app landed on top

This is the section worth reading if you are wondering whether the seams above
are real. While the branch was in review, `main` grew Roles, Goals, Purpose and
a fifth app -- about ten thousand lines, touching every layer. Integrating it
took the following, and nothing else:

| To get | Cost |
|---|---|
| Server mode, for every new record | one module, `domains/purpose.rs`, and a line in `domains/mod.rs` |
| A scope that can be withheld on its own | one variant, `Scope::Purpose` |
| Live refresh | two variants, `Kind::Role` and `Kind::Goal`, and two rows in the interface's router |
| The typed client, for thirteen new calls | `make fix`, which regenerates it |
| Quick actions in the tray, the palette and the app bar | main's four `tray.register` entries became four rows in the one action table |
| The assistant's new tools | nothing: `Domain::Goals` declares its own sensitivity and `available` does the rest |

Two things did *not* fall out and had to be thought about.

**Trackers moved out of journals in the same change**, so `log_reading` and
`delete_tracker` changed shape. That is a *wire* change, which is exactly what
`surface.json` exists to make visible: the snapshot test failed, the diff
showed `journalId` going from required to optional and `delete_tracker` losing
an argument, and the generated client changed to match in one command.

**A purpose is a pointer that inherits**, so filing one task under a goal
changes every chart the Overview draws -- while the change event says only
"task". Reloading the Overview for every task edit would redraw a year of
balance on every autosave, so `PURPOSE_BEARING` in the interface's router
reloads it for those kinds *only while it is the app on screen*. A chart nobody
is looking at does not need to be right yet, and the view re-reads when it is
opened.

## What was done differently

Four places where the built thing is not what this document said, each for a
stated reason rather than by omission.

**1. The wire is declared, not derived.** The plan called for JSON Schema on
every argument and result type, generated from serde. That means a
`JsonSchema` derive on roughly forty types in `everyday-core`, which is a
large dependency in a crate that is deliberately small and has no derive
macros in it today.

What was built instead: each command declares its argument names and their
TypeScript types beside the Rust struct they mirror, `surface.json` is written
from that table by a test and committed, and `ui/scripts/gen-api.mjs`
generates the typed client from it. `make lint` fails if either is stale, and
`tests/surface.rs` fails if the table changes without somebody looking. The
record *types* -- `Entry`, `Task`, `Item` -- stay hand-written in `types.ts`.

That covers the drift that actually bites, which is in the *call*: a renamed
argument, a removed command, a changed scope. It does not catch a record type
that gained a field on one side only. Full derivation is still the right end
state; it is a change to the core crate and belongs in its own pull request.

**2. Tools still have their own bodies.** The plan said a tool whose body
duplicates a command's should be rewritten to call the command. It cannot, as
things stand: the tool catalogue is in `everyday-core` and the commands are in
`everyday-service`, which depends on it. Making tools call commands means
moving 1,200 lines of catalogue up a layer.

What was built instead: `list_tools` and `run_tool` on the command surface, so
a palette, a script or the CLI runs the same verb through the same
confirmation rule. The duplication is real and pre-existing; the *security*
half of that phase -- a secret domain never being offered to a model -- was
worth doing now and was done in the core, where it belongs.

**3. A tool's title is derived from its name.** The plan said add a human
`title` to each of the thirty-four entries. Every tool in this application is
named `verb_noun`, which is one underscore away from a sentence, so
`title_of` derives it. A second string on thirty-four entries would be
thirty-four more places for the two to disagree, and a tool whose derived
title reads badly is a tool whose *name* reads badly.

**4. The palette raises the window.** Phase 6c's follow-up -- a palette in a
window of its own, over whatever is in front, without taking focus -- is not
built. It needs its own connection to the vault, its own copy of the action
table and its own answer to what "the open shelf" means, because a second
webview shares no memory with the first. That is a feature rather than a
detail, and the in-window palette should settle what belongs in it first.

## Still to do

Nothing in the phases above. What the review turned up and this work
deliberately left for the app that needs it:

- **A blind index for passwords.** Matching a stored login by site needs
  either the server's in-memory search index, which works for a running
  server, or a keyed hash of the normalised site as a column. Decide before
  that schema exists; see the table above.
- **Step-up authentication.** `Ctx::proved_at` and `Command::sensitive` are on
  the wire and nothing consults them. The check is one function, switched on
  with the first sensitive command. It matters slightly more than it did:
  `set_agent_key` and `save_agent_settings` can be overwritten by any paired
  device, which means pointing the assistant at somebody else's endpoint.
- **The browser-extension host.** The local socket it talks to is built and
  tested. The native-messaging host process is not — and neither, still, is
  the desktop app's own end of that socket: `serve_socket` is called by
  `everyday serve` and by nothing else, so a local process cannot reach a
  vault held by the window. The assistant's routines did not need it (the
  scheduler runs inside the same service), so it is still open.
- **A resumable upload.** `POST /v1/blob` takes one body. The blob format is
  already chunked at 256 KiB, so a chunked form fits it naturally; a large
  video over a phone connection is what will ask for it.
- **The call audit.** Phase 3 said to count calls in a typical minute before
  shipping. The obvious two were done -- `pollAutoLock` is replaced by the
  `lockState` event for a remote client, and change events are coalesced --
  but the measurement was not, so `touch` is still per-interaction.

## Risks, and what to do about them

- **The size of phase 1.** About 1,700 lines move. Mechanical, but every command is touched. Move by domain in a fixed order, keep the bodies byte-identical, and let the new `call` test plus a manual walk of each app be the proof.
- **Certificate pinning through reqwest.** Spike before phase 2 is designed around it.
- **Two runtimes.** The server must run on Tauri's own tokio runtime or the blocking pool doubles and the vault's write serialisation is not shared.
- **The read pool and SQLite.** WAL mode is already set. A reader pool must not defeat `synchronous = FULL` on the writer, and `PRAGMA` settings are per connection, so the pool applies them on open. The conformance suite is the check.
- **Codegen drift.** The generated types are committed and diffed in `make lint`, so a change to the wire is visible in review rather than found in a client.
- **A bearer token on a stolen laptop.** Keychain storage, thirty-day expiry, revocation from the server, and the server lock. State this trade in Settings.
- **Plaintext on the wire under TLS.** A different trust model from a hosted Postgres vault, which sees only ciphertext. The connect screen and the share switch both say so.
- **Cached plaintext in client memory.** The LRU is the one place a remote client holds vault content outside the webview. It is dropped on lock and on disconnect, and it never touches disk.
- **No offline mode.** Deliberate. The connect screen says it.
