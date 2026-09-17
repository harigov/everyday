<script lang="ts">
  // One proposal, drawn where its record would be.
  //
  // The one row style every app uses for work the assistant prepared and did
  // not do: a dotted outline, faded, the word "proposed", the reason on hover,
  // and two answers on the row itself. What the record *looks like* is the
  // caller's business -- pass `children` to draw a task with the task row's
  // own formatting; leave it out and the caption is drawn. See
  // docs/plans/dreaming.md.

  import type { Snippet } from 'svelte'
  import { DECLINE_REASONS, proposals } from '../lib/proposals.svelte'
  import type { DeclineReason, Proposal } from '../lib/types'
  import Icon from './Icon.svelte'

  let {
    proposal,
    compact = false,
    color = null,
    onopen,
    onaccepted,
    children,
  }: {
    proposal: Proposal
    /** Buttons only, no reason line -- for a block on the time grid. */
    compact?: boolean
    /** The record's own colour, drawn faded. */
    color?: string | null
    /** Open the record in its app's own editor. Saving there is accept-with-edits. */
    onopen?: () => void
    onaccepted?: (p: Proposal) => void
    children?: Snippet
  } = $props()

  let choosing = $state(false)
  const busy = $derived(proposals.isBusy(proposal.id))

  async function accept(e: Event) {
    e.stopPropagation()
    const closed = await proposals.accept(proposal)
    if (closed) onaccepted?.(closed)
  }

  async function decline(e: Event, reason: DeclineReason | null) {
    e.stopPropagation()
    choosing = false
    await proposals.decline(proposal, reason)
  }

  function toggleReasons(e: Event) {
    e.stopPropagation()
    choosing = !choosing
  }
</script>

<div
  class="ghost proposal"
  class:compact
  style:--ghost-color={color ?? 'var(--accent)'}
  data-proposal={proposal.id}
  data-kind={proposal.kind}
  title={proposal.why || proposal.caption}
>
  <div class="body">
    {#if onopen}
      <button class="open" onclick={onopen} aria-label="Open proposal: {proposal.caption}">
        {#if children}{@render children()}{:else}{proposal.caption}{/if}
      </button>
    {:else}
      <span class="caption">
        {#if children}{@render children()}{:else}{proposal.caption}{/if}
      </span>
    {/if}
    {#if !compact}
      <span class="meta">
        <span class="tag"><Icon name="sparkle" size={11} /> proposed</span>
        {#if proposal.why}<span class="why">{proposal.why}</span>{/if}
      </span>
    {/if}
  </div>

  <div class="answers">
    <button
      class="yes"
      disabled={busy}
      onclick={accept}
      aria-label="Accept proposal"
      title="Accept"
    >
      <Icon name="tick" size={13} />
      {#if !compact}Accept{/if}
    </button>
    <button
      class="no"
      disabled={busy}
      onclick={toggleReasons}
      aria-label="Decline proposal"
      aria-expanded={choosing}
      title="Decline"
    >
      <Icon name="close" size={13} />
      {#if !compact}Decline{/if}
    </button>
    {#if choosing}
      <div class="reasons" role="menu">
        <button role="menuitem" onclick={(e) => decline(e, null)}>Just decline</button>
        {#each DECLINE_REASONS as r (r.label)}
          <button role="menuitem" onclick={(e) => decline(e, r.reason)}>{r.label}</button>
        {/each}
      </div>
    {/if}
  </div>
</div>

<style>
  .ghost {
    position: relative;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-1) var(--sp-2);
    border: 1px dashed color-mix(in oklab, var(--ghost-color) 60%, var(--border));
    border-radius: var(--radius);
    background: color-mix(in oklab, var(--ghost-color) 6%, transparent);
    color: var(--fg-muted);
    font-size: var(--text-sm);
    min-width: 0;
  }
  .ghost.compact {
    padding: 2px var(--sp-1);
    font-size: var(--text-xs);
    align-items: flex-start;
    height: 100%;
    box-sizing: border-box;
  }
  .body {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    flex: 1;
  }
  .caption,
  .open {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    opacity: 0.85;
  }
  .open {
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    color: inherit;
    text-align: left;
    cursor: pointer;
  }
  .open:hover {
    color: var(--fg);
  }
  .meta {
    display: flex;
    gap: var(--sp-2);
    align-items: baseline;
    min-width: 0;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .tag {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    flex: none;
    color: color-mix(in oklab, var(--ghost-color) 80%, var(--fg-muted));
    text-transform: lowercase;
  }
  .why {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .answers {
    display: flex;
    gap: var(--sp-1);
    flex: none;
  }
  .answers > button {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    padding: 2px var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    color: var(--fg-muted);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .compact .answers > button {
    padding: 1px 3px;
  }
  .answers > button:hover:not(:disabled) {
    color: var(--fg);
    background: var(--bg-hover);
  }
  .answers > button.yes:hover:not(:disabled) {
    color: var(--fg-on-accent);
    background: var(--accent);
    border-color: var(--accent);
  }
  .answers > button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .reasons {
    position: absolute;
    right: 0;
    top: 100%;
    z-index: 20;
    display: flex;
    flex-direction: column;
    min-width: 160px;
    margin-top: 2px;
    padding: var(--sp-1);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    box-shadow: var(--shadow);
  }
  .reasons button {
    padding: var(--sp-1) var(--sp-2);
    border: none;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    text-align: left;
    cursor: pointer;
  }
  .reasons button:hover {
    background: var(--bg-hover);
  }
</style>
