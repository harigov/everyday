<script lang="ts">
  // Karma a day, a square a day -- the shape GitHub made everybody read.
  //
  // Diverging rather than sequential, because Karma has a sign: a day can
  // come to less than nothing. Two hues and a neutral middle -- teal for
  // what was earned, orange for what was lost, the sunken surface for a day
  // that came to nothing either way -- never green and red, which are the
  // one pair a red-green colour-blind reader cannot tell apart. The pair is
  // fixed rather than a theme token: it is checked for colour-blind
  // separation once, here, against both a light and a dark surface, and a
  // theme that tinted it would undo that.
  //
  // The steps are fixed bands of the day's ceiling (`karmaLevel`), not a
  // scale fitted to the best day on screen, so a square means the same in
  // every window. Days before Karma started are drawn faint: they are not
  // zero days, they are days nothing was counted.
  //
  // Colour never carries a day alone: hovering or focusing a square writes
  // the day out underneath -- what was earned, what was lost, and why.

  import { weekdayShort } from '../lib/format'
  import { DEED_LABELS, karmaLevel, signed } from '../lib/karma'
  import { addDays, todayIso } from '../lib/time'
  import type { KarmaDay } from '../lib/types'

  interface Props {
    days: KarmaDay[]
    from: string
    to: string
    /** The day Karma started. */
    since: string
    weekStart: number
  }

  const { days, from, to, since, weekStart }: Props = $props()

  const byDate = $derived(new Map(days.map((d) => [d.date, d])))

  /** Columns of seven, oldest first, aligned to the week's first day. */
  const columns = $derived.by(() => {
    const start = new Date(`${from}T00:00:00`)
    const shift = (start.getDay() - weekStart + 7) % 7
    let at = addDays(from, -shift)
    const out: string[][] = []
    for (let guard = 0; at <= to && guard < 60; guard++) {
      out.push(Array.from({ length: 7 }, (_, i) => addDays(at, i)))
      at = addDays(at, 7)
    }
    return out
  })

  /**
   * A month's name over the column holding its first day. The opening
   * column is named after its own month too, unless a first falls within
   * the next three columns, where the two names would run into each other.
   */
  const months = $derived.by(() => {
    const name = (iso: string) =>
      new Date(`${iso}T00:00:00`).toLocaleDateString(undefined, { month: 'short' })
    const firsts = columns.map((week) => week.find((d) => d.endsWith('-01')) ?? null)
    return columns.map((week, i) => {
      const first = firsts[i]
      if (first) return name(first)
      if (i === 0 && !firsts.slice(1, 4).some(Boolean)) return name(week[0]!)
      return ''
    })
  })

  /** Every other row named, GitHub's way, from the days the first column holds. */
  const rowNames = $derived(
    (columns[0] ?? []).map((iso, i) => (i % 2 === 1 ? weekdayShort(iso) : '')),
  )

  // Opened on the newest weeks: a card too narrow for the whole year scrolls
  // sideways, and what was done this week is what is looked for.
  let scroller: HTMLDivElement | undefined = $state()
  $effect(() => {
    void columns.length
    if (scroller) scroller.scrollLeft = scroller.scrollWidth
  })

  let picked = $state<string | null>(null)
  const shown = $derived(picked ?? todayIso())

  function when(iso: string): string {
    return new Date(`${iso}T00:00:00`).toLocaleDateString(undefined, {
      weekday: 'short',
      month: 'short',
      day: 'numeric',
    })
  }

  function title(iso: string): string {
    if (iso < since) return `${when(iso)} — before Karma started`
    const day = byDate.get(iso)
    if (!day) return `${when(iso)} — nothing counted`
    return `${when(iso)} — ${signed(day.net)} Karma`
  }

  const detail = $derived.by(() => {
    const day = byDate.get(shown)
    if (shown < since) return { head: `${when(shown)}: before Karma started`, deeds: [] }
    if (!day) return { head: `${when(shown)}: nothing counted yet`, deeds: [] }
    const parts = [`${signed(day.net)} Karma`]
    if (day.lost < 0) parts.push(`${signed(day.earned)} earned, ${signed(day.lost)} lost`)
    return { head: `${when(shown)}: ${parts.join(' · ')}`, deeds: day.deeds.slice(0, 6) }
  })
</script>

