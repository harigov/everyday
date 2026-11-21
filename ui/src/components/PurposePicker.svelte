<script lang="ts">
  // Choose what something is for: a goal, or an area directly.
  //
  // One control, used from a task's detail panel, a project's settings, a
  // block, an entry, a shelf item and the calendar's subscription sheet. The
  // list is short — a handful of areas with a few goals under each — so it
  // is drawn whole rather than searched, and the typed line is there to
  // *create* rather than to filter: the commonest reason to open this and
  // not find what you want is that the goal does not exist yet, and making
  // somebody leave for the Overview to add it is how a field stops being
  // used.
  //
  // Clearing is a first-class row and not a hidden gesture. Purpose is
  // optional everywhere, so "actually, nothing" has to be as easy to say as
  // anything else.

  import Icon from './Icon.svelte'
  import { purpose as store, samePurpose } from '../lib/purpose.svelte'
  import type { Purpose, AreaId } from '../lib/types'

  interface Props {
    value: Purpose | null | undefined
    onchange: (next: Purpose | null) => void
    /** Close the popover. The host owns whether it is open. */
    onclose?: () => void
    /**
     * Offer areas only, no goals. What a subscribed calendar takes: a feed
     * serves an area, and its forty meetings are not each yours to file.
     */
    areasOnly?: boolean
  }

  const { value, onchange, onclose, areasOnly = false }: Props = $props()

  let draft = $state('')
  /** Which area a newly typed goal would go under. */
  let under = $state<AreaId | null>(null)
  let busy = $state(false)

  // Loaded on open rather than at startup: a vault whose owner never files
  // anything under a goal never pays for the two queries.
  $effect(() => {
    void store.load()
  })

  const areas = $derived(store.activeAreas)
  const typed = $derived(draft.trim())

  // The area a new goal lands under: the one explicitly chosen, else the one
  // the current value already belongs to, else the first. Never nothing —
  // a goal must have an area, and a picker that could not say which would
  // have to refuse the Enter key.
  const target = $derived(under ?? store.areaOf(value) ?? areas[0]?.id ?? null)

  function choose(next: Purpose | null) {
    onchange(next)
    onclose?.()
  }

  async function createGoal() {
    if (!typed || !target || busy) return
    busy = true
    const goal = await store.addGoal(target, typed)
    busy = false
    if (!goal) return
    draft = ''
    choose({ type: 'goal', id: goal.id })
  }

  async function createArea() {
    if (!typed || busy) return
    busy = true
    const area = await store.addArea(typed)
    busy = false
    if (!area) return
    draft = ''
    choose({ type: 'area', id: area.id })
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      e.preventDefault()
      void (areasOnly ? createArea() : createGoal())
    } else if (e.key === 'Escape') {
      e.preventDefault()
      onclose?.()
    }
  }
</script>

