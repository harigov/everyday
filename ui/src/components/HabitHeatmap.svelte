<script lang="ts">
  // Four months of one habit, a square a day.
  //
  // Sequential rather than categorical: the question a cell answers is "how
  // much", so it is one hue from light to dark and never a second colour.
  // The hue is the tracker's own, so the map matches the chip somebody
  // already recognises, and the steps are opacity over the surface rather
  // than a ramp of mixed colours — which keeps it monotonic by construction
  // and correct in both themes without a second palette to validate.
  //
  // A day with no reading is not a light cell, it is an empty one: the
  // difference between "nothing recorded" and "recorded as nothing" is the
  // whole reason a `Check` stores a 0 rather than being absent, and a ramp
  // that started at the surface colour would erase it.

  import { addDays } from '../lib/time'
  import { formatValue } from '../lib/tracker'
  import type { Tracker, TrackerDay } from '../lib/types'

  interface Props {
    tracker: Tracker
    days: TrackerDay[]
    from: string
    to: string
    weekStart: number
  }

  const { tracker, days, from, to, weekStart }: Props = $props()

  const byDate = $derived(new Map(days.map((d) => [d.date, d])))

  /** The value a cell is worth, by what the tracker *is*. */
  function worth(day: TrackerDay): number {
    if (tracker.kind === 'check') return day.count
    if (tracker.kind === 'scale') return day.count === 0 ? 0 : day.sum / day.count
    return day.sum
  }

  const ceiling = $derived(Math.max(1, ...days.map(worth)))

  /** Columns of seven, oldest first, aligned to the week's first day. */
  const columns = $derived.by(() => {
    const start = new Date(`${from}T00:00:00`)
    // Back up to the week's start so every column is a real week and the
    // rows read as weekdays rather than as an arbitrary seven.
    const shift = (start.getDay() - weekStart + 7) % 7
    let at = addDays(from, -shift)
    const out: string[][] = []
    for (let guard = 0; at <= to && guard < 80; guard++) {
      out.push(Array.from({ length: 7 }, (_, i) => addDays(at, i)))
      at = addDays(at, 7)
    }
    return out
  })

  function level(iso: string): number {
    const day = byDate.get(iso)
    if (!day || day.count === 0) return 0
    // Four steps. Fewer than that and a good day looks like a poor one;
    // more and the difference between them stops being visible at 11px.
    return Math.min(4, Math.ceil((worth(day) / ceiling) * 4))
  }

  function title(iso: string): string {
    const day = byDate.get(iso)
    if (!day || day.count === 0) return `${iso} — nothing recorded`
    return `${iso} — ${formatValue(tracker, worth(day))}`
  }
</script>

<div class="map" role="img" aria-label="{tracker.name}: the last few months">
  {#each columns as week (week[0])}
    <div class="week">
      {#each week as iso (iso)}
        {@const inRange = iso >= from && iso <= to}
        <div
          class="cell"
          class:out={!inRange}
          data-level={inRange ? level(iso) : 0}
          style="--c: {tracker.color}"
          title={inRange ? title(iso) : ''}
        ></div>
      {/each}
    </div>
  {/each}
</div>

<style>
  .map {
    display: flex;
    gap: 2px;
    overflow-x: auto;
    padding-bottom: 2px;
  }

  .week {
    display: flex;
    flex: none;
    flex-direction: column;
    /* The same 2px surface gap that separates every other touching mark. */
    gap: 2px;
  }

  .cell {
    width: 11px;
    height: 11px;
    border-radius: 2px;
    background: var(--bg-sunken);
  }

  /* Nothing recorded keeps the sunken surface, so an empty day is visibly
     empty rather than being the palest step of the ramp. */
  .cell[data-level='1'] {
    background: color-mix(in oklab, var(--c) 28%, var(--bg-sunken));
  }
  .cell[data-level='2'] {
    background: color-mix(in oklab, var(--c) 52%, var(--bg-sunken));
  }
  .cell[data-level='3'] {
    background: color-mix(in oklab, var(--c) 76%, var(--bg-sunken));
  }
  .cell[data-level='4'] {
    background: var(--c);
  }

  /* Days the padding week reaches outside the window. Drawn, so the grid
     keeps its shape, and faint, so they are not read as data. */
  .cell.out {
    opacity: 0.25;
  }
</style>
