<script lang="ts">
  // One medication: everything about it that a person sets.
  //
  // Opened on `health.editingMedication` -- a blank one for "Add medication",
  // or one already on the list -- and copied into local fields, so nothing is
  // kept until Save.
  //
  // Four things are worth explaining.
  //
  // The schedule is what makes a dose "due": times of day, on every day, on
  // chosen weekdays or every few days, or none at all for something taken as
  // needed. Times are local clock times; the vault turns them into moments
  // for the day on screen.
  //
  // Timing rules ("nothing to eat for 30 minutes after", "four hours apart
  // from calcium") are checked after the fact and reported as notes, never as
  // alarms. A rule can be written here, and where a condition's plan suggests
  // rules for a tag the medication carries, those are offered with one press.
  //
  // Stopping and pausing are kept with a reason and a date rather than by
  // deleting, so the history stays whole.
  //
  // A change of dose or strength is recorded in the medication's history by
  // the vault when it is saved, dated that day -- it is what a test recheck
  // is counted from. It is listed here, read-only, beside the recent doses.
  //
  // Nothing on this page advises a dose or a schedule.

  import { health } from '../lib/health.svelte'
  import { ruleText, scheduleText } from '../lib/health'
  import { trapFocus } from '../lib/focus'
  import { formatInstantTime, friendlyDate } from '../lib/format'
  import { isoDate, todayIso } from '../lib/time'
  import type { MedKind, MedRule, MedSchedule, MedStatus, Medication, Supply } from '../lib/types'
  import Icon from './Icon.svelte'

  const KINDS: { value: MedKind; label: string }[] = [
    { value: 'prescription', label: 'Prescription' },
    { value: 'overTheCounter', label: 'Over the counter' },
    { value: 'supplement', label: 'Supplement' },
  ]
  const STATUSES: { value: MedStatus; label: string }[] = [
    { value: 'active', label: 'Taking it' },
    { value: 'paused', label: 'Paused' },
    { value: 'stopped', label: 'Stopped' },
  ]
  const SCHEDULES: { value: MedSchedule['type']; label: string }[] = [
    { value: 'daily', label: 'Every day' },
    { value: 'weekly', label: 'On certain days of the week' },
    { value: 'everyNDays', label: 'Every few days' },
    { value: 'asNeeded', label: 'Only as needed' },
  ]
  const WEEKDAYS = [
    { n: 1, label: 'Mon' },
    { n: 2, label: 'Tue' },
    { n: 3, label: 'Wed' },
    { n: 4, label: 'Thu' },
    { n: 5, label: 'Fri' },
    { n: 6, label: 'Sat' },
    { n: 7, label: 'Sun' },
  ]
  const COMMON_TAGS = ['thyroid', 'calcium', 'iron', 'insulin']
  const DOSE_WORDS = { taken: 'Taken', skipped: 'Skipped', missed: 'Missed' }

  /** What the editor was opened on, read once. The parent only shows it
   *  while there is one. */
  const base = $state.snapshot(health.editingMedication) as Medication
  const stored = health.medications.some((m) => m.id === base.id)

  let med = $state<Medication>(structuredClone(base))
  let saving = $state(false)
  let formError = $state('')

  function close() {
    health.editingMedication = null
  }

  // Deleting asks first, through the store; once the medication is gone from
  // its list, so is this editor.
  $effect(() => {
    if (stored && !health.medications.some((m) => m.id === base.id)) close()
  })

  const sched = $derived(med.schedule)

  // ── Schedule ────────────────────────────────────────────────────────────

  function setScheduleType(type: MedSchedule['type']) {
    const cur = med.schedule
    const times = cur.type === 'asNeeded' ? ['08:00:00'] : [...cur.times]
    switch (type) {
      case 'daily':
        med.schedule = { type: 'daily', times }
        break
      case 'weekly':
        med.schedule = { type: 'weekly', days: [1, 2, 3, 4, 5], times }
        break
      case 'everyNDays':
        med.schedule = { type: 'everyNDays', n: 2, from: todayIso(), times }
        break
      default:
        med.schedule = { type: 'asNeeded' }
    }
  }

  function setTimes(times: string[]) {
    const s = med.schedule
    if (s.type !== 'asNeeded') med.schedule = { ...s, times }
  }
  function addTime() {
    const s = med.schedule
    if (s.type !== 'asNeeded')
      setTimes([...s.times, s.times.length === 0 ? '08:00:00' : '20:00:00'])
  }
  function changeTime(i: number, hhmm: string) {
    const s = med.schedule
    if (!hhmm || s.type === 'asNeeded') return
    setTimes(s.times.map((t, j) => (j === i ? `${hhmm}:00` : t)))
  }
  function removeTime(i: number) {
    const s = med.schedule
    if (s.type !== 'asNeeded') setTimes(s.times.filter((_, j) => j !== i))
  }
  function toggleDay(n: number) {
    const s = med.schedule
    if (s.type !== 'weekly') return
    const days = s.days.includes(n) ? s.days.filter((d) => d !== n) : [...s.days, n].sort()
    med.schedule = { ...s, days }
  }
  function setEvery(n: number) {
    const s = med.schedule
    if (s.type === 'everyNDays' && Number.isFinite(n) && n >= 1) {
      med.schedule = { ...s, n: Math.round(n) }
    }
  }
  function setEveryFrom(day: string) {
    const s = med.schedule
    if (s.type === 'everyNDays' && day) med.schedule = { ...s, from: day }
  }

  // ── Tags ────────────────────────────────────────────────────────────────

  let tagText = $state('')
  function addTag(raw: string) {
    const t = raw.trim().toLowerCase()
    if (t && !med.tags.includes(t)) med.tags = [...med.tags, t]
  }
  function commitTags() {
    for (const part of tagText.split(',')) addTag(part)
    tagText = ''
  }
  function removeTag(t: string) {
    med.tags = med.tags.filter((x) => x !== t)
  }

  // ── Rules ───────────────────────────────────────────────────────────────

  let ruleKind = $state<MedRule['type']>('emptyStomach')
  let ruleMinutes = $state(30)
  let ruleHours = $state(4)
  let ruleTags = $state('')
  let ruleError = $state('')

  const sameRule = (a: MedRule, b: MedRule) => JSON.stringify(a) === JSON.stringify(b)

  function addRule() {
    ruleError = ''
    const minutes = Math.max(5, Math.min(240, Math.round(Number(ruleMinutes) || 30)))
    let rule: MedRule
    if (ruleKind === 'emptyStomach') rule = { type: 'emptyStomach', minutes }
    else if (ruleKind === 'withFood') rule = { type: 'withFood', minutes }
    else {
      const tags = ruleTags
        .split(',')
        .map((t) => t.trim().toLowerCase())
        .filter(Boolean)
      if (tags.length === 0) {
        ruleError = 'Say which medicines or supplements, by tag: calcium, iron…'
        return
      }
      rule = { type: 'apartFrom', tags, hours: Math.max(0.5, Number(ruleHours) || 4) }
    }
    if (!med.rules.some((r) => sameRule(r, rule))) med.rules = [...med.rules, rule]
    ruleTags = ''
  }
  function removeRule(i: number) {
    med.rules = med.rules.filter((_, j) => j !== i)
  }

  /** Rules a condition's plan suggests for the tags this medication carries
   *  that it does not have yet. */
  const suggestions = $derived.by(() => {
    const out: { rule: MedRule; tag: string; from: string }[] = []
    for (const tag of med.tags) {
      for (const c of health.conditions) {
        for (const rule of c.plan.medRules[tag] ?? []) {
          if (med.rules.some((r) => sameRule(r, rule))) continue
          if (out.some((o) => sameRule(o.rule, rule))) continue
          out.push({ rule, tag, from: c.name })
        }
      }
    }
    return out
  })
  function adopt(rule: MedRule) {
    if (!med.rules.some((r) => sameRule(r, rule))) med.rules = [...med.rules, $state.snapshot(rule)]
  }

  // ── Status, conditions, supply ──────────────────────────────────────────

  function setStatus(status: MedStatus) {
    med.status = status
    if (status === 'stopped' && !med.stopped) med.stopped = todayIso()
    if (status !== 'stopped') med.stopped = null
    if (status === 'active') med.stopReason = ''
  }

  function toggleCondition(id: string) {
    med.conditionIds = med.conditionIds.includes(id)
      ? med.conditionIds.filter((c) => c !== id)
      : [...med.conditionIds, id]
  }

  function trackSupply(on: boolean) {
    med.supply = on ? (med.supply ?? { remaining: 30, refillDays: 7, lastRefill: null }) : null
  }
  function setSupply(patch: Partial<Supply>) {
    if (med.supply) med.supply = { ...med.supply, ...patch }
  }
  function numberOf(raw: string): number | null {
    const n = Number(raw.replace(',', '.'))
    return raw.trim() !== '' && Number.isFinite(n) && n >= 0 ? n : null
  }

  // ── What the vault holds about it ───────────────────────────────────────

  const changes = $derived([...med.history].sort((a, b) => (a.on < b.on ? 1 : -1)))
  const recent = $derived(
    health.doses
      .filter((d) => d.medicationId === med.id)
      .sort((a, b) => Date.parse(b.at) - Date.parse(a.at))
      .slice(0, 12),
  )

  // ── Saving ──────────────────────────────────────────────────────────────

  async function save() {
    formError = ''
    const draft = $state.snapshot(med)
    const name = draft.name.trim()
    if (!name) {
      formError = 'Give the medication a name.'
      return
    }
    if (!(Number(draft.dose) > 0)) {
      formError = 'The amount for each time needs to be more than zero.'
      return
    }
    const s = draft.schedule
    let schedule: MedSchedule = s
    if (s.type !== 'asNeeded') {
      const times = [...new Set(s.times)].sort()
      if (times.length === 0) {
        formError = 'Add at least one time of day.'
        return
      }
      if (s.type === 'weekly' && s.days.length === 0) {
        formError = 'Choose at least one day of the week.'
        return
      }
      schedule = { ...s, times }
    }
    const supply = draft.supply
      ? {
          ...draft.supply,
          remaining: Math.max(0, Number(draft.supply.remaining) || 0),
          refillDays: Math.max(0, Math.round(Number(draft.supply.refillDays) || 0)),
        }
      : null
    saving = true
    try {
      const saved = await health.saveMedication({
        ...draft,
        name,
        form: draft.form.trim(),
        strength: draft.strength.trim(),
        doseUnit: draft.doseUnit.trim(),
        schedule,
        supply,
        stopReason: draft.status === 'active' ? '' : draft.stopReason.trim(),
        prescriber: draft.prescriber.trim(),
        pharmacy: draft.pharmacy.trim(),
        notes: draft.notes.trim(),
        updatedAt: new Date().toISOString(),
      })
      if (saved) close()
      else formError = 'The medication could not be saved. Try again.'
    } finally {
      saving = false
    }
  }

  function remove() {
    const m = health.medications.find((x) => x.id === base.id)
    if (m) health.deleteMedication(m)
  }
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape' && !health.question) close()
  }}
