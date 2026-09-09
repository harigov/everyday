<script lang="ts">
  // What the day recorded, under the day's heading.
  //
  // One chip per tracker, in a row you can read without reading: colour and
  // silhouette say which tracker, and a chip that has been recorded is
  // filled while one that has not is an outline. That is the whole state
  // model -- there is deliberately no third appearance for "partly done",
  // because a habit either happened or has not happened *yet*, and a day in
  // progress should not look like a day failed.
  //
  // A check is one click. Everything else opens a small panel, because a
  // dose, a severity and a quantity all need a number, and guessing one on
  // the user's behalf is how a tracker fills up with readings nobody meant.

  import { app } from '../lib/state.svelte'
  import { dayValue, formatDay, formatValue, shownTrackers } from '../lib/tracker'
  import { tracking } from '../lib/tracking.svelte'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import type { Reading, Tracker } from '../lib/types'
  import { dismissable } from '../lib/dismiss'
  import { focusOnMount } from '../lib/focus'
  import { todayIso } from '../lib/time'
  import Icon from './Icon.svelte'
  import TrackerIcon from './TrackerIcon.svelte'

  let { journalId, date }: { journalId: string; date: string } = $props()

  const trackers = $derived(
    shownTrackers(app.journals.find((j) => j.id === journalId) ?? null, tracking.trackers),
  )

  /** The tracker whose panel is open, and which side it opens towards. */
  let open = $state<string | null>(null)
  let flip = $state(false)
  /** The number in the panel's field, as typed. */
  let draft = $state('')
  let strip = $state<HTMLDivElement>()

  const locale = navigator.language || 'en'
  const timeFmt = new Intl.DateTimeFormat(locale, { hour: 'numeric', minute: '2-digit' })

  $effect(() => {
    void tracking.open(journalId, date)
  })

  function readingsOf(tracker: Tracker): Reading[] {
    return tracking.of(tracker.id)
  }

  function shortTime(at: string | null | undefined): string {
    return at ? timeFmt.format(new Date(at)) : ''
  }

  /** `HH:MM` for a time input, from an instant or from the clock. */
  function clockValue(at: string | null | undefined): string {
    const d = at ? new Date(at) : new Date()
    const p = (n: number) => String(n).padStart(2, '0')
    return `${p(d.getHours())}:${p(d.getMinutes())}`
  }

  /** An instant on *this* page's day at `HH:MM`, for the time field. */
  function instantAt(hhmm: string): string | null {
    if (!/^\d{2}:\d{2}$/.test(hhmm)) return null
    const d = new Date(`${date}T${hhmm}:00`)
    return Number.isNaN(d.getTime()) ? null : d.toISOString()
  }

  function toggle(tracker: Tracker, event: MouseEvent) {
    if (tracker.kind === 'check') {
      if (tracking.has(tracker.id)) void tracking.clear(tracker.id)
      else void tracking.log(tracker, 1)
      return
    }
    if (open === tracker.id) {
      open = null
      return
    }
    openPanel(tracker, (event.currentTarget as HTMLElement).getBoundingClientRect())
  }

  /** Open the recording panel beside a chip. */
  function openPanel(tracker: Tracker, chip: DOMRect) {
    // Open towards whichever side has room. A panel that runs off the right
    // of a narrow editor is a panel with its buttons outside the window.
    const bounds = strip?.getBoundingClientRect()
    flip = !!bounds && chip.left - bounds.left > bounds.width - 280
    draft = String(tracker.defaultValue)
    open = tracker.id
  }

  /**
   * What a right-click on a chip offers.
   *
   * The chip itself is one gesture and can only mean one thing -- record, or
   * open the panel that records -- so this is where the other two live: undo
   * the day, and the switch that decides whether these readings are drawn on
   * the calendar as well as counted here.
   *
   * The chip's rectangle is taken now rather than when an item is chosen,
   * because by then the menu has closed and there is no event to read it
   * from.
   */
  function chipMenu(tracker: Tracker, chip: DOMRect): MenuItem[] {
    const readings = readingsOf(tracker)
    const on = readings.length > 0
    return tidyMenu([
      tracker.kind === 'check'
        ? {
            label: on ? 'Clear it' : 'Record it',
            icon: on ? 'close' : 'tick',
            run: () => (on ? tracking.clear(tracker.id) : tracking.log(tracker, 1)),
          }
        : { label: 'Record…', icon: 'plus', run: () => openPanel(tracker, chip) },
      on &&
        tracker.kind !== 'check' && {
          label: readings.length === 1 ? 'Clear it' : `Clear all ${readings.length}`,
          icon: 'close',
          hint: formatDay(tracker, readings),
          run: () => tracking.clear(tracker.id),
        },
      SEP,
      {
        label: 'Show on the calendar',
        checked: tracker.onCalendar,
        run: () => tracking.setOnCalendar(tracker.id, !tracker.onCalendar),
      },
    ])
  }

  function commit(tracker: Tracker, hhmm: string | null) {
    const value = Number(draft)
    if (!Number.isFinite(value)) return
    // Zero is a real answer on a severity scale -- "no headache today" is
    // worth recording, and is what makes a run of good days visible rather
    // than indistinguishable from a run of days you forgot to answer. It is
    // not a real answer anywhere else: nought minutes is not an amount.
    if (tracker.kind === 'scale' ? value < 0 : value <= 0) return
    void tracking.log(tracker, value, hhmm ? instantAt(hhmm) : null)
    open = null
  }

  /** Fraction of the day's goal met, or null when there is no goal. */
  function progress(tracker: Tracker): number | null {
    if (!tracker.target || tracker.kind === 'scale') return null
    const done = dayValue(tracker, readingsOf(tracker))
    return Math.max(0, Math.min(1, done / tracker.target))
  }

  function step(tracker: Tracker, by: number) {
    const next = Number(draft) + by * (tracker.defaultValue || 1)
    draft = String(Math.max(0, Math.round(next * 100) / 100))
  }
