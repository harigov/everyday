<script lang="ts">
  // A note, as a tile in the notes app's colour with its symbol: the title,
  // the first lines, and a quieter line of where it is or what it holds.
  import Symbol from '../../components/Symbol.svelte'
  import { repeats } from '../cards'
  import type { PanelBlock } from '../panel'

  let {
    block,
    cardTitle,
  }: {
    block: Extract<PanelBlock, { type: 'note' }>
    /** The card's own title: the note's is left out where it says the same, if there is more. */
    cardTitle: string
  } = $props()

  const titled = $derived(!(block.excerpt && repeats(block.title, cardTitle)))
</script>

<div class="pc-tile note" style:--tile="var(--app-notes)">
  <span class="mark"><Symbol name="note" size={18} /></span>
  <span class="words">
    {#if titled}<span class="title">{block.title}</span>{/if}
    {#if block.excerpt}<span class="excerpt">{block.excerpt}</span>{/if}
    {#if block.detail}<span class="detail">{block.detail}</span>{/if}
  </span>
</div>

<style>
  .note {
    display: flex;
    align-items: flex-start;
    gap: 10px;
  }

  .mark {
    flex: none;
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: var(--radius);
    background: color-mix(in oklab, var(--tile) 16%, transparent);
    color: var(--tile);
  }

  .words {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
    overflow-wrap: anywhere;
  }

  .title {
    font-size: var(--text-sm);
    font-weight: 600;
    line-height: var(--leading-tight);
  }

  .excerpt {
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
    color: var(--fg-muted);
    white-space: pre-line;
    overflow: hidden;
    display: -webkit-box;
    -webkit-line-clamp: 4;
    line-clamp: 4;
    -webkit-box-orient: vertical;
  }

  .detail {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
</style>
