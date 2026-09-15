<script lang="ts" generics="T">
  // Our own face on `virtua`'s Svelte adapter, so the rest of the interface
  // depends on this component and never on `virtua` directly -- the same
  // reason `RichText.svelte` sits in front of TipTap. Swapping the library
  // behind this one file, or teaching it a second backing implementation, is
  // the whole payoff of not having spelt `import { Virtualizer } from
  // 'virtua/svelte'` in every list that draws more rows than it should keep
  // in the DOM.
  //
  // Headless on purpose: this draws the rows and nothing around them, rather
  // than owning a scrolling `<div>` of its own the way `virtua`'s `VList`
  // does. It measures the *parent* element's scrolling instead (`virtua`'s
  // own `Virtualizer`, one layer under `VList`). That is what lets it sit
  // inside `EntryList.svelte`'s existing `.rows.scroll` element -- the one
  // element `use:rovingFocus` and the row context menu are already wired to
  // -- unchanged, rather than asking every caller to move those onto a second
  // scrolling `<div>` this component would otherwise have introduced.
  //
  // Rows are keyed by id -- `getKey`, defaulting to `(item) => item.id` -- so
  // a row keeps its place, its measured height and its scroll position when
  // the array behind it is replaced wholesale, which is how every store here
  // delivers a fresh page. Heights are variable and unannounced: `virtua`
  // measures each row itself once it is drawn and remembers the answer, so
  // there is no `itemSize` to get wrong for a list whose rows carry a
  // one-line title some days and three lines of excerpt and a cover image on
  // others.
  //
  // `items` -- and every row inside it -- should be handed in as `$state.raw`
  // by the caller, not `$state`. Svelte's ordinary `$state` deep-proxies every
  // object and array reachable from it, which is the reactivity a form wants
  // and is pure overhead here: this component never mutates a row in place,
  // it only ever receives a new array and new row objects from a store's
  // `refresh` (or, now, a patched-in summary from `live-apply.ts`), so there
  // is nothing for a deep proxy to catch that a shallow, `.raw` reference
  // would not. Proxying every one of a hundred thousand summaries to watch
  // for a mutation that never happens is exactly the tax the plan this
  // component exists for (`docs/plans/mail.md`, "Collections that are large")
  // was written to stop paying.
  import { Virtualizer, type VirtualizerHandle } from 'virtua/svelte'
  import type { Snippet } from 'svelte'

  interface Props<T> {
    /** The full, already-ordered list. See the module doc for `$state.raw`. */
    items: readonly T[]
    /** A row's identity. Defaults to `(item) => (item as { id: string }).id`. */
    getKey?: (item: T, index: number) => string | number
    /**
     * The row that should be scrolled into view when this changes -- the
     * journal's `j`/`k`, or any other keyboard move that changes the
     * selection without the reader's hand touching the scrollbar. Compared
     * by `getKey`, not by reference, so a store that hands back a fresh
     * object for the same id (every `refresh` does) does not re-trigger it.
     */
    selectedId?: string | number | null | undefined
    /**
     * Called once when scrolled within `endThreshold` of the bottom -- a
     * keyset page's cue to ask its store for the next batch. Not called
     * again until `items.length` changes, so a store that does not grow the
     * list in response (there is no more) is not asked on every further
     * pixel of scroll.
     */
    onEndReached?: () => void
    /** Pixels from the end that count as "reached". */
    endThreshold?: number
    /**
     * Keep the scroll position anchored to what is on screen, rather than to
     * the top, when rows are spliced onto the *front* of `items` -- `virtua`'s
     * own `shift`. For a keyset page loaded upward (older rows prepended to
     * the same array); leave it off for the ordinary case of a list replaced
     * wholesale, or a page appended at the end, where it does nothing useful.
     */
    shift?: boolean
    /** An estimate for a row not yet measured. Unset: `virtua` estimates it. */
    itemSize?: number
    /** How to draw one row. */
    children: Snippet<[item: T, index: number]>
  }

  let {
    items,
    getKey,
    selectedId = null,
    onEndReached,
    endThreshold = 600,
    shift = false,
    itemSize,
    children: renderRow,
  }: Props<T> = $props()

  /** `getKey`, or the default -- a plain function rather than `$derived`, so
   * its type is the one signature `Virtualizer` wants rather than a union of
   * two. */
  function keyOf(item: T, index: number): string | number {
    return getKey ? getKey(item, index) : (item as { id: string }).id
  }

  let handle: VirtualizerHandle | undefined = $state()

  // ── Scroll the selected row into view ───────────────────────────────────
  //
  // Not on every render: only when `selectedId` itself changes, so scrolling
  // by hand and then pressing `j` does not first snap back to wherever the
  // selection already was.
  let lastSelected: string | number | null | undefined = undefined
  $effect(() => {
    const id = selectedId
    if (id === lastSelected) return
    if (id === null || id === undefined) {
      lastSelected = id
      return
    }
    // Remembered only once the scroll has actually happened. Before the
    // handle is bound, or before the selected row's page has arrived, this
    // waits: `handle` and `items` are both read here, so the effect runs
    // again when either changes, and the selection is scrolled to then.
    if (!handle) return
    const index = items.findIndex((item, i) => keyOf(item, i) === id)
    if (index < 0) return
    lastSelected = id
    handle.scrollToIndex(index, { align: 'nearest' })
  })

  // ── The end-of-list callback ─────────────────────────────────────────────
  //
  // `virtua` has no such event of its own -- it only reports the raw scroll
  // offset -- so this compares that against the viewport and the scrolled
  // size on every `onscroll` and fires once per growth of `items`, the same
  // "ask, then wait to be given more before asking again" shape a keyset page
  // wants.
  let endFired = $state(false)
  $effect(() => {
    void items.length
    endFired = false
  })
  function checkEnd() {
    if (!onEndReached || endFired || !handle) return
    const viewport = handle.getViewportSize()
    const remaining = handle.getScrollSize() - (handle.getScrollOffset() + viewport)
    if (remaining <= endThreshold) {
      endFired = true
      onEndReached()
    }
  }

  /**
   * Scroll a data index into view -- for `rovingFocus`'s virtualized path
   * (`lib/roving.ts`'s `RovingVirtual`), which needs a row on screen before
   * it can focus it, and cannot reach `handle` itself: that is this
   * component's own, not something a caller holding `bind:this` sees.
   * `align` defaults to `'nearest'`, the same choice the `selectedId` effect
   * above already makes, so Home does not additionally re-centre a row that
   * was already on screen.
   */
  export function scrollToIndex(
    index: number,
    opts: { align?: 'start' | 'center' | 'end' | 'nearest' } = {},
  ): void {
    handle?.scrollToIndex(index, { align: opts.align ?? 'nearest' })
  }
</script>

<Virtualizer bind:this={handle} data={items} getKey={keyOf} {itemSize} {shift} onscroll={checkEnd}>
  {#snippet children(item: T, index: number)}
    {@render renderRow(item, index)}
  {/snippet}
</Virtualizer>
