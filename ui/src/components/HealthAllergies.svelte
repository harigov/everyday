<script lang="ts">
  // Allergies and intolerances: what, how it shows and how serious it was.
  //
  // A short list that matters most on the day it is needed, so it is kept
  // plain: the substance, its kind, the reaction in the person's own words
  // and a severity written out as a word, never as a colour alone. Added and
  // corrected in place; removing one asks first (the store does the asking).

  import { health } from '../lib/health.svelte'
  import { focusOnMount } from '../lib/focus'
  import { todayIso } from '../lib/time'
  import type { Allergy, AllergyKind, Severity } from '../lib/types'
  import Icon from './Icon.svelte'

  const KINDS: { value: AllergyKind; label: string }[] = [
    { value: 'drug', label: 'Medicine' },
    { value: 'food', label: 'Food' },
    { value: 'environment', label: 'Environment' },
    { value: 'other', label: 'Something else' },
  ]
  const SEVERITIES: { value: Severity; label: string }[] = [
    { value: 'mild', label: 'Mild' },
    { value: 'moderate', label: 'Moderate' },
    { value: 'severe', label: 'Severe' },
  ]
  const kindName = (kind: AllergyKind) => KINDS.find((k) => k.value === kind)?.label ?? kind
  const severityName = (s: Severity) => SEVERITIES.find((v) => v.value === s)?.label ?? s

  const allergies = $derived(health.allergies)

  // ── Editing one ─────────────────────────────────────────────────────────

  /** The allergy being edited; `fresh` is one not saved yet. */
  let editing = $state<string | null>(null)
  let fresh = $state<Allergy | null>(null)
  let substance = $state('')
  let kind = $state<AllergyKind>('drug')
  let reaction = $state('')
  let severity = $state<Severity>('moderate')
  let noted = $state('')
  let formError = $state<string | null>(null)
  let saving = $state(false)

  function edit(allergy: Allergy) {
    editing = allergy.id
    substance = allergy.substance
    kind = allergy.kind
    reaction = allergy.reaction
    severity = allergy.severity
    noted = allergy.noted ?? ''
    formError = null
    if (fresh && fresh.id !== allergy.id) fresh = null
  }

  function startNew() {
    const now = new Date().toISOString()
    const blank: Allergy = {
      id: crypto.randomUUID(),
      substance: '',
      kind: 'drug',
      reaction: '',
      severity: 'moderate',
      noted: null,
      notes: '',
      createdAt: now,
      updatedAt: now,
    }
    fresh = blank
    edit(blank)
  }

  function cancel() {
    editing = null
    fresh = null
  }

  async function save(base: Allergy) {
    if (!substance.trim()) {
      formError = 'Say what it is an allergy to.'
      return
    }
    saving = true
    try {
      const saved = await health.saveAllergy({
        ...$state.snapshot(base),
        substance: substance.trim(),
        kind,
        reaction: reaction.trim(),
        severity,
        noted: noted || null,
        updatedAt: new Date().toISOString(),
      })
      if (saved) {
        editing = null
        fresh = null
      } else {
        formError = 'It could not be saved. Try again.'
      }
    } finally {
      saving = false
    }
  }
</script>

