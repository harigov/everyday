<!--
  The player, in the top bar: what is playing, skip back, play and pause, skip
  forward, how far it is and how long is left, its speed, and a way to open
  the panel with the picture and Up Next, and to ask the assistant about what
  is playing. ListenPlayer's neighbour, in the same pill: it stays where it is
  while somebody moves between apps, and the sound goes on.

  This only draws the player and passes the buttons on; the store (`player`)
  owns what is loaded, and the one media element is `PlayerHost`'s. When
  something could not be played the pill says so politely, beside what it was
  trying to play, and a press puts the message away; with nothing loaded the
  message stands alone, since there is nothing left for the pill to be a
  player of.
-->
<script lang="ts">
  import { agent } from '../lib/agent.svelte'
  import type { IconName } from '../lib/icons'
  import { menu } from '../lib/menu.svelte'
  import { PLAYER_SPEEDS, player } from '../lib/player.svelte'
  import { formatRemaining } from '../lib/podcast'
  import { upnext } from '../lib/upnext.svelte'
  import { voice } from '../lib/voice.svelte'
  import Icon from './Icon.svelte'

  const current = $derived(player.current)
  const back = $derived(upnext.settings?.skipBackS ?? 15)
  const forward = $derived(upnext.settings?.skipForwardS ?? 30)

  const glyph = $derived<IconName>(
    current?.kind === 'video' ? 'film' : current?.what.type === 'story' ? 'podcast' : 'music',
  )

  /** A picture that would not load is not tried again. */
  let broken = $state<string | null>(null)
  const artwork = $derived(current?.artwork && current.artwork !== broken ? current.artwork : null)

  const duration = $derived(player.durationMs || current?.durationMs || 0)
  const share = $derived(
    duration > 0 ? Math.min(100, Math.max(0, (player.positionMs / duration) * 100)) : 0,
  )

  /** What the speed button shows: "1×", "1.5×". */
  function speedLabel(speed: number): string {
    return `${speed}×`
  }

  /** The speeds as a menu under the button: seven numbers are a list to pick
   *  from, not a button to press through. */
  function speedMenu(e: MouseEvent) {
    const box = (e.currentTarget as HTMLElement).getBoundingClientRect()
    menu.showAt(
      box.left,
      box.bottom + 4,
      PLAYER_SPEEDS.map((s) => ({
        label: speedLabel(s),
        checked: player.speed === s,
        run: () => player.setSpeed(s),
      })),
    )
  }

  function toggleExpanded() {
    player.expanded = !player.expanded
  }

  /** Talking to the assistant about this is on offer where it can be done at
   *  all, and not while it is already being done. */
  const askable = $derived(voice.available && agent.supported)
</script>

