# MCP on the wire, as of revision 2026-07-28

Reference notes for implementing `docs/plans/mcp.md`, gathered from
`modelcontextprotocol.io/specification/2026-07-28` on 10 September 2026.
Everything here is quoted or paraphrased from the specification; where this
file and the specification disagree, the specification wins.

**Read this before writing any protocol code.** The revision current at the
time of writing is *not* the one most tutorials, blog posts and SDK examples
describe, and the differences are not cosmetic.

## The headline: there are two eras

| Era        | Revisions                  | Shape                                                    |
| ---------- | -------------------------- | -------------------------------------------------------- |
| **Modern** | `2026-07-28` and later     | Stateless. No handshake. Per-request `_meta`.            |
| **Legacy** | `2025-11-25` and earlier   | `initialize` handshake, connection-scoped session.       |

Revision `2026-07-28` **removed**, from the Streamable HTTP transport:

- the `initialize` / `notifications/initialized` handshake,
- protocol-level sessions and the `Mcp-Session-Id` header,
- the standalone `GET` stream endpoint,
- resumable streams via `Last-Event-ID`,
- the server's ability to send JSON-RPC *requests* to the client.

We implement **both eras** (the specification calls this "dual-era"). Modern
because it is the specification; legacy because the revision is six weeks old
and the clients on this desk today almost certainly still speak `2025-11-25`
or `2025-06-18`. A dual-era server selects its behaviour from how the client
opens:

- a request carrying modern per-request `_meta` is served statelessly under
  `2026-07-28`;
- an `initialize` request selects legacy semantics for that session.

`SUPPORTED_VERSIONS = ["2026-07-28", "2025-11-25", "2025-06-18"]`.

## Modern (2026-07-28)

### Statelessness

> Servers **MUST NOT** rely on prior requests over the same connection to
> establish context (e.g., capabilities, protocol version, client identity).
> Every request supplies this metadata in its `_meta` field.

There is no session object to write. An open connection is not a conversation.

### Required `_meta` on every request

| Key                                          | Type                 | Required |
| -------------------------------------------- | -------------------- | -------- |
| `io.modelcontextprotocol/protocolVersion`    | `string`             | **Yes**  |
| `io.modelcontextprotocol/clientInfo`         | `Implementation`     | No       |
| `io.modelcontextprotocol/clientCapabilities` | `ClientCapabilities` | **Yes**  |
| `io.modelcontextprotocol/logLevel`           | `LoggingLevel`       | No       |

A request missing a required field is malformed: reject with `-32602`
(Invalid params), HTTP `400`.

Results **SHOULD** carry `_meta["io.modelcontextprotocol/serverInfo"] =
{ name, version }`.

### Every result carries `resultType`

> The `result` **MUST** include a `resultType` field.

`"complete"` for everything we do. (`"input_required"` is the MRTR path, which
we do not implement — we never ask the client for input.) Clients **MUST**
treat an absent `resultType` as `"complete"`, which is how legacy results stay
valid.

### Version negotiation is per request, not a handshake

