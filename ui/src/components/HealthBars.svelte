<script lang="ts">
  // Bars over time, from a baseline, with an optional goal line: the shared
  // chart of every Health pane that counts something per day -- active
  // energy, exercise minutes, steps, calories eaten.
  //
  // Bars rather than a line, for ColumnChart's reason: what is plotted is a
  // total for a day, and a line between two days asserts a value on the days
  // in between. A day with nothing recorded is a gap, so it draws nothing.
  //
  // Drawn as SVG at the pixel size of its box (the box is measured and the
  // viewBox is made to match), not scaled from a fixed size, so the text
  // stays the size of the text around it however wide the pane is.
  //
  // House style: one colour, the subject's own; text in text tokens, never
  // the series colour; bars at most 24 px wide and rounded 4 px at the data
  // end, square on the baseline; hairline axes; labels sparing -- a few date
  // ticks and the value ticks, everything else in the tooltip. The goal is a
  // dashed hairline, named at its right end.
  //
  // Keyboard: the chart is one tab stop. Left and right move between bars
  // (the tooltip follows, and is read out), Home and End jump to the ends,
  // and Enter picks the bar the way a click does.
  //
  // A finger cannot hover, so on a phone the first tap on a bar reads its
  // value -- the tooltip stays -- and the tooltip, or a second tap on the
  // same bar, picks it. Dragging along the chart reads each bar in turn.

  import { dateFormat } from '../lib/format'
  import { phone } from '../lib/phone.svelte'
  import { locale, startOfDay } from '../lib/time'
  import Icon from './Icon.svelte'

  interface Props {
    points: { at: string; value: number }[]
    unit?: string
    goal?: number | null
    color?: string
    height?: number
    /** The tooltip / label text for one point. */
    label?: (point: { at: string; value: number }) => string
    /** Click a bar (e.g. open that day). */
    onpick?: (point: { at: string; value: number }) => void
  }

  const {
    points,
    unit = '',
    goal = null,
    color = 'var(--accent)',
    height = 140,
    label,
    onpick,
  }: Props = $props()

  type Point = Props['points'][number]

  /** Room above the plot for the unit, and the band of date labels below it. */
  const PAD_TOP = 18
  const AXIS = 22

  let width = $state(600)
  let active = $state<number | null>(null)

  const n = $derived(points.length)
  const W = $derived(Math.max(160, width))

  // ── the value axis ─────────────────────────────────────────────────────

  const top = $derived(Math.max(0, goal ?? 0, ...points.map((p) => p.value)))

  /** A step of 1, 2, 2.5, 5 or 10 times a power of ten that gives about `count` ticks. */
  function niceStep(max: number, count: number): number {
    if (!(max > 0)) return 1
    const raw = max / count
    const mag = 10 ** Math.floor(Math.log10(raw))
    const norm = raw / mag
    const step = norm <= 1 ? 1 : norm <= 2 ? 2 : norm <= 2.5 ? 2.5 : norm <= 5 ? 5 : 10
    return step * mag
  }

  const step = $derived(niceStep(top, 3))
  const ceiling = $derived(top > 0 ? Math.ceil(top / step - 1e-9) * step : 1)
  const ticks = $derived(Array.from({ length: Math.round(ceiling / step) + 1 }, (_, i) => i * step))

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
  /** A value as written in the tooltip: no more digits than it means. */
  function fmt(v: number): string {
    const a = Math.abs(v)
    return numberFormat({ maximumFractionDigits: a >= 100 ? 0 : a >= 10 ? 1 : 2 }).format(v)
  }
  /** A value as written on the axis: 12.5k, not 12,500. */
  function tick(v: number): string {
    return Math.abs(v) >= 10_000
      ? numberFormat({ notation: 'compact', maximumFractionDigits: 1 }).format(v)
      : fmt(v)
  }

  // ── geometry ───────────────────────────────────────────────────────────

  const gutter = $derived(Math.max(1, ...ticks.map((t) => tick(t).length)) * 7 + 10)
  const plotW = $derived(Math.max(40, W - gutter - 2))
  const slot = $derived(n > 0 ? plotW / n : plotW)
  const barW = $derived(Math.min(24, Math.max(1, slot * 0.72)))
  const svgH = $derived(PAD_TOP + height + AXIS)

  const yOf = (v: number) => PAD_TOP + height * (1 - v / ceiling)

  /** A bar rounded 4 px at the top and square on the baseline. */
  function barPath(x: number, y: number, w: number, h: number): string {
    const r = Math.min(4, w / 2, h)
    return (
      `M${x.toFixed(2)},${(y + h).toFixed(2)}V${(y + r).toFixed(2)}` +
      `Q${x.toFixed(2)},${y.toFixed(2)} ${(x + r).toFixed(2)},${y.toFixed(2)}` +
      `H${(x + w - r).toFixed(2)}Q${(x + w).toFixed(2)},${y.toFixed(2)} ` +
      `${(x + w).toFixed(2)},${(y + r).toFixed(2)}V${(y + h).toFixed(2)}Z`
    )
  }

  // ── the date axis ──────────────────────────────────────────────────────

  /** `YYYY-MM-DD` is a local day; anything longer is an instant. */
  function when(at: string): Date {
    return at.length <= 10 ? startOfDay(at) : new Date(at)
  }

  const dates = $derived(points.map((p) => when(p.at)))
  const spanDays = $derived(
    n > 1 ? ((dates[n - 1]?.getTime() ?? 0) - (dates[0]?.getTime() ?? 0)) / 86_400_000 : 0,
  )
  const crossesYears = $derived(n > 1 && dates[0]?.getFullYear() !== dates[n - 1]?.getFullYear())

  const axisFormat = $derived(
    n <= 7 && spanDays <= 7
      ? dateFormat({ weekday: 'short' })
      : spanDays > 200
        ? dateFormat({ month: 'short', ...(crossesYears ? { year: '2-digit' as const } : {}) })
        : dateFormat({ day: 'numeric', month: 'short' }),
  )

  /** Every `every`th bar gets a date, and a month named twice is named once. */
  const every = $derived(Math.max(1, Math.ceil(n / Math.max(2, Math.floor(plotW / 60)))))
  const axisLabels = $derived.by(() => {
    const out: { i: number; x: number; text: string; anchor: 'start' | 'middle' | 'end' }[] = []
    let last = ''
    for (let i = 0; i < n; i += every) {
      const d = dates[i]
      if (!d) continue
      const text = axisFormat.format(d).replace('.', '')
      if (text === last) continue
      last = text
      const x = gutter + i * slot + slot / 2
      const anchor = x < gutter + 18 ? 'start' : x > W - 18 ? 'end' : 'middle'
      out.push({ i, x: anchor === 'start' ? gutter : anchor === 'end' ? W : x, text, anchor })
    }
    return out
  })

  // ── the tooltip ────────────────────────────────────────────────────────

  function tipText(p: Point, d: Date): string {
    if (label) return label(p)
    const day = dateFormat({ weekday: 'short', day: 'numeric', month: 'short' }).format(d)
    return `${day} · ${fmt(p.value)}${unit ? ` ${unit}` : ''}`
  }

  const activeText = $derived.by(() => {
    if (active === null) return ''
    const p = points[active]
    const d = dates[active]
    return p && d ? tipText(p, d) : ''
  })
  const tipX = $derived(
    active === null ? 0 : Math.min(Math.max(gutter + active * slot + slot / 2, 70), W - 70),
  )

  const ariaLabel = $derived(
    `${unit ? `${unit}, ` : ''}${n} values, ` +
      (onpick
        ? 'arrow keys move between them and Enter opens one'
        : 'arrow keys move between them'),
  )

  function indexAt(e: MouseEvent): number | null {
    const box = (e.currentTarget as HTMLElement).getBoundingClientRect()
    const x = e.clientX - box.left - gutter
    if (x < 0 || x > plotW) return null
    return Math.min(n - 1, Math.max(0, Math.floor(x / slot)))
  }

  function move(e: PointerEvent) {
    active = indexAt(e)
  }

  function leave(e: PointerEvent) {
    // A finger lifting is not a pointer leaving: the value read stays.
    if (e.pointerType !== 'touch') active = null
  }

  function pick(e: MouseEvent) {
    const i = indexAt(e)
    const p = i === null ? undefined : points[i]
    if (i !== null && phone.on && active !== i) {
      active = i
      return
    }
    if (p && onpick) onpick(p)
  }

  function pickActive(e: MouseEvent) {
    // Not on to the chart's own tap, which would read another bar.
    e.stopPropagation()
    const p = active === null ? undefined : points[active]
    if (p && onpick) onpick(p)
  }

  function key(e: KeyboardEvent) {
    if (n === 0) return
    const at = active ?? n - 1
    if (e.key === 'ArrowRight') active = Math.min(n - 1, at + 1)
    else if (e.key === 'ArrowLeft') active = Math.max(0, at - 1)
    else if (e.key === 'Home') active = 0
    else if (e.key === 'End') active = n - 1
    else if (e.key === 'Enter' || e.key === ' ') {
      const p = active === null ? undefined : points[active]
      if (p && onpick) {
        e.preventDefault()
        onpick(p)
      }
      return
    } else return
    e.preventDefault()
  }

  function focused(e: FocusEvent) {
    if (active === null && (e.currentTarget as HTMLElement).matches(':focus-visible')) {
      active = n - 1
    }
  }
