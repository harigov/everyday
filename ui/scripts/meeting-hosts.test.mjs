// Behaviour checks for the TypeScript half of "does this look like an
// online call?" -- the copy the event popover uses so it can decide whether
// to draw "Take notes" without a round trip to the backend. See
// `meeting-hosts.ts`'s own doc for why this duplicates
// `crates/everyday-core/src/meeting/detect.rs` rather than calling it.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: hosts, close } = await load('/src/lib/meeting-hosts.ts')
const { eventInProgress, joinLink, looksLikeOnlineCall, MEETING_HOSTS } = hosts

/** A minimal event, with just the fields these functions read. */
function event(overrides = {}) {
  return {
    url: '',
    location: '',
    description: '',
    status: 'confirmed',
    busy: true,
    start: '2026-09-16T10:00:00Z',
    end: '2026-09-16T10:30:00Z',
    ...overrides,
  }
}

// ── join links ──────────────────────────────────────────────────────────

assert.equal(
  joinLink(event({ url: 'https://meet.google.com/abc-defg-hij' })),
  'https://meet.google.com/abc-defg-hij',
)
assert.equal(
  joinLink(event({ location: 'Join at https://zoom.us/j/123456' })),
  'https://zoom.us/j/123456',
)
assert.equal(
  joinLink(event({ description: 'Dial in via teams.microsoft.com/l/meetup-join/xyz' })),
  'teams.microsoft.com/l/meetup-join/xyz',
)
assert.equal(joinLink(event({ location: 'Conference Room 4B' })), null, 'no host, no link')
assert.equal(joinLink(event()), null, 'nothing set at all')

// URL wins over location, which wins over description -- the same order
// `is_online_call` reads them in.
assert.equal(
  joinLink(event({ url: 'https://meet.google.com/x', description: 'https://zoom.us/j/1' })),
  'https://meet.google.com/x',
)

// ── looksLikeOnlineCall ───────────────────────────────────────────────

assert.ok(looksLikeOnlineCall(event({ url: 'https://meet.google.com/abc' })))
assert.ok(
  !looksLikeOnlineCall(event({ url: 'https://meet.google.com/abc', busy: false })),
  'a free event is not a call to record',
)
assert.ok(
  !looksLikeOnlineCall(event({ url: 'https://meet.google.com/abc', status: 'cancelled' })),
  'a cancelled event is not a call to record',
)
assert.ok(!looksLikeOnlineCall(event()), 'no host anywhere')

// Every host in the list is actually recognised -- catches a typo added to
// the list without a matching case.
for (const host of MEETING_HOSTS) {
  assert.ok(
    looksLikeOnlineCall(event({ description: `Join: https://${host}/somewhere` })),
    `${host} should be recognised`,
  )
}

// ── in progress ─────────────────────────────────────────────────────────

const now = new Date('2026-09-16T10:15:00Z')
assert.ok(eventInProgress(event(), now), 'inside the window')
assert.ok(!eventInProgress(event(), new Date('2026-09-16T09:59:00Z')), 'before it starts')
assert.ok(!eventInProgress(event(), new Date('2026-09-16T10:30:00Z')), 'end is exclusive')

await close()
console.log('meeting-hosts: all checks passed')
