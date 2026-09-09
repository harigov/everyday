// One tab stop per list, and the arrow keys to move within it.
//
// Every list in this application draws its rows as `<button>`s, which is
// right -- they are activated, they take Enter and Space, and a screen
// reader announces them as what they are. What it also means, left alone, is
// that a journal with four hundred entries in it puts four hundred stops
// between the search field and the editor. Tabbing out of the list is then
// something nobody does twice, and reaching the editor from the keyboard is
// not really possible at all. A task row is worse: it carries a tick, a
// title and a disclosure, so two thousand tasks are six thousand stops.
//
// The fix is the pattern every native list uses and the one the ARIA
// practices call a roving tabindex: exactly one control in the whole list is
// tabbable, Tab enters and leaves in one step, and the arrow keys move once
// you are inside. The stop follows the selection, so tabbing back into a
// list returns you to where you were rather than to the top of it.
//
// # The markup contract
//
//   data-row={id}    marks a row. The id is what `current` is matched
//                    against, and document order is the order the arrow keys
//                    move in.
//   data-rowfocus    optional, marks which control in the row holds the stop.
//                    Without it the row element itself holds it when it is
//                    focusable, and otherwise its first focusable descendant
//                    does.
//
// A row owns the controls whose nearest marked ancestor it is. In today's
// markup that is the same as "its descendants" -- a task's subtasks are
// drawn as sibling rows beside it rather than inside it -- but the rule is
// written the careful way so that a row which one day does contain another
// cannot quietly annex its controls.
//
// Every other focusable control belonging to the row is taken out of the tab
// order and reached with Left and Right instead. Which is why Left and Right
// have two meanings, and can: in a row of several controls they step between
// them, and in a grid of one-control cards they step along the line. No row
// is ever both.
//
// Deliberately *not* here: changing the selection as focus moves. Opening an
// entry is a read from the vault, and a held arrow key would be one per row.
// Focus moves, the row shows it, and Enter or Space opens -- which is what
// the buttons already do.

const ROW = '[data-row]'

/**
 * What Tab would reach, in the order it would reach it.
 *
 * `tabindex="-1"` is excluded on purpose even though this is the very
 * attribute being handed out below: what is wanted is the set of controls
 * that *would* be stops if this action were not here, which is why the query
 * asks about kind rather than about the current tabindex.
 */
const FOCUSABLE =
  'a[href], button, input, select, textarea, summary, [contenteditable]:not([contenteditable="false"])'

function visible(el: HTMLElement): boolean {
  return el.offsetParent !== null || el.getClientRects().length > 0
}

function rowsIn(node: HTMLElement): HTMLElement[] {
  // Rendered rows only. A list mid-transition, or one whose rows are hidden
  // by a filter, should not be a stop that focus lands on and vanishes from.
  return [...node.querySelectorAll<HTMLElement>(ROW)].filter(visible)
}

/**
 * The focusable controls that belong to `row` itself.
 *
 * `closest` rather than a plain descendant query, because rows nest: a
 * parent task must not claim its subtasks' ticks, or Left from the parent
 * would walk down into a row of its own.
 */
function controlsOf(row: HTMLElement): HTMLElement[] {
  const own = [...row.querySelectorAll<HTMLElement>(FOCUSABLE)].filter(
    (el) => el.closest(ROW) === row && !el.hasAttribute('disabled') && visible(el),
  )
  return row.matches(FOCUSABLE) ? [row, ...own] : own
}

/**
 * Every control in the list that belongs to some row.
 *
 * One query and a short walk per hit, rather than a query per row: the
 * sweep runs whenever the list changes, and a query per row over two
 * thousand tasks is two thousand tree walks for what one can answer.
 *
 * The filter is what keeps it from reaching past the rows. A task's subtask
 * capture line is a sibling of the row it hangs under rather than a part of
 * it, and taking that field out of the tab order would leave somebody typing
 * into it with no way to have got there.
 */
