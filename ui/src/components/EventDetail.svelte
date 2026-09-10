<script lang="ts">
  // The right-hand rail below the timer.
  //
  // Two panels sharing one column, because they answer the two questions you
  // have in front of a calendar and you are never asking both at once:
  //
  //   nothing selected   "what should I be putting in the gaps?"
  //   something selected "what is this, and can I move it?"
  //
  // The unscheduled list is the drag source that makes the todo app and the
  // calendar one program: a task dropped on Tuesday afternoon becomes a
  // planned time block pointing at it, and drops off this list the moment it
  // does, because a plan you have already made is not a thing to nag about.

  import { calendar } from '../lib/calendar.svelte'
  import { formatMinutes, friendlyDate } from '../lib/format'
  import { locale, minutesBetween, offsetInDay } from '../lib/time'
  import { menu } from '../lib/menu.svelte'
  import { blockMenu, calendarTaskMenu, eventMenu } from '../lib/menus'
  import Icon from './Icon.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import type { CalendarEvent, Task, TimeBlock } from '../lib/types'

  const timeFmt = new Intl.DateTimeFormat(locale(), { hour: 'numeric', minute: '2-digit' })
  const dayFmt = new Intl.DateTimeFormat(locale(), {
    weekday: 'long',
    day: 'numeric',
    month: 'long',
  })

  let pendingDelete = $state<TimeBlock | null>(null)

  const selected = $derived(calendar.selected)
  const block = $derived(
    calendar.selection?.kind === 'block' ? (selected as TimeBlock | null) : null,
  )
  const event = $derived(
    calendar.selection?.kind === 'event' ? (selected as CalendarEvent | null) : null,
  )

  function span(start: string, end: string, allDay = false): string {
    if (allDay) return 'All day'
    return `${timeFmt.format(new Date(start))} – ${timeFmt.format(new Date(end))}`
  }

  function onTaskDragStart(e: DragEvent, task: Task) {
    e.dataTransfer?.setData('text/x-everyday-task', task.id)
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'copy'
  }

  /** Move a block by editing its clock time in the panel. */
  function setClock(field: 'start' | 'end', value: string) {
    if (!block) return
    const [h, m] = value.split(':').map(Number)
    if (h === undefined || m === undefined) return
    const minutes = h * 60 + m
    if (field === 'start') {
      calendar.moveBlock(block.id, block.localDate, minutes)
    } else {
      calendar.resizeBlock(block.id, minutes - offsetInDay(block.start, block.localDate))
    }
  }

  function clockValue(ts: string): string {
    const at = new Date(ts)
    return `${String(at.getHours()).padStart(2, '0')}:${String(at.getMinutes()).padStart(2, '0')}`
  }

  async function remove() {
    const doomed = pendingDelete
    pendingDelete = null
    if (doomed) await calendar.removeBlock(doomed.id)
  }
</script>

