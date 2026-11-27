<script lang="ts">
  // Looking at a vault on another computer.
  //
  // The link is what somebody copied from that machine's settings, or scanned
  // off its screen. It carries three things: where to dial, a one-time code,
  // and the fingerprint of the certificate to pin — so the check that this is
  // the right computer happens before anything secret is sent.
  //
  // The honest limit is stated here rather than discovered on a train: there is
  // no offline mode. A client keeps no copy of the vault, which is the whole
  // point of the arrangement, so it can do nothing at all without a route to
  // the machine that holds it.
  import { app } from '../lib/state.svelte'
  import { mobileShell } from '../lib/api'
  import { friendlyDate } from '../lib/format'
  import { isoDate } from '../lib/time'
  import { isPairingLink } from '../lib/legacy'
  import Icon from './Icon.svelte'

  /** Drawn in Settings → Vault → Sharing rather than on the setup screen: a
   *  computer that already has a vault of its own, which the setup screen
   *  is never shown to. Headed like the sections beside it there. */
  let { inSettings = false }: { inSettings?: boolean } = $props()

  let link = $state('')
  let busy = $state(false)

  const known = $derived(app.remotes)
  // A link copied under a former name (`karmayoga://pair?`, `everyday://pair?`)
  // is let through too: the shell reads every spelling and says what is wrong
  // with a link it cannot use, so this only keeps the button off for text that
  // is plainly not a link at all.
  const ready = $derived(isPairingLink(link))

  async function connect() {
    if (!ready || busy) return
    busy = true
    try {
      await app.connectRemote(link.trim())
    } finally {
      busy = false
    }
  }

  async function reconnect(id: string) {
    busy = true
    try {
      await app.reconnectRemote(id)
    } finally {
      busy = false
    }
  }
</script>

<section class="connect" class:in-settings={inSettings}>
  {#if inSettings}
    <span class="eyebrow">Use a vault on another computer</span>
  {:else}
    <h2>{mobileShell ? 'Connect to your computer' : 'Use a vault on another computer'}</h2>
  {/if}
  <p class="lead">
    Open <strong>Settings → Vault → Share on the network</strong> on the computer that holds your vault,
    then paste the link it shows here.
  </p>
  {#if inSettings}
    <p class="lead">
      This window then shows that vault instead of this one, which it closes -- and stops sharing --
      until you choose <strong>Stop using the other computer</strong> in the command palette.
    </p>
  {/if}

  {#if known.length > 0}
    <ul class="known">
      {#each known as connection (connection.id)}
        <li>
          <button class="row" onclick={() => reconnect(connection.id)} disabled={busy}>
            <Icon name="monitor" />
            <span class="what">
              <span class="name">{connection.name}</span>
              <!-- `paired` is an instant; friendlyDate wants a day, and given
                   the instant it drew an invalid date and threw, taking the
                   whole list down with it. -->
              <span class="host"
                >{connection.host} · paired {friendlyDate(
                  isoDate(new Date(connection.paired)),
                )}</span
              >
            </span>
          </button>
          <button
            class="forget"
            onclick={() => app.forgetRemote(connection.id)}
            disabled={busy}
            title="Forget this computer">Forget</button
          >
        </li>
      {/each}
    </ul>
  {/if}

  <label class="linkfield">
    <span>Pairing link</span>
    <input
      type="text"
      bind:value={link}
      placeholder="jatayu://pair?host=…"
      spellcheck="false"
      autocomplete="off"
      disabled={busy}
      onkeydown={(e) => e.key === 'Enter' && connect()}
    />
  </label>

  <button class="primary" onclick={connect} disabled={!ready || busy}>
    {busy ? 'Connecting…' : 'Connect'}
  </button>

  <p class="caveat">
    Nothing is copied to this {mobileShell ? 'phone' : 'computer'}. Your entries, your key and your
    search index stay on the machine that holds them, so this {mobileShell ? 'phone' : 'window'} shows
    nothing at all when that machine is unreachable.
  </p>
</section>

<style>
  .connect {
    display: flex;
    flex-direction: column;
    gap: 0.9rem;
    max-width: 30rem;
  }

  /* Spaced from the panel above it as SharePanel spaces itself: Settings'
     own `section + section` rule does not reach a child component's. */
  .connect.in-settings {
    margin-top: var(--sp-6);
    gap: var(--sp-3);
    min-width: 0;
  }

  h2 {
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 600;
  }

  .lead,
  .caveat {
    margin: 0;
    color: var(--fg-muted);
    font-size: var(--text-sm);
    line-height: 1.55;
  }

  .caveat {
    border-top: 1px solid var(--border);
    padding-top: 0.75rem;
  }

  .known {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }

  .known li {
    display: flex;
    align-items: center;
    gap: 0.35rem;
  }

  .row {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.55rem 0.7rem;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    text-align: left;
    cursor: pointer;
  }

  .row:hover:not(:disabled) {
    border-color: var(--accent);
  }

  .what {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .name {
    font-weight: 600;
  }

  .host {
    color: var(--fg-muted);
    font-size: var(--text-xs);
  }

  .forget {
    padding: 0.4rem 0.6rem;
    border: none;
    background: none;
    color: var(--fg-muted);
    font-size: var(--text-xs);
    cursor: pointer;
  }

  .forget:hover:not(:disabled) {
    color: var(--danger);
  }

  /* Not `.field`: that is app.css's text box (a height, a border, a fill),
     and on this label it drew a box round the caption with the input
     spilling out of it and over the Connect button. */
  .linkfield {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    font-size: var(--text-sm);
  }

  .linkfield input {
    padding: 0.5rem 0.6rem;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    color: inherit;
    font-family: var(--font-mono, monospace);
    font-size: var(--text-sm);
  }

  .primary {
    align-self: flex-start;
    padding: 0.5rem 1.1rem;
    border: none;
    border-radius: var(--radius);
    background: var(--accent);
    color: var(--fg-on-accent);
    font-weight: 600;
    cursor: pointer;
  }

  .primary:disabled {
    opacity: 0.5;
    cursor: default;
  }

  /* A phone: fields at the size that keeps iOS from zooming into them (a
     rule here beats the shared one), and every press a finger wide. The
     button spans the card, the way a form's one action does on a phone. */
  :global([data-phone]) .connect {
    max-width: none;
  }
  :global([data-phone]) .linkfield input {
    min-height: 48px;
    font-size: max(16px, 1em);
  }
  :global([data-phone]) .primary {
    align-self: stretch;
    min-height: 48px;
  }
  :global([data-phone]) .row {
    min-height: 56px;
  }
  :global([data-phone]) .forget {
    min-width: 64px;
    min-height: 48px;
    font-size: var(--text-sm);
  }
  :global([data-phone]) .host {
    overflow-wrap: anywhere;
  }
</style>
