// Karma Yoga Screen time: the options page. Paste a connection code,
// connect, see what the extension is doing, disconnect.

"use strict";

(function () {
  const L = globalThis.ScreenTimeLib;
  const { ext, ask, ago, liveText, destination } = globalThis.ScreenTimeUi;

  const $ = (id) => document.getElementById(id);
  const form = $("connect-form");
  const code = $("code");
  const connectButton = $("connect");
  const connectError = $("connect-error");

  function show(el, text) {
    el.textContent = text || "";
    el.hidden = !text;
  }

  function render(status) {
    $("connected").hidden = !status.connected;
    $("connect-title").textContent = status.connected
      ? "Connect with a new code"
      : "Connect to Karma Yoga";
    if (!status.connected) return;

    $("live").textContent = liveText(status);
    $("dest").textContent =
      destination(status) +
      (status.version ? " (version " + status.version + ")" : "");
    $("device").textContent = status.device || "named by the app";
    $("browser").textContent =
      status.browser + ', sent as "' + status.collector + '"';
    $("last-sent").textContent = status.lastSent
      ? ago(status.lastSent) +
        (status.lastAccepted ? ", " + plural(status.lastAccepted, "run") : "")
      : "nothing yet";
    $("queued").textContent =
      plural(status.queuedToday, "run") +
      " from today" +
      (status.queued > status.queuedToday
        ? ", " + status.queued + " in all"
        : "") +
      (status.dropped
        ? ". " +
          plural(status.dropped, "older run") +
          " were let go while the app could not be reached."
        : "");
    $("titles").textContent = status.keepTitles
      ? "sent"
      : "not sent (Karma Yoga does not keep them)";
    $("idle").textContent =
      plural(status.idleMinutes, "minute") +
      " without input, as set in Karma Yoga";

    let problem = "";
    if (status.revoked) {
      problem =
        status.lastError ||
        "The app no longer accepts this connection code. Connect again with a new one.";
    } else if (status.lastError) {
      problem = status.lastError + " (" + ago(status.lastErrorAt) + ")";
    }
    show($("last-error"), problem);
  }

  function plural(n, word) {
    return n + " " + word + (n === 1 ? "" : "s");
  }

  async function refresh() {
    try {
      render(await ask("status"));
    } catch (e) {
      show(connectError, e.message);
    }
  }

  form.addEventListener("submit", (event) => {
    event.preventDefault();
    show(connectError, "");
    let parsed;
    try {
      parsed = L.parseConnectionCode(code.value);
    } catch (e) {
      show(connectError, e.message);
      code.focus();
      return;
    }

    // The permission to reach the address has to be asked for right here,
    // in the click, before anything is waited on: browsers only show the
    // prompt for a request a person's action made. For the app on this
    // computer it is already granted and nothing is shown.
    let asked;
    try {
      asked = ext.permissions.request({
        origins: [L.originPattern(parsed.endpoint)],
      });
    } catch (e) {
      asked = Promise.reject(e);
    }

    connectButton.disabled = true;
    connectButton.textContent = "Connecting…";
    Promise.resolve(asked)
      .then((granted) => {
        if (!granted) {
          throw new Error(
            "The extension needs your permission to reach " +
              parsed.endpoint +
              " before it can send there.",
          );
        }
        return ask("connect", { code: code.value });
      })
      .then((status) => {
        code.value = "";
        render(status);
      })
      .catch((e) => show(connectError, (e && e.message) || String(e)))
      .finally(() => {
        connectButton.disabled = false;
        connectButton.textContent = "Connect";
      });
  });

  $("disconnect").addEventListener("click", async () => {
    if (
      !confirm(
        "Disconnect? What is waiting to be sent is thrown away, and nothing more is counted until you connect again.",
      )
    ) {
      return;
    }
    try {
      render(await ask("disconnect"));
    } catch (e) {
      show($("last-error"), e.message);
    }
  });

  $("send").addEventListener("click", async () => {
    const button = $("send");
    button.disabled = true;
    try {
      render(await ask("send"));
    } catch (e) {
      show($("last-error"), e.message);
    } finally {
      button.disabled = false;
    }
  });

  refresh();
  // Keep "last sent" and the live line honest while the page is open.
  setInterval(() => {
    if (!document.hidden) refresh();
  }, 15 * 1000);
})();
