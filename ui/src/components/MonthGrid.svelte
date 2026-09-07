<script lang="ts">
  // The month view: six weeks, every one the same height.
  //
  // A month cell is not a small week column. There is no room to place
  // things by time, so nothing here is positioned by minute — each day is a
  // short list, ordered as the day runs, and the point of the view is the
  // *shape* of a month rather than the detail of an hour. Clicking a day
  // opens it in the day view, which is where the detail lives.
  //
  // Six rows always, even when a month fits in five. A grid whose height
  // changes as you page through the year makes everything below it jump.

  import { calendar } from '../lib/calendar.svelte'
  import { daysFrom, startOfWeek, todayIso } from '../lib/time'
  import Icon from './Icon.svelte'

  let { days }: { days: string[] } = $props()

  const locale = navigator.language || 'en'
  const weekdayFmt = new Intl.DateTimeFormat(locale, { weekday: 'short' })
  const timeFmt = new Intl.DateTimeFormat(locale, { hour: 'numeric', minute: '2-digit' })
  const monthFmt = new Intl.DateTimeFormat(locale, { month: 'short' })

  /** Most rows a cell shows before collapsing into "+n more". */
  const MAX_ROWS = 4

  const headings = $derived(
    daysFrom(startOfWeek(todayIso(), calendar.weekStart), 7).map((iso) =>
      weekdayFmt.format(new Date(iso + 'T00:00')),
    ),
  )

  interface Row {
    key: string
    label: string
    time: string
    color: string
    kind: 'planned' | 'actual' | 'event' | 'task'
    muted?: boolean
    onopen: () => void
  }

  /** Everything on one day, in the order the day runs. */
  function rowsOn(iso: string): Row[] {
    const out: Row[] = []

    for (const event of calendar.allDayOn(iso)) {
      const cal = calendar.calendarOf(event.calendarId)
      out.push({
        key: `ad:${event.id}`,
        label: event.title,
        time: '',
        color: cal?.color ?? 'var(--fg-subtle)',
        kind: 'event',
        muted: event.status === 'cancelled',
        onopen: () => (calendar.selection = { kind: 'event', id: event.id }),
      })
    }
    for (const task of calendar.tasksOn(iso)) {
      out.push({
        key: `task:${task.id}`,
        label: task.title,
        time: '',
        color: calendar.projectOf(task.projectId)?.color ?? 'var(--accent)',
        kind: 'task',
        muted: task.status === 'done',
        onopen: () => calendar.goto(iso),
      })
    }
    for (const slot of calendar.slotsOn(iso).sort((a, b) => a.start - b.start)) {
      out.push({
        key: slot.key,
        label: slot.title,
        time: timeFmt.format(new Date(slot.block?.start ?? slot.event!.start)),
        color: slot.color,
        kind: slot.kind,
        muted: slot.cancelled,
        onopen: () => calendar.select(slot),
      })
    }
    return out
  }

  const currentDay = $derived(todayIso())
</script>

<div class="month">
  <div class="headings">
    {#each headings as label, i (i)}<div class="heading">{label}</div>{/each}
  </div>

  <div class="weeks scroll">
    {#each days as iso (iso)}
      {@const rows = rowsOn(iso)}
      {@const first = Number(iso.slice(8, 10)) === 1}
      <div
        class="cell"
        class:outside={!calendar.inAnchorMonth(iso)}
        class:today={iso === currentDay}
      >
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
            {#if first}<span class="mon">{monthFmt.format(new Date(iso + 'T00:00'))}</span>{/if}
            {Number(iso.slice(8, 10))}
          </button>
          {#if calendar.hasEntry(iso)}
            <span class="wrote" title="You wrote something on this day">
              <Icon name="quote" size={10} weight={2} />
            </span>
          {/if}
        </div>

        <div class="rows">
          {#each rows.slice(0, MAX_ROWS) as row (row.key)}
            <button
              class="row {row.kind}"
              class:muted={row.muted}
              style="--c: {row.color}"
              title={row.label}
              onclick={row.onopen}
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
      </div>
    {/each}
  </div>
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

  /* The dot carries the whole visual language at this size: filled for a
     record, hollow for a plan, ringed for somebody else's calendar. Four
     shapes in six pixels is more than a month cell can spare, so the rest of
     the distinction is left to the day view. */
  .dot {
    width: 6px;
    height: 6px;
    flex: none;
    border-radius: 50%;
    border: 1.5px solid var(--c);
  }
  .row.actual .dot {
    background: var(--c);
  }
  .row.event .dot {
    background: color-mix(in oklab, var(--c) 35%, transparent);
  }
  .row.task .dot {
    border-radius: 1.5px;
  }

  .at {
    font-variant-numeric: tabular-nums;
    color: var(--fg-faint);
    flex: none;
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
</style>
