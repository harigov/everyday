<script lang="ts">
  // The days as a list: what a phone shows for a week, and under a month's
  // grid for the day picked in it.
  //
  // Seven columns of a time grid are a hundred pixels each on a desktop and
  // fifty on a phone, where a meeting's title is the first thing to go. A
  // list gives each day the whole width instead -- the day's name, then
  // everything on it in the order it runs, a row a finger can hit -- and the
  // week reads top to bottom the way one reads a week. Everything the grid
  // draws is here too, from the same store and by the same rules (a block of
  // your own, an event, a tracked stretch, a proposed block, a task that is
  // due); only the drawing differs. A phone's day is the time grid, in
  // `TimeGrid.svelte`, where the hours are what matters.
  //
  // Tapping a row opens it (`calendar.select`), and a long press -- or, where
  // a row has no panel of its own, a tap -- raises the same menus the grids
  // do. Dragging to another time is the desktop's; the editor moves things.

  import { calendar, type DayItem } from '../lib/calendar.svelte'
  import { dateFormat, formatMinutes, friendlyDate, timeOfDay } from '../lib/format'
  import { menu } from '../lib/menu.svelte'
  import type { MenuItem } from '../lib/menu'
  import { calendarTaskMenu, dayMenu, eventMenu, readingMenu, slotMenu } from '../lib/menus'
  import { todayIso } from '../lib/time'
  import Icon from './Icon.svelte'

  let {
    days,
    heads = true,
    scrollToToday = false,
    empty = 'Nothing planned',
  }: {
    days: string[]
    /** A heading over each day, that opens it in the day view. */
    heads?: boolean
    /** Open scrolled to today, when today is among the days. */
    scrollToToday?: boolean
    /** What a day with nothing on it says. */
    empty?: string
  } = $props()

  interface Row {
    key: string
    /** The left column: a start and an end, or the one word that stands for them. */
    when: string[]
    label: string
    sub: string
    color: string
    kind: 'block' | 'event' | 'task' | 'reading'
    muted?: boolean
    /** A pending proposal rather than a real record. */
    ghost?: boolean
    /** The timer is running in it. */
    live?: boolean
    onopen: (e: MouseEvent) => void
    /** A function, not an array: a menu nobody opened is not worth building. */
    menu: () => MenuItem[]
  }

  function clockOf(minutes: number): string {
    const at = new Date()
    at.setHours(0, 0, 0, 0)
    at.setMinutes(minutes)
    return timeOfDay(at)
  }

  /** Everything on one day, in the order the day runs (`dayItems`). */
  function rowsOn(iso: string): Row[] {
    return calendar.dayItems(iso).map(rowOf)
  }

  function rowOf(item: DayItem): Row {
    switch (item.kind) {
      case 'allDay': {
        const { event } = item
        const cal = calendar.calendarOf(event.calendarId)
        return {
          key: `ad:${event.id}`,
          when: ['All day'],
          label: event.title,
          sub: cal?.name ?? '',
          color: cal?.color ?? 'var(--fg-subtle)',
          kind: 'event',
          muted: event.status === 'cancelled',
          onopen: () => (calendar.selection = { kind: 'event', id: event.id }),
          menu: () => eventMenu(event),
        }
      }
      case 'note': {
        const { mark } = item
        return {
          key: mark.key,
          when: ['Noted'],
          label: mark.label,
          sub: '',
          color: mark.tracker.color,
          kind: 'reading',
          onopen: (e) => menu.show(e, readingMenu(mark.reading, mark.tracker)),
          menu: () => readingMenu(mark.reading, mark.tracker),
        }
      }
      case 'task': {
        const { task } = item
        const project = calendar.projectOf(task.projectId)
        return {
          key: `task:${task.id}`,
          when: ['Due'],
          label: task.title,
          sub: project?.name ?? '',
          color: project?.color ?? 'var(--accent)',
          kind: 'task',
          muted: task.status === 'done',
          onopen: (e) => menu.show(e, calendarTaskMenu(task)),
          menu: () => calendarTaskMenu(task),
        }
      }
      case 'slot': {
        const { slot } = item
        const proposal = slot.proposal
        return {
          key: slot.key,
          when: slot.live ? ['Now'] : [clockOf(slot.start), clockOf(slot.end)],
          label: slot.reading ? `${slot.title} · ${slot.subtitle}` : slot.title,
          sub: slot.reading ? '' : proposal ? 'Proposed' : slot.subtitle,
          color: slot.color,
          kind: slot.kind,
          muted: slot.cancelled,
          ghost: !!proposal,
          live: slot.live,
          // A reading and the running timer have no panel -- `select` would
          // only deselect -- so a tap on one shows what can be done with it.
          onopen: proposal
            ? () => calendar.selectProposal(proposal)
            : slot.reading || slot.live
              ? (e) => menu.show(e, slotMenu(slot))
              : () => calendar.select(slot),
          menu: proposal ? () => [] : () => slotMenu(slot),
        }
      }
      case 'mark': {
        const { mark } = item
        return {
          key: mark.key,
          when: [clockOf(mark.minute ?? 0)],
          label: mark.label,
          sub: '',
          color: mark.tracker.color,
          kind: 'reading',
          onopen: (e) => menu.show(e, readingMenu(mark.reading, mark.tracker)),
          menu: () => readingMenu(mark.reading, mark.tracker),
        }
      }
    }
  }

  const currentDay = $derived(todayIso())

  function openDay(iso: string) {
    calendar.view = 'day'
    calendar.goto(iso)
  }

  /** "Today", "Tomorrow", a weekday -- and for the rest, the weekday in full. */
  function dayName(iso: string): string {
    const at = new Date(iso + 'T00:00')
    return dateFormat({ weekday: 'long' }).format(at)
  }
  function dayNote(iso: string): string {
    const at = new Date(iso + 'T00:00')
    const near = friendlyDate(iso)
    const date = dateFormat({ month: 'short', day: 'numeric' }).format(at)
    return near === 'Today' || near === 'Tomorrow' || near === 'Yesterday'
      ? `${near}, ${date}`
      : date
  }

  // Open on today rather than on the first day of a week that began on
  // Sunday: keyed on the window, so a refresh does not yank the list back.
  let box = $state<HTMLDivElement | null>(null)
  let scrolledFor = $state('')
  $effect(() => {
    const key = `${days[0]}:${days.length}`
    if (!box || !scrollToToday || scrolledFor === key) return
    scrolledFor = key
    const at = box.querySelector<HTMLElement>('[data-today]')
    // `box` is positioned, so a day's offset is from the top of the list.
    if (at) box.scrollTop = at.offsetTop
  })
