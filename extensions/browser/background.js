// Every Day Screen time, the browser extension: the background page.
//
// Chrome runs this as a service worker, Firefox as an event page. Either
// may be stopped whenever it has been quiet for a while and started again
// by the next event, so nothing that matters lives in a variable here:
//
//   chrome.storage.session  `state`   the runs (see lib.js) and
//                           `live`    what the last look found, for the popup
//   chrome.storage.local    `config`  {endpoint, token, device, idleMinutes,
//                                      keepTitles}, set by Connect
//                           `status`  {lastSent, lastError, revoked, ...}
//
// # Looking
//
// Every event that could change the answer -- a window gaining or losing
// focus, another tab, a tab's address, title or sound changing, going idle
// -- and an alarm every thirty seconds lead to one `record`: ask the
// browser which window was focused last and whether it still is, take its
// active tab, and hand lib.js's `observe` the tab's host, or null when
// nothing counts. Looking afresh each time, rather than trusting what an
// event said, means a missed event costs at most thirty seconds.
//
// What counts (docs/plans/screentime.md, "focused, not idle, not locked"):
// the active tab of a focused, non-private window, showing a web page,
// while the person is not idle and the screen is not locked. The one
// exception: a tab playing sound keeps counting while idle, because
// somebody watching a film does not touch the keyboard.
//
// # Sending
//
// The thirty-second alarm also flushes: whatever runs are waiting go to
// `POST /v1/screentime/heartbeats`. What does not arrive stays queued (up
// to lib.js's cap) and is tried again, backing off while the app is shut
// or locked. A 401 or 403 means the code was revoked or never fitted, so
// sending stops until the person connects again.

"use strict";

if (
  typeof ScreenTimeLib === "undefined" &&
  typeof importScripts === "function"
) {
  importScripts("lib.js");
}
const L = globalThis.ScreenTimeLib;

// Firefox has `browser` (promises everywhere) and `chrome`; Chrome only
// `chrome`, which in Manifest V3 also answers with promises.
const ext = globalThis.browser || globalThis.chrome;

const sessionArea = ext.storage.session || ext.storage.local;
const localArea = ext.storage.local;

const TICK = "screentime-tick";
const HELLO = "screentime-hello";
const HELLO_EVERY_MIN = 6 * 60;
const FETCH_TIMEOUT_MS = 15 * 1000;

// Which browser this is, sent as each beat's `app` and, lower-cased, as the
// collector's name.
const BROWSER = L.browserName(navigator.userAgent, {
  brave: typeof navigator.brave !== "undefined",
  brands: navigator.userAgentData && navigator.userAgentData.brands,
});
const COLLECTOR = L.collectorName(BROWSER);

// ----- Storage -----------------------------------------------------------

async function getState() {
  const got = await sessionArea.get("state");
  return L.normalizeState(got.state);
}

async function setState(state) {
  await sessionArea.set({ state });
}

async function getConfig() {
  const got = await localArea.get("config");
  const c = got.config;
  return c && c.endpoint && c.token ? c : null;
}

async function getStatus() {
  const got = await localArea.get("status");
  return got.status || {};
}

async function updateStatus(changes) {
  const status = { ...(await getStatus()), ...changes };
  await localArea.set({ status });
  return status;
}

// Every change to the runs goes through here, one at a time: two events
// arriving together must not both read the old state and each write back
// their own version of it.
let chain = Promise.resolve();
function serial(fn) {
  const run = chain.then(fn, fn);
  chain = run.catch(() => {});
  return run;
}

function idleSeconds(config) {
  const minutes = Number(config && config.idleMinutes) || 3;
  // The browser's floor is 15 s.
  return Math.max(15, Math.round(Math.min(60, Math.max(1, minutes)) * 60));
}

