// Behaviour checks for the notification service's routing and stacking.
//
// The same reasoning as `quickadd.test.mjs` and `calendar.test.mjs`: most of
// the interface is checked by the type checker and by the fact that it
// compiles, and the exceptions are the modules that are made of decisions.
// `notify-policy.ts` is one -- where a message goes, whether it replaces one
// already on screen, and which one is dropped when there is no room are four
// rules that a later change could reverse without anything failing to build.
//
// They are also rules whose bugs are quiet. A notification routed to the
// operating system when it should have been a toast is not an error; it is a
// banner over the app you were already using. One dropped from a full stack
// because it happened to be oldest is not an error either; it is the message
// about your unsaved journal disappearing behind three remarks about
// calendars.
//
// No test framework, deliberately -- one dependency-free file, run by
// `npm run check`. The TypeScript is loaded through Vite so it is compiled
// exactly as the application compiles it.

import assert from 'node:assert/strict'
import { createServer } from 'vite'

const server = await createServer({
  configFile: false,
  root: new URL('..', import.meta.url).pathname,
  server: { middlewareMode: true },
  appType: 'custom',
  logLevel: 'error',
})

const { channelFor, push, toToast, DWELL, MAX_TOASTS } = await server.ssrLoadModule(
  '/src/lib/notify-policy.ts',
)

const AWAY = { focused: false, osReady: true }
const HERE = { focused: true, osReady: true }
const MUTED = { focused: false, osReady: false }

// ── routing ───────────────────────────────────────────────────────────

// The default reach never leaves the window, whatever else is true.
assert.equal(channelFor({ title: 'x' }, AWAY), 'toast')
assert.equal(channelFor({ title: 'x', reach: 'app' }, AWAY), 'toast')
assert.equal(
  channelFor({ title: 'x', level: 'error' }, AWAY),
  'toast',
  'reach is absolute: severity alone never buys a notification its way out of the window',
)

// A window in front of the user is told in the window.
assert.equal(channelFor({ title: 'x', reach: 'user' }, HERE), 'toast')
assert.equal(channelFor({ title: 'x', reach: 'user', level: 'error' }, HERE), 'toast')

// Away, and the OS will take it.
assert.equal(channelFor({ title: 'x', reach: 'user', timeout: 5000 }, AWAY), 'os')

// Away, and it will not.
assert.equal(
  channelFor({ title: 'x', reach: 'user', timeout: 5000 }, MUTED),
  'toast',
  'a refused permission must not swallow the message',
)

// Both, when the toast carries something the banner cannot.
assert.equal(
  channelFor(
    { title: 'x', reach: 'user', timeout: 5000, action: { label: 'Undo', run() {} } },
    AWAY,
  ),
  'both',
  'the button is the point, and it only exists on the toast',
)
assert.equal(
  channelFor({ title: 'x', reach: 'user', timeout: null }, AWAY),
  'both',
  'a notification centre retires banners on its own schedule; sticky means sticky',
)
assert.equal(
  channelFor({ title: 'x', reach: 'user', level: 'error' }, AWAY),
  'both',
  'errors are sticky by default, so they get the same treatment without asking',
)

// ── defaults ──────────────────────────────────────────────────────────

assert.equal(toToast({ title: 'x' }, 1).level, 'info', 'info is the unremarkable default')
assert.equal(
  toToast({ title: 'x', level: 'error' }, 1).timeout,
  null,
  'errors wait to be dismissed',
)
assert.equal(toToast({ title: 'x', level: 'success' }, 1).timeout, DWELL.success)
assert.equal(
  toToast({ title: 'x', level: 'error', timeout: 1000 }, 1).timeout,
  1000,
  'an explicit timeout beats the level default',
)
assert.equal(
  toToast({ title: 'x', level: 'success', timeout: null }, 1).timeout,
  null,
  'and null is an explicit timeout, not an absent one',
)

// ── the stack ─────────────────────────────────────────────────────────

const t = (id, opts = {}) => toToast({ title: `t${id}`, ...opts }, id)

// Unkeyed toasts stack in arrival order.
let stack = push(push([], t(1)), t(2))
assert.deepEqual(
  stack.map((x) => x.id),
  [1, 2],
)

// A keyed toast replaces its predecessor *where it stands*. Moving it to the
// end would make one recurring fault look like a queue of new ones.
stack = push(push(push([], t(1, { key: 'save' })), t(2)), t(3, { key: 'save' }))
assert.deepEqual(
  stack.map((x) => x.id),
  [3, 2],
  'the replacement keeps the position and takes a fresh id',
)
assert.equal(stack.length, 2, 'and does not stack a second copy')

// Different keys do not collide.
stack = push(push([], t(1, { key: 'a' })), t(2, { key: 'b' }))
assert.equal(stack.length, 2)

// Overflow drops the oldest dismissible toast, not simply the oldest.
stack = []
stack = push(stack, t(1, { level: 'error' })) // sticky
for (let i = 2; i <= 2 + MAX_TOASTS; i++) stack = push(stack, t(i))
assert.equal(stack.length, MAX_TOASTS)
assert.ok(
  stack.some((x) => x.id === 1),
  'a run of chatter must not push an unresolved error off the screen',
)
assert.deepEqual(
  stack.map((x) => x.id),
  [1, ...Array.from({ length: MAX_TOASTS - 1 }, (_, i) => i + 4)],
  'and the chatter is dropped oldest-first',
)

// When everything on screen is sticky, something still has to give.
stack = []
for (let i = 1; i <= MAX_TOASTS + 1; i++) stack = push(stack, t(i, { timeout: null }))
assert.equal(stack.length, MAX_TOASTS)
assert.equal(stack[0].id, 2, 'the oldest goes, because there is no other rule left')
assert.equal(
  stack.at(-1).id,
  MAX_TOASTS + 1,
  'and the one that just arrived is not the one that goes',
)

// The toast being pushed is never the one dropped to make room for it. A
// stack already full of sticky errors would otherwise find the arrival to be
// its only dismissible toast and drop it before it was ever drawn -- silently,
// and for the user having the worst time.
stack = []
for (let i = 1; i <= MAX_TOASTS; i++) stack = push(stack, t(i, { timeout: null }))
stack = push(stack, t(99))
assert.equal(stack.length, MAX_TOASTS)
assert.ok(
  stack.some((x) => x.id === 99),
  'the arriving toast survives even when every toast it joins is sticky',
)
assert.equal(stack[0].id, 2, 'and the oldest sticky one is what makes room for it')

await server.close()
console.log('notify: all checks passed')
