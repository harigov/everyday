// Tests for lib.js, the extension's pure logic. Run with
// `node --test extensions/browser/test/`. No browser and no dependencies:
// lib.js touches nothing but its arguments.

import { test } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const L = require("../lib.js");

const PREFIX = "everyday-screentime:";
const code = (value) =>
  PREFIX + Buffer.from(JSON.stringify(value)).toString("base64url");

// ----- Connection codes ----------------------------------------------------

test("parseConnectionCode reads a loopback code", () => {
  const got = L.parseConnectionCode(
    code({
      endpoint: "http://127.0.0.1:7399",
      token: "tok-123",
      device: "laptop",
    }),
  );
  assert.deepEqual(got, {
    endpoint: "http://127.0.0.1:7399",
    token: "tok-123",
    device: "laptop",
    loopback: true,
  });
});

test("parseConnectionCode reads a sharing server code, keeping its path", () => {
  const got = L.parseConnectionCode(
    code({
      endpoint: "https://box.tail1234.ts.net/everyday/",
      token: "t",
      device: "desk",
    }),
  );
  assert.equal(got.endpoint, "https://box.tail1234.ts.net/everyday");
  assert.equal(got.loopback, false);
  assert.equal(got.device, "desk");
});

test("parseConnectionCode drops a trailing slash and tolerates pasted whitespace", () => {
  const raw = code({
    endpoint: "http://localhost:7399/",
    token: " tok ",
    device: "",
  });
  const pasted = "  " + raw.slice(0, 25) + "\n" + raw.slice(25) + "\n";
  const got = L.parseConnectionCode(pasted);
  assert.equal(got.endpoint, "http://localhost:7399");
  assert.equal(got.token, "tok");
  assert.equal(got.device, "");
  assert.equal(got.loopback, true);
});

test("parseConnectionCode takes padded base64 and a device name beyond ASCII", () => {
  const b64 = Buffer.from(
    JSON.stringify({
      endpoint: "http://[::1]:7399",
      token: "x",
      device: "Hari’s laptop",
    }),
  ).toString("base64url");
  const padded = b64 + "=".repeat((4 - (b64.length % 4)) % 4);
  const got = L.parseConnectionCode(PREFIX + padded);
  assert.equal(got.device, "Hari’s laptop");
  assert.equal(got.loopback, true);
});

test("parseConnectionCode leaves device empty when the code has none", () => {
  const got = L.parseConnectionCode(
    code({ endpoint: "http://127.0.0.1:7399", token: "x" }),
  );
  assert.equal(got.device, "");
});

test("parseConnectionCode refuses what is not a good code", () => {
  const bad = [
    ["", /Paste/],
    ["   ", /Paste/],
    [null, /Paste/],
    ["hello", /not a screen time connection code/],
    [
      Buffer.from("{}").toString("base64url"),
      /not a screen time connection code/,
    ],
    [PREFIX, /damaged/],
    [PREFIX + "%%%not-base64%%%", /damaged/],
    [PREFIX + Buffer.from("not json").toString("base64url"), /damaged/],
    [code([1, 2, 3]), /damaged/],
    [code("a string"), /damaged/],
    [code({ endpoint: "http://127.0.0.1:7399" }), /no token/],
    [code({ endpoint: "http://127.0.0.1:7399", token: "   " }), /no token/],
    [code({ endpoint: "http://127.0.0.1:7399", token: 42 }), /no token/],
    [code({ token: "t" }), /no address/],
    [code({ endpoint: "", token: "t" }), /no address/],
    [code({ endpoint: "not a url", token: "t" }), /not a web address/],
    [code({ endpoint: "ftp://127.0.0.1", token: "t" }), /not http or https/],
    [
      code({ endpoint: "javascript:alert(1)", token: "t" }),
      /not http or https/,
    ],
    [code({ endpoint: "http://192.168.1.5:7399", token: "t" }), /plain http/],
    [code({ endpoint: "http://box.example.net", token: "t" }), /plain http/],
    [
      code({ endpoint: "https://u:p@box.example.net", token: "t" }),
      /more in it/,
    ],
    [
      code({ endpoint: "https://box.example.net/?x=1", token: "t" }),
      /more in it/,
    ],
    [
      code({ endpoint: "https://box.example.net/#x", token: "t" }),
      /more in it/,
    ],
  ];
  for (const [input, message] of bad) {
    assert.throws(
      () => L.parseConnectionCode(input),
      message,
      `for ${JSON.stringify(input)}`,
    );
  }
});

