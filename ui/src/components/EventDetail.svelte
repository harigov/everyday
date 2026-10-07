<script lang="ts">
  // The right-hand rail below the timer.
  //
  // Two panels sharing one column, because they answer the two questions you
  // have in front of a calendar and you are never asking both at once:
  //
  //   nothing selected   "what should I be putting in the gaps?"
  //   something selected "what is this, and can I move it?"
  //
  // -- and a third face for the moment something is being *written*: a new
  // event on an account calendar, or one of its events being changed. That
  // is `EventEditor`, here because it is the same rail and the same
  // question, "what is this", answered before the answer exists.
  //
  // The unscheduled list is the drag source that makes the todo app and the
  // calendar one program: a task dropped on Tuesday afternoon becomes a
  // planned time block pointing at it, and drops off this list the moment it
  // does, because a plan you have already made is not a thing to nag about.

  import { calendar } from '../lib/calendar.svelte'
  import {
    dayHeading,
    formatMinutes,
    friendlyDate,
    timeOfDay,
    toLocalTimeValue,
  } from '../lib/format'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import { eventInProgress, looksLikeOnlineCall } from '../lib/meeting-hosts'
  import { meetings } from '../lib/meetings.svelte'
  import { minutesBetween, offsetInDay } from '../lib/time'
  import { menu } from '../lib/menu.svelte'
  import {
    accountLabel,
    blockMenu,
    calendarChoiceItems,
    calendarTaskMenu,
    eventMenu,
  } from '../lib/menus'
  import { panels } from '../lib/panels.svelte'
  import { describeRecurrence } from '../lib/recurrence'
  import BlockProposalDetail from './BlockProposalDetail.svelte'
  import EventEditor from './EventEditor.svelte'
  import Icon from './Icon.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import RepeatField from './RepeatField.svelte'
  import type {
    CalendarEvent,
    CalendarId,
    EventDraft,
    EventScope,
    Recurrence,
    Task,
    TimeBlock,
  } from '../lib/types'

  let pendingDelete = $state<TimeBlock | null>(null)
  let startNotesError = $state<string | null>(null)

  /**
   * A deletion that has to ask which part is meant: one block of a series,
   * or one occurrence of a repeating event. `ConfirmDialog` has one way to
   * say yes, and these have two or three, so this is its shape with a row
   * of answers instead -- the same scrim, sheet and focus rules.
   */
  interface Choice {
    title: string
    detail: string
    options: { label: string; danger?: boolean; run: () => unknown }[]
  }
  let choice = $state<Choice | null>(null)
  /** Why the last write from the event panel did not go, beside its buttons. */
  let eventError = $state<string | null>(null)
  /** Asking the server whether an event repeats, before asking which of it to delete. */
  let checking = $state(false)

  function answer(option: Choice['options'][number]) {
    choice = null
    void option.run()
  }

  /**
   * Delete a block, asking first only where there is a real question. A
   * block on its own is a fifteen-second thing to make again, and the
   * existing dialog says so; one of a series could mean three things.
   */
  function askDeleteBlock(block: TimeBlock) {
    if (!block.series) {
      pendingDelete = block
      return
    }
    const id = block.id
    choice = {
      title: 'Delete a repeating block?',
      detail:
        `${describeRecurrence(block.series.rule, block.start, block.tz)}.` +
        (block.subject.type === 'adhoc'
          ? ''
          : ' The task or project it points at is not affected.'),
      options: [
        { label: 'Only this one', run: () => calendar.removeBlock(id) },
        {
          label: 'This and the ones after it',
          run: () => calendar.deleteBlockSeries(id, 'following'),
        },
        {
          label: 'Every one in the series',
          danger: true,
          run: () => calendar.deleteBlockSeries(id, 'all'),
        },
      ],
    }
  }

  /**
   * Delete an account calendar's event.
   *
   * Whether it repeats is asked of its server first -- the stored copy is
   * one occurrence and does not say -- and while that is in flight the
   * button says so. If the server will not answer, the question falls back
   * to the one that is always safe: this occurrence alone.
   */
  async function askDeleteEvent(ev: CalendarEvent) {
    eventError = null
    checking = true
    const loaded = await calendar.loadEditable(ev.id)
    checking = false
    const name = calendar.calendarOf(ev.calendarId)?.name ?? 'its calendar'
    const invited = (ev.attendees?.length ?? 0) > 0 || (loaded?.draft.attendees.length ?? 0) > 0
    const consequence =
      loaded && !loaded.own
        ? ` ${loaded.organizer || 'Somebody else'} organised it; this only takes it off ${name}.`
        : invited
          ? ' Everyone invited is sent a cancellation.'
          : ''
    const run = (scope: EventScope) => async () => {
      eventError = await calendar.deleteEvent(ev.id, scope)
    }
    choice = loaded?.recurring
      ? {
          title: `Delete “${ev.title}”?`,
          detail: `It is one of a series.${consequence}`,
          options: [
            { label: 'This event', run: run('occurrence') },
            { label: 'All events', danger: true, run: run('series') },
          ],
        }
      : {
          title: `Delete “${ev.title}”?`,
          detail: `It is removed from ${name}.${consequence}`,
          options: [{ label: 'Delete', danger: true, run: run('occurrence') }],
        }
  }

  /** Offer a block's calendars -- choosing an account's opens it as a draft bound there. */
  function chooseCalendarFor(e: MouseEvent, block: TimeBlock) {
    const box = (e.currentTarget as HTMLElement).getBoundingClientRect()
    const id = block.id
    menu.showAt(
      box.left,
      box.bottom + 4,
      calendarChoiceItems(null, (target) => {
        if (target) void calendar.moveBlockToCalendar(id, target)
      }),
    )
  }

  /** `Ann Lee <ann@x.com>` as a name to show and an address to hover. */
  function person(raw: string): { name: string; email: string } {
    const angled = /^(.*?)\s*<([^<>]+)>$/.exec(raw.trim())
    if (angled) return { name: angled[1]!.trim() || angled[2]!, email: angled[2]! }
    return { name: raw, email: raw.includes('@') ? raw : '' }
  }

  /** Could a new event go somewhere other than this computer? */
  const anyAccountCalendar = $derived(
    calendar.calendars.some((c) => c.origin.type === 'account' && c.access !== 'readOnly'),
  )

  // `meetings.starting` (not a local flag) is what disables the button
  // below -- see that field's own doc: a press here must also disable
  // `MeetingOfferBanner`'s and `NotesNav`'s own "take notes" controls, and
  // vice versa, so the three surfaces cannot race each other into starting
  // two recordings at once.
  async function takeNotes(ev: CalendarEvent) {
    startNotesError = null
    try {
      await meetings.startCapture({ eventId: ev.id, title: ev.title })
    } catch (e) {
      startNotesError = e instanceof Error ? e.message : String(e)
    }
  }

  const selected = $derived(calendar.selected)
  const block = $derived(
    calendar.selection?.kind === 'block' ? (selected as TimeBlock | null) : null,
  )
  const event = $derived(
    calendar.selection?.kind === 'event' ? (selected as CalendarEvent | null) : null,
  )

  function span(start: string, end: string, allDay = false): string {
    if (allDay) return 'All day'
    return `${timeOfDay(new Date(start))} – ${timeOfDay(new Date(end))}`
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
    return toLocalTimeValue(new Date(ts))
  }

  async function remove() {
    const doomed = pendingDelete
    pendingDelete = null
    if (doomed) await calendar.removeBlock(doomed.id)
  }
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (choice && e.key === 'Escape') choice = null
  }}
