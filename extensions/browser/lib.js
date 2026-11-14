// Karma Yoga Screen time, the browser extension: the pure part.
//
// Nothing in this file touches `chrome.*`, storage or the network, so the
// same file is loaded three ways:
//
//   - by Chrome's service worker, through `importScripts('lib.js')`,
//   - by Firefox's event page, listed before `background.js` in
//     `background.scripts`, and by the options page and the popup through a
//     <script> tag -- each of these finds it on `globalThis.ScreenTimeLib`,
//   - by the tests under `test/`, through node's `require`.
//
// # What the extension reports
//
// One thing: which website was the active tab of the *focused* browser
// window while the person was not idle and the screen was not locked (see
// "The rule that matters" in docs/plans/screentime.md). The background page
// looks every time something changes and every thirty seconds, and hands
// what it saw to `observe`: a `{host, title}`, or `null` when nothing
// counts. `observe` joins observations of the same host into *runs*; the
// runs wait in a queue until `drain` hands them out to be sent, and
// `beatsFor` turns them into the heartbeats `POST /v1/screentime/heartbeats`
// takes.
//
// # The state
//
// A plain JSON object, so it can live in `chrome.storage.session` between
// a service worker's suspensions:
//
//   {
//     open:    {host, title, start, end} | null,   the run still going
//     queue:   [{seq, host, title, start, end}],   closed runs, oldest first
//     seq:     number,                             the last seq handed out
//     dropped: number,                             runs lost to the cap
//   }
//
// Times are milliseconds since the epoch. Every function here returns a new
// state and leaves the one it was given alone.

