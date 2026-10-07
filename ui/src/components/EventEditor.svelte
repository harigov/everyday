<script lang="ts">
  // An event as it is being written: a new one, before it exists anywhere,
  // or an account calendar's event read fresh from its server to be changed.
  //
  // One editor for both, because they are the same form -- a title, a time,
  // a repeat, a place, the people, a description -- and two copies would
  // disagree within a month about which of those an event has. What differs
  // is small and said where it differs: a new event chooses its calendar,
  // an existing one stays on its own; Save on an existing repeating event
  // asks whether it means this one or all of them.
  //
  // The working copy lives in the calendar store, not here (`calendar.draft`
  // and `calendar.editing`), and every change goes back through `onpatch`.
  // That is what lets the grid draw a new event's slot from the same draft
  // the fields show, and lets dragging the slot move the times typed here.
  //
  // Times are edited on this machine's clock, as the grid draws them. An
  // all-day event's days are the days of the zone it is quoted in, which is
  // what "all day" means to the server it is written to.

  import { calendar } from '../lib/calendar.svelte'
  import { formatMinutes, toLocalTimeValue } from '../lib/format'
  import { focusOnMount } from '../lib/focus'
  import { menu } from '../lib/menu.svelte'
  import { calendarChoiceItems } from '../lib/menus'
  import { carryRule, dayIn, midnightIn, sameRecurrence } from '../lib/recurrence'
  import { addDays, instantAt, isoDate, minutesBetween, offsetInDay, startOfDay } from '../lib/time'
  import { looksLikeEmail } from '../lib/guests'
  import GuestsField from './GuestsField.svelte'
  import Icon from './Icon.svelte'
  import RepeatField from './RepeatField.svelte'
  import type {
    Attendee,
    CalendarId,
    EditableEvent,
    EventDraft,
    EventScope,
    Recurrence,
  } from '../lib/types'

  let {
    draft,
    calendarId,
    editing = null,
    moving = false,
    onpatch,
    oncalendar,
    onsave,
    oncancel,
  }: {
    /** The working copy. Read here; changed only through `onpatch`. */
    draft: EventDraft
    /** Where it is written: an account calendar, or `null` for this computer. */
    calendarId: CalendarId | null
    /** The event as its server had it, when changing one; absent for a new one. */
    editing?: EditableEvent | null
    /** A block of your own on its way to an account calendar. */
    moving?: boolean
    onpatch: (changes: Partial<EventDraft>) => unknown
    /** Offered only for a new event: an existing one stays on its calendar. */
    oncalendar?: (id: CalendarId | null) => unknown
    /** Write it. Answers why not, to be shown here. */
    onsave: (scope: EventScope) => Promise<string | null>
    oncancel: () => unknown
  } = $props()

  const uid = $props.id()

  const isNew = $derived(!editing)
  const local = $derived(calendarId === null)
  const cal = $derived(calendarId ? calendar.calendarOf(calendarId) : null)
  const colour = $derived(calendar.colorOfCalendar(calendarId))

  /** The first day it covers: the zone's own day when all-day, this machine's otherwise. */
  const firstDay = $derived(
    draft.allDay ? dayIn(draft.start, draft.tz) : isoDate(new Date(draft.start)),
  )
  /**
   * The last day an all-day event covers. Its end is the midnight *after*
   * that day, so a moment before the end is on it.
   */
  const lastDay = $derived.by(() => {
    if (!draft.allDay) return firstDay
    const last = dayIn(new Date(Date.parse(draft.end) - 1).toISOString(), draft.tz)
    return last < firstDay ? firstDay : last
  })
  const fromClock = $derived(toLocalTimeValue(new Date(draft.start)))
  const toClock = $derived(toLocalTimeValue(new Date(draft.end)))
  const endsNextDay = $derived(!draft.allDay && isoDate(new Date(draft.end)) > firstDay)

  let saving = $state(false)
  let error = $state<string | null>(null)
  /** Save was pressed on one of a series, and is waiting to hear which part. */
  let askingScope = $state(false)
  /** An address typed into Guests and not yet a chip. */
  let guestText = $state('')

  /**
   * Was the repeat itself changed? Then the change can only mean the whole
   * series -- "this one occurrence now repeats monthly" is not a thing any
   * calendar can hold -- and Save does not ask.
   */
  const ruleChanged = $derived(
    !!editing &&
      !sameRecurrence(editing.draft.recurrence, draft.recurrence, editing.draft.start, draft.tz),
  )

  const hasGuests = $derived(!local && draft.attendees.length > 0)
  const saveLabel = $derived(
    saving
      ? 'Saving…'
      : !hasGuests
        ? 'Save'
        : isNew
          ? 'Save and send invitations'
          : 'Save and notify guests',
  )

  /** `HH:MM` to minutes since midnight, or `null` for a cleared field. */
  function clockMinutes(clock: string): number | null {
    const [h, m] = clock.split(':').map(Number)
    if (h === undefined || m === undefined || !Number.isFinite(h) || !Number.isFinite(m)) {
      return null
    }
    return h * 60 + m
  }

  function daysApart(a: string, b: string): number {
    return Math.round((startOfDay(b).getTime() - startOfDay(a).getTime()) / 86_400_000)
  }

  /** Move it to another day, keeping its time, its length and what its repeat meant. */
  function setDay(day: string) {
    if (!day) return
    const recurrence = carryRule(draft.recurrence, firstDay, day)
    if (draft.allDay) {
      const span = daysApart(firstDay, lastDay)
      onpatch({
        start: midnightIn(day, draft.tz),
        end: midnightIn(addDays(day, span + 1), draft.tz),
        recurrence,
      })
      return
    }
    const from = offsetInDay(draft.start, firstDay)
    const length = minutesBetween(draft.start, draft.end)
    onpatch({ start: instantAt(day, from), end: instantAt(day, from + length), recurrence })
  }

  function setLastDay(day: string) {
    if (!day) return
    onpatch({ end: midnightIn(addDays(day < firstDay ? firstDay : day, 1), draft.tz) })
  }

  /** A new start keeps the length, the way dragging it on the grid does. */
  function setFrom(clock: string) {
    const minutes = clockMinutes(clock)
    if (minutes === null) return
    const length = minutesBetween(draft.start, draft.end)
    onpatch({ start: instantAt(firstDay, minutes), end: instantAt(firstDay, minutes + length) })
  }

  /**
   * An end earlier than the start is the next morning -- "22:00 to 01:00"
   * is a late night, not a mistake. Equal to it is a mistake, and Save says
   * so.
   */
  function setTo(clock: string) {
    const minutes = clockMinutes(clock)
    if (minutes === null) return
    const from = offsetInDay(draft.start, firstDay)
    onpatch({ end: instantAt(firstDay, minutes < from ? minutes + 24 * 60 : minutes) })
  }

  function setAllDay(on: boolean) {
    if (on) {
      onpatch({
        allDay: true,
        start: midnightIn(firstDay, draft.tz),
        end: midnightIn(addDays(firstDay, 1), draft.tz),
      })
    } else {
      // The first hour of the working day: an all-day event has no time of
      // its own to go back to.
      onpatch({
        allDay: false,
        start: instantAt(firstDay, 9 * 60),
        end: instantAt(firstDay, 10 * 60),
      })
    }
  }

  function chooseCalendar(e: MouseEvent) {
    const box = (e.currentTarget as HTMLElement).getBoundingClientRect()
    // A block on its way to a calendar is already on this computer: sending
    // it "back" would write a second copy beside the first. Cancel is how
    // it stays where it is.
    menu.showAt(
      box.left,
      box.bottom + 4,
      calendarChoiceItems(calendarId, (id) => oncalendar?.(id), { local: !moving }),
    )
  }

  /** What stops this being saved, in a sentence -- or nothing. */
  function problem(): string | null {
    if (!draft.title.trim()) return 'Give it a title.'
    if (!draft.allDay && Date.parse(draft.end) <= Date.parse(draft.start)) {
      return 'It has to end after it starts.'
    }
    if (!local) {
      if (guestText.trim()) return `Finish adding “${guestText.trim()}” as a guest, or clear it.`
      const bad = draft.attendees.find((g) => !looksLikeEmail(g.email))
      if (bad) return `“${bad.email}” is not an email address.`
    }
    const until = draft.recurrence?.until
    if (until && until < firstDay) return 'The repeat cannot end before the event first happens.'
    return null
  }

  async function save() {
    error = problem()
    if (error) return
    if (editing?.recurring && !ruleChanged) {
      askingScope = true
      return
    }
    await commit(ruleChanged ? 'series' : 'occurrence')
  }

  async function commit(scope: EventScope) {
    askingScope = false
    saving = true
    error = await onsave(scope)
    saving = false
  }
