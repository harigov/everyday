<script lang="ts">
  import { plural } from '../lib/format'
  import { FILTERS, coverRatio, library, type Filter } from '../lib/library.svelte'
  import type { ItemSort } from '../lib/types'
  import AddItem from './AddItem.svelte'
  import Cover from './Cover.svelte'
  import Icon from './Icon.svelte'
  import ItemCard from './ItemCard.svelte'
  import ItemDetail from './ItemDetail.svelte'
  import Rating from './Rating.svelte'

  // Loaded when the view appears rather than when the store is imported, so
  // a vault whose owner never opens this app never reads a shelf. `start` is
  // idempotent, so coming back refreshes instead of blanking the grid.
  void library.start()

  const SORTS: { id: ItemSort; label: string }[] = [
    { id: 'addedDesc', label: 'Recently added' },
    { id: 'titleAsc', label: 'Title' },
    { id: 'ratingDesc', label: 'Your rating' },
    { id: 'finishedDesc', label: 'Recently finished' },
    { id: 'yearDesc', label: 'Newest first' },
    { id: 'yearAsc', label: 'Oldest first' },
  ]

  const shelf = $derived(library.kind)
  const target = $derived(library.defaultShelf)
  const item = $derived(library.item)

  /** What a filter tab says, in the open shelf's own language. */
  function filterLabel(filter: Filter): string {
    if (filter === 'ahead') return 'To do'
    if (filter === 'all') return 'All'
    return library.label(shelf, filter)
  }

  const heading = $derived(library.favouritesOnly ? 'Favourites' : (shelf?.name ?? 'Everything'))
  // One frame for the whole grid; see `coverRatio` for why that beats one
  // frame per card. The everything view has no shelf, so it gets portrait.
  const ratio = $derived(coverRatio(library.favouritesOnly ? null : shelf))
</script>

