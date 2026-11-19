<script lang="ts">
  // The pill at the bottom of the screen. The shell decides what is
  // happening; this draws it and sends the buttons back. It never asks for the
  // keyboard: what is dictated goes to the field that has it. (GNOME on
  // Wayland hands the window the keyboard anyway; Escape then cancels.)
  import { invoke } from '@tauri-apps/api/core'
  import { listen } from '@tauri-apps/api/event'
  import { onMount } from 'svelte'
  import {
    accepts,
    barHeight,
    hint,
    isQuiet,
    pushLevel,
    remedy,
    silence,
    title,
    type DictationState,
  } from './dictation'

  let shown = $state<DictationState | null>(null)
  let levels = $state<number[]>(silence())

  function take(next: DictationState) {
    if (!accepts(shown, next)) return
    if (!shown || next.seq !== shown.seq) levels = silence()
    shown = next
  }

  onMount(() => {
    let live = true
    const stops: Array<() => void> = []
    void listen<DictationState>('karmayoga://dictation/state', (e) => take(e.payload)).then(
      (stop) => (live ? stops.push(stop) : stop()),
    )
    void listen<number>('karmayoga://dictation/level', (e) => {
      if (shown?.phase === 'listening') levels = pushLevel(levels, e.payload)
    }).then((stop) => (live ? stops.push(stop) : stop()))
    // A page that loaded after the dictation began asks where it has got to.
    void invoke<DictationState | null>('dictation_state')
      .then((current) => {
        if (current && live) take(current)
      })
      .catch(console.error)
    return () => {
      live = false
      for (const stop of stops) stop()
    }
  })

  function send(command: string) {
    invoke(command).catch(console.error)
  }

  const working = $derived(shown?.phase === 'transcribing' || shown?.phase === 'writing')
  const small = $derived(shown ? hint(shown) : null)
  const fix = $derived(shown ? remedy(shown) : null)
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape' && shown) send('dictation_cancel')
  }}
/>