test("isLoopback knows this computer from another", () => {
  assert.equal(L.isLoopback("http://127.0.0.1:7399"), true);
  assert.equal(L.isLoopback("http://127.1.2.3"), true);
  assert.equal(L.isLoopback("http://localhost:7399/x"), true);
  assert.equal(L.isLoopback("http://[::1]:7399"), true);
  assert.equal(L.isLoopback("https://box.tail1234.ts.net"), false);
  assert.equal(L.isLoopback("http://192.168.1.5"), false);
  assert.equal(L.isLoopback("http://localhost.example.com"), false);
  assert.equal(L.isLoopback("nonsense"), false);
});

test("originPattern gives a match pattern without the port", () => {
  assert.equal(L.originPattern("http://127.0.0.1:7399"), "http://127.0.0.1/*");
  assert.equal(
    L.originPattern("https://box.tail1234.ts.net:8443/everyday"),
    "https://box.tail1234.ts.net/*",
  );
});

// ----- hostOf ----------------------------------------------------------------

test("hostOf answers the host of a web page, as the browser gave it", () => {
  assert.equal(L.hostOf("https://github.com/harigov/everyday"), "github.com");
  assert.equal(L.hostOf("http://example.org"), "example.org");
  assert.equal(
    L.hostOf("https://mail.google.com/mail/u/0/#inbox"),
    "mail.google.com",
  );
  // `www.` is kept: the app groups hosts into domains itself.
  assert.equal(L.hostOf("https://www.example.com/"), "www.example.com");
  // URL lower-cases the host and leaves the port out.
  assert.equal(L.hostOf("https://WWW.Example.COM/Path"), "www.example.com");
  assert.equal(L.hostOf("http://localhost:5173/"), "localhost");
  assert.equal(L.hostOf("http://127.0.0.1:8080/x"), "127.0.0.1");
  assert.equal(L.hostOf("https://[::1]:8443/"), "[::1]");
});

test("hostOf answers null for anything that is not a web page", () => {
  const none = [
    "chrome://newtab/",
    "chrome://settings/",
    "chrome-search://local-ntp/local-ntp.html",
    "chrome-extension://abcdefghijklmnop/options.html",
    "edge://newtab/",
    "brave://newtab/",
    "vivaldi://startpage/",
    "opera://startpage/",
    "about:blank",
    "about:newtab",
    "about:home",
    "about:preferences",
    "about:reader?url=about:blank",
    "moz-extension://1234-5678/options.html",
    "file:///home/hari/notes.txt",
    "view-source:https://example.com/",
    "data:text/html,<p>hi</p>",
    "blob:https://example.com/0f8a4c2e-1111-2222-3333-444455556666",
    "javascript:void(0)",
    "ftp://ftp.example.com/file",
    "https://www.google.com/_/chrome/newtab?ie=UTF-8",
    "http://",
    "not a url",
    "",
    undefined,
    null,
    42,
  ];
  for (const address of none) {
    assert.equal(L.hostOf(address), null, `for ${String(address)}`);
  }
});

test("hostOf counts Firefox reader view as the page it shows", () => {
  assert.equal(
    L.hostOf("about:reader?url=https%3A%2F%2Fnews.example.org%2Fstory"),
    "news.example.org",
  );
  assert.equal(
    L.hostOf("about:reader?url=https://news.example.org/story"),
    "news.example.org",
  );
});

// ----- Browsers ----------------------------------------------------------------

const UA = {
  firefox:
    "Mozilla/5.0 (X11; Linux x86_64; rv:121.0) Gecko/20100101 Firefox/121.0",
  chrome:
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36",
  edge: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 Edg/130.0.0.0",
  opera:
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 OPR/115.0.0.0",
  vivaldi:
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 Vivaldi/7.0",
  chromium:
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chromium/130.0.0.0 Safari/537.36",
};

