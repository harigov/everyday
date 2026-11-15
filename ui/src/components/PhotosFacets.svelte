<script lang="ts">
  // What the Photos timeline can be narrowed to: the years, people, places,
  // tags and cameras the catalog knows, each with how many pictures it has.
  // One at a time -- picking another replaces the first -- and "All photos"
  // puts the whole timeline back.
  //
  // Places are a list and not a map: there is no map in this application, and
  // a place's name is what somebody remembers it by anyway.

  import { files, type PhotoFacet } from '../lib/files.svelte'
  import { limitFacets } from '../lib/photos'
  import type { Facet } from '../lib/types'

  /** Rows a long list shows before "Show all". */
  const SHOWN = 8

  const SECTIONS: { kind: PhotoFacet['kind']; title: string; rows: () => Facet[] }[] = [
    { kind: 'year', title: 'Years', rows: () => files.facets?.years ?? [] },
    { kind: 'person', title: 'People', rows: () => files.facets?.people ?? [] },
    { kind: 'place', title: 'Places', rows: () => files.facets?.places ?? [] },
    { kind: 'tag', title: 'Tags', rows: () => files.facets?.tags ?? [] },
    { kind: 'camera', title: 'Cameras', rows: () => files.facets?.cameras ?? [] },
  ]

  /** Sections whose whole list is showing. */
  let open = $state<Record<string, boolean>>({})

  const active = $derived(files.facet)

  function on(kind: PhotoFacet['kind'], name: string): boolean {
    return active?.kind === kind && active.name === name
  }

  /** Pick a facet, or put the timeline back when it is the one already picked. */
  function pick(kind: PhotoFacet['kind'], name: string) {
    files.setFacet(on(kind, name) ? null : { kind, name })
  }
</script>

<nav class="facets side-nav scroll" aria-label="Narrow the photos">
  <button class="side-row" class:sel={!active} onclick={() => files.setFacet(null)}>
    <span class="side-text">All photos</span>
  </button>

  {#each SECTIONS as section (section.kind)}
    {@const rows = section.rows()}
    {#if rows.length > 0}
      {@const current = active?.kind === section.kind ? active.name : null}
      {@const shown = limitFacets(rows, SHOWN, current, !!open[section.kind])}
      <div class="side-head"><span class="eyebrow">{section.title}</span></div>
      {#each shown as row (row.name)}
        <button
          class="side-row"
          class:sel={on(section.kind, row.name)}
          aria-pressed={on(section.kind, row.name)}
          onclick={() => pick(section.kind, row.name)}
        >
          <span class="side-text">{row.name}</span>
          <span class="side-count">{row.count.toLocaleString()}</span>
        </button>
      {/each}
      {#if rows.length > SHOWN}
        <button class="more" onclick={() => (open[section.kind] = !open[section.kind])}>
          {open[section.kind] ? 'Show fewer' : `Show all ${rows.length}`}
        </button>
      {/if}
    {/if}
  {/each}

  {#if files.facets && SECTIONS.every((s) => s.rows().length === 0)}
    <p class="hint none">
      Years, people, places and tags appear here as pictures are added and named.
    </p>
  {/if}
</nav>

<style>
  /* A column in the pane beside the timeline, scrolling on its own. The
     sidebar's rows (`app.css`) are what its entries are drawn with. */
  .facets {
    flex: none;
    width: 208px;
    min-height: 0;
    padding-top: var(--sp-2);
    border-right: 1px solid var(--border);
  }
  .more {
    display: block;
    margin: 2px 0 0 var(--sp-3);
    padding: 2px var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-xs);
    font-weight: 550;
    color: var(--fg-subtle);
  }
  .more:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .none {
    padding: var(--sp-3);
  }
</style>