{#if shown}
  <div class="stage" class:overlay={shown.pill === 'overlay'}>
    <section class="pill" aria-label="Dictation">
      <div class="lead" aria-hidden="true">
        {#if shown.phase === 'starting'}
          <span class="pulse"></span>
        {:else if shown.phase === 'listening'}
          <span class="meter" class:quiet={isQuiet(levels)}>
            {#each levels as level, i (i)}
              <i style:height="{Math.round(barHeight(level) * 100)}%" style:--i={i}></i>
            {/each}
          </span>
        {:else if working}
          <span class="spinner"></span>
        {:else if shown.phase === 'done'}
          <svg viewBox="0 0 24 24" class="mark ok"><path d="M5 12.5l4.5 4.5L19 7.5" /></svg>
        {:else}
          <svg viewBox="0 0 24 24" class="mark bad"><path d="M12 6.5v7M12 17.4v.1" /></svg>
        {/if}
      </div>

      <div class="words">
        <p
          class="title"
          class:clamp={shown.phase === 'failed'}
          aria-live="polite"
          title={shown.phase === 'failed' ? title(shown) : undefined}
        >
          {title(shown)}
        </p>
        {#if small}
          <p class="hint">{small}</p>
        {/if}
      </div>

      <div class="actions">
        {#if fix === 'copy'}
          <button class="text" onclick={() => send('dictation_copy')}>Copy</button>
        {:else if fix === 'open'}
          <button class="text" onclick={() => send('dictation_open_main')}>Open Karma Yoga</button>
        {/if}
        {#if shown.phase === 'listening'}
          <button
            class="icon"
            aria-label="Finish and type"
            title="Finish and type"
            onclick={() => send('dictation_stop')}
          >
            <svg viewBox="0 0 24 24"><rect x="7" y="7" width="10" height="10" rx="2" /></svg>
          </button>
        {/if}
        {#if shown.phase !== 'done'}
          <button
            class="icon"
            aria-label="Cancel"
            title="Cancel"
            onclick={() => send('dictation_cancel')}
          >
            <svg viewBox="0 0 24 24" class="stroke"><path d="M7 7l10 10M17 7L7 17" /></svg>
          </button>
        {/if}
      </div>
    </section>
  </div>
{/if}

<style>
  :global(html),
  :global(body) {
    margin: 0;
    background: transparent;
    overflow: hidden;
  }

  /* The window is the pill, or covers the work area and the pill sits in a
     box at its bottom middle -- the shell lets clicks elsewhere go through. */
  .stage {
    width: 100vw;
    height: 100vh;
  }

  .stage.overlay {
    pointer-events: none;
  }

  .pill {
    pointer-events: auto;
    box-sizing: border-box;
    width: 100%;
    height: 100%;
    padding: 0 12px 0 16px;
    display: flex;
    align-items: center;
    gap: 12px;
    background: var(--float);
    color: var(--fg);
    border: 1px solid var(--border-strong);
    border-radius: 36px;
    box-shadow: var(--shadow);
    font-family: var(--font-ui);
    user-select: none;
  }

  .overlay .pill {
    position: fixed;
    left: 50%;
    bottom: 32px;
    width: 360px;
    height: 72px;
    margin-left: -180px;
  }

  .lead {
    flex: none;
    width: 32px;
    height: 32px;
    display: grid;
    place-items: center;
  }

  .words {
    flex: 1;
    min-width: 0;
  }

  .title,
  .hint {
    margin: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .title {
    font-size: var(--text-base);
    font-weight: 600;
    line-height: 1.3;
  }

  /* A failure says why, in up to three lines of the room there is. */
  .title.clamp {
    font-size: var(--text-xs);
    font-weight: 550;
    white-space: normal;
    display: -webkit-box;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    -webkit-box-orient: vertical;
    overflow-wrap: anywhere;
  }

  .hint {
    font-size: var(--text-xs);
    color: var(--fg-muted);
    line-height: 1.3;
  }

  .actions {
    flex: none;
    display: flex;
    align-items: center;
    gap: 4px;
  }

  button {
    border: 0;
    font: inherit;
    color: var(--fg-muted);
    background: transparent;
    cursor: pointer;
  }

  .icon {
    width: 32px;
    height: 32px;
    padding: 0;
    border-radius: 50%;
    display: grid;
    place-items: center;
  }

  .icon svg {
    width: 20px;
    height: 20px;
    fill: currentColor;
  }

  .icon svg.stroke {
    fill: none;
    stroke: currentColor;
    stroke-width: 2;
    stroke-linecap: round;
  }

  .text {
    height: 30px;
    padding: 0 12px;
    border-radius: 999px;
    font-size: var(--text-sm);
    font-weight: 550;
    white-space: nowrap;
    background: var(--accent);
    color: var(--fg-on-accent);
  }

  .text:hover {
    background: var(--accent-hover);
  }

  .icon:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  /* ── what the left of the pill shows ───────────────────────────────── */

  .pulse {
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: var(--accent);
    animation: pulse 1.4s ease-in-out infinite;
  }

  .spinner {
    width: 20px;
    height: 20px;
    border-radius: 50%;
    border: 2.5px solid var(--border-strong);
    border-top-color: var(--accent);
    animation: spin 0.9s linear infinite;
  }

  .meter {
    width: 32px;
    height: 28px;
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .meter i {
    display: block;
    width: 2px;
    border-radius: 2px;
    background: var(--accent);
    transition: height 70ms linear;
  }

  /* Nothing said lately: the bars breathe in a wave instead of sitting still. */
  .meter.quiet i {
    animation: breathe 1.8s ease-in-out infinite;
    animation-delay: calc(var(--i) * 120ms);
  }

  .mark {
    width: 24px;
    height: 24px;
    fill: none;
    stroke-width: 2.5;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .mark.ok {
    stroke: var(--success);
  }

  .mark.bad {
    stroke: var(--danger);
  }

  @keyframes pulse {
    0%,
    100% {
      transform: scale(0.7);
      opacity: 0.5;
    }
    50% {
      transform: scale(1);
      opacity: 1;
    }
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  @keyframes breathe {
    0%,
    100% {
      transform: scaleY(1);
    }
    50% {
      transform: scaleY(1.6);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .pulse,
    .spinner,
    .meter.quiet i {
      animation: none;
    }

    .spinner {
      border-top-color: var(--border-strong);
      border-right-color: var(--accent);
    }

    .meter i {
      transition: none;
    }
  }
</style>
