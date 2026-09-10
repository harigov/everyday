<script lang="ts">
  // The calendar app's main pane: a header, a grid, and the rail beside it.
  //
  // The header carries three controls and no more. Which days (‹ today ›),
  // how many (day / week / month), and which of plan and record to draw —
  // that last one being the only control here that another calendar
  // application would not have, and the reason this one is worth using.

  import { onMount } from 'svelte'
  import { calendar, type Layer, type View } from '../lib/calendar.svelte'
  import { formatMinutes } from '../lib/format'
  import { locale } from '../lib/time'
  import Icon from './Icon.svelte'
  import TimeGrid from './TimeGrid.svelte'
  import MonthGrid from './MonthGrid.svelte'
  import EventDetail from './EventDetail.svelte'
  import TrackNow from './TrackNow.svelte'
  import type { IconName } from '../lib/icons'

  void calendar.start()
  // Readings are written in the journal app, under an entry, so coming back
  // to the calendar is the moment this store can learn about them. `start`
  // only runs once in a session; this runs every time the app is shown.
  onMount(() => void calendar.refreshReadings())

  const VIEWS: { id: View; label: string; icon: IconName; key: string }[] = [
    { id: 'day', label: 'Day', icon: 'day', key: 'D' },
    { id: 'week', label: 'Week', icon: 'week', key: 'W' },
    { id: 'month', label: 'Month', icon: 'month', key: 'M' },
  ]

  const LAYERS: { id: Layer; label: string; title: string }[] = [
    { id: 'both', label: 'Both', title: 'Plans and what actually happened' },
    { id: 'planned', label: 'Plan', title: 'Only what you meant to do' },
    { id: 'actual', label: 'Record', title: 'Only what you actually did' },
  ]

  /** What the header calls the window on screen. */
  const heading = $derived.by(() => {
    const days = calendar.days
    const first = new Date(days[0] + 'T00:00')
    const last = new Date(days[days.length - 1] + 'T00:00')
    if (calendar.view === 'day') {
      return new Intl.DateTimeFormat(locale(), {
        weekday: 'long',
        day: 'numeric',
        month: 'long',
      }).format(first)
    }
    if (calendar.view === 'month') {
      return new Intl.DateTimeFormat(locale(), { month: 'long', year: 'numeric' }).format(
        new Date(calendar.anchor + 'T00:00'),
      )
    }
    // A week that straddles two months should say so, and one that straddles
    // two years doubly so. `formatRange` decides all of that for itself and
    // in the reader's own language, which the two hand-built halves this
    // replaces did not: asking for a day and a year with no month between
    // them is a combination `Intl` has no pattern for, so a week inside one
    // month came out reading "Sep 6 – 2026 (day: 12)".
    return new Intl.DateTimeFormat(locale(), {
      day: 'numeric',
      month: 'short',
      year: 'numeric',
    }).formatRange(first, last)
  })

  /** The one-line summary: what is booked, and what it came to. */
  const summary = $derived.by(() => {
    let planned = 0
    let logged = 0
    for (const iso of calendar.days) {
      const t = calendar.totalsOn(iso)
      planned += t.planned
      logged += t.logged
    }
    const bits: string[] = []
    if (planned > 0) bits.push(`${formatMinutes(planned)} planned`)
    if (logged > 0) bits.push(`${formatMinutes(logged)} logged`)
    const meetings = calendar.events.length
    if (meetings > 0) bits.push(`${meetings} ${meetings === 1 ? 'event' : 'events'}`)
    return bits.join(' · ')
  })

  // The bare-letter shortcuts this view used to own -- d, w, m, t, the
  // arrows and Delete -- are rows in `lib/shortcuts.svelte.ts` now, gated on
  // the calendar being the app on screen. They were a second
  // `<svelte:window>` keydown handler beside the one in `App.svelte`, with
  // its own idea of what counted as typing and no way for anything to list
  // them; see that file for what having one table buys.
