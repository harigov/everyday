#!/usr/bin/env node
// The phone check: every app's screens at phone sizes, in Chromium (Android's
// WebView) and WebKit (the iPhone's), on the mock backend, with the glitches
// a script can see reported and a screenshot of each screen kept.
//
//   node scripts/phone/check.mjs                      every app, both engines, three sizes
//   node scripts/phone/check.mjs --apps mail,todo     only these apps' scenarios
//   node scripts/phone/check.mjs --engines chromium --sizes 360x780
//   node scripts/phone/check.mjs --only mail/thread   one scenario
//   node scripts/phone/check.mjs --url http://localhost:5417   a dev server already running
//   node scripts/phone/check.mjs --theme dark --out /tmp/phone
//   node scripts/phone/check.mjs --desktop --out /tmp/before   the same scenarios at a
//       desktop size, as the desktop app shows them, for compare.py: phone rules
//       must leave the desktop exactly as it was
//
// What it finds, per screen (see `inspect` below):
//   overflow   something visible past the left or right edge of the screen
//              that no box clips -- a row too wide, a toolbar cut off, a pane
//              that slid out and left the page scrolling sideways. Fails.
//   under-bar  the last thing in a scrolling list cannot be scrolled clear of
//              the bottom bar. Fails.
//   errors     an exception or a console error on the page. Fails.
//   small      a control smaller than 24 by 24 px: too small for a finger
//              (WCAG 2.2's minimum). Reported, not failed.
//   covered    a control another element sits on top of, so a tap lands on
//              the other one. Reported, not failed: an open dialog covers
//              what is under it on purpose.
//
// The scenarios -- what to open, press and type to reach each screen -- are
// in `scenarios.mjs`, one list per app. Screenshots and `report.json` go to
// `--out` (default: a fresh folder under the system's temporary directory,
// printed at the end). Exits non-zero when anything fails.
//
// Uses `playwright-core` and the browsers Playwright keeps in
// ~/.cache/ms-playwright; it downloads nothing. See docs/mobile.md.

import { chromium, webkit } from 'playwright-core'
import { spawn } from 'node:child_process'
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { SCENARIOS } from './scenarios.mjs'

const UI = join(dirname(fileURLToPath(import.meta.url)), '..', '..')

function arg(name, fallback) {
  const i = process.argv.indexOf(`--${name}`)
  return i >= 0 ? process.argv[i + 1] : fallback
}
const list = (v) =>
  v
    ? v
        .split(',')
        .map((s) => s.trim())
        .filter(Boolean)
    : null

const ENGINES = { chromium, webkit }
const desktop = process.argv.includes('--desktop')
// Read by `helpers.mjs`, which clicks rather than taps without a touchscreen.
globalThis.PHONE_CHECK_DESKTOP = desktop
const engines = list(arg('engines')) ?? (desktop ? ['chromium'] : ['chromium', 'webkit'])
// A small Android phone, an iPhone 15/16, a large Android phone -- or, with
// --desktop, a laptop's window.
const defaultSizes = desktop ? ['1280x800'] : ['360x780', '393x852', '412x915']
const sizes = (list(arg('sizes')) ?? defaultSizes).map((s) => {
  const [width, height] = s.split('x').map(Number)
  return { width, height, label: s }
})
const apps = list(arg('apps'))
const only = list(arg('only'))
const theme = arg('theme', 'light')
const out = arg('out') ?? mkdtempSync(join(tmpdir(), 'phone-check-'))
const strict = process.argv.includes('--strict')

async function freePort() {
  return new Promise((resolve) => {
    const server = createServer()
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address()
      server.close(() => resolve(port))
    })
  })
}

/** A dev server of our own, on a port nobody else holds: a worktree's
 *  usual 5273 is often another worktree's. */
async function startVite() {
  const port = await freePort()
  const child = spawn(
    process.execPath,
    [join(UI, 'node_modules/vite/bin/vite.js'), '--port', String(port), '--strictPort'],
    { cwd: UI, stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, BROWSER: 'none' } },
  )
  let log = ''
  child.stdout.on('data', (d) => (log += d))
  child.stderr.on('data', (d) => (log += d))
  const url = `http://127.0.0.1:${port}`
  for (let i = 0; i < 200; i++) {
    if (child.exitCode !== null) throw new Error(`vite exited:\n${log}`)
    try {
      const res = await fetch(url)
      if (res.ok) return { url, stop: () => child.kill() }
    } catch {
      // not up yet
    }
    await new Promise((r) => setTimeout(r, 150))
  }
  child.kill()
  throw new Error(`vite did not start:\n${log}`)
}

