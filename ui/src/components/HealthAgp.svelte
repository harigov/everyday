<script lang="ts">
  // The ambulatory glucose profile: every day in the report folded onto one
  // 24-hour clock, so the pattern of a typical day shows through the noise
  // of any one of them.
  //
  // Drawn as the standard report draws it: the median as a line, the middle
  // half of readings (25th to 75th percentile) as a darker band and the
  // wider spread (5th to 95th) as a paler one, with the target range shaded
  // behind both. One series and its spread, so there is no legend box of
  // colours to match -- a keyed line under the chart says what each shape
  // is, and the table underneath has every figure for anyone who cannot or
  // would rather not read it off a curve.
  //
  // Values arrive in mg/dL, the canonical unit, and are converted through
  // the store for the axis and the readout; the geometry is always done in
  // mg/dL so a change of unit never moves the curve.

  import { health } from '../lib/health.svelte'
  import { hourLabel } from '../lib/format'
  import { phone } from '../lib/phone.svelte'
  import type { AgpSlot, GlucoseReport } from '../lib/types'

  interface Props {
    report: GlucoseReport | null | undefined
    /** The plot's height in pixels, axis included. */
    height?: number
  }

  const { report, height = 230 }: Props = $props()

  const LEFT = 40
  const RIGHT = 10
  const TOP = 10
  const BOTTOM = 24

  /** The chart's measured width. Pixels rather than a 0-100 box, so text and
   *  circles are not stretched with it. */
  let width = $state(640)

  const slots = $derived([...(report?.agp ?? [])].sort((a, b) => a.minute - b.minute))
  const lowTarget = $derived(health.settings?.glucoseLow ?? 70)
  const highTarget = $derived(health.settings?.glucoseHigh ?? 180)

  const yMin = 40
  const yMax = $derived(Math.max(250, Math.ceil(Math.max(0, ...slots.map((s) => s.p95)) / 50) * 50))
  const plotW = $derived(Math.max(10, width - LEFT - RIGHT))
  const plotH = $derived(Math.max(10, height - TOP - BOTTOM))

  function x(minute: number): number {
    return LEFT + (Math.min(1440, Math.max(0, minute)) / 1440) * plotW
  }
  function y(value: number): number {
    const clamped = Math.min(yMax, Math.max(yMin, value))
    return TOP + (1 - (clamped - yMin) / (yMax - yMin)) * plotH
  }

  /** The area between two percentile lines, as one closed path. */
  function band(lo: 'p5' | 'p25', hi: 'p75' | 'p95'): string {
    if (slots.length < 2) return ''
    const up = slots.map((s) => `${x(s.minute).toFixed(1)},${y(s[hi]).toFixed(1)}`)
    const down = [...slots].reverse().map((s) => `${x(s.minute).toFixed(1)},${y(s[lo]).toFixed(1)}`)
    return `M${up.join('L')}L${down.join('L')}Z`
  }
  const wide = $derived(band('p5', 'p95'))
  const middle = $derived(band('p25', 'p75'))
  const median = $derived(
    slots.length < 2
      ? ''
      : `M${slots.map((s) => `${x(s.minute).toFixed(1)},${y(s.p50).toFixed(1)}`).join('L')}`,
  )

  /** A glucose value as the person reads it: whole mg/dL, one decimal of mmol/L. */
  function show(value: number): string {
    const shown = health.toDisplay('blood_glucose', value)
    return health.displayUnit('blood_glucose') === 'mmol/L'
      ? shown.toFixed(1)
      : String(Math.round(shown))
  }
  const unit = $derived(health.displayUnit('blood_glucose'))

  const ticks = $derived.by(() => {
    const out = [lowTarget, highTarget]
    if (yMax - highTarget > 40) out.push(250)
    return [...new Set(out)].filter((v) => v > yMin && v < yMax)
  })
  const hours = [0, 6, 12, 18, 24]

  /** "3:00 AM", from minutes after midnight. */
  function clock(minute: number): string {
    const h = Math.floor(minute / 60) % 24
    const m = Math.round(minute % 60)
    const at = new Date()
    at.setHours(h, m, 0, 0)
    return at.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' })
  }

  let hoverX = $state<number | null>(null)
  const hovered = $derived.by((): AgpSlot | null => {
    if (hoverX === null || slots.length === 0) return null
    const minute = ((hoverX - LEFT) / plotW) * 1440
    let best: AgpSlot | null = null
    for (const s of slots) {
      if (!best || Math.abs(s.minute - minute) < Math.abs(best.minute - minute)) best = s
    }
    return best
  })

  function track(e: PointerEvent) {
    const box = (e.currentTarget as HTMLElement).getBoundingClientRect()
    const px = e.clientX - box.left
    hoverX = px >= LEFT && px <= box.width - RIGHT ? px : null
  }

  /** Every third hour, for the table twin: the slot nearest each. */
  const table = $derived(
    [0, 180, 360, 540, 720, 900, 1080, 1260].flatMap((minute) => {
      let best: AgpSlot | null = null
      for (const s of slots) {
        if (!best || Math.abs(s.minute - minute) < Math.abs(best.minute - minute)) best = s
      }
      return best ? [{ minute, slot: best }] : []
    }),
  )
