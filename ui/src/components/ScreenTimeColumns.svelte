<script module lang="ts">
  import type { ScreenCategoryId } from '../lib/types'

  /** One column: an hour or a day, its time split by category. */
  export interface Column {
    /** Unique within the chart: an hour, a date. */
    id: string
    /** What the column is, in words, for its tooltip: "Tue 4 Nov", "9 AM". */
    name: string
    /** Written under the column, when set. Kept sparse by the caller. */
    tick?: string
    /** Bottom first, in the categories' own order. */
    segments: { category: ScreenCategoryId | null; secs: number; color: string }[]
    /** Open what the column stands for. Makes the column a button. */
    onpick?: () => void
    /** The button's verb: "Open on the timeline". */
    pickLabel?: string
  }
</script>

<script lang="ts">
  // Screen time over a range, as columns stacked by category: one per hour
  // of a day, or one per day of a week or a month.
  //
  // Columns, not a line, for the reason `ColumnChart` gives: an hour or a day
  // with nothing in it is a gap, not a slope down to zero and back. Stacked,
  // because the question beside "how much" is "of what", and the category
  // split under the chart is its legend -- the same swatches, named, with
  // their figures -- so the colour here only reinforces what that list says.
  //
  // The house marks: at most 20px wide, 4px rounded at the data end and
  // square on the baseline, a 2px surface gap between segments, one solid
  // hairline for the axis. Only the tallest column is labelled; everything
  // else is in each column's tooltip and accessible name.
  //
  // A column that can be opened (a day, onto the timeline) is a real button
  // the width of its slot and the height of the plot, so the target is the
  // time it stands for and not just the ink.

  import { formatDuration } from '../lib/screentime'
  import { screentime } from '../lib/screentime.svelte'

  interface Props {
    columns: Column[]
    /** Plot height in pixels, the axis band below it not included. */
    height?: number
    /** Read out before the columns: "Screen time by hour". */
    label: string
  }

  const { columns, height = 132, label }: Props = $props()

  const totals = $derived(columns.map((c) => c.segments.reduce((sum, s) => sum + s.secs, 0)))
  const ceiling = $derived(Math.max(1, ...totals))
  /** The tallest column, labelled; the first of equals. */
  const peak = $derived(totals.reduce((best, t, i) => (t > (totals[best] ?? 0) ? i : best), 0))
  const gap = $derived(columns.length > 40 ? 1 : 2)

  function describe(c: Column, total: number): string {
    if (total === 0) return `${c.name}: nothing`
    const parts = c.segments
      .filter((s) => s.secs > 0)
      .sort((a, b) => b.secs - a.secs)
      .map(
        (s) =>
          `${screentime.categoryOf(s.category)?.name ?? 'Uncategorised'} ${formatDuration(s.secs)}`,
      )
    return `${c.name}: ${formatDuration(total)}\n${parts.join('\n')}`
  }
</script>

<div class="chart" style:--h="{height}px" style:--gap="{gap}px">
  <div class="plot" role="group" aria-label={label}>
    {#each columns as column, i (column.id)}
      {@const total = totals[i] ?? 0}
      {@const at = columns.length > 1 ? i / (columns.length - 1) : 0.5}
      {@const tip = describe(column, total)}
      {@const spoken = tip.split('\n').join(', ')}
      <svelte:element
        this={column.onpick ? 'button' : 'div'}
        class="slot"
        class:pick={!!column.onpick}
        role={column.onpick ? 'button' : 'img'}
        aria-label={column.onpick && column.pickLabel ? `${spoken}. ${column.pickLabel}` : spoken}
        title={tip}
        onclick={column.onpick}
      >
        {#if i === peak && total > 0}
          <span
            class="peak"
            class:start={at < 0.12}
            class:end={at > 0.88}
            style:bottom="calc({(total / ceiling) * 100}% + 4px)">{formatDuration(total)}</span
          >
        {/if}
        {#if total > 0}
          <span class="col" style:height="max(3px, {(total / ceiling) * 100}%)">
            {#each column.segments as s, j (j)}
              {#if s.secs > 0}
                <span class="seg" style:flex-grow={s.secs} style:background={s.color}></span>
              {/if}
            {/each}
          </span>
        {/if}
      </svelte:element>
    {/each}
  </div>
  <!-- The baseline, and the only rule drawn. -->
  <div class="axis" aria-hidden="true">
    {#each columns as column (column.id)}
      <span class="tick">{column.tick ?? ''}</span>
    {/each}
  </div>
</div>

<style>
  .chart {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .plot {
    display: flex;
    align-items: flex-end;
    gap: var(--gap);
    height: var(--h);
    padding-top: var(--sp-5);
  }

  /* One slot per hour or day, empty or not, so the spacing is time. */
  .slot {
    position: relative;
    flex: 1 1 0;
    min-width: 0;
    height: 100%;
    display: flex;
    align-items: flex-end;
    justify-content: center;
    padding: 0;
    border-radius: 4px 4px 0 0;
  }
  .slot.pick {
    cursor: pointer;
  }
  .slot.pick:hover {
    background: var(--bg-hover);
  }
  .slot.pick:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  /* Capped, never the whole slot: the leftover is the air. Rounded at the
     data end only. The clip is on marks alone -- no text lives inside. */
  .col {
    display: flex;
    flex-direction: column-reverse;
    gap: 2px;
    width: 100%;
    max-width: 20px;
    border-radius: 4px 4px 0 0;
    overflow: hidden;
  }
  .seg {
    flex-basis: 0;
    min-height: 1px;
  }

  .peak {
    position: absolute;
    left: 50%;
    translate: -50% 0;
    white-space: nowrap;
    color: var(--fg-muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    pointer-events: none;
  }
  .peak.start {
    left: 0;
    translate: 0 0;
  }
  .peak.end {
    left: auto;
    right: 0;
    translate: 0 0;
  }

  .axis {
    display: flex;
    gap: var(--gap);
    padding-top: var(--sp-1);
    border-top: 1px solid var(--border);
  }
  /* A flex box, so a label wider than its slot overhangs both sides
     equally and stays centred under its column. */
  .tick {
    flex: 1 1 0;
    min-width: 0;
    display: flex;
    justify-content: center;
    white-space: nowrap;
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
</style>
