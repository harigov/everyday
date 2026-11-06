# Screen time: which app and which site had your attention, and for how long

> **Delivered** (5 Nov 2026). An eleventh app, Screen time, built by eight
> agents against a contract laid down first: the records, the store trait,
> the vault facade, schema v25 (v24 when laid down; main took v24 for mail
> first), every exhaustive enum's arm, the
> `everyday-focus` crate's API, the listener's `Forward` trait, the shell's
> commands, and the interface's types and store. Read this for the
> reasoning; read the code for what was done. What went differently from
> the plan below, each for a reason worth keeping:
>
> - **The GNOME extension was tried in a real GNOME Shell 50.1** (a private
>   headless one, its own bus and HOME): the right desktop ids, names and
>   titles; a switch ends one run where the next begins; counting stops a
>   minute after the last input and at the lock screen; disabling cleans up.
>   **The browser extension was tried in Chromium inside that session**:
>   sites by host, split at the tab switch. Headless Chromium and Xvfb both
>   report the screen locked, so neither counts there -- correctly.
> - **Retention clears titles, never time.** A tracker's readings are worked
>   out from spans, not stored, so deleting old spans would have rewritten
>   every old day as empty and every max target as met.
>   `ScreenTimeSettings::title_days` clears window titles older than that;
>   the sweep keeps a bookmark (`titles_cleared_to`, the vault's own, never
>   sent to the interface or exported) so each pass reads only what aged
>   past the cut-off since the last. No daily rollups were needed.
> - **A goal measures an app or site by its item key, not its label**
>   (`TrackerSource::ScreenTime { category, item }`): resetting or deleting a
>   label must not change what a goal counts. `TrackerSource` lost `Copy` to
>   carry the string. Deleting a category a tracker measures is refused,
>   naming the tracker.
> - **The loopback listener names the device itself, always**, whatever a
>   body says: a connection code or GNOME config written before a rename
>   would otherwise send the old name, and a browser's sites are reconciled
>   with its app only within one device.
> - **The gap between two items on one stream is credited to the first**
>   when it is under the 45-second pulse (`merge::bridge`): at a five-second
>   sample and a switch every twenty seconds that gap was a quarter of the
>   day.
> - **A span is never longer than twelve hours** -- the store refuses one --
>   so a range read looks back no further than that before its start and the
>   `start_us` index bounds both ends.
> - **Screen-time reads arm the fetch check** as mail and money reads do,
>   and an app's or site's name reaches the model under `untrusted_text`:
>   names arrive from a write-only heartbeats token, which can leak from a
>   browser profile. MCP needs `screentime` by name, as Money does; research
>   jobs are never offered the domain.
> - **The shell never flips GNOME's `disable-user-extensions`**: that would
>   turn every extension the person switched off back on. Status says so
>   instead.
> - **`everyday-focus` is its own crate**, small enough to cross-check for
>   Windows and macOS without a webview, and it was: clippy clean on
>   `x86_64-pc-windows-gnu` and `aarch64-apple-darwin`. The Windows, macOS,
>   X11, wlroots and KDE backends have never run on those desktops.
> - **The AT-SPI fallback for GNOME was not built.** Without the extension,
>   GNOME reports nothing, and Settings offers to install it.
>
> **Still open:**
>
> - The Windows, macOS, X11, wlroots and KDE watchers are compiled and
>   unit-tested, never run on a real desktop of each kind.
> - A day's hourly columns are not split by category: `ScreenReport.hours`
>   is totals only.
> - The browser extension queues in `storage.session`, so what was not yet
>   sent is lost when the browser quits (about thirty seconds while the app
>   is reachable).
> - A POST that landed but whose answer was lost is sent again; if the
>   stream has moved on, the vault records an overlapping span, which the
>   report's later-start-wins rule then settles.


A high-level plan, written 5 November 2026 against the tree at `c640fea`.
Nothing here blocks or limits anything. It records what was in front of you
and draws analytics from it.

## What this is

1. **One heartbeat format and one ingest endpoint.** Every collector sends
   the same small heartbeat. The server merges heartbeats into spans.
2. **Collectors**, each deciding *on its own* whether you are using what it
   watches:
   - a GNOME Shell extension,
   - an in-process collector in the app shell for every other desktop
     (Windows, macOS, X11, wlroots, KDE),
   - a browser extension for Chrome and Firefox, where each website counts as
     its own app.
3. **An analytics screen**, plus usage as a source for goal trackers.

## The rule that matters: focused, not idle, not locked

A collector sends a heartbeat only while all three hold:

- **Focused.** Only the frontmost window. In a browser, only the active tab
  of the focused browser window. A browser window that is open but not
  focused counts for nothing.
- **Not idle.** No input for N minutes (default 3) stops heartbeats. The one
  exception is a focused tab that is playing audio or video, which the
  browser extension can see.
- **Not locked.**

Each collector applies the rule itself, so the server never has to guess.
If a collector goes silent (crash, sleep, network), its span simply ends at
the last heartbeat. Nothing is counted that was not reported.

