<script lang="ts">
  // The week at a glance, for the weekly brief: one column a day.
  //
  // Each column says how full the day is -- a column of its meeting hours
  // from a common baseline, on one scale for the whole week, so the heavy
  // day is visibly the heavy day -- and what else is on it: the trip or
  // holiday that runs across it, how many tasks fall due, and the forecast
  // when the brief has one. The busiest day is tinted behind and its hours
  // written over its column; the others carry theirs in a tooltip and in
  // the table read out in place of the picture. One series, so no legend:
  // the section's heading says what it is.
  //
  // An all-day item that runs over consecutive days is one chip across them
  // ("Lisbon offsite", Wednesday to Thursday), stacked in rows of its own
  // when two overlap, as a calendar's month view does.
  //
  // Colour: the columns are the calendar app's own; the text is text ink.

  import { weatherIcon } from '../lib/brief'
  import { dateFormat, dayNumber, formatMinutes, plural, weekdayShort } from '../lib/format'
  import { startOfDay } from '../lib/time'
  import type { BriefDay, BriefDigest, BriefWeatherDay } from '../lib/types'
  import Icon from './Icon.svelte'

  let { digest }: { digest: BriefDigest } = $props()

  const days = $derived(digest.week)
  const n = $derived(Math.max(1, days.length))

  /** One scale for the week: the busiest day, and never less than an hour. */
  const ceiling = $derived(Math.max(60, ...days.map((d) => d.minutes)))
  const busiest = $derived.by(() => {
    let best = -1
    days.forEach((d, i) => {
      if (d.minutes > 0 && (best < 0 || d.minutes > days[best]!.minutes)) best = i
    })
    return best
  })

  const forecast = $derived(
    new Map<string, BriefWeatherDay>((digest.weather?.outlook ?? []).map((o) => [o.date, o])),
  )
  const hasWeather = $derived(days.some((d) => forecast.has(d.date)))

  interface Span {
    title: string
    from: number
    len: number
    row: number
  }

  /** All-day items, joined across consecutive days and stacked so none overlap. */
  const spans = $derived.by(() => {
    const out: Span[] = []
    days.forEach((d, i) => {
      for (const title of d.allDay) {
        const run = out.find((s) => s.title === title && s.from + s.len === i)
        if (run) run.len += 1
        else out.push({ title, from: i, len: 1, row: 0 })
      }
    })
    out.sort((a, b) => a.from - b.from || b.len - a.len)
    const rowEnds: number[] = []
    for (const s of out) {
      let row = rowEnds.findIndex((end) => end <= s.from)
      if (row < 0) {
        row = rowEnds.length
        rowEnds.push(0)
      }
      rowEnds[row] = s.from + s.len
      s.row = row
    }
    return out
  })
  const spanRows = $derived(spans.reduce((most, s) => Math.max(most, s.row + 1), 0))

  // Grid rows, in order: the day, the forecast, the plot, the meetings, the
  // all-day rows, and what is due. Numbered here so every cell is placed
  // explicitly and the busiest day's tint can span them all.
  const ROW_HEAD = 1
  const rowWeather = $derived(hasWeather ? 2 : 0)
  const rowPlot = $derived(hasWeather ? 3 : 2)
  const rowCount = $derived(rowPlot + 1)
  const rowSpans = $derived(rowPlot + 2)
  const rowDue = $derived(rowSpans + spanRows)
  const template = $derived(
    [
      'auto',
      hasWeather ? 'auto' : '',
      'var(--plot-h)',
      'auto',
      ...Array.from({ length: spanRows }, () => 'auto'),
      'auto',
    ]
      .filter(Boolean)
      .join(' '),
  )

  function longDay(iso: string): string {
    return dateFormat({ weekday: 'long', day: 'numeric', month: 'long' }).format(startOfDay(iso))
  }

  function deg(t: number | null | undefined): string {
    return t == null ? '–' : `${Math.round(t)}°`
  }

  function meetingsLine(d: BriefDay): string {
    if (d.meetings > 0) return plural(d.meetings, 'meeting')
    if (d.events.length === 0 && d.allDay.length === 0) return 'Free'
    return 'No meetings'
  }

  function tip(d: BriefDay): string {
    const parts = [longDay(d.date)]
    if (d.meetings > 0) parts.push(`${plural(d.meetings, 'meeting')}, ${formatMinutes(d.minutes)}`)
    else parts.push('no meetings')
    for (const e of d.events) parts.push(e.title)
    return parts.join(' · ')
  }
