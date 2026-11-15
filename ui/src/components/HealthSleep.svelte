<script lang="ts">
  // Sleep over the window: a bar for each night, stacked by stage, with the
  // goal as a dashed line -- and, for the night picked, the night itself.
  //
  // Three questions, three forms. "How much, how often": columns per night,
  // each one the night's time asleep (deep at the foot, then core, REM, and
  // time awake on top, so the part that counts as sleep is the part under
  // the goal line). "What happened in the night": a hypnogram, stage over
  // time as bands stepping between four rows, the way sleep trackers draw it.
  // "How was it scored": the score's parts listed in words with their numbers
  // -- the score is this app's own rule of thumb, and showing how it was
  // worked out is what keeps it from being an oracle.
  //
  // Stage colours are the helper's, one per stage, and a keyed legend names
  // each, so colour is never the only thing telling deep from REM.

  import { health } from '../lib/health.svelte'
  import { durationText, SLEEP_STAGE_COLORS, stageName } from '../lib/health'
  import { dateFormat, formatInstantTime } from '../lib/format'
  import { locale, startOfDay } from '../lib/time'
  import type { SleepNight, SleepStage } from '../lib/types'
  import StatTile from './StatTile.svelte'

  const nights = $derived(health.sleep)
  const withData = $derived(nights.filter((n) => n.asleepMin > 0))
  const goalMin = $derived((health.settings?.goals.sleepHours ?? 0) * 60)

  // ── the averages ───────────────────────────────────────────────────────

  function mean(values: (number | null | undefined)[]): number | null {
    const ok = values.filter((v): v is number => typeof v === 'number' && Number.isFinite(v))
    return ok.length > 0 ? ok.reduce((a, b) => a + b, 0) / ok.length : null
  }
  const avgAsleep = $derived(mean(withData.map((n) => n.asleepMin)))
  const avgEfficiency = $derived(mean(withData.map((n) => n.efficiency)))
  const avgScore = $derived(mean(withData.map((n) => n.score)))
  const avgHrv = $derived(mean(withData.map((n) => n.hrvMs)))

  // ── the night picked ───────────────────────────────────────────────────

  let picked = $state<string | null>(null)
  const selected = $derived(
    withData.find((n) => n.date === picked) ?? withData[withData.length - 1] ?? null,
  )

  // ── the bars ───────────────────────────────────────────────────────────

  const PAD_TOP = 18
  const AXIS = 22
  const GUTTER = 38
  const HEIGHT = 150

  let width = $state(600)
  let active = $state<number | null>(null)

  const W = $derived(Math.max(200, width))
  const n = $derived(nights.length)

  /** A night's stack, bottom to top: deep, core, REM, sleep with no stage, awake. */
  function stack(night: SleepNight): { stage: SleepStage; min: number }[] {
    const plain = Math.max(0, night.asleepMin - night.coreMin - night.deepMin - night.remMin)
    const all: { stage: SleepStage; min: number }[] = [
      { stage: 'deep', min: night.deepMin },
      { stage: 'core', min: night.coreMin },
      { stage: 'rem', min: night.remMin },
      { stage: 'asleep', min: plain },
      { stage: 'awake', min: night.awakeMin },
    ]
    return all.filter((p) => p.min > 0)
  }
  const totalOf = (night: SleepNight) => stack(night).reduce((sum, p) => sum + p.min, 0)

  /** Two-hour steps, or four if a night or the goal runs past twelve hours. */
  const peak = $derived(Math.max(goalMin, ...nights.map(totalOf)))
  const hourStep = $derived(peak / 60 > 12 ? 4 : 2)
  const ceiling = $derived(
    Math.max(hourStep, Math.ceil(peak / 60 / hourStep - 1e-9) * hourStep) * 60,
  )
  const ticks = $derived(
    Array.from({ length: ceiling / 60 / hourStep + 1 }, (_, i) => i * hourStep * 60),
  )

  const plotW = $derived(Math.max(40, W - GUTTER - 2))
  const slot = $derived(n > 0 ? plotW / n : plotW)
  const barW = $derived(Math.min(24, Math.max(1, slot * 0.72)))
  const yOf = (min: number) => PAD_TOP + HEIGHT * (1 - min / ceiling)

  function topRounded(x: number, y: number, w: number, h: number): string {
    const r = Math.min(4, w / 2, h)
    return (
      `M${x.toFixed(2)},${(y + h).toFixed(2)}V${(y + r).toFixed(2)}` +
      `Q${x.toFixed(2)},${y.toFixed(2)} ${(x + r).toFixed(2)},${y.toFixed(2)}` +
      `H${(x + w - r).toFixed(2)}Q${(x + w).toFixed(2)},${y.toFixed(2)} ` +
      `${(x + w).toFixed(2)},${(y + r).toFixed(2)}V${(y + h).toFixed(2)}Z`
    )
  }

  /** Everything one night's bar is drawn from, so the template only draws. */
  const bars = $derived(
    nights.map((night, i) => {
      const x = GUTTER + i * slot + (slot - barW) / 2
      let floor = 0
      const parts = stack(night)
      const pieces = parts.map((p, j) => {
        const lo = floor
        floor += p.min
        const top = yOf(floor)
        const h = Math.max(1, yOf(lo) - top)
        return {
          stage: p.stage,
          rounded: j === parts.length - 1,
          x,
          y: top,
          h,
          d: j === parts.length - 1 ? topRounded(x, top, barW, h) : '',
        }
      })
      return { date: night.date, i, pieces }
    }),
  )

  const crossesYears = $derived(
    n > 1 && nights[0]?.date.slice(0, 4) !== nights[n - 1]?.date.slice(0, 4),
  )
  const dayOf = startOfDay
  const axisFormat = $derived(
    n <= 7
      ? dateFormat({ weekday: 'short' })
      : n > 200
        ? dateFormat({ month: 'short', ...(crossesYears ? { year: '2-digit' as const } : {}) })
        : dateFormat({ day: 'numeric', month: 'short' }),
  )
  const every = $derived(Math.max(1, Math.ceil(n / Math.max(2, Math.floor(plotW / 60)))))
  const axisLabels = $derived.by(() => {
    const out: { i: number; x: number; text: string; anchor: 'start' | 'middle' | 'end' }[] = []
    let last = ''
    for (let i = 0; i < n; i += every) {
      const night = nights[i]
      if (!night) continue
      const text = axisFormat.format(dayOf(night.date)).replace('.', '')
      if (text === last) continue
      last = text
      const x = GUTTER + i * slot + slot / 2
      const anchor = x < GUTTER + 18 ? 'start' : x > W - 18 ? 'end' : 'middle'
      out.push({ i, x: anchor === 'start' ? GUTTER : anchor === 'end' ? W : x, text, anchor })
    }
    return out
  })

  /** The stages named in the legend: the ones some night in the window had. */
  const legend = $derived(
    (['awake', 'rem', 'core', 'asleep', 'deep'] as SleepStage[]).filter((s) =>
      nights.some((night) => stack(night).some((p) => p.stage === s)),
    ),
  )

  function tipText(night: SleepNight): string {
    const day = dateFormat({ weekday: 'short', day: 'numeric', month: 'short' }).format(
      dayOf(night.date),
    )
    if (night.asleepMin <= 0) return `${day} · no sleep recorded`
    const awake = night.awakeMin > 0 ? `, ${durationText(night.awakeMin)} awake` : ''
    return `${day} · ${durationText(night.asleepMin)} asleep${awake}`
  }
  const activeNight = $derived(active === null ? null : (nights[active] ?? null))
  const activeText = $derived(activeNight ? tipText(activeNight) : '')
  const tipX = $derived(
    active === null ? 0 : Math.min(Math.max(GUTTER + active * slot + slot / 2, 90), W - 90),
  )

  function indexAt(e: MouseEvent): number | null {
    const box = (e.currentTarget as HTMLElement).getBoundingClientRect()
    const x = e.clientX - box.left - GUTTER
    if (x < 0 || x > plotW || n === 0) return null
    return Math.min(n - 1, Math.max(0, Math.floor(x / slot)))
  }
  function choose(index: number | null) {
    const night = index === null ? undefined : nights[index]
    if (night && night.asleepMin > 0) picked = night.date
  }
  function key(e: KeyboardEvent) {
    if (n === 0) return
    const at = active ?? n - 1
    if (e.key === 'ArrowRight') active = Math.min(n - 1, at + 1)
    else if (e.key === 'ArrowLeft') active = Math.max(0, at - 1)
    else if (e.key === 'Home') active = 0
    else if (e.key === 'End') active = n - 1
    else if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault()
      choose(active)
      return
    } else return
    e.preventDefault()
  }
  function focused(e: FocusEvent) {
    if (active === null && (e.currentTarget as HTMLElement).matches(':focus-visible')) {
      active = n - 1
    }
  }

  const goalText = $derived(goalMin > 0 ? durationText(goalMin) : '')

  // ── the hypnogram ──────────────────────────────────────────────────────

  /** Awake at the top, deep at the foot; sleep with no stage shares the core row. */
  const ROWS: SleepStage[] = ['awake', 'rem', 'core', 'deep']
  const rowOf = (stage: SleepStage) =>
    stage === 'awake' ? 0 : stage === 'rem' ? 1 : stage === 'deep' ? 3 : 2
  const LABEL_W = 52
  const ROW_H = 20
  const ROW_GAP = 8
  const HYP_H = ROWS.length * ROW_H + (ROWS.length - 1) * ROW_GAP

  let hypWidth = $state(600)
  const HW = $derived(Math.max(240, hypWidth))
  const hypPlotW = $derived(Math.max(40, HW - LABEL_W - 4))

  const hyp = $derived.by(() => {
    const night = selected
    if (!night) return null
    const segs = night.segments
      .filter((s) => s.stage !== 'inBed')
      .map((s) => ({ stage: s.stage, a: Date.parse(s.start), b: Date.parse(s.end) }))
      .filter((s) => Number.isFinite(s.a) && Number.isFinite(s.b) && s.b > s.a)
      .sort((x, y) => x.a - y.a)
    const first = segs[0]
    if (!first) return null
    const t0 = first.a
    const t1 = Math.max(...segs.map((s) => s.b))
    const span = t1 - t0
    const xOf = (t: number) => LABEL_W + ((t - t0) / span) * hypPlotW
    const yOfRow = (r: number) => r * (ROW_H + ROW_GAP)

    const bands = segs.map((s) => ({
      stage: s.stage,
      x: xOf(s.a),
      w: Math.max(1.5, xOf(s.b) - xOf(s.a)),
      y: yOfRow(rowOf(s.stage)),
    }))
    // A fine line from one band to the next where the stage changed.
    const links: { x: number; y1: number; y2: number }[] = []
    for (let i = 1; i < segs.length; i++) {
      const before = segs[i - 1]
      const here = segs[i]
      if (!before || !here) continue
      const r1 = rowOf(before.stage)
      const r2 = rowOf(here.stage)
      if (r1 !== r2 && here.a - before.b < 120_000) {
        links.push({
          x: xOf(here.a),
          y1: yOfRow(r1) + ROW_H / 2,
          y2: yOfRow(r2) + ROW_H / 2,
        })
      }
    }
    // Hour marks along the foot, thinned to what the width can label.
    const hours = span / 3_600_000
    const stepH = Math.max(1, Math.ceil(hours / Math.max(2, Math.floor(hypPlotW / 70))))
    const marks: { x: number; text: string }[] = []
    const at = new Date(t0)
    at.setMinutes(0, 0, 0)
    if (at.getTime() < t0) at.setHours(at.getHours() + 1)
    for (let t = at.getTime(); t <= t1; t += stepH * 3_600_000) {
      marks.push({ x: xOf(t), text: dateFormat({ hour: 'numeric' }).format(new Date(t)) })
    }
    return { bands, links, marks }
  })

  /** The night's stages in numbers, for the key under the hypnogram. */
  const stages = $derived(selected ? stack(selected) : [])

  // ── the score ──────────────────────────────────────────────────────────

  const PART_NAMES: Record<string, string> = {
    duration: 'Duration',
    efficiency: 'Efficiency',
    deep: 'Deep sleep',
    rem: 'REM sleep',
    stages: 'Deep and REM sleep',
    deepRem: 'Deep and REM sleep',
    deep_rem: 'Deep and REM sleep',
    regularity: 'Regular bed and wake times',
    consistency: 'Regular bed and wake times',
    timing: 'Regular bed and wake times',
    bedtime: 'Bedtime',
    wake: 'Wake time',
  }
  function partName(key: string): string {
    const known = PART_NAMES[key]
    if (known) return known
    const words = key
      .replace(/([a-z])([A-Z])/g, '$1 $2')
      .replace(/[_-]+/g, ' ')
      .trim()
      .toLowerCase()
    return words.charAt(0).toUpperCase() + words.slice(1)
  }
  const parts = $derived(
    Object.entries(selected?.scoreParts ?? {}).map(([key, value]) => ({
      key,
      name: partName(key),
      value,
    })),
  )

  const percent = (share: number) => `${Math.round(share * 100)}%`
  const heading = $derived(
    selected
      ? dateFormat({ weekday: 'long', day: 'numeric', month: 'long' }).format(dayOf(selected.date))
      : '',
  )
  const timesAsleep = $derived(
    selected?.sleepStart && selected.sleepEnd
      ? `${formatInstantTime(selected.sleepStart)} – ${formatInstantTime(selected.sleepEnd)}`
      : undefined,
  )
  const count = (v: number) => Math.round(v).toLocaleString(locale())
