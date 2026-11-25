<script lang="ts">
  // Settings → Health: where the data comes from, how it is shown, what it
  // is held against, and the one key the app needs.
  //
  //   Sources, Import  how data gets here: the iPhone, an Apple export.
  //   Units            how a number is shown. Storage is always in one unit
  //                    (kg, metres, kcal, mg/dL); this only changes what
  //                    is drawn and what a typed number is taken to mean.
  //   Goals, targets   the person's own, for the meters on Today and
  //                    Activity and for Nutrition. Nobody else's idea of
  //                    what they should reach.
  //   Glucose notices  where a notice appears after the fact. Not an alarm:
  //                    a sensor's own alarms stay the safety alarm.
  //   Notices          dose reminders, and how long a quiet source may be.
  //   Food lookup      a key for the USDA's FoodData Central, held here as
  //                    Money holds Plaid's: typed into this component's own
  //                    field, sent once, the field emptied whatever the
  //                    answer, and only the last characters ever shown.
  //
  // Every control saves as it is changed, as Screen time's do. A number is
  // read when the field is left, kept inside what makes sense, and shown
  // back as it was understood.

  import { health } from '../lib/health.svelte'
  import { digitsFor, fromDisplay, toDisplay, unitFor } from '../lib/health'
  import { openExternal } from '../lib/open-external'
  import type {
    ActivityGoals,
    HealthSettings,
    HealthUnits,
    Metric,
    NutritionTargets,
  } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import HealthImport from './HealthImport.svelte'
  import HealthSources from './HealthSources.svelte'
  import Icon from './Icon.svelte'

  // Opened straight from Settings, before the app ever was, nothing here is
  // loaded: ask for it.
  if (!health.loaded) void health.start()

  /** Where api.data.gov hands out keys. A constant: nothing here opens an
   *  address that came from data. */
  const KEY_SIGNUP = 'https://api.data.gov/signup/'

  const settings = $derived(health.settings)
  const keys = $derived(health.keys)

  function save(patch: Partial<HealthSettings>) {
    const current = health.settings
    if (!current) return
    void health.saveSettings({ ...$state.snapshot(current), ...patch })
  }

  /** A typed number, or `null` for an empty or unreadable field. */
  function typed(raw: string): number | null {
    const s = raw.trim().replace(',', '.')
    if (s === '') return null
    const n = Number(s)
    return Number.isFinite(n) ? n : null
  }

  function clamp(n: number, min: number, max: number): number {
    return Math.min(max, Math.max(min, n))
  }

  /** A value for an input: converted for the shown unit, to one decimal at
   *  most, with no thousands separators (a field is not a label). */
  function shown(value: number | null | undefined, metric?: Metric): string {
    if (value === null || value === undefined || !settings) return ''
    const v = metric ? toDisplay(metric, value, settings.units) : value
    return String(Math.round(v * 10) / 10)
  }

  // ── units ─────────────────────────────────────────────────────────────

  const UNITS: { key: keyof HealthUnits; label: string; options: [string, string][] }[] = [
    {
      key: 'glucose',
      label: 'Blood glucose',
      options: [
        ['mg/dL', 'mg/dL'],
        ['mmol/L', 'mmol/L'],
      ],
    },
    {
      key: 'weight',
      label: 'Weight',
      options: [
        ['kg', 'Kilograms (kg)'],
        ['lb', 'Pounds (lb)'],
      ],
    },
    {
      key: 'distance',
      label: 'Distance',
      options: [
        ['km', 'Kilometres (km)'],
        ['mi', 'Miles (mi)'],
      ],
    },
    {
      key: 'energy',
      label: 'Energy',
      options: [
        ['kcal', 'Calories (kcal)'],
        ['kJ', 'Kilojoules (kJ)'],
      ],
    },
    {
      key: 'temperature',
      label: 'Temperature',
      options: [
        ['C', 'Celsius (°C)'],
        ['F', 'Fahrenheit (°F)'],
      ],
    },
    {
      key: 'length',
      label: 'Height and length',
      options: [
        ['cm', 'Centimetres (cm)'],
        ['in', 'Inches (in)'],
      ],
    },
  ]

  function setUnit(key: keyof HealthUnits, value: string) {
    const current = health.settings
    if (!current) return
    save({ units: { ...current.units, [key]: value } })
  }

  // ── activity goals ────────────────────────────────────────────────────

  interface GoalField {
    key: keyof ActivityGoals
    label: string
    /** A unit that does not change; empty when `metric` names it. */
    unit: string
    /** The metric whose unit and conversion it follows. */
    metric?: Metric
    min: number
    max: number
  }

  const GOALS: GoalField[] = [
    { key: 'moveKcal', label: 'Move', unit: '', metric: 'active_energy', min: 50, max: 10000 },
    { key: 'exerciseMin', label: 'Exercise', unit: 'min', min: 1, max: 600 },
    { key: 'standHours', label: 'Stand', unit: 'hours', min: 1, max: 24 },
    { key: 'steps', label: 'Steps', unit: 'steps', min: 500, max: 100000 },
    { key: 'sleepHours', label: 'Sleep', unit: 'hours', min: 3, max: 14 },
  ]

  function unitOf(field: { unit: string; metric?: Metric }): string {
    if (field.metric && settings) return unitFor(field.metric, settings.units, health.catalog)
    return field.unit
  }

  function saveGoal(g: GoalField, input: HTMLInputElement) {
    const current = health.settings
    if (!current) return
    const old = current.goals[g.key]
    const n = typed(input.value)
    if (n === null) {
      input.value = shown(old, g.metric)
      return
    }
    const canonical = clamp(g.metric ? fromDisplay(g.metric, n, current.units) : n, g.min, g.max)
    const next = g.key === 'sleepHours' ? Math.round(canonical * 2) / 2 : Math.round(canonical)
    input.value = shown(next, g.metric)
    if (next !== old) save({ goals: { ...current.goals, [g.key]: next } })
  }

  // ── nutrition targets ─────────────────────────────────────────────────

  interface TargetField {
    key: keyof NutritionTargets
    label: string
    unit: string
    metric?: Metric
    min: number
    max: number
  }

  const TARGETS: TargetField[] = [
    { key: 'kcal', label: 'Energy', unit: '', metric: 'dietary_energy', min: 500, max: 10000 },
    { key: 'proteinG', label: 'Protein', unit: 'g', min: 5, max: 500 },
    { key: 'carbsG', label: 'Carbohydrate', unit: 'g', min: 5, max: 1000 },
    { key: 'fatG', label: 'Fat', unit: 'g', min: 5, max: 500 },
    { key: 'fiberG', label: 'Fibre', unit: 'g', min: 1, max: 200 },
    { key: 'sodiumMg', label: 'Sodium', unit: 'mg', min: 100, max: 20000 },
    { key: 'carbsPerMealG', label: 'Carbohydrate per meal', unit: 'g', min: 5, max: 500 },
  ]

  function saveTarget(t: TargetField, input: HTMLInputElement) {
    const current = health.settings
    if (!current) return
    const old = current.nutrition[t.key] ?? null
    const n = typed(input.value)
    // Empty means no target: a day is not measured against anything.
    if (input.value.trim() === '') {
      input.value = ''
      if (old !== null) save({ nutrition: { ...current.nutrition, [t.key]: null } })
      return
    }
    if (n === null) {
      input.value = shown(old, t.metric)
      return
    }
    const canonical = clamp(t.metric ? fromDisplay(t.metric, n, current.units) : n, t.min, t.max)
    const next = Math.round(canonical)
    input.value = shown(next, t.metric)
    if (next !== old) save({ nutrition: { ...current.nutrition, [t.key]: next } })
  }

  // ── glucose notices ───────────────────────────────────────────────────

  let glucoseProblem = $state<string | null>(null)

  /** A glucose threshold, shown in the chosen unit with the decimals it
   *  deserves: 3.9 mmol/L, 70 mg/dL. */
  function glucoseShown(value: number | null | undefined): string {
    if (value === null || value === undefined || !settings) return ''
    const digits = digitsFor('blood_glucose', settings.units)
    const v = toDisplay('blood_glucose', value, settings.units)
    return String(Number(v.toFixed(digits)))
  }

  function setGlucose(which: 'glucoseLow' | 'glucoseHigh', value: number | null) {
    save(which === 'glucoseLow' ? { glucoseLow: value } : { glucoseHigh: value })
  }

  function saveGlucose(which: 'glucoseLow' | 'glucoseHigh', input: HTMLInputElement) {
    const current = health.settings
    if (!current) return
    glucoseProblem = null
    const old = current[which] ?? null
    if (input.value.trim() === '') {
      input.value = ''
      if (old !== null) setGlucose(which, null)
      return
    }
    const n = typed(input.value)
    if (n === null) {
      input.value = glucoseShown(old)
      return
    }
    const next = Math.round(clamp(fromDisplay('blood_glucose', n, current.units), 40, 400))
    const other = which === 'glucoseLow' ? current.glucoseHigh : current.glucoseLow
    const lowest = which === 'glucoseLow' ? next : other
    const highest = which === 'glucoseLow' ? other : next
    if (lowest !== null && lowest !== undefined && highest !== null && highest !== undefined) {
      if (lowest >= highest) {
        glucoseProblem = 'The low threshold has to be below the high one.'
        input.value = glucoseShown(old)
        return
      }
    }
    input.value = glucoseShown(next)
    if (next !== old) setGlucose(which, next)
  }

  // ── notices ───────────────────────────────────────────────────────────

  function saveStale(input: HTMLInputElement) {
    const current = health.settings
    if (!current) return
    const n = typed(input.value)
    const hours = n === null ? current.staleHours : Math.round(clamp(n, 1, 720))
    input.value = String(hours)
    if (hours !== current.staleHours) save({ staleHours: hours })
  }

  // ── food lookup key ───────────────────────────────────────────────────

  let fdcKey = $state('')
  let savingKey = $state(false)
  let removingKey = $state(false)

  const canSaveKey = $derived(!savingKey && fdcKey.trim() !== '')

  async function saveKey() {
    if (!canSaveKey) return
    savingKey = true
    try {
      await health.saveKeys(fdcKey)
    } finally {
      // Whatever happened, the key does not stay in the field.
      fdcKey = ''
      savingKey = false
    }
  }

  async function removeKey() {
    removingKey = false
    await health.clearKeys()
  }
