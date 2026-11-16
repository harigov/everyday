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

  // A point `r` out from the wheel's centre, `angle` degrees clockwise from
  // straight up.
  const at = (angle: number, r: number) => {
    const a = (angle * Math.PI) / 180
    return { x: r * Math.sin(a), y: -r * Math.cos(a) }
  }

  // Four lines through the centre make the eight spokes, stopping at the
  // glass; eight amber rays float between them.
  const SPOKES = [0, 45, 90, 135].map((angle) => ({
    angle,
    from: at(angle, -8.5),
    to: at(angle, 8.5),
  }))
  const RAYS = Array.from({ length: 8 }, (_, k) => {
    const angle = 22.5 + k * 45
    return { angle, from: at(angle, 4.77), to: at(angle, 7.19) }
  })

  // The bezel's marks are dashes along the rim's middle, radius 9.75. A
  // circle's stroke starts at three o'clock, an hour mark, and half a dash of
  // offset centres each mark on its hour.
  const RIM = 9.75
  const around = 2 * Math.PI * RIM
  const HOURS = `0.32 ${around / 24 - 0.32}`
  const BETWEEN = `0.13 ${around / 72 - 0.13}`

  // The wheel is drawn 11 in radius. On the tile it keeps the icon's
  // proportion, 300 of an 824 tile; bare, it fills the box. The fine marks
  // and the glass's lit edges only show from 40px up.
  const scale = $derived(tile ? 8.74 / 11 : 1)
  const fine = $derived(size >= 40)
</script>

<!--
  The same Konark wheel as the application icon
  (crates/karmayoga-app/icons/icon.svg), redrawn on a 24-unit grid: a rim of
  frosted glass ruled with the hours, slim ice-white spokes, and the sun
  with its amber rays. It is not a scaled-down copy: at 18px the icon's
  spokes and rays thin to nothing and its shadows are a smudge, so here they
  are a little heavier and there are no shadows.

  Bare, in currentColor, the glass is the ink at low strength with the hour
  marks at full strength on it, the rays are softer than the spokes, and a
  groove round the hub keeps it from reading as a plain dot.
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
    {#if tile}
      <linearGradient id="sky-{uid}" x1="0" y1="0" x2="0" y2="1">
        <stop offset="0" stop-color="#3fd2ff" />
        <stop offset=".55" stop-color="#1a7cf5" />
        <stop offset="1" stop-color="#0b4fd6" />
      </linearGradient>
      <radialGradient id="sheen-{uid}" cx="12" cy="1.1" r="16.3" gradientUnits="userSpaceOnUse">
        <stop offset="0" stop-color="#fff" stop-opacity=".28" />
        <stop offset="1" stop-color="#fff" stop-opacity="0" />
      </radialGradient>
      <linearGradient id="ice-{uid}" x1="0" y1="-10" x2="0" y2="10" gradientUnits="userSpaceOnUse">
        <stop offset="0" stop-color="#f3f8ff" />
        <stop offset="1" stop-color="#d6e5fa" />
      </linearGradient>
      <linearGradient
        id="amber-{uid}"
        x1="0"
        y1="-7.5"
        x2="0"
        y2="7.5"
        gradientUnits="userSpaceOnUse"
      >
        <stop offset="0" stop-color="#ffd25a" />
        <stop offset="1" stop-color="#ff9a2a" />
      </linearGradient>
      <linearGradient
        id="glass-{uid}"
        x1="0"
        y1="-11"
        x2="0"
        y2="11"
        gradientUnits="userSpaceOnUse"
      >
        <stop offset="0" stop-color="#fff" stop-opacity=".55" />
        <stop offset="1" stop-color="#fff" stop-opacity=".22" />
      </linearGradient>
      <linearGradient id="sun-{uid}" x1="0" y1="-2" x2="0" y2="2" gradientUnits="userSpaceOnUse">
        <stop offset="0" stop-color="#ffd25a" />
        <stop offset="1" stop-color="#ff8a1f" />
      </linearGradient>
    {:else}
      <mask id="groove-{uid}">
        <rect x="-12" y="-12" width="24" height="24" fill="#fff" />
        <circle r="2.65" stroke="#000" stroke-width=".55" />
      </mask>
    {/if}
  </defs>
  {#if tile}
    <path d={TILE} fill="url(#sky-{uid})" />
    <path d={TILE} fill="url(#sheen-{uid})" />
  {/if}
  <g transform="translate(12 12) scale({scale})">
    <g
      mask={tile ? undefined : `url(#groove-${uid})`}
      stroke={tile ? `url(#ice-${uid})` : 'currentColor'}
      fill={tile ? `url(#ice-${uid})` : 'currentColor'}
    >
      {#each SPOKES as spoke (spoke.angle)}
        <line
          x1={spoke.from.x}
          y1={spoke.from.y}
          x2={spoke.to.x}
          y2={spoke.to.y}
          stroke-width="1.05"
        />
      {/each}
      <circle r="3.08" stroke="none" />
    </g>

    <circle
      r={RIM}
      stroke={tile ? `url(#glass-${uid})` : 'currentColor'}
      stroke-opacity={tile ? 1 : 0.3}
      stroke-width="2.5"
    />
    {#if tile && fine}
      <circle r="10.95" stroke="#fff" stroke-opacity=".85" stroke-width=".1" />
      <circle r="8.55" stroke="#fff" stroke-opacity=".5" stroke-width=".1" />
    {/if}
    {#if fine}
      <circle
        r={RIM}
        stroke={tile ? '#fff' : 'currentColor'}
        stroke-opacity=".6"
        stroke-width=".73"
        stroke-dasharray={BETWEEN}
        stroke-dashoffset=".065"
      />
    {/if}
    <circle
      r={RIM}
      stroke={tile ? '#fff' : 'currentColor'}
      stroke-opacity=".95"
      stroke-width="1.32"
      stroke-dasharray={HOURS}
      stroke-dashoffset=".16"
    />

    <g
      stroke={tile ? `url(#amber-${uid})` : 'currentColor'}
      stroke-opacity={tile ? 1 : 0.6}
      stroke-width=".6"
      stroke-linecap="round"
    >
      {#each RAYS as ray (ray.angle)}
        <line x1={ray.from.x} y1={ray.from.y} x2={ray.to.x} y2={ray.to.y} />
      {/each}
    </g>
    {#if tile}
      <circle r="1.98" fill="url(#sun-{uid})" />
    {/if}
  </g>
</svg>

<style>
  .logo {
    display: block;
    flex: none;
  }
</style>
