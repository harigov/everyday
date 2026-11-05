// How keeping in touch is put into words, and the one thing the interface
// decides for itself: which person a "which Ravi?" answer names.

import { load, makeCheck } from './harness.mjs'

const { module: kit, close } = await load('/src/lib/keepintouch.ts')
const { lateness, every, lastLine, span, choose, catchUpSource, isContactShelf } = kit
const { check, finish } = makeCheck()

check('due today', lateness({ daysOver: 0 }), 'Due today')
check('days late', lateness({ daysOver: 3 }), '3 days overdue')
check('one day late is singular', lateness({ daysOver: 1 }), '1 day overdue')
check('weeks late', lateness({ daysOver: 21 }), '3 weeks overdue')
check('months late', lateness({ daysOver: 95 }), '3 months overdue')
check('still in time', lateness({ daysOver: -4 }), 'Due in 4 days')
check('a fortnight is weeks', span(14), '2 weeks')

check('weekly', every(7), 'Every week')
check('fortnightly', every(14), 'Every 2 weeks')
check('a month in days', every(30), 'Every 30 days')
check('a quarter in days', every(91), 'Every 91 days')

const friendly = (iso) => `<${iso}>`
check('never caught up', lastLine({ lastContact: null }, friendly), 'No catch-up yet')
check(
  'with how',
  lastLine({ lastContact: '2026-10-03', lastVia: 'call' }, friendly),
  'Last caught up <2026-10-03> by phone',
)
check(
  'an unsaid means is left unsaid',
  lastLine({ lastContact: '2026-10-03', lastVia: 'other' }, friendly),
  'Last caught up <2026-10-03>',
)

const asked = {
  log: { id: 'l1', itemId: 'a', event: 'revisited', date: '2026-11-05', tz: 'UTC', note: '' },
  candidates: [
    { itemId: 'a', name: 'Ravi Kumar' },
    { itemId: 'b', name: 'Ravi Menon' },
  ],
  askedAs: 'Ravi',
}
check('choosing names the person', choose(asked, 'b').log.itemId, 'b')
check('and leaves the proposal as it was', asked.log.itemId, 'a')
check('somebody who was not offered', choose(asked, 'z'), null)

check(
  'a row from the journal says so',
  catchUpSource({ type: 'entry', id: 'e' }),
  'From your journal',
)
check('a typed row says nothing', catchUpSource(null), '')
check('the Contacts shelf', isContactShelf({ slug: 'contact' }), true)
check('any other', isContactShelf({ slug: 'book' }), false)

finish('keepintouch')
await close()
