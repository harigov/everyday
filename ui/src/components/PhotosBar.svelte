<script lang="ts">
  // The slim row between the Files toolbar and the pictures: what that toolbar
  // has no place for. The size of the tiles, the button that shows or hides
  // the facets, the facet the timeline is narrowed to (with the way out of
  // it), and -- while anything is selected -- the few things only a picture
  // can be: hidden from the timeline, stacked under another, and in an open
  // album, its cover or taken out of it. Favourite, rate, tag, album, move
  // and trash are the toolbar's own selection bar, above.

  import { files, type PhotoFacet } from '../lib/files.svelte'
  import { ZOOMS, stackSize, type Zoom } from '../lib/photos'
  import Icon from './Icon.svelte'

  let {
    zoom,
    onzoom,
    facetsOpen = false,
    ontoggleFacets,
  }: {
    zoom: Zoom
    onzoom: (zoom: Zoom) => void
    /** The facet column is showing. */
    facetsOpen?: boolean
    /** Show or hide it; without this there is no such button (an album has no facets). */
    ontoggleFacets?: () => void
  } = $props()

  const FACET_KIND: Record<PhotoFacet['kind'], string> = {
    year: 'Year',
    person: 'Person',
    place: 'Place',
    tag: 'Tag',
    camera: 'Camera',
  }

  const ids = $derived(files.selection)
  const picked = $derived(files.selectedFiles)
  const allHidden = $derived(picked.length > 0 && picked.every((f) => f.hidden))
  const stacked = $derived(picked.some((f) => f.stack || stackSize(f, files.page?.stackCounts) > 0))
  /** The album open, when this is the Albums view with one open. */
  const album = $derived(
    files.view === 'albums' && files.album !== null
      ? (files.albums.find((v) => v.album.id === files.album)?.album ?? null)
      : null,
  )
</script>

<div class="strip">
  {#if ontoggleFacets}
    <button
      class="btn btn-sm"
      class:on={facetsOpen}
      aria-pressed={facetsOpen}
      title={facetsOpen ? 'Hide the list of years and places' : 'Browse by year, person or place'}
      onclick={ontoggleFacets}
    >
      <Icon name="filter" size={14} />
      <span>Browse by</span>
    </button>
  {/if}
  {#if files.facet}
    {@const facet = files.facet}
    <button class="narrowed" title="Show every photo again" onclick={() => files.setFacet(null)}>
      <span class="kind">{FACET_KIND[facet.kind]}</span>
      {facet.name}
      <Icon name="close" size={12} />
    </button>
  {/if}

  <span class="grow"></span>

  {#if ids.length > 0}
    <div class="acts" role="toolbar" aria-label="For the selected pictures">
      <button
        class="btn btn-sm"
        aria-pressed={allHidden}
        title={allHidden ? 'Show in the timeline again' : 'Leave out of the timeline'}
        onclick={() => void files.update(ids, { hidden: !allHidden })}
      >
        <Icon name="hidden" size={14} />
        <span>{allHidden ? 'Unhide' : 'Hide'}</span>
      </button>
      {#if ids.length > 1}
        <button
          class="btn btn-sm"
          title="Put the others under the first selected, as one stack"
          onclick={() => void files.stack(ids[0]!, ids.slice(1))}
        >
          <Icon name="layers" size={14} />
          <span>Stack</span>
        </button>
      {/if}
      {#if stacked}
        <button
          class="btn btn-sm"
          title="Take out of their stacks"
          onclick={() => void files.unstack(ids)}
        >
          <Icon name="layers" size={14} />
          <span>Unstack</span>
        </button>
      {/if}
      {#if album}
        <button
          class="btn btn-sm"
          disabled={ids.length !== 1}
          title={ids.length === 1 ? 'Use this picture as the album’s cover' : 'Select one picture'}
          onclick={() => void files.setAlbumCover(album, ids[0]!)}
        >
          <Icon name="pin" size={14} />
          <span>Set as cover</span>
        </button>
        <button
          class="btn btn-sm"
          title="Take out of this album; the pictures themselves stay"
          onclick={() => void files.removeFromAlbum(album.id, ids)}
        >
          <Icon name="minus" size={14} />
          <span>Remove from album</span>
        </button>
      {/if}
    </div>
  {/if}

  <div class="filters" role="group" aria-label="Size of the pictures">
    {#each ZOOMS as z (z.id)}
      <button
        class="filter"
        class:on={zoom === z.id}
        aria-pressed={zoom === z.id}
        onclick={() => onzoom(z.id)}
      >
        {z.label}
      </button>
    {/each}
  </div>
</div>

<style>
  .strip {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    flex: none;
    padding: var(--sp-2) var(--sp-4);
    /* A narrow pane drops the words from the buttons (below), not the buttons. */
    container-type: inline-size;
  }
  .grow {
    flex: 1;
  }
  .acts {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
  }
  .btn.on {
    background: var(--bg-selected);
    color: var(--accent);
  }
  .btn:disabled {
    opacity: 0.45;
    pointer-events: none;
  }

  /* The facet the timeline is narrowed to, as a chip with a cross. */
  .narrowed {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    height: 28px;
    padding: 0 var(--sp-3);
    border-radius: 999px;
    background: var(--bg-selected);
    color: var(--accent);
    font-size: var(--text-sm);
    font-weight: 600;
    white-space: nowrap;
  }
  .narrowed:hover {
    background: color-mix(in oklab, var(--accent) 22%, transparent);
  }
  .kind {
    font-weight: 500;
    opacity: 0.7;
  }

  @container (max-width: 760px) {
    .btn span {
      display: none;
    }
  }

  /* ── A phone ──────────────────────────────────────────────────────────
     Wraps rather than squeezes: the facets button and the sizes on one row,
     what the selection can be given and the facet it is narrowed to under
     them, and every button says what it does. */
  :global([data-phone]) .strip {
    flex-wrap: wrap;
    padding: var(--sp-1) var(--sp-3) var(--sp-2);
  }
  :global([data-phone]) .strip .btn span {
    display: inline;
  }
  :global([data-phone]) .strip .grow {
    display: none;
  }
  :global([data-phone]) .acts {
    order: 3;
    flex: 1 1 100%;
    flex-wrap: wrap;
  }
  :global([data-phone]) .filters {
    margin-left: auto;
  }
  :global([data-phone]) .narrowed {
    height: 40px;
    padding: 0 var(--sp-3) 0 var(--sp-4);
  }
</style>
