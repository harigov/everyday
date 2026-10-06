// Folding each app's sidebar, and the peek that borrows it back.
//
// Per app and remembered; the peek on two delays, so a pointer crossing the
// window's edge on its way to the app bar does not flash it open. See
// `lib/sidebar.svelte.ts`.

import { load, makeCheck, stubBrowser } from './harness.mjs'

// A store that actually stores: the harness's default stub forgets
// everything, and remembering is half of what is checked here.
const kept = new Map()
Object.defineProperty(globalThis, 'localStorage', {
  configurable: true,
  value: {
    getItem: (k) => (kept.has(k) ? kept.get(k) : null),
    setItem: (k, v) => kept.set(k, String(v)),
    removeItem: (k) => kept.delete(k),
  },
})
stubBrowser({})

const { module, close } = await load('/src/lib/sidebar.svelte.ts', { svelte: true })
const { sidebar } = module
const { check, ok, finish } = makeCheck()
const wait = (ms) => new Promise((r) => setTimeout(r, ms))

check('nothing is folded to begin with', sidebar.folded('mail'), false)
sidebar.toggle('mail')
check('folding one app folds that app', sidebar.folded('mail'), true)
check('and no other', sidebar.folded('notes'), false)
check('and is remembered', localStorage.getItem('everyday.sidebar.folded'), '["mail"]')
sidebar.toggle('mail')
check('unfolding forgets it', localStorage.getItem('everyday.sidebar.folded'), '[]')

sidebar.reach()
check('not at once', sidebar.peeking, false)
await wait(220)
check('after a moment, drawn', sidebar.peeking, true)
sidebar.release()
await wait(60)
check('not gone the instant the pointer leaves', sidebar.peeking, true)
sidebar.reach()
await wait(400)
check('coming back in time keeps it', sidebar.peeking, true)
sidebar.release()
await wait(420)
check('gone once the pointer stays away', sidebar.peeking, false)

sidebar.reach()
sidebar.release()
await wait(250)
check('a pointer that only crosses the edge never opens it', sidebar.peeking, false)

sidebar.peek()
ok('peek is immediate', sidebar.peeking)
sidebar.hide()
ok('and so is hide', !sidebar.peeking)

// Typing in it: the pointer drifting off must not take it away.
sidebar.reach()
await wait(220)
sidebar.hold('focus')
sidebar.release()
await wait(420)
check('focus inside keeps it open after the pointer leaves', sidebar.peeking, true)
sidebar.letGo('focus')
await wait(420)
check('and it goes once focus does too', sidebar.peeking, false)

// A row's context menu, drawn outside the panel, holds it until it closes.
sidebar.reach()
await wait(220)
sidebar.hold('menu')
sidebar.release()
await wait(420)
check('a menu raised from it keeps it open', sidebar.peeking, true)
sidebar.letGo('menu')
await wait(420)
check('until the menu closes', sidebar.peeking, false)

sidebar.letGo('menu')
check('letting go of a hold that is not there changes nothing', sidebar.peeking, false)

finish('sidebar')
await close()