## Heartbeats and spans

A heartbeat, every ~5 s, batched:

```json
{ "device": "laptop", "source": "gnome", "app": "org.gnome.Ptyxis",
  "title": "…", "domain": null, "ts": "2026-11-05T09:14:05Z" }
```

- `source` is `gnome`, `desktop` (the in-process collector) or `browser`.
- `app` is the desktop app id or executable. For `browser` it is the browser
  and `domain` holds the site.
- The **site is the registrable domain** (`mail.google.com` → `google.com`),
  with a per-domain override so `docs.google.com` can be split from Gmail.
- Titles are optional and can be switched off per collector.

On the server, a heartbeat with the same key as the open span, arriving
within ~15 s of the last one, extends it. Otherwise it closes that span and
opens a new one.

**Storage** is a table of its own in the vault (device, source, app, domain,
start, end), like the mail index, not records, because there will be
thousands of rows a day. Daily rollups per app/domain/device feed the
analytics. How long raw spans are kept is a setting; rollups are kept.

## Ingest API

- `POST /api/usage/heartbeats`, which takes a batch.
- A new **`Usage` scope** in `everyday_service::ctx::Scope`, write-only. A
  usage token can add heartbeats and nothing else, so a stolen one can do no
  more than add fake usage.
- The same handler is served in two places:
  - **on loopback in the app** (plain HTTP on 127.0.0.1, beside MCP's
    listener). Both extensions post here by default,
  - **on the shared server**, over its pinned TLS, for machines where the app
    is not open or where the vault lives elsewhere.

## Collectors

### GNOME Shell extension (first)

GNOME on Wayland gives ordinary programs no way to learn the focused window.
`Shell.Introspect` is restricted to a hardcoded allowlist, `Eval` has been off
since GNOME 41, and Mutter implements no foreign-toplevel protocol. Code
running inside gnome-shell can see it, so we ship a small extension (~100
lines of JS):

- focus: `global.display` → `notify::focus-window`, read app id and title,
- idle: `global.backend.get_core_idle_monitor()`,
- lock: `Main.screenShield.locked` / `Main.sessionMode`,
- network: libsoup3, **async only**. Anything blocking here freezes the whole
  desktop. Keep a small in-memory queue with a cap and drop the oldest past
  it. When posting to the server, pin its certificate through Soup's
  `accept-certificate`.

The app installs and enables it with `gnome-extensions` and writes the
endpoint and a fresh `Usage` token into its settings, so there is no manual
pairing. On Wayland a new extension loads only at the next login, so the app
says so once.

### In-process desktop collector (the app shell)

The same heartbeats from inside `everyday-app`, one backend per platform:

| Platform          | Focused app                                    | Idle / lock                                   |
|-------------------|------------------------------------------------|-----------------------------------------------|
| Windows           | `GetForegroundWindow` → process exe            | `GetLastInputInfo`, session lock notifications |
| macOS             | `NSWorkspace.frontmostApplication` (titles need Accessibility) | `CGEventSourceSecondsSinceLastEventType`, screen-lock notifications |
| X11               | `_NET_ACTIVE_WINDOW`                           | XScreenSaver extension, logind `LockedHint`   |
| wlroots (Sway, Hyprland, river…) | `wlr-foreign-toplevel-management` via `wayland-client` | `ext-idle-notify`, logind `LockedHint` |
| KDE               | a KWin script loaded over D-Bus at runtime     | `ext-idle-notify`, logind `LockedHint`        |
| GNOME             | stands aside for the extension; AT-SPI as a partial fallback until it loads | n/a |

### Browser extension (one WebExtension codebase for Chrome and Firefox)

- Tracks `windows.onFocusChanged` (`WINDOW_ID_NONE` means the browser lost
  focus, so it stops), `tabs.onActivated` / `onUpdated`,
  `idle.onStateChanged`, and the tab's `audible` flag.
- MV3 service workers get suspended, so it queues in `storage` and flushes on
  an alarm.
- Pairs once by pasting a usage token from Settings.
- Posts to the app on loopback. On a machine without the app it posts to the
  server's tailnet HTTPS name, which has a real certificate, because an
  extension cannot pin a self-signed one.

## Not counting the browser twice

While a browser is focused, the desktop collector reports "Firefox" and the
extension reports "github.com" for the same seconds. When reading, browser
time is split by the extension's spans, and what is left over shows as
"Firefox (other)". Without the extension a browser is one app.

## Analytics

- Today / week: total, top apps and sites, an hourly timeline, per device and
  all devices together, compared with last week.
- Categories ("work", "social") are assigned per app or domain by the user.
- Usage feeds the existing derived trackers, so a goal like "under 1 h of
  social a day" works with no new goal machinery.
- Charts follow the house style.

## Order of work

1. **Span store + ingest endpoint + `Usage` scope**, with heartbeat-merge
   tests.
2. **GNOME extension** posting to loopback, installed and tokened by the app.
   It ends up on your own machine first.
