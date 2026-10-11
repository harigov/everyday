<script lang="ts">
  // The Feed's list column: what the sidebar has selected, newest first, as
  // rows you read down and finish.
  //
  // `VirtualList`-backed, so a source with ten thousand stories costs the same
  // DOM as one with ten -- the shape Mail's thread list has, and for the same
  // reason. The page after this one is asked for as the end comes into view.
  //
  // The chips over it are the one choice a feed reader has to make all day:
  // what is still unread, everything, or what was kept. Unread is first and
  // the default, because the point of a feed is to get to the end of it.

  import { feed, FEED_FILTERS } from '../lib/feed.svelte'
  import {
    FILTER_LABELS,
    bylineParts,
    hostOf,
    isHackerNews,
    pointsLine,
    scopeTitle,
    scopeUnread,
    sourceLabel,
  } from '../lib/feed'
  import { relativeTime } from '../lib/format'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import type { Story } from '../lib/types'
  import Avatar from './Avatar.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import PaneResizer from './PaneResizer.svelte'
  import SourceIcon from './SourceIcon.svelte'
  import VirtualList from './VirtualList.svelte'

  const rows = $derived(feed.visible)
  const title = $derived(scopeTitle(feed.scope, feed.sources))
  const unread = $derived(scopeUnread(feed.scope, feed.sources))
  const sub = $derived.by(() => {
    if (feed.sources.length === 0) return ''
    return unread > 0 ? `${unread.toLocaleString()} unread` : 'Nothing unread'
  })
  /** Points only mean something on a Hacker News source, so only there can they sort. */
  const canSortByPoints = $derived(isHackerNews(feed.source?.source))
  const anyUnread = $derived(rows.some((s) => !s.read))
  const searching = $derived(feed.query.trim() !== '')
  const caughtUpNote = $derived(
    feed.scope.kind === 'all' ? 'Nothing is unread.' : `Nothing in ${title} is unread.`,
  )
  /** What to try when a search finds nothing: Unread and Saved leave a lot out. */
  const searchNote = $derived(
    feed.filter === 'all'
      ? 'Try fewer words.'
      : 'Try fewer words, or choose All to look through what you have read.',
  )

  /** Ask the network for what is in view: a source, a folder's worth, or everything. */
  function refreshScope() {
    const scope = feed.scope
    if (scope.kind === 'source') {
      void feed.refresh(scope.id)
    } else if (scope.kind === 'folder') {
      const inFolder = feed.folders.find((f) => f.name === scope.name)?.sources ?? []
      void Promise.all(inFolder.map((s) => feed.refresh(s.source.id)))
    } else {
      void feed.refresh()
    }
  }

  function storyTitle(story: Story): string {
    return story.title.trim() || hostOf(story.url) || 'Untitled'
  }

  /** "Source · author · 3 hours ago". */
  function meta(story: Story): string {
    const source = feed.sourceOf(story)
    return [
      ...bylineParts(story, source ? sourceLabel(source.source) : ''),
      relativeTime(story.publishedAt),
    ].join(' · ')
  }

  function rowKey(story: Story): string {
    return story.id
  }

  // ── What a right-click offers ─────────────────────────────────────────

  function rowMenu(story: Story): MenuItem[] {
    return tidyMenu([
      { label: 'Open', icon: 'newspaper', run: () => void feed.open(story.id) },
      {
        label: story.read ? 'Mark unread' : 'Mark read',
        icon: 'tick',
        run: () => void feed.toggleRead(story.id),
      },
      {
        label: story.saved ? 'Unsave' : 'Save',
        icon: 'star',
        run: () => void feed.toggleSaved(story.id),
      },
      { label: 'Add to Library', icon: 'book', run: () => void feed.addToLibrary(story.id) },
      SEP,
      !!story.url && {
        label: 'Open original',
        icon: 'link',
        run: () => void feed.openOriginal(story.id),
      },
      !!story.discussionUrl && {
        label: 'Open discussion',
        icon: 'link',
        run: () => void feed.openDiscussion(story.id),
      },
    ])
  }

  /** The column itself, where there is no row under the pointer. */
  function listMenu(): MenuItem[] {
    return tidyMenu([
      { label: 'Refresh', icon: 'refresh', hint: 'R', run: refreshScope },
      {
        label: 'Mark all read',
        icon: 'tick',
        hint: 'M',
        disabled: !anyUnread,
        run: () => void feed.markAllRead(),
      },
      SEP,
      {
        label: 'Show',
        icon: 'eye',
        items: FEED_FILTERS.map((f) => ({
          label: FILTER_LABELS[f],
          checked: feed.filter === f,
          run: () => void feed.setFilter(f),
        })),
      },
      canSortByPoints && {
        label: 'Sort by',
        icon: 'layers',
        items: [
          { label: 'Newest', checked: feed.sort === 'newest', run: () => feed.setSort('newest') },
          { label: 'Points', checked: feed.sort === 'points', run: () => feed.setSort('points') },
        ],
      },
    ])
  }
