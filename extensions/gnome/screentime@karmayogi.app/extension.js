// Karma Yoga Screen time: which application had your attention, reported to
// the Karma Yoga app as heartbeats.
//
// GNOME on Wayland tells no ordinary program which window has focus, so the
// app cannot watch it from outside; code running inside gnome-shell can. This
// is that code, and nothing more: it watches the focused window, the idle
// time and the lock screen, joins what it sees into runs, and posts the runs
// every thirty seconds. What it watches, when and why is decided here, by the
// same rule every collector applies -- focused, not idle, not locked -- so the
// app never has to guess. See `docs/plans/screentime.md` in the app's tree.
//
// It never blocks: the shell's main loop is the whole desktop's, and a
// synchronous request here would freeze every window. The config file is
// read asynchronously, the network is libsoup's async API, and a delivery that
// fails stays in a small queue in memory (capped, oldest dropped first) for
// the next one.
//
// The config is written by the app when it installs this extension:
// `$XDG_CONFIG_HOME/karmayoga/screentime-gnome.json`, holding the endpoint,
// a heartbeats-only token, this machine's name, and -- only for a sharing
// server's https address -- the SHA-256 fingerprint of its certificate. It is
// read again every minute, so installing later, or a fresh token, works
// without reloading the shell.
//
// `session-modes` includes `unlock-dialog` so that locking the screen does
// not disable the extension and throw away the queue and the run in progress
// at every lock. Nothing is drawn, here or on the lock screen; while locked
// it only stops counting.

import GLib from "gi://GLib";
import Gio from "gi://Gio";
import Soup from "gi://Soup?version=3.0";
import Shell from "gi://Shell";

import * as Main from "resource:///org/gnome/shell/ui/main.js";
import { Extension } from "resource:///org/gnome/shell/extensions/extension.js";

// How often the focused window is looked at, besides every focus change.
const TICK_SECONDS = 5;
// How often runs are sent.
const FLUSH_SECONDS = 30;
// How often the config file is read again.
const CONFIG_SECONDS = 60;
// How often the vault's idle setting is asked for again.
const HELLO_SECONDS = 600;
// Two samples further apart than this are not one run: the machine slept,
// the shell stalled, or the clock moved. A gap is never counted.
const MAX_GAP_MS = 30 * 1000;
// The most heartbeats kept while the app cannot be reached.
const MAX_QUEUE = 2000;
const DEFAULT_IDLE_MINUTES = 3;
const COLLECTOR = "gnome";

export default class ScreenTimeExtension extends Extension {
  enable() {
    // The config as last read (null until there is one), and its text, so
    // an unchanged file is not reparsed.
    this._config = null;
    this._configText = null;
    this._toldMissing = false;
    this._toldFailing = false;
    // From GET /v1/screentime/hello.
    this._idleMinutes = DEFAULT_IDLE_MINUTES;
    this._helloDevice = "";
    // The run in progress, finished runs not yet queued, the queue of
    // heartbeats waiting to be sent, and when the last sample was taken.
    this._open = null;
    this._done = [];
    this._queue = [];
    this._last = 0;
    this._sending = false;
    this._asking = false;

    this._cancellable = new Gio.Cancellable();
    this._session = new Soup.Session({ timeout: 20 });
    this._tracker = Shell.WindowTracker.get_default();

    this._focusId = global.display.connect("notify::focus-window", () =>
      this._sample(),
    );
    this._tickId = GLib.timeout_add_seconds(
      GLib.PRIORITY_DEFAULT,
      TICK_SECONDS,
      () => {
        this._sample();
        return GLib.SOURCE_CONTINUE;
      },
    );
    this._flushId = GLib.timeout_add_seconds(
      GLib.PRIORITY_DEFAULT,
      FLUSH_SECONDS,
      () => {
        this._flush();
        return GLib.SOURCE_CONTINUE;
      },
    );
    this._configId = GLib.timeout_add_seconds(
      GLib.PRIORITY_DEFAULT,
      CONFIG_SECONDS,
      () => {
        this._readConfig();
        return GLib.SOURCE_CONTINUE;
      },
    );
    this._helloId = GLib.timeout_add_seconds(
      GLib.PRIORITY_DEFAULT,
      HELLO_SECONDS,
      () => {
        this._hello();
        return GLib.SOURCE_CONTINUE;
      },
    );

    this._readConfig();
  }

  disable() {
    if (this._focusId) global.display.disconnect(this._focusId);
    for (const id of [
      this._tickId,
      this._flushId,
      this._configId,
      this._helloId,
    ]) {
      if (id) GLib.Source.remove(id);
    }
    // Cancelled first, so a callback already queued sees it and returns
    // without touching anything below.
    this._cancellable?.cancel();
    this._session?.abort();

    this._focusId = null;
    this._tickId = null;
    this._flushId = null;
    this._configId = null;
    this._helloId = null;
    this._cancellable = null;
    this._session = null;
    this._tracker = null;
    this._config = null;
    this._configText = null;
    this._open = null;
    this._done = null;
    this._queue = null;
  }

