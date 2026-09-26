// Warn when the running Node is older than the interface needs.
//
// A warning, not a failure: `make setup` has already installed everything by
// the time this runs, and an older Node still builds most of the interface --
// Vite is what refuses, and it says so itself. The point is to say it once,
// early, with the fix, rather than leave somebody to find it in a stack trace.
//
// The minimum is read from `engines.node` in ui/package.json, so there is one
// place that says it. It used to be written into the Makefile as a
// `node -e '...'` spread over several lines with a backslash at the end of
// each; make hands those backslashes to the shell, the shell keeps them inside
// single quotes, and Node was handed a program with a `\` in it and never ran
// the check at all.
//
// Written plainly, with no syntax newer than the Node versions it exists to
// catch, because a check that cannot be parsed by the Node it is checking is
// the same bug again.

import { readFileSync } from 'node:fs'

const pkg = JSON.parse(readFileSync(new URL('../ui/package.json', import.meta.url), 'utf8'))
const wanted = ((pkg.engines && pkg.engines.node) || '').replace(/^>=\s*/, '')
const [needMajor = 0, needMinor = 0] = wanted.split('.').map(Number)
const [major, minor] = process.versions.node.split('.').map(Number)

if (major < needMajor || (major === needMajor && minor < needMinor)) {
  console.warn(
    `\nwarning: Node ${process.versions.node} is below the ${wanted} this project expects ` +
      '(ui/package.json; ui/.nvmrc names the version CI uses). Vite may refuse to start.',
  )
}