</script>

{#if days.length > 0}
  <figure class="week" aria-label="The week at a glance">
    <div class="scroller">
      <div
        class="grid"
        aria-hidden="true"
        style="--n: {n}; grid-template-rows: {template}; --plot-h: 96px"
      >
        {#if busiest >= 0}
          <span class="busiest" style="grid-column: {busiest + 1}; grid-row: 1 / -1"></span>
        {/if}

        {#each days as d, i (d.date)}
          {@const col = i + 1}
          {@const wx = forecast.get(d.date)}
          {@const pct = (d.minutes / ceiling) * 100}

          <div
            class="head"
            style="grid-column: {col}; grid-row: {ROW_HEAD}"
            title={longDay(d.date)}
          >
            <span class="wd">{weekdayShort(d.date)}</span>
            <span class="dn">{dayNumber(d.date)}</span>
          </div>

          {#if rowWeather}
            <div class="wx" style="grid-column: {col}; grid-row: {rowWeather}">
              {#if wx}
                <span class="wx-icon" title={wx.condition}
                  ><Icon name={weatherIcon(wx.condition)} size={16} /></span
                >
                <span class="wx-t">{deg(wx.high)}<span class="wx-lo">{deg(wx.low)}</span></span>
              {/if}
            </div>
          {/if}

          <div class="plot" style="grid-column: {col}; grid-row: {rowPlot}" title={tip(d)}>
            <div class="scale">
              {#if d.minutes > 0}
                <span class="col" style="height: max(3px, {pct}%)"></span>
                {#if i === busiest}
                  <span class="cap" style="bottom: calc({pct}% + 4px)"
                    >{formatMinutes(d.minutes)}</span
                  >
                {/if}
              {/if}
            </div>
          </div>

          <div
            class="count"
            class:quiet={d.meetings === 0}
            style="grid-column: {col}; grid-row: {rowCount}"
          >
            {meetingsLine(d)}
          </div>

          <div class="due" style="grid-column: {col}; grid-row: {rowDue}">
            {#if d.due.length > 0}
              <span class="due-pill" title={d.due.join('\n')}>
                <Icon name="flag" size={11} weight={1.8} />
                {d.due.length} due
              </span>
            {/if}
          </div>
        {/each}

        {#each spans as s (`${s.title}-${s.from}`)}
          <span
            class="span"
            style="grid-column: {s.from + 1} / span {s.len}; grid-row: {rowSpans + s.row}"
            title="{s.title} · all day"
          >
            {s.title}
          </span>
        {/each}
      </div>
    </div>

    <!-- The picture as a table, read instead of it. -->
    <div class="sr-only">
      <table>
        <caption>The week ahead, day by day</caption>
        <thead>
          <tr>
            <th scope="col">Day</th>
            <th scope="col">Meetings</th>
            <th scope="col">All day</th>
            <th scope="col">Due</th>
            {#if hasWeather}<th scope="col">Weather</th>{/if}
          </tr>
        </thead>
        <tbody>
          {#each days as d, i (d.date)}
            {@const wx = forecast.get(d.date)}
            <tr>
              <th scope="row">{longDay(d.date)}{i === busiest ? ' (the busiest)' : ''}</th>
              <td
                >{d.meetings > 0
                  ? `${plural(d.meetings, 'meeting')}, ${formatMinutes(d.minutes)}`
                  : 'none'}</td
              >
              <td>{d.allDay.join(', ') || 'nothing'}</td>
              <td>{d.due.join(', ') || 'nothing'}</td>
              {#if hasWeather}
                <td>{wx ? `${wx.condition}, ${deg(wx.high)} / ${deg(wx.low)}` : '—'}</td>
              {/if}
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  </figure>
{/if}

<style>
  .week {
    position: relative;
    display: block;
    min-width: 0;
    margin: 0;
    padding: var(--sp-3) var(--sp-3) var(--sp-3);
    background: var(--bg-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--edge);
  }

  /* Seven columns need about 60px each to say anything; on a narrower pane
     the week scrolls sideways rather than squeezing every label away. */
  .scroller {
    /* Sideways only: `overflow-x` alone turns the other axis to `auto` too,
       and the busiest day's highlight then grew a vertical scrollbar. */
    overflow: auto hidden;
  }

  .grid {
    position: relative;
    display: grid;
    grid-template-columns: repeat(var(--n), minmax(0, 1fr));
    min-width: calc(var(--n) * 60px);
    row-gap: var(--sp-2);
  }

  /* The busiest day, tinted behind everything in its column. */
  .busiest {
    z-index: 0;
    margin: calc(var(--sp-1) * -1) 2px;
    border-radius: var(--radius);
    background: color-mix(in oklab, var(--app-calendar) 9%, transparent);
  }

  .head,
  .wx,
  .plot,
  .count,
  .due {
    position: relative;
    z-index: 1;
    min-width: 0;
  }

  .head {
    display: flex;
    flex-direction: column;
    align-items: center;
    padding-top: var(--sp-1);
    line-height: 1.2;
  }
  .wd {
    color: var(--fg-muted);
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .dn {
    color: var(--fg);
    font-size: var(--text-lg);
    font-weight: 600;
  }

  .wx {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 4px;
    color: var(--fg-muted);
  }
  .wx-icon {
    display: flex;
  }
  .wx-t {
    display: inline-flex;
    gap: 3px;
    color: var(--fg);
    font-size: var(--text-xs);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .wx-lo {
    color: var(--fg-subtle);
    font-weight: 450;
  }

  /* The baseline, a solid hairline right across the week: every column
     grows from the same line. No column gap, so it does not break. */
  .plot {
    border-bottom: 1px solid var(--border-strong);
  }
  /* Headroom above the tallest column for its label. */
  .scale {
    position: absolute;
    inset: 18px 0 0 0;
  }
  /* At most 24px wide and never the whole slot -- the leftover is the air
     that keeps a row of columns quiet. Rounded at the data end, square at
     the baseline. */
  .col {
    position: absolute;
    bottom: 0;
    left: 50%;
    width: min(24px, 56%);
    translate: -50% 0;
    border-radius: 4px 4px 0 0;
    background: var(--app-calendar);
  }
  .cap {
    position: absolute;
    left: 0;
    right: 0;
    color: var(--fg-muted);
    font-size: var(--text-xs);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    text-align: center;
    white-space: nowrap;
  }

  .count {
    padding: 0 2px;
    color: var(--fg-muted);
    font-size: var(--text-xs);
    text-align: center;
  }
  .count.quiet {
    color: var(--fg-faint);
  }

  .span {
    position: relative;
    z-index: 1;
    min-width: 0;
    margin: 0 3px;
    padding: 3px var(--sp-2);
    overflow: hidden;
    border-radius: 6px;
    background: color-mix(in oklab, var(--app-calendar) 16%, var(--bg-raised));
    box-shadow: inset 3px 0 0 var(--app-calendar);
    color: var(--fg);
    font-size: var(--text-xs);
    font-weight: 550;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .due {
    display: flex;
    justify-content: center;
    min-height: 20px;
  }
  .due-pill {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 20px;
    padding: 0 var(--sp-2);
    border-radius: 999px;
    background: var(--bg-sunken);
    color: var(--fg-muted);
    font-size: var(--text-xs);
    font-weight: 550;
    white-space: nowrap;
  }

  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }

  /* A phone's card is under 300 px inside, and seven days do not fit in it
     with their weather and their counts: the week is the strip it scrolls
     along, a day to a thumb's swipe, with columns wide enough that the
     forecast does not run into the next day's. */
  :global([data-phone]) .grid {
    min-width: calc(var(--n) * 64px);
  }
  :global([data-phone]) .scroller {
    overscroll-behavior-x: contain;
    scrollbar-width: none;
  }
</style>
