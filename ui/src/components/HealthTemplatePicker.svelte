<script lang="ts">
  // Adding a condition from a template.
  //
  // A template is a starting point, never a programme: it carries the
  // targets, tests, symptom trackers and medication timing rules that
  // published guidance suggests for a condition, and this screen shows every
  // one as a checklist the person edits before anything is made. What is
  // kept comes from the person -- a target they untick is not added, a range
  // they change is the range used, an interval they lengthen is the interval
  // -- and from then on the plan belongs to the condition, not the template.
  //
  // Targets are in the units the person reads (mmol/L for a person who reads
  // glucose that way) and converted back on the way out, so the plan is
  // stored the way the vault stores everything, in canonical units.

  import { health } from '../lib/health.svelte'
  import { fromDisplay, ruleText } from '../lib/health'
  import { closesOnBack, phone } from '../lib/phone.svelte'
  import { todayIso } from '../lib/time'
  import type {
    CarePlan,
    CareTarget,
    Condition,
    LabSchedule,
    Measure,
    MedRule,
    TemplateChoice,
    TemplateInfo,
  } from '../lib/types'
  import Icon from './Icon.svelte'

  interface Props {
    /** Back out without adding anything. */
    onclose: () => void
    /** Called with the condition once it has been made. */
    ondone?: (condition: Condition) => void
  }

  const { onclose, ondone }: Props = $props()

  // ── Reading a plan ──────────────────────────────────────────────────────

  /** What a lab's code is shown as, from the plan's own list of tests. */
  function labName(plan: CarePlan, code: string): string {
    return plan.labs.find((l) => l.code === code)?.name ?? `Lab ${code}`
  }
  function measureName(plan: CarePlan, m: Measure): string {
    return m.type === 'metric'
      ? (health.metricInfo(m.metric)?.name ?? m.metric)
      : labName(plan, m.code)
  }
  const isShare = (t: CareTarget) =>
    t.measure.type === 'metric' && t.measure.stat.type === 'inRange'
  /** The unit a target's numbers are in. A lab's is whatever the report
   *  said, so only the label speaks of it. */
  function unitOf(t: CareTarget): string {
    if (t.measure.type !== 'metric') return ''
    return isShare(t) ? '%' : health.displayUnit(t.measure.metric)
  }
  function shown(t: CareTarget, v: number | null | undefined): string {
    if (v == null) return ''
    const n = t.measure.type === 'metric' && !isShare(t) ? health.toDisplay(t.measure.metric, v) : v
    return String(Math.round(n * 100) / 100)
  }
  function stored(t: CareTarget, raw: string): number | null {
    const n = Number(raw.replace(',', '.'))
    if (raw.trim() === '' || !Number.isFinite(n)) return null
    const units = health.settings?.units
    return t.measure.type === 'metric' && !isShare(t) && units
      ? fromDisplay(t.measure.metric, n, units)
      : n
  }
  function statWords(t: CareTarget): string {
    if (t.measure.type !== 'metric') return 'latest result'
    switch (t.measure.stat.type) {
      case 'mean':
        return 'average'
      case 'last':
        return 'latest'
      case 'min':
        return 'lowest'
      case 'max':
        return 'highest'
      case 'sum':
        return 'total'
      case 'count':
        return 'count'
      case 'inRange':
        return 'share of readings in range'
      default:
        return ''
    }
  }

  // ── The checklist ───────────────────────────────────────────────────────

  interface TargetRow {
    on: boolean
    target: CareTarget
    low: string
    high: string
  }
  type Unit = 'days' | 'weeks' | 'months' | 'years'
  const UNIT_DAYS: Record<Unit, number> = { days: 1, weeks: 7, months: 30, years: 365 }
  interface TestRow {
    on: boolean
    test: LabSchedule
    every: string
    unit: Unit
    afterWeeks: string
  }
  interface RuleRow {
    on: boolean
    tag: string
    rule: MedRule
  }

  let chosen = $state<TemplateInfo | null>(null)
  let targets = $state<TargetRow[]>([])
  let tests = $state<TestRow[]>([])
  let rules = $state<RuleRow[]>([])
  let symptoms = $state<{ on: boolean; name: string }[]>([])
  let diagnosed = $state('')
  let busy = $state(false)
  let failed = $state(false)

  function unitFor(days: number): Unit {
    if (days % 365 === 0) return 'years'
    if (days % 30 === 0) return 'months'
    if (days % 7 === 0) return 'weeks'
    return 'days'
  }

  function pick(template: TemplateInfo) {
    const plan = $state.snapshot(template.plan)
    chosen = $state.snapshot(template)
    targets = plan.targets.map((t) => ({
      on: true,
      target: t,
      low: shown(t, t.low),
      high: shown(t, t.high),
    }))
    tests = plan.labs.map((l) => {
      const unit = unitFor(l.everyDays)
      return {
        on: true,
        test: l,
        every: String(l.everyDays / UNIT_DAYS[unit]),
        unit,
        afterWeeks:
          l.afterChangeDays != null ? String(Math.round((l.afterChangeDays / 7) * 10) / 10) : '',
      }
    })
    rules = Object.entries(plan.medRules).flatMap(([tag, list]) =>
      list.map((rule) => ({ on: true, tag, rule })),
    )
    symptoms = template.symptoms.map((name) => ({ on: true, name }))
    diagnosed = ''
    failed = false
  }

  function back() {
    chosen = null
  }

  // On a phone, back from a template's plan goes to the list of templates
  // (and from the list, to the conditions: the parent's).
  closesOnBack(
    () => phone.on && chosen !== null,
    () => back(),
  )

  async function add() {
    const template = chosen
    if (!template || busy) return
    const plan: CarePlan = {
      ...$state.snapshot(template.plan),
      targets: targets
        .filter((r) => r.on)
        .map((r) => ({
          ...$state.snapshot(r.target),
          low: r.target.useReferenceRange ? r.target.low : stored(r.target, r.low),
          high: r.target.useReferenceRange ? r.target.high : stored(r.target, r.high),
        })),
      labs: tests
        .filter((r) => r.on)
        .map((r) => {
          const every = Number(r.every.replace(',', '.'))
          const after = Number(r.afterWeeks.replace(',', '.'))
          return {
            ...$state.snapshot(r.test),
            everyDays:
              Number.isFinite(every) && every > 0
                ? Math.round(every * UNIT_DAYS[r.unit])
                : r.test.everyDays,
            afterChangeDays:
              r.test.afterChangeDays == null
                ? null
                : r.afterWeeks.trim() !== '' && Number.isFinite(after) && after > 0
                  ? Math.round(after * 7)
                  : r.test.afterChangeDays,
          }
        }),
      medRules: {},
    }
    for (const r of rules) {
      if (!r.on) continue
      const list = plan.medRules[r.tag] ?? []
      list.push($state.snapshot(r.rule))
      plan.medRules[r.tag] = list
    }
    const choice: TemplateChoice = {
      plan,
      symptoms: symptoms.filter((s) => s.on).map((s) => s.name),
      diagnosed: diagnosed || null,
    }
    busy = true
    failed = false
    try {
      const made = await health.applyTemplate(template.key, choice)
      if (made) {
        health.openCondition(made.id)
        ondone?.(made)
      } else {
        failed = true
      }
    } finally {
      busy = false
    }
  }

  const watching = $derived.by((): string[] => {
    const c = chosen
    return c ? c.plan.watch.map((m) => measureName(c.plan, m)) : []
  })