test("browserName tells the browsers apart", () => {
  assert.equal(L.browserName(UA.firefox), "Firefox");
  assert.equal(L.browserName(UA.chrome), "Chrome");
  assert.equal(L.browserName(UA.edge), "Edge");
  assert.equal(L.browserName(UA.opera), "Opera");
  assert.equal(L.browserName(UA.vivaldi), "Vivaldi");
  assert.equal(L.browserName(UA.chromium), "Chromium");
  assert.equal(L.browserName(""), "Browser");
  assert.equal(L.browserName(undefined), "Browser");
});

test("browserName needs to be told about Brave, which says it is Chrome", () => {
  assert.equal(L.browserName(UA.chrome, { brave: true }), "Brave");
  assert.equal(L.browserName(UA.chrome, { brave: false }), "Chrome");
  assert.equal(
    L.browserName(UA.chrome, { brands: [{ brand: "Brave", version: "130" }] }),
    "Brave",
  );
});

test("browserName reads client hint brands when there", () => {
  assert.equal(
    L.browserName(UA.chrome, {
      brands: [{ brand: "Microsoft Edge", version: "130" }],
    }),
    "Edge",
  );
  assert.equal(
    L.browserName(UA.chrome, { brands: [{ brand: "Opera", version: "115" }] }),
    "Opera",
  );
  assert.equal(
    L.browserName("", { brands: [{ brand: "Google Chrome", version: "130" }] }),
    "Chrome",
  );
  // Firefox wins over anything a brand list might say.
  assert.equal(L.browserName(UA.firefox, { brave: true }), "Firefox");
});

test("collectorName is lower-case letters, digits and dashes", () => {
  assert.equal(L.collectorName("Firefox"), "firefox");
  assert.equal(L.collectorName("Chrome"), "chrome");
  assert.equal(L.collectorName("Microsoft Edge"), "microsoft-edge");
  assert.equal(L.collectorName("  Chrome!! "), "chrome");
  assert.equal(L.collectorName("Opera GX 2"), "opera-gx-2");
  assert.equal(L.collectorName(""), "browser");
  assert.equal(L.collectorName("***"), "browser");
  assert.equal(L.collectorName(undefined), "browser");
  const long = L.collectorName("a".repeat(50));
  assert.equal(long.length, 40);
  assert.match(L.collectorName("x".repeat(39) + " y"), /^[a-z0-9-]+$/);
  assert.doesNotMatch(L.collectorName("x".repeat(39) + " y"), /-$/);
});

// ----- Runs ------------------------------------------------------------------------

const S = 1000;
const seen = (host, title = "") => ({ host, title });

// Fold a list of [time, observation] into a state.
function play(steps, state = L.emptyState()) {
  return steps.reduce((s, [t, o]) => L.observe(s, t, o), state);
}

test("observe opens a run on the first look", () => {
  const s = L.observe(L.emptyState(), 1000, seen("github.com", "Repo"));
  assert.deepEqual(s.open, {
    host: "github.com",
    title: "Repo",
    start: 1000,
    end: 1000,
  });
  assert.deepEqual(s.queue, []);
});

test("observe joins looks at the same host into one run, keeping the newest title", () => {
  const s = play([
    [0, seen("a.com", "One")],
    [10 * S, seen("a.com", "Two")],
    [40 * S, seen("a.com", "")], // exactly 30 s later still joins; an empty title keeps the last
  ]);
  assert.deepEqual(s.open, {
    host: "a.com",
    title: "Two",
    start: 0,
    end: 40 * S,
  });
  assert.equal(s.queue.length, 0);
});

test("observe starts a new run after a gap over 30 seconds", () => {
  const s = play([
    [0, seen("a.com")],
    [20 * S, seen("a.com")],
    [20 * S + L.GAP_MS + 1, seen("a.com")],
  ]);
  // The first run ends where it was last seen, not where the gap ended.
  assert.equal(s.queue.length, 1);
  assert.deepEqual(
    { start: s.queue[0].start, end: s.queue[0].end, host: s.queue[0].host },
    { start: 0, end: 20 * S, host: "a.com" },
  );
  assert.deepEqual(s.open, {
    host: "a.com",
    title: "",
    start: 20 * S + L.GAP_MS + 1,
    end: 20 * S + L.GAP_MS + 1,
  });
});