<aside class="rail">
  {#if block}
    {@const colour = calendar.colorOfBlock(block)}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="panel scroll"
      style="--c: {colour}"
      oncontextmenu={(e) => menu.show(e, blockMenu(block))}
    >
      <header class="phead">
        <span class="kind {block.kind}">
          {block.kind === 'actual' ? 'What happened' : 'Planned'}
        </span>
        <button
          class="x"
          title="Close"
          aria-label="Close"
          onclick={() => (calendar.selection = null)}
        >
          <Icon name="close" size={15} />
        </button>
      </header>

      <input
        class="titlefield"
        placeholder={calendar.titleOfBlock(block)}
        value={block.title}
        oninput={(e) => calendar.patch(block.id, { title: e.currentTarget.value })}
      />

      <p class="when">
        {dayFmt.format(new Date(block.start))}
        <span class="dot">·</span>
        {formatMinutes(minutesBetween(block.start, block.end))}
      </p>

      <div class="times">
        <label class="timefield">
          <span class="lbl">From</span>
          <input
            type="time"
            step="900"
            value={clockValue(block.start)}
            onchange={(e) => setClock('start', e.currentTarget.value)}
          />
        </label>
        <label class="timefield">
          <span class="lbl">To</span>
          <input
            type="time"
            step="900"
            value={clockValue(block.end)}
            onchange={(e) => setClock('end', e.currentTarget.value)}
          />
        </label>
      </div>

      {#if block.subject.type === 'task'}
        {@const task = calendar.taskOf(block.subject.id)}
        <div class="subject">
          <Icon name="check" size={14} weight={1.6} />
          <span class="stitle">{task?.title ?? 'A task'}</span>
          {#if task?.estimateMinutes}
            <span class="est">est. {formatMinutes(task.estimateMinutes)}</span>
          {/if}
        </div>
      {:else if block.subject.type === 'project'}
        {@const project = calendar.projectOf(block.subject.id)}
        <div class="subject">
          <span class="pmark">{project?.icon ?? '•'}</span>
          <span class="stitle">{project?.name ?? 'A project'}</span>
        </div>
      {/if}

      <textarea
        class="notes"
        rows="3"
        placeholder="Notes"
        value={block.notes}
        oninput={(e) => calendar.patch(block.id, { notes: e.currentTarget.value })}
      ></textarea>

      <div class="actions">
        {#if block.kind === 'planned'}
          <!-- Two rows rather than one edited row. The pair is the point:
               a plan overwritten by its outcome cannot be compared with it. -->
          <button class="btn" onclick={() => calendar.logAsDone(block.id)}>
            <Icon name="tick" size={14} weight={2} />
            This is what happened
          </button>
        {:else}
          <button class="btn" onclick={() => calendar.toggleKind(block.id)}>
            <Icon name="clock" size={14} />
            Make it a plan again
          </button>
        {/if}
        <button class="btn btn-ghost-danger" onclick={() => (pendingDelete = block)}>
          <Icon name="trash" size={14} />
          Delete
        </button>
      </div>
    </div>
  {:else if event}
    {@const cal = calendar.calendarOf(event.calendarId)}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="panel scroll"
      style="--c: {cal?.color ?? 'var(--fg-subtle)'}"
      oncontextmenu={(e) => menu.show(e, eventMenu(event))}
    >
      <header class="phead">
        <span class="kind event">
          <span class="cdot" aria-hidden="true"></span>
          {cal?.name ?? 'Subscribed calendar'}
        </span>
        <button
          class="x"
          title="Close"
          aria-label="Close"
          onclick={() => (calendar.selection = null)}
        >
          <Icon name="close" size={15} />
        </button>
      </header>

      <h2 class="etitle" class:cancelled={event.status === 'cancelled'}>{event.title}</h2>
      <p class="when">
        {dayFmt.format(new Date(event.start))}
        <span class="dot">·</span>
        {span(event.start, event.end, event.allDay)}
      </p>

      {#if event.status !== 'confirmed'}
        <p class="badge">{event.status === 'cancelled' ? 'Cancelled' : 'Tentative'}</p>
      {/if}

      {#if event.location}
        <p class="meta"><Icon name="place" size={14} /><span>{event.location}</span></p>
      {/if}
      {#if event.organizer}
        <p class="meta"><Icon name="inbox" size={14} /><span>{event.organizer}</span></p>
      {/if}
      {#if event.url}
        <p class="meta">
          <Icon name="link" size={14} /><a href={event.url} rel="noreferrer">{event.url}</a>
        </p>
      {/if}
      {#if event.description}
        <p class="desc">{event.description}</p>
      {/if}

      <!-- The one honest thing to say about a read-only calendar: this is a
           copy, and editing it here would not reach the people in the room. -->
      <p class="readonly">
        <Icon name="globe" size={13} />
        Read-only. Changes belong on {cal?.name ?? 'the calendar this came from'}.
      </p>

      <div class="actions">
        <button
          class="btn"
          onclick={() =>
            calendar.book({
              subject: { type: 'adhoc' },
              day: event.localDate,
              startMinutes: offsetInDay(event.start, event.localDate),
              minutes: minutesBetween(event.start, event.end),
              title: event.title,
            })}
        >
          <Icon name="clock" size={14} />
          Set this time aside
        </button>
        <button class="btn" onclick={() => calendar.startTimer({ type: 'adhoc' }, event.title)}>
          <Icon name="play" size={14} />
          I'm in it now
        </button>
      </div>
    </div>
  {:else}
    <div class="panel scroll">
      <div class="phead">
        <span class="eyebrow">Not scheduled</span>
      </div>
      <p class="blurb">Drag any of these onto the grid to set time aside for it.</p>

      {#each calendar.unscheduled as task (task.id)}
        {@const project = calendar.projectOf(task.projectId)}
        <!-- A button, not a div with a drag handler. Dragging is the better
             gesture and the rail is built around it, but a feature only a
             mouse can reach is one that half the people using it do not
             have -- so Enter finds the task a slot instead. -->
        <button
          class="todo"
          class:overdue={!!task.dueDate && task.dueDate < calendar.anchor}
          style="--c: {project?.color ?? 'var(--fg-subtle)'}"
          title="Drag onto the grid, or press Enter to find it a slot"
          draggable="true"
          ondragstart={(e) => onTaskDragStart(e, task)}
          onclick={() => calendar.scheduleNext(task.id)}
          oncontextmenu={(e) => menu.show(e, calendarTaskMenu(task))}
        >
          <span class="grip"><Icon name="grip" size={14} /></span>
          <span class="tinfo">
            <span class="tname">{task.title}</span>
            <span class="tmeta">
              {#if project}<span class="pname">{project.name}</span>{/if}
              {#if task.dueDate}<span>{friendlyDate(task.dueDate)}</span>{/if}
              {#if task.estimateMinutes}<span>{formatMinutes(task.estimateMinutes)}</span>{/if}
            </span>
          </span>
        </button>
      {/each}

      {#if calendar.unscheduled.length === 0}
        <p class="blank">
          Everything open has time booked against it. That is either very organised or a sign the
          todo list needs a look.
        </p>
      {/if}
    </div>
  {/if}
</aside>

{#if pendingDelete}
  <ConfirmDialog
    title="Delete this block of time?"
    detail="The task or project it points at is not affected."
    confirmLabel="Delete"
    onconfirm={remove}
    oncancel={() => (pendingDelete = null)}
  />
{/if}

<style>
  .rail {
    width: var(--list-w);
    flex: none;
    display: flex;
    flex-direction: column;
    min-height: 0;
    border-left: 1px solid var(--border);
    background: var(--bg-panel);
  }

  .panel {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    padding: var(--sp-4);
  }

  .phead {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2);
  }

  .kind {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--c);
  }
  .kind.planned {
    opacity: 0.85;
  }
  .cdot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--c);
  }

  .x {
    width: 24px;
    height: 24px;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .x:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .titlefield {
    width: 100%;
    border: none;
    background: none;
    font-size: var(--text-lg);
    font-weight: 600;
    letter-spacing: -0.012em;
    user-select: text;
  }
  .titlefield::placeholder {
    color: var(--fg-subtle);
    font-weight: 550;
  }
  .titlefield:focus {
    outline: none;
  }

  .etitle {
    font-size: var(--text-lg);
    font-weight: 600;
    letter-spacing: -0.012em;
  }
  .etitle.cancelled {
    text-decoration: line-through;
    color: var(--fg-subtle);
  }

  .when {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .when .dot {
    opacity: 0.5;
    margin: 0 3px;
  }

  .badge {
    align-self: flex-start;
    padding: 2px var(--sp-2);
    border-radius: 99px;
    background: color-mix(in oklab, var(--c) 15%, transparent);
    color: color-mix(in oklab, var(--c) 70%, var(--fg));
    font-size: var(--text-xs);
    font-weight: 600;
  }

  .times {
    display: flex;
    gap: var(--sp-2);
  }
  .timefield {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .lbl {
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--fg-faint);
  }
  .timefield input {
    height: 30px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    user-select: text;
  }
  .timefield input:focus {
    outline: none;
    border-color: var(--accent);
  }

  .subject {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-2);
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
    font-size: var(--text-sm);
    color: var(--fg-muted);
    min-width: 0;
  }
  .pmark {
    font-size: var(--text-sm);
    line-height: 1;
  }
  .stitle {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .est {
    font-size: var(--text-xs);
    color: var(--fg-faint);
    flex: none;
  }

  .notes {
    width: 100%;
    padding: var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    resize: vertical;
    user-select: text;
  }
  .notes:focus {
    outline: none;
    border-color: var(--accent);
  }

  .meta {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    font-size: var(--text-sm);
    color: var(--fg-muted);
    word-break: break-word;
  }
  .meta :global(a) {
    user-select: text;
  }

  .desc {
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-muted);
    white-space: pre-wrap;
    user-select: text;
  }

  .readonly {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: var(--text-xs);
    color: var(--fg-faint);
    line-height: var(--leading-normal);
  }

  .actions {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    margin-top: auto;
  }
  .actions .btn {
    justify-content: flex-start;
  }

  /* ── The unscheduled list ───────────────────────────────────────────── */

  .blurb {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    line-height: var(--leading-normal);
  }

  .todo {
    width: 100%;
    text-align: left;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-2);
    border-radius: var(--radius-sm);
    border: 1px solid var(--border);
    border-left: 3px solid var(--c);
    background: var(--bg-raised);
    cursor: grab;
    min-width: 0;
    transition:
      box-shadow var(--fast) var(--ease),
      border-color var(--fast) var(--ease);
  }
  .todo:hover {
    box-shadow: var(--shadow-sm);
    border-color: var(--border-strong);
    border-left-color: var(--c);
  }
  .todo:active {
    cursor: grabbing;
  }
  .todo.overdue .tname {
    color: var(--danger);
  }

  .grip {
    color: var(--fg-faint);
    display: flex;
    flex: none;
  }
  .tinfo {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .tname {
    font-size: var(--text-sm);
    font-weight: 550;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tmeta {
    display: flex;
    gap: var(--sp-2);
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .pname {
    color: color-mix(in oklab, var(--c) 70%, var(--fg-faint));
  }

  .blank {
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-faint);
  }
</style>
