<script lang="ts">
  // The day and week grid: hours down the side, days across the top, and
  // everything that happens in them drawn in between.
  //
  // Three kinds of thing share the grid and are told apart by weight rather
  // than by colour, because colour is already spoken for -- it says which
  // project or which calendar something belongs to, and a scheme that also
  // used it for "planned versus done" would have nothing left to say either
  // with.
  //
  //   planned   a translucent wash, dashed left rail    -- an intention
  //   actual    a solid fill, solid left rail           -- a record
  //   event     a hairline outline, tinted ground       -- somebody else's
  //
  // Overlapping things are packed into lanes by `packLanes`, so two clashing
  // meetings each take half the column and an unrelated one later in the day
  // is untouched.

  import { calendar, DEFAULT_BLOCK_MINUTES, type Slot } from '../lib/calendar.svelte'
  import { formatMinutes } from '../lib/format'
  import {
    MIN_BLOCK_MINUTES,
    SNAP_MINUTES,
    minutesOfDay,
    packLanes,
    snap,
    todayIso,
  } from '../lib/time'
  import Icon from './Icon.svelte'

  let { days }: { days: string[] } = $props()

  /** Height of one hour. The single number the whole geometry is derived from. */
  const HOUR = 46
  const DAY_HEIGHT = HOUR * 24
  /** How close to the bottom edge counts as "resize" rather than "move". */
  const RESIZE_GRIP = 7

  const locale = navigator.language || 'en'
  const weekdayFmt = new Intl.DateTimeFormat(locale, { weekday: 'short' })
  const hourFmt = new Intl.DateTimeFormat(locale, { hour: 'numeric' })
  const timeFmt = new Intl.DateTimeFormat(locale, { hour: 'numeric', minute: '2-digit' })

  let body = $state<HTMLDivElement | null>(null)

  /**
   * A gesture in flight: creating, moving or resizing.
   *
   * Held here rather than written through on every pointer move. A drag is
   * sixty events a second and each one would be a store mutation, a re-pack
   * of every lane in the column and a queued write; instead the draft draws
   * itself and one change lands on pointer-up.
   */
  type Drag =
    | { mode: 'create'; day: string; from: number; to: number }
    | {
        mode: 'move'
        id: string
        day: string
        start: number
        length: number
        /** Where in the block it was grabbed, so it does not jump. */
        grabbed: number
        /** Where it began, so a click that moved nothing writes nothing. */
        from: { day: string; start: number }
      }
    | {
        mode: 'resize'
        id: string
        day: string
        start: number
        end: number
        from: { length: number }
      }
    | null
  let drag = $state<Drag>(null)
  /** A task being dragged in from the rail, and the slot it is over. */
  let dropAt = $state<{ day: string; minutes: number } | null>(null)

  const hours = Array.from({ length: 24 }, (_, h) => h)

  function hourLabel(h: number): string {
    const at = new Date()
    at.setHours(h, 0, 0, 0)
    return hourFmt.format(at)
  }

  function clockOf(minutes: number): string {
    const at = new Date()
    at.setHours(0, 0, 0, 0)
    at.setMinutes(minutes)
    return timeFmt.format(at)
  }

  /** Minutes from midnight for a pointer at `clientY` within `el`. */
  function minutesAt(el: HTMLElement, clientY: number): number {
    const rect = el.getBoundingClientRect()
    const raw = ((clientY - rect.top) / HOUR) * 60
    return Math.max(0, Math.min(24 * 60, raw))
  }

  // ── laying a column out ────────────────────────────────────────────────

  interface Placed {
    slot: Slot
    top: number
    height: number
    left: number
    width: number
  }

  function place(iso: string): Placed[] {
    const slots = calendar.slotsOn(iso).filter((s) => !isDragging(s))
    return packLanes(slots.map((s) => ({ ...s, start: s.start, end: s.end }))).map(
      ({ item, lane, lanes }) => {
        const slot = item as unknown as Slot
        const height = Math.max(
          ((slot.end - slot.start) / 60) * HOUR,
          (MIN_BLOCK_MINUTES / 60) * HOUR,
        )
        // Lanes overlap slightly rather than tiling exactly: a 4% bleed lets
        // three clashing meetings still show a sliver of the one behind,
        // which is how you notice there are three.
        const width = lanes === 1 ? 100 : 100 / lanes + 4
        return {
          slot,
          top: (slot.start / 60) * HOUR,
          height,
          left: (lane * (100 - width)) / Math.max(1, lanes - 1),
          width,
        }
      },
    )
  }

  /** Is this slot the one currently under the pointer? It is drawn as the draft. */
  function isDragging(slot: Slot): boolean {
    return (
      !!drag &&
      (drag.mode === 'move' || drag.mode === 'resize') &&
      slot.block?.id === drag.id
    )
  }

  /** The draft rectangle, if a gesture is in flight on `iso`. */
  function draftOn(iso: string): { top: number; height: number; label: string } | null {
    if (!drag || drag.day !== iso) return null
    const [from, to] =
      drag.mode === 'create'
        ? [Math.min(drag.from, drag.to), Math.max(drag.from, drag.to)]
        : drag.mode === 'move'
          ? [drag.start, drag.start + drag.length]
          : [drag.start, drag.end]
    const height = Math.max(((to - from) / 60) * HOUR, (MIN_BLOCK_MINUTES / 60) * HOUR)
    return {
      top: (from / 60) * HOUR,
      height,
      label: `${clockOf(from)} – ${clockOf(Math.max(to, from + MIN_BLOCK_MINUTES))}`,
    }
  }

  // ── gestures ───────────────────────────────────────────────────────────

  // Note what these handlers deliberately do *not* do: capture the pointer.
  // Capturing would send every move to the column the gesture started in,
  // which is precisely the thing that must not happen -- dragging Tuesday's
  // block onto Thursday is the gesture this grid exists for. Instead each
  // column reports the moves that happen over it, and a window-level
  // pointerup ends the gesture even if it finishes off the grid entirely.

  function onColumnPointerDown(e: PointerEvent, iso: string) {
    // Only the background starts a create; a pointerdown on a block is
    // handled below and stops propagating.
    if (e.button !== 0) return
    const at = snap(minutesAt(e.currentTarget as HTMLElement, e.clientY))
    drag = { mode: 'create', day: iso, from: at, to: at + SNAP_MINUTES }
  }

  function onSlotPointerDown(e: PointerEvent, placed: Placed, iso: string) {
    const slot = placed.slot
    if (!slot.movable || !slot.block || e.button !== 0) {
      // Not draggable, but still selectable.
      calendar.select(slot)
      e.stopPropagation()
      return
    }
    e.stopPropagation()
    calendar.select(slot)
    const el = (e.currentTarget as HTMLElement).parentElement!
    const box = (e.currentTarget as HTMLElement).getBoundingClientRect()

    if (box.bottom - e.clientY <= RESIZE_GRIP) {
      drag = {
        mode: 'resize',
        id: slot.block.id,
        day: iso,
        start: slot.start,
        end: slot.end,
        from: { length: slot.end - slot.start },
      }
    } else {
      drag = {
        mode: 'move',
        id: slot.block.id,
        day: iso,
        start: slot.start,
        length: Math.max(MIN_BLOCK_MINUTES, slot.end - slot.start),
        grabbed: minutesAt(el, e.clientY) - slot.start,
        from: { day: iso, start: slot.start },
      }
    }
  }

  function onColumnPointerMove(e: PointerEvent, iso: string) {
    if (!drag) return
    const column = e.currentTarget as HTMLElement
    const at = minutesAt(column, e.clientY)
    if (drag.mode === 'create') {
      drag = { ...drag, to: snap(at) }
    } else if (drag.mode === 'move') {
      // The day comes from the column the pointer is over, so a block can be
      // dragged across the week and not merely up and down its own day.
      drag = { ...drag, day: iso, start: Math.max(0, snap(at - drag.grabbed)) }
    } else {
      drag = { ...drag, end: Math.max(drag.start + MIN_BLOCK_MINUTES, snap(at)) }
    }
  }

  async function endGesture() {
    const gesture = drag
    drag = null
    if (!gesture) return

    if (gesture.mode === 'create') {
      const from = Math.min(gesture.from, gesture.to)
      const to = Math.max(gesture.from, gesture.to)
      // A click rather than a drag: give it a sensible hour rather than the
      // fifteen minutes the pointer technically covered.
      const minutes = to - from <= SNAP_MINUTES ? DEFAULT_BLOCK_MINUTES : to - from
      await calendar.book({
        subject: { type: 'adhoc' },
        day: gesture.day,
        startMinutes: from,
        minutes,
      })
    } else if (gesture.mode === 'move') {
      // Only if it actually moved. A plain click on a block selects it, and
      // that must not queue a write of identical values -- which would bump
      // `updatedAt` on every glance and make "recently changed" meaningless.
      if (gesture.day !== gesture.from.day || gesture.start !== gesture.from.start) {
        calendar.moveBlock(gesture.id, gesture.day, gesture.start)
      }
    } else if (gesture.end - gesture.start !== gesture.from.length) {
      calendar.resizeBlock(gesture.id, gesture.end - gesture.start)
    }
  }

  // ── dropping a task in from the rail ───────────────────────────────────

  function onDragOver(e: DragEvent, iso: string) {
    if (!e.dataTransfer?.types.includes('text/x-everyday-task')) return
    e.preventDefault()
    e.dataTransfer.dropEffect = 'copy'
    dropAt = { day: iso, minutes: snap(minutesAt(e.currentTarget as HTMLElement, e.clientY)) }
  }

  async function onDrop(e: DragEvent, iso: string) {
    const id = e.dataTransfer?.getData('text/x-everyday-task')
    const at = dropAt
    dropAt = null
    if (!id) return
    e.preventDefault()
    await calendar.scheduleTask(id, iso, at?.minutes ?? snap(minutesAt(e.currentTarget as HTMLElement, e.clientY)))
  }

  // ── the "now" line ─────────────────────────────────────────────────────

  const currentDay = $derived(todayIso())
  // `calendar.now` ticks, so this recomputes with it and the line creeps
  // down the column without a second timer in here.
  const nowMinutes = $derived.by(() => {
    void calendar.now
    return minutesOfDay(new Date())
  })

  // Open on the working day rather than at midnight: eight hours of empty
  // grid above the first thing on the calendar is eight hours of scrolling.
  //
  // Keyed on the window rather than on its contents, so booking an hour at
  // seven in the morning does not yank the view back up to it.
  let scrolledFor = $state('')
  $effect(() => {
    const key = `${days[0]}:${days.length}`
    if (!body || scrolledFor === key) return
    scrolledFor = key
    const first = Math.min(
      ...days.flatMap((d) => calendar.slotsOn(d).map((s) => s.start)),
      8 * 60,
    )
    body.scrollTop = Math.max(0, ((first - 30) / 60) * HOUR)
  })
