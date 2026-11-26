<script lang="ts">
  // Vitals: every measurement the body gives -- heart rate, weight, blood
  // pressure, glucose, temperature, oxygen -- one list to choose from and one
  // to look at.
  //
  // The list is grouped the way a person thinks of them (heart, body, vitals,
  // breathing, glucose) and shows only what has been measured, each with its
  // latest value and how long ago, plus which way it went since the reading
  // before. The arrow says direction and nothing else: a falling weight and a
  // falling heart rate are not the same news, and this is not the place that
  // decides which.
  //
  // The measurement chosen is a line over the window with its daily low and
  // high as a faint band behind. Blood pressure is two numbers read together,
  // so it is one row and two lines -- top and bottom -- on a shared time
  // axis, one above the other. Glucose carries the person's own range as a
  // shaded band.
  //
  // Readings can be added by hand, any measurement can be shown on the
  // Overview (it becomes a tracker, so a goal can be set on it), and the
  // readings in the window can be deleted -- which asks first, because a
  // phone cannot send that deletion back.

  import { health } from '../lib/health.svelte'
  import { dayInstants } from '../lib/health'
  import { plural, relativeTime } from '../lib/format'
  import { details, phone } from '../lib/phone.svelte'
  import type { Latest, Metric, MetricEntry, MetricGroup, SeriesResult } from '../lib/types'
  import HealthLine from './HealthLine.svelte'
  import Icon from './Icon.svelte'
  import PhoneBack from './PhoneBack.svelte'
  import StatTile from './StatTile.svelte'

  /** The Health app's own colour: every line here is a reading of the body. */
  const HEALTH = 'var(--app-health, var(--accent))'

  const GROUPS: { group: MetricGroup; title: string }[] = [
    { group: 'heart', title: 'Heart' },
    { group: 'body', title: 'Body' },
    { group: 'vitals', title: 'Vitals' },
    { group: 'respiratory', title: 'Breathing' },
    { group: 'metabolic', title: 'Glucose and insulin' },
  ]

  const SYS: Metric = 'blood_pressure_systolic'
  const DIA: Metric = 'blood_pressure_diastolic'

  /** Blood pressure is one row, led by its top number. */
  const metric = $derived(health.vitalsMetric === DIA ? SYS : health.vitalsMetric)
  const isPressure = $derived(metric === SYS)
  const entry = $derived(health.metricInfo(metric))
  const name = $derived(isPressure ? 'Blood pressure' : (entry?.name ?? metric))
  const unit = $derived(health.displayUnit(metric))

  const latestBy = $derived(new Map(health.latest.map((l) => [l.metric, l] as const)))
  const latest = $derived(latestBy.get(metric) ?? null)

  // ── words for a value ──────────────────────────────────────────────────

  const whole = (m: Metric, v: number) => Math.round(health.toDisplay(m, v))

  function pressure(sys: number, dia: number): string {
    return `${whole(SYS, sys)}/${whole(DIA, dia)} ${health.displayUnit(SYS)}`
  }

  /** A latest reading as text: a pressure's two numbers together. */
  function valueText(l: Latest): string {
    if (l.metric === SYS) {
      const dia = latestBy.get(DIA)
      if (dia) return pressure(l.value, dia.value)
    }
    return health.display(l.metric, l.value)
  }

  /** Which way it went since the reading before. */
  function arrow(l: Latest): { mark: string; words: string } | null {
    if (l.previous === null || l.previous === undefined) return null
    const dia = l.metric === SYS ? latestBy.get(DIA) : undefined
    const before =
      dia && dia.previous !== null && dia.previous !== undefined
        ? pressure(l.previous, dia.previous)
        : health.display(l.metric, l.previous)
    if (Math.abs(l.value - l.previous) <= Math.abs(l.previous) * 0.005) {
      return { mark: '→', words: `Steady at ${before}` }
    }
    return l.value > l.previous
      ? { mark: '↑', words: `Up from ${before}` }
      : { mark: '↓', words: `Down from ${before}` }
  }

  const latestNote = $derived.by(() => {
    if (!latest) return 'Nothing measured yet'
    const a = arrow(latest)
    return [a ? `${a.mark} ${a.words}` : '', relativeTime(latest.at)].filter(Boolean).join(' · ')
  })

  // ── the list ───────────────────────────────────────────────────────────

  interface Row {
    metric: Metric
    name: string
    value: string
    when: string
    mark: string
    words: string
  }

  function rowFor(m: MetricEntry, l: Latest | undefined): Row {
    const a = l ? arrow(l) : null
    return {
      metric: m.metric,
      name: m.metric === SYS ? 'Blood pressure' : m.name,
      value: l ? valueText(l) : 'No readings yet',
      when: l ? relativeTime(l.at) : '',
      mark: a?.mark ?? '',
      words: a?.words ?? '',
    }
  }

  const groups = $derived.by(() => {
    const out: { group: MetricGroup; title: string; rows: Row[] }[] = []
    for (const g of GROUPS) {
      const rows: Row[] = []
      for (const m of health.catalog) {
        if (m.group !== g.group || m.metric === DIA) continue
        const l = latestBy.get(m.metric)
        // What has been measured, and whatever is open even if nothing has.
        if (!l && m.metric !== metric) continue
        rows.push(rowFor(m, l))
      }
      if (rows.length > 0) out.push({ ...g, rows })
    }
    return out
  })

  // ── the chart ──────────────────────────────────────────────────────────

  /** Only a series that is the open measurement's: the last one may still be arriving. */
  const series = $derived(health.series && health.series.metric === metric ? health.series : null)

  // Blood pressure's bottom number is loaded beside the top one by the store.
  const second = $derived(
    isPressure && health.seriesSecond?.metric === DIA ? health.seriesSecond : null,
  )

  function toLine(s: SeriesResult | null, m: Metric) {
    return (s?.points ?? []).map((p) => ({
      at: p.at,
      value: health.toDisplay(m, p.value),
      min: p.min === null || p.min === undefined ? null : health.toDisplay(m, p.min),
      max: p.max === null || p.max === undefined ? null : health.toDisplay(m, p.max),
    }))
  }
  const line = $derived(toLine(series, metric))
  const lineSecond = $derived(toLine(second, DIA))

  /** Glucose is read against the person's own range, the consensus one if they have not set it. */
  const band = $derived(
    metric === 'blood_glucose'
      ? {
          low: health.toDisplay(metric, health.settings?.glucoseLow ?? 70),
          high: health.toDisplay(metric, health.settings?.glucoseHigh ?? 180),
        }
      : null,
  )

  function summarize(s: SeriesResult | null) {
    const points = s?.points ?? []
    if (points.length === 0) return null
    let sum = 0
    let lo = Infinity
    let hi = -Infinity
    let readings = 0
    for (const p of points) {
      sum += p.value
      lo = Math.min(lo, p.min ?? p.value)
      hi = Math.max(hi, p.max ?? p.value)
      readings += p.count
    }
    return { mean: sum / points.length, lo, hi, readings }
  }
  const stats = $derived(summarize(series))
  const statsSecond = $derived(summarize(second))

  // ── acts ───────────────────────────────────────────────────────────────

  /** Measurements put on the Overview in this visit, so the button says so. */
  let tracked = $state<Record<string, boolean>>({})
  let tracking = $state(false)

  async function track() {
    if (tracked[metric] || tracking) return
    tracking = true
    health.error = null
    try {
      await health.trackOnOverview(metric, { type: 'auto' }, name)
      if (!health.error) tracked[metric] = true
    } finally {
      tracking = false
    }
  }

  /** Asks first. The readings of the window on screen, whole days. */
  function removeRange(m: Metric, label: string) {
    const span = dayInstants(health.from, health.to)
    health.deleteSamples(m, span.from, span.to, label)
  }

  const hasReadings = $derived((series?.points.length ?? 0) > 0)

  // ── On a phone ───────────────────────────────────────────────────────
  //
  // The list and what it opens do not fit side by side, and one under the
  // other leaves a choice made at the top answered out of sight at the
  // bottom. So the list is a page, and the measurement chosen from it is a
  // page of its own over it, with a way back (`details`, as the journal's
  // entry has: back closes it, and so does choosing Vitals again in the
  // drawer). It is the list again whenever this pane is left and come back to.
  const showList = $derived(!phone.on || !details.isOpen('health'))
  const showDetail = $derived(!phone.on || details.isOpen('health'))

  $effect(() => () => details.close('health'))

  function choose(m: Metric) {
    health.setVitalsMetric(m)
    details.open('health')
  }
