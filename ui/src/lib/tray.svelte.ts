// Quick actions in the system tray.
//
// The shell owns the icon and the menu widget; this owns what is in it. What
// is *in* it is no longer a registry of its own: it is a filtered view over
// the one action table in `shortcuts.svelte.ts`, taking the rows that wear
// `tray: true`.
//
// That is the whole change, and it is worth stating why. There were three
// registries -- the keyboard's, the tray's and the context menus' -- and the
// third one to learn about a new action was always the one nobody remembered.
// A row now reaches the menu bar by wearing a flag rather than by being
// declared a second time, and the palette offers it without being told.
//
// What has not changed is the good part of the old design. A `when` is a
// *function*, re-read whenever anything it reads changes, so an action that is
// only sometimes possible disappears rather than failing when it is chosen: a
// menu built once at startup would still offer "Add a task" on a vault whose
// backend has no tasks, and would still offer everything after the vault
// locked.
//
// Handlers stay on this side. What crosses to Rust is a description --
// labels, ids, enabled flags -- and what comes back is the id that was
// chosen. See `crates/everyday-app/src/tray.rs`.

import { api, isMock, onTrayAction } from './api'
import { ACTIONS, GROUPS, type Group } from './shortcuts.svelte'
import type { Binding } from './keys'
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

const SHOW_KEY = 'everyday.tray'

/**
 * The tray rows an app offers right now, as tray entries.
 *
 * Read from the one table, filtered by the group's own name. `id` is required
 * for a tray row -- it is what crosses to Rust and comes back -- so a row
 * wearing `tray` without one is a mistake worth skipping rather than sending.
 */
function trayEntries(group: Group): TrayAction[] {
  return ACTIONS.filter(
    (a): a is Binding & { group: Group; id: string } =>
      a.tray === true && a.group === group && a.id !== undefined && (!a.when || a.when()),
  ).map((a) => ({
    id: a.id,
    label: a.label,
    checked: a.checked?.(),
    raise: a.raise,
    // An action's `run` may answer with anything -- most of them return a
    // store's own promise -- and the tray only ever awaits it.
    run: async () => {
      await a.run()
    },
  }))
}

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
   * One app's quick actions, as that app would offer them right now.
   *
   * `group` is the heading in the action table -- `Journal`, `Todo` -- not a
   * section id, and it is typed as one so that a caller cannot ask for a
   * heading the table has never heard of and get an empty menu back. For a
   * menu that is not the tray's: the app bar raises this on a right-click,
   * and it has to be the same list rather than a second one written beside
   * it. Two answers to "what can this app start right now" drift, and the one
   * nobody is looking at is always the stale one.
   */
  entriesFor(group: Group): TrayEntry[] {
    return trayEntries(group)
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
    // view, and its contents depend on state spread across every app's store.
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
   * The order is `GROUPS`, the one the help sheet reads too, rather than a
   * list of its own kept beside it: two orderings of the same headings is how
   * an app ends up fifth in one menu and last in the other. Groups with
   * nothing in the tray -- `Everywhere`, `Go to` -- simply come back empty.
   *
   * The separators are put in here rather than by the apps because only this
   * knows which groups came back empty -- and a menu that opens with a rule,
   * or shows two in a row where the todo app had nothing to say, is the
   * usual way a composed menu goes wrong.
   */
  #compose(): TrayEntry[] {
    const out: TrayEntry[] = []
    for (const group of GROUPS) {
      const entries = trayEntries(group)
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
