<script lang="ts">
  // The Feed app's half of the sidebar: everything you follow, in the folders
  // you put it in.
  //
  // Unfiled sources come first with nothing over them, because a folder row
  // above a lone source would be a door with one room behind it. Each folder
  // folds, and which are folded is remembered on this device the way Mail
  // remembers which accounts are open -- except the other way about: a folder
  // is open until somebody shuts it, since the point of putting a source in
  // one is to find it again, and an account's folders are rarely wanted.
  //
  // The same three menu layers as every other sidebar list: the blank space,
  // a folder, a source. What a source's menu offers is what has a command
  // behind it; the rest of what can be changed is in its settings.
  //
  // Above the following, three places that are lists of their own and not
  // sources: Up Next, what is lined up to read, hear and watch; Recommended,
  // what the Feed would pick from everything unread; and History, what has
  // been read, heard and watched, by day.
  //
  // There is one list of what is followed -- newsletters, web feeds and shows
  // together, each in its folder or unfiled -- and two ways to narrow it,
  // both over everything at the top. The chips choose a kind, and with one on
  // All is only that kind's stories: that is the store's, and is remembered
  // on this device. The search field is this sidebar's alone: it finds a row
  // by its name, or a folder by its own, and leaves the stories as they are.

  import { feed, type FeedFolder, type FeedScope } from '../lib/feed.svelte'
  import {
    FEED_KINDS,
    KIND_LABELS,
    KIND_NONE,
    NOT_NEWSLETTER_DETAIL,
    searchNav,
    siteOf,
    sourceLabel,
    unfollowDetail,
  } from '../lib/feed'
  import { upnext } from '../lib/upnext.svelte'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { openExternal } from '../lib/open-external'
  import { phone } from '../lib/phone.svelte'
  import { pref } from '../lib/prefs'
  import { sidebar } from '../lib/sidebar.svelte'
  import type { SourceSummary } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'
  import Symbol from './Symbol.svelte'
  import SourceIcon from './SourceIcon.svelte'
  import SourceSettings from './SourceSettings.svelte'

  let editing = $state<SourceSummary | null>(null)
  let pendingUnfollow = $state<SourceSummary | null>(null)

  // ── Which folders are shut ────────────────────────────────────────────

  const closedPref = pref<string[]>(
    'jatayu.feed.folders.closed',
    (raw) => {
      const parsed: unknown = raw ? JSON.parse(raw) : []
      return Array.isArray(parsed) ? parsed.filter((v): v is string => typeof v === 'string') : []
    },
    [],
    (names) => JSON.stringify(names),
  )
  let closed = $state<Set<string>>(new Set(closedPref.get()))

  /** The folder holding the open source, always unfolded: the selected row is never hidden. */
  const selectedFolder = $derived(feed.source ? feed.source.source.folder.trim() : null)

  /** Open, or showing a match: a find that hid its answer in a shut folder would be no find. */
  function folderOpen(name: string): boolean {
    return searching || !closed.has(name) || selectedFolder === name
  }

  function toggleFolder(name: string) {
    // Every folder with a match is open while one is looked for, so there is
    // nothing to fold; and a fold made now would be one nobody saw.
    if (searching) return
    const next = new Set(closed)
    if (folderOpen(name)) next.add(name)
    else next.delete(name)
    closed = next
    closedPref.set([...next])
  }

  // ── Finding a row ─────────────────────────────────────────────────────

  let search = $state('')
  const searching = $derived(search.trim() !== '')
  /** A short list is read at a glance and the top bar's search finds stories;
   *  this field narrows the sidebar's rows, so it appears once there are more
   *  rows than fit. It stays while something is typed, so it cannot vanish
   *  under the cursor as the list shrinks. */
  const LONG_LIST = 8
  const showFind = $derived(searching || feed.sources.length > LONG_LIST)

  /** The folders under the chip, as the search leaves them. */
  const shown = $derived(searchNav(feed.folders, search))

  function findKey(e: KeyboardEvent) {
    if (e.key !== 'Escape' || search === '') return
    // Clears the field and nothing else: the key is not also asked to close
    // whatever is open behind it.
    e.preventDefault()
    e.stopPropagation()
    search = ''
  }

  // ── What the rows show ────────────────────────────────────────────────

  /** Is this the place the list is showing? */
  function isScope(scope: FeedScope): boolean {
    const now = feed.scope
    if (scope.kind === 'folder') return now.kind === 'folder' && now.name === scope.name
    if (scope.kind === 'source') return now.kind === 'source' && now.id === scope.id
    return now.kind === scope.kind
  }

  function rowTitle(summary: SourceSummary): string {
    const name = sourceLabel(summary.source)
    const state = summary.source.paused ? ' (paused)' : ''
    return summary.unread > 0 ? `${name}${state} — ${summary.unread} unread` : `${name}${state}`
  }

  const upNextCount = $derived(upnext.items.length)

  // ── What a right-click offers ─────────────────────────────────────────

  /**
   * Mark everything in `scope` read.
   *
   * By opening it first: "mark all read" is the store's verb for what is in
   * view, and a menu that quietly marked a list you cannot see would be one
   * you could not trust. The Saved chip is stepped off, because "all read" on
   * a source that is showing only its starred stories would mark those and
   * say it had done the lot.
   */
  async function markScopeRead(scope: FeedScope) {
    await feed.select(scope)
    if (feed.filter === 'saved') await feed.setFilter('unread')
    await feed.markAllRead()
  }

  function sourceMenu(summary: SourceSummary): MenuItem[] {
    const id = summary.source.id
    const site = siteOf(summary.source)
    return tidyMenu([
      { label: 'Refresh', icon: 'refresh', run: () => void feed.refresh(id) },
      {
        label: 'Mark all read',
        icon: 'tick',
        disabled: summary.unread === 0,
        hint: summary.unread > 0 ? String(summary.unread) : undefined,
        run: () => void markScopeRead({ kind: 'source', id }),
      },
      SEP,
      { label: 'Settings…', icon: 'settings', run: () => (editing = summary) },
      site !== '' && { label: 'Open site', icon: 'globe', run: () => void openExternal(site) },
      SEP,
      // A newsletter the Feed found was never followed, so there is nothing to
      // unfollow: what is said is that it is not one, and it is not found again.
      {
        label: summary.source.found ? 'Not a newsletter…' : 'Unfollow…',
        icon: 'trash',
        danger: true,
        run: () => (pendingUnfollow = summary),
      },
    ])
  }

  function folderMenu(folder: FeedFolder): MenuItem[] {
    const scope: FeedScope = { kind: 'folder', name: folder.name }
    return tidyMenu([
      {
        label: 'Open',
        icon: 'newspaper',
        disabled: isScope(scope),
        run: () => void feed.select(scope),
      },
      {
        label: 'Refresh',
        icon: 'refresh',
        run: () => {
          for (const s of folder.sources) void feed.refresh(s.source.id)
        },
      },
      {
        label: 'Mark all read',
        icon: 'tick',
        disabled: folder.unread === 0,
        hint: folder.unread > 0 ? String(folder.unread) : undefined,
        run: () => void markScopeRead(scope),
      },
      SEP,
      {
        label: folderOpen(folder.name) ? 'Fold' : 'Unfold',
        icon: 'chevron',
        run: () => toggleFolder(folder.name),
      },
    ])
  }

  /** The blank space, and the heading: nothing in either is about one source. */
  function navMenu(): MenuItem[] {
    return tidyMenu([
      { label: 'Follow something…', icon: 'plus', run: () => feed.openAdd() },
      { label: 'Browse podcasts and feeds…', icon: 'compass', run: () => feed.openDirectory() },
      feed.sources.length > 0 && {
        label: 'Refresh everything',
        icon: 'refresh',
        run: () => void feed.refresh(),
      },
      feed.unreadTotal > 0 && {
        label: 'Mark all read',
        icon: 'tick',
        hint: String(feed.unreadTotal),
        run: () => void markScopeRead({ kind: 'all' }),
      },
    ])
  }
