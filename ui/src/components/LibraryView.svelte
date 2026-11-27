<script lang="ts">
  // The library app's main pane: the capture line, the filter chips, and the
  // shelf itself as covers or as a list.
  //
  // Covers by default, and that is the editorial point of the app. A shelf
  // people recognise at a glance is one they browse; a table of titles is one
  // they only ever search. The list is one keystroke away for when a shelf has
  // grown past the point where pictures help.

  import { untrack } from 'svelte'
  import { files } from '../lib/files.svelte'
  import { keysLabel } from '../lib/keys'
  import { userAgentMac } from '../lib/platform'
  import { article, plural } from '../lib/format'
  import { FILTERS, coverRatio, library, type Filter } from '../lib/library.svelte'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { closesOnBack, phone } from '../lib/phone.svelte'
  import { itemMenu } from '../lib/menus'
  import { isContactShelf, lateness } from '../lib/keepintouch'
  import type { Item, ItemSort } from '../lib/types'
  import AddItem from './AddItem.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Cover from './Cover.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import ItemCard from './ItemCard.svelte'
  import { rovingFocus } from '../lib/roving'
  import {
    CARD_MIN,
    CARD_MIN_PHONE,
    CARD_TEXT_H,
    ROW_H,
    ROW_H_PHONE,
    frameHeight,
    gridMetrics,
    keepLine,
    lineTop,
    shelfLines,
    shelfWindow,
  } from '../lib/library-grid'
  import { scrollFor } from '../lib/photos'
  import { proposals } from '../lib/proposals.svelte'
  import ProposalGhost from './ProposalGhost.svelte'
  import ItemDetail from './ItemDetail.svelte'
  import Rating from './Rating.svelte'

  // Loaded when the view appears rather than when the store is imported, so
  // a vault whose owner never opens this app never reads a shelf. Coming back
  // keeps what was drawn, or refreshes it, instead of blanking the grid.
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

  // ── On a phone ───────────────────────────────────────────────────────
  //
  // An item opened from the shelf is a page over it, with a way back
  // (`PhoneBack`, in the item's own header) and Android's back button to the
  // same end. Judged by the item rather than by a flag set where a card is
  // pressed, since a shelf also opens an item that was just made from a file,
  // or found by the search bar, or asked for by the assistant. The shelf stays
  // where it is underneath, scrolled to the same place.
  const layered = $derived(phone.on && item !== null)
  closesOnBack(
    () => layered,
    () => library.close(),
  )

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
        hint: keysLabel('mod+n', userAgentMac),
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
        icon: 'sort',
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

  /**
   * How many are shown, and of how many when something narrows them.
   *
   * "0 of 1 items, to hear" says where the empty list went; the bare count
   * of the filtered list read as the size of the shelf. The total is the
   * open shelf's own count, or the whole library in the everything view.
   */
  const countLine = $derived.by(() => {
    const shown = library.items.length
    const total = library.favouritesOnly
      ? null
      : shelf
        ? shelf.items
        : (library.stats?.items ?? null)
    const how = library.filter !== 'all' ? filterLabel(library.filter).toLowerCase() : ''
    if (total !== null && total > shown) {
      return `${shown} of ${plural(total, 'item')}${how ? `, ${how}` : ''}`
    }
    return `${plural(shown, 'item')}${how ? `, ${how}` : ''}`
  })

  /**
   * Catch-ups the assistant read out of the journal, waiting to be told who
   * with. Drawn at the top of the Contacts shelf, where the accepted row
   * would be, and nowhere a person is not what is being looked at.
   */
  const catchUps = $derived(
    proposals.enabled && isContactShelf(shelf) ? proposals.forKind('log') : [],
  )

  const sortLabel = $derived(
    (library.dueFirst && isContactShelf(shelf)
      ? 'Due to get in touch'
      : (SORTS.find((s) => s.id === library.sort)?.label ?? '')
    ).toLowerCase(),
  )

  /** What is still ahead on a shelf: the number the sidebar shows beside it. */
  function openOn(id: string): number {
    return library.stats?.byKind.find((c) => c.kindId === id)?.open ?? 0
  }

  const heading = $derived(library.favouritesOnly ? 'Favourites' : (shelf?.name ?? 'Everything'))
  // One frame for the whole grid; see `coverRatio` for why that beats one
  // frame per card. The everything view has no shelf, so it gets portrait.
  const ratio = $derived(coverRatio(library.favouritesOnly ? null : shelf))

  // ── Drawing a window of the shelf ────────────────────────────────────
  //
  // Only the lines near the viewport are drawn; padding stands for the rest
  // (see `library-grid.ts`). What that is cut from is measured here: where
  // the shelf sits against the pane that scrolls it, how tall that pane is,
  // how wide the shelf is and the gaps between cards, and how tall a card or
  // a row turned out to be. Scrolling is heard with a passive listener on the
  // pane and never with a wheel listener, which would take WebKitGTK's
  // scrolling off its own thread.

  let mainEl = $state<HTMLElement | null>(null)
  let bodyEl = $state<HTMLElement | null>(null)
  let shelfEl = $state<HTMLElement | null>(null)

  /** The pane that scrolls: the body, or on a phone held sideways, where the
   *  heading and the bars scroll away too, the whole column. */
  let scroller: HTMLElement | null = null
  /** The top of the scrolling pane's viewport, against the window. */
  let viewportTop = 0
  let view = $state.raw({ top: 0, height: 0, width: 0, colGap: 0, rowGap: 0 })

  /** Read where things are. `full` also reads what only a layout change
   *  moves: which pane scrolls, its place, the shelf's width and gaps. */
  function measure(full = false) {
    const shelfNode = shelfEl
    const body = bodyEl
    const main = mainEl
    if (!shelfNode || !body || !main) return
    let { width, colGap, rowGap } = view
    if (full || !scroller) {
      scroller = getComputedStyle(body).overflowY === 'visible' ? main : body
      viewportTop = scroller.getBoundingClientRect().top + scroller.clientTop
      const style = getComputedStyle(shelfNode)
      width = shelfNode.clientWidth
      colGap = parseFloat(style.columnGap) || 0
      rowGap = parseFloat(style.rowGap) || 0
    }
    const top = viewportTop - shelfNode.getBoundingClientRect().top
    const height = scroller.clientHeight
    const was = view
    if (
      top !== was.top ||
      height !== was.height ||
      width !== was.width ||
      colGap !== was.colGap ||
      rowGap !== was.rowGap
    ) {
      view = { top, height, width, colGap, rowGap }
    }
  }

  // The panes' sizes -- the window, the detail panel opening beside the
  // shelf -- and their scrolling.
  $effect(() => {
    const main = mainEl
    const body = bodyEl
    if (!main || !body) return
    const resized = new ResizeObserver(() => measure(true))
    resized.observe(main)
    resized.observe(body)
    const scrolled = () => measure()
    main.addEventListener('scroll', scrolled, { passive: true })
    body.addEventListener('scroll', scrolled, { passive: true })
    return () => {
      resized.disconnect()
      main.removeEventListener('scroll', scrolled)
      body.removeEventListener('scroll', scrolled)
    }
  })

  // The shelf drawn, or something above it in the pane come or gone.
  $effect(() => {
    void shelfEl
    void library.view
    void library.items.length
    void catchUps.length
    void phone.on
    untrack(() => measure(true))
  })

  const grid = $derived(library.view === 'grid')
  const columns = $derived(
    grid
      ? gridMetrics(view.width, phone.on ? CARD_MIN_PHONE : CARD_MIN, view.colGap)
      : { cols: 1, cell: view.width },
  )
  const frame = $derived(frameHeight(ratio, columns.cell))
  /** What a card's or a row's height follows from. */
  const shape = $derived(`${library.view}|${columns.cols}|${columns.cell}|${ratio}|${phone.on}`)
  /** A card or a row as measured, for the shape it was measured at. */
  let measured = $state.raw<{ shape: string; height: number } | null>(null)
  const itemHeight = $derived(
    measured && measured.shape === shape
      ? measured.height
      : grid
        ? frame + CARD_TEXT_H
        : phone.on
          ? ROW_H_PHONE
          : ROW_H,
  )
  const lines = $derived(
    shelfLines(library.items.length, columns.cols, itemHeight, grid ? view.rowGap : 0),
  )
  const win = $derived(
    view.width > 0
      ? shelfWindow(lines, library.items.length, view.top, view.height || innerHeight)
      : { from: 0, to: 0, before: 0, after: 0 },
  )
  /** Where each item is in the whole shelf, for the arrow keys. */
  const indexOf = $derived(new Map(library.items.map((row, i) => [row.id, i])))
  /** The card or row with focus, by id. Its line stays drawn wherever the
   *  shelf is scrolled to (`keepLine`): unmounted, it would take focus with
   *  it out of the shelf, and the arrow keys with it. */
  let focusedId = $state<string | null>(null)
  const kept = $derived(focusedId === null ? null : (indexOf.get(focusedId) ?? null))
  const drawnShelf = $derived(keepLine(win, lines, library.items.length, kept))
  // Numbers, so a scroll that stays within the same lines draws nothing new.
  const from = $derived(drawnShelf.from)
  const to = $derived(drawnShelf.to)
  const before = $derived(drawnShelf.before)
  const after = $derived(drawnShelf.after)
  const thenFrom = $derived(drawnShelf.then.from)
  const thenTo = $derived(drawnShelf.then.to)
  const skip = $derived(drawnShelf.then.skip)
  /** What is drawn, in shelf order: `null` is the space standing for the
   *  lines between the window and the focused card's line apart from it. */
  const shown = $derived<(Item | null)[]>(
    thenFrom < thenTo
      ? [...library.items.slice(from, to), null, ...library.items.slice(thenFrom, thenTo)]
      : library.items.slice(from, to),
  )
  /** The cover's longer side in a card, for the scaled copy it asks for. */
  const drawn = $derived(Math.max(columns.cell, frame))

  // A card or a row is measured once it is drawn at a new shape: the window
  // is cut by what the page actually laid out, so lines never drift.
  const drawing = $derived(shown.length > 0)
  $effect(() => {
    void drawing
    const now = shape
    const first = shelfEl?.firstElementChild
    if (!first) return
    const height = first.getBoundingClientRect().height
    untrack(() => {
      if (height > 0 && (measured?.shape !== now || Math.abs(measured.height - height) > 0.01)) {
        measured = { shape: now, height }
      }
    })
  })

  function focusIn(e: FocusEvent) {
    const row = e.target instanceof Element ? e.target.closest('[data-row]') : null
    focusedId = row?.getAttribute('data-row') ?? null
  }

  function focusOut(e: FocusEvent) {
    // Gone elsewhere on the page, or nowhere. Not the window losing focus,
    // which leaves it on the card to come back to.
    const next = e.relatedTarget instanceof Node ? e.relatedTarget : document.activeElement
    if (!bodyEl?.contains(next)) focusedId = null
  }

  /** Bring item `index`'s line into view; the scroll draws it. */
  function scrollToIndex(index: number) {
    measure(true)
    const pane = scroller
    const shelfNode = shelfEl
    if (!pane || !shelfNode) return
    const offset = shelfNode.getBoundingClientRect().top - viewportTop + pane.scrollTop
    const line = { top: offset + lineTop(lines, index), height: itemHeight }
    pane.scrollTop = scrollFor(line, pane.scrollTop, pane.clientHeight)
    measure()
  }

  // One tab stop, and the arrow keys over the whole shelf rather than the
  // lines drawn: a card not drawn is scrolled to and then focused.
  const roving = $derived({
    current: library.selected,
    virtual: {
      length: library.items.length,
      idAt: (index: number) => library.items[index]?.id ?? null,
      indexOf: (id: string) => indexOf.get(id) ?? -1,
      scrollToIndex,
    },
  })
