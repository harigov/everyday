<script lang="ts">
  // The day at a glance: the brief's day as a strip of hours, with what is
  // in it laid along them.
  //
  // `timelineOf` does the arithmetic -- the window, the lanes no two items
  // share, the free stretches, where now is -- and is pinned by tests; this
  // only draws it. One row per lane, so two things at once are drawn one
  // above the other instead of one over the other.
  //
  // What each mark is, and why it looks the way it does:
  //
  //   event       its calendar's colour as a wash with a solid left edge,
  //               the title in text ink -- never in the colour, which a
  //               pale calendar would make unreadable
  //   set aside   a dashed outline and no fill: time somebody kept for a
  //               purpose, not somebody else's claim on it
  //   clash       an alert mark on each, and a band in the danger hue where
  //               they overlap, across every lane it touches
  //   free        a measuring line under the lanes, "2h free"
  //   now         a line in the accent, when the day drawn is today
  //
  // Every mark has its title and times in a tooltip and in a list read to
  // assistive technology in place of the picture, and a bar wide enough to
  // hold its title shows it. A legend names the calendars, so colour
  // reinforces what the words say rather than carrying it alone.
  //
  // Compact is the Overview card's: two lanes at most, thinner, and no
  // words under the free stretches.

  import { at, timelineOf, type Timeline, type TimelineItem } from '../lib/brief'
  import { formatMinutes, hourLabel, timeOfDay } from '../lib/format'
  import type { BriefDigest } from '../lib/types'
  import Icon from './Icon.svelte'

  let { digest, compact = false }: { digest: BriefDigest; compact?: boolean } = $props()

  const HOUR = 3_600_000

  // The now line moves with the clock while the page is open.
  let now = $state(Date.now())
  $effect(() => {
    const timer = setInterval(() => (now = Date.now()), 60_000)
    return () => clearInterval(timer)
  })

  const tl = $derived(timelineOf(digest, now))
  const allDay = $derived(digest.events.filter((e) => e.allDay))

  /** The plot's width in pixels, for which labels fit. */
  let width = $state(0)

  const maxLanes = $derived(compact ? 2 : Number.POSITIVE_INFINITY)
  const lanes = $derived(tl ? Math.min(tl.lanes, maxLanes) : 0)
  /** Bars stay at or under 24px, the house's thickest. */
  const bar = $derived(compact ? 16 : 24)
  const gap = $derived(compact ? 3 : 4)
  const lanesHeight = $derived(lanes * bar + Math.max(0, lanes - 1) * gap)

  const hoursShown = $derived(tl ? (tl.to - tl.from) / HOUR : 1)
  const perHour = $derived(width > 0 ? width / hoursShown : 60)
  /** Label every hour when there is room, else every second or third. */
  const step = $derived(
    compact
      ? perHour >= 44
        ? 2
        : perHour >= 26
          ? 3
          : 4
      : perHour >= 52
        ? 1
        : perHour >= 28
          ? 2
          : 3,
  )

  const ticks = $derived(
    tl
      ? tl.hours.map((t) => {
          const hour = new Date(t).getHours()
          return { t, hour, labelled: hour % step === 0 }
        })
      : [],
  )

  /** Width in pixels of a span that is `pct` of the plot. */
  const px = (pct: number) => (pct / 100) * width

  /** The lane an item is drawn in: the overflow of a compact strip shares the last one. */
  const laneOf = (item: TimelineItem) => Math.min(item.lane, lanes - 1)

  function span(item: { start: number; end: number }): string {
    return `${timeOfDay(new Date(item.start))} – ${timeOfDay(new Date(item.end))}`
  }

  function tip(item: TimelineItem): string {
    return [
      item.title || 'Untitled',
      span(item),
      item.kind === 'block' ? 'time set aside' : '',
      item.location,
      item.video ? 'video call' : '',
      item.clash ? 'clashes with something else' : '',
    ]
      .filter(Boolean)
      .join(' · ')
  }

  /**
   * Where clashing items overlap, merged: drawn behind the lanes in the
   * danger hue so the two halves of a clash read as one problem.
   */
  function clashZones(t: Timeline): { start: number; end: number }[] {
    const clashing = t.items.filter((i) => i.clash)
    const zones: { start: number; end: number }[] = []
    for (let a = 0; a < clashing.length; a++) {
      for (let b = a + 1; b < clashing.length; b++) {
        const x = clashing[a]!
        const y = clashing[b]!
        const start = Math.max(x.start, y.start)
        const end = Math.min(x.end, y.end)
        if (end > start) zones.push({ start, end })
      }
    }
    zones.sort((p, q) => p.start - q.start)
    const merged: { start: number; end: number }[] = []
    for (const z of zones) {
      const last = merged[merged.length - 1]
      if (last && z.start <= last.end) last.end = Math.max(last.end, z.end)
      else merged.push({ ...z })
    }
    return merged
  }

  const zones = $derived(tl ? clashZones(tl) : [])

  /** The calendars on the strip, for the legend: each once, in order of appearance. */
  const calendars = $derived.by(() => {
    const seen: Record<string, string> = {}
    for (const e of digest.events) {
      if (e.allDay) continue
      const name = e.calendar || 'Calendar'
      if (!(name in seen)) seen[name] = e.color
    }
    return Object.entries(seen).map(([name, color]) => ({ name, color }))
  })
  const hasBlocks = $derived(digest.blocks.length > 0)

  const summary = $derived.by(() => {
    if (!tl) return allDay.length > 0 ? `${allDay.length} all-day` : 'Nothing timed'
    const events = tl.items.filter((i) => i.kind === 'event').length
    const free = tl.free.reduce((sum, f) => sum + f.minutes, 0)
    return [
      `${events} ${events === 1 ? 'event' : 'events'}`,
      free > 0 ? `${formatMinutes(free)} free` : '',
      zones.length > 0 ? `${zones.length === 1 ? 'a clash' : `${zones.length} clashes`}` : '',
    ]
      .filter(Boolean)
      .join(', ')
  })
