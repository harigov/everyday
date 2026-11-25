<script lang="ts">
  // One condition, read at a glance: how its targets stand, which tests are
  // due, which medicines belong to it and how steadily they are taken, and
  // anything the timing rules noticed.
  //
  // For diabetes there is a glucose block -- time in range, the standard
  // summary figures, the typical day and the lows and highs -- and the day
  // overlay beside meals and doses. For an underactive thyroid, TSH and free
  // T4 over time against each report's own reference range, with every
  // change of dose marked on the line.
  //
  // It reports; it never advises. Targets are the person's to set with their
  // clinician (the plan came from a template, and they edited it), "not met"
  // is a plain fact about a number rather than a verdict, and the status
  // words always come with a mark so nothing depends on telling two colours
  // apart.

  import { health } from '../lib/health.svelte'
  import {
    durationText,
    GLUCOSE_COLORS,
    labKey,
    scheduleText,
    type GlucoseBand,
  } from '../lib/health'
  import { dateFormat, formatInstantTime, formatMinutes } from '../lib/format'
  import { addDays, minutesBetween, startOfDay, todayIso } from '../lib/time'
  import type {
    CareTarget,
    Condition,
    ConditionStatus,
    LabResult,
    LabTrend,
    TargetStanding,
  } from '../lib/types'
  import HealthAgp from './HealthAgp.svelte'
  import HealthGlucoseDay from './HealthGlucoseDay.svelte'
  import HealthLine from './HealthLine.svelte'
  import HealthVisitBrief from './HealthVisitBrief.svelte'
  import Icon from './Icon.svelte'
  import Meter from './Meter.svelte'
  import StatTile from './StatTile.svelte'
  import { steadiness } from '../lib/health-doses'

  const STATUSES: { value: ConditionStatus; label: string }[] = [
    { value: 'active', label: 'Active' },
    { value: 'remission', label: 'In remission' },
    { value: 'resolved', label: 'Resolved' },
  ]
  const accent = 'var(--app-health, var(--accent))'

  const report = $derived(
    health.conditionReport && health.conditionReport.condition.id === health.conditionId
      ? health.conditionReport
      : null,
  )
  const condition = $derived<Condition | null>(health.condition ?? report?.condition ?? null)

  const niceDate = (iso: string) =>
    dateFormat({ day: 'numeric', month: 'short', year: 'numeric' }).format(startOfDay(iso))
  const niceDay = (iso: string) =>
    dateFormat({ day: 'numeric', month: 'short' }).format(startOfDay(iso))

  // ── The condition itself ────────────────────────────────────────────────

  let editing = $state(false)
  let name = $state('')
  let status = $state<ConditionStatus>('active')
  let diagnosed = $state('')
  let notes = $state('')
  let formError = $state('')
  let saving = $state(false)
  let briefOpen = $state(false)

  function startEdit() {
    if (!condition) return
    name = condition.name
    status = condition.status
    diagnosed = condition.diagnosed ?? ''
    notes = condition.notes
    formError = ''
    editing = true
  }

  async function saveEdit() {
    if (!condition) return
    if (!name.trim()) {
      formError = 'Give the condition a name.'
      return
    }
    saving = true
    try {
      const saved = await health.saveCondition({
        ...$state.snapshot(condition),
        name: name.trim(),
        status,
        diagnosed: diagnosed || null,
        notes: notes.trim(),
        updatedAt: new Date().toISOString(),
      })
      if (saved) editing = false
      else formError = 'It could not be saved. Try again.'
    } finally {
      saving = false
    }
  }

  function remove() {
    if (condition) health.deleteCondition(condition)
  }

  const statusName = (s: ConditionStatus) => STATUSES.find((x) => x.value === s)?.label ?? s

  // ── Lab data, from the trends if the store holds them, else the results ─

  function groupLabs(labs: LabResult[]): LabTrend[] {
    const map = new Map<string, LabTrend>()
    for (const l of labs) {
      const key = labKey(l)
      let trend = map.get(key)
      if (!trend) {
        trend = { key, name: l.name, unit: l.unit, results: [] }
        map.set(key, trend)
      }
      trend.results.push(l)
    }
    return [...map.values()]
  }
  const trends = $derived(health.labTrends.length > 0 ? health.labTrends : groupLabs(health.labs))
  const labUnit = (code: string) => trends.find((t) => t.key === code)?.unit ?? ''

  function findTrend(code: string, words: string[]): LabTrend | undefined {
    return (
      trends.find((t) => t.key === code) ??
      trends.find((t) => words.some((w) => t.name.toLowerCase().includes(w)))
    )
  }

  // ── Targets ─────────────────────────────────────────────────────────────

  function fmtValue(t: CareTarget, v: number): string {
    const m = t.measure
    if (m.type === 'metric') {
      // A time-in-range target and its standing are shares, 0..=1.
      return m.stat.type === 'inRange' ? `${Math.round(v * 100)} %` : health.display(m.metric, v)
    }
    const unit = labUnit(m.code)
    return `${Math.round(v * 100) / 100}${unit ? ` ${unit}` : ''}`
  }
  function targetText(t: CareTarget): string {
    if (t.useReferenceRange) return 'within the lab’s own reference range'
    const lo = t.low != null ? fmtValue(t, t.low) : null
    const hi = t.high != null ? fmtValue(t, t.high) : null
    if (lo && hi) return `${lo} to ${hi}`
    if (lo) return `above ${lo}`
    if (hi) return `below ${hi}`
    return 'no range set'
  }
  function standingText(s: TargetStanding): string {
    if (s.value == null) return 'No reading yet'
    const when = s.at ? ` on ${niceDay(s.at.slice(0, 10))}` : ''
    return `${fmtValue(s.target, s.value)}${when}`
  }
  const STATE_WORDS: Record<string, string> = {
    met: 'Met',
    notMet: 'Not met',
    unknown: 'No data yet',
  }

  // ── Tests due ───────────────────────────────────────────────────────────

  const labsDue = $derived(
    [...(report?.labsDue ?? [])].sort((a, b) => {
      if (a.overdue !== b.overdue) return a.overdue ? -1 : 1
      return (a.due ?? '9999').localeCompare(b.due ?? '9999')
    }),
  )

  // ── Medicines ───────────────────────────────────────────────────────────

  const span = $derived({ from: addDays(todayIso(), -30), to: addDays(todayIso(), -1) })

  // ── Which blocks this condition has ─────────────────────────────────────

  const isDiabetes = $derived(
    !!condition &&
      (condition.template === 'type1Diabetes' ||
        condition.template === 'type2Diabetes' ||
        condition.plan.watch.some((m) => m.type === 'metric' && m.metric === 'blood_glucose')),
  )
  const isThyroid = $derived(
    !!condition &&
      (condition.template === 'hypothyroidism' ||
        condition.plan.labs.some((l) => l.code === '3016-3') ||
        condition.plan.targets.some(
          (t) => t.measure.type === 'lab' && t.measure.code === '3016-3',
        )),
  )

  // ── Glucose ─────────────────────────────────────────────────────────────

  const g = $derived(health.glucose)
  const BANDS: {
    band: GlucoseBand
    key: 'veryHigh' | 'high' | 'inRange' | 'low' | 'veryLow'
    name: string
    range: string
  }[] = [
    { band: 'veryHigh', key: 'veryHigh', name: 'Very high', range: 'above 250' },
    { band: 'high', key: 'high', name: 'High', range: '181 to 250' },
    { band: 'inRange', key: 'inRange', name: 'In range', range: '70 to 180' },
    { band: 'low', key: 'low', name: 'Low', range: '54 to 69' },
    { band: 'veryLow', key: 'veryLow', name: 'Very low', range: 'below 54' },
  ]
  /** A threshold as the person reads it, from the fixed mg/dL range text. */
  function rangeText(range: string): string {
    return range.replace(/\d+/g, (n) => {
      const shown = health.toDisplay('blood_glucose', Number(n))
      return health.displayUnit('blood_glucose') === 'mmol/L'
        ? shown.toFixed(1)
        : String(Math.round(shown))
    })
  }
  const glucoseUnit = $derived(health.displayUnit('blood_glucose'))
  /** The bar reads low to high, left to right. */
  const tirOrder = $derived([...BANDS].reverse())
  const provisional = $derived(!!g && g.coverage < 0.7)
  /** The bar's one-line reading, for a screen reader. */
  const tirText = $derived(
    g
      ? `Time in range: ${tirOrder.map((b) => `${b.name} ${Math.round(g[b.key] * 100)} percent`).join(', ')}`
      : '',
  )
  const events = $derived(
    [...(g?.events ?? [])].sort((a, b) => Date.parse(b.start) - Date.parse(a.start)),
  )
  let allEvents = $state(false)
  const shownEvents = $derived(allEvents ? events : events.slice(0, 8))
  const EVENT_NAMES: Record<string, string> = {
    veryLow: 'Very low',
    low: 'Low',
    high: 'High',
    veryHigh: 'Very high',
  }

  // ── Thyroid ─────────────────────────────────────────────────────────────

  const tsh = $derived(findTrend('3016-3', ['tsh', 'thyroid stimulating']))
  const ft4 = $derived(findTrend('3024-7', ['free t4', 'ft4', 'free thyroxine']))

  /** Every change of dose of a medicine tagged thyroid, as a mark on the line. */
  const doseMarks = $derived(
    health.medications
      .filter((m) => m.tags.includes('thyroid'))
      .flatMap((m) =>
        m.history.map((h) => ({
          at: startOfDay(h.on).toISOString(),
          label: `${m.name} ${h.strength || `${h.dose}${m.doseUnit ? ` ${m.doseUnit}` : ''}`}`,
        })),
      ),
  )

  interface Plotted {
    trend: LabTrend
    series: { at: string; value: number }[]
    band: { low: number; high: number } | null
    from: string
    to: string
    last: LabResult | null
  }
  function plot(trend: LabTrend | undefined): Plotted | null {
    if (!trend) return null
    const results = trend.results
      .filter((r) => r.value != null)
      .sort((a, b) => a.collected.localeCompare(b.collected))
    if (results.length === 0) return null
    const series = results.map((r) => ({
      at: r.collectedAt ?? startOfDay(r.collected).toISOString(),
      value: r.value ?? 0,
    }))
    const last = results[results.length - 1] ?? null
    const withRange = [...results].reverse().find((r) => r.refLow != null && r.refHigh != null)
    const stamps = [
      ...series.map((p) => Date.parse(p.at)),
      ...doseMarks.map((m) => Date.parse(m.at)),
    ]
    return {
      trend,
      series,
      band:
        withRange && withRange.refLow != null && withRange.refHigh != null
          ? { low: withRange.refLow, high: withRange.refHigh }
          : null,
      from: new Date(Math.min(...stamps)).toISOString(),
      to: new Date(Math.max(...stamps, Date.now())).toISOString(),
      last,
    }
  }
  const tshPlot = $derived(plot(tsh))
  const ft4Plot = $derived(plot(ft4))
  const thyroidPlots = $derived(
    [
      { name: 'TSH', plotted: tshPlot },
      { name: 'Free T4', plotted: ft4Plot },
    ].filter((p): p is { name: string; plotted: Plotted } => p.plotted !== null),
  )
