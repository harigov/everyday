// The order the bar at the top of the window offers things in.
//
// Coarse tiers rather than a fuzzy distance -- see `lib/commandbar.ts` --
// so these pin the tiers and the tie-break, which is what decides which row
// Enter takes.

import { load, makeCheck } from './harness.mjs'

const { module: bar, close } = await load('/src/lib/commandbar.ts')
const { score, rankActions, headingOf, keyOf, mentionsWhen } = bar
const { check, finish } = makeCheck()

const act = (label, group = 'Everywhere', keywords) => ({ label, group, keywords, run: () => {} })

const lock = act('Lock this screen', 'Vault', ['sign out'])
const library = act('Library, covers or a list', 'Library')
const sidebar = act('Show or hide the sidebar', 'Everywhere', ['fold', 'collapse'])
const aside = act('Set an hour aside', 'Calendar')

check('nothing typed matches everything equally', score(lock, ''), 1)
check('a prefix of the label is best', score(lock, 'lo'), 4)
check('the start of a later word is next', score(sidebar, 'side'), 3.5)
check('anywhere in the label after that', score(aside, 'side'), 3)
check('then a keyword', score(sidebar, 'fold'), 2)
check('then the heading it is filed under', score(library, 'libr'), 4)
check('then nothing', score(lock, 'zzz'), 0)

check(
  'best first, the table’s own order breaking ties',
  rankActions([aside, lock, sidebar, library], 'side').map((r) => r.action.label),
  ['Show or hide the sidebar', 'Set an hour aside'],
)
check(
  'with nothing typed, the table’s order',
  rankActions([aside, lock, sidebar], '').map((r) => r.action.label),
  ['Set an hour aside', 'Lock this screen', 'Show or hide the sidebar'],
)
check('a limit', rankActions([aside, lock, sidebar], '', 2).length, 2)

check('a search row is filed under Search', headingOf({ kind: 'search', label: 'x' }), 'Search')
check(
  'an action under its own group',
  headingOf({ kind: 'action', action: lock, score: 1 }),
  'Vault',
)
check(
  'a capture keeps its key while the text changes',
  keyOf({ kind: 'capture', label: 'Add a task: mil', icon: 'check', hint: '', run: () => {} }) ===
    keyOf({ kind: 'capture', label: 'Add a task: milk', icon: 'check', hint: '', run: () => {} }),
  true,
)

// Booking is offered only for a line that names a day or a time.
check('a day name is a when', mentionsWhen('Lunch with Sam on Friday'), true)
check('a clock time is a when', mentionsWhen('call the bank at 3pm'), true)
check('a date is a when', mentionsWhen('dentist 2026-12-03'), true)
check('a relative span is a when', mentionsWhen('check the oven in 20 minutes'), true)
check('a month and a day are a when', mentionsWhen('Mum’s birthday Dec 14'), true)
check('a plain sentence is not', mentionsWhen('buy milk'), false)
check('a month name alone is not', mentionsWhen('may I ask'), false)

finish('commandbar')
await close()