{#if current}
  <div class="player" class:problem={!!player.error} role="group" aria-label="Player">
    <button
      class="what"
      aria-expanded={player.expanded}
      aria-label="{player.expanded ? 'Close' : 'Open'} the player: {current.title}"
      title="{current.title}{current.source ? ` · ${current.source}` : ''}"
      onclick={toggleExpanded}
    >
      <span class="glyph" aria-hidden="true">
        {#if artwork}
          <img src={artwork} alt="" onerror={() => (broken = artwork)} />
        {:else}
          <Icon name={glyph} size={14} />
        {/if}
      </span>
      <span class="title">{current.title}</span>
    </button>
    {#if player.error}
      <!-- Why it would not play, while the pill stays to say what it was.
           A press puts it away. -->
      <button
        class="note"
        aria-live="polite"
        title={player.error}
        aria-label="{player.error} (dismiss)"
        onclick={() => (player.error = null)}
      >
        {player.error}
      </button>
    {/if}

    <button
      class="ctl"
      title="Back {back} seconds"
      aria-label="Back {back} seconds"
      onclick={() => player.skip(-back)}
    >
      <Icon name="skip-back" size={15} weight={1.6} />
    </button>
    <button
      class="ctl main"
      class:busy={player.buffering}
      title={player.playing ? 'Pause' : 'Play'}
      aria-label={player.playing ? 'Pause' : 'Play'}
      onclick={() => player.toggle()}
    >
      <Icon name={player.playing ? 'pause' : 'play'} size={14} weight={1.6} filled />
    </button>
    <button
      class="ctl"
      title="Forward {forward} seconds"
      aria-label="Forward {forward} seconds"
      onclick={() => player.skip(forward)}
    >
      <Icon name="skip-forward" size={15} weight={1.6} />
    </button>

    {#if duration > 0}
      <span class="progress">
        <span
          class="track"
          role="progressbar"
          aria-label="Played"
          aria-valuemin="0"
          aria-valuemax="100"
          aria-valuenow={Math.round(share)}
        >
          <span class="fill" style:width="{share}%"></span>
        </span>
        <span class="left">{formatRemaining(player.positionMs, duration)}</span>
      </span>
    {/if}

    <button
      class="speed"
      title="Speed: {speedLabel(player.speed)}"
      aria-label="Speed {speedLabel(player.speed)}, change"
      aria-haspopup="menu"
      onclick={speedMenu}
    >
      {speedLabel(player.speed)}
    </button>
    {#if askable}
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
      class="ctl"
      class:on={player.expanded}
      title="Up Next and the picture"
      aria-label="Up Next and the picture"
      aria-pressed={player.expanded}
      onclick={toggleExpanded}
    >
      <Icon name="list" size={15} weight={1.6} />
    </button>
    <button class="ctl" title="Stop" aria-label="Stop" onclick={() => player.stop()}>
      <Icon name="close" size={14} weight={1.6} />
    </button>
  </div>
{:else if player.error}
  <div class="player trouble" role="alert">
    <span class="title" title={player.error}>{player.error}</span>
    <button
      class="ctl"
      title="Dismiss"
      aria-label="Dismiss the message"
      onclick={() => (player.error = null)}
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
    padding: 0 4px 0 var(--sp-1);
    border-radius: var(--radius-pill);
    background: color-mix(in oklab, var(--accent) 12%, transparent);
    color: var(--accent);
    font-size: var(--text-xs);
  }
  .player.trouble {
    padding-left: var(--sp-3);
    background: color-mix(in oklab, var(--danger) 12%, transparent);
    color: var(--danger);
  }

  /* The picture and the name, which open the panel: the pill's one wide
     target, since the controls beside it are small. */
  .what {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    height: 24px;
    padding: 0 6px 0 2px;
    border-radius: var(--radius-pill);
    color: inherit;
    transition: background var(--fast) var(--ease);
  }
  .what:hover {
    background: color-mix(in oklab, var(--accent) 18%, transparent);
  }
  .glyph {
    flex: none;
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    overflow: hidden;
    border-radius: 5px;
  }
  .glyph img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .title {
    min-width: 0;
    max-width: 150px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
  }
  .trouble .title {
    max-width: 260px;
  }
  .note {
    flex: 0 1 auto;
    min-width: 0;
    max-width: 200px;
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

  .ctl {
    flex: none;
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    border-radius: var(--radius-pill);
    color: inherit;
    transition: background var(--fast) var(--ease);
  }
  .ctl:hover {
    background: color-mix(in oklab, var(--accent) 18%, transparent);
  }
  .ctl:disabled {
    opacity: 0.45;
    pointer-events: none;
  }
  .trouble .ctl:hover {
    background: color-mix(in oklab, var(--danger) 18%, transparent);
  }
  .ctl.main,
  .ctl.on {
    background: color-mix(in oklab, var(--accent) 16%, transparent);
  }
  .ctl.busy {
    animation: breathe 1s ease-in-out infinite;
  }
  @keyframes breathe {
    50% {
      opacity: 0.55;
    }
  }

  /* How far, as a hairline, and what is left in words. */
  .progress {
    flex: none;
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0 4px;
  }
  .track {
    display: block;
    width: 44px;
    height: 3px;
    border-radius: 2px;
    background: color-mix(in oklab, var(--accent) 22%, transparent);
    overflow: hidden;
  }
  .fill {
    display: block;
    height: 100%;
    background: var(--accent);
  }
  .left {
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .speed {
    flex: none;
    height: 24px;
    min-width: 38px;
    padding: 0 6px;
    border-radius: var(--radius-pill);
    color: inherit;
    font-size: var(--text-xs);
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    transition: background var(--fast) var(--ease);
  }
  .speed:hover {
    background: color-mix(in oklab, var(--accent) 18%, transparent);
  }

  /* Narrower windows drop the hairline first, then the words, keeping the
     picture and the controls, so the bar in the middle is never squeezed for
     them. */
  @media (max-width: 1200px) {
    .track {
      display: none;
    }
  }
  @media (max-width: 1000px) {
    .title,
    .left {
      display: none;
    }
    .trouble .title {
      display: block;
      max-width: 140px;
    }
    .what {
      padding: 0 2px;
    }
  }

  /* ── On a phone ──────────────────────────────────────────────────────── */

  /* The top bar is the search field's, and a pill of nine buttons would
     leave it a sliver. What is playing is two round controls there -- its
     picture, which opens the player, and play and pause -- and everything
     else (skip, speed, asking about it, stopping it, Up Next) is in the
     player's panel, one press away and with room for a finger. */
  :global([data-phone]) .player {
    height: 40px;
    gap: 2px;
    margin-right: 0;
    padding: 0;
    background: none;
  }
  :global([data-phone]) .player:not(.trouble) > :not(.what):not(.ctl.main) {
    display: none;
  }
  :global([data-phone]) .what {
    height: 40px;
    padding: 0;
    border-radius: 50%;
  }
  :global([data-phone]) .what .title {
    display: none;
  }
  :global([data-phone]) .glyph {
    width: 36px;
    height: 36px;
    border-radius: 10px;
    background: color-mix(in oklab, var(--accent) 12%, transparent);
  }
  :global([data-phone]) .player.problem .glyph {
    box-shadow: 0 0 0 2px var(--danger);
  }
  :global([data-phone]) .ctl.main {
    width: 40px;
    height: 40px;
    background: color-mix(in oklab, var(--accent) 16%, transparent);
  }

  /* Said alone, with nothing loaded: a short line and a way to put it away. */
  :global([data-phone]) .player.trouble {
    display: flex;
    max-width: 52vw;
    padding: 0 0 0 var(--sp-3);
    background: color-mix(in oklab, var(--danger) 12%, transparent);
  }
  :global([data-phone]) .player.trouble .title {
    display: block;
    max-width: none;
  }
  :global([data-phone]) .player.trouble .ctl {
    width: 40px;
    height: 40px;
  }
</style>
