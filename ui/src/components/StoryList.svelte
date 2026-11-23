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
  //
  // A row that is an episode says what is true of an episode -- how long it
  // is, how far along, whether it is downloaded, whether it is lined up --
  // and ends in the round control a podcast app has there. It is kept to
  // reads of what the stores already hold, once per row drawn: this list is
  // virtualised so that ten thousand stories cost the DOM of ten, and a row
  // that did work of its own would take that back.

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
  import { player } from '../lib/player.svelte'
  import { downloadLine, episodeProgress, formatDuration, workLine } from '../lib/podcast'
  import { upnext } from '../lib/upnext.svelte'
  import type { ContentRef, FileId, Story } from '../lib/types'
  import Avatar from './Avatar.svelte'
  import EmptyState from './EmptyState.svelte'
  import { openTranscript, playEpisode } from '../lib/episodes'
  import EpisodeActions from './EpisodeActions.svelte'
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
    } else if (scope.kind === 'podcasts') {
      void Promise.all(feed.podcastSources.map((s) => feed.refresh(s.source.id)))
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

  /** Open a transcript from a menu; one that is gone is said under the list. */
  async function openFromMenu(file: FileId) {
    if (!(await openTranscript(file))) {
      feed.note = 'That transcript is no longer in Files. Transcribe the episode to make it again.'
    }
  }

  /** The listening half of a story's menu: an episode's play and download, and Up Next for any. */
  function listeningMenu(story: Story): (MenuItem | false)[] {
    const ref: ContentRef = { type: 'story', id: story.id }
    const queued = upnext.isQueued(ref)
    const queueItem: MenuItem = queued
      ? { label: 'Take out of Up Next', icon: 'queue-add', run: () => void upnext.removeFor(ref) }
      : { label: 'Add to Up Next', icon: 'queue-add', run: () => void upnext.add(ref) }
    if (!story.media) {
      // An article: it can be saved as audio, and played once it has been.
      const audio = upnext.workFor({ story: story.id }, 'audio')
      // Only what the vault was last heard to say about the file: a row is not
      // looked up, and a menu is raised by a click, so it asks for the next one.
      const saved = upnext.linkedFile(story.files?.audio) !== null
      void upnext.checkLinks([story.files?.audio])
      return [
        saved && {
          label: player.isPlaying(ref) ? 'Pause audio' : 'Play audio',
          icon: player.isPlaying(ref) ? 'pause' : 'play',
          run: () => playEpisode(story),
        },
        !saved &&
          !story.expired &&
          (audio && audio.state !== 'failed'
            ? { label: workLine(audio), icon: 'volume', disabled: true }
            : {
                label: 'Save as audio',
                icon: 'volume',
                run: () => void upnext.renderAudio(story.id),
              }),
        queueItem,
      ]
    }
    const transcript = upnext.linkedFile(story.files?.transcript)
    void upnext.checkLinks([story.files?.transcript])
    const transcribing = upnext.workFor({ story: story.id }, 'transcript')
    const writing = upnext.workFor({ story: story.id }, 'article')
    const downloaded = !!story.files?.media
    const download = upnext.downloadOf(story.id)
    const running = download?.state === 'downloading' || download?.state === 'waiting'
    const played = !!upnext.progressOf(ref)?.finished
    return [
      downloaded
        ? {
            label: player.isPlaying(ref) ? 'Pause' : 'Play',
            icon: player.isPlaying(ref) ? 'pause' : 'play',
            run: () => playEpisode(story),
          }
        : running
          ? {
              label: 'Cancel download',
              icon: 'close',
              run: () => void upnext.cancelDownload(story.id),
            }
          : { label: 'Download', icon: 'download', run: () => void upnext.download(story.id) },
      queueItem,
      {
        label: played ? 'Mark unplayed' : 'Mark played',
        icon: 'tick',
        run: () => void upnext.markFinished(ref, !played),
      },
      downloaded && {
        label: 'Remove download',
        icon: 'trash',
        run: () => void upnext.removeDownload(story.id),
      },
      SEP,
      transcript
        ? { label: 'Open transcript', icon: 'quote', run: () => void openFromMenu(transcript) }
        : transcribing && transcribing.state !== 'failed'
          ? { label: workLine(transcribing), icon: 'quote', disabled: true }
          : { label: 'Transcribe', icon: 'quote', run: () => void upnext.transcribe(story.id) },
      writing && writing.state !== 'failed'
        ? { label: workLine(writing), icon: 'newspaper', disabled: true }
        : {
            label: 'Read as an article',
            icon: 'newspaper',
            run: () => void upnext.readAsArticle(story),
          },
    ]
  }

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
      ...listeningMenu(story),
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
     it, with two lines of what it says.

     An episode adds a line of what is true of it, and the round control at the
     end. That control is beside the row's button and not in it -- a button
     holding a button is not something a browser or a screen reader will
     treat as two -- so the row is wrapped, and the wrapper is what the
     context menu belongs to, so a right-click on the control opens the row's
     menu too. -->
{#snippet storyRow(story: Story)}
  {@const source = feed.sourceOf(story)}
  {@const points = pointsLine(story)}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="cell" oncontextmenu={(e) => menu.show(e, rowMenu(story))}>
    <button
      class="row list-row"
      class:sel={feed.selected === story.id}
      class:unread={!story.read}
      class:episode={!!story.media}
      onclick={() => void feed.open(story.id)}
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
          {#if story.files?.audio}
            <span class="mark" title="Saved as audio" role="img" aria-label="Saved as audio">
              <Icon name="volume" size={13} />
            </span>
          {/if}
        </div>
        <div class="meta">{meta(story)}</div>
        {#if story.media}{@render episodeLine(story)}{/if}
        {#if story.summary}<p class="summary">{story.summary}</p>{/if}
        {#if points}<div class="points">{points}</div>{/if}
      </div>
    </button>
    {#if story.media}
      <span class="end"><EpisodeActions {story} compact /></span>
    {/if}
  </div>
{/snippet}

<!-- What an episode row says beyond what any story's does: how long, how far
     along, where the file is, and whether it is lined up. A bar only once it
     has been started: an episode with none reads as new. -->
{#snippet episodeLine(story: Story)}
  {@const ref = { type: 'story', id: story.id } as const}
  {@const length = story.media?.durationMs}
  {@const progress = episodeProgress(upnext.progressOf(ref), length)}
  {@const download = upnext.downloadOf(story.id)}
  {@const work = upnext.workFor({ story: story.id })}
  <div class="facts">
    {#if length}<span>{formatDuration(length)}</span>{/if}
    {#if progress}<span class:played={progress.finished}>{progress.label}</span>{/if}
    {#if download}
      <span class:bad={download.state === 'failed'}>{downloadLine(download)}</span>
    {:else if story.files?.media}
      <span class="fact-mark" title="Downloaded">
        <Icon name="download" size={12} />
        <span class="vh">Downloaded</span>
      </span>
    {/if}
    {#if story.files?.transcript}
      <span class="fact-mark" title="Has a transcript">
        <Icon name="quote" size={12} />
        <span class="vh">Has a transcript</span>
      </span>
    {/if}
    {#if work}
      <span class:bad={work.state === 'failed'}>{workLine(work)}</span>
    {/if}
    {#if upnext.isQueued(ref)}
      <span class="fact-mark queued" title="In Up Next">
        <Icon name="queue-add" size={12} /> Up Next
      </span>
    {/if}
  </div>
  {#if progress && !progress.finished}
    <div
      class="bar"
      role="progressbar"
      aria-label="How much has been played"
      aria-valuemin="0"
      aria-valuemax="100"
      aria-valuenow={Math.round(progress.fraction * 100)}
    >
      <span style:width="{progress.fraction * 100}%"></span>
    </div>
  {/if}
{/snippet}

<style>
  .list {
    /* As tall as the stage by a stated height, not stretched: see
       `.shell-stage` in app.css. */
    height: 100%;
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

  /* The row's button and, for an episode, the control beside it. */
  .cell {
    position: relative;
  }
  .end {
    position: absolute;
    top: 50%;
    right: calc(var(--sp-2) + var(--sp-3));
    translate: 0 -50%;
    display: flex;
  }
  /* Room for the control, so a long title does not run under it. */
  .row.episode {
    padding-right: calc(34px + var(--sp-3) * 2);
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

  /* An episode's facts, one quiet line that wraps rather than clips. */
  .facts {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0 var(--sp-3);
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .facts .played {
    color: var(--fg-faint);
  }
  .facts .bad {
    color: var(--danger);
  }
  .fact-mark {
    display: inline-flex;
    align-items: center;
    gap: 3px;
  }
  .fact-mark.queued {
    color: var(--accent);
  }
  /* How far along: a hairline under the facts, in the accent. */
  .bar {
    height: 3px;
    margin-top: 3px;
    border-radius: 2px;
    background: var(--border);
    overflow: hidden;
  }
  .bar > span {
    display: block;
    height: 100%;
    background: var(--accent);
  }
  /* Said to a screen reader, not drawn: the icon beside it is the picture. */
  .vh {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }
</style>
