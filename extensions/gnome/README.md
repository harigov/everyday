# Every Day Screen time for GNOME Shell

GNOME on Wayland tells no ordinary program which window has focus, so the
Every Day app cannot watch it from outside the way it does on Windows, macOS,
X11, wlroots or KDE. Code inside gnome-shell can see it, so on GNOME this
small extension is the collector: it watches the focused window, the idle
time and the lock screen, and posts runs of "this application, from then to
then" to the app every thirty seconds. See `docs/plans/screentime.md`.

## Installing

From the app: **Settings → Screen time → Install the GNOME extension**. The
app copies `screentime@everyday.journal/` (compiled into its binary) to
`~/.local/share/gnome-shell/extensions/`, mints a heartbeats-only token, writes
`~/.config/everyday/screentime-gnome.json` (mode 0600) and enables the
extension. On Wayland GNOME Shell only discovers a new extension at login, so
the first install needs one log out and back in; Settings says so.

By hand, for development:

```sh
cp -r extensions/gnome/screentime@everyday.journal ~/.local/share/gnome-shell/extensions/
gnome-extensions enable screentime@everyday.journal   # after the next login
```

The config is the app's to write -- installing from Settings once is the easy
way to get a token -- and looks like this:

```json
{
  "endpoint": "http://127.0.0.1:7399",
  "token": "…",
  "device": "laptop",
  "fingerprint": ""
}
```

`fingerprint` is the lower-case hex SHA-256 of the server certificate, only
for a sharing server's `https` address; empty uses ordinary TLS, or plain HTTP
to the app on loopback.

## What it does, and does not

- Counts only while a window has focus, input came within the vault's idle
  minutes (`GET /v1/screentime/hello`, default 3) and the screen is unlocked.
- Sends the application id (the desktop file id, else the window class), its
  name and the window title. The app drops titles if Settings says not to keep
  them.
- Never blocks the shell: async file reads, libsoup's async API, and a queue
  of at most 2000 heartbeats in memory while the app is unreachable.
- Draws nothing. It stays enabled on the lock screen (`unlock-dialog`) only so
  that a lock does not throw away the queue; while locked it counts nothing.

Logs go to the shell's journal: `journalctl --user -b /usr/bin/gnome-shell | grep "Every Day"`.
