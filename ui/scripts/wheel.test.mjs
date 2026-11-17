// The wheel is listened for in one place, and only while it is wanted.
//
// WebKitGTK scrolls a pane on its own thread only while nothing in the window
// listens for `wheel` -- one listener anywhere, passive or not, and every
// pane scrolls only as fast as the page's script comes free (see
// `src/lib/wheel.ts`). Nothing fails when somebody adds an `onwheel` to a
// component; the window just goes back to scrolling in fits. So this file
// fails instead: no wheel listener in the interface but `hearWheelWhen`'s,
// and that one on the window only while its caller wants it.

import { readdirSync, readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join, relative } from 'node:path'
import { load, makeCheck } from './harness.mjs'

const { check, finish } = makeCheck()

const src = join(dirname(fileURLToPath(import.meta.url)), '..', 'src')
const OWN = 'lib/wheel.ts'
const LISTENS =
  /\bon(?:mouse)?wheel\s*[={]|addEventListener\(\s*['"`](?:mouse)?wheel|\bon\([^,()]+,\s*['"`]wheel/

function* files(dir) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name)
    if (entry.isDirectory()) yield* files(path)
    else if (/\.(svelte|ts|js)$/.test(entry.name)) yield path
  }
}

const offenders = []
for (const path of files(src)) {
  const name = relative(src, path)
  if (name === OWN) continue
  readFileSync(path, 'utf8')
    .split('\n')
    .forEach((line, i) => {
      if (LISTENS.test(line)) offenders.push(`${name}:${i + 1}`)
    })
}
check('no wheel listener outside lib/wheel.ts', offenders, [])

// The pattern itself, on the spellings it has to catch and the comment it
// must not.
for (const [line, want] of [
  ['<svelte:window onwheel={() => app.touch()} />', true],
  ["window.addEventListener('wheel', f, { passive: true })", true],
  ["on(window, 'wheel', f)", true],
  ['el.onmousewheel = f', true],
  ['// `virtua` puts a passive `wheel` listener on that parent', false],
]) {
  check(`pattern on ${line}`, LISTENS.test(line), want)
}

// `hearWheelWhen` against a stand-in window.
globalThis.window ??= new EventTarget()
const {
  module: { hearWheelWhen },
  close,
} = await load('/src/lib/wheel.ts')
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
const turn = () => window.dispatchEvent(new Event('wheel'))

let want = false
let heard = 0
const stop = hearWheelWhen(
  () => want,
  () => {
    heard++
    want = false
  },
  10,
)
turn()
check('not heard while not wanted', heard, 0)
want = true
await sleep(40)
turn()
check('heard once wanted', heard, 1)
turn()
check('taken off at the first turn it hears', heard, 1)
want = true
await sleep(40)
stop()
turn()
check('nothing heard once stopped', heard, 1)

await close()
finish('wheel')