3. **Analytics screen**, minimal: today, top apps, timeline.
4. **Browser extension** and the double-counting merge.
5. **In-process collectors**: Windows, macOS, X11, then wlroots and KDE.
6. Categories, tracker integration, retention settings.

## Deliberately not here

- Blocking, limits, nudges.
- Phones. Android's `UsageStatsManager` would fit the same endpoint later;
  iOS offers nothing usable.
- Screenshots or keystroke counts. Focus and idle state only.

## The contract (laid down before the build)

Code names: the app is **Screen time** on screen and `screentime` in code;
the UI section is `screentime`. Records: `ScreenSpan` (not a record kind --
written by the thousand, never edited), `ScreenApp` (a person's label for
an app or site) and `ScreenCategory`, ids `ScreenSpanId` / `ScreenAppId` /
`ScreenCategoryId`. Scopes: `screentime` (the app) and `heartbeats`
(write-only ingest). Change kinds: `screenApp`, `screenCategory`,
`screenSpan` (never with an id). Schema step **v25** (planned as v24; main took v24 first).

### Item keys

Everything names an app or site by one string: `app:<app id lower-case>`
or `site:<registrable domain>` (`site:<host>` when the domain's label has
`split`). Sites are stored as the full host and grouped at report time, so
a change of mind about splitting applies to every recorded day.
`everyday_core::screentime::item_key` is the only maker of a key.

### Storage (v25)

- `screen_spans (id, stream, start_us, end_us, data)`; indexes
  `(stream, end_us)`, `(end_us)`, `(start_us)`. `stream` is a keyed BLAKE3
  hash (label `everyday.screentime.stream.v1`) of `device \0 collector`, so
  a stream's last span is one indexed lookup and the column means nothing
  outside the vault.
- `screen_apps (id, created_us, updated_us, data)`,
  `screen_categories (id, sort_order, created_us, updated_us, data)`,
  `screen_settings (id = 1, data)`.

### Commands

| name | scope | effect | change | args | returns |
|---|---|---|---|---|---|
| `record_screen_time` | heartbeats | write | screenSpan/updated | `HeartbeatBatch` fields: `device`, `collector`, `beats` | `IngestReport` |
| `screen_time_collector` | heartbeats | read | | | `CollectorConfig` |
| `screen_time_report` | screentime | read | | `query: ScreenQuery` | `ScreenReport` |
| `screen_time_timeline` | screentime | read | | `date`, `tz`, `devices?` | `ScreenTimeline` |
| `list_screen_apps` | screentime | read | | | `ScreenAppRow[]` |
| `save_screen_app` | screentime | write | screenApp/updated | `app: ScreenApp` | void |
| `delete_screen_app` | screentime | destructive | screenApp/deleted | `id` | void |
| `list_screen_categories` | screentime | read | | | `ScreenCategory[]` |
| `save_screen_category` | screentime | write | screenCategory/updated | `category: ScreenCategory` | void |
| `delete_screen_category` | screentime | destructive | screenCategory/deleted | `id` | void |
| `list_screen_devices` | screentime | read | | | `ScreenDevice[]` |
| `screen_time_settings` | screentime | read | | | `ScreenTimeSettings` |
| `save_screen_time_settings` | screentime | write | settings/updated | `settings` | void |
| `forget_screen_item` | screentime | destructive | screenSpan/deleted | `key` | number |
| `delete_screen_time` | screentime | destructive | screenSpan/deleted | `from`, `to` (instants) | number |

`list_screen_categories` writes the starter categories on first call
(`ensure_screen_defaults`), as Money's does.

### The ingest routes (stable: extensions are not built from this workspace)

```text
GET  /v1/screentime/hello       Authorization: Bearer <heartbeats token>
  -> {"device": "laptop", "idleMinutes": 3, "keepTitles": true, "version": "0.1.0"}
POST /v1/screentime/heartbeats  Authorization: Bearer <heartbeats token>
  <- {"device"?: "laptop", "collector": "firefox",
      "beats": [{"kind": "site", "app": "Firefox", "host": "github.com",
                 "title": "…", "start": "2026-11-05T17:00:00Z",
                 "end": "2026-11-05T17:00:30Z"}]}
  -> {"accepted": 1, "rejected": 0, "spans": 1}
```

Served by the shell's loopback listener (`127.0.0.1:7399`, fills in
`device`) and by the sharing server's TLS router (must name `device`).
401 without a valid token, 403 without the scope, 400 for a bad body, 413
past 2 MB, 409 `locked` while the vault is locked.

### Connection code

What Settings → Screen time hands a browser extension, once:
`everyday-screentime:` followed by base64url (no padding) of
`{"endpoint": "http://127.0.0.1:7399", "token": "…", "device": "laptop"}`.

### The GNOME extension's config

`$XDG_CONFIG_HOME/everyday/screentime-gnome.json`, written `0600` by the
shell when it installs the extension:
`{"endpoint": "http://127.0.0.1:7399", "token": "…", "device": "laptop",
"fingerprint": ""}` -- `fingerprint` (SHA-256 of the server certificate,
hex) only when the endpoint is a sharing server's `https` address.
