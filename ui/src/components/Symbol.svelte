<script lang="ts">
  // A filled, two-tone icon -- see `symbols.ts` for the layers and the
  // house rules. One colour, the current one: the back layer is that colour
  // at reduced strength, and detail is cut out of the front rather than
  // painted, so a symbol sits on any backdrop a theme draws. A name with no
  // symbol drawn yet is shown as its line icon, so a sidebar row is never
  // left empty by a missing drawing.
  import type { IconName } from '../lib/icons'
  import { SYMBOLS, type SymbolName } from '../lib/symbols'
  import Icon from './Icon.svelte'

  let {
    name,
    size = 16,
  }: {
    name: SymbolName
    /** Rendered edge length in CSS pixels. */
    size?: number
  } = $props()

  // Unique per instance: the cut is a mask, and two symbols on one page
  // must not share one.
  const uid = $props.id()
  const art = $derived(SYMBOLS[name])
  // Read when drawn. The bar is redrawn often enough -- every change of app
  // -- that a date a few hours stale after midnight is the worst case.
  const mark = $derived(art?.mark?.replace('{day}', String(new Date().getDate())))
</script>

{#if art}
  <svg
    class="symbol"
    width={size}
    height={size}
    viewBox="0 0 24 24"
    aria-hidden="true"
    focusable="false"
  >
    <!-- eslint-disable svelte/no-at-html-tags -- compile-time constants from symbols.ts -->
    {#if art.cut}
      <mask id="{uid}-cut">
        <rect width="24" height="24" fill="#fff" />
        <g fill="#000" stroke="#000">{@html art.cut}</g>
      </mask>
    {/if}
    {#if art.back}<g class="back">{@html art.back}</g>{/if}
    <g class="front" mask={art.cut ? `url(#${uid}-cut)` : undefined}>{@html art.front}</g>
    {#if mark}<g class="mark">{@html mark}</g>{/if}
    <!-- eslint-enable svelte/no-at-html-tags -->
  </svg>
{:else}
  <Icon name={name as IconName} {size} />
{/if}

<style>
  .symbol {
    display: block;
    flex: none;
    overflow: visible;
  }
  .symbol g {
    stroke-width: 0;
  }
  .back,
  .front,
  .mark {
    fill: currentColor;
    stroke: currentColor;
  }
  /* The secondary layer. A custom property so a place that wants more or
     less depth -- the selected row, say -- can say so without a copy. */
  .back {
    opacity: var(--symbol-back, 0.38);
  }
  .symbol :global(.s) {
    fill: none;
    stroke-width: 2.3;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
</style>