/>

<div class="scrim"></div>
<div
  class="sheet wide"
  role="dialog"
  aria-modal="true"
  aria-label={stored ? 'Edit medication' : 'Add a medication'}
  use:trapFocus
>
  <div class="sheet-scroll">
    <h2 class="title">{stored ? 'Edit medication' : 'Add a medication'}</h2>

    <!-- About it -->
    <h3 class="eyebrow">About it</h3>
    <div class="grid">
      <div class="span2">
        <label class="field-label" for="med-name">Name</label>
        <input
          id="med-name"
          class="field"
          bind:value={med.name}
          placeholder="Levothyroxine"
          autocomplete="off"
        />
      </div>
      <div>
        <label class="field-label" for="med-kind">Kind</label>
        <select id="med-kind" class="field" bind:value={med.kind}>
          {#each KINDS as k (k.value)}<option value={k.value}>{k.label}</option>{/each}
        </select>
      </div>
      <div>
        <label class="field-label" for="med-form">Form</label>
        <input
          id="med-form"
          class="field"
          list="med-forms"
          bind:value={med.form}
          placeholder="Tablet"
          autocomplete="off"
        />
        <datalist id="med-forms">
          {#each ['Tablet', 'Capsule', 'Injection', 'Drops', 'Liquid', 'Patch', 'Inhaler', 'Cream'] as f (f)}
            <option value={f}></option>
          {/each}
        </datalist>
      </div>
      <div>
        <label class="field-label" for="med-strength">Strength</label>
        <input
          id="med-strength"
          class="field"
          bind:value={med.strength}
          placeholder="50 mcg"
          autocomplete="off"
        />
      </div>
      <div class="pair">
        <div>
          <label class="field-label" for="med-dose">Each time</label>
          <input
            id="med-dose"
            class="field"
            type="number"
            min="0"
            step="any"
            inputmode="decimal"
            bind:value={med.dose}
          />
        </div>
        <div>
          <label class="field-label" for="med-unit">Unit</label>
          <input
            id="med-unit"
            class="field"
            bind:value={med.doseUnit}
            placeholder="tablet"
            autocomplete="off"
          />
        </div>
      </div>
    </div>

    <!-- When -->
    <h3 class="eyebrow">When</h3>
    <div class="grid">
      <div class="span2">
        <label class="field-label" for="med-sched">How often</label>
        <select
          id="med-sched"
          class="field"
          value={sched.type}
          onchange={(e) => setScheduleType(e.currentTarget.value as MedSchedule['type'])}
        >
          {#each SCHEDULES as s (s.value)}<option value={s.value}>{s.label}</option>{/each}
        </select>
      </div>
    </div>

    {#if sched.type === 'weekly'}
      <div class="days" role="group" aria-label="Days of the week">
        {#each WEEKDAYS as d (d.n)}
          <button
            class="chip"
            class:on={sched.days.includes(d.n)}
            aria-pressed={sched.days.includes(d.n)}
            onclick={() => toggleDay(d.n)}>{d.label}</button
          >
        {/each}
      </div>
    {:else if sched.type === 'everyNDays'}
      <div class="every">
        <label class="inline-label" for="med-every">Every</label>
        <input
          id="med-every"
          class="field small num"
          type="number"
          min="1"
          step="1"
          value={sched.n}
          oninput={(e) => setEvery(e.currentTarget.valueAsNumber)}
        />
        <span class="inline-label">days, starting</span>
        <input
          class="field small"
          type="date"
          aria-label="First day"
          value={sched.from}
          onchange={(e) => setEveryFrom(e.currentTarget.value)}
        />
      </div>
    {/if}

    {#if sched.type !== 'asNeeded'}
      <ul class="times">
        {#each sched.times as t, i (i)}
          <li>
            <input
              class="field small"
              type="time"
              aria-label="Time {i + 1}"
              value={t.slice(0, 5)}
              onchange={(e) => changeTime(i, e.currentTarget.value)}
            />
            <button
              class="icon-btn danger"
              aria-label="Remove time {i + 1}"
              title="Remove this time"
              onclick={() => removeTime(i)}
            >
              <Icon name="close" size={13} />
            </button>
          </li>
        {/each}
        <li>
          <button class="btn btn-outline btn-sm" onclick={addTime}>
            <Icon name="plus" size={13} /> Add a time
          </button>
        </li>
      </ul>
    {:else}
      <p class="hint small">Nothing is due; log a dose from the list whenever you take one.</p>
    {/if}
    <p class="hint small">{scheduleText(med.schedule)}</p>

    <label class="check">
      <input type="checkbox" bind:checked={med.remind} />
      Remind me at each time (when dose reminders are on in Settings)
    </label>

    <!-- Tags and rules -->
    <h3 class="eyebrow">Tags and timing rules</h3>
    <p class="hint small">
      A tag names what it is, so a rule on another medicine can refer to it: thyroid, calcium, iron.
    </p>
    <div class="tags">
      {#each med.tags as t (t)}
        <span class="tag removable">
          {t}
          <button aria-label="Remove tag {t}" onclick={() => removeTag(t)}>
            <Icon name="close" size={11} />
          </button>
        </span>
      {/each}
      <input
        class="field small tag-input"
        bind:value={tagText}
        placeholder="Add a tag"
        aria-label="Add a tag"
        autocomplete="off"
        onkeydown={(e) => {
          if (e.key === 'Enter' || e.key === ',') {
            e.preventDefault()
            commitTags()
          }
        }}
        onblur={commitTags}
      />
    </div>
    <div class="tags">
      {#each COMMON_TAGS.filter((t) => !med.tags.includes(t)) as t (t)}
        <button class="chip" onclick={() => addTag(t)}>+ {t}</button>
      {/each}
    </div>

    {#if med.rules.length > 0}
      <ul class="rules">
        {#each med.rules as rule, i (i)}
          <li>
            <span>{ruleText(rule)}</span>
            <button
              class="icon-btn danger"
              aria-label="Remove rule: {ruleText(rule)}"
              title="Remove this rule"
              onclick={() => removeRule(i)}
            >
              <Icon name="close" size={13} />
            </button>
          </li>
        {/each}
      </ul>
    {/if}

    {#if suggestions.length > 0}
      <div class="suggest">
        <p class="hint small">From your conditions’ plans, for what this medicine carries:</p>
        {#each suggestions as s (ruleText(s.rule))}
          <div class="suggest-row">
            <span>{ruleText(s.rule)} <span class="faint">({s.tag}, {s.from})</span></span>
            <button class="btn btn-outline btn-sm" onclick={() => adopt(s.rule)}>Use it</button>
          </div>
        {/each}
      </div>
    {/if}

    <div class="add-rule">
      <label class="sr" for="rule-kind">Kind of rule</label>
      <select id="rule-kind" class="field small" bind:value={ruleKind}>
        <option value="emptyStomach">Empty stomach</option>
        <option value="withFood">With food</option>
        <option value="apartFrom">Apart from other medicines</option>
      </select>
      {#if ruleKind === 'apartFrom'}
        <input
          class="field small num"
          type="number"
          min="0.5"
          step="0.5"
          aria-label="Hours apart"
          bind:value={ruleHours}
        />
        <span class="inline-label">hours from</span>
        <input
          class="field small grow"
          bind:value={ruleTags}
          placeholder="calcium, iron"
          aria-label="Tags to keep apart from"
          autocomplete="off"
        />
      {:else}
        <input
          class="field small num"
          type="number"
          min="5"
          max="240"
          step="5"
          aria-label="Minutes"
          bind:value={ruleMinutes}
        />
        <span class="inline-label"
          >minutes {ruleKind === 'emptyStomach'
            ? 'without food after'
            : 'either side of a meal'}</span
        >
      {/if}
      <button class="btn btn-outline btn-sm" onclick={addRule}>Add rule</button>
    </div>
    {#if ruleError}<p class="error" role="alert">{ruleError}</p>{/if}
    <p class="hint small">
      Rules are yours to set with your clinician. They are checked afterwards and shown as notes,
      never as alarms. This is not medical advice.
    </p>

    <!-- Status -->
    <h3 class="eyebrow">Status and dates</h3>
    <div class="grid">
      <div>
        <label class="field-label" for="med-status">Status</label>
        <select
          id="med-status"
          class="field"
          value={med.status}
          onchange={(e) => setStatus(e.currentTarget.value as MedStatus)}
        >
          {#each STATUSES as s (s.value)}<option value={s.value}>{s.label}</option>{/each}
        </select>
      </div>
      <div>
        <label class="field-label" for="med-started">Started</label>
        <input
          id="med-started"
          class="field"
          type="date"
          max={todayIso()}
          value={med.started ?? ''}
          onchange={(e) => (med.started = e.currentTarget.value || null)}
        />
      </div>
      {#if med.status !== 'active'}
        <div class:span2={med.status !== 'stopped'}>
          <label class="field-label" for="med-reason">
            Why {med.status === 'stopped' ? 'stopped' : 'paused'}
          </label>
          <input
            id="med-reason"
            class="field"
            bind:value={med.stopReason}
            placeholder="Changed by my doctor, side effects…"
            autocomplete="off"
          />
        </div>
      {/if}
      {#if med.status === 'stopped'}
        <div>
          <label class="field-label" for="med-stopped">Stopped on</label>
          <input
            id="med-stopped"
            class="field"
            type="date"
            max={todayIso()}
            value={med.stopped ?? ''}
            onchange={(e) => (med.stopped = e.currentTarget.value || null)}
          />
        </div>
      {/if}
    </div>

    <!-- Conditions -->
    {#if health.conditions.length > 0}
      <h3 class="eyebrow">What it treats</h3>
      <div class="conditions" role="group" aria-label="Conditions it treats">
        {#each health.conditions as c (c.id)}
          <label class="check">
            <input
              type="checkbox"
              checked={med.conditionIds.includes(c.id)}
              onchange={() => toggleCondition(c.id)}
            />
            {c.name}
          </label>
        {/each}
      </div>
    {/if}

    <!-- Who and where -->
    <h3 class="eyebrow">Who and where</h3>
    <div class="grid">
      <div>
        <label class="field-label" for="med-prescriber">Prescribed by</label>
        <input id="med-prescriber" class="field" bind:value={med.prescriber} autocomplete="off" />
      </div>
      <div>
        <label class="field-label" for="med-pharmacy">Pharmacy</label>
        <input id="med-pharmacy" class="field" bind:value={med.pharmacy} autocomplete="off" />
      </div>
    </div>

    <!-- Supply -->
    <h3 class="eyebrow">Supply</h3>
    <label class="check">
      <input
        type="checkbox"
        checked={!!med.supply}
        onchange={(e) => trackSupply(e.currentTarget.checked)}
      />
      Keep count of what is left, and tell me when to refill
    </label>
    {#if med.supply}
      <div class="grid">
        <div>
          <label class="field-label" for="sup-left">Left ({med.doseUnit || 'doses'})</label>
          <input
            id="sup-left"
            class="field"
            inputmode="decimal"
            value={med.supply.remaining}
            onchange={(e) => {
              const n = numberOf(e.currentTarget.value)
              if (n !== null) setSupply({ remaining: n })
            }}
          />
        </div>
        <div>
          <label class="field-label" for="sup-days">Remind when this many days are left</label>
          <input
            id="sup-days"
            class="field"
            inputmode="numeric"
            value={med.supply.refillDays}
            onchange={(e) => {
              const n = numberOf(e.currentTarget.value)
              if (n !== null) setSupply({ refillDays: Math.round(n) })
            }}
          />
        </div>
        <div>
          <label class="field-label" for="sup-refill">Last refilled</label>
          <input
            id="sup-refill"
            class="field"
            type="date"
            max={todayIso()}
            value={med.supply.lastRefill ?? ''}
            onchange={(e) => setSupply({ lastRefill: e.currentTarget.value || null })}
          />
        </div>
      </div>
      <p class="hint small">
        Each dose you log as taken comes off what is left, by the amount for each time.
      </p>
    {/if}

    <!-- Notes -->
    <h3 class="eyebrow">Notes</h3>
    <textarea
      class="field"
      rows="2"
      bind:value={med.notes}
      aria-label="Notes"
      placeholder="Anything worth remembering about it"
    ></textarea>

    <!-- History -->
    {#if stored}
      <h3 class="eyebrow">Changes of dose</h3>
      {#if changes.length === 0}
        <p class="hint small">
          None yet. A change of dose or strength is noted here when you save it.
        </p>
      {:else}
        <ul class="log">
          {#each changes as c (c.on + c.dose + c.strength)}
            <li>
              <span class="when">{friendlyDate(c.on)}</span>
              <span
                >{c.dose}{med.doseUnit ? ` ${med.doseUnit}` : ''}{c.strength
                  ? ` · ${c.strength}`
                  : ''}</span
              >
              {#if c.note}<span class="faint">{c.note}</span>{/if}
            </li>
          {/each}
        </ul>
      {/if}

      <h3 class="eyebrow">Recent doses</h3>
      {#if recent.length === 0}
        <p class="hint small">No doses logged yet.</p>
      {:else}
        <ul class="log">
          {#each recent as d (d.id)}
            <li>
              <span class="when"
                >{friendlyDate(isoDate(new Date(d.at)))}, {formatInstantTime(d.at)}</span
              >
              <span>{DOSE_WORDS[d.status]}</span>
              {#if d.note}<span class="faint">{d.note}</span>{/if}
            </li>
          {/each}
        </ul>
      {/if}
    {/if}

    {#if formError}<p class="error" role="alert">{formError}</p>{/if}
  </div>

  <div class="sheet-row">
    {#if stored}
      <button class="btn btn-ghost-danger" onclick={remove}>Delete</button>
    {/if}
    <span class="spacer"></span>
    <button class="btn" onclick={close}>Cancel</button>
    <button class="btn btn-primary" disabled={saving} onclick={() => void save()}>Save</button>
  </div>
</div>

<style>
  .sheet.wide {
    top: 4%;
    width: min(680px, calc(100vw - var(--sp-8)));
    max-height: 92vh;
  }

  .title {
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
  }
  .eyebrow {
    margin: var(--sp-5) 0 var(--sp-2);
  }
  .title + .eyebrow {
    margin-top: var(--sp-3);
  }
  .field-label {
    margin-bottom: var(--sp-1);
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--sp-3);
  }
  .span2 {
    grid-column: 1 / -1;
  }
  .pair {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--sp-2);
  }

  .field.small {
    height: 32px;
    font-size: var(--text-sm);
    width: auto;
  }
  .field.num {
    width: 5.5em;
  }
  .field.grow {
    flex: 1;
    min-width: 0;
  }
  textarea.field {
    resize: vertical;
  }

  .hint.small {
    margin: var(--sp-1) 0 0;
    font-size: var(--text-xs);
  }
  .faint {
    color: var(--fg-faint);
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }
  .inline-label {
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }

  .days {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-1);
    margin-top: var(--sp-3);
  }
  .chip {
    display: inline-flex;
    align-items: center;
    height: 28px;
    padding: 0 var(--sp-3);
    border-radius: 999px;
    border: 1px solid var(--border-strong);
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-muted);
  }
  .chip:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .chip.on {
    background: var(--bg-selected);
    border-color: transparent;
    color: var(--accent);
  }

  .every {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--sp-2);
    margin-top: var(--sp-3);
  }

  .times {
    margin: var(--sp-3) 0 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2) var(--sp-3);
  }
  .times li {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
  }

  .check {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin-top: var(--sp-3);
    font-size: var(--text-sm);
    color: var(--fg-muted);
    cursor: pointer;
  }
  .check input {
    accent-color: var(--accent);
  }
  .conditions {
    display: flex;
    flex-direction: column;
  }
  .conditions .check {
    margin-top: var(--sp-2);
  }

  .tags {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-1) var(--sp-2);
    margin-top: var(--sp-2);
  }
  .tag.removable {
    gap: 4px;
    padding-right: 4px;
  }
  .tag.removable button {
    display: grid;
    place-items: center;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .tag.removable button:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .tag-input {
    width: 9em;
  }

  .rules {
    margin: var(--sp-3) 0 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
  }
  .rules li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2);
    padding: 4px var(--sp-3);
    border-radius: var(--radius);
    background: var(--bg-sunken);
    font-size: var(--text-sm);
    color: var(--fg);
  }
  .suggest {
    margin-top: var(--sp-3);
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
  }
  .suggest-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    font-size: var(--text-sm);
    color: var(--fg);
  }
  .add-rule {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--sp-2);
    margin-top: var(--sp-3);
  }

  .icon-btn {
    flex: none;
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .icon-btn.danger:hover {
    background: color-mix(in oklab, var(--danger) 12%, transparent);
    color: var(--danger);
  }

  .log {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .log li {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-1) var(--sp-3);
  }
  .log .when {
    min-width: 11em;
    color: var(--fg);
    font-variant-numeric: tabular-nums;
  }

  .error {
    margin: var(--sp-3) 0 0;
  }
</style>
