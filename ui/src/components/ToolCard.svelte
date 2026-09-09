<script lang="ts">
  // One thing the assistant did, or is asking to do.
  //
  // The card exists so that a turn which changed something says so in the
  // transcript rather than only in the prose around it — a model that claims
  // to have added a task and did not is a thing that happens, and the card is
  // the part that cannot lie about it.
  //
  // Its most important state is `waiting`. That is a destructive call stopped
  // before it ran, and the two buttons on it are the last point at which
  // anything can be undone, because this application has no undo.

  import type { ToolCard } from '../lib/agent'
  import Icon from './Icon.svelte'

  let { card, onanswer }: { card: ToolCard; onanswer: (approved: boolean) => void } = $props()

  /**
   * The tool's name as a phrase.
   *
   * Derived from the name rather than looked up in a table: the catalogue is
   * the Rust core's and grows there, and a mapping kept here would go stale
   * silently. `create_task` reads as "create task", which is not elegant and
   * is never wrong.
   */
  const phrase = $derived(card.name.replaceAll('_', ' '))
</script>

<div class="card" class:waiting={card.state === 'waiting'} class:bad={card.state === 'failed'}>
  <div class="row">
    <span class="dot" data-state={card.state}></span>
    <span class="what">
      {phrase}
      {#if card.subject}<b>{card.subject}</b>{/if}
    </span>
    {#if card.state === 'done' && card.summary}
      <span class="said">{card.summary}</span>
    {:else if card.state === 'failed'}
      <span class="said bad">{card.summary || 'failed'}</span>
    {:else if card.state === 'declined'}
      <span class="said">declined</span>
    {/if}
  </div>

  {#if card.state === 'waiting'}
    <p class="ask">This cannot be undone.</p>
    <div class="answer">
      <button class="no" onclick={() => onanswer(false)}>Don't</button>
      <button class="yes" onclick={() => onanswer(true)}>
        <Icon name="trash" size={13} />
        Delete
      </button>
    </div>
  {/if}
</div>

<style>
  .card {
    padding: var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    font-size: var(--text-sm);
  }
  /* A question is not a log line, so it does not look like one. */
  .card.waiting {
    border-color: var(--danger);
    background: color-mix(in oklab, var(--danger) 7%, var(--bg-raised));
  }
  .card.bad {
    border-color: color-mix(in oklab, var(--danger) 45%, var(--border));
  }

  .row {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    min-width: 0;
  }
  .what {
    color: var(--fg-muted);
    overflow-wrap: anywhere;
  }
  .what b {
    color: var(--fg);
    font-weight: 600;
  }
  .said {
    margin-left: auto;
    flex: none;
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }
  .said.bad {
    color: var(--danger);
  }

  .dot {
    width: 6px;
    height: 6px;
    flex: none;
    align-self: center;
    border-radius: 50%;
    background: var(--fg-faint);
  }
  .dot[data-state='running'] {
    background: var(--accent);
    animation: pulse 1.1s ease-in-out infinite;
  }
  .dot[data-state='done'] {
    background: var(--accent);
  }
  .dot[data-state='failed'],
  .dot[data-state='waiting'] {
    background: var(--danger);
  }
  @keyframes pulse {
    0%,
    100% {
      opacity: 0.3;
    }
    50% {
      opacity: 1;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .dot[data-state='running'] {
      animation: none;
    }
  }

  .ask {
    margin: var(--sp-2) 0 0;
    color: var(--fg-muted);
  }
  .answer {
    display: flex;
    gap: var(--sp-2);
    margin-top: var(--sp-2);
  }
  .answer button {
    padding: var(--sp-1) var(--sp-3);
    border-radius: var(--radius-sm);
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
  }
  /* Declining is the wider, plainer button and comes first: it is the
     recoverable answer, and the one a misread click should land on. */
  .no {
    flex: 1;
    border: 1px solid var(--border-strong);
    background: var(--bg-raised);
    color: var(--fg);
  }
  .no:hover {
    background: var(--bg-hover);
  }
  .yes {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    border: 1px solid transparent;
    background: var(--danger);
    color: #fff;
  }
  .yes:hover {
    filter: brightness(1.08);
  }
</style>
