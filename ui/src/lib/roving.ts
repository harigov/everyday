// One tab stop per list, and the arrow keys to move within it.
//
// Every list in this application draws its rows as `<button>`s, which is
// right -- they are activated, they take Enter and Space, and a screen
// reader announces them as what they are. What it also means, left alone, is
// that a journal with four hundred entries in it puts four hundred stops
// between the search field and the editor. Tabbing out of the list is then
// something nobody does twice, and reaching the editor from the keyboard is
// not really possible at all.
//
// The fix is the pattern every native list uses and the one the ARIA
// practices call a roving tabindex: exactly one row is tabbable, Tab enters
// and leaves the list in one step, and Up/Down (Home/End, Page Up/Down) move
// between rows once you are in it. The row that holds the stop is whichever
// one is selected, so tabbing back into a list returns you to where you were
// rather than to the top of it.
//
// Deliberately *not* here: changing the selection as focus moves. Opening an
// entry is a read from the vault, and a held arrow key would be one per row.
// Focus moves, the row shows it, and Enter or Space opens -- which is what
// the buttons already do.

/** Marks a row: `data-row={id}`, where the id is what `current` is matched against. */
const ROW = '[data-row]'

function rowsIn(node: HTMLElement): HTMLElement[] {
  // Rendered rows only. A list mid-transition, or one whose rows are hidden
  // by a filter, should not be a stop that focus lands on and vanishes from.
  return [...node.querySelectorAll<HTMLElement>(ROW)].filter((el) => el.getClientRects().length > 0)
}

/**
 * Give `node`'s rows a single tab stop and arrow-key movement.
 *
 * `current` is the id of the selected row, so the stop follows the
 * selection. Pass `null` where there is none and the first row holds it.
 */
export function rovingFocus(node: HTMLElement, current: string | null = null) {
  function place(current: string | null) {
    const rows = rowsIn(node)
    if (rows.length === 0) return
    // Not while the list has focus: the stop is wherever the reader has
    // arrowed to, and moving it under them would send the next Tab
    // somewhere they did not leave from.
    if (node.contains(document.activeElement)) return
    const chosen = rows.find((el) => el.dataset.row === current) ?? rows[0]!
    for (const el of rows) el.tabIndex = el === chosen ? 0 : -1
  }

  function focusRow(rows: HTMLElement[], index: number) {
    const target = rows[Math.max(0, Math.min(index, rows.length - 1))]
    if (!target) return
    for (const el of rows) el.tabIndex = el === target ? 0 : -1
    target.focus()
  }

  /**
   * How many rows sit on one visual line.
   *
   * One, for a list. However many the grid happens to be showing, for the
   * library's cards -- measured rather than configured, because it is a
   * responsive grid and the answer changes with the width of the window.
   * Down in a grid of five columns has to mean "the card below this one",
   * not "the next card".
   */
  function columnsOf(rows: HTMLElement[]): number {
    // Measured against the viewport rather than `offsetTop`, which is
    // relative to whichever ancestor happens to be positioned -- and in the
    // library's grid the marked element is a button inside a positioned
    // card, so every one of them reports the same nought.
    const top = rows[0]!.getBoundingClientRect().top
    const line = rows.filter((el) => Math.abs(el.getBoundingClientRect().top - top) < 1).length
    return Math.max(1, line)
  }

  function onKeydown(e: KeyboardEvent) {
    // A modifier means the key belongs to something else -- Home with Shift
    // is a selection, and the window's own shortcuts are all modified.
    if (e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return
    const rows = rowsIn(node)
    if (rows.length === 0) return
    const at = rows.findIndex((el) => el.contains(document.activeElement))
    // The keys only mean this once focus is on a row. A field inside the
    // list -- the search box above it -- keeps its own use of Home and End.
    if (at < 0) return

    const columns = columnsOf(rows)
    // Roughly a screen, worked out from the list rather than assumed, so it
    // is right for a dense row and for a card.
    const lineHeight = rows[at]!.getBoundingClientRect().height || 1
    const page = Math.max(1, Math.round(node.clientHeight / lineHeight) - 1) * columns

    switch (e.key) {
      case 'ArrowDown':
        focusRow(rows, at + columns)
        break
      case 'ArrowUp':
        focusRow(rows, at - columns)
        break
      // Left and right are a step along the line, and only where there is a
      // line to step along: in a single-column list the arrow keys belong to
      // whatever horizontal scrolling or text the row contains.
      case 'ArrowRight':
        if (columns === 1) return
        focusRow(rows, at + 1)
        break
      case 'ArrowLeft':
        if (columns === 1) return
        focusRow(rows, at - 1)
        break
      case 'Home':
        focusRow(rows, 0)
        break
      case 'End':
        focusRow(rows, rows.length - 1)
        break
      case 'PageDown':
        focusRow(rows, at + page)
        break
      case 'PageUp':
        focusRow(rows, at - page)
        break
      default:
        return
    }
    // Only once a key was one of ours: the browser would otherwise scroll
    // the list out from under the row it just moved to.
    e.preventDefault()
  }

  node.addEventListener('keydown', onKeydown)
  // After the rows exist. The action runs before the children of a keyed
  // `{#each}` have been inserted on first render.
  const frame = requestAnimationFrame(() => place(current))

  return {
    update(next: string | null = null) {
      place(next)
    },
    destroy() {
      cancelAnimationFrame(frame)
      node.removeEventListener('keydown', onKeydown)
    },
  }
}