/>

<aside class="rail">
  {#if calendar.selectedProposal}
    <BlockProposalDetail proposal={calendar.selectedProposal} />
  {:else if calendar.draft}
    {@const open = calendar.draft}
    {#key open}
      <EventEditor
        draft={open.draft}
        calendarId={open.calendarId}
        moving={!!open.fromBlock}
        onpatch={(changes: Partial<EventDraft>) => calendar.patchDraft(changes)}
        oncalendar={(id: CalendarId | null) => calendar.setDraftCalendar(id)}
        onsave={() => calendar.saveDraft()}
        oncancel={() => calendar.cancelDraft()}
      />
    {/key}
  {:else if block}
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
        {dayHeading(new Date(block.start))}
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

      {#if block.subject.type === 'adhoc' && anyAccountCalendar}
        <!-- Where it lives. On this computer, until an account calendar is
             picked -- which opens it as a draft bound there, to be saved
             (with guests, if it wants them) or cancelled. -->
        <div class="setting">
          <span class="lbl">Calendar</span>
          <button
            class="pick choose"
            title="Move it onto an account's calendar"
            onclick={(e) => chooseCalendarFor(e, block)}
          >
            <span class="pdot" aria-hidden="true"></span>
            <span class="pname">This computer</span>
            <span class="chev" aria-hidden="true"><Icon name="chevron" size={11} /></span>
          </button>
        </div>
      {/if}

      {#if block.kind === 'planned'}
        <!-- Only a plan repeats: a record of time spent happened once. -->
        <div class="setting">
          <label class="lbl" for="repeat-{block.id}">Repeat</label>
          {#key block.id}
            <RepeatField
              id="repeat-{block.id}"
              value={block.series?.rule ?? null}
              start={block.start}
              tz={block.tz}
              confirm
              onchange={(rule: Recurrence | null) => calendar.repeatBlock(block.id, rule)}
            />
          {/key}
          {#if block.series}
            <p class="series">
              <Icon name="refresh" size={12} />
              <span>{describeRecurrence(block.series.rule, block.start, block.tz)}</span>
            </p>
            <!-- Each block of a series is its own record, so an edit here
                 stays here until it is carried forward on purpose. -->
            <button
              class="btn btn-outline small"
              title="Make the blocks after this one match it"
              onclick={() => calendar.applyToFollowing(block.id)}
            >
              Apply changes to following
            </button>
          {/if}
        </div>
      {/if}

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
        <button class="btn btn-ghost-danger" onclick={() => askDeleteBlock(block)}>
          <Icon name="trash" size={14} />
          Delete
        </button>
      </div>
    </div>
  {:else if event}
    {@const cal = calendar.calendarOf(event.calendarId)}
    {@const edit = calendar.editing?.id === event.id ? calendar.editing : null}
    {#if edit?.status === 'ready' && edit.event.own}
      {#key edit.id}
        <EventEditor
          draft={edit.draft}
          calendarId={edit.event.calendarId}
          editing={edit.event}
          onpatch={(changes: Partial<EventDraft>) => calendar.patchEdit(changes)}
          onsave={(scope: EventScope) => calendar.saveEdit(scope)}
          oncancel={() => calendar.cancelEdit()}
        />
      {/key}
    {:else}
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
          {dayHeading(new Date(event.start))}
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
        {#if event.attendees?.length}
          <!-- Who is coming, as the calendar gave them: a name where it had
               one, the address where it did not. The editor has them by
               address, with their answers, once it is opened. -->
          <div class="meta">
            <Icon name="people" size={14} />
            <ul class="guestlist" aria-label="Guests">
              {#each event.attendees as raw, i (i)}
                {@const who = person(raw)}
                <li title={who.email}>{who.name}</li>
              {/each}
            </ul>
          </div>
        {/if}
        {#if event.url}
          <p class="meta">
            <Icon name="link" size={14} /><a href={event.url} rel="noreferrer">{event.url}</a>
          </p>
        {/if}
        {#if event.description}
          <p class="desc">{event.description}</p>
        {/if}

        {#if edit?.status === 'loading'}
          <p class="readonly">
            <span class="spin"><Icon name="refresh" size={13} /></span>
            Asking {cal?.name ?? 'the calendar'} for the latest…
          </p>
        {:else if edit?.status === 'failed'}
          <p class="error">{edit.error}</p>
        {:else if edit?.status === 'ready' && !edit.event.own}
          <!-- Somebody else's meeting on your calendar: theirs to change, and
               a change made here would only diverge from the one they send. -->
          <p class="readonly">
            <Icon name="lock" size={13} />
            Only {edit.event.organizer || 'whoever organised it'} can change this event.
          </p>
        {/if}

        {#if cal?.access === 'needsSignIn'}
          <p class="readonly">
            <Icon name="lock" size={13} />
            <span>
              Sign in to {accountLabel(cal) ?? 'its account'} again to change events here.
              <button class="textlink" onclick={() => panels.openSettings('accounts')}
                >Open Accounts</button
              >
            </span>
          </p>
        {:else if cal?.access !== 'writable'}
          <!-- The one honest thing to say about a read-only calendar: this is a
               copy, and editing it here would not reach the people in the room. -->
          <p class="readonly">
            <Icon name="globe" size={13} />
            Read-only. Changes belong on {cal?.name ?? 'the calendar this came from'}.
          </p>
        {/if}

        <div class="actions">
          {#if cal?.access === 'writable'}
            <div class="pair">
              <button
                class="btn btn-outline"
                disabled={edit?.status === 'loading' ||
                  (edit?.status === 'ready' && !edit.event.own)}
                onclick={() => calendar.editEvent(event.id)}
              >
                <Icon name="pencil" size={14} />
                Edit
              </button>
              <button
                class="btn btn-ghost-danger"
                disabled={checking}
                onclick={() => void askDeleteEvent(event)}
              >
                <Icon name="trash" size={14} />
                {checking ? 'Checking…' : 'Delete'}
              </button>
            </div>
          {/if}
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
          {#if meetings.supported && !meetings.capture && looksLikeOnlineCall(event) && eventInProgress(event)}
            <button class="btn" disabled={meetings.starting} onclick={() => void takeNotes(event)}>
              <Icon name="mic" size={14} />
              {meetings.starting ? 'Starting…' : 'Take notes'}
            </button>
          {/if}
        </div>
        {#if eventError}<p class="notes-error">{eventError}</p>{/if}
        {#if startNotesError}<p class="notes-error">{startNotesError}</p>{/if}
      </div>
    {/if}
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

{#if choice}
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div class="scrim" onclick={() => (choice = null)}></div>
  <div class="sheet" role="alertdialog" aria-modal="true" aria-label={choice.title} use:trapFocus>
    <h2 class="sheet-title">{choice.title}</h2>
    {#if choice.detail}<p class="hint">{choice.detail}</p>{/if}
    <!-- The answers stacked, widest reach last, so the three read as a
         scale -- this one, these, all of them -- rather than as a row of
         buttons whose order has to be puzzled out. -->
    <div class="choices">
      {#each choice.options as option (option.label)}
        <button
          class="btn"
          class:btn-outline={!option.danger}
          class:btn-danger={option.danger}
          onclick={() => answer(option)}>{option.label}</button
        >
      {/each}
    </div>
    <div class="sheet-row">
      <span class="spacer"></span>
      <!-- Focus on Cancel, as in `ConfirmDialog`: this appears because
           something irreversible was asked for, and Enter on a sheet not yet
           read should not be what answers it. -->
      <button class="btn" use:focusOnMount onclick={() => (choice = null)}>Cancel</button>
    </div>
  </div>
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

  /* The foot is kept clear of the floating assistant button, which sits
     over this corner of the window: the actions at the bottom of a panel
     are the ones a person reaches for last, and one hidden under the
     button cannot be reached at all. */
  .panel {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    padding: var(--sp-4) var(--sp-4) var(--fab-clear);
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
  /* Edit and Delete side by side above the rest: the two things done to the
     event itself, apart from the two done with your own time. */
  .pair {
    display: flex;
    gap: var(--sp-2);
    margin-bottom: var(--sp-1);
  }
  .pair .btn {
    flex: 1;
    justify-content: center;
  }

  /* ── A block's calendar and repeat ──────────────────────────────────── */

  /* Not `.field`: that is app.css's text input, and a wrapper wearing it
     would be drawn as one. */
  .setting {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  .pick {
    width: 100%;
    min-width: 0;
    height: 30px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    font-size: var(--text-sm);
  }
  .choose {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    text-align: left;
    color: var(--fg);
  }
  .choose:hover {
    border-color: var(--border-strong);
  }
  .pdot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--accent);
  }
  .pname {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chev {
    display: flex;
    rotate: 90deg;
    color: var(--fg-faint);
  }
  .series {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    line-height: var(--leading-normal);
  }
  .small {
    align-self: flex-start;
    height: 28px;
    padding: 0 var(--sp-3);
    font-size: var(--text-sm);
  }

  /* ── An event's guests ──────────────────────────────────────────────── */

  .guestlist {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    margin: 0;
    padding: 0;
    list-style: none;
    user-select: text;
  }
  .guestlist li {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .textlink {
    color: var(--accent);
    font-size: inherit;
  }
  .textlink:hover {
    text-decoration: underline;
  }
  .spin {
    display: flex;
    animation: turn 1.1s linear infinite;
  }
  @keyframes turn {
    to {
      rotate: 360deg;
    }
  }

  /* ── The choice sheet ───────────────────────────────────────────────── */

  .sheet-title {
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
  }
  .sheet-title + .hint {
    margin-top: var(--sp-2);
  }
  .choices {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    margin-top: var(--sp-4);
  }
  .choices .btn {
    justify-content: center;
  }
  .notes-error {
    margin: var(--sp-2) 0 0;
    color: var(--danger);
    font-size: var(--text-xs);
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
