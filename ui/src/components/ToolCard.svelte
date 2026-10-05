<script lang="ts">
  // One thing the assistant did, or is asking to do.
  //
  // The card exists so that a turn which changed something says so in the
  // transcript rather than only in the prose around it — a model that claims
  // to have added a task and did not is a thing that happens, and the card is
  // the part that cannot lie about it.
  //
  // Its most important state is `waiting`. That is a call stopped before it
  // ran, and the buttons on it are the last point at which anything can be
  // undone, because this application has no undo.

  import { toolLabel, type ToolCard } from '../lib/agent'
  import type { IconName } from '../lib/icons'
  import { mail } from '../lib/mail.svelte'
  import Icon from './Icon.svelte'

  let {
    card,
    onanswer,
  }: { card: ToolCard; onanswer: (answer: 'confirm' | 'decline' | 'later') => void } = $props()

  /**
   * What the call is called, in the tense the card is in -- "Searching the
   * web for", "Listed tasks". Derived from the name rather than looked up;
   * see `toolLabel` for why, and for the few tools that say more.
   */
  const label = $derived(toolLabel(card.name, card.arguments, card.state))

  /**
   * The tools that reach outside the vault get a mark of their own. They
   * are the ones somebody scanning a long turn most wants to find: what it
   * looked up, and where.
   */
  const ICONS: Record<string, IconName> = {
    web_search: 'search',
    read_web_page: 'globe',
    get_weather: 'sun',
  }
  const icon = $derived(ICONS[card.name] ?? null)

  /**
   * Whether this card is asking about something leaving the vault rather
   * than something being removed. Drawn in the accent colour rather than
   * the danger one, because these are a different kind of decision: one
   * asks "can this be undone" and the others ask "should this go out at
   * all". See `ConfirmKind` in `lib/types.ts`.
   */
  const outward = $derived(
    card.confirmKind === 'outward' || card.confirmKind === 'search' || card.confirmKind === 'fetch',
  )

  /** The question, and the button that says yes to it, per kind. */
  const ASKS = {
    destructive: { ask: 'This cannot be undone.', yes: 'Delete', icon: 'trash' },
    outward: { ask: 'This will reach somebody outside the vault.', yes: 'Send', icon: 'mail' },
    search: {
      ask: 'This would send words from mail just read to a search engine.',
      yes: 'Search',
      icon: 'search',
    },
    fetch: {
      ask: 'Neither you nor a search gave it this address. Opening it tells that site it was asked for.',
      yes: 'Open',
      icon: 'globe',
    },
  } as const satisfies Record<string, { ask: string; yes: string; icon: IconName }>
  const asking = $derived(ASKS[card.confirmKind ?? 'destructive'])
</script>

<div
  class="card"
  class:waiting={card.state === 'waiting'}
  class:outward
  class:bad={card.state === 'failed'}
>
  <div class="row">
    {#if card.state === 'running'}
      <span class="spin" aria-hidden="true"></span>
    {:else if icon}
      <span class="mark" data-state={card.state}><Icon name={icon} size={13} weight={1.9} /></span>
    {:else}
      <span class="dot" data-state={card.state} class:outward></span>
    {/if}
    <!-- The label and what came of it share a line when both fit. A result
         that does not -- a remembered fact is a whole sentence -- drops to a
         line of its own under the label and wraps there, rather than
         squeezing the label down to one letter a line. -->
    <div class="text">
      <span class="what">
        {label.text}
        <!-- A question names its subject in full -- the whole address a page
             would be fetched from, the task a delete would remove -- because
             that is what is being decided. Otherwise the short form: a host,
             a query, a place. -->
        {#if card.subject}<b>{card.subject}</b>{:else if label.detail}<b>{label.detail}</b>{/if}
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
      {:else if card.state === 'stopped'}
        <span class="said">stopped</span>
      {/if}
    </div>
  </div>

  {#if card.state === 'waiting'}
    <p class="ask">{asking.ask}</p>
    <div class="answer">
      <button class="no" onclick={() => onanswer('decline')}>Don't</button>
      {#if card.canPark}
        <button class="later" onclick={() => onanswer('later')}>Later</button>
      {/if}
      <button class="yes" class:outward onclick={() => onanswer('confirm')}>
        <Icon name={asking.icon} size={13} />
        {asking.yes}
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

  /* The mark is pinned to the label's first line rather than centred on the
     whole card, which once the result has wrapped is two lines or more. Each
     mark below is offset by half of what its own height leaves of a line. */
  .row {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    min-width: 0;
  }
  .text {
    display: flex;
    flex: 1;
    flex-wrap: wrap;
    align-items: baseline;
    column-gap: var(--sp-2);
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
  /* Free to shrink, so that once it has a line of its own it wraps inside it
     instead of running off the card. Right-aligned while it shares the
     label's line, as before. */
  .said {
    margin-left: auto;
    min-width: 0;
    color: var(--fg-faint);
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
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

  /* A running call turns, rather than blinks: a dot pulsing at the same
     rate as every other dot in a long turn reads as decoration, and the one
     thing this card has to say while it runs is "still going". */
  .spin {
    width: 10px;
    height: 10px;
    flex: none;
    margin-top: calc((1lh - 10px) / 2);
    border: 1.5px solid color-mix(in oklab, var(--accent) 25%, transparent);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: turn 0.8s linear infinite;
  }
  @keyframes turn {
    to {
      rotate: 360deg;
    }
  }
  .mark {
    display: grid;
    flex: none;
    height: 1lh;
    place-items: center;
    color: var(--accent);
  }
  .mark[data-state='failed'] {
    color: var(--danger);
  }
  .mark[data-state='declined'],
  .mark[data-state='later'],
  .mark[data-state='stopped'] {
    color: var(--fg-faint);
  }

  .dot {
    width: 6px;
    height: 6px;
    flex: none;
    margin-top: calc((1lh - 6px) / 2);
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
    .spin {
      animation: pulse 1.1s ease-in-out infinite;
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