/** Console errors that are this harness's own doing, not the page's.
 *  Playwright's stopped clock (`context.clock`) injects a script into every
 *  frame, and a mail body or a feed story is a sandboxed frame that runs no
 *  script, so it refuses it; WebKit warns, at error level, about a viewport
 *  key it does not know if one is ever added back (`interactive-widget`). */
const NOT_THE_PAGES = ["Blocked script execution in 'about:srcdoc'", 'interactive-widget']

/** Open the interface on `section` as the phone shell would show it, and
 *  unlock the mock vault. */
export async function openApp(page, url, section) {
  const params = new URLSearchParams({ theme, section })
  if (!desktop) params.set('mobile', '')
  await page.goto(`${url}/?${params}`)
  const password = page.locator('input[type=password]')
  await password.waitFor({ timeout: 15000 })
  await password.fill('karmayoga')
  await password.press('Enter')
  await page.locator('.shell').waitFor({ timeout: 15000 })
  await settle(page)
}

/** Long enough for a store's first load and a transition to finish. */
export async function settle(page, ms = 400) {
  await page.waitForLoadState('networkidle').catch(() => {})
  await page.waitForTimeout(ms)
}

/** Runs in the page: what a script can see wrong with the screen. */
export function inspect() {
  // The screen's width, not the window's: something wider than a phone's
  // screen makes its browser lay the page out wider and zoom out to fit,
  // and `innerWidth` grows with it -- measured against that, nothing would
  // ever be past the edge.
  const vw = Math.min(window.innerWidth, screen.width)
  const vh = window.innerHeight
  const name = (el) => {
    const parts = []
    for (let e = el, i = 0; e && e !== document.body && i < 4; e = e.parentElement, i++) {
      let p = e.tagName.toLowerCase()
      const cls = [...e.classList].filter((c) => !c.startsWith('svelte-')).slice(0, 2)
      if (cls.length) p += '.' + cls.join('.')
      const label = e.getAttribute('aria-label')
      if (label && i === 0) p += `[aria-label="${label.slice(0, 30)}"]`
      parts.unshift(p)
    }
    return parts.join(' > ')
  }
  const shown = (el) => {
    const s = getComputedStyle(el)
    if (s.display === 'none' || s.visibility === 'hidden' || Number(s.opacity) === 0) return false
    const r = el.getBoundingClientRect()
    return r.width > 1 && r.height > 1
  }
  const visibleChain = (el) => {
    for (let e = el; e && e !== document.documentElement; e = e.parentElement) {
      const s = getComputedStyle(e)
      if (s.display === 'none' || s.visibility === 'hidden' || Number(s.opacity) === 0) return false
    }
    return true
  }
  // Why something past the screen's edge matters, or null when it does not.
  // Hidden on purpose: wholly outside the box that clips it (a panel slid
  // away), clipped inside the screen (an ellipsis), or in a short strip that
  // scrolls sideways on purpose. Cut off: clipped by a box that reaches the
  // screen's edge -- the app's own frame, usually -- so the screen cuts it.
  const why = (el, r) => {
    for (let a = el.parentElement; a && a !== document.documentElement; a = a.parentElement) {
      const s = getComputedStyle(a)
      if (s.overflowX === 'visible') continue
      const ar = a.getBoundingClientRect()
      if (r.right <= ar.left + 1 || r.left >= ar.right - 1) return null
      const scrolls = /(auto|scroll)/.test(s.overflowX)
      if (scrolls && ar.height < 150) return null
      const atEdge = (r.right > vw + 1 && ar.right >= vw - 1) || (r.left < -1 && ar.left <= 1)
      if (!atEdge) return null
      return scrolls ? 'scrolls sideways' : 'cut off'
    }
    return 'past the edge'
  }

  const overflow = []
  if (window.innerWidth > screen.width + 1) {
    // What widened it: the outermost things past the screen's edge that no
    // box clips, shown or not -- an invisible menu parked off to the side
    // widens a page as well as a visible row does. Not anything fixed, which
    // is sized by the page rather than sizing it.
    const culprits = []
    for (const el of document.body.querySelectorAll('*')) {
      const r = el.getBoundingClientRect()
      if (r.right <= screen.width + 1 || r.width === 0) continue
      // Something placed `absolute` is clipped only by the box it is placed
      // in and those around that, not by a plain box between: a scrolling
      // toolbar's hidden label still reaches out of it.
      // Nor by the body's own overflow, which is the window's unless <html>
      // has one: a phone's browser lays the page out wider all the same.
      const top =
        getComputedStyle(document.documentElement).overflowX === 'visible'
          ? document.body
          : document.documentElement
      let clipped = false
      let placed = getComputedStyle(el).position === 'absolute'
      for (let a = el; a && a !== top; a = a.parentElement) {
        const s = getComputedStyle(a)
        if (s.position === 'fixed') {
          clipped = true
          break
        }
        if (a === el) continue
        const holds = s.position !== 'static' || s.transform !== 'none'
        if (s.overflowX !== 'visible' && (!placed || holds)) {
          clipped = true
          break
        }
        if (placed && holds) placed = s.position === 'absolute'
      }
      if (clipped || culprits.some((c) => c.contains(el))) continue
      culprits.push(el)
    }
    overflow.push({
      el: 'html',
      reason: `laid out wider than the screen by ${culprits.map(name).join(', ') || 'something'}`,
      left: 0,
      right: window.innerWidth,
      width: window.innerWidth,
    })
  }
  const reported = new Set()
  for (const el of document.body.querySelectorAll('*')) {
    if (!shown(el)) continue
    const r = el.getBoundingClientRect()
    if (r.bottom <= 0 || r.top >= vh) continue
    if (r.right <= vw + 1 && r.left >= -1) continue
    if (!visibleChain(el)) continue
    const reason = why(el, r)
    if (!reason) continue
    // Only the outermost offender: its children overflow because it does.
    let parentReported = false
    for (let a = el.parentElement; a; a = a.parentElement) {
      if (reported.has(a)) {
        parentReported = true
        break
      }
    }
    reported.add(el)
    if (parentReported) continue
    overflow.push({
      el: name(el),
      reason,
      left: Math.round(r.left),
      right: Math.round(r.right),
      width: Math.round(r.width),
    })
  }

  const CONTROLS =
    'button, a[href], input:not([type=hidden]), select, textarea, [role=button], [role=tab], [role=menuitem], [role=checkbox], [role=switch], [role=option]'
  const small = []
  const covered = []
  for (const el of document.querySelectorAll(CONTROLS)) {
    if (!shown(el) || !visibleChain(el)) continue
    const r = el.getBoundingClientRect()
    if (r.bottom <= 0 || r.top >= vh || r.right <= 0 || r.left >= vw) continue
    // A checkbox or radio inside its label is tapped through the label.
    const viaLabel = el.closest('label')
    const box = viaLabel ? viaLabel.getBoundingClientRect() : r
    if (box.width < 24 || box.height < 24) {
      small.push({ el: name(el), w: Math.round(box.width), h: Math.round(box.height) })
    }
    const cx = r.left + r.width / 2
    const cy = r.top + r.height / 2
    if (cx >= 0 && cx < vw && cy >= 0 && cy < vh) {
      const hit = document.elementFromPoint(cx, cy)
      if (hit && !el.contains(hit) && !hit.contains(el) && !(viaLabel && viaLabel.contains(hit))) {
        covered.push({ el: name(el), by: name(hit) })
      }
    }
  }

  // The bottom bar, where there is one: a list has to scroll its last row
  // clear of it.
  const bar = document.querySelector('[data-bottom-bar]')
  const limit = bar && shown(bar) ? bar.getBoundingClientRect().top : vh
  const underBar = []
  for (const sc of document.querySelectorAll('*')) {
    const s = getComputedStyle(sc)
    if (!/(auto|scroll)/.test(s.overflowY) || sc.scrollHeight <= sc.clientHeight + 2) continue
    if (!shown(sc) || !visibleChain(sc)) continue
    const before = sc.scrollTop
    sc.scrollTop = sc.scrollHeight
    let last = null
    for (const child of sc.querySelectorAll('*')) {
      if (!shown(child) || child.children.length > 0) continue
      const r = child.getBoundingClientRect()
      if (!last || r.bottom > last.bottom) last = { r, el: child, bottom: r.bottom }
    }
    const box = sc.getBoundingClientRect()
    // The bar's top where the bar is drawn over the list, the screen's foot
    // where it is not: a drawer or a sheet over the bar has nothing under it
    // but still has to fit on the screen.
    const over = bar && document.elementFromPoint(box.left + box.width / 2, limit + 4)
    const end = over && bar.contains(over) ? limit : vh
    if (last && last.bottom > end + 1 && box.bottom > end + 1) {
      underBar.push({
        scroller: name(sc),
        last: name(last.el),
        bottom: Math.round(last.bottom),
        limit: Math.round(end),
      })
    }
    sc.scrollTop = before
  }

  return {
    overflow,
    underBar,
    small,
    covered,
    pageScroll: document.scrollingElement.scrollWidth - vw,
  }
}

