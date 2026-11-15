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
  // straight up. Worked out here rather than with rotate() so every part
  // stays in one coordinate space, and the gold runs top to bottom across
  // the whole wheel instead of turning with each spoke.
  const at = (angle: number, r: number) => {
    const a = (angle * Math.PI) / 180
    return { x: r * Math.sin(a), y: -r * Math.cos(a) }
  }

  // Eight spokes from inside the hub to under the rim, with the medallion
  // (or the slim spoke's bead) halfway along.
  const spokes = (offset: number) =>
    Array.from({ length: 8 }, (_, k) => {
      const angle = offset + k * 45
      return { angle, from: at(angle, 2), to: at(angle, 9.4), mid: at(angle, 6.05) }
    })
  const BROAD = spokes(0)
  const SLIM = spokes(22.5)

  // The wheel is drawn 11 in radius. On the tile it keeps the icon's
  // proportion, 312 of an 824 tile; bare, it fills the box.
  const scale = $derived(tile ? 9.1 / 11 : 1)
  const ink = $derived(tile ? `url(#gold-${uid})` : 'currentColor')
</script>

<!--
  The same Konark wheel as the application icon
  (crates/karmayoga-app/icons/icon.svg), redrawn on a 24-unit grid. It is
  not a scaled-down copy: at 18px the icon's lit edges, shadow and filleted
  joints close up into a smudge, so here the spokes are plain strokes, a
  little heavier than the icon's, the medallions are holes without their
  dot and the rim is one band.
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
      <radialGradient id="sky-{uid}" cx=".5" cy=".46" r=".62">
        <stop offset="0" stop-color="#262a72" />
        <stop offset="1" stop-color="#0a0c26" />
      </radialGradient>
      <linearGradient id="gold-{uid}" x1="0" y1="-11" x2="0" y2="11" gradientUnits="userSpaceOnUse">
        <stop offset="0" stop-color="#ffe596" />
        <stop offset=".5" stop-color="#ffc24d" />
        <stop offset="1" stop-color="#ea8a2a" />
      </linearGradient>
    {/if}
    <mask id="cuts-{uid}">
      <rect x="-12" y="-12" width="24" height="24" fill="#fff" />
      <!-- The groove round the hub's boss. -->
      <circle r="1.65" fill="none" stroke="#000" stroke-width=".45" />
      {#each BROAD as spoke (spoke.angle)}
        <circle cx={spoke.mid.x} cy={spoke.mid.y} r=".6" fill="#000" />
      {/each}
    </mask>
  </defs>
  {#if tile}
    <path d={TILE} fill="url(#sky-{uid})" />
  {/if}
  <g transform="translate(12 12) scale({scale})" mask="url(#cuts-{uid})" fill={ink}>
    <circle r="10.15" fill="none" stroke={ink} stroke-width="1.7" />
    {#each BROAD as spoke (spoke.angle)}
      <line
        x1={spoke.from.x}
        y1={spoke.from.y}
        x2={spoke.to.x}
        y2={spoke.to.y}
        stroke={ink}
        stroke-width="1.3"
      />
      <circle cx={spoke.mid.x} cy={spoke.mid.y} r="1.35" />
    {/each}
    {#each SLIM as spoke (spoke.angle)}
      <line
        x1={spoke.from.x}
        y1={spoke.from.y}
        x2={spoke.to.x}
        y2={spoke.to.y}
        stroke={ink}
        stroke-width=".5"
      />
      <circle cx={spoke.mid.x} cy={spoke.mid.y} r=".45" />
    {/each}
    <circle r="2.7" />
  </g>
</svg>

<style>
  .logo {
    display: block;
    flex: none;
  }
</style>