</script>

{#if !condition}
  <p class="quiet">Loading…</p>
{:else}
  <div class="dash">
    <!-- The condition -->
    <section class="card head" aria-labelledby="cd-name">
      {#if editing}
        <form
          class="edit"
          onsubmit={(e) => {
            e.preventDefault()
            void saveEdit()
          }}
        >
          <div class="grid">
            <div class="span2">
              <label class="field-label" for="cd-edit-name">Name</label>
              <input id="cd-edit-name" class="field" bind:value={name} autocomplete="off" />
            </div>
            <div>
              <label class="field-label" for="cd-edit-status">Status</label>
              <select id="cd-edit-status" class="field" bind:value={status}>
                {#each STATUSES as s (s.value)}<option value={s.value}>{s.label}</option>{/each}
              </select>
            </div>
            <div>
              <label class="field-label" for="cd-edit-diagnosed">Diagnosed</label>
              <input
                id="cd-edit-diagnosed"
                class="field"
                type="date"
                max={todayIso()}
                bind:value={diagnosed}
              />
            </div>
            <div class="span2">
              <label class="field-label" for="cd-edit-notes">Notes</label>
              <textarea id="cd-edit-notes" class="field" rows="3" bind:value={notes}></textarea>
            </div>
          </div>
          {#if formError}<p class="error" role="alert">{formError}</p>{/if}
          <div class="row">
            <span class="grow"></span>
            <button type="button" class="btn" onclick={() => (editing = false)}>Cancel</button>
            <button type="submit" class="btn btn-primary" disabled={saving}>Save</button>
          </div>
        </form>
      {:else}
        <div class="title-row">
          <div class="titles">
            <h2 class="cond-name" id="cd-name">{condition.name}</h2>
            <p class="cond-sub">
              {statusName(condition.status)}{condition.diagnosed
                ? ` · diagnosed ${niceDate(condition.diagnosed)}`
                : ''}
            </p>
          </div>
          <div class="actions">
            <button class="btn btn-primary" onclick={() => (briefOpen = true)}>
              Prepare for a visit
            </button>
            <button
              class="icon-btn"
              aria-label="Edit this condition"
              title="Edit"
              onclick={startEdit}
            >
              <Icon name="pencil" size={15} />
            </button>
            <button
              class="icon-btn danger"
              aria-label="Delete this condition"
              title="Delete"
              onclick={remove}
            >
              <Icon name="trash" size={15} />
            </button>
          </div>
        </div>
        {#if condition.notes}<p class="cond-notes">{condition.notes}</p>{/if}
      {/if}
    </section>

    <!-- Targets -->
    <section class="card" aria-labelledby="cd-targets">
      <h2 class="eyebrow" id="cd-targets">Targets</h2>
      {#if !report}
        <p class="quiet">Reading where things stand…</p>
      {:else if report.targets.length === 0}
        <p class="quiet">
          No targets are set for this condition. Targets are yours to set with your clinician.
        </p>
      {:else}
        <ul class="targets">
          {#each report.targets as s, i (i)}
            <li class="target">
              <span class="state {s.state}">
                <Icon
                  name={s.state === 'met' ? 'tick' : s.state === 'notMet' ? 'alert' : 'info'}
                  size={14}
                />
                {STATE_WORDS[s.state] ?? s.state}
              </span>
              <span class="t-main">
                <span class="t-label">{s.target.label}</span>
                <span class="t-sub">
                  {standingText(s)} · target {targetText(s.target)}
                </span>
                {#if s.target.note}<span class="t-note">{s.target.note}</span>{/if}
              </span>
            </li>
          {/each}
        </ul>
        <p class="foot">
          Targets are yours to set with your clinician. This is not medical advice.
        </p>
      {/if}
    </section>

    <!-- Tests -->
    {#if report && labsDue.length > 0}
      <section class="card" aria-labelledby="cd-tests">
        <h2 class="eyebrow" id="cd-tests">Tests</h2>
        <ul class="tests">
          {#each labsDue as d, i (i)}
            <li class="test">
              <span class="test-main">
                <span class="t-label">{d.schedule.name}</span>
                <span class="t-sub">
                  {d.last ? `Last done ${niceDate(d.last)}` : 'Not done yet'}{d.reason
                    ? ` · ${d.reason}`
                    : ''}
                </span>
              </span>
              <span class="due" class:overdue={d.overdue}>
                {#if d.overdue}<Icon name="alert" size={13} />{/if}
                {d.overdue ? 'Overdue' : d.due ? `Due ${niceDate(d.due)}` : 'No date'}
              </span>
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    <!-- Medicines -->
    {#if report}
      <section class="card" aria-labelledby="cd-meds">
        <h2 class="eyebrow" id="cd-meds">Medicines</h2>
        {#if report.medications.length === 0}
          <p class="quiet">
            No medicines are linked to this condition. Link them from the medication’s own page.
          </p>
        {:else}
          <ul class="meds">
            {#each report.medications as m (m.id)}
              {@const steady = steadiness(m, health.doses, span.from, span.to)}
              <li class="med">
                <span class="med-main">
                  <span class="t-label">
                    {m.name}{#if m.strength}<span class="t-strength">&nbsp;{m.strength}</span>{/if}
                  </span>
                  <span class="t-sub">{scheduleText(m.schedule)}</span>
                </span>
                {#if m.status !== 'active'}
                  <span class="tag">{m.status === 'paused' ? 'Paused' : 'Stopped'}</span>
                {:else if steady.share !== null}
                  <span class="med-meter">
                    <Meter
                      label="Last 30 days"
                      value={steady.share}
                      color={accent}
                      note="{steady.taken} of {steady.expected} doses"
                    />
                  </span>
                {/if}
              </li>
            {/each}
          </ul>
          {#if report.adherence.due > 0}
            <p class="foot">
              Recently, {report.adherence.taken} of {report.adherence.due} due doses were taken.
            </p>
          {/if}
        {/if}
      </section>

      {#if report.alerts.length > 0}
        <section class="card" aria-labelledby="cd-alerts">
          <h2 class="eyebrow" id="cd-alerts">Timing notes</h2>
          <ul class="notes">
            {#each report.alerts as a (a.medicationId + a.at + a.message)}
              <li>
                <Icon name="info" size={14} />
                <span class="t-main">
                  <span class="t-label plain">{a.message}</span>
                  <span class="t-sub">
                    {niceDay(a.at.slice(0, 10))}, {formatInstantTime(a.at)}
                  </span>
                </span>
              </li>
            {/each}
          </ul>
          <p class="foot">
            These are notes after the fact, not alarms. Targets and rules are yours to set with your
            clinician. This is not medical advice.
          </p>
        </section>
      {/if}
    {/if}

    <!-- Glucose -->
    {#if isDiabetes}
      <section class="card" aria-labelledby="cd-glucose">
        <h2 class="eyebrow" id="cd-glucose">
          Glucose{g?.from && g?.to ? `, ${niceDay(g.from)} to ${niceDay(g.to)}` : ''}
        </h2>
        {#if !g || g.readings === 0}
          <p class="quiet">
            No glucose readings in this window yet. They arrive from a sensor through a connected
            source, or can be added by hand under Vitals.
          </p>
        {:else}
          <div class="tiles">
            <StatTile
              label="Time in range"
              value="{Math.round(g.inRange * 100)} %"
              note="{rangeText('70 to 180')} {glucoseUnit}"
            />
            {#if g.gmi != null}
              <StatTile
                label="Glucose management indicator"
                value="{g.gmi.toFixed(1)} %"
                note="An estimate of A1c from the average"
              />
            {/if}
            {#if g.mean != null}
              <StatTile
                label="Average"
                value={health.display('blood_glucose', g.mean)}
                note="Across {g.readings.toLocaleString()} readings"
              />
            {/if}
            {#if g.cv != null}
              <StatTile
                label="Variability"
                value="{Math.round(g.cv)} %"
                note="How much it swings around the average"
              />
            {/if}
            <StatTile
              label="Sensor coverage"
              value="{Math.round(g.coverage * 100)} %"
              note={provisional
                ? 'Under 70% of the time was recorded, so these numbers are provisional.'
                : 'Of the window, with a reading'}
              warn={provisional}
            />
          </div>

          <!-- Time in range: one bar, low to high, and its key -->
          <div class="tir" role="img" aria-label={tirText}>
            {#each tirOrder as b (b.key)}
              {#if g[b.key] > 0}
                <span
                  class="seg"
                  style="flex: {g[b.key]}; background: {GLUCOSE_COLORS[b.band]}"
                  title="{b.name} ({rangeText(b.range)} {glucoseUnit}): {Math.round(
                    g[b.key] * 100,
                  )}%"
                ></span>
              {/if}
            {/each}
          </div>
          <ul class="tir-key">
            {#each BANDS as b (b.key)}
              <li>
                <span class="swatch" style="background: {GLUCOSE_COLORS[b.band]}"></span>
                <span class="tir-name">{b.name}</span>
                <span class="tir-range">{rangeText(b.range)} {glucoseUnit}</span>
                <span class="tir-pct">{Math.round(g[b.key] * 100)}%</span>
                <span class="tir-time">{durationText(g[b.key] * 1440)} a day</span>
              </li>
            {/each}
          </ul>
        {/if}
      </section>

      {#if g && g.readings > 0}
        <section class="card" aria-labelledby="cd-agp">
          <h2 class="eyebrow" id="cd-agp">A typical day</h2>
          <HealthAgp report={g} />
        </section>

        {#if events.length > 0}
          <section class="card" aria-labelledby="cd-events">
            <h2 class="eyebrow" id="cd-events">Lows and highs</h2>
            <ul class="events">
              {#each shownEvents as e (e.start)}
                <li>
                  <span class="ev-kind">{EVENT_NAMES[e.kind] ?? e.kind}</span>
                  <span class="ev-value">{health.display('blood_glucose', e.extreme)}</span>
                  <span class="ev-when">
                    {niceDay(e.start.slice(0, 10))}, {formatInstantTime(e.start)}
                  </span>
                  <span class="ev-for">for {formatMinutes(minutesBetween(e.start, e.end))}</span>
                </li>
              {/each}
            </ul>
            {#if events.length > 8}
              <button class="link" onclick={() => (allEvents = !allEvents)}>
                {allEvents ? 'Show fewer' : `Show all ${events.length}`}
              </button>
            {/if}
          </section>
        {/if}
      {/if}

      <section class="card" aria-labelledby="cd-day">
        <h2 class="eyebrow" id="cd-day">A day, with what shaped it</h2>
        <HealthGlucoseDay />
      </section>
    {/if}

    <!-- Thyroid -->
    {#if isThyroid}
      <section class="card" aria-labelledby="cd-thyroid">
        <h2 class="eyebrow" id="cd-thyroid">Thyroid tests over time</h2>
        {#if thyroidPlots.length === 0}
          <p class="quiet">
            No TSH or free T4 results yet. Add them from a lab report under Records and they will
            appear here, against each report’s own reference range.
          </p>
        {:else}
          {#each thyroidPlots as p (p.name)}
            <div class="plot">
              <p class="plot-head">
                <span class="plot-name">{p.name}</span>
                {#if p.plotted.last && p.plotted.last.value != null}
                  <span class="plot-last">
                    {p.plotted.last.value}
                    {p.plotted.trend.unit} on {niceDate(p.plotted.last.collected)}
                  </span>
                {/if}
              </p>
              <HealthLine
                series={p.plotted.series}
                band={p.plotted.band}
                unit={p.plotted.trend.unit}
                color={accent}
                height={150}
                dots
                markers={doseMarks}
                from={p.plotted.from}
                to={p.plotted.to}
              />
            </div>
          {/each}
          <p class="foot">
            The shaded band is the reference range on the latest report.
            {#if doseMarks.length > 0}Vertical marks are changes of dose.{/if}
          </p>
        {/if}
      </section>
    {/if}
  </div>
{/if}

{#if briefOpen && condition}
  <HealthVisitBrief conditionId={condition.id} onclose={() => (briefOpen = false)} />
{/if}

<style>
  .dash {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    min-width: 0;
  }
  .quiet,
  .foot {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }
  .foot {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .link {
    align-self: flex-start;
    color: var(--accent);
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .link:hover {
    text-decoration: underline;
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

  /* The condition */
  .title-row {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: var(--sp-3);
  }
  .titles {
    min-width: 0;
  }
  .cond-name {
    font-size: var(--text-xl);
    font-weight: 680;
    letter-spacing: -0.015em;
    line-height: var(--leading-tight);
    color: var(--fg);
  }
  .cond-sub {
    margin: 2px 0 0;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .cond-notes {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-muted);
    white-space: pre-wrap;
    user-select: text;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
  }
  .icon-btn {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .icon-btn:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .icon-btn.danger:hover {
    background: color-mix(in oklab, var(--danger) 12%, transparent);
    color: var(--danger);
  }
  .edit {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--sp-3);
  }
  .span2 {
    grid-column: 1 / -1;
  }
  .field-label {
    margin-bottom: var(--sp-1);
  }
  textarea.field {
    resize: vertical;
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .grow {
    flex: 1;
  }

  /* Rows of targets, tests and medicines */
  .targets,
  .tests,
  .meds,
  .notes,
  .events {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .target,
  .test,
  .med,
  .notes li {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    padding: var(--sp-2) 0;
  }
  .target + .target,
  .test + .test,
  .med + .med,
  .notes li + li {
    border-top: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }
  .t-main,
  .test-main,
  .med-main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .t-label {
    font-size: var(--text-base);
    font-weight: 600;
    color: var(--fg);
  }
  .t-label.plain {
    font-weight: 450;
    font-size: var(--text-sm);
  }
  .t-strength {
    font-weight: 450;
    color: var(--fg-muted);
  }
  .t-sub {
    font-size: var(--text-sm);
    color: var(--fg-muted);
    font-variant-numeric: tabular-nums;
  }
  .t-note {
    font-size: var(--text-xs);
    line-height: var(--leading-snug);
    color: var(--fg-subtle);
  }

  /* A state is a word and a mark; the colour only agrees with them. */
  .state {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    width: 8.5em;
    padding-top: 2px;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg);
  }
  .state :global(svg) {
    flex: none;
  }
  .state.met :global(svg) {
    color: var(--success);
  }
  .state.notMet :global(svg) {
    color: var(--warning);
  }
  .state.unknown {
    font-weight: 500;
    color: var(--fg-muted);
  }
  .state.unknown :global(svg) {
    color: var(--fg-faint);
  }

  .due {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding-top: 2px;
    font-size: var(--text-sm);
    color: var(--fg-muted);
    font-variant-numeric: tabular-nums;
  }
  .due.overdue {
    font-weight: 650;
    color: var(--danger);
  }

  .med {
    align-items: center;
  }
  .med-meter {
    flex: none;
    width: 14rem;
  }

  .notes li {
    align-items: flex-start;
    color: var(--fg-muted);
  }
  .notes li :global(svg) {
    flex: none;
    margin-top: 3px;
  }

  /* Glucose */
  .tiles {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-4) var(--sp-8);
    padding: var(--sp-1) 0;
  }
  .tiles > :global(*) {
    max-width: 16rem;
  }

  /* A real width for the segments' flex shares to resolve against. */
  .tir {
    display: flex;
    gap: 2px;
    width: 100%;
    height: 14px;
    border-radius: 999px;
    overflow: hidden;
  }
  .seg {
    min-width: 3px;
  }
  .tir-key {
    display: grid;
    gap: var(--sp-1);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .tir-key li {
    display: grid;
    grid-template-columns: auto 5.5em 1fr auto 7.5em;
    align-items: baseline;
    gap: var(--sp-2);
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .swatch {
    width: 9px;
    height: 9px;
    border-radius: 2px;
    translate: 0 -1px;
  }
  .tir-pct {
    color: var(--fg);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    text-align: right;
  }
  .tir-range,
  .tir-time {
    font-size: var(--text-xs);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }
  .tir-time {
    text-align: right;
  }

  .events li {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: var(--sp-1) var(--sp-3);
    padding: 4px 0;
    font-size: var(--text-sm);
  }
  .ev-kind {
    width: 5.5em;
    font-weight: 600;
    color: var(--fg);
  }
  .ev-value {
    min-width: 6em;
    color: var(--fg);
    font-variant-numeric: tabular-nums;
  }
  .ev-when,
  .ev-for {
    color: var(--fg-muted);
    font-variant-numeric: tabular-nums;
  }

  /* Thyroid */
  .plot {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    min-width: 0;
  }
  .plot + .plot {
    margin-top: var(--sp-2);
  }
  .plot-head {
    display: flex;
    align-items: baseline;
    gap: var(--sp-3);
    margin: 0;
  }
  .plot-name {
    font-size: var(--text-base);
    font-weight: 600;
    color: var(--fg);
  }
  .plot-last {
    font-size: var(--text-sm);
    color: var(--fg-muted);
    font-variant-numeric: tabular-nums;
  }

  /* ── A phone ────────────────────────────────────────────────────────
     One column of cards whose rows have no room for a column of their own
     beside their words: a target's state sits over what it says, a test's
     due date and a medicine's meter under theirs, and the time in each
     glucose band reads as a line and a half rather than five columns. */
  :global([data-phone]) .title-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
  }
  :global([data-phone]) .actions {
    gap: var(--sp-2);
  }
  :global([data-phone]) .actions .btn {
    flex: 1;
  }
  :global([data-phone]) .icon-btn {
    width: 44px;
    height: 44px;
  }
  :global([data-phone]) .grid {
    grid-template-columns: minmax(0, 1fr);
  }
  :global([data-phone]) .row {
    flex-wrap: wrap;
    justify-content: flex-end;
  }
  :global([data-phone]) .row .grow {
    display: none;
  }
  :global([data-phone]) .target {
    flex-direction: column;
    gap: var(--sp-1);
  }
  :global([data-phone]) .state {
    width: auto;
    padding-top: 0;
  }
  :global([data-phone]) .test,
  :global([data-phone]) .med {
    flex-wrap: wrap;
  }
  :global([data-phone]) .test .due {
    padding-top: 0;
  }
  :global([data-phone]) .med-meter {
    width: 100%;
  }
  :global([data-phone]) .tiles > :global(*) {
    max-width: none;
  }
  :global([data-phone]) .tir-key li {
    grid-template-columns: auto minmax(0, 1fr) auto auto;
    row-gap: 0;
  }
  :global([data-phone]) .tir-key .swatch {
    grid-column: 1;
    grid-row: 1;
    align-self: center;
  }
  :global([data-phone]) .tir-key .tir-name {
    grid-column: 2;
    grid-row: 1;
  }
  :global([data-phone]) .tir-key .tir-pct {
    grid-column: 3;
    grid-row: 1;
    min-width: 3em;
  }
  :global([data-phone]) .tir-key .tir-time {
    grid-column: 4;
    grid-row: 1;
    min-width: 6em;
  }
  :global([data-phone]) .tir-key .tir-range {
    grid-column: 2 / -1;
    grid-row: 2;
  }
  :global([data-phone]) .link {
    display: inline-flex;
    align-items: center;
    min-height: 44px;
  }
  :global([data-phone]) .plot-head {
    flex-wrap: wrap;
    gap: 0 var(--sp-3);
  }
</style>
