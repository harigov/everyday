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
import { mail } from './mail.svelte'
import { menu } from './menu.svelte'
import { notes } from './notes.svelte'
import { overview } from './overview.svelte'
import { panels } from './panels.svelte'
import { assistant } from './assistant.svelte'
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
  return modalInDom()
}

/**
 * The DOM half of `dialogOpen`, answered once per sweep of the table.
 *
 * The query itself is cheap; asking it once per *binding* is not. `match`
 * calls `when` on every candidate row and most of the `when`s here are
 * `anywhere`, so a single press asked the document thirty-two times -- four
 * even with the caret in a field, where the table narrows to the chords that
 * fire while typing. And the keyboard handler runs on every keystroke,
 * including every keystroke typed into an editor, where nothing is over the
 * window and the answer is the same `false` every time.
 *
 * It is also worse than it looks in a micro-benchmark: typing mutates the
 * document, which throws away whatever the engine had cached about the
 * selector, so each of those queries is a fresh walk. Measured on a long
 * journal entry this was the largest piece of the application's own code on
 * the typing path -- more than the editor's own `onUpdate`.
 *
 * Scoped rather than cached outright, because the answer genuinely does
 * change: a dialog opens and the next keystroke must see it. `oneSweep` holds
 * it for the length of one pass over the table and drops it on the way out,
 * so nothing that runs *between* two passes -- an action opening a dialog,
 * a component mounting one -- can be answered from a stale reading.
 *
 * And the scope belongs to whoever opened it. `sweeping` is what makes that
 * true: this table has readers other than the keyboard -- the tray composing
 * its menu, the app bar building a right-click menu from `entriesFor` -- and
 * they run outside any sweep. Caching for them as well would mean a menu
 * built from whatever was on screen at some unrelated earlier moment, which
 * a dialog dismissed with the mouse leaves nothing behind to correct.
 */
let sweeping = false
let modalSeen: boolean | null = null

function modalInDom(): boolean {
  if (!sweeping) return document.querySelector('[aria-modal="true"]') !== null
  modalSeen ??= document.querySelector('[aria-modal="true"]') !== null
  return modalSeen
}

/**
 * Run `sweep` with one shared answer to "is a dialog over the window?".
 *
 * Wrap a pass over the table, never the running of what it found: an action
 * is entitled to open a dialog, and the next thing to ask has to see it.
 *
 * The outer state is put back rather than cleared, so that a sweep nested
 * inside another -- there is none today, and an action reading the table
 * would make one -- cannot end the answer the outer pass is still using.
 */
function oneSweep<T>(sweep: () => T): T {
  const outer = sweeping
  sweeping = true
  try {
    return sweep()
  } finally {
    sweeping = outer
    if (!outer) modalSeen = null
  }
}

/** Anywhere past the lock screen, with no dialog over the window. */
function anywhere(): boolean {
  return app.screen === 'main' && !dialogOpen()
}

/**
 * Dev-only: log how long a Mail shortcut takes from key to redrawn screen.
 *
 * Against "The speed budget" in `docs/plans/mail.md` -- 60ms, key to
 * redrawn screen -- which is measured, not admired, so this measures it.
 * `performance.now()` is read on either side of the frame the key's own
 * synchronous `run()` schedules; a `requestAnimationFrame` after `run()`
 * fires once the browser has actually painted that work, which is the
 * moment the budget is about, not the moment `run()` returns -- most of
 * these are optimistic and return before their `await` has gone anywhere.
 * Eliminated from a release build the same way `MOCK` is: `import.meta.env.DEV`
 * is a literal Vite substitutes at build time.
 */
