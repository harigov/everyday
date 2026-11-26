<script lang="ts">
  // Health's half of the sidebar: its eight panes, the conditions being
  // managed under Conditions, and the way to Health's settings.
  //
  // Each condition is a row of its own under Conditions that opens straight
  // to it -- somebody managing diabetes opens the sidebar to get to
  // diabetes, not to the list that happens to hold it. The one count in the
  // sidebar is on Medications: doses that are due and have not been logged,
  // which is the thing in here that wants doing today.
  //
  // The icons are filled and two-tone like every other sidebar's: the
  // `health-*` symbols in `symbols.ts`.

  import { HEALTH_VIEWS, health, type HealthView } from '../lib/health.svelte'
  import { panels } from '../lib/panels.svelte'
  import { details } from '../lib/phone.svelte'
  import Symbol from './Symbol.svelte'

  const LABELS: Record<HealthView, string> = {
    today: 'Today',
    activity: 'Activity',
    sleep: 'Sleep',
    vitals: 'Vitals',
    nutrition: 'Nutrition',
    medications: 'Medications',
    conditions: 'Conditions',
    records: 'Records',
  }

  /** Doses that were due and have not been taken or skipped. */
  const overdue = $derived(health.dosesDue.filter((d) => !d.dose && d.overdue).length)

  function go(view: HealthView) {
    // Choosing a pane that has a thing open goes back to the pane itself.
    if (view === 'activity' && health.workoutId) health.openWorkout(null)
    if (view === 'conditions' && health.conditionId) health.openCondition(null)
    if (view === 'records' && health.documentId) health.openDocument(null)
    // ...and Vitals again, from its measurement, to its list (a phone's).
    if (view === 'vitals') details.close('health')
    health.setView(view)
  }

  function openSettings() {
    panels.openSettings('health')
  }
</script>

<nav class="scroll side-nav" aria-label="Health">
  {#each HEALTH_VIEWS as view (view)}
    <button
      class="side-row"
      class:sel={health.view === view && !(view === 'conditions' && health.conditionId)}
      aria-current={health.view === view && !(view === 'conditions' && health.conditionId)
        ? 'page'
        : undefined}
      onclick={() => go(view)}
    >
      <span class="side-icon"><Symbol name={`health-${view}`} size={17} /></span>
      <span class="side-text">{LABELS[view]}</span>
      {#if view === 'medications' && overdue > 0}
        <span class="side-count strong" title="Doses to log"
          >{overdue}<span class="sr-only"> {overdue === 1 ? 'dose' : 'doses'} to log</span></span
        >
      {/if}
    </button>

    {#if view === 'conditions'}
      {#each health.conditions as condition (condition.id)}
        {@const open = health.view === 'conditions' && health.conditionId === condition.id}
        <button
          class="side-row sub"
          class:sel={open}
          aria-current={open ? 'page' : undefined}
          title={condition.name}
          onclick={() => health.openCondition(condition.id)}
        >
          <span class="side-dot"></span>
          <span class="side-text">{condition.name}</span>
          {#if condition.status !== 'active'}
            <span class="side-count"
              >{condition.status === 'remission' ? 'in remission' : 'resolved'}</span
            >
          {/if}
        </button>
      {/each}
    {/if}
  {/each}

  <button class="side-row settings" onclick={openSettings}>
    <span class="side-icon"><Symbol name="settings" size={16} /></span>
    <span class="side-text">Health settings</span>
  </button>
</nav>

<style>
  /* The rows, headings and counts are the shared sidebar's -- see app.css's
     "Sidebars". What is Health's own: the condition rows
     tucked under Conditions, and the settings row kept apart at the foot. */

  /* A condition, a step in from its parent: the dot sits under the parent's
     icon and the name is the quieter of the two. */
  .sub {
    padding-left: calc(var(--sp-3) + 6px);
    font-size: var(--text-sm);
  }

  .settings {
    margin-top: var(--sp-4);
    color: var(--fg-subtle);
  }
</style>