</script>

<div class="panel scroll" class:fresh={isNew} style="--c: {colour}">
  <header class="phead">
    <span class="kind">
      <span class="cdot" aria-hidden="true"></span>
      {#if isNew}
        {moving ? 'Move to a calendar' : 'New event'}
      {:else}
        {cal?.name ?? 'Event'}
      {/if}
    </span>
    <button class="x" title="Close without saving" aria-label="Close" onclick={() => oncancel()}>
      <Icon name="close" size={15} />
    </button>
  </header>

  <input
    class="titlefield"
    placeholder="Add a title"
    aria-label="Title"
    value={draft.title}
    use:focusOnMount={isNew && !draft.title}
    oninput={(e) => onpatch({ title: e.currentTarget.value })}
    onkeydown={(e) => {
      if (e.key === 'Enter' && !e.isComposing) {
        e.preventDefault()
        void save()
      }
    }}
  />

  {#if isNew}
    <div class="row">
      <span class="lbl">Calendar</span>
      <button class="pick choose" title="Where this is saved" onclick={chooseCalendar}>
        <span class="pdot" aria-hidden="true"></span>
        <span class="pname">{cal?.name ?? 'This computer'}</span>
        <span class="chev" aria-hidden="true"><Icon name="chevron" size={11} /></span>
      </button>
    </div>
  {/if}

  <div class="times">
    <label class="timefield">
      <span class="lbl">{draft.allDay ? 'From' : 'Date'}</span>
      <input type="date" value={firstDay} onchange={(e) => setDay(e.currentTarget.value)} />
    </label>
    {#if draft.allDay}
      <label class="timefield">
        <span class="lbl">To</span>
        <input
          type="date"
          min={firstDay}
          value={lastDay}
          onchange={(e) => setLastDay(e.currentTarget.value)}
        />
      </label>
    {/if}
  </div>

  {#if !draft.allDay}
    <div class="times">
      <label class="timefield">
        <span class="lbl">From</span>
        <input
          type="time"
          step="900"
          value={fromClock}
          onchange={(e) => setFrom(e.currentTarget.value)}
        />
      </label>
      <label class="timefield">
        <span class="lbl">
          To
          {#if endsNextDay}<span class="nextday">next day</span>{/if}
        </span>
        <input
          type="time"
          step="900"
          value={toClock}
          onchange={(e) => setTo(e.currentTarget.value)}
        />
      </label>
    </div>
    <p class="length">{formatMinutes(minutesBetween(draft.start, draft.end))}</p>
  {/if}

  <!-- This computer has nowhere to draw an all-day block of its own -- the
       band over the grid holds calendars' days -- so the switch is only
       offered where the answer can be kept. -->
  {#if !local}
    <label class="check">
      <input
        type="checkbox"
        checked={draft.allDay}
        onchange={(e) => setAllDay(e.currentTarget.checked)}
      />
      All day
    </label>
  {/if}

  <div class="row">
    <label class="lbl" for="{uid}-repeat">Repeat</label>
    {#if editing?.customRecurrence}
      <p class="note">
        This repeats in a way that can only be changed where it was made. Everything else here can
        be.
      </p>
    {:else}
      <RepeatField
        id="{uid}-repeat"
        value={draft.recurrence ?? null}
        start={draft.allDay ? firstDay : draft.start}
        tz={draft.tz}
        onchange={(recurrence: Recurrence | null) => onpatch({ recurrence })}
      />
    {/if}
  </div>

  <div class="row">
    <label class="lbl" for="{uid}-where">Location</label>
    <input
      id="{uid}-where"
      class="pick"
      placeholder="A place, or a link to the call"
      value={draft.location}
      oninput={(e) => onpatch({ location: e.currentTarget.value })}
    />
  </div>

  <div class="row">
    <label class="lbl" for="{uid}-guests">Guests</label>
    {#if local}
      <p class="note">
        Guests need an account calendar to be invited from -- this computer cannot send them
        anything.
      </p>
    {:else}
      <GuestsField
        id="{uid}-guests"
        guests={draft.attendees}
        bind:typed={guestText}
        onchange={(attendees: Attendee[]) => onpatch({ attendees })}
      />
    {/if}
  </div>

  <div class="row">
    <label class="lbl" for="{uid}-about">Description</label>
    <textarea
      id="{uid}-about"
      class="notes"
      rows="4"
      placeholder={local ? 'Notes' : 'Anything the people coming should know'}
      value={draft.description}
      oninput={(e) => onpatch({ description: e.currentTarget.value })}
    ></textarea>
  </div>

  {#if error}<p class="error">{error}</p>{/if}

  <div class="actions">
    {#if askingScope}
      <!-- Asked here rather than in a dialog over the window: it is the
           second half of pressing Save, and the fields it is about should
           stay in view while it is answered. -->
      <p class="ask">This is one of a series. Save the change to…</p>
      <button class="btn btn-primary" onclick={() => commit('occurrence')}>Only this event</button>
      <button class="btn" onclick={() => commit('series')}>All events in the series</button>
      <button class="btn" onclick={() => (askingScope = false)}>Back</button>
    {:else}
      <button class="btn btn-primary" disabled={saving} onclick={() => save()}>{saveLabel}</button>
      <button class="btn" disabled={saving} onclick={() => oncancel()}>Cancel</button>
    {/if}
  </div>
</div>

<style>
  /* The rail's own panel and field styles, as `EventDetail` draws them --
     this is that rail's other face, and it should not look like a form
     dropped into it from somewhere else. The foot has room to scroll clear
     of the floating assistant button, as the rail's other panels do. */
  .panel {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    padding: var(--sp-4) var(--sp-4) var(--fab-clear);
  }
  /* Not on any calendar yet: the dashed edge a proposed block has too. */
  .panel.fresh {
    border-top: 3px dashed color-mix(in oklab, var(--c) 55%, var(--border));
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

  .row {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  .lbl {
    display: flex;
    align-items: baseline;
    gap: 6px;
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--fg-faint);
  }
  .nextday {
    font-weight: 500;
    color: var(--fg-subtle);
  }

  .times {
    display: flex;
    gap: var(--sp-2);
  }
  .timefield {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .timefield input,
  .pick {
    width: 100%;
    min-width: 0;
    height: 30px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    color: var(--fg);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    user-select: text;
  }
  .timefield input:focus,
  .pick:focus {
    outline: none;
    border-color: var(--accent);
  }
  .length {
    margin-top: calc(-1 * var(--sp-2));
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }

  /* A field-shaped button: the calendar, with its colour, opening a menu. */
  .choose {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    text-align: left;
  }
  .choose:hover {
    border-color: var(--border-strong);
  }
  .pdot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--c);
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

  .check {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .check input {
    accent-color: var(--accent);
  }

  .note {
    font-size: var(--text-xs);
    color: var(--fg-faint);
    line-height: var(--leading-normal);
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

  /* From the left, as the block panel's actions are: the floating
     assistant button sits over the rail's bottom-right corner, and a Save
     pushed against that edge is a Save half under it. */
  .actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-start;
    gap: var(--sp-2);
    margin-top: auto;
    padding-top: var(--sp-2);
  }
  .ask {
    flex-basis: 100%;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
</style>
