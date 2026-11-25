<script lang="ts">
  // Recommended: what the Feed would pick from everything unread, best first.
  //
  // The same two panes as every list in the Feed -- a column of stories and the
  // one being read beside it, which `FeedView` supplies -- but the column is
  // not newest-first and is not the sidebar's source: it is whatever the
  // ranking put at the top, from every source, with the reasons it did. Each
  // reason is a question of Hari's own (Settings → Feed) that the story
  // answered yes to, said in his words, so a recommendation is never "because
  // the model said so".
  //
  // A story here is opened in place and marked read the way any story is, and
  // the row keeps its place in the column until the list is asked for again --
  // a row that left the moment it was read would take the next one up under
  // the pointer. What leaves it is putting it in Up Next, which is what the
  // button at the row's end is: the service does not recommend what is already
  // lined up.
  //
  // The line under the heading says how the order was made, and when it is
  // only newest-first -- no criteria, or no model to judge with -- says that
  // and where to put it right, because a list that looks ranked and is not
  // would be a quiet lie.

  import { onMount } from 'svelte'
  import { feed } from '../lib/feed.svelte'
  import { bylineParts, rankingNote, sourceLabel } from '../lib/feed'
  import { relativeTime, plural } from '../lib/format'
  import { episodeProgress, formatDuration } from '../lib/podcast'
  import { panels } from '../lib/panels.svelte'
  import { phone } from '../lib/phone.svelte'
  import { upnext } from '../lib/upnext.svelte'
  import type { Recommendation } from '../lib/types'
  import Avatar from './Avatar.svelte'
  import EmptyState from './EmptyState.svelte'
  import EpisodeActions from './EpisodeActions.svelte'
  import Icon from './Icon.svelte'
  import PaneResizer from './PaneResizer.svelte'
  import SourceIcon from './SourceIcon.svelte'

  /** A phone shows a story over the list, which stays as it was beneath it. */
  let { under = false }: { under?: boolean } = $props()

  onMount(() => {
    void upnext.loadRecommended()
    // The criteria are what the note under the heading is checked against.
    void upnext.loadSettings()
  })

  const recs = $derived(upnext.recommended)
  const ranking = $derived(rankingNote(upnext.ranking, upnext.settings))
  /** Where the note's own words lead: settings can put these right. */
  const fixable = $derived(
    ranking?.kind === 'off' || ranking?.kind === 'criteria' || ranking?.kind === 'judge',
  )

  /** Stories opened here, so their rows read as read before the list is asked for again. */
  let opened = $state<Set<string>>(new Set())

  function open(rec: Recommendation) {
    opened = new Set([...opened, rec.story.id])
    void feed.open(rec.story.id)
  }

  function isRead(rec: Recommendation): boolean {
    return rec.story.read || opened.has(rec.story.id)
  }

  function storyTitle(rec: Recommendation): string {
    return rec.story.title.trim() || 'Untitled'
  }

  /** "Source · author · 3 hours ago". */
  function meta(rec: Recommendation): string {
    const found = feed.sourceOf(rec.story)
    const name = rec.sourceTitle.trim() || (found ? sourceLabel(found.source) : '')
    return [...bylineParts(rec.story, name), relativeTime(rec.story.publishedAt)].join(' · ')
  }

  /** -1 to 1 as a share of a bar: 0 is half, which is "neither". */
  const fit = (score: number) => Math.min(1, Math.max(0, (score + 1) / 2))

  function scoreWord(score: number): string {
    return `Score ${score > 0 ? '+' : ''}${score.toFixed(2)} on your criteria`
  }

  // How many reasons a row has room to show; the rest are counted.
  const REASONS = 3

  function toggleQueued(rec: Recommendation) {
    const ref = { type: 'story', id: rec.story.id } as const
    if (upnext.isQueued(ref)) void upnext.removeFor(ref)
    else void upnext.add(ref)
  }

  // ── The keyboard ──────────────────────────────────────────────────────
  //
  // The arrow keys walk the rows; Enter, which a button already takes, opens.
  // `j` and `k` are the Feed's own for the list `feed` holds, which this is
  // not, so the arrows are what is offered here.

  let rows = $state<HTMLElement | null>(null)

  function rowKeys(e: KeyboardEvent) {
    if (e.key !== 'ArrowUp' && e.key !== 'ArrowDown') return
    const all = [...(rows?.querySelectorAll<HTMLElement>('.row') ?? [])]
    const at = all.indexOf(e.target as HTMLElement)
    if (at < 0) return
    e.preventDefault()
    all[at + (e.key === 'ArrowUp' ? -1 : 1)]?.focus()
  }
</script>

