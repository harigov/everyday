<script lang="ts">
  // Nutrition: what was eaten on a day, against the targets the person has
  // set, and how the days have gone over a longer stretch.
  //
  // Three things, in the order they are wanted. The day first: meals grouped
  // by slot, each with its own numbers, and under them the day's totals as
  // meters against the targets in Settings (a meter only where there is a
  // target to measure against -- without one there is a plain figure and no
  // pretence of a goal). Then, for a person who has a glucose sensor, each
  // meal's response on demand: how far it rose, when it peaked and the
  // curve itself with the meal marked. Last, the days side by side: calories
  // or carbs as bars with the target as a line, what was logged here and
  // what another app wrote to the phone drawn apart.
  //
  // The day is `health.date` and the range is the store's, so this pane
  // moves with the rest of the app. A day's meals come from `health.meals`;
  // the numbers shown are worked out from them, never kept twice.

  import { health } from '../lib/health.svelte'
  import { durationText, slotName, sumNutrients } from '../lib/health'
  import { dateFormat, friendlyDate } from '../lib/format'
  import { addDays, isoDate, startOfDay, todayIso } from '../lib/time'
  import type {
    Meal,
    MealId,
    MealResponse,
    MealSlot,
    Metric,
    NutritionDay,
    NutritionTargets,
  } from '../lib/types'
  import HealthBars from './HealthBars.svelte'
  import HealthLine from './HealthLine.svelte'
  import HealthMealEditor from './HealthMealEditor.svelte'
  import Icon from './Icon.svelte'
  import Meter from './Meter.svelte'

  const SLOTS: MealSlot[] = ['breakfast', 'lunch', 'dinner', 'snack']
  const accent = 'var(--app-health, var(--accent))'

  // ── The day ─────────────────────────────────────────────────────────────

  const date = $derived(health.date || todayIso())
  const atPresent = $derived(date >= todayIso())
  const targets = $derived(health.settings?.nutrition ?? null)

  /** A meal's own wall-clock moment, as the UTC fields of a shifted instant:
   *  the clock it was eaten by, wherever the person is now. */
  function shifted(meal: Meal): Date {
    return new Date(Date.parse(meal.at) + meal.offsetMin * 60_000)
  }
  const mealDay = (meal: Meal) => shifted(meal).toISOString().slice(0, 10)
  const mealClock = (meal: Meal) =>
    dateFormat({ hour: 'numeric', minute: '2-digit', timeZone: 'UTC' }).format(shifted(meal))

  const dayMeals = $derived(
    health.meals
      .filter((m) => mealDay(m) === date)
      .sort((a, b) => Date.parse(a.at) - Date.parse(b.at)),
  )
  const groups = $derived(
    SLOTS.map((slot) => ({ slot, meals: dayMeals.filter((m) => m.slot === slot) })),
  )

  const mealTotals = (meal: Meal) => sumNutrients(meal.items.map((i) => i.nutrients))
  const logged = $derived(sumNutrients(dayMeals.flatMap((m) => m.items.map((i) => i.nutrients))))
  /** What another app wrote to the phone for the day, already merged against
   *  what was logged here by the vault. */
  const imported = $derived(health.nutrition.find((d) => d.date === date)?.imported ?? null)

  type Macro = 'kcal' | 'carbsG' | 'proteinG' | 'fatG'
  const MACROS: { key: Macro; metric: Metric; label: string; target: keyof NutritionTargets }[] = [
    { key: 'kcal', metric: 'dietary_energy', label: 'Energy', target: 'kcal' },
    { key: 'carbsG', metric: 'dietary_carbs', label: 'Carbohydrate', target: 'carbsG' },
    { key: 'proteinG', metric: 'dietary_protein', label: 'Protein', target: 'proteinG' },
    { key: 'fatG', metric: 'dietary_fat', label: 'Fat', target: 'fatG' },
  ]
  const macros = $derived(
    MACROS.map((m) => ({
      ...m,
      value: logged[m.key] + (imported?.[m.key] ?? 0),
      goal: targets?.[m.target] ?? null,
    })),
  )
  const anyTarget = $derived(macros.some((m) => m.goal))

  function newMeal(slot?: MealSlot) {
    const now = new Date()
    const today = todayIso()
    // A slot asked for gets its usual hour; on a past day with no slot, the
    // middle of it; otherwise now.
    const hours: Record<MealSlot, [number, number]> = {
      breakfast: [8, 0],
      lunch: [12, 30],
      dinner: [19, 0],
      snack: [15, 30],
    }
    const when = startOfDay(date)
    if (slot) when.setHours(...hours[slot])
    else if (date === today) when.setHours(now.getHours(), now.getMinutes())
    else when.setHours(12, 0)
    const hour = when.getHours()
    const chosen: MealSlot =
      slot ?? (hour < 11 ? 'breakfast' : hour < 15 ? 'lunch' : hour < 21 ? 'dinner' : 'snack')
    const stamp = now.toISOString()
    health.editingMeal = {
      id: crypto.randomUUID(),
      at: when.toISOString(),
      offsetMin: -when.getTimezoneOffset(),
      slot: chosen,
      name: '',
      items: [],
      photo: null,
      note: '',
      createdAt: stamp,
      updatedAt: stamp,
    }
  }

  function edit(meal: Meal) {
    health.editingMeal = $state.snapshot(meal)
  }

  // ── A meal's glucose response, on demand ────────────────────────────────

  let shown = $state<Record<MealId, boolean>>({})
  let responses = $state<Record<MealId, MealResponse | null>>({})
  let loading = $state<Record<MealId, boolean>>({})

  async function toggleResponse(meal: Meal) {
    const id = meal.id
    if (shown[id]) {
      shown[id] = false
      return
    }
    shown[id] = true
    if (id in responses) return
    loading[id] = true
    try {
      responses[id] = await health.mealResponse(id)
    } finally {
      loading[id] = false
    }
  }

  const glucoseUnit = $derived(health.displayUnit('blood_glucose'))

  // ── Day by day ──────────────────────────────────────────────────────────

  let measure = $state<'kcal' | 'carbs'>('kcal')
  const rangeMetric = $derived<Metric>(measure === 'kcal' ? 'dietary_energy' : 'dietary_carbs')
  const rangeKey = $derived<Macro>(measure === 'kcal' ? 'kcal' : 'carbsG')
  const rangeGoal = $derived((measure === 'kcal' ? targets?.kcal : targets?.carbsG) ?? null)
  const rangeUnit = $derived(health.displayUnit(rangeMetric))

  function bars(pick: (d: NutritionDay) => number) {
    return health.nutrition.map((d) => ({
      at: startOfDay(d.date).toISOString(),
      value: health.toDisplay(rangeMetric, pick(d)),
    }))
  }
  const totalBars = $derived(bars((d) => d.logged[rangeKey] + d.imported[rangeKey]))
  const importedBars = $derived(bars((d) => d.imported[rangeKey]))
  const anyLogged = $derived(totalBars.some((p) => p.value > 0))
  const anyImported = $derived(importedBars.some((p) => p.value > 0))
  const goalShown = $derived(rangeGoal ? health.toDisplay(rangeMetric, rangeGoal) : null)

  function barLabel(p: { at: string; value: number }): string {
    return `${friendlyDate(isoDate(new Date(p.at)))}: ${Math.round(p.value).toLocaleString()} ${rangeUnit}`
  }
  function openDay(p: { at: string; value: number }) {
    health.setDate(isoDate(new Date(p.at)))
  }