No negotiation step. Every request declares its version; the server accepts or
rejects it independently. If the version is unsupported:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "error": {
    "code": -32022,
    "message": "Unsupported protocol version",
    "data": {
      "supported": ["2026-07-28", "2025-11-25"],
      "requested": "1900-01-01"
    }
  }
}
```

HTTP status `400`.

### `server/discover` is mandatory

> Servers **MUST** implement `server/discover`.

Request params carry only `_meta`. Result:

```json
{
  "jsonrpc": "2.0",
  "id": "discover-1",
  "result": {
    "resultType": "complete",
    "supportedVersions": ["2026-07-28"],
    "capabilities": { "tools": {} },
    "_meta": {
      "io.modelcontextprotocol/serverInfo": { "name": "Every Day", "version": "0.1.0" }
    },
    "instructions": "...",
    "ttlMs": 3600000,
    "cacheScope": "public"
  }
}
```

`instructions` is optional natural-language guidance for the calling model. It
is worth writing well: it is where we say that ids come from a search tool,
that this is somebody's private journal, and that a locked vault offers
nothing.

### Error codes

MCP reserves `-32020`..`-32099`. Do not invent codes in that range.

| Code     | Name                              | Used for                                        |
| -------- | --------------------------------- | ------------------------------------------------ |
| `-32020` | `HeaderMismatch`                  | header does not match body, or missing/malformed |
| `-32021` | `MissingRequiredClientCapability` | `data.requiredCapabilities` lists what is missing |
| `-32022` | `UnsupportedProtocolVersion`      | `data.supported`, `data.requested`               |

Plus the standard JSON-RPC codes (`-32700`, `-32600`..`-32603`). `-32601` is
method-not-found; on HTTP it **MUST** be returned with status `404`, not 200.

## Streamable HTTP binding (modern)

### Endpoint and methods

One path, POST only. `GET` or `DELETE` to it: respond `405 Method Not Allowed`.
An `Mcp-Session-Id` header on a request: ignore it, and do not mint or echo
one. A `Last-Event-ID` header: ignore it.

### Required request headers

| Header                 | Mirrors                       | Required for                             |
| ---------------------- | ----------------------------- | ---------------------------------------- |
| `MCP-Protocol-Version` | `_meta` protocol version      | every POST                               |
| `Mcp-Method`           | `method`                      | every request                            |
| `Mcp-Name`             | `params.name` or `params.uri` | `tools/call`, `resources/read`, `prompts/get` |

The server **MUST** validate that each header matches the body, and reject a
mismatch — or a missing/malformed required header — with HTTP `400` and
`-32020`. `Mcp-Name` may arrive Base64-wrapped as `=?base64?<b64>?=` and must
be decoded before comparison. Header *names* compare case-insensitively;
header *values* are case-sensitive.

The client **MUST** send `Accept: application/json, text/event-stream`.

### Responses

- Body is a JSON-RPC **request** → answer with `application/json` (a single
  JSON object) **or** `text/event-stream` (an SSE stream scoped to that
  request, carrying related notifications then the final response).
- Body is a JSON-RPC **notification** → `202 Accepted`, no body, if accepted;
  an HTTP error status otherwise.
- Clients never send JSON-RPC responses; servers never send JSON-RPC requests.

For our tools, plain `application/json` is the right answer everywhere except
`subscriptions/listen`.

### Origin, and binding

> Servers **MUST** validate the `Origin` header on all incoming connections to
> prevent DNS rebinding attacks. If the `Origin` header is present and invalid,
> servers **MUST** respond with HTTP 403 Forbidden.
>
> When running locally, servers **SHOULD** bind only to localhost (127.0.0.1)
> rather than all network interfaces (0.0.0.0).

Note the exact condition: *present and invalid*. A request with no `Origin` at
all is not refused by this rule — a native client sends none.

### Cancellation

Closing the SSE response stream **is** the cancellation signal; there is no
`notifications/cancelled` on HTTP. The server should stop work and send nothing
further for that request.

### Long-lived notifications: `subscriptions/listen`

The GET stream is gone. A client that wants `notifications/tools/list_changed`
POSTs a `subscriptions/listen` request whose *response* is a long-lived SSE
stream. First event is `notifications/subscriptions/acknowledged`. Every
notification on that stream **MUST** carry
`_meta["io.modelcontextprotocol/subscriptionId"]` correlating it to the
originating request.

Include `X-Accel-Buffering: no` on SSE responses, and emit a `:\r\n` comment
line periodically as a keep-alive.

## Tools

### Capability

```json
{ "capabilities": { "tools": { "listChanged": true } } }
```

### `tools/list`

Supports pagination (`cursor` in, `nextCursor` out) and caching (`ttlMs`,
`cacheScope`). We have thirty-odd tools and return them in one page — but the
result still needs `resultType`.

> Servers **SHOULD** return tools in a deterministic order.

Our catalogue is a static slice, so this is free — do not sort into a
`HashMap` anywhere along the way.

Crucially, and this is exactly our locked-vault and scopes case:

> This set **MAY** be empty and **MAY** change over time, but **MUST NOT** vary
> per-connection or as a side effect of other requests on the connection. The
> set **MAY** vary by the authorization presented on the request — for example,
> returning only the tools the caller's granted scopes permit — since
> credentials are per-request input, not connection state.

### Tool object

`name`, optional `title`, `description`, `inputSchema` (JSON Schema, defaults
to 2020-12, **MUST** be a valid schema object and not `null`), optional
`outputSchema`, optional `annotations`, optional `icons`.

For a tool with no arguments, prefer
`{ "type": "object", "additionalProperties": false }`.

Names: 1–128 chars, `[A-Za-z0-9_.-]` only. Every tool in
`everyday-core/src/agent/tools.rs` already complies.

### `tools/call` result

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "resultType": "complete",
    "content": [{ "type": "text", "text": "..." }],
    "isError": false
  }
}
```

