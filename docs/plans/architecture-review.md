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
  presenting a retired one and signing the whole account out.
- Generated pictures are written to a scratch folder only their owner can
  read.

## What was found, by area

| Area | The main findings |
|---|---|
| Core storage | 55 identical `RecordDescriptor` impls; five copies of the secrets wrapper; 69 `pick_domain(..)` calls; 14 hand-written singleton rows in SQL; six sites reaching into a raw key for one keyed hash; key-derivation labels not all pinned by a test; two `Debug` derives that would print a password |
| Core domains | 28 copies of "this zone name, or UTC"; five HTML strippers, three entity decoders, two escapers; six identical whitespace flatteners; percent-encoding living in `websearch`; `agent.rs` mixing records with prompt building; a hand-parsed loopback check |
| Service runtime | `agent.rs` at 15k lines; the public-only network guard copied in up to ten places; seven `reqwest` builders and five spellings of the user agent; 73 flat modules |
| Mail and calendar | ten flat `mail*` modules; `mailai.rs` holding three features; two copies of the judge picker; calendar token refresh drifted from mail's; a trait and an enum both called `CalendarProvider` |
| Media and speech | the network guard again, and the `Route` enum copied between two podcast modules; generated pictures in a world-readable folder |
| Desktop and phone shell | platform code inline in feature modules (201 `target_os` sites) where `desk/` and `karmayoga-focus` already show a per-OS layout; three copies of the floating-window builder; the main window's label spelled out 41 times |
| Network surfaces | the handshake types written twice, once for each side; route paths and headers as string literals on both sides; four bearer-token extractors with different leniency; three SSE readers; `relay.rs` at 4k lines |
| Interface data | platform sniffing in 22 places with three different meanings of "Mac"; 145 mock commands listed twice; a few utility copies |
| Interface components | 34 hand-rolled modal skeletons; a 24-way `{#if}` for Settings pages; the Health sidebar carrying its own copy of the symbol renderer; three copies of a secondary window's bootstrap |

## Ownership while the work was in flight

Seven Rust refactors ran in their own worktrees (one shared target directory,
one build lock), two interface refactors in the branch's own. No two owned
the same file. The shared pieces were laid first, on the branch:

- the service's `mail/` module tree (a pure move of the eleven flat modules);
- `karmayoga_service::http`'s `USER_AGENT`, `BROWSER_USER_AGENT`,
  `PublicOnly`, `public_redirect_policy`, `RedirectSchemes`, `literal_ip`
  and `names_forbidden_literal`;
- empty `karmayoga_core::{hex, markup, text, urlenc, zone}` modules;
- `ui/src/lib/platform.ts`.

| Worker | Owns |
|---|---|
| net | `karmayoga-service`: `http.rs`, `connections/`, `webpage.rs`, `news/{mod,fetch,downloads,transcribe,extract,directory}.rs`, `browser/`, `local/files.rs`, `money/plaid.rs`, `images/local.rs`; `karmayoga-chat`: `relay.rs` |
| agentrs | `karmayoga-service`: `agent.rs`, `agent/` |
| mail | `karmayoga-service`: `mail/`, `accountcal/`, and the trait rename's call sites |
| corestore | `karmayoga-core`: `record.rs`, `crypto.rs`, `hex.rs`, `blobstore.rs`, `store.rs`, `id.rs`, `vault/`, `tests/pinned_strings.rs`; `karmayoga-store-sql`; `karmayoga-vault` |
| coredomain | the rest of `karmayoga-core/src` |
| server | `karmayoga-server`, `karmayoga-mcp`, `karmayoga-connect/src/sse.rs`, `karmayoga-cli/src/mcp_pipe.rs` |
| app | `karmayoga-app`, `karmayoga-focus` |
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

`karmayoga-focus` (`Watcher`/`Backend`, one `detect()`) and the app's
`desk/{linux,macos}` already work this way. A platform with no
implementation gets a fallback in `mod.rs` that says so, rather than a
compile error. Code that only runs on one platform but is tested everywhere
keeps its `cfg(any(target_os = "...", test))`.

The phone apps are remote clients: everything they need from the computer
crosses `karmayoga-server`'s `/v1` API, whose request and response types
now live once, in `karmayoga_server::wire`, for the router and the client
both.

In the interface, what the page is running on is answered by
`ui/src/lib/platform.ts` and nowhere else; the phone's layout rule stays
`phone.svelte.ts` and `docs/mobile.md`.