</script>

<div class="picker">
  {#if !chosen}
    <div class="head">
      <h2 class="eyebrow">Add a condition from a template</h2>
      <button class="btn btn-sm" onclick={onclose}>Cancel</button>
    </div>
    <p class="hint">
      A template fills in what is usually worth watching: readings, targets, tests and the questions
      worth tracking. You choose what to keep before anything is added, and the plan is yours to
      change afterwards.
    </p>

    {#if health.templates.length === 0}
      <p class="quiet">No templates are available.</p>
    {:else}
      <ul class="cards">
        {#each health.templates as t (t.key)}
          <li>
            <button class="tcard" onclick={() => pick(t)}>
              <span class="tname">{t.name}</span>
              <span class="tsum">{t.summary}</span>
              <span class="tmeta">
                {t.plan.targets.length} targets · {t.plan.labs.length} tests
                {#if t.symptoms.length > 0}· {t.symptoms.length} symptoms to track{/if}
              </span>
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  {:else}
    <div class="head">
      <h2 class="eyebrow">{chosen.name}</h2>
      <span class="grow"></span>
      <button class="btn btn-sm" onclick={back}>Back</button>
      <button class="btn btn-sm" onclick={onclose}>Cancel</button>
    </div>
    <p class="hint">{chosen.summary}</p>
    <p class="hint">
      Everything below is a suggestion. Untick what you do not want and change what you do. Targets
      are yours to set with your clinician. This is not medical advice.
    </p>

    {#if watching.length > 0}
      <p class="watch"><b>It will watch:</b> {watching.join(', ')}.</p>
    {/if}

    <label class="field-label diag" for="tp-diagnosed">Diagnosed (optional)</label>
    <input
      id="tp-diagnosed"
      class="field diag-field"
      type="date"
      max={todayIso()}
      bind:value={diagnosed}
    />

    <!-- Targets -->
    {#if targets.length > 0}
      <h3 class="eyebrow section">Targets</h3>
      <ul class="rows">
        {#each targets as row, i (i)}
          {@const t = row.target}
          <li class="row" class:off={!row.on}>
            <label class="check">
              <input type="checkbox" bind:checked={row.on} />
              <span class="rtext">
                <span class="rname">{t.label}</span>
                <span class="rsub">
                  {measureName(chosen.plan, t.measure)}{statWords(t)
                    ? `, ${statWords(t)}`
                    : ''}{t.windowDays > 0 ? `, over ${t.windowDays} days` : ''}
                </span>
                {#if t.note}<span class="rnote">{t.note}</span>{/if}
              </span>
            </label>
            {#if t.useReferenceRange}
              <span class="refnote">Uses each report’s own reference range</span>
            {:else}
              <span class="bounds">
                <label class="bound">
                  <span>At least</span>
                  <input
                    class="field small"
                    inputmode="decimal"
                    bind:value={row.low}
                    disabled={!row.on}
                    aria-label="At least, for {t.label}"
                  />
                </label>
                <label class="bound">
                  <span>At most</span>
                  <input
                    class="field small"
                    inputmode="decimal"
                    bind:value={row.high}
                    disabled={!row.on}
                    aria-label="At most, for {t.label}"
                  />
                </label>
                {#if unitOf(t)}<span class="unit">{unitOf(t)}</span>{/if}
              </span>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}

    <!-- Tests -->
    {#if tests.length > 0}
      <h3 class="eyebrow section">Tests</h3>
      <ul class="rows">
        {#each tests as row, i (i)}
          <li class="row" class:off={!row.on}>
            <label class="check">
              <input type="checkbox" bind:checked={row.on} />
              <span class="rtext">
                <span class="rname">{row.test.name}</span>
                {#if row.test.afterChangeDays != null}
                  <span class="rsub">
                    Also checked after a change of dose of a {row.test.afterChangeTag || 'related'}
                    medicine.
                  </span>
                {/if}
              </span>
            </label>
            <span class="bounds">
              <span class="unit">Every</span>
              <input
                class="field small num"
                inputmode="decimal"
                bind:value={row.every}
                disabled={!row.on}
                aria-label="Every, for {row.test.name}"
              />
              <select
                class="field small"
                bind:value={row.unit}
                disabled={!row.on}
                aria-label="Unit of time, for {row.test.name}"
              >
                <option value="days">days</option>
                <option value="weeks">weeks</option>
                <option value="months">months</option>
                <option value="years">years</option>
              </select>
              {#if row.test.afterChangeDays != null}
                <span class="unit">and</span>
                <input
                  class="field small num"
                  inputmode="decimal"
                  bind:value={row.afterWeeks}
                  disabled={!row.on}
                  aria-label="Weeks after a change of dose, for {row.test.name}"
                />
                <span class="unit">weeks after a change</span>
              {/if}
            </span>
          </li>
        {/each}
      </ul>
    {/if}

    <!-- Symptoms -->
    {#if symptoms.length > 0}
      <h3 class="eyebrow section">Symptoms to track</h3>
      <p class="hint small">
        Each becomes a 0 to 10 tracker you can log from the Overview, so how you feel sits beside
        the numbers.
      </p>
      <div class="symptoms">
        {#each symptoms as s, i (i)}
          <label class="check inline">
            <input type="checkbox" bind:checked={s.on} />
            {s.name}
          </label>
        {/each}
      </div>
    {/if}

    <!-- Medication rules -->
    {#if rules.length > 0}
      <h3 class="eyebrow section">Medication timing rules</h3>
      <p class="hint small">
        Offered for any medication you tag with the name shown. They are checked afterwards and
        shown as notes, never as alarms.
      </p>
      <ul class="rows">
        {#each rules as r, i (i)}
          <li class="row" class:off={!r.on}>
            <label class="check">
              <input type="checkbox" bind:checked={r.on} />
              <span class="rtext">
                <span class="rname">{ruleText(r.rule)}</span>
                <span class="rsub">For medicines tagged “{r.tag}”</span>
              </span>
            </label>
          </li>
        {/each}
      </ul>
    {/if}

    {#if failed}
      <p class="error" role="alert">The condition could not be added. Try again.</p>
    {/if}

    <div class="foot">
      <span class="grow"></span>
      <button class="btn" onclick={back}>Back</button>
      <button class="btn btn-primary" disabled={busy} onclick={() => void add()}>
        <Icon name="plus" size={14} />
        {busy ? 'Adding…' : 'Add this condition'}
      </button>
    </div>
  {/if}
</div>

<style>
  .picker {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    min-width: 0;
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2);
  }
  .grow {
    flex: 1;
  }
  .hint {
    margin: 0;
  }
  .hint.small {
    font-size: var(--text-xs);
  }
  .quiet {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }

  .cards {
    margin: var(--sp-2) 0 0;
    padding: 0;
    list-style: none;
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(230px, 1fr));
    gap: var(--sp-3);
  }
  .tcard {
    width: 100%;
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--sp-1);
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    text-align: left;
    background: var(--bg-raised);
    transition:
      border-color var(--fast) var(--ease),
      background var(--fast) var(--ease);
  }
  .tcard:hover {
    border-color: var(--border-strong);
    background: var(--bg-hover);
  }
  .tname {
    font-size: var(--text-md);
    font-weight: 620;
    color: var(--fg);
  }
  .tsum {
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-muted);
  }
  .tmeta {
    margin-top: auto;
    padding-top: var(--sp-2);
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }

  .watch {
    margin: var(--sp-2) 0 0;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .watch b {
    color: var(--fg);
    font-weight: 600;
  }
  .diag {
    margin: var(--sp-3) 0 var(--sp-1);
  }
  .diag-field {
    width: 12em;
  }

  .section {
    margin: var(--sp-5) 0 0;
  }
  .rows {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2) var(--sp-4);
    padding: var(--sp-2) 0;
  }
  .row + .row {
    border-top: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }
  .row.off .rname,
  .row.off .rsub,
  .row.off .rnote {
    opacity: 0.55;
  }

  .check {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    flex: 1 1 260px;
    min-width: 0;
    cursor: pointer;
  }
  .check input {
    margin-top: 3px;
    accent-color: var(--accent);
  }
  .check.inline {
    flex: none;
    align-items: center;
    font-size: var(--text-sm);
    color: var(--fg);
  }
  .check.inline input {
    margin-top: 0;
  }
  .rtext {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .rname {
    font-size: var(--text-base);
    font-weight: 550;
    color: var(--fg);
  }
  .rsub {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .rnote {
    font-size: var(--text-xs);
    line-height: var(--leading-snug);
    color: var(--fg-muted);
  }
  .refnote {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }

  .bounds {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }
  .bound {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .unit {
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .field.small {
    width: 5.5em;
    height: 30px;
    font-size: var(--text-sm);
  }
  .field.small.num {
    width: 4.5em;
  }
  select.field.small {
    width: auto;
  }

  .symptoms {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2) var(--sp-5);
  }

  .error {
    margin: var(--sp-3) 0 0;
  }
  .foot {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin-top: var(--sp-4);
  }

  /* ── A phone ────────────────────────────────────────────────────────
     Every row a person ticks is a finger tall, with a checkbox big enough
     to hit; the number fields are 40 px; the heading's buttons and the
     foot's wrap beneath their text rather than off the screen. */
  :global([data-phone]) .head,
  :global([data-phone]) .foot {
    flex-wrap: wrap;
  }
  :global([data-phone]) .foot {
    justify-content: flex-end;
  }
  :global([data-phone]) .foot .grow,
  :global([data-phone]) .head .grow {
    display: none;
  }
  :global([data-phone]) .head .btn:first-of-type {
    margin-left: auto;
  }
  :global([data-phone]) .check {
    min-height: 44px;
    flex-basis: 100%;
  }
  :global([data-phone]) .check input {
    width: 20px;
    height: 20px;
    margin-top: 2px;
    flex: none;
  }
  :global([data-phone]) .check.inline {
    flex: none;
    min-height: 44px;
  }
  :global([data-phone]) .field.small {
    height: 40px;
  }
  :global([data-phone]) .diag-field {
    width: 100%;
  }
  :global([data-phone]) .bounds {
    padding-left: 28px;
  }
</style>