  // --- The config file -------------------------------------------------

  _readConfig() {
    const path = GLib.build_filenamev([
      GLib.get_user_config_dir(),
      "karmayoga",
      "screentime-gnome.json",
    ]);
    const cancellable = this._cancellable;
    Gio.File.new_for_path(path).load_contents_async(
      cancellable,
      (file, result) => {
        if (cancellable.is_cancelled()) return;
        let text;
        try {
          const [, contents] = file.load_contents_finish(result);
          text = new TextDecoder().decode(contents);
        } catch (e) {
          if (!this._toldMissing) {
            console.log(
              `Karma Yoga Screen time: waiting for the app to set this up (${path}: ${e.message})`,
            );
            this._toldMissing = true;
          }
          this._config = null;
          this._configText = null;
          return;
        }
        this._toldMissing = false;
        if (text === this._configText) return;
        const config = parseConfig(text);
        if (!config) {
          console.warn(
            `Karma Yoga Screen time: ${path} is not a config this version understands`,
          );
          this._config = null;
          this._configText = null;
          return;
        }
        this._config = config;
        this._configText = text;
        this._hello();
      },
    );
  }

  // --- Watching ----------------------------------------------------------

  // Look at what is in front of the person now, and fold it into the runs.
  // Not configured means nobody to tell, so nothing is watched at all.
  _sample() {
    if (!this._config) return;
    join(this, Date.now(), this._observe());
  }

  // The focused application, or null when nothing counts: locked, idle past
  // the vault's setting, or no window has focus.
  _observe() {
    if (Main.screenShield?.locked || Main.sessionMode.isLocked) return null;
    const idleMs = global.backend.get_core_idle_monitor().get_idletime();
    if (idleMs >= this._idleMinutes * 60 * 1000) return null;
    const win = global.display.focus_window;
    if (!win) return null;
    const app = this._tracker.get_window_app(win);
    let id = app?.get_id() ?? "";
    if (id.endsWith(".desktop")) id = id.slice(0, -".desktop".length);
    // A window with no desktop file is a `window:<n>` app to the
    // tracker, which names nothing that lasts past the window.
    if (!id || id.startsWith("window:"))
      id = win.get_wm_class() || win.get_wm_class_instance() || "";
    if (!id) return null;
    return {
      app: id,
      name: app?.get_name() ?? "",
      title: win.get_title() ?? "",
    };
  }

  // --- Sending -------------------------------------------------------------

  _flush() {
    for (const run of take(this)) {
      this._queue.push({
        kind: "app",
        app: run.app,
        name: run.name,
        title: run.title,
        start: new Date(run.start).toISOString(),
        end: new Date(run.end).toISOString(),
      });
    }
    trimQueue(this._queue);
    if (!this._config || this._sending || this._queue.length === 0) return;

    const beats = this._queue.splice(0, this._queue.length);
    const body = { collector: COLLECTOR, beats };
    // On the app's own listener `hello` names this machine as the app
    // currently calls it; a sharing server answers with no name, and
    // then the one written into the config is it.
    const device = this._helloDevice || this._config.device;
    if (device) body.device = device;
    const msg = this._message("POST", "/v1/screentime/heartbeats", body);
    if (!msg) {
      this._requeue(beats);
      return;
    }

    this._sending = true;
    const cancellable = this._cancellable;
    this._session.send_and_read_async(
      msg,
      GLib.PRIORITY_DEFAULT,
      cancellable,
      (session, result) => {
        if (cancellable.is_cancelled()) return;
        this._sending = false;
        let status = 0;
        try {
          session.send_and_read_finish(result);
          status = msg.get_status();
        } catch (e) {
          this._failing(e.message);
          this._requeue(beats);
          return;
        }
        if (status >= 200 && status < 300) {
          this._toldFailing = false;
          return;
        }
        if (status === 400 || status === 413) {
          // Sending the same body again would be refused again, and
          // would block everything queued behind it.
          console.warn(
            `Karma Yoga Screen time: the app refused ${beats.length} heartbeats (HTTP ${status})`,
          );
          return;
        }
        if (status === 401 || status === 403) {
          // Most likely a fresh token in the file, written by a
          // reinstall; read it now rather than in a minute.
          this._configText = null;
          this._readConfig();
        }
        this._failing(`HTTP ${status}`);
        this._requeue(beats);
      },
    );
  }

