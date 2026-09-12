<script lang="ts">
  import { app } from '../lib/state.svelte'
  import { dayNumber, groupLabel, plural, weekdayShort } from '../lib/format'
  import { mediaUrl } from '../lib/api'
  import { menu } from '../lib/menu.svelte'
  import { onOffPref } from '../lib/prefs'
  import { rovingFocus } from '../lib/roving'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { purposeItems } from '../lib/menus'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import EmptyState from './EmptyState.svelte'
  import EntryCalendar from './EntryCalendar.svelte'
  import Icon from './Icon.svelte'
  import type { EntryId, EntrySummary, JournalId } from '../lib/types'

  let pendingDelete = $state<{ id: EntryId; title: string } | null>(null)

  const showCalendarPref = onOffPref('everyday.journal.calendar')

  /**
   * Whether the month is showing above the list.
   *
   * Remembered locally, like every other piece of chrome state: somebody who
   * navigates by date wants it there every time, and somebody who reads down
   * the list wants the height back.
   */
  let showCalendar = $state(showCalendarPref.get())
  function toggleCalendar() {
    showCalendar = !showCalendar
    showCalendarPref.set(showCalendar)
  }

  async function remove() {
    const doomed = pendingDelete
    pendingDelete = null
    if (doomed) await app.deleteEntry(doomed.id)
  }

  // Group by the label the reader would use ("Today", "March"), preserving
  // the order the backend already sorted into.
  const groups = $derived.by(() => {
    const out: { label: string; rows: EntrySummary[] }[] = []
    for (const row of app.entries) {
      const label = groupLabel(row.localDate)
      const last = out[out.length - 1]
      if (last && last.label === label) last.rows.push(row)
      else out.push({ label, rows: [row] })
    }
    return out
  })

  const heading = $derived(app.showStarredOnly ? 'Starred' : (app.journal?.name ?? 'All entries'))

  function colorOf(id: string): string {
    return app.journals.find((j) => j.id === id)?.color ?? 'var(--accent)'
  }

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
      { label: 'Open', icon: 'quote', run: () => app.openEntry(row.id) },
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
      { label: 'Open', icon: 'quote', run: () => app.openEntry(hit.id) },
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
      { label: "Today's entry", icon: 'plus', hint: 'Ctrl+N', run: () => app.newEntry() },
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

<section class="list">
  <header class="top">
    <h1 class="heading">{heading}</h1>
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
      onclick={() => app.newEntry()}
      title="Today's entry (Ctrl+N)"
      aria-label="Today's entry"
    >
      <Icon name="plus" size={17} />
    </button>
  </header>

  <div class="searchbar">
    <span class="glass"><Icon name="search" size={15} /></span>
    <input
      class="search"
      data-search
      type="search"
      placeholder="Search"
      value={app.query}
      oninput={(e) => app.setQuery(e.currentTarget.value)}
      onkeydown={(e) => {
        if (e.key === 'Escape') app.clearSearch()
      }}
    />
  </div>

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
    oncontextmenu={(e) => menu.show(e, listMenu())}
    use:rovingFocus={app.selectedEntry}
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
            class="row"
            class:sel={app.selectedEntry === hit.id}
            data-row={hit.id}
            onclick={() => app.openEntry(hit.id)}
            oncontextmenu={(e) => menu.show(e, hitMenu(hit))}
          >
            <span class="bar" style="background: {colorOf(hit.journalId)}"></span>
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
            <button class="btn btn-primary" onclick={() => app.newEntry()}>
              Write today's entry
            </button>
          {/if}
        {/snippet}
      </EmptyState>
    {:else}
      {#each groups as group (group.label)}
        <div class="grouphead"><span class="eyebrow">{group.label}</span></div>
        {#each group.rows as row (row.id)}
          <button
            class="row"
            class:sel={app.selectedEntry === row.id}
            data-row={row.id}
            onclick={() => app.openEntry(row.id)}
            oncontextmenu={(e) => menu.show(e, entryMenu(row))}
          >
            <span class="bar" style="background: {colorOf(row.journalId)}"></span>

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
                    <span class="place"
                      ><Icon name="place" size={12} weight={1.6} /> {row.place}</span
                    >
                  {/if}
                  {#each row.tags.slice(0, 3) as tag (tag)}<span class="chip">{tag}</span>{/each}
                </div>
              {/if}
            </div>

            {#if row.cover}
              <img class="thumb" src={mediaUrl(row.cover)} alt="" loading="lazy" />
            {/if}
          </button>
        {/each}
      {/each}
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
  .list {
    width: var(--list-w);
    flex: none;
    display: flex;
    flex-direction: column;
    background: var(--bg-panel);
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
  .heading {
    flex: 1;
    min-width: 0;
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .new {
    width: 30px;
    height: 30px;
    flex: none;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    color: var(--fg-muted);
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

  .searchbar {
    position: relative;
    padding: 0 var(--sp-3) var(--sp-3);
    flex: none;
  }
  .glass {
    position: absolute;
    left: calc(var(--sp-3) + 9px);
    top: 8px;
    color: var(--fg-faint);
    pointer-events: none;
  }
  .search {
    width: 100%;
    height: 30px;
    padding: 0 var(--sp-3) 0 30px;
    border: 1px solid transparent;
    border-radius: var(--radius);
    background: var(--bg-sunken);
    color: var(--fg);
    font-size: var(--text-base);
    user-select: text;
    transition:
      border-color var(--fast) var(--ease),
      background var(--fast) var(--ease);
  }
  .search::placeholder {
    color: var(--fg-faint);
  }
  .search::-webkit-search-cancel-button {
    -webkit-appearance: none;
  }
  .search:focus {
    outline: none;
    border-color: var(--accent);
    background: var(--bg-raised);
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

  .row {
    position: relative;
    display: flex;
    gap: var(--sp-3);
    align-items: flex-start;
    width: 100%;
    padding: var(--sp-3) var(--sp-4);
    text-align: left;
    color: inherit;
    transition: background var(--fast) var(--ease);
  }
  .row:hover {
    background: var(--bg-hover);
  }
  .row.sel {
    background: var(--bg-selected);
  }

  .bar {
    position: absolute;
    left: 0;
    top: 6px;
    bottom: 6px;
    width: 2px;
    border-radius: 0 2px 2px 0;
    opacity: 0;
    transition: opacity var(--fast) var(--ease);
  }
  .row.sel .bar {
    opacity: 1;
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
    color: #e0a92b;
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
</style>
