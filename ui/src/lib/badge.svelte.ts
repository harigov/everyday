// The number on the application's icon -- the Dock's badge on macOS, the
// launcher's count on Linux, an overlay on the taskbar button on Windows.
//
// This side only remembers what to count and tells the shell; the shell does
// the counting, and keeps it current. That split is the tray's meeting line's
// over again (see `tray.svelte.ts`), for the same reason: the window can be
// hidden with the process still running, and a count kept by a webview that
// is not looking would go stale exactly when somebody glances at the Dock to
// see whether anything arrived. See `crates/everyday-app/src/badge.rs`.

import { api, isMock } from './api'
import { pref } from './prefs'
import type { BadgeSources } from './types'

const NOTHING: BadgeSources = { tasks: 'off', mail: false, assistant: false }

const TASKS: readonly BadgeSources['tasks'][] = ['off', 'due', 'open']

/**
 * Remembered on this machine, like the tray's switches: it is about this
 * desktop's Dock, not about the vault, and the shell has to be told before
 * anything is unlocked so that it clears rather than keeps a stale number.
 * Off by default. Whatever is stored is read field by field, so a value
 * written by a version that offered different choices falls back to "off"
 * for what it no longer recognises rather than failing to parse at all.
 */
const sourcesPref = pref<BadgeSources>(
  'everyday.badge',
  (raw) => {
    const stored = raw ? (JSON.parse(raw) as Partial<BadgeSources>) : {}
    return {
      tasks: TASKS.includes(stored.tasks as BadgeSources['tasks'])
        ? (stored.tasks as BadgeSources['tasks'])
        : 'off',
      mail: stored.mail === true,
      assistant: stored.assistant === true,
    }
  },
  NOTHING,
  (v) => JSON.stringify(v),
)

class Badge {
  /** What the number on the icon adds up. Everything off means no badge. */
  sources = $state<BadgeSources>(sourcesPref.get())

  #started = false

  /** Is there a shell to put a badge on an icon? False in the browser mock. */
  get supported(): boolean {
    return !isMock
  }

  /** Tell the shell what to count, now and whenever the choice changes. Once. */
  start() {
    if (this.#started || !this.supported) return
    this.#started = true
    $effect.root(() => {
      $effect(() => {
        // A badge is a convenience: failing to set one must not put an
        // error over the window somebody is working in.
        void api.setBadge($state.snapshot(this.sources)).catch(() => {})
      })
    })
  }

  /** Change part of the choice, and remember the whole of it. */
  set(change: Partial<BadgeSources>) {
    this.sources = { ...this.sources, ...change }
    sourcesPref.set(this.sources)
  }
}

export const badge = new Badge()
