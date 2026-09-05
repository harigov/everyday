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
 */
export function focusOnMount(node: HTMLElement) {
  const id = requestAnimationFrame(() => node.focus())
  return { destroy: () => cancelAnimationFrame(id) }
}
