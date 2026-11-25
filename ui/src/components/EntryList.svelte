<script module lang="ts">
  // What survives the list being put away -- on a phone an entry opened over
  // it unmounts it -- so that back finds it where it was.
  const memory = { key: '', shown: 0, top: 0 }
</script>

<script lang="ts">
  import { app } from '../lib/state.svelte'
  import { details, phone } from '../lib/phone.svelte'
  import { dayNumber, plural, weekdayShort } from '../lib/format'
  import { fileUrl, mediaUrl } from '../lib/api'
  import { menu } from '../lib/menu.svelte'
  import { onOffPref } from '../lib/prefs'
  import { rovingFocus, type RovingParams, type RovingVirtual } from '../lib/roving'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { purposeItems } from '../lib/menus'
  import { toRows, type EntryRow } from '../lib/entry-rows'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import EmptyState from './EmptyState.svelte'
  import EntryCalendar from './EntryCalendar.svelte'
  import Icon from './Icon.svelte'
  import VirtualList from './VirtualList.svelte'
  import type { EntryId, EntrySummary, JournalId } from '../lib/types'

  let pendingDelete = $state<{ id: EntryId; title: string } | null>(null)

  // ── On a phone ───────────────────────────────────────────────────────
  //
  // The list is not virtualised there. `VirtualList` keeps a few rows in the
  // page and re-renders that window, and mounts and measures the row that
  // comes in, on every scroll event; on a phone's processor a fling spent
  // most of its frames in that -- long tasks of 50 to 250 ms, a third of the
  // frames late -- where a plain list, scrolled by the compositor alone,
  // costs the page nothing at all. So the rows are drawn as they are asked
  // for: the first screens at once, and forty more whenever the end comes
  // within a screen or two (`more`, below), after which they stay, so
  // scrolling back is free. Nor does the list sweep its rows for tab stops
  // (`rovingFocus`, which reads layout each time the rows change), since a
  // phone has no Tab or arrow keys for it to serve.

  /** How many rows are added at a time. */
  const PAGE = 40

  /** `rovingFocus`, except on a phone: see above. */
  function roving(node: HTMLElement, params: RovingParams) {
    if (phone.on) return {}
    return rovingFocus(node, params)
  }

  /** Which list this is, so a remembered place is not another's. */
  const listKey = () => `${app.selectedJournal ?? ''}|${app.showStarredOnly}`

  /** How many rows are drawn, and where the list was scrolled to. */
  let shown = $state(memory.key === listKey() ? memory.shown : PAGE)
  let scroller = $state<HTMLElement | null>(null)
  let sentinel = $state<HTMLElement | null>(null)

  const showCalendarPref = onOffPref('karmayoga.journal.calendar')

  /**
   * Whether the month is showing above the list.
   *
   * Remembered locally, like every other piece of chrome state: somebody who
   * navigates by date wants it there every time, and somebody who reads down
   * the list wants the height back.
   */
  let showCalendar = $state(
    showCalendarPref.get() &&
      // On a phone, the month starts folded away unless it was asked for: it
      // is a third of the screen, over the entries that are what the page is
      // for. The device keeps its own choice from then on.
      !(phone.on && localStorage.getItem('karmayoga.journal.calendar') === null),
  )
  /** Open an entry -- over the list, on a phone (`details`). */
  function open(id: EntryId) {
    void app.openEntry(id)
    details.open('journal')
  }

  /** Today's entry, opened the same way. */
  function startToday() {
    void app.newEntry()
    details.open('journal')
  }

  function toggleCalendar() {
    showCalendar = !showCalendar
    showCalendarPref.set(showCalendar)
  }

  async function remove() {
    const doomed = pendingDelete
    pendingDelete = null
    if (doomed) await app.deleteEntry(doomed.id)
  }

  // Flattened for `VirtualList`: a header row and an entry row interleaved,
  // in the order the backend already sorted `app.entries` into. See
  // `entry-rows.ts` for why a heading is a row of its own kind rather than a
  // nested list.
  const rows = $derived(toRows(app.entries))

  /** `rovingFocus`'s own handle on `VirtualList`, so Home/End/PageUp/
   *  PageDown can ask it to scroll a row into view before focusing it --
   *  see `VirtualList.svelte`'s `scrollToIndex` and `roving.ts`'s
   *  "Virtualized lists". */
  // A `bind:this` component instance is `any` to typescript-eslint, which
  // cannot resolve types through a `.svelte` import the way svelte-check
  // does -- see `TodoView.svelte`'s own note; that checker does verify this.
  // eslint-disable-next-line @typescript-eslint/no-redundant-type-constituents, @typescript-eslint/no-unsafe-assignment
  let listHandle: ReturnType<typeof VirtualList<EntryRow>> | undefined = $state()

  /** The navigable rows -- headers excluded, a header is never a stop --
   *  which is what a data index means to `rovingFocus`'s virtualized path. */
  const navRows = $derived(
    rows.filter((r): r is Extract<EntryRow, { kind: 'entry' }> => r.kind === 'entry'),
  )

  /** `undefined` while a search is showing: those results are drawn in full
   *  by a plain `{#each}`, not `VirtualList`, so the ordinary DOM-only path
   *  already reaches every row and there is nothing virtualized to adapt. */
  const virtual = $derived<RovingVirtual | undefined>(
    app.query.trim() || navRows.length === 0
      ? undefined
      : {
          length: navRows.length,
          idAt: (i: number) => navRows[i]?.entry.id ?? null,
          indexOf: (id: string) => navRows.findIndex((r) => r.entry.id === id),
          scrollToIndex: (i: number) => {
            const at = rows.indexOf(navRows[i]!)
            // eslint-disable-next-line @typescript-eslint/no-unsafe-call, @typescript-eslint/no-unsafe-member-access
            if (at >= 0) listHandle?.scrollToIndex(at)
          },
        },
  )

  /** The place the list is at, kept for when it is back. Cheap: one read. */
  function remember() {
    if (!scroller) return
    memory.key = listKey()
    memory.shown = shown
    memory.top = scroller.scrollTop
  }

  // Back where it was: the rows that were drawn, and the place in them. Once.
  let restored = false
  $effect(() => {
    if (!scroller || restored || !phone.on) return
    restored = true
    if (memory.key === listKey() && memory.top > 0) scroller.scrollTop = memory.top
  })

  // Another journal, or the starred, is another list: from its top.
  let drawn = listKey()
  $effect(() => {
    const key = listKey()
    if (key === drawn) return
    drawn = key
    shown = PAGE
    if (scroller) scroller.scrollTop = 0
  })

  // More rows as the end comes near. Watched by the page's own observer, off
  // the scroll itself, so that nothing runs on the way there. Set again after
  // each batch, since it only reports a change: a batch that leaves the end
  // still near has to be asked about again.
  $effect(() => {
    void shown
    if (!phone.on || !scroller || !sentinel || shown >= rows.length) return
    const root = scroller
    const watch = new IntersectionObserver(
      (hits) => {
        if (hits.some((h) => h.isIntersecting)) {
          shown = Math.min(rows.length, shown + PAGE)
          memory.shown = shown
        }
      },
      { root, rootMargin: '0px 0px 1500px 0px' },
    )
    watch.observe(sentinel)
    return () => watch.disconnect()
  })

  const heading = $derived(app.showStarredOnly ? 'Starred' : (app.journal?.name ?? 'All entries'))

  /**
   * Where an entry can be filed instead.
   *
   * Omitted entirely when there is one journal, because a menu whose only
   * choice is the one already in force is a row that does nothing.
   */
  function moveItem(id: EntryId, journalId: JournalId): MenuItem | false {
    return (
      app.journals.length > 1 && {
        label: 'Move to',
        icon: 'layers',
        items: app.journals.map((j) => ({
          label: j.name,
          dot: j.color,
          checked: j.id === journalId,
          run: () => app.moveEntry(id, j.id),
        })),
      }
    )
  }

  function entryMenu(row: EntrySummary): MenuItem[] {
    return tidyMenu([
      { label: 'Open', icon: 'quote', run: () => open(row.id) },
      SEP,
      {
        label: row.starred ? 'Remove star' : 'Star',
        icon: 'star',
        run: () => app.toggleStar(row.id),
      },
      SEP,
      {
        label: 'File under',
        icon: 'compass',
        items: purposeItems(row.purpose, (purpose) => app.setEntryPurpose(row.id, purpose)),
      },
      moveItem(row.id, row.journalId),
      SEP,
      {
        label: 'Delete entry…',
        icon: 'trash',
        danger: true,
        run: () => (pendingDelete = { id: row.id, title: row.title }),
      },
    ])
  }

  /**
   * The same, for a search result.
   *
   * A hit is not a row: it carries a score and a snippet rather than the
   * flags, so the one action that needs to know whether an entry is starred
   * is not offered here rather than being offered wrongly.
   */
  function hitMenu(hit: { id: EntryId; title: string; journalId: JournalId }): MenuItem[] {
    return tidyMenu([
      { label: 'Open', icon: 'quote', run: () => open(hit.id) },
      SEP,
      moveItem(hit.id, hit.journalId),
      SEP,
      {
        label: 'Delete entry…',
        icon: 'trash',
        danger: true,
        run: () => (pendingDelete = { id: hit.id, title: hit.title }),
      },
    ])
  }

  /** The list itself, where there is no row under the pointer. */
  function listMenu(): MenuItem[] {
    return tidyMenu([
      { label: "Today's entry", icon: 'plus', hint: 'Ctrl+N', run: () => startToday() },
      !!app.query.trim() && { label: 'Clear search', icon: 'close', run: () => app.clearSearch() },
      SEP,
      {
        label: 'Show the month',
        icon: 'calendar',
        checked: showCalendar,
        run: toggleCalendar,
      },
    ])
  }

  /** Split a snippet on its highlight ranges so matches can be marked. */
  function pieces(text: string, ranges: [number, number][]) {
    const out: { text: string; hit: boolean }[] = []
    let at = 0
    for (const [s, e] of ranges) {
      if (s > at) out.push({ text: text.slice(at, s), hit: false })
      out.push({ text: text.slice(s, e), hit: true })
      at = e
    }
    if (at < text.length) out.push({ text: text.slice(at), hit: false })
    return out
  }
