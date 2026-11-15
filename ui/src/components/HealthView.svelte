<script lang="ts">
  // Health's main pane: a heading, the stretch of time it is looking at, and
  // whichever of the eight panes the sidebar has chosen.
  //
  // The heading is drawn here once for every pane, so moving between them
  // never moves the title. What sits beside it follows the pane: Today steps
  // a day at a time; Activity, Sleep, Vitals and Nutrition share one window
  // -- a week, a month, three months, a year -- that is stepped and resized
  // here and read by all four; Medications, Conditions and Records are not
  // about a stretch of time and show nothing. An open workout is a place of
  // its own and hides the window too.
  //
  // Until nothing at all has arrived -- no source, no reading -- the panes
  // that would only be empty say how to start instead. Everything else
  // (medications, conditions, records) stays reachable: a care plan can be
  // written before the first sample comes in.

  import {
    HEALTH_RANGES,
    HEALTH_RANGE_VIEWS,
    health,
    type HealthRange,
    type HealthView as Pane,
  } from '../lib/health.svelte'
  import { app } from '../lib/state.svelte'
  import { panels } from '../lib/panels.svelte'
  import { longDate } from '../lib/format'
  import { todayIso } from '../lib/time'
  import EmptyState from './EmptyState.svelte'
  import HealthActivity from './HealthActivity.svelte'
  import HealthConditions from './HealthConditions.svelte'
  import HealthMedications from './HealthMedications.svelte'
  import HealthNutrition from './HealthNutrition.svelte'
  import HealthReadingDialog from './HealthReadingDialog.svelte'
  import HealthRecords from './HealthRecords.svelte'
  import HealthSleep from './HealthSleep.svelte'
  import HealthToday from './HealthToday.svelte'
  import HealthVitals from './HealthVitals.svelte'
  import Icon from './Icon.svelte'

  // Loaded when the view appears, so a vault whose owner never opens Health
  // never reads a sample.
  void health.start()

  const view = $derived(health.view)

  const TITLES: Record<Pane, string> = {
    today: 'Today',
    activity: 'Activity',
    sleep: 'Sleep',
    vitals: 'Vitals',
    nutrition: 'Nutrition',
    medications: 'Medications',
    conditions: 'Conditions',
    records: 'Records',
  }

  const RANGE_LABELS: Record<HealthRange, string> = {
    week: 'Week',
    month: 'Month',
    quarter: '3 months',
    year: 'Year',
  }

  const day = $derived(health.date || todayIso())

  /** The panes that look at the shared window. An open workout hides it. */
  const ranged = $derived(
    HEALTH_RANGE_VIEWS.includes(view) && !(view === 'activity' && health.workoutId),
  )

  const sub = $derived(view === 'today' ? longDate(day) : '')

  /** Nothing has ever arrived: the panes that are only charts say how to start. */
  const hasData = $derived(
    health.latest.length > 0 ||
      (health.today?.latest.length ?? 0) > 0 ||
      (health.today?.activity.steps ?? 0) > 0 ||
      health.activity.some((d) => d.activeKcal > 0 || d.steps > 0 || d.exerciseMin > 0) ||
      health.sleep.some((n) => n.asleepMin > 0),
  )
  const blank = $derived(
    health.loaded &&
      health.sources.length === 0 &&
      !hasData &&
      (view === 'today' || view === 'activity' || view === 'sleep' || view === 'vitals'),
  )

  function openSettings() {
    panels.openSettings('health')
  }
</script>