(function (root, factory) {
  const lib = factory();
  root.ScreenTimeLib = lib;
  if (typeof module === "object" && module && module.exports) {
    module.exports = lib;
  }
})(typeof globalThis !== "undefined" ? globalThis : this, function () {
  "use strict";

  // What a connection code starts with. The rest is base64url (no padding)
  // of `{"endpoint": "...", "token": "...", "device": "..."}`.
  const CODE_PREFIX = "karmayoga-screentime:";

  // Two observations of the same host further apart than this are two runs.
  // The background page looks at least every thirty seconds while anything
  // counts, so a longer silence means it was asleep, suspended past its
  // alarm, or the computer was. The app joins runs again across gaps of up
  // to 45 s (`PULSE_SECS`), so an alarm that fires a little late costs
  // nothing.
  const GAP_MS = 30 * 1000;

  // A run is closed and a new one begun once it has lasted this long. The
  // app refuses a single heartbeat longer than a span may be (12 h); an
  // hour keeps every beat far inside that and lets a long run reach the app
  // in pieces even if the browser quits before the run ends.
  const MAX_RUN_MS = 60 * 60 * 1000;

  // The most closed runs kept waiting while the app cannot be reached. Past
  // it the oldest go: a browser left open for days with the app shut should
  // not grow its storage without end.
  const QUEUE_CAP = 2000;

  // How many beats one POST carries. The app takes up to 5000 and 2 MB.
  const BATCH = 500;

  // The app cuts longer titles; cutting here keeps a batch well under 2 MB.
  const MAX_TITLE = 500;

  // The app's limit on a collector's name.
  const MAX_COLLECTOR = 40;

  // ----- Connection codes ------------------------------------------------

  // Is `hostname` (as `URL.hostname` gives it) this computer?
  function isLoopbackHost(hostname) {
    const h = String(hostname || "").toLowerCase();
    return (
      h === "localhost" ||
      h === "[::1]" ||
      h === "::1" ||
      /^127(\.\d{1,3}){3}$/.test(h)
    );
  }

  // Is this endpoint the app on this computer (rather than a sharing
  // server)? On loopback the app fills in the device's name itself.
  function isLoopback(endpoint) {
    try {
      return isLoopbackHost(new URL(endpoint).hostname);
    } catch (_) {
      return false;
    }
  }

  // base64url, padded or not, to a UTF-8 string. `atob` and `TextDecoder`
  // are there in browsers and in node alike.
  function decodeBase64Url(text) {
    let b64 = text.replace(/-/g, "+").replace(/_/g, "/");
    if (!/^[A-Za-z0-9+/]*={0,2}$/.test(b64)) throw new Error("not base64url");
    b64 = b64.replace(/=+$/, "");
    if (b64.length % 4 === 1) throw new Error("not base64url");
    while (b64.length % 4) b64 += "=";
    const binary = atob(b64);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  }

  // Read what Settings → Screen time → Connect a browser handed out.
  //
  // Answers `{endpoint, token, device, loopback}` with the endpoint's
  // trailing slashes gone, or throws an Error whose message can be shown
  // as it is. Plain `http` is taken only for this computer: anywhere else
  // the token would cross a network in the clear, and an extension can
  // only reach a sharing server over `https` (it cannot pin a self-signed
  // certificate, so the server's tailnet name with a real one is the way).
  function parseConnectionCode(code) {
    const text = String(code == null ? "" : code).replace(/\s+/g, "");
    if (!text) throw new Error("Paste the connection code first.");
    if (!text.startsWith(CODE_PREFIX)) {
      throw new Error(
        'That is not a screen time connection code. It starts with "' +
          CODE_PREFIX +
          '".',
      );
    }
    let value;
    try {
      value = JSON.parse(decodeBase64Url(text.slice(CODE_PREFIX.length)));
    } catch (_) {
      throw new Error(
        "The connection code is damaged. Copy it again from Settings.",
      );
    }
    if (!value || typeof value !== "object" || Array.isArray(value)) {
      throw new Error(
        "The connection code is damaged. Copy it again from Settings.",
      );
    }

    const token = typeof value.token === "string" ? value.token.trim() : "";
    if (!token) throw new Error("The connection code carries no token.");

    if (typeof value.endpoint !== "string" || !value.endpoint.trim()) {
      throw new Error("The connection code names no address.");
    }
    let url;
    try {
      url = new URL(value.endpoint.trim());
    } catch (_) {
      throw new Error(
        "The connection code names an address that is not a web address.",
      );
    }
    if (url.protocol !== "http:" && url.protocol !== "https:") {
      throw new Error(
        "The connection code names an address that is not http or https.",
      );
    }
    if (url.username || url.password || url.search || url.hash) {
      throw new Error(
        "The connection code names an address with more in it than an address.",
      );
    }
    const loopback = isLoopbackHost(url.hostname);
    if (url.protocol === "http:" && !loopback) {
      throw new Error(
        "The connection code names a plain http address on another computer. " +
          "Only this computer may be reached without https.",
      );
    }
    const endpoint = (url.origin + url.pathname).replace(/\/+$/, "");

    const device = typeof value.device === "string" ? value.device.trim() : "";
    return { endpoint, token, device, loopback };
  }

  // The match pattern `chrome.permissions.request` wants for an endpoint
  // (patterns carry no port: one covers every port on that host).
  function originPattern(endpoint) {
    const url = new URL(endpoint);
    return url.protocol + "//" + url.hostname + "/*";
  }

  // ----- Pages -----------------------------------------------------------

  // The host a tab's address counts as, or null for anything that is not a
  // web page: `chrome://`, `edge://`, `about:`, `moz-extension:`,
  // `chrome-extension:`, `file:`, `view-source:`, `data:`, `blob:`, the new
  // tab page, an empty tab.
  //
  // The host is kept as the browser gave it (`URL` lower-cases it already
  // and leaves the port out): the app groups hosts into domains, and does
  // so when reporting, so a change of mind applies to every day recorded.
  //
  // Firefox's reader view (`about:reader?url=...`) counts as the page it is
  // showing.
  function hostOf(address) {
    if (typeof address !== "string" || !address) return null;
    let url;
    try {
      url = new URL(address);
    } catch (_) {
      return null;
    }
    if (url.protocol === "about:" && url.pathname === "reader") {
      const inner = url.searchParams.get("url");
      return inner && !inner.startsWith("about:") ? hostOf(inner) : null;
    }
    if (url.protocol !== "http:" && url.protocol !== "https:") return null;
    if (!url.hostname) return null;
    // An old Chrome new tab page served by the search engine.
    if (url.pathname.startsWith("/_/chrome/newtab")) return null;
    return url.hostname;
  }

  // ----- Browsers --------------------------------------------------------

  // The browser's name as people know it, which is what the app shows and
  // what it uses to tell this browser's site time from its own app time.
  //
  // `userAgent` is `navigator.userAgent`. `hints` carries what the user
  // agent string does not say: `{brave: true}` when `navigator.brave`
  // exists (Brave reports itself as Chrome), and `brands`, the names in
  // `navigator.userAgentData.brands`, when there.
  function browserName(userAgent, hints) {
    const ua = String(userAgent || "");
    const h = hints || {};
    const brands = Array.isArray(h.brands)
      ? h.brands.map((b) => String((b && b.brand) || b))
      : [];
    const branded = (name) => brands.some((b) => b === name);

    if (/Firefox\//.test(ua) && !/Seamonkey\//.test(ua)) return "Firefox";
    if (/\bEdgA?\//.test(ua) || branded("Microsoft Edge")) return "Edge";
    if (/\bOPR\//.test(ua) || branded("Opera")) return "Opera";
    if (/\bVivaldi\//.test(ua)) return "Vivaldi";
    if (h.brave || branded("Brave")) return "Brave";
    if (/\bChrome\//.test(ua) || branded("Google Chrome")) return "Chrome";
    if (/\bChromium\//.test(ua) || branded("Chromium")) return "Chromium";
    return "Browser";
  }

  // The name this extension sends as `collector`: lower-case letters,
  // digits and `-`, as the app requires. `Firefox` → `firefox`.
  function collectorName(name) {
    const cleaned = String(name || "")
      .toLowerCase()
      .replace(/[^a-z0-9-]+/g, "-")
      .replace(/-+/g, "-")
      .replace(/^-+|-+$/g, "")
      .slice(0, MAX_COLLECTOR)
      .replace(/-+$/, "");
    return cleaned || "browser";
  }

  // ----- Runs ------------------------------------------------------------

  function emptyState() {
    return { open: null, queue: [], seq: 0, dropped: 0 };
  }

  // A state read back from storage, made whole: missing or damaged fields
  // become their empty values rather than throwing later.
  function normalizeState(value) {
    const s = value && typeof value === "object" ? value : {};
    const run = (r) =>
      r &&
      typeof r.host === "string" &&
      Number.isFinite(r.start) &&
      Number.isFinite(r.end);
    return {
      open: run(s.open) ? s.open : null,
      queue: Array.isArray(s.queue) ? s.queue.filter(run) : [],
      seq: Number.isFinite(s.seq) ? s.seq : 0,
      dropped: Number.isFinite(s.dropped) ? s.dropped : 0,
    };
  }

  function cutTitle(title) {
    const t = typeof title === "string" ? title.trim() : "";
    return t.length > MAX_TITLE ? t.slice(0, MAX_TITLE) : t;
  }

  // Put the open run, if any, in the queue. A run that lasted no time at
  // all (seen once, then gone) is not worth sending and is let go.
  function close(state) {
    const open = state.open;
    if (!open) return state;
    if (open.end <= open.start) return { ...state, open: null };
    const seq = state.seq + 1;
    let queue = state.queue.concat([
      {
        seq,
        host: open.host,
        title: open.title,
        start: open.start,
        end: open.end,
      },
    ]);
    let dropped = state.dropped;
    if (queue.length > QUEUE_CAP) {
      dropped += queue.length - QUEUE_CAP;
      queue = queue.slice(queue.length - QUEUE_CAP);
    }
    return { ...state, open: null, queue, seq, dropped };
  }

  // Take one look: at `now`, `seen` (`{host, title}`) was the active tab of
  // the focused window, or `seen` is null because nothing counted (the
  // browser lost focus, the person went idle, the screen locked, the tab is
  // not a web page).
  //
  // - The same host as the open run, within GAP_MS of its end: the run goes
  //   on to `now`, taking the newest title.
  // - Another host, or null, within GAP_MS: the open run ends at `now` (it
  //   had attention until this moment) and, for a host, a new one starts.
  // - Past GAP_MS, or a clock that went backwards: the open run ends where
  //   it was last seen, and a host starts afresh.
  function observe(state, now, seen) {
    let s = normalizeState(state);
    const host =
      seen && typeof seen.host === "string" && seen.host ? seen.host : null;
    const title = host ? cutTitle(seen.title) : "";
    const open = s.open;
    const fresh = open && now >= open.end && now - open.end <= GAP_MS;

    if (open && fresh) {
      if (host === open.host) {
        const run = { ...open, end: now, title: title || open.title };
        s = { ...s, open: run };
        // Long enough: close it here and carry on in a new one.
        if (run.end - run.start >= MAX_RUN_MS) {
          s = close(s);
          s = { ...s, open: { host, title: run.title, start: now, end: now } };
        }
        return s;
      }
      s = close({ ...s, open: { ...open, end: now } });
    } else if (open) {
      s = close(s);
    }

    if (host) s = { ...s, open: { host, title, start: now, end: now } };
    return s;
  }

  // Hand out up to `maxBeats` runs to send: the oldest closed ones, and the
  // open run as it stands if there is room. Sending a run still going is
  // what keeps the app's picture current; when it is sent again, longer,
  // the app extends the span it already made rather than adding another
  // (same host, same start).
  //
  // Answers `{beats, upto, state}`: the runs (`{host, title, start, end}`,
  // times in ms -- `beatsFor` makes heartbeats of them), the seq of the
  // last closed run among them, and the state as it will be once they are
  // known to have arrived. The background page sends first and only then
  // calls `acknowledge(current, upto)`, because looks taken while the
  // request was out must not be lost.
  function drain(state, maxBeats) {
    const s = normalizeState(state);
    const max =
      Number.isFinite(maxBeats) && maxBeats > 0 ? Math.floor(maxBeats) : BATCH;
    const closed = s.queue.slice(0, max);
    const beats = closed.map(({ host, title, start, end }) => ({
      host,
      title,
      start,
      end,
    }));
    if (beats.length < max && s.open && s.open.end > s.open.start) {
      const { host, title, start, end } = s.open;
      beats.push({ host, title, start, end });
    }
    const upto = closed.length ? closed[closed.length - 1].seq : 0;
    return { beats, upto, state: acknowledge(s, upto) };
  }

  // The state with every closed run up to and including seq `upto` gone:
  // they arrived.
  function acknowledge(state, upto) {
    const s = normalizeState(state);
    if (!upto) return s;
    return { ...s, queue: s.queue.filter((r) => r.seq > upto) };
  }

  // How many runs are waiting, counting the open one. With `since` (ms),
  // only those still going at or after it: "today's" when `since` is
  // midnight, a run across midnight included.
  function queuedCount(state, since) {
    const s = normalizeState(state);
    const runs = s.queue.concat(
      s.open && s.open.end > s.open.start ? [s.open] : [],
    );
    const from = Number.isFinite(since) ? since : -Infinity;
    return runs.filter((r) => r.end >= from).length;
  }

  // Runs as the heartbeats the app takes:
  // `{kind: "site", app: "Firefox", host, title, start, end}`, with ISO
  // instants. `options.titles === false` sends every title empty.
  function beatsFor(runs, browser, options) {
    const titles = !(options && options.titles === false);
    return (runs || []).map((r) => ({
      kind: "site",
      app: browser,
      host: r.host,
      title: titles ? cutTitle(r.title) : "",
      start: new Date(r.start).toISOString(),
      end: new Date(r.end).toISOString(),
    }));
  }

  // The body of one POST. On loopback the app fills in `device`, and must
  // be left to: it knows this machine's name better than an old code. A
  // sharing server needs it named.
  function heartbeatBody(config, collector, beats) {
    const body = { collector, beats };
    if (!isLoopback(config.endpoint) && config.device)
      body.device = config.device;
    return body;
  }

  // How long to wait before trying again after `failures` failures in a
  // row: 30 s, 1 min, 2 min, ... up to 10 min.
  function backoffMs(failures) {
    const n = Math.max(0, Math.min(10, (failures | 0) - 1));
    return Math.min(10 * 60 * 1000, 30 * 1000 * Math.pow(2, n));
  }

  return {
    CODE_PREFIX,
    GAP_MS,
    MAX_RUN_MS,
    QUEUE_CAP,
    BATCH,
    MAX_TITLE,
    isLoopback,
    isLoopbackHost,
    parseConnectionCode,
    originPattern,
    hostOf,
    browserName,
    collectorName,
    emptyState,
    normalizeState,
    observe,
    drain,
    acknowledge,
    queuedCount,
    beatsFor,
    heartbeatBody,
    backoffMs,
  };
});
