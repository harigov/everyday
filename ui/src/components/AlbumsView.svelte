<script lang="ts">
  // The Albums pane. With no album open it is a grid of them -- a cover, a
  // name and how many it holds -- and a card to start another. With one open
  // it is that album: its name, which is renamed where it stands, a line
  // about it, and its pictures in the same timeline Photos draws.
  //
  // The Files toolbar above supplies the title, the search (which narrows the
  // grid of albums by name), and the selection's favourite, rate, tag, album
  // and trash. Deleting an album asks first, through the store, and takes the
  // album alone: the pictures stay where they are.

  import { api } from '../lib/api'
  import { handle } from '../lib/errors'
  import { files } from '../lib/files.svelte'
  import { filesUi, newAlbumAsk } from '../lib/files-ui.svelte'
  import { plural } from '../lib/format'
  import { menu } from '../lib/menu.svelte'
  import { phone } from '../lib/phone.svelte'
  import { remembered, type Zoom } from '../lib/photos'
  import type { Album, AlbumView } from '../lib/types'
  import EmptyState from './EmptyState.svelte'
  import FileThumb from './FileThumb.svelte'
  import Icon from './Icon.svelte'
  import Loading from './Loading.svelte'
  import PhotosBar from './PhotosBar.svelte'
  import PhotosTimeline from './PhotosTimeline.svelte'

  /** A cover's edge in the grid of albums. */
  const COVER = 188

  const open = $derived(
    files.album !== null ? files.albums.find((v) => v.album.id === files.album) : undefined,
  )
  const album = $derived(open?.album)
  const shown = $derived(files.visibleAlbums)
  /** A different list of pictures -- another album, another search or order --
   *  starts at the top. */
  const resetKey = $derived(
    [files.album, files.search.trim(), files.sort, files.descending].join('\u0000'),
  )

  let zoom = $state(remembered.zoom)
  function setZoom(next: Zoom) {
    zoom = next
    remembered.zoom = next
  }

  function newAlbum() {
    filesUi.askName(newAlbumAsk([]))
  }

  function cardMenu(e: MouseEvent, view: AlbumView) {
    menu.show(e, [
      { label: 'Open', icon: 'eye', run: () => files.openAlbum(view.album.id) },
      {
        label: 'Delete album…',
        icon: 'trash',
        danger: true,
        run: () => files.deleteAlbum(view.album),
      },
    ])
  }

  // ---- renaming and describing, where they stand ----------------------------

  let editing = $state<'name' | 'description' | null>(null)
  let draft = $state('')

  function edit(field: 'name' | 'description') {
    if (!album) return
    draft = field === 'name' ? album.name : album.description
    editing = field
  }

  /** Save what was typed. `editing` is cleared first, so the blur that follows
   *  an Enter, or the removal of the field, finds nothing left to do. */
  async function commit() {
    const field = editing
    editing = null
    if (!field || !album) return
    if (field === 'name') await files.renameAlbum(album, draft)
    else await describe(album, draft)
  }

  async function describe(target: Album, text: string) {
    const description = text.trim()
    if (description === target.description) return
    try {
      await api.saveAlbum({ ...$state.snapshot(target), description })
      await files.reload()
    } catch (e) {
      await handle(e)
    }
  }

  /** Give a field the keyboard, its text selected, as it appears. */
  function focusAndSelect(node: HTMLInputElement | HTMLTextAreaElement) {
    const frame = requestAnimationFrame(() => {
      node.focus()
      node.select()
    })
    return { destroy: () => cancelAnimationFrame(frame) }
  }
</script>

