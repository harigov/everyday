<script lang="ts">
  // The files as rows: a small picture, the name over a quiet line saying
  // what it is or where it is, when, how big, and the marks it carries.
  // Subfolders first, as rows of their own.
  //
  // The same behaviour as the grid, in one column: a click selects, Ctrl or
  // Cmd adds, Shift extends; the arrows walk, Enter opens, Delete trashes,
  // F2 renames; files drag onto a folder to move into it. Rows are windowed
  // by `VirtualList` and the store pages the rest in near the end. The
  // column heads sort, and say which way.

  import { files } from '../lib/files.svelte'
  import { fileWhen, formatSize } from '../lib/files'
  import {
    KIND_LABELS,
    acceptDrag,
    cellsOf,
    clearAll,
    dropInto,
    filesUi,
    folderMenu,
    formatDuration,
    pick,
    renameField,
    showFileMenu,
    startDrag,
    type Cell,
  } from '../lib/files-ui.svelte'
  import { plural, rowDate } from '../lib/format'
  import { isTyping } from '../lib/keys'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu } from '../lib/menu'
  import type { FileEntry, FileSort } from '../lib/types'
  import FileThumb from './FileThumb.svelte'
  import Icon from './Icon.svelte'
  import VirtualList from './VirtualList.svelte'

  const ROW = 52

  let width = $state(900)
  /** Narrow panes give up the date, then the size. */
  const showWhen = $derived(width >= 520)
  const showSize = $derived(width >= 400)

  const cells = $derived(cellsOf())
  const picked = $derived(new Set(files.selection))
  const trashed = $derived(files.view === 'trash')
  const browsing = $derived(files.view === 'browse')

  // ── Cursor ─────────────────────────────────────────────────────────

  let cursor = $state<string | null>(null)
  let scroller = $state<HTMLElement | null>(null)

  $effect(() => {
    if (cursor !== null && !cells.some((c) => c.key === cursor)) cursor = null
  })

  function go(index: number, extend: boolean) {
    const cell = cells[Math.max(0, Math.min(cells.length - 1, index))]
    if (!cell) return
    cursor = cell.key
    if (cell.kind === 'file')
      pick({ shiftKey: extend, ctrlKey: false, metaKey: false }, cell.file.id)
  }

  function onkeydown(e: KeyboardEvent) {
    if (isTyping(e.target) || e.defaultPrevented || e.altKey) return
    const meta = e.ctrlKey || e.metaKey
    const here = cells.findIndex((c) => c.key === cursor)
    const from =
      here >= 0 ? here : cells.findIndex((c) => c.kind === 'file' && picked.has(c.file.id))
    const by = (n: number) => {
      e.preventDefault()
      go(from < 0 ? 0 : from + n, e.shiftKey)
    }
    switch (e.key) {
      case 'ArrowDown':
        if (!meta) by(1)
        break
      case 'ArrowUp':
        if (!meta) by(-1)
        break
      case 'PageDown':
        by(10)
        break
      case 'PageUp':
        by(-10)
        break
      case 'Home':
        e.preventDefault()
        go(0, e.shiftKey)
        break
      case 'End':
        e.preventDefault()
        go(cells.length - 1, e.shiftKey)
        break
      case 'a':
      case 'A':
        if (meta) {
          e.preventDefault()
          files.selectAll()
        }
        break
      case 'Enter':
      case ' ': {
        e.preventDefault()
        const cell = cells[from]
        if (cell?.kind === 'folder') files.openFolder(cell.folder.path)
        else if (cell?.kind === 'file') files.open(cell.file.id)
        break
      }
      case 'Escape':
        if (files.selection.length > 0) {
          e.preventDefault()
          clearAll()
        }
        break
      case 'F2': {
        e.preventDefault()
        if (cells[from]) renaming = cells[from].key
        break
      }
      case 'Delete':
      case 'Backspace': {
        e.preventDefault()
        const ids = [...files.selection]
        if (ids.length > 0) {
          if (files.view === 'trash') files.deleteForever(ids)
          else if (files.view === 'missing') files.forgetMissing(ids)
          else void files.trash(ids)
        } else if (e.key === 'Backspace' && browsing) files.up()
        break
      }
    }
  }

  // ── Mouse ──────────────────────────────────────────────────────────

  function clickFile(e: MouseEvent, cell: Extract<Cell, { kind: 'file' }>) {
    cursor = cell.key
    pick(e, cell.file.id)
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

  function menuPane(e: MouseEvent) {
    if ((e.target as Element).closest('.line')) return
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
    if (!(e.target as Element).closest('.line')) clearAll()
  }

  let over = $state<string | null>(null)

  // ── Rename in place ────────────────────────────────────────────────

  let renaming = $state<string | null>(null)

  function commit(cell: Cell, input: HTMLInputElement) {
    // Enter removes the field, which blurs it: the second call finds nothing
    // being renamed and does nothing.
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

  // ── Words ──────────────────────────────────────────────────────────

  /** The quiet line under a file's name. */
  function line(file: FileEntry): string {
    if (file.caption) return file.caption
    if (browsing) {
      const m = file.media
      const bits = [KIND_LABELS[file.kind]]
      if (m.width && m.height) bits.push(`${m.width} × ${m.height}`)
      if (m.durationMs) bits.push(formatDuration(m.durationMs))
      return bits.join(' · ')
    }
    if (file.storage.kind === 'sealed') return 'Encrypted, kept in the vault'
    const at = file.storage.path.lastIndexOf('/')
    return at < 0 ? 'The top of the files folder' : file.storage.path.slice(0, at)
  }

  // ── Sorting ────────────────────────────────────────────────────────

  /** The head of a column that sorts the list. Recent and Photos keep their
   *  own order whatever is chosen, so there the heads only say what is. */
  const fixed = $derived(files.view === 'recent' || files.view === 'photos')

  function sortBy(sort: FileSort) {
    if (fixed) return
    files.setSort(
      sort,
      files.sort === sort ? !files.descending : sort === 'when' || sort === 'size',
    )
  }

  function sorted(sort: FileSort): 'ascending' | 'descending' | 'none' {
    if (fixed || files.sort !== sort) return 'none'
    return files.descending ? 'descending' : 'ascending'
  }
</script>

<div class="list" bind:clientWidth={width}>
  <div class="head" class:narrow={!showWhen} class:slim={!showSize}>
    <span></span>
    <button class="col" class:on={sorted('name') !== 'none'} onclick={() => sortBy('name')}>
      Name
      {#if sorted('name') !== 'none'}
        <span class="dir" class:down={files.descending}><Icon name="chevron" size={11} /></span>
      {/if}
    </button>
    {#if showWhen}
      <button class="col" class:on={sorted('when') !== 'none'} onclick={() => sortBy('when')}>
        Date
        {#if sorted('when') !== 'none'}
          <span class="dir" class:down={files.descending}><Icon name="chevron" size={11} /></span>
        {/if}
      </button>
    {/if}
    {#if showSize}
      <button class="col end" class:on={sorted('size') !== 'none'} onclick={() => sortBy('size')}>
        Size
        {#if sorted('size') !== 'none'}
          <span class="dir" class:down={files.descending}><Icon name="chevron" size={11} /></span>
        {/if}
      </button>
    {/if}
    <span></span>
  </div>

  <div
    class="rows scroll"
    bind:this={scroller}
    role="listbox"
    aria-multiselectable="true"
    aria-label="Files"
    tabindex="0"
    {onkeydown}
    onclick={clickPane}
    oncontextmenu={menuPane}
  >
    <VirtualList
      items={cells}
      getKey={(c: Cell) => c.key}
      selectedId={cursor}
      itemSize={ROW}
      onEndReached={() => {
        if (files.hasMore) void files.loadMore()
      }}
    >
      {#snippet children(cell: Cell)}
        {#if cell.kind === 'folder'}
          {@const folder = cell.folder}
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_interactive_supports_focus -->
          <div
            class="line list-row"
            class:drop={over === cell.key}
            class:cursor={cursor === cell.key}
            class:narrow={!showWhen}
            class:slim={!showSize}
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
            <span class="pic folder-icon"><Icon name="folder" size={20} /></span>
            <span class="main">
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
            {#if showWhen}
              <span class="when">{folder.modifiedAt ? rowDate(folder.modifiedAt) : ''}</span>
            {/if}
            {#if showSize}<span class="size"></span>{/if}
            <span class="marks"></span>
          </div>
        {:else if cell.kind === 'file'}
          {@const file = cell.file}
          {@const missing = file.status.state === 'missing'}
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_interactive_supports_focus -->
          <div
            class="line list-row"
            class:sel={picked.has(file.id)}
            class:cursor={cursor === cell.key}
            class:gone={missing}
            class:narrow={!showWhen}
            class:slim={!showSize}
            role="option"
            aria-selected={picked.has(file.id)}
            draggable={!trashed && renaming !== cell.key}
            onclick={(e) => clickFile(e, cell)}
            ondblclick={() => files.open(file.id)}
            oncontextmenu={(e) => menuFile(e, cell)}
            ondragstart={(e) => startDrag(e, file)}
          >
            <span class="pic"><FileThumb {file} size={36} /></span>
            <span class="main">
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
              <span class="sub">{line(file)}</span>
            </span>
            {#if showWhen}<span class="when">{rowDate(fileWhen(file))}</span>{/if}
            {#if showSize}<span class="size">{formatSize(file.size)}</span>{/if}
            <span class="marks">
              {#if missing}
                <span class="mark bad" title="Missing from the folder"
                  ><Icon name="alert" size={14} /></span
                >
              {/if}
              {#if file.favourite}
                <span class="mark star" title="Favourite"
                  ><Icon name="star" size={13} filled /></span
                >
              {/if}
              {#if file.rating}
                <span class="mark" title="{file.rating} of 5">{file.rating}★</span>
              {/if}
              {#if file.storage.kind === 'sealed'}
                <span class="mark" title="Encrypted"><Icon name="lock" size={13} /></span>
              {/if}
              {#if file.stack}
                <span class="mark" title="In a stack"><Icon name="layers" size={13} /></span>
              {/if}
            </span>
          </div>
        {/if}
      {/snippet}
    </VirtualList>
  </div>
</div>

<style>
  .list {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  /* The column heads, on the grid the rows use so they line up. */
  .head,
  .line {
    display: grid;
    grid-template-columns: 36px minmax(0, 1fr) 7.5em 5.5em 6.5em;
    align-items: center;
    column-gap: var(--sp-3);
  }
  .head.narrow,
  .line.narrow {
    grid-template-columns: 36px minmax(0, 1fr) 5.5em 6.5em;
  }
  .head.slim,
  .line.slim {
    grid-template-columns: 36px minmax(0, 1fr) 6.5em;
  }
  .head.narrow.slim,
  .line.narrow.slim {
    grid-template-columns: 36px minmax(0, 1fr) 6.5em;
  }
  .head {
    flex: none;
    height: 30px;
    margin: 0 var(--sp-2);
    padding: 0 var(--sp-3);
    border-bottom: 1px solid var(--border);
  }
  .col {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--fg-faint);
    text-align: left;
  }
  .col.end {
    justify-content: flex-end;
  }
  .col:hover {
    color: var(--fg);
  }
  .col.on {
    color: var(--fg-muted);
  }
  .dir {
    display: flex;
    rotate: -90deg;
  }
  .dir.down {
    rotate: 90deg;
  }

  .rows {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: var(--sp-1) 0 var(--sp-6);
    outline: none;
    user-select: none;
  }

  .line {
    height: 52px;
    padding: 0 var(--sp-3);
    cursor: default;
  }
  .line.cursor:not(.sel) {
    background: var(--bg-hover);
  }
  .line.drop {
    background: var(--bg-selected);
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .line.gone .pic,
  .line.gone .main {
    opacity: 0.55;
  }

  .pic {
    display: flex;
    width: 36px;
    height: 36px;
    border-radius: var(--radius-sm);
    overflow: hidden;
  }
  .pic.folder-icon {
    align-items: center;
    justify-content: center;
    color: var(--app, var(--accent));
  }
  .main {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-base);
    font-weight: 560;
    color: var(--fg);
  }
  .line.sel .name {
    color: var(--accent);
  }
  .sub {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .when,
  .size {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .size {
    text-align: right;
  }
  .marks {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--sp-1);
    min-width: 0;
  }
  .mark {
    display: inline-flex;
    align-items: center;
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--fg-subtle);
  }
  .mark.star {
    color: var(--star);
  }
  .mark.bad {
    color: var(--danger);
  }

  .rename {
    width: 100%;
    min-width: 0;
    height: 26px;
    padding: 0 var(--sp-1);
    border: 1px solid var(--accent);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-base);
    user-select: text;
  }
  .rename:focus {
    outline: none;
  }
</style>
