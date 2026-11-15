<script lang="ts">
  // Medications: what is due today, what is taken, what is on the list, and
  // what has been logged.
  //
  // The day comes first, because that is what is opened this pane for: each
  // dose due with Taken, Skip and "at another time" beside it, answered in
  // one press. Under it the list itself -- name, strength, the schedule in
  // words, how steadily the last thirty days were taken and how much supply
  // is left -- then any timing notes the rules found (written as notices
  // after the fact, never alarms), the history of doses with a way to remove
  // a mistaken one, and allergies.
  //
  // Nothing here advises a dose. Schedules, rules and supply are the person's
  // own, written with their clinician.

  import { health } from '../lib/health.svelte'
  import { scheduleText } from '../lib/health'
  import { daysOfSupply, steadiness } from '../lib/health-doses'
  import { addDays, isoDate, todayIso } from '../lib/time'
  import { formatInstantTime, friendlyDate, plural } from '../lib/format'
  import type { Dose, DueDose, Medication, MedStatus } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import HealthAllergies from './HealthAllergies.svelte'
  import HealthMedicationEditor from './HealthMedicationEditor.svelte'
  import Icon from './Icon.svelte'
  import Meter from './Meter.svelte'

  const accent = 'var(--app-health, var(--accent))'
  const STATUS_NAMES: Record<MedStatus, string> = {
    active: 'Active',
    paused: 'Paused',
    stopped: 'Stopped',
  }
  const DOSE_WORDS: Record<Dose['status'], string> = {
    taken: 'Taken',
    skipped: 'Skipped',
    missed: 'Missed',
  }

  const today = $derived(todayIso())
  const date = $derived(health.date || today)

  // ── Today's doses ───────────────────────────────────────────────────────

  const due = $derived(
    [...health.dosesDue].sort((a, b) => Date.parse(a.dueAt) - Date.parse(b.dueAt)),
  )

  /** The row asking for another time, by `medication|due time`. */
  let other = $state<string | null>(null)
  let otherTime = $state('')
  const rowKey = (d: DueDose) => `${d.medicationId}|${d.dueAt}`

  function take(d: DueDose) {
    void health.logDose(d.medicationId, { status: 'taken', scheduledAt: d.dueAt })
  }
  function skip(d: DueDose) {
    void health.logDose(d.medicationId, { status: 'skipped', scheduledAt: d.dueAt })
  }
  function askOther(d: DueDose) {
    other = rowKey(d)
    otherTime = new Date().toTimeString().slice(0, 5)
  }
  function logOther(d: DueDose) {
    const at = new Date(`${isoDate(new Date(d.dueAt))}T${otherTime}:00`)
    if (!otherTime || Number.isNaN(at.getTime())) return
    other = null
    void health.logDose(d.medicationId, {
      status: 'taken',
      at: at.toISOString(),
      scheduledAt: d.dueAt,
    })
  }

  // ── The list ────────────────────────────────────────────────────────────

  const byOrder = (a: Medication, b: Medication) =>
    a.sortOrder - b.sortOrder || a.name.localeCompare(b.name)
  const current = $derived(
    health.medications
      .filter((m) => m.status !== 'stopped')
      .sort((a, b) => (a.status === b.status ? byOrder(a, b) : a.status === 'active' ? -1 : 1)),
  )
  const stopped = $derived(health.medications.filter((m) => m.status === 'stopped').sort(byOrder))
  let showStopped = $state(false)

  /** The last thirty full days, for how steadily doses were taken. */
  const span = $derived({ from: addDays(today, -30), to: addDays(today, -1) })

  function edit(med: Medication) {
    health.editingMedication = $state.snapshot(med)
  }

  function add() {
    const now = new Date().toISOString()
    health.editingMedication = {
      id: crypto.randomUUID(),
      name: '',
      kind: 'prescription',
      form: '',
      strength: '',
      dose: 1,
      doseUnit: '',
      schedule: { type: 'daily', times: ['08:00:00'] },
      rules: [],
      tags: [],
      status: 'active',
      started: today,
      stopped: null,
      stopReason: '',
      history: [],
      conditionIds: [],
      prescriber: '',
      pharmacy: '',
      supply: null,
      remind: true,
      codes: [],
      sourceNames: [],
      notes: '',
      sortOrder: health.medications.reduce((n, m) => Math.max(n, m.sortOrder + 1), 0),
      createdAt: now,
      updatedAt: now,
    }
  }

  function doseText(m: Medication): string {
    if (!m.dose) return ''
    const amount = Number.isInteger(m.dose) ? String(m.dose) : m.dose.toFixed(2).replace(/0+$/, '')
    return `${amount}${m.doseUnit ? ` ${m.doseUnit}` : ''} each time`
  }

  const nameOf = (id: string) => health.medications.find((m) => m.id === id)?.name ?? 'A medicine'

  // ── Timing notes ────────────────────────────────────────────────────────

  const alerts = $derived(
    [...health.alerts].sort((a, b) => Date.parse(b.at) - Date.parse(a.at)).slice(0, 12),
  )

  // ── History ─────────────────────────────────────────────────────────────

  let allHistory = $state(false)
  const history = $derived.by(() => {
    const now = Date.now()
    const rows = health.doses
      .filter((d) => {
        const at = Date.parse(d.at)
        return at >= now - 30 * 86_400_000 && at <= now + 86_400_000
      })
      .sort((a, b) => Date.parse(b.at) - Date.parse(a.at))
    const days = new Map<string, Dose[]>()
    for (const d of rows) {
      const day = isoDate(new Date(d.at))
      const list = days.get(day)
      if (list) list.push(d)
      else days.set(day, [d])
    }
    return [...days].map(([day, doses]) => ({ day, doses }))
  })
  const shownHistory = $derived(allHistory ? history : history.slice(0, 7))

  let deleting = $state<Dose | null>(null)
