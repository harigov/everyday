<script lang="ts">
  // Every app and site ever seen, and what you have said about each: its
  // name, its category, whether it counts at all, and -- for a site --
  // whether its subdomains count as one.
  //
  // Edited where they stand, the way a list of labels wants to be: the name
  // is a field that looks like text until it is clicked, the category is the
  // same chip the summary uses, and ignoring is one button. Nothing waits on
  // a Save; each change is a label patch, shown at once and put back by the
  // store if the vault refuses it.
  //
  // Ordered by the last thirty days' time, most first, because the items
  // worth a label are the ones that take the time. The two things that
  // cannot be undone with a click -- dropping a label and forgetting an
  // item's history -- are on the row's menu, and the second asks first.

  import { formatDuration, keyText, rowName } from '../lib/screentime'
  import { screentime, type LabelPatch } from '../lib/screentime.svelte'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { relativeTime } from '../lib/format'
  import type { ScreenAppRow, ScreenCategoryId } from '../lib/types'
  import Icon from './Icon.svelte'
  import ScreenTimeCategoryPicker from './ScreenTimeCategoryPicker.svelte'

  type Filter = 'all' | 'apps' | 'sites' | 'uncategorised' | 'ignored'
  const FILTERS: { id: Filter; label: string }[] = [
    { id: 'all', label: 'All' },
    { id: 'apps', label: 'Apps' },
    { id: 'sites', label: 'Sites' },
    { id: 'uncategorised', label: 'Uncategorised' },
    { id: 'ignored', label: 'Ignored' },
  ]

  let filter = $state<Filter>('all')
  let query = $state('')

  /** The category a row counts under, and whether that is only a suggestion. */
  function categoryOf(row: ScreenAppRow): { id: ScreenCategoryId | null; suggested: boolean } {
    const label = row.label
    if (label?.uncategorized) return { id: null, suggested: false }
    if (label?.category) return { id: label.category, suggested: false }
    return { id: row.suggested, suggested: row.suggested !== null }
  }

  function matches(row: ScreenAppRow, f: Filter): boolean {
    switch (f) {
      case 'apps':
        return row.kind === 'app'
      case 'sites':
        return row.kind === 'site'
      case 'uncategorised':
        return categoryOf(row).id === null && !row.label?.ignored
      case 'ignored':
        return !!row.label?.ignored
      default:
        return true
    }
  }

  const counts = $derived(
    Object.fromEntries(
      FILTERS.map((f) => [f.id, screentime.apps.filter((r) => matches(r, f.id)).length]),
    ) as Record<Filter, number>,
  )

  const needle = $derived(query.trim().toLowerCase())
  const rows = $derived(
    screentime.apps
      .filter((r) => matches(r, filter))
      .filter(
        (r) =>
          needle === '' ||
          rowName(r).toLowerCase().includes(needle) ||
          r.key.toLowerCase().includes(needle) ||
          r.hosts.some((h) => h.toLowerCase().includes(needle)),
      )
      .sort((a, b) => b.recentSecs - a.recentSecs || rowName(a).localeCompare(rowName(b))),
  )

  function rename(row: ScreenAppRow, input: HTMLInputElement) {
    const next = input.value.trim()
    const now = row.label?.name ?? ''
    if (next === now || (next === row.defaultName && now === '')) {
      input.value = rowName(row)
      return
    }
    // Typing the collector's own name back, or nothing, goes back to it.
    void screentime.saveLabel(row.key, row.kind, { name: next === row.defaultName ? '' : next })
  }

  function nameKeys(row: ScreenAppRow, e: KeyboardEvent) {
    const input = e.currentTarget as HTMLInputElement
    if (e.key === 'Enter') {
      e.preventDefault()
      input.blur()
    } else if (e.key === 'Escape') {
      e.preventDefault()
      e.stopPropagation()
      input.value = rowName(row)
      input.blur()
    }
  }

  function rowMenu(row: ScreenAppRow): MenuItem[] {
    const name = rowName(row)
    return tidyMenu([
      {
        label: row.label?.ignored ? 'Count it again' : 'Ignore',
        icon: row.label?.ignored ? 'eye' : 'hidden',
        run: () => void screentime.saveLabel(row.key, row.kind, { ignored: !row.label?.ignored }),
      },
      SEP,
      {
        label: 'Reset label',
        icon: 'refresh',
        disabled: !row.label,
        hint: row.label ? undefined : 'none set',
        run: () => void screentime.clearLabel(row.key),
      },
      {
        label: 'Forget history…',
        icon: 'trash',
        danger: true,
        run: () => screentime.forgetItem(row.key, name),
      },
    ])
  }
</script>

