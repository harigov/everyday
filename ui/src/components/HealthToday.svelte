<script lang="ts">
  // Health at a glance, for one day: what is due, how the body moved and
  // slept, the latest readings, what was eaten, and what needs a look.
  //
  // Every block hides when it has nothing, so a day with only a night's sleep
  // is a page with only a night's sleep on it, and not a grid of empty
  // boxes. Forms follow the question each answers, as in Screen time: a
  // ratio against a goal is a meter (never a ring), a current value is a
  // figure, a day of glucose is a stacked bar with its keyed list, and sleep
  // is one thin strip of stages.
  //
  // Doses due come first because they are the one thing here somebody has
  // to *do*: Taken and Skip log the dose against the time it was due, so
  // the schedule and the history agree about which dose it answers.
  //
  // Nothing here judges a number. A trend arrow says which way a reading
  // went, never whether that is good; timing notes are notices after the
  // fact, and the assistant is never the one to change a dose.

  import { health } from '../lib/health.svelte'
  import {
    activityName,
    durationText,
    GLUCOSE_BANDS,
    GLUCOSE_COLORS,
    glucoseBandName,
    SLEEP_STAGE_COLORS,
    stageName,
    sumNutrients,
    type GlucoseBand,
  } from '../lib/health'
  import { panels } from '../lib/panels.svelte'
  import { dateFormat, formatInstantTime, friendlyDate, plural, relativeTime } from '../lib/format'
  import { isoDate, locale, minutesBetween } from '../lib/time'
  import type { DoseStatus, DueDose, HealthSource, Latest, Metric, SleepStage } from '../lib/types'
  import Icon from './Icon.svelte'
  import Meter from './Meter.svelte'
  import ShareBar from './ShareBar.svelte'
  import StatTile from './StatTile.svelte'

  // Each measure's own colour, drawn at the theme's lightness and chroma so
  // none is louder than another and all of them sit on every theme.
  const MOVE = 'oklch(var(--app-lightness) var(--app-chroma) 25)'
  const EXERCISE = 'oklch(var(--app-lightness) var(--app-chroma) 148)'
  const STAND = 'oklch(var(--app-lightness) var(--app-chroma) 232)'
  const FOOD = 'oklch(var(--app-lightness) var(--app-chroma) 62)'

  const day = $derived(health.today)
  const act = $derived(day?.activity ?? null)
  const goals = $derived(day?.goals ?? health.settings?.goals ?? null)
  const targets = $derived(health.settings?.nutrition ?? null)

  const count = (n: number) => Math.round(n).toLocaleString(locale())
  const ratio = (value: number, goal: number | null | undefined) =>
    goal && goal > 0 ? value / goal : 0
  const pct = (share: number) => Math.round(share * 100)

  // ── activity ───────────────────────────────────────────────────────────

  const hasActivity = $derived(
    !!act && (act.activeKcal > 0 || act.exerciseMin > 0 || act.standHours > 0 || act.steps > 0),
  )

  const stepsNote = $derived.by(() => {
    if (!act) return undefined
    const parts: string[] = []
    if (goals && goals.steps > 0) parts.push(`of ${count(goals.steps)}`)
    if (act.distanceM > 0) parts.push(health.display('walking_running_distance', act.distanceM))
    return parts.join(' · ') || undefined
  })

  // ── sleep ──────────────────────────────────────────────────────────────

  const night = $derived(day?.sleep ?? null)

  /** The night's stages in order, as the strip's proportions. */
  const strip = $derived(
    (night?.segments ?? [])
      .filter((s) => s.stage !== 'inBed')
      .map((s) => ({ stage: s.stage, min: minutesBetween(s.start, s.end) }))
      .filter((s) => s.min > 0),
  )

  const stageTotals = $derived.by((): [SleepStage, number][] => {
    if (!night) return []
    const plain = Math.max(0, night.asleepMin - night.coreMin - night.deepMin - night.remMin)
    const all: [SleepStage, number][] = [
      ['deep', night.deepMin],
      ['core', night.coreMin],
      ['rem', night.remMin],
      ['asleep', plain],
      ['awake', night.awakeMin],
    ]
    return all.filter(([, min]) => min > 0)
  })

  const slept = $derived(
    night?.sleepStart && night.sleepEnd
      ? `${formatInstantTime(night.sleepStart)} – ${formatInstantTime(night.sleepEnd)}`
      : undefined,
  )

  // ── latest readings ────────────────────────────────────────────────────

  interface Tile {
    key: string
    label: string
    value: string
    note: string
  }

  /** The groups that are readings of the body; activity and food have their own blocks. */
  const READINGS = new Set(['heart', 'respiratory', 'body', 'vitals', 'metabolic'])
  const MOST_READINGS = 8

  /** Which way a reading went since the one before it, in words as well as an arrow. */
  function trend(now: number, before: number | null | undefined, said: string): string {
    if (before === null || before === undefined) return ''
    const same = Math.abs(now - before) <= Math.abs(before) * 0.005
    if (same) return '→ steady'
    return `${now > before ? '↑' : '↓'} from ${said}`
  }

  function age(l: Latest, date: string): string {
    return isoDate(new Date(l.at)) === date ? '' : relativeTime(l.at)
  }

  const vitals = $derived.by((): Tile[] => {
    if (!day) return []
    const list = day.latest
    const out: Tile[] = []
    for (const l of list) {
      const group = health.metricInfo(l.metric)?.group
      if (!group || !READINGS.has(group) || l.metric === 'blood_pressure_diastolic') continue
      const when = age(l, day.date)
      const dia =
        l.metric === 'blood_pressure_systolic'
          ? list.find((x) => x.metric === 'blood_pressure_diastolic')
          : undefined
      if (dia) {
        // One tile for a pressure: 122/78, and the one before it the same way.
        const n = (m: Metric, v: number) => Math.round(health.toDisplay(m, v))
        const unit = health.displayUnit(l.metric)
        const before =
          l.previous != null && dia.previous != null
            ? `${n(l.metric, l.previous)}/${n(dia.metric, dia.previous)} ${unit}`
            : ''
        out.push({
          key: l.metric,
          label: 'Blood pressure',
          value: `${n(l.metric, l.value)}/${n(dia.metric, dia.value)} ${unit}`,
          note: [before ? trend(l.value, l.previous, before) : '', when]
            .filter(Boolean)
            .join(' · '),
        })
        continue
      }
      out.push({
        key: l.metric,
        label: health.metricInfo(l.metric)?.name ?? l.metric,
        value: health.display(l.metric, l.value),
        note: [
          l.previous != null
            ? trend(l.value, l.previous, health.display(l.metric, l.previous))
            : '',
          when,
        ]
          .filter(Boolean)
          .join(' · '),
      })
    }
    return out
  })

  // ── doses ──────────────────────────────────────────────────────────────

  const due = $derived(
    [...(day?.dosesDue ?? [])].sort((a, b) => Date.parse(a.dueAt) - Date.parse(b.dueAt)),
  )

  /** Doses being logged, so a double click is one dose and the buttons say so. */
  let pending = $state<Record<string, boolean>>({})
  const slot = (d: DueDose) => `${d.medicationId}|${d.dueAt}`

  async function mark(d: DueDose, status: DoseStatus) {
    const key = slot(d)
    if (pending[key]) return
    pending[key] = true
    try {
      await health.logDose(d.medicationId, { scheduledAt: d.dueAt, status })
    } finally {
      pending[key] = false
    }
  }

  function doseWords(d: DueDose): string {
    if (!d.dose) return d.overdue ? 'Overdue' : 'Due'
    if (d.dose.status === 'taken') return `Taken at ${formatInstantTime(d.dose.at)}`
    if (d.dose.status === 'skipped') return 'Skipped'
    return 'Marked missed'
  }

  // ── food ───────────────────────────────────────────────────────────────

  const eaten = $derived(day ? sumNutrients([day.nutrition.logged, day.nutrition.imported]) : null)
  const showFood = $derived(!!day && (day.nutrition.meals > 0 || (eaten?.kcal ?? 0) > 0))

  interface FoodTile {
    key: string
    label: string
    metric: Metric
    value: number
    target: number | null
  }

  const food = $derived.by((): FoodTile[] => {
    if (!eaten) return []
    const t = targets
    return [
      {
        key: 'kcal',
        label: 'Calories',
        metric: 'dietary_energy',
        value: eaten.kcal,
        target: t?.kcal ?? null,
      },
      {
        key: 'carbs',
        label: 'Carbohydrate',
        metric: 'dietary_carbs',
        value: eaten.carbsG,
        target: t?.carbsG ?? null,
      },
      {
        key: 'protein',
        label: 'Protein',
        metric: 'dietary_protein',
        value: eaten.proteinG,
        target: t?.proteinG ?? null,
      },
    ]
  })

  // ── glucose ────────────────────────────────────────────────────────────

  const glucose = $derived(day?.glucose && day.glucose.readings > 0 ? day.glucose : null)

  /** The five consensus bands, each with the range it covers in the person's unit. */
  const bands = $derived.by(() => {
    const g = glucose
    if (!g) return []
    const at = (mgdl: number) => health.display('blood_glucose', mgdl)
    const share: Record<GlucoseBand, number> = {
      veryLow: g.veryLow,
      low: g.low,
      inRange: g.inRange,
      high: g.high,
      veryHigh: g.veryHigh,
    }
    const covers: Record<GlucoseBand, string> = {
      veryLow: `under ${at(54)}`,
      low: `${at(54)} to ${at(70)}`,
      inRange: `${at(70)} to ${at(180)}`,
      high: `${at(180)} to ${at(250)}`,
      veryHigh: `over ${at(250)}`,
    }
    return GLUCOSE_BANDS.map((band) => ({
      id: band,
      name: glucoseBandName(band),
      value: share[band],
      label: covers[band],
      color: GLUCOSE_COLORS[band],
    }))
  })

  // ── the rest ───────────────────────────────────────────────────────────

  const alerts = $derived(day?.alerts ?? [])
  const labs = $derived(day?.labsDue ?? [])
  const workouts = $derived(day?.workouts ?? [])
  const stale = $derived(day?.staleSources ?? [])

  const any = $derived(
    hasActivity ||
      !!night ||
      vitals.length > 0 ||
      due.length > 0 ||
      showFood ||
      !!glucose ||
      alerts.length > 0 ||
      labs.length > 0 ||
      workouts.length > 0,
  )

  function heard(s: HealthSource): string {
    const at = s.status.lastReceivedAt
    return at ? `last heard from ${relativeTime(at)}` : 'never heard from'
  }

  function openSettings() {
    panels.openSettings('health')
  }

  const workoutDay = (start: string) =>
    dateFormat({ weekday: 'short', day: 'numeric', month: 'short' }).format(new Date(start))
