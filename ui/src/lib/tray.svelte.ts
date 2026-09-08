// Quick actions in the system tray.
//
// The shell owns the icon and the menu widget; this owns what is in it. An
// app registers a function that returns the actions it can offer *right
// now*, and that is the whole API:
//
//     tray.register('todo', TRAY_ORDER.todo, () => {
//       if (!app.supportsTasks) return []
//       return [{ id: 'todo:add', label: 'Add a task', run: () => ... }]
//     })
//
// The function is re-run whenever anything it reads changes, so an action
// that is only sometimes possible says so by disappearing or by setting
// `enabled: false`, rather than by failing when it is chosen. That is the
// reason the registration takes a function rather than a list: a menu built
// once at startup would still be offering "Add a task" on a vault whose
// backend has no tasks, and would still be offering anything at all after
// the vault locked.
//
// Handlers stay on this side. What crosses to Rust is a description --
// labels, ids, enabled flags -- and what comes back is the id that was
// chosen. See `crates/everyday-app/src/tray.rs`.

import { api, isMock, onTrayAction } from './api'
import type { TrayMenuItem } from './types'

/** A thing the menu can do. */
export interface TrayAction {
  /**
   * Stable and unique across every app. Prefix it with the app's name --
   * `todo:add` -- and do not start it with `everyday:`, which the shell
   * keeps for its own two entries.
   */
  id: string
  label: string
  /** Default true. False greys the item out but leaves it visible. */
  enabled?: boolean
  /**
   * Renders a checkbox. For an action that is also a state -- a timer that
   * is or is not running -- where hiding the "off" version would cost the
   * reader the fact that it is off.
   */
  checked?: boolean
  /**
   * Bring the window forward before running. Default true, because almost
   * every quick action ends with a cursor somewhere. Set false for the ones
   * that are precisely about *not* coming back: locking, stopping a timer.
   */
  raise?: boolean
  run: () => void | Promise<void>
}

/** An action, a rule, or a submenu of them. */
export type TrayEntry =
  TrayAction | { separator: true } | { label: string; enabled?: boolean; items: TrayEntry[] }

/**
 * Where each app's actions sit in the menu, top to bottom.
 *
 * A number rather than registration order, because registration order is
 * import order and would make the menu's shape depend on which file happens
 * to import which. Gaps are deliberate: a fourth app can be slotted between
 * two of these without renumbering them.
 */
export const TRAY_ORDER = {
  journal: 10,
  todo: 20,
  calendar: 30,
  /** The vault itself -- locking. Last, and away from the capture actions. */
  vault: 90,
} as const

interface Group {
  source: string
  order: number
  actions: () => TrayEntry[]
}

const SHOW_KEY = 'everyday.tray'

function isSeparator(e: TrayEntry): e is { separator: true } {
  return 'separator' in e
}

function isSubmenu(e: TrayEntry): e is { label: string; enabled?: boolean; items: TrayEntry[] } {
  return 'items' in e
}

class TrayRegistry {
  /**
   * Is the user asking for a tray icon at all? Remembered locally, like the
   * theme: it is a preference about this machine's desktop, not about the
   * vault, and it has to be known before anything is unlocked.
   */
  enabled = $state(true)
  /** Is one actually up? False while the setting is off, or if it failed. */
  showing = $state(false)
  /**
   * Does the shell report nowhere to put an icon? A Linux session with no
   * StatusNotifier host is the usual reason, and Settings says so rather
   * than leaving a switch that appears to do nothing.
   *
   * Only ever the shell's own answer, never an inference from a failed
   * call, and it clears again if a later one succeeds -- a tray host can
   * arrive mid-session.
   */
  unavailable = $state(false)

  #groups = $state<Group[]>([])
  /** Handlers for the menu currently up, by item id. */
  #handlers = new Map<string, TrayAction>()
  /** The last description sent, so an unchanged menu is not resent. */
  #sent: string | null = null
  /** Pushes in flight, chained, so two cannot land out of order. */
  #chain: Promise<unknown> = Promise.resolve()
  #started = false

  constructor() {
    // Read before `start`, because the switch in Settings is drawn from it
    // and the lock screen is drawn before the tray is ever pushed.
    this.enabled = localStorage.getItem(SHOW_KEY) !== 'off'
  }

  /**
   * Is there a shell to ask for a tray at all? False in the browser mock.
   *
   * A build-level fact, deliberately not folded together with
   * `unavailable`: "this is a web page" means the setting is meaningless and
   * should not be drawn, while "this desktop has no tray" means it is
   * meaningful and cannot be honoured, which is worth saying out loud.
   */
  get supported(): boolean {
    return !isMock
  }

