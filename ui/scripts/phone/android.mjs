// Driving the real app on an Android device or emulator, over adb.
//
// `check.mjs` is the interface in desktop browsers at phone sizes; this is the
// app itself -- the Rust shell, Android's webview, real touches, the back
// button -- for the scripts that test it there (`android-voice.mjs`,
// `android-sweep.mjs`). Needs `adb` on PATH (`. scripts/android-env.sh`), a
// device or emulator attached, and a debug build installed, whose webview
// can be debugged.

import { _android as android } from 'playwright-core'
import { execFileSync } from 'node:child_process'

export const PACKAGE = 'app.karmayogi.KarmaYoga'

export function adb(...args) {
  return execFileSync('adb', args, { encoding: 'utf8' })
}

/** Start the app afresh, and attach to its webview once the interface is
 *  up. Answers `{ device, page, close }`. */
export async function launch({ fresh = true } = {}) {
  if (fresh) adb('shell', 'am', 'force-stop', PACKAGE)
  adb('shell', 'am', 'start', '-n', `${PACKAGE}/.MainActivity`)
  const [device] = await android.devices()
  if (!device) throw new Error('no Android device or emulator is attached')
  await dismissNotResponding()
  const webview = await device.webView({ pkg: PACKAGE }, { timeout: 30000 })
  const page = await webview.page()
  return { device, page, close: () => device.close() }
}

/** Tap a native control -- a system dialog's button -- by its text, found in
 *  a dump of the screen's views. Answers whether there was one. */
export function tapNative(text) {
  let xml
  try {
    adb('shell', 'uiautomator', 'dump', '/sdcard/ky-ui.xml')
    xml = adb('shell', 'cat', '/sdcard/ky-ui.xml')
  } catch {
    return false
  }
  const pattern = text instanceof RegExp ? text : new RegExp(text)
  for (const node of xml.matchAll(/<node [^>]*>/g)) {
    const label = node[0].match(/ text="([^"]*)"/)?.[1] ?? ''
    if (!pattern.test(label)) continue
    const b = node[0].match(/bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"/)
    if (!b) continue
    const [x1, y1, x2, y2] = b.slice(1).map(Number)
    adb('shell', 'input', 'tap', String((x1 + x2) >> 1), String((y1 + y2) >> 1))
    return true
  }
  return false
}

/** "System UI isn't responding": a slow emulator, just booted, puts this up
 *  over everything. Wait it out rather than fail on it. */
export async function dismissNotResponding() {
  for (let i = 0; i < 3 && tapNative(/^Wait$/); i++) await wait(1500)
}

/** A finger's tap on the first element matching `selector`: a real touch,
 *  through Android, at its centre. */
export async function touch(page, selector) {
  const target = page.locator(selector).first()
  await target.waitFor({ state: 'visible', timeout: 15000 })
  // Visible to Playwright includes scrolled out of sight inside a list: a
  // tap at its centre would land on whatever is on screen there instead.
  await target.scrollIntoViewIfNeeded()
  const box = await target.boundingBox()
  const scale = await page.evaluate(() => window.devicePixelRatio)
  const x = Math.round((box.x + box.width / 2) * scale)
  const y = Math.round((box.y + box.height / 2) * scale)
  adb('shell', 'input', 'tap', String(x), String(y))
}

/** Android's back button. */
export function back() {
  adb('shell', 'input', 'keyevent', 'KEYCODE_BACK')
}

/** A swipe from one point to another, in CSS pixels of `page`. */
export async function swipe(page, from, to, ms = 250) {
  const scale = await page.evaluate(() => window.devicePixelRatio)
  const px = (v) => String(Math.round(v * scale))
  adb('shell', 'input', 'swipe', px(from.x), px(from.y), px(to.x), px(to.y), String(ms))
}

export const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms))
