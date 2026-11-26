// What `check.mjs` opens on each app to reach every screen worth checking on
// a phone. One file per app (or group of apps) in `scenarios/`, each
// exporting a default object of lists keyed by section id, each scenario a
// fresh page on that app, unlocked, before `run` does anything:
//
//   export default {
//     mail: [
//       { name: 'home' },
//       { name: 'thread', run: async ({ page, settle }) => { await tap(page, '...'); await settle() } },
//     ],
//   }
//
// `section` on a scenario opens a different app first (the frame's
// scenarios use it). `run` gets the Playwright page and `settle(ms)`, which
// waits for loads and transitions to finish. Use the helpers in
// `helpers.mjs` -- `tap`, not click, for anything a finger presses.
//
// Prefer selectors a person would recognise -- roles, labels, text -- over
// class names, which a restyle changes.

import { readdirSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const dir = join(dirname(fileURLToPath(import.meta.url)), 'scenarios')

export const SCENARIOS = {}
for (const file of readdirSync(dir)
  .filter((f) => f.endsWith('.mjs'))
  .sort()) {
  const mod = await import(pathToFileURL(join(dir, file)).href)
  for (const [app, list] of Object.entries(mod.default)) {
    ;(SCENARIOS[app] ??= []).push(...list)
  }
}
