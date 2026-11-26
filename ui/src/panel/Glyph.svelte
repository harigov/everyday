<script lang="ts">
  // A line mark: one of the app's own icons (`lib/icons.ts`), or one of the
  // three the panel needs that the app has no drawing of -- open elsewhere,
  // join a call, send. Drawn to the same rules as `Icon.svelte` (24-unit
  // grid, round caps, a stroke that keeps its weight at any size), so the
  // three sit beside the rest as one family.
  import Icon from '../components/Icon.svelte'
  import type { IconName } from '../lib/icons'
  import type { GlyphName } from './cards'

  let {
    name,
    size = 16,
    weight = 1.75,
  }: {
    name: GlyphName
    size?: number
    weight?: number
  } = $props()

  const OWN: Partial<Record<GlyphName, string>> = {
    // An arrow leaving a box: open this in the main window.
    open:
      '<path d="M14 4h6v6"/><path d="M20 4 11 13"/>' +
      '<path d="M18 14v4a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4"/>',
    // A camera: join the call.
    join:
      '<rect x="2.5" y="6" width="13" height="12" rx="2.5"/>' +
      '<path d="m15.5 10.5 6-3.5v10l-6-3.5Z"/>',
    // A paper plane: send.
    send: '<path d="M21 3 10.5 13.5"/><path d="M21 3 14.5 21l-4-7.5L3 9.5Z"/>',
  }

  const own = $derived(OWN[name])
  const strokeWidth = $derived((weight * 24) / size)
</script>

{#if own}
  <svg
    class="glyph"
    width={size}
    height={size}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width={strokeWidth}
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
    focusable="false"
  >
    <!-- eslint-disable-next-line svelte/no-at-html-tags -- compile-time constants above -->
    {@html own}
  </svg>
{:else}
  <Icon name={name as IconName} {size} {weight} />
{/if}

<style>
  .glyph {
    display: block;
    flex: none;
    overflow: visible;
  }
</style>
