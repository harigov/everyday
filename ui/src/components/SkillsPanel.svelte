<script lang="ts">
  // Processes the assistant follows for a certain kind of request. Written
  // here by hand, or written by the assistant itself and left as a
  // proposal for a yes or a no -- see `AssistantPanes.svelte`'s "Waiting
  // for you" pane and `SkillProposalPreview.svelte` for that path. Either
  // way this is where every skill ends up, and where one already switched
  // off is turned back on: the assistant may create, change and delete a
  // skill, but never re-enable one, which is why `editing.enabled` here has
  // no counterpart in what it can ask for. See
  // `everyday_core::agent::Skill`'s module docs, "Skills, and progressive
  // disclosure". A skill's name and description are the only part that
  // reaches every conversation, as an index the assistant is told to load
  // from before it acts on a matching request; the instructions themselves
  // are read in full only when it actually does, through `read_skill`.
  //
  // The shape here is Routines', not Memory's: a skill is a short form with
  // several fields rather than one sentence typed and blurred, so it gets an
  // explicit editor with a Save and a Cancel the same way a routine does,
  // not an autosaving line.

  import { onDestroy } from 'svelte'
  import { api } from '../lib/api'
  import { notify } from '../lib/notify.svelte'
  import { proposals } from '../lib/proposals.svelte'
  import type { Skill, SkillId } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'

  let skills = $state<Skill[]>([])
  let loading = $state(true)
  let editing = $state<Skill | null>(null)
  let pendingDelete = $state<{ id: SkillId; name: string } | null>(null)

  async function load() {
    loading = true
    try {
      skills = await api.skills()
    } catch (e) {
      notify.error(e instanceof Error ? e.message : String(e))
    } finally {
      loading = false
    }
  }
  void load()

  // A skill proposal accepted from "Waiting for you" -- a different tab of
  // Settings, open in the same window -- does not come back as a change
  // event this window raises itself; see `onAccepted` on
  // `proposals.svelte.ts`. This is the one list that record went into, so
  // it is the one store this view has to reload for itself.
  const unsubscribeSkillAccepted = proposals.onAccepted('skill', () => load())
  onDestroy(unsubscribeSkillAccepted)

  async function draft() {
    try {
      editing = await api.newSkill()
    } catch (e) {
      notify.error(e instanceof Error ? e.message : String(e))
    }
  }

  function edit(skill: Skill) {
    editing = { ...skill }
  }

  function cancelEdit() {
    editing = null
  }

  async function save() {
    const current = editing
    if (!current) return
    try {
      await api.saveSkill($state.snapshot(current))
      editing = null
      await load()
    } catch (e) {
      notify.error(e instanceof Error ? e.message : String(e))
    }
  }

  async function toggle(skill: Skill) {
    try {
      await api.saveSkill({ ...$state.snapshot(skill), enabled: !skill.enabled })
      await load()
    } catch (e) {
      notify.error(e instanceof Error ? e.message : String(e))
    }
  }

  async function remove() {
    const target = pendingDelete
    pendingDelete = null
    if (!target) return
    try {
      await api.deleteSkill(target.id)
      if (editing?.id === target.id) editing = null
      await load()
    } catch (e) {
      notify.error(e instanceof Error ? e.message : String(e))
    }
  }
</script>