<section class="list" class:under style:--tint={feed.accent}>
  <div class="top">
    <div class="pane-titles">
      <span class="pane-title">Recommended</span>
      {#if recs.length > 0}
        <span class="pane-sub">{plural(recs.length, 'story', 'stories')}</span>
      {/if}
    </div>
    <button
      class="plus"
      class:spinning={upnext.recommendedLoading}
      title={upnext.recommendedLoading ? 'Looking again…' : 'Look again'}
      aria-label={upnext.recommendedLoading ? 'Looking again' : 'Look again'}
      aria-busy={upnext.recommendedLoading}
      onclick={() => void upnext.loadRecommended()}
    >
      <Icon name="refresh" size={17} />
    </button>
  </div>

  {#if ranking}
    <p class="ranking" class:problem={ranking.kind === 'problem'} role="status">
      {ranking.text}
      {#if fixable}
        <button class="link" onclick={() => panels.openSettings('feed')}>Settings → Feed</button>.
      {/if}
    </p>
  {/if}

  {#if upnext.recommendedLoading && recs.length === 0}
    <p class="hint">Looking…</p>
  {:else if recs.length === 0}
    <EmptyState lead="Nothing to recommend.">
      {#snippet note()}
        Recommended draws on what you have not read from what you follow. Follow a few more, or come
        back after the next refresh.
      {/snippet}
      {#snippet action()}
        <button class="btn" onclick={() => feed.openDirectory()}>Browse podcasts and feeds</button>
      {/snippet}
    </EmptyState>
  {:else}
    <!-- The scrolling parent the rows need; `section.list` itself never scrolls. -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="scroll story-rows" bind:this={rows} onkeydown={rowKeys}>
      {#each recs as rec (rec.story.id)}
        {@render recRow(rec)}
      {/each}
    </div>
  {/if}

  {#if feed.note}
    <p class="note" role="status">{feed.note}</p>
  {/if}

  {#if !phone.on}
    <PaneResizer
      cssVar="--feed-list-w"
      storageKey="pane-w:feed-list"
      defaultWidth={380}
      min={300}
      max={640}
    />
  {/if}
</section>

<!-- One recommendation. Laid out as a story's row is -- the unread dot, the
     source's mark, a loud title, a quiet line, two lines of what it says -- so
     that moving between this column and any other is not learning a second
     list. What is its own: a bar for the score, the reasons as chips, and at
     its end the controls, beside the row's button and not in it (a button
     holding a button is not two buttons to a screen reader). -->
{#snippet recRow(rec: Recommendation)}
  {@const story = rec.story}
  {@const source = feed.sourceOf(story)}
  {@const ref = { type: 'story', id: story.id } as const}
  {@const queued = upnext.isQueued(ref)}
  {@const reasons = rec.reasons}
  {@const length = story.media?.durationMs}
  {@const progress = story.media ? episodeProgress(upnext.progressOf(ref), length) : null}
  <div class="cell">
    <button
      class="row list-row"
      class:sel={feed.selected === story.id}
      class:unread={!isRead(rec)}
      onclick={() => open(rec)}
    >
      <span class="dot" class:on={!isRead(rec)} aria-hidden="true"></span>
      <span class="face">
        {#if source}
          <SourceIcon source={source.source} size={34} />
        {:else}
          <Avatar name={rec.sourceTitle || story.title} size={34} />
        {/if}
      </span>
      <div class="body">
        <div class="line1">
          <span class="title">{storyTitle(rec)}</span>
          {#if rec.score !== null && rec.score !== undefined}
            <span
              class="score"
              role="img"
              title={scoreWord(rec.score)}
              aria-label={scoreWord(rec.score)}
              style:--fit={fit(rec.score)}
            >
              <i></i>
            </span>
          {/if}
        </div>
        <div class="meta">{meta(rec)}</div>
        {#if story.media}
          <div class="facts">
            {#if length}<span>{formatDuration(length)}</span>{/if}
            {#if progress}<span>{progress.label}</span>{/if}
            {#if story.files?.media}
              <span class="fact-mark" title="Downloaded">
                <Icon name="download" size={12} />
                <span class="vh">Downloaded</span>
              </span>
            {/if}
          </div>
        {/if}
        {#if reasons.length > 0}
          <div class="reasons" aria-label="Why it is here">
            {#each reasons.slice(0, REASONS) as reason, i (i)}
              <span class="tag" title={reason}>{reason}</span>
            {/each}
            {#if reasons.length > REASONS}
              <span class="tag more" title={reasons.slice(REASONS).join('\n')}>
                +{reasons.length - REASONS}
              </span>
            {/if}
          </div>
        {/if}
        {#if story.summary}<p class="summary">{story.summary}</p>{/if}
      </div>
    </button>
    <span class="end">
      {#if story.media}<EpisodeActions {story} compact />{/if}
      <button
        class="round"
        class:on={queued}
        aria-pressed={queued}
        title={queued ? 'Take it out of Up Next' : 'Add to Up Next'}
        aria-label={queued ? 'Take it out of Up Next' : 'Add to Up Next'}
        onclick={() => toggleQueued(rec)}
      >
        <Icon name={queued ? 'tick' : 'queue-add'} size={15} />
      </button>
    </span>
  </div>
{/snippet}

<style>
  .list {
    /* As tall as the stage by a stated height, not stretched: see
       `.shell-stage` in app.css. */
    height: 100%;
    position: relative;
    /* The Feed's list width, shared with `StoryList`: dragging it in one is
       dragging it in both, and the column does not jump between them. */
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

  /* How the order was made: one quiet paragraph, in the danger colour only
     when something is wrong and not merely missing. */
  .ranking {
    flex: none;
    margin: 0;
    padding: 0 var(--sp-5) var(--sp-3);
    font-size: var(--text-xs);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }
  .ranking.problem {
    color: var(--danger);
  }
  .link {
    color: var(--accent);
    text-decoration: underline;
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

  /* ── Rows ───────────────────────────────────────────────────────── */

  .story-rows {
    flex: 1;
    min-height: 0;
    padding-bottom: var(--sp-2);
  }
  .cell {
    position: relative;
  }
  .row {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    /* Room at the end for the controls. */
    padding: var(--sp-3) calc(34px + var(--sp-3) * 2) var(--sp-3) var(--sp-5);
  }
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
  /* The score, as a short bar in the accent: how full it is says how well the
     story answered. Quiet on purpose -- it is a hint at the order, not a
     grade. */
  .score {
    flex: none;
    width: 26px;
    height: 4px;
    margin-top: 8px;
    border-radius: 2px;
    background: var(--border);
    overflow: hidden;
  }
  .score > i {
    display: block;
    width: calc(var(--fit) * 100%);
    height: 100%;
    background: color-mix(in oklab, var(--accent) 75%, transparent);
  }
  .meta {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .facts {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0 var(--sp-3);
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .fact-mark {
    display: inline-flex;
    align-items: center;
    gap: 3px;
  }
  .reasons {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin-top: 2px;
  }
  /* The shared chip, kept to one line: a reason is a question somebody wrote,
     and a long one is shortened here and whole in its tooltip. */
  .reasons .tag {
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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

  /* The controls at the row's end: play or get an episode, and Up Next. */
  .end {
    position: absolute;
    top: 50%;
    right: calc(var(--sp-2) + var(--sp-3));
    translate: 0 -50%;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
  }
  .round {
    position: relative;
    display: grid;
    place-items: center;
    flex: none;
    width: 34px;
    height: 34px;
    border: 1px solid var(--border-strong);
    border-radius: 50%;
    background: var(--bg-raised);
    color: var(--fg-muted);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease),
      border-color var(--fast) var(--ease);
  }
  .round:hover {
    color: var(--fg);
    border-color: var(--fg-faint);
  }
  .round.on {
    border-color: color-mix(in oklab, var(--accent) 45%, transparent);
    background: color-mix(in oklab, var(--accent) 10%, transparent);
    color: var(--accent);
  }

  .vh {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }

  /* ── On a phone ──────────────────────────────────────────────────────── */

  /* The list is the whole screen, not a column beside a story, and stays
     where it is while a story is over it. */
  :global([data-phone]) .list {
    width: auto;
    flex: 1;
    min-width: 0;
    border-right: 0;
  }
  :global([data-phone]) .list.under {
    visibility: hidden;
  }
  :global([data-phone]) .top {
    padding: var(--sp-2) var(--sp-2) var(--sp-1) var(--sp-4);
  }
  :global([data-phone]) .plus {
    width: 44px;
    height: 44px;
  }
  :global([data-phone]) .ranking {
    padding: 0 var(--sp-4) var(--sp-3);
  }
  :global([data-phone]) .hint {
    padding: var(--sp-4);
  }
  :global([data-phone]) .row {
    padding-right: calc(40px + var(--sp-3) * 2);
  }
  :global([data-phone]) .end {
    right: var(--sp-3);
  }
  :global([data-phone]) .round {
    width: 40px;
    height: 40px;
  }
  :global([data-phone]) .note {
    padding: var(--sp-2) var(--sp-4);
  }

  @media (max-height: 500px) {
    :global([data-phone]) .top {
      min-height: 0;
      padding-block: var(--sp-1);
    }
    :global([data-phone]) .pane-sub {
      display: none;
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
