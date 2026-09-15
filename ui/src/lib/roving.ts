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
//
// # Virtualized lists
//
// `VirtualList` draws only the rows near the viewport, so a query for
// `[data-row]` only ever sees that window -- Home, End, PageUp and PageDown
// would stop at whichever edge happened to be rendered rather than the
// list's actual edge, and a row that scrolls into view for the first time
// would join the tab order at `tabindex="0"` (an element's default) until
// something got round to correcting it.
//
// Pass a `virtual` adapter (`RovingVirtual`) to fix both: the arrow-key
// arithmetic runs over the *data*'s length rather than what is drawn
// (`nextRovingIndex`, pure and tested on its own), and reaching a row that
// is not currently rendered asks the list to scroll it into view first and
// waits for it to mount before focusing it (`waitForRow`). The tab-index
// sweep below is unconditional either way -- see `sweep`'s own doc for why
// that is what stops a newly-drawn row from becoming a stop of its own.

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

/** A key `nextRovingIndex` and the handler below both move the selection
 *  for; every other key falls through to whatever else wants it. */
export type RovingKey =
  'ArrowDown' | 'ArrowUp' | 'ArrowLeft' | 'ArrowRight' | 'Home' | 'End' | 'PageUp' | 'PageDown'

/**
 * Pure arithmetic for where a roving-focus key moves the selected index,
 * clamped to `[0, length)` -- `null` when this key does not move anything
 * here (a key this action does not claim, or Left/Right on a single-column
 * list with nothing of its own to step between).
 *
 * Shared by the DOM sweep below, over whatever is currently rendered, and
 * the virtualized path, over the full data set -- see the module doc's
 * "Virtualized lists" -- so Home means the same row in a hundred-thousand-row
 * mailbox as it does in a four-row list, which is finding 6.
 */
export function nextRovingIndex(
  current: number,
  key: string,
  length: number,
  opts: { columns?: number; page?: number; withinRow?: boolean } = {},
): number | null {
  if (length === 0) return null
  const columns = opts.columns ?? 1
  const page = opts.page ?? columns
  const clamp = (i: number) => Math.max(0, Math.min(i, length - 1))
  switch (key as RovingKey) {
    case 'ArrowDown':
      return clamp(current + columns)
    case 'ArrowUp':
      return clamp(current - columns)
    case 'ArrowRight':
      return !opts.withinRow && columns > 1 ? clamp(current + 1) : null
    case 'ArrowLeft':
      return !opts.withinRow && columns > 1 ? clamp(current - 1) : null
    case 'Home':
      return 0
    case 'End':
      return length - 1
    case 'PageDown':
      return clamp(current + page)
    case 'PageUp':
      return clamp(current - page)
    default:
      return null
  }
}

/**
 * What a virtualized list hands `rovingFocus` so Home, End, PageUp,
 * PageDown and the arrows can reason about the whole data set rather than
 * only the window `VirtualList` currently has drawn. `EntryList.svelte`'s
 * own use is the reference: `length` and `idAt` over the flattened,
 * header-and-entry row array with the headers filtered out (a header is
 * never a stop), `scrollToIndex` a thin forward to the same component's
 * exported method.
 */
export interface RovingVirtual {
  /** How many navigable rows the data holds -- not just what is drawn. */
  length: number
  /** The row id at a data index, `null` past the end. */
  idAt(index: number): string | null
  /** The data index `id` is at, or -1. */
  indexOf(id: string): number
  /** Ask the list to scroll this index into view. Rendering follows
   *  asynchronously -- `waitForRow` is what then waits for it. */
  scrollToIndex(index: number): void
}

export type RovingParams = string | null | { current: string | null; virtual?: RovingVirtual }

function normalizeParams(params: RovingParams): {
  current: string | null
  virtual?: RovingVirtual
} {
  return typeof params === 'object' && params !== null ? params : { current: params }
}

/** Poll for a row to appear -- up to `tries` animation frames -- once
 *  `virtual.scrollToIndex` has asked the list to draw it. A frame each
 *  rather than a fixed delay, because "rendered" means "after `virtua`'s own
 *  effect has run", which is a frame, not a duration. */
function waitForRow(node: HTMLElement, id: string, tries = 30): Promise<HTMLElement | null> {
  return new Promise((resolve) => {
    function attempt(left: number) {
      const el = node.querySelector<HTMLElement>(`[data-row="${CSS.escape(id)}"]`)
      if (el || left <= 0) {
        resolve(el)
        return
      }
      requestAnimationFrame(() => attempt(left - 1))
    }
    attempt(tries)
  })
}

/**
 * Give `node`'s rows a single tab stop and arrow-key movement.
 *
 * `current` is the id of the selected row, so the stop follows the
 * selection -- pass a bare id, or `null` where there is none and the first
 * row holds it. Pass `{ current, virtual }` instead for a list `VirtualList`
 * backs; see `RovingVirtual` and the module doc's "Virtualized lists".
 */
