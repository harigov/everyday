<script lang="ts">
  // How something repeats: a menu of the few repeats people actually pick,
  // worded for the day it starts on, and -- behind "Custom…" -- the handful
  // of fields that cover the rest.
  //
  // Used twice: in the event editor, where a change is just part of the
  // draft and costs nothing until Save; and in a block's panel, where a
  // change writes two years of blocks at once. That second use is why a
  // custom rule can be held back until Apply (`confirm`): typing "3" on the
  // way to "30" in the interval field should not rewrite a series twice.
  // A preset is a single deliberate choice, so it always goes at once.
  //
  // The wording and the arithmetic are `lib/recurrence.ts`'s, where they
  // are tested; this file is only the controls.

  import { addMonths, localeWeekStart } from '../lib/time'
  import {
    WEEKDAYS,
    dayIn,
    describeRecurrence,
    matchPreset,
    ordinalWeek,
    presetsFor,
    weekOfMonth,
    weekdayOf,
    weekdayTitle,
    type RepeatDay,
  } from '../lib/recurrence'
  import type { Frequency, Recurrence } from '../lib/types'

  let {
    value,
    start,
    tz = null,
    disabled = false,
    confirm = false,
    id,
    onchange,
  }: {
    value: Recurrence | null
    /** When it starts: an instant or a bare day. The presets are about that day. */
    start: string
    /** The zone `start` is quoted in, so its day is that zone's day. */
    tz?: string | null
    disabled?: boolean
    /** Hold a custom rule until Apply, rather than sending every keystroke. */
    confirm?: boolean
    /** For a `<label for>` outside. */
    id?: string
    onchange: (rule: Recurrence | null) => unknown
  } = $props()

  const day = $derived(dayIn(start, tz))
  const weekday = $derived(weekdayOf(day))
  const where = $derived(weekOfMonth(day))
  const presets = $derived(presetsFor(start, tz))
  const matched = $derived(matchPreset(value, start, tz))

  /**
   * "Custom…" was chosen from the menu, so its fields stay open even while
   * what they spell is still one of the presets -- otherwise picking
   * Custom on a weekly repeat would snap straight back to "Every week on
   * Tuesday" and offer nothing to change.
   */
  let customOpen = $state(false)
  /** A custom rule not yet applied. Only ever set with `confirm`. */
  let pending = $state<Recurrence | null>(null)

  const showingCustom = $derived(customOpen || pending !== null || matched === 'custom')
  const working = $derived<Recurrence>(pending ?? value ?? fallback())

  /** What Custom starts from when nothing repeats yet: every week, on this day. */
  function fallback(): Recurrence {
    return { frequency: 'weekly', interval: 1, weekdays: [weekday] }
  }

  /** The week as the reader's own calendar draws it: Sunday first, or Monday. */
  const week = $derived.by(() => {
    const first = localeWeekStart() === 0 ? 6 : 0
    return [...WEEKDAYS.slice(first), ...WEEKDAYS.slice(0, first)]
  })

  const UNITS: { value: Frequency; one: string; many: string }[] = [
    { value: 'daily', one: 'day', many: 'days' },
    { value: 'weekly', one: 'week', many: 'weeks' },
    { value: 'monthly', one: 'month', many: 'months' },
    { value: 'yearly', one: 'year', many: 'years' },
  ]

  /**
   * One spelling for whatever the fields add up to: the days only where the
   * frequency has any, one ending at most, the interval inside what a
   * calendar will take. The core refuses anything else, and refusing it at
   * Save would be a worse moment to hear about it than never sending it.
   */
  function tidy(rule: Recurrence): Recurrence {
    const out: Recurrence = {
      frequency: rule.frequency,
      interval: Math.min(999, Math.max(1, Math.round(rule.interval || 1))),
    }
    if (rule.frequency === 'weekly') {
      const days = WEEKDAYS.filter((d) => rule.weekdays?.includes(d))
      out.weekdays = days.length > 0 ? days : [weekday]
    }
    if (rule.frequency === 'monthly' && rule.weekOfMonth) {
      out.weekOfMonth = rule.weekOfMonth
      out.weekdays = [weekday]
    }
    if (rule.count) out.count = Math.min(1000, Math.max(1, Math.round(rule.count)))
    else if (rule.until) out.until = rule.until
    return out
  }

  function send(rule: Recurrence) {
    if (confirm) pending = rule
    else onchange(rule)
  }

  function edit(changes: Partial<Recurrence>) {
    send(tidy({ ...$state.snapshot(working), ...changes }))
  }

  function pick(key: string) {
    if (key === 'custom') {
      customOpen = true
      // Committed at once without `confirm`: a menu reading "Custom…" over
      // a draft that still does not repeat would be saved as not repeating.
      send(tidy($state.snapshot(working)))
      return
    }
    customOpen = false
    pending = null
    onchange(presets.find((p) => p.key === key)?.rule ?? null)
  }

  function toggleDay(d: RepeatDay) {
    const days = working.weekdays?.length ? working.weekdays : [weekday]
    const next = days.includes(d) ? days.filter((x) => x !== d) : [...days, d]
    // Never none: a weekly repeat on no day at all is not a repeat.
    if (next.length > 0) edit({ weekdays: next })
  }

  type Ending = 'never' | 'until' | 'count'
  const ending = $derived<Ending>(working.count ? 'count' : working.until ? 'until' : 'never')

  function setEnding(next: Ending) {
    if (next === 'never') edit({ count: null, until: null })
    if (next === 'until') edit({ count: null, until: working.until ?? addMonths(day, 3) })
    if (next === 'count') edit({ until: null, count: working.count ?? 10 })
  }

  function apply() {
    if (pending) onchange(pending)
    pending = null
  }

  function revert() {
    pending = null
    customOpen = false
  }
