// Every keyboard shortcut in the application, in one table.
//
// The mechanics -- what a chord is, how a two-key sequence is matched -- are
// in `keys.ts`, which has no stores in it and is tested on its own. This file
// is the table and the dispatcher, and it is the only place a shortcut is
// declared. That matters more than it sounds: shortcuts were previously
// spread across `App.svelte` (five with a modifier) and `CalendarView.svelte`
// (seven bare letters, on a `<svelte:window>` handler of its own), so nothing
// could say what the application's keyboard was, no two of them agreed about
// what counted as "typing", and there was nowhere for a help sheet to read.
//
// # The shape of it
//
// Superhuman's, because it is the one that scales past a dozen. A modifier
// combination for the handful of things every desktop application has --
// new, find, save, lock -- and for everything else a short *sequence*: `g`
// then `j` for the journal, `g` then `l` for the library. It reads as a
// sentence, there are as many as you like of them, and none of them collides
// with what the platform or the webview has already taken.
//
// Bare single letters are reserved for what is on screen: `d`, `w` and `m`
// switch the calendar's view, `s` stars the open entry, `v` flips the
// library between covers and a list. Every one of those is `when`-gated to
// the app that owns it, so the same letter can mean something different in
// two apps without either of them being ambiguous.
//
// # The one rule
//
// A bare letter is a letter when the caret is in a field. `whileTyping` is
// how the few exceptions -- the ones with a modifier in them -- say so.

import { agent } from './agent.svelte'
import { calendar } from './calendar.svelte'
import { SEQUENCE_MS, chordOf, isTyping, match, type Binding } from './keys'
import { library } from './library.svelte'
import { notes } from './notes.svelte'
import { overview } from './overview.svelte'
import { panels } from './panels.svelte'
import { app, type Section } from './state.svelte'
import { todo } from './todo.svelte'

/**
 * Put the caret in whatever the open app calls its search.
 *
 * Found by attribute rather than by selector-per-app: several apps have a
 * search field, they are in different components, and the alternative is this
 * function knowing all their class names. `data-search` is the contract, and
 * a new app gets the shortcut by wearing it.
 */
function focusSearch() {
  document.querySelector<HTMLInputElement>('[data-search]')?.focus()
}

/** Is this app the one on screen, and past the lock? */
function inApp(section: Section): () => boolean {
  return () => app.screen === 'main' && app.section === section
}

/**
 * Is a dialog over the window?
 *
 * Asked of the document rather than of `panels`, and that is the point.
 * `panels` knows about the two dialogs it owns -- settings and this help
 * sheet -- and knows nothing about the half-dozen a component raises for
 * itself: the delete confirmation, a journal's settings, the "subscribe to a
 * calendar" sheet. So a bare letter stayed live behind all of them, and `j`
 * pressed over a confirmation asking whether to delete an entry quietly
 * opened a different one behind it.
 *
 * The invariant this leans on is already written down and already relied on:
 * every modal in the application declares `aria-modal="true"`, which is what
 * `trapFocus` is a promise about. A dialog that forgets it has a bigger
 * problem than its shortcuts.
 */
function dialogOpen(): boolean {
  // Nothing counts while the help sheet is asking what applies: it is asking
  // about the window it will not be covering. See `panels.listing`.
  if (panels.listing) return false
  if (panels.settings !== null || panels.shortcuts || panels.palette) return true
  return document.querySelector('[aria-modal="true"]') !== null
}

/** Anywhere past the lock screen, with no dialog over the window. */
function anywhere(): boolean {
  return app.screen === 'main' && !dialogOpen()
}

/** Move the journal's selection by `step` rows through the loaded list. */
function stepEntry(step: 1 | -1) {
  const rows = app.entries
  if (rows.length === 0) return
  const at = rows.findIndex((e) => e.id === app.selectedEntry)
  const next = rows[Math.min(rows.length - 1, Math.max(0, at + step))]
  if (next && next.id !== app.selectedEntry) void app.openEntry(next.id)
}

/**
 * The table.
 *
 * Ordered by group, and the groups by how often they are reached for. The
 * help sheet draws it in this order, so this is also the reading order.
 */