async function main() {
  const server = arg('url') ? { url: arg('url'), stop: () => {} } : await startVite()
  const results = []
  let failed = 0
  try {
    for (const engineName of engines) {
      const browser = await ENGINES[engineName].launch()
      try {
        for (const size of sizes) {
          const context = await browser.newContext({
            viewport: { width: size.width, height: size.height },
            deviceScaleFactor: 2,
            // Chromium honours a phone's meta viewport and touch with
            // these; WebKit takes touch.
            ...(engineName === 'chromium' && !desktop ? { isMobile: true } : {}),
            hasTouch: !desktop,
          })
          // The same screens every run, so two runs can be compared
          // (`compare.py`): the mock's clock stopped at one moment -- its
          // "today", "2 hours ago" and the calendar's now-line with it --
          // and its shuffles seeded. Timers still run.
          await context.clock.setFixedTime(new Date('2026-11-25T10:30:00'))
          await context.addInitScript(() => {
            let seed = 42
            Math.random = () => {
              seed = (seed + 0x6d2b79f5) | 0
              let t = Math.imul(seed ^ (seed >>> 15), 1 | seed)
              t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
              return ((t ^ (t >>> 14)) >>> 0) / 4294967296
            }
          })
          for (const [app, scenarios] of Object.entries(SCENARIOS)) {
            if (apps && !apps.includes(app)) continue
            for (const scenario of scenarios) {
              const id = `${app}/${scenario.name}`
              if (only && !only.includes(id)) continue
              const page = await context.newPage()
              const errors = []
              page.on('pageerror', (e) => errors.push(`exception: ${e.message}`))
              page.on('console', (m) => {
                if (m.type() === 'error' && !NOT_THE_PAGES.some((n) => m.text().includes(n)))
                  errors.push(`console: ${m.text().slice(0, 300)}`)
              })
              let found
              let failure = null
              try {
                await openApp(page, server.url, scenario.section ?? app)
                await scenario.run?.({
                  page,
                  settle: (ms) => settle(page, ms),
                  size,
                  engine: engineName,
                })
                await settle(page, 250)
                const dir = join(out, engineName, size.label)
                mkdirSync(dir, { recursive: true })
                await page.screenshot({ path: join(dir, `${app}--${scenario.name}.png`) })
                found = await page.evaluate(inspect)
              } catch (e) {
                failure = String(e.message ?? e).split('\n')[0]
                const dir = join(out, engineName, size.label)
                mkdirSync(dir, { recursive: true })
                await page
                  .screenshot({ path: join(dir, `${app}--${scenario.name}--FAILED.png`) })
                  .catch(() => {})
              }
              await page.close()
              const bad =
                failure !== null ||
                errors.length > 0 ||
                (found && (found.overflow.length > 0 || found.underBar.length > 0)) ||
                (strict && found && (found.small.length > 0 || found.covered.length > 0))
              if (bad) failed++
              results.push({ engine: engineName, size: size.label, id, failure, errors, ...found })
              const counts = found
                ? `overflow ${found.overflow.length}  under-bar ${found.underBar.length}  errors ${errors.length}  small ${found.small.length}  covered ${found.covered.length}`
                : `FAILED: ${failure}`
              console.log(
                `${bad ? 'FAIL' : 'ok  '} ${engineName.padEnd(8)} ${size.label.padEnd(8)} ${id.padEnd(34)} ${counts}`,
              )
              if (bad) {
                for (const o of found?.overflow ?? [])
                  console.log(`       overflow  ${o.el}  ${o.reason} (${o.left}..${o.right})`)
                for (const u of found?.underBar ?? [])
                  console.log(
                    `       under-bar ${u.scroller}: ${u.last} ends at ${u.bottom}, bar at ${u.limit}`,
                  )
                for (const e of errors.slice(0, 5)) console.log(`       ${e}`)
              }
            }
          }
          await context.close()
        }
      } finally {
        await browser.close()
      }
    }
  } finally {
    server.stop()
  }
  writeFileSync(join(out, 'report.json'), JSON.stringify(results, null, 2))
  console.log(
    `\n${results.length - failed}/${results.length} screens clean. Screenshots and report.json in ${out}`,
  )
  process.exit(failed > 0 ? 1 : 0)
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  main().catch((e) => {
    console.error(e)
    process.exit(2)
  })
}
