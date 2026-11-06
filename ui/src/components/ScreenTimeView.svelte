<script lang="ts">
  // Screen time's main pane: a heading, the stretch of time it is looking
  // at, and whichever of the four panes the sidebar has chosen.
  //
  // The heading is drawn here once for every pane, so moving between them
  // never moves the title. The arrows beside it step what the pane is about:
  // the summary's day, week or month, or the timeline's one day. The apps
  // and categories are not about a stretch of time and show none.
  //
  // Until anything at all has been recorded -- no device has ever reported
  // -- the summary and the timeline say how to start instead of drawing
  // empty charts: this computer records by itself, GNOME needs its
  // extension, and websites need a browser connected. Every way in is a
  // button to Settings → Screen time, where each is done.

  import { formatDuration } from '../lib/screentime'
  import { screentime, type ScreenView as Pane } from '../lib/screentime.svelte'
  import { panels } from '../lib/panels.svelte'
  import { friendlyDate, plural } from '../lib/format'
  import { addDays, todayIso } from '../lib/time'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import ScreenTimeApps from './ScreenTimeApps.svelte'
  import ScreenTimeCategories from './ScreenTimeCategories.svelte'
  import ScreenTimeSummary from './ScreenTimeSummary.svelte'
  import ScreenTimeTimeline from './ScreenTimeTimeline.svelte'

  // Loaded when the view appears, so a vault whose owner never opens Screen
  // time never reads a report. `ready` is the view's own: "nothing recorded
  // yet" for the length of one read is a flash of the wrong sentence.
  let ready = $state(false)
  void screentime.start().finally(() => (ready = true))

  const view = $derived(screentime.view)
  const nothing = $derived(ready && screentime.knownDevices.length === 0)
  const blank = $derived(nothing && (view === 'summary' || view === 'timeline'))
  const status = $derived(screentime.status)
  const gnomeWaiting = $derived(
    status?.backend === 'gnome' && !!status.extension && !status.extension.active,
  )

  const TITLES: Record<Pane, string> = {
    summary: 'Summary',
    timeline: 'Timeline',
    apps: 'Apps & sites',
    categories: 'Categories',
  }

  const timelineDate = $derived(screentime.timelineDate || todayIso())
  const dated = $derived(view === 'summary' || view === 'timeline')
  const atPresent = $derived(
    view === 'timeline' ? timelineDate >= todayIso() : screentime.atPresent,
  )
  const stretch = $derived(view === 'timeline' ? friendlyDate(timelineDate) : screentime.rangeLabel)

  const deviceWords = $derived(
    screentime.devices.length === 0
      ? 'All devices'
      : screentime.devices.length === 1
        ? screentime.devices[0]
        : plural(screentime.devices.length, 'device'),
  )

  const sub = $derived.by(() => {
    switch (view) {
      case 'summary': {
        const filter = screentime.categoryOf(screentime.category)
        return [deviceWords, filter?.name].filter(Boolean).join(' · ')
      }
      case 'timeline': {
        const runs = screentime.timeline?.runs ?? []
        const secs = runs.reduce(
          (sum, r) => sum + Math.max(0, (Date.parse(r.end) - Date.parse(r.start)) / 1000),
          0,
        )
        return secs > 0 ? `${deviceWords} · ${formatDuration(secs)}` : deviceWords
      }
      case 'apps': {
        const n = screentime.apps.length
        return n > 0 ? `${plural(n, 'app or site', 'apps and sites')} · the last 30 days` : ''
      }
      case 'categories': {
        const n = screentime.categories.length
        return n > 0 ? plural(n, 'category', 'categories') : ''
      }
      default:
        return ''
    }
  })

  function step(delta: -1 | 1) {
    if (view === 'timeline') screentime.openTimeline(addDays(timelineDate, delta))
    else screentime.step(delta)
  }

  function goToday() {
    if (view === 'timeline') screentime.openTimeline(todayIso())
    else screentime.goToday()
  }

  function openSettings() {
    panels.openSettings('screentime')
  }
</script>

<main class="screentime">
  <header class="top">
    <div class="pane-titles">
      <h1 class="pane-title">{TITLES[view]}</h1>
      {#if sub && !blank}<span class="pane-sub">{sub}</span>{/if}
    </div>

    {#if dated && !blank}
      <div class="tools">
        {#if !atPresent}
          <button class="btn btn-outline today" onclick={goToday}>Today</button>
        {/if}
        <div class="stepper" role="group" aria-label={view === 'timeline' ? 'Day' : 'Range'}>
          <button
            class="step"
            aria-label={view === 'timeline' ? 'The day before' : 'The one before'}
            title={view === 'timeline' ? 'The day before' : 'The one before'}
            onclick={() => step(-1)}
          >
            <span class="back"><Icon name="chevron" size={13} /></span>
          </button>
          <span class="stretch" aria-live="polite">{stretch}</span>
          <button
            class="step"
            aria-label={view === 'timeline' ? 'The day after' : 'The one after'}
            title={view === 'timeline' ? 'The day after' : 'The one after'}
            disabled={atPresent}
            onclick={() => step(1)}
          >
            <Icon name="chevron" size={13} />
          </button>
        </div>
      </div>
    {/if}
  </header>

  {#if status?.tracking === false && !blank}
    <p class="note" role="status">
      This computer is not recording its screen time.
      <button class="inline" onclick={openSettings}>Turn it on</button>
    </p>
  {/if}

  {#if blank}
    <EmptyState lead="No screen time recorded yet.">
      {#snippet icon()}<Icon name="monitor" size={30} weight={1.3} />{/snippet}
      {#snippet note()}
        {#if status?.tracking === false}
          Recording is switched off on this computer. Turn it on in Settings → Screen time and the
          app in front of you is counted from then on, minus any time away from the keyboard.
        {:else if gnomeWaiting}
          On GNOME, the window in front of you can only be seen from inside the desktop, so a small
          Shell extension does the watching. Install it from Settings → Screen time, then log out
          and back in. Websites count once a browser is connected too.
        {:else}
          This computer records which app is in front of you by itself while Every Day is open,
          leaving out time away from the keyboard. Websites count as their own apps once a browser
          is connected. Nothing is ever blocked.
        {/if}
      {/snippet}
      {#snippet action()}
        <div class="actions">
          <button class="btn btn-primary" onclick={openSettings}>
            {gnomeWaiting ? 'Install the GNOME extension' : 'Set up this computer'}
          </button>
          <button class="btn" onclick={openSettings}>Connect a browser</button>
        </div>
      {/snippet}
    </EmptyState>
  {:else if view === 'summary'}
    <ScreenTimeSummary />
  {:else if view === 'timeline'}
    <ScreenTimeTimeline />
  {:else if view === 'apps'}
    <ScreenTimeApps />
  {:else if view === 'categories'}
    <ScreenTimeCategories />
  {/if}
</main>

<style>
  .screentime {
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
     track, so it reads as one control -- the shape Money's month has. */
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

  .note {
    flex: none;
    margin: 0 var(--sp-6) var(--sp-2);
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .inline {
    color: var(--accent);
    font-size: inherit;
    font-weight: 550;
  }
  .inline:hover {
    text-decoration: underline;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: var(--sp-2);
  }
</style>
