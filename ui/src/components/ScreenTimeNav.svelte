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
  import { app } from '../lib/state.svelte'
  import { panels } from '../lib/panels.svelte'
  import { relativeTime } from '../lib/format'
  import type { IconName } from '../lib/icons'
  import Symbol from './Symbol.svelte'

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

  /** Recording is set up on the computer that holds the vault, so from here
   *  the row is not "this computer's" settings. */
  const elsewhere = $derived(app.remote || (!screentime.status && !!screentime.statusError))

  /** Devices narrow Summary and Timeline only; the other two panes count
   *  every device, so the block is dimmed there and says so. */
  const devicesApply = $derived(screentime.view === 'summary' || screentime.view === 'timeline')
  const devicesTitle = 'Applies to Summary and Timeline'

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
      <span class="side-icon"><Symbol name={ICONS[view]} size={17} /></span>
      <span class="side-text">{LABELS[view]}</span>
      {#if view === 'summary' && today !== null && today > 0}
        <span class="side-count" title="Today so far">{formatDuration(today)}</span>
      {/if}
    </button>
  {/each}

  <div class="range desktop-only">
    <div
      class="filters"
      role="group"
      aria-label="How long the summary covers"
      title="Applies to Summary"
    >
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

  <div class="side-head" class:dim={!devicesApply} title={devicesTitle}>
    <span class="eyebrow">Devices</span>
  </div>
  <button
    class="side-row"
    class:sel={picked === null}
    class:dim={!devicesApply}
    title={devicesTitle}
    aria-pressed={picked === null}
    onclick={() => screentime.setDevices([])}
  >
    <span class="side-icon"><Symbol name="layers" size={16} /></span>
    <span class="side-text">All devices</span>
  </button>
  {#each screentime.knownDevices as d (d.device)}
    <button
      class="side-row"
      class:sel={picked === d.device}
      class:dim={!devicesApply}
      aria-pressed={picked === d.device}
      title="{d.device} — last heard from {relativeTime(d.lastSeen)}"
      onclick={() => screentime.setDevices(picked === d.device ? [] : [d.device])}
    >
      <span class="side-icon"><Symbol name="monitor" size={16} /></span>
      <span class="side-text">{d.device}</span>
      {#if screentime.status && d.device === screentime.status.device}
        <span class="tag">this one</span>
      {/if}
    </button>
  {/each}
  {#if screentime.knownDevices.length === 0}
    <p class="hint">None has reported yet.</p>
  {/if}

  <button class="side-row settings" onclick={openSettings}>
    <span class="side-icon"><Symbol name="settings" size={16} /></span>
    <span class="side-text">{elsewhere ? 'Screen time settings…' : 'This computer…'}</span>
    {#if notTracking && !app.remote}
      <span class="tag">off</span>
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

  /* Still clickable, only quieter: the choice is kept for when the summary or
     the timeline is back. */
  .dim {
    opacity: 0.55;
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
