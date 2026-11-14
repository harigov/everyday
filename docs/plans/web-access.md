# Web access

The interface in a browser on another of your devices, served by the
machine that holds the vault. Not a second interface and not a viewer:
the same `ui/` build the desktop window runs, talking HTTP instead of
Tauri's bridge.

## What it is for, and what it is not

A phone or a borrowed laptop on your tailnet opening your vault without
installing anything. The vault password is the login; there is no account,
no pairing and no token to manage. Unlocking from the browser is unlocking
the vault, locking from it locks the vault, and the vault locking anywhere
ends every browser session.

It is not reachable from the internet, by design and not by configuration.
The first version answers **only devices on this machine's tailnet**, over
a certificate Tailscale issues for this machine's tailnet name. There is no
setting that widens that to "every interface".

## The four parts, and which of them grow

```text
crates/everyday-server/src/web/
  mod.rs        the switch: Config (web.json), start, status, the supervisor
  network.rs    WHO may connect      -- Network::Tailscale today
  cert.rs       WHICH certificate    -- Certificate::Tailscale today
  tailscale.rs  tailscaled's local API, for both of the above
  session.rs    password -> session cookie + media token
  assets.rs     the built interface: Tauri's embedded copy, or a folder
  routes.rs     /web/* and the gatekeeper that runs before every route
```

`Network` and `Certificate` are tagged enums in `web.json`
(`{"kind": "tailscale"}`), so a WireGuard interface or a self-generated
certificate is a new variant with its own match arms, not a rewrite:

- `Network::WireGuard { interface }` would bind that interface's addresses
  and admit peers inside its subnet. No who-is, so no per-user check.
- `Certificate::SelfSigned` would reuse `tls::Identity` (already used for
  sharing) or grow it into a small local CA; the browser would have to be
  told to trust it once.

Everything else -- sessions, the gatekeeper, the routes, the interface --
does not know which network or certificate it is running under.

## Who gets in

Every request, static files included, goes through the gatekeeper before
any route sees it:

1. **The listener** binds only this machine's Tailscale addresses (from
   `tailscaled`'s status), never `0.0.0.0` and never loopback. Funnel and
   `tailscale serve` proxy to loopback, so they cannot reach it. Each address
   is bound on its own, so an IPv6 address that cannot be bound does not
   keep IPv4 from answering.
2. **The peer address** must be in `100.64.0.0/10` or
   `fd7a:115c:a1e0::/48`.
3. **Who-is**: tailscaled must know the address as a node on this tailnet,
   and (default `peers: "sameUser"`) that node must belong to the same
   Tailscale user as this machine. A device shared in from someone else's
   tailnet is refused even if they know the password. `peers: {"logins":
   [...]}` names the accounts instead. A tagged device belongs to nobody
   and is always refused; a tagged *host* has no owner to compare with, so
   it needs `logins`. Verdicts are cached for a minute; tailscaled failing
   to answer is not cached, so a restart of it is not a minute of 403s.
4. **No proxy headers** (`Forwarded`, `X-Forwarded-For`, `X-Real-IP`,
   `Via`). Nothing here is ever meant to sit behind a proxy, so one present
   means something is forwarding traffic it should not.
5. **Origin**: a write carrying an `Origin` that is not this server's is
   refused.

Then the password, then a session.

## Sessions

`POST /web/login {password}`:

- refused for a vault with no password (`encrypted: false`): it would have
  no gate at all;
- runs through the same one-at-a-time Argon2 gate and lockout as a paired
  device's `unlock` (`Registry::unlock_gate`), keyed by the peer's address;
- `unlock`s a locked vault (unless `allowUnlock` is off), and otherwise
  `verify_password`s an open one -- the password is checked every time,
  never skipped because the desktop already has the vault open.

A success sets `__Host-everyday` (HttpOnly, Secure, SameSite=Strict,
Path=/) and returns a **media token**. Sessions are bound to the peer
address and to the vault whose password was typed: they end after an hour
unused or twelve hours in all (swept every minute, not merely refused at
the next lookup), all of them when the vault locks (`WebSink` watches
`lock_state`), and all of them when a different vault is opened underneath
a running server. Their event streams end with them, after passing on
whatever was already said -- `lockState` included.

Every tab in a browser shares the one session. A tab sends its own id
(`X-Everyday-Tab`), and writes are stamped `<session>.<tab>`, so the other
tabs' streams still hear about them.

No HSTS: it would bind the machine's tailnet name on every port, breaking
any plain-HTTP service on that name for a year, and there is no plain-HTTP
listener here for it to protect.

The cookie carries every API call. Calls also carry `X-Everyday-Protocol`,
which a cross-site page cannot add without a preflight nothing here
answers, so SameSite plus that header is the CSRF defence.

The media token exists for one reason: a mail body is drawn in a sandboxed
`srcdoc` frame with an opaque origin, and a browser will not send a
SameSite cookie for the pictures inside it. So every picture, attachment
and body address is `/m/<media token>/v1/...`; the gatekeeper moves the
token out of the path and allows it only on `GET` of the read-only media
routes (`/v1/blob`, `/v1/mail`, `/v1/news`). The token is in the page's
memory, never in storage, and dies with the session.

## The interface

`api.ts` now has three backends: Tauri, the mock, and `web.ts`. Which one
runs is decided at load: a Tauri webview has `__TAURI_INTERNALS__`, a dev
server without it is the mock, and anything else is web.

- Service commands go to `/v1/call/{name}`; a turn streams from
  `/v1/stream/send_message`; changes and lock state arrive on
  `/web/events` (read with `fetch`, because `EventSource` cannot send the
  protocol header).
- `unlock` and `verify_password` are the login; locking this screen
  (`Ctrl+L`, the idle timer) also ends this browser's session.
- What only the desktop shell can do is answered locally: no tray, badge,
  hotkey, app windows, meeting capture, screen-time collectors, sharing or
  MCP panels. Files go through the browser's own upload and download, which
  `transfer.svelte.ts` and `mail-api.ts` already knew how to do.
- Known gap: adding an OAuth account (Google, Microsoft) from a browser
  does not work -- the sign-in's redirect goes to a loopback address on the
  machine holding the vault. Add accounts from the desktop.

## Where it runs

- **Desktop:** Settings -> Vault -> "Open this vault in a browser". Same
  shape as sharing and MCP: a switch remembered in `web.json`, resumed at
  startup, its sink folded into the event fan-out. The first look at
  Tailscale (and the first certificate, which can be an ACME round trip)
  happens on the supervisor, so it never holds the window's startup. Assets come from Tauri's
  embedded copy of the interface.
- **`everyday serve --web`**: opt-in per run, as `--mcp` is; assets from
  `--web-ui DIR` (default: the repo's `ui/dist`).

## Setting it up

Once per tailnet: MagicDNS and HTTPS certificates on in the Tailscale admin
console. Once per machine: `sudo tailscale set --operator=$USER`, so the app
may ask tailscaled for a certificate without root. Then the URL is
`https://<machine>.<tailnet>.ts.net:7400/` (7400 by default: sharing has 7397, MCP
7398 and the screen-time collectors' loopback listener 7399).