</script>

<div class="sleep scroll">
  <div class="page">
    <div class="tiles">
      <StatTile
        label="Asleep, a night on average"
        value={avgAsleep === null ? '—' : durationText(avgAsleep)}
        note={goalMin > 0 && avgAsleep !== null ? `Goal ${goalText}` : undefined}
      />
      <StatTile
        label="Efficiency"
        value={avgEfficiency === null ? '—' : percent(avgEfficiency)}
        note={avgEfficiency === null ? undefined : 'Time asleep of time in bed'}
      />
      <StatTile label="Sleep score" value={avgScore === null ? '—' : count(avgScore)} />
      <StatTile
        label="HRV while asleep"
        value={avgHrv === null ? '—' : health.display('heart_rate_variability', avgHrv)}
      />
    </div>

    <section class="card" aria-labelledby="hs-nights">
      <h2 class="eyebrow" id="hs-nights">Nights</h2>
      {#if withData.length === 0}
        <p class="quiet">No sleep is recorded in this window.</p>
      {:else}
        <div class="chart" bind:clientWidth={width}>
          <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
          <div
            class="stage"
            role="group"
            tabindex="0"
            aria-label="{n} nights, arrow keys move between them and Enter shows one"
            onpointermove={(e) => (active = indexAt(e))}
            onpointerleave={() => (active = null)}
            onclick={(e) => choose(indexAt(e))}
            onkeydown={key}
            onfocus={focused}
            onblur={() => (active = null)}
          >
            <svg
              viewBox="0 0 {W} {PAD_TOP + HEIGHT + AXIS}"
              width="100%"
              height={PAD_TOP + HEIGHT + AXIS}
              aria-hidden="true"
              focusable="false"
            >
              {#each ticks as t (t)}
                <line
                  x1={GUTTER}
                  x2={W}
                  y1={yOf(t)}
                  y2={yOf(t)}
                  class={t === 0 ? 'base' : 'grid'}
                />
                <text x={GUTTER - 6} y={yOf(t) + 4} text-anchor="end" class="t">{t / 60} h</text>
              {/each}

              {#if selected}
                {@const si = nights.findIndex((x) => x.date === selected.date)}
                {#if si >= 0}
                  <rect
                    x={GUTTER + si * slot}
                    y={PAD_TOP}
                    width={slot}
                    height={HEIGHT}
                    class="chosen"
                  />
                {/if}
              {/if}
              {#if active !== null}
                <rect
                  x={GUTTER + active * slot}
                  y={PAD_TOP}
                  width={slot}
                  height={HEIGHT}
                  class="hl"
                />
              {/if}

              {#each bars as bar (bar.i)}
                {#each bar.pieces as p (p.stage)}
                  {#if p.rounded}
                    <path d={p.d} style="fill: {SLEEP_STAGE_COLORS[p.stage]}" class="piece" />
                  {:else}
                    <rect
                      x={p.x}
                      y={p.y}
                      width={barW}
                      height={p.h}
                      style="fill: {SLEEP_STAGE_COLORS[p.stage]}"
                      class="piece"
                    />
                  {/if}
                {/each}
              {/each}

              {#if goalMin > 0}
                {@const gy = yOf(goalMin)}
                <line x1={GUTTER} x2={W} y1={gy} y2={gy} class="goal" />
                <text x={W} y={gy - 4} text-anchor="end" class="t goal-text">Goal {goalText}</text>
              {/if}

              {#each axisLabels as a (a.i)}
                <text x={a.x} y={PAD_TOP + HEIGHT + 15} text-anchor={a.anchor} class="t"
                  >{a.text}</text
                >
              {/each}
            </svg>

            {#if activeNight}
              <div class="tip" style="left: {tipX}px">{activeText}</div>
            {/if}
          </div>
          <span class="sr" aria-live="polite">{activeText}</span>
        </div>

        <ul class="key" aria-label="Sleep stages">
          {#each legend as stage (stage)}
            <li>
              <span class="swatch" style="background: {SLEEP_STAGE_COLORS[stage]}"></span>
              {stageName(stage)}
            </li>
          {/each}
        </ul>
        <p class="foot">Pick a night to see it hour by hour.</p>
      {/if}
    </section>

    {#if selected}
      <section class="card" aria-labelledby="hs-night">
        <div class="night-head">
          <h2 class="eyebrow" id="hs-night">{heading}</h2>
        </div>
        <div class="tiles">
          <StatTile label="Asleep" value={durationText(selected.asleepMin)} note={timesAsleep} />
          {#if selected.inBedMin > 0}
            <StatTile
              label="In bed"
              value={durationText(selected.inBedMin)}
              note={typeof selected.efficiency === 'number'
                ? `${percent(selected.efficiency)} of it asleep`
                : undefined}
            />
          {/if}
          {#if typeof selected.hr === 'number'}
            <StatTile label="Heart rate" value={health.display('heart_rate', selected.hr)} />
          {/if}
          {#if typeof selected.hrvMs === 'number'}
            <StatTile
              label="HRV"
              value={health.display('heart_rate_variability', selected.hrvMs)}
            />
          {/if}
          {#if typeof selected.respiratoryRate === 'number'}
            <StatTile
              label="Breathing"
              value={health.display('respiratory_rate', selected.respiratoryRate)}
            />
          {/if}
        </div>

        {#if hyp}
          <div class="hyp" bind:clientWidth={hypWidth}>
            <svg
              viewBox="0 0 {HW} {HYP_H + AXIS}"
              width="100%"
              height={HYP_H + AXIS}
              role="img"
              aria-label="Stage over the night: {stages
                .map((p) => `${stageName(p.stage)} ${durationText(p.min)}`)
                .join(', ')}"
            >
              {#each ROWS as stage, r (stage)}
                <text x="0" y={r * (ROW_H + ROW_GAP) + ROW_H / 2 + 4} class="t"
                  >{stageName(stage)}</text
                >
              {/each}
              {#each hyp.marks as m, i (i)}
                <line x1={m.x} x2={m.x} y1="0" y2={HYP_H} class="grid" />
                <text x={m.x} y={HYP_H + 15} text-anchor="middle" class="t">{m.text}</text>
              {/each}
              {#each hyp.links as l, i (i)}
                <line x1={l.x} x2={l.x} y1={l.y1} y2={l.y2} class="link" />
              {/each}
              {#each hyp.bands as b, i (i)}
                <rect
                  x={b.x}
                  y={b.y}
                  width={b.w}
                  height={ROW_H}
                  rx="3"
                  style="fill: {SLEEP_STAGE_COLORS[b.stage]}"
                />
              {/each}
            </svg>
          </div>
          <ul class="key">
            {#each stages as p (p.stage)}
              <li>
                <span class="swatch" style="background: {SLEEP_STAGE_COLORS[p.stage]}"></span>
                {stageName(p.stage)}
                <span class="amount">{durationText(p.min)}</span>
              </li>
            {/each}
          </ul>
        {:else}
          <p class="quiet">No stages were recorded for this night.</p>
        {/if}
      </section>

      {#if typeof selected.score === 'number'}
        <section class="card" aria-labelledby="hs-score">
          <h2 class="eyebrow" id="hs-score">How the score was worked out</h2>
          <StatTile
            label="Sleep score"
            value={count(selected.score)}
            note="This is a rule of thumb of this app's own, not a medical measure."
          />
          {#if parts.length > 0}
            <ul class="parts">
              {#each parts as part (part.key)}
                <li>
                  <span class="pname">{part.name}</span>
                  <span class="pbar" aria-hidden="true">
                    <span class="pfill" style="width: {Math.min(100, Math.max(0, part.value))}%"
                    ></span>
                  </span>
                  <span class="pval">{count(part.value)}</span>
                </li>
              {/each}
            </ul>
          {/if}
        </section>
      {/if}
    {/if}
  </div>
</div>

<style>
  .sleep {
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

  .tiles {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: var(--sp-4) var(--sp-8);
  }
  .page > .tiles {
    padding: var(--sp-2) 0;
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
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .foot {
    margin: 0;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }

  .chart,
  .hyp {
    position: relative;
    min-width: 0;
  }
  .stage {
    position: relative;
    cursor: pointer;
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
  .base {
    stroke: var(--border-strong);
    stroke-width: 1;
  }
  .grid {
    stroke: var(--border);
    stroke-width: 1;
    opacity: 0.7;
  }
  .link {
    stroke: var(--border-strong);
    stroke-width: 1;
  }
  .hl {
    fill: var(--bg-hover);
  }
  .chosen {
    fill: var(--bg-selected);
  }

  /* The surface between two stacked stages is the separator: a 1px stroke in
     the card's own colour, so nothing is outlined in a colour of its own. */
  .piece {
    stroke: var(--bg);
    stroke-width: 1;
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

  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }

  .key {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-1) var(--sp-4);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .key li {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .swatch {
    flex: none;
    width: 9px;
    height: 9px;
    border-radius: 2px;
    translate: 0 -1px;
  }
  .amount {
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  /* Each part of the score: its name, a thin bar, its number. The bar sits in
     a grid track, which is a real width for its fill's percentage. */
  .parts {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .parts li {
    display: grid;
    grid-template-columns: minmax(8em, 14em) minmax(0, 1fr) 2.5em;
    align-items: center;
    gap: var(--sp-3);
  }
  .pname {
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }
  .pbar {
    display: block;
    height: 4px;
    border-radius: 999px;
    background: color-mix(in oklab, var(--accent) 16%, transparent);
    overflow: hidden;
  }
  .pfill {
    display: block;
    height: 100%;
    border-radius: 999px;
    background: var(--accent);
  }
  .pval {
    text-align: right;
    color: var(--fg);
    font-size: var(--text-sm);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
</style>
