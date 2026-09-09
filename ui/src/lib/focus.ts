/**
 * Move focus to an element the moment it appears.
 *
 * `autofocus` is not a substitute here. The HTML attribute is only honoured
 * while the document is loading; on an element inserted later -- an inline
 * "new journal" field that appears when a button is clicked -- browsers
 * ignore it, and focus stays on the button that was just pressed. The field
 * then looks broken: it is on screen, but everything typed goes nowhere.
 *
 * The focus call is deferred by a frame because the click that created the
 * element has not finished dispatching yet, and the default focus behaviour
 * of that click would otherwise land after ours and undo it.
 *
 * Pass `false` to opt out. That is for a field that is always on screen --
 * the todo app's quick-add bar -- which wants this behaviour only when it is
 * the reason the view was opened, and stealing focus otherwise would fight
 * with whatever the user was actually doing.
 */
export function focusOnMount(node: HTMLElement, enabled: boolean = true) {
  if (!enabled) return
  const id = requestAnimationFrame(() => node.focus())
  return { destroy: () => cancelAnimationFrame(id) }
}

/**
 * What Tab can reach, in the order it reaches it.
 *
 * `tabindex="-1"` is deliberately excluded: it means "focusable, but not by
 * Tab", which is exactly what a dialog's own container is marked as.
 */
const TABBABLE =
  'a[href], area[href], button, input, select, textarea, summary, iframe, ' +
  'audio[controls], video[controls], [contenteditable]:not([contenteditable="false"]), ' +
  '[tabindex]:not([tabindex^="-"])'

function tabbable(root: HTMLElement): HTMLElement[] {
  return [...root.querySelectorAll<HTMLElement>(TABBABLE)].filter(
    (el) =>
      !el.hasAttribute('disabled') &&
      el.getAttribute('aria-hidden') !== 'true' &&
      // Off-screen and display:none elements have no boxes. Cheaper and more
      // honest than reading computed styles for every candidate.
      el.getClientRects().length > 0,
  )
}

/**
 * Keep Tab inside a dialog, and give focus back when it closes.
 *
 * Every modal in the application declares `aria-modal="true"`, which is a
 * promise to assistive technology that nothing behind it is reachable. That
 * promise was not being kept: Tab walked straight out of the sheet and into
 * the entry list underneath, so a keyboard user could be typing into a
 * journal they could not see, behind a dialog they could not leave.
 *
 * Restoring focus on close matters as much as trapping it. Without it,
 * dismissing a dialog drops focus on `<body>` and the next Tab starts again
 * from the top of the window -- which loses the reader's place every time
 * they confirm anything.
 */
export function trapFocus(node: HTMLElement) {
  const returnTo = document.activeElement as HTMLElement | null

  // The dialog itself takes focus when nothing inside it asked for it, so
  // that Escape and the trap below have somewhere to fire from. Deferred by
  // a frame, for `focusOnMount`'s reason and so that an element inside that
  // does want focus wins.
  const frame = requestAnimationFrame(() => {
    if (!node.contains(document.activeElement)) {
      const first = tabbable(node)[0]
      if (first) first.focus()
      else {
        node.tabIndex = -1
        node.focus()
      }
    }
  })

  function onKeydown(e: KeyboardEvent) {
    if (e.key !== 'Tab') return
    const stops = tabbable(node)
    if (stops.length === 0) {
      e.preventDefault()
      return
    }
    const first = stops[0]!
    const last = stops[stops.length - 1]!
    const at = document.activeElement
    // Wrapping is handled here rather than left to the browser, because the
    // browser's next stop is outside the dialog and that is the whole
    // problem. `!node.contains(at)` covers focus having escaped already --
    // a click on the scrim, say -- by pulling it back in.
    if (e.shiftKey && (at === first || !node.contains(at))) {
      e.preventDefault()
      last.focus()
    } else if (!e.shiftKey && (at === last || !node.contains(at))) {
      e.preventDefault()
      first.focus()
    }
  }

  node.addEventListener('keydown', onKeydown)
  // On the node rather than the document: a dialog that opens another one
  // -- the journal settings sheet raising a confirmation -- must not have
  // its parent's trap fighting the child's for the same Tab.
  return {
    destroy() {
      cancelAnimationFrame(frame)
      node.removeEventListener('keydown', onKeydown)
      // Only if the dialog still had focus. Something that closed itself by
      // moving focus somewhere deliberate should keep it there.
      if (node.contains(document.activeElement) && returnTo?.isConnected) returnTo.focus()
    },
  }
}
