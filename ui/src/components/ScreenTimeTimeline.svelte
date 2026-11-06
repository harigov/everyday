<script lang="ts">
  // One day, as it happened: a 24-hour track per device with every run drawn
  // where it fell in its category's colour, and the runs as a list under it.
  //
  // The track answers "when", which a total cannot: a morning of mail and an
  // evening of video come to the same two hours. A track per device rather
  // than one merged, because two machines can be on at once and a merged
  // track would draw one over the other.
  //
  // The blocks are not buttons -- a run of eight seconds is a sliver no
  // pointer should have to hit. Each has a tooltip, and the list below says
  // the same things in words for a keyboard or a screen reader. In the list
  // a run is joined to the next when it is the same app on the same device,
  // so flicking to a terminal and back reads as two lines, not forty.

  import { clock, formatDuration } from '../lib/screentime'
  import { screentime } from '../lib/screentime.svelte'
  import { hourLabel } from '../lib/format'
  import { addDays, offsetInDay, startOfDay, todayIso } from '../lib/time'
  import type { TimelineRun } from '../lib/types'

  const date = $derived(screentime.timelineDate || todayIso())
  const runs = $derived(
    [...(screentime.timeline?.runs ?? [])].sort(
      (a, b) => Date.parse(a.start) - Date.parse(b.start),
    ),
  )
  const loading = $derived(screentime.timeline === null)

  /** Minutes in the day: 1440, or 1380 and 1500 on the days the clocks change. */
  const dayMinutes = $derived(
    (startOfDay(addDays(date, 1)).getTime() - startOfDay(date).getTime()) / 60_000,
  )

  const devices = $derived.by(() => {
    const seen = new Map<string, number>()
    for (const r of runs) seen.set(r.device, (seen.get(r.device) ?? 0) + secsOf(r))
    return [...seen.entries()].map(([device, secs]) => ({ device, secs }))
  })

  function secsOf(r: { start: string; end: string }): number {
    return Math.max(0, (Date.parse(r.end) - Date.parse(r.start)) / 1000)
  }

  function pct(ts: string): number {
    return Math.min(100, Math.max(0, (offsetInDay(ts, date) / dayMinutes) * 100))
  }

  function tip(r: TimelineRun): string {
    const head = `${r.name} · ${clock(r.start)}–${clock(r.end)} · ${formatDuration(secsOf(r))}`
    return r.title ? `${head}\n${r.title}` : head
  }

  const HOURS = Array.from({ length: 25 }, (_, h) => h)

  /** Where now falls, on today's track only. */
  let now = $state(Date.now())
  $effect(() => {
    const timer = setInterval(() => (now = Date.now()), 60_000)
    return () => clearInterval(timer)
  })
  const nowPct = $derived(date === todayIso() ? pct(new Date(now).toISOString()) : null)

  interface Joined {
    id: string
    start: string
    end: string
    key: string
    name: string
    category: TimelineRun['category']
    device: string
    title: string
    secs: number
  }

  /** Consecutive runs of one item on one device, joined. */
  const joined = $derived.by(() => {
    const out: Joined[] = []
    for (const r of runs) {
      const prev = out[out.length - 1]
      if (prev && prev.key === r.key && prev.device === r.device) {
        if (Date.parse(r.end) > Date.parse(prev.end)) prev.end = r.end
        prev.secs += secsOf(r)
        continue
      }
      const row: Joined = {
        id: `${r.device}\u0000${r.start}\u0000${r.key}`,
        start: r.start,
        end: r.end,
        key: r.key,
        name: r.name,
        category: r.category,
        device: r.device,
        title: r.title,
        secs: secsOf(r),
      }
      out.push(row)
    }
    return out
  })
</script>