</script>

{#if tracking.enabled && trackers.length > 0}
  <div class="strip" bind:this={strip}>
    {#each trackers as tracker (tracker.id)}
      {@const readings = readingsOf(tracker)}
      {@const on = readings.length > 0}
      {@const fill = progress(tracker)}
      <div class="slot">
        <button
          class="chip"
          class:on
          class:busy={tracking.busy === tracker.id}
          style="--c: {tracker.color}; --fill: {fill === null ? 0 : fill * 100}%"
          aria-pressed={on}
          aria-haspopup={tracker.kind === 'check' ? undefined : 'dialog'}
          aria-expanded={tracker.kind === 'check' ? undefined : open === tracker.id}
          title={on ? formatDay(tracker, readings) : `Record ${tracker.name}`}
          onclick={(e) => toggle(tracker, e)}
          oncontextmenu={(e) =>
            menu.show(e, chipMenu(tracker, e.currentTarget.getBoundingClientRect()))}
        >
          {#if fill !== null && on}<span class="meter" aria-hidden="true"></span>{/if}
          <TrackerIcon name={tracker.icon} color={tracker.color} size={22} solid={on} />
          <span class="name">{tracker.name}</span>
          {#if on}
            <span class="value">{formatDay(tracker, readings)}</span>
            {#if readings.length === 1 && readings[0]!.at}
              <span class="at">{shortTime(readings[0]!.at)}</span>
            {/if}
          {/if}
        </button>

        {#if open === tracker.id}
          <!-- Today in the *local* calendar, not in UTC. `toISOString()`
               here would put someone at UTC+5:30 on tomorrow's date from
               18:30, so a reading made on yesterday's page would be stamped
               with the current clock -- the pin at an hour nothing happened
               that this whole design exists to avoid. -->
          {@const timed = date === todayIso()}
          <!-- Dismissed by a listener rather than by a sheet of glass over
               the window: the glass caught the click that dismissed it, so
               with a chip open the next press on anything -- the app bar
               most of all -- did nothing. See `lib/dismiss.ts`. -->
          <div
            class="panel"
            class:flip
            role="dialog"
            aria-label={tracker.name}
            style="--c: {tracker.color}"
            use:dismissable={{ onaway: () => (open = null), within: '.chip' }}
          >
            {#if tracker.kind === 'scale'}
              <p class="lead">How bad, out of {tracker.scaleMax}?</p>
              <div class="scale">
                {#each Array.from({ length: tracker.scaleMax + 1 }, (_, i) => i) as n (n)}
                  <button
                    class="notch"
                    style="--w: {n / tracker.scaleMax}"
                    onclick={() => {
                      draft = String(n)
                      commit(tracker, timed ? clockValue(null) : null)
                    }}>{n}</button
                  >
                {/each}
              </div>
            {:else}
              <p class="lead">
                {tracker.kind === 'dose' ? 'How much did you take?' : 'How much?'}
              </p>
              <div class="amount">
                <button class="step" aria-label="Less" onclick={() => step(tracker, -1)}>
                  <Icon name="minus" size={15} />
                </button>
                <label class="field">
                  <input
                    type="number"
                    inputmode="decimal"
                    min="0"
                    step="any"
                    bind:value={draft}
                    use:focusOnMount
                    onkeydown={(e) => {
                      if (e.key === 'Enter') commit(tracker, timed ? clockValue(null) : null)
                      if (e.key === 'Escape') open = null
                    }}
                  />
                  {#if tracker.unit}<span class="unit">{tracker.unit}</span>{/if}
                </label>
                <button class="step" aria-label="More" onclick={() => step(tracker, 1)}>
                  <Icon name="plus" size={15} />
                </button>
                <button class="add" onclick={() => commit(tracker, timed ? clockValue(null) : null)}
                  >Add</button
                >
              </div>
            {/if}

            {#if !timed}
              <p class="hint">
                Filed under {date} with no time, because this page is not today's.
              </p>
            {/if}

            {#if readings.length}
              <ul class="log">
                {#each readings as r (r.id)}
                  <li>
                    {#if r.at}
                      <input
                        class="time"
                        type="time"
                        value={clockValue(r.at)}
                        aria-label="Time recorded"
                        onchange={(e) => {
                          const at = instantAt(e.currentTarget.value)
                          if (at) void tracking.save({ ...r, at })
                        }}
                      />
                    {:else}
                      <button
                        class="time untimed"
                        title="Give this a time"
                        onclick={() =>
                          void tracking.save({ ...r, at: instantAt(clockValue(null)) })}
                        >no time</button
                      >
                    {/if}
                    <span class="amt">{formatValue(tracker, r.value)}</span>
                    <button
                      class="drop"
                      aria-label="Remove this reading"
                      onclick={() => void tracking.remove(r)}
                    >
                      <Icon name="close" size={12} weight={1.8} />
                    </button>
                  </li>
                {/each}
              </ul>
            {/if}
          </div>
        {/if}
      </div>
    {/each}
  </div>
{/if}

<style>
  .strip {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    margin-top: var(--sp-4);
  }

  .slot {
    position: relative;
  }

  /* ── The chip ──────────────────────────────────────────────────────── */

  .chip {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    height: 34px;
    padding: 0 var(--sp-3) 0 5px;
    border-radius: 99px;
    border: 1px dashed var(--border-strong);
    background: transparent;
    color: var(--fg-muted);
    font-size: var(--text-base);
    font-weight: 500;
    overflow: hidden;
    transition:
      background var(--fast) var(--ease),
      border-color var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }

  .chip:hover {
    border-color: var(--c);
    color: var(--fg);
  }

  .chip.on {
    border-style: solid;
    border-color: color-mix(in oklab, var(--c) 34%, transparent);
    background: color-mix(in oklab, var(--c) 10%, transparent);
    color: var(--fg);
  }

  .chip.busy {
    opacity: 0.6;
  }

  /* Progress towards a daily goal, as the chip filling up behind its own
     label. A separate bar would be a second thing to look at; this is the
     same thing, further along. */
  .meter {
    position: absolute;
    inset: 0 auto 0 0;
    width: var(--fill);
    background: color-mix(in oklab, var(--c) 14%, transparent);
    transition: width var(--med) var(--ease);
  }

  .name,
  .value,
  .at {
    position: relative;
  }

  .value {
    font-variant-numeric: tabular-nums;
    color: color-mix(in oklab, var(--c) 76%, var(--fg));
    font-weight: 560;
  }

  .at {
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: var(--fg-subtle);
  }

  /* ── The panel ─────────────────────────────────────────────────────── */

  .panel {
    position: absolute;
    z-index: 21;
    top: calc(100% + 6px);
    left: 0;
    width: 264px;
    padding: var(--sp-3);
    border-radius: var(--radius-lg);
    border: 1px solid var(--border);
    background: var(--bg-raised);
    box-shadow: var(--shadow-lg);
  }

  .panel.flip {
    left: auto;
    right: 0;
  }

  .lead {
    font-size: var(--text-sm);
    color: var(--fg-muted);
    margin-bottom: var(--sp-2);
  }

  .hint {
    margin-top: var(--sp-2);
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }

  /* A severity picker, weighted: the notches darken as they climb, so "how
     bad" is answered by aiming at a shade rather than by reading a number. */
  .scale {
    display: grid;
    grid-template-columns: repeat(6, 1fr);
    gap: 4px;
  }

  .notch {
    height: 30px;
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    color: var(--fg-muted);
    background: color-mix(in oklab, var(--c) calc(var(--w) * 26%), var(--bg-hover));
  }

  .notch:hover {
    outline: 2px solid var(--c);
    outline-offset: -2px;
    color: var(--fg);
  }

  .amount {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
  }

  .step {
    display: grid;
    place-items: center;
    width: 28px;
    height: 32px;
    flex: none;
    border-radius: var(--radius-sm);
    color: var(--fg-muted);
    background: var(--bg-hover);
  }
  .step:hover {
    color: var(--fg);
    background: var(--bg-active);
  }

  .field {
    display: flex;
    align-items: baseline;
    gap: 3px;
    flex: 1;
    min-width: 0;
    height: 32px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    border: 1px solid var(--border-strong);
    background: var(--bg);
  }
  .field:focus-within {
    border-color: var(--c);
  }

  .field input {
    width: 100%;
    min-width: 0;
    border: none;
    background: none;
    font: inherit;
    font-variant-numeric: tabular-nums;
    font-weight: 560;
    color: var(--fg);
    user-select: text;
  }
  .field input:focus {
    outline: none;
  }
  /* The spinner duplicates the two buttons either side of the field. */
  .field input::-webkit-inner-spin-button {
    appearance: none;
  }

  .unit {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }

  .add {
    height: 32px;
    padding: 0 var(--sp-3);
    border-radius: var(--radius-sm);
    background: var(--c);
    color: #fff;
    font-size: var(--text-sm);
    font-weight: 560;
  }
  .add:hover {
    filter: brightness(1.08);
  }

  /* ── What has already been recorded ────────────────────────────────── */

  .log {
    list-style: none;
    margin-top: var(--sp-3);
    border-top: 1px solid var(--border);
    padding-top: var(--sp-2);
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .log li {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-sm);
  }

  .time {
    width: 74px;
    padding: 2px 4px;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: none;
    font: inherit;
    font-variant-numeric: tabular-nums;
    color: var(--fg-subtle);
    text-align: left;
  }
  .time:hover {
    border-color: var(--border-strong);
  }
  .time.untimed {
    font-style: italic;
    color: var(--fg-faint);
  }

  .amt {
    flex: 1;
    font-variant-numeric: tabular-nums;
    color: var(--fg);
  }

  .drop {
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    border-radius: 99px;
    color: var(--fg-faint);
  }
  .drop:hover {
    color: var(--danger);
    background: var(--bg-hover);
  }
</style>
