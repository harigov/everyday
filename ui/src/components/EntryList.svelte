<script lang="ts">
  import { app } from '../lib/state.svelte'
  import { dayNumber, groupLabel, plural, weekdayShort } from '../lib/format'
  import { mediaUrl } from '../lib/api'
  import Icon from './Icon.svelte'
  import type { EntrySummary } from '../lib/types'

  // Group by the label the reader would use ("Today", "March"), preserving
  // the order the backend already sorted into.
  const groups = $derived.by(() => {
    const out: { label: string; rows: EntrySummary[] }[] = []
    for (const row of app.entries) {
      const label = row.pinned ? 'Pinned' : groupLabel(row.localDate)
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
      class="new"
      onclick={() => app.newEntry()}
      title="New entry (Ctrl+N)"
      aria-label="New entry"
    >
      <Icon name="plus" size={17} />
    </button>
  </header>

  <div class="searchbar">
    <span class="glass"><Icon name="search" size={15} /></span>
    <input
      class="search"
      type="search"
      placeholder="Search"
      value={app.query}
      oninput={(e) => app.setQuery(e.currentTarget.value)}
      onkeydown={(e) => {
        if (e.key === 'Escape') app.clearSearch()
      }}
    />
  </div>

  <div class="scroll rows">
    {#if app.query.trim()}
      {#if app.searching && app.results.length === 0}
        <p class="note">Searching…</p>
      {:else if app.results.length === 0}
        <p class="note">Nothing matches “{app.query}”.</p>
      {:else}
        <div class="grouphead">
          <span class="eyebrow">{plural(app.results.length, 'result')}</span>
        </div>
        {#each app.results as hit (hit.id)}
          <button
            class="row"
            class:sel={app.selectedEntry === hit.id}
            onclick={() => app.openEntry(hit.id)}
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
      <div class="blank">
        <p>No entries yet.</p>
        <button class="btn btn-primary" onclick={() => app.newEntry()}>Write the first one</button>
      </div>
    {:else}
      {#each groups as group (group.label)}
        <div class="grouphead"><span class="eyebrow">{group.label}</span></div>
        {#each group.rows as row (row.id)}
          <button
            class="row"
            class:sel={app.selectedEntry === row.id}
            onclick={() => app.openEntry(row.id)}
          >
            <span class="bar" style="background: {colorOf(row.journalId)}"></span>

            <div class="cal" aria-hidden="true">
              <span class="dow">{weekdayShort(row.localDate)}</span>
              <span class="dom">{dayNumber(row.localDate)}</span>
            </div>

            <div class="body">
              <div class="title">
                {#if row.pinned}<span class="pin" title="Pinned"
                    ><Icon name="pin" size={12} weight={1.7} /></span
                  >{/if}
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
    justify-content: space-between;
    height: 46px;
    padding: 0 var(--sp-3) 0 var(--sp-4);
    flex: none;
  }
  .heading {
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
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    color: var(--fg-muted);
  }
  .new:hover {
    background: var(--bg-hover);
    color: var(--fg);
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
    font-size: 9px;
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
  .pin {
    color: var(--fg-faint);
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
    font-size: 10px;
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
  .blank {
    padding: var(--sp-10) var(--sp-4);
    text-align: center;
  }
  .blank p {
    color: var(--fg-subtle);
    margin-bottom: var(--sp-4);
  }
</style>