`structuredContent` may carry any JSON value. A tool returning structured
content **SHOULD** also put the serialized JSON in a text block, for clients
that ignore it. That is what we do: the `serde_json::Value` a tool returns goes
into `structuredContent`, and its pretty-printed form into `content[0].text`.

### Two kinds of error, and the distinction matters

- **Protocol errors** — unknown tool, malformed request, server fault — are
  JSON-RPC errors. `-32602` for an unknown tool name.
- **Tool execution errors** — validation failures, business-logic refusals —
  are *results* with `"isError": true` and the message as text content.

> Clients **SHOULD** provide tool execution errors to language models to enable
> self-correction.

This is precisely what the `Args` accessors in `tools.rs` were written for, and
what `CommandError` carries. Map by code: `unknown_tool` → `-32602`; everything
else (`invalid`, `locked`, `unsupported`, `confirm_required`, `not_found`,
`conflict`, …) → `isError: true` with the message.

### `notifications/tools/list_changed`

```json
{ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" }
```

Sent only to clients that opened a `subscriptions/listen` stream with
`toolsListChanged: true` (modern), or on the GET stream (legacy).

## Legacy (2025-11-25 / 2025-06-18) — what we also answer

Enough to serve today's clients:

- `initialize` → result with `protocolVersion` (echo the client's if we support
  it, else our newest legacy version), `capabilities: { tools: { listChanged:
  true } }`, `serverInfo: { name, version }`. **No `resultType`** — that field
  does not exist in this era.
- `notifications/initialized` → `202 Accepted`, no body.
- `Mcp-Session-Id`: minted on the initialize response, required on subsequent
  requests, `DELETE` terminates it.
- `GET` on the endpoint opens the standalone SSE stream that carries
  `notifications/tools/list_changed`.
- `tools/list` and `tools/call` as above but without `resultType`.

A dual-era server **MAY** serve both eras concurrently on the same endpoint,
which is what we do: the presence of modern `_meta` (or an `initialize` method)
decides which envelope the answer takes.

Per the specification, a modern-only server **SHOULD** name its supported
versions in any error it returns to an `initialize` request, because legacy
clients have no way to fall forward. We answer `initialize` properly instead,
so this does not arise — but keep the courtesy in mind if legacy support is
ever dropped.

## stdio binding

Newline-delimited JSON-RPC over the standard streams of a client-launched
subprocess. Nothing but protocol on stdout, ever. Modern clients probe with
`server/discover` and fall back to `initialize` on any error that is not a
recognized modern error.

We do not implement this binding directly: `everyday mcp` is a pipe to the HTTP
endpoint (see `docs/plans/mcp.md`, phase 3), so both eras arrive over HTTP and
are answered in one place.

## What we do not implement

`resources/*`, `prompts/*`, `completion/*`, `logging/*`, sampling, elicitation,
roots, MRTR / `InputRequiredResult`, tasks, pagination beyond a single page,
and `x-mcp-header` annotations on our own tools (we set none; the client-side
requirement to honour them is not ours). Unknown methods get `-32601` with HTTP
`404`.