<div class="library" style="--tint: {library.accent}">
  <div class="main">
    <header class="bar">
      <div class="titles">
        <h1>{heading}</h1>
        <span class="sub">
          {plural(library.items.length, 'item')}
          {#if library.loading}· loading…{/if}
        </span>
      </div>

      <div class="search">
        <Icon name="search" size={14} />
        <input
          type="search"
          placeholder="Search titles, people, notes…"
          value={library.query}
          oninput={(e) => library.setQuery(e.currentTarget.value)}
        />
      </div>

      <select
        class="sort"
        aria-label="Sort"
        value={library.sort}
        onchange={(e) => library.setSort(e.currentTarget.value as ItemSort)}
      >
        {#each SORTS as s (s.id)}
          <option value={s.id}>{s.label}</option>
        {/each}
      </select>

      <div class="views" role="group" aria-label="View">
        <button
          class="view"
          class:on={library.view === 'grid'}
          title="Covers"
          aria-label="Covers"
          onclick={() => library.setView('grid')}
        >
          <Icon name="grid" size={14} />
        </button>
        <button
          class="view"
          class:on={library.view === 'list'}
          title="List"
          aria-label="List"
          onclick={() => library.setView('list')}
        >
          <Icon name="list" size={14} />
        </button>
      </div>
    </header>

    <div class="tools">
      <div class="filters" role="tablist" aria-label="Status">
        {#each FILTERS as filter (filter)}
          <button
            class="filter"
            class:on={library.filter === filter}
            role="tab"
            aria-selected={library.filter === filter}
            onclick={() => library.setFilter(filter)}
          >
            {filterLabel(filter)}
          </button>
        {/each}
      </div>

      {#if target}
        <div class="capture"><AddItem kind={target} /></div>
      {/if}
    </div>

    {#if library.note}
      <p class="note">{library.note}</p>
    {/if}

    <div class="scroll body">
      {#if library.items.length === 0 && !library.loading}
        <div class="empty">
          {#if library.query.trim()}
            <p class="lead">Nothing matches “{library.query.trim()}”.</p>
            <p>Try fewer words, or switch the filter to <b>All</b>.</p>
          {:else if library.favouritesOnly}
            <p class="lead">No favourites yet.</p>
            <p>The star on a card puts it here.</p>
          {:else if target}
            <p class="lead">
              Nothing on this shelf {library.filter === 'all' ? 'yet' : 'under this filter'}.
            </p>
            <p>
              Type a title above and press Enter. With the
              <Icon name="sparkle" size={12} /> on, the cover and the details are looked up for you; with
              it off, exactly what you type is what you get.
            </p>
          {:else}
            <p class="lead">No shelves yet.</p>
            <p>Add one in the sidebar — books, films, restaurants, anything you keep a list of.</p>
          {/if}
        </div>
      {:else if library.view === 'grid'}
        <div class="grid">
          {#each library.items as row (row.id)}
            <ItemCard
              item={row}
              kind={library.kindOf(row)}
              {ratio}
              selected={library.selected === row.id}
              onopen={() => void library.open(row.id)}
            />
          {/each}
        </div>
      {:else}
        <div class="rows">
          {#each library.items as row (row.id)}
            {@const kind = library.kindOf(row)}
            <button
              class="row"
              class:sel={library.selected === row.id}
              onclick={() => void library.open(row.id)}
            >
              <span class="thumb">
                <Cover
                  blob={row.cover}
                  title={row.title}
                  icon={kind?.icon ?? ''}
                  color={kind?.color ?? 'var(--accent)'}
                  ratio="1 / 1"
                />
              </span>
              <span class="cell name">
                <span class="t">{row.title}</span>
                {#if row.creator}<span class="c">{row.creator}</span>{/if}
              </span>
              <span class="cell shelf">{kind?.name ?? ''}</span>
              <span class="cell state">{library.label(kind, row.status)}</span>
              <span class="cell year">{row.year ?? ''}</span>
              <span class="cell score">
                {#if row.rating !== null && row.rating !== undefined}
                  <Rating value={row.rating} size={12} readonly />
                {/if}
              </span>
            </button>
          {/each}
        </div>
      {/if}
    </div>
  </div>

  {#if item}
    <ItemDetail {item} kind={library.kindOf(item)} />
  {/if}
</div>

<style>
  .library {
    display: flex;
    flex: 1;
    min-width: 0;
    min-height: 0;
    background: var(--bg);
  }
  .main {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
    min-height: 0;
  }

  .bar {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    height: 46px;
    flex: none;
    padding: 0 var(--sp-4);
    border-bottom: 1px solid var(--border);
  }
  .titles {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    min-width: 0;
  }
  h1 {
    margin: 0;
    font-size: var(--text-md);
    font-weight: 650;
    letter-spacing: -0.01em;
    color: var(--fg);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .sub {
    font-size: var(--text-xs);
    color: var(--fg-faint);
    white-space: nowrap;
  }

  .search {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    flex: 1;
    max-width: 320px;
    margin-left: auto;
    height: 28px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
    color: var(--fg-faint);
  }
  .search input {
    flex: 1;
    min-width: 0;
    border: 0;
    background: none;
    font-size: var(--text-sm);
    color: var(--fg);
    user-select: text;
  }
  .search input:focus {
    outline: none;
  }

  .sort {
    height: 28px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }

  .views {
    display: flex;
    gap: 2px;
    padding: 2px;
    border-radius: var(--radius-sm);
    background: var(--bg-active);
  }
  .view {
    display: grid;
    place-items: center;
    width: 24px;
    height: 22px;
    border-radius: 3px;
    color: var(--fg-subtle);
  }
  .view.on {
    background: var(--bg-raised);
    color: var(--fg);
    box-shadow: var(--shadow-sm);
  }

  .tools {
    display: flex;
    align-items: center;
    gap: var(--sp-4);
    flex: none;
    padding: var(--sp-3) var(--sp-4);
  }
  .filters {
    display: flex;
    gap: 2px;
    flex-wrap: wrap;
  }
  .filter {
    height: 26px;
    padding: 0 var(--sp-3);
    border-radius: 999px;
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-subtle);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .filter:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .filter.on {
    background: color-mix(in oklab, var(--tint) 14%, transparent);
    color: color-mix(in oklab, var(--tint) 78%, var(--fg));
  }

  .capture {
    flex: 1;
    max-width: 380px;
    margin-left: auto;
  }

  .note {
    margin: 0 var(--sp-4) var(--sp-2);
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }

  .body {
    flex: 1;
    padding: var(--sp-1) var(--sp-4) var(--sp-10);
  }

  .grid {
    display: grid;
    /* Auto-fill rather than a fixed column count: a wide window should show
       more covers, not wider ones -- a stretched jacket is the one thing a
       cover grid must never do. */
    grid-template-columns: repeat(auto-fill, minmax(146px, 1fr));
    gap: var(--sp-5) var(--sp-4);
  }

  .rows {
    display: flex;
    flex-direction: column;
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    width: 100%;
    padding: var(--sp-2) var(--sp-2);
    border-radius: var(--radius-sm);
    border-bottom: 1px solid var(--border);
    text-align: left;
  }
  .row:hover {
    background: var(--bg-hover);
  }
  .row.sel {
    background: var(--bg-selected);
  }
  .thumb {
    display: block;
    width: 34px;
    flex: none;
  }
  .cell {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .name {
    display: flex;
    flex-direction: column;
    gap: 1px;
    flex: 1;
    min-width: 0;
  }
  .name .t {
    font-family: var(--font-read);
    font-size: var(--text-base);
    font-weight: 600;
    color: var(--fg);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .name .c {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .shelf {
    width: 100px;
    flex: none;
  }
  .state {
    width: 90px;
    flex: none;
  }
  .year {
    width: 48px;
    flex: none;
    font-variant-numeric: tabular-nums;
  }
  .score {
    width: 82px;
    flex: none;
  }

  .empty {
    max-width: 34rem;
    margin: var(--sp-16) auto;
    text-align: center;
    color: var(--fg-subtle);
  }
  .empty p {
    margin: 0 0 var(--sp-2);
    font-size: var(--text-base);
    line-height: var(--leading-normal);
  }
  .empty .lead {
    font-family: var(--font-read);
    font-size: var(--text-lg);
    color: var(--fg-muted);
  }
</style>
