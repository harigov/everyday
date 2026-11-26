<script lang="ts">
  // Text shown whole and exactly, in a well: what is about to be typed or
  // sent, the thing a question is about. In the interface's own face -- it is
  // somebody's words, not code -- and selectable, so it can be copied by hand
  // too. Long text scrolls inside the well rather than pushing the buttons
  // away.
  import type { PanelBlock } from '../panel'

  let { block }: { block: Extract<PanelBlock, { type: 'quote' }> } = $props()
</script>

<figure class="quote">
  {#if block.label}<figcaption class="pc-label">{block.label}</figcaption>{/if}
  <blockquote class="pc-well">{block.text}</blockquote>
</figure>

<style>
  figure {
    margin: 0;
  }

  blockquote {
    margin: 0;
    max-height: 168px;
    overflow-y: auto;
    overscroll-behavior: contain;
    scrollbar-width: thin;
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
    color: var(--fg);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    -webkit-user-select: text;
    user-select: text;
    cursor: text;
  }
</style>
