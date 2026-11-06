<script lang="ts">
  // Screen time's half of the sidebar: its four panes, how long the summary
  // covers, the devices it can be narrowed to, and the way to this
  // computer's settings.
  //
  // The range sits under the panes as chips rather than as three more rows:
  // it is a setting of the summary, not a place to go, and a row of chips
  // is what every app uses for "narrow what is beside this". Today's total
  // sits where every other sidebar puts a count, on the Summary row, while
  // the summary is looking at today.
  //
  // Devices are one choice, not several: "All devices" or one machine. The
  // list comes from the vault, so a laptop that has not reported in a while
  // is still there to look back at.

  import { SCREEN_VIEWS, screentime, type ScreenView } from '../lib/screentime.svelte'
  import { formatDuration, type RangeKind } from '../lib/screentime'
  import { panels } from '../lib/panels.svelte'
  import { relativeTime } from '../lib/format'
  import type { IconName } from '../lib/icons'
  import Icon from './Icon.svelte'

  const LABELS: Record<ScreenView, string> = {
    summary: 'Summary',
    timeline: 'Timeline',
    apps: 'Apps & sites',
    categories: 'Categories',
  }
  const ICONS: Record<ScreenView, IconName> = {
    summary: 'chart',
    timeline: 'clock',
    apps: 'grid',
    categories: 'tag',
  }
  const RANGES: { id: RangeKind; label: string }[] = [
    { id: 'day', label: 'Day' },
    { id: 'week', label: 'Week' },
    { id: 'month', label: 'Month' },
  ]

  /** Today's total, when the summary on screen is today. */
  const today = $derived(
    screentime.range === 'day' && screentime.atPresent && screentime.report
      ? screentime.report.totalSecs
      : null,
  )

  const picked = $derived(screentime.devices.length === 1 ? screentime.devices[0] : null)
  const notTracking = $derived(screentime.status?.tracking === false)

  function openSettings() {
    panels.openSettings('screentime')
  }
</script>

<nav class="scroll side-nav" aria-label="Screen time">
  {#each SCREEN_VIEWS as view (view)}
    <button
      class="side-row"
      class:sel={screentime.view === view}
      aria-current={screentime.view === view ? 'page' : undefined}
      onclick={() => screentime.setView(view)}
    >
      <span class="side-icon"><Icon name={ICONS[view]} size={16} /></span>
      <span class="side-text">{LABELS[view]}</span>
      {#if view === 'summary' && today !== null && today > 0}
        <span class="side-count" title="Today so far">{formatDuration(today)}</span>
      {/if}
    </button>
  {/each}

  <div class="range">
    <div class="filters" role="group" aria-label="How long the summary covers">
      {#each RANGES as r (r.id)}
        <button
          class="filter"
          class:on={screentime.range === r.id}
          aria-pressed={screentime.range === r.id}
          onclick={() => {
            screentime.setRange(r.id)
            if (screentime.view !== 'summary') screentime.setView('summary')
          }}
        >
          {r.label}
        </button>
      {/each}
    </div>
  </div>

  <div class="side-head">
    <span class="eyebrow">Devices</span>
  </div>
  <button
    class="side-row"
    class:sel={picked === null}
    aria-pressed={picked === null}
    onclick={() => screentime.setDevices([])}
  >
    <span class="side-icon"><Icon name="layers" size={15} /></span>
    <span class="side-text">All devices</span>
  </button>
  {#each screentime.knownDevices as d (d.device)}
    <button
      class="side-row"
      class:sel={picked === d.device}
      aria-pressed={picked === d.device}
      title="{d.device} — last heard from {relativeTime(d.lastSeen)}"
      onclick={() => screentime.setDevices(picked === d.device ? [] : [d.device])}
    >
      <span class="side-icon"><Icon name="monitor" size={15} /></span>
      <span class="side-text">{d.device}</span>
      {#if screentime.status && d.device === screentime.status.device}
        <span class="side-count">this one</span>
      {/if}
    </button>
  {/each}
  {#if screentime.knownDevices.length === 0}
    <p class="hint">None has reported yet.</p>
  {/if}

  <button class="side-row settings" onclick={openSettings}>
    <span class="side-icon"><Icon name="settings" size={15} /></span>
    <span class="side-text">This computer…</span>
    {#if notTracking}
      <span class="side-count">off</span>
    {/if}
  </button>
</nav>

<style>
  /* The rows, headings and counts are the shared sidebar's -- see app.css's
     "Sidebars". What is Screen time's own: the range chips under the panes,
     and the settings row kept apart at the foot. */

  .range {
    display: flex;
    padding: var(--sp-2) var(--sp-2) 0;
  }
  .range .filters {
    flex: 1;
  }
  .range .filter {
    flex: 1;
    justify-content: center;
  }

  .hint {
    margin: 0;
    padding: var(--sp-1) var(--sp-3) var(--sp-2);
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }

  .settings {
    margin-top: var(--sp-4);
    color: var(--fg-subtle);
  }
</style>
