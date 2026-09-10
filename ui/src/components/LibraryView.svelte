<script lang="ts">
  // The library app's main pane: the capture line, the filter chips, and the
  // shelf itself as covers or as a list.
  //
  // Covers by default, and that is the editorial point of the app. A shelf
  // people recognise at a glance is one they browse; a table of titles is one
  // they only ever search. The list is one keystroke away for when a shelf has
  // grown past the point where pictures help.

  import { article, plural } from '../lib/format'
  import { FILTERS, coverRatio, library, type Filter } from '../lib/library.svelte'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { itemMenu } from '../lib/menus'
  import type { Item, ItemSort } from '../lib/types'
  import AddItem from './AddItem.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Cover from './Cover.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import ItemCard from './ItemCard.svelte'
  import { rovingFocus } from '../lib/roving'
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

  let pendingDelete = $state<Item | null>(null)

  /** A card or a row: the same menu, built in one place. */
  function rowMenu(row: Item): MenuItem[] {
    return itemMenu(row, { onDelete: () => (pendingDelete = row) })
  }

  /**
   * The grid itself, where there is no card under the pointer.
   *
   * The same three controls the bar above carries, within reach of where the
   * pointer already is rather than at the top of a long shelf.
   */
  function shelfMenu(): MenuItem[] {
    return tidyMenu([
      target && {
        label: `Add ${article(target.singular)} ${target.singular.toLowerCase()}`,
        icon: 'plus',
        hint: 'Ctrl+N',
        run: () => library.focusCapture(),
      },
      SEP,
      {
        label: 'Show',
        icon: 'grid',
        items: [
          { label: 'Covers', checked: library.view === 'grid', run: () => library.setView('grid') },
          { label: 'List', checked: library.view === 'list', run: () => library.setView('list') },
        ],
      },
      {
        label: 'Sort by',
        icon: 'layers',
        items: SORTS.map((s) => ({
          label: s.label,
          checked: library.sort === s.id,
          run: () => library.setSort(s.id),
        })),
      },
      {
        label: 'Status',
        icon: 'circle',
        items: FILTERS.map((f) => ({
          label: filterLabel(f),
          checked: library.filter === f,
          run: () => library.setFilter(f),
        })),
      },
      {
        label: 'Favourites only',
        icon: 'star',
        checked: library.favouritesOnly,
        run: () => library.setFavouritesOnly(!library.favouritesOnly),
      },
      library.narrowed && {
        label: 'Clear the filters',
        icon: 'close',
        run: () => library.clearFilters(),
      },
    ])
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
          data-search
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

    <!-- The chips centred on the pane, with the capture line pushed to the
         right and a matching spacer on the left holding it there. See
         `.toolbar` in `app.css`: this is the shape every app uses. -->
    <div class="toolbar">
      <div class="toolbar-end">
        <button
          class="star"
          class:on={library.favouritesOnly}
          aria-pressed={library.favouritesOnly}
          title={library.favouritesOnly ? 'Show everything again' : 'Favourites only'}
          onclick={() => library.setFavouritesOnly(!library.favouritesOnly)}
        >
          <Icon name="star" size={14} filled={library.favouritesOnly} />
          Favourites
        </button>
        {#if library.narrowed}
          <button class="clear" onclick={() => library.clearFilters()}>Clear</button>
        {/if}
      </div>

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

      <div class="toolbar-end right">
        {#if target}
          <div class="capture"><AddItem kind={target} /></div>
        {/if}
      </div>
    </div>

    <!-- In the everything view the shelves are a filter of their own, and it
         is the one people actually reach for: "show me the films" is a more
         common question here than "show me the things I have paused". They
         are a second row rather than more chips in the first, because they
         narrow a different axis and mixing the two makes a row where no two
         neighbouring chips are comparable. -->
    {#if shelf === null && !library.favouritesOnly && library.visibleKinds.length > 1}
      <div class="shelves">
        {#each library.visibleKinds as k (k.id)}
          <button
            class="shelfchip"
            style="--dot: {k.color}"
            onclick={() => void library.selectShelf(k.id)}
          >
            <span class="dot"></span>{k.name}
            {#if k.items > 0}<span class="n">{k.items}</span>{/if}
          </button>
        {/each}
      </div>
    {/if}

    {#if library.note}
      <p class="note">{library.note}</p>
    {/if}

    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <!-- One tab stop for the shelf, and the arrow keys inside it. The grid
         is two-dimensional and `rovingFocus` measures that for itself, so
         Down means the card below rather than the next card along. -->
    <div
      class="scroll body"
      oncontextmenu={(e) => menu.show(e, shelfMenu())}
      use:rovingFocus={library.selected}
    >
      {#if library.items.length === 0 && !library.loading}
        {#if library.query.trim()}
          <EmptyState lead={`Nothing matches “${library.query.trim()}”.`}>
            {#snippet note()}Try fewer words, or widen the filter to <b>All</b>.{/snippet}
            {#snippet action()}
              <button class="btn" onclick={() => library.clearFilters()}>Clear the filters</button>
            {/snippet}
          </EmptyState>
        {:else if library.favouritesOnly}
          <EmptyState lead="No favourites yet.">
            {#snippet note()}The star on a card puts it here.{/snippet}
          </EmptyState>
        {:else if library.filter !== 'all'}
          <EmptyState lead="Nothing here under this filter.">
            {#snippet note()}
              Nothing on {shelf ? `the ${shelf.name.toLowerCase()} shelf` : 'any shelf'} is
              <b>{filterLabel(library.filter).toLowerCase()}</b> at the moment.
            {/snippet}
            {#snippet action()}
              <button class="btn" onclick={() => library.setFilter('all')}>Show everything</button>
            {/snippet}
          </EmptyState>
        {:else if target}
          <EmptyState lead={shelf ? `Nothing on this shelf yet.` : 'Nothing in the library yet.'}>
            {#snippet note()}
              Type a title in the box above and press Enter. With the
              <Icon name="sparkle" size={12} /> on, the cover and the details are looked up for you; with
              it off, exactly what you type is what you get.
            {/snippet}
            {#snippet action()}
              <button class="btn btn-primary" onclick={() => library.focusCapture()}>
                Add {article(target.singular)}
                {target.singular.toLowerCase()}
              </button>
            {/snippet}
          </EmptyState>
        {:else}
          <EmptyState lead="No shelves yet.">
            {#snippet note()}
              Add one in the sidebar — books, films, restaurants, anything you keep a list of.
            {/snippet}
          </EmptyState>
        {/if}
      {:else if library.view === 'grid'}
        <div class="grid">
          {#each library.items as row (row.id)}
            <ItemCard
              item={row}
              kind={library.kindOf(row)}
              {ratio}
              selected={library.selected === row.id}
              onopen={() => void library.open(row.id)}
              onmenu={(e: MouseEvent) => menu.show(e, rowMenu(row))}
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
              data-row={row.id}
              onclick={() => void library.open(row.id)}
              oncontextmenu={(e) => menu.show(e, rowMenu(row))}
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

{#if pendingDelete}
  <ConfirmDialog
    title={'Delete “' + pendingDelete.title + '”?'}
    detail="Everything recorded about it goes too — your rating, your notes and its history. This cannot be undone."
    confirmLabel="Delete"
    onconfirm={() => {
      const doomed = pendingDelete
      pendingDelete = null
      if (doomed) void library.remove(doomed.id)
    }}
    oncancel={() => (pendingDelete = null)}
  />
{/if}

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
    height: var(--header-h);
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

  .capture {
    flex: 1;
    max-width: 380px;
    margin-left: auto;
  }

  .star,
  .clear {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    height: 28px;
    padding: 0 var(--sp-3);
    border-radius: 999px;
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-subtle);
    white-space: nowrap;
  }
  .star:hover,
  .clear:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .star.on {
    background: color-mix(in oklab, #e0a92b 16%, transparent);
    color: #a9781a;
  }

  .shelves {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: var(--sp-2);
    flex: none;
    padding: 0 var(--sp-4) var(--sp-3);
  }
  .shelfchip {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    height: 26px;
    padding: 0 var(--sp-3);
    border: 1px solid var(--border);
    border-radius: 999px;
    font-size: var(--text-sm);
    color: var(--fg-muted);
    white-space: nowrap;
  }
  .shelfchip:hover {
    border-color: var(--dot);
    color: var(--fg);
  }
  .shelfchip .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--dot);
  }
  .shelfchip .n {
    font-size: var(--text-xs);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }

  .note {
    margin: 0 var(--sp-4) var(--sp-2);
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }

  .body {
    flex: 1;
    display: flex;
    flex-direction: column;
    /* The tail clears the floating assistant button. */
    padding: var(--sp-1) var(--sp-4) var(--fab-clear);
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
</style>