function timeToPaint(hit: Binding) {
  if (!import.meta.env.DEV || hit.group !== 'Mail') return
  const start = performance.now()
  requestAnimationFrame(() => {
    requestAnimationFrame(() => {
      const ms = performance.now() - start
      console.debug(`[mail] ${hit.keys} "${hit.label}" → paint in ${ms.toFixed(1)}ms`)
    })
  })
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
 * The headings actions are filed under, in the order every surface draws
 * them.
 *
 * One list rather than two. The help sheet and the system tray both draw the
 * table under headings, and each used to decide the order for itself -- the
 * tray from a `TRAY_GROUPS` list of its own, the sheet from whichever row
 * happened to be declared first. So an app added later sat fifth in one menu
 * and last in the other, for no reason anybody could see. The order here is
 * the app bar's, with the two headings that belong to no app at the top and
 * the vault's at the foot.
 */
export const GROUPS = [
  'Everywhere',
  'Go to',
  'Journal',
  'Notes',
  'Todo',
  'Calendar',
  'Library',
  'Mail',
  'Overview',
  'Assistant',
  'Vault',
] as const
export type Group = (typeof GROUPS)[number]

/**
 * The table.
 *
 * Grouped by app, keyboard rows first and then the quick actions, so a new
 * shortcut has an obvious place to go. What the *reader* sees is ordered by
 * [`GROUPS`] rather than by this file, so where a row sits here decides only
 * its place within its own heading.
 *
 * `group` is narrowed to [`Group`] rather than left as `Binding`'s wider
 * `string`: a misspelt heading would otherwise be a section of one that no
 * surface knows how to order.
 */
export const ACTIONS: (Binding & { group: Group })[] = [
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
    keys: 'g m',
    label: 'Mail',
    group: 'Go to',
    when: () => anywhere() && app.canShow('mail'),
    run: () => app.setSection('mail'),
  },
  {
    keys: 'g o',
    label: 'Overview',
    group: 'Go to',
    when: () => anywhere() && app.canShow('overview'),
    run: () => app.setSection('overview'),
  },
  {
    keys: 'g a',
    label: 'Assistant',
    group: 'Go to',
    when: () => anywhere() && app.canShow('assistant'),
    run: () => app.setSection('assistant'),
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
    keywords: ['preferences', 'options', 'configure'],
    icon: 'settings',
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
    // Every store that debounces a write, through the one registry -- not a
    // list of them kept here. This row named four stores and the notes app
    // was never added to it, so the only shortcut in the application whose
    // whole promise is "everything, now" quietly did not write notes.
    run: () => void app.flushAll(),
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

  // ── Mail ────────────────────────────────────────────────────────────
  //
  // Three deliberate departures from `docs/plans/mail.md`'s own wording,
  // each because the table this file already agreed to collided with it:
  //
  // - `Shift+U`/`Shift+I` (mark unread/read) cannot exist in this table at
  //   all. `chordOf` in `keys.ts` never records Shift on a single printable
  //   character -- "what is typed is what is matched", so `?` works on every
  //   layout -- which means a live Shift+U keypress and a live U keypress
  //   both resolve to the chord `u`, and a binding declared `shift+u` can
  //   never be reached by an actual key. Bare `u`/`i` are used instead:
  //   still Gmail's own mnemonic, just without a modifier that cannot survive
  //   the trip through this file's own matcher.
  // - `a` for reply-all collides with the global "Everywhere" row that
  //   toggles the assistant open, declared earlier in this table and so
  //   always the one `match` picks. Reply-all is `w` here -- arbitrary, kept
  //   short of a better mnemonic once `a` was gone.
  // - `g t` for Sent collides with Todo's own `g t`, declared above and
  //   `when`-gated to nothing narrower than "the backend has tasks", so it
  //   wins from inside Mail too. Sent is `g u` -- also arbitrary, for the
  //   same reason.
  {
    keys: 'j',
    label: 'The thread below',
    group: 'Mail',
    when: () => anywhere() && inApp('mail')(),
    run: () => void mail.moveSelection(1),
  },
  {
    keys: 'k',
    label: 'The thread above',
    group: 'Mail',
    when: () => anywhere() && inApp('mail')(),
    run: () => void mail.moveSelection(-1),
  },
  {
    keys: 'Enter',
    label: 'Open the thread',
    group: 'Mail',
    when: () => anywhere() && inApp('mail')() && !!mail.selectedThread,
    run: () => void mail.openThreadById(mail.selectedThread!),
  },
  {
    keys: 'o',
    label: 'Open the thread',
    group: 'Mail',
    when: () => anywhere() && inApp('mail')() && !!mail.selectedThread,
    run: () => void mail.openThreadById(mail.selectedThread!),
  },
  {
    keys: 'Escape',
    label: 'Back to the list',
    group: 'Mail',
    when: () => anywhere() && inApp('mail')() && !!mail.openThread,
    run: () => mail.closeThread(),
  },
  {
    keys: 'e',
    label: 'Archive',
    group: 'Mail',
    when: () => anywhere() && inApp('mail')() && !!mail.selectedThread,
    run: () => void mail.archive(mail.selectedThread!),
  },
  {
    keys: '#',
    label: 'Trash',
    group: 'Mail',
    when: () => anywhere() && inApp('mail')() && !!mail.selectedThread,
    run: () => void mail.trash(mail.selectedThread!),
  },
  {
    keys: 's',
    label: 'Star',
    group: 'Mail',
    when: () => anywhere() && inApp('mail')() && !!mail.selectedThread,
    run: () => void mail.toggleStar(mail.selectedThread!),
  },
  {
    keys: 'u',
    label: 'Mark unread',
    group: 'Mail',
    when: () => anywhere() && inApp('mail')() && !!mail.selectedThread,
    run: () => void mail.markUnread(mail.selectedThread!),
  },
  {
    keys: 'i',
    label: 'Mark read',
    group: 'Mail',
    when: () => anywhere() && inApp('mail')() && !!mail.selectedThread,
    run: () => void mail.markRead(mail.selectedThread!),
  },
  {
    keys: 'h',
    label: 'Snooze…',
    group: 'Mail',
    when: () => anywhere() && inApp('mail')() && !!mail.selectedThread,
    run: () => (mail.wantsSnooze = mail.selectedThread),
  },
  {
    keys: 'l',
    label: 'Label…',
    group: 'Mail',
    when: () => anywhere() && inApp('mail')() && !!mail.selectedThread,
    run: () => promptLabel(),
  },
  {
    keys: 'v',
    label: 'Move to…',
    group: 'Mail',
    when: () => anywhere() && inApp('mail')() && !!mail.selectedThread,
    run: () => moveMenu(),
  },
  {
    keys: 'r',
    label: 'Reply',
    group: 'Mail',
    when: () => anywhere() && inApp('mail')() && !!mail.openThread,
    run: () => void mail.reply(mail.openThread!.messages.at(-1)!.id, false),
  },
  {
    keys: 'w',
    label: 'Reply all',
    group: 'Mail',
    when: () => anywhere() && inApp('mail')() && !!mail.openThread,
    run: () => void mail.reply(mail.openThread!.messages.at(-1)!.id, true),
  },
  {
    keys: 'f',
    label: 'Forward',
    group: 'Mail',
    when: () => anywhere() && inApp('mail')() && !!mail.openThread,
    run: () => void mail.forward(mail.openThread!.messages.at(-1)!.id),
  },
  {
    keys: 'g i',
    label: 'Inbox',
    group: 'Mail',
    when: () => anywhere() && inApp('mail')(),
    run: () => void selectMailboxByRole('inbox'),
  },
  {
    keys: 'g s',
    label: 'Starred',
    group: 'Mail',
    when: () => anywhere() && inApp('mail')(),
    run: () => void selectMailboxByName('Starred'),
  },
  {
    keys: 'g d',
    label: 'Drafts',
    group: 'Mail',
    when: () => anywhere() && inApp('mail')(),
    run: () => void selectMailboxByRole('drafts'),
  },
  {
    keys: 'g u',
    label: 'Sent',
    group: 'Mail',
    when: () => anywhere() && inApp('mail')(),
    run: () => void selectMailboxByRole('sent'),
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
    id: 'overview:page',
    // The Overview used to offer four rows here, one per pane. There are no
    // panes: it is one page of whatever somebody put on it, so it is one row.
    label: 'How things are going',
    group: 'Overview',
    keywords: ['now', 'day', 'week', 'balance', 'dashboard', 'chart', 'progress'],
    icon: 'compass',
    tray: true,
    when: () => app.screen === 'main' && app.supportsOverview,
    run: () => void app.goTo('overview'),
  },
  {
    id: 'overview:log',
    label: 'Record a reading',
    group: 'Overview',
    keywords: ['tracker', 'habit', 'dose', 'tick'],
    icon: 'plus',
    tray: true,
    when: () => app.screen === 'main' && app.supportsOverview,
    run: async () => {
      // Straight to the page, and the field opens itself. A quick action from
      // the menu bar has no component to reach for, which is the same problem
      // `focusCapture` solves for the todo app's line.
      if (await app.goTo('overview')) overview.wantsLog = true
    },
  },
  {
    id: 'overview:arrange',
    label: 'Arrange the Overview',
    group: 'Overview',
    keywords: ['widget', 'dashboard', 'layout', 'customise', 'customize'],
    icon: 'grip',
    when: () => app.screen === 'main' && app.supportsOverview,
    run: async () => {
      if (await app.goTo('overview')) overview.editing = true
    },
  },
  {
    id: 'todo:goals',
    label: 'Goals',
    group: 'Todo',
    keywords: ['goal', 'aim', 'intention', 'purpose', 'role'],
    icon: 'target',
    tray: true,
    when: () => app.screen === 'main' && app.supportsOverview && app.supportsTasks,
    run: async () => {
      if (await app.goTo('todo')) await todo.setScope({ kind: 'goals' })
    },
  },
  {
    // Palette-only: the tray already offers the pane, and one more row for
    // the thing that pane opens with would be two ways to say the same thing.
    label: 'Add a goal',
    group: 'Todo',
    keywords: ['new goal', 'aim'],
    icon: 'plus',
    when: () => app.screen === 'main' && app.supportsOverview && app.supportsTasks,
    run: () => void newGoal(),
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
    id: 'mail:compose',
    label: 'Compose a message',
    group: 'Mail',
    keywords: ['email', 'new', 'write'],
    icon: 'plus',
    tray: true,
    when: () => app.screen === 'main' && app.supportsMail,
    run: async () => {
      if (await app.goTo('mail')) await mail.compose()
    },
  },
  {
    id: 'assistant:runs',
    label: 'What the assistant did',
    group: 'Assistant',
    keywords: ['runs', 'log', 'routine', 'brief', 'report'],
    icon: 'inbox',
    tray: true,
    when: () => app.screen === 'main' && app.supportsAssistant,
    run: async () => {
      if (await app.goTo('assistant')) assistant.setPane('runs')
    },
  },
  {
    id: 'assistant:new-routine',
    label: 'New routine',
    group: 'Assistant',
    keywords: ['schedule', 'standing', 'every day', 'automate'],
    icon: 'clock',
    tray: true,
    when: () => app.screen === 'main' && app.supportsRoutines,
    run: async () => {
      if (await app.goTo('assistant')) await assistant.draft()
    },
  },
  {
    label: 'What the assistant remembers',
    group: 'Assistant',
    keywords: ['memory', 'facts', 'forget'],
    icon: 'sparkle',
    when: () => app.screen === 'main' && app.supportsAssistant,
    run: async () => {
      if (await app.goTo('assistant')) assistant.setPane('memory')
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
    label: 'About you',
    group: 'Everywhere',
    keywords: ['profile', 'name', 'birthday', 'me', 'owner'],
    icon: 'star',
    when: () => app.screen === 'main',
    run: () => panels.openSettings('profile'),
  },
  {
    label: 'Accounts',
    group: 'Everywhere',
    keywords: ['mail', 'email', 'google', 'microsoft', 'icloud', 'fastmail', 'sign in', 'oauth'],
    icon: 'inbox',
    when: () => app.screen === 'main',
    run: () => panels.openSettings('accounts'),
  },
  {
    label: 'Refresh subscribed calendars',
    group: 'Calendar',
    keywords: ['sync', 'feed', 'ics'],
    when: () => app.screen === 'main' && app.supportsCalendar,
    run: () => calendar.syncDue(true),
  },
  {
    label: 'Export my data',
    group: 'Vault',
    keywords: ['export', 'download', 'markdown', 'csv', 'backup', 'leave', 'zip'],
    icon: 'upload',
    when: () => app.screen === 'main',
    run: () => panels.openSettings('data'),
  },
  {
    label: 'Import an export',
    group: 'Vault',
    keywords: ['import', 'restore', 'markdown', 'csv', 'zip', 'read in'],
    icon: 'upload',
    when: () => app.screen === 'main',
    run: () => panels.openSettings('data'),
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

/**
 * What "the next thing" means in the app that is open.
 *
 * A table keyed by `Section`, the same shape `App.svelte`'s `ACCENTS` uses
 * and for the same reason: this used to be an `if`/`else` chain with the
 * journal on the end of it, so a new app that forgot to add a clause did not
 * fail to compile, it silently opened a fresh journal entry instead -- the
 * one answer that is right nowhere else. This does not compile until the
 * new app says what its own "next thing" is.
 */
const CREATE: Record<Section, () => unknown> = {
  journal: () => app.newEntry(),
  notes: () => notes.create(),
  // The goals pane has no task line to put a cursor in, and the next thing
  // somebody wants there is a goal.
  todo: () => (todo.showingGoals ? focusNewGoal() : todo.focusCapture()),
  calendar: () => calendar.bookNow(),
  library: () => library.focusCapture(),
  mail: () => mail.compose(),
  overview: () => (overview.wantsLog = true),
  assistant: () => assistant.draft(),
}

function create() {
  void CREATE[app.section]()
}

/**
 * "The next thing" in the todo app's goals pane is a goal.
 *
 * The composer belongs to that pane and there is one per role, so this goes
 * there and puts the caret in the first of them. Reaching it by selector is
 * what this file already does for the search field.
 */
async function newGoal() {
  if (!(await app.goTo('todo'))) return
  await todo.setScope({ kind: 'goals' })
  focusNewGoal()
}

function focusNewGoal() {
  setTimeout(() => document.querySelector<HTMLInputElement>('[data-newgoal]')?.focus(), 0)
}

function removeSelectedBlock() {
  const selection = calendar.selection
  if (selection?.kind === 'block') void calendar.removeBlock(selection.id)
}

/** `g i`/`g d`/`g u`: the first mailbox of this role in the currently
 *  loaded set, across every account. */
function selectMailboxByRole(role: 'inbox' | 'drafts' | 'sent') {
  const box = mail.mailboxes.find((m) => m.role === role)
  if (box) void mail.selectMailbox(box.id)
}

/** `g s`: the pseudo-mailbox by name -- see `mock-mail.ts`'s own note on
 *  why "Starred" is not a real IMAP mailbox yet. */
function selectMailboxByName(name: string) {
  const box = mail.mailboxes.find((m) => m.remoteName === name)
  if (box) void mail.selectMailbox(box.id)
}

/**
 * `l`: ask for a label and apply it.
 *
 * A plain prompt rather than a picker of existing labels: the mock has
 * nothing resembling Gmail's "create or choose" affordance yet, and a
 * dialog built only to wrap one text field would be a second `ConfirmDialog`
 * wearing a different name. Worth revisiting once real accounts have real
 * label lists to offer.
 */
function promptLabel() {
  const id = mail.selectedThread
  if (!id) return
  const label = window.prompt('Label this thread as:')?.trim()
  if (label) void mail.label(id, label)
}

/**
 * `v`: move to another mailbox, offered as a menu built the same way a
 * row's right-click menu is -- but with no row to click, since this is the
 * keyboard's own path to it. `menu.show` only reads `preventDefault`,
 * `stopPropagation`, `target` and the pointer position off the event it is
 * given, all of which a constructed one answers safely without ever being
 * dispatched.
 */
function moveMenu() {
  const id = mail.selectedThread
  if (!id) return
  const accountId =
    mail.threads.find((t) => t.id === id)?.accountId ?? mail.openThread?.thread.accountId
  const boxes = mail.mailboxes.filter((m) => m.accountId === accountId && m.role !== 'other')
  const items = boxes.map((box) => ({
    label: box.remoteName,
    run: () => void mail.moveTo(id, box.id),
  }))
  const centre = new MouseEvent('contextmenu', {
    clientX: window.innerWidth / 2,
    clientY: window.innerHeight / 2,
    bubbles: false,
    cancelable: true,
  })
  menu.show(centre, items)
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
    // One pass over the table, and -- see `oneSweep` -- one answer to the
    // dialog question for the whole of it. `run` is deliberately outside:
    // what it opens must be visible to the next press, not to this one.
    const { hit, pending } = oneSweep(() => match(ACTIONS, pressed, typing))

    if (hit) {
      this.#clear()
      event.preventDefault()
      timeToPaint(hit)
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
    return distinct(ACTIONS.filter((a) => !a.when || a.when()))
  } finally {
    panels.listing = false
  }
}

/**
 * One row per thing somebody can do, in declaration order.
 *
 * Two chords for one action -- `c` and Ctrl+N, `/` and Ctrl+F -- are one
 * offer, not two, and the first declared is the one that carries it. Both
 * surfaces that *list* the table have to agree about that, and until this was
 * shared only the help sheet did it: the palette drew each pair twice under
 * one key, which Svelte refuses outright.
 */
function distinct(bindings: Binding[]): Binding[] {
  const seen = new Set<string>()
  return bindings.filter((b) => {
    const key = `${b.group}\u0000${b.label}`
    if (seen.has(key)) return false
    seen.add(key)
    return true
  })
}

/**
 * The bindings that apply right now, grouped, for the help sheet.
 *
 * `panels.listing` is raised for the duration, so the sheet does not
 * disqualify the very shortcuts it exists to list -- see the field's own
 * comment. It is lowered in a `finally`, because a `when` closure reads the
 * apps' stores and a throw from any of them would otherwise leave the window
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

/**
 * The applicable bindings, under their headings, in [`GROUPS`] order.
 *
 * A heading nothing applies to is left out rather than drawn empty: on a
 * vault whose backend holds journals and nothing else, most of them are.
 */
function group(bindings: Binding[]): { group: string; items: Binding[] }[] {
  const items = new Map<string, Binding[]>()
  for (const binding of distinct(bindings.filter((b) => !b.when || b.when()))) {
    const under = items.get(binding.group)
    if (under) under.push(binding)
    else items.set(binding.group, [binding])
  }
  return GROUPS.filter((g) => items.has(g)).map((g) => ({ group: g, items: items.get(g)! }))
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
