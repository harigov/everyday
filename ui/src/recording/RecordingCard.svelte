<script lang="ts">
  // The corner card. The shell decides what it says and when the countdown
  // ends; this only draws that and sends the button back.
  import { invoke } from '@tauri-apps/api/core'
  import { listen } from '@tauri-apps/api/event'
  import { onMount } from 'svelte'

  interface Card {
    key: string
    title: string
    phase: 'join' | 'countdown'
    seconds: number
    canJoin: boolean
  }

  let card = $state<Card | null>(null)
  let busy = $state(false)

  onMount(() => {
    void invoke<Card | null>('recording_notice').then((current) => {
      card = current
    })
    let unlisten = () => {}
    void listen<Card | null>('recording-notice', (event) => {
      card = event.payload
    }).then((stop) => (unlisten = stop))
    return () => unlisten()
  })

  async function answer(which: 'take' | 'join' | 'notNow') {
    if (!card || busy) return
    busy = true
    try {
      await invoke('meeting_offer_answer', { key: card.key, answer: which })
    } catch (e) {
      console.error(e)
    } finally {
      busy = false
    }
  }
</script>

{#if card}
  <div class="stage">
    <section class="card">
      {#if card.phase === 'countdown'}
        <p class="kicker">Recording starts in</p>
        <p class="count">{card.seconds}<span>s</span></p>
        <h1>{card.title}</h1>
        <div class="actions">
          <button class="btn" disabled={busy} onclick={() => void answer('notNow')}>Cancel</button>
          <button class="btn primary" disabled={busy} onclick={() => void answer('take')}>
            Take notes
          </button>
        </div>
      {:else}
        <p class="kicker">Meeting</p>
        <h1>{card.title}</h1>
        <div class="actions">
          <button class="btn" disabled={busy} onclick={() => void answer('notNow')}>Not now</button>
          <button class="btn primary" disabled={busy} onclick={() => void answer('join')}>
            {card.canJoin ? 'Join and start recording' : 'Start recording'}
          </button>
        </div>
      {/if}
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

  .stage {
    width: 100vw;
    height: 100vh;
  }

  .card {
    box-sizing: border-box;
    width: 100%;
    height: 100%;
    padding: 16px 18px 14px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    background: var(--bg-raised);
    color: var(--fg);
    border: 1px solid var(--border);
    border-radius: 16px;
    box-shadow: var(--shadow-lg, 0 12px 40px rgb(0 0 0 / 18%));
    font-family: var(--font-ui);
  }

  .kicker {
    margin: 0;
    font-size: 12px;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--fg-muted);
  }

  h1 {
    margin: 0;
    font-size: 16px;
    font-weight: 600;
    line-height: 1.3;
    overflow: hidden;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
  }

  .count {
    margin: 0;
    font-size: 28px;
    font-weight: 650;
    font-variant-numeric: tabular-nums;
    line-height: 1;
  }

  .count span {
    font-size: 16px;
    margin-left: 2px;
    color: var(--fg-muted);
  }

  .actions {
    margin-top: auto;
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  .btn {
    height: 32px;
    padding: 0 12px;
    border: 0;
    border-radius: 999px;
    background: transparent;
    color: var(--fg-muted);
    font: inherit;
    font-size: 13px;
    cursor: pointer;
  }

  .btn:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .btn.primary {
    background: var(--accent);
    color: var(--fg-on-accent);
  }

  .btn:disabled {
    opacity: 0.5;
    pointer-events: none;
  }
</style>
