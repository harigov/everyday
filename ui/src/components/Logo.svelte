<script lang="ts">
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

  // One petal, pointing up from the sun's centre, as on the icon: from 6.2
  // out to 10.2, pointed at both ends.
  const PETAL = 'M0-6.2C1.05-7.35 1.15-8.95 0-10.2C-1.15-8.95-1.05-7.35 0-6.2Z'

  // Seven, 24 degrees apart, the middle one tallest and the outer ones
  // fainter -- the same fan as the icon's.
  const PETALS = [-72, -48, -24, 0, 24, 48, 72].map((angle) => ({
    angle,
    stretch: 1 - Math.abs(angle) / 500,
    opacity: 1 - Math.abs(angle) / 260,
  }))

  // On the tile the mark keeps a margin; bare, it fills the box.
  const horizon = $derived(tile ? 14.2 : 14.5)
  const scale = $derived(tile ? 0.82 : 1)
</script>

<!--
  The same sunrise as the application icon (crates/karmayoga-app/icons/icon.svg),
  redrawn on a 24-unit grid. It is not a scaled-down copy: at 20px the icon's
  soft glow and blurred reflection close up into a smudge, so here the petals
  are solid, the horizon is a clean line and the reflection is two strokes.
-->
<svg
  class="logo"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  aria-hidden="true"
  focusable="false"
>
  {#if tile}
    <defs>
      <clipPath id="tile-{uid}"><path d={TILE} /></clipPath>
      <linearGradient id="sky-{uid}" x1="0" y1="0" x2="0" y2="1">
        <stop offset="0" stop-color="#1b1840" />
        <stop offset=".42" stop-color="#4a2a6a" />
        <stop offset=".76" stop-color="#d4506a" />
        <stop offset="1" stop-color="#ffae5a" />
      </linearGradient>
      <linearGradient id="water-{uid}" x1="0" y1="0" x2="0" y2="1">
        <stop offset="0" stop-color="#3b2550" />
        <stop offset="1" stop-color="#120d22" />
      </linearGradient>
      <linearGradient id="sun-{uid}" x1="0" y1="0" x2="0" y2="1">
        <stop offset="0" stop-color="#fff4c9" />
        <stop offset=".55" stop-color="#ffc24a" />
        <stop offset="1" stop-color="#ff8f2e" />
      </linearGradient>
    </defs>
    <g clip-path="url(#tile-{uid})">
      <rect width="24" height={horizon} fill="url(#sky-{uid})" />
      <rect y={horizon} width="24" height={24 - horizon} fill="url(#water-{uid})" />
    </g>
  {/if}
  <g transform="translate(12 {horizon}) scale({scale})">
    {#each PETALS as petal (petal.angle)}
      <path
        d={PETAL}
        transform="rotate({petal.angle}) scale(1 {petal.stretch})"
        fill={tile ? '#ffe2a0' : 'currentColor'}
        opacity={petal.opacity * (tile ? 0.9 : 1)}
      />
    {/each}
    <path d="M-5 0A5 5 0 0 1 5 0Z" fill={tile ? `url(#sun-${uid})` : 'currentColor'} />
  </g>
  <rect
    x={tile ? 0 : 1.5}
    y={horizon - 0.45}
    width={tile ? 24 : 21}
    height=".9"
    rx=".45"
    fill={tile ? '#ffd27a' : 'currentColor'}
    opacity={tile ? 0.85 : 1}
  />
  <g fill={tile ? '#ffc24a' : 'currentColor'}>
    <rect x="8.4" y={horizon + 2} width="7.2" height="1.1" rx=".55" opacity={tile ? 0.6 : 0.55} />
    <rect x="9.9" y={horizon + 4.1} width="4.2" height="1" rx=".5" opacity={tile ? 0.38 : 0.32} />
  </g>
</svg>

<style>
  .logo {
    display: block;
    flex: none;
  }
</style>