<div class="panel">
  {#if !editing}
    <header class="head">
      {#if loading}<span class="dim">Loading…</span>{/if}
      <span class="spacer"></span>
      <button class="btn btn-primary" onclick={() => void draft()}>New skill</button>
    </header>
  {/if}

  <div class="body">
    {#if editing}
      <section class="editor">
        <label class="field-row">
          <span class="eyebrow">Name</span>
          <input class="field" placeholder="Plan a trip" bind:value={editing.name} />
        </label>

        <label class="field-row">
          <span class="eyebrow">When should the assistant use this?</span>
          <input
            class="field"
            placeholder="Use when asked to plan a trip or a multi-day journey."
            bind:value={editing.description}
          />
          <span class="hint">
            One or two sentences. This is what the assistant is told about every skill, so it has to
            say when it applies rather than what it does.
          </span>
        </label>

        <label class="field-row">
          <span class="eyebrow">Instructions</span>
          <textarea
            class="field area"
            rows="10"
            placeholder={'Check the calendar for the travel dates and flag any conflicts.\n' +
              'Check the weather for the destination over those dates.\n' +
              'Propose blocks of time for packing and for travel itself.\n' +
              'Draft a task with a packing list suited to the weather and the trip.'}
            bind:value={editing.instructions}
          ></textarea>
          <span class="hint">
            The process itself, in as much detail as it takes. Only read when this skill is actually
            used, so it costs nothing on the turns that do not need it.
          </span>
        </label>

        <label class="toggle">
          <input type="checkbox" bind:checked={editing.enabled} />
          <span>
            <b>Switched on</b>
            <small>
              Off keeps it here without offering it — for a process written for an occasion that has
              passed.
            </small>
          </span>
        </label>

        <footer class="editorfoot">
          {#if skills.some((s) => s.id === editing?.id)}
            <button
              class="btn btn-danger"
              onclick={() => editing && (pendingDelete = { id: editing.id, name: editing.name })}
            >
              Delete
            </button>
          {/if}
          <span class="spacer"></span>
          <button class="btn" onclick={cancelEdit}>Cancel</button>
          <button
            class="btn btn-primary"
            disabled={!editing.name.trim() ||
              !editing.description.trim() ||
              !editing.instructions.trim()}
            onclick={() => void save()}
          >
            Save
          </button>
        </footer>
      </section>
    {:else if skills.length === 0 && !loading}
      <EmptyState lead="It has no skills yet">
        {#snippet icon()}<Icon name="book" size={34} weight={1.4} />{/snippet}
        {#snippet note()}
          A skill is a process to follow for a certain kind of request. "Plan a trip", for instance:
          check the calendar for conflicts, check the weather, propose blocks of time, and draft a
          packing-list task — written once, used whenever you ask.
        {/snippet}
        {#snippet action()}
          <button class="btn btn-primary" onclick={() => void draft()}>Add a skill</button>
        {/snippet}
      </EmptyState>
    {:else}
      <!-- Accounts and Memory both open with a sentence saying what the list
           below is; a populated Skills list had none. -->
      <p class="lead">
        A skill is a process to follow for a certain kind of request, written once and used whenever
        it applies.
      </p>
      {#each skills as skill (skill.id)}
        <article class="card" class:off={!skill.enabled}>
          <header class="cardhead">
            <span class="name">{skill.name}</span>
            <span class="spacer"></span>
          </header>
          {#if skill.description}<p class="said">{skill.description}</p>{/if}
          <footer class="cardfoot">
            <button class="link" onclick={() => edit(skill)}>Edit</button>
            <button class="link" onclick={() => void toggle(skill)}>
              {skill.enabled ? 'Switch off' : 'Switch on'}
            </button>
            <button
              class="link"
              onclick={() => (pendingDelete = { id: skill.id, name: skill.name })}
            >
              Delete
            </button>
          </footer>
        </article>
      {/each}
    {/if}
  </div>
</div>

{#if pendingDelete}
  <ConfirmDialog
    title="Delete this skill?"
    detail={'“' + pendingDelete.name + '” will be removed. This cannot be undone.'}
    confirmLabel="Delete"
    onconfirm={remove}
    oncancel={() => (pendingDelete = null)}
  />
{/if}

<style>
  .panel {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }

  .head {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }

  .spacer {
    flex: 1;
  }

  .dim {
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }

  .lead {
    color: var(--fg-muted);
    font-size: var(--text-sm);
    line-height: 1.55;
  }

  .body {
    display: grid;
    align-content: start;
    gap: var(--sp-3);
    min-width: 0;
  }

  .card {
    display: grid;
    gap: var(--sp-2);
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
  }

  .card.off {
    opacity: 0.6;
  }

  .cardhead,
  .cardfoot {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }

  .name {
    font-weight: 600;
  }

  .said {
    color: var(--fg-muted);
    font-size: var(--text-sm);
    line-height: 1.55;
  }

  .link {
    color: var(--accent);
    font-size: var(--text-sm);
  }

  .link:hover {
    text-decoration: underline;
  }

  .editor {
    display: grid;
    gap: var(--sp-4);
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
  }

  .field-row {
    display: grid;
    gap: var(--sp-2);
  }

  .area {
    resize: vertical;
    font-family: inherit;
    line-height: 1.5;
    white-space: pre-wrap;
  }

  .hint {
    color: var(--fg-faint);
    font-size: var(--text-xs);
    line-height: 1.5;
  }

  .toggle {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
  }

  .toggle small {
    display: block;
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }

  .editorfoot {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }
</style>
