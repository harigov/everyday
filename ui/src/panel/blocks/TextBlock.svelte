<script lang="ts">
  // A paragraph, perhaps headed: a description, an agenda, a summary.
  import type { PanelBlock } from '../panel'

  let { block }: { block: Extract<PanelBlock, { type: 'text' }> } = $props()
</script>

<section class="text">
  {#if block.heading}<h3 class="pc-label">{block.heading}</h3>{/if}
  <p>{block.text}</p>
</section>

<style>
  p {
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
    color: var(--fg);
    white-space: pre-line;
    overflow-wrap: anywhere;
  }
</style>