test("observe closes the run at the moment of a null within the gap", () => {
  const s = play([
    [0, seen("a.com", "A")],
    [10 * S, seen("a.com", "A")],
    [25 * S, null],
  ]);
  assert.equal(s.open, null);
  assert.deepEqual(s.queue, [
    { seq: 1, host: "a.com", title: "A", start: 0, end: 25 * S },
  ]);
});

test("observe closes the run where it was last seen when a null comes after a gap", () => {
  const s = play([
    [0, seen("a.com")],
    [10 * S, seen("a.com")],
    [10 * S + L.GAP_MS + 1, null],
  ]);
  assert.equal(s.open, null);
  assert.equal(s.queue[0].end, 10 * S);
});

test("observe with null and nothing open changes nothing", () => {
  const s = play([
    [0, null],
    [5 * S, null],
  ]);
  assert.deepEqual(s, L.emptyState());
});

test("observe ends a run when the host changes and starts the next at once", () => {
  const s = play([
    [0, seen("a.com")],
    [10 * S, seen("a.com")],
    [15 * S, seen("b.com", "B")],
  ]);
  assert.deepEqual(s.queue, [
    { seq: 1, host: "a.com", title: "", start: 0, end: 15 * S },
  ]);
  assert.deepEqual(s.open, {
    host: "b.com",
    title: "B",
    start: 15 * S,
    end: 15 * S,
  });
});

test("observe lets go of a run that lasted no time at all", () => {
  const s = play([
    [0, seen("a.com")],
    [0, null],
  ]);
  assert.deepEqual(s, L.emptyState());
  const t = play([
    [0, seen("a.com")],
    [L.GAP_MS + 1, seen("b.com")],
  ]);
  assert.equal(t.queue.length, 0);
  assert.equal(t.open.host, "b.com");
});

test("observe does not join across a clock that went backwards", () => {
  const s = play([
    [100 * S, seen("a.com")],
    [110 * S, seen("a.com")],
    [50 * S, seen("a.com")],
  ]);
  assert.deepEqual(
    s.queue.map((r) => [r.start, r.end]),
    [[100 * S, 110 * S]],
  );
  assert.deepEqual([s.open.start, s.open.end], [50 * S, 50 * S]);
});

test("observe splits a run once it has lasted MAX_RUN_MS", () => {
  const steps = [];
  for (let t = 0; t <= L.MAX_RUN_MS; t += 30 * S)
    steps.push([t, seen("video.example", "Film")]);
  const s = play(steps);
  assert.deepEqual(s.queue, [
    {
      seq: 1,
      host: "video.example",
      title: "Film",
      start: 0,
      end: L.MAX_RUN_MS,
    },
  ]);
  assert.deepEqual(s.open, {
    host: "video.example",
    title: "Film",
    start: L.MAX_RUN_MS,
    end: L.MAX_RUN_MS,
  });
  // And carries on joining after the split.
  const t = L.observe(s, L.MAX_RUN_MS + 30 * S, seen("video.example"));
  assert.equal(t.open.end - t.open.start, 30 * S);
});

test("observe treats an observation without a host as null", () => {
  const s = play([
    [0, seen("a.com")],
    [10 * S, { host: "", title: "x" }],
  ]);
  assert.equal(s.open, null);
  assert.equal(s.queue[0].end, 10 * S);
});

test("observe cuts long titles", () => {
  const s = L.observe(
    L.emptyState(),
    0,
    seen("a.com", "x".repeat(L.MAX_TITLE + 100)),
  );
  assert.equal(s.open.title.length, L.MAX_TITLE);
});

test("observe leaves the state it was given alone", () => {
  const before = play([
    [0, seen("a.com")],
    [10 * S, seen("a.com")],
  ]);
  const copy = JSON.parse(JSON.stringify(before));
  L.observe(before, 20 * S, seen("b.com"));
  L.observe(before, 20 * S, null);
  assert.deepEqual(before, copy);
});

