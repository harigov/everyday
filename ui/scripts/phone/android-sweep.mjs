#!/usr/bin/env node
// Every app in the real app, on an Android emulator or phone: what the
// browser check (`check.mjs`) cannot see -- Android's own webview, real
// touches and flings, the back button, the keyboard, turning the phone,
// leaving the app and coming back, how long it takes to start and how
// smoothly it scrolls.
//
//   node scripts/phone/android-sweep.mjs [--out DIR] [--apps notes,todo]
//
// Needs a debug build installed and paired with a vault that has something
// in it (`scripts/phone-test-vault.sh` and enough tasks, notes and entries to
// scroll -- see docs/mobile.md). Writes a screenshot of every screen and
// `sweep.json` to --out, and prints a summary. Exits non-zero when a screen
// overflows, a list cannot be scrolled clear of the bar, the page throws, or
// back does not do what it should.

import { mkdirSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { inspect } from './check.mjs'
import { PACKAGE, adb, back, dismissNotResponding, launch, swipe, touch, wait } from './android.mjs'

const arg = (name, fallback) => {
  const i = process.argv.indexOf(`--${name}`)
  return i >= 0 ? process.argv[i + 1] : fallback
}
const out = arg('out', join(tmpdir(), 'ky-sweep'))
mkdirSync(out, { recursive: true })

// Every app, by the label on its button.
const APPS = [
  ['overview', 'Overview'],
  ['journal', 'Journal'],
  ['notes', 'Notes'],
  ['todo', 'Todo'],
  ['calendar', 'Calendar'],
  ['mail', 'Mail'],
  ['library', 'Library'],
  ['feed', 'Feed'],
  ['files', 'Files'],
  ['health', 'Health'],
  ['money', 'Money'],
  ['screentime', 'Screen time'],
  ['assistant', 'Assistant'],
]
const only = arg('apps')?.split(',')

// What opening "the first thing" in an app's list means, tried in order.
const FIRST_ITEM = {
  journal: ['.entries [role=option]', '[role=option]'],
  notes: ['[data-note]', '.notes-home [role=option]', '.note-row', '[role=option]'],
  todo: ['.task-row .title', '[data-task]', '.task-row'],
  calendar: ['.agenda button.row.event', '.agenda button.row', '.block'],
  assistant: ['.thread-row', '.conversations button.row', '[data-conversation]'],
}

const report = { started: new Date().toISOString(), screens: [], checks: [] }
let failures = 0
const fail = (what) => {
  failures++
  console.log(`FAIL ${what}`)
}

// ── Starting ──────────────────────────────────────────────────────────────
adb('shell', 'am', 'force-stop', PACKAGE)
await wait(1500)
const started = Date.now()
const am = adb('shell', 'am', 'start', '-W', '-n', `${PACKAGE}/.MainActivity`)
const firstFrame = Number(am.match(/TotalTime: (\d+)/)?.[1] ?? NaN)
const { device, page, close } = await launch({ fresh: false })
await page.locator('.shell').waitFor({ timeout: 60000 })
const ready = Date.now() - started
const pss = Number(
  adb('shell', 'dumpsys', 'meminfo', PACKAGE).match(/TOTAL PSS:\s+(\d+)/)?.[1] ?? NaN,
)
report.start = {
  firstFrameMs: firstFrame,
  interfaceReadyMs: ready,
  memoryMb: Math.round(pss / 1024),
}
console.log(
  `start: first frame ${firstFrame} ms, interface ready ${ready} ms, ${Math.round(pss / 1024)} MB`,
)

const errors = []
page.on('console', (m) => {
  if (m.type() === 'error') errors.push(m.text().slice(0, 200))
})
page.on('pageerror', (e) => errors.push(`exception: ${e.message.slice(0, 200)}`))

async function dismissKeyboard() {
  if (await page.evaluate(() => document.documentElement.hasAttribute('data-keyboard'))) {
    back()
    await wait(800)
  }
}

/** Go to an app the way a person would: its tab, or More then its tile. */
async function openApp(label) {
  await dismissKeyboard()
  const tab = page.locator(`[data-bottom-bar] button:has-text("${label}")`).first()
  if (await tab.isVisible().catch(() => false)) {
    await touch(page, `[data-bottom-bar] button:has-text("${label}")`)
  } else {
    await touch(page, '[data-bottom-bar] button:has-text("More")')
    await wait(700)
    await touch(page, `[role=dialog] button:has-text("${label}")`)
  }
  await wait(1800)
}

/** Fling the biggest visible list three times and time every frame. */
async function scrollFrames() {
  const box = await page.evaluate(() => {
    let best = null
    for (const el of document.querySelectorAll('*')) {
      const s = getComputedStyle(el)
      if (!/(auto|scroll)/.test(s.overflowY) || el.scrollHeight <= el.clientHeight + 40) continue
      const r = el.getBoundingClientRect()
      if (r.width < 100 || r.height < 200) continue
      if (!best || r.width * r.height > best.area)
        best = { x: r.x, y: r.y, w: r.width, h: r.height, area: r.width * r.height }
    }
    return best
  })
  if (!box) return null
  await page.evaluate(() => {
    window.__frames = []
    let last = performance.now()
    const tick = (now) => {
      window.__frames.push(now - last)
      last = now
      if (window.__frames.length < 2000) window.__frameLoop = requestAnimationFrame(tick)
    }
    window.__frameLoop = requestAnimationFrame(tick)
  })
  const x = box.x + box.w / 2
  for (let i = 0; i < 3; i++) {
    await swipe(page, { x, y: box.y + box.h * 0.85 }, { x, y: box.y + box.h * 0.15 }, 120)
    await wait(900)
  }
  const frames = await page.evaluate(() => {
    cancelAnimationFrame(window.__frameLoop)
    return window.__frames.slice(1)
  })
  if (frames.length < 10) return null
  const janky = frames.filter((d) => d > 34).length
  const mean = frames.reduce((a, b) => a + b, 0) / frames.length
  return {
    frames: frames.length,
    fps: Math.round(1000 / mean),
    jankPercent: Math.round((100 * janky) / frames.length),
    worstMs: Math.round(Math.max(...frames)),
  }
}

async function screen(id) {
  await wait(400)
  await device.screenshot({ path: join(out, `${id}.png`) })
  const found = await page.evaluate(inspect)
  const entry = { id, errors: errors.splice(0), ...found }
  report.screens.push(entry)
  const bad = found.overflow.length + found.underBar.length + entry.errors.length
  if (bad) {
    fail(
      `${id}: overflow ${found.overflow.length}, under-bar ${found.underBar.length}, errors ${entry.errors.length}`,
    )
    for (const o of found.overflow.slice(0, 3)) console.log(`       overflow ${o.el} ${o.reason}`)
    for (const e of entry.errors.slice(0, 3)) console.log(`       ${e}`)
  }
  return entry
}

// ── Every app ────────────────────────────────────────────────────────────
for (const [section, label] of APPS) {
  if (only && !only.includes(section)) continue
  try {
    await openApp(label)
    const entry = await screen(`${section}--home`)
    entry.scroll = await scrollFrames()
    const s = entry.scroll
    console.log(
      `${section.padEnd(11)} small ${String(entry.small.length).padStart(3)}  ` +
        (s
          ? `scroll ${s.fps} fps, ${s.jankPercent}% janky frames, worst ${s.worstMs} ms`
          : 'nothing to scroll'),
    )

    // Something in its list opened, and back to the list.
    for (const selector of FIRST_ITEM[section] ?? []) {
      if (
        !(await page
          .locator(selector)
          .first()
          .isVisible()
          .catch(() => false))
      )
        continue
      await touch(page, selector)
      await wait(1500)
      const opened = await page
        .locator('.phone-back')
        .first()
        .isVisible()
        .catch(() => false)
      if (!opened) {
        // Back now would be back out of the app: say so, rather than test it.
        console.log(`       ${section}: tapping ${selector} opened nothing`)
        break
      }
      await screen(`${section}--item`)
      back()
      await wait(1200)
      const closed = !(await page
        .locator('.phone-back')
        .first()
        .isVisible()
        .catch(() => false))
      report.checks.push({ id: `${section}: back closes what was opened`, ok: !opened || closed })
      if (opened && !closed) fail(`${section}: back did not close what was opened`)
      break
    }
  } catch (e) {
    fail(`${section}: ${String(e.message).split('\n')[0]}`)
    await device.screenshot({ path: join(out, `${section}--FAILED.png`) }).catch(() => {})
    await dismissNotResponding()
  }
}

// ── The frame ────────────────────────────────────────────────────────────
try {
  await openApp('Todo')
  await touch(page, 'button[aria-label="Show the sidebar"]')
  await wait(900)
  await screen('frame--drawer')
  back()
  await wait(900)
  const drawerClosed = !(await page
    .locator('.sidebar.peeking')
    .isVisible()
    .catch(() => false))
  report.checks.push({ id: 'back closes the drawer', ok: drawerClosed })
  if (!drawerClosed) fail('back did not close the drawer')

  // The keyboard: a field typed into stays on screen, the bar steps aside.
  await touch(page, '[data-search]')
  await wait(1800)
  const kb = await page.evaluate(() => {
    const field = document.activeElement?.getBoundingClientRect()
    const bar = document.querySelector('[data-bottom-bar]')
    return {
      up: document.documentElement.hasAttribute('data-keyboard'),
      fieldVisible: !!field && field.bottom <= window.innerHeight && field.top >= 0,
      barHidden: !bar || getComputedStyle(bar).display === 'none',
    }
  })
  await screen('frame--keyboard')
  report.checks.push({
    id: 'keyboard: field on screen, bar out of the way',
    ok: kb.up && kb.fieldVisible && kb.barHidden,
    ...kb,
  })
  if (!(kb.up && kb.fieldVisible && kb.barHidden)) fail(`keyboard: ${JSON.stringify(kb)}`)
  back()
  await wait(1000)
  await dismissKeyboard()

  // On its side.
  adb('shell', 'settings', 'put', 'system', 'accelerometer_rotation', '0')
  adb('shell', 'settings', 'put', 'system', 'user_rotation', '1')
  await wait(2500)
  await screen('frame--landscape')
  adb('shell', 'settings', 'put', 'system', 'user_rotation', '0')
  await wait(2500)
  await screen('frame--portrait-again')

  // Leaving and coming back: the connection picks up again.
  adb('shell', 'input', 'keyevent', 'KEYCODE_HOME')
  await wait(15000)
  adb('shell', 'am', 'start', '-n', `${PACKAGE}/.MainActivity`)
  await wait(3000)
  const resumed = await page
    .locator('.shell')
    .isVisible()
    .catch(() => false)
  report.checks.push({ id: 'comes back after 15 s away', ok: resumed })
  if (!resumed) fail('the app did not come back after being left')
  await screen('frame--resumed')
} catch (e) {
  fail(`frame: ${String(e.message).split('\n')[0]}`)
}

await close()
writeFileSync(join(out, 'sweep.json'), JSON.stringify(report, null, 2))
console.log(
  `\n${failures === 0 ? 'clean' : `${failures} failure(s)`}. Screenshots and sweep.json in ${out}`,
)
process.exit(failures === 0 ? 0 : 1)