</script>

{#snippet entryRow(r: EntryRow)}
  {#if r.kind === 'header'}
    <div class="grouphead"><span class="eyebrow">{r.label}</span></div>
  {:else}
    {@const row: EntrySummary = (r as Extract<EntryRow, { kind: 'entry' }>).entry}
    <button
      class="row list-row"
      class:sel={!phone.on && app.selectedEntry === row.id}
      data-row={row.id}
      onclick={() => open(row.id)}
      oncontextmenu={(e) => menu.show(e, entryMenu(row))}
    >
      <div class="cal" aria-hidden="true">
        <span class="dow">{weekdayShort(row.localDate)}</span>
        <span class="dom">{dayNumber(row.localDate)}</span>
      </div>

      <div class="body">
        <div class="title">
          <span class="titletext">{row.title || 'Untitled entry'}</span>
          {#if row.starred}<span class="star" title="Starred"
              ><Icon name="star" size={12} filled /></span
            >{/if}
        </div>
        {#if row.excerpt}<div class="excerpt">{row.excerpt}</div>{/if}
        {#if row.tags.length || row.place}
          <div class="chips">
            {#if row.place}
              <span class="place"><Icon name="place" size={12} weight={1.6} /> {row.place}</span>
            {/if}
            {#each row.tags.slice(0, 3) as tag (tag)}<span class="chip">{tag}</span>{/each}
          </div>
        {/if}
      </div>

      {#if row.cover}
        <img class="thumb" src={mediaUrl(row.cover)} alt="" loading="lazy" />
      {:else if row.coverFile}
        <img class="thumb" src={fileUrl(row.coverFile)} alt="" loading="lazy" />
      {/if}
    </button>
  {/if}
{/snippet}

<section class="list">
  <header class="top">
    <div class="pane-titles">
      <h1 class="pane-title">{heading}</h1>
    </div>
    <button
      class="new icon"
      class:on={showCalendar}
      onclick={toggleCalendar}
      title={showCalendar ? 'Hide the month' : 'Show the month'}
      aria-pressed={showCalendar}
      aria-label="Show the month"
    >
      <Icon name="calendar" size={16} />
    </button>
    <button
      class="new"
      onclick={() => startToday()}
      title="Today's entry (Ctrl+N)"
      aria-label="Today's entry"
    >
      <Icon name="plus" size={17} />
    </button>
  </header>

  <!-- Hidden while searching: results are ranked by relevance across every
       month, so a calendar of one of them would be answering a question
       nobody asked. -->
  {#if showCalendar && !app.query.trim()}
    <EntryCalendar />
  {/if}

  <!-- One tab stop for the whole list, and the arrow keys inside it: see
       `lib/roving.ts`. Without it, Tab from the search field walked through
       every entry in the journal before it reached the editor. -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="scroll rows"
    bind:this={scroller}
    oncontextmenu={(e) => menu.show(e, listMenu())}
    onscroll={phone.on ? remember : undefined}
    use:roving={{ current: app.selectedEntry, virtual }}
  >
    {#if app.query.trim()}
      {#if app.searching && app.results.length === 0}
        <p class="note">Searching…</p>
      {:else if app.results.length === 0}
        <EmptyState lead={`Nothing matches “${app.query.trim()}”.`}>
          {#snippet note()}Try a shorter word, or a different journal.{/snippet}
          {#snippet action()}
            <button class="btn" onclick={() => app.clearSearch()}>Clear the search</button>
          {/snippet}
        </EmptyState>
      {:else}
        <div class="grouphead">
          <span class="eyebrow">{plural(app.results.length, 'result')}</span>
        </div>
        {#each app.results as hit (hit.id)}
          <button
            class="row list-row"
            class:sel={!phone.on && app.selectedEntry === hit.id}
            data-row={hit.id}
            onclick={() => open(hit.id)}
            oncontextmenu={(e) => menu.show(e, hitMenu(hit))}
          >
            <div class="body">
              <div class="title">{hit.title || 'Untitled entry'}</div>
              <div class="snippet">
                {#each pieces(hit.snippet, hit.highlights) as p, i (i)}
                  {#if p.hit}<mark>{p.text}</mark>{:else}{p.text}{/if}
                {/each}
              </div>
            </div>
          </button>
        {/each}
      {/if}
    {:else if app.entries.length === 0}
      <EmptyState lead={app.showStarredOnly ? 'Nothing starred yet.' : 'No entries yet.'}>
        {#snippet note()}
          {#if app.showStarredOnly}
            The star on a row keeps it here, whichever journal it is in.
          {:else}
            One entry a day, in whichever journal it belongs to.
          {/if}
        {/snippet}
        {#snippet action()}
          {#if !app.showStarredOnly}
            <button class="btn btn-primary" onclick={() => startToday()}>
              Write today's entry
            </button>
          {/if}
        {/snippet}
      </EmptyState>
    {:else if phone.on}
      {#each rows.slice(0, shown) as r (r.key)}{@render entryRow(r)}{/each}
      {#if shown < rows.length}
        <!-- Within a screen or two of this, the next rows are drawn. -->
        <div class="sentinel" bind:this={sentinel} aria-hidden="true"></div>
      {/if}
    {:else}
      <VirtualList
        bind:this={listHandle}
        items={rows}
        getKey={(r: EntryRow) => r.key}
        selectedId={app.selectedEntry}
      >
        {#snippet children(r: EntryRow)}{@render entryRow(r)}{/snippet}
      </VirtualList>
    {/if}
  </div>
</section>

{#if pendingDelete}
  <ConfirmDialog
    title={'Delete “' + (pendingDelete.title || 'Untitled entry') + '”?'}
    detail="The entry and anything attached to it are removed. This cannot be undone."
    confirmLabel="Delete entry"
    onconfirm={remove}
    oncancel={() => (pendingDelete = null)}
  />
{/if}

<style>
  /* No background of its own: it stands on the raised surface `App.svelte`
     lays under every app, as the page beside it does. */
  .list {
    /* As tall as the stage by a stated height, not stretched: see
       `.shell-stage` in app.css. */
    height: 100%;
    width: var(--list-w);
    flex: none;
    display: flex;
    flex-direction: column;
    border-right: 1px solid var(--border);
  }

  .top {
    display: flex;
    align-items: center;
    gap: 2px;
    height: var(--header-h);
    padding: 0 var(--sp-3) 0 var(--sp-4);
    flex: none;
  }
  /* Round, the size of Mail's list-header buttons. */
  .new {
    width: 34px;
    height: 34px;
    flex: none;
    display: grid;
    place-items: center;
    border-radius: 50%;
    color: var(--fg-muted);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .new:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .new.icon {
    color: var(--fg-faint);
  }
  .new.icon.on {
    color: var(--journal-accent, var(--accent));
  }

  .rows {
    flex: 1;
    display: flex;
    flex-direction: column;
    padding-bottom: var(--sp-4);
  }

  .grouphead {
    padding: var(--sp-4) var(--sp-4) var(--sp-2);
  }

  /* The card, its hover and the open one's tint are `.list-row`'s -- see
     app.css's "Lists". This is only what sits inside it. */
  .row {
    display: flex;
    gap: var(--sp-3);
    align-items: flex-start;
    padding: var(--sp-3) var(--sp-4);
    color: inherit;
  }

  .cal {
    display: flex;
    flex-direction: column;
    align-items: center;
    width: 26px;
    flex: none;
    padding-top: 1px;
  }
  .dow {
    font-size: 10px;
    font-weight: 650;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }
  .dom {
    font-size: var(--text-md);
    font-weight: 550;
    line-height: 1.1;
    color: var(--fg-muted);
    font-variant-numeric: tabular-nums lining-nums;
  }
  .row.sel .dom {
    color: var(--fg);
  }

  .body {
    flex: 1;
    min-width: 0;
  }

  .title {
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: var(--text-base);
    font-weight: 600;
    line-height: var(--leading-snug);
    color: var(--fg);
  }
  .titletext {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .star {
    color: var(--star);
    display: flex;
  }
  .excerpt {
    margin-top: 2px;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-1);
    margin-top: var(--sp-2);
  }
  .chip,
  .place {
    font-size: var(--text-xs);
    font-weight: 500;
    color: var(--fg-faint);
    padding: 1px var(--sp-2);
    border-radius: 99px;
    background: var(--bg-sunken);
  }
  .place {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    background: none;
    padding-left: 0;
  }

  .thumb {
    width: 42px;
    height: 42px;
    flex: none;
    border-radius: var(--radius-sm);
    object-fit: cover;
    background: var(--bg-sunken);
  }

  .snippet {
    margin-top: 2px;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .snippet mark {
    background: color-mix(in oklab, var(--accent) 26%, transparent);
    color: var(--fg);
    border-radius: 2px;
  }

  .note {
    padding: var(--sp-6) var(--sp-4);
    color: var(--fg-subtle);
    font-size: var(--text-sm);
    text-align: center;
  }

  /* ── On a phone ──────────────────────────────────────────────────────── */

  /* The list is the width of the screen, the buttons in its header the size
     of a finger, and its tail clears the floating assistant button. */
  :global([data-phone]) .list {
    width: 100%;
    border-right: none;
  }
  :global([data-phone]) .top {
    padding: 0 var(--sp-2) 0 var(--sp-4);
  }
  :global([data-phone]) .new {
    width: 44px;
    height: 44px;
  }
  :global([data-phone]) .rows {
    padding-bottom: var(--fab-clear);
  }
  :global([data-phone]) .row:active {
    background: var(--bg-hover);
  }
  :global([data-phone]) .chip,
  :global([data-phone]) .place {
    font-size: var(--text-xs);
  }
  .sentinel {
    flex: none;
    height: 1px;
  }
</style>
