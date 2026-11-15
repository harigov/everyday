// Behaviour checks for `connections.svelte.ts` itself -- the class, not the
// pure helpers `connections.test.mjs` covers.
//
// Driven through the real mock backend (`mock-connections.ts`) the way
// `money-store.test.mjs` is: what is worth checking is the round trip -- a
// write that replaces its connection in the list, a token that is nowhere in
// what comes back, a sign-in that waits and can be withdrawn.
//
// Written against the mock's seed: Linear, signed in with six tools, and
// Paseo, which needs no sign-in.

import assert from 'node:assert/strict'
import { load, stubBrowser } from './harness.mjs'

const opened = []

stubBrowser({
  window: globalThis,
  localStorage: true,
  matchMedia: true,
  document: {
    querySelector: () => null,
    documentElement: {
      style: { setProperty: () => {} },
      classList: { toggle: () => {} },
      setAttribute: () => {},
      removeAttribute: () => {},
    },
    addEventListener: () => {},
  },
  navigator: { userAgent: 'node', language: 'en-US' },
  location: new URL('http://localhost/?unlocked=1'),
  createObjectURL: () => 'blob:stub',
})
// What `openExternal` reaches for in a browser tab.
globalThis.open = (url) => {
  opened.push(url)
  return {}
}

const {
  modules: [storeModule, stateModule, helpers, liveApply],
  close,
} = await load(
  [
    '/src/lib/connections.svelte.ts',
    '/src/lib/state.svelte.ts',
    '/src/lib/connections.ts',
    '/src/lib/live-apply.ts',
  ],
  { svelte: true },
)
const { connections } = storeModule
const { app } = stateModule
const { formFromPreset, formProblem } = helpers
const { applierFor } = liveApply

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

/** Wait for something that happens in the background, up to two seconds. */
async function until(what, condition) {
  for (let i = 0; i < 100; i++) {
    if (condition()) return
    await sleep(20)
  }
  assert.fail(`never happened: ${what}`)
}

await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')

// ── load ─────────────────────────────────────────────────────────────────

assert.equal(connections.view, null, 'nothing is read until asked')
assert.deepEqual(connections.connections, [])
await connections.load()
assert.equal(connections.error, null)
assert.deepEqual(
  connections.connections.map((v) => v.connection.name),
  ['Linear', 'Paseo'],
  'the mock vault ships with two connections',
)
assert.equal(connections.presets.length, 9)
assert.deepEqual(
  connections.presets.map((p) => p.key),
  [
    'slack',
    'google-drive',
    'linear',
    'github',
    'paseo',
    'chrome',
    'datadog',
    'custom-http',
    'custom-stdio',
  ],
)
const linear = connections.connections[0]
const paseo = connections.connections[1]
assert.equal(linear.status, 'ready')
assert.equal(linear.secrets.signedIn, true)
assert.equal(linear.state.tools.length, 6)
assert.equal(paseo.status, 'ready')
assert.equal(connections.find(linear.connection.id)?.connection.name, 'Linear')
assert.equal(connections.find('nope'), undefined)

// ── add, from a preset, with a token that must not come back ─────────────

const github = connections.presets.find((p) => p.key === 'github')
const TOKEN = 'ghp_a-very-secret-token'
let githubId
{
  const form = formFromPreset(
    github,
    connections.connections.map((v) => v.connection.name),
  )
  assert.equal(formProblem(form, github), 'Paste the personal access token.')
  form.secrets = { bearer: TOKEN, headers: { Empty: '' }, env: {} }
  assert.equal(formProblem(form, github), null)
  const added = await connections.add(form)
  assert.ok(added, 'a filled-in form is added')
  githubId = added.connection.id
  assert.equal(connections.connections.length, 3, 'the new one joins the list')
  assert.equal(connections.connections[2].connection.id, githubId, 'at the end')
  assert.equal(added.connection.preset, 'github')
  assert.equal(added.secrets.bearer, true, 'which says a token is set')
  assert.deepEqual(added.secrets.headers, [], 'and an empty header is no header')
  assert.equal(added.status, 'unknown', 'but nothing has been tried yet')
  assert.ok(
    !JSON.stringify(connections.view).includes(TOKEN),
    'and the token is nowhere in what the store holds',
  )
  assert.equal(connections.error, null)
}

