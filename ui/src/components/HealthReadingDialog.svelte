<script lang="ts">
  // Add a reading by hand: a weight off the scale, a pressure off the cuff, a
  // glucose off a finger-stick -- the measurements a watch does not take.
  //
  // The value is typed in the person's own unit (the one `displayUnit` says),
  // and sent with that unit's name so the vault converts it once on the way
  // in. A blood pressure is two numbers, so it asks for both. The time
  // defaults to now and cannot be in the future: a reading is something that
  // already happened.
  //
  // Whoever opens it sets `health.readingOpen`, and this closes it the same
  // way. It starts on the measurement Vitals has open, if it is one that can
  // be added.

  import { health } from '../lib/health.svelte'
  import { focusOnMount } from '../lib/focus'
  import { toLocalInputValue } from '../lib/format'
  import { phone } from '../lib/phone.svelte'
  import Sheet from './Sheet.svelte'
  import type { Metric } from '../lib/types'

  interface Choice {
    id: string
    metric: Metric
    name: string
  }

  const CHOICES: Choice[] = [
    { id: 'weight', metric: 'weight', name: 'Weight' },
    { id: 'pressure', metric: 'blood_pressure_systolic', name: 'Blood pressure' },
    { id: 'glucose', metric: 'blood_glucose', name: 'Blood glucose' },
    { id: 'temperature', metric: 'body_temperature', name: 'Body temperature' },
    { id: 'pulse', metric: 'heart_rate', name: 'Heart rate' },
    { id: 'oxygen', metric: 'oxygen_saturation', name: 'Blood oxygen (SpO2)' },
    { id: 'fat', metric: 'body_fat', name: 'Body fat' },
    { id: 'waist', metric: 'waist_circumference', name: 'Waist' },
  ]

  function start(): string {
    const m = health.vitalsMetric
    if (m === 'blood_pressure_systolic' || m === 'blood_pressure_diastolic') return 'pressure'
    return CHOICES.find((c) => c.metric === m)?.id ?? 'weight'
  }

  let choiceId = $state(start())
  let value = $state<number | null>(null)
  let second = $state<number | null>(null)
  let at = $state(toLocalInputValue(new Date()))
  let busy = $state(false)
  let problem = $state<string | null>(null)

  const choice = $derived(CHOICES.find((c) => c.id === choiceId) ?? CHOICES[0]!)
  const pressure = $derived(choice.id === 'pressure')
  const unit = $derived(health.displayUnit(choice.metric))
  /** The latest a reading can be dated: now, as the field writes a time. */
  const maxAt = $derived(toLocalInputValue(new Date()))

  const positive = (n: number | null): n is number =>
    typeof n === 'number' && Number.isFinite(n) && n > 0
  const when = $derived(new Date(at))
  const timeOk = $derived(!Number.isNaN(when.getTime()) && when.getTime() <= Date.now() + 60_000)

  const ready = $derived(positive(value) && (!pressure || positive(second)) && timeOk && !busy)

  function close() {
    if (!busy) health.readingOpen = false
  }

  async function save() {
    if (!positive(value) || (pressure && !positive(second)) || !timeOk || busy) return
    if (pressure && second !== null && value < second) {
      problem = 'The top number is the larger one. Check the two are not the wrong way round.'
      return
    }
    busy = true
    problem = null
    health.error = null
    try {
      const ok = await health.addReading({
        metric: choice.metric,
        at: when.toISOString(),
        value,
        unit,
        second: pressure ? second : null,
        tz: Intl.DateTimeFormat().resolvedOptions().timeZone,
      })
      if (ok) health.readingOpen = false
      else problem = health.error ?? 'The reading could not be saved.'
    } finally {
      busy = false
    }
  }
</script>

<!-- Back, on a phone, closes the sheet as Cancel does. -->
<Sheet label="Add a reading" onclose={close} backWhen={() => phone.on}>
  <form
    onsubmit={(e) => {
      e.preventDefault()
      void save()
    }}
  >
    <h2>Add a reading</h2>

    <div class="row">
      <label class="field-label" for="hr-what">What was measured</label>
      <select id="hr-what" class="field" bind:value={choiceId} disabled={busy}>
        {#each CHOICES as c (c.id)}
          <option value={c.id}>{c.name}</option>
        {/each}
      </select>
    </div>

    {#if pressure}
      <div class="pair">
        <div class="row">
          <label class="field-label" for="hr-top">Top number (systolic)</label>
          <div class="with-unit">
            <input
              id="hr-top"
              class="field"
              type="number"
              inputmode="decimal"
              step="any"
              min="0"
              bind:value
              disabled={busy}
              use:focusOnMount
            />
            <span class="unit">{unit}</span>
          </div>
        </div>
        <div class="row">
          <label class="field-label" for="hr-bottom">Bottom number (diastolic)</label>
          <div class="with-unit">
            <input
              id="hr-bottom"
              class="field"
              type="number"
              inputmode="decimal"
              step="any"
              min="0"
              bind:value={second}
              disabled={busy}
            />
            <span class="unit">{unit}</span>
          </div>
        </div>
      </div>
    {:else}
      <div class="row">
        <label class="field-label" for="hr-value">Value</label>
        <div class="with-unit">
          <input
            id="hr-value"
            class="field"
            type="number"
            inputmode="decimal"
            step="any"
            min="0"
            bind:value
            disabled={busy}
            use:focusOnMount
          />
          {#if unit}<span class="unit">{unit}</span>{/if}
        </div>
      </div>
    {/if}

    <div class="row">
      <label class="field-label" for="hr-at">When</label>
      <input
        id="hr-at"
        class="field"
        type="datetime-local"
        max={maxAt}
        bind:value={at}
        disabled={busy}
      />
    </div>

    {#if !timeOk}
      <p class="error">A reading cannot be from the future.</p>
    {:else if problem}
      <p class="error">{problem}</p>
    {/if}

    <div class="sheet-row">
      <span class="spacer"></span>
      <button type="button" class="btn" disabled={busy} onclick={close}>Cancel</button>
      <button type="submit" class="btn btn-primary" disabled={!ready}>
        {busy ? 'Saving…' : 'Add'}
      </button>
    </div>
  </form>
</Sheet>

<style>
  h2 {
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
    margin-bottom: var(--sp-3);
  }
  .row {
    margin-bottom: var(--sp-3);
    min-width: 0;
  }
  .pair {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--sp-3);
  }
  .with-unit {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .unit {
    flex: none;
    color: var(--fg-subtle);
    font-size: var(--text-sm);
  }
  .error {
    margin: 0 0 var(--sp-3);
  }

  /* A phone: the two blood-pressure numbers one above the other, each with
     room for its label, and the buttons wrapping rather than running off. */
  :global([data-phone]) .pair {
    grid-template-columns: minmax(0, 1fr);
    gap: 0;
  }
  :global([data-phone]) .sheet-row {
    flex-wrap: wrap;
    justify-content: flex-end;
  }
</style>
