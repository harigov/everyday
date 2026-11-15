<script lang="ts">
  // Conditions: the long-running things a person is looking after, each
  // with its own readings, targets, tests and medicines.
  //
  // With none open this is the list, and the way to add one: from a
  // template, which fills in a plan worth reviewing, or blank, for anything
  // a template does not cover. With one open it is that condition's
  // dashboard, with a way back.
  //
  // A condition is the person's record, not a diagnosis the app makes. The
  // name, status and dates are theirs; a template only ever offers a
  // starting plan.

  import { health } from '../lib/health.svelte'
  import { dateFormat } from '../lib/format'
  import { startOfDay, todayIso } from '../lib/time'
  import type { Condition, ConditionStatus } from '../lib/types'
  import EmptyState from './EmptyState.svelte'
  import HealthConditionDashboard from './HealthConditionDashboard.svelte'
  import HealthTemplatePicker from './HealthTemplatePicker.svelte'
  import Icon from './Icon.svelte'

  const STATUSES: { value: ConditionStatus; label: string }[] = [
    { value: 'active', label: 'Active' },
    { value: 'remission', label: 'In remission' },
    { value: 'resolved', label: 'Resolved' },
  ]
  const statusName = (s: ConditionStatus) => STATUSES.find((x) => x.value === s)?.label ?? s

  const open = $derived(health.conditionId)

  // The open condition going away -- deleted from its own dashboard -- sends
  // the pane back to the list. Only once it has been seen, so a condition
  // that is still being read in does not bounce straight back.
  let seen: string | null = null
  $effect(() => {
    const id = health.conditionId
    if (!id) return
    if (health.conditions.some((c) => c.id === id)) seen = id
    else if (seen === id) health.openCondition(null)
  })

  const list = $derived(
    [...health.conditions].sort((a, b) =>
      a.status === b.status
        ? a.name.localeCompare(b.name)
        : a.status === 'active'
          ? -1
          : b.status === 'active'
            ? 1
            : a.status.localeCompare(b.status),
    ),
  )

  function since(c: Condition): string {
    return c.diagnosed
      ? `since ${dateFormat({ month: 'long', year: 'numeric' }).format(startOfDay(c.diagnosed))}`
      : ''
  }
  const templateName = (c: Condition) =>
    c.template ? (health.templates.find((t) => t.key === c.template)?.name ?? '') : ''
  const medCount = (c: Condition) =>
    health.medications.filter((m) => m.conditionIds.includes(c.id) && m.status !== 'stopped').length

  // ── Adding ──────────────────────────────────────────────────────────────

  let adding = $state<'template' | 'blank' | null>(null)
  let name = $state('')
  let status = $state<ConditionStatus>('active')
  let diagnosed = $state('')
  let notes = $state('')
  let formError = $state('')
  let saving = $state(false)

  function startBlank() {
    name = ''
    status = 'active'
    diagnosed = ''
    notes = ''
    formError = ''
    adding = 'blank'
  }

  async function saveBlank() {
    if (!name.trim()) {
      formError = 'Give the condition a name.'
      return
    }
    const now = new Date().toISOString()
    saving = true
    try {
      const made = await health.saveCondition({
        id: crypto.randomUUID(),
        name: name.trim(),
        template: null,
        status,
        diagnosed: diagnosed || null,
        codes: [],
        plan: {
          watch: [],
          targets: [],
          labs: [],
          medRules: {},
          trackerIds: [],
          templateVersion: 0,
        },
        notes: notes.trim(),
        createdAt: now,
        updatedAt: now,
      })
      if (made) {
        adding = null
        health.openCondition(made.id)
      } else {
        formError = 'The condition could not be saved. Try again.'
      }
    } finally {
      saving = false
    }
  }
</script>

