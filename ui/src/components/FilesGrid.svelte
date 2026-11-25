<script lang="ts">
  // The files as tiles: a picture each, with its name under it, subfolders
  // first as small cards of their own.
  //
  // A folder can hold thousands of pictures, so this never draws them all.
  // The tiles are cut into rows as wide as the pane allows and handed to
  // `VirtualList`, which keeps only the rows on screen -- and the store pages
  // the files in as the end comes near (`files.loadMore`). Two things keep a
  // scroll through them cheap on WebKitGTK, where it was not: no blurred
  // shadow on a tile (a selection is an outline, a badge is a flat chip), and
  // the scrolling box has a height of its own, not one stretched out of a
  // flex column.
  //
  // Picking works as the mail list's does -- a click selects, Ctrl or Cmd
  // adds, Shift extends -- and the keyboard walks the grid: the arrows move,
  // Enter opens, Delete trashes, F2 renames. A folder opens on a click, as
  // it does in Dropbox, and takes files dragged onto it.

  import { files } from '../lib/files.svelte'
  import { fileWhen, formatSize } from '../lib/files'
  import {
    acceptDrag,
    cellsOf,
    clearAll,
    dropInto,
    filesUi,
    folderMenu,
    formatDuration,
    pick,
    renameField,
    rowsOf,
    showFileMenu,
    startDrag,
    type Cell,
    type CellRow,
  } from '../lib/files-ui.svelte'
  import { plural, rowDate } from '../lib/format'
  import { isTyping } from '../lib/keys'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu } from '../lib/menu'
  import { phone } from '../lib/phone.svelte'
  import FileThumb from './FileThumb.svelte'
  import Icon from './Icon.svelte'
  import VirtualList from './VirtualList.svelte'

  /** The narrowest a tile is drawn, and what separates two. A phone's are
   *  closer, and a little narrower: two to a row from the smallest phone to
   *  the largest. */
  const TILE = $derived(phone.on ? 140 : 156)
  const GAP = $derived(phone.on ? 8 : 12)
  const PAD = $derived(phone.on ? 10 : 16)

  let width = $state(900)
  const cols = $derived(Math.max(1, Math.floor((width - PAD * 2 + GAP) / (TILE + GAP))))
  /** What a row of tiles is expected to measure before it has been drawn. */
  const rowEstimate = $derived(Math.round((width - PAD * 2 - (cols - 1) * GAP) / cols) + 50 + GAP)

  const cells = $derived(cellsOf())
  const rows = $derived(rowsOf(cells, cols))
  const picked = $derived(new Set(files.selection))
  const trashed = $derived(files.view === 'trash')

  /** How many files sit under each stack's top, among those loaded. */
  const stacked = $derived.by(() => {
    const counts = new Map<string, number>()
    for (const c of cells) {
      if (c.kind === 'file' && c.file.stack && c.file.stack !== c.file.id) {
        counts.set(c.file.stack, (counts.get(c.file.stack) ?? 0) + 1)
      }
    }
    return counts
  })

  // ── Cursor ─────────────────────────────────────────────────────────

  /** The cell the keyboard is on, by key; the selection follows it. */
  let cursor = $state<string | null>(null)
  let scroller = $state<HTMLElement | null>(null)

  const cursorRow = $derived(rows.find((r) => r.cells.some((c) => c.key === cursor))?.key ?? null)

  // A cell that is no longer listed -- trashed, moved, filtered -- is no
  // longer where the keyboard is.
  $effect(() => {
    if (cursor !== null && !cells.some((c) => c.key === cursor)) cursor = null
  })

  function at(key: string | null): { r: number; c: number } | null {
    if (key === null) return null
    for (let r = 0; r < rows.length; r++) {
      const c = rows[r]!.cells.findIndex((cell) => cell.key === key)
      if (c >= 0) return { r, c }
    }
    return null
  }

  function cellAt(r: number, c: number): Cell | undefined {
    const row = rows[Math.max(0, Math.min(rows.length - 1, r))]
    return row?.cells[Math.max(0, Math.min((row?.cells.length ?? 1) - 1, c))]
  }

  /** Put the cursor on `cell`; a file is selected with it. */
  function go(cell: Cell | undefined, extend: boolean) {
    if (!cell) return
    cursor = cell.key
    if (cell.kind === 'file')
      pick({ shiftKey: extend, ctrlKey: false, metaKey: false }, cell.file.id)
  }

  function step(key: string, shift: boolean) {
    const here = at(cursor) ?? at(files.selection[0] ?? null)
    if (rows.length === 0) return
    if (!here) return go(cellAt(0, 0), false)
    switch (key) {
      case 'ArrowLeft': {
        if (here.c > 0) go(cellAt(here.r, here.c - 1), shift)
        else if (here.r > 0) go(cellAt(here.r - 1, Infinity), shift)
        break
      }
      case 'ArrowRight': {
        const row = rows[here.r]!
        if (here.c < row.cells.length - 1) go(cellAt(here.r, here.c + 1), shift)
        else if (here.r < rows.length - 1) go(cellAt(here.r + 1, 0), shift)
        break
      }
      case 'ArrowUp':
        go(cellAt(here.r - 1, here.c), shift)
        break
      case 'ArrowDown':
        go(cellAt(here.r + 1, here.c), shift)
        break
      case 'PageUp':
        go(cellAt(here.r - 4, here.c), shift)
        break
      case 'PageDown':
        go(cellAt(here.r + 4, here.c), shift)
        break
      case 'Home':
        go(cellAt(0, 0), shift)
        break
      case 'End':
        go(cellAt(rows.length - 1, Infinity), shift)
        break
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (isTyping(e.target) || e.defaultPrevented || e.altKey) return
    const meta = e.ctrlKey || e.metaKey
    const key = e.key
    if (
      !meta &&
      [
        'ArrowLeft',
        'ArrowRight',
        'ArrowUp',
        'ArrowDown',
        'PageUp',
        'PageDown',
        'Home',
        'End',
      ].includes(key)
    ) {
      e.preventDefault()
      step(key, e.shiftKey)
    } else if (meta && key.toLowerCase() === 'a') {
      e.preventDefault()
      files.selectAll()
    } else if (key === 'Enter' || key === ' ') {
      e.preventDefault()
      openCursor()
    } else if (key === 'Escape') {
      if (files.selection.length > 0) {
        e.preventDefault()
        clearAll()
      }
    } else if (key === 'F2') {
      e.preventDefault()
      const cell = cells.find((c) => c.key === cursor)
      if (cell) renaming = cell.key
    } else if (key === 'Delete' || (key === 'Backspace' && files.selection.length > 0)) {
      e.preventDefault()
      removeSelection()
    } else if (key === 'Backspace' && files.view === 'browse') {
      e.preventDefault()
      files.up()
    }
  }

  function openCursor() {
    const cell = cells.find((c) => c.key === cursor)
    if (cell?.kind === 'folder') files.openFolder(cell.folder.path)
    else if (cell?.kind === 'file') files.open(cell.file.id)
    else if (files.selection[0]) files.open(files.selection[0])
  }

  function removeSelection() {
    const ids = [...files.selection]
    if (ids.length === 0) return
    if (files.view === 'trash') files.deleteForever(ids)
    else if (files.view === 'missing') files.forgetMissing(ids)
    else void files.trash(ids)
  }

  // ── Mouse ──────────────────────────────────────────────────────────

  function clickFile(e: MouseEvent, cell: Extract<Cell, { kind: 'file' }>) {
    cursor = cell.key
    // A finger has no double-click and no Ctrl: a tap opens the file, unless
    // some are picked already, when it picks another -- as in any gallery.
    if (phone.on) {
      if (files.selection.length > 0) files.select(cell.file.id, 'toggle')
      else files.open(cell.file.id)
      return
    }
    pick(e, cell.file.id)
  }

  /** The circle on a tile: picks it, which is how a phone starts picking. */
  function tickFile(e: MouseEvent, cell: Extract<Cell, { kind: 'file' }>) {
    e.stopPropagation()
    cursor = cell.key
    files.select(cell.file.id, 'toggle')
  }

  function menuFile(e: MouseEvent, cell: Extract<Cell, { kind: 'file' }>) {
    if (!picked.has(cell.file.id)) {
      cursor = cell.key
      pick({ shiftKey: false, ctrlKey: false, metaKey: false }, cell.file.id)
    }
    void showFileMenu(e, cell.file, { rename: () => (renaming = cell.key) })
  }

  function menuFolder(e: MouseEvent, cell: Extract<Cell, { kind: 'folder' }>) {
    menu.show(e, folderMenu(cell.folder, { rename: () => (renaming = cell.key) }))
  }

  /** On the pane between the tiles: what can be done with the folder itself. */
  function menuPane(e: MouseEvent) {
    if ((e.target as Element).closest('.tile, .folder')) return
    const browsing = files.view === 'browse'
    menu.show(
      e,
      tidyMenu([
        files.view !== 'trash' &&
          files.view !== 'missing' && {
            label: 'Add files…',
            icon: 'upload',
            run: () => files.create(),
          },
        browsing && {
          label: 'New folder',
          icon: 'plus',
          run: () =>
            filesUi.askName({
              title: 'New folder',
              label: 'Name',
              confirm: 'Create',
              run: (name) => files.createFolder(name.trim()),
            }),
        },
        SEP,
        { label: 'Select all', icon: 'tick', run: () => files.selectAll() },
      ]),
    )
  }

  function clickPane(e: MouseEvent) {
    if (!(e.target as Element).closest('.tile, .folder')) clearAll()
  }

  // ── Folders as drop targets ────────────────────────────────────────

  let over = $state<string | null>(null)

  // ── Rename in place ────────────────────────────────────────────────

  let renaming = $state<string | null>(null)

  function commit(cell: Cell, input: HTMLInputElement) {
    // Pressing Enter removes the field, which blurs it: the second call finds
    // nothing being renamed and does nothing.
    if (renaming !== cell.key) return
    renaming = null
    const name = input.value.trim()
    if (cell.kind === 'file') {
      if (name && name !== cell.file.name) void files.renameFile(cell.file.id, name)
    } else if (name && name !== cell.folder.name) {
      void files.renameFolder(cell.folder.path, name)
    }
    scroller?.focus({ preventScroll: true })
  }

  function renameKeys(e: KeyboardEvent, cell: Cell) {
    e.stopPropagation()
    if (e.key === 'Enter') {
      e.preventDefault()
      commit(cell, e.currentTarget as HTMLInputElement)
    } else if (e.key === 'Escape') {
      e.preventDefault()
      renaming = null
      scroller?.focus({ preventScroll: true })
    }
  }
</script>

<div
  class="grid scroll"
  bind:this={scroller}
  bind:clientWidth={width}
  style:--cols={cols}
  style:--gap="{GAP}px"
  style:--pad="{PAD}px"
  role="listbox"
  aria-multiselectable="true"
  aria-label="Files"
  tabindex="0"
  {onkeydown}
  onclick={clickPane}
  oncontextmenu={menuPane}
>
  <VirtualList
    items={rows}
    getKey={(r: CellRow) => r.key}
    selectedId={cursorRow}
    itemSize={rowEstimate}
    onEndReached={() => {
      if (files.hasMore) void files.loadMore()
    }}
  >
    {#snippet children(row: CellRow)}
      <div class="row" class:dirs={row.kind === 'folders'}>
        {#each row.cells as cell (cell.key)}
          {#if cell.kind === 'folder'}
            {@const folder = cell.folder}
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_interactive_supports_focus -->
            <div
              class="folder"
              class:drop={over === cell.key}
              class:cursor={cursor === cell.key}
              role="option"
              aria-selected="false"
              onclick={() => {
                cursor = cell.key
                files.openFolder(folder.path)
              }}
              oncontextmenu={(e) => menuFolder(e, cell)}
              ondragover={(e) => {
                if (acceptDrag(e)) over = cell.key
              }}
              ondragleave={() => (over = null)}
              ondrop={(e) => {
                over = null
                dropInto(e, folder.path)
              }}
            >
              <span class="folder-icon"><Icon name="folder" size={20} /></span>
              <span class="folder-text">
                {#if renaming === cell.key}
                  <input
                    class="rename"
                    value={folder.name}
                    aria-label="Folder name"
                    use:renameField
                    onclick={(e) => e.stopPropagation()}
                    onkeydown={(e) => renameKeys(e, cell)}
                    onblur={(e) => commit(cell, e.currentTarget)}
                  />
                {:else}
                  <span class="name" title={folder.name}>{folder.name}</span>
                {/if}
                <span class="sub">{plural(folder.items, 'item')}</span>
              </span>
            </div>
          {:else if cell.kind === 'file'}
            {@const file = cell.file}
            {@const missing = file.status.state === 'missing'}
            {@const length = file.media.durationMs}
            {@const under = stacked.get(file.id) ?? 0}
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_interactive_supports_focus -->
            <div
              class="tile"
              class:sel={picked.has(file.id)}
              class:cursor={cursor === cell.key}
              class:gone={missing}
              role="option"
              aria-selected={picked.has(file.id)}
              draggable={!trashed && renaming !== cell.key && !phone.on}
              onclick={(e) => clickFile(e, cell)}
              ondblclick={() => files.open(file.id)}
              oncontextmenu={(e) => menuFile(e, cell)}
              ondragstart={(e) => startDrag(e, file)}
            >
              <div class="art">
                <FileThumb {file} />
                {#if phone.on}
                  <button
                    class="tick"
                    class:on={picked.has(file.id)}
                    aria-label="Select {file.name}"
                    aria-pressed={picked.has(file.id)}
                    onclick={(e) => tickFile(e, cell)}
                  >
                    <Icon name="check" size={14} weight={2.4} />
                  </button>
                {/if}
                <span class="marks">
                  {#if file.favourite}
                    <span class="mark star" title="Favourite"
                      ><Icon name="star" size={11} filled /></span
                    >
                  {/if}
                  {#if file.rating}
                    <span class="mark" title="{file.rating} of 5"
                      ><Icon name="star" size={11} filled /> {file.rating}</span
                    >
                  {/if}
                </span>
                <span class="marks end">
                  {#if file.storage.kind === 'sealed'}
                    <span class="mark" title="Encrypted"><Icon name="lock" size={11} /></span>
                  {/if}
                  {#if under > 0}
                    <span class="mark" title="A stack of {under + 1}"
                      ><Icon name="layers" size={11} /> {under + 1}</span
                    >
                  {:else if file.stack && file.stack !== file.id}
                    <span class="mark" title="In a stack"><Icon name="layers" size={11} /></span>
                  {/if}
                </span>
                {#if missing}
                  <span class="mark gone-mark" title="Missing from the folder">
                    <Icon name="alert" size={12} /> Missing
                  </span>
                {:else if length}
                  <span class="mark time">{formatDuration(length)}</span>
                {/if}
              </div>
              <div class="cap">
                {#if renaming === cell.key}
                  <input
                    class="rename"
                    value={file.name}
                    aria-label="File name"
                    use:renameField
                    onclick={(e) => e.stopPropagation()}
                    ondblclick={(e) => e.stopPropagation()}
                    onkeydown={(e) => renameKeys(e, cell)}
                    onblur={(e) => commit(cell, e.currentTarget)}
                  />
                {:else}
                  <span class="name" title={file.name}>{file.name}</span>
                {/if}
                <span class="sub">{formatSize(file.size)} · {rowDate(fileWhen(file))}</span>
              </div>
            </div>
          {/if}
        {/each}
      </div>
    {/snippet}
  </VirtualList>
</div>

<style>
  .grid {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: var(--sp-1) 0 var(--sp-6);
    outline: none;
    /* Tiles are picked, not typed into; a Shift-click would otherwise select
       the names between them. */
    user-select: none;
  }

  .row {
    display: grid;
    grid-template-columns: repeat(var(--cols), minmax(0, 1fr));
    gap: var(--gap);
    padding: 0 var(--pad) var(--gap);
  }

  /* A tile is a picture over two quiet lines. No shadow, blurred or not: a
     few hundred of them scrolling past is what a blur cannot keep up with. */
  .tile {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
    padding: 6px;
    border-radius: var(--radius-lg);
    cursor: default;
  }
  .tile:hover {
    background: var(--bg-hover);
  }
  .tile.sel {
    background: var(--bg-selected);
  }
  .art {
    position: relative;
    aspect-ratio: 1 / 1;
    border-radius: var(--radius);
    overflow: hidden;
  }
  .tile.sel .art {
    outline: 3px solid var(--accent);
    outline-offset: -3px;
  }
  .grid:focus-visible .tile.cursor:not(.sel) .art,
  .grid:focus-visible .folder.cursor {
    outline: 2px solid var(--border-strong);
    outline-offset: -2px;
  }
  .tile.gone .art {
    opacity: 0.5;
  }

  /* Badges: flat chips on a tint of the page, legible on any picture. */
  .marks {
    position: absolute;
    top: 5px;
    left: 5px;
    display: flex;
    gap: 3px;
  }
  .marks.end {
    left: auto;
    right: 5px;
  }
  .mark {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    height: 18px;
    padding: 0 5px;
    border-radius: 9px;
    background: color-mix(in oklab, var(--bg) 82%, transparent);
    color: var(--fg);
    font-size: var(--text-xs);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .mark.star {
    color: var(--star);
  }
  .mark.time,
  .mark.gone-mark {
    position: absolute;
    right: 5px;
    bottom: 5px;
  }
  .mark.gone-mark {
    right: auto;
    left: 5px;
    color: var(--danger);
  }

  .cap {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
    padding: 0 2px;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    font-weight: 560;
    color: var(--fg);
  }
  .tile.sel .name {
    color: var(--accent);
  }
  .sub {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .rename {
    width: 100%;
    min-width: 0;
    height: 24px;
    padding: 0 var(--sp-1);
    border: 1px solid var(--accent);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    user-select: text;
  }
  .rename:focus {
    outline: none;
  }

  /* A folder: a card in the tile's column, as tall as its two lines. */
  .folder {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 0;
    height: 52px;
    padding: 0 var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    cursor: default;
  }
  .folder:hover {
    background: var(--bg-hover);
  }
  .folder.drop {
    background: var(--bg-selected);
    border-color: var(--accent);
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .folder-icon {
    flex: none;
    display: flex;
    color: var(--app, var(--accent));
  }
  .folder-text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  /* ── A phone ──────────────────────────────────────────────────────────
     A tap opens a file, so picking is its own control: a ring in the
     picture's corner, filled once picked, with a finger's room around it.
     The favourite and the rating make way for it. */
  .tick {
    position: absolute;
    z-index: 2;
    top: 6px;
    left: 6px;
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border: 2px solid rgb(255 255 255 / 0.92);
    border-radius: 50%;
    background: rgb(0 0 0 / 0.22);
    color: transparent;
  }
  /* The room around it, inside the picture. */
  .tick::before {
    content: '';
    position: absolute;
    inset: -6px;
  }
  .tick.on {
    border-color: var(--accent);
    background: var(--accent);
    color: var(--fg-on-accent);
  }
  :global([data-phone]) .marks:not(.end) {
    left: 38px;
  }
  :global([data-phone]) .grid {
    padding-bottom: calc(var(--fab-size) + var(--sp-6));
  }
  :global([data-phone]) .folder {
    height: 56px;
  }
</style>
