# Karma Yoga Screen time, for Chrome and Firefox

A small browser extension that tells Karma Yoga which website had your
attention: the active tab of the **focused** browser window, while you are
**not idle** and the screen is **not locked**. Each site then shows in
Screen time as an app of its own, and the browser keeps only the time no
site claimed ("Firefox (other)").

One codebase serves Chrome, Chromium, Edge, Brave, Vivaldi, Opera and
Firefox 121 or later. Plain JavaScript, no build step, no dependencies.

## Install

### Chrome, Chromium, Edge, Brave

1. Open `chrome://extensions` (`edge://extensions`, `brave://extensions`).
2. Turn on **Developer mode**.
3. Choose **Load unpacked** and pick the `extensions/browser` folder.

Chrome warns that `background.scripts` is ignored. That key is Firefox's;
Chrome uses `background.service_worker`.

### Firefox

1. Open `about:debugging`, then **This Firefox**.
2. Choose **Load Temporary Add-on…** and pick `extensions/browser/manifest.json`.

A temporary add-on is gone when Firefox quits. Installing it for good needs
the add-on signed by Mozilla (`web-ext sign`, listed or unlisted), or a
Firefox Developer Edition or Nightly with `xpinstall.signatures.required`
set to `false`.

## Connect

1. In Karma Yoga, open **Settings → Screen time** and choose **Connect a browser**.
2. Copy the code it shows (it starts with `karmayoga-screentime:`).
3. Open the extension's options (right-click its toolbar button → Options, or
   the popup's **Options** link), paste the code and choose **Connect**.

The extension checks the code with the app before keeping it. A code for the
app on this computer points at `http://127.0.0.1:7399`. A code for a sharing
server points at its `https` address, and the browser asks once for
permission to reach it. Plain `http` is accepted only for this computer.

The options page shows where it sends, the last delivery, anything that went
wrong and how much is waiting. **Disconnect** forgets the code and everything
not yet sent. If the code is revoked in Settings, the extension stops sending
until you connect it again with a new one.

## What is sent

To the address in the code, every 30 seconds while something counts:

- the site's host (`mail.google.com`), never the rest of the address,
- the tab's title, unless Karma Yoga is set not to keep titles (the app says
  so when the extension connects, and then none are sent),
- when you started and stopped looking at it,
- the browser's name (`Firefox`, `Chrome`, ...) and, for a sharing server,
  this computer's name from the code.

## What is not

- Full addresses, page contents, history, keystrokes or screenshots.
- Tabs in windows that are not focused, background tabs, and anything while
  the browser is not the focused app.
- Anything after the idle time set in Karma Yoga (3 minutes by default), or
  while the screen is locked. A focused tab that is playing sound keeps
  counting while you are idle, so a film you are watching counts.
- Private windows, browser pages (`chrome://`, `about:`), extension pages,
  local files and the new tab page.

Nothing leaves the browser for anywhere but the address in the code. What
could not be sent waits in the browser's session storage (up to 2000 runs,
oldest dropped first) and is gone when the browser quits.

## Files

| File                                        | What it is                                                                                                                 |
| ------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| `manifest.json`                             | Manifest V3, for Chrome and Firefox alike.                                                                                 |
| `lib.js`                                    | The pure part: connection codes, which addresses count, joining looks into runs, the queue, the heartbeats. No `chrome.*`. |
| `background.js`                             | Watches focus, tabs and idle; looks every 30 s; sends.                                                                     |
| `options.html`, `options.js`, `options.css` | Connect, status, disconnect.                                                                                               |
| `popup.html`, `popup.js`                    | What is being counted right now.                                                                                           |
| `ui.js`                                     | What the options page and the popup share.                                                                                 |
| `test/lib.test.mjs`                         | Tests for `lib.js`: `node --test extensions/browser/test/lib.test.mjs`.                                                    |
| `package.json`                              | Not a package: only tells node to load `lib.js` as a script.                                                               |

The two routes it speaks, `GET /v1/screentime/hello` and
`POST /v1/screentime/heartbeats`, are described in
`docs/plans/screentime.md` under "The ingest routes". They only ever gain
optional fields, so an extension installed today keeps working with later
versions of the app.