function startOfToday() {
  const d = new Date();
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

// ----- Looking -----------------------------------------------------------

// What is in front of the person right now. Answers `{seen, reason}`:
// `seen` is `{host, title}` or null, `reason` one of `counting`, `audible`
// (counting though idle), `unfocused`, `idle`, `locked`, `private`,
// `not-a-site`, `disconnected`.
//
// `hint.unfocused` is set when the browser has just said no window of its
// has focus, and `hint.idle` carries the idle state an event just gave, so
// neither is asked again.
async function look(config, hint) {
  const h = hint || {};
  if (!config) return { seen: null, reason: "disconnected" };
  if (h.unfocused) return { seen: null, reason: "unfocused" };
  if (h.idle === "locked") return { seen: null, reason: "locked" };

  let win = null;
  try {
    win = await ext.windows.getLastFocused({ populate: true });
  } catch (_) {
    win = null;
  }
  if (!win || !win.focused) return { seen: null, reason: "unfocused" };
  const tab = (win.tabs || []).find((t) => t.active);
  if (!tab) return { seen: null, reason: "unfocused" };
  // A private window is somebody asking not to be remembered.
  if (win.incognito || tab.incognito) return { seen: null, reason: "private" };
  const host = L.hostOf(tab.url || tab.pendingUrl || "");
  if (!host) return { seen: null, reason: "not-a-site" };

  let idle = h.idle;
  if (!idle) {
    try {
      idle = await ext.idle.queryState(idleSeconds(config));
    } catch (_) {
      idle = "active";
    }
  }
  if (idle === "locked") return { seen: null, reason: "locked" };
  if (idle === "idle" && !tab.audible) return { seen: null, reason: "idle" };

  const title = config.keepTitles === false ? "" : tab.title || "";
  return {
    seen: { host, title },
    reason: idle === "idle" ? "audible" : "counting",
  };
}

// Look, and fold what was seen into the runs.
function record(hint) {
  return serial(async () => {
    const config = await getConfig();
    const found = await look(config, hint);
    const now = Date.now();
    await setState(L.observe(await getState(), now, found.seen));
    const live = {
      counting: !!found.seen,
      host: found.seen ? found.seen.host : "",
      reason: found.reason,
      at: now,
    };
    await sessionArea.set({ live });
    return live;
  });
}

// ----- Talking to the app ------------------------------------------------

function where(config) {
  return L.isLoopback(config.endpoint)
    ? "the Every Day app on this computer"
    : config.endpoint;
}

async function call(config, method, path, body) {
  const init = {
    method,
    headers: { Authorization: "Bearer " + config.token },
    signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
    cache: "no-store",
    credentials: "omit",
  };
  if (body !== undefined) {
    init.headers["Content-Type"] = "application/json";
    init.body = JSON.stringify(body);
  }
  return fetch(config.endpoint + path, init);
}

// What a failed answer means, in words the options page can show.
function explain(config, res) {
  switch (res.status) {
    case 401:
      return "The app no longer accepts this connection code. Make a new one in Settings → Screen time and connect again.";
    case 403:
      return "This connection code is not allowed to send screen time. Make a new one in Settings → Screen time.";
    case 409:
      return "Every Day is locked. What was seen is kept and sent once it is unlocked.";
    case 413:
      return "The app refused a delivery as too large.";
    case 400:
      return "The app refused a delivery it could not read.";
    default:
      return where(config) + " answered " + res.status + ".";
  }
}

// `GET /v1/screentime/hello`: is the code good, and what does the app want?
// Answers the hello, or throws an Error to show; `error.revoked` is set for
// a 401 or 403.
async function hello(config) {
  let res;
  try {
    res = await call(config, "GET", "/v1/screentime/hello");
  } catch (_) {
    throw new Error("Could not reach " + where(config) + ". Is it running?");
  }
  if (!res.ok) {
    const err = new Error(explain(config, res));
    err.revoked = res.status === 401 || res.status === 403;
    throw err;
  }
  let answer;
  try {
    answer = await res.json();
  } catch (_) {
    throw new Error(where(config) + " did not answer like Every Day.");
  }
  if (!answer || typeof answer !== "object") {
    throw new Error(where(config) + " did not answer like Every Day.");
  }
  return answer;
}

// Take what a hello said into the config.
function withHello(config, answer) {
  const idle = Number(answer.idleMinutes);
  return {
    ...config,
    // On loopback the app names this machine; a sharing server answers
    // empty and the code's name stands.
    device:
      (typeof answer.device === "string" && answer.device.trim()) ||
      config.device ||
      "",
    idleMinutes:
      Number.isFinite(idle) && idle >= 1
        ? Math.min(60, idle)
        : config.idleMinutes || 3,
    keepTitles: answer.keepTitles !== false,
  };
}

async function applyIdle(config) {
  try {
    ext.idle.setDetectionInterval(idleSeconds(config));
  } catch (_) {
    // Not fatal: `queryState` is asked with the right threshold anyway.
  }
}

// Ask the app again what it wants (every six hours, and at start-up when
// it has been longer than that).
async function refresh() {
  const config = await getConfig();
  if (!config) return;
  try {
    const answer = await hello(config);
    const next = withHello(config, answer);
    await localArea.set({ config: next });
    await applyIdle(next);
    const changes = {
      helloAt: Date.now(),
      version: typeof answer.version === "string" ? answer.version : "",
    };
    // A code the app accepts again (a vault restored, say) sends again.
    if ((await getStatus()).revoked)
      Object.assign(changes, { revoked: false, lastError: "" });
    await updateStatus(changes);
  } catch (e) {
    if (e.revoked)
      await updateStatus({
        revoked: true,
        lastError: e.message,
        lastErrorAt: Date.now(),
      });
  }
}

// Send what is waiting. One flush at a time; each sends up to a few
// batches of lib.js's BATCH.
let flushing = null;
function flush(options) {
  if (!flushing) {
    flushing = sendWaiting(options || {}).finally(() => {
      flushing = null;
    });
  }
  return flushing;
}

async function sendWaiting({ force }) {
  const config = await getConfig();
  if (!config) return;
  const status = await getStatus();
  if (status.revoked) return;
  if (!force && status.retryAt && Date.now() < status.retryAt) return;

  for (let round = 0; round < 4; round++) {
    // Read without the lock: nothing is changed here. What arrived is
    // removed afterwards by seq, under the lock, so looks taken while the
    // request was out are kept.
    const { beats, upto } = L.drain(await getState(), L.BATCH);
    if (!beats.length) return;
    const body = L.heartbeatBody(
      config,
      COLLECTOR,
      L.beatsFor(beats, BROWSER, { titles: config.keepTitles !== false }),
    );

    let res;
    try {
      res = await call(config, "POST", "/v1/screentime/heartbeats", body);
    } catch (_) {
      await failed(
        "Could not reach " +
          where(config) +
          ". What was seen is kept and sent later.",
      );
      return;
    }

    if (res.ok) {
      let report = {};
      try {
        report = await res.json();
      } catch (_) {
        report = {};
      }
      await serial(async () => setState(L.acknowledge(await getState(), upto)));
      await updateStatus({
        lastSent: Date.now(),
        lastAccepted: Number.isFinite(report.accepted)
          ? report.accepted
          : beats.length,
        lastError: "",
        failures: 0,
        retryAt: 0,
      });
      // Only the open run, or a short batch: nothing more to send.
      if (!upto || beats.length < L.BATCH) return;
      continue;
    }

    if (res.status === 401 || res.status === 403) {
      await updateStatus({
        revoked: true,
        lastError: explain(config, res),
        lastErrorAt: Date.now(),
      });
      return;
    }
    if (res.status === 400) {
      // Sending the same batch again would be refused again and hold up
      // everything behind it, so it is let go.
      await serial(async () => setState(L.acknowledge(await getState(), upto)));
      await updateStatus({
        lastError: explain(config, res),
        lastErrorAt: Date.now(),
      });
      return;
    }
    await failed(explain(config, res));
    return;
  }
}

async function failed(message) {
  const status = await getStatus();
  const failures = (status.failures || 0) + 1;
  await updateStatus({
    lastError: message,
    lastErrorAt: Date.now(),
    failures,
    retryAt: Date.now() + L.backoffMs(failures),
  });
}

// ----- Alarms ------------------------------------------------------------

// Alarms outlive the worker, so they are made only when missing: making
// one again would push its next firing back every time the worker woke.
async function ensureAlarms() {
  if (!(await ext.alarms.get(TICK))) {
    ext.alarms.create(TICK, { periodInMinutes: 0.5 });
  }
  if (!(await ext.alarms.get(HELLO))) {
    ext.alarms.create(HELLO, { periodInMinutes: HELLO_EVERY_MIN });
  }
}

async function start() {
  await ensureAlarms();
  const config = await getConfig();
  if (!config) return;
  await applyIdle(config);
  const status = await getStatus();
  if (
    !status.helloAt ||
    Date.now() - status.helloAt > HELLO_EVERY_MIN * 60 * 1000
  ) {
    await refresh();
  }
}

// ----- What the options page and the popup ask ---------------------------

async function describe() {
  const config = await getConfig();
  const status = await getStatus();
  const state = await getState();
  const { live } = await sessionArea.get("live");
  return {
    ok: true,
    connected: !!config,
    endpoint: config ? config.endpoint : "",
    loopback: config ? L.isLoopback(config.endpoint) : false,
    device: config ? config.device || "" : "",
    keepTitles: config ? config.keepTitles !== false : true,
    idleMinutes: config ? config.idleMinutes || 3 : 3,
    version: status.version || "",
    revoked: !!status.revoked,
    lastSent: status.lastSent || 0,
    lastAccepted: status.lastAccepted || 0,
    lastError: status.lastError || "",
    lastErrorAt: status.lastErrorAt || 0,
    retryAt: status.retryAt || 0,
    queued: L.queuedCount(state),
    queuedToday: L.queuedCount(state, startOfToday()),
    dropped: state.dropped,
    browser: BROWSER,
    collector: COLLECTOR,
    live: live || null,
  };
}

const handlers = {
  // A fresh look, then everything there is to show.
  async status() {
    await record();
    return describe();
  },

  // Check a connection code against the app and keep it. The options page
  // has already asked for the host permission (that needs the click).
  async connect(msg) {
    const parsed = L.parseConnectionCode(msg.code);
    const candidate = {
      endpoint: parsed.endpoint,
      token: parsed.token,
      device: parsed.device,
      idleMinutes: 3,
      keepTitles: true,
    };
    const answer = await hello(candidate);
    const config = withHello(candidate, answer);
    if (!parsed.loopback && !config.device) {
      throw new Error(
        "The connection code does not say what this computer is called. Make a new one in Settings.",
      );
    }
    await localArea.set({ config });
    await localArea.set({
      status: {
        helloAt: Date.now(),
        version: typeof answer.version === "string" ? answer.version : "",
        revoked: false,
        failures: 0,
        retryAt: 0,
        lastError: "",
      },
    });
    await applyIdle(config);
    await ensureAlarms();
    await record();
    await flush({ force: true });
    return describe();
  },

  // Forget the connection and everything waiting to be sent.
  async disconnect() {
    await serial(async () => {
      await localArea.remove(["config", "status"]);
      await sessionArea.remove(["state", "live"]);
    });
    return describe();
  },

  // Send now, whatever the back-off says.
  async send() {
    await record();
    await flush({ force: true });
    return describe();
  },
};

// ----- Events ------------------------------------------------------------
//
// Listeners are added at the top level, every time this file runs: an
// event that wakes the worker is only delivered to listeners added during
// that first run.

// Errors in work an event started have nobody to go to but the console.
function quietly(promise) {
  return promise.catch((e) => console.warn("screentime:", e));
}

ext.windows.onFocusChanged.addListener((windowId) => {
  quietly(record({ unfocused: windowId === ext.windows.WINDOW_ID_NONE }));
});

ext.tabs.onActivated.addListener(() => {
  quietly(record());
});

ext.tabs.onUpdated.addListener((_tabId, change, tab) => {
  if (!tab || !tab.active) return;
  if ("url" in change || "title" in change || "audible" in change)
    quietly(record());
});

ext.idle.onStateChanged.addListener((newState) => {
  quietly(record({ idle: newState }));
});

ext.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === TICK) {
    quietly(quietly(record()).then(() => flush()));
  } else if (alarm.name === HELLO) {
    quietly(refresh());
  }
});

ext.runtime.onStartup.addListener(() => {
  quietly(start());
});

ext.runtime.onInstalled.addListener(() => {
  quietly(start());
});

ext.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  // Only this extension's own pages.
  if (sender && sender.id && sender.id !== ext.runtime.id) return false;
  const handler =
    msg &&
    Object.prototype.hasOwnProperty.call(handlers, msg.type) &&
    handlers[msg.type];
  if (!handler) return false;
  handler(msg).then(sendResponse, (e) =>
    sendResponse({ ok: false, error: (e && e.message) || String(e) }),
  );
  // The answer comes later: keep the channel open (Chrome needs this).
  return true;
});

quietly(start());