</script>

<div class="library" class:layered style="--tint: {library.accent}">
  <div class="main" bind:this={mainEl}>
    <header class="bar">
      <div class="pane-titles">
        <h1 class="pane-title">{heading}</h1>
        <span class="pane-sub">
          {countLine}
          {#if library.loading}· loading…{/if}
        </span>
      </div>

      <!-- On a phone a button of its own: the select lies over it, unseen,
           and is what opens. The word it would say is a third of the screen. -->
      <span class="sort-wrap">
        <select
          class="sort"
          aria-label="Sort"
          title="Sorted by {sortLabel}"
          value={library.dueFirst && isContactShelf(shelf) ? 'due' : library.sort}
          onchange={(e) => {
            const value = e.currentTarget.value
            if (value === 'due') library.setDueFirst()
            else library.setSort(value as ItemSort)
          }}
        >
          {#if isContactShelf(shelf)}
            <option value="due">Due to get in touch</option>
          {/if}
          {#each SORTS as s (s.id)}
            <option value={s.id}>{s.label}</option>
          {/each}
        </select>
        <span class="sort-icon phone-only" aria-hidden="true">
          <Icon name="sort" size={18} />
        </span>
      </span>

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
          aria-label="Favourites"
          title={library.favouritesOnly ? 'Show everything again' : 'Favourites only'}
          onclick={() => library.setFavouritesOnly(!library.favouritesOnly)}
        >
          <Icon name="star" size={14} filled={library.favouritesOnly} />
          <span class="star-word">Favourites</span>
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
            title="{k.name} — {plural(k.items, 'item')}, {openOn(k.id)} still ahead"
            onclick={() => void library.selectShelf(k.id)}
          >
            <span class="dot"></span>{k.name}
            {#if openOn(k.id) > 0}<span class="n">{openOn(k.id)}</span>{/if}
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
      bind:this={bodyEl}
      oncontextmenu={(e) => menu.show(e, shelfMenu())}
      onfocusin={focusIn}
      onfocusout={focusOut}
      use:rovingFocus={roving}
    >
      {#each catchUps as p (p.id)}
        <div class="ghosts"><ProposalGhost proposal={p} /></div>
      {/each}
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
          <EmptyState
            lead={`Nothing ${shelf ? `on the ${shelf.name.toLowerCase()} shelf` : 'on any shelf'} is ${filterLabel(library.filter).toLowerCase()}`}
          >
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
              {#if files.supported}
                A PDF or an EPUB from your files can go on a shelf too: the
                <Icon name="file" size={12} /> button picks one.
              {/if}
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
        <div
          class="grid"
          bind:this={shelfEl}
          style:grid-template-columns="repeat({columns.cols}, minmax(0, 1fr))"
          style:padding-top="{before}px"
          style:padding-bottom="{after}px"
        >
          {#each shown as row (row?.id ?? '')}
            {#if row}
              <ItemCard
                item={row}
                kind={library.kindOf(row)}
                {ratio}
                {drawn}
                selected={library.selected === row.id}
                onopen={() => void library.open(row.id)}
                onmenu={(e: MouseEvent) => menu.show(e, rowMenu(row))}
              />
            {:else}
              <div class="skip" style:height="{skip}px"></div>
            {/if}
          {/each}
        </div>
      {:else}
        <div
          class="rows"
          bind:this={shelfEl}
          style:padding-top="{before}px"
          style:padding-bottom="{after}px"
        >
          {#each shown as row (row?.id ?? '')}
            {#if row}
              {@const kind = library.kindOf(row)}
              {@const due = library.dueOf(row.id)}
              <button
                class="row list-row"
                class:sel={library.selected === row.id}
                data-row={row.id}
                onclick={() => void library.open(row.id)}
                oncontextmenu={(e) => menu.show(e, rowMenu(row))}
              >
                <span class="thumb">
                  <!-- A jacket taller than wide fills the square by its width,
                       so its longer side is drawn half as long again. -->
                  <Cover
                    blob={row.cover}
                    title={row.title}
                    icon={kind?.icon ?? ''}
                    color={kind?.color ?? 'var(--accent)'}
                    ratio="1 / 1"
                    drawn={(phone.on ? 40 : 34) * 1.5}
                  />
                </span>
                <span class="cell name">
                  <span class="t">
                    {row.title}
                    {#if row.files.length > 0}
                      <!-- A file to read: Read opens it from the panel. -->
                      <span class="filed" title="Has a file to read">
                        <Icon name="file" size={12} />
                      </span>
                    {/if}
                  </span>
                  {#if due && due.daysOver >= -7}
                    <span class="c">{lateness(due)}</span>
                  {:else if row.creator}<span class="c">{row.creator}</span>
                  {:else if kind}<span class="c phone-only">{kind.name}</span>{/if}
                </span>
                <span class="cell shelf">{kind?.name ?? ''}</span>
                <!-- Side by side as columns; on a phone the state over the
                     stars, in the one column there is room for. -->
                <span class="aside">
                  <span class="cell state">{library.label(kind, row.status)}</span>
                  <span class="cell year">{row.year ?? ''}</span>
                  <span class="cell score">
                    {#if row.rating !== null && row.rating !== undefined}
                      <Rating value={row.rating} size={12} readonly />
                    {/if}
                  </span>
                </span>
              </button>
            {:else}
              <div class="skip" style:height="{skip}px"></div>
            {/if}
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
  /* No background of its own: it stands on the raised surface `App.svelte`
     lays under every app. */
  .library {
    /* As tall as the stage by a stated height, not stretched: see
       `.shell-stage` in app.css. */
    height: 100%;
    display: flex;
    flex: 1;
    min-width: 0;
    min-height: 0;
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

  /* Nothing but a holder on a desktop, where the select is its own pill. */
  .sort-wrap,
  .aside {
    display: contents;
  }

  /* A pill, like the view switch beside it and the chips under it, and as
     tall as the switch. */
  .sort {
    height: 34px;
    padding: 0 var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-pill);
    background: var(--bg-raised);
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }

  /* Two round buttons in a pill, 34px tall overall like Mail's list-header
     buttons. */
  .views {
    display: flex;
    gap: 2px;
    padding: 2px;
    border-radius: var(--radius-pill);
    background: var(--bg-active);
  }
  .view {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .view.on {
    background: var(--bg-raised);
    color: var(--fg);
    box-shadow: var(--shadow-sm);
  }

  /* `.toolbar`'s three children (this, `.filters`, `.toolbar-end.right`)
     share `app.css`'s `flex: 1 1 0` and `min-width: 0` -- fine with two
     short, similar-sized ends either side of a row of chips, which is every
     other app's toolbar, but with the detail panel also narrowing this
     pane, nothing here protected either end's own *content* from shrinking
     past what it actually needs. The two overrides below give each end back
     a floor at its own irreducible width (a button's text, the capture
     line's icons with its field run all the way down) -- see `.filters`,
     below, for where the space those floors claim actually comes from. */
  .toolbar-end:not(.right) {
    min-width: 120px;
  }
  /* Wide enough for the field to stay a field -- at the icons-only floor it
     was a shelf picker and two buttons with nowhere to type. The status tabs
     scroll to make the room. A third button, for adding from a file, takes
     what the field would have had. */
  .toolbar-end.right {
    min-width: 250px;
  }

  .capture {
    flex: 1;
    /* Without this, the capture line's own incompressible content (the
       shelf-picker lead and the two icon buttons either side of the field)
       set this flex item's automatic minimum, which is what the overflow
       above was: the line kept its full width and overflowed `.toolbar-end
       .right` leftward over the status tabs rather than shrinking into it --
       Bug: the add box drawn on top of them, "Given up" hidden behind it.
       `.field` inside already shrinks its placeholder text away first; this
       is what lets the box itself follow it down to the floor above. */
    min-width: 0;
    max-width: 380px;
    margin-left: auto;
  }

  /* `app.css`'s own `.filters` wraps its chips onto a second line rather
     than overflow -- fine on its own, but as a flex *item* here it held its
     full seven-tab width rather than ever giving any of it up, which is what
     left the two ends above nothing to shrink into in the first place, and
     wrapping would have changed the toolbar's height every time the detail
     panel opened or closed. Scrolling this row instead costs neither: it
     stays one line, and a chip that is off the edge is a drag (or a focus
     move -- the browser scrolls a focused button into view on its own) away
     rather than gone. */
  .filters {
    flex: 0 1 auto;
    min-width: 0;
    flex-wrap: nowrap;
    justify-content: flex-start;
    overflow-x: auto;
    scrollbar-width: none;
  }
  .filters::-webkit-scrollbar {
    display: none;
  }

  .star,
  .clear {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    height: 28px;
    padding: 0 var(--sp-3);
    border-radius: var(--radius-pill);
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
    background: color-mix(in oklab, var(--star) 16%, transparent);
    color: color-mix(in oklab, var(--star) 70%, var(--fg));
  }

  .shelves {
    display: flex;
    /* Centred wrapping wants its last line close to full: with seven chips
       it was six, centred, above a seventh centred alone on its own line --
       a small count that reads as a mistake rather than a shelf. Scrolling
       keeps it one line and centred stays meaningful (there is nothing
       to the left of "Books" worth seeing), same reasoning as `.filters`,
       above. */
    flex-wrap: nowrap;
    /* Centred by the auto margins below rather than by `justify-content:
       center`, which centres an overflowing row too -- pushing "Books" off
       the left edge, where no scroll can reach it. Auto margins centre while
       there is room and give way to zero once there is not. */
    justify-content: flex-start;
    gap: var(--sp-2);
    flex: none;
    padding: 0 var(--sp-4) var(--sp-3);
    overflow-x: auto;
    scrollbar-width: none;
  }
  .shelves::-webkit-scrollbar {
    display: none;
  }
  .shelves > :first-child {
    margin-left: auto;
  }
  .shelves > :last-child {
    margin-right: auto;
  }
  .shelfchip {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    height: 26px;
    padding: 0 var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-pill);
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

  .ghosts {
    margin-bottom: var(--sp-3);
  }

  .grid {
    display: grid;
    flex: none;
    /* As many columns as keep a card at least 146px wide, worked out in
       script (`library-grid.ts`) the way `auto-fill` did, since the window
       of lines drawn has to know: a wide window should show more covers, not
       wider ones -- a stretched jacket is the one thing a cover grid must
       never do. */
    gap: var(--sp-5) var(--sp-4);
  }

  /* The lines not drawn between the window and the focused card's own,
     drawn apart from it (`keepLine`). */
  .skip {
    grid-column: 1 / -1;
    flex: none;
  }

  /* Pulled out by exactly the inset `.list-row` gives each card (see
     app.css's "Lists"), so a card's edge lines up with the covers' in the
     grid view and the text inside it stays where it always was. */
  .rows {
    display: flex;
    flex: none;
    flex-direction: column;
    /* Capped so the year and stars stay near the title on a wide window,
       instead of a column a metre from the name it belongs to. */
    max-width: var(--page-w-wide);
    margin-inline: calc(var(--sp-2) * -1) auto;
  }
  .row {
    display: flex;
    flex: none;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-2) var(--sp-2);
    /* Room for a title over a byline whether a row has one or not, so every
       row is one height and the list can be drawn a window at a time. */
    --name-line: calc(var(--text-base) * var(--leading-snug));
    --byline-line: calc(var(--text-sm) * var(--leading-snug));
    height: calc(2 * var(--sp-2) + var(--name-line) + 1px + var(--byline-line));
  }
  /* The hairline between rows, starting under the title rather than the
     cover -- the column of covers stays one unbroken edge -- and clear of
     the card's rounded corners. Hidden under a hovered or open card, whose
     own tint does the separating. */
  .row::after {
    content: '';
    position: absolute;
    left: calc(var(--sp-2) + 34px + var(--sp-3));
    right: var(--sp-2);
    bottom: 0;
    height: 1px;
    background: var(--border);
  }
  .row:hover::after,
  .row.sel::after {
    opacity: 0;
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
  .filed {
    display: inline-flex;
    margin-left: var(--sp-1);
    vertical-align: -2px;
    color: var(--fg-faint);
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

  /* ── On a phone ──────────────────────────────────────────────────────── */

  /* The item is a page over the shelf, not a column beside it. The shelf is
     hidden, not taken away, so it is as it was -- scrolled to the same cover --
     when the page is put away. */
  :global([data-phone]) .library {
    position: relative;
  }
  :global([data-phone]) .library.layered .main {
    visibility: hidden;
  }

  :global([data-phone]) .bar {
    gap: var(--sp-2);
    padding: 0 var(--sp-3) 0 var(--sp-4);
  }
  /* The sort is an icon, 40 px like the view buttons; the select itself lies
     over it, unseen, and is what opens when it is pressed. */
  :global([data-phone]) .sort-wrap {
    position: relative;
    display: grid;
    place-items: center;
    flex: none;
    width: 40px;
    height: 40px;
    border-radius: 50%;
    background: var(--bg-active);
    color: var(--fg-muted);
  }
  :global([data-phone]) .sort {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    padding: 0;
    border: 0;
    border-radius: 50%;
    opacity: 0;
  }
  :global([data-phone]) .sort-icon {
    display: grid;
    pointer-events: none;
  }
  :global([data-phone]) .views {
    flex: none;
  }
  :global([data-phone]) .view {
    width: 40px;
    height: 40px;
  }

  /* Three rows where a desktop has one: the line to add a thing, across the
     screen, then Favourites and the status tabs, which scroll. */
  :global([data-phone]) .toolbar {
    flex-wrap: wrap;
    gap: var(--sp-2);
    padding: var(--sp-3) var(--sp-3) var(--sp-2);
  }
  :global([data-phone]) .toolbar-end.right {
    order: 1;
    flex: 1 1 100%;
    min-width: 0;
  }
  :global([data-phone]) .capture {
    max-width: none;
    margin-left: 0;
  }
  :global([data-phone]) .toolbar-end:not(.right) {
    order: 2;
    flex: none;
    min-width: 0;
  }
  :global([data-phone]) .filters {
    order: 3;
    flex: 1 1 0;
  }
  :global([data-phone]) .star,
  :global([data-phone]) .clear {
    height: 40px;
    min-width: 40px;
    justify-content: center;
  }
  :global([data-phone]) .star-word {
    display: none;
  }

  :global([data-phone]) .shelves {
    padding: 0 var(--sp-3) var(--sp-2);
  }
  :global([data-phone]) .shelfchip {
    height: 34px;
    padding: 0 var(--sp-3);
  }
  :global([data-phone]) .note {
    margin: 0 var(--sp-3) var(--sp-2);
  }

  :global([data-phone]) .body {
    padding: var(--sp-1) var(--sp-3) var(--fab-clear);
  }
  /* Two covers across every phone held upright (the least a column can be is
     140 px, `CARD_MIN_PHONE`), more on its side: a third across a portrait
     screen is too small to tell apart, one is a column of posters. */
  :global([data-phone]) .grid {
    gap: var(--sp-4) var(--sp-3);
  }

  /* A row: the cover, the title over who made it, and how far along over the
     stars. The shelf and the year are the cover's and the byline's to say. */
  :global([data-phone]) .row {
    min-height: 60px;
    height: 60px;
    gap: var(--sp-3);
    padding: var(--sp-2);
  }
  :global([data-phone]) .row::after {
    left: calc(var(--sp-2) + 40px + var(--sp-3));
  }
  :global([data-phone]) .thumb {
    width: 40px;
  }
  :global([data-phone]) .shelf,
  :global([data-phone]) .year {
    display: none;
  }
  :global([data-phone]) .aside {
    display: flex;
    flex: none;
    flex-direction: column;
    align-items: flex-end;
    gap: 3px;
    max-width: 38%;
  }
  :global([data-phone]) .state {
    width: auto;
    max-width: 100%;
    font-size: var(--text-xs);
  }
  :global([data-phone]) .score {
    width: auto;
    min-height: 12px;
  }

  /* A phone on its side has a couple of hundred pixels of screen between its
     two bars: the heading, the line to add a thing and the tabs are part of
     what scrolls there, and the covers get the whole of it once they are
     scrolled past. */
  @media (max-height: 500px) {
    :global([data-phone]) .main {
      overflow-y: auto;
    }
    :global([data-phone]) .main > * {
      flex: none;
    }
    :global([data-phone]) .body {
      overflow: visible;
      min-height: auto;
    }
  }

  /* A finger leaves nothing under it: no highlight stuck to the row last
     pressed or last opened, only the press itself. */
  :global([data-phone]) .row:hover,
  :global([data-phone]) .row.sel {
    background: transparent;
  }
  :global([data-phone]) .row:active {
    background: var(--bg-hover);
  }
  :global([data-phone]) .row.sel::after,
  :global([data-phone]) .row:hover::after {
    opacity: 1;
  }
</style>
