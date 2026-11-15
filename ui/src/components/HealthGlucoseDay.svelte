<script lang="ts">
  // One day of glucose beside what shaped it: meals, doses, insulin, workouts
  // and sleep, on a single clock.
  //
  // The line is the glucose; everything else is context drawn so the eye can
  // run down from an event to the line under it. Meals are marked along the
  // top with their carbohydrate, workouts are shaded spans behind the line,
  // doses and insulin sit in a lane under the plot, and sleep is a band along
  // the very bottom. The 70-180 range the person aims for is shaded behind
  // the line, and a gap in the sensor's readings is a gap in the line rather
  // than a slope across it.
  //
  // Each kind of marker has its own shape as well as its own place, so
  // nothing depends on telling two colours apart, and the list under the chart
  // says all of it in words.
  //
  // The day is `health.date`; the arrows step it, and the store reloads the
  // timeline.

  import { health } from '../lib/health.svelte'
  import { activityName, slotName, SLEEP_STAGE_COLORS, stageName } from '../lib/health'
  import { friendlyDate, formatInstantTime, hourLabel } from '../lib/format'
  import { addDays, offsetInDay, todayIso } from '../lib/time'
  import type { Dose, Meal, SamplePoint } from '../lib/types'
  import Icon from './Icon.svelte'

  const date = $derived(health.date || todayIso())
  const atPresent = $derived(date >= todayIso())
  const tl = $derived(health.timeline && health.timeline.date === date ? health.timeline : null)

  // ── Geometry ────────────────────────────────────────────────────────────

  const LEFT = 40
  const RIGHT = 10
  const MEAL_LANE = 40
  const PLOT_H = 150
  const DOSE_LANE = 24
  const SLEEP_H = 8
  const AXIS_H = 22

  const plotTop = MEAL_LANE
  const plotBottom = plotTop + PLOT_H
  const doseMid = plotBottom + DOSE_LANE / 2 + 2
  const sleepTop = plotBottom + DOSE_LANE + 4
  const height = sleepTop + SLEEP_H + AXIS_H

  let width = $state(640)
  const plotW = $derived(Math.max(10, width - LEFT - RIGHT))

  function tx(instant: string): number {
    const minutes = Math.min(1440, Math.max(0, offsetInDay(instant, date)))
    return LEFT + (minutes / 1440) * plotW
  }
  function xMinute(minute: number): number {
    return LEFT + (Math.min(1440, Math.max(0, minute)) / 1440) * plotW
  }

  // ── Glucose ─────────────────────────────────────────────────────────────

  const glucose = $derived(
    [...(tl?.glucose ?? [])].sort((a, b) => Date.parse(a.start) - Date.parse(b.start)),
  )
  const lowTarget = $derived(health.settings?.glucoseLow ?? 70)
  const highTarget = $derived(health.settings?.glucoseHigh ?? 180)

  const yLo = $derived(
    glucose.length > 0
      ? Math.min(50, Math.floor((Math.min(...glucose.map((p) => p.value)) - 10) / 10) * 10)
      : 50,
  )
  const yHi = $derived(
    glucose.length > 0
      ? Math.max(250, Math.ceil((Math.max(...glucose.map((p) => p.value)) + 10) / 10) * 10)
      : 250,
  )
  function y(value: number): number {
    const clamped = Math.min(yHi, Math.max(yLo, value))
    return plotTop + (1 - (clamped - yLo) / (yHi - yLo)) * PLOT_H
  }

  /** The longest silence the line is drawn across, in milliseconds. */
  const GAP = 30 * 60_000

  const runs = $derived.by((): SamplePoint[][] => {
    const out: SamplePoint[][] = []
    let run: SamplePoint[] = []
    let last = Number.NEGATIVE_INFINITY
    for (const p of glucose) {
      const at = Date.parse(p.start)
      if (run.length > 0 && at - last > GAP) {
        out.push(run)
        run = []
      }
      run.push(p)
      last = at
    }
    if (run.length > 0) out.push(run)
    return out
  })
  function path(run: SamplePoint[]): string {
    return `M${run.map((p) => `${tx(p.start).toFixed(1)},${y(p.value).toFixed(1)}`).join('L')}`
  }

  function showGlucose(value: number): string {
    const shown = health.toDisplay('blood_glucose', value)
    return health.displayUnit('blood_glucose') === 'mmol/L'
      ? shown.toFixed(1)
      : String(Math.round(shown))
  }
  const unit = $derived(health.displayUnit('blood_glucose'))
  const ticks = $derived.by(() => {
    const out = [lowTarget, highTarget]
    if (yHi - highTarget > 40) out.push(250)
    return [...new Set(out)].filter((v) => v > yLo && v < yHi)
  })

  // ── Meals, doses, insulin ───────────────────────────────────────────────

  function carbsOf(meal: Meal): number {
    return meal.items.reduce((sum, i) => sum + i.nutrients.carbsG, 0)
  }

  interface MealMark {
    id: string
    x: number
    label: string
    row: 0 | 1 | null
    from: number
    anchor: 'start' | 'end'
  }
  /** Labels run across the top in up to two rows; one that would land on
   *  another is left to its tooltip and the list below. */
  const mealMarks = $derived.by((): MealMark[] => {
    if (!tl) return []
    const ends = [Number.NEGATIVE_INFINITY, Number.NEGATIVE_INFINITY]
    return [...tl.meals]
      .sort((a, b) => Date.parse(a.at) - Date.parse(b.at))
      .map((m): MealMark => {
        const x = tx(m.at)
        const carbs = carbsOf(m)
        const label = `${slotName(m.slot)}${carbs > 0 ? ` · ${health.display('dietary_carbs', carbs)}` : ''}`
        const w = label.length * 6.3
        const flip = x + 8 + w > width - RIGHT
        const from = flip ? x - 8 - w : x + 8
        const row: 0 | 1 | null =
          (ends[0] ?? 0) + 4 <= from ? 0 : (ends[1] ?? 0) + 4 <= from ? 1 : null
        if (row !== null) ends[row] = from + w
        return { id: m.id, x, label, row, from, anchor: flip ? 'end' : 'start' }
      })
  })

  const doses = $derived((tl?.doses ?? []).filter((d) => d.status !== 'missed'))
  const insulin = $derived(tl?.insulin ?? [])

  function medName(d: Dose): string {
    return health.medications.find((m) => m.id === d.medicationId)?.name ?? 'A medicine'
  }

  // ── Workouts and sleep ──────────────────────────────────────────────────

  const workouts = $derived(tl?.workouts ?? [])
  const sleep = $derived(
    (tl?.sleep ?? [])
      .filter((s) => s.stage !== 'inBed')
      .map((s) => ({ ...s, x1: tx(s.start), x2: tx(s.end) }))
      .filter((s) => s.x2 > s.x1),
  )

  // ── Reading a time off the chart ────────────────────────────────────────

  let hoverX = $state<number | null>(null)
  const hovered = $derived.by((): SamplePoint | null => {
    if (hoverX === null || glucose.length === 0) return null
    const minute = ((hoverX - LEFT) / plotW) * 1440
    let best: SamplePoint | null = null
    let gap = Number.POSITIVE_INFINITY
    for (const p of glucose) {
      const d = Math.abs(offsetInDay(p.start, date) - minute)
      if (d < gap) {
        gap = d
        best = p
      }
    }
    return best && gap <= 20 ? best : null
  })
  function track(e: PointerEvent) {
    const box = (e.currentTarget as HTMLElement).getBoundingClientRect()
    const px = e.clientX - box.left
    hoverX = px >= LEFT && px <= box.width - RIGHT ? px : null
  }

  // ── The same in words ───────────────────────────────────────────────────

  const events = $derived.by(() => {
    if (!tl) return []
    const rows: { at: number; text: string }[] = []
    for (const m of tl.meals) {
      const kcal = m.items.reduce((sum, i) => sum + i.nutrients.kcal, 0)
      rows.push({
        at: Date.parse(m.at),
        text: `${formatInstantTime(m.at)}  ${slotName(m.slot)}${m.name ? `, ${m.name}` : ''} (${health.display('dietary_carbs', carbsOf(m))} carbs, ${health.display('dietary_energy', kcal)})`,
      })
    }
    for (const d of doses) {
      rows.push({
        at: Date.parse(d.at),
        text: `${formatInstantTime(d.at)}  ${medName(d)} ${d.status === 'skipped' ? 'skipped' : 'taken'}`,
      })
    }
    for (const i of insulin) {
      rows.push({
        at: Date.parse(i.start),
        text: `${formatInstantTime(i.start)}  Insulin ${health.display('insulin_bolus', i.value)}`,
      })
    }
    for (const w of workouts) {
      rows.push({
        at: Date.parse(w.start),
        text: `${formatInstantTime(w.start)}–${formatInstantTime(w.end)}  ${w.name || activityName(w.activity)}`,
      })
    }
    return rows.sort((a, b) => a.at - b.at)
  })

  const summary = $derived(
    glucose.length === 0
      ? `No glucose readings on ${friendlyDate(date)}.`
      : `Glucose on ${friendlyDate(date)}: ${glucose.length} readings, from ${showGlucose(
          Math.min(...glucose.map((p) => p.value)),
        )} to ${showGlucose(Math.max(...glucose.map((p) => p.value)))} ${unit}.`,
  )
