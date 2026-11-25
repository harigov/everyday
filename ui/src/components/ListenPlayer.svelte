<!--
  The player for whatever is being read aloud: an email's briefing, an
  article. A compact pill in the top bar, so it stays where it is while
  somebody moves between apps and goes on playing.

  One session at a time, and the store (`listening`) owns it; this only draws
  it and passes the buttons on. When a session ends with a reason -- the
  voice could not start, the model was not reachable -- the pill gives way to
  a short message that goes when it is dismissed, since there is nothing left
  playing for the pill to be a player of.

  The glyphs are drawn here, on the same 24-unit grid and stroke rule as
  `icons.ts`, because the set has no pause or skip yet; `play` and `stop` are
  the set's own.
-->
<script lang="ts">
  import { agent } from '../lib/agent.svelte'
  import { listening, LISTEN_SPEEDS } from '../lib/listen.svelte'
  import { voice } from '../lib/voice.svelte'
  import Icon from './Icon.svelte'

  const preparing = $derived(listening.state === 'preparing')
  const playing = $derived(listening.state === 'playing')

  /** What the speed button shows: "1×", "1.25×". */
  function speedLabel(speed: number): string {
    return `${speed}×`
  }

  /** Each press moves to the next speed and wraps after the last, which is
   *  one button where a menu would be a popover for six numbers. */
  function nextSpeed() {
    const at = LISTEN_SPEEDS.findIndex((s) => s === listening.speed)
    // A speed that is not on the list (kept from an older version) starts the
    // round again from the beginning.
    listening.setSpeed(LISTEN_SPEEDS[(at + 1) % LISTEN_SPEEDS.length] ?? 1)
  }

  const hint = $derived(preparing ? 'Preparing…' : listening.state === 'paused' ? 'Paused' : '')
</script>