</script>

{#if n === 0}
  <p class="none">Nothing recorded in this window.</p>
{:else}
  <div class="chart" style="--c: {color}" bind:clientWidth={width}>
    <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
    <div
      class="stage"
      class:pick={!!onpick}
      role="group"
      tabindex="0"
      aria-label={ariaLabel}
      onpointermove={move}
      onpointerleave={leave}
      onclick={pick}
      onkeydown={key}
      onfocus={focused}
      onblur={() => (active = null)}
    >
      <svg viewBox="0 0 {W} {svgH}" width="100%" height={svgH} aria-hidden="true" focusable="false">
        {#if unit}
          <text x="0" y="10" class="t unit">{unit}</text>
        {/if}

        {#each ticks as t (t)}
          <line x1={gutter} x2={W} y1={yOf(t)} y2={yOf(t)} class={t === 0 ? 'base' : 'grid'} />
          <text x={gutter - 6} y={yOf(t) + 4} text-anchor="end" class="t">{tick(t)}</text>
        {/each}

        {#if active !== null}
          <rect x={gutter + active * slot} y={PAD_TOP} width={slot} {height} class="hl" />
        {/if}

        {#each points as p, i (i)}
          {#if p.value > 0}
            {@const h = Math.max(3, (height * p.value) / ceiling)}
            <path
              d={barPath(gutter + i * slot + (slot - barW) / 2, PAD_TOP + height - h, barW, h)}
              class="bar"
              class:dim={active !== null && active !== i}
            />
          {/if}
        {/each}

        {#if goal !== null && goal > 0}
          {@const gy = yOf(goal)}
          <line x1={gutter} x2={W} y1={gy} y2={gy} class="goal" />
          <text x={W} y={gy - 4} text-anchor="end" class="t goal-text">Goal {fmt(goal)}</text>
        {/if}

        {#each axisLabels as a (a.i)}
          <text x={a.x} y={PAD_TOP + height + 15} text-anchor={a.anchor} class="t">{a.text}</text>
        {/each}
      </svg>

      {#if active !== null && activeText}
        {#if phone.on && onpick}
          <button type="button" class="tip go" style="left: {tipX}px" onclick={pickActive}>
            {activeText}
            <Icon name="chevron" size={12} weight={2} />
          </button>
        {:else}
          <div class="tip" style="left: {tipX}px">{activeText}</div>
        {/if}
      {/if}
    </div>
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
  .stage.pick {
    cursor: pointer;
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

  /* The baseline the bars grow from, and the fainter rules the value ticks
     sit on. Hairlines, solid, a step off the surface. */
  .base {
    stroke: var(--border-strong);
    stroke-width: 1;
  }
  .grid {
    stroke: var(--border);
    stroke-width: 1;
    opacity: 0.7;
  }

  .bar {
    fill: var(--c);
    transition: opacity var(--fast) var(--ease);
  }
  .bar.dim {
    opacity: 0.55;
  }

  .hl {
    fill: var(--bg-hover);
  }

  .goal {
    stroke: var(--fg-subtle);
    stroke-width: 1;
    stroke-dasharray: 4 3;
  }
  .goal-text {
    fill: var(--fg-muted);
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

  /* On a phone the tooltip is what opens the day: a finger-sized button. */
  .tip.go {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-1);
    min-height: 34px;
    padding: 0 var(--sp-3);
    color: var(--accent);
    font-weight: 600;
    pointer-events: auto;
  }

  .none {
    margin: 0;
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }

  /* Read out by a screen reader as the bars are walked, never drawn. */
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
</style>