</script>

<div class="vitals scroll">
  <div class="page">
    {#if groups.length === 0}
      <section class="card">
        <p class="quiet">
          Nothing has been measured yet. Readings arrive from Apple Health once it is connected, or
          you can add one by hand.
        </p>
        <div>
          <button class="btn btn-primary" onclick={() => (health.readingOpen = true)}>
            Add a reading
          </button>
        </div>
      </section>
    {:else}
      <div class="cols">
        {#if showList}
          <nav class="metrics" aria-label="Measurements">
            {#each groups as g (g.group)}
              <h2 class="eyebrow group">{g.title}</h2>
              {#each g.rows as r (r.metric)}
                <button
                  class="list-row metric"
                  class:sel={r.metric === metric}
                  aria-current={r.metric === metric ? 'true' : undefined}
                  onclick={() => choose(r.metric)}
                >
                  <span class="mname">{r.name}</span>
                  <span class="mval">
                    {r.value}
                    {#if r.mark}<span class="mark" title={r.words}>{r.mark}</span>{/if}
                  </span>
                  <span class="mwhen">{r.when}</span>
                </button>
              {/each}
            {/each}
          </nav>
        {/if}

        {#if showDetail}
          <section class="detail" aria-label={name}>
            <div class="head">
              <PhoneBack onclick={() => details.close('health')} label="Measurements" />
              <h2 class="title">{name}</h2>
              <div class="acts">
                <button class="btn btn-sm btn-primary" onclick={() => (health.readingOpen = true)}>
                  <Icon name="plus" size={13} />
                  Add reading
                </button>
                <button
                  class="btn btn-sm btn-outline"
                  disabled={tracked[metric] || tracking}
                  onclick={() => void track()}
                >
                  {tracked[metric] ? 'On the Overview' : 'Show on Overview'}
                </button>
              </div>
            </div>

            <div class="tiles">
              <StatTile label="Latest" value={latest ? valueText(latest) : '—'} note={latestNote} />
              {#if stats}
                {#if isPressure && statsSecond}
                  <StatTile
                    label="Average"
                    value={pressure(stats.mean, statsSecond.mean)}
                    note="Of the daily values"
                  />
                {:else if !isPressure}
                  <StatTile
                    label="Average"
                    value={health.display(metric, stats.mean)}
                    note="Of the daily values"
                  />
                {/if}
                {#if !isPressure}
                  <StatTile label="Lowest" value={health.display(metric, stats.lo)} />
                  <StatTile label="Highest" value={health.display(metric, stats.hi)} />
                {/if}
                <StatTile
                  label="Readings"
                  value={String(stats.readings)}
                  note={health.rangeLabel}
                />
              {/if}
            </div>

            {#if isPressure}
              <section class="card" aria-labelledby="hv-top">
                <h2 class="eyebrow" id="hv-top">Top number (systolic), {health.rangeLabel}</h2>
                <HealthLine series={line} {unit} color={HEALTH} from={health.from} to={health.to} />
              </section>
              <section class="card" aria-labelledby="hv-bottom">
                <h2 class="eyebrow" id="hv-bottom">
                  Bottom number (diastolic), {health.rangeLabel}
                </h2>
                <HealthLine
                  series={lineSecond}
                  {unit}
                  color="var(--fg-muted)"
                  from={health.from}
                  to={health.to}
                />
              </section>
            {:else}
              <section class="card" aria-labelledby="hv-line">
                <h2 class="eyebrow" id="hv-line">{name}, {health.rangeLabel}</h2>
                <HealthLine
                  series={line}
                  {band}
                  {unit}
                  color={HEALTH}
                  from={health.from}
                  to={health.to}
                />
                {#if band}
                  <p class="foot">The shaded band is your glucose range.</p>
                {:else if stats && line.some((p) => p.min !== p.value || p.max !== p.value)}
                  <p class="foot">
                    The faint band behind the line is each day's lowest to highest.
                  </p>
                {/if}
              </section>
            {/if}

            {#if hasReadings}
              <div class="removal">
                {#if isPressure}
                  <button
                    class="btn btn-sm btn-ghost-danger"
                    onclick={() => removeRange(SYS, 'top-number blood pressure')}
                  >
                    Delete top numbers in {health.rangeLabel}…
                  </button>
                  <button
                    class="btn btn-sm btn-ghost-danger"
                    onclick={() => removeRange(DIA, 'bottom-number blood pressure')}
                  >
                    Delete bottom numbers in {health.rangeLabel}…
                  </button>
                {:else}
                  <button
                    class="btn btn-sm btn-ghost-danger"
                    onclick={() => removeRange(metric, name.toLowerCase())}
                  >
                    Delete {plural(stats?.readings ?? 0, 'reading')} in {health.rangeLabel}…
                  </button>
                {/if}
              </div>
            {/if}
          </section>
        {/if}
      </div>
    {/if}
  </div>
</div>

<style>
  .vitals {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .page {
    max-width: 1080px;
    margin: 0 auto;
    padding: var(--sp-2) var(--sp-6) var(--fab-clear);
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
  }

  /* The list beside what it opens: a column that stays its own width, and a
     detail that takes the rest. They wrap, list first, when the pane is
     narrow. */
  .cols {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-start;
    gap: var(--sp-5);
  }
  .metrics {
    flex: 0 0 260px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .detail {
    flex: 1 1 420px;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }

  .group {
    padding: var(--sp-4) var(--sp-3) var(--sp-1);
  }
  .group:first-child {
    padding-top: 0;
  }

  /* One row: its name over its value, and when it was measured at the end.
     The shared `.list-row` insets itself from its column's edges, which is
     what this is. */
  .metric {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    column-gap: var(--sp-3);
    row-gap: 1px;
    padding: var(--sp-2) var(--sp-3);
  }
  .mname {
    grid-column: 1 / -1;
    color: var(--fg);
    font-size: var(--text-base);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .mval {
    color: var(--fg-muted);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
  }
  .mark {
    margin-left: var(--sp-1);
    color: var(--fg-subtle);
  }
  .mwhen {
    justify-self: end;
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }
  .metric.sel .mname {
    color: var(--accent);
    font-weight: 620;
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: var(--sp-3);
  }
  .title {
    font-size: var(--text-xl);
    font-weight: 650;
    letter-spacing: -0.015em;
    line-height: var(--leading-tight);
  }
  .acts {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }

  .tiles {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: var(--sp-4) var(--sp-8);
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
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }
  .foot {
    margin: 0;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }

  .removal {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }

  /* A phone: the list is a page (see `details`), each measurement a row
     a finger can hit, and the one chosen a page with its way back at the
     head. */
  :global([data-phone]) .cols {
    display: block;
  }
  :global([data-phone]) .metrics {
    width: 100%;
  }
  :global([data-phone]) .metric {
    min-height: 56px;
  }
  :global([data-phone]) .head {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    align-items: center;
    gap: var(--sp-1) var(--sp-2);
  }
  :global([data-phone]) .head .acts {
    grid-column: 1 / -1;
  }
  :global([data-phone]) .title {
    font-size: var(--text-lg);
  }
</style>
