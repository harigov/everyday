<script lang="ts">
  // A tracker's icon, in its tile.
  //
  // Two things rather than one, because they always travel together: the
  // glyph and the soft square of the tracker's own colour behind it. That
  // tile is what makes a row of twenty chips scannable -- at a glance you
  // are matching colours and silhouettes, not reading five-letter words --
  // and it is the reason this set is filled rather than stroked.

  import { trackerIconPath } from '../lib/tracker-icons'

  let {
    name,
    color = 'currentColor',
    size = 30,
    /** Draw the tile behind the glyph. Off gives the bare mark. */
    tile = true,
    /** Solid tile with the glyph knocked out: what "recorded" looks like. */
    solid = false,
  }: {
    name: string
    color?: string
    size?: number
    tile?: boolean
    solid?: boolean
  } = $props()

  // The glyph is drawn at 62% of the tile, which is the proportion that keeps
  // a 20-unit mark optically centred in a rounded square rather than crowding
  // its corners.
  const glyph = $derived(Math.round(size * 0.62))
</script>

<span
  class="tile"
  class:bare={!tile}
  class:solid
  style="--c: {color}; --size: {size}px; --radius: {Math.round(size * 0.3)}px"
>
  <svg width={glyph} height={glyph} viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
    <!-- eslint-disable-next-line svelte/no-at-html-tags -- compile-time constants from tracker-icons.ts -->
    {@html trackerIconPath(name)}
  </svg>
</span>

<style>
  .tile {
    display: grid;
    place-items: center;
    flex: none;
    width: var(--size);
    height: var(--size);
    border-radius: var(--radius);
    /* The tint is mixed against the page rather than being a fixed alpha, so
       one definition works in both themes: 14% of the tracker's colour over
       whatever is behind it. */
    background: color-mix(in oklab, var(--c) 14%, transparent);
    color: var(--c);
  }

  .tile.solid {
    background: var(--c);
    color: var(--bg-raised);
  }

  .tile.bare {
    background: none;
    width: auto;
    height: auto;
  }
</style>
