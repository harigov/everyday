// Behaviour checks for the TypeScript half of "is this a meeting, and where
// is its call?" -- the copy the event popover uses so it can decide whether
// to draw "Take notes" without a round trip to the backend. See
// `meeting-hosts.ts`'s own doc for why this duplicates
// `crates/everyday-core/src/meeting/detect.rs` rather than calling it.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: hosts, close } = await load('/src/lib/meeting-hosts.ts')
const { joinHref, joinLink, MEETING_HOSTS, takeNotesWindow } = hosts

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

// Every host in the list is actually recognised -- catches a typo added to
// the list without a matching case.
for (const host of MEETING_HOSTS) {
  assert.ok(
    joinLink(event({ description: `Join: https://${host}/somewhere` })) !== null,
    `${host} should be recognised`,
  )
}

const now = new Date('2026-09-16T10:15:00Z')

// ── the take-notes window ───────────────────────────────────────────────

const withPeople = event({ attendees: ['Ann Lee <ann@x.com>'], allDay: false })
assert.ok(takeNotesWindow(withPeople, now), 'in progress, with another person')
assert.ok(
  takeNotesWindow(withPeople, new Date('2026-09-16T09:55:00Z')),
  'five minutes before the start is in',
)
assert.ok(
  !takeNotesWindow(withPeople, new Date('2026-09-16T09:54:59Z')),
  'earlier than that is not',
)
assert.ok(!takeNotesWindow(withPeople, new Date('2026-09-16T10:30:00Z')), 'end is exclusive')
assert.ok(!takeNotesWindow(event({ allDay: false }), now), 'nobody else invited')
assert.ok(!takeNotesWindow(event({ allDay: false, attendees: [] }), now), 'empty guest list')
assert.ok(!takeNotesWindow({ ...withPeople, allDay: true }, now), 'all-day is not a meeting')
assert.ok(!takeNotesWindow({ ...withPeople, status: 'cancelled' }, now), 'cancelled')
assert.ok(
  takeNotesWindow({ ...withPeople, location: 'Room 4B' }, now),
  'a join link is not required',
)
assert.ok(!takeNotesWindow({ ...withPeople, busy: false }, now), 'a free event is not a meeting')
assert.ok(
  !takeNotesWindow({ ...withPeople, organizer: 'Sam <sam@x.com>', attendees: ['sam@x.com'] }, now),
  'the organizer inviting only themselves is not a meeting',
)
assert.ok(
  takeNotesWindow(
    { ...withPeople, organizer: 'ann@x.com', attendees: ['Ann <ANN@x.com>', 'Bo'] },
    now,
  ),
  'a bare name is somebody else',
)

// ── join hrefs ──────────────────────────────────────────────────────────

assert.equal(joinHref('https://zoom.us/j/1'), 'https://zoom.us/j/1')
assert.equal(joinHref('zoom.us/j/1'), 'https://zoom.us/j/1', 'a bare host gets a scheme')

await close()
console.log('meeting-hosts: all checks passed')