  // Ask the vault how long without input counts as idle, and this
  // machine's name.
  _hello() {
    if (!this._config || this._asking) return;
    const msg = this._message("GET", "/v1/screentime/hello", null);
    if (!msg) return;
    this._asking = true;
    const cancellable = this._cancellable;
    this._session.send_and_read_async(
      msg,
      GLib.PRIORITY_DEFAULT,
      cancellable,
      (session, result) => {
        if (cancellable.is_cancelled()) return;
        this._asking = false;
        try {
          const bytes = session.send_and_read_finish(result);
          if (msg.get_status() !== 200) return;
          const hello = JSON.parse(
            new TextDecoder().decode(bytes.get_data() ?? new Uint8Array()),
          );
          const minutes = Number(hello.idleMinutes);
          if (Number.isFinite(minutes) && minutes >= 1 && minutes <= 60)
            this._idleMinutes = minutes;
          this._helloDevice =
            typeof hello.device === "string" ? hello.device.trim() : "";
        } catch (e) {
          // Not reachable yet; the next hello, or the next config
          // change, asks again. The default idle time stands.
        }
      },
    );
  }

  _message(method, path, body) {
    let msg = null;
    try {
      msg = Soup.Message.new(method, this._config.endpoint + path);
    } catch (e) {
      msg = null;
    }
    if (!msg) {
      this._failing(`not an address: ${this._config.endpoint}`);
      return null;
    }
    msg.request_headers.append("Authorization", `Bearer ${this._config.token}`);
    if (body !== null) {
      const bytes = new TextEncoder().encode(JSON.stringify(body));
      msg.set_request_body_from_bytes(
        "application/json",
        new GLib.Bytes(bytes),
      );
    }
    const pin = this._config.fingerprint;
    if (pin) {
      // A sharing server's certificate is its own, signed by nobody,
      // so the usual check refuses it; it is taken only when it is
      // byte for byte the one the app was told about.
      msg.connect(
        "accept-certificate",
        (_msg, cert) => fingerprintOf(cert) === pin,
      );
    }
    return msg;
  }

  _requeue(beats) {
    if (!this._queue) return;
    this._queue.unshift(...beats);
    trimQueue(this._queue);
  }

  _failing(why) {
    if (this._toldFailing) return;
    console.log(
      `Karma Yoga Screen time: could not reach the app (${why}); keeping heartbeats for later`,
    );
    this._toldFailing = true;
  }
}

// --- Pure helpers: the same joining the app's shell does -------------------

// Fold one sample into the runs held on `state` (`_open`, `_done`, `_last`).
// `seen` is the application in use at `now`, or null. Samples of the same app
// join; a switch ends the old run where the new one starts; a sample after a
// gap, or with nothing in use, ends the run at the last sample that saw it.
function join(state, now, seen) {
  const gap = now - state._last;
  const continuous = state._last > 0 && gap >= 0 && gap <= MAX_GAP_MS;
  state._last = now;
  if (!continuous) close(state);
  if (!seen) {
    close(state);
    return;
  }
  const open = state._open;
  if (open && open.app === seen.app) {
    open.end = now;
    if (seen.title) open.title = seen.title;
    if (seen.name) open.name = seen.name;
    return;
  }
  if (open) {
    open.end = now;
    finish(state, open);
  }
  state._open = {
    app: seen.app,
    name: seen.name,
    title: seen.title,
    start: now,
    end: now,
  };
}

function close(state) {
  if (state._open) {
    finish(state, state._open);
    state._open = null;
  }
}

// A run of one sample has no length, and is dropped.
function finish(state, run) {
  if (run.end > run.start) state._done.push(run);
}

// Everything finished, plus what the open run has added since the last
// flush; the open run carries on from where this leaves it.
function take(state) {
  const out = state._done;
  state._done = [];
  const open = state._open;
  if (open && open.end > open.start) {
    out.push({ ...open });
    open.start = open.end;
  }
  return out;
}

function trimQueue(queue) {
  if (queue.length > MAX_QUEUE) queue.splice(0, queue.length - MAX_QUEUE);
}

function parseConfig(text) {
  let raw;
  try {
    raw = JSON.parse(text);
  } catch (e) {
    return null;
  }
  if (
    typeof raw?.endpoint !== "string" ||
    typeof raw?.token !== "string" ||
    !raw.token
  )
    return null;
  const endpoint = raw.endpoint.trim().replace(/\/+$/, "");
  if (!/^https?:\/\//.test(endpoint)) return null;
  return {
    endpoint,
    token: raw.token,
    device: typeof raw.device === "string" ? raw.device.trim() : "",
    fingerprint:
      typeof raw.fingerprint === "string"
        ? raw.fingerprint.replace(/:/g, "").trim().toLowerCase()
        : "",
  };
}

// Lower-case hex SHA-256 of a certificate's DER bytes.
function fingerprintOf(cert) {
  const der = cert.certificate;
  if (!der) return "";
  const bytes = der instanceof GLib.Bytes ? der : new GLib.Bytes(der);
  return GLib.compute_checksum_for_bytes(GLib.ChecksumType.SHA256, bytes) ?? "";
}