</script>

<section class="list" style:--tint={feed.accent}>
  <div class="top">
    <div class="pane-titles">
      <span class="pane-title">{title}</span>
      {#if sub}<span class="pane-sub">{sub}</span>{/if}
    </div>
    <button
      class="plus"
      class:spinning={feed.refreshing}
      title={feed.refreshing ? 'Refreshing…' : 'Refresh (R)'}
      aria-label={feed.refreshing ? 'Refreshing' : 'Refresh'}
      aria-busy={feed.refreshing}
      onclick={refreshScope}
    >
      <Icon name="refresh" size={17} />
    </button>
    <button
      class="plus"
      title="Mark everything in view read (M)"
      aria-label="Mark all read"
      disabled={!anyUnread}
      onclick={() => void feed.markAllRead()}
    >
      <Icon name="tick" size={17} />
    </button>
  </div>

  <div class="tabs">
    <div class="filters" role="tablist" aria-label="Show">
      {#each FEED_FILTERS as filter (filter)}
        <button
          class="filter"
          class:on={feed.filter === filter}
          role="tab"
          aria-selected={feed.filter === filter}
          onclick={() => void feed.setFilter(filter)}
        >
          {FILTER_LABELS[filter]}
        </button>
      {/each}
    </div>
    {#if canSortByPoints}
      <div class="filters" role="group" aria-label="Sort by">
        <button
          class="filter"
          class:on={feed.sort === 'newest'}
          aria-pressed={feed.sort === 'newest'}
          onclick={() => feed.setSort('newest')}
        >
          Newest
        </button>
        <button
          class="filter"
          class:on={feed.sort === 'points'}
          aria-pressed={feed.sort === 'points'}
          onclick={() => feed.setSort('points')}
        >
          Points
        </button>
      </div>
    {/if}
  </div>

  {#if feed.loading && feed.stories.length === 0}
    <p class="hint">Loading…</p>
  {:else if rows.length === 0}
    {#if searching}
      <EmptyState lead={`Nothing matches “${feed.query.trim()}”.`}>
        {#snippet note()}{searchNote}{/snippet}
      </EmptyState>
    {:else if feed.filter === 'saved'}
      <EmptyState lead="Nothing saved here.">
        {#snippet note()}The star on a story keeps it for good, whatever its age.{/snippet}
      </EmptyState>
    {:else if feed.filter === 'unread'}
      <EmptyState lead="You are all caught up.">
        {#snippet note()}{caughtUpNote}{/snippet}
        {#snippet action()}
          <button class="btn" onclick={() => void feed.setFilter('all')}>Show everything</button>
        {/snippet}
      </EmptyState>
    {:else}
      <EmptyState lead={feed.refreshing ? 'Fetching stories…' : 'Nothing here yet.'}>
        {#snippet note()}Stories appear once a source has been refreshed.{/snippet}
        {#snippet action()}
          <button class="btn" disabled={feed.refreshing} onclick={refreshScope}>Refresh</button>
        {/snippet}
      </EmptyState>
    {/if}
  {:else}
    <!-- The scrolling parent `VirtualList` needs: `virtua` watches its
         container's parent for scroll, and `section.list` itself never
         scrolls. -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="scroll story-rows" oncontextmenu={(e) => menu.show(e, listMenu())}>
      <VirtualList
        items={rows}
        getKey={rowKey}
        selectedId={feed.selected}
        onEndReached={() => void feed.loadMore()}
      >
        {#snippet children(story: Story)}
          {@render storyRow(story)}
        {/snippet}
      </VirtualList>
      {#if feed.next}
        <!-- The scroll cue above only fires on a scroll, and a narrow search
             can leave the column too short to scroll at all. -->
        <button class="load-more" disabled={feed.loading} onclick={() => void feed.loadMore()}>
          {feed.loading ? 'Loading…' : 'Load more'}
        </button>
      {/if}
    </div>
  {/if}

  {#if feed.note}
    <p class="note" role="status">{feed.note}</p>
  {/if}

  <PaneResizer
    cssVar="--feed-list-w"
    storageKey="pane-w:feed-list"
    defaultWidth={380}
    min={300}
    max={640}
  />
</section>

<!-- One story in the list. The unread dot sits in the gutter left of the
     face, where the eye starts; the title is the loud thing, bold while the
     story is unread; and the source, the author and the age sit quietly under
     it, with two lines of what it says. -->
{#snippet storyRow(story: Story)}
  {@const source = feed.sourceOf(story)}
  {@const points = pointsLine(story)}
  <button
    class="row list-row"
    class:sel={feed.selected === story.id}
    class:unread={!story.read}
    onclick={() => void feed.open(story.id)}
    oncontextmenu={(e) => menu.show(e, rowMenu(story))}
  >
    <span class="dot" class:on={!story.read} aria-hidden="true"></span>
    <span class="face">
      {#if source}
        <SourceIcon source={source.source} size={34} />
      {:else}
        <Avatar name={story.author || story.title} size={34} />
      {/if}
    </span>
    <div class="body">
      <div class="line1">
        <span class="title">{storyTitle(story)}</span>
        {#if story.saved}
          <span class="mark" title="Saved"><Icon name="star" size={13} filled /></span>
        {/if}
      </div>
      <div class="meta">{meta(story)}</div>
      {#if story.summary}<p class="summary">{story.summary}</p>{/if}
      {#if points}<div class="points">{points}</div>{/if}
    </div>
  </button>
{/snippet}

<style>
  .list {
    position: relative;
    /* Its own variable, so a drag here leaves Mail's and the journal's lists
       alone. The fallback is the width it opens at before anything is stored. */
    width: var(--feed-list-w, 380px);
    flex: none;
    display: flex;
    flex-direction: column;
    border-right: 1px solid var(--border);
  }
  .top {
    display: flex;
    align-items: center;
    gap: 2px;
    min-height: var(--header-h);
    padding: var(--sp-2) var(--sp-3) var(--sp-1) var(--sp-5);
    flex: none;
  }
  .plus {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    border-radius: 50%;
    color: var(--fg-muted);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .plus:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .plus:disabled {
    opacity: 0.4;
    pointer-events: none;
  }
  /* A refresh in progress: the icon turns, the button stays put. Stopped
     outright under reduced motion -- the title still says "Refreshing…". */
  .plus.spinning :global(svg) {
    animation: spin 1s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .plus.spinning :global(svg) {
      animation: none;
    }
  }

  /* The shared chips, wrapping rather than clipping: on a Hacker News source
     there are two sets of them, and a narrow column puts the second under the
     first. */
  .tabs {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: var(--sp-2);
    flex: none;
    padding: var(--sp-1) var(--sp-4) var(--sp-3);
  }

  .hint {
    padding: var(--sp-4) var(--sp-5);
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }
  .note {
    flex: none;
    margin: 0;
    padding: var(--sp-2) var(--sp-5);
    border-top: 1px solid var(--border);
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .load-more {
    width: 100%;
    padding: var(--sp-3);
    text-align: center;
    color: var(--accent);
    font-size: var(--text-sm);
  }
  .load-more:hover:not(:disabled) {
    background: var(--bg-hover);
  }

  /* ── Rows ───────────────────────────────────────────────────────── */

  .story-rows {
    flex: 1;
    min-height: 0;
    padding-bottom: var(--sp-2);
  }

  /* Each row is the shared `.list-row` card (app.css's "Lists"); what is
     here is the story row's own layout inside it. */
  .row {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    padding: var(--sp-3) var(--sp-3) var(--sp-3) var(--sp-5);
  }
  /* The hairline between rows, starting under the text rather than the face,
     so the column of icons stays one unbroken edge. */
  .row::after {
    content: '';
    position: absolute;
    left: calc(var(--sp-5) + 34px + var(--sp-3));
    right: var(--sp-3);
    bottom: 0;
    height: 1px;
    background: var(--border);
  }
  .row.sel::after,
  .row:hover::after {
    opacity: 0;
  }
  .dot {
    position: absolute;
    left: 7px;
    top: calc(var(--sp-3) + 13px);
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: transparent;
  }
  .dot.on {
    background: var(--accent);
  }
  .face {
    flex: none;
    display: flex;
    margin-top: 2px;
  }

  .body {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .line1 {
    display: flex;
    align-items: flex-start;
    gap: 6px;
    min-width: 0;
  }
  .title {
    flex: 1;
    min-width: 0;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    font-size: var(--text-base);
    font-weight: 520;
    line-height: 1.3;
    color: var(--fg-muted);
    overflow-wrap: anywhere;
  }
  .row.unread .title {
    font-weight: 680;
    color: var(--fg);
  }
  .mark {
    flex: none;
    display: grid;
    place-items: center;
    margin-top: 2px;
    color: var(--accent);
  }
  .meta {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .summary {
    margin: 2px 0 0;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    font-size: var(--text-sm);
    line-height: 1.4;
    color: var(--fg-subtle);
    overflow-wrap: anywhere;
  }
  .points {
    margin-top: 2px;
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
</style>
