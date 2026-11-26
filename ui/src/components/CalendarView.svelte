<script lang="ts">
  // The calendar app's main pane: a header, a grid, and the rail beside it.
  //
  // The header carries two controls and no more: which days (‹ today ›)
  // and how many (day / week / month), the same two every calendar has.

  import { onMount } from 'svelte'
  import { calendar, type View } from '../lib/calendar.svelte'
  import { dateFormat, dayHeading, formatMinutes, monthYear } from '../lib/format'
  import { closesOnBack, details, phone } from '../lib/phone.svelte'
  import { proposals } from '../lib/proposals.svelte'
  import { addDays, instantAt, todayIso } from '../lib/time'
  import Icon from './Icon.svelte'
  import CalendarAgenda from './CalendarAgenda.svelte'
  import TimeGrid from './TimeGrid.svelte'
  import MonthGrid from './MonthGrid.svelte'
  import EventDetail from './EventDetail.svelte'
  import TrackNow from './TrackNow.svelte'
  import type { IconName } from '../lib/icons'

  void calendar.start()
  // Readings are written in the journal app, under an entry, so coming back
  // to the calendar is the moment this store can learn about them. `start`
  // only runs once in a session; this runs every time the app is shown.
  onMount(() => {
    void calendar.refreshReadings()
    void calendar.reloadCalendars()
  })

  // Every pending block proposal in the window on screen -- the week grid's
  // ghosts and the month grid's chips both come from here -- is seen the
  // moment it is drawn.
  $effect(() => {
    const [from, to] = calendar.range
    const shown = proposals.inRange(from, to, 'block')
    if (shown.length > 0) void proposals.markSeen(shown)
  })

  const VIEWS: { id: View; label: string; icon: IconName; key: string }[] = [
    { id: 'day', label: 'Day', icon: 'day', key: 'D' },
    { id: 'week', label: 'Week', icon: 'week', key: 'W' },
    { id: 'month', label: 'Month', icon: 'month', key: 'M' },
  ]

  /** What the header calls the window on screen. */
  const heading = $derived.by(() => {
    const days = calendar.days
    const first = new Date(days[0] + 'T00:00')
    const last = new Date(days[days.length - 1] + 'T00:00')
    if (calendar.view === 'day') {
      // "Wednesday, November 25" is as wide as a small phone's heading, with
      // the arrows beside it: the month short is not.
      return phone.on
        ? dateFormat({ weekday: 'long', month: 'short', day: 'numeric' }).format(first)
        : dayHeading(first)
    }
    if (calendar.view === 'month') {
      return monthYear(new Date(calendar.anchor + 'T00:00'))
    }
    // A week that straddles two months should say so, and one that straddles
    // two years doubly so. `formatRange` decides all of that for itself and
    // in the reader's own language, which the two hand-built halves this
    // replaces did not: asking for a day and a year with no month between
    // them is a combination `Intl` has no pattern for, so a week inside one
    // month came out reading "Sep 6 – 2026 (day: 12)".
    return dateFormat({
      day: 'numeric',
      month: 'short',
      year: 'numeric',
    }).formatRange(first, last)
  })

  /** What the summary line below says, and no longer always one line --
   *  see `.summary`'s own doc for why it wraps now. Each bit's own spaces
   *  are non-breaking, so a wrap (week and month view, with more days
   *  behind the totals, are the ones long enough to need it) only ever
   *  falls at a `·`, never inside "2h 30m" or "10 events". */
  const summary = $derived.by(() => {
    let booked = 0
    for (const iso of calendar.days) booked += calendar.minutesOn(iso)
    const bits: string[] = []
    if (booked > 0) bits.push(`${formatMinutes(booked)} booked`.replace(/ /g, '\u00a0'))
    const meetings = calendar.events.length
    if (meetings > 0) {
      bits.push(`${meetings}\u00a0${meetings === 1 ? 'event' : 'events'}`)
    }
    return bits.join(' · ')
  })

  // ── On a phone ───────────────────────────────────────────────────────
  //
  // The grid has the whole screen, and what the rail holds -- an event
  // opened, one being written, the list of what is not scheduled -- is a
  // page laid over it, with a way back (`PhoneBack`, in the page's own
  // header) and Android's back button to the same end. The grid stays where
  // it was underneath, scroll and all.

  /** Something is open in the rail: an event, a block, a proposal, a draft. */
  const opened = $derived(
    calendar.selection !== null || calendar.selectedProposal !== null || calendar.draft !== null,
  )
  /** The list of what is not scheduled, opened from the header. */
  const listing = $derived(details.isOpen('calendar'))
  const layered = $derived(phone.on && (opened || listing))

  closesOnBack(
    () => phone.on && opened,
    () => {
      calendar.selection = null
      calendar.closeProposal()
    },
  )
  // An event being changed goes back to the event, not off the page.
  closesOnBack(
    () => phone.on && calendar.editing?.status === 'ready' && calendar.editing.event.own,
    () => calendar.cancelEdit(),
  )

  /**
   * A new event on the day picked in the month, or today if it is on screen,
   * or the day the view is on: at the next whole hour of today -- nine
   * tomorrow when that is past eleven at night -- or at nine on any other
   * day, written to the calendar new events go to. Opened as a draft, so
   * nothing exists until Save and Cancel leaves nothing behind -- a tap on
   * an empty hour is how this would be made on a desktop, and on a phone a
   * tap is too often a scroll.
   */
  function startEvent() {
    const today = todayIso()
    const picked =
      calendar.view === 'month' && calendar.picked && calendar.days.includes(calendar.picked)
        ? calendar.picked
        : null
    let day = picked ?? (calendar.days.includes(today) ? today : calendar.anchor)
    let startMinutes = 9 * 60
    if (day === today) {
      const next = (new Date().getHours() + 1) * 60
      if (next <= 23 * 60) startMinutes = next
      else day = addDays(today, 1)
    }
    const target = calendar.defaultCalendar
    // Nowhere for it to go: `newEvent` says so, once.
    if (!target && !calendar.thisComputerOn) {
      void calendar.newEvent({ day, startMinutes, minutes: 60 })
      return
    }
    calendar.openDraft(target?.id ?? null, {
      title: '',
      description: '',
      location: '',
      start: instantAt(day, startMinutes),
      end: instantAt(day, startMinutes + 60),
      allDay: false,
      tz: Intl.DateTimeFormat().resolvedOptions().timeZone,
      attendees: [],
      recurrence: null,
    })
  }

  // The bare-letter shortcuts this view used to own -- d, w, m, t, the
  // arrows and Delete -- are rows in `lib/shortcuts.svelte.ts` now, gated on
  // the calendar being the app on screen. They were a second
  // `<svelte:window>` keydown handler beside the one in `App.svelte`, with
  // its own idea of what counted as typing and no way for anything to list
  // them; see that file for what having one table buys.
