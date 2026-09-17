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
  import { mail } from '../lib/mail.svelte'
  import Icon from './Icon.svelte'

  let {
    card,
    onanswer,
  }: { card: ToolCard; onanswer: (answer: 'confirm' | 'decline' | 'later') => void } = $props()

  /**
   * The tool's name as a phrase.
   *
   * Derived from the name rather than looked up in a table: the catalogue is
   * the Rust core's and grows there, and a mapping kept here would go stale
   * silently. `create_task` reads as "create task", which is not elegant and
   * is never wrong.
   */
  const phrase = $derived(card.name.replaceAll('_', ' '))

  /**
   * Whether this card is asking about something leaving the vault --
   * sending mail -- rather than something being removed. Drawn in its own
   * colour and with its own question, because the two are different kinds
   * of decision: one asks "can this be undone" and the other asks "should
   * this reach them at all". See `ConfirmKind` in `lib/types.ts`.
   */
  const outward = $derived(card.confirmKind === 'outward')
  const ask = $derived(
    outward
      ? 'This will reach somebody outside the vault.'
      : card.confirmKind === 'search'
        ? 'This would send words from mail just read to a search provider.'
        : 'This cannot be undone.',
  )
</script>

<div
  class="card"
  class:waiting={card.state === 'waiting'}
  class:outward
  class:bad={card.state === 'failed'}
>
  <div class="row">
    <span class="dot" data-state={card.state} class:outward></span>
    <span class="what">
      {phrase}
      {#if card.subject}<b>{card.subject}</b>{/if}
    </span>
    {#if card.state === 'done' && card.mailLink}
      <!-- What the plan calls "the transcript links to what the assistant
           did": a mail write's own result named a thread, so the sentence
           that already describes it opens Mail there rather than sitting
           inert beside a card nobody can act on. -->
      <button
        class="said link"
        onclick={() => void mail.openFromElsewhere(card.mailLink!.threadId)}
      >
        {card.summary || `Opened ${card.mailLink.subject}`} &rarr;
      </button>
    {:else if card.state === 'done' && card.summary}
      <span class="said">{card.summary}</span>
    {:else if card.state === 'failed'}
      <span class="said bad">{card.summary || 'failed'}</span>
    {:else if card.state === 'declined'}
      <span class="said">declined</span>
    {:else if card.state === 'later'}
      <span class="said">saved for later</span>
    {/if}
  </div>

  {#if card.state === 'waiting'}
    <p class="ask">{ask}</p>
    <div class="answer">
      <button class="no" onclick={() => onanswer('decline')}>Don't</button>
      {#if card.canPark}
        <button class="later" onclick={() => onanswer('later')}>Later</button>
      {/if}
      <button class="yes" class:outward onclick={() => onanswer('confirm')}>
        <Icon name={outward ? 'mail' : 'trash'} size={13} />
        {outward ? 'Send' : 'Delete'}
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
  /* Sending mail is a different kind of decision from a delete -- nothing
     is destroyed, so the red the delete card reaches for would say the
     wrong thing. The accent colour is what the rest of the interface
     already uses for "this is on its way to happen", which is exactly
     what a queued send is. */
  .card.waiting.outward {
    border-color: var(--accent);
    background: color-mix(in oklab, var(--accent) 7%, var(--bg-raised));
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
  /* A card's own summary, made a link when a mail write named a thread --
     same size and place as the plain `.said` text, just clickable and in
     the accent colour that says "this goes somewhere". */
  button.said.link {
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--accent);
    cursor: pointer;
  }
  button.said.link:hover {
    text-decoration: underline;
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
  .dot[data-state='waiting'].outward {
    background: var(--accent);
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
  /* The quiet third answer: no border colour of its own, so it never reads
     as urgently as "don't" or as committing as "delete"/"send". */
  .later {
    flex: none;
    border: 1px solid transparent;
    background: none;
    color: var(--fg-muted);
  }
  .later:hover {
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
  .yes.outward {
    background: var(--accent);
  }
  .yes:hover {
    filter: brightness(1.08);
  }
</style>