<div class="wrap">
  <div class="scroller" bind:this={scroller}>
    <!-- Labelled here, on a box of its own: the rows and weeks inside are
         laid straight into this grid (`display: contents`), and WebKit has
         dropped elements drawn that way from what a screen reader is told. -->
    <div class="grid" role="group" aria-label="Karma a day" style="--weeks: {columns.length}">
      <!-- The corner over the names, then a name for every other row. -->
      <div class="rows">
        <span aria-hidden="true"></span>
        {#each rowNames as name, i (i)}<span aria-hidden="true">{name}</span>{/each}
      </div>
      {#each columns as week, i (week[0])}
        <div class="week">
          <span class="month" aria-hidden="true">{months[i]}</span>
          {#each week as iso (iso)}
            {@const inRange = iso >= from && iso <= to}
            {@const counted = inRange && iso >= since && iso <= todayIso()}
            <button
              class="cell"
              class:out={!counted}
              class:today={iso === todayIso()}
              data-level={counted ? karmaLevel(byDate.get(iso)?.net ?? 0) : 0}
              title={inRange ? title(iso) : ''}
              aria-label={inRange ? title(iso) : undefined}
              tabindex={inRange ? 0 : -1}
              disabled={!inRange}
              onclick={() => (picked = iso)}
              onpointerenter={() => (picked = iso)}
              onfocus={() => (picked = iso)}
              onpointerleave={() => (picked = null)}
              onblur={() => (picked = null)}
            ></button>
          {/each}
        </div>
      {/each}
    </div>
  </div>

  <div class="foot">
    <div class="detail" aria-live="polite">
      <p class="head">{detail.head}</p>
      {#if detail.deeds.length > 0}
        <ul>
          {#each detail.deeds as deed, i (i)}
            <li>
              <span class="pts" class:neg={deed.points < 0}>{signed(deed.points)}</span>
              <span class="what">{deed.label || DEED_LABELS[deed.kind]}</span>
              <span class="kind">{DEED_LABELS[deed.kind]}</span>
            </li>
          {/each}
        </ul>
      {/if}
    </div>

    <div class="key" aria-hidden="true">
      <span>Lost</span>
      {#each [-4, -3, -2, -1, 0, 1, 2, 3, 4] as level (level)}
        <span class="cell swatch" data-level={level}></span>
      {/each}
      <span>Earned</span>
    </div>
  </div>
</div>

<style>
  .wrap {
    --good: #0d9488;
    --bad: #e8590c;
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }

  /* One grid for the names of the days, the months and the squares, so the
     names line up with their rows at whatever size the squares come out.
     The squares fill the card's width up to a size that still reads as a
     grid of days rather than a chequerboard, and shrink together when the
     card is narrow: they were a fixed 11px, which on a full-width card drew
     the year in its first third and left the rest of it empty. Never below
     10px -- small enough to fit a year in a third of a wide window, large
     enough to point at, and no shorter than the names beside the rows -- and
     past that the year scrolls sideways, as it always did. */
  .scroller {
    overflow-x: auto;
    padding-bottom: 2px;
  }
  .grid {
    display: grid;
    grid-auto-flow: column;
    grid-template-rows: 16px repeat(7, auto);
    grid-template-columns: max-content repeat(var(--weeks), minmax(10px, 24px));
    gap: 3px;
  }
  .rows,
  .week {
    display: contents;
  }
  .rows span {
    align-self: center;
    padding-right: var(--sp-1);
    color: var(--fg-faint);
    font-size: 10px;
    line-height: 1;
  }

  .month {
    overflow: visible;
    color: var(--fg-faint);
    font-size: 10px;
    line-height: 16px;
    white-space: nowrap;
  }

  .cell {
    display: block;
    width: 100%;
    aspect-ratio: 1;
    padding: 0;
    border: 0;
    border-radius: 3px;
    background: var(--bg-sunken);
  }
  button.cell:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .cell.today {
    box-shadow: inset 0 0 0 1px var(--fg-faint);
  }

  .cell[data-level='1'] {
    background: color-mix(in oklab, var(--good) 30%, var(--bg-sunken));
  }
  .cell[data-level='2'] {
    background: color-mix(in oklab, var(--good) 55%, var(--bg-sunken));
  }
  .cell[data-level='3'] {
    background: color-mix(in oklab, var(--good) 78%, var(--bg-sunken));
  }
  .cell[data-level='4'] {
    background: var(--good);
  }
  .cell[data-level='-1'] {
    background: color-mix(in oklab, var(--bad) 30%, var(--bg-sunken));
  }
  .cell[data-level='-2'] {
    background: color-mix(in oklab, var(--bad) 55%, var(--bg-sunken));
  }
  .cell[data-level='-3'] {
    background: color-mix(in oklab, var(--bad) 78%, var(--bg-sunken));
  }
  .cell[data-level='-4'] {
    background: var(--bad);
  }

  /* Outside the window, or before Karma started: drawn, so the grid keeps
     its shape, and faint, so they are not read as zero days. */
  .cell.out {
    opacity: 0.3;
  }

  /* Under the squares: the day being looked at on the left, the key on the
     right, side by side while the card is wide enough for both. */
  .foot {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--sp-3) var(--sp-6);
  }

  .key {
    display: flex;
    flex: none;
    align-items: center;
    gap: 3px;
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }
  .key span:first-child {
    margin-right: var(--sp-1);
  }
  .key span:last-child {
    margin-left: var(--sp-1);
  }
  .swatch {
    width: 12px;
  }

  .detail {
    flex: 1;
    min-width: min(100%, 18rem);
    min-height: 2.6em;
  }
  .head {
    margin: 0;
    color: var(--fg);
    font-size: var(--text-sm);
    font-weight: 550;
  }
  /* Held to a reading width, so on a full-width card a deed's kind stays
     beside its name rather than at the far edge of the window. */
  ul {
    display: flex;
    flex-direction: column;
    gap: 2px;
    max-width: 30rem;
    margin: var(--sp-1) 0 0;
    padding: 0;
    list-style: none;
  }
  li {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    font-size: var(--text-sm);
  }
  .pts {
    flex: none;
    min-width: 3ch;
    color: var(--fg);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    text-align: right;
  }
  .what {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    color: var(--fg);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .kind {
    flex: none;
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }

  /* A phone: squares a fingertip can land on -- 24 px, with a little gap --
     so the year is the strip it scrolls along rather than a grid of
     specks, opened on the newest weeks (see `scroller`). */
  :global([data-phone]) .grid {
    grid-template-columns: max-content repeat(var(--weeks), 24px);
    gap: 4px;
  }
</style>