</script>

<div class="repeat">
  <select
    {id}
    class="pick"
    {disabled}
    value={showingCustom ? 'custom' : matched}
    onchange={(e) => pick(e.currentTarget.value)}
  >
    {#each presets as preset (preset.key)}
      <option value={preset.key}>{preset.label}</option>
    {/each}
    <option value="custom">Custom…</option>
  </select>

  {#if showingCustom}
    <div class="custom">
      <div class="line">
        <span class="word">Every</span>
        <input
          class="pick num"
          type="number"
          min="1"
          max="999"
          aria-label="How many"
          {disabled}
          value={working.interval}
          onchange={(e) => edit({ interval: Number(e.currentTarget.value) || 1 })}
        />
        <select
          class="pick"
          aria-label="Unit"
          {disabled}
          value={working.frequency}
          onchange={(e) =>
            edit({ frequency: e.currentTarget.value as Frequency, weekOfMonth: null })}
        >
          {#each UNITS as unit (unit.value)}
            <option value={unit.value}>{working.interval === 1 ? unit.one : unit.many}</option>
          {/each}
        </select>
      </div>

      {#if working.frequency === 'weekly'}
        <div class="days" role="group" aria-label="On these days">
          {#each week as d (d)}
            {@const on = (working.weekdays?.length ? working.weekdays : [weekday]).includes(d)}
            <button
              type="button"
              class="day"
              class:on
              aria-pressed={on}
              aria-label={weekdayTitle(d)}
              title={weekdayTitle(d)}
              {disabled}
              onclick={() => toggleDay(d)}>{weekdayTitle(d).charAt(0)}</button
            >
          {/each}
        </div>
      {:else if working.frequency === 'monthly'}
        <select
          class="pick"
          aria-label="On which day of the month"
          {disabled}
          value={String(working.weekOfMonth ?? 0)}
          onchange={(e) => edit({ weekOfMonth: Number(e.currentTarget.value) || null })}
        >
          <option value="0">On day {Number(day.slice(8))}</option>
          {#if where.nth <= 4}
            <option value={String(where.nth)}>
              On the {ordinalWeek(where.nth)}
              {weekdayTitle(weekday)}
            </option>
          {/if}
          {#if where.last}
            <option value="-1">On the last {weekdayTitle(weekday)}</option>
          {/if}
        </select>
      {/if}

      <div class="line">
        <span class="word">Ends</span>
        <select
          class="pick"
          aria-label="Ends"
          {disabled}
          value={ending}
          onchange={(e) => setEnding(e.currentTarget.value as Ending)}
        >
          <option value="never">Never</option>
          <option value="until">On a date</option>
          <option value="count">After a number of times</option>
        </select>
      </div>
      {#if ending === 'until'}
        <input
          class="pick"
          type="date"
          aria-label="Last day"
          min={day}
          {disabled}
          value={working.until ?? ''}
          onchange={(e) => e.currentTarget.value && edit({ until: e.currentTarget.value })}
        />
      {:else if ending === 'count'}
        <div class="line">
          <input
            class="pick num"
            type="number"
            min="1"
            max="1000"
            aria-label="How many times"
            {disabled}
            value={working.count ?? 10}
            onchange={(e) => edit({ count: Number(e.currentTarget.value) || 1 })}
          />
          <span class="word">{working.count === 1 ? 'time' : 'times'}</span>
        </div>
      {/if}

      <p class="said">{describeRecurrence(working, start, tz)}</p>

      {#if pending}
        <div class="line">
          <button type="button" class="btn btn-outline small" onclick={apply}>Apply</button>
          <button type="button" class="btn small" onclick={revert}>Cancel</button>
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .repeat {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    min-width: 0;
  }

  /* The same field the rail's clock inputs are: one height, one border. */
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
    user-select: text;
  }
  .pick:focus {
    outline: none;
    border-color: var(--accent);
  }
  .pick:disabled {
    opacity: 0.45;
  }
  .pick.num {
    flex: 0 0 64px;
    font-variant-numeric: tabular-nums;
  }

  /* Set in from the menu above, on a sunken ground, so it reads as the
     inside of "Custom…" rather than as more fields of the panel. */
  .custom {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: var(--sp-2);
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
  }
  .line {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }
  .word {
    flex: none;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }

  .days {
    display: flex;
    gap: 4px;
  }
  .day {
    flex: 1;
    height: 28px;
    min-width: 0;
    border-radius: 999px;
    border: 1px solid var(--border);
    background: var(--bg-raised);
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--fg-subtle);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .day:hover {
    border-color: var(--border-strong);
    color: var(--fg);
  }
  .day.on {
    border-color: var(--accent);
    background: var(--accent);
    color: var(--fg-on-accent);
  }

  .said {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    line-height: var(--leading-normal);
  }

  .small {
    height: 28px;
    padding: 0 var(--sp-3);
    font-size: var(--text-sm);
  }
</style>
