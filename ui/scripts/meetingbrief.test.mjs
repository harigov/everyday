// `lib/taskperson.ts`: reading somebody typed into a task's "Raise with" field.
//
// It has to come out as the core's `TaskPerson::said` makes it -- the cases
// below are the ones `parse_attendee`'s own test in `meeting/identify.rs`
// holds, and what `said` does with them -- because the brief before a meeting
// finds the person among its guests by address as well as by name. A
// `Josh Smith <josh@acme.com>` kept whole as a name would never be found.

import { load, makeCheck } from './harness.mjs'

const { check, finish } = makeCheck()

const { module: m, close } = await load('/src/lib/taskperson.ts')

// ── as the core reads an attendee ────────────────────────────────────────

check('a name and an address', m.parseAttendee('Priya Raman <priya@x.com>'), {
  name: 'Priya Raman',
  email: 'priya@x.com',
})
check('a quoted name keeps its comma', m.parseAttendee('"Raman, Priya" <p@x.com>'), {
  name: 'Raman, Priya',
  email: 'p@x.com',
})
check('a mailto: address', m.parseAttendee('mailto:p@x.com'), { email: 'p@x.com' })
check('a bare address', m.parseAttendee('p@x.com'), { email: 'p@x.com' })
check('a bare name', m.parseAttendee('Priya Raman'), { name: 'Priya Raman' })
check('nothing', m.parseAttendee(''), {})
check('only spaces', m.parseAttendee('   '), {})
check('an address in angle brackets alone', m.parseAttendee('<p@x.com>'), { email: 'p@x.com' })
check('a mailto: inside the brackets', m.parseAttendee('Priya <mailto:P@X.com>'), {
  name: 'Priya',
  email: 'p@x.com',
})
check('addresses are lower-cased', m.parseAttendee('Priya <Priya@X.COM>').email, 'priya@x.com')
check('names are not', m.parseAttendee('PRIYA <p@x.com>').name, 'PRIYA')
check('an opening bracket with no close is a name', m.parseAttendee('Priya <p@x.com'), {
  name: 'Priya <p@x.com',
})
check('an at-sign in a phrase is a name', m.parseAttendee('lunch @ noon'), { name: 'lunch @ noon' })

// ── as a person is stored ────────────────────────────────────────────────

check('Josh', m.personSaid('Josh'), { name: 'Josh' })
check('Josh, with room around him', m.personSaid('  Josh  '), { name: 'Josh' })
check('a name and an address', m.personSaid('Josh Smith <josh@acme.com>'), {
  name: 'Josh Smith',
  email: 'josh@acme.com',
})
check('a bare address is its own name', m.personSaid('Josh@Acme.com'), {
  name: 'josh@acme.com',
  email: 'josh@acme.com',
})
check('an address in brackets is its own name', m.personSaid('<josh@acme.com>'), {
  name: 'josh@acme.com',
  email: 'josh@acme.com',
})
check('nothing typed is no name', m.personSaid('').name, '')
check('empty brackets are no name', m.personSaid('<>').name, '')

await close()
finish('meetingbrief')
