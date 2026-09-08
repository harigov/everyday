// The one context menu the window has.
//
// A singleton rather than a menu component per list, for the reason every
// menu on every platform is a singleton: there is exactly one open at a
// time, opening a second has to close the first, and a list that owned its
// own would have to be told about that by every other list.
//
// So a row's whole obligation is one attribute:
//
//   oncontextmenu={(e) => menu.show(e, itemsForThisRow())}
//
// and `App.svelte` mounts `ContextMenu` once, beside the toasts.
//
// What is deliberately *not* wired up: text. An editor, a search field or a
// notes box keeps the webview's own menu, because that one carries cut,
// paste, the spelling suggestions and the input-method entries, none of
// which this application could offer and all of which people expect from a
// right-click on a word.

import type { MenuItem } from './menu'

class MenuState {
  items = $state<MenuItem[]>([])
  /** Where it was raised, in client coordinates. `null` when closed. */
  at = $state<Point | null>(null)

  /** Who to give focus back to, so Escape does not strand the keyboard. */
  #opener: HTMLElement | null = null

  /**
   * Open a menu for `event`'s row.
   *
   * Takes the event because it has to: preventing the webview's own menu and
   * stopping the row's ancestors from opening theirs on top are both part of
   * the same decision, and a caller that had to remember them separately
   * would eventually forget one.
   */
  show(event: MouseEvent, items: MenuItem[]) {
    if (items.length === 0) return
    // Text is not ours. A field or an editor keeps the webview's own menu,
    // whatever it happens to sit inside -- the notes box in the calendar's
    // detail panel is a textarea in the middle of a panel that has a menu of
    // its own, and taking that right-click would cost the person typing in
    // it paste, spelling and their input method.
    if (isEditable(event.target)) return
    event.preventDefault()
    // Innermost wins. A task row sits inside a list that has a menu of its
    // own, and without this both would fire and the outer would win.
    event.stopPropagation()
    this.items = items
    this.at = pointOf(event)
    this.#opener = document.activeElement instanceof HTMLElement ? document.activeElement : null
  }

  /** Close, returning focus where it was if the keyboard opened this. */
  close(restoreFocus = true) {
    if (!this.at) return
    this.at = null
    this.items = []
    const opener = this.#opener
    this.#opener = null
    if (restoreFocus && opener?.isConnected) opener.focus()
  }
}

interface Point {
  x: number
  y: number
}

/** Is this something being typed into? */
function isEditable(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false
  return target.isContentEditable || target.tagName === 'INPUT' || target.tagName === 'TEXTAREA'
}

/**
 * Where the menu was asked for.
 *
 * Not always a pointer: the context-menu key and Shift+F10 raise the same
 * event with no coordinates behind it, and a menu drawn in the corner of the
 * window in answer to a key pressed on a row half way down the sidebar reads
 * as a bug. Those get the row itself as their anchor.
 */
function pointOf(event: MouseEvent): Point {
  const target = event.currentTarget ?? event.target
  if (event.clientX === 0 && event.clientY === 0 && target instanceof Element) {
    const box = target.getBoundingClientRect()
    return { x: box.left + 12, y: box.bottom - 4 }
  }
  return { x: event.clientX, y: event.clientY }
}

export const menu = new MenuState()
