<script lang="ts">
  // A row of chips the quick model proposed, and the one way to accept them.
  //
  // Every suggestion in this application is drawn by this component, so the
  // interaction is the same wherever it appears: a row that arrives *after*
  // the thing it is about was saved, one tap to take a suggestion, one to
  // close the row, and no state of the application waiting on any of it.
  //
  // It is deliberately not a dialog, not a toast and not an inline
  // replacement of a field. A dialog would make a suggestion something to
  // dismiss before carrying on; a toast would time out and take an answer
  // with it; and a field that fills itself is the thing the whole feature is
  // written to avoid -- what you typed has to survive being disagreed with.

  import Icon from './Icon.svelte'

  let {
    items = [],
    /** Drawn small and grey before the chips, e.g. "Also worth tracking". */
    label = '',
    /** Whether a request is in flight, so the row can say so rather than pop. */
    busy = false,
    onaccept,
    ondismiss,
  }: {
    items?: { key: string; label: string }[]
    label?: string
    busy?: boolean
    onaccept: (key: string) => void
    ondismiss: () => void
  } = $props()

  /**
   * Which chips have been taken, so they can leave without the parent having
   * to rebuild the list.
   *
   * Held here rather than by removing from `items` upstream because accepting
   * one of four suggestions should not redraw the other three -- and because
   * the parent's list is usually the answer it was given, which it should be
   * free to keep for as long as the row is open.
   */
  let taken = $state<Set<string>>(new Set())
  const left = $derived(items.filter((i) => !taken.has(i.key)))

  function accept(key: string) {
    taken = new Set([...taken, key])
    onaccept(key)
    // The last one taken closes the row: there is nothing left to look at,
    // and an empty row with a close button is furniture.
    if (taken.size >= items.length) ondismiss()
  }
</script>

{#if busy || left.length > 0}
  <div class="suggestions" class:busy>
    {#if label}<span class="label">{label}</span>{/if}
    {#if busy}
      <span class="thinking">Looking…</span>
    {:else}
      {#each left as item (item.key)}
        <button class="chip" onclick={() => accept(item.key)}>
          <Icon name="sparkle" size={12} />
          {item.label}
        </button>
      {/each}
      <button class="close" onclick={ondismiss} aria-label="Dismiss these suggestions">
        <Icon name="close" size={12} />
      </button>
    {/if}
  </div>
{/if}

<style>
  .suggestions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-2) 0;
    font-size: var(--text-xs);
  }

  .label,
  .thinking {
    color: var(--fg-subtle);
  }

  /* The row is drawn in the accent at low opacity rather than in a colour of
     its own. A suggestion is not a status: it is not a warning, it did not
     succeed at anything, and giving it a semantic colour would make three of
     them in a sidebar look like an error state. */
  .chip {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-1);
    padding: 3px var(--sp-2);
    border: 1px solid color-mix(in oklab, var(--accent) 30%, transparent);
    border-radius: 999px;
    background: color-mix(in oklab, var(--accent) 8%, transparent);
    color: var(--fg);
    font: inherit;
    cursor: pointer;
  }

  .chip:hover {
    background: color-mix(in oklab, var(--accent) 16%, transparent);
  }

  .close {
    display: inline-flex;
    padding: 3px;
    border: 0;
    border-radius: 999px;
    background: none;
    color: var(--fg-faint);
    cursor: pointer;
  }

  .close:hover {
    background: var(--bg-hover);
    color: var(--fg-muted);
  }
</style>
