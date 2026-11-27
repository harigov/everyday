# Architecture review, November 2026

A second pass over the whole tree, a year of features after the first
([`architecture-refactor.md`](architecture-refactor.md)). Nine read-only
reviews -- core storage, core domains, service runtime, mail and calendar,
the service's media and speech, the desktop and phone shell, the network
surfaces, the interface's data layer and its components -- each looked for
duplication, unclear boundaries, naming, missing interfaces, security gaps
and what stands in the way of more platforms. This page is what they found,
what was done about it, and what was left, and why.

## The rule

The same as last time: **every change is behaviour-preserving**. Nothing
written to a vault, sent over the wire, shown to a model or seen by a person
changes. No command, tool, route, header, serde name, AEAD label, key
derivation label or stored string changes. A refactor that needs one of
those to change has stopped being a refactor.

The exceptions are named here, each a security or robustness fix found on
the way, each small, each with a test:

- `llm::is_loopback` parses an endpoint the way the HTTP client will, so an
  address like `http://evil.example\@127.0.0.1/` is no longer taken for this
  computer (and an API key no longer sent to it in the clear).
- `websearch::is_invisible` strips every character `hidden::is_hidden` does,
  so the two lists of invisible characters cannot drift apart again.
- The calendar's token refresh honours a rotated refresh token that mail
  could not save yet, as mail's own refresh already does, instead of
  presenting a retired one and signing the whole account out. A pending
  token is cleared only while it is still the one saved, a pending token the
  provider refuses is dropped, and a fresh sign-in that brings its own
  refresh token supersedes it.
- Generated pictures are written to a scratch folder only their owner can
  read, created fresh rather than adopted if something already has the name.
- `VaultConfig` and `BackendSettings` print their password and database URL
  redacted when formatted for debugging.

## What was found, by area

| Area | The main findings |
|---|---|
| Core storage | 55 identical `RecordDescriptor` impls; five copies of the secrets wrapper; 69 `pick_domain(..)` calls; 14 hand-written singleton rows in SQL; six sites reaching into a raw key for one keyed hash; key-derivation labels not all pinned by a test; two `Debug` derives that would print a password |
| Core domains | 28 copies of "this zone name, or UTC"; five HTML strippers, three entity decoders, two escapers; six identical whitespace flatteners; percent-encoding living in `websearch`; `agent.rs` mixing records with prompt building; a hand-parsed loopback check |
| Service runtime | `agent.rs` at 15k lines; the public-only network guard copied in up to ten places; seven `reqwest` builders and five spellings of the user agent; 73 flat modules |
| Mail and calendar | ten flat `mail*` modules; `mailai.rs` holding three features; two copies of the judge picker; calendar token refresh drifted from mail's; a trait and an enum both called `CalendarProvider` |
| Media and speech | the network guard again, and the `Route` enum copied between two podcast modules; generated pictures in a world-readable folder |
| Desktop and phone shell | platform code inline in feature modules (201 `target_os` sites) where `desk/` and `jatayu-focus` already show a per-OS layout; three copies of the floating-window builder; the main window's label spelled out 41 times |
| Network surfaces | the handshake types written twice, once for each side; route paths and headers as string literals on both sides; four bearer-token extractors with different leniency; three SSE readers; `relay.rs` at 4k lines |
| Interface data | platform sniffing in 22 places with three different meanings of "Mac"; 145 mock commands listed twice; a few utility copies |
| Interface components | 34 hand-rolled modal skeletons; a 24-way `{#if}` for Settings pages; the Health sidebar carrying its own copy of the symbol renderer; three copies of a secondary window's bootstrap |

## What was done

Every item below is a move, a rename in code, or one copy kept where several
were; the record schema, the command and tool surfaces and every golden file
are unchanged apart from doc comments naming moved modules.

**Service**
- `mail/`: the eleven flat modules (`mailai`, `mailsync`, `outbox`, …) are one
  tree with a table in `mail/mod.rs`; `mail/ai` is split into its three
  passes (`categorize`, `summary`, `autodraft`); `address`, `text` and
  `judge` hold what several passes shared.
- `agent.rs` (15,312 lines) is a 407-line hub over `agent/{confirm, gate,
  trust, prompt, own_tools/, turn, notes, replay, stream, summary, tests}`.
- `http.rs` is the one public-only guard (`PublicOnly`,
  `public_redirect_policy`, `literal_ip`, `names_forbidden_literal`) and the
  one `USER_AGENT`/`BROWSER_USER_AGENT`; the MCP connections, the Feed, the
  podcast downloader and transcriber, page reading and the browser's proxy
  call it. `news/route.rs` is the one copy of the podcasts' `Route`.
- `accountcal::CalendarProtocol` (was a second `CalendarProvider`).
- Modules nothing outside the crate uses are `pub(crate)`.
- Key-derivation labels for mail's storage, the documents cache and the chat
  store are pinned by tests.

**Core**
- `zone`, `text`, `markup`, `urlenc` and `hex`: one copy each of what was
  written out across the domain modules.
- `agent.rs` keeps the settings and tools; `agent/records.rs` and
  `agent/prompt.rs` hold the records and the prompt builders.
- `record.rs`: one `record_descriptors!` table instead of 55 identical impls.
- `vault/`: one `with_secrets`/`with_agent`; typed accessors on `Unlocked`
  (`u.mail()?`, `u.money()?`, `u.subkey(..)`); `SecretKey::keyed_id` keeps the
  raw key inside `crypto.rs`; `vault/files.rs` is `vault/files/`.
- `store-sql`: `get_singleton`/`put_singleton` for the thirteen one-row
  tables, with byte-identical SQL.
- `VaultConfig` and `BackendSettings` print their secrets redacted.

