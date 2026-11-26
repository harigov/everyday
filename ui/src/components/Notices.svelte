<!--
  The things the main screen has to be able to say.

  There had been nowhere to say them. `handle` set `app.error` on every failed
  call, and only the setup, lock and error *screens* rendered it — so a save
  that failed while someone was writing set a string nobody displayed. The
  most important message this app can show, "what you just typed is not on
  disk", was invisible.

  Three notices, in the order they matter:

    conflict   the entry changed elsewhere; nothing was written, and the
               author has to choose which version survives
    read-only  another process holds the vault's write lock, so nothing at
               all can be saved
    error      anything else that failed, dismissible

  And above all three, when the window is reading another computer's vault
  and has lost it: not dismissible either, since it stays true until the
  stream comes back, and clears itself then. It is its own block rather than
  a fourth case because it can hold at the same time as a read-only vault or
  an error, and each of those is still worth seeing.

  The conflict is not dismissible on purpose. It is the only one with a
  decision attached, and clearing it without answering would leave autosave
  silently off.
-->
<script lang="ts">
  import { app } from '../lib/state.svelte'
  import { live } from '../lib/live.svelte'

  const readOnly = $derived(app.status?.writable === false)
  // The computer's own name for the vault where it is known; failing that, a
  // plain "your computer", which is true of a browser on web access too.
  const computer = $derived(app.remote?.name ?? app.status?.name ?? 'your computer')
</script>

{#if !live.reachable}
  <div class="notice offline" role="status">
    <div class="text">
      <strong>Can’t reach {computer}.</strong>
      <span>Showing what was last loaded. Changes will come through when it is back.</span>
    </div>
  </div>
{/if}

{#if app.conflict}
  <div class="notice conflict" role="alert">
    <div class="text">
      <strong>This entry was changed somewhere else.</strong>
      <span>
        Nothing has been overwritten. What you see here is your version and it has not been saved;
        the other one is in the vault.
      </span>
    </div>
    <div class="actions">
      <button class="btn" onclick={() => app.takeTheirs()} disabled={app.saving}>
        Discard mine
      </button>
      <button class="btn btn-primary" onclick={() => app.keepMine()} disabled={app.saving}>
        Keep mine
      </button>
    </div>
  </div>
{:else if readOnly}
  <div class="notice readonly" role="status">
    <div class="text">
      <strong>Read-only.</strong>
      <span>
        This vault is open for writing somewhere else — another window, or
        <code>karmayoga</code> on the command line. You can read and search; nothing can be saved until
        the other one closes.
      </span>
    </div>
  </div>
{:else if app.error}
  <div class="notice error" role="alert">
    <div class="text"><span>{app.error}</span></div>
    <div class="actions">
      <button class="btn" onclick={() => (app.error = null)}>Dismiss</button>
    </div>
  </div>
{/if}

<style>
  .notice {
    display: flex;
    gap: var(--sp-4);
    align-items: center;
    justify-content: space-between;
    padding: var(--sp-3) var(--sp-4);
    border-bottom: 1px solid var(--border);
    font-size: var(--text-sm);
  }
  .text {
    display: flex;
    flex-wrap: wrap;
    gap: 0 var(--sp-2);
    min-width: 0;
  }
  .text span {
    color: var(--fg-muted);
  }
  .actions {
    display: flex;
    gap: var(--sp-2);
    flex-shrink: 0;
  }

  /* Both of the states that mean "your writing is not on disk" are tinted
     with the danger colour rather than only the outright error, because
     that is the thing worth noticing. */
  .conflict,
  .error {
    background: color-mix(in oklab, var(--danger) 10%, var(--bg-raised));
  }
  .conflict strong,
  .error span {
    color: var(--danger);
  }

  .readonly,
  .offline {
    background: var(--bg-sunken);
  }

  /* A phone: the words across the whole width, and the answers under them. */
  :global([data-phone]) .notice {
    flex-direction: column;
    align-items: stretch;
    gap: var(--sp-3);
    padding: var(--sp-3) calc(var(--sp-4) + var(--safe-right)) var(--sp-3)
      calc(var(--sp-4) + var(--safe-left));
  }
  :global([data-phone]) .text {
    display: block;
    line-height: var(--leading-normal);
  }
  :global([data-phone]) .actions {
    justify-content: flex-end;
  }

  code {
    font-family: var(--font-mono);
    font-size: var(--text-xs);
  }
</style>