</script>

<div class="today scroll">
  <div class="page">
    {#if !day}
      <p class="quiet">Loading…</p>
    {:else}
      {#if stale.length > 0}
        <p class="stale" role="status">
          <Icon name="info" size={15} />
          <span>
            Some sources have gone quiet: {stale.map((s) => `${s.name} (${heard(s)})`).join(', ')}.
            <button class="link" onclick={openSettings}>Open Health settings</button>
          </span>
        </p>
      {/if}

      {#if due.length > 0}
        <section class="card" aria-labelledby="ht-doses">
          <div class="card-head">
            <h2 class="eyebrow" id="ht-doses">Medications</h2>
            <button class="link" onclick={() => health.setView('medications')}>All of them</button>
          </div>
          <ul class="doses">
            {#each due as d (slot(d))}
              <li>
                <span class="at">{formatInstantTime(d.dueAt)}</span>
                <span class="what">
                  <span class="name">{d.name}</span>
                  <span class="state" class:late={!d.dose && d.overdue}>{doseWords(d)}</span>
                </span>
                {#if !d.dose}
                  <span class="acts">
                    <button
                      class="btn btn-sm btn-primary"
                      disabled={pending[slot(d)]}
                      aria-label="Mark {d.name} taken"
                      onclick={() => void mark(d, 'taken')}>Taken</button
                    >
                    <button
                      class="btn btn-sm"
                      disabled={pending[slot(d)]}
                      aria-label="Skip {d.name}"
                      onclick={() => void mark(d, 'skipped')}>Skip</button
                    >
                  </span>
                {/if}
              </li>
            {/each}
          </ul>
        </section>
      {/if}

      {#if hasActivity || night}
        <div class="pair">
          {#if hasActivity && act}
            <section class="card" aria-labelledby="ht-move">
              <div class="card-head">
                <h2 class="eyebrow" id="ht-move">Activity</h2>
                <button class="link" onclick={() => health.setView('activity')}>More</button>
              </div>
              <div class="meters">
                <Meter
                  label="Move"
                  value={ratio(act.activeKcal, goals?.moveKcal)}
                  color={MOVE}
                  note={goals && goals.moveKcal > 0
                    ? `${health.display('active_energy', act.activeKcal)} of ${health.display('active_energy', goals.moveKcal)}`
                    : health.display('active_energy', act.activeKcal)}
                  onpick={() => health.setView('activity')}
                />
                <Meter
                  label="Exercise"
                  value={ratio(act.exerciseMin, goals?.exerciseMin)}
                  color={EXERCISE}
                  note={goals && goals.exerciseMin > 0
                    ? `${durationText(act.exerciseMin)} of ${durationText(goals.exerciseMin)}`
                    : durationText(act.exerciseMin)}
                  onpick={() => health.setView('activity')}
                />
                <Meter
                  label="Stand"
                  value={ratio(act.standHours, goals?.standHours)}
                  color={STAND}
                  note={goals && goals.standHours > 0
                    ? `${act.standHours} of ${plural(goals.standHours, 'hour')}`
                    : plural(act.standHours, 'hour')}
                  onpick={() => health.setView('activity')}
                />
              </div>
              <StatTile label="Steps" value={count(act.steps)} note={stepsNote} />
            </section>
          {/if}

          {#if night}
            <section class="card" aria-labelledby="ht-sleep">
              <div class="card-head">
                <h2 class="eyebrow" id="ht-sleep">Last night</h2>
                <button class="link" onclick={() => health.setView('sleep')}>More</button>
              </div>
              <div class="tiles">
                <StatTile label="Asleep" value={durationText(night.asleepMin)} note={slept} />
                {#if typeof night.score === 'number'}
                  <StatTile label="Score" value={String(Math.round(night.score))} />
                {/if}
              </div>
              {#if strip.length > 0}
                <div
                  class="strip"
                  role="img"
                  aria-label="Stages: {stageTotals
                    .map(([s, m]) => `${stageName(s)} ${durationText(m)}`)
                    .join(', ')}"
                >
                  {#each strip as s, i (i)}
                    <span style="flex: {s.min}; background: {SLEEP_STAGE_COLORS[s.stage]}"></span>
                  {/each}
                </div>
              {/if}
              {#if stageTotals.length > 0}
                <ul class="key">
                  {#each stageTotals as [stage, min] (stage)}
                    <li>
                      <span class="swatch" style="background: {SLEEP_STAGE_COLORS[stage]}"></span>
                      <span class="kname">{stageName(stage)}</span>
                      <span class="kval">{durationText(min)}</span>
                    </li>
                  {/each}
                </ul>
              {/if}
            </section>
          {/if}
        </div>
      {/if}

      {#if vitals.length > 0}
        <section class="card" aria-labelledby="ht-vitals">
          <div class="card-head">
            <h2 class="eyebrow" id="ht-vitals">Latest readings</h2>
            <button class="link" onclick={() => health.setView('vitals')}>All readings</button>
          </div>
          <div class="tiles wide">
            {#each vitals.slice(0, MOST_READINGS) as t (t.key)}
              <StatTile label={t.label} value={t.value} note={t.note || undefined} />
            {/each}
          </div>
        </section>
      {/if}

      {#if showFood || glucose}
        <div class="pair">
          {#if showFood}
            <section class="card" aria-labelledby="ht-food">
              <div class="card-head">
                <h2 class="eyebrow" id="ht-food">Eaten today</h2>
                <button class="link" onclick={() => health.setView('nutrition')}>Nutrition</button>
              </div>
              <div class="meters">
                {#each food as f (f.key)}
                  {#if f.target && f.target > 0}
                    <Meter
                      label={f.label}
                      value={f.value / f.target}
                      color={FOOD}
                      note="{health.display(f.metric, f.value)} of {health.display(
                        f.metric,
                        f.target,
                      )}"
                      onpick={() => health.setView('nutrition')}
                    />
                  {:else}
                    <StatTile label={f.label} value={health.display(f.metric, f.value)} />
                  {/if}
                {/each}
              </div>
            </section>
          {/if}

          {#if glucose}
            <section class="card" aria-labelledby="ht-glucose">
              <h2 class="eyebrow" id="ht-glucose">Glucose today</h2>
              <div class="tiles">
                <StatTile
                  label="Time in range"
                  value="{pct(glucose.inRange)}%"
                  note={glucose.coverage < 0.7
                    ? 'Part of the day has no readings yet'
                    : plural(glucose.readings, 'reading')}
                />
                {#if typeof glucose.mean === 'number'}
                  <StatTile label="Average" value={health.display('blood_glucose', glucose.mean)} />
                {/if}
              </div>
              <ShareBar slices={bands} />
            </section>
          {/if}
        </div>
      {/if}

      {#if alerts.length > 0}
        <section class="card" aria-labelledby="ht-alerts">
          <h2 class="eyebrow" id="ht-alerts">Medicine timing</h2>
          <ul class="notes">
            {#each alerts as a, i (i)}
              <li>
                <span class="mark"><Icon name="info" size={15} /></span>
                <span class="msg">{a.message}</span>
                <span class="when">{formatInstantTime(a.at)}</span>
              </li>
            {/each}
          </ul>
        </section>
      {/if}

      {#if labs.length > 0}
        <section class="card" aria-labelledby="ht-labs">
          <div class="card-head">
            <h2 class="eyebrow" id="ht-labs">Tests coming due</h2>
            <button class="link" onclick={() => health.setView('records')}>Records</button>
          </div>
          <ul class="notes">
            {#each labs as l, i (i)}
              <li>
                <span class="msg">
                  <span class="name">{l.schedule.name}</span>
                  {#if l.reason}<span class="why">{l.reason}</span>{/if}
                </span>
                <span class="when" class:late={l.overdue}>
                  {l.overdue ? 'Overdue' : l.due ? `Due ${friendlyDate(l.due)}` : 'Due'}
                </span>
              </li>
            {/each}
          </ul>
        </section>
      {/if}

      {#if workouts.length > 0}
        <section class="card" aria-labelledby="ht-workouts">
          <div class="card-head">
            <h2 class="eyebrow" id="ht-workouts">Workouts</h2>
            <button class="link" onclick={() => health.setView('activity')}>Activity</button>
          </div>
          <ul class="rows">
            {#each workouts as w (w.id)}
              <li>
                <button class="list-row workout" onclick={() => health.openWorkout(w.id)}>
                  <span class="what">
                    <span class="name">{w.name || activityName(w.activity)}</span>
                    <span class="when">{workoutDay(w.start)} · {formatInstantTime(w.start)}</span>
                  </span>
                  <span class="fig">{durationText(minutesBetween(w.start, w.end))}</span>
                  <span class="fig">
                    {w.distanceM ? health.display('walking_running_distance', w.distanceM) : ''}
                  </span>
                  <span class="fig">
                    {w.activeEnergyKcal ? health.display('active_energy', w.activeEnergyKcal) : ''}
                  </span>
                </button>
              </li>
            {/each}
          </ul>
        </section>
      {/if}

      {#if !any}
        <p class="quiet">Nothing is recorded for this day.</p>
      {/if}
    {/if}
  </div>
</div>

<style>
  .today {
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
  .card-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--sp-3);
  }
  .pair {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
    gap: var(--sp-5);
  }

  .quiet {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }

  .link {
    color: var(--accent);
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .link:hover {
    text-decoration: underline;
  }

  /* A quiet line, not an alarm: nothing is wrong with the person, only with
     the pipe that brings their data. */
  .stale {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    margin: 0;
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--radius);
    background: var(--bg-sunken);
    color: var(--fg-muted);
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
  }
  .stale :global(svg) {
    flex: none;
    margin-top: 2px;
  }

  .meters {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .tiles {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: var(--sp-4) var(--sp-8);
  }
  .tiles.wide {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
    align-items: start;
    gap: var(--sp-4);
  }

  /* One strip for the night. A real width for each segment's flex share to
     resolve against, and a 1px gap in the surface as the separator. */
  .strip {
    display: flex;
    gap: 1px;
    width: 100%;
    height: 10px;
    border-radius: 999px;
    overflow: hidden;
  }
  .strip span {
    min-width: 1px;
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
    width: 9px;
    height: 9px;
    border-radius: 2px;
    translate: 0 -1px;
  }
  .kval {
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  .doses,
  .notes,
  .rows {
    display: flex;
    flex-direction: column;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .doses li,
  .notes li {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 0;
    padding: var(--sp-2) 0;
  }
  .doses li + li,
  .notes li + li {
    border-top: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }
  .at {
    flex: none;
    width: 5.5em;
    color: var(--fg-muted);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
  }
  .what {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .name {
    color: var(--fg);
    font-size: var(--text-base);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .state,
  .when,
  .why {
    color: var(--fg-subtle);
    font-size: var(--text-xs);
  }
  /* Late is said in words and in the warning colour, never in colour alone. */
  .late {
    color: var(--warning);
    font-weight: 600;
  }
  .acts {
    flex: none;
    display: flex;
    gap: var(--sp-2);
  }

  .notes .mark {
    flex: none;
    display: grid;
    place-items: center;
    color: var(--fg-subtle);
  }
  .notes .msg {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
    color: var(--fg-muted);
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
  }
  .notes .when {
    flex: none;
    font-variant-numeric: tabular-nums;
  }

  /* The shared `.list-row` is drawn for a column of its own; inside a card it
     fills the card instead of being inset from it. */
  .workout {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 5.5em 5.5em 5.5em;
    align-items: center;
    gap: var(--sp-3);
    width: 100%;
    margin-inline: 0;
    padding: var(--sp-2) var(--sp-3);
  }
  .workout .fig {
    text-align: right;
    color: var(--fg-muted);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
  }
</style>