// A second, with no token: the preset refuses it in the form, and the vault
// still takes it (the token can come later) -- but the list says what is missing.
{
  const form = formFromPreset(
    github,
    connections.connections.map((v) => v.connection.name),
  )
  assert.equal(form.name, 'GitHub 2')
  const added = await connections.add(form)
  assert.equal(added.status, 'needsSetup', 'a token is still wanted')
  assert.equal(await connections.remove(added.connection.id), true)
  assert.equal(connections.connections.length, 3)
}

// A name taken, ignoring case, is refused and says so.
{
  const form = formFromPreset(github, [])
  form.name = 'linear'
  assert.equal(await connections.add(form), null)
  assert.match(connections.error, /already a connection called/)
  assert.equal(connections.connections.length, 3, 'nothing was added')
  const again = formFromPreset(github, ['GitHub'])
  assert.ok(await connections.add(again), 'a good add after it works')
  assert.equal(connections.error, null, 'and clears the message')
  await connections.remove(connections.connections[3].connection.id)
}

// ── update and secrets ───────────────────────────────────────────────────

{
  const before = connections.find(githubId).connection
  assert.equal(await connections.update({ ...before, name: 'Work GitHub', unattended: true }), true)
  assert.equal(connections.find(githubId).connection.name, 'Work GitHub')
  assert.equal(connections.find(githubId).connection.unattended, true)
  assert.equal(connections.connections.length, 3, 'in place, not added')
  assert.equal(
    await connections.update({ ...before, name: 'PASEO' }),
    false,
    'a name taken by another is refused',
  )
  assert.match(connections.error, /already a connection/)
  assert.equal(connections.find(githubId).connection.name, 'Work GitHub', 'and nothing changes')

  assert.equal(await connections.setSecrets(githubId, { bearer: '' }), true)
  assert.equal(connections.find(githubId).secrets.bearer, false, 'an empty string removes it')
  assert.equal(connections.find(githubId).status, 'needsSetup')
  await connections.setSecrets(githubId, { bearer: TOKEN })
  assert.equal(connections.find(githubId).secrets.bearer, true)
  assert.ok(!JSON.stringify(connections.view).includes(TOKEN))
  assert.deepEqual(Object.keys(connections.busy), [], 'nothing is left busy')
}

// ── policy ───────────────────────────────────────────────────────────────

{
  const id = linear.connection.id
  await connections.setPolicy(id, 'list_issues', 'ask')
  assert.equal(connections.find(id).connection.policies.list_issues, 'ask')
  await connections.setPolicy(id, 'list_issues', null)
  assert.ok(
    !('list_issues' in connections.find(id).connection.policies),
    'null goes back to the tool’s own hints',
  )
  assert.equal(connections.error, null)
}

// ── check ────────────────────────────────────────────────────────────────

{
  const before = connections.find(paseo.connection.id).state.lastOk
  await sleep(5)
  await connections.check(paseo.connection.id)
  const after = connections.find(paseo.connection.id)
  assert.equal(after.status, 'ready')
  assert.ok(after.state.lastOk >= before)
  assert.equal(after.state.tools.length, 4)
  assert.equal(after.live, true, 'a client is open to it now')

  // Somewhere the mock cannot reach: the call works, the view says why not.
  await connections.check(githubId)
  const github = connections.find(githubId)
  assert.equal(connections.error, null, 'a server that is down is not a failed call')
  assert.equal(github.status, 'error')
  assert.equal(github.state.lastError, 'Could not reach api.githubcopilot.com')
  assert.deepEqual(Object.keys(connections.busy), [])
}

// ── sign in, out, and in again ───────────────────────────────────────────