{#snippet editor(allergy: Allergy)}
  <form
    class="editor"
    onsubmit={(e) => {
      e.preventDefault()
      void save(allergy)
    }}
  >
    <div class="grid">
      <label for="al-what">Allergic to</label>
      <input
        id="al-what"
        class="field small"
        bind:value={substance}
        placeholder="Penicillin, peanuts, latex…"
        autocomplete="off"
        use:focusOnMount
      />

      <label for="al-kind">Kind</label>
      <select id="al-kind" class="field small" bind:value={kind}>
        {#each KINDS as k (k.value)}<option value={k.value}>{k.label}</option>{/each}
      </select>

      <label for="al-reaction">What happens</label>
      <input
        id="al-reaction"
        class="field small"
        bind:value={reaction}
        placeholder="Hives, swelling, trouble breathing…"
        autocomplete="off"
      />

      <label for="al-severity">How serious</label>
      <select id="al-severity" class="field small" bind:value={severity}>
        {#each SEVERITIES as s (s.value)}<option value={s.value}>{s.label}</option>{/each}
      </select>

      <label for="al-noted">First noticed</label>
      <input id="al-noted" class="field small" type="date" max={todayIso()} bind:value={noted} />
    </div>

    {#if formError}<p class="error" role="alert">{formError}</p>{/if}

    <div class="editor-row">
      <span class="spacer"></span>
      <button type="button" class="btn" onclick={cancel}>Cancel</button>
      <button type="submit" class="btn btn-primary" disabled={saving}>Save</button>
    </div>
  </form>
{/snippet}

<section class="allergies" aria-labelledby="allergies-h">
  <div class="head">
    <h2 class="eyebrow" id="allergies-h">Allergies</h2>
    <button class="btn btn-outline btn-sm" disabled={!!fresh} onclick={startNew}>
      <Icon name="plus" size={13} /> Add an allergy
    </button>
  </div>

  {#if allergies.length === 0 && !fresh}
    <p class="quiet">No allergies recorded. Add any you know of; they are worth having to hand.</p>
  {/if}

  <ul class="list">
    {#if fresh && editing === fresh.id}
      <li class="row open">{@render editor(fresh)}</li>
    {/if}
    {#each allergies as allergy (allergy.id)}
      <li class="row" class:open={editing === allergy.id}>
        {#if editing === allergy.id}
          {@render editor(allergy)}
        {:else}
          <div class="text">
            <span class="line">
              <span class="name">{allergy.substance}</span>
              <span class="tag">{kindName(allergy.kind)}</span>
              <span class="sev" class:severe={allergy.severity === 'severe'}>
                {#if allergy.severity === 'severe'}<Icon name="alert" size={12} />{/if}
                {severityName(allergy.severity)}
              </span>
            </span>
            {#if allergy.reaction || allergy.noted}
              <span class="sub">
                {allergy.reaction}{allergy.reaction && allergy.noted ? ' · ' : ''}{allergy.noted
                  ? `since ${allergy.noted}`
                  : ''}
              </span>
            {/if}
          </div>
          <div class="actions">
            <button
              class="icon-btn"
              aria-label="Edit {allergy.substance}"
              title="Edit"
              onclick={() => edit(allergy)}
            >
              <Icon name="pencil" size={14} />
            </button>
            <button
              class="icon-btn danger"
              aria-label="Delete {allergy.substance}"
              title="Delete"
              onclick={() => health.deleteAllergy(allergy)}
            >
              <Icon name="trash" size={14} />
            </button>
          </div>
        {/if}
      </li>
    {/each}
  </ul>
</section>

<style>
  .allergies {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
  }

  .quiet {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }

  .list {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-height: 46px;
    padding: var(--sp-2) 0;
  }
  .row + .row {
    border-top: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }
  .row.open {
    display: block;
    padding: var(--sp-3) 0;
  }
  .row:hover .actions,
  .row:focus-within .actions {
    opacity: 1;
  }

  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .line {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }
  .name {
    font-size: var(--text-base);
    font-weight: 600;
    color: var(--fg);
  }
  .sub {
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  /* Written out, and a mark beside the one that matters most. */
  .sev {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--fg-muted);
  }
  .sev.severe {
    color: var(--danger);
  }

  .actions {
    flex: none;
    display: flex;
    gap: 2px;
    opacity: 0.4;
    transition: opacity var(--fast) var(--ease);
  }
  .icon-btn {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .icon-btn:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .icon-btn.danger:hover {
    background: color-mix(in oklab, var(--danger) 12%, transparent);
    color: var(--danger);
  }

  .editor {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .grid {
    display: grid;
    grid-template-columns: 110px minmax(0, 1fr);
    align-items: center;
    gap: var(--sp-2) var(--sp-3);
  }
  .grid > label {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .field.small {
    height: 32px;
    font-size: var(--text-sm);
  }
  select.field {
    padding-right: var(--sp-2);
  }
  .editor-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .spacer {
    flex: 1;
  }

  /* A phone: the edit and delete buttons are always there at a finger's
     size (nothing waits for a hover), and the form's labels sit over their
     fields rather than beside them in a column too narrow for either. */
  :global([data-phone]) .actions {
    opacity: 1;
    gap: 0;
  }
  :global([data-phone]) .icon-btn {
    width: 40px;
    height: 40px;
  }
  :global([data-phone]) .grid {
    grid-template-columns: minmax(0, 1fr);
    gap: var(--sp-1);
  }
  :global([data-phone]) .grid > label:not(:first-child) {
    margin-top: var(--sp-2);
  }
  :global([data-phone]) .field.small {
    height: 40px;
  }
  :global([data-phone]) .editor-row {
    flex-wrap: wrap;
    justify-content: flex-end;
  }
</style>
