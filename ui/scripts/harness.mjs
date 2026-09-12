// The plumbing every `*.test.mjs` file needs, so the file itself is the
// behaviour it checks and nothing else.
//
// Before this, nineteen of these files each booted their own bare Vite
// server with the same `configFile: false` / `watch: null` incantation
// (copied in whole from whichever file was newest when the next one was
// written), seven stubbed `globalThis` for a store written for a browser in
// four slightly different shapes, and six hand-rolled the same
// got/want-by-`JSON.stringify` comparison under a different name. None of
// that is the assertion; all of it is what somebody has to read past to find
// the assertion, and a real change to any of it -- a new field the stub needs,
// a message the comparison should print -- meant editing it in place in every
// file that happened to have a copy.
//
// `load` and `stubBrowser` are used from inside a test file, one call each,
// in place of the boilerplate they replace. `check`/`ok`/`finish` are the
// same for the got/want pattern. The runner at the foot of this file is what
// `npm test` calls instead of the twenty-one-clause `&&` chain that used to
// list every file by hand -- a file dropped in `scripts/` matching
// `*.test.mjs` is picked up without editing `package.json`.

import { createServer } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'
import { readdirSync } from 'node:fs'
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

const here = dirname(fileURLToPath(import.meta.url))
const root = join(here, '..')

/**
 * Boot the bare server a test loads its module through, load `path` (or each
 * path in it, for a test that needs more than one module), and hand back a
 * `close` to call before the file exits.
 *
 * `svelte: true` adds the Svelte plugin under its default options, needed
 * only for a `.svelte.ts` module -- it is what turns `$state` into something
 * that exists -- and left off for plain TypeScript, which does not need a
 * compiler this heavy just to load. Pass an options object instead of `true`
 * for the one file that wants `{ compilerOptions: { hmr: false } }`.
 *
 * The project's own `vite.config.ts` is deliberately not read: loading a
 * config file makes Vite watch it, which is a second watcher on top of the
 * one `watch: null` below is already refusing to open, and a suite that
 * opens one server per file exhausts a machine's supply of them (`EMFILE`)
 * quickly enough that this bit everyone who tried skipping it.
 */
export async function load(paths, opts = {}) {
  const server = await createServer({
    configFile: false,
    root,
    plugins: opts.svelte ? [svelte(opts.svelte === true ? undefined : opts.svelte)] : [],
    server: { middlewareMode: true, watch: null },
    appType: 'custom',
    logLevel: 'error',
  })
  const list = Array.isArray(paths) ? paths : [paths]
  const modules = await Promise.all(list.map((p) => server.ssrLoadModule(p)))
  return { module: modules[0], modules, close: () => server.close() }
}

/**
 * Enough of a browser for a store written for one to evaluate outside it.
 *
 * Every key is opt-in, and `true` asks for the shape most of these files had
 * already agreed on; anything else is used outright, which is how the one
 * test with its own dialog-counting `document`, or its own `localStorage`
 * missing `removeItem`, still costs one line here instead of the six it used
 * to. Nothing is set that was not asked for: a file that never mentions
 * `navigator` gets none of this file's opinions about it, which matters on a
 * Node new enough to already have one of its own.
 *
 * `??=` throughout, the same guard every one of the originals used by hand,
 * so a value already on `globalThis` -- Node's own `crypto`, say -- is left
 * alone rather than shadowed by a stub weaker than the real thing.
 */
const DEFAULT_STUBS = {
  window: {},
  localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
  matchMedia: () => ({
    matches: false,
    addEventListener: () => {},
    removeEventListener: () => {},
  }),
  document: {
    querySelector: () => null,
    documentElement: { style: { setProperty: () => {} }, classList: { toggle: () => {} } },
    addEventListener: () => {},
  },
  navigator: { userAgent: 'node' },
  location: { search: '', href: 'http://localhost/' },
}

export function stubBrowser(overrides = {}) {
  for (const [key, value] of Object.entries(overrides)) {
    if (key === 'createObjectURL') continue
    globalThis[key] ??= value === true ? DEFAULT_STUBS[key] : value
  }
  // Not a `globalThis` key of its own -- `URL.createObjectURL` -- so it is
  // handled apart from the loop above rather than forcing every caller to
  // know that.
  if (overrides.createObjectURL) {
    globalThis.URL.createObjectURL ??= overrides.createObjectURL
  }
}

/**
 * The got/want comparison six of these files hand-rolled under five spellings
 * of the same failure message.
 *
 * Compared by `JSON.stringify` rather than `assert.deepEqual`: these files
 * only ever compare plain data, and a test that wants `node:assert`'s own
 * checks alongside this one -- several do -- gets to use both without the two
 * disagreeing about what "equal" means for anything. Counted rather than
 * thrown, so one bad case does not hide the rest that follow it in the same
 * file; `finish` turns the count into the process's exit code and the same
 * closing line every file printed by hand.
 */
export function makeCheck() {
  let failed = 0
  function check(what, got, want) {
    const a = JSON.stringify(got)
    const b = JSON.stringify(want)
    if (a === b) return
    failed += 1
    console.error(`✗ ${what}\n  got  ${a}\n  want ${b}`)
  }
  function ok(what, condition) {
    if (condition) return
    failed += 1
    console.error(`✗ ${what}`)
  }
  function finish(name) {
    if (failed > 0) {
      console.error(`\n${failed} ${name} check${failed === 1 ? '' : 's'} failed`)
      process.exitCode = 1
      return
    }
    console.log(`${name}: all checks passed`)
  }
  return { check, ok, finish }
}

// ── the runner ──────────────────────────────────────────────────────────
//
// Only reached when this file is run directly -- `node scripts/harness.mjs`,
// which is what `npm test` now calls -- not when a test file imports it for
// the helpers above.

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const files = readdirSync(here)
    .filter((f) => f.endsWith('.test.mjs'))
    .sort()

  let passed = 0
  const failedFiles = []
  for (const file of files) {
    // Each file gets its own process, not just its own Vite server: a dozen
    // of them stub `globalThis` with `??=`, which only keeps a file safe on
    // its own if nothing before it in the same process has already set the
    // same key.
    const result = spawnSync(process.execPath, [join(here, file)], { stdio: 'inherit' })
    if (result.status === 0) passed += 1
    else failedFiles.push(file)
  }

  console.log(`\n${passed}/${files.length} test files passed`)
  if (failedFiles.length > 0) {
    console.error(`failed: ${failedFiles.join(', ')}`)
    process.exitCode = 1
  }
}