test("the queue keeps the newest QUEUE_CAP runs and counts what it dropped", () => {
  const extra = 5;
  const steps = [];
  for (let i = 0; i <= L.QUEUE_CAP + extra; i++) {
    steps.push([i * S, seen(i % 2 ? "b.com" : "a.com")]);
  }
  const s = play(steps);
  assert.equal(L.QUEUE_CAP, 2000);
  assert.equal(s.queue.length, L.QUEUE_CAP);
  assert.equal(s.dropped, extra);
  assert.equal(s.seq, L.QUEUE_CAP + extra);
  // The oldest went.
  assert.equal(s.queue[0].seq, extra + 1);
  assert.equal(s.queue[0].start, extra * S);
  assert.equal(s.queue[s.queue.length - 1].seq, L.QUEUE_CAP + extra);
});

test("normalizeState makes a damaged or missing state whole", () => {
  assert.deepEqual(L.normalizeState(undefined), L.emptyState());
  assert.deepEqual(L.normalizeState("nonsense"), L.emptyState());
  assert.deepEqual(
    L.normalizeState({
      open: { host: "a.com" },
      queue: [{ host: "b.com" }, 7],
      seq: "x",
    }),
    L.emptyState(),
  );
  const good = {
    open: null,
    queue: [{ seq: 1, host: "a", title: "", start: 0, end: 5 }],
    seq: 1,
    dropped: 0,
  };
  assert.deepEqual(L.normalizeState(good), good);
});

// ----- drain and acknowledge ----------------------------------------------------------

// Three closed runs (a 0-10, b 10-20, c 20-30) and d open from 30 to 40.
function busy() {
  return play([
    [0, seen("a.com", "A")],
    [10 * S, seen("b.com", "B")],
    [20 * S, seen("c.com", "C")],
    [30 * S, seen("d.com", "D")],
    [40 * S, seen("d.com", "D")],
  ]);
}

test("drain hands out closed runs, oldest first, and the open run as it stands", () => {
  const s = busy();
  const { beats, upto, state } = L.drain(s, 10);
  assert.deepEqual(beats, [
    { host: "a.com", title: "A", start: 0, end: 10 * S },
    { host: "b.com", title: "B", start: 10 * S, end: 20 * S },
    { host: "c.com", title: "C", start: 20 * S, end: 30 * S },
    { host: "d.com", title: "D", start: 30 * S, end: 40 * S },
  ]);
  assert.equal(upto, 3);
  assert.deepEqual(state.queue, []);
  // The open run stays open: it is still going.
  assert.deepEqual(state.open, s.open);
});

test("drain stops at maxBeats and leaves the open run for later", () => {
  const { beats, upto, state } = L.drain(busy(), 2);
  assert.deepEqual(
    beats.map((b) => b.host),
    ["a.com", "b.com"],
  );
  assert.equal(upto, 2);
  assert.deepEqual(
    state.queue.map((r) => r.host),
    ["c.com"],
  );
});

test("drain of only an open run answers upto 0 and changes nothing", () => {
  const s = play([
    [0, seen("a.com")],
    [10 * S, seen("a.com")],
  ]);
  const { beats, upto, state } = L.drain(s, 10);
  assert.deepEqual(beats, [
    { host: "a.com", title: "", start: 0, end: 10 * S },
  ]);
  assert.equal(upto, 0);
  assert.deepEqual(state, s);
});

test("drain sends no open run that has lasted no time yet, and nothing from an empty state", () => {
  const s = L.observe(L.emptyState(), 0, seen("a.com"));
  assert.deepEqual(L.drain(s, 10).beats, []);
  assert.deepEqual(L.drain(L.emptyState(), 10), {
    beats: [],
    upto: 0,
    state: L.emptyState(),
  });
});

test("drain uses BATCH when maxBeats is missing or nonsense", () => {
  assert.equal(L.BATCH, 500);
  const steps = [];
  for (let i = 0; i <= 600; i++)
    steps.push([i * S, seen(i % 2 ? "b.com" : "a.com")]);
  const s = play(steps);
  assert.equal(L.drain(s).beats.length, L.BATCH);
  assert.equal(L.drain(s, -1).beats.length, L.BATCH);
});

