<script lang="ts">
  // The month view: six weeks, every one the same height.
  //
  // A month cell is not a small week column. There is no room to place
  // things by time, so nothing here is positioned by minute — each day is a
  // short list, ordered as the day runs, and the point of the view is the
  // *shape* of a month rather than the detail of an hour. Clicking a day
  // opens it in the day view, which is where the detail lives.
  //
  // What can be moved can be dragged onto another day here, and keeps its
  // time of day -- see `day-move.ts`. The hour is changed in the week.
  //
  // Six rows always, even when a month fits in five. A grid whose height
  // changes as you page through the year makes everything below it jump.

  import { calendar, type DayItem } from '../lib/calendar.svelte'
  import { dayDropTarget, startDayMove, type DayMove } from '../lib/day-move'
  import { daysFrom, startOfWeek, todayIso } from '../lib/time'
  import { dateFormat, dayHeading, timeOfDay } from '../lib/format'
  import { menu } from '../lib/menu.svelte'
  import { calendarTaskMenu, dayMenu, eventMenu, readingMenu, slotMenu } from '../lib/menus'
  import { phone } from '../lib/phone.svelte'
  import CalendarAgenda from './CalendarAgenda.svelte'
  import Icon from './Icon.svelte'
  import type { MenuItem } from '../lib/menu'

  let { days }: { days: string[] } = $props()

  /** Most rows a cell shows before collapsing into "+n more". */
  const MAX_ROWS = 4

  const headings = $derived(
    daysFrom(startOfWeek(todayIso(), calendar.weekStart), 7).map((iso) =>
      dateFormat({ weekday: 'short' }).format(new Date(iso + 'T00:00')),
    ),
  )

  interface Row {
    key: string
    label: string
    time: string
    color: string
    kind: 'block' | 'event' | 'task' | 'reading'
    muted?: boolean
    /** A pending proposal rather than a real record -- drawn as a small ghost chip. */
    ghost?: boolean
    /** What dragging it onto another day moves, when it can be moved. */
    move?: DayMove
    /** Moved, and its server not yet told. */
    saving?: boolean
    onopen: () => void
    /**
     * The same menus the week grid raises, on the same things.
     *
     * A function, not an array: `rowsOn` runs for all forty-two cells on
     * every redraw, and a month of built menus that nobody right-clicked is
     * a month of wasted work.
     */
    menu: () => MenuItem[]
  }

  /** Everything on one day, in the order the day runs (`dayItems`). */
  function rowsOn(iso: string): Row[] {
    return calendar.dayItems(iso).map((item) => rowOf(item, iso))
  }

  function rowOf(item: DayItem, iso: string): Row {
    switch (item.kind) {
      case 'allDay': {
        const { event } = item
        const cal = calendar.calendarOf(event.calendarId)
        return {
          key: `ad:${event.id}`,
          label: event.title,
          time: '',
          color: cal?.color ?? 'var(--fg-subtle)',
          kind: 'event',
          muted: event.status === 'cancelled',
          move: calendar.canMove(event) ? { kind: 'event', id: event.id, from: iso } : undefined,
          saving: calendar.moves.has(event.id),
          onopen: () => (calendar.selection = { kind: 'event', id: event.id }),
          menu: () => eventMenu(event),
        }
      }
      // Readings with no time of day sit with the other undated things,
      // above everything the clock can order.
      case 'note': {
        const { mark } = item
        return {
          key: mark.key,
          label: mark.label,
          time: '',
          color: mark.tracker.color,
          kind: 'reading',
          onopen: () => calendar.goto(iso),
          menu: () => readingMenu(mark.reading, mark.tracker),
        }
      }
      case 'task': {
        const { task } = item
        return {
          key: `task:${task.id}`,
          label: task.title,
          time: '',
          color: calendar.projectOf(task.projectId)?.color ?? 'var(--accent)',
          kind: 'task',
          muted: task.status === 'done',
          onopen: () => calendar.goto(iso),
          menu: () => calendarTaskMenu(task),
        }
      }
      case 'slot': {
        const { slot } = item
        const start = slot.block?.start ?? slot.event?.start ?? slot.reading?.at
        const proposal = slot.proposal
        // What dragging it onto another day moves: a block of your own, or an
        // event `canMove` allows -- whole or not, since a night that runs into
        // the next day still moves by its day. Never a ghost.
        const move: DayMove | undefined = proposal
          ? undefined
          : slot.event && calendar.canMove(slot.event)
            ? { kind: 'event', id: slot.event.id, from: iso }
            : slot.block && slot.movable
              ? { kind: 'block', id: slot.block.id, from: iso }
              : undefined
        return {
          key: slot.key,
          label: slot.reading ? `${slot.title} · ${slot.subtitle}` : slot.title,
          time: start ? timeOfDay(new Date(start)) : '',
          color: slot.color,
          kind: slot.kind,
          muted: slot.cancelled,
          ghost: !!proposal,
          move,
          saving: slot.saving,
          // A ghost opens its own proposal pane; a right-click on it does the
          // same, rather than raising the ordinary block menu -- there is no
          // row on disk yet for "delete" or "rename" to act on.
          onopen: proposal ? () => calendar.selectProposal(proposal) : () => calendar.select(slot),
          menu: proposal ? () => [] : () => slotMenu(slot),
        }
      }
      case 'mark': {
        const { mark } = item
        return {
          key: mark.key,
          label: mark.label,
          time: mark.reading.at ? timeOfDay(new Date(mark.reading.at)) : '',
          color: mark.tracker.color,
          kind: 'reading',
          onopen: () => calendar.goto(iso),
          menu: () => readingMenu(mark.reading, mark.tracker),
        }
      }
    }
  }

  const currentDay = $derived(todayIso())

  // On a phone a day is fifty pixels wide, which holds a number and a few
  // dots and no more. The cell picks the day, and what is on it is listed
  // under the grid -- the same rows the week's list has -- until another is
  // picked or the month moves.
  const pick = $derived(
    calendar.picked && days.includes(calendar.picked) ? calendar.picked : calendar.anchor,
  )
  $effect(() => {
    void calendar.anchor
    calendar.picked = null
  })

  /** "Thursday, November 26" for the dots' own label. */
  function dateOf(iso: string): string {
    return dateFormat({ weekday: 'long', month: 'long', day: 'numeric' }).format(
      new Date(iso + 'T00:00'),
    )
  }

  /** The day itself, hour by hour. */
  function openDay(iso: string) {
    calendar.view = 'day'
    calendar.goto(iso)
  }

  /** The day something is being dragged over. */
  let dropOn = $state<string | null>(null)
  const cells = dayDropTarget(
    (iso) => (dropOn = iso),
    (move, iso) => calendar.moveToDay(move, iso),
  )
