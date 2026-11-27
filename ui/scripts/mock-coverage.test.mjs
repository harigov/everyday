// The browser-only mock (`lib/mock.ts` and the `mock-*.ts` beside it) answers
// the commands the interface sends when there is no shell to ask. A command
// the service gains and the mock never learns is a pane that fails in
// `npm run dev` and nowhere else -- and only when somebody happens to open it.
// This file says so up front: every name in `SERVICE_COMMANDS` is answered by
// some mock, or is listed below as one nobody has needed there yet.
//
// "Answered" is read from the source: a `case 'name':` label in a mock file,
// or a quoted name on a line of its own (the `Set`s the domains keep for
// `mock-registry.ts`). That is loose on purpose -- it cannot tell a label in a
// dead branch from a live one -- but it is what makes a new command's missing
// mock show up as a one-line failure rather than a runtime surprise.

import { readdirSync, readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { load, makeCheck, stubBrowser } from './harness.mjs'

const { check, ok, finish } = makeCheck()

stubBrowser({
  window: true,
  localStorage: true,
  matchMedia: true,
  document: true,
  navigator: true,
  location: true,
  createObjectURL: () => 'blob:stub',
})

/**
 * Commands the mock does not answer. Each is one the browser preview has no
 * use for: a recording or a dictation needs a microphone, the writing popup
 * and the tool and command lists are other windows' or the assistant's own,
 * and the rest are maintenance. Add a name here only with a reason; the better
 * fix is a line in the mock.
 */
const UNMOCKED = new Set([
  // Microphone and recording: nothing to record in a browser tab.
  'append_recording_chunk',
  'begin_recording',
  'finish_recording',
  'meeting_now',
  'dictate',
  // The writing popup's own window.
  'list_writing_assists',
  'rewrite_selection',
  'settle_writing_assist',
  // The assistant's tool surface and the service's own housekeeping.
  'list_commands',
  'list_tools',
  'run_tool',
  'collect_garbage',
  'flush',
  'set_health_source_token',
])

const here = dirname(fileURLToPath(import.meta.url))
const lib = join(here, '..', 'src', 'lib')

const answered = new Set()
for (const file of readdirSync(lib).filter((f) => /^mock.*\.ts$/.test(f))) {
  const text = readFileSync(join(lib, file), 'utf8')
  for (const m of text.matchAll(/case '([a-z0-9_]+)'/g)) answered.add(m[1])
  for (const m of text.matchAll(/^\s+'([a-z0-9_]+)',?$/gm)) answered.add(m[1])
}

const {
  modules: [commandsModule, registryModule],
  close,
} = await load(['/src/lib/generated/commands.ts', '/src/lib/mock-registry.ts'])
const { SERVICE_COMMANDS } = commandsModule
const { MOCK_DOMAINS, mockDomainFor } = registryModule

const missing = [...SERVICE_COMMANDS].filter((c) => !answered.has(c) && !UNMOCKED.has(c)).sort()
check('every service command has a mock, or is listed as unmocked', missing, [])

const stale = [...UNMOCKED].filter((c) => answered.has(c) || !SERVICE_COMMANDS.has(c)).sort()
check('the unmocked list names only commands that are service commands with no mock', stale, [])

// The registry: no command answered by two domains, and the way each domain
// treats the vault is the way the mock always did.
const owners = new Map()
for (const [i, domain] of MOCK_DOMAINS.entries()) {
  for (const cmd of SERVICE_COMMANDS) {
    if (domain.handles(cmd)) owners.set(cmd, [...(owners.get(cmd) ?? []), i])
  }
}
check(
  'no command belongs to two domains',
  [...owners].filter(([, list]) => list.length > 1).map(([cmd]) => cmd),
  [],
)

// The shell's own commands stay answerable with the vault locked.
for (const cmd of [
  'pick_folder',
  'screentime_status',
  'screentime_set_tracking',
  'screentime_set_device',
  'screentime_install_gnome_extension',
  'screentime_connect_browser',
  'health_push_setup',
  'health_push_revoke',
  'health_import_apple',
]) {
  const domain = mockDomainFor(cmd)
  ok(`${cmd} is a domain's`, domain !== undefined)
  ok(`${cmd} does not want the vault open`, domain !== undefined && !domain.needsVault(cmd))
}
for (const cmd of [
  'run_backup',
  'connections',
  'list_transactions',
  'mock_money_link_outcome',
  'screen_time_report',
  'health_today',
]) {
  const domain = mockDomainFor(cmd)
  ok(`${cmd} wants the vault open`, domain !== undefined && domain.needsVault(cmd))
}

await close()
finish('mock-coverage')
