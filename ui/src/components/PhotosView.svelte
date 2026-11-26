<script lang="ts">
  // The Photos pane: every picture and film, in the order they were taken,
  // under month and day headings; the years, people, places, tags and cameras
  // down the side to narrow it to.
  //
  // The Files toolbar above it supplies the title, the search, the favourite,
  // rate, tag, album and trash of a selection, and the way to add. This is
  // what is under it: the facet column (`PhotosFacets`), a slim bar for what
  // only photos need (`PhotosBar`), and the timeline (`PhotosTimeline`).
  //
  // The store lists Photos itself -- images and films only, a stack's top
  // alone, newest first -- and pages it; this asks for the next page as the
  // end of what is loaded comes into view.

  import { focusOnMount, trapFocus } from '../lib/focus'
  import { files } from '../lib/files.svelte'
  import { closesOnBack, phone } from '../lib/phone.svelte'
  import { remembered, type Zoom } from '../lib/photos'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import Loading from './Loading.svelte'
  import PhotosBar from './PhotosBar.svelte'
  import PhotosFacets from './PhotosFacets.svelte'
  import PhotosTimeline from './PhotosTimeline.svelte'

  /** The narrowest pane that has room for the facet column beside the tiles. */
  const ROOM_FOR_FACETS = 640

  let zoom = $state(remembered.zoom)
  let facetsOpen = $state(remembered.facets)
  let paneWidth = $state(0)

  function setZoom(next: Zoom) {
    zoom = next
    remembered.zoom = next
  }

  function toggleFacets() {
    facetsOpen = !facetsOpen
    remembered.facets = facetsOpen
  }

  // A phone has no room for the column: the years, people and places are a
  // sheet, shut until asked for, that back puts away and a choice closes.
  let sheet = $state(false)
  closesOnBack(
    () => sheet && phone.on,
    () => (sheet = false),
  )
  $effect(() => {
    void files.facet
    sheet = false
  })

  const narrowed = $derived(!!files.facet || !!files.search.trim())
  /** A change of any of these is a different list: back to the top. */
  const resetKey = $derived(
    [files.facet?.kind, files.facet?.name, files.search.trim(), files.showHidden].join('\u0000'),
  )
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape' && sheet && phone.on) {
      e.preventDefault()
      sheet = false
    }
  }}
/>

<!-- Measured here, not asked of a container query: a `container-type` would
     make this box the containing block of the viewer and the album popover,
     which are `position: fixed` and belong to the window. -->
<div class="photos" style="--tint: {files.accent}" bind:clientWidth={paneWidth}>
  {#if facetsOpen && paneWidth >= ROOM_FOR_FACETS && !phone.on}
    <div class="side"><PhotosFacets /></div>
  {/if}

  <div class="main">
    <PhotosBar
      {zoom}
      onzoom={setZoom}
      facetsOpen={phone.on ? sheet : facetsOpen}
      ontoggleFacets={phone.on ? () => (sheet = !sheet) : toggleFacets}
    />

    <PhotosTimeline
      items={files.visibleFiles}
      hasMore={files.hasMore}
      onend={() => void files.loadMore()}
      {zoom}
      {resetKey}
    >
      {#snippet empty()}
        {#if files.page === null}
          <Loading label="Loading the photos…" centred />
        {:else if narrowed}
          <EmptyState lead="No pictures match.">
            {#snippet note()}
              {#if files.facet}
                Nothing is filed under that.
              {:else}
                Nothing was found for “{files.search.trim()}”.
              {/if}
              Hidden pictures stay out of the timeline unless the eye in the toolbar is on.
            {/snippet}
            {#snippet action()}
              <button
                class="btn"
                onclick={() => {
                  files.setFacet(null)
                  files.setSearch('')
                }}
              >
                Show every photo
              </button>
            {/snippet}
          </EmptyState>
        {:else}
          <EmptyState lead="No photos yet.">
            {#snippet icon()}<Icon name="image" size={28} weight={1.2} />{/snippet}
            {#snippet note()}
              Pictures and films you add, or that appear in your files folder, are laid out here by
              the day they were taken.
            {/snippet}
            {#snippet action()}
              <button class="btn btn-primary" onclick={() => files.create()}>Add photos</button>
            {/snippet}
          </EmptyState>
        {/if}
      {/snippet}
    </PhotosTimeline>
  </div>
</div>

{#if sheet && phone.on}
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div class="scrim" onclick={() => (sheet = false)}></div>
  <div class="sheet browse" role="dialog" aria-modal="true" aria-label="Browse by" use:trapFocus>
    <div class="browse-head">
      <h2>Browse by</h2>
      <button class="btn btn-sm" use:focusOnMount onclick={() => (sheet = false)}>Done</button>
    </div>
    <div class="browse-list"><PhotosFacets /></div>
  </div>
{/if}

<style>
  /* No background of its own: it stands on the raised surface the shell lays
     under every app. A row, so the facet column and the timeline each get a
     definite height from it. */
  .photos {
    display: flex;
    flex: 1;
    min-width: 0;
    min-height: 0;
  }
  .side {
    display: flex;
    flex: none;
    min-height: 0;
  }
  .main {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
    min-height: 0;
  }

  /* ── A phone ──────────────────────────────────────────────────────────
     The facets as a sheet: a heading and Done over the list, which scrolls
     inside the sheet and not with it. */
  .browse {
    display: flex;
    flex-direction: column;
    padding: var(--sp-3) var(--sp-3) var(--sp-2);
  }
  .browse-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex: none;
    padding: 0 var(--sp-1) var(--sp-2);
  }
  .browse-head h2 {
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
  }
  .browse-list {
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow-y: auto;
  }
</style>
