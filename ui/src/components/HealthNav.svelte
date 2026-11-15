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
  // The icons are filled and two-tone like every other sidebar's, drawn here
  // because the shared set has no heart or pill. Same grid, same layers
  // (see `symbols.ts`): the bigger shape is the front, the supporting one the
  // back, detail painted at full strength as a mark.

  import { HEALTH_VIEWS, health, type HealthView } from '../lib/health.svelte'
  import { panels } from '../lib/panels.svelte'
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

  interface Art {
    back?: string
    front: string
    mark?: string
  }

  const ART: Record<HealthView, Art> = {
    // The sun: rays behind, disc in front.
    today: {
      back: '<path class="s" d="M12 2.5v2.2M12 19.3v2.2M2.5 12h2.2M19.3 12h2.2M5.3 5.3l1.6 1.6M17.1 17.1l1.6 1.6M5.3 18.7l1.6-1.6M17.1 6.9l1.6-1.6"/>',
      front: '<circle cx="12" cy="12" r="4.8"/>',
    },
    // Rings: three meters, drawn as the ring they replace in other apps' icons.
    activity: {
      back: '<circle cx="12" cy="12" r="9.5"/>',
      front:
        '<path fill-rule="evenodd" d="M12 5.8a6.2 6.2 0 1 0 0 12.4 6.2 6.2 0 0 0 0-12.4Zm0 2.7a3.5 3.5 0 1 1 0 7 3.5 3.5 0 0 1 0-7Z"/>',
      mark: '<circle cx="12" cy="12" r="1.7"/>',
    },
    // The moon, and a star beside it.
    sleep: {
      back: '<circle cx="17.6" cy="6.4" r="1.7"/>',
      front: '<path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z"/>',
    },
    // A heart with its pulse.
    vitals: {
      back: '<path d="M19 14c1.49-1.46 3-3.21 3-5.5A5.5 5.5 0 0 0 16.5 3c-1.76 0-3 .5-4.5 2-1.5-1.5-2.74-2-4.5-2A5.5 5.5 0 0 0 2 8.5c0 2.3 1.5 4.05 3 5.5l7 7Z"/>',
      front: '',
      mark: '<path class="s" d="M5.5 12.2h3.2l1.8-3 3 6 1.8-3h3.2"/>',
    },
    // An apple and its leaf.
    nutrition: {
      back: '<path d="M12 8.2c0-2.6 1.5-4.4 4-4.7 0 2.5-1.5 4.5-4 4.7Z"/>',
      front:
        '<path d="M12 8.6C10.2 7.2 7.4 7 5.6 8.8 3.6 10.8 4 14.2 5.7 17.2c1.1 2 2.7 3.6 4.1 3.6.9 0 1.4-.5 2.2-.5s1.3.5 2.2.5c1.4 0 3-1.6 4.1-3.6 1.7-3 2.1-6.4.1-8.4-1.8-1.8-4.6-1.6-6.4-.2Z"/>',
    },
    // A capsule, half of it solid.
    medications: {
      back: '<rect x="2.5" y="8.5" width="19" height="7" rx="3.5" transform="rotate(-45 12 12)"/>',
      front: '<path transform="rotate(-45 12 12)" d="M6 8.5h6v7H6a3.5 3.5 0 0 1 0-7Z"/>',
    },
    // A clipboard with a cross.
    conditions: {
      back: '<rect x="4.5" y="4" width="15" height="17.5" rx="3"/>',
      front: '<rect x="8.5" y="2.5" width="7" height="4.5" rx="1.6"/>',
      mark: '<path class="s" d="M12 10.5v6M9 13.5h6"/>',
    },
    // A folder.
    records: {
      back: '<path d="M3 7.5A2.5 2.5 0 0 1 5.5 5h3.7a2 2 0 0 1 1.5.7L12 7h6.5A2.5 2.5 0 0 1 21 9.5v8a2.5 2.5 0 0 1-2.5 2.5h-13A2.5 2.5 0 0 1 3 17.5Z"/>',
      front: '<path d="M3 10.5h18v7a2.5 2.5 0 0 1-2.5 2.5h-13A2.5 2.5 0 0 1 3 17.5Z"/>',
    },
  }

  /** Doses that were due and have not been taken or skipped. */
  const overdue = $derived(health.dosesDue.filter((d) => !d.dose && d.overdue).length)

  function go(view: HealthView) {
    // Choosing a pane that has a thing open goes back to the pane itself.
    if (view === 'activity' && health.workoutId) health.openWorkout(null)
    if (view === 'conditions' && health.conditionId) health.openCondition(null)
    if (view === 'records' && health.documentId) health.openDocument(null)
    health.setView(view)
  }

  function openSettings() {
    panels.openSettings('health')
  }
</script>

<nav class="scroll side-nav" aria-label="Health">
  {#each HEALTH_VIEWS as view (view)}
    {@const art = ART[view]}
    <button
      class="side-row"
      class:sel={health.view === view && !(view === 'conditions' && health.conditionId)}
      aria-current={health.view === view && !(view === 'conditions' && health.conditionId)
        ? 'page'
        : undefined}
      onclick={() => go(view)}
    >
      <span class="side-icon">
        <svg
          class="symbol"
          width="17"
          height="17"
          viewBox="0 0 24 24"
          aria-hidden="true"
          focusable="false"
        >
          <!-- eslint-disable svelte/no-at-html-tags -- compile-time constants above -->
          {#if art.back}<g class="back">{@html art.back}</g>{/if}
          {#if art.front}<g class="front">{@html art.front}</g>{/if}
          {#if art.mark}<g class="mark">{@html art.mark}</g>{/if}
          <!-- eslint-enable svelte/no-at-html-tags -->
        </svg>
      </span>
      <span class="side-text">{LABELS[view]}</span>
      {#if view === 'medications' && overdue > 0}
        <span class="side-count strong" title="Doses to log">{overdue}</span>
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
    <span class="side-text">Health settings…</span>
  </button>
</nav>

<style>
  /* The rows, headings and counts are the shared sidebar's -- see app.css's
     "Sidebars". What is Health's own: the drawn icons, the condition rows
     tucked under Conditions, and the settings row kept apart at the foot. */

  .symbol {
    display: block;
    flex: none;
    overflow: visible;
  }
  .symbol g {
    stroke-width: 0;
  }
  .back,
  .front,
  .mark {
    fill: currentColor;
    stroke: currentColor;
  }
  .back {
    opacity: var(--symbol-back, 0.38);
  }
  .symbol :global(.s) {
    fill: none;
    stroke-width: 2.3;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

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