export const ACTIONS: Binding[] = [
  // ── The palette ─────────────────────────────────────────────────────
  //
  // First in the table because it is the way to everything else in it. The
  // chord rather than a bare letter, and `whileTyping`, because the whole
  // point is to be reachable from wherever the cursor already is.
  {
    keys: 'mod+k',
    label: 'Find a command',
    group: 'Everywhere',
    keywords: ['palette', 'command', 'run', 'search actions'],
    icon: 'search',
    whileTyping: true,
    // Past the lock screen, and *not* gated on `anywhere()`: the palette is a
    // dialog, so a rule about dialogs would stop it being closed by the same
    // key that opened it.
    when: () => app.screen === 'main',
    run: () => (panels.palette ? panels.closePalette() : panels.openPalette()),
  },
  // ── Go to ───────────────────────────────────────────────────────────
  {
    keys: 'g j',
    label: 'Journal',
    group: 'Go to',
    when: () => anywhere() && app.canShow('journal'),
    run: () => app.setSection('journal'),
  },
  {
    keys: 'g n',
    label: 'Notes',
    group: 'Go to',
    when: () => anywhere() && app.canShow('notes'),
    run: () => app.setSection('notes'),
  },
  {
    keys: 'g t',
    label: 'Todo',
    group: 'Go to',
    when: () => anywhere() && app.canShow('todo'),
    run: () => app.setSection('todo'),
  },
  {
    keys: 'g c',
    label: 'Calendar',
    group: 'Go to',
    when: () => anywhere() && app.canShow('calendar'),
    run: () => app.setSection('calendar'),
  },
  {
    keys: 'g l',
    label: 'Library',
    group: 'Go to',
    when: () => anywhere() && app.canShow('library'),
    run: () => app.setSection('library'),
  },
  {
    keys: 'g o',
    label: 'Overview',
    group: 'Go to',
    when: () => anywhere() && app.canShow('overview'),
    run: () => app.setSection('overview'),
  },
  {
    keys: 'mod+j',
    label: 'The next app',
    group: 'Go to',
    whileTyping: true,
    when: anywhere,
    run: () => app.nextSection(),
  },

  // ── Everywhere ──────────────────────────────────────────────────────
  {
    keys: 'mod+n',
    label: 'Start the next thing',
    group: 'Everywhere',
    whileTyping: true,
    when: anywhere,
    run: () => create(),
  },
  {
    keys: 'c',
    label: 'Start the next thing',
    group: 'Everywhere',
    when: anywhere,
    run: () => create(),
  },
  {
    keys: '/',
    label: 'Search this app',
    group: 'Everywhere',
    when: anywhere,
    run: focusSearch,
  },
  {
    keys: 'mod+f',
    label: 'Search this app',
    group: 'Everywhere',
    whileTyping: true,
    when: anywhere,
    run: focusSearch,
  },
  {
    keys: 'a',
    label: 'The assistant',
    group: 'Everywhere',
    when: () => anywhere() && agent.supported,
    run: () => void agent.toggle(),
  },
  {
    keys: 'mod+,',
    label: 'Settings',
    group: 'Everywhere',
    whileTyping: true,
    when: anywhere,
    run: () => panels.openSettings(),
  },
  // These two are deliberately the only chords a dialog does not stop, and
  // for the same reason: neither moves the caret or navigates. Writing to
  // disk and sealing the vault are things somebody should be able to do from
  // wherever they are, including from on top of a confirmation they have not
  // answered -- and a lock takes every dialog with it on the way out.
  {
    keys: 'mod+s',
    label: 'Write everything to disk now',
    group: 'Everywhere',
    whileTyping: true,
    when: () => app.screen === 'main',
    run: () => {
      void app.flush()
      void todo.flush()
      void calendar.flush()
      void library.flush()
    },
  },
  {
    keys: 'mod+l',
    label: 'Lock the screen',
    group: 'Everywhere',
    whileTyping: true,
    when: () => app.screen === 'main',
    run: () => void app.lockScreen(),
  },
  {
    keys: '?',
    label: 'This list',
    group: 'Everywhere',
    // Opens when nothing is over the window, and closes when the thing over
    // the window is this sheet -- which is what makes it a toggle rather
    // than a key that only works in one direction.
    when: () => app.screen === 'main' && (panels.shortcuts || anywhere()),
    run: () => panels.toggleShortcuts(),
  },

  // ── Journal ─────────────────────────────────────────────────────────
  {
    keys: 'j',
    label: 'The entry below',
    group: 'Journal',
    when: () => anywhere() && inApp('journal')(),
    run: () => stepEntry(1),
  },
  {
    keys: 'k',
    label: 'The entry above',
    group: 'Journal',
    when: () => anywhere() && inApp('journal')(),
    run: () => stepEntry(-1),
  },
  {
    keys: 's',
    label: 'Star this entry',
    group: 'Journal',
    when: () => anywhere() && inApp('journal')() && !!app.selectedEntry,
    run: () => void app.toggleStar(app.selectedEntry!),
  },
  {
    keys: 'p',
    label: 'Pin this entry to the top',
    group: 'Journal',
    when: () => anywhere() && inApp('journal')() && !!app.selectedEntry,
    run: () => void app.togglePin(app.selectedEntry!),
  },

  // ── Todo ────────────────────────────────────────────────────────────
  {
    keys: 'x',
    label: 'Show or hide finished tasks',
    group: 'Todo',
    when: () => anywhere() && inApp('todo')(),
    run: () => todo.setStatusFilter(todo.statusFilter === 'done' ? 'open' : 'done'),
  },
  {
    keys: 'b',
    label: 'The board, or the list',
    group: 'Todo',
    when: () => anywhere() && inApp('todo')() && todo.boardable,
    run: () => todo.setView(todo.view === 'board' ? 'list' : 'board'),
  },

  // ── Calendar ────────────────────────────────────────────────────────
  //
  // These were a `<svelte:window>` handler inside `CalendarView`, which is
  // why they are the only group here that is a move rather than an addition.
  {
    keys: 'd',
    label: 'One day',
    group: 'Calendar',
    when: () => anywhere() && inApp('calendar')(),
    run: () => calendar.setView('day'),
  },
  {
    keys: 'w',
    label: 'A week',
    group: 'Calendar',
    when: () => anywhere() && inApp('calendar')(),
    run: () => calendar.setView('week'),
  },
  {
    keys: 'm',
    label: 'A month',
    group: 'Calendar',
    when: () => anywhere() && inApp('calendar')(),
    run: () => calendar.setView('month'),
  },
  {
    keys: 't',
    label: 'Jump to today',
    group: 'Calendar',
    when: () => anywhere() && inApp('calendar')(),
    run: () => calendar.goToday(),
  },
  {
    keys: 'ArrowLeft',
    label: 'Back',
    group: 'Calendar',
    when: () => anywhere() && inApp('calendar')(),
    run: () => calendar.step(-1),
  },
  {
    keys: 'ArrowRight',
    label: 'Forward',
    group: 'Calendar',
    when: () => anywhere() && inApp('calendar')(),
    run: () => calendar.step(1),
  },
  {
    keys: 'Escape',
    label: 'Drop the selection',
    group: 'Calendar',
    when: () => anywhere() && inApp('calendar')() && calendar.selection !== null,
    run: () => (calendar.selection = null),
  },
  {
    keys: 'Backspace',
    label: 'Remove the selected block',
    group: 'Calendar',
    when: () => anywhere() && inApp('calendar')() && calendar.selection?.kind === 'block',
    run: () => removeSelectedBlock(),
  },
  {
    keys: 'Delete',
    label: 'Remove the selected block',
    group: 'Calendar',
    when: () => anywhere() && inApp('calendar')() && calendar.selection?.kind === 'block',
    run: () => removeSelectedBlock(),
  },

  // ── Library ─────────────────────────────────────────────────────────
  {
    keys: 'v',
    label: 'Covers, or a list',
    group: 'Library',
    when: () => anywhere() && inApp('library')(),
    run: () => library.setView(library.view === 'grid' ? 'list' : 'grid'),
  },
  {
    keys: 's',
    label: 'Favourite this item',
    group: 'Library',
    when: () => anywhere() && inApp('library')() && !!library.selected,
    run: () => void library.toggleFavourite(library.selected!),
  },

  // ── Quick actions ───────────────────────────────────────────────────
  //
  // The rows the tray offers as well, and the ones that have no shortcut at
  // all. They are here rather than beside each store for the reason the whole
  // table exists: three surfaces read this list -- the keyboard, the tray and
  // the palette -- and an action declared anywhere else reaches at most one of
  // them.
  //
  // A tray row needs an `id`, because the shell sends a description across to
  // Rust and gets an id back; everything else stays on this side. `group` is
  // what decides where it sits in the menu, so the menu's shape is this
  // table's reading order.
  {
    id: 'journal:new-entry',
    label: 'New journal entry',
    group: 'Journal',
    keywords: ['write', 'today', 'note'],
    icon: 'pencil',
    tray: true,
    // A vault with no journal in it has nowhere to put an entry.
    when: () => app.screen === 'main' && app.journals.length > 0,
    run: async () => {
      if (await app.goTo('journal')) await app.newEntry()
    },
  },
  {
    id: 'todo:add',
    label: 'Add a task',
    group: 'Todo',
    keywords: ['todo', 'capture'],
    icon: 'plus',
    tray: true,
    // A backend that holds journals only has no task domain, so the action is
    // not hidden behind a disabled item -- it is not there.
    when: () => app.screen === 'main' && app.supportsTasks,
    run: async () => {
      if (await app.goTo('todo')) todo.focusCapture()
    },
  },
  {
    id: 'calendar:book-now',
    label: 'Set an hour aside',
    group: 'Calendar',
    keywords: ['book', 'time', 'schedule'],
    icon: 'calendar',
    tray: true,
    when: () => app.screen === 'main' && app.supportsCalendar,
    run: async () => {
      if (await app.goTo('calendar')) await calendar.bookNow()
    },
  },
  {
    id: 'calendar:timer',
    // One row that is also a state, rather than two that are sometimes greyed
    // out. A tray is where you glance to find out whether you left the timer
    // running, so the tick has to be readable without opening anything
    // further -- and the elapsed figure is deliberately *not* in the label,
    // because a menu you have to open is not a clock and putting it there
    // would rebuild the menu once a second.
    label: 'Track time',
    group: 'Calendar',
    keywords: ['timer', 'stopwatch', 'stop'],
    icon: 'clock',
    tray: true,
    checked: () => calendar.timer !== null,
    // Starting or stopping a timer is a thing you do on your way past.
    raise: false,
    when: () => app.screen === 'main' && app.supportsCalendar,
    run: () => (calendar.timer ? calendar.stopTimer() : calendar.startTimer({ type: 'adhoc' })),
  },
  {
    id: 'overview:today',
    label: 'How today is going',
    group: 'Overview',
    keywords: ['now', 'day', 'progress'],
    icon: 'compass',
    tray: true,
    when: () => app.screen === 'main' && app.supportsOverview,
    run: async () => {
      if (await app.goTo('overview')) overview.setPane('today')
    },
  },
  {
    id: 'overview:week',
    label: 'Where the week went',
    group: 'Overview',
    keywords: ['balance', 'report', 'time', 'chart'],
    icon: 'compass',
    tray: true,
    when: () => app.screen === 'main' && app.supportsOverview,
    run: async () => {
      if (await app.goTo('overview')) overview.setPane('week')
    },
  },
  {
    id: 'overview:log',
    label: 'Record a reading',
    group: 'Overview',
    keywords: ['tracker', 'habit', 'dose', 'tick'],
    icon: 'compass',
    tray: true,
    when: () => app.screen === 'main' && app.supportsOverview,
    run: async () => {
      // Straight to the pane that has the field, and the field opens itself.
      // A quick action from the menu bar has no component to reach for, which
      // is the same problem `focusCapture` solves for the todo app's line.
      if (await app.goTo('overview')) {
        overview.setPane('today')
        overview.wantsLog = true
      }
    },
  },
  {
    id: 'overview:goals',
    label: 'Goals',
    group: 'Overview',
    keywords: ['goal', 'aim', 'intention', 'purpose'],
    icon: 'compass',
    tray: true,
    when: () => app.screen === 'main' && app.supportsOverview,
    run: async () => {
      if (await app.goTo('overview')) overview.setPane('goals')
    },
  },
  {
    // The Overview's capture action, reached by `C` there. Palette-only: the
    // tray already offers the pane, and one more row for the thing that pane
    // opens with would be two ways to say the same thing.
    label: 'Add a goal',
    group: 'Overview',
    keywords: ['new goal', 'aim'],
    icon: 'plus',
    when: () => app.screen === 'main' && app.supportsOverview,
    run: async () => {
      if (await app.goTo('overview')) newGoal()
    },
  },
  {
    id: 'library:add',
    // The action a tray is for: something was recommended to you while you
    // were doing something else, and it needs to land somewhere before you
    // forget it.
    label: 'Add to library',
    group: 'Library',
    keywords: ['shelf', 'book', 'film', 'read', 'watch'],
    icon: 'plus',
    tray: true,
    when: () => app.screen === 'main' && app.supportsLibrary,
    run: async () => {
      if (await app.goTo('library')) library.focusCapture()
    },
  },
  {
    id: 'notes:new',
    label: 'New note',
    group: 'Notes',
    keywords: ['write', 'jot', 'memo', 'draft'],
    icon: 'pencil',
    tray: true,
    when: () => app.screen === 'main' && app.supportsNotes,
    run: async () => {
      if (await app.goTo('notes')) await notes.create()
    },
  },
  {
    label: 'Search notes',
    group: 'Notes',
    keywords: ['find', 'note'],
    icon: 'search',
    when: () => app.screen === 'main' && app.supportsNotes,
    run: async () => {
      if (await app.goTo('notes')) focusSearch()
    },
  },
  {
    id: 'vault:lock',
    label: 'Lock this screen',
    group: 'Vault',
    keywords: ['sign out', 'seal', 'away'],
    icon: 'lock',
    tray: true,
    // The one action here that is about *not* coming back to the window.
    raise: false,
    // Nothing to lock on a vault with no password on it.
    when: () => app.screen === 'main' && app.status?.encrypted === true,
    run: () => void app.lockScreen(),
  },
  {
    id: 'vault:lock-all',
    label: 'Lock the vault everywhere',
    group: 'Vault',
    keywords: ['sign out', 'seal', 'forget', 'key', 'everywhere', 'all'],
    icon: 'lock',
    tray: true,
    raise: false,
    // The heavier of the two, and worth spelling out where it is offered:
    // this drops the key. Every other window looking at this vault goes to
    // its lock screen, and the assistant stops until somebody types the
    // password again.
    when: () => app.screen === 'main' && app.status?.encrypted === true,
    run: () => void app.lock(),
  },

  // ── Palette only ────────────────────────────────────────────────────
  //
  // Worth doing and not worth a key. There are more things than there are
  // comfortable chords, and a palette is where the rest live -- which is also
  // what stops the shortcut table growing a second tier of unmemorable
  // three-key sequences.
  {
    label: 'Settings',
    group: 'Everywhere',
    keywords: ['preferences', 'options', 'configure'],
    icon: 'settings',
    when: () => app.screen === 'main',
    run: () => panels.openSettings(),
  },
  {
    label: 'About you',
    group: 'Everywhere',
    keywords: ['profile', 'name', 'birthday', 'me', 'owner'],
    icon: 'star',
    when: () => app.screen === 'main',
    run: () => panels.openSettings('profile'),
  },
  {
    label: 'Habits',
    group: 'Overview',
    keywords: ['streak', 'tracker', 'heatmap'],
    when: () => app.screen === 'main' && app.supportsOverview,
    run: async () => {
      if (await app.goTo('overview')) overview.setPane('habits')
    },
  },
  {
    label: 'Refresh subscribed calendars',
    group: 'Calendar',
    keywords: ['sync', 'feed', 'ics'],
    when: () => app.screen === 'main' && app.supportsCalendar,
    run: () => calendar.syncDue(true),
  },
  {
    label: 'Use a vault on another computer',
    group: 'Vault',
    keywords: ['remote', 'server', 'connect', 'pair'],
    icon: 'monitor',
    when: () => app.screen === 'main' && !app.remote,
    run: () => panels.openSettings('vault'),
  },
  {
    label: 'Stop using the other computer',
    group: 'Vault',
    keywords: ['disconnect', 'local', 'remote'],
    icon: 'monitor',
    when: () => app.screen === 'main' && !!app.remote,
    run: () => app.disconnectRemote(),
  },
]

