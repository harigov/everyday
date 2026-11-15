<script lang="ts">
  // Activity over the window: how much the person moved, a day at a time,
  // and every workout in it.
  //
  // Three questions, three charts, each with the goal it is held against as a
  // dashed line: energy burned moving (active energy), minutes of exercise,
  // and steps. Not rings -- the house has no donuts, and a column per day
  // answers "how has this been going" where one ring only answers "how is
  // today". A day's bar opens that day on Today.
  //
  // The averages are over days something was recorded on, so a week that
  // starts on Sunday and has been looked at on Tuesday is not averaged with
  // four days that have not happened yet.
  //
  // A workout opens in place of the list, which is HealthWorkout; its Back
  // brings the list again.

  import { health } from '../lib/health.svelte'
  import { activityName, durationText } from '../lib/health'
  import { dateFormat, formatInstantTime, plural } from '../lib/format'
  import { locale, minutesBetween } from '../lib/time'
  import type { ActivityDay } from '../lib/types'
  import HealthBars from './HealthBars.svelte'
  import HealthWorkout from './HealthWorkout.svelte'
  import StatTile from './StatTile.svelte'

  const MOVE = 'oklch(var(--app-lightness) var(--app-chroma) 25)'
  const EXERCISE = 'oklch(var(--app-lightness) var(--app-chroma) 148)'
  const STEPS = 'oklch(var(--app-lightness) var(--app-chroma) 232)'

  const days = $derived(health.activity)
  const goals = $derived(health.settings?.goals ?? null)

  /** Days that have something on them. */
  const recorded = $derived(
    days.filter((d) => d.activeKcal > 0 || d.steps > 0 || d.exerciseMin > 0),
  )
  const mean = (f: (d: ActivityDay) => number) =>
    recorded.length > 0 ? recorded.reduce((sum, d) => sum + f(d), 0) / recorded.length : 0

  const count = (n: number) => Math.round(n).toLocaleString(locale())

  const energyUnit = $derived(health.displayUnit('active_energy') || 'kcal')
  const exerciseUnit = $derived(health.displayUnit('exercise_time') || 'min')
  const stepsUnit = $derived(health.displayUnit('steps') || 'steps')

  const energy = $derived(
    days.map((d) => ({ at: d.date, value: health.toDisplay('active_energy', d.activeKcal) })),
  )
  const exercise = $derived(days.map((d) => ({ at: d.date, value: d.exerciseMin })))
  const steps = $derived(days.map((d) => ({ at: d.date, value: d.steps })))

  const energyGoal = $derived(
    goals && goals.moveKcal > 0 ? health.toDisplay('active_energy', goals.moveKcal) : null,
  )

  const workouts = $derived(
    [...health.workouts].sort((a, b) => Date.parse(b.start) - Date.parse(a.start)),
  )

  /** A bar opens its day on Today. */
  function openDay(p: { at: string }) {
    health.setDate(p.at)
    health.setView('today')
  }

  const dayText = (start: string) =>
    dateFormat({ weekday: 'short', day: 'numeric', month: 'short' }).format(new Date(start))
</script>

{#if health.workoutId}
  <HealthWorkout />
{:else}
  <div class="activity scroll">
    <div class="page">
      <div class="tiles">
        <StatTile
          label="Move, a day on average"
          value={recorded.length > 0
            ? health.display(
                'active_energy',
                mean((d) => d.activeKcal),
              )
            : '—'}
          note={recorded.length > 0
            ? `Over ${plural(recorded.length, 'day')}`
            : 'Nothing recorded yet'}
        />
        <StatTile
          label="Exercise, a day on average"
          value={recorded.length > 0 ? durationText(mean((d) => d.exerciseMin)) : '—'}
        />
        <StatTile
          label="Steps, a day on average"
          value={recorded.length > 0 ? count(mean((d) => d.steps)) : '—'}
        />
        <StatTile
          label="Workouts"
          value={String(workouts.length)}
          note={workouts.length === 0 ? 'None in this window' : undefined}
        />
      </div>

      <section class="card" aria-labelledby="ha-move">
        <h2 class="eyebrow" id="ha-move">Active energy</h2>
        <HealthBars
          points={energy}
          unit={energyUnit}
          goal={energyGoal}
          color={MOVE}
          onpick={openDay}
        />
      </section>

      <section class="card" aria-labelledby="ha-exercise">
        <h2 class="eyebrow" id="ha-exercise">Exercise</h2>
        <HealthBars
          points={exercise}
          unit={exerciseUnit}
          goal={goals && goals.exerciseMin > 0 ? goals.exerciseMin : null}
          color={EXERCISE}
          onpick={openDay}
        />
      </section>

      <section class="card" aria-labelledby="ha-steps">
        <h2 class="eyebrow" id="ha-steps">Steps</h2>
        <HealthBars
          points={steps}
          unit={stepsUnit}
          goal={goals && goals.steps > 0 ? goals.steps : null}
          color={STEPS}
          onpick={openDay}
        />
      </section>

      <section class="card" aria-labelledby="ha-workouts">
        <h2 class="eyebrow" id="ha-workouts">Workouts</h2>
        {#if workouts.length === 0}
          <p class="quiet">No workouts in this window.</p>
        {:else}
          <ul class="rows">
            {#each workouts as w (w.id)}
              <li>
                <button class="list-row workout" onclick={() => health.openWorkout(w.id)}>
                  <span class="what">
                    <span class="name">{w.name || activityName(w.activity)}</span>
                    <span class="when">{dayText(w.start)} · {formatInstantTime(w.start)}</span>
                  </span>
                  <span class="fig">{durationText(minutesBetween(w.start, w.end))}</span>
                  <span class="fig">
                    {w.distanceM ? health.display('walking_running_distance', w.distanceM) : ''}
                  </span>
                  <span class="fig">
                    {w.activeEnergyKcal ? health.display('active_energy', w.activeEnergyKcal) : ''}
                  </span>
                  <span class="fig">{w.avgHr ? health.display('heart_rate', w.avgHr) : ''}</span>
                </button>
              </li>
            {/each}
          </ul>
        {/if}
      </section>
    </div>
  </div>
{/if}

<style>
  .activity {
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

  .rows {
    display: flex;
    flex-direction: column;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  /* The shared `.list-row` is drawn for a column of its own; inside a card it
     fills the card instead of being inset from it. Every figure keeps its
     column whether or not the workout has it, so a row without a distance
     does not pull its heart rate under somebody else's energy. */
  .workout {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 5.5em 5.5em 5.5em 5.5em;
    align-items: center;
    gap: var(--sp-3);
    width: 100%;
    margin-inline: 0;
    padding: var(--sp-2) var(--sp-3);
  }
  .what {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg);
    font-size: var(--text-base);
  }
  .when {
    color: var(--fg-subtle);
    font-size: var(--text-xs);
  }
  .fig {
    text-align: right;
    color: var(--fg-muted);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
  }
</style>
