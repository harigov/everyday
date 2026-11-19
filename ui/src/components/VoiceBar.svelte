<script lang="ts">
  // What the composer shows while the assistant is being talked to: a light
  // that says what it is doing, the words for it, and the two things to reach
  // for -- the microphone off, and the end of the conversation.
  //
  // It takes the place of the text box and its buttons, and nothing else
  // changes, so what was said still scrolls by in the thread above. The
  // light is the only thing that moves, and it moves with `transform` and
  // `opacity` alone: those stay on the compositor in WebKitGTK, where
  // animating anything that paints (a shadow, a blur, a size) is what made
  // the scroll stutter.

  import { voice } from '../lib/voice.svelte'
  import Icon from './Icon.svelte'

  /**
   * How the light behaves. Several states share one: waiting on you to speak
   * is the same gentle breathing whether it is your turn or your answer; the
   * click it cannot hear is the one that stands still.
   */
  const look = $derived(
    voice.muted
      ? 'still'
      : voice.state === 'hearing'
        ? 'hearing'
        : voice.state === 'speaking'
          ? 'bars'
          : voice.state === 'starting' ||
              voice.state === 'understanding' ||
              voice.state === 'thinking'
            ? 'dots'
            : voice.state === 'listening' || voice.state === 'asking'
              ? 'listening'
              : 'still',
  )
</script>

<div class="voice" role="group" aria-label="Talking to the assistant">
  <span class="light {look}" aria-hidden="true">
    {#if look === 'dots'}
      <i></i><i></i><i></i>
    {:else if look === 'bars'}
      <i></i><i></i><i></i><i></i>
    {:else}
      <b class="ring"></b><b class="core"></b>
    {/if}
  </span>

  <span class="label" aria-live="polite">
    {voice.label}{voice.muted && voice.state !== 'listening' ? ' · Microphone off' : ''}
  </span>
  {#if voice.error}
    <!-- The session carries on, so this is a remark and not an ending. -->
    <span class="trouble" title={voice.error}>{voice.error}</span>
  {/if}

  <button
    class="tool"
    class:off={voice.muted}
    onclick={() => void voice.toggleMute()}
    aria-pressed={voice.muted}
    aria-label={voice.muted ? 'Turn the microphone on' : 'Turn the microphone off'}
    title={voice.muted ? 'Turn the microphone on' : 'Turn the microphone off'}
  >
    <Icon name={voice.muted ? 'mic-off' : 'mic'} size={16} />
  </button>
  <button class="end" onclick={() => void voice.stop()} title="End the conversation (Esc)">
    End
  </button>
</div>

<style>
  .voice {
    display: flex;
    flex: 1;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
    /* One button's height, so the bar is no shorter than the box it replaced
       on the page; the composer holds the rest. */
    min-height: 32px;
  }

  /* ── The light ───────────────────────────────────────────────────── */

  .light {
    position: relative;
    display: flex;
    flex: none;
    align-items: center;
    justify-content: center;
    gap: 3px;
    width: 28px;
    height: 24px;
    color: var(--accent);
  }
  .light.still {
    color: var(--fg-faint);
  }

  /* A dot, and a ring behind it that grows and fades. */
  .light .core,
  .light .ring {
    width: 10px;
    height: 10px;
    border-radius: 50%;
  }
  .light .core {
    background: currentColor;
  }
  .light .ring {
    position: absolute;
    border: 2px solid currentColor;
    opacity: 0;
  }
  .light.listening .ring {
    animation: breathe 2.4s ease-out infinite;
  }
  /* Louder while there is a voice to hear: the dot swells and the ring is
     quicker and wider. */
  .light.hearing .core {
    transform: scale(1.25);
  }
  .light.hearing .ring {
    animation: breathe-wide 1.1s ease-out infinite;
  }

  /* Three dots that come and go in turn, as the thread's own do. */
  .light.dots i {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: currentColor;
    animation: blink 1.2s ease-in-out infinite;
  }
  .light.dots i:nth-child(2) {
    animation-delay: 0.2s;
  }
  .light.dots i:nth-child(3) {
    animation-delay: 0.4s;
  }

  /* Bars that rise and fall out of step. They are one height and are only
     squashed, so nothing is laid out again. */
  .light.bars i {
    width: 3px;
    height: 16px;
    border-radius: 2px;
    background: currentColor;
    animation: speak 0.9s ease-in-out infinite;
  }
  .light.bars i:nth-child(2) {
    animation-delay: 0.15s;
  }
  .light.bars i:nth-child(3) {
    animation-delay: 0.3s;
  }
  .light.bars i:nth-child(4) {
    animation-delay: 0.45s;
  }

  @keyframes breathe {
    0% {
      transform: scale(0.8);
      opacity: 0.55;
    }
    100% {
      transform: scale(2);
      opacity: 0;
    }
  }
  @keyframes breathe-wide {
    0% {
      transform: scale(0.9);
      opacity: 0.7;
    }
    100% {
      transform: scale(2.4);
      opacity: 0;
    }
  }
  @keyframes blink {
    0%,
    100% {
      opacity: 0.25;
    }
    50% {
      opacity: 1;
    }
  }
  @keyframes speak {
    0%,
    100% {
      transform: scaleY(0.3);
    }
    50% {
      transform: scaleY(1);
    }
  }

  /* Nothing moves; what is going on is still told by the words, and by the
     shape. */
  @media (prefers-reduced-motion: reduce) {
    .light.listening .ring,
    .light.hearing .ring,
    .light.dots i,
    .light.bars i {
      animation: none;
    }
    .light.listening .ring,
    .light.hearing .ring {
      opacity: 0.35;
      transform: scale(1.5);
    }
    .light.dots i {
      opacity: 0.7;
    }
    .light.bars i:nth-child(odd) {
      transform: scaleY(0.55);
    }
    .light.bars i:nth-child(even) {
      transform: scaleY(0.9);
    }
  }

  /* ── The words, and the two buttons ──────────────────────────────── */

  .label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }
  .tool {
    display: grid;
    flex: none;
    place-items: center;
    width: 32px;
    height: 32px;
    border: 0;
    border-radius: var(--radius);
    background: none;
    color: var(--fg-muted);
    cursor: pointer;
  }
  .tool:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  /* Off is a state, not a hover: it keeps its fill. */
  .tool.off {
    background: var(--bg-hover);
    color: var(--danger);
  }
  .end {
    flex: none;
    height: 28px;
    padding: 0 var(--sp-3);
    border: 0;
    border-radius: 999px;
    background: var(--fg);
    color: var(--bg);
    font-size: var(--text-sm);
    font-weight: 600;
    cursor: pointer;
  }

  .trouble {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    color: var(--danger, var(--fg-muted));
    font-size: var(--text-xs, 12px);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