/** What "the next thing" means in the app that is open. */
function create() {
  if (app.section === 'notes') void notes.create()
  else if (app.section === 'todo') todo.focusCapture()
  else if (app.section === 'calendar') void calendar.bookNow()
  else if (app.section === 'library') library.focusCapture()
  else if (app.section === 'overview') newGoal()
  else void app.newEntry()
}

/**
 * "The next thing" in the Overview is a goal.
 *
 * The composer belongs to the goals pane and there is one per role, so this
 * goes to that pane and puts the caret in the first of them. Reaching it by
 * selector is what this file already does for the search field.
 */
function newGoal() {
  overview.setPane('goals')
  setTimeout(() => document.querySelector<HTMLInputElement>('[data-newgoal]')?.focus(), 0)
}

function removeSelectedBlock() {
  const selection = calendar.selection
  if (selection?.kind === 'block') void calendar.removeBlock(selection.id)
}

/**
 * The dispatcher: one window handler, and the half-finished sequence.
 *
 * The pending chords are `$state` so the window can show what has been
 * pressed -- `g …` in the corner -- which is the difference between a
 * sequence that feels deliberate and one that feels like the keyboard
 * stopped responding for a second.
 */
class Shortcuts {
  /** The chords pressed so far, when a sequence is half done. */
  pending = $state<string[]>([])
  #timer: ReturnType<typeof setTimeout> | null = null

