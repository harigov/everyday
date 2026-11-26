<script lang="ts">
  // A message: who it is from -- their face, their name and address -- and
  // when, then the subject and its first lines, and what came with it. Drawn
  // on the card itself rather than in a tile: on a mail card it is the card.
  import Avatar from '../Avatar.svelte'
  import Glyph from '../Glyph.svelte'
  import { plural, repeats } from '../cards'
  import type { PanelBlock } from '../panel'

  let {
    block,
    cardTitle,
  }: {
    block: Extract<PanelBlock, { type: 'mail' }>
    /** The card's own title: the subject is left out where it says the same. */
    cardTitle: string
  } = $props()

  const address = $derived(
    block.from.email && block.from.email !== block.from.name ? block.from.email : null,
  )
</script>

<div class="mail">
  <div class="from">
    <Avatar person={block.from} size={34} />
    <span class="who">
      <span class="name">{block.from.name}</span>
      {#if address}<span class="address">{address}</span>{/if}
    </span>
    {#if block.received}<time>{block.received}</time>{/if}
  </div>
  {#if !repeats(block.subject, cardTitle)}
    <p class="subject">{block.subject}</p>
  {/if}
  {#if block.snippet}
    <p class="snippet">{block.snippet}</p>
  {/if}
  {#if block.attachments > 0 || block.account}
    <p class="meta">
      {#if block.attachments > 0}
        <span class="clip">
          <Glyph name="paperclip" size={13} />
          {plural(block.attachments, 'attachment')}
        </span>
      {/if}
      {#if block.account}<span class="to">To {block.account}</span>{/if}
    </p>
  {/if}
</div>

<style>
  .mail {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }

  .from {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
    margin-bottom: 2px;
  }

  .who {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }

  .name {
    font-size: var(--text-base);
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .address {
    font-size: var(--text-xs);
    color: var(--fg-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  time {
    flex: none;
    align-self: flex-start;
    margin-top: 2px;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: var(--fg-subtle);
  }

  .subject {
    font-size: var(--text-sm);
    font-weight: 600;
    line-height: var(--leading-tight);
    overflow-wrap: anywhere;
  }

  .snippet {
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
    color: var(--fg-muted);
    overflow: hidden;
    overflow-wrap: anywhere;
    display: -webkit-box;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    -webkit-box-orient: vertical;
  }

  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 2px 12px;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }

  .clip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }

  .to {
    overflow-wrap: anywhere;
  }
</style>