</script>

{#if slots.length < 2}
  <p class="none">Not enough readings for a daily profile yet.</p>
{:else}
  <div class="agp">
    <p class="readout" aria-live="off">
      {#if hovered}
        <b>{clock(hovered.minute)}</b>
        Typical {show(hovered.p50)}
        {unit}. Middle half {show(hovered.p25)}–{show(hovered.p75)}, most days {show(
          hovered.p5,
        )}–{show(hovered.p95)}.
      {:else}
        {phone.on
          ? 'Touch the chart to read a time of day.'
          : 'Move across the chart to read a time of day.'}
      {/if}
    </p>

    <div
      class="wrap"
      role="img"
      aria-label="Ambulatory glucose profile: a typical day, midnight to midnight. The table below has the figures."
      bind:clientWidth={width}
      onpointerdown={track}
      onpointermove={track}
      onpointerleave={(e) => {
        // A finger lifting is not a pointer leaving: the time it touched
        // stays read out.
        if (e.pointerType !== 'touch') hoverX = null
      }}
    >
      <svg {width} {height} viewBox="0 0 {width} {height}" aria-hidden="true">
        <!-- The target range, behind everything -->
        <rect
          class="target"
          x={LEFT}
          y={y(highTarget)}
          width={plotW}
          height={Math.max(0, y(lowTarget) - y(highTarget))}
        />
        {#each ticks as t (t)}
          <line class="rule" x1={LEFT} x2={LEFT + plotW} y1={y(t)} y2={y(t)} />
          <text class="tick" x={LEFT - 6} y={y(t) + 4} text-anchor="end">{show(t)}</text>
        {/each}
        <line class="axis" x1={LEFT} x2={LEFT + plotW} y1={TOP + plotH} y2={TOP + plotH} />

        <path class="wide" d={wide} />
        <path class="middle" d={middle} />
        <path class="median" d={median} />

        {#each hours as h (h)}
          <text
            class="tick"
            x={x(h * 60)}
            y={height - 6}
            text-anchor={h === 0 ? 'start' : h === 24 ? 'end' : 'middle'}>{hourLabel(h % 24)}</text
          >
        {/each}

        {#if hovered}
          <line
            class="cursor"
            x1={x(hovered.minute)}
            x2={x(hovered.minute)}
            y1={TOP}
            y2={TOP + plotH}
          />
          <circle class="dot" cx={x(hovered.minute)} cy={y(hovered.p50)} r="4" />
        {/if}
      </svg>
    </div>

    <ul class="key">
      <li><span class="sw line"></span>Typical (median)</li>
      <li><span class="sw mid"></span>Middle half of readings</li>
      <li><span class="sw outer"></span>Most days (5th to 95th)</li>
      <li>
        <span class="sw goal"></span>Target {show(lowTarget)}–{show(highTarget)}
        {unit}
      </li>
    </ul>

    <details class="twin">
      <summary>As a table</summary>
      <table>
        <thead>
          <tr>
            <th scope="col">Time</th>
            <th scope="col" class="num">Typical</th>
            <th scope="col" class="num">Middle half</th>
            <th scope="col" class="num">Most days</th>
          </tr>
        </thead>
        <tbody>
          {#each table as row (row.minute)}
            <tr>
              <th scope="row">{clock(row.minute)}</th>
              <td class="num">{show(row.slot.p50)}</td>
              <td class="num">{show(row.slot.p25)}–{show(row.slot.p75)}</td>
              <td class="num">{show(row.slot.p5)}–{show(row.slot.p95)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
      <p class="unit">In {unit}.</p>
    </details>
  </div>
{/if}

<style>
  .agp {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    min-width: 0;
  }

  .readout {
    margin: 0;
    min-height: 1.45em;
    font-size: var(--text-sm);
    color: var(--fg-muted);
    font-variant-numeric: tabular-nums;
  }
  .readout b {
    color: var(--fg);
    font-weight: 600;
    margin-right: var(--sp-1);
  }

  /* A real width for the svg to measure and fill. */
  .wrap {
    width: 100%;
    min-width: 0;
    touch-action: pan-y;
  }
  svg {
    display: block;
    max-width: 100%;
  }

  /* The plot's own colour is the app's; text never wears it. */
  .target {
    fill: var(--fg);
    fill-opacity: 0.05;
  }
  .rule {
    stroke: var(--border);
    stroke-width: 1;
  }
  .axis {
    stroke: var(--border-strong);
    stroke-width: 1;
  }
  .tick {
    font-size: 11px;
    fill: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .wide {
    fill: var(--app-health, var(--accent));
    fill-opacity: 0.13;
  }
  .middle {
    fill: var(--app-health, var(--accent));
    fill-opacity: 0.3;
  }
  .median {
    fill: none;
    stroke: var(--app-health, var(--accent));
    stroke-width: 2;
    stroke-linejoin: round;
    stroke-linecap: round;
  }
  .cursor {
    stroke: var(--border-strong);
    stroke-width: 1;
  }
  .dot {
    fill: var(--app-health, var(--accent));
    stroke: var(--bg);
    stroke-width: 2;
  }

  .key {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-1) var(--sp-4);
    margin: 0;
    padding: 0;
    list-style: none;
    font-size: var(--text-xs);
    color: var(--fg-muted);
  }
  .key li {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .sw {
    flex: none;
    display: inline-block;
    width: 14px;
    height: 8px;
    border-radius: 2px;
  }
  .sw.line {
    height: 2px;
    background: var(--app-health, var(--accent));
  }
  .sw.mid {
    background: color-mix(in oklab, var(--app-health, var(--accent)) 30%, transparent);
  }
  .sw.outer {
    background: color-mix(in oklab, var(--app-health, var(--accent)) 13%, transparent);
  }
  .sw.goal {
    background: color-mix(in oklab, var(--fg) 8%, transparent);
    border: 1px solid var(--border);
  }

  .none {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }

  .twin summary {
    cursor: pointer;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .twin table {
    margin-top: var(--sp-2);
    border-collapse: collapse;
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
  }
  .twin th,
  .twin td {
    padding: 3px var(--sp-4) 3px 0;
    text-align: left;
    font-weight: 400;
    color: var(--fg-muted);
    border-bottom: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }
  .twin thead th {
    color: var(--fg-subtle);
    font-size: var(--text-xs);
  }
  .twin .num {
    text-align: right;
  }
  .unit {
    margin: var(--sp-1) 0 0;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }

  /* A phone: the table fits the card (its columns share it), and the
     summary that opens it is a finger tall. */
  :global([data-phone]) .twin summary {
    display: flex;
    align-items: center;
    min-height: 44px;
  }
  :global([data-phone]) .twin table {
    width: 100%;
  }
  :global([data-phone]) .twin th,
  :global([data-phone]) .twin td {
    padding-right: var(--sp-2);
  }
</style>
