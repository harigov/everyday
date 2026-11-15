<script lang="ts">
  // Pictures and films as a timeline: month and day headings over a dense grid
  // of square tiles, newest (or oldest) first. The Photos view draws it from
  // what the store has loaded, and an open album draws its pictures with it.
  //
  // It is windowed. Every tile is the same square and a heading's height is a
  // constant, so `photos.ts` knows every line's offset before anything is
  // drawn; only the lines near the viewport are in the DOM, however many
  // pictures there are, and the pane has a definite height of its own. Near
  // the bottom it asks for the next page (`onend`).
  //
  // The tile is cheap on purpose, because this pane is what WebKitGTK is
  // asked to scroll: flat fills and a hard-edged ring, no blurred shadows, and
  // `contain: strict` so a tile repainting (its hover, its ring) never reaches
  // its neighbours.
  //
  // Selection and the inspector go through the `files` store: a click selects
  // (shift for a range, ctrl for one more, and the store points the inspector
  // at it). A double-click or Enter opens the viewer, which is drawn here and
  // kept in this component rather than in the store's `viewing`: it walks
  // this timeline's own pictures, an open stack's members among them, which
  // the store's list leaves out, and nothing else in the window opens a
  // second over it. A stack is a top picture with others under it; its badge
  // opens it in place, the members following the top under the same day.
  //
  // The context menu is the Files app's own (`fileMenu`) with what only a
  // picture has put above it: album, stack and hidden.

  import { tick, untrack, type Snippet } from 'svelte'
  import { api } from '../lib/api'
  import { handle } from '../lib/errors'
  import { files, type SelectMode } from '../lib/files.svelte'
  import { clearAll, fileMenu, filesUi, formatDuration } from '../lib/files-ui.svelte'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import {
    GAP,
    PAD,
    layoutTimeline,
    rowLine,
    scrollFor,
    showWithStacks,
    stackQuery,
    stackSize,
    stepIndex,
    visibleLines,
    zoomTarget,
    type MoveKey,
    type Zoom,
  } from '../lib/photos'
  import type { FileEntry, FileId } from '../lib/types'
  import AlbumPicker from './AlbumPicker.svelte'
  import FileThumb from './FileThumb.svelte'
  import FileViewer from './FileViewer.svelte'
  import Icon from './Icon.svelte'

  let {
    items,
    hasMore = false,
    headings = true,
    zoom = 'medium',
    resetKey = '',
    onend,
    empty,
  }: {
    /** What to draw, in the order to draw it: the store's loaded files. */
    items: readonly FileEntry[]
    /** There is more to load past the end. */
    hasMore?: boolean
    /** Month and day headings; off for a listing that is not in date order. */
    headings?: boolean
    zoom?: Zoom
    /** A change of this goes back to the top and forgets open stacks. */
    resetKey?: string
    /** Scrolled near the end: load the next page. */
    onend?: () => void
    /** What to show when there is nothing to draw. */
    empty?: Snippet
  } = $props()

  const ARROWS: readonly string[] = [
    'ArrowLeft',
    'ArrowRight',
    'ArrowUp',
    'ArrowDown',
    'Home',
    'End',
  ]

  let scroller: HTMLElement | undefined = $state()
  let width = $state(0)
  let viewport = $state(0)
  let scrollTop = $state(0)
  /** The tile Tab lands on, and the arrow keys move from. */
  let cursor = $state<FileId | null>(null)
  /** What each open stack holds, fetched when it was opened. */
  let members = $state.raw<Record<FileId, FileEntry[]>>({})
  const fetching = new Set<FileId>()
  /** The picture open in the viewer. */
  let viewerId = $state<FileId | null>(null)
  /** The album popover, while it is open: where it was raised, and for what. */
  let picking = $state<{ x: number; y: number; ids: FileId[] } | null>(null)

  const shown = $derived(showWithStacks(items, members))
  const list = $derived(shown.map((s) => s.file))
  const at = $derived(new Map(list.map((f, i) => [f.id, i])))
  const layout = $derived(
    layoutTimeline(
      shown.map((s) => ({ id: s.file.id, when: s.when })),
      Math.max(0, width - 2 * PAD),
      zoomTarget(zoom),
      headings,
    ),
  )
  const range = $derived(visibleLines(layout, scrollTop, viewport))
  const from = $derived(range.from)
  const to = $derived(range.to)
  const lines = $derived(layout.lines.slice(from, to))
  const selected = $derived(new Set(files.selection))
  const selecting = $derived(files.selection.length > 0)
  const viewing = $derived(viewerId ? (at.get(viewerId) ?? -1) : -1)

  /** The items drawn now, `[lo, hi)`: the Tab stop has to be one of them. */
  const span = $derived.by(() => {
    let lo = -1
    let hi = -1
    for (const line of lines) {
      if (line.kind !== 'row') continue
      if (lo < 0) lo = line.start
      hi = line.end
    }
    return { lo: Math.max(lo, 0), hi: Math.max(hi, 0) }
  })
  const stop = $derived.by(() => {
    const index = cursor === null ? -1 : (at.get(cursor) ?? -1)
    return index >= span.lo && index < span.hi ? index : span.lo
  })

  // ---- scrolling ------------------------------------------------------------

  /** The list length the next page was last asked for at, so one growth of the
   *  list is one request however far the pane is scrolled. */
  let asked = -1
  /** Replaced on a reset, so the effect below looks again at the new list. */
  let fresh = $state.raw({})

  $effect(() => {
    void fresh
    if (!hasMore || !onend) return
    const remaining = layout.height - (scrollTop + viewport)
    if (remaining > viewport * 1.5 || asked === items.length) return
    asked = items.length
    // Not tracked: asking reads and writes the store's own state, which is
    // not a reason to run this again.
    untrack(onend)
  })

  $effect(() => {
    void resetKey
    members = {}
    cursor = null
    viewerId = null
    asked = -1
    fresh = {}
    scrollTop = 0
    if (scroller) scroller.scrollTop = 0
  })

  /** Give the keyboard back to the tile it was on once the viewer is closed. */
  let wasViewing = false
  $effect(() => {
    const now = viewerId !== null
    if (wasViewing && !now) void focusTile(cursor)
    wasViewing = now
  })

  async function focusTile(id: FileId | null) {
    if (id === null) return
    await tick()
    scroller?.querySelector<HTMLElement>(`[data-tile="${CSS.escape(id)}"] .hit`)?.focus()
  }

  // ---- selection ---------------------------------------------------------------

  function modeOf(e: MouseEvent | KeyboardEvent): SelectMode {
    if (e.shiftKey) return 'range'
    if (e.ctrlKey || e.metaKey) return 'toggle'
    return 'replace'
  }

  /** Select (the store points the inspector at it) and make it the cursor. */
  function choose(id: FileId, mode: SelectMode) {
    cursor = id
    files.select(id, mode)
  }

  /** Open the viewer on a picture, and point the inspector at it. */
  function openViewer(id: FileId) {
    cursor = id
    viewerId = id
    files.focus(id)
  }

  /** The viewer moved to another picture; near the end, the next page is
   *  asked for before it is wanted. */
  function viewerMoved(index: number) {
    const next = list[index]
    if (!next) return
    openViewer(next.id)
    if (hasMore && index >= list.length - 3) onend?.()
  }

  /** Move the cursor to item `index`: select it, bring its row on screen, and
   *  give it the keyboard. */
  async function go(index: number, mode: SelectMode) {
    const file = list[index]
    if (!file) return
    choose(file.id, mode)
    const line = rowLine(layout, index)
    if (line && scroller) {
      scroller.scrollTop = scrollFor(line, scroller.scrollTop, scroller.clientHeight)
      scrollTop = scroller.scrollTop
    }
    await focusTile(file.id)
  }

  /** The pictures a key or the menu acts on: the selection, or just this one. */
  function targets(file: FileEntry): FileId[] {
    return files.selection.includes(file.id) ? [...files.selection] : [file.id]
  }

  function onTileKey(e: KeyboardEvent, file: FileEntry, index: number) {
    if (e.ctrlKey || e.metaKey) {
      if (e.key.toLowerCase() === 'a') {
        e.preventDefault()
        files.selectAll()
      }
      return
    }
    if (e.altKey) return
    if (ARROWS.includes(e.key)) {
      e.preventDefault()
      void go(
        stepIndex(layout, list.length, index, e.key as MoveKey),
        e.shiftKey ? 'range' : 'replace',
      )
      return
    }
    switch (e.key) {
      case 'Enter':
        e.preventDefault()
        openViewer(file.id)
        break
      case ' ':
        e.preventDefault()
        choose(file.id, 'toggle')
        break
      case 'Escape':
        if (!selecting) return
        e.preventDefault()
        clearAll()
        break
      case 'a': {
        e.preventDefault()
        const box = (e.currentTarget as HTMLElement).getBoundingClientRect()
        picking = { x: box.left, y: box.bottom, ids: targets(file) }
        break
      }
      case 'Delete':
      case 'Backspace':
        e.preventDefault()
        void files.trash(targets(file))
        break
      case 'f':
        e.preventDefault()
        void files.update(targets(file), { favourite: !file.favourite })
        break
      case '0':
      case '1':
      case '2':
      case '3':
      case '4':
      case '5':
        e.preventDefault()
        void files.setRating(targets(file), Number(e.key))
        break
    }
  }

  // ---- stacks ----------------------------------------------------------------

  async function toggleStack(top: FileEntry) {
    if (members[top.id]) {
      members = Object.fromEntries(Object.entries(members).filter(([id]) => id !== top.id))
      return
    }
    if (fetching.has(top.id)) return
    fetching.add(top.id)
    try {
      const page = await api.listFiles(stackQuery(top.id, files.currentQuery))
      members = { ...members, [top.id]: page.files.filter((f) => f.stack === top.id) }
    } catch (e) {
      await handle(e)
    } finally {
      fetching.delete(top.id)
    }
  }

  // ---- the context menu ---------------------------------------------------------

  async function onTileMenu(e: MouseEvent, file: FileEntry) {
    e.preventDefault()
    e.stopPropagation()
    const { clientX: x, clientY: y } = e
    // A right-click on a picture that is not selected acts on that one.
    if (!selected.has(file.id)) choose(file.id, 'replace')
    await filesUi.ensureAlbums()
    menu.showAt(x, y, tileMenu(file, x, y))
  }

  function tileMenu(file: FileEntry, x: number, y: number): MenuItem[] {
    const ids = targets(file)
    const picked = ids.flatMap((id) => files.fileById(id) ?? [])
    const hidden = picked.length > 0 && picked.every((f) => f.hidden)
    const stacked = picked.some((f) => f.stack || stackSize(f, files.page?.stackCounts) > 0)
    const others = ids.filter((id) => id !== file.id)
    const album =
      files.view === 'albums' && files.album !== null
        ? files.albums.find((v) => v.album.id === files.album)?.album
        : undefined
    // The Files menu, but for its "Open", which would raise the store's
    // viewer, and its album submenu: the popover finds an album by typing and
    // starts one, which a long submenu does not.
    const common = fileMenu(file).filter(
      (item) =>
        item.kind === 'separator' ||
        item.kind === 'heading' ||
        (item.label !== 'Open' && item.label !== 'Add to album'),
    )
    return tidyMenu([
      ids.length === 1 && {
        label: 'Open',
        icon: 'eye',
        hint: 'Enter',
        run: () => openViewer(file.id),
      },
      { label: 'Add to album…', icon: 'image', hint: 'A', run: () => (picking = { x, y, ids }) },
      album && {
        label: 'Set as album cover',
        icon: 'pin',
        disabled: ids.length !== 1,
        run: () => void files.setAlbumCover(album, file.id),
      },
      album && {
        label: 'Remove from album',
        icon: 'minus',
        run: () => void files.removeFromAlbum(album.id, ids),
      },
      others.length > 0 && {
        label: `Stack ${ids.length} pictures`,
        icon: 'layers',
        run: () => void files.stack(file.id, others),
      },
      stackSize(file, files.page?.stackCounts) > 0 && {
        label: members[file.id] ? 'Close the stack' : 'Open the stack',
        icon: 'layers',
        run: () => void toggleStack(file),
      },
      stacked && { label: 'Unstack', icon: 'layers', run: () => void files.unstack(ids) },
      {
        label: hidden ? 'Show in the timeline' : 'Hide from the timeline',
        icon: 'hidden',
        run: () => void files.update(ids, { hidden: !hidden }),
      },
      SEP,
      ...common,
    ])
  }

  /** The background: what a selection has, with none to act on. */
  function onPaneMenu(e: MouseEvent) {
    if (!selecting) return
    menu.show(e, [{ label: 'Clear the selection', icon: 'close', run: () => clearAll() }])
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="pane scroll"
  bind:this={scroller}
  bind:clientWidth={width}
  bind:clientHeight={viewport}
  onscroll={(e) => (scrollTop = e.currentTarget.scrollTop)}
  oncontextmenu={onPaneMenu}
>
  {#if list.length === 0}
    {@render empty?.()}
  {:else}
    <div
      class="canvas"
      class:selecting
      role="group"
      aria-label="Pictures"
      style="--pad: {PAD}px; --gap: {GAP}px; height: calc({layout.height}px + var(--fab-clear))"
    >
      {#each lines as line (line.key)}
        {#if line.kind === 'month'}
          <h2 class="month" style="top: {line.top}px; height: {line.height}px">
            {line.label}
            <span class="n">{line.count.toLocaleString()}</span>
          </h2>
        {:else if line.kind === 'day'}
          <h3 class="day" style="top: {line.top}px; height: {line.height}px">
            {line.label}
            <span class="n">{line.count.toLocaleString()}</span>
          </h3>
        {:else if line.kind === 'row'}
          <div class="row" style="top: {line.top}px; height: {layout.cell}px">
            {#each list.slice(line.start, line.end) as file, k (file.id)}
              {@const i = line.start + k}
              {@const sel = selected.has(file.id)}
              {@const under = stackSize(file, files.page?.stackCounts)}
              <div
                class="tile"
                class:sel
                class:member={shown[i]?.member}
                data-tile={file.id}
                style="width: {layout.cell}px; height: {layout.cell}px"
              >
                <button
                  class="hit"
                  tabindex={i === stop ? 0 : -1}
                  aria-label={file.name}
                  aria-pressed={sel}
                  onclick={(e) => choose(file.id, modeOf(e))}
                  ondblclick={() => openViewer(file.id)}
                  oncontextmenu={(e) => onTileMenu(e, file)}
                  onkeydown={(e) => onTileKey(e, file, i)}
                  ondragstart={(e) => e.preventDefault()}
                >
                  <FileThumb {file} size={Math.ceil(layout.cell)} fit="cover" />
                </button>

                <button
                  class="dot check"
                  class:on={sel}
                  tabindex="-1"
                  aria-label={sel ? 'Deselect' : 'Select'}
                  aria-pressed={sel}
                  onclick={() => choose(file.id, 'toggle')}
                >
                  <Icon name="check" size={13} weight={2.2} />
                </button>
                <button
                  class="dot fav"
                  class:on={file.favourite}
                  tabindex="-1"
                  aria-label={file.favourite ? 'Take out of favourites' : 'Favourite'}
                  aria-pressed={file.favourite}
                  onclick={() => void files.toggleFavourite(file.id)}
                >
                  <Icon name="star" size={13} filled={file.favourite} />
                </button>

                {#if under > 0 || file.stack || file.rating}
                  <div class="badges">
                    {#if under > 0}
                      <button
                        class="chip stack"
                        class:open={!!members[file.id]}
                        tabindex="-1"
                        aria-expanded={!!members[file.id]}
                        title={`A stack of ${under + 1}: show or hide the others`}
                        onclick={() => void toggleStack(file)}
                      >
                        <Icon name="layers" size={11} />
                        {under + 1}
                      </button>
                    {:else if file.stack}
                      <span class="chip" title="Under another picture in a stack">
                        <Icon name="layers" size={11} />
                      </span>
                    {/if}
                    {#if file.rating}
                      <span class="chip rate" title={`${file.rating} of 5 stars`}>
                        <Icon name="star" size={10} filled />
                        {file.rating}
                      </span>
                    {/if}
                  </div>
                {/if}
                {#if file.kind === 'video'}
                  <span class="chip film" title="A film">
                    <Icon name="play" size={9} filled />
                    {#if file.media.durationMs}{formatDuration(file.media.durationMs)}{/if}
                  </span>
                {/if}
              </div>
            {/each}
          </div>
        {/if}
      {/each}
    </div>
  {/if}
</div>

{#if picking}
  <AlbumPicker
    ids={picking.ids}
    at={{ x: picking.x, y: picking.y }}
    onclose={() => (picking = null)}
  />
{/if}

<!-- Kept while a picture is open even once it has left the list (trashed, say):
     the viewer then shows its neighbour and says so through `onindex`. -->
{#if viewerId !== null}
  <FileViewer
    files={list}
    index={Math.max(viewing, 0)}
    onclose={() => (viewerId = null)}
    onindex={viewerMoved}
  />
{/if}

<style>
  /* The scrolling box: a definite height from its flex parent, the tiles
     inside it laid out by hand in `.canvas`. */
  .pane {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    /* Keeps the width the grid was laid out for whether or not the bar is
       showing, where the engine supports it. */
    scrollbar-gutter: stable;
  }
  .canvas {
    position: relative;
    flex: none;
  }

  /* ── headings ───────────────────────────────────────────────────────── */
  .month,
  .day {
    position: absolute;
    left: var(--pad);
    right: var(--pad);
    display: flex;
    gap: var(--sp-2);
    padding-bottom: 6px;
    /* The room above each is part of its height; the text sits on the tiles. */
    align-items: flex-end;
  }
  .month {
    font-size: var(--text-xl);
    font-weight: 680;
    letter-spacing: -0.015em;
  }
  .day {
    font-size: var(--text-sm);
    font-weight: 620;
    color: var(--fg-muted);
  }
  .n {
    font-size: var(--text-xs);
    font-weight: 500;
    letter-spacing: 0;
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }

  /* ── a row of tiles ──────────────────────────────────────────────────── */
  .row {
    position: absolute;
    left: var(--pad);
    right: var(--pad);
    display: flex;
    gap: var(--gap);
  }

  .tile {
    position: relative;
    flex: none;
    background: var(--bg-sunken);
    /* A tile is a box of a known size, so what happens inside it -- a hover,
       a ring, a badge -- can never make the engine look further than it. */
    contain: strict;
  }
  .hit {
    position: absolute;
    inset: 0;
    display: block;
    overflow: hidden;
    cursor: default;
  }
  .hit:focus-visible {
    outline-offset: -3px;
    border-radius: 0;
  }
  /* The selection: a hard ring and a wash in the accent -- no blur anywhere. */
  .hit::after {
    content: '';
    position: absolute;
    inset: 0;
    border: 3px solid transparent;
    pointer-events: none;
  }
  .tile.sel .hit::after {
    border-color: var(--accent);
    background: color-mix(in oklab, var(--accent) 16%, transparent);
  }
  /* A member of an open stack: a bar down its edge ties it to the top before it. */
  .tile.member::before {
    content: '';
    position: absolute;
    z-index: 2;
    left: 0;
    top: 0;
    bottom: 0;
    width: 3px;
    background: var(--accent);
    pointer-events: none;
  }

  /* ── what sits on a picture ──────────────────────────────────────────────
     White over a dark wash whatever the theme: a picture is not themed, and
     its overlay has to be read against it (as the library's cover chips do). */
  .dot {
    position: absolute;
    z-index: 3;
    top: 6px;
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: 50%;
    color: #fff;
    background: rgb(0 0 0 / 0.38);
    opacity: 0;
    transition: opacity var(--fast) var(--ease);
  }
  .check {
    left: 6px;
  }
  .check.on {
    background: var(--accent);
    color: var(--fg-on-accent);
  }
  .fav {
    right: 6px;
  }
  .fav.on {
    color: var(--star);
  }
  .tile:hover .dot,
  .tile:focus-within .dot,
  .canvas.selecting .check,
  .dot.on {
    opacity: 1;
  }

  .badges {
    position: absolute;
    z-index: 3;
    left: 6px;
    bottom: 6px;
    display: flex;
    gap: 4px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    height: 18px;
    padding: 0 6px;
    border-radius: 999px;
    background: rgb(0 0 0 / 0.5);
    color: #fff;
    font-size: var(--text-xs);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    line-height: 1;
  }
  .chip.rate :global(svg) {
    color: var(--star);
  }
  .chip.stack {
    cursor: pointer;
  }
  .chip.stack:hover,
  .chip.stack.open {
    background: var(--accent);
    color: var(--fg-on-accent);
  }
  .film {
    position: absolute;
    z-index: 3;
    right: 6px;
    bottom: 6px;
  }
</style>
