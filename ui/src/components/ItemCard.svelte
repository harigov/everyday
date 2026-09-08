<script lang="ts">
  import { library } from '../lib/library.svelte'
  import type { Item, KindInfo } from '../lib/types'
  import Cover from './Cover.svelte'
  import Icon from './Icon.svelte'
  import Rating from './Rating.svelte'

  let {
    item,
    kind,
    ratio,
    selected = false,
    onopen,
  }: {
    item: Item
    /** The shelf it is on: its colour, its emoji and its verbs. */
    kind: KindInfo | null
    /**
     * The frame every card in this grid is drawn in.
     *
     * Passed in rather than derived per card, deliberately: see
     * `coverRatio`. One grid, one ratio, so the titles share a baseline.
     */
    ratio: string
    selected?: boolean
    onopen: () => void
  } = $props()

  const color = $derived(kind?.color ?? 'var(--accent)')

  const progress = $derived(
    item.progress && item.progress.total
      ? Math.min(Math.max(item.progress.position / item.progress.total, 0), 1)
      : null,
  )
</script>

<div class="card" class:selected style="--tint: {color}">
  <button class="hit" onclick={onopen} aria-label={item.title}>
    <div class="art">
      <Cover blob={item.cover} title={item.title} icon={kind?.icon ?? ''} {color} {ratio} />

      {#if item.status !== 'wishlist'}
        <!-- The status rides on the cover rather than under the title,
             because on a shelf of forty the question is "which of these am I
             in the middle of" and that has to be answerable without reading. -->
        <span class="chip" class:done={item.status === 'done'}>
          {library.label(kind, item.status)}
        </span>
      {/if}

      {#if progress !== null}
        <div class="bar" aria-hidden="true"><span style="width: {progress * 100}%"></span></div>
      {/if}
    </div>

    <div class="text">
      <span class="title">{item.title}</span>
      {#if item.creator || item.year}
        <span class="byline">
          {item.creator}{#if item.creator && item.year}&nbsp;·&nbsp;{/if}{item.year ?? ''}
        </span>
      {/if}
    </div>
  </button>

  <div class="foot">
    {#if item.rating !== null && item.rating !== undefined}
      <Rating value={item.rating} size={12} readonly />
    {:else if item.external.length > 0}
      <!-- Somebody else's score, drawn faintly, so an unrated card is not a
           blank row -- and so the two are never mistaken for each other. -->
      <span class="external" title="{item.external[0]!.source}: not your rating">
        <Rating value={item.external[0]!.score} size={12} readonly muted />
      </span>
    {/if}
    <span class="spacer"></span>
    <button
      class="star"
      class:on={item.favourite}
      title={item.favourite ? 'Remove from favourites' : 'Add to favourites'}
      aria-label={item.favourite ? 'Remove from favourites' : 'Add to favourites'}
      onclick={() => void library.toggleFavourite(item.id)}
    >
      <Icon name="star" size={13} filled={item.favourite} />
    </button>
  </div>
</div>

<style>
  .card {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .hit {
    display: block;
    width: 100%;
    text-align: left;
    border-radius: var(--radius-lg);
    padding: 0;
  }
  .hit:focus-visible {
    outline: 2px solid var(--tint);
    outline-offset: 3px;
  }

  .art {
    position: relative;
    /* The lift on hover is the whole of the grid's interaction feedback, so
       it is worth a real shadow rather than a border change. */
    transition:
      transform var(--med) var(--ease),
      box-shadow var(--med) var(--ease);
    border-radius: var(--radius);
    box-shadow: var(--shadow-sm);
  }
  .card:hover .art {
    transform: translateY(-3px);
    box-shadow: var(--shadow);
  }
  .selected .art {
    box-shadow:
      0 0 0 2px var(--tint),
      var(--shadow);
  }

  .chip {
    position: absolute;
    top: var(--sp-2);
    left: var(--sp-2);
    max-width: calc(100% - var(--sp-4));
    padding: 2px 7px;
    border-radius: 999px;
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.01em;
    color: #fff;
    background: color-mix(in oklab, var(--tint) 88%, black);
    box-shadow: 0 1px 3px rgb(0 0 0 / 0.3);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* Finished is the one status that should recede: it is the resting state
     of a shelf, and forty bright chips would be a shelf you cannot read. */
  .chip.done {
    color: var(--fg);
    background: color-mix(in oklab, var(--bg-raised) 82%, var(--tint));
  }

  .bar {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    height: 3px;
    background: rgb(0 0 0 / 0.28);
    border-radius: 0 0 var(--radius) var(--radius);
    overflow: hidden;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--tint);
    transition: width var(--med) var(--ease);
  }

  .text {
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding: var(--sp-2) 2px 0;
    min-width: 0;
  }
  .title {
    font-family: var(--font-read);
    font-size: var(--text-md);
    font-weight: 600;
    line-height: var(--leading-tight);
    color: var(--fg);
    /* Two lines, then ellipsis. One is too few for a real book title and
       three lets a long one push its neighbours' baselines out of line. */
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .byline {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .foot {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    min-height: 22px;
    padding: 2px 2px 0;
  }
  .spacer {
    flex: 1;
  }
  .external {
    display: inline-flex;
    opacity: 0.75;
  }

  .star {
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
    /* Hidden until wanted: a grid with forty grey stars in it reads as forty
       things you have already rated. */
    opacity: 0;
    transition: opacity var(--fast) var(--ease);
  }
  .card:hover .star,
  .star:focus-visible,
  .star.on {
    opacity: 1;
  }
  .star.on {
    color: #e0a92b;
  }
  .star:hover {
    background: var(--bg-hover);
    color: #e0a92b;
  }
</style>
