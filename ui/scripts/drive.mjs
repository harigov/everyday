#!/usr/bin/env node
// Run JavaScript in a window of a Jatayu build made with `--features drive`
// -- the Mac app or the iOS simulator, where no WebDriver reaches. See
// `crates/jatayu-app/src/drive.rs` for the other end and `docs/mobile.md`.
//
//   JATAYU_DRIVE=/path/to.sock node scripts/drive.mjs 'return document.title'
//   echo 'return location.href' | node scripts/drive.mjs --window main -
//
// The script is the body of an async function: `return` what you want back,
// `await` as you like. Prints the value as JSON; exits 1 if the script threw.
//
// From another script: `import { drive } from './drive.mjs'` and
// `await drive('return 1 + 1')`.

import { createConnection } from 'node:net'
import { fileURLToPath } from 'node:url'

export function drive(
  script,
  { socket = process.env.JATAYU_DRIVE, window = 'main', timeout = 75_000 } = {},
) {
  return new Promise((resolve, reject) => {
    if (!socket) {
      reject(new Error('JATAYU_DRIVE names no socket'))
      return
    }
    const conn = createConnection(socket)
    // The shell gives up on a script after a minute and says so, but an app
    // that is not running at all -- an iPhone app in the background, which
    // iOS suspends -- cannot say anything, and the wait would be forever.
    conn.setTimeout(timeout, () => {
      conn.destroy()
      reject(new Error(`no answer in ${timeout / 1000} s: is the app suspended or gone?`))
    })
    const chunks = []
    conn.on('connect', () => conn.end(`@${window}\n${script}`))
    conn.on('data', (chunk) => chunks.push(chunk))
    conn.on('error', reject)
    conn.on('end', () => {
      let answer
      try {
        answer = JSON.parse(Buffer.concat(chunks).toString('utf8'))
      } catch (e) {
        reject(new Error(`no answer from the app: ${e.message}`))
        return
      }
      // The window's own answers carry an id and their value as JSON text;
      // the shell's (no such window, no answer in time) are plain text. An
      // answer with no value at all is null rather than a crash here.
      let value = answer.value
      if (answer.id !== undefined) {
        try {
          value = value === undefined ? null : JSON.parse(value)
        } catch (e) {
          reject(new Error(`the app's answer was not JSON: ${e.message}`))
          return
        }
      }
      if (answer.ok) resolve(value)
      else reject(new Error(String(value)))
    })
  })
}

async function readStdin() {
  const chunks = []
  for await (const chunk of process.stdin) chunks.push(chunk)
  return Buffer.concat(chunks).toString('utf8')
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2)
  let window = 'main'
  const w = args.indexOf('--window')
  if (w >= 0) {
    window = args[w + 1]
    args.splice(w, 2)
  }
  const script = args[0] === '-' || args.length === 0 ? await readStdin() : args.join(' ')
  try {
    console.log(JSON.stringify(await drive(script, { window }), null, 2))
  } catch (e) {
    console.error(e.message)
    process.exit(1)
  }
}