{#if listening.active}
  <div class="player" role="group" aria-label="Reading aloud">
    <span class="glyph" aria-hidden="true">
      <svg viewBox="0 0 24 24" width="15" height="15" class="draw">
        <path
          d="M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 18 0v7a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3"
        />
      </svg>
    </span>
    <span class="title" title={listening.title}>{listening.title || 'Reading aloud'}</span>
    {#if hint}<span class="hint">{hint}</span>{/if}
    {#if listening.error}
      <!-- What the session said along the way -- an article read as written,
           a sentence skipped -- while it carries on. A click puts it away. -->
      <button
        class="note"
        aria-live="polite"
        title={listening.error}
        aria-label="{listening.error} (dismiss)"
        onclick={() => (listening.error = null)}
      >
        {listening.error}
      </button>
    {/if}

    <button
      class="ctl"
      title="Back a sentence"
      aria-label="Back a sentence"
      disabled={preparing}
      onclick={() => listening.back()}
    >
      <svg viewBox="0 0 24 24" width="15" height="15" class="draw" aria-hidden="true">
        <path d="M19 5.5v13L9 12Z" /><path d="M5 5v14" />
      </svg>
    </button>
    <button
      class="ctl main"
      title={playing ? 'Pause' : 'Play'}
      aria-label={playing ? 'Pause' : 'Play'}
      disabled={preparing}
      onclick={() => listening.toggle()}
    >
      {#if playing}
        <svg viewBox="0 0 24 24" width="15" height="15" class="draw" aria-hidden="true">
          <path d="M8 5v14" /><path d="M16 5v14" />
        </svg>
      {:else}
        <Icon name="play" size={15} weight={1.6} />
      {/if}
    </button>
    <button
      class="ctl"
      title="Forward a sentence"
      aria-label="Forward a sentence"
      disabled={preparing}
      onclick={() => listening.forward()}
    >
      <svg viewBox="0 0 24 24" width="15" height="15" class="draw" aria-hidden="true">
        <path d="M5 5.5v13l10-6.5Z" /><path d="M19 5v14" />
      </svg>
    </button>
    <button
      class="speed"
      title="Speed: {speedLabel(listening.speed)} (press for the next)"
      aria-label="Speed {speedLabel(listening.speed)}, change"
      onclick={nextSpeed}
    >
      {speedLabel(listening.speed)}
    </button>
    {#if voice.available && agent.supported}
      <!-- Ask about what is being read: the reading pauses while they talk,
           and picks up again after, as the player's does. -->
      <button
        class="ctl"
        disabled={voice.active}
        title={voice.active ? 'You are talking to the assistant' : 'Ask about this'}
        aria-label="Ask about this"
        onclick={() => void voice.askAboutPlaying()}
      >
        <Icon name="mic" size={15} weight={1.6} />
      </button>
    {/if}
    <button
      class="ctl stop"
      title="Stop reading"
      aria-label="Stop reading"
      onclick={() => listening.stop()}
    >
      <Icon name="close" size={14} weight={1.6} />
    </button>
  </div>
{:else if listening.error}
  <div class="player trouble" role="alert">
    <span class="title" title={listening.error}>{listening.error}</span>
    <button
      class="ctl"
      title="Dismiss"
      aria-label="Dismiss the message"
      onclick={() => (listening.error = null)}
    >
      <Icon name="close" size={14} weight={1.6} />
    </button>
  </div>
{/if}

<style>
  .player {
    display: flex;
    align-items: center;
    gap: 2px;
    min-width: 0;
    height: 28px;
    margin-right: var(--sp-2);
    padding: 0 4px 0 var(--sp-3);
    border-radius: 999px;
    background: color-mix(in oklab, var(--accent) 12%, transparent);
    color: var(--accent);
    font-size: var(--text-xs);
  }
  .note {
    flex: 0 1 auto;
    min-width: 0;
    max-width: 260px;
    overflow: hidden;
    margin: 0 var(--sp-1);
    padding: 0;
    border: 0;
    background: none;
    color: var(--danger);
    font: inherit;
    text-overflow: ellipsis;
    white-space: nowrap;
    cursor: pointer;
  }
  .player.trouble {
    background: color-mix(in oklab, var(--danger) 12%, transparent);
    color: var(--danger);
  }

  .glyph {
    flex: none;
    display: grid;
    place-items: center;
    margin-right: 6px;
  }
  /* The same stroke an `Icon` of this size draws: 1.5px on screen. */
  .draw {
    display: block;
    fill: none;
    stroke: currentColor;
    stroke-width: 2.4;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .title {
    min-width: 0;
    max-width: 160px;
    margin-right: 4px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
  }
  .trouble .title {
    max-width: 260px;
  }
  .hint {
    flex: none;
    margin-right: 4px;
    color: var(--fg-subtle);
    white-space: nowrap;
  }

  .ctl {
    flex: none;
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    border-radius: 999px;
    color: inherit;
    transition: background var(--fast) var(--ease);
  }
  .ctl:hover:not(:disabled) {
    background: color-mix(in oklab, var(--accent) 18%, transparent);
  }
  .trouble .ctl:hover {
    background: color-mix(in oklab, var(--danger) 18%, transparent);
  }
  .ctl:disabled {
    opacity: 0.4;
  }
  .ctl.main {
    background: color-mix(in oklab, var(--accent) 16%, transparent);
  }

  .speed {
    flex: none;
    height: 24px;
    min-width: 38px;
    padding: 0 6px;
    border-radius: 999px;
    color: inherit;
    font-size: var(--text-xs);
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    transition: background var(--fast) var(--ease);
  }
  .speed:hover {
    background: color-mix(in oklab, var(--accent) 18%, transparent);
  }

  /* Narrow windows keep the controls and drop the words, so the bar in the
     middle is never squeezed for them. */
  @media (max-width: 1000px) {
    .glyph,
    .title,
    .hint {
      display: none;
    }
    .player {
      padding-left: 4px;
    }
    .player.trouble .title {
      display: block;
      max-width: 140px;
    }
  }

  /* ── On a phone ──────────────────────────────────────────────────────── */

  /* The top bar is the search field's: reading aloud is play and pause and a
     way to stop, there. (A phone is a remote client and cannot read aloud on
     its own; this is only so that what a computer's voice reads is not a
     pill that crowds the field out.) */
  :global([data-phone]) .player {
    height: 40px;
    margin-right: 0;
    padding: 0;
    background: none;
  }
  :global([data-phone]) .player:not(.trouble) > :not(.ctl.main):not(.stop) {
    display: none;
  }
  :global([data-phone]) .ctl {
    width: 40px;
    height: 40px;
  }
  :global([data-phone]) .player.trouble {
    max-width: 52vw;
    padding-left: var(--sp-3);
    background: color-mix(in oklab, var(--danger) 12%, transparent);
  }
  :global([data-phone]) .player.trouble .title {
    display: block;
    max-width: none;
  }
</style>
