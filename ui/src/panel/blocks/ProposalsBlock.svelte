<script lang="ts">
  // A meeting's action items, each taken or left on its own: a row with the
  // task and who or when it is for, and two round buttons -- leave it out, add
  // it. Decided, a row says so: an added one is tinted with success and
  // ticked, a declined one fades and is struck through. A press shows at once
  // and waits, switched off, until the shell has answered (`proposalView` in
  // `cards.ts`). The heading says how many there are and how far the deciding
  // has got.
  import { proposalsTally, proposalsTitle } from '../cards'
  import type { Decision, PanelBlock, Proposal } from '../panel'

  let {
    block,
    viewOf,
    ondecide,
  }: {
    block: Extract<PanelBlock, { type: 'proposals' }>
    /** How an item is drawn: where it stands, and whether it is waiting on the shell. */
    viewOf: (item: Proposal) => { state: Decision; locked: boolean }
    ondecide: (item: Proposal, to: 'accepted' | 'rejected') => void
  } = $props()

  const views = $derived(block.items.map((item) => ({ item, view: viewOf(item) })))
  const tally = $derived(proposalsTally(views.map(({ view }) => view.state)))
</script>

<section class="proposals">
  <h3 class="pc-label">
    <span>{proposalsTitle(block.heading, block.items.length)}</span>
    {#if tally}<span class="pc-aside">{tally}</span>{/if}
  </h3>
  <ul>
    {#each views as { item, view } (item.id)}
      <li class={view.state} class:locked={view.locked}>
        <span class="words">
          <span class="title">{item.title}</span>
          {#if item.detail || item.due}
            <span class="detail">
              {#if item.due}<span class="due">{item.due}</span>{/if}
              {#if item.detail}<span>{item.detail}</span>{/if}
            </span>
          {/if}
        </span>
        {#if view.state === 'pending'}
          <span class="choose">
            <button
              type="button"
              class="round reject"
              title="Decline"
              aria-label="Decline: {item.title}"
              disabled={view.locked}
              onclick={() => ondecide(item, 'rejected')}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M7 7l10 10M17 7 7 17" /></svg>
            </button>
            <button
              type="button"
              class="round accept"
              title="Add to your tasks"
              aria-label="Add to your tasks: {item.title}"
              disabled={view.locked}
              onclick={() => ondecide(item, 'accepted')}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5.5 12.5 10 17l8.5-9.5" /></svg>
            </button>
          </span>
        {:else}
          <span class="verdict">
            {#if view.state === 'accepted'}
              <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5.5 12.5 10 17l8.5-9.5" /></svg>
              Added
            {:else}
              Declined
            {/if}
          </span>
        {/if}
      </li>
    {/each}
  </ul>
</section>

<style>
  ul {
    padding: 0;
    list-style: none;
    border-radius: var(--radius-lg);
    box-shadow: inset 0 0 0 1px var(--border);
  }

  li {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 8px 8px 12px;
    transition:
      background var(--fast) var(--ease),
      opacity var(--fast) var(--ease);
  }

  li + li {
    border-top: 1px solid var(--border);
  }

  li:first-child {
    border-radius: var(--radius-lg) var(--radius-lg) 0 0;
  }

  li:last-child {
    border-radius: 0 0 var(--radius-lg) var(--radius-lg);
  }

  li:only-child {
    border-radius: var(--radius-lg);
  }

  .words {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
    overflow-wrap: anywhere;
  }

  .title {
    font-size: var(--text-sm);
    font-weight: 600;
    line-height: var(--leading-snug);
  }

  .detail {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 2px 8px;
    font-size: var(--text-xs);
    color: var(--fg-muted);
  }

  .due {
    padding: 0 7px;
    border-radius: var(--radius-pill);
    background: color-mix(in oklab, var(--app-todo) 15%, transparent);
    color: color-mix(in oklab, var(--app-todo) 68%, var(--fg));
    font-weight: 650;
    line-height: 18px;
  }

  .choose {
    flex: none;
    display: flex;
    gap: 6px;
  }

  .round {
    width: 30px;
    height: 30px;
    padding: 0;
    display: grid;
    place-items: center;
    border: 0;
    border-radius: 50%;
    cursor: pointer;
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease),
      box-shadow var(--fast) var(--ease);
  }

  .round svg {
    width: 16px;
    height: 16px;
    fill: none;
    stroke: currentColor;
    stroke-width: 2.4;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .round:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .reject {
    background: transparent;
    box-shadow: inset 0 0 0 1px var(--border-strong);
    color: var(--fg-muted);
  }

  .reject:hover {
    background: color-mix(in oklab, var(--danger) 12%, transparent);
    box-shadow: inset 0 0 0 1px color-mix(in oklab, var(--danger) 40%, transparent);
    color: var(--danger);
  }

  .accept {
    background: color-mix(in oklab, var(--success) 15%, transparent);
    color: color-mix(in oklab, var(--success) 75%, var(--fg));
  }

  .accept:hover {
    background: var(--success);
    color: var(--float);
  }

  /* Decided. */
  li.accepted {
    background: color-mix(in oklab, var(--success) 9%, transparent);
  }

  li.rejected {
    opacity: 0.55;
  }

  li.rejected .title {
    text-decoration: line-through;
    text-decoration-color: color-mix(in oklab, var(--fg-muted) 70%, transparent);
  }

  /* Sent, and not yet answered: decided in look, a touch faint. */
  li.locked .verdict {
    opacity: 0.6;
  }

  .verdict {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding-right: 4px;
    font-size: var(--text-xs);
    font-weight: 650;
    color: var(--fg-muted);
  }

  li.accepted .verdict {
    color: color-mix(in oklab, var(--success) 75%, var(--fg));
  }

  .verdict svg {
    width: 14px;
    height: 14px;
    fill: none;
    stroke: currentColor;
    stroke-width: 2.6;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
</style>
