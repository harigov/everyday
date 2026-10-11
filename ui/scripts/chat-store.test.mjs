// Behaviour checks for `chat.svelte.ts` itself: loading, saving, polling and
// removing links against the mock backend. `chat.test.mjs` only loads the
// pure rules in `chat.ts`, never this class. The mock stands in for the
// service the way the real one will: a link just saved reads `connecting`,
// and the next `chat_links` -- the panel's poll -- finds it `online`.

import assert from 'node:assert/strict'
import { load, stubBrowser } from './harness.mjs'

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
  navigator: { userAgent: 'node', language: 'en-GB' },
  location: new URL('http://localhost/?unlocked=1'),
  createObjectURL: () => 'blob:stub',
})

const {
  modules: [chatModule, stateModule, apiModule],
  close,
} = await load(['/src/lib/chat.svelte.ts', '/src/lib/state.svelte.ts', '/src/lib/api.ts'], {
  svelte: true,
})
const { chat } = chatModule
const { app } = stateModule
const { api } = apiModule

await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')

// ── load ─────────────────────────────────────────────────────────────

assert.equal(chat.loading, false)
await chat.load()
assert.deepEqual(chat.list, [], 'a vault with no bots starts with none')
assert.equal(chat.loading, false, 'loading settles back to false once the load lands')

// ── create ───────────────────────────────────────────────────────────

const blank = await chat.create()
assert.ok(blank, 'a fresh link comes back')
assert.equal(blank.enabled, true, 'switched on')
assert.equal(blank.service.kind, 'matrix')
assert.equal(blank.service.homeserver, '')
assert.equal(blank.service.userId, '')
assert.equal(blank.service.owner, '')
assert.equal(typeof blank.freshAfterMinutes, 'number')
assert.notEqual((await chat.create()).id, blank.id, 'every one has an id of its own')
assert.deepEqual(chat.list, [], 'making one saves nothing')

// ── save: refused ────────────────────────────────────────────────────

{
  const refused = await chat.save(blank, null)
  assert.equal(refused.ok, false, 'an empty link is refused')
  assert.match(refused.error, /homeserver address must start with/, "in the core's words")
  assert.deepEqual(chat.list, [], 'and nothing is added')
}

// ── save: accepted ───────────────────────────────────────────────────

const link = {
  ...blank,
  service: {
    ...blank.service,
    homeserver: 'http://127.0.0.1:6167',
    userId: '@bot:example.org',
    owner: '@me:example.org',
  },
}

{
  const saved = await chat.save(link, 'hunter2')
  assert.equal(saved.ok, true)
  assert.equal(saved.view.status.state, 'connecting', 'a link just saved is signing in')
  assert.equal(saved.view.hasPassword, true, 'the password is held for it')
  assert.equal(saved.view.signedIn, false, 'and it has no session yet')
  assert.equal(chat.list.length, 1, 'the row is in the list at once, before any poll')
  assert.equal(chat.view(link.id)?.status.state, 'connecting')
  assert.equal(chat.view('nope'), undefined)
}

// ── a poll moves it on ─────────────────────────────────────────────────

await chat.refresh()
{
  const view = chat.view(link.id)
  assert.equal(view.status.state, 'online', 'the next read finds it online')
  assert.match(view.status.detail, /@bot:example.org/)
  assert.match(view.status.detail, /@me:example.org/)
  assert.equal(view.signedIn, true)
  assert.equal(view.hasPassword, false, 'the password is dropped once there is a session')
}
await chat.refresh()
assert.equal(chat.view(link.id).status.state, 'online', 'and it stays there')

// ── a poll that began before a save does not undo it ──────────────────────
//
// The mock answers in the order it is asked, so this race is staged by hand:
// the poll's read is held back with the list as it was, the save lands, and
// only then does the poll's answer arrive.

{
  const second = await chat.create()
  const asItWas = JSON.parse(JSON.stringify(chat.list))
  const realRead = api.chatLinks
  let release
  const held = new Promise((resolve) => (release = resolve))
  api.chatLinks = () => held.then(() => asItWas)
  try {
    const stale = chat.refresh()
    const good = await chat.save(
      {
        ...second,
        service: {
          kind: 'matrix',
          homeserver: 'https://matrix.example.org',
          userId: '@bot2:example.org',
          owner: '@me:example.org',
        },
      },
      'pw',
    )
    assert.equal(good.ok, true)
    release()
    await stale
  } finally {
    api.chatLinks = realRead
  }
  assert.ok(chat.view(second.id), 'the new row survives a poll that started before it existed')
  assert.equal(chat.list.length, 2)
  assert.equal(await chat.remove(second.id), true)
  assert.equal(chat.list.length, 1)
}

// ── editing keeps the sign-in it can keep ─────────────────────────────

{
  const created = chat.view(link.id).link.createdAt
  const edited = await chat.save({ ...link, freshAfterMinutes: 0 }, null)
  assert.equal(edited.ok, true)
  assert.equal(edited.view.signedIn, true, 'no password needed again for the same bot')
  assert.equal(edited.view.link.freshAfterMinutes, 0)
  assert.equal(edited.view.link.createdAt, created, 'created stays')
  assert.equal(edited.view.status.state, 'connecting', 'a save restarts the bot')
  assert.equal(chat.list.length, 1, 'an edit is not a second row')

  const moved = await chat.save(
    { ...link, service: { ...link.service, userId: '@other:example.org' } },
    null,
  )
  assert.equal(moved.view.signedIn, false, 'a different bot starts from scratch')
  assert.equal(moved.view.status.state, 'needsSignIn', 'and says it needs a password')

  const off = await chat.save({ ...link, enabled: false }, 'pw')
  assert.equal(off.view.status.state, 'off', 'switched off')
  await chat.refresh()
  assert.equal(chat.view(link.id).status.state, 'off', 'and a poll does not turn it on')
}

// ── remove ───────────────────────────────────────────────────────────

assert.equal(await chat.remove(link.id), true)
assert.deepEqual(chat.list, [])
await chat.refresh()
assert.deepEqual(chat.list, [], 'it is gone from the backend too')

// ── reset ────────────────────────────────────────────────────────────

await chat.save(link, 'pw')
assert.equal(chat.list.length, 1)
chat.reset()
assert.deepEqual(chat.list, [], 'a lock empties the list')
await chat.load()
assert.equal(chat.list.length, 1, 'and the next open loads it again')

await close()
console.log('chat-store: all checks passed')

// `app.start()` arms the idle-vault poll; see `editor.test.mjs`'s own note.
process.exit(0)