<main class="health">
  <header class="top">
    <div class="pane-titles">
      <h1 class="pane-title">{TITLES[view]}</h1>
      {#if sub && !blank}<span class="pane-sub">{sub}</span>{/if}
    </div>

    {#if app.supportsHealth && !blank && (view === 'today' || ranged)}
      <div class="tools">
        {#if ranged}
          <div class="filters" role="group" aria-label="How much time to show">
            {#each HEALTH_RANGES as r (r)}
              <button
                class="filter"
                class:on={health.range === r}
                aria-pressed={health.range === r}
                onclick={() => health.setRange(r)}
              >
                {RANGE_LABELS[r]}
              </button>
            {/each}
          </div>
        {/if}
        {#if !health.atPresent}
          <button class="btn btn-outline today" onclick={() => health.goToday()}>Today</button>
        {/if}
        <div class="stepper" role="group" aria-label={ranged ? 'Range' : 'Day'}>
          <button
            class="step"
            aria-label={ranged ? 'The one before' : 'The day before'}
            title={ranged ? 'The one before' : 'The day before'}
            onclick={() => health.step(-1)}
          >
            <span class="back"><Icon name="chevron" size={13} /></span>
          </button>
          <span class="stretch" aria-live="polite">{health.rangeLabel}</span>
          <button
            class="step"
            aria-label={ranged ? 'The one after' : 'The day after'}
            title={ranged ? 'The one after' : 'The day after'}
            disabled={health.atPresent}
            onclick={() => health.step(1)}
          >
            <Icon name="chevron" size={13} />
          </button>
        </div>
      </div>
    {/if}
  </header>

  {#if health.error}
    <div class="error problem" role="alert">
      <span>{health.error}</span>
      <button class="dismiss" aria-label="Dismiss" onclick={() => (health.error = null)}>
        <Icon name="close" size={12} />
      </button>
    </div>
  {/if}

  {#if !app.supportsHealth}
    <EmptyState lead="Health is not available here.">
      {#snippet icon()}<Icon name="info" size={30} weight={1.3} />{/snippet}
      {#snippet note()}
        This vault cannot keep health data. Open one on a computer where Karma Yoga runs, or ask
        whoever runs this one to turn Health on.
      {/snippet}
    </EmptyState>
  {:else if blank}
    <EmptyState lead="No health data yet.">
      {#snippet icon()}<Icon name="info" size={30} weight={1.3} />{/snippet}
      {#snippet note()}
        Bring in your history from Apple Health, or let an iPhone send new readings as they happen.
        You can also add a reading by hand.
      {/snippet}
      {#snippet action()}
        <div class="actions">
          <button class="btn btn-primary" onclick={openSettings}>Connect Apple Health</button>
          <button class="btn" onclick={() => (health.readingOpen = true)}>Add a reading</button>
        </div>
      {/snippet}
    </EmptyState>
  {:else if view === 'today'}
    <HealthToday />
  {:else if view === 'activity'}
    <HealthActivity />
  {:else if view === 'sleep'}
    <HealthSleep />
  {:else if view === 'vitals'}
    <HealthVitals />
  {:else if view === 'nutrition'}
    <HealthNutrition />
  {:else if view === 'medications'}
    <HealthMedications />
  {:else if view === 'conditions'}
    <HealthConditions />
  {:else if view === 'records'}
    <HealthRecords />
  {/if}
</main>

{#if app.supportsHealth && health.readingOpen}
  <HealthReadingDialog />
{/if}

<style>
  .health {
    /* As tall as the stage by a stated height, not stretched: see
       `.shell-stage` in app.css. */
    height: 100%;
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }

  .top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    height: var(--header-h);
    padding: 0 var(--sp-4) 0 var(--sp-6);
    flex: none;
  }

  .tools {
    flex: none;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .today {
    height: 30px;
    font-size: var(--text-sm);
  }

  /* The stretch of time between its two arrows, in the chips' rounded
     track, so it reads as one control -- the shape Money's month and Screen
     time's range have. */
  .stepper {
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
  .stretch {
    min-width: 8.5em;
    padding: 0 var(--sp-2);
    text-align: center;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .problem {
    flex: none;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin: 0 var(--sp-6) var(--sp-2);
    overflow-wrap: anywhere;
  }
  .problem span {
    flex: 1;
    min-width: 0;
  }
  .dismiss {
    flex: none;
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: 50%;
    color: inherit;
  }
  .dismiss:hover {
    background: color-mix(in oklab, var(--danger) 16%, transparent);
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: var(--sp-2);
  }
</style>