</script>

<div class="nutrition scroll">
  <div class="page">
    <!-- The day, and the way to add to it -->
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
      <span class="grow"></span>
      <button class="btn btn-primary" onclick={() => newMeal()}>
        <Icon name="plus" size={14} /> Log a meal
      </button>
    </div>

    <!-- Totals against targets -->
    <section class="card" aria-labelledby="nut-totals">
      <h2 class="eyebrow" id="nut-totals">The day so far</h2>
      <div class="meters">
        {#each macros as m (m.key)}
          {#if m.goal}
            <Meter
              label={m.label}
              value={m.value / m.goal}
              color={accent}
              note="{health.display(m.metric, m.value)} of {health.display(m.metric, m.goal)}"
            />
          {:else}
            <div class="plain">
              <span class="plain-name">{m.label}</span>
              <span class="plain-value">{health.display(m.metric, m.value)}</span>
              <span class="plain-note">No target set</span>
            </div>
          {/if}
        {/each}
      </div>
      {#if imported && imported.kcal > 0}
        <p class="foot">
          Includes {health.display('dietary_energy', imported.kcal)} another app on your phone wrote for
          the day.
        </p>
      {/if}
      <p class="foot">
        Targets are yours to set with your clinician. This is not medical advice.
        {#if !anyTarget}Daily targets can be set in Settings → Health.{/if}
      </p>
    </section>

    <!-- Meals by slot -->
    {#each groups as group (group.slot)}
      <section class="card" aria-labelledby="slot-{group.slot}">
        <header class="slot-head">
          <h2 class="eyebrow" id="slot-{group.slot}">{slotName(group.slot)}</h2>
          {#if group.meals.length > 0}
            {@const t = sumNutrients(group.meals.flatMap((m) => m.items.map((i) => i.nutrients)))}
            <span class="slot-sum">
              {health.display('dietary_energy', t.kcal)} · {health.display(
                'dietary_carbs',
                t.carbsG,
              )}
              carbs
            </span>
          {/if}
          <span class="grow"></span>
          <button class="link" onclick={() => newMeal(group.slot)}>
            Add {slotName(group.slot).toLowerCase()}
          </button>
        </header>

        {#if group.meals.length === 0}
          <p class="quiet">Nothing logged.</p>
        {:else}
          <ul class="meals">
            {#each group.meals as meal (meal.id)}
              {@const t = mealTotals(meal)}
              <li class="meal">
                <button class="list-row meal-main" onclick={() => edit(meal)}>
                  <span class="meal-top">
                    <span class="meal-name">{meal.name || slotName(meal.slot)}</span>
                    <span class="meal-time">{mealClock(meal)}</span>
                  </span>
                  {#if meal.items.length > 0}
                    <span class="meal-items">{meal.items.map((i) => i.name).join(', ')}</span>
                  {/if}
                  <span class="meal-macros">
                    {health.display('dietary_energy', t.kcal)} · {health.display(
                      'dietary_carbs',
                      t.carbsG,
                    )} carbs · {health.display('dietary_protein', t.proteinG)} protein · {health.display(
                      'dietary_fat',
                      t.fatG,
                    )} fat
                    {#if meal.items.some((i) => i.estimated)}
                      · <span class="est">estimated</span>
                    {/if}
                  </span>
                </button>

                {#if targets?.carbsPerMealG}
                  <div class="meal-meter">
                    <Meter
                      label="Carbs in this meal"
                      value={t.carbsG / targets.carbsPerMealG}
                      color={accent}
                      note="{health.display('dietary_carbs', t.carbsG)} of {health.display(
                        'dietary_carbs',
                        targets.carbsPerMealG,
                      )} per meal"
                    />
                  </div>
                {/if}

                <div class="meal-tools">
                  <button
                    class="link"
                    aria-expanded={!!shown[meal.id]}
                    onclick={() => void toggleResponse(meal)}
                  >
                    {shown[meal.id] ? 'Hide glucose response' : 'Glucose response'}
                  </button>
                </div>

                {#if shown[meal.id]}
                  <div class="response">
                    {#if loading[meal.id]}
                      <p class="quiet">Reading the glucose around this meal…</p>
                    {:else}
                      {@const r = responses[meal.id]}
                      {#if !r || r.points.length === 0}
                        <p class="quiet">There are no glucose readings around this meal.</p>
                      {:else}
                        <dl class="facts">
                          {#if r.rise != null}
                            <div>
                              <dt>Rise</dt>
                              <dd>+{health.display('blood_glucose', r.rise)}</dd>
                            </div>
                          {/if}
                          {#if r.peak != null}
                            <div>
                              <dt>Peak</dt>
                              <dd>
                                {health.display(
                                  'blood_glucose',
                                  r.peak,
                                )}{#if r.peakAfterMin != null},
                                  {durationText(r.peakAfterMin)} after{/if}
                              </dd>
                            </div>
                          {/if}
                          {#if r.baseline != null}
                            <div>
                              <dt>Before the meal</dt>
                              <dd>{health.display('blood_glucose', r.baseline)}</dd>
                            </div>
                          {/if}
                          {#if r.peak != null}
                            <div>
                              <dt>Back to where it began</dt>
                              <dd>
                                {r.recoveredAfterMin != null
                                  ? `${durationText(r.recoveredAfterMin)} after`
                                  : 'not within 3 hours'}
                              </dd>
                            </div>
                          {/if}
                        </dl>
                        <HealthLine
                          series={r.points.map((p) => ({
                            at: p.start,
                            value: health.toDisplay('blood_glucose', p.value),
                          }))}
                          unit={glucoseUnit}
                          color={accent}
                          height={100}
                          markers={[{ at: meal.at, label: slotName(meal.slot) }]}
                        />
                        <p class="foot">
                          Three hours from the meal, {health.display('dietary_carbs', r.carbsG)} of carbs.
                        </p>
                      {/if}
                    {/if}
                  </div>
                {/if}
              </li>
            {/each}
          </ul>
        {/if}
      </section>
    {/each}

    <!-- Day by day -->
    <section class="card" aria-labelledby="nut-range">
      <div class="card-head">
        <h2 class="eyebrow" id="nut-range">Day by day</h2>
        <div class="filters" role="group" aria-label="What to show">
          <button
            class="filter"
            class:on={measure === 'kcal'}
            aria-pressed={measure === 'kcal'}
            onclick={() => (measure = 'kcal')}>Energy</button
          >
          <button
            class="filter"
            class:on={measure === 'carbs'}
            aria-pressed={measure === 'carbs'}
            onclick={() => (measure = 'carbs')}>Carbs</button
          >
        </div>
      </div>

      <!-- The stretch is the header's: its range chips and stepper drive this
           chart as they do every range pane. -->
      {#if !anyLogged}
        <p class="quiet">Nothing logged in this stretch yet.</p>
      {:else}
        <div class="chart">
          <p class="chart-name">
            {measure === 'kcal' ? 'Energy' : 'Carbohydrate'}, {rangeUnit} a day
            {#if goalShown}
              <span class="chart-goal">· target {Math.round(goalShown).toLocaleString()}</span>
            {/if}
          </p>
          <HealthBars
            points={totalBars}
            unit={rangeUnit}
            goal={goalShown}
            color={accent}
            height={150}
            label={barLabel}
            onpick={openDay}
          />
        </div>
        {#if anyImported}
          <div class="chart">
            <p class="chart-name">Of that, written by another app on your phone</p>
            <HealthBars
              points={importedBars}
              unit={rangeUnit}
              color="var(--fg-subtle)"
              height={70}
              label={barLabel}
              onpick={openDay}
            />
          </div>
        {/if}
        <p class="foot">The first chart counts everything. Pick a day to open it above.</p>
      {/if}
    </section>
  </div>
</div>

{#if health.editingMeal}
  {#key health.editingMeal.id}
    <HealthMealEditor />
  {/key}
{/if}

<style>
  .nutrition {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .page {
    max-width: 860px;
    margin: 0 auto;
    padding: var(--sp-2) var(--sp-6) var(--fab-clear);
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }

  .bar {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-1) 0;
  }
  .grow {
    flex: 1;
  }

  /* The day, between its two arrows, in the same rounded track as the
     filter chips so it reads as one control. */
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
    min-width: 8.5em;
    height: 26px;
    padding: 0 var(--sp-2);
    border-radius: 999px;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg);
    font-variant-numeric: tabular-nums;
  }
  .when:hover:not(:disabled) {
    background: var(--bg-raised);
  }
  .when:disabled {
    cursor: default;
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
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
  }

  .meters {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(190px, 1fr));
    gap: var(--sp-4) var(--sp-6);
  }
  .plain {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .plain-name {
    font-size: var(--text-base);
    color: var(--fg);
  }
  .plain-value {
    font-size: var(--text-md);
    font-weight: 600;
    color: var(--fg);
    font-variant-numeric: tabular-nums;
  }
  .plain-note {
    font-size: var(--text-xs);
    color: var(--fg-faint);
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
    color: var(--accent);
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .link:hover {
    text-decoration: underline;
  }

  .slot-head {
    display: flex;
    align-items: baseline;
    gap: var(--sp-3);
  }
  .slot-sum {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }

  .meals {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
  }
  .meal {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
  }
  .meal-main {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: var(--sp-2) var(--sp-3);
  }
  .meal-top {
    display: flex;
    align-items: baseline;
    gap: var(--sp-3);
  }
  .meal-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-base);
    font-weight: 600;
    color: var(--fg);
  }
  .meal-time {
    flex: none;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .meal-items {
    font-size: var(--text-sm);
    color: var(--fg-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .meal-macros {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .est {
    font-weight: 600;
    color: var(--fg-muted);
  }
  .meal-meter {
    padding: 0 calc(var(--sp-2) + var(--sp-3));
    max-width: 340px;
  }
  .meal-tools {
    padding: 0 calc(var(--sp-2) + var(--sp-3)) var(--sp-1);
  }

  .response {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    margin: 0 var(--sp-2) var(--sp-2);
    padding: var(--sp-3);
    border-radius: var(--radius);
    background: var(--bg-sunken);
  }
  .facts {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2) var(--sp-6);
    margin: 0;
  }
  .facts div {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .facts dt {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .facts dd {
    margin: 0;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg);
    font-variant-numeric: tabular-nums;
  }

  .chart {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    min-width: 0;
  }
  .chart-name {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .chart-goal {
    color: var(--fg-subtle);
  }
</style>