function rowControlsIn(node: HTMLElement): HTMLElement[] {
  return [...node.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((el) => el.closest(ROW))
}

/** The control in `row` that holds the tab stop. */
function stopOf(row: HTMLElement): HTMLElement | null {
  const named = [...row.querySelectorAll<HTMLElement>('[data-rowfocus]')].find(
    (el) => el.closest(ROW) === row,
  )
  return named ?? controlsOf(row)[0] ?? null
}

/**
 * Give `node`'s rows a single tab stop and arrow-key movement.
 *
 * `current` is the id of the selected row, so the stop follows the
 * selection. Pass `null` where there is none and the first row holds it.
 */
export function rovingFocus(node: HTMLElement, current: string | null = null) {
  /** Hand the tab stop to `chosen`'s stop control and take it off every other. */
  function assign(chosen: HTMLElement) {
    const stop = stopOf(chosen)
    for (const control of rowControlsIn(node)) control.tabIndex = control === stop ? 0 : -1
  }

  function place(current: string | null) {
    const rows = rowsIn(node)
    if (rows.length === 0) return
    // Not while the list has focus: the stop is wherever the reader has
    // arrowed to, and moving it under them would send the next Tab
    // somewhere they did not leave from.
    if (node.contains(document.activeElement)) return
    assign(rows.find((el) => el.dataset.row === current) ?? rows[0]!)
  }

  function focusRow(rows: HTMLElement[], index: number) {
    const target = rows[Math.max(0, Math.min(index, rows.length - 1))]
    if (!target) return
    assign(target)
    stopOf(target)?.focus()
  }

  /** Move to another control of the row focus is already in. */
  function focusControl(row: HTMLElement, delta: number) {
    const controls = controlsOf(row)
    const at = controls.findIndex((el) => el === document.activeElement)
    const target = controls[Math.max(0, Math.min(at + delta, controls.length - 1))]
    if (!target || target === document.activeElement) return
    // The row keeps its single stop wherever within it focus has got to, so
    // Tab leaves from where the reader is rather than from the title.
    for (const el of controls) el.tabIndex = el === target ? 0 : -1
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
   *
   * Measured against the viewport rather than `offsetTop`, which is relative
   * to whichever ancestor happens to be positioned -- and in the library's
   * grid the marked element is a button inside a positioned card, so every
   * one of them reports the same nought. Rows are in document order, so the
   * first line is a prefix and this stops at its end rather than measuring
   * two thousand rectangles to count to one.
   */
  function columnsOf(rows: HTMLElement[]): number {
    const top = rows[0]!.getBoundingClientRect().top
    let n = 1
    while (n < rows.length && Math.abs(rows[n]!.getBoundingClientRect().top - top) < 1) n++
    return n
  }

  function onKeydown(e: KeyboardEvent) {
    // A modifier means the key belongs to something else -- Home with Shift
    // is a selection, and the window's own shortcuts are all modified.
    if (e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return
    const active = document.activeElement
    // A row can contain a field: the line for adding a subtask opens inside
    // the row it belongs to. Every key here is one somebody typing needs.
    if (isEditable(active)) return
    const rows = rowsIn(node)
    if (rows.length === 0) return
    // The innermost row, because they nest. `findIndex` on `contains` would
    // answer with a subtask's parent, and every arrow key would move from
    // the wrong place.
    const row = active instanceof HTMLElement ? active.closest<HTMLElement>(ROW) : null
    const at = row ? rows.indexOf(row) : -1
    // The keys only mean this once focus is on a row. A field above the list
    // -- the search box -- keeps its own use of Home and End.
    if (at < 0 || !row) return

    const columns = columnsOf(rows)
    // A row of several controls spends Left and Right on them; a grid of
    // single-control cards spends them on the line. See the header.
    const withinRow = controlsOf(row).length > 1
    // Roughly a screen, worked out from the list rather than assumed, so it
    // is right for a dense row and for a card.
    const lineHeight = row.getBoundingClientRect().height || 1
    const page = Math.max(1, Math.round(node.clientHeight / lineHeight) - 1) * columns

    switch (e.key) {
      case 'ArrowDown':
        focusRow(rows, at + columns)
        break
      case 'ArrowUp':
        focusRow(rows, at - columns)
        break
      case 'ArrowRight':
        if (withinRow) focusControl(row, 1)
        else if (columns > 1) focusRow(rows, at + 1)
        else return
        break
      case 'ArrowLeft':
        if (withinRow) focusControl(row, -1)
        else if (columns > 1) focusRow(rows, at - 1)
        else return
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

  // Re-placed when the rows change, not only when `current` does.
  //
  // The action runs before a keyed `{#each}` has inserted anything, and a
  // list whose store is still loading has no rows at all -- so the first
  // placement usually happens against an empty list. Re-running on the
  // parameter alone was not enough: the journal got away with it because
  // opening the newest entry moves `selectedEntry` off null as the load
  // lands, and the todo list, whose selection stays null until somebody
  // clicks a task, was left with every one of its controls a tab stop.
  //
  // `childList` only. Handing out `tabindex` is an attribute change, and
  // observing those as well would be a loop.
  let queued: number | null = null
  let latest = current
  const restack = () => {
    if (queued !== null) return
    queued = requestAnimationFrame(() => {
      queued = null
      place(latest)
    })
  }
  const observer = new MutationObserver(restack)
  observer.observe(node, { childList: true, subtree: true })
  restack()

  return {
    update(next: string | null = null) {
      latest = next
      place(next)
    },
    destroy() {
      if (queued !== null) cancelAnimationFrame(queued)
      observer.disconnect()
      node.removeEventListener('keydown', onKeydown)
    },
  }
}

/** Is this something being typed into? */
function isEditable(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false
  return target.isContentEditable || target.tagName === 'INPUT' || target.tagName === 'TEXTAREA'
}