**Network surfaces**
- `jatayu_server::wire`: `Hello`, `PairRequest`, `Paired` and
  `ServerEvent` once for the router and the client, with the `/v1` paths and
  header names as constants -- the interface the phone apps depend on.
- `auth::bearer(headers, Leniency)`: one reader of `Authorization`, each route
  keeping its own strictness; `Registry::authenticate_for` for the
  scope-named routes.
- `jatayu-chat`'s `relay.rs` is `relay/`; `routes.rs`' tests moved out.

**Desktop and phone shell**
- One `MAIN_WINDOW` label for both shells; Linux's notification and
  microphone-watch code in `notice/linux.rs` and `micwatch/linux.rs`; one
  `overlay::popup_builder` for the three floating windows; `run()`'s setup
  and window events as named functions.

**Interface**
- `lib/platform.ts`, `lib/mock-registry.ts` (with a coverage test),
  `components/Sheet.svelte` (five dialogs), `lib/settings-pages.ts`,
  `lib/window-boot.ts`, the Health sidebar on the shared symbols, and a few
  utility copies merged. Every changed surface was compared before and after
  in a headless browser: the markup is identical but for Svelte's comment
  anchors, attribute order and two empty `<g>` groups no longer drawn.

## What was left, and why

- **macOS, Windows and iOS bodies** were not moved into per-platform
  modules: nothing here compiles them and CI builds Linux only, so a broken
  relative path would ship. `badge.rs`, `hotkey.rs`, `writing/` and the
  `device_name` trio are the candidates, for a session with a Mac or a
  cross-compiling check.
- **Trimming the phone build's dependencies** (`jatayu-vault`'s backends
  and keychain on Android/iOS): a Cargo feature change that wants a phone
  build to prove it.
- **The three SSE readers** stay apart: the client's reads each `data:` line
  alone, and swapping in the full parser would change how a malformed stream
  is read.
- **The judge pickers** in `mail/check.rs` and `mail/followup.rs` stay two:
  their gates, fallbacks and wording differ.
- **Status-code wording** in maps, weather and search, the differing
  truncators, byte formatters and duration formatters: each copy's output is
  seen by a person or a model, and they differ on purpose or by accident in
  ways a merge would change.
- **`Service`'s session state** was not regrouped: `locked()` and `close()`
  clear different things on purpose, and the order matters.
- **Large components** (`MailCompose`, `OverviewWidget`, `ModelsPanel`) and
  the 34 hand-rolled modal skeletons beyond the five moved to `Sheet`: each
  wants screenshots per theme and phone, one at a time.
- **Grouping `components/` and `lib/` into folders per app**: thousands of
  import lines for no change in behaviour, and a merge conflict with every
  branch open at the time.

## Ownership while the work was in flight

Seven Rust refactors ran in their own worktrees (one shared target directory,
one build lock), two interface refactors in the branch's own. No two owned
the same file. The shared pieces were laid first, on the branch:

- the service's `mail/` module tree (a pure move of the eleven flat modules);
- `jatayu_service::http`'s `USER_AGENT`, `BROWSER_USER_AGENT`,
  `PublicOnly`, `public_redirect_policy`, `RedirectSchemes`, `literal_ip`
  and `names_forbidden_literal`;
- empty `jatayu_core::{hex, markup, text, urlenc, zone}` modules;
- `ui/src/lib/platform.ts`.

| Worker | Owns |
|---|---|
| net | `jatayu-service`: `http.rs`, `connections/`, `webpage.rs`, `news/{mod,fetch,downloads,transcribe,extract,directory}.rs`, `browser/`, `local/files.rs`, `money/plaid.rs`, `images/local.rs`; `jatayu-chat`: `relay.rs` |
| agentrs | `jatayu-service`: `agent.rs`, `agent/` |
| mail | `jatayu-service`: `mail/`, `accountcal/`, and the trait rename's call sites |
| corestore | `jatayu-core`: `record.rs`, `crypto.rs`, `hex.rs`, `blobstore.rs`, `store.rs`, `id.rs`, `vault/`, `tests/pinned_strings.rs`; `jatayu-store-sql`; `jatayu-vault` |
| coredomain | the rest of `jatayu-core/src` |
| server | `jatayu-server`, `jatayu-mcp`, `jatayu-connect/src/sse.rs`, `jatayu-cli/src/mcp_pipe.rs` |
| app | `jatayu-app`, `jatayu-focus` |
| ui-data | `ui/src/lib` (not the three files below), and the platform checks in the components that sniff |
| ui-components | `Sheet.svelte` and the dialogs moved onto it, `SettingsView.svelte`, `HealthNav.svelte`, `lib/symbols.ts`, `lib/settings-pages.ts`, `lib/window-boot.ts`, the secondary windows' `main.ts` |

## Platforms: the convention

Code that differs by operating system lives in a module of its own per
system, chosen once, rather than in `#[cfg]` arms through a feature's
logic:

```text
feature/
  mod.rs      the feature's logic, cfg-free, and one place that picks the
              platform's implementation
  linux.rs    one file per platform that has one
  macos.rs
  windows.rs
```

`jatayu-focus` (`Watcher`/`Backend`, one `detect()`) and the app's
`desk/{linux,macos}` already work this way. A platform with no
implementation gets a fallback in `mod.rs` that says so, rather than a
compile error. Code that only runs on one platform but is tested everywhere
keeps its `cfg(any(target_os = "...", test))`.

The phone apps are remote clients: everything they need from the computer
crosses `jatayu-server`'s `/v1` API, whose request and response types
now live once, in `jatayu_server::wire`, for the router and the client
both.

In the interface, what the page is running on is answered by
`ui/src/lib/platform.ts` and nowhere else; the phone's layout rule stays
`phone.svelte.ts` and `docs/mobile.md`.