<div class="conditions scroll">
  <div class="page">
    {#if open}
      <div class="back-row">
        <button class="back" onclick={() => health.openCondition(null)}>
          <span class="flip"><Icon name="chevron" size={13} /></span>
          All conditions
        </button>
      </div>
      <HealthConditionDashboard />
    {:else if adding === 'template'}
      <HealthTemplatePicker onclose={() => (adding = null)} ondone={() => (adding = null)} />
    {:else if adding === 'blank'}
      <form
        class="blank card"
        onsubmit={(e) => {
          e.preventDefault()
          void saveBlank()
        }}
      >
        <h2 class="eyebrow">A condition of your own</h2>
        <div class="grid">
          <div class="span2">
            <label class="field-label" for="cond-name">Name</label>
            <input
              id="cond-name"
              class="field"
              bind:value={name}
              placeholder="High blood pressure, asthma, migraine…"
              autocomplete="off"
            />
          </div>
          <div>
            <label class="field-label" for="cond-status">Status</label>
            <select id="cond-status" class="field" bind:value={status}>
              {#each STATUSES as s (s.value)}<option value={s.value}>{s.label}</option>{/each}
            </select>
          </div>
          <div>
            <label class="field-label" for="cond-diagnosed">Diagnosed (optional)</label>
            <input
              id="cond-diagnosed"
              class="field"
              type="date"
              max={todayIso()}
              bind:value={diagnosed}
            />
          </div>
          <div class="span2">
            <label class="field-label" for="cond-notes">Notes (optional)</label>
            <textarea id="cond-notes" class="field" rows="3" bind:value={notes}></textarea>
          </div>
        </div>
        <p class="hint small">
          You can add targets, tests and medicines to it afterwards, and link medications to it from
          their own page.
        </p>
        {#if formError}<p class="error" role="alert">{formError}</p>{/if}
        <div class="row">
          <span class="grow"></span>
          <button type="button" class="btn" onclick={() => (adding = null)}>Cancel</button>
          <button type="submit" class="btn btn-primary" disabled={saving}>Add condition</button>
        </div>
      </form>
    {:else}
      <div class="bar">
        <p class="hint grow">
          What you are looking after, with its readings, tests and medicines in one place.
        </p>
        <button class="btn btn-primary" onclick={() => (adding = 'template')}>
          <Icon name="plus" size={14} /> Add a condition
        </button>
        <button class="btn btn-outline" onclick={startBlank}>A blank one</button>
      </div>

      {#if list.length === 0}
        <EmptyState lead="No conditions yet.">
          {#snippet note()}
            Add one from a template to start with a sensible plan — for type 1 or type 2 diabetes,
            or an underactive thyroid — or write your own. Nothing is added until you have reviewed
            it.
          {/snippet}
        </EmptyState>
      {:else}
        <ul class="list">
          {#each list as c (c.id)}
            <li>
              <button class="list-row row-main" onclick={() => health.openCondition(c.id)}>
                <span class="top">
                  <span class="name">{c.name}</span>
                  {#if c.status !== 'active'}<span class="tag">{statusName(c.status)}</span>{/if}
                </span>
                <span class="sub">
                  {[
                    templateName(c),
                    since(c),
                    medCount(c) > 0 ? `${medCount(c)} medicine${medCount(c) === 1 ? '' : 's'}` : '',
                  ]
                    .filter(Boolean)
                    .join(' · ') || 'No plan yet'}
                </span>
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    {/if}
  </div>
</div>

<style>
  .conditions {
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
  .grow {
    flex: 1;
  }

  .bar {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }
  .bar .hint {
    margin: 0;
  }

  .back-row {
    display: flex;
  }
  .back {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-1);
    height: 28px;
    padding: 0 var(--sp-3) 0 var(--sp-2);
    border-radius: 999px;
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-muted);
  }
  .back:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .flip {
    display: grid;
    place-items: center;
    rotate: 180deg;
  }

  .list {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
  }
  .row-main {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: var(--sp-3) var(--sp-4);
  }
  .top {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .name {
    font-size: var(--text-md);
    font-weight: 620;
    color: var(--fg);
  }
  .sub {
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }

  .card {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--sp-3);
  }
  .span2 {
    grid-column: 1 / -1;
  }
  .field-label {
    margin-bottom: var(--sp-1);
  }
  textarea.field {
    resize: vertical;
  }
  .hint.small {
    margin: 0;
    font-size: var(--text-xs);
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
</style>
