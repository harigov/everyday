// Every Day Screen time: what the options page and the popup share. Both
// talk only to the background page, which owns the connection and the runs.

"use strict";

(function (root) {
  const ext = root.browser || root.chrome;

  // Ask the background page something (see `handlers` in background.js).
  // Answers its reply, or throws an Error with its message.
  async function ask(type, extra) {
    const reply = await ext.runtime.sendMessage({ type, ...(extra || {}) });
    if (!reply) throw new Error("The extension did not answer. Try again.");
    if (reply.ok === false)
      throw new Error(reply.error || "Something went wrong.");
    return reply;
  }

  // "just now", "3 min ago", "2 h ago", else a date and time.
  function ago(ms, now) {
    if (!ms) return "never";
    const secs = Math.round(((now || Date.now()) - ms) / 1000);
    if (secs < 45) return "just now";
    if (secs < 3600) return Math.round(secs / 60) + " min ago";
    if (secs < 12 * 3600) return Math.round(secs / 3600) + " h ago";
    return new Date(ms).toLocaleString();
  }

  // One line on what is being counted right now.
  function liveText(status) {
    if (!status.connected) return "Not connected to Every Day";
    const live = status.live;
    if (!live) return "Not counting yet";
    switch (live.reason) {
      case "counting":
        return "Counting " + live.host;
      case "audible":
        return (
          "Counting " + live.host + ": it is playing sound while you are away"
        );
      case "unfocused":
        return "Not counting: the browser is not focused";
      case "idle":
        return "Not counting: you are idle";
      case "locked":
        return "Not counting: the screen is locked";
      case "private":
        return "Not counting: private windows are never counted";
      case "not-a-site":
        return "Not counting: this tab is not a web page";
      default:
        return "Not counting";
    }
  }

  // Where the extension sends, in words.
  function destination(status) {
    if (!status.connected) return "";
    return status.loopback
      ? "the Every Day app on this computer"
      : status.endpoint;
  }

  root.ScreenTimeUi = { ext, ask, ago, liveText, destination };
})(globalThis);
