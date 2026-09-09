// Close a thing when the pointer goes somewhere else.
//
// # Why this exists rather than a scrim
//
// The obvious way to dismiss a menu or a popover is an invisible element
// stretched over the window with a click handler on it, and that is what
// every one of them here used to do. It works, and it costs one click:
//
//   1. The pointer goes down on the scrim. The scrim is the target.
//   2. The scrim's handler closes the panel, so the scrim leaves the DOM.
//   3. The pointer comes up over the button that was underneath it.
//   4. The browser dispatches `click` on the nearest common ancestor of the
//      down target and the up target -- and since one of them no longer has
//      a parent, that is `<body>`.
//
// So the button was pressed and nothing happened. Whatever was clicked has
// to be clicked twice, and only sometimes: exactly when a menu or a popover
// happened to be open, which from the outside is "sometimes the app bar does
// not respond". A right-click somewhere, a glance at the app bar, a click on
// Library, and the window stays on the calendar.
//
// Listening on the window instead of standing in front of it removes the
// interception entirely. The panel closes on `pointerdown`, the pointer
// comes up on the button it was aimed at, and the click lands. One gesture,
// one dismissal, and the thing you were pointing at gets the press --
// which, for a menu that is *about* the row it was raised on, is the reading
// that matches what people expect from a toolbar.
//
// # Why capture, and why pointerdown
//
// Capture, so a panel that stops propagation on its own contents cannot stop
// this from noticing a press outside it. `pointerdown` rather than `click`
// because a dismissal should happen when the gesture starts -- waiting for
// the release leaves an open menu under a pointer that has already committed
// to something else.

/** Options for `dismissable`. */
export interface DismissOptions {
  /** Called when the pointer goes down anywhere outside the node. */
  onaway: () => void
  /**
   * Anything else that counts as inside.
   *
   * A menu and the button that opens it are one thing to a reader, so a press
   * on the button must not both close the menu and reopen it. A selector is
   * enough for every case here and avoids threading element references
   * through three components.
   */
  within?: string
  /** Set false to stop listening without removing the action. */
  enabled?: boolean
}

/**
 * Close `node` when the pointer goes down outside it.
 *
 * ```svelte
 * <div class="panel" use:dismissable={{ onaway: () => (open = false) }}>
 * ```
 *
 * The listener is added on the next frame rather than immediately, because
 * the press that *opened* the panel is often still being dispatched when the
 * panel mounts -- and a popover that closes on the click that opened it
 * looks exactly like a button that does nothing.
 */
export function dismissable(node: HTMLElement, options: DismissOptions) {
  let current = options

  function onDown(event: PointerEvent) {
    if (current.enabled === false) return
    const target = event.target
    if (!(target instanceof Node)) return
    if (node.contains(target)) return
    if (current.within && target instanceof Element && target.closest(current.within)) return
    current.onaway()
  }

  let listening = false
  const frame = requestAnimationFrame(() => {
    window.addEventListener('pointerdown', onDown, true)
    listening = true
  })

  return {
    update(next: DismissOptions) {
      current = next
    },
    destroy() {
      cancelAnimationFrame(frame)
      if (listening) window.removeEventListener('pointerdown', onDown, true)
    },
  }
}
