<script lang="ts">
  // A pane's own right-edge handle: drag to resize, double-click to reset,
  // arrow keys when it has focus. Written once so the sidebar and the Mail
  // app's thread list -- the two panes a person actually wants narrower or
  // wider some days and not others -- share one dragging, one keyboard and
  // one persistence story rather than three slightly different ones.
  //
  // Deliberately not `width = $bindable()`: the one thing every caller
  // actually wants out of this is the CSS variable its own stylesheet
  // already sizes the pane from (`--sidebar-w`, `--mail-list-w`) kept in
  // step, not a number handed back to thread through props and into an
  // inline `style`. Setting the custom property directly on
  // `documentElement` reaches every rule that reads it -- `Sidebar.svelte`'s
  // own `width: var(--sidebar-w)` and `SettingsView.svelte`'s, both -- with
  // nothing here needing to know that a second reader exists at all.
  //
  // The stored width is read and applied as plain top-level script, not
  // inside an `$effect`: an effect's first run is scheduled after the DOM
  // this component's own markup produces has already been attached, which
  // is one tick later than "before the pane using it is first painted."
  // Plain script in a Svelte component runs synchronously during mount,
  // ahead of the browser's next paint, which is what keeps the pane from
  // flashing at its default width before snapping to whatever was dragged
  // last time.

  let {
    cssVar,
    storageKey,
    defaultWidth,
    min,
    max,
  }: {
    /** The custom property this pane's own width already comes from. */
    cssVar: string
    /** Where the dragged width persists, across restarts. */
    storageKey: string
    defaultWidth: number
    min: number
    max: number
  } = $props()

  function clamp(n: number): number {
    return Math.min(max, Math.max(min, n))
  }

  function readStored(): number {
    try {
      const raw = localStorage.getItem(storageKey)
      const n = raw ? Number(raw) : NaN
      return Number.isFinite(n) ? clamp(n) : defaultWidth
    } catch {
      // A browser that refuses `localStorage` (private mode, some
      // embedders) just never remembers -- every session opens at the
      // default, which is no worse than before this existed.
      return defaultWidth
    }
  }

  function apply(n: number): void {
    document.documentElement.style.setProperty(cssVar, `${n}px`)
  }

  function persist(n: number): void {
    try {
      localStorage.setItem(storageKey, String(n))
    } catch {
      // Ditto -- the width simply does not survive a reload.
    }
  }

  // `$state`, not a bare `let`: `aria-valuenow` and the dragging state below
  // both read this back, and a plain local variable's later reassignment
  // (every drag, every arrow key) would never tell the template to redraw.
  const initial = readStored()
  let width = $state(initial)
  apply(initial)

  let dragging = $state(false)
  let startClientX = 0
  let startWidth = 0

  function onPointerDown(e: PointerEvent) {
    dragging = true
    startClientX = e.clientX
    startWidth = width
    ;(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId)
  }

  function onPointerMove(e: PointerEvent) {
    if (!dragging) return
    width = clamp(startWidth + (e.clientX - startClientX))
    apply(width)
  }

  function onPointerUp(e: PointerEvent) {
    if (!dragging) return
    dragging = false
    ;(e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId)
    persist(width)
  }

  function onDoubleClick() {
    width = defaultWidth
    apply(width)
    persist(width)
  }

  /** Left/Right by 16px -- the one case this is also a *vertical*
   *  separator's keyboard story needs to spell out, since the mouse
   *  gesture it stands in for drags along the horizontal axis despite the
   *  handle itself running top to bottom. */
  function onKeydown(e: KeyboardEvent) {
    if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return
    e.preventDefault()
    width = clamp(width + (e.key === 'ArrowRight' ? 16 : -16))
    apply(width)
    persist(width)
  }
</script>

<!-- A focusable `separator` is ARIA's own window-splitter pattern -- an
     interactive widget, whatever Svelte's static list of non-interactive
     roles says -- and the keyboard handler is what makes it one. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<div
  class="resizer"
  class:dragging
  role="separator"
  aria-orientation="vertical"
  aria-valuenow={Math.round(width)}
  aria-valuemin={min}
  aria-valuemax={max}
  tabindex="0"
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  onpointercancel={onPointerUp}
  ondblclick={onDoubleClick}
  onkeydown={onKeydown}
>
  <div class="line" aria-hidden="true"></div>
</div>

<style>
  /* The hit area is much wider than the line it draws -- a 1px target is
     not one a pointer can reliably land on -- positioned so it straddles
     the pane's own right edge rather than sitting entirely inside or
     entirely outside it. The pane this is dropped into needs
     `position: relative` for `right: 0` here to mean its own edge. */
  .resizer {
    position: absolute;
    top: 0;
    bottom: 0;
    right: -5px;
    width: 11px;
    display: flex;
    justify-content: center;
    cursor: col-resize;
    touch-action: none;
    z-index: 5;
  }
  .resizer:focus-visible {
    outline: none;
  }
  .line {
    width: 1px;
    height: 100%;
    background: transparent;
    transition: background var(--fast) var(--ease);
  }
  .resizer:hover .line,
  .resizer.dragging .line,
  .resizer:focus-visible .line {
    background: var(--journal-accent, var(--accent));
  }
</style>
