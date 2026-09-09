<script lang="ts">
  // How much of something is done, as a filled circle.
  //
  // The shape Things uses beside a project, and it is the right one for a
  // reason worth writing down: a number like "2/5" has to be read, and a
  // filled circle is *seen*. Down a list of twenty rows that is the whole
  // difference between scanning and reading.
  //
  // A pie rather than a ring, so a finished thing is a solid disc. A ring at
  // 100% is a ring, which looks like a thing still in progress; a filled disc
  // looks finished, which is what it is. The outline stays either way, so an
  // untouched item is an empty circle rather than nothing at all.

  let {
    done,
    total,
    size = 15,
    /** Draws in the accent unless a row wants its own colour. */
    color = 'currentColor',
    /** Read out and shown on hover. Falls back to "2 of 5 done". */
    title,
  }: {
    done: number
    total: number
    size?: number
    color?: string
    title?: string
  } = $props()

  const fraction = $derived(total > 0 ? Math.min(1, Math.max(0, done / total)) : 0)
  const complete = $derived(total > 0 && done >= total)
  const label = $derived(title ?? `${done} of ${total} done`)

  // A wedge as a stroked arc rather than as a path with an arc command: one
  // number changes, the geometry does not, and there is no case to special
  // for the wedge that crosses 180°. The stroke is half the radius wide and
  // sits at half the radius out, so it paints from the centre to the edge.
  const R = 10
  const CIRCUMFERENCE = 2 * Math.PI * (R / 2)
</script>

<span class="pie" style="--size: {size}px; --tint: {color}" {title} aria-label={label} role="img">
  <svg viewBox="0 0 24 24" width={size} height={size} aria-hidden="true">
    <circle class="track" cx="12" cy="12" r={R} fill="none" stroke-width="1.6" />
    {#if complete}
      <circle class="fill" cx="12" cy="12" r={R} />
    {:else if fraction > 0}
      <circle
        class="wedge"
        cx="12"
        cy="12"
        r={R / 2}
        fill="none"
        stroke-width={R}
        stroke-dasharray="{fraction * CIRCUMFERENCE} {CIRCUMFERENCE}"
        transform="rotate(-90 12 12)"
      />
    {/if}
  </svg>
</span>

<style>
  .pie {
    display: inline-flex;
    flex: none;
    line-height: 0;
    color: var(--tint);
  }
  .track {
    stroke: currentColor;
    opacity: 0.35;
  }
  .fill,
  .wedge {
    fill: currentColor;
    stroke: currentColor;
    /* The wedge grows rather than jumping: a subtask ticked off should look
       like it moved the dial, which is most of why the dial is here. */
    transition: stroke-dasharray var(--med) var(--ease);
  }
  @media (prefers-reduced-motion: reduce) {
    .wedge {
      transition: none;
    }
  }
</style>
