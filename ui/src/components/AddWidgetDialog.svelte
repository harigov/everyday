<script lang="ts">
  // Everything the Overview could show, as a dialog opened from its Add
  // button, filed under what each card is about.
  //
  // It used to be the Overview's half of the sidebar: a permanent column
  // listing the whole catalogue, spent on every visit on a list somebody
  // reads when rearranging and otherwise never. A dialog costs nothing when
  // it is closed, and when it is open it has the room to lay the catalogue
  // out as tiles rather than a column of rows, each one saying what it
  // answers and how wide it arrives.
  //
  // Picking a tile does not add it. It hands the choice back to the page,
  // which then asks where on the page it goes -- see `placing` in
  // `OverviewView`.

  import { GROUPS, SIZE_LABELS, WIDGET_TYPES, specOf, type WidgetType } from '../lib/dashboard'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import { overview } from '../lib/overview.svelte'
  import Icon from './Icon.svelte'

  const { onpick, onclose }: { onpick: (type: WidgetType) => void; onclose: () => void } = $props()

  /** How many of each type are already on the page, so a tile can say so. */
  const counts = $derived.by(() => {
    const out = new Map<WidgetType, number>()
    for (const w of overview.widgets) out.set(w.type, (out.get(w.type) ?? 0) + 1)
    return out
  })

  const byGroup = $derived(
    GROUPS.map((group) => ({
      group,
      types: WIDGET_TYPES.filter((t) => specOf(t).group === group),
    })).filter((g) => g.types.length > 0),
  )
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape') onclose()
  }}
/>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="scrim" onclick={onclose}></div>
<div
  class="sheet picker"
  role="dialog"
  aria-modal="true"
  aria-label="Add to the Overview"
  use:trapFocus
>
  <header>
    <div>
      <h2>Add to the Overview</h2>
      <p class="lead">Pick a card, then choose where on the page it goes.</p>
    </div>
    <button class="close" aria-label="Close" use:focusOnMount onclick={onclose}>
      <Icon name="close" size={14} />
    </button>
  </header>

  <div class="scroll groups">
    {#each byGroup as section (section.group)}
      <section>
        <h3 class="eyebrow">{section.group}</h3>
        <div class="tiles">
          {#each section.types as type (type)}
            {@const spec = specOf(type)}
            {@const on = counts.get(type) ?? 0}
            <button class="tile" onclick={() => onpick(type)}>
              <span class="top">
                <span class="icon"><Icon name={spec.icon} size={15} /></span>
                <span class="name">{spec.label}</span>
              </span>
              <span class="note">{spec.note}</span>
              <span class="meta">
                <span>{SIZE_LABELS[spec.size]}</span>
                {#if on > 0}
                  <!-- A count rather than a tick that disables the tile: two
                       heatmaps of two habits is the ordinary case. -->
                  <span class="count">{on} on the page</span>
                {/if}
              </span>
            </button>
          {/each}
        </div>
      </section>
    {/each}
  </div>
</div>

<style>
  /* Wider and taller than a confirmation: this is a catalogue of seventeen
     cards, and a 420px sheet would make it a column of rows again. */
  .picker {
    top: 8%;
    display: flex;
    flex-direction: column;
    width: min(820px, calc(100vw - var(--sp-8)));
    max-height: 84vh;
    padding: 0;
  }

  header {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    padding: var(--sp-4) var(--sp-4) var(--sp-3);
    border-bottom: 1px solid var(--border);
  }
  header > div {
    flex: 1;
  }
  h2 {
    margin: 0;
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
  }
  .lead {
    margin: var(--sp-1) 0 0;
    color: var(--fg-subtle);
    font-size: var(--text-sm);
  }
  .close {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: var(--radius-sm);
    color: var(--fg-subtle);
  }
  .close:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .groups {
    flex: 1;
    min-height: 0;
    padding: var(--sp-2) var(--sp-4) var(--sp-4);
  }
  h3 {
    margin: var(--sp-4) 0 var(--sp-2);
  }

  .tiles {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
    gap: var(--sp-2);
  }

  .tile {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    text-align: left;
    transition:
      border-color var(--fast) var(--ease),
      background var(--fast) var(--ease);
  }
  .tile:hover,
  .tile:focus-visible {
    border-color: var(--accent);
    background: var(--bg-hover);
  }

  .top {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .icon {
    display: grid;
    flex: none;
    place-items: center;
    color: var(--fg-faint);
  }
  .name {
    font-size: var(--text-base);
    font-weight: 600;
    color: var(--fg);
  }
  .note {
    flex: 1;
    color: var(--fg-subtle);
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
  }
  .meta {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin-top: var(--sp-1);
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }
  .count {
    padding: 0 6px;
    border-radius: 999px;
    background: var(--bg-active);
    color: var(--fg-muted);
    font-weight: 600;
  }
</style>
