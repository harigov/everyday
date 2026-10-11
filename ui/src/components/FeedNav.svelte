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

  import { feed, type FeedFolder, type FeedScope } from '../lib/feed.svelte'
  import { siteOf, sourceLabel, unfollowDetail } from '../lib/feed'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { openExternal } from '../lib/open-external'
  import { pref } from '../lib/prefs'
  import type { SourceSummary } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'
  import SourceIcon from './SourceIcon.svelte'
  import SourceSettings from './SourceSettings.svelte'

  let editing = $state<SourceSummary | null>(null)
  let pendingUnfollow = $state<SourceSummary | null>(null)

  // ── Which folders are shut ────────────────────────────────────────────

  const closedPref = pref<string[]>(
    'everyday.feed.folders.closed',
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

  function folderOpen(name: string): boolean {
    return !closed.has(name) || selectedFolder === name
  }

  function toggleFolder(name: string) {
    const next = new Set(closed)
    if (folderOpen(name)) next.add(name)
    else next.delete(name)
    closed = next
    closedPref.set([...next])
  }

  // ── What the rows show ────────────────────────────────────────────────

  /** Is this the place the list is showing? */
  function isScope(scope: FeedScope): boolean {
    const now = feed.scope
    if (scope.kind === 'all') return now.kind === 'all'
    if (scope.kind === 'folder') return now.kind === 'folder' && now.name === scope.name
    return now.kind === 'source' && now.id === scope.id
  }

  function rowTitle(summary: SourceSummary): string {
    const name = sourceLabel(summary.source)
    const state = summary.source.paused ? ' (paused)' : ''
    return summary.unread > 0 ? `${name}${state} — ${summary.unread} unread` : `${name}${state}`
  }

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
      { label: 'Unfollow…', icon: 'trash', danger: true, run: () => (pendingUnfollow = summary) },
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
  <button
    class="side-row"
    class:sel={isScope({ kind: 'all' })}
    title={feed.unreadTotal > 0 ? `All — ${feed.unreadTotal} unread` : 'All'}
    onclick={() => void feed.select({ kind: 'all' })}
  >
    <span class="side-icon"><Icon name="newspaper" size={16} /></span>
    <span class="side-text">All</span>
    {#if feed.unreadTotal > 0}<span class="side-count strong">{feed.unreadTotal}</span>{/if}
  </button>

  <div class="side-head">
    <span class="eyebrow">Following</span>
    <button
      class="side-add"
      title="Follow something"
      aria-label="Follow something"
      onclick={() => feed.openAdd()}
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

  {#if feed.sources.length === 0}
    <p class="hint">Nothing followed yet. The plus above is the way in.</p>
  {/if}

  {#each feed.folders as folder (folder.name)}
    {#if folder.name === ''}
      {#each folder.sources as summary (summary.source.id)}
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
        {#each folder.sources as summary (summary.source.id)}
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
  <ConfirmDialog
    title={'Unfollow “' + sourceLabel(pendingUnfollow.source) + '”?'}
    detail={unfollowDetail(pendingUnfollow.source)}
    confirmLabel="Unfollow"
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
</style>
