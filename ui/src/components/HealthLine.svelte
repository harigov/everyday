<script lang="ts">
  // A line over time, with an optional shaded target band, a faint min-to-max
  // band behind it, and markers for events -- the shared chart of every Health
  // pane that follows a reading: heart rate, weight, glucose, a lab result, a
  // workout's pulse.
  //
  // The x axis is time, not the index of a point, so a week with no readings
  // is a gap on the chart and not a kink in it. The line is broken where the
  // gap between two readings is several times the usual one, for the same
  // reason: joining the two would claim a value on days nobody measured.
  // `dots` draws the points alone (lab results, which are a handful of
  // dated tests and not a signal), and a short series always shows its dots,
  // because three weights are three readings, not a curve.
  //
  // Values are drawn exactly as given -- the caller converts to the person's
  // units first -- and `unit` is only the label.
  //
  // Like HealthBars: SVG at the pixel size of its box, one colour (the
  // subject's), text in text tokens, hairline axes, a tooltip for everything
  // the axes do not say. The chart is one tab stop; left and right walk the
  // points.

  import { dateFormat } from '../lib/format'
  import { addDays, locale, startOfDay } from '../lib/time'

  interface Props {
    series: { at: string; value: number; min?: number | null; max?: number | null }[]
    /** A shaded band (a target or reference range), in the series' units. */
    band?: { low: number; high: number } | null
    unit?: string
    color?: string
    height?: number
    /** Points in time to mark: a meal, a dose, a dose change. */
    markers?: { at: string; label: string; color?: string }[]
    yMin?: number
    yMax?: number
    /** The x extent; the series' own when absent. */
    from?: string
    to?: string
    /** Draw dots at each point (lab results), not a continuous line. */
    dots?: boolean
  }

  const {
    series,
    band = null,
    unit = '',
    color = 'var(--accent)',
    height = 160,
    markers = [],
    yMin,
    yMax,
    from,
    to,
    dots = false,
  }: Props = $props()

  type Row = Props['series'][number] & { t: number }

  const PAD_TOP = 18
  const AXIS = 22

  let width = $state(600)
  let active = $state<number | null>(null)

  const W = $derived(Math.max(160, width))

  // ── time ───────────────────────────────────────────────────────────────

  /** `YYYY-MM-DD` is the start of a local day; anything longer is an instant. */
  function ms(at: string): number {
    return at.length <= 10 ? startOfDay(at).getTime() : Date.parse(at)
  }

  const rows = $derived(
    series
      .map((p): Row => ({ ...p, t: ms(p.at) }))
      .filter((p) => Number.isFinite(p.t) && Number.isFinite(p.value))
      .sort((a, b) => a.t - b.t),
  )

  /** The window drawn. A date as `to` means the whole of that day. */
  const extent = $derived.by(() => {
    const first = rows[0]?.t ?? 0
    const last = rows[rows.length - 1]?.t ?? first
    let a = from ? ms(from) : first
    let b = to ? (to.length <= 10 ? startOfDay(addDays(to, 1)).getTime() : ms(to)) : last
    if (!Number.isFinite(a)) a = first
    if (!Number.isFinite(b)) b = last
    if (b <= a) {
      a -= 12 * 3_600_000
      b += 12 * 3_600_000
    }
    return { a, b }
  })
  const span = $derived(extent.b - extent.a)

  const vis = $derived(rows.filter((p) => p.t >= extent.a && p.t <= extent.b))

  // ── the value axis ─────────────────────────────────────────────────────

  const yr = $derived.by(() => {
    let lo = Infinity
    let hi = -Infinity
    for (const p of vis) {
      lo = Math.min(lo, p.min ?? p.value, p.value)
      hi = Math.max(hi, p.max ?? p.value, p.value)
    }
    if (band) {
      lo = Math.min(lo, band.low)
      hi = Math.max(hi, band.high)
    }
    if (!Number.isFinite(lo) || !Number.isFinite(hi)) {
      lo = 0
      hi = 1
    }
    const spread = hi - lo || Math.abs(hi) * 0.1 || 1
    let a = yMin ?? lo - spread * 0.15
    const b0 = yMax ?? hi + spread * 0.15
    // A quantity that is never negative does not get an axis that is.
    if (yMin === undefined && lo >= 0 && a < 0) a = 0
    return { a, b: b0 <= a ? a + 1 : b0 }
  })

  function niceStep(range: number, count: number): number {
    if (!(range > 0)) return 1
    const raw = range / count
    const mag = 10 ** Math.floor(Math.log10(raw))
    const norm = raw / mag
    const step = norm <= 1 ? 1 : norm <= 2 ? 2 : norm <= 2.5 ? 2.5 : norm <= 5 ? 5 : 10
    return step * mag
  }

  const ticks = $derived.by(() => {
    const step = niceStep(yr.b - yr.a, 3)
    const out: number[] = []
    for (let k = Math.ceil(yr.a / step - 1e-9); k * step <= yr.b + 1e-9; k++) out.push(k * step)
    return out
  })

  const formats = new Map<string, Intl.NumberFormat>()
  function numberFormat(options: Intl.NumberFormatOptions): Intl.NumberFormat {
    const key = `${locale()}\u0000${JSON.stringify(options)}`
    let made = formats.get(key)
    if (!made) {
      made = new Intl.NumberFormat(locale(), options)
      formats.set(key, made)
    }
    return made
  }
  function fmt(v: number): string {
    // A tick a hair below zero rounds to "-0"; zero is zero.
    if (Math.abs(v) < 1e-9) v = 0
    const a = Math.abs(v)
    return numberFormat({ maximumFractionDigits: a >= 100 ? 0 : a >= 10 ? 1 : 2 }).format(v)
  }
  function tick(v: number): string {
    return Math.abs(v) >= 10_000
      ? numberFormat({ notation: 'compact', maximumFractionDigits: 1 }).format(v)
      : fmt(v)
  }

  // ── geometry ───────────────────────────────────────────────────────────

  const gutter = $derived(Math.max(1, ...ticks.map((t) => tick(t).length)) * 7 + 10)
  const plotW = $derived(Math.max(40, W - gutter - 6))
  const svgH = $derived(PAD_TOP + height + AXIS)

  const xOf = (t: number) => gutter + ((t - extent.a) / span) * plotW
  const yOf = (v: number) => PAD_TOP + height * (1 - (v - yr.a) / (yr.b - yr.a))
  const pt = (p: Row, v: number = p.value) => `${xOf(p.t).toFixed(1)},${yOf(v).toFixed(1)}`

  /** Runs of points with no long gap inside them. */
  const runs = $derived.by(() => {
    const out: Row[][] = []
    const first = vis[0]
    if (!first) return out
    const gaps = vis
      .slice(1)
      .map((p, i) => p.t - (vis[i]?.t ?? p.t))
      .sort((x, y) => x - y)
    const median = gaps[Math.floor(gaps.length / 2)] ?? 0
    const limit = vis.length >= 5 && median > 0 ? median * 4 : Infinity
    let run: Row[] = [first]
    for (let i = 1; i < vis.length; i++) {
      const p = vis[i]
      const before = vis[i - 1]
      if (!p || !before) continue
      if (p.t - before.t > limit) {
        out.push(run)
        run = [p]
      } else run.push(p)
    }
    out.push(run)
    return out
  })

  const line = $derived(
    dots
      ? ''
      : runs
          .filter((r) => r.length > 1)
          .map((r) => `M${r.map((p) => pt(p)).join('L')}`)
          .join(''),
  )

  /** The min-to-max band behind a series that carries one. */
  const area = $derived(
    dots
      ? ''
      : runs
          .filter((r) => r.length > 1 && r.some((p) => p.min != null || p.max != null))
          .map((r) => {
            const up = r.map((p) => pt(p, p.max ?? p.value)).join('L')
            const down = [...r]
              .reverse()
              .map((p) => pt(p, p.min ?? p.value))
              .join('L')
            return `M${up}L${down}Z`
          })
          .join(''),
  )

  /** Few points, or a run of one, are drawn as dots: a line needs two. */
  const dotRows = $derived.by(() => {
    if (dots || vis.length <= 60) return vis
    return runs.filter((r) => r.length === 1).flatMap((r) => r)
  })

  // ── markers ────────────────────────────────────────────────────────────

  const marks = $derived(
    markers
      .map((m) => ({ ...m, t: ms(m.at) }))
      .filter((m) => Number.isFinite(m.t) && m.t >= extent.a && m.t <= extent.b),
  )

  // ── the date axis ──────────────────────────────────────────────────────

  const spanDays = $derived(span / 86_400_000)

  function axisText(t: number): string {
    const d = new Date(t)
    if (spanDays <= 2) return dateFormat({ hour: 'numeric', minute: '2-digit' }).format(d)
    if (spanDays > 200) return dateFormat({ month: 'short', year: '2-digit' }).format(d)
    return dateFormat({ day: 'numeric', month: 'short' }).format(d)
  }

  const axisLabels = $derived.by(() => {
    const count = Math.min(6, Math.max(2, Math.floor(plotW / 110)))
    const out: { x: number; text: string; anchor: 'start' | 'middle' | 'end'; key: number }[] = []
    for (let i = 0; i < count; i++) {
      const last = i === count - 1
      const t = last ? extent.b - 60_000 : extent.a + (span * i) / (count - 1)
      out.push({
        key: i,
        x: i === 0 ? gutter : last ? gutter + plotW : xOf(t),
        text: axisText(t),
        anchor: i === 0 ? 'start' : last ? 'end' : 'middle',
      })
    }
    return out
  })

  // ── the tooltip ────────────────────────────────────────────────────────

  function whenText(t: number): string {
    const d = new Date(t)
    if (spanDays <= 2) {
      return dateFormat({
        weekday: 'short',
        day: 'numeric',
        month: 'short',
        hour: 'numeric',
        minute: '2-digit',
      }).format(d)
    }
    return dateFormat({
      weekday: 'short',
      day: 'numeric',
      month: 'short',
      ...(spanDays > 300 ? { year: 'numeric' as const } : {}),
    }).format(d)
  }

  const activeRow = $derived(active === null ? null : (vis[active] ?? null))
  const activeText = $derived.by(() => {
    const p = activeRow
    if (!p) return ''
    const range =
      p.min != null && p.max != null && (p.min !== p.value || p.max !== p.value)
        ? ` (${fmt(p.min)}–${fmt(p.max)})`
        : ''
    return `${whenText(p.t)} · ${fmt(p.value)}${unit ? ` ${unit}` : ''}${range}`
  })
  const tipX = $derived(activeRow ? Math.min(Math.max(xOf(activeRow.t), 90), W - 90) : 0)

  const ariaLabel = $derived(
    `${unit ? `${unit}, ` : ''}${vis.length} readings, arrow keys move between them`,
  )

  function nearest(e: MouseEvent): number | null {
    if (vis.length === 0) return null
    const box = (e.currentTarget as HTMLElement).getBoundingClientRect()
    const x = e.clientX - box.left
    if (x < gutter - 8 || x > gutter + plotW + 8) return null
    let best = 0
    let bestGap = Infinity
    for (let i = 0; i < vis.length; i++) {
      const p = vis[i]
      if (!p) continue
      const gap = Math.abs(xOf(p.t) - x)
      if (gap < bestGap) {
        best = i
        bestGap = gap
      }
    }
    return best
  }

  function key(e: KeyboardEvent) {
    const n = vis.length
    if (n === 0) return
    const at = active ?? n - 1
    if (e.key === 'ArrowRight') active = Math.min(n - 1, at + 1)
    else if (e.key === 'ArrowLeft') active = Math.max(0, at - 1)
    else if (e.key === 'Home') active = 0
    else if (e.key === 'End') active = n - 1
    else return
    e.preventDefault()
  }

  function focused(e: FocusEvent) {
    if (active === null && (e.currentTarget as HTMLElement).matches(':focus-visible')) {
      active = vis.length - 1
    }
  }