</script>

<nav class="scroll side-nav" oncontextmenu={(e) => menu.show(e, navMenu())}>
  <!-- The shared filter bar, with every kind named: there are only four, and
       each is a word. Over All, which it narrows, and over the rows. -->
  <div class="filters kinds" role="tablist" aria-label="Show">
    {#each FEED_KINDS as kind (kind)}
      <button
        class="filter"
        class:on={feed.kind === kind}
        role="tab"
        aria-selected={feed.kind === kind}
        onclick={() => void feed.setKind(kind)}
      >
        {KIND_LABELS[kind]}
      </button>
    {/each}
  </div>

  {#if showFind}
    <input
      class="field find"
      type="search"
      bind:value={search}
      placeholder="Filter sources"
      aria-label="Filter sources"
      spellcheck="false"
      autocomplete="off"
      onkeydown={findKey}
    />
  {/if}

  <button
    class="side-row"
    class:sel={isScope({ kind: 'all' })}
    title={feed.unreadTotal > 0 ? `All — ${feed.unreadTotal} unread` : 'All'}
    onclick={() => void feed.select({ kind: 'all' })}
  >
    <span class="side-icon"><Symbol name="newspaper" size={17} /></span>
    <span class="side-text">All</span>
    {#if feed.unreadTotal > 0}<span class="side-count strong">{feed.unreadTotal}</span>{/if}
  </button>

  <button
    class="side-row"
    class:sel={isScope({ kind: 'upnext' })}
    title={upNextCount > 0 ? `Up Next — ${upNextCount} lined up` : 'Up Next'}
    onclick={() => void feed.select({ kind: 'upnext' })}
  >
    <span class="side-icon"><Symbol name="play" size={17} /></span>
    <span class="side-text">Up Next</span>
    {#if upNextCount > 0}<span class="side-count">{upNextCount}</span>{/if}
  </button>

  <button
    class="side-row"
    class:sel={isScope({ kind: 'recommended' })}
    title="Recommended"
    onclick={() => void feed.select({ kind: 'recommended' })}
  >
    <span class="side-icon"><Symbol name="sparkle" size={17} /></span>
    <span class="side-text">Recommended</span>
  </button>

  <button
    class="side-row"
    class:sel={isScope({ kind: 'history' })}
    title="History: what you have read, heard and watched"
    onclick={() => void feed.select({ kind: 'history' })}
  >
    <span class="side-icon"><Symbol name="clock" size={17} /></span>
    <span class="side-text">History</span>
  </button>

  <div class="side-head">
    <span class="eyebrow">Following</span>
    <button
      class="side-add"
      title="Browse podcasts and feeds"
      aria-label="Browse podcasts and feeds"
      onclick={() => {
        // A phone's drawer goes with the choice it was opened for.
        if (phone.on) sidebar.hide()
        feed.openDirectory()
      }}
    >
      <Icon name="compass" size={15} />
    </button>
    <button
      class="side-add"
      title="Follow something"
      aria-label="Follow something"
      onclick={() => {
        if (phone.on) sidebar.hide()
        feed.openAdd()
      }}
    >
      <Icon name="plus" size={15} />
    </button>
  </div>

  {#snippet sourceRow(summary: SourceSummary, nested: boolean)}
    <button
      class="side-row"
      class:sel={isScope({ kind: 'source', id: summary.source.id })}
      class:nested
      class:paused={summary.source.paused}
      title={rowTitle(summary)}
      onclick={() => void feed.select({ kind: 'source', id: summary.source.id })}
      oncontextmenu={(e) => menu.show(e, sourceMenu(summary))}
    >
      <span class="side-icon"><SourceIcon source={summary.source} size={18} /></span>
      <span class="side-text">{sourceLabel(summary.source)}</span>
      {#if summary.lastError}
        <!-- Not the red of an alarm: a source that failed to refresh still has
             every story it already fetched, and is readable. -->
        <span
          class="side-dot"
          style:--dot="var(--danger)"
          title="Could not refresh: {summary.lastError}"
          role="img"
          aria-label="Could not refresh: {summary.lastError}"
        ></span>
      {/if}
      {#if summary.unread > 0}<span class="side-count">{summary.unread}</span>{/if}
    </button>
  {/snippet}

  {#if shown.length === 0}
    <p class="hint">
      {#if searching}
        No feeds match “{search.trim()}”.
      {:else if feed.kind === 'all'}
        Nothing followed yet. The plus above is the way in.
      {:else}
        {KIND_NONE[feed.kind]}
      {/if}
    </p>
  {/if}

  {#each shown as { folder, rows } (folder.name)}
    {#if folder.name === ''}
      {#each rows as summary (summary.source.id)}
        {@render sourceRow(summary, false)}
      {/each}
    {:else}
      {@const open = folderOpen(folder.name)}
      {@const scope = { kind: 'folder', name: folder.name } as const}
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div class="folder" oncontextmenu={(e) => menu.show(e, folderMenu(folder))}>
        <button
          class="fold"
          aria-expanded={open}
          aria-label="{open ? 'Fold' : 'Unfold'} {folder.name}"
          onclick={() => toggleFolder(folder.name)}
        >
          <span class="chev" class:down={open}><Icon name="chevron" size={12} /></span>
        </button>
        <button
          class="side-row folder-name"
          class:sel={isScope(scope)}
          title={folder.unread > 0 ? `${folder.name} — ${folder.unread} unread` : folder.name}
          onclick={() => void feed.select(scope)}
        >
          <span class="side-text">{folder.name}</span>
          {#if !open && folder.unread > 0}<span class="side-count">{folder.unread}</span>{/if}
        </button>
      </div>
      {#if open}
        {#each rows as summary (summary.source.id)}
          {@render sourceRow(summary, true)}
        {/each}
      {/if}
    {/if}
  {/each}
</nav>

{#if editing}
  <SourceSettings source={editing.source} onclose={() => (editing = null)} />
{/if}

{#if pendingUnfollow}
  {@const found = !!pendingUnfollow.source.found}
  <!-- The same delete either way; what differs is what is said about it, and
       that the service remembers a found newsletter was said not to be one. -->
  <ConfirmDialog
    title={found
      ? 'Take “' + sourceLabel(pendingUnfollow.source) + '” out of the Feed?'
      : 'Unfollow “' + sourceLabel(pendingUnfollow.source) + '”?'}
    detail={found ? NOT_NEWSLETTER_DETAIL : unfollowDetail(pendingUnfollow.source)}
    confirmLabel={found ? 'Not a newsletter' : 'Unfollow'}
    onconfirm={() => {
      const doomed = pendingUnfollow
      pendingUnfollow = null
      if (doomed) void feed.deleteSource(doomed.source.id)
    }}
    oncancel={() => (pendingUnfollow = null)}
  />
{/if}

<style>
  /* The rows, headings and counts are the shared sidebar's -- see app.css's
     "Sidebars". What is the Feed's own: a folder that folds, a source set in
     under it, and a source that is paused. */

  /* The kind chips are four, set as two rows of two: at the sidebar's width
     they used to wrap with the last one alone on a row of its own. */
  .kinds {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    margin: var(--sp-1) 0 var(--sp-2);
  }
  .kinds .filter {
    justify-content: center;
    padding: 0 6px;
    font-size: var(--text-xs);
  }

  /* The shared field, one row high like the rows it finds. */
  .find {
    height: var(--side-row-h);
    margin-bottom: var(--sp-2);
    font-size: var(--text-sm);
  }

  /* A source inside a folder, set in under the folder's name. */
  .side-row.nested {
    padding-left: 30px;
  }

  /* Paused is quiet, not gone: it can still be opened, refreshed by hand and
     read. */
  .side-row.paused {
    opacity: 0.55;
  }

  .folder {
    display: flex;
    align-items: center;
    gap: 2px;
    margin-top: 2px;
  }
  .fold {
    flex: none;
    display: grid;
    place-items: center;
    width: 22px;
    height: var(--side-row-h);
    border-radius: var(--radius);
    color: var(--fg-faint);
  }
  .fold:hover {
    color: var(--fg);
  }
  .chev {
    display: grid;
    place-items: center;
    transition: transform var(--fast) var(--ease);
  }
  .chev.down {
    transform: rotate(90deg);
  }
  .folder-name {
    flex: 1;
    min-width: 0;
    width: auto;
    padding-left: var(--sp-2);
    font-weight: 600;
    color: var(--fg);
  }

  .hint {
    padding: var(--sp-3);
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }

  /* The folder's fold button: a finger's width. */
  :global([data-phone]) .fold {
    width: 36px;
  }
</style>