</script>

<main class="cal">
  <div class="pane">
    <header class="top">
      <div class="nav">
        <button
          class="step"
          title="Previous (←)"
          aria-label="Previous"
          onclick={() => calendar.step(-1)}
        >
          <span class="back"><Icon name="chevron" size={15} /></span>
        </button>
        <button class="today" title="Jump to today (T)" onclick={() => calendar.goToday()}
          >Today</button
        >
        <button class="step" title="Next (→)" aria-label="Next" onclick={() => calendar.step(1)}>
          <Icon name="chevron" size={15} />
        </button>
        <h1 class="heading">{heading}</h1>
      </div>

      <div class="tools">
        <div class="views" role="group" aria-label="View">
          {#each VIEWS as v (v.id)}
            <button
              class="view"
              class:on={calendar.view === v.id}
              title="{v.label} ({v.key})"
              aria-label="{v.label} view"
              aria-pressed={calendar.view === v.id}
              onclick={() => calendar.setView(v.id)}
            >
              <Icon name={v.icon} size={15} />
            </button>
          {/each}
        </div>
      </div>
    </header>

    <!-- Plan and record, as the chips every other app narrows its list with.
         They were a segmented control in the top right corner, which is
         where each app used to put its own idea of a filter --
         so the same job looked like three different controls depending on
         which app you were in. The row is centred on the pane and the
         summary is pushed to its edge; see `.toolbar` in `app.css`.

         The control no other calendar has, either way. Plan and record are
         separate rows in storage precisely so this toggle can exist. -->
    <div class="toolbar">
      <div class="toolbar-end"></div>

      <div class="filters" role="group" aria-label="Show">
        {#each LAYERS as l (l.id)}
          <button
            class="filter"
            class:on={calendar.layer === l.id}
            title={l.title}
            aria-pressed={calendar.layer === l.id}
            onclick={() => (calendar.layer = l.id)}
          >
            {l.label}
          </button>
        {/each}
      </div>

      <div class="toolbar-end right">
        <span class="summary">{summary}</span>
        {#if calendar.syncNote}<span class="note">{calendar.syncNote}</span>{/if}
      </div>
    </div>

    {#if calendar.view === 'month'}
      <MonthGrid days={calendar.days} />
    {:else}
      <TimeGrid days={calendar.days} />
    {/if}
  </div>

  <div class="side">
    <TrackNow />
    <EventDetail />
  </div>
</main>

<style>
  .cal {
    flex: 1;
    min-width: 0;
    display: flex;
  }
  .pane {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .side {
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  /* The rail is one column: the timer sits above the panel, and the panel
     scrolls under it rather than pushing it off screen. */
  .side :global(.rail) {
    flex: 1;
    min-height: 0;
    border-top: none;
  }

  .top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    height: var(--header-h);
    padding: 0 var(--sp-4);
    flex: none;
  }

  .nav {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    min-width: 0;
  }
  .step {
    width: 26px;
    height: 26px;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    color: var(--fg-subtle);
  }
  .step:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .back {
    display: flex;
    rotate: 180deg;
  }

  .today {
    height: 26px;
    padding: 0 var(--sp-2);
    margin: 0 2px;
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-subtle);
  }
  .today:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .heading {
    margin-left: var(--sp-2);
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .tools {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    flex: none;
  }

  .views {
    display: flex;
    gap: 2px;
    padding: 2px;
    border-radius: var(--radius-sm);
    background: var(--bg-active);
  }

  .view {
    width: 26px;
    height: 22px;
    display: grid;
    place-items: center;
    border-radius: 4px;
    color: var(--fg-subtle);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .view:hover {
    color: var(--fg);
  }
  .view.on {
    background: var(--bg-raised);
    color: var(--fg);
    box-shadow: var(--shadow-sm);
  }

  .summary,
  .note {
    font-size: var(--text-xs);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }
  .summary {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .note {
    color: var(--fg-subtle);
  }
</style>