</script>

<svelte:window onpointerup={endGesture} onpointercancel={() => (drag = null)} />

<div class="grid" style="--hour: {HOUR}px; --day-h: {DAY_HEIGHT}px; --cols: {days.length}">
  <!-- Day headings, and the band of things that have no time of day. -->
  <div class="head">
    <div class="corner"></div>
    {#each days as iso (iso)}
      {@const totals = calendar.totalsOn(iso)}
      <div class="dayhead" class:now={iso === currentDay}>
        <button class="daylabel" onclick={() => { calendar.view = 'day'; calendar.goto(iso) }}>
          <span class="weekday">{weekdayFmt.format(new Date(iso + 'T00:00'))}</span>
          <span class="daynum">{Number(iso.slice(8, 10))}</span>
        </button>
        <div class="daymeta">
          {#if calendar.hasEntry(iso)}
            <span class="wrote" title="You wrote something on this day">
              <Icon name="quote" size={11} weight={2} />
            </span>
          {/if}
          {#if totals.logged > 0}
            <span class="tally logged" title="Time logged">{formatMinutes(totals.logged)}</span>
          {:else if totals.planned > 0}
            <span class="tally" title="Time planned">{formatMinutes(totals.planned)}</span>
          {/if}
        </div>
      </div>
    {/each}
  </div>

  <div class="band">
    <div class="bandgutter">All day</div>
    {#each days as iso (iso)}
      <div class="bandcell">
        {#each calendar.allDayOn(iso) as event (event.id)}
          {@const cal = calendar.calendarOf(event.calendarId)}
          <button
            class="chip event"
            class:cancelled={event.status === 'cancelled'}
            style="--c: {cal?.color ?? 'var(--fg-subtle)'}"
            title={event.title + (cal ? ` — ${cal.name}` : '')}
            onclick={() => (calendar.selection = { kind: 'event', id: event.id })}
          >{event.title}</button>
        {/each}
        {#each calendar.tasksOn(iso) as task (task.id)}
          <button
            class="chip task"
            class:done={task.status === 'done'}
            style="--c: {calendar.projectOf(task.projectId)?.color ?? 'var(--accent)'}"
            title="Due: {task.title}"
            draggable="true"
            ondragstart={(e) => e.dataTransfer?.setData('text/x-everyday-task', task.id)}
            onclick={() => calendar.selection = null}
          >
            <Icon name={task.status === 'done' ? 'check' : 'circle'} size={11} weight={2} />
            {task.title}
          </button>
        {/each}
      </div>
    {/each}
  </div>

  <!-- The grid proper. -->
  <div class="body scroll" bind:this={body}>
    <div class="canvas">
      <div class="gutter">
        {#each hours as h (h)}
          <div class="hour"><span class="hourlabel">{h === 0 ? '' : hourLabel(h)}</span></div>
        {/each}
      </div>

      {#each days as iso (iso)}
        {@const draft = draftOn(iso)}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div
          class="col"
          class:today={iso === currentDay}
          onpointerdown={(e) => onColumnPointerDown(e, iso)}
          onpointermove={(e) => onColumnPointerMove(e, iso)}
          ondragover={(e) => onDragOver(e, iso)}
          ondragleave={() => (dropAt = null)}
          ondrop={(e) => onDrop(e, iso)}
        >
          {#each hours as h (h)}<div class="rule" style="top: {h * HOUR}px"></div>{/each}

          {#each place(iso) as p (p.slot.key)}
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div
              class="slot {p.slot.kind}"
              class:cancelled={p.slot.cancelled}
              class:free={p.slot.free}
              class:live={p.slot.live}
              class:sel={p.slot.block
                ? calendar.selection?.kind === 'block' && calendar.selection.id === p.slot.block.id
                : calendar.selection?.kind === 'event' && calendar.selection.id === p.slot.event?.id}
              style="top: {p.top}px; height: {p.height}px; left: {p.left}%; width: {p.width}%; --c: {p.slot.color}"
              onpointerdown={(e) => onSlotPointerDown(e, p, iso)}
            >
              <span class="slottime">{clockOf(p.slot.start)}</span>
              <span class="slottitle">{p.slot.title}</span>
              {#if p.height > 42 && p.slot.subtitle}
                <span class="slotsub">{p.slot.subtitle}</span>
              {/if}
              {#if p.slot.movable}<span class="handle"></span>{/if}
            </div>
          {/each}

          {#if draft}
            <div class="draft" style="top: {draft.top}px; height: {draft.height}px">
              {draft.label}
            </div>
          {/if}

          {#if dropAt?.day === iso}
            <div class="dropline" style="top: {(dropAt.minutes / 60) * HOUR}px"></div>
          {/if}

          {#if iso === currentDay}
            <div class="nowline" style="top: {(nowMinutes / 60) * HOUR}px">
              <span class="nowdot"></span>
            </div>
          {/if}
        </div>
      {/each}
    </div>
  </div>
</div>

<style>
  .grid {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    --gutter: 54px;
  }

  /* ── Headings ───────────────────────────────────────────────────────── */

  .head,
  .band {
    display: grid;
    grid-template-columns: var(--gutter) repeat(var(--cols), minmax(0, 1fr));
    flex: none;
  }

  .dayhead {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--sp-2);
    padding: var(--sp-2) var(--sp-2) var(--sp-1);
    border-left: 1px solid var(--border);
    min-width: 0;
  }
  .daylabel {
    display: flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
    border-radius: var(--radius-sm);
    padding: 0 3px;
  }
  .daylabel:hover { background: var(--bg-hover); }
  .weekday {
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }
  .daynum {
    font-size: var(--text-md);
    font-weight: 600;
    letter-spacing: -0.01em;
    font-variant-numeric: tabular-nums;
    color: var(--fg-muted);
  }
  /* Today is marked by weight and colour, not by a filled pill: a solid disc
     in the header competes with the blocks below it for the eye. */
  .now .weekday, .now .daynum { color: var(--accent); }
  .now .daynum { font-weight: 700; }

  .daymeta { display: flex; align-items: center; gap: 6px; flex: none; }
  .wrote { color: var(--fg-faint); display: flex; }
  .tally {
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: var(--fg-faint);
  }
  .tally.logged { color: var(--fg-subtle); font-weight: 550; }

  /* ── The all-day band ───────────────────────────────────────────────── */

  .band {
    border-bottom: 1px solid var(--border);
    min-height: 30px;
    max-height: 104px;
    overflow-y: auto;
    scrollbar-width: none;
  }
  .bandgutter {
    padding: 6px var(--sp-2) 6px 0;
    text-align: right;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .bandcell {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 4px 3px;
    border-left: 1px solid var(--border);
    min-width: 0;
  }

  .chip {
    display: flex;
    align-items: center;
    gap: 4px;
    height: 19px;
    padding: 0 6px;
    border-radius: 4px;
    font-size: var(--text-xs);
    font-weight: 500;
    text-align: left;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    background: color-mix(in oklab, var(--c) 15%, transparent);
    color: color-mix(in oklab, var(--c) 72%, var(--fg));
    border-left: 2px solid var(--c);
  }
  .chip:hover { background: color-mix(in oklab, var(--c) 24%, transparent); }
  /* A task on the band is a deadline, not a booking: outlined rather than
     filled, so it does not read as time that has been set aside. */
  .chip.task { background: none; border: 1px solid color-mix(in oklab, var(--c) 40%, transparent); border-left-width: 2px; }
  .chip.task:hover { background: color-mix(in oklab, var(--c) 12%, transparent); }
  .chip.done { opacity: 0.5; text-decoration: line-through; }
  .chip.cancelled { opacity: 0.55; text-decoration: line-through; }

  /* ── The grid ───────────────────────────────────────────────────────── */

  .body { flex: 1; min-height: 0; }
  .canvas {
    position: relative;
    display: grid;
    grid-template-columns: var(--gutter) repeat(var(--cols), minmax(0, 1fr));
    height: var(--day-h);
  }

  .gutter { position: relative; }
  .hour { height: var(--hour); position: relative; }
  .hourlabel {
    position: absolute;
    top: -7px;
    right: var(--sp-2);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: var(--fg-faint);
    background: var(--bg);
    padding: 0 2px;
  }

  .col {
    position: relative;
    border-left: 1px solid var(--border);
    min-width: 0;
    touch-action: none;
  }
  .col.today { background: color-mix(in oklab, var(--accent) 3.5%, transparent); }

  .rule {
    position: absolute;
    left: 0;
    right: 0;
    height: 1px;
    background: var(--border);
    opacity: 0.65;
    pointer-events: none;
  }

  /* ── Blocks and events ──────────────────────────────────────────────── */

  .slot {
    position: absolute;
    overflow: hidden;
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding: 2px 5px 2px 7px;
    border-radius: 5px;
    font-size: var(--text-xs);
    line-height: 1.25;
    cursor: pointer;
    transition: box-shadow var(--fast) var(--ease), filter var(--fast) var(--ease);
  }
  .slot:hover { filter: brightness(1.04); z-index: 3; }
  .slot.sel { box-shadow: 0 0 0 2px var(--c), var(--shadow); z-index: 4; }

  .slottime { font-variant-numeric: tabular-nums; opacity: 0.72; font-size: 10px; }
  .slottitle {
    font-weight: 570;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .slotsub { opacity: 0.66; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }

  /* An intention: a wash, and a dashed rail. Deliberately lighter than the
     record beside it -- a plan is a claim about the future, and it should
     not look as settled as something that actually happened. */
  .slot.planned {
    background: color-mix(in oklab, var(--c) 13%, var(--bg-raised));
    color: color-mix(in oklab, var(--c) 62%, var(--fg));
    border-left: 3px dashed var(--c);
    cursor: grab;
  }

  /* A record: solid, and heavier. */
  .slot.actual {
    background: color-mix(in oklab, var(--c) 26%, var(--bg-raised));
    color: color-mix(in oklab, var(--c) 74%, var(--fg));
    border-left: 3px solid var(--c);
    cursor: grab;
  }
  .slot.actual .slottitle { font-weight: 620; }

  /* Somebody else's: outlined rather than filled, so a busy work calendar
     never drowns out the two hours you set aside for yourself. */
  .slot.event {
    background: color-mix(in oklab, var(--c) 9%, var(--bg-raised));
    color: color-mix(in oklab, var(--c) 66%, var(--fg));
    border: 1px solid color-mix(in oklab, var(--c) 34%, transparent);
    border-left: 3px solid var(--c);
  }
  .slot.event.free { border-left-style: dotted; opacity: 0.8; }
  .slot.cancelled { opacity: 0.55; }
  .slot.cancelled .slottitle { text-decoration: line-through; }

  /* The one being timed right now: a soft pulse, so it is findable in a
     full week without being a flashing light. */
  .slot.live {
    border-left-style: solid;
    box-shadow: 0 0 0 1.5px color-mix(in oklab, var(--c) 55%, transparent);
    animation: breathe 2.4s var(--ease) infinite;
    cursor: default;
  }
  @keyframes breathe {
    0%, 100% { box-shadow: 0 0 0 1.5px color-mix(in oklab, var(--c) 50%, transparent); }
    50% { box-shadow: 0 0 0 3.5px color-mix(in oklab, var(--c) 22%, transparent); }
  }

  /* The bottom few pixels resize instead of moving. */
  .handle {
    position: absolute;
    left: 0; right: 0; bottom: 0;
    height: 7px;
    cursor: ns-resize;
  }

  .draft {
    position: absolute;
    left: 0; right: 2px;
    display: flex;
    align-items: flex-start;
    padding: 3px 6px;
    border-radius: 5px;
    background: color-mix(in oklab, var(--accent) 16%, var(--bg-raised));
    border: 1px dashed var(--accent);
    color: var(--accent);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    font-weight: 550;
    pointer-events: none;
    z-index: 5;
  }

  /* Where a dragged task would land. */
  .dropline {
    position: absolute;
    left: 0; right: 0;
    height: 2px;
    background: var(--accent);
    border-radius: 2px;
    pointer-events: none;
    z-index: 5;
  }

  .nowline {
    position: absolute;
    left: 0; right: 0;
    height: 1.5px;
    background: var(--danger);
    pointer-events: none;
    z-index: 6;
  }
  .nowdot {
    position: absolute;
    left: -3px; top: -3px;
    width: 7.5px; height: 7.5px;
    border-radius: 50%;
    background: var(--danger);
  }
</style>
