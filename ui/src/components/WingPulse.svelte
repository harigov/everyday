<script lang="ts">
  import { BOX, FEATHERS, ROOT, along } from '../lib/wing'

  let {
    size = 18,
  }: {
    /** In CSS pixels, square. */
    size?: number
  } = $props()

  // A step deeper than the icon's gold, and edged: the icon's palest front
  // feather sits on midnight there, but here it may sit on a white page,
  // where it would all but vanish.
  const WARM = ['#ffd24d', '#ffa536', '#ff7424', '#e65a1c'] as const

  // Back feather first. Each one closes and opens a little further than the
  // one in front of it, and a beat after it, so the change runs out through
  // the wing like a breath rather than the fan swinging as one piece.
  const STACK = FEATHERS.map((d, i) => ({
    d,
    i,
    fill: along(WARM, i / (FEATHERS.length - 1)),
    style:
      `--close: ${(1.2 + 2.3 * i).toFixed(1)}deg; --open: ${(-1.2 - 1.5 * i).toFixed(1)}deg; ` +
      `animation-delay: ${(i * 0.06 - 2.4).toFixed(2)}s`,
  })).reverse()

  // The wing fills the box with room for its feathers to swing. Framed by
  // the viewBox rather than a transform on a group, so the root -- given in
  // the grid's own units -- is where each feather's rotation is centred.
  const side = Math.max(BOX.width, BOX.height) * 1.14
  const view = [BOX.x + BOX.width / 2 - side / 2, BOX.y + BOX.height / 2 - side / 2, side, side]
    .map((n) => n.toFixed(2))
    .join(' ')
</script>

<!--
  The application's wing at work: what the chat shows while a turn is still
  running, in place of three blinking dots. Its feathers open and close
  about their root, slowly, the change running outward through them.

  Plain SVG and CSS -- no canvas, no script after the first draw, no filters,
  which in WebKitGTK repaint on every frame -- and still with reduced motion
  asked for.
-->
<svg
  class="wing"
  width={size}
  height={size}
  viewBox={view}
  aria-hidden="true"
  focusable="false"
  style="--root-x: {ROOT.x}px; --root-y: {ROOT.y}px"
>
  {#each STACK as feather (feather.i)}
    <path class="feather" d={feather.d} fill={feather.fill} style={feather.style} />
  {/each}
</svg>

<style>
  .wing {
    display: block;
    flex: none;
    overflow: visible;
  }
  .feather {
    stroke: #b4530f;
    stroke-opacity: 0.4;
    stroke-width: 0.35;
    transform-box: view-box;
    transform-origin: var(--root-x) var(--root-y);
    animation: breathe 2.4s ease-in-out infinite;
  }
  @keyframes breathe {
    0%,
    100% {
      transform: rotate(var(--close));
    }
    50% {
      transform: rotate(var(--open));
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .feather {
      animation: none;
    }
  }
</style>
