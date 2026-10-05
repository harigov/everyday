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

  // One card of the icon's stack, 12 units square and centred on the origin,
  // with the icon's continuous corner (26%, 60% smoothing).
  const CARD =
    'M1.01-6c1.75 0 2.62 0 3.29.34a3.12 3.12 0 0 1 1.36 1.36c.34.67.34 1.54.34 3.29L6 1.01' +
    'c0 1.75 0 2.62-.34 3.29a3.12 3.12 0 0 1-1.36 1.36c-.67.34-1.54.34-3.29.34L-1.01 6' +
    'c-1.75 0-2.62 0-3.29-.34a3.12 3.12 0 0 1-1.36-1.36c-.34-.67-.34-1.54-.34-3.29L-6-1.01' +
    'c0-1.75 0-2.62.34-3.29a3.12 3.12 0 0 1 1.36-1.36c.67-.34 1.54-.34 3.29-.34Z'

  // The tile, filling the whole box (22.5%, 60% smoothing).
  const TILE =
    'M15.36 0c3.02 0 4.54 0 5.69.59a5.4 5.4 0 0 1 2.36 2.36c.59 1.16.59 2.67.59 5.69L24 15.36' +
    'c0 3.02 0 4.54-.59 5.69a5.4 5.4 0 0 1-2.36 2.36c-1.16.59-2.67.59-5.69.59L8.64 24' +
    'c-3.02 0-4.54 0-5.69-.59a5.4 5.4 0 0 1-2.36-2.36c-.59-1.16-.59-2.67-.59-5.69L0 8.64' +
    'c0-3.02 0-4.54.59-5.69a5.4 5.4 0 0 1 2.36-2.36c1.16-.59 2.67-.59 5.69-.59Z'

  // Back to front, as on the icon: each card further down and to the right,
  // and turned eight degrees more. On the tile they keep the icon's margin;
  // the bare mark has no tile to keep a margin inside, so it fills the box.
  const places = $derived(
    tile
      ? [
          'translate(8.3 8.3) rotate(-8) scale(.875)',
          'translate(12 12) scale(.875)',
          'translate(15.7 15.7) rotate(8) scale(.875)',
        ]
      : ['translate(8 8) rotate(-8)', 'translate(12 12)', 'translate(16 16) rotate(8)'],
  )
</script>

<!--
  The same three cards as the application icon (crates/everyday-app/icons/icon.svg),
  redrawn on a 24-unit grid. It is not a scaled-down copy. The icon's glyphs
  are left off, because at 20px a sparkle, a tick and a pen nib are three
  specks. And each card is cut away a little round the one in front of it
  (the masks) instead of being separated by a shadow, because at this size
  a shadow closes up into a smudge and the gap is what still reads as three
  cards.
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
  <defs>
    <mask id="gap-back-{uid}" maskUnits="userSpaceOnUse" x="0" y="0" width="24" height="24">
      <rect width="24" height="24" fill="#fff" />
      <path d={CARD} transform={places[1]} fill="#000" stroke="#000" stroke-width="1.8" />
    </mask>
    <mask id="gap-middle-{uid}" maskUnits="userSpaceOnUse" x="0" y="0" width="24" height="24">
      <rect width="24" height="24" fill="#fff" />
      <path d={CARD} transform={places[2]} fill="#000" stroke="#000" stroke-width="1.8" />
    </mask>
    {#if tile}
      <linearGradient id="night-{uid}" x1="0" y1="0" x2="0" y2="1">
        <stop offset="0" stop-color="#2a1f47" />
        <stop offset="1" stop-color="#120d22" />
      </linearGradient>
      <linearGradient id="violet-{uid}" x1="0" y1="0" x2="1" y2="1">
        <stop offset="0" stop-color="#6f6cff" />
        <stop offset="1" stop-color="#a557ff" />
      </linearGradient>
      <linearGradient id="coral-{uid}" x1="0" y1="0" x2="1" y2="1">
        <stop offset="0" stop-color="#ff4f8b" />
        <stop offset="1" stop-color="#ff7a59" />
      </linearGradient>
      <linearGradient id="amber-{uid}" x1="0" y1="0" x2="1" y2="1">
        <stop offset="0" stop-color="#ff8f2e" />
        <stop offset="1" stop-color="#ffc24a" />
      </linearGradient>
    {/if}
  </defs>
  {#if tile}
    <path d={TILE} fill="url(#night-{uid})" />
  {/if}
  <!-- The bare mark steps its opacity down towards the back, the way the
       icon's colours cool towards the back. -->
  <g mask="url(#gap-back-{uid})">
    <path
      d={CARD}
      transform={places[0]}
      fill={tile ? `url(#violet-${uid})` : 'currentColor'}
      opacity={tile ? 1 : 0.38}
    />
  </g>
  <g mask="url(#gap-middle-{uid})">
    <path
      d={CARD}
      transform={places[1]}
      fill={tile ? `url(#coral-${uid})` : 'currentColor'}
      opacity={tile ? 1 : 0.62}
    />
  </g>
  <path d={CARD} transform={places[2]} fill={tile ? `url(#amber-${uid})` : 'currentColor'} />
</svg>

<style>
  .logo {
    display: block;
    flex: none;
  }
</style>
