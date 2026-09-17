// Behaviour checks for `chatContext`, the one line `ChatPanel` sends with
// every message so "this", "here" and "that one" resolve.
//
// One case per `Section` -- see `SECTIONS` in `state.svelte.ts` -- and the
// branches inside the sections that have more than one answer: the todo
// app's goals view with and without a goal picked, the library with and
// without a shelf, notes open and closed, the overview with and without
// cards on it, and the journal with and without an entry, with and without
// a title on it.
//
// No test framework, deliberately, like the other files here. The function
// takes plain values rather than stores, so every branch is reachable with
// an object literal -- that is the whole point of having pulled it out of
// `ChatPanel.svelte`.

import { load, makeCheck } from './harness.mjs'

const { module: chatContextModule, close } = await load('/src/lib/chat-context.ts')
const { chatContext } = chatContextModule

const { check, finish } = makeCheck()

/** A context with every section's fields at their emptiest, so a test only
 *  has to name what it is overriding. */
function base(section, overrides = {}) {
  return {
    section,
    todo: { showingGoals: false, goalTitle: null, projectName: null },
    library: { shelfName: null },
    notes: { openTitle: null },
    overview: { widgets: [] },
    assistant: { paneLabel: 'Runs' },
    mail: { subject: null, mailboxName: null },
    entry: null,
    ...overrides,
  }
}

// ── todo ─────────────────────────────────────────────────────────────

check('todo, task list, no project', chatContext(base('todo')), 'the todo app')
check(
  'todo, task list, a project',
  chatContext(
    base('todo', { todo: { showingGoals: false, goalTitle: null, projectName: 'Kitchen' } }),
  ),
  'the todo app, project "Kitchen"',
)
check(
  'todo, goals view, nothing selected',
  chatContext(base('todo', { todo: { showingGoals: true, goalTitle: null, projectName: null } })),
  "the todo app's goals, grouped by role",
)
check(
  'todo, goals view, a goal selected',
  chatContext(
    base('todo', { todo: { showingGoals: true, goalTitle: 'Get fit', projectName: null } }),
  ),
  'the todo app\'s goals, the goal "Get fit"',
)

// ── calendar ─────────────────────────────────────────────────────────

check('calendar', chatContext(base('calendar')), 'the calendar')

// ── library ──────────────────────────────────────────────────────────

check('library, no shelf', chatContext(base('library')), 'the library')
check(
  'library, a shelf',
  chatContext(base('library', { library: { shelfName: 'Sci-Fi' } })),
  'the library, shelf "Sci-Fi"',
)

// ── notes ────────────────────────────────────────────────────────────

check('notes, closed', chatContext(base('notes')), 'the notes app')
check(
  'notes, a note open',
  chatContext(base('notes', { notes: { openTitle: 'Grocery list' } })),
  'the notes app, the note "Grocery list"',
)

// ── overview ─────────────────────────────────────────────────────────

check(
  'overview, nothing on the page',
  chatContext(base('overview')),
  'their overview page, which they have not put anything on yet',
)
check(
  'overview, cards on the page',
  chatContext(base('overview', { overview: { widgets: [{ type: 'dueToday' }] } })),
  'their overview page, showing due today',
)

// ── assistant ────────────────────────────────────────────────────────

check(
  'assistant, on a pane',
  chatContext(base('assistant', { assistant: { paneLabel: 'Proposals' } })),
  'your own routines and what they did, on the "Proposals" page',
)

// ── journal ──────────────────────────────────────────────────────────

check('journal, no entry', chatContext(base('journal')), 'the journal')
check(
  'journal, an entry with no title',
  chatContext(base('journal', { entry: { title: '', localDate: '2026-09-17' } })),
  'the journal, an entry dated 2026-09-17',
)
check(
  'journal, an entry with a title',
  chatContext(base('journal', { entry: { title: 'Morning pages', localDate: '2026-09-17' } })),
  'the journal, entry "Morning pages" dated 2026-09-17',
)

// ── mail ─────────────────────────────────────────────────────────────
//
// KNOWN BUG, pinned rather than fixed here: there is no `case 'mail'` yet,
// so it falls through to the journal's `default` and reports the journal's
// own selection -- 'the journal' below, on a screen that has no entry on it
// at all -- instead of anything about the mail app. Phase 1 of
// docs/plans/architecture-refactor.md adds the case; this is the "before"
// picture the fix's test flips.

check('mail (bug): falls through to the journal default', chatContext(base('mail')), 'the journal')

await close()
finish('chat-context')
