// Opening a link outside the webview -- OAuth's authorization URL, chiefly,
// but written generally enough for anything else that ever needs a real
// browser rather than the sandboxed one this interface renders in.
//
// # Why `tauri-plugin-opener` and not a loopback plugin of its own
//
// `everyday-app`'s Cargo.toml already carries `tauri-plugin-opener`, granted
// exactly one permission (`opener:allow-open-url`) in
// `crates/everyday-app/capabilities/default.json` -- nothing here adds a new
// capability, it uses one that was already narrow. The loopback that catches
// the browser's *answer* is a different concern entirely: see
// `crates/everyday-mail/src/oauth/loopback.rs`'s module doc for why that is
// a plain `TcpListener` rather than a Tauri plugin. This file is only the
// "open a tab" half.
//
// # Why this can fail on purpose
//
// There are three worlds this interface runs in, and only one of them has a
// browser to hand a URL to:
//
//   * the packaged desktop app, where `openUrl` reaches the system browser
//     through the plugin;
//   * `npm run dev`'s mock backend, running in a plain browser tab already --
//     `window.open` is the nearest equivalent a tab has to "open a browser";
//   * `everyday-server` serving this same build to a browser on another
//     machine, where there is no Tauri bridge to ask and nothing this
//     process can open on the user's behalf at all.
//
// `begin_oauth_sign_in`'s answer has to be usable in every one of those, so
// this never throws: it says whether it actually opened something, and the
// caller -- the Accounts settings screen -- shows the URL to copy when it did
// not. That is also why this returns before the sign-in itself is known to
// have worked: opening the tab and finishing the OAuth dance are different
// moments, and conflating them would make a browser that took its time to
// load look like a failure.

import { isMock } from './api'

/**
 * Open `url` in a real browser, if this process can reach one on the user's
 * behalf. Answers whether it did.
 */
export async function openExternal(url: string): Promise<boolean> {
  if (isMock) {
    const opened = window.open(url, '_blank', 'noopener,noreferrer')
    return opened !== null
  }
  try {
    const plugin = await import('@tauri-apps/plugin-opener')
    await plugin.openUrl(url)
    return true
  } catch {
    // No Tauri bridge to ask -- `everyday-server` serving this build to a
    // plain browser, most likely. Not an error: the caller's fallback is to
    // show `url` for the person to copy themselves.
    return false
  }
}