export function rovingFocus(node: HTMLElement, params: RovingParams = null) {
  let { current, virtual } = normalizeParams(params)

  /** Hand the tab stop to `chosen`'s stop control and take it off every other. */
  function assign(chosen: HTMLElement) {
    const stop = stopOf(chosen)
    for (const control of rowControlsIn(node)) control.tabIndex = control === stop ? 0 : -1
  }

  /** Whichever control already holds the tab stop -- focus itself, if it is
   *  on one of this list's controls, otherwise the control `current` names,
   *  otherwise the first row's. */
  function stopControl(rows: HTMLElement[]): HTMLElement | null {
    const active = document.activeElement
    if (active instanceof HTMLElement && rowControlsIn(node).includes(active)) return active
    const row = rows.find((el) => el.dataset.row === current) ?? rows[0]
    return row ? stopOf(row) : null
  }

  /**
   * Set `tabindex` on every rendered control, unconditionally -- this used
   * to bail out whenever focus was already inside the list, on the grounds
   * that the stop should not move under a reader who has arrowed away from
   * `current`. That was right about the stop and wrong about the sweep: a
   * row `VirtualList` draws newly, while the list already has focus, kept
   * its element's native `tabindex="0"` because nothing ran to correct it,
   * so Tab visited it -- finding 6. `stopControl` now keeps focus's own
   * control as the stop without the sweep skipping every other row drawn
   * since. Run on every DOM mutation (`restack`, below) rather than only at
   * mount, this is what keeps every row but the one actually focused off
   * the tab order as it is drawn.
   */
  function sweep() {
    const rows = rowsIn(node)
    if (rows.length === 0) return
    const stop = stopControl(rows)
    for (const control of rowControlsIn(node)) control.tabIndex = control === stop ? 0 : -1
  }

  function focusRow(rows: HTMLElement[], index: number) {
    const target = rows[Math.max(0, Math.min(index, rows.length - 1))]
    if (!target) return
    assign(target)
    stopOf(target)?.focus()
  }

  /** The virtualized path: scroll the data index into view, wait for its
   *  row to mount, then focus it -- `focusRow` above cannot, because the
   *  row is not in the DOM yet to be found. */
  async function focusVirtualIndex(index: number) {
    const id = virtual?.idAt(index)
    if (!virtual || id === null || id === undefined) return
    virtual.scrollToIndex(index)
    const target = await waitForRow(node, id)
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
    const domAt = row ? rows.indexOf(row) : -1
    // The keys only mean this once focus is on a row. A field above the list
    // -- the search box -- keeps its own use of Home and End.
    if (domAt < 0 || !row) return

    const columns = columnsOf(rows)
    // A row of several controls spends Left and Right on them; a grid of
    // single-control cards spends them on the line. See the header.
    const withinRow = controlsOf(row).length > 1
    // Roughly a screen, worked out from the list rather than assumed, so it
    // is right for a dense row and for a card.
    const lineHeight = row.getBoundingClientRect().height || 1
    const page = Math.max(1, Math.round(node.clientHeight / lineHeight) - 1) * columns

    // Left/Right within a multi-control row step between its controls, not
    // the selected index -- `nextRovingIndex` knows nothing about controls,
    // only rows, so this is decided here rather than folded into it.
    if (withinRow && (e.key === 'ArrowRight' || e.key === 'ArrowLeft')) {
      focusControl(row, e.key === 'ArrowRight' ? 1 : -1)
      e.preventDefault()
      return
    }

    if (virtual) {
      // Finding 6: the data's own index, not the DOM's -- `domAt` counts
      // only what `VirtualList` currently has drawn, which is why Home in a
      // long mailbox used to stop at the top of the rendered window rather
      // than the top of the mailbox.
      const id = row.dataset.row
      const at = id ? virtual.indexOf(id) : -1
      if (at < 0) return
      const target = nextRovingIndex(at, e.key, virtual.length, { columns, page, withinRow })
      if (target === null || target === at) return
      e.preventDefault()
      void focusVirtualIndex(target)
      return
    }

    const target = nextRovingIndex(domAt, e.key, rows.length, { columns, page, withinRow })
    if (target === null) return
    focusRow(rows, target)
    // Only once a key was one of ours: the browser would otherwise scroll
    // the list out from under the row it just moved to.
    e.preventDefault()
  }

  node.addEventListener('keydown', onKeydown)

  // Swept when the rows change, not only when `current` does.
  //
  // The action runs before a keyed `{#each}` has inserted anything, and a
  // list whose store is still loading has no rows at all -- so the first
  // sweep usually happens against an empty list. Re-running on the
  // parameter alone was not enough: the journal got away with it because
  // opening the newest entry moves `selectedEntry` off null as the load
  // lands, and the todo list, whose selection stays null until somebody
  // clicks a task, was left with every one of its controls a tab stop. A
  // virtualized list needs this same re-run for a second reason again --
  // see `sweep`'s own doc.
  //
  // `childList` only. Handing out `tabindex` is an attribute change, and
  // observing those as well would be a loop.
  let queued: number | null = null
  const restack = () => {
    if (queued !== null) return
    queued = requestAnimationFrame(() => {
      queued = null
      sweep()
    })
  }
  const observer = new MutationObserver(restack)
  observer.observe(node, { childList: true, subtree: true })
  restack()

  return {
    update(next: RovingParams = null) {
      const normalized = normalizeParams(next)
      current = normalized.current
      virtual = normalized.virtual
      sweep()
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