</script>

<div class="panes">
  <HealthSources />

  <HealthImport />

  {#if !settings}
    <p class="hint">Loading…</p>
  {:else}
    <section aria-labelledby="hs-units">
      <span class="eyebrow" id="hs-units">Units</span>
      <p class="hint">
        How numbers are shown. They are kept in one unit underneath, so changing this never changes
        what was recorded.
      </p>
      <div class="units">
        {#each UNITS as u (u.key)}
          <label class="setting">
            <span>{u.label}</span>
            <select
              value={settings.units[u.key]}
              onchange={(e) => setUnit(u.key, e.currentTarget.value)}
            >
              {#each u.options as [value, label] (value)}
                <option {value}>{label}</option>
              {/each}
            </select>
          </label>
        {/each}
      </div>
    </section>

    <section aria-labelledby="hs-goals">
      <span class="eyebrow" id="hs-goals">Activity goals</span>
      <p class="hint">
        Your own daily goals, for the meters on Today and Activity. Nothing here is a
        recommendation.
      </p>
      {#each GOALS as g (g.key)}
        <label class="row-setting">
          <span><b>{g.label}</b></span>
          <span class="with-unit">
            <input
              type="number"
              step="any"
              value={shown(settings.goals[g.key], g.metric)}
              onchange={(e) => saveGoal(g, e.currentTarget)}
            />
            {unitOf(g)}
          </span>
        </label>
      {/each}
    </section>

    <section aria-labelledby="hs-nutrition">
      <span class="eyebrow" id="hs-nutrition">Nutrition targets</span>
      <p class="hint">
        What a day of eating is measured against. Leave one empty to have no target for it.
      </p>
      {#each TARGETS as t (t.key)}
        <label class="row-setting">
          <span><b>{t.label}</b></span>
          <span class="with-unit">
            <input
              type="number"
              step="any"
              placeholder="None"
              value={shown(settings.nutrition[t.key], t.metric)}
              onchange={(e) => saveTarget(t, e.currentTarget)}
            />
            {unitOf(t)}
          </span>
        </label>
      {/each}
    </section>

    <section aria-labelledby="hs-glucose">
      <span class="eyebrow" id="hs-glucose">Glucose notices</span>
      <p class="hint">
        Karma Yoga can tell you afterwards that your glucose went below or above these. It is a
        notice in the app, never an alarm: your sensor’s own alarms stay your safety alarm, and
        nothing here is advice about what your range should be. Leave one empty for no notice.
      </p>
      <label class="row-setting">
        <span><b>Below</b></span>
        <span class="with-unit">
          <input
            type="number"
            step="any"
            placeholder="None"
            value={glucoseShown(settings.glucoseLow)}
            onchange={(e) => saveGlucose('glucoseLow', e.currentTarget)}
          />
          {settings.units.glucose}
        </span>
      </label>
      <label class="row-setting">
        <span><b>Above</b></span>
        <span class="with-unit">
          <input
            type="number"
            step="any"
            placeholder="None"
            value={glucoseShown(settings.glucoseHigh)}
            onchange={(e) => saveGlucose('glucoseHigh', e.currentTarget)}
          />
          {settings.units.glucose}
        </span>
      </label>
      {#if glucoseProblem}<p class="error" role="alert">{glucoseProblem}</p>{/if}
    </section>

    <section aria-labelledby="hs-notices">
      <span class="eyebrow" id="hs-notices">Notices</span>
      <label class="toggle">
        <input
          type="checkbox"
          checked={settings.doseReminders}
          onchange={(e) => save({ doseReminders: e.currentTarget.checked })}
        />
        <span>
          <b>Remind me when a dose is due</b>
          <small>
            A notice at the time you set for each medicine. It never says whether to take it or how
            much.
          </small>
        </span>
      </label>
      <label class="row-setting">
        <span>
          <b>Tell me when a source goes quiet</b>
          <small>
            If the phone, say, has not sent anything for this long, you get one notice a day until
            it does.
          </small>
        </span>
        <span class="with-unit">
          <input
            type="number"
            min="1"
            max="720"
            value={settings.staleHours}
            onchange={(e) => saveStale(e.currentTarget)}
          />
          hours
        </span>
      </label>
    </section>
  {/if}

  <section aria-labelledby="hs-food">
    <span class="eyebrow" id="hs-food">Food lookup</span>
    <p class="hint">
      Searching for a food uses the USDA’s FoodData Central, and scanning a barcode uses Open Food
      Facts. Only the words you search for, or the barcode, leave this computer. FoodData Central
      works without a key, but the shared one allows only a few searches an hour. A key of your own
      is free and removes that limit.
    </p>
    <div>
      <button class="link" onclick={() => void openExternal(KEY_SIGNUP)}>
        Get a free key at api.data.gov <Icon name="link" size={13} />
      </button>
    </div>

    {#if keys?.fdcKeySet}
      <div class="stored">
        <span>
          <Icon name="lock" size={14} />
          A key is set{keys.fdcKeyHint ? `, ending …${keys.fdcKeyHint}` : ''}.
        </span>
        <button class="link danger" onclick={() => (removingKey = true)}>Remove key</button>
      </div>
    {/if}

    <form
      class="form"
      onsubmit={(e) => {
        e.preventDefault()
        void saveKey()
      }}
    >
      <label class="setting">
        <span>{keys?.fdcKeySet ? 'Replace the key' : 'FoodData Central key'}</span>
        <input
          type="password"
          bind:value={fdcKey}
          placeholder="Paste the key from api.data.gov"
          spellcheck="false"
          autocomplete="new-password"
        />
      </label>
      <div class="save">
        <button class="btn btn-primary" type="submit" disabled={!canSaveKey}>
          {savingKey ? 'Saving…' : 'Save key'}
        </button>
      </div>
    </form>
  </section>

  <p class="hint">
    Your health data is sealed in the vault with everything else, so it is unreadable while the
    vault is locked. The assistant can read it to answer you, using the models you have set up under
    Models: if one of them runs in the cloud, what it reads goes there. A photo of a meal or the
    pages of a document are sent only when you ask for them to be read. The token on your phone can
    only add readings: it cannot read anything, and you can revoke it at any time. This is not a
    medical device. It makes no diagnosis and gives no advice about a dose.
  </p>
</div>

{#if removingKey}
  <ConfirmDialog
    title="Remove the food lookup key?"
    detail="Searches go back to the shared key, which allows only a few an hour. Foods already in your log are not affected."
    confirmLabel="Remove key"
    onconfirm={() => void removeKey()}
    oncancel={() => (removingKey = false)}
  />
{/if}

<style>
  .panes {
    display: flex;
    flex-direction: column;
    gap: var(--sp-8);
    min-width: 0;
  }
  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }
  .hint {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
    overflow-wrap: anywhere;
  }
  .error {
    margin: 0;
  }

  .units {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(14rem, 1fr));
    gap: var(--sp-3);
  }
  .setting {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    min-width: 0;
  }
  .setting > span {
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-muted);
  }

  .toggle {
    display: flex;
    gap: var(--sp-3);
    align-items: flex-start;
    min-width: 0;
    cursor: pointer;
  }
  .toggle input {
    flex: none;
    margin-top: 3px;
    accent-color: var(--accent);
  }
  .toggle > span,
  .row-setting > span:first-child {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  b {
    font-size: var(--text-base);
    font-weight: 600;
  }
  small {
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }
  .row-setting {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-4);
    min-width: 0;
  }
  .with-unit {
    flex: none;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 6.5rem;
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }
  .with-unit input {
    width: 6rem;
    text-align: right;
  }

  input:not([type='checkbox']),
  select {
    min-width: 0;
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-base);
    user-select: text;
  }
  input:not([type='checkbox']):focus,
  select:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 16%, transparent);
  }
  .setting input,
  .setting select {
    width: 100%;
  }

  .form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }
  .stored {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-sunken);
    font-size: var(--text-sm);
    min-width: 0;
  }
  .stored span {
    display: flex;
    flex: 1;
    flex-wrap: wrap;
    align-items: baseline;
    gap: var(--sp-1) var(--sp-2);
    min-width: 0;
    line-height: var(--leading-normal);
    color: var(--fg-muted);
  }
  .link {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: var(--sp-1);
    color: var(--accent);
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .link:hover {
    text-decoration: underline;
  }
  .link.danger {
    color: var(--danger);
  }
  .save {
    display: flex;
    justify-content: flex-end;
  }

  /* ── A phone ──────────────────────────────────────────────────────────
     The sources are drawn by `HealthSources`, which knows nothing of a
     phone; what a source's card and the order of devices need on one is
     said here, over its own rules. A card's name gets the width beside its
     mark with the buttons on a line of their own under it; the little
     round buttons are a finger wide; the order is moved by its arrows, not
     by a drag a finger cannot start. */
  :global([data-phone]) .panes :global(.sources .card-head) {
    flex-wrap: wrap;
  }
  :global([data-phone]) .panes :global(.sources .titles) {
    flex: 1 1 calc(100% - 32px - var(--sp-3));
  }
  :global([data-phone]) .panes :global(.sources .actions) {
    flex: 1 1 100%;
    justify-content: flex-end;
    gap: var(--sp-2);
  }
  :global([data-phone]) .panes :global(.sources .icon) {
    width: 40px;
    height: 40px;
  }
  :global([data-phone]) .panes :global(.sources .grip) {
    display: none;
  }
  :global([data-phone]) .panes :global(.sources .rank) {
    padding-block: 0;
  }
</style>
