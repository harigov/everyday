<script lang="ts">
  // The countdown after Send, with its Undo. Mounted once in App.svelte, not
  // in the Mail app: the timer in the mail store goes on and sends whether or
  // not Mail is on screen, so leaving Mail mid-countdown used to take away
  // the only way to take it back. A status region, so it is announced.
  import { mail } from '../lib/mail.svelte'
  import { relativeTime } from '../lib/format'

  const pending = $derived(mail.sendingUndo)
</script>

<div class="undo-pill-region" role="status" aria-live="polite">
  {#if pending}
    <div class="undo-pill">
      <span>
        {#if pending.scheduled}
          <!-- A "send later" window can be hours out -- a countdown in
               seconds would read as an absurd number. `relativeTime` gives
               the same "in 3 hours" shape the rest of the app uses. -->
          Scheduled — sending {relativeTime(new Date(pending.at).toISOString())}
        {:else}
          Sending in {pending.secondsLeft}s…
        {/if}
      </span>
      <button class="link" onclick={() => void mail.undoSend()}>Undo</button>
    </div>
  {/if}
</div>

<style>
  /* Centred at the foot, where it always was. The toasts stack at the right
     corner, so the two sit side by side rather than on top of each other;
     above them in stacking order so Undo is never covered. */
  .undo-pill-region {
    position: fixed;
    left: 0;
    right: 0;
    bottom: var(--sp-8);
    z-index: 95;
    display: flex;
    justify-content: center;
    pointer-events: none;
  }
  .undo-pill {
    pointer-events: auto;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-2) var(--sp-4);
    border-radius: var(--radius-pill);
    background: var(--float);
    border: 1px solid var(--border);
    box-shadow: var(--shadow-lg);
    font-size: var(--text-sm);
    max-width: calc(100vw - var(--sp-4) * 2);
  }
  .link {
    color: var(--journal-accent, var(--accent));
    font-weight: 650;
  }

  /* Above the bar of apps, not behind it. */
  :global([data-phone]) .undo-pill-region {
    bottom: calc(var(--bottombar-h) + var(--safe-bottom) + var(--sp-4));
  }
  :global([data-phone]) .link {
    min-height: 44px;
    padding: 0 var(--sp-2);
  }
</style>