</script>

<main class="cal" class:layered>
  <div class="pane">
    {#if phone.on}
      <!-- Two short rows rather than the desktop's one long one: where we
           are and the two ways to move, then how many days and the two things
           done most -- go home, make one. -->
      <header class="ptop">
        <div class="prow">
          <div class="ptitle">
            <h1 class="pane-title">{heading}</h1>
            {#if summary || calendar.syncNote}
              <span class="psummary">{calendar.syncNote ?? summary}</span>
            {/if}
          </div>
          <button class="pstep" aria-label="Previous" onclick={() => calendar.step(-1)}>
            <span class="back"><Icon name="chevron" size={18} /></span>
          </button>
          <button class="pstep" aria-label="Next" onclick={() => calendar.step(1)}>
            <Icon name="chevron" size={18} />
          </button>
        </div>
        <div class="prow">
          <div class="pviews" role="group" aria-label="View">
            {#each VIEWS as v (v.id)}
              <button
                class="pview"
                class:on={calendar.view === v.id}
                aria-label="{v.label} view"
                aria-pressed={calendar.view === v.id}
                onclick={() => calendar.setView(v.id)}>{v.label}</button
              >
            {/each}
          </div>
          <button class="btn btn-outline ptoday" onclick={() => calendar.goToday()}>Today</button>
          <button class="pnew" aria-label="New event" onclick={startEvent}>
            <Icon name="plus" size={20} weight={2} />
          </button>
        </div>
        <!-- What is waiting to be given a time, a page of its own. -->
        <button
          class="pwaiting"
          aria-label="Not scheduled{calendar.unscheduled.length > 0
            ? `, ${calendar.unscheduled.length}`
            : ''}"
          onclick={() => details.open('calendar')}
        >
          <Icon name="clock" size={15} />
          <span class="plabel">Not scheduled</span>
          {#if calendar.unscheduled.length > 0}
            <span class="pcount">{calendar.unscheduled.length}</span>
          {/if}
          <span class="pgo"><Icon name="chevron" size={14} /></span>
        </button>
      </header>
      <!-- Only while one runs: starting one is on the page of what is not
           scheduled, and an idle timer is not worth a strip of the screen. -->
      {#if calendar.timer}<TrackNow />{/if}
    {:else}
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
          <button
            class="btn btn-outline"
            title="Jump to today (T)"
            onclick={() => calendar.goToday()}>Today</button
          >
          <button class="step" title="Next (→)" aria-label="Next" onclick={() => calendar.step(1)}>
            <Icon name="chevron" size={15} />
          </button>
          <div class="pane-titles">
            <h1 class="pane-title">{heading}</h1>
          </div>
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

      <!-- The window's totals, under the header. There used to be a set of
         Both / Plan / Record chips here; a block is an event like any other
         now, and what counts as time spent is decided by the clock -- see
         `Vault::settle_blocks`. -->
      <div class="toolbar">
        <div class="toolbar-end right">
          <span class="summary">{summary}</span>
          {#if calendar.syncNote}<span class="note">{calendar.syncNote}</span>{/if}
        </div>
      </div>
    {/if}

    {#if calendar.view === 'month'}
      <MonthGrid days={calendar.days} />
    {:else if phone.on && calendar.view === 'week'}
      <!-- A week is seven columns of fifty pixels on a phone: a list of the
           days, each with the whole width, instead. -->
      <CalendarAgenda days={calendar.days} scrollToToday />
    {:else}
      <TimeGrid days={calendar.days} />
    {/if}
  </div>

  <div class="side">
    <!-- On a phone the timer is under the heading of the page of what is not
         scheduled, where it is started (`EventDetail`). -->
    {#if !phone.on}<TrackNow />{/if}
    <EventDetail />
  </div>
</main>

<style>
  .cal {
    /* As tall as the stage by a stated height, not stretched: see
       `.shell-stage` in app.css. */
    height: 100%;
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

  /* Stretched, so the heading inside it has all the room the view switch
     leaves and is cut short only when there is genuinely none. */
  .nav {
    flex: 1;
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    min-width: 0;
  }
  /* Round, like every other small icon button in a header; "Today" between
     them is the shared outlined `.btn`. */
  .step {
    width: 26px;
    height: 26px;
    display: grid;
    place-items: center;
    border-radius: 50%;
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

  .pane-titles {
    margin-left: var(--sp-2);
  }

  .tools {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    flex: none;
  }

  /* A pill of round buttons, like the filter chips under it. */
  .views {
    display: flex;
    gap: 2px;
    padding: 2px;
    border-radius: 999px;
    background: var(--bg-active);
  }

  .view {
    width: 26px;
    height: 22px;
    display: grid;
    place-items: center;
    border-radius: 999px;
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
    /* Was `nowrap` + ellipsis, which in week/month view (more days, so
       longer totals and an event count on top) clipped the count itself --
       Bug: "10 eve…" with no way to read the rest. Wrapping instead, the
       toolbar grows a second line precisely when the summary needs one,
       rather than needing a sizing rule of its own.
       `min-width: 0` is what lets a flex item wrap at all instead of
       holding its one-line width regardless of what it is given. */
    flex: 1;
    min-width: 0;
    text-align: right;
  }
  .note {
    color: var(--fg-subtle);
  }

  /* ── On a phone ──────────────────────────────────────────────────────── */

  /* The rail is not beside the grid: it is a page over it, while something
     is open in it. The grid is only hidden, not taken away, so it is as it
     was -- scrolled to the same hour -- when the page is put away. */
  :global([data-phone]) .cal {
    position: relative;
  }
  :global([data-phone]) .side {
    display: none;
  }
  :global([data-phone]) .cal.layered .side {
    display: flex;
    position: absolute;
    inset: 0;
    z-index: 3;
    background: var(--bg-panel);
  }
  :global([data-phone]) .cal.layered .pane {
    visibility: hidden;
  }

  .ptop {
    flex: none;
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: var(--sp-1) var(--sp-3) var(--sp-2);
  }
  .prow {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .ptitle {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .psummary {
    font-size: var(--text-xs);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .pstep {
    width: 44px;
    height: 44px;
    flex: none;
    display: grid;
    place-items: center;
    border-radius: 50%;
    color: var(--fg-muted);
  }
  .pstep:active {
    background: var(--bg-active);
  }

  /* Day, week and month as words, in a pill that takes what Today and the
     new-event button leave. */
  .pviews {
    flex: 1;
    min-width: 0;
    display: flex;
    gap: 2px;
    padding: 2px;
    border-radius: 999px;
    background: var(--bg-active);
  }
  .pview {
    flex: 1;
    min-width: 0;
    height: 38px;
    border-radius: 999px;
    font-size: var(--text-base);
    font-weight: 550;
    color: var(--fg-subtle);
  }
  .pview.on {
    background: var(--bg-raised);
    color: var(--fg);
    box-shadow: var(--shadow-sm);
  }

  .ptoday {
    flex: none;
    height: 44px;
  }
  .pnew {
    width: 44px;
    height: 44px;
    flex: none;
    display: grid;
    place-items: center;
    border-radius: 50%;
    background: var(--accent);
    color: var(--fg-on-accent);
    box-shadow: var(--shadow-sm);
  }
  .pnew:active {
    background: var(--accent-hover);
  }

  /* One quiet line that opens the list of what has no time yet. */
  .pwaiting {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-height: 40px;
    padding: 0 var(--sp-3);
    border-radius: var(--radius);
    background: var(--bg-sunken);
    font-size: var(--text-base);
    color: var(--fg-muted);
    text-align: left;
  }
  .pwaiting:active {
    background: var(--bg-active);
  }
  .plabel {
    flex: 1;
  }
  .pcount {
    min-width: 22px;
    padding: 1px 7px;
    border-radius: 99px;
    background: var(--bg-active);
    font-size: var(--text-xs);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    text-align: center;
    color: var(--fg-muted);
  }
  .pgo {
    display: flex;
    color: var(--fg-faint);
  }

  /* A phone on its side has a hundred pixels of screen between its two bars
     for the grid, if the header is three rows. One row: the heading, the
     steps, the three views, Today, new, and what is not scheduled as its
     icon and count. */
  @media (max-height: 500px) {
    .ptop {
      flex-direction: row;
      align-items: center;
      padding-block: 0;
    }
    .ptop > .prow:first-child {
      flex: 1;
      min-width: 0;
    }
    .ptop > .prow:nth-child(2) {
      flex: none;
    }
    .psummary,
    .plabel,
    .pgo {
      display: none;
    }
    .pviews {
      flex: none;
      width: 232px;
    }
    .pwaiting {
      flex: none;
      min-height: 44px;
      padding: 0 var(--sp-3);
    }
    .pcount {
      padding-inline: 6px;
    }
  }
</style>