</script>

<div class="agenda scroll" bind:this={box}>
  {#each days as iso (iso)}
    {@const rows = rowsOn(iso)}
    {@const booked = calendar.minutesOn(iso)}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <section
      class="day"
      class:today={iso === currentDay}
      data-today={iso === currentDay ? '' : undefined}
      oncontextmenu={(e) => menu.show(e, dayMenu(iso))}
    >
      {#if heads}
        <button
          class="head"
          aria-label="Open {dayName(iso)}, {dayNote(iso)}"
          onclick={() => openDay(iso)}
        >
          <span class="num">{Number(iso.slice(8, 10))}</span>
          <span class="name">
            <span class="weekday">{dayName(iso)}</span>
            <span class="note">{dayNote(iso)}</span>
          </span>
          {#if booked > 0}<span class="tally">{formatMinutes(booked)}</span>{/if}
          <span class="go"><Icon name="chevron" size={14} /></span>
        </button>
      {/if}

      {#each rows as row (row.key)}
        <button
          class="row {row.kind}"
          class:muted={row.muted}
          class:ghost={row.ghost}
          class:live={row.live}
          style="--c: {row.color}"
          onclick={row.onopen}
          oncontextmenu={(e) => menu.show(e, row.menu())}
        >
          <span class="when">
            {#each row.when as line, i (i)}<span class:end={i > 0}>{line}</span>{/each}
          </span>
          <span class="bar" aria-hidden="true"></span>
          <span class="what">
            <span class="label">{row.label}</span>
            {#if row.sub}<span class="sub">{row.sub}</span>{/if}
          </span>
        </button>
      {/each}

      {#if rows.length === 0}
        <p class="empty">{empty}</p>
      {/if}
    </section>
  {/each}
</div>

<style>
  .agenda {
    position: relative;
    flex: 1;
    min-height: 0;
    padding: 0 var(--sp-3) var(--fab-clear);
  }

  .day {
    padding-bottom: var(--sp-2);
  }

  /* The day's name stays at the top while its rows go by, and the next
     day's pushes it off. */
  .head {
    position: sticky;
    top: 0;
    z-index: 1;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    width: 100%;
    min-height: 48px;
    padding: var(--sp-1) var(--sp-1);
    border-bottom: 1px solid var(--border);
    background: var(--surface);
    text-align: left;
  }
  .num {
    width: 32px;
    flex: none;
    font-size: var(--text-lg);
    font-weight: 650;
    font-variant-numeric: tabular-nums;
    text-align: center;
    color: var(--fg-muted);
  }
  .name {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .weekday {
    font-size: var(--text-base);
    font-weight: 600;
    color: var(--fg);
  }
  .note {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .today .num,
  .today .weekday {
    color: var(--accent);
  }
  .today .num {
    font-weight: 700;
  }
  .tally {
    flex: none;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: var(--fg-faint);
  }
  .go {
    display: flex;
    flex: none;
    color: var(--fg-faint);
  }

  .row {
    display: flex;
    align-items: stretch;
    gap: var(--sp-3);
    width: 100%;
    min-height: 56px;
    margin-top: var(--sp-1);
    padding: var(--sp-2) var(--sp-2) var(--sp-2) 0;
    border-radius: var(--radius-sm);
    text-align: left;
  }
  .row:active {
    background: var(--bg-hover);
  }

  .when {
    flex: none;
    width: 68px;
    display: flex;
    flex-direction: column;
    justify-content: center;
    padding-left: var(--sp-1);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    text-align: right;
    color: var(--fg-muted);
  }
  .when .end {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }

  /* The kinds of thing are told apart by the bar's weight, as on the grid:
     solid for your own time, a tint for a calendar's, a ring for a task. */
  .bar {
    flex: none;
    width: 4px;
    border-radius: 2px;
    background: var(--c);
  }
  .event .bar {
    background: color-mix(in oklab, var(--c) 55%, transparent);
    box-shadow: inset 0 0 0 1px var(--c);
  }
  .task .bar,
  .reading .bar {
    background: none;
    box-shadow: inset 0 0 0 1.5px color-mix(in oklab, var(--c) 70%, transparent);
  }
  .reading .bar {
    background: color-mix(in oklab, var(--c) 30%, transparent);
    box-shadow: none;
  }
  .ghost .bar {
    background: none;
    box-shadow: none;
    border-left: 3px dashed var(--c);
  }

  .what {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 1px;
  }
  .label {
    font-size: var(--text-base);
    font-weight: 560;
    color: var(--fg);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sub {
    font-size: var(--text-xs);
    color: var(--fg-faint);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .muted .label {
    color: var(--fg-subtle);
    text-decoration: line-through;
  }
  .muted .when,
  .muted .what {
    opacity: 0.6;
  }
  .ghost .what {
    font-style: italic;
  }
  .live .when {
    color: var(--accent);
    font-weight: 600;
  }

  .empty {
    padding: var(--sp-2) var(--sp-1) var(--sp-2) 80px;
    font-size: var(--text-sm);
    color: var(--fg-faint);
  }
</style>