{
  const id = linear.connection.id
  await connections.signOut(id)
  assert.equal(connections.find(id).secrets.signedIn, false)
  assert.equal(connections.find(id).status, 'needsSignIn')

  const p = connections.signIn(id)
  await until('the sign-in is waiting', () => connections.pending !== null)
  assert.equal(connections.busy[id], 'signingIn')
  assert.equal(connections.pending.id, id)
  assert.match(connections.pending.url, /^https:\/\/example\.invalid\/authorize\?/)
  assert.equal(
    await connections.signIn(id),
    false,
    'a second sign-in does not start while one is waiting',
  )
  assert.equal(await p, true, 'the sign-in finishes')
  assert.equal(opened.length, 1, 'and the browser was opened once')
  assert.match(opened[0], /^https:\/\/example\.invalid\/authorize/)
  assert.equal(connections.pending, null)
  assert.equal(connections.busy[id], undefined)
  assert.equal(connections.error, null)
  assert.equal(connections.find(id).status, 'ready')
  assert.equal(connections.find(id).secrets.signedIn, true)
  assert.equal(connections.find(id).state.tools.length, 6)
}

// Withdrawn: not an error, and nothing changes.
{
  const id = linear.connection.id
  await connections.signOut(id)
  const p = connections.signIn(id)
  await until('the sign-in is waiting', () => connections.pending !== null)
  await connections.cancelSignIn()
  assert.equal(await p, false)
  assert.equal(connections.error, null, 'cancelling is the person’s own doing')
  assert.equal(connections.pending, null)
  assert.equal(connections.busy[id], undefined)
  assert.equal(connections.find(id).status, 'needsSignIn')
  await connections.cancelSignIn() // nothing pending: nothing happens
  await connections.signIn(id)
  assert.equal(connections.find(id).status, 'ready', 'and a later one still works')
}

// A service that wants a client ID of the person's own says so.
{
  const slack = connections.presets.find((p) => p.key === 'slack')
  const form = formFromPreset(slack, [])
  form.secrets = { clientSecret: 'shh' }
  const added = await connections.add(form)
  assert.ok(added)
  assert.equal(added.status, 'needsSignIn')
  assert.equal(await connections.signIn(added.connection.id), false)
  assert.match(connections.error, /client ID of your own/)
  assert.equal(connections.pending, null)
  await connections.load()
  assert.equal(connections.find(added.connection.id).status, 'needsSetup')
  await connections.remove(added.connection.id)
}

// ── watch: reloads on a settings change only while a pane is open ────────

{
  connections.reset()
  const stop = connections.watch()
  await until('the pane loads', () => connections.view !== null)
  const decline = applierFor('status')
  assert.ok(decline, 'an open pane listens for settings changes')
  assert.equal(
    decline([{ kind: 'settings', op: 'updated', origin: 'remote' }]),
    false,
    'and declines',
  )
  connections.view = null
  decline([{ kind: 'settings', op: 'updated', origin: 'remote' }])
  await until('a settings change reloads it', () => connections.view !== null)
  stop()
  connections.view = null
  decline([{ kind: 'settings', op: 'updated', origin: 'remote' }])
  await sleep(150)
  assert.equal(connections.view, null, 'a closed pane does not')
}

// ── remove ───────────────────────────────────────────────────────────────

{
  await connections.load()
  const before = connections.connections.length
  assert.equal(await connections.remove(githubId), true)
  assert.equal(connections.connections.length, before - 1)
  assert.equal(connections.find(githubId), undefined)
  assert.equal(connections.presets.length, 9, 'the presets stay')
  assert.deepEqual(Object.keys(connections.busy), [])
}

// ── reset ────────────────────────────────────────────────────────────────

connections.error = 'old'
connections.busy = { x: 'saving' }
connections.reset()
assert.equal(connections.view, null)
assert.equal(connections.error, null)
assert.deepEqual(connections.busy, {})
assert.equal(connections.pending, null)

await close()
console.log('connections-store: all checks passed')

// `app.start()` arms the idle-vault poll, which would otherwise hold this
// process open for ever.
process.exit(0)
