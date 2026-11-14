// Karma Yoga Screen time: the toolbar popup. Whether the extension is
// connected, what it is counting right now, and the way to the options.

"use strict";

(function () {
  const { ext, ask, ago, liveText, destination } = globalThis.ScreenTimeUi;
  const $ = (id) => document.getElementById(id);

  $("options").addEventListener("click", (event) => {
    event.preventDefault();
    ext.runtime.openOptionsPage();
    window.close();
  });

  ask("status")
    .then((status) => {
      $("live").textContent = liveText(status);
      if (!status.connected) {
        $("where").textContent =
          "Open the options and paste the code from Settings → Screen time.";
        return;
      }
      $("where").textContent =
        "Sending to " +
        destination(status) +
        ". Last sent " +
        ago(status.lastSent) +
        ".";
      if (status.revoked) {
        $("problem").textContent =
          "The app no longer accepts this browser. Connect again in the options.";
        $("problem").hidden = false;
      } else if (status.lastError) {
        $("problem").textContent = status.lastError;
        $("problem").hidden = false;
      }
    })
    .catch((e) => {
      $("live").textContent = e.message;
    });
})();