</script>

<div class="day">
  <div class="bar">
    <div class="days" role="group" aria-label="Day">
      <button
        class="step"
        aria-label="The day before"
        title="The day before"
        onclick={() => health.setDate(addDays(date, -1))}
      >
        <span class="back"><Icon name="chevron" size={13} /></span>
      </button>
      <button
        class="when"
        title={atPresent ? friendlyDate(date) : 'Back to today'}
        disabled={atPresent}
        onclick={() => health.setDate(todayIso())}
      >
        {friendlyDate(date)}
      </button>
      <button
        class="step"
        aria-label="The day after"
        title="The day after"
        disabled={atPresent}
        onclick={() => health.setDate(addDays(date, 1))}
      >
        <Icon name="chevron" size={13} />
      </button>
    </div>
    <p class="readout">
      {#if hovered}
        <b>{formatInstantTime(hovered.start)}</b>
        {showGlucose(hovered.value)}
        {unit}
      {/if}
    </p>
  </div>

  {#if !tl}
    <p class="none">Loading the day…</p>
  {:else}
    {#if glucose.length === 0}
      <p class="none">No glucose readings on this day.</p>
    {/if}

    <div
      class="wrap"
      role="img"
      aria-label="{summary} The list below the chart has everything that happened that day."
      bind:clientWidth={width}
      onpointermove={track}
      onpointerleave={() => (hoverX = null)}
    >
      <svg {width} {height} viewBox="0 0 {width} {height}" aria-hidden="true">
        <!-- The range aimed for -->
        <rect
          class="target"
          x={LEFT}
          y={y(highTarget)}
          width={plotW}
          height={Math.max(0, y(lowTarget) - y(highTarget))}
        />

        <!-- Workouts, shaded behind the line -->
        {#each workouts as w (w.id)}
          {@const x1 = tx(w.start)}
          {@const x2 = tx(w.end)}
          <g>
            <title
              >{w.name || activityName(w.activity)}, {formatInstantTime(
                w.start,
              )}–{formatInstantTime(w.end)}</title
            >
            <rect class="workout" x={x1} y={plotTop} width={Math.max(3, x2 - x1)} height={PLOT_H} />
            {#if x2 - x1 > 46}
              <text class="small" x={x1 + 4} y={plotBottom - 5}
                >{w.name || activityName(w.activity)}</text
              >
            {/if}
          </g>
        {/each}

        {#each ticks as t (t)}
          <line class="rule" x1={LEFT} x2={LEFT + plotW} y1={y(t)} y2={y(t)} />
          <text class="tick" x={LEFT - 6} y={y(t) + 4} text-anchor="end">{showGlucose(t)}</text>
        {/each}
        <line class="axis" x1={LEFT} x2={LEFT + plotW} y1={plotBottom} y2={plotBottom} />

        <!-- Meals: a stem down to the line, a dot and a label above -->
        {#each mealMarks as m (m.id)}
          <line class="stem" x1={m.x} x2={m.x} y1={MEAL_LANE - 10} y2={plotBottom} />
        {/each}

        <!-- The glucose line -->
        {#each runs as run, i (i)}
          {#if run.length === 1 && run[0]}
            <circle class="lone" cx={tx(run[0].start)} cy={y(run[0].value)} r="3" />
          {:else}
            <path class="line" d={path(run)} />
          {/if}
        {/each}

        {#each mealMarks as m (m.id)}
          <circle class="meal" cx={m.x} cy={MEAL_LANE - 10} r="4.5" />
          {#if m.row !== null}
            <text
              class="label"
              x={m.anchor === 'start' ? m.from : m.from + m.label.length * 6.3}
              y={m.row === 0 ? MEAL_LANE - 6 : MEAL_LANE - 22}
              text-anchor={m.anchor}>{m.label}</text
            >
          {/if}
        {/each}

        <!-- Doses and insulin, in a lane of their own -->
        {#each doses as d (d.id)}
          {@const cx = tx(d.at)}
          <path
            class="dose"
            class:skipped={d.status === 'skipped'}
            d="M{cx},{doseMid - 5}L{cx + 5},{doseMid}L{cx},{doseMid + 5}L{cx - 5},{doseMid}Z"
          >
            <title
              >{medName(d)}
              {d.status === 'skipped' ? 'skipped' : 'taken'}, {formatInstantTime(d.at)}</title
            >
          </path>
        {/each}
        {#each insulin as i (i.start)}
          {@const cx = tx(i.start)}
          <path
            class="insulin"
            d="M{cx},{doseMid - 5}L{cx + 5},{doseMid + 4}L{cx - 5},{doseMid + 4}Z"
          >
            <title
              >Insulin {health.display('insulin_bolus', i.value)}, {formatInstantTime(
                i.start,
              )}</title
            >
          </path>
        {/each}

        <!-- Sleep, along the bottom -->
        {#each sleep as s, i (i)}
          <rect
            class="sleep"
            class:awake={s.stage === 'awake'}
            x={s.x1}
            y={sleepTop}
            width={s.x2 - s.x1}
            height={SLEEP_H}
            style:fill={SLEEP_STAGE_COLORS[s.stage]}
          >
            <title
              >{stageName(s.stage)}, {formatInstantTime(s.start)}–{formatInstantTime(s.end)}</title
            >
          </rect>
        {/each}

        {#each [0, 6, 12, 18, 24] as h (h)}
          <text
            class="tick"
            x={xMinute(h * 60)}
            y={height - 6}
            text-anchor={h === 0 ? 'start' : h === 24 ? 'end' : 'middle'}>{hourLabel(h % 24)}</text
          >
        {/each}

        {#if hovered}
          <line
            class="cursor"
            x1={tx(hovered.start)}
            x2={tx(hovered.start)}
            y1={plotTop}
            y2={plotBottom}
          />
          <circle class="dot" cx={tx(hovered.start)} cy={y(hovered.value)} r="4" />
        {/if}
      </svg>
    </div>

    <ul class="key">
      <li><span class="k line"></span>Glucose</li>
      <li><span class="k meal"></span>Meal, with carbs</li>
      {#if doses.length > 0}<li><span class="k dose"></span>Dose</li>{/if}
      {#if insulin.length > 0}<li><span class="k insulin"></span>Insulin</li>{/if}
      {#if workouts.length > 0}<li><span class="k workout"></span>Workout</li>{/if}
      {#if sleep.length > 0}<li><span class="k sleepkey"></span>Sleep</li>{/if}
      <li><span class="k goal"></span>{showGlucose(lowTarget)}–{showGlucose(highTarget)} {unit}</li>
    </ul>

    {#if events.length > 0}
      <details class="twin">
        <summary>The day in a list</summary>
        <ul class="events">
          {#each events as e (e.at + e.text)}
            <li>{e.text}</li>
          {/each}
        </ul>
      </details>
    {/if}
  {/if}
</div>

<style>
  .day {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    min-width: 0;
  }

  .bar {
    display: flex;
    align-items: center;
    gap: var(--sp-4);
    min-height: 30px;
  }
  .days {
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 2px;
    border-radius: 999px;
    background: var(--bg-sunken);
  }
  .step {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .step:hover:not(:disabled) {
    background: var(--bg-raised);
    color: var(--fg);
  }
  .step:disabled {
    opacity: 0.35;
  }
  .back {
    display: grid;
    place-items: center;
    rotate: 180deg;
  }
  .when {
    min-width: 7.5em;
    height: 26px;
    padding: 0 var(--sp-2);
    border-radius: 999px;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg);
  }
  .when:hover:not(:disabled) {
    background: var(--bg-raised);
  }
  .when:disabled {
    cursor: default;
  }
  .readout {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--fg-muted);
    font-variant-numeric: tabular-nums;
  }
  .readout b {
    color: var(--fg);
    font-weight: 600;
    margin-right: var(--sp-1);
  }

  .wrap {
    width: 100%;
    min-width: 0;
    touch-action: pan-y;
  }
  svg {
    display: block;
    max-width: 100%;
  }

  .target {
    fill: var(--app-health, var(--accent));
    fill-opacity: 0.09;
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
  .label {
    font-size: 11px;
    fill: var(--fg-muted);
  }
  .small {
    font-size: 10px;
    fill: var(--fg-subtle);
  }
  .workout {
    fill: var(--fg-muted);
    fill-opacity: 0.13;
  }
  .stem {
    stroke: var(--border-strong);
    stroke-width: 1;
  }
  .line {
    fill: none;
    stroke: var(--app-health, var(--accent));
    stroke-width: 2;
    stroke-linejoin: round;
    stroke-linecap: round;
  }
  .lone {
    fill: var(--app-health, var(--accent));
  }
  .meal {
    fill: var(--app-health, var(--accent));
    stroke: var(--bg);
    stroke-width: 2;
  }
  .dose {
    fill: var(--fg-muted);
    stroke: var(--bg);
    stroke-width: 1.5;
  }
  .dose.skipped {
    fill: none;
    stroke: var(--fg-muted);
  }
  .insulin {
    fill: var(--accent);
    stroke: var(--bg);
    stroke-width: 1.5;
  }
  .sleep {
    fill-opacity: 0.9;
  }
  .sleep.awake {
    fill-opacity: 0.4;
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
  .k {
    flex: none;
    display: inline-block;
    width: 12px;
    height: 12px;
  }
  .k.line {
    height: 2px;
    width: 14px;
    background: var(--app-health, var(--accent));
  }
  .k.meal {
    border-radius: 50%;
    width: 9px;
    height: 9px;
    background: var(--app-health, var(--accent));
  }
  .k.dose {
    width: 9px;
    height: 9px;
    background: var(--fg-muted);
    rotate: 45deg;
  }
  .k.insulin {
    width: 0;
    height: 0;
    border-left: 5px solid transparent;
    border-right: 5px solid transparent;
    border-bottom: 9px solid var(--accent);
  }
  .k.workout {
    width: 14px;
    height: 9px;
    background: color-mix(in oklab, var(--fg-muted) 22%, transparent);
  }
  .k.sleepkey {
    width: 14px;
    height: 5px;
    border-radius: 2px;
    background: var(--fg-faint);
  }
  .k.goal {
    width: 14px;
    height: 9px;
    background: color-mix(in oklab, var(--app-health, var(--accent)) 14%, transparent);
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
  .events {
    margin: var(--sp-2) 0 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: var(--text-sm);
    color: var(--fg-muted);
    white-space: pre-wrap;
    font-variant-numeric: tabular-nums;
  }
</style>
