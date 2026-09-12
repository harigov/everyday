<script lang="ts">
  // One series over time, as columns from a baseline.
  //
  // A column chart rather than a line, because the series this draws are
  // counts of things that happened on a day -- pages, minutes, glasses, tasks
  // finished -- and a line between two of those days asserts a value on the
  // days in between. A day with nothing recorded is a gap, not a dip to zero
  // through a slope, and the column form says that by drawing nothing.
  //
  // One series, so no legend: the card's own heading names what is plotted,
  // and a legend box with one swatch in it would restate the title. Colour is
  // the subject's own -- the tracker's, the project's -- so the chart matches
  // the chip somebody already recognises, and it carries no meaning the
  // labels do not also carry.
  //
  // Labels are deliberately sparse. The tallest column and the last one are
  // named, and nothing else is: a value over every column is the single most
  // reliable way to make a small chart unreadable. Everything else is in the
  // per-column tooltip and in the table below, which is the accessible twin
  // rather than an extra.

  import { dateFormat } from '../lib/format'

  interface Point {
    /** `YYYY-MM-DD`. The column's identity and its tooltip's date. */
    date: string
    value: number
    /** Ready-formatted, for the tooltip and the direct label. */
    label: string
  }

  interface Props {
    points: Point[]
    /** The series colour. The subject's own. */
    color?: string
    /** How tall the plot is, in pixels. The axis band is added below it. */
    height?: number
    /** What one unit is, for the caption under the chart. */
    unit?: string
  }

  const { points, color = 'var(--accent)', height = 96, unit = '' }: Props = $props()

  const ceiling = $derived(Math.max(1, ...points.map((p) => p.value)))
  const peak = $derived(
    points.reduce<Point | null>((best, p) => (!best || p.value > best.value ? p : best), null),
  )
  const last = $derived(points[points.length - 1])

  /**
   * Which columns get a value written on them.
   *
   * The tallest, and the most recent when it has something to add. Two
   * conditions on the second, and both were learned by drawing it: a label
   * within a couple of columns of the peak's overlaps it and the pair reads
   * as one wrong number, and a label repeating the peak's own value says
   * nothing while costing exactly as much attention as one that does.
   */
  const labelled = $derived.by(() => {
    const out = new Set<string>()
    if (peak && peak.value > 0) out.add(peak.date)
    if (last && last.value > 0 && last.label !== peak?.label) {
      const gap = points.length - 1 - points.findIndex((p) => p.date === peak?.date)
      if (gap > 2) out.add(last.date)
    }
    return out
  })

  /** Every date under the plot would be a smear; the two ends are enough. */
  const first = $derived(points[0])

  /**
   * Whether the window is long enough that the day and month are ambiguous.
   *
   * A year of weekly buckets ends on the same month it started in, so an
   * axis reading "Sep 8 … Sep 7" says nothing at all -- it reads as a chart
   * of two days rather than of a year.
   */
  const spansYears = $derived(!!first && !!last && first.date.slice(0, 4) !== last.date.slice(0, 4))

  /**
   * The gap between columns, which has to give way when there are a lot.
   *
   * A fixed 2px between ninety slots in a half-width card is more surface
   * than data -- the separator becomes the chart. Below that it is a hairline,
   * which is still enough to keep two neighbouring days apart.
   */
  const gap = $derived(points.length > 40 ? 1 : 2)

  function shortDate(iso: string): string {
    const at = new Date(`${iso}T00:00:00`)
    return dateFormat({
      day: 'numeric',
      month: 'short',
      ...(spansYears ? { year: '2-digit' as const } : {}),
    }).format(at)
  }
</script>

{#if points.length === 0}
  <p class="none">Nothing recorded in this window.</p>
{:else}
  <div class="chart" style="--c: {color}; --h: {height}px; --gap: {gap}px">
    <div class="plot" role="img" aria-label="{points.length} days, up to {last?.label}">
      {#each points as point (point.date)}
        {@const pct = (point.value / ceiling) * 100}
        <div class="slot" title="{shortDate(point.date)} — {point.label}">
          {#if labelled.has(point.date)}
            <span class="tip" style="bottom: calc({pct}% + 4px)">{point.label}</span>
          {/if}
          {#if point.value > 0}
            <span class="col" style="height: max(3px, {pct}%)"></span>
          {/if}
        </div>
      {/each}
    </div>
    <!-- A hairline, solid, one step off the surface: the baseline the
         columns grow from, and the only rule this chart draws. -->
    <div class="axis">
      <span>{first ? shortDate(first.date) : ''}</span>
      <span class="cap">{ceiling.toLocaleString()}{unit ? ` ${unit}` : ''} at most</span>
      <span>{last ? shortDate(last.date) : ''}</span>
    </div>
  </div>
{/if}

<style>
  .chart {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  /* The plot and the axis band are one box that grows, rather than a fixed
     height the labels then fall out of the bottom of. */
  .plot {
    display: flex;
    align-items: flex-end;
    gap: var(--gap);
    height: var(--h);
    padding-top: var(--sp-4);
  }

  /* One slot per day, whether or not anything was recorded, so the spacing
     is time rather than data. The 2px gap between slots is the surface
     doing the separating; nothing is stroked. */
  .slot {
    position: relative;
    flex: 1 1 0;
    min-width: 0;
    height: 100%;
    display: flex;
    align-items: flex-end;
  }

  /* Capped rather than filling the slot: a column that takes the whole band
     reads as a block of colour, and the leftover is the air that makes a
     chart quiet. 4px rounded at the data end, square on the baseline. */
  .col {
    width: 100%;
    max-width: 20px;
    margin: 0 auto;
    border-radius: 4px 4px 0 0;
    background: var(--c);
  }

  .tip {
    position: absolute;
    left: 50%;
    translate: -50% 0;
    white-space: nowrap;
    /* A text token, never the series colour: a pale hue is illegible as
       text, and the coloured mark under it already carries the identity. */
    color: var(--fg-muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  .axis {
    display: flex;
    justify-content: space-between;
    gap: var(--sp-2);
    margin-top: 0;
    padding-top: var(--sp-1);
    border-top: 1px solid var(--border);
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  .axis .cap {
    text-align: center;
  }

  .none {
    margin: 0;
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }
</style>