test("acknowledge removes only what was sent, keeping runs closed meanwhile", () => {
  const sent = L.drain(busy(), 10);
  // While the request was out, d ended and e began.
  const meanwhile = L.observe(busy(), 45 * S, seen("e.com"));
  const after = L.acknowledge(meanwhile, sent.upto);
  assert.deepEqual(
    after.queue.map((r) => [r.seq, r.host]),
    [[4, "d.com"]],
  );
  assert.equal(after.open.host, "e.com");
  // Acknowledging nothing changes nothing.
  assert.deepEqual(L.acknowledge(meanwhile, 0), meanwhile);
});

test("queuedCount counts waiting runs, and those still going since a moment", () => {
  const s = busy();
  assert.equal(L.queuedCount(s), 4);
  assert.equal(L.queuedCount(s, 15 * S), 3); // b ended at 20 s, so it counts
  assert.equal(L.queuedCount(s, 35 * S), 1);
  assert.equal(L.queuedCount(L.emptyState()), 0);
});

// ----- Heartbeats ------------------------------------------------------------------------

test("beatsFor makes site heartbeats with ISO instants", () => {
  const t0 = Date.UTC(2026, 10, 5, 17, 0, 0);
  const beats = L.beatsFor(
    [{ host: "github.com", title: "Repo", start: t0, end: t0 + 30 * S }],
    "Firefox",
  );
  assert.deepEqual(beats, [
    {
      kind: "site",
      app: "Firefox",
      host: "github.com",
      title: "Repo",
      start: "2026-11-05T17:00:00.000Z",
      end: "2026-11-05T17:00:30.000Z",
    },
  ]);
});

test("beatsFor sends no titles when titles are off", () => {
  const beats = L.beatsFor(
    [{ host: "a.com", title: "Secret", start: 0, end: S }],
    "Chrome",
    {
      titles: false,
    },
  );
  assert.equal(beats[0].title, "");
  assert.equal(beats[0].app, "Chrome");
  assert.deepEqual(L.beatsFor([], "Chrome"), []);
  assert.deepEqual(L.beatsFor(undefined, "Chrome"), []);
});

test("beatsFor round-trips through drain and JSON as the wire wants", () => {
  const wire = JSON.parse(
    JSON.stringify(L.beatsFor(L.drain(busy(), 10).beats, "Edge")),
  );
  assert.equal(wire.length, 4);
  for (const beat of wire) {
    assert.deepEqual(Object.keys(beat).sort(), [
      "app",
      "end",
      "host",
      "kind",
      "start",
      "title",
    ]);
    assert.equal(beat.kind, "site");
    assert.ok(Date.parse(beat.end) >= Date.parse(beat.start));
  }
});

test("heartbeatBody leaves device to the app on loopback and names it elsewhere", () => {
  const beats = [{ kind: "site" }];
  assert.deepEqual(
    L.heartbeatBody(
      { endpoint: "http://127.0.0.1:7399", device: "laptop" },
      "firefox",
      beats,
    ),
    { collector: "firefox", beats },
  );
  assert.deepEqual(
    L.heartbeatBody(
      { endpoint: "https://box.tail1234.ts.net", device: "laptop" },
      "chrome",
      beats,
    ),
    { collector: "chrome", beats, device: "laptop" },
  );
});

test("backoffMs doubles from 30 seconds up to 10 minutes", () => {
  assert.equal(L.backoffMs(0), 30 * S);
  assert.equal(L.backoffMs(1), 30 * S);
  assert.equal(L.backoffMs(2), 60 * S);
  assert.equal(L.backoffMs(3), 120 * S);
  assert.equal(L.backoffMs(5), 480 * S);
  assert.equal(L.backoffMs(6), 600 * S);
  assert.equal(L.backoffMs(1000), 600 * S);
});

test("lib.js also sets globalThis.ScreenTimeLib for the extension pages", () => {
  assert.equal(globalThis.ScreenTimeLib, L);
});
