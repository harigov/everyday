// Whether each app's sidebar is showing, and the moment it is borrowed.
//
// Folded per app rather than once for the window, because the sidebars are
// not one thing. The mail app's is a list of folders somebody picks from a
// few times a day, and folding it away gives the thread list the room; the
// notes app's *is* the list of notes. Somebody who folds the first has not
// said anything about the second.
//
// A folded sidebar is still there, one gesture away: resting the pointer on
// the window's left edge, or on the button that folds it, draws it over the
// app for a quick choice -- a peek -- and it goes again once the pointer
// leaves. Nothing is reflowed by a peek, so the list somebody was reading
// does not jump under them. Two delays make that bearable: a short one
// before it appears, so a pointer crossing the edge on its way to the app
// bar does not flash it open, and a longer one before it goes, so a pointer
// that overshoots the panel's edge by a few pixels does not lose it.
//
// The folded sidebar stays *mounted*, only hidden and inert. That is not an
// optimisation: the navs do work when they mount -- the mail one starts the
// mailbox list and polls sync status -- and a sidebar that unmounted when
// folded would quietly stop an app from loading at all.

import type { Section } from './apps'
import { APP_ORDER } from './apps'
import { pref } from './prefs'

/** How long the pointer rests before a folded sidebar is drawn. */
const OPEN_DELAY_MS = 140
/** How long it stays after the pointer leaves. */
const CLOSE_DELAY_MS = 320

const foldedPref = pref<Section[]>(
  'everyday.sidebar.folded',
  (raw) => {
    const list: unknown = JSON.parse(raw ?? '[]')
    // Anything not an app today is dropped rather than trusted: a section
    // that was renamed or removed must not make the list unreadable.
    return Array.isArray(list)
      ? list.filter((s): s is Section => (APP_ORDER as readonly unknown[]).includes(s))
      : []
  },
  [],
  (list) => JSON.stringify(list),
)

/**
 * What is keeping a peek open. Separate, because they come and go
 * separately: a pointer that drifts off the panel while somebody is typing
 * a project's name into it has not finished with it, and nor has one that
 * moved onto the context menu a row in it raised.
 */
type Hold = 'pointer' | 'focus' | 'menu'

class SidebarState {
  #folded = $state<Section[]>(foldedPref.get())
  /** Drawn over a folded sidebar's app for the moment, without unfolding. */
  peeking = $state(false)
  #holds = new Set<Hold>()
  #timer: ReturnType<typeof setTimeout> | null = null

  /** Is this app's sidebar folded away? */
  folded(section: Section): boolean {
    return this.#folded.includes(section)
  }

  /** Fold or unfold one app's sidebar, and remember it. */
  toggle(section: Section) {
    this.#folded = this.folded(section)
      ? this.#folded.filter((s) => s !== section)
      : [...this.#folded, section]
    foldedPref.set(this.#folded)
    this.hide()
  }

  /** The pointer arrived somewhere that should show it -- after a moment,
   *  so one only passing by does not flash it open. */
  reach() {
    this.hold('pointer')
  }

  /** ...and left every one of them. */
  release() {
    this.letGo('pointer')
  }

  /**
   * Something wants it shown. The pointer waits `OPEN_DELAY_MS`; focus
   * arriving and a menu raised from it do not, since neither happens by
   * accident on the way to somewhere else.
   */
  hold(why: Hold) {
    this.#holds.add(why)
    this.#clear()
    if (this.peeking) return
    if (why === 'pointer') this.#timer = setTimeout(() => this.#settle(true), OPEN_DELAY_MS)
    else this.#settle(true)
  }

  /** One reason has gone. The peek goes too, after `CLOSE_DELAY_MS`, once
   *  every reason has. */
  letGo(why: Hold) {
    if (!this.#holds.delete(why)) return
    if (this.#holds.size > 0) return
    this.#clear()
    if (this.peeking) this.#timer = setTimeout(() => this.#settle(false), CLOSE_DELAY_MS)
    // Still waiting to open, and nothing wants it any more: it never opens.
  }

  /** Show it now, as though focus had arrived -- the keyboard's way in. */
  peek() {
    this.hold('focus')
  }

  /** Put it away now, whatever is holding it: Escape, a lock, another app. */
  hide() {
    this.#holds.clear()
    this.#settle(false)
  }

  #settle(peeking: boolean) {
    this.#clear()
    this.peeking = peeking
  }

  #clear() {
    if (this.#timer) clearTimeout(this.#timer)
    this.#timer = null
  }
}

export const sidebar = new SidebarState()