<div class="apps">
  <div class="toolbar">
    <div class="toolbar-end">
      <label class="search">
        <Icon name="search" size={14} />
        <input
          type="search"
          bind:value={query}
          placeholder="Find an app or site"
          aria-label="Find an app or site"
          spellcheck="false"
          autocomplete="off"
        />
      </label>
    </div>
    <div class="filters" role="group" aria-label="Show">
      {#each FILTERS as f (f.id)}
        <button
          class="filter"
          class:on={filter === f.id}
          aria-pressed={filter === f.id}
          onclick={() => (filter = f.id)}
        >
          {f.label}
          {#if f.id !== 'all' && counts[f.id] > 0}<span class="n">{counts[f.id]}</span>{/if}
        </button>
      {/each}
    </div>
    <div class="toolbar-end right"></div>
  </div>

  <div class="list scroll">
    {#if screentime.apps.length === 0}
      <p class="quiet">No apps or sites seen yet. They appear here as they are used.</p>
    {:else if rows.length === 0}
      <p class="quiet">
        {needle ? `Nothing matches “${query.trim()}”.` : 'Nothing here.'}
      </p>
    {:else}
      <ul>
        {#each rows as row (row.key)}
          {@const cat = categoryOf(row)}
          {@const name = rowName(row)}
          {@const ignored = !!row.label?.ignored}
          <li>
            <!-- The menu is also the ⋯ button's, so the keyboard reaches it. -->
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div class="list-row" class:ignored oncontextmenu={(e) => menu.show(e, rowMenu(row))}>
              <span class="glyph" aria-hidden="true">
                <Icon name={row.kind === 'site' ? 'globe' : 'monitor'} size={15} />
              </span>

              <div class="who">
                <input
                  class="name"
                  value={name}
                  aria-label="Name of {keyText(row.key)}"
                  title="Rename"
                  spellcheck="false"
                  autocomplete="off"
                  onblur={(e) => rename(row, e.currentTarget)}
                  onkeydown={(e) => nameKeys(row, e)}
                />
                <span class="key">
                  {keyText(row.key)}{#if row.lastSeen}&nbsp;· seen {relativeTime(row.lastSeen)}{/if}
                </span>
                {#if row.kind === 'site' && row.hosts.length > 1}
                  <label class="split">
                    <input
                      type="checkbox"
                      checked={!!row.label?.split}
                      onchange={(e) =>
                        void screentime.saveLabel(row.key, row.kind, {
                          split: e.currentTarget.checked,
                        })}
                    />
                    <span>Count subdomains separately</span>
                    <span class="hosts" title={row.hosts.join(', ')}>{row.hosts.join(', ')}</span>
                  </label>
                {/if}
              </div>

              <span class="secs" title="The last 30 days">
                {row.recentSecs > 0 ? formatDuration(row.recentSecs) : '—'}
              </span>

              <div class="cat">
                <ScreenTimeCategoryPicker
                  value={cat.id}
                  suggested={cat.suggested}
                  suggestion={row.suggested}
                  itemName={name}
                  align="right"
                  disabled={ignored}
                  onchange={(patch: LabelPatch) => screentime.saveLabel(row.key, row.kind, patch)}
                />
              </div>

              <button
                class="icon-btn"
                class:on={ignored}
                aria-pressed={ignored}
                aria-label={ignored ? `Count ${name} again` : `Ignore ${name}`}
                title={ignored
                  ? 'Ignored: left out of every total. Count it again'
                  : 'Ignore: leave it out of every total'}
                onclick={() => void screentime.saveLabel(row.key, row.kind, { ignored: !ignored })}
              >
                <Icon name={ignored ? 'hidden' : 'eye'} size={15} />
              </button>
              <button
                class="icon-btn"
                aria-label="More for {name}"
                title="More"
                onclick={(e) => menu.show(e, rowMenu(row))}
              >
                <Icon name="more" size={15} />
              </button>
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  </div>
</div>

<style>
  .apps {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .search {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    max-width: 240px;
    height: 32px;
    padding: 0 var(--sp-3);
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--bg);
    color: var(--fg-faint);
  }
  .search:focus-within {
    border-color: var(--accent);
  }
  .search input {
    flex: 1;
    min-width: 0;
    border: none;
    background: none;
    color: var(--fg);
    font-size: var(--text-sm);
    outline: none;
    user-select: text;
  }

  .list {
    flex: 1;
    min-height: 0;
    padding-bottom: var(--fab-clear);
  }
  ul {
    max-width: 960px;
    margin: 0 auto;
    padding: 0;
    list-style: none;
  }

  .list-row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-2) var(--sp-3);
  }
  .list-row.ignored .who,
  .list-row.ignored .secs,
  .list-row.ignored .glyph {
    opacity: 0.5;
  }

  .glyph {
    flex: none;
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
    color: var(--fg-subtle);
  }

  .who {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  /* A field that reads as the name until it is clicked. */
  .name {
    width: 100%;
    min-width: 0;
    margin-left: -5px;
    padding: 1px 4px;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-base);
    text-overflow: ellipsis;
    user-select: text;
  }
  .name:hover {
    border-color: var(--border);
  }
  .name:focus {
    outline: none;
    border-color: var(--accent);
    background: var(--bg);
  }
  .key {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }

  .split {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
    margin-top: 2px;
    font-size: var(--text-xs);
    color: var(--fg-muted);
    cursor: pointer;
  }
  .split input {
    flex: none;
    margin: 0;
    accent-color: var(--accent);
  }
  .split > span:not(.hosts) {
    flex: none;
  }
  .hosts {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg-faint);
  }

  .secs {
    flex: none;
    width: 6em;
    text-align: right;
    color: var(--fg-muted);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
  }
  .cat {
    flex: none;
    width: 9.5em;
    display: flex;
    justify-content: flex-end;
  }

  .icon-btn {
    flex: none;
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .icon-btn:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .icon-btn.on {
    color: var(--fg);
    background: var(--bg-active);
  }

  .quiet {
    margin: 0;
    padding: var(--sp-8) var(--sp-6);
    text-align: center;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
</style>