  /**
   * Register an app's quick actions.
   *
   * Call it once, at module scope beside the store whose state the actions
   * read. `actions` may be called at any time and must be cheap and free of
   * side effects: it is the menu's definition, not a command.
   *
   * Registering `source` twice replaces the first, which is what makes a hot
   * reload of a store during `make run` leave one copy of its actions in the
   * menu rather than two.
   */
  register(source: string, order: number, actions: () => TrayEntry[]) {
    const rest = this.#groups.filter((g) => g.source !== source)
    this.#groups = [...rest, { source, order, actions }].sort((a, b) => a.order - b.order)
  }

  /** Start listening for clicks and keeping the menu in step. Once. */
  start() {
    if (this.#started || !this.supported) return
    this.#started = true

    // The catch is the listener's, not the handler's: a store method that
    // fails has already put the reason over the window through `handle`, and
    // what is left here is only the unhandled rejection.
    onTrayAction((id) => void this.#run(id).catch(() => {}))

    // A reactive scope with no component to own it: the tray outlives every
    // view, and its contents depend on state spread across all three stores.
    // The alternative -- each store calling a `sync()` after every mutation
    // that might matter -- is a list that is quietly wrong the first time
    // somebody forgets one, and a stale menu is worse than no menu because
    // it is indistinguishable from a working one until it is clicked.
    $effect.root(() => {
      $effect(() => {
        const entries = this.enabled ? this.#compose() : []
        this.#push(this.enabled, entries)
      })
    })
  }

  setEnabled(on: boolean) {
    this.enabled = on
    localStorage.setItem(SHOW_KEY, on ? 'on' : 'off')
  }

  /**
   * Every group's actions, in order, with a rule between the groups that
   * offered any.
   *
   * The separators are put in here rather than by the apps because only this
   * knows which groups came back empty -- and a menu that opens with a rule,
   * or shows two in a row where the todo app had nothing to say, is the
   * usual way a composed menu goes wrong.
   */
  #compose(): TrayEntry[] {
    const out: TrayEntry[] = []
    for (const group of this.#groups) {
      const entries = group.actions()
      if (entries.length === 0) continue
      if (out.length > 0) out.push({ separator: true })
      out.push(...entries)
    }
    return out
  }

  /** Send a menu, or take the icon down, at most once per real change. */
  #push(enabled: boolean, entries: TrayEntry[]) {
    this.#handlers = new Map()
    const items = entries.map((e) => this.#toItem(e))
    const key = enabled ? JSON.stringify(items) : null
    if (key === this.#sent) return
    // Claimed before the send rather than after, so a burst of changes
    // queues one call each rather than one per change per change in flight.
    this.#sent = key

    this.#chain = this.#chain
      .then(async () => {
        if (!enabled) {
          await api.hideTray()
          this.showing = false
          return
        }
        // The shell answers false only for "this desktop has nowhere to put
        // an icon", which is a lasting fact worth repeating in Settings --
        // and it can stop being true mid-session, when somebody turns on
        // the extension that provides the tray, so it is tracked rather
        // than latched.
        const up = await api.setTrayMenu(items)
        this.showing = up
        this.unavailable = !up
      })
      .catch(() => {
        // A tray is a convenience: a failure here must not put an error over
        // the window somebody is writing in. But it is *not* evidence that
        // the desktop has no tray, so `unavailable` is left alone -- one
        // transient failure must not tell the user their machine cannot do
        // this. Forgetting the description is what makes it retry: the next
        // change to the same state no longer looks like a menu already sent.
        this.#sent = null
        this.showing = false
      })
  }

  /** Strip the handler off an entry, keeping it here under the item's id. */
  #toItem(entry: TrayEntry): TrayMenuItem {
    if (isSeparator(entry)) return { kind: 'separator' }
    if (isSubmenu(entry)) {
      return {
        kind: 'submenu',
        label: entry.label,
        enabled: entry.enabled ?? true,
        items: entry.items.map((e) => this.#toItem(e)),
      }
    }
    this.#handlers.set(entry.id, entry)
    return {
      kind: 'action',
      id: entry.id,
      label: entry.label,
      enabled: entry.enabled ?? true,
      checked: entry.checked ?? null,
      raise: entry.raise ?? true,
    }
  }

  /**
   * Run what was chosen.
   *
   * An id with no handler is not a bug worth shouting about: the menu can be
   * a fraction of a second behind the state it was built from -- a vault
   * that locked while the menu was open is the ordinary case -- and the
   * honest response to "add a task" arriving too late is to do nothing.
   */
  async #run(id: string) {
    const action = this.#handlers.get(id)
    if (!action) return
    await action.run()
  }
}

export const tray = new TrayRegistry()