</script>

{#snippet medRow(m: Medication)}
  {@const steady = steadiness(m, health.doses, span.from, span.to)}
  {@const left = daysOfSupply(m)}
  {@const recent =
    m.schedule.type === 'asNeeded' ? steadiness(m, health.doses, addDays(today, -29), today) : null}
  <li class="med" class:off={m.status !== 'active'}>
    <button class="list-row med-main" onclick={() => edit(m)}>
      <span class="med-top">
        <span class="med-name">{m.name}</span>
        {#if m.strength}<span class="med-strength">{m.strength}</span>{/if}
        {#if m.status !== 'active'}<span class="tag">{STATUS_NAMES[m.status]}</span>{/if}
      </span>
      <span class="med-sub">
        {scheduleText(m.schedule)}{doseText(m) ? ` · ${doseText(m)}` : ''}
      </span>
      {#if m.tags.length > 0}
        <span class="chips">
          {#each m.tags as t (t)}<span class="tag">{t}</span>{/each}
        </span>
      {/if}
    </button>

    <div class="med-side">
      {#if m.status !== 'stopped'}
        {#if recent}
          <p class="side-line">
            {plural(recent.taken, 'dose')} taken in the last 30 days
          </p>
          <button class="btn btn-outline btn-sm" onclick={() => void health.logDose(m.id)}>
            Log a dose now
          </button>
        {:else if steady.share !== null}
          <Meter
            label="Last 30 days"
            value={steady.share}
            color={accent}
            note="{steady.taken} of {steady.expected} doses{steady.skipped > 0
              ? `, ${steady.skipped} skipped`
              : ''}"
          />
        {:else}
          <p class="side-line">Too new to show how steadily it is taken.</p>
        {/if}
        {#if m.supply}
          {@const low = left !== null && left <= m.supply.refillDays}
          <p class="side-line" class:low>
            {#if left !== null}
              About {plural(Math.max(0, Math.round(left)), 'day')} of supply left{#if low}. Time to
                ask for a refill.{/if}
            {:else}
              {m.supply.remaining} {m.doseUnit || 'doses'} left
            {/if}
          </p>
        {/if}
      {/if}
    </div>
  </li>
{/snippet}

<div class="meds scroll">
  <div class="page">
    <div class="bar">
      <span class="grow"></span>
      <button class="btn btn-primary" onclick={add}>
        <Icon name="plus" size={14} /> Add medication
      </button>
    </div>

    <!-- Due today -->
    {#if due.length > 0}
      <section class="card" aria-labelledby="med-due">
        <h2 class="eyebrow" id="med-due">Due {friendlyDate(date).toLowerCase()}</h2>
        <ul class="dues">
          {#each due as d (rowKey(d))}
            <li class="due">
              <span class="due-time">{formatInstantTime(d.dueAt)}</span>
              <span class="due-name">{d.name}</span>
              {#if d.dose}
                <span class="due-done">
                  <Icon name={d.dose.status === 'skipped' ? 'minus' : 'tick'} size={13} />
                  {DOSE_WORDS[d.dose.status]}
                  {formatInstantTime(d.dose.at)}
                </span>
                <button
                  class="link"
                  onclick={() => {
                    if (d.dose) void health.deleteDose(d.dose)
                  }}
                >
                  Undo
                </button>
              {:else if other === rowKey(d)}
                <form
                  class="other"
                  onsubmit={(e) => {
                    e.preventDefault()
                    logOther(d)
                  }}
                >
                  <label class="sr" for="other-{rowKey(d)}">Time it was taken</label>
                  <input
                    id="other-{rowKey(d)}"
                    class="field small"
                    type="time"
                    bind:value={otherTime}
                  />
                  <button class="btn btn-sm btn-primary" type="submit">Save</button>
                  <button class="btn btn-sm" type="button" onclick={() => (other = null)}>
                    Cancel
                  </button>
                </form>
              {:else}
                {#if d.overdue}<span class="overdue">Overdue</span>{/if}
                <span class="due-actions">
                  <button class="btn btn-sm btn-outline" onclick={() => take(d)}>Taken</button>
                  <button class="btn btn-sm" onclick={() => skip(d)}>Skip</button>
                  <button class="link" onclick={() => askOther(d)}>At another time</button>
                </span>
              {/if}
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    <!-- The list -->
    <section class="card" aria-labelledby="med-list">
      <h2 class="eyebrow" id="med-list">Medications</h2>
      {#if health.medications.length === 0}
        <p class="quiet">
          Nothing here yet. Add what you take, with when, and the app can remind you and keep a
          record to show your clinician.
        </p>
      {:else}
        <ul class="list">
          {#each current as m (m.id)}
            {@render medRow(m)}
          {/each}
        </ul>
        {#if stopped.length > 0}
          <button class="link toggle" onclick={() => (showStopped = !showStopped)}>
            {showStopped ? 'Hide' : 'Show'} stopped ({stopped.length})
          </button>
          {#if showStopped}
            <ul class="list">
              {#each stopped as m (m.id)}
                {@render medRow(m)}
              {/each}
            </ul>
          {/if}
        {/if}
      {/if}
    </section>

    <!-- Timing notes -->
    {#if alerts.length > 0}
      <section class="card" aria-labelledby="med-alerts">
        <h2 class="eyebrow" id="med-alerts">Timing notes</h2>
        <ul class="notes">
          {#each alerts as a (a.medicationId + a.at + a.message)}
            <li>
              <Icon name="info" size={14} />
              <span class="note-text">
                {a.message}
                <span class="note-when">
                  {nameOf(a.medicationId)} · {friendlyDate(isoDate(new Date(a.at)))}, {formatInstantTime(
                    a.at,
                  )}
                </span>
              </span>
            </li>
          {/each}
        </ul>
        <p class="foot">
          These are notes after the fact, not alarms. Your timing rules are yours to set with your
          clinician. This is not medical advice.
        </p>
      </section>
    {/if}

    <!-- History -->
    <section class="card" aria-labelledby="med-history">
      <h2 class="eyebrow" id="med-history">The last 30 days</h2>
      {#if history.length === 0}
        <p class="quiet">No doses logged in the last 30 days.</p>
      {:else}
        {#each shownHistory as group (group.day)}
          <div class="day">
            <h3 class="day-name">{friendlyDate(group.day)}</h3>
            <ul class="doses">
              {#each group.doses as d (d.id)}
                <li class="dose">
                  <span class="dose-time">{formatInstantTime(d.at)}</span>
                  <span class="dose-name">{nameOf(d.medicationId)}</span>
                  <span class="dose-status" class:off={d.status !== 'taken'}>
                    {DOSE_WORDS[d.status]}
                  </span>
                  {#if d.note}<span class="dose-note">{d.note}</span>{/if}
                  <span class="grow"></span>
                  <button
                    class="icon-btn danger"
                    aria-label="Delete this {nameOf(d.medicationId)} entry"
                    title="Delete a mistaken entry"
                    onclick={() => (deleting = d)}
                  >
                    <Icon name="trash" size={13} />
                  </button>
                </li>
              {/each}
            </ul>
          </div>
        {/each}
        {#if history.length > 7}
          <button class="link toggle" onclick={() => (allHistory = !allHistory)}>
            {allHistory ? 'Show fewer days' : `Show all ${history.length} days`}
          </button>
        {/if}
      {/if}
    </section>

    <HealthAllergies />
  </div>
</div>

{#if deleting}
  <ConfirmDialog
    title="Delete this dose entry?"
    detail="{nameOf(deleting.medicationId)}, {friendlyDate(
      isoDate(new Date(deleting.at)),
    )} at {formatInstantTime(deleting.at)}. Use this for an entry made by mistake."
    onconfirm={() => {
      const doomed = deleting
      deleting = null
      if (doomed) void health.deleteDose(doomed)
    }}
    oncancel={() => (deleting = null)}
  />
{/if}

{#if health.editingMedication}
  {#key health.editingMedication.id}
    <HealthMedicationEditor />
  {/key}
{/if}

<style>
  .meds {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .page {
    max-width: 860px;
    margin: 0 auto;
    padding: var(--sp-2) var(--sp-6) var(--fab-clear);
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  .bar {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }
  .grow {
    flex: 1;
  }

  .card {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg);
  }

  .quiet,
  .foot {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }
  .foot {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .link {
    color: var(--accent);
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .link:hover {
    text-decoration: underline;
  }
  .toggle {
    align-self: flex-start;
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }

  /* Doses due */
  .dues {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .due {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--sp-2) var(--sp-3);
    min-height: 42px;
    padding: var(--sp-1) 0;
  }
  .due + .due {
    border-top: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }
  .due-time {
    flex: none;
    width: 5.5em;
    font-size: var(--text-sm);
    color: var(--fg-muted);
    font-variant-numeric: tabular-nums;
  }
  .due-name {
    flex: 1 1 140px;
    min-width: 0;
    font-weight: 600;
    color: var(--fg);
  }
  .overdue {
    font-size: var(--text-xs);
    font-weight: 650;
    color: var(--danger);
  }
  .due-actions {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .due-done {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .other {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .field.small {
    width: auto;
    height: 28px;
    font-size: var(--text-sm);
  }

  /* The list */
  .list {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
  }
  .med {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 15rem);
    align-items: center;
    gap: var(--sp-2) var(--sp-4);
    padding: var(--sp-1) 0;
  }
  .med + .med {
    border-top: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }
  .med.off .med-main {
    opacity: 0.7;
  }
  .med-main {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: var(--sp-2) var(--sp-3);
  }
  .med-top {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: var(--sp-2);
    min-width: 0;
  }
  .med-name {
    font-size: var(--text-base);
    font-weight: 620;
    color: var(--fg);
  }
  .med-strength {
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .med-sub {
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-1);
  }
  .med-side {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--sp-1);
    min-width: 0;
    padding-right: var(--sp-2);
  }
  .side-line {
    margin: 0;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .side-line.low {
    color: var(--danger);
    font-weight: 600;
  }
  @media (max-width: 640px) {
    .med {
      grid-template-columns: minmax(0, 1fr);
    }
    .med-side {
      padding-left: calc(var(--sp-2) + var(--sp-3));
    }
  }

  /* Timing notes */
  .notes {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .notes li {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    color: var(--fg-muted);
  }
  .notes :global(svg) {
    flex: none;
    margin-top: 2px;
  }
  .note-text {
    display: flex;
    flex-direction: column;
    gap: 1px;
    font-size: var(--text-sm);
    color: var(--fg);
  }
  .note-when {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }

  /* History */
  .day {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .day + .day {
    margin-top: var(--sp-2);
  }
  .day-name {
    font-size: var(--text-xs);
    font-weight: 650;
    color: var(--fg-subtle);
  }
  .doses {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .dose {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-height: 32px;
    font-size: var(--text-sm);
  }
  .dose-time {
    flex: none;
    width: 5.5em;
    color: var(--fg-muted);
    font-variant-numeric: tabular-nums;
  }
  .dose-name {
    color: var(--fg);
    font-weight: 550;
  }
  .dose-status {
    color: var(--fg-muted);
  }
  .dose-status.off {
    font-weight: 600;
  }
  .dose-note {
    color: var(--fg-subtle);
    font-size: var(--text-xs);
  }
  .icon-btn {
    flex: none;
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: 50%;
    color: var(--fg-faint);
  }
  .icon-btn.danger:hover {
    background: color-mix(in oklab, var(--danger) 12%, transparent);
    color: var(--danger);
  }
</style>
