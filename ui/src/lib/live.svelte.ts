// A write that landed somewhere else, and the list that has to notice.
//
// One window's save is another window's stale list. On a local vault the two
// windows are the same one and it already knows; under server mode they are two
// machines, and this is how the second finds out.
//
// The backend raises a `changed` event after any command that writes, carrying
// what it touched and who did it. The origin is compared on the *server* side
// for a remote client and here for a local one, so a window never reloads under
// its own cursor mid-scroll.
//
// # Why this is a router and not a subscription per store
//
// Because the granularity of the event is deliberately coarse -- an app, not a
// table -- and the thing that reloads is a store's own `refresh`, which each
// store already has for its own reasons. A subscription per store would mean
// every store learning about the event system; one router means adding an app
// is adding a line here.

import { onChange, onLockState, onPalette } from './api'
import { calendar } from './calendar.svelte'
import { library } from './library.svelte'
import { notes } from './notes.svelte'
import { overview } from './overview.svelte'
import { panels } from './panels.svelte'
import { app } from './state.svelte'
import { todo } from './todo.svelte'
import { tracking } from './tracking.svelte'
import type { ChangeEvent, ChangeKind } from './types'

/**
 * How long to gather changes before acting on them.
 *
 * A calendar sync writes a thousand events and a board reorder writes forty
 * tasks, each of which announces itself. Reloading once per row would be a
 * thousand round trips to draw one grid. The window is short enough that a
 * single edit on another machine still feels immediate.
 */
const COALESCE_MS = 250

/**
 * The things that can be reloaded, by name.
 *
 * Named rather than referred to by function, and that is the whole reason this
 * indirection exists. A batch routinely carries several kinds that reload the
 * *same* store -- a task and a project, an item and a log row -- and
 * deduplicating by the function meant comparing two different arrow literals
 * that happened to call the same method. They never matched, so a calendar
 * sync reloaded the grid three times in a row.
 */
export const RELOAD: Record<string, () => Promise<unknown> | void> = {
  journals: () => app.refreshJournals(),
  entries: () => app.queueListRefresh(),
  todo: () => todo.refresh(),
  calendar: () => calendar.refresh(),
  notes: () => notes.refresh(),
  shelves: () => library.refreshKinds(),
  library: () => library.refresh(),
  tracking: () => tracking.refresh(),
  overview: () => overview.refresh(),
  // Settings that moved: the auto-lock, the assistant's configuration. What
  // draws them re-reads on open, so the useful thing is the status word.
  status: () => app.refreshStatus(),
}

/**
 * The kinds that can carry a purpose, and so change what the reports say.
 *
 * A purpose is a pointer on a project, a task, a block, an entry, a shelf item
 * and a tracker, and it *inherits* -- a block's own, else its task's, else that
 * task's project's. So filing one task under a goal changes every chart the
 * Overview draws, while the change event says only "task".
 *
 * Reloading the Overview for all of these unconditionally would mean redrawing
 * a year of balance on every keystroke's autosave. So it is done only when the
 * Overview is the app on screen: a chart nobody is looking at does not need to
 * be right yet, and it re-reads when it is opened.
 */
const PURPOSE_BEARING: ReadonlySet<ChangeKind> = new Set([
  'project',
  'task',
  'block',
  'entry',
  'note',
  'item',
  'tracker',
  'reading',
  'calendar',
])

/** Which of those a change of each kind asks for. */
export const RELOADS: Record<ChangeKind, keyof typeof RELOAD | null> = {
  journal: 'journals',
  entry: 'entries',
  note: 'notes',
  project: 'todo',
  task: 'todo',
  block: 'calendar',
  calendar: 'calendar',
  event: 'calendar',
  shelf: 'shelves',
  item: 'library',
  log: 'library',
  tracker: 'tracking',
  reading: 'tracking',
  // The Overview's own records, and the reports drawn from them. A role or a
  // goal moving changes every chart, which is why they reload the app rather
  // than a list within it -- and why they collapse to one reload when a batch
  // carries both.
  role: 'overview',
  goal: 'overview',
  // The assistant's own thread. The panel reads it when it is opened, and a
  // reply arriving on another machine is not something to interrupt this one
  // with -- so nothing reloads, and the event exists for a future history list.
  conversation: null,
  memory: null,
  settings: 'status',
}

/**
 * What one batch of changes should reload, once each.
 *
 * Its own function so the collapsing is testable without the stores: it is a
 * rule about a table, and the rule is the part that was wrong.
 */
export function targetsFor(kinds: ChangeKind[], overviewShowing = false): string[] {
  const targets = new Set(kinds.map((kind) => RELOADS[kind]).filter((t) => t !== null))
  // See `PURPOSE_BEARING`: these do not name the Overview and still change what
  // it says, but only matter while somebody is looking at it.
  if (overviewShowing && kinds.some((kind) => PURPOSE_BEARING.has(kind))) {
    targets.add('overview')
  }
  return [...targets]
}

class Live {
  #pending = new Set<ChangeKind>()
  #timer: ReturnType<typeof setTimeout> | null = null
  #started = false

  /**
   * Begin listening. Called once, from the root component.
   *
   * Idempotent, because hot reloading in development mounts the root more than
   * once and two subscriptions would mean two reloads per change.
   */
  start() {
    if (this.#started) return
    this.#started = true

    onChange((change: ChangeEvent) => this.#note(change))
    // The OS-wide hotkey. The shell has already raised the window by the time
    // this arrives; all that is left is to open the palette -- and only past
    // the lock screen, because a key pressed while the vault is sealed should
    // not put a list of its contents on screen.
    onPalette(() => {
      if (app.screen === 'main') panels.openPalette()
    })
    onLockState((locked: boolean) => {
      // Not a list to reload: every screen has to leave. A vault that locked
      // itself on the machine holding it locks for everybody looking at it.
      if (locked) {
        void app.lockedElsewhere()
        return
      }
      // An *unlock* only matters if this window is on the lock screen and did
      // not do it itself. The local `unlock` path raises this event too, and
      // starting up again from here ran `app.start()` concurrently with the
      // `enterMain()` that unlock was already running -- two loads of the same
      // lists, racing, with the loser's answer drawn.
      if (app.screen === 'locked' && !app.unlocking) void app.start()
    })
  }

  #note(change: ChangeEvent) {
    if (app.screen !== 'main') return
    // A window never reloads for its own write. It already drew the result,
    // and reloading would move the list under the cursor that made the change.
    //
    // `local` is this process, whichever vault it is looking at. A remote
    // server filters by device id before it sends, so what arrives here from
    // another machine is already somebody else's; this covers the case where
    // the vault is in this process and the sink is talking to itself.
    if (change.origin === 'local') return
    this.#pending.add(change.kind)
    if (this.#timer !== null) return
    this.#timer = setTimeout(() => {
      this.#timer = null
      const kinds = [...this.#pending]
      this.#pending.clear()
      // Deduplicated by *what would run*, not by the kind: a task and a
      // project both reload the todo app, and doing it twice is a wasted round
      // trip on a connection that may be a phone's.
      for (const target of targetsFor(kinds, app.section === 'overview')) {
        void Promise.resolve(RELOAD[target]!()).catch(() => {})
      }
    }, COALESCE_MS)
  }
}

export const live = new Live()
