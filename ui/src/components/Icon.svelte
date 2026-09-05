<script lang="ts">
  import { ICONS, type IconName } from '../lib/icons'

  let {
    name,
    size = 16,
    weight = 1.5,
    filled = false,
  }: {
    name: IconName
    /** Rendered edge length in CSS pixels. */
    size?: number
    /** Stroke thickness in CSS pixels, held constant across sizes. */
    weight?: number
    /** Fill the shape instead of stroking it -- a starred entry, say. */
    filled?: boolean
  } = $props()

  // The artwork is drawn on a 24-unit grid, so a stroke of `w` units renders
  // at `w * size / 24` pixels. Solving that for a constant on-screen weight
  // is what keeps a 16px sidebar icon and a 19px toolbar icon looking like
  // they came from the same family -- scaling the viewBox alone would make
  // the smaller one visibly lighter.
  const strokeWidth = $derived((weight * 24) / size)
</script>

<svg
  class="icon"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill={filled ? 'currentColor' : 'none'}
  stroke={filled ? 'none' : 'currentColor'}
  stroke-width={strokeWidth}
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
  focusable="false"
>
  <!-- eslint-disable-next-line svelte/no-at-html-tags -- compile-time constants from icons.ts -->
  {@html ICONS[name]}
</svg>

<style>
  /* Icons sit inline with text more often than not; this keeps them centred
     on the cap height rather than hanging off the baseline. */
  .icon {
    display: block;
    flex: none;
    overflow: visible;
  }
</style>
