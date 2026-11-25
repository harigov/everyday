#!/usr/bin/env node
// One spoken turn on the phone, end to end: the app's microphone button and
// Android's permission for it, the phone's audio up to the computer it is
// paired with, that computer's models and assistant, and the reply spoken
// back on the phone.
//
//   node scripts/phone/android-voice.mjs [--clip person.wav] [--keep]
//
// What it needs (see docs/mobile.md, "Testing voice"):
//   - a debug build installed on an emulator or phone, paired with a vault
//     served from this machine (`scripts/phone-test-vault.sh`) whose models
//     are chosen: a recogniser and a voice, and a model to answer;
//   - a WAV of somebody speaking (16-bit mono, any rate), padded with a
//     second of silence before and a couple after.
//
// The clip is handed to the app as its microphone's recording
// (`voice/test_microphone.rs`), because an emulator's microphone hears this
// machine only fitfully. The turn is in a new conversation, and passes when
// the clip's words show in the thread as what was said, a reply comes back,
// and the voice bar goes through "Speaking" -- the reply's audio sent to the
// phone and played there -- and back to "Listening".

import { readFileSync, writeFileSync } from 'node:fs'
import { homedir, tmpdir } from 'node:os'
import { join } from 'node:path'
import { PACKAGE, adb, back, launch, tapNative, touch, wait } from './android.mjs'

const arg = (name, fallback) => {
  const i = process.argv.indexOf(`--${name}`)
  return i >= 0 ? process.argv[i + 1] : fallback
}
const clip = arg('clip', join(homedir(), '.cache/ky-android/clips/person-padded.wav'))
const shot = arg('shot', join(tmpdir(), 'ky-voice-after.png'))

/** The clip as 16 kHz mono 16-bit samples, nearest-neighbour resampled:
 *  plenty for a recogniser. */
function toRaw(wav) {
  const bytes = readFileSync(wav)
  const rate = bytes.readUInt32LE(24)
  const pcm = bytes.subarray(bytes.indexOf(Buffer.from('data')) + 8)
  const count = Math.floor(pcm.length / 2)
  const out = Buffer.alloc(Math.floor((count * 16000) / rate) * 2)
  for (let i = 0; i < out.length / 2; i++) {
    const from = Math.min(count - 1, Math.floor((i * rate) / 16000))
    out.writeInt16LE(pcm.readInt16LE(from * 2), i * 2)
  }
  return out
}

const raw = join(tmpdir(), 'ky-test-microphone.raw')
writeFileSync(raw, toRaw(clip))
adb('push', raw, '/data/local/tmp/ky-test-microphone.raw')
adb(
  'shell',
  'run-as',
  PACKAGE,
  'cp',
  '/data/local/tmp/ky-test-microphone.raw',
  './test-microphone.raw',
)

// --keep: use the app as it is running, rather than starting it afresh.
const { page, close } = await launch({ fresh: !process.argv.includes('--keep') })
await page.locator('.shell').waitFor({ timeout: 40000 })
await wait(1500)
if (await page.evaluate(() => document.documentElement.hasAttribute('data-keyboard'))) {
  back()
  await wait(1000)
}
if (!(await page.locator('button[aria-label="Talk to the assistant"]').isVisible())) {
  await touch(page, '[data-bottom-bar] button:has-text("Assistant")')
  await wait(2000)
}
await touch(page, 'button[title^="New conversation"]')
await wait(1500)

// Every state the voice bar shows, as it shows it.
await page.evaluate(() => {
  window.__voiceStates = []
  const seen = () => {
    const label = document.querySelector('.voice .label')?.textContent?.trim()
    if (label && window.__voiceStates.at(-1) !== label) window.__voiceStates.push(label)
  }
  new MutationObserver(seen).observe(document.body, {
    subtree: true,
    childList: true,
    characterData: true,
  })
})
await touch(page, 'button[aria-label="Talk to the assistant"]')
for (let i = 0; i < 10; i++) {
  if (tapNative(/^While using the app$/) || (await page.locator('.voice').isVisible())) break
  await wait(800)
}

const thread = () => page.evaluate(() => document.body.innerText)
const states = () => page.evaluate(() => window.__voiceStates)
let reply = false
for (let i = 0; i < 240 && !reply; i++) {
  await wait(250)
  reply = (await thread()).includes('half past nine')
}
for (let i = 0; i < 160; i++) {
  const seen = await states()
  if (seen.includes('Speaking') && seen.at(-1) === 'Listening') break
  await wait(250)
}
await page.screenshot({ path: shot })
const heard = /how I sound/i.test(await thread())
const seen = await states()
await touch(page, '.voice .end').catch(() => {})
await wait(1000)
await close()

console.log('voice bar:', seen.join(' → '))
console.log('heard in the thread:', heard ? 'yes' : 'no')
console.log('reply in the thread:', reply ? 'yes' : 'no')
console.log(`screenshot: ${shot}`)
process.exit(heard && reply && seen.includes('Speaking') ? 0 : 1)