</script>

{#if vis.length === 0}
  <p class="none">Nothing recorded in this window.</p>
{:else}
  <div class="chart" style="--c: {color}" bind:clientWidth={width}>
    <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
    <div
      class="stage"
      role="group"
      tabindex="0"
      aria-label={ariaLabel}
      onpointerdown={(e) => (active = nearest(e))}
      onpointermove={(e) => (active = nearest(e))}
      onpointerleave={(e) => {
        // A finger lifting is not a pointer leaving: the reading it touched
        // stays on show until it touches elsewhere.
        if (e.pointerType !== 'touch') active = null
      }}
      onkeydown={key}
      onfocus={focused}
      onblur={() => (active = null)}
    >
      <svg viewBox="0 0 {W} {svgH}" width="100%" height={svgH} aria-hidden="true" focusable="false">
        {#if unit}
          <text x="0" y="10" class="t unit">{unit}</text>
        {/if}

        {#each ticks as t (t)}
          <line
            x1={gutter}
            x2={gutter + plotW}
            y1={yOf(t)}
            y2={yOf(t)}
            class={t === ticks[0] ? 'base' : 'grid'}
          />
          <text x={gutter - 6} y={yOf(t) + 4} text-anchor="end" class="t">{tick(t)}</text>
        {/each}

        {#if band}
          <rect
            x={gutter}
            y={yOf(band.high)}
            width={plotW}
            height={Math.max(1, yOf(band.low) - yOf(band.high))}
            class="target"
          />
        {/if}

        {#each marks as m, i (i)}
          {@const mx = xOf(m.t)}
          <line x1={mx} x2={mx} y1={PAD_TOP} y2={PAD_TOP + height} class="mark" />
          <circle cx={mx} cy={PAD_TOP} r="3.5" style="fill: {m.color ?? 'var(--fg-muted)'}">
            <title>{m.label}</title>
          </circle>
          {#if marks.length <= 3}
            <text
              x={mx + (mx > W - 110 ? -7 : 7)}
              y={PAD_TOP + 4}
              text-anchor={mx > W - 110 ? 'end' : 'start'}
              class="t mark-text">{m.label.length > 22 ? `${m.label.slice(0, 21)}…` : m.label}</text
            >
          {/if}
        {/each}

        {#if area}<path d={area} class="spread" />{/if}
        {#if line}<path d={line} class="trace" />{/if}

        {#each dotRows as p, i (i)}
          <circle cx={xOf(p.t)} cy={yOf(p.value)} r={dots ? 3.5 : 2.5} class="dot" />
        {/each}

        {#if activeRow}
          {@const ax = xOf(activeRow.t)}
          <line x1={ax} x2={ax} y1={PAD_TOP} y2={PAD_TOP + height} class="guide" />
          <circle cx={ax} cy={yOf(activeRow.value)} r="4.5" class="here" />
        {/if}

        {#each axisLabels as a (a.key)}
          <text x={a.x} y={PAD_TOP + height + 15} text-anchor={a.anchor} class="t">{a.text}</text>
        {/each}
      </svg>

      {#if activeRow && activeText}
        <div class="tip" style="left: {tipX}px">{activeText}</div>
      {/if}
    </div>
    {#if marks.length > 0}
      <ul class="sr">
        {#each marks as m, i (i)}<li>{m.label}</li>{/each}
      </ul>
    {/if}
    <span class="sr" aria-live="polite">{activeText}</span>
  </div>
{/if}

<style>
  .chart {
    position: relative;
    min-width: 0;
  }

  .stage {
    position: relative;
    border-radius: var(--radius-sm);
    touch-action: pan-y;
  }

  svg {
    display: block;
    width: 100%;
    overflow: visible;
  }

  .t {
    fill: var(--fg-faint);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
  .unit {
    fill: var(--fg-subtle);
  }
  .mark-text {
    fill: var(--fg-muted);
  }

  .base {
    stroke: var(--border-strong);
    stroke-width: 1;
  }
  .grid {
    stroke: var(--border);
    stroke-width: 1;
    opacity: 0.7;
  }

  /* A target or reference range, in the one colour that means "as it should
     be". A tint, so the line and its points stay the loudest thing. */
  .target {
    fill: color-mix(in oklab, var(--success) 13%, transparent);
  }

  .spread {
    fill: color-mix(in oklab, var(--c) 16%, transparent);
    stroke: none;
  }
  .trace {
    fill: none;
    stroke: var(--c);
    stroke-width: 2;
    stroke-linejoin: round;
    stroke-linecap: round;
  }
  .dot {
    fill: var(--c);
  }

  .mark {
    stroke: var(--fg-faint);
    stroke-width: 1;
    stroke-dasharray: 2 3;
  }

  .guide {
    stroke: var(--fg-faint);
    stroke-width: 1;
  }
  .here {
    fill: var(--bg);
    stroke: var(--c);
    stroke-width: 2;
  }

  .tip {
    position: absolute;
    top: 0;
    translate: -50% 0;
    padding: 3px var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    box-shadow: var(--shadow-sm);
    color: var(--fg);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    pointer-events: none;
  }

  .none {
    margin: 0;
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }

  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    margin: 0;
    padding: 0;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
    list-style: none;
  }
</style>
