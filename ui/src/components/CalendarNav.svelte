<script lang="ts">
  // The calendar app's half of the sidebar: a small month to navigate by,
  // and the list of calendars being drawn.
  //
  // The mini month is not decoration. It is the only place in the app that
  // shows a whole month while you are looking at a week, and a dot under a
  // day is the cheapest possible answer to "is there anything on then".

  import { calendar } from '../lib/calendar.svelte'
  import { addMonths, daysFrom, monthGrid, startOfWeek, todayIso } from '../lib/time'
  import { relativeTime } from '../lib/format'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { colourItems, dayMenu } from '../lib/menus'
  import Icon from './Icon.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import AddCalendar from './AddCalendar.svelte'
  import type { CalendarInfo } from '../lib/types'

  const locale = navigator.language || 'en'
  const monthFmt = new Intl.DateTimeFormat(locale, { month: 'long', year: 'numeric' })
  const initialFmt = new Intl.DateTimeFormat(locale, { weekday: 'narrow' })

  let adding = $state(false)
  let pendingDelete = $state<CalendarInfo | null>(null)
  /** The month the small calendar is showing; follows the main view. */
  let miniAnchor = $state(todayIso())

  // Keep the mini month on the same month as the main view when that moves,
  // but let it be paged independently once you start using it.
  let lastAnchor = $state(calendar.anchor)
  $effect(() => {
    if (calendar.anchor !== lastAnchor) {
      lastAnchor = calendar.anchor
      miniAnchor = calendar.anchor
    }
  })

  const miniDays = $derived(monthGrid(miniAnchor, calendar.weekStart))
  const initials = $derived(
    daysFrom(startOfWeek(todayIso(), calendar.weekStart), 7).map((iso) =>
      initialFmt.format(new Date(iso + 'T00:00')),
    ),
  )
  const shown = $derived(new Set(calendar.days))
  const currentDay = $derived(todayIso())

  /** Does anything happen on this day? Drives the dot under the number. */
  function busy(iso: string): boolean {
    return (
      calendar.slotsOn(iso).length > 0 ||
      calendar.allDayOn(iso).length > 0 ||
      calendar.tasksOn(iso).length > 0
    )
  }

  function originLabel(cal: CalendarInfo): string {
    if (cal.origin.type === 'file') return cal.origin.label
    if (cal.lastError) return cal.lastError
    if (cal.lastSyncedAt) return `Refreshed ${relativeTime(cal.lastSyncedAt)}`
    return 'Not refreshed yet'
  }

  async function remove() {
    const doomed = pendingDelete
    pendingDelete = null
    if (doomed) await calendar.unsubscribe(doomed.id)
  }

  const anySubscribed = $derived(calendar.calendars.some((c) => c.origin.type === 'url'))

  /**
   * What a right-click on a subscribed calendar offers.
   *
   * As in the other two sidebars, this gesture used to go straight to the
   * unsubscribe dialog. Everything here except the last row is something you
   * might do weekly; that one is at the bottom, behind a confirmation, where
   * it was always meant to be.
   */
  function calendarMenu(cal: CalendarInfo): MenuItem[] {
    return tidyMenu([
      {
        // The label says which way it goes, so there is no tick as well: one
        // row cannot both be a switch and describe the thing it switches.
        label: cal.visible ? 'Hide from the grid' : 'Show on the grid',
        icon: cal.visible ? 'hidden' : 'calendar',
        run: () => calendar.toggleVisible(cal.id),
      },
      cal.origin.type === 'url' && {
        label: 'Refresh now',
        icon: 'refresh',
        disabled: calendar.syncing,
        run: () => calendar.syncOne(cal.id),
      },
      {
        label: 'Colour',
        dot: cal.color,
        items: colourItems(cal.color, (color) => calendar.setCalendarColor(cal.id, color)),
      },
      SEP,
      { label: 'Unsubscribe…', icon: 'trash', danger: true, run: () => (pendingDelete = cal) },
    ])
  }

  /** The panel itself, where there is no calendar under the pointer. */
  function navMenu(): MenuItem[] {
    return tidyMenu([
      { label: 'Add a calendar…', icon: 'plus', run: () => (adding = true) },
      anySubscribed && {
        label: 'Refresh every calendar',
        icon: 'refresh',
        disabled: calendar.syncing,
        run: () => calendar.syncDue(true),
      },
      SEP,
      { label: 'Jump to today', icon: 'sun', run: () => calendar.goToday() },
    ])
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<nav class="scroll nav" oncontextmenu={(e) => menu.show(e, navMenu())}>
  <!-- ── The small month ─────────────────────────────────────────────── -->
  <div class="minihead">
    <button
      class="ministep"
      aria-label="Previous month"
      onclick={() => (miniAnchor = addMonths(miniAnchor, -1))}
      ><span class="back"><Icon name="chevron" size={13} /></span></button
    >
    <button class="minititle" onclick={() => calendar.goto(miniAnchor)}>
      {monthFmt.format(new Date(miniAnchor + 'T00:00'))}
    </button>
    <button
      class="ministep"
      aria-label="Next month"
      onclick={() => (miniAnchor = addMonths(miniAnchor, 1))}
      ><Icon name="chevron" size={13} /></button
    >
  </div>

  <div class="mini">
    {#each initials as letter, i (i)}<span class="initial">{letter}</span>{/each}
    {#each miniDays as iso (iso)}
      <button
        class="minday"
        class:out={iso.slice(0, 7) !== miniAnchor.slice(0, 7)}
        class:on={shown.has(iso)}
        class:today={iso === currentDay}
        onclick={() => calendar.goto(iso)}
        oncontextmenu={(e) => menu.show(e, dayMenu(iso))}
      >
        {Number(iso.slice(8, 10))}
        {#if busy(iso)}<span class="bump" aria-hidden="true"></span>{/if}
      </button>
    {/each}
  </div>

  <!-- ── Subscribed calendars ────────────────────────────────────────── -->
  <div class="head">
    <span class="eyebrow">Calendars</span>
    <div class="headtools">
      {#if anySubscribed}
        <button
          class="plus"
          class:spin={calendar.syncing}
          title="Refresh all calendars"
          aria-label="Refresh all calendars"
          onclick={() => calendar.syncDue(true)}
        >
          <Icon name="refresh" size={14} />
        </button>
      {/if}
      <button
        class="plus"
        title="Add a calendar"
        aria-label="Add a calendar"
        onclick={() => (adding = true)}
      >
        <Icon name="plus" size={15} />
      </button>
    </div>
  </div>

  {#each calendar.calendars as cal (cal.id)}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="row"
      class:hidden={!cal.visible}
      class:failed={!!cal.lastError}
      oncontextmenu={(e) => menu.show(e, calendarMenu(cal))}
    >
      <button
        class="tick"
        style="--dot: {cal.color}"
        title={cal.visible ? 'Hide from the grid' : 'Show on the grid'}
        aria-pressed={cal.visible}
        onclick={() => calendar.toggleVisible(cal.id)}
      >
        <span class="swatch" aria-hidden="true"></span>
      </button>
      <button class="text" title={originLabel(cal)} onclick={() => calendar.toggleVisible(cal.id)}>
        <span class="cname">{cal.name}</span>
        <span class="cmeta">
          {#if cal.lastError}
            <span class="warn"><Icon name="alert" size={11} weight={2} /></span>
          {/if}
          {cal.events}
          {cal.events === 1 ? 'event' : 'events'}
        </span>
      </button>
      {#if cal.origin.type === 'url'}
        <button
          class="mini-action"
          title="Refresh this calendar"
          aria-label="Refresh {cal.name}"
          onclick={() => calendar.syncOne(cal.id)}
        >
          <Icon name="refresh" size={13} />
        </button>
      {/if}
    </div>
  {/each}

  {#if calendar.calendars.length === 0}
    <p class="blank">
      Nothing subscribed. Add the secret address of a Google, Outlook or Apple calendar and its
      events appear here, read-only, beside your own time.
    </p>
  {/if}

  {#if calendar.calendars.some((c) => c.lastError)}
    <p class="blank quiet">
      A calendar that cannot be reached keeps the events it already had, so a dropped connection
      never empties one.
    </p>
  {/if}
</nav>

{#if adding}
  <AddCalendar onclose={() => (adding = false)} />
{/if}

{#if pendingDelete}
  <ConfirmDialog
    title={'Unsubscribe from “' + pendingDelete.name + '”?'}
    detail="Its {pendingDelete.events} events are removed from this vault. Nothing on the calendar it came from is touched, and you can subscribe again with the same address."
    confirmLabel="Unsubscribe"
    onconfirm={remove}
    oncancel={() => (pendingDelete = null)}
  />
{/if}

<style>
  /* The same metrics as the other navs: one sidebar, four apps, and a
     row that changed height when you switched would read as three programs. */
  .nav {
    flex: 1;
    padding: var(--sp-2) var(--sp-2) var(--sp-4);
  }

  /* ── The small month ────────────────────────────────────────────────── */

  .minihead {
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 0 2px var(--sp-1);
  }
  .minititle {
    flex: 1;
    height: 24px;
    padding: 0 var(--sp-1);
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    font-weight: 600;
    letter-spacing: -0.004em;
    text-align: left;
    color: var(--fg-muted);
  }
  .minititle:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .ministep {
    width: 22px;
    height: 22px;
    flex: none;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .ministep:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .back {
    display: flex;
    rotate: 180deg;
  }

  .mini {
    display: grid;
    grid-template-columns: repeat(7, 1fr);
    gap: 1px;
    padding: 0 1px;
  }
  .initial {
    height: 18px;
    display: grid;
    place-items: center;
    font-size: 10px;
    font-weight: 600;
    color: var(--fg-faint);
  }
  .minday {
    position: relative;
    height: 22px;
    display: grid;
    place-items: center;
    border-radius: 4px;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: var(--fg-muted);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .minday:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .minday.out {
    color: var(--fg-faint);
    opacity: 0.6;
  }
  /* The days the main view is currently showing, so the small month says
     where you are as well as where you could go. */
  .minday.on {
    background: var(--bg-active);
    color: var(--fg);
    font-weight: 600;
  }
  .minday.today {
    color: var(--accent);
    font-weight: 700;
  }

  .bump {
    position: absolute;
    bottom: 2px;
    width: 3px;
    height: 3px;
    border-radius: 50%;
    background: currentColor;
    opacity: 0.55;
  }

  /* ── The calendar list ──────────────────────────────────────────────── */

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--sp-5) var(--sp-2) var(--sp-2);
  }
  .headtools {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .plus {
    width: 20px;
    height: 20px;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .plus:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .plus.spin {
    animation: turn 1.1s linear infinite;
    color: var(--accent);
  }
  @keyframes turn {
    to {
      rotate: 360deg;
    }
  }

  .row {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    min-height: 29px;
    padding: 0 var(--sp-1) 0 2px;
    border-radius: var(--radius-sm);
  }
  .row:hover {
    background: var(--bg-hover);
  }
  .row.hidden .cname,
  .row.hidden .cmeta {
    opacity: 0.45;
  }

  .tick {
    width: 22px;
    height: 22px;
    flex: none;
    display: grid;
    place-items: center;
  }
  /* A filled swatch is shown, a hollow one is hidden -- the same affordance
     as a checkbox, and it doubles as the colour key for the grid. */
  .swatch {
    width: 11px;
    height: 11px;
    border-radius: 3.5px;
    border: 1.5px solid var(--dot);
    background: var(--dot);
    transition: background var(--fast) var(--ease);
  }
  .row.hidden .swatch {
    background: transparent;
  }

  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 0;
    padding: 3px 0;
    text-align: left;
  }
  .cname {
    font-size: var(--text-base);
    color: var(--fg-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .row:hover .cname {
    color: var(--fg);
  }
  .cmeta {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-xs);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }
  .warn {
    color: var(--danger);
    display: flex;
  }
  .row.failed .cmeta {
    color: var(--danger);
  }

  .mini-action {
    width: 22px;
    height: 22px;
    flex: none;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
    opacity: 0;
  }
  .row:hover .mini-action {
    opacity: 1;
  }
  .mini-action:hover {
    background: var(--bg-active);
    color: var(--fg);
  }

  .blank {
    padding: var(--sp-2);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-faint);
  }
  .blank.quiet {
    padding-top: 0;
    opacity: 0.85;
  }
</style>