<div class="albums" style="--tint: {files.accent}">
  {#if files.album === null}
    <div class="grid-pane scroll">
      {#if files.albums.length === 0 && files.loaded}
        <EmptyState lead="No albums yet.">
          {#snippet icon()}<Icon name="image" size={28} weight={1.2} />{/snippet}
          {#snippet note()}
            An album gathers pictures from anywhere in your files, without moving them. Start one,
            then add to it from Photos.
          {/snippet}
          {#snippet action()}
            <button class="btn btn-primary" onclick={newAlbum}>
              <Icon name="plus" size={14} /> New album
            </button>
          {/snippet}
        </EmptyState>
      {:else}
        <div class="grid">
          <button class="card start" onclick={newAlbum}>
            <span class="cover blank"><Icon name="plus" size={26} weight={1.3} /></span>
            <span class="name">New album</span>
          </button>
          {#each shown as view (view.album.id)}
            <button
              class="card"
              title={view.album.description || undefined}
              onclick={() => files.openAlbum(view.album.id)}
              oncontextmenu={(e) => cardMenu(e, view)}
            >
              <span class="cover">
                {#if view.cover}
                  <FileThumb file={view.cover} size={phone.on ? undefined : COVER} fit="cover" />
                {:else}
                  <span class="blank"><Icon name="image" size={26} weight={1.3} /></span>
                {/if}
              </span>
              <span class="name">{view.album.name}</span>
              <span class="count">{plural(view.count, 'item')}</span>
            </button>
          {/each}
        </div>
        {#if shown.length === 0 && files.search.trim()}
          <p class="hint none">No album has “{files.search.trim()}” in its name.</p>
        {/if}
      {/if}
    </div>
  {:else}
    <header class="head">
      <button class="back" onclick={() => files.openAlbum(null)} aria-label="All albums">
        <span class="turn"><Icon name="chevron" size={14} /></span>
        Albums
      </button>

      <div class="titles">
        {#if editing === 'name'}
          <input
            class="field name-field"
            use:focusAndSelect
            bind:value={draft}
            aria-label="Album name"
            autocomplete="off"
            spellcheck="false"
            onblur={() => void commit()}
            onkeydown={(e) => {
              if (e.key === 'Enter') void commit()
              else if (e.key === 'Escape') {
                e.stopPropagation()
                editing = null
              }
            }}
          />
        {:else}
          <h2 class="pane-title">
            <button class="rename" title="Rename" disabled={!album} onclick={() => edit('name')}>
              {album?.name ?? 'Album'}
              <Icon name="pencil" size={13} />
            </button>
          </h2>
        {/if}

        {#if editing === 'description'}
          <textarea
            class="field about-field"
            rows="2"
            use:focusAndSelect
            bind:value={draft}
            aria-label="About this album"
            placeholder="What this album is"
            onblur={() => void commit()}
            onkeydown={(e) => {
              if (e.key === 'Enter' && !e.shiftKey) {
                e.preventDefault()
                void commit()
              } else if (e.key === 'Escape') {
                e.stopPropagation()
                editing = null
              }
            }}
          ></textarea>
        {:else if album}
          <button class="about" class:none={!album.description} onclick={() => edit('description')}>
            {album.description || 'Add a line about it'}
          </button>
        {/if}
      </div>

      <span class="pane-sub">{open ? plural(open.count, 'item') : ''}</span>
      {#if album}
        <button class="btn btn-sm btn-ghost-danger" onclick={() => files.deleteAlbum(album)}>
          <Icon name="trash" size={14} />
          Delete album…
        </button>
      {/if}
    </header>

    <PhotosBar {zoom} onzoom={setZoom} />

    <PhotosTimeline
      items={files.visibleFiles}
      hasMore={files.hasMore}
      onend={() => void files.loadMore()}
      headings={files.sort === 'when'}
      {zoom}
      {resetKey}
    >
      {#snippet empty()}
        {#if files.page === null}
          <Loading label="Loading the album…" centred />
        {:else if files.search.trim()}
          <EmptyState lead="No pictures here match.">
            {#snippet action()}
              <button class="btn" onclick={() => files.setSearch('')}>Clear the search</button>
            {/snippet}
          </EmptyState>
        {:else}
          <EmptyState lead="This album is empty.">
            {#snippet note()}
              Select pictures in Photos and choose Album in the toolbar, or press A on a picture.
            {/snippet}
            {#snippet action()}
              <button class="btn" onclick={() => files.setView('photos')}>Go to Photos</button>
            {/snippet}
          </EmptyState>
        {/if}
      {/snippet}
    </PhotosTimeline>
  {/if}
</div>

<style>
  /* No background of its own: it stands on the raised surface the shell lays
     under every app. A column, so the album's timeline gets a definite height. */
  .albums {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
    min-height: 0;
  }

  /* ── the grid of albums ──────────────────────────────────────────────── */
  .grid-pane {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
    padding: var(--sp-3) var(--sp-4) var(--fab-clear);
  }
  .grid {
    display: grid;
    /* Fixed-width cards, so a wide window shows more albums and never a
       stretched cover. */
    grid-template-columns: repeat(auto-fill, 188px);
    gap: var(--sp-5) var(--sp-4);
    justify-content: start;
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 2px;
    width: 188px;
    text-align: left;
  }
  .cover {
    display: grid;
    place-items: center;
    width: 188px;
    height: 188px;
    margin-bottom: var(--sp-2);
    border-radius: var(--radius-lg);
    overflow: hidden;
    background: var(--bg-sunken);
    color: var(--fg-faint);
    transition: background var(--fast) var(--ease);
  }
  .blank {
    display: grid;
    place-items: center;
  }
  .card:hover .cover {
    background: var(--bg-active);
  }
  .card.start .cover {
    border: 1.5px dashed var(--border-strong);
  }
  .card.start:hover .cover {
    color: var(--accent);
    border-color: var(--accent);
  }
  .name {
    font-size: var(--text-base);
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .card.start .name {
    color: var(--fg-muted);
    font-weight: 550;
  }
  .count {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .none {
    margin-top: var(--sp-6);
  }

  /* ── an open album ──────────────────────────────────────────────────── */
  .head {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    flex: none;
    padding: var(--sp-3) var(--sp-4) var(--sp-1);
  }
  .back {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-1);
    height: 28px;
    padding: 0 var(--sp-3) 0 var(--sp-2);
    border-radius: var(--radius-pill);
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-subtle);
  }
  .back:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  /* The chevron points right everywhere else; here it points the way back. */
  .turn {
    display: flex;
    transform: scaleX(-1);
  }
  .titles {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1;
    min-width: 0;
  }
  .rename {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    max-width: 100%;
    text-align: left;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .rename :global(svg) {
    flex: none;
    color: var(--fg-faint);
    opacity: 0;
    transition: opacity var(--fast) var(--ease);
  }
  .rename:hover :global(svg),
  .rename:focus-visible :global(svg) {
    opacity: 1;
  }
  .name-field {
    height: 34px;
    max-width: 420px;
    font-size: var(--text-lg);
    font-weight: 680;
  }
  .about {
    max-width: 60ch;
    text-align: left;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    line-height: var(--leading-snug);
  }
  .about.none {
    color: var(--fg-faint);
  }
  .about:hover {
    color: var(--fg);
  }
  .about-field {
    max-width: 60ch;
    font-size: var(--text-sm);
  }
  .head .pane-sub {
    flex: none;
    align-self: center;
  }

  /* ── A phone ──────────────────────────────────────────────────────────
     Two albums across, each cover as wide as its column and square; the
     open album's head wraps -- the way back and the count and the delete on
     one row, the name and its line under them -- and the pencil that a
     pointer finds on hover is always there. */
  :global([data-phone]) .grid-pane {
    padding: var(--sp-2) var(--sp-3) var(--fab-clear);
  }
  :global([data-phone]) .grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--sp-4) var(--sp-3);
  }
  :global([data-phone]) .card {
    width: auto;
    min-width: 0;
  }
  :global([data-phone]) .cover {
    width: 100%;
    height: auto;
    aspect-ratio: 1 / 1;
  }
  :global([data-phone]) .head {
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-1) var(--sp-2);
    padding: var(--sp-1) var(--sp-3) 0;
  }
  :global([data-phone]) .back {
    height: 44px;
    margin-left: calc(var(--sp-2) * -1);
    padding: 0 var(--sp-3) 0 var(--sp-2);
  }
  :global([data-phone]) .head .pane-sub {
    flex: 1;
  }
  :global([data-phone]) .titles {
    order: 3;
    flex: 1 1 100%;
  }
  :global([data-phone]) .rename {
    min-height: 40px;
  }
  :global([data-phone]) .rename :global(svg) {
    opacity: 0.6;
  }
  :global([data-phone]) .about {
    min-height: 36px;
  }
  :global([data-phone]) .name-field {
    max-width: none;
  }
  :global([data-phone]) .about-field {
    max-width: none;
  }
</style>