</script>

<div class="month">
  <div class="headings">
    {#each headings as label, i (i)}<div class="heading">{label}</div>{/each}
  </div>

  <div class="weeks scroll">
    {#each days as iso (iso)}
      {@const rows = rowsOn(iso)}
      {@const first = Number(iso.slice(8, 10)) === 1}
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="cell"
        class:outside={!calendar.inAnchorMonth(iso)}
        class:today={iso === currentDay}
        class:droptarget={dropOn === iso}
        oncontextmenu={(e) => menu.show(e, dayMenu(iso))}
        ondragover={(e) => cells.over(e, iso)}
        ondragleave={cells.leave}
        ondrop={(e) => cells.drop(e, iso)}
      >
        {#if phone.on}
          <button
            class="pday"
            class:picked={iso === pick}
            aria-label="{dateOf(iso)}{rows.length > 0
              ? `, ${rows.length} ${rows.length === 1 ? 'item' : 'items'}`
              : ''}"
            aria-pressed={iso === pick}
            onclick={() => (calendar.picked = iso)}
          >
            <span class="pnum">{Number(iso.slice(8, 10))}</span>
            <span class="pdots" aria-hidden="true">
              {#each rows.slice(0, 4) as row (row.key)}
                <span class="pdot {row.kind}" class:ghost={row.ghost} style="--c: {row.color}"
                ></span>
              {/each}
            </span>
          </button>
        {:else}
          <div class="cellhead">
            <button
              class="num"
              title="Open this day"
              onclick={() => {
                calendar.view = 'day'
                calendar.goto(iso)
              }}
            >
              <!-- The first of a month names itself, so the boundary between
                   two months is legible without counting back to a heading. -->
              {#if first}<span class="mon"
                  >{dateFormat({ month: 'short' }).format(new Date(iso + 'T00:00'))}</span
                >{/if}
              {Number(iso.slice(8, 10))}
            </button>
            {#if calendar.hasEntry(iso)}
              <span
                class="wrote"
                role="img"
                title="You wrote something on this day"
                aria-label="You wrote something on this day"
              >
                <Icon name="quote" size={10} weight={2} />
              </span>
            {/if}
          </div>

          <div class="rows">
            {#each rows.slice(0, MAX_ROWS) as row (row.key)}
              <button
                class="row {row.kind}"
                class:muted={row.muted}
                class:ghost={row.ghost}
                class:movable={!!row.move}
                class:saving={row.saving}
                style="--c: {row.color}"
                title={(row.ghost ? `Proposed: ${row.label}` : row.label) +
                  (row.time ? ` · ${row.time}` : '')}
                draggable={!!row.move}
                ondragstart={(e) => row.move && startDayMove(e, row.move)}
                ondragend={() => (dropOn = null)}
                onclick={row.onopen}
                oncontextmenu={(e) => menu.show(e, row.menu())}
              >
                <span class="dot" aria-hidden="true"></span>
                {#if row.time}<span class="at">{row.time}</span>{/if}
                <span class="what">{row.label}</span>
              </button>
            {/each}
            {#if rows.length > MAX_ROWS}
              <button
                class="more"
                onclick={() => {
                  calendar.view = 'day'
                  calendar.goto(iso)
                }}>{rows.length - MAX_ROWS} more</button
              >
            {/if}
          </div>
        {/if}
      </div>
    {/each}
  </div>

  {#if phone.on}
    <section class="plist" aria-label="The day">
      <header class="phead">
        <h2 class="ptitle">{dayHeading(new Date(pick + 'T00:00'))}</h2>
        <button class="btn btn-outline popen" onclick={() => openDay(pick)}>
          Day
          <Icon name="chevron" size={14} />
        </button>
      </header>
      <CalendarAgenda days={[pick]} heads={false} empty="Nothing on this day." />
    </section>
  {/if}
</div>

<style>
  .month {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .headings {
    display: grid;
    grid-template-columns: repeat(7, minmax(0, 1fr));
    flex: none;
    border-bottom: 1px solid var(--border);
  }
  .heading {
    padding: var(--sp-2) var(--sp-2) 6px;
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }

  .weeks {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: repeat(7, minmax(0, 1fr));
    grid-auto-rows: minmax(0, 1fr);
  }

  .cell {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-height: 0;
    min-width: 0;
    padding: 3px 3px 4px;
    border-left: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
    overflow: hidden;
  }
  .cell:nth-child(7n + 1) {
    border-left: none;
  }
  /* Days spilling in from the neighbouring months are still real days --
     things happen on them -- so they are dimmed rather than emptied. */
  .cell.outside {
    background: var(--bg-sunken);
  }
  .cell.outside .num {
    color: var(--fg-faint);
  }
  .cell.today {
    background: color-mix(in oklab, var(--accent) 4.5%, transparent);
  }
  /* The day something is being dragged onto. */
  .cell.droptarget {
    background: color-mix(in oklab, var(--accent) 9%, transparent);
  }

  .cellhead {
    display: flex;
    align-items: center;
    gap: 4px;
    flex: none;
  }
  .num {
    display: flex;
    align-items: baseline;
    gap: 4px;
    height: 19px;
    padding: 0 5px;
    border-radius: var(--radius-sm);
    font-size: var(--text-xs);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    color: var(--fg-muted);
  }
  .num:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .mon {
    font-weight: 650;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    opacity: 0.75;
  }
  .today .num {
    color: var(--accent);
  }
  .wrote {
    color: var(--fg-faint);
    display: flex;
  }

  .rows {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-height: 0;
    overflow: hidden;
    /* So a chip can ask how wide its cell is: see `.at` below. */
    container-type: inline-size;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 5px;
    height: 18px;
    padding: 0 5px;
    border-radius: 4px;
    font-size: var(--text-xs);
    text-align: left;
    color: var(--fg-muted);
    min-width: 0;
  }
  .row:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .row.muted {
    opacity: 0.5;
    text-decoration: line-through;
  }
  .row.movable {
    cursor: grab;
  }
  /* Moved, and its server not yet told. */
  .row.saving {
    opacity: 0.6;
    cursor: progress;
  }

  /* The dot carries the whole visual language at this size: filled for
     your own time, ringed for somebody else's calendar, square for a task. */
  .dot {
    width: 6px;
    height: 6px;
    flex: none;
    border-radius: 50%;
    border: 1.5px solid var(--c);
  }
  .row.block .dot {
    background: var(--c);
  }
  .row.event .dot {
    background: color-mix(in oklab, var(--c) 35%, transparent);
  }
  .row.task .dot {
    border-radius: 1.5px;
  }
  /* A reading is a fifth shape: a filled pip with a halo, which reads as a
     point at six pixels where a fourth outline would read as a smudge. */
  .row.reading .dot {
    border-color: transparent;
    background: var(--c);
    box-shadow: 0 0 0 1.5px color-mix(in oklab, var(--c) 30%, transparent);
  }

  /* A pending proposal: a dashed outline rather than a filled or hollow dot,
     the same "not yet real" it draws everywhere else in the app. */
  .row.ghost {
    opacity: 0.75;
  }
  .row.ghost .dot {
    background: none;
    border-style: dashed;
  }
  .row.ghost .what {
    font-style: italic;
  }

  .at {
    font-variant-numeric: tabular-nums;
    color: var(--fg-faint);
    flex: none;
  }
  /* In a narrow cell the clock would take most of the row and leave the title
     as an ellipsis. The time stays in the chip's tooltip and the day view. */
  @container (max-width: 120px) {
    .at {
      display: none;
    }
  }
  .what {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .more {
    height: 16px;
    padding: 0 5px;
    font-size: var(--text-xs);
    color: var(--fg-faint);
    text-align: left;
  }
  .more:hover {
    color: var(--fg-muted);
  }

  /* ── On a phone ──────────────────────────────────────────────────────── */

  /* The grid is as tall as its six rows need and no more; what is on the
     picked day gets the rest of the screen. */
  :global([data-phone]) .heading {
    padding: var(--sp-1) 0;
    text-align: center;
  }
  :global([data-phone]) .weeks {
    flex: none;
    overflow: visible;
    grid-auto-rows: 46px;
    padding: 0 var(--sp-1);
  }
  :global([data-phone]) .cell {
    padding: 0;
    border-left: none;
  }

  /* A day: its number, and under it a dot for each of the first four things
     on it, in what the dots mean on the desktop -- filled for your own time,
     ringed for somebody else's, square for a task. */
  .pday {
    width: 100%;
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 3px;
    border-radius: var(--radius);
  }
  .pnum {
    width: 30px;
    height: 30px;
    display: grid;
    place-items: center;
    border-radius: 50%;
    font-size: var(--text-md);
    font-weight: 560;
    font-variant-numeric: tabular-nums;
    color: var(--fg);
  }
  .cell.outside .pnum {
    color: var(--fg-faint);
  }
  .cell.today .pnum {
    color: var(--accent);
    font-weight: 700;
  }
  .pday.picked .pnum {
    background: var(--bg-active);
  }
  .cell.today .pday.picked .pnum {
    background: color-mix(in oklab, var(--accent) 14%, transparent);
  }
  .pday:active .pnum {
    background: var(--bg-active);
  }

  .pdots {
    height: 7px;
    display: flex;
    align-items: center;
    gap: 3px;
  }
  .pdot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    border: 1.5px solid var(--c);
  }
  .pdot.block {
    background: var(--c);
  }
  .pdot.event {
    background: color-mix(in oklab, var(--c) 35%, transparent);
  }
  .pdot.task {
    border-radius: 1.5px;
  }
  .pdot.reading {
    border-color: transparent;
    background: var(--c);
  }
  .pdot.ghost {
    background: none;
    border-style: dashed;
  }

  .plist {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    border-top: 1px solid var(--border);
    background: var(--bg-panel);
  }
  .phead {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2);
    padding: var(--sp-1) var(--sp-3) var(--sp-1) var(--sp-4);
  }
  .ptitle {
    flex: 1;
    min-width: 0;
    font-size: var(--text-md);
    font-weight: 650;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .popen {
    flex: none;
    gap: var(--sp-1);
  }

  /* A phone on its side: the month on the left, the picked day's list on the
     right, each scrolling by itself, since there is not height for both. */
  @media (max-height: 500px) {
    :global([data-phone]) .month {
      display: grid;
      grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
      grid-template-rows: auto minmax(0, 1fr);
    }
    :global([data-phone]) .headings {
      grid-column: 1;
    }
    :global([data-phone]) .weeks {
      grid-column: 1;
      overflow-y: auto;
      grid-auto-rows: 40px;
    }
    .plist {
      grid-column: 2;
      grid-row: 1 / -1;
      border-top: none;
      border-left: 1px solid var(--border);
    }
    .pnum {
      width: 26px;
      height: 26px;
      font-size: var(--text-base);
    }
    .pday {
      gap: 1px;
    }
  }
</style>