</script>

<figure class="tl" class:compact aria-label="The day at a glance: {summary}">
  {#if allDay.length > 0}
    <ul class="allday" aria-label="All day">
      {#each allDay as e, i (i)}
        <li class="chip" style="--c: {e.color || 'var(--fg-subtle)'}" title="{e.title} · all day">
          <span class="swatch" aria-hidden="true"></span>
          <span class="chip-text">{e.title}</span>
          {#if !compact}<span class="chip-when">all day</span>{/if}
        </li>
      {/each}
    </ul>
  {/if}

  {#if tl}
    <div
      class="plot"
      bind:clientWidth={width}
      style="--lanes-h: {lanesHeight}px; --bar: {bar}px"
      aria-hidden="true"
    >
      <div class="lanes">
        {#each ticks as tick (tick.t)}
          <span class="grid" class:major={tick.labelled} style="left: {at(tl, tick.t)}%"></span>
        {/each}

        {#each zones as z (z.start)}
          <span
            class="zone"
            style="left: {at(tl, z.start)}%; width: {at(tl, z.end) - at(tl, z.start)}%"
          ></span>
        {/each}

        {#each tl.items as item, i (i)}
          {@const left = at(tl, item.start)}
          {@const w = at(tl, item.end) - left}
          {@const room = px(w)}
          <div
            class="item {item.kind}"
            class:clash={item.clash}
            class:tentative={!item.busy}
            style="left: {left}%; width: {w}%; top: {laneOf(item) *
              (bar + gap)}px; --c: {item.color || 'var(--fg-subtle)'}"
            title={tip(item)}
          >
            {#if item.clash && room >= 22}
              <span class="warn"><Icon name="alert" size={compact ? 10 : 12} weight={2} /></span>
            {/if}
            {#if room >= (compact ? 56 : 46) + (item.clash ? 16 : 0)}
              <span class="name">{item.title}</span>
              {#if !compact && room >= 150}
                <span class="time">{timeOfDay(new Date(item.start))}</span>
              {/if}
            {/if}
          </div>
        {/each}

        {#if tl.now !== null}
          <span class="now" style="left: {at(tl, tl.now)}%" title="Now"></span>
        {/if}
      </div>

      {#if tl.free.length > 0}
        <div class="free-row">
          {#each tl.free as f (f.start)}
            {@const left = at(tl, f.start)}
            {@const w = at(tl, f.end) - left}
            <div
              class="free"
              style="left: {left}%; width: {w}%"
              title="{formatMinutes(f.minutes)} free · {span(f)}"
            >
              {#if !compact && px(w) >= 64}
                <span class="free-text">{formatMinutes(f.minutes)} free</span>
              {/if}
            </div>
          {/each}
        </div>
      {/if}

      <div class="axis">
        {#each ticks as tick (tick.t)}
          {#if tick.labelled}
            <span class="tick" style="left: {at(tl, tick.t)}%">{hourLabel(tick.hour)}</span>
          {/if}
        {/each}
      </div>
    </div>

    {#if !compact && (calendars.length > 0 || hasBlocks || zones.length > 0)}
      <ul class="legend" aria-hidden="true">
        {#each calendars as c (c.name)}
          <li><span class="key event-key" style="--c: {c.color}"></span>{c.name}</li>
        {/each}
        {#if hasBlocks}<li><span class="key block-key"></span>Set aside</li>{/if}
        {#if zones.length > 0}<li><span class="key zone-key"></span>Clash</li>{/if}
        {#if tl.free.length > 0}<li><span class="key free-key"></span>Free</li>{/if}
      </ul>
    {/if}

    <!-- The picture, read out: every mark in order, which is what a screen
         reader gets instead of the strip. -->
    <ul class="sr-only">
      {#each tl.items as item, i (i)}
        <li>{tip(item)}</li>
      {/each}
      {#each tl.free as f (f.start)}
        <li>{formatMinutes(f.minutes)} free, {span(f)}</li>
      {/each}
    </ul>
  {/if}
</figure>

<style>
  .tl {
    position: relative;
    display: block;
    width: 100%;
    min-width: 0;
    margin: 0;
    padding: var(--sp-4) var(--sp-5) var(--sp-3);
    background: var(--bg-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--edge);
  }
  /* In a card already: no frame of its own. */
  .tl.compact {
    padding: 0;
    background: none;
    border: none;
    border-radius: 0;
    box-shadow: none;
  }

  /* ── All day ──────────────────────────────────────────────────────── */

  .allday {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-1) var(--sp-2);
    margin: 0 0 var(--sp-3);
    padding: 0;
    list-style: none;
  }
  .compact .allday {
    margin-bottom: var(--sp-2);
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    max-width: 100%;
    height: 24px;
    padding: 0 var(--sp-2) 0 var(--sp-2);
    border-radius: 6px;
    background: color-mix(in oklab, var(--c) 14%, var(--bg-raised));
    color: var(--fg);
    font-size: var(--text-xs);
    font-weight: 550;
  }
  .compact .chip {
    height: 20px;
  }
  .swatch {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 2px;
    background: var(--c);
  }
  .chip-text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chip-when {
    color: var(--fg-subtle);
    font-weight: 450;
  }

  /* ── The plot ─────────────────────────────────────────────────────── */

  /* A block with a real width -- every mark inside is a percentage of it.
     The first and last hour labels sit centred on their ticks and so hang
     half outside it, into the frame's padding (or the card's, in compact). */
  .plot {
    position: relative;
    display: block;
    width: 100%;
  }

  .lanes {
    position: relative;
    height: var(--lanes-h);
  }

  /* Hairlines, solid, one step off the surface: the hours. */
  .grid {
    position: absolute;
    top: -4px;
    bottom: -4px;
    width: 1px;
    translate: -0.5px 0;
    background: color-mix(in oklab, var(--border) 55%, transparent);
  }
  .grid.major {
    background: var(--border);
  }

  .zone {
    position: absolute;
    top: -6px;
    bottom: -6px;
    border-radius: 3px;
    background: color-mix(in oklab, var(--danger) 13%, transparent);
  }

  .item {
    position: absolute;
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 3px;
    height: var(--bar);
    padding: 0 6px 0 8px;
    overflow: hidden;
    border-radius: 4px;
    font-size: var(--text-xs);
    line-height: 1;
    white-space: nowrap;
  }

  /* The calendar's colour as a wash, and solid down the leading edge: enough
     to find a calendar at a glance, not enough to colour what is written on
     it. Mixed over the raised card rather than over nothing, so a glass
     theme's backdrop does not show through and muddy the hue. */
  .item.event {
    background: color-mix(in oklab, var(--c) 20%, var(--bg-raised));
    box-shadow: inset 3px 0 0 var(--c);
    color: var(--fg);
  }
  /* Free-to-attend, or marked as not busy: lighter. */
  .item.event.tentative {
    background: color-mix(in oklab, var(--c) 9%, var(--bg-raised));
  }

  .item.block {
    background: color-mix(in oklab, var(--bg-raised) 70%, transparent);
    border: 1.5px dashed var(--border-strong);
    color: var(--fg-muted);
  }

  /* A clash: a ring in the danger hue outside the bar and an alert mark in
     it -- a shape and a word in the tooltip as well as a colour. */
  .item.clash {
    outline: 1.5px solid var(--danger);
    outline-offset: 1px;
  }
  .warn {
    display: flex;
    flex: none;
    color: var(--danger);
  }

  .compact .item {
    padding: 0 5px 0 7px;
    font-size: 11px;
  }

  .name {
    min-width: 0;
    overflow: hidden;
    font-weight: 600;
    text-overflow: ellipsis;
  }
  .time {
    flex: none;
    margin-left: auto;
    padding-left: 4px;
    color: var(--fg-muted);
    font-variant-numeric: tabular-nums;
  }

  .now {
    position: absolute;
    top: -8px;
    bottom: -6px;
    z-index: 2;
    width: 2px;
    translate: -1px 0;
    border-radius: 1px;
    background: var(--accent);
    pointer-events: auto;
  }
  .now::before {
    content: '';
    position: absolute;
    top: -3px;
    left: -3px;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--accent);
    box-shadow: 0 0 0 2px var(--bg-raised);
  }

  /* ── Free stretches ───────────────────────────────────────────────── */

  .free-row {
    position: relative;
    height: 22px;
    margin-top: 10px;
  }
  .compact .free-row {
    height: 8px;
    margin-top: 6px;
  }

  /* A measuring line: end stops and a rule between them, the length written
     on it. No fill -- free time is the absence of anything, drawn as a
     span rather than as a block of its own. */
  .free {
    position: absolute;
    top: 0;
    display: flex;
    align-items: center;
    gap: 6px;
    height: 100%;
    padding: 0 1px;
    border-inline: 1.5px solid var(--border-strong);
  }
  .free::before,
  .free::after {
    content: '';
    flex: 1;
    height: 1px;
    background: var(--border-strong);
  }
  .compact .free {
    border-inline-width: 1px;
  }
  .free-text {
    flex: none;
    color: var(--fg-subtle);
    font-size: var(--text-xs);
    font-weight: 550;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  /* ── The hours ────────────────────────────────────────────────────── */

  .axis {
    position: relative;
    height: 18px;
    margin-top: 6px;
  }
  .compact .axis {
    height: 14px;
    margin-top: 4px;
  }
  .tick {
    position: absolute;
    top: 0;
    translate: -50% 0;
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    line-height: 1.4;
    white-space: nowrap;
  }
  .compact .tick {
    font-size: 11px;
  }

  /* ── Legend ───────────────────────────────────────────────────────── */

  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-1) var(--sp-4);
    margin: var(--sp-3) 0 0;
    padding: var(--sp-2) 0 0;
    border-top: 1px solid var(--border);
    list-style: none;
    color: var(--fg-subtle);
    font-size: var(--text-xs);
  }
  .legend li {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .key {
    display: inline-block;
    width: 14px;
    height: 10px;
    border-radius: 2px;
  }
  .event-key {
    background: color-mix(in oklab, var(--c) 20%, var(--bg-raised));
    box-shadow: inset 3px 0 0 var(--c);
  }
  .block-key {
    border: 1.5px dashed var(--border-strong);
  }
  .zone-key {
    background: color-mix(in oklab, var(--danger) 13%, transparent);
    outline: 1.5px solid var(--danger);
    outline-offset: -1.5px;
  }
  .free-key {
    height: 8px;
    border-inline: 1.5px solid var(--border-strong);
    border-radius: 0;
    background: linear-gradient(var(--border-strong), var(--border-strong)) center / 100% 1px
      no-repeat;
  }
</style>