<div class="timeline scroll">
  <div class="page">
    {#if loading}
      <p class="quiet">Loading…</p>
    {:else if runs.length === 0}
      <p class="quiet">Nothing was recorded on this day.</p>
    {:else}
      <section class="card" aria-label="The day by device">
        {#each devices as d (d.device)}
          <div class="device">
            <div class="device-head">
              <span class="device-name">{d.device}</span>
              <span class="device-secs">{formatDuration(d.secs)}</span>
            </div>
            <div
              class="track"
              role="img"
              aria-label="{d.device}: what was in front of you, hour by hour"
            >
              {#each HOURS as h (h)}
                <span class="hour" style:left="{(h / 24) * 100}%"></span>
              {/each}
              {#each runs.filter((r) => r.device === d.device) as r (r.start + r.key)}
                {@const left = pct(r.start)}
                <span
                  class="run"
                  style:left="{left}%"
                  style:width="max(1px, {pct(r.end) - left}%)"
                  style:background={screentime.colorOf(r.category)}
                  title={tip(r)}
                ></span>
              {/each}
              {#if nowPct !== null}
                <span class="now" style:left="{nowPct}%" title="Now"></span>
              {/if}
            </div>
          </div>
        {/each}
        <div class="scale" aria-hidden="true">
          {#each HOURS.filter((h) => h % 3 === 0 && h < 24) as h (h)}
            <span style:left="{(h / 24) * 100}%">{hourLabel(h)}</span>
          {/each}
        </div>
      </section>

      <section class="card" aria-labelledby="tl-runs">
        <h2 class="eyebrow" id="tl-runs">What was in front of you</h2>
        <ol class="runs">
          {#each joined as r (r.id)}
            <li>
              <span class="when">{clock(r.start)}–{clock(r.end)}</span>
              <span class="dot" style:background={screentime.colorOf(r.category)}></span>
              <span class="what">
                <span class="name" title={r.name}>{r.name}</span>
                {#if r.title}<span class="title" title={r.title}>{r.title}</span>{/if}
              </span>
              {#if devices.length > 1}<span class="dev">{r.device}</span>{/if}
              <span class="secs">{formatDuration(r.secs)}</span>
            </li>
          {/each}
        </ol>
      </section>
    {/if}
  </div>
</div>

<style>
  .timeline {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .page {
    max-width: 960px;
    margin: 0 auto;
    padding: var(--sp-2) var(--sp-6) var(--fab-clear);
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
  }

  .card {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg);
  }

  .quiet {
    margin: 0;
    padding: var(--sp-4) 0;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }

  .device {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
  }
  .device-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--sp-3);
  }
  .device-name {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg);
  }
  .device-secs {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }

  /* The day, left to right, with a real width for every percentage below
     to resolve against. */
  .track {
    position: relative;
    width: 100%;
    height: 28px;
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
  }
  .hour {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 1px;
    background: color-mix(in oklab, var(--border) 70%, transparent);
  }
  .run {
    position: absolute;
    top: 4px;
    bottom: 4px;
    border-radius: 2px;
  }
  .now {
    position: absolute;
    top: -2px;
    bottom: -2px;
    width: 2px;
    margin-left: -1px;
    border-radius: 1px;
    background: var(--fg-muted);
  }

  .scale {
    position: relative;
    height: 1.2em;
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
  .scale span {
    position: absolute;
    top: 0;
    white-space: nowrap;
  }

  .runs {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .runs li {
    display: flex;
    align-items: baseline;
    gap: var(--sp-3);
    min-width: 0;
    padding: 6px 0;
  }
  .runs li + li {
    border-top: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }
  .when {
    flex: none;
    /* Wide enough for "10:45 AM – 11:05 AM" in one line; a 24-hour locale
       just leaves room. */
    min-width: 11.5em;
    white-space: nowrap;
    color: var(--fg-subtle);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
  }
  .dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    align-self: center;
  }
  .what {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
  }
  .name {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg);
    font-size: var(--text-base);
  }
  .title {
    flex: 1 1 0;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }
  .dev {
    flex: none;
    max-width: 10em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }
  .secs {
    flex: none;
    width: 5.5em;
    text-align: right;
    color: var(--fg-muted);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
  }
</style>