  /** Handle a key press. Returns true if a shortcut took it. */
  press(event: KeyboardEvent): boolean {
    // Something nearer the key has already dealt with it. The assistant's
    // resize handle answers the arrow keys, and without this the same
    // ArrowLeft both narrowed the rail and paged the calendar behind it.
    if (event.defaultPrevented) return false
    const chord = chordOf(event)
    if (chord === null) return false

    // Escape always abandons a half-finished sequence first, and is then
    // matched on its own. It is never part of a sequence and never
    // swallowed on a miss: what it usually closes is whatever is open, and
    // that is the business of the thing that is open.
    if (chord === 'Escape') this.#clear()

    const typing = isTyping(event.target)
    const pressed = chord === 'Escape' ? [chord] : [...this.pending, chord]
    const { hit, pending } = match(ACTIONS, pressed, typing)

    if (hit) {
      this.#clear()
      event.preventDefault()
      hit.run()
      return true
    }

    if (pending) {
      this.pending = pressed
      // Swallowed, because the `g` of `g j` must not also reach the page.
      event.preventDefault()
      this.#arm()
      return true
    }

    // Not a shortcut. If a sequence was in progress it is abandoned here,
    // and the key is *not* re-interpreted on its own: pressing `g` and then
    // `q` should do nothing, rather than doing whatever `q` does.
    if (this.pending.length > 0) {
      this.#clear()
      event.preventDefault()
      return true
    }
    return false
  }

