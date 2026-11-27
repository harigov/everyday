<script lang="ts">
  import { BOX, FEATHERS, GOLD, MARK, along } from '../lib/wing'

  let {
    size = 20,
    tile = false,
  }: {
    size?: number
    /** Draw the full app tile. Otherwise the mark alone, in `currentColor`. */
    tile?: boolean
  } = $props()

  // Unique per instance: two gradients with the same id on one page collapse
  // into whichever the browser saw first.
  const uid = $props.id()

  // The tile, filling the whole box (22.5%, 60% smoothing).
  const TILE =
    'M15.36 0c3.02 0 4.54 0 5.69.59a5.4 5.4 0 0 1 2.36 2.36c.59 1.16.59 2.67.59 5.69L24 15.36' +
    'c0 3.02 0 4.54-.59 5.69a5.4 5.4 0 0 1-2.36 2.36c-1.16.59-2.67.59-5.69.59L8.64 24' +
    'c-3.02 0-4.54 0-5.69-.59a5.4 5.4 0 0 1-2.36-2.36c-.59-1.16-.59-2.67-.59-5.69L0 8.64' +
    'c0-3.02 0-4.54.59-5.69a5.4 5.4 0 0 1 2.36-2.36c1.16-.59 2.67-.59 5.69-.59Z'

  // Back feather first, so the long one in front is drawn last.
  const STACK = FEATHERS.map((d, i) => ({
    d,
    i,
    fill: along(GOLD, i / (FEATHERS.length - 1)),
  })).reverse()

  // Bare, the wing fills the box with a hair of room; on the tile it keeps
  // the icon's place, which the feather data is already drawn in.
  const side = Math.max(BOX.width, BOX.height) * 1.04
  const fit = `translate(12 12) scale(${24 / side}) translate(${-(BOX.x + BOX.width / 2)} ${-(BOX.y + BOX.height / 2)})`

  // Under about 32px the nine feathers merge into a smudge, and one flat
  // shape with channels between them reads better.
  const fine = $derived(size >= 32)
</script>

<!--
  Jatayu's wing, the application icon (crates/jatayu-app/icons/icon.svg),
  drawn from the same geometry (src/lib/wing.ts) on a 24-unit grid. On the
  tile: the midnight squircle with its sheen and a warm glow, and the nine
  feathers stepping from pale gold to deep orange -- or, at the top bar's
  18px, the one flat shape in gold, since nine fills that small are a blur.
  The icon's shadows and lit edges are left out; at these sizes they are a
  smudge.

  Bare, in currentColor: the feathers in steps of the ink's strength, front
  strongest, or the flat shape when small.
-->
<svg
  class="logo"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  aria-hidden="true"
  focusable="false"
>
  {#if tile}
    <defs>
      <linearGradient id="night-{uid}" x1="0" y1="0" x2="0" y2="1">
        <stop offset="0" stop-color="#2a2f7a" />
        <stop offset=".5" stop-color="#161a4d" />
        <stop offset="1" stop-color="#080a26" />
      </linearGradient>
      <radialGradient id="sheen-{uid}" cx="12" cy="1.2" r="16.3" gradientUnits="userSpaceOnUse">
        <stop offset="0" stop-color="#fff" stop-opacity=".26" />
        <stop offset="1" stop-color="#fff" stop-opacity="0" />
      </radialGradient>
      <radialGradient id="glow-{uid}" cx="11.65" cy="10.8" r="12.5" gradientUnits="userSpaceOnUse">
        <stop offset="0" stop-color="#ff9a3d" stop-opacity=".24" />
        <stop offset="1" stop-color="#ff9a3d" stop-opacity="0" />
      </radialGradient>
      <linearGradient id="gold-{uid}" x1="0" y1="3" x2="0" y2="21" gradientUnits="userSpaceOnUse">
        <stop offset="0" stop-color="#ffe9a6" />
        <stop offset="1" stop-color="#ff8a2c" />
      </linearGradient>
    </defs>
    <path d={TILE} fill="url(#night-{uid})" />
    <path d={TILE} fill="url(#sheen-{uid})" />
    <path d={TILE} fill="url(#glow-{uid})" />
    {#if fine}
      {#each STACK as feather (feather.i)}
        <path d={feather.d} fill={feather.fill} />
      {/each}
    {:else}
      <path d={MARK} fill="url(#gold-{uid})" />
    {/if}
  {:else}
    <g transform={fit} fill="currentColor">
      {#if fine}
        {#each STACK as feather (feather.i)}
          <path d={feather.d} opacity={1 - 0.55 * (feather.i / (FEATHERS.length - 1))} />
        {/each}
      {:else}
        <path d={MARK} />
      {/if}
    </g>
  {/if}
</svg>

<style>
  .logo {
    display: block;
    flex: none;
  }
</style>