<div class="picker">
  <div class="rows" role="listbox" aria-label="What this is for">
    <!-- Always first, always available. Purpose is optional everywhere. -->
    <button
      class="row none"
      class:on={!value}
      role="option"
      aria-selected={!value}
      onclick={() => choose(null)}
    >
      <span class="swatch none"></span>
      <span class="name">Nothing in particular</span>
      {#if !value}<Icon name="check" size={13} />{/if}
    </button>

    {#each areas as area (area.id)}
      {@const chosen = samePurpose(value, { type: 'area', id: area.id })}
      <div class="group">
        <button
          class="row area"
          class:on={chosen}
          role="option"
          aria-selected={chosen}
          onclick={() => choose({ type: 'area', id: area.id })}
        >
          <span class="swatch" style="background: {area.color}"></span>
          <span class="glyph">{area.icon}</span>
          <span class="name">{area.name}</span>
          {#if chosen}<Icon name="check" size={13} />{/if}
        </button>

        {#if !areasOnly}
          {#each store
            .goalsOf(area.id)
            .filter((g) => g.status === 'active' || g.status === 'paused') as goal (goal.id)}
            {@const picked = samePurpose(value, { type: 'goal', id: goal.id })}
            <button
              class="row goal"
              class:on={picked}
              role="option"
              aria-selected={picked}
              onclick={() => choose({ type: 'goal', id: goal.id })}
            >
              <span class="rail" style="background: {area.color}"></span>
              <Icon name="target" size={13} />
              <span class="name" class:paused={goal.status === 'paused'}>{goal.title}</span>
              {#if picked}<Icon name="check" size={13} />{/if}
            </button>
          {/each}
        {/if}
      </div>
    {/each}

    {#if areas.length === 0 && !store.loading}
      <p class="blank-note">
        No areas yet. Type a name below to make one — an area is a part of your life, like
        <em>Family</em> or <em>Work</em>.
      </p>
    {/if}
  </div>

  <div class="new">
    <!-- Focused on open: this popover exists to be typed into, and the rows
         above it are reachable with Tab and the arrow keys. -->
    <!-- svelte-ignore a11y_autofocus -->
    <input
      autofocus
      bind:value={draft}
      {onkeydown}
      placeholder={areasOnly || areas.length === 0 ? 'New area…' : 'New goal…'}
      aria-label={areasOnly || areas.length === 0 ? 'New area' : 'New goal'}
    />
    {#if !areasOnly && areas.length > 0}
      <!-- Which area the typed goal lands under. Shown only when there is a
           choice to make: with one area the answer is not interesting. -->
      {#if areas.length > 1}
        <select bind:value={under} aria-label="Under which area">
          {#each areas as area (area.id)}
            <option value={area.id} selected={area.id === target}>{area.name}</option>
          {/each}
        </select>
      {/if}
      <button
        class="go"
        disabled={!typed || busy || !target}
        onclick={() => void createGoal()}
        title="Add this goal (Enter)"
      >
        <Icon name="plus" size={14} />
      </button>
    {:else}
      <button
        class="go"
        disabled={!typed || busy}
        onclick={() => void createArea()}
        title="Add this area (Enter)"
      >
        <Icon name="plus" size={14} />
      </button>
    {/if}
  </div>
</div>

<style>
  .picker {
    display: flex;
    flex-direction: column;
    min-width: 260px;
    max-width: 320px;
  }

  .rows {
    max-height: 320px;
    overflow-y: auto;
    padding: var(--sp-1);
  }

  .group + .group {
    margin-top: 2px;
  }

  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    min-height: var(--row-h);
    padding: 0 var(--sp-2);
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    text-align: left;
    cursor: pointer;
  }

  .row:hover,
  .row:focus-visible {
    background: var(--bg-sunken);
  }

  .row.on {
    font-weight: 550;
  }

  .row.goal {
    padding-left: var(--sp-3);
    color: var(--fg-muted);
  }

  .row.goal.on {
    color: var(--fg);
  }

  .name {
    flex: 1 1 auto;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* A paused goal is still offered — it is on the books — but it should not
     read as the same thing as one you are working on this month. */
  .name.paused {
    opacity: 0.6;
  }

  .swatch {
    flex: none;
    width: 9px;
    height: 9px;
    border-radius: 50%;
  }

  .swatch.none {
    border: 1px solid var(--border);
    background: none;
  }

  /* The goal rows hang off their area's colour rather than repeating the
     dot, so a column of six goals does not read as six separate things. */
  .rail {
    flex: none;
    width: 2px;
    height: 16px;
    border-radius: 1px;
    opacity: 0.5;
  }

  .glyph {
    flex: none;
    font-size: var(--text-sm);
    line-height: 1;
  }

  .new {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    padding: var(--sp-2);
    border-top: 1px solid var(--border);
  }

  .new input {
    flex: 1 1 auto;
    min-width: 0;
    height: 28px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
  }

  .new select {
    flex: none;
    max-width: 96px;
    height: 28px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    color: var(--fg-muted);
    font: inherit;
    font-size: var(--text-xs);
  }

  .go {
    display: grid;
    flex: none;
    place-items: center;
    width: 28px;
    height: 28px;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--fg-muted);
    cursor: pointer;
  }

  .go:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .go:not(:disabled):hover {
    background: var(--bg-sunken);
    color: var(--fg);
  }

  .blank-note {
    margin: 0;
    padding: var(--sp-3) var(--sp-2);
  }
</style>