  #arm() {
    if (this.#timer) clearTimeout(this.#timer)
    this.#timer = setTimeout(() => {
      this.#timer = null
      this.pending = []
    }, SEQUENCE_MS)
  }

  #clear() {
    if (this.#timer) clearTimeout(this.#timer)
    this.#timer = null
    if (this.pending.length > 0) this.pending = []
  }
}

export const shortcuts = new Shortcuts()

/**
 * Every action that applies right now, in the order the table declares them.
 *
 * What the palette draws. `panels.listing` is raised for the duration for the
 * same reason the help sheet raises it -- the palette is a dialog, and without
 * this every `when` that asks "is a dialog open" would answer yes and the
 * palette would offer nothing at all.
 */
export function applicableActions(): Binding[] {
  panels.listing = true
  try {
    return ACTIONS.filter((a) => !a.when || a.when())
  } finally {
    panels.listing = false
  }
}

/**
 * The bindings that apply right now, grouped, for the help sheet.
 *
 * `panels.listing` is raised for the duration, so the sheet does not
 * disqualify the very shortcuts it exists to list -- see the field's own
 * comment. It is lowered in a `finally`, because a `when` closure reads four
 * stores and a throw from any of them would otherwise leave the window
 * believing no dialog is open.
 */
export function applicableBindings(): { group: string; items: Binding[] }[] {
  panels.listing = true
  try {
    return group(ACTIONS.filter((a) => a.keys !== undefined))
  } finally {
    panels.listing = false
  }
}

function group(bindings: Binding[]): { group: string; items: Binding[] }[] {
  const out: { group: string; items: Binding[] }[] = []
  const seen = new Map<string, Set<string>>()
  for (const binding of bindings) {
    if (binding.when && !binding.when()) continue
    let group = out.find((g) => g.group === binding.group)
    if (!group) {
      group = { group: binding.group, items: [] }
      out.push(group)
      seen.set(binding.group, new Set())
    }
    // Two chords for one action -- `c` and Ctrl+N, `/` and Ctrl+F -- is one
    // row in the sheet with both spellings on it, not two rows saying the
    // same thing. The first one declared wins the row.
    const labels = seen.get(binding.group)!
    if (labels.has(binding.label)) continue
    labels.add(binding.label)
    group.items.push(binding)
  }
  return out
}

/** Every spelling of one action, for the row the sheet draws. */
export function spellings(binding: Binding): string[] {
  panels.listing = true
  try {
    return ACTIONS.filter(
      (b) =>
        b.keys !== undefined &&
        b.group === binding.group &&
        b.label === binding.label &&
        (!b.when || b.when()),
    ).map((b) => b.keys!)
  } finally {
    panels.listing = false
  }
}
