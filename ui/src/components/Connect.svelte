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
  import { friendlyDate } from '../lib/format'
  import Icon from './Icon.svelte'

  let link = $state('')
  let busy = $state(false)

  const known = $derived(app.remotes)
  const ready = $derived(link.trim().startsWith('everyday://pair?'))

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

<section class="connect">
  <h2>Use a vault on another computer</h2>
  <p class="lead">
    Open <strong>Settings → Vault → Share on the network</strong> on the computer that holds your vault,
    then paste the link it shows here.
  </p>

  {#if known.length > 0}
    <ul class="known">
      {#each known as connection (connection.id)}
        <li>
          <button class="row" onclick={() => reconnect(connection.id)} disabled={busy}>
            <Icon name="monitor" />
            <span class="what">
              <span class="name">{connection.name}</span>
              <span class="host">{connection.host} · paired {friendlyDate(connection.paired)}</span>
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

  <label class="field">
    <span>Pairing link</span>
    <input
      type="text"
      bind:value={link}
      placeholder="everyday://pair?host=…"
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
    Nothing is copied to this computer. Your entries, your key and your search index stay on the
    machine that holds them, so this window shows nothing at all when that machine is unreachable.
  </p>
</section>

<style>
  .connect {
    display: flex;
    flex-direction: column;
    gap: 0.9rem;
    max-width: 30rem;
  }

  h2 {
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 600;
  }

  .lead,
  .caveat {
    margin: 0;
    color: var(--text-muted);
    font-size: var(--text-sm);
    line-height: 1.55;
  }

  .caveat {
    border-top: 1px solid var(--rule);
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
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    background: var(--surface);
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
    color: var(--text-muted);
    font-size: var(--text-xs);
  }

  .forget {
    padding: 0.4rem 0.6rem;
    border: none;
    background: none;
    color: var(--text-muted);
    font-size: var(--text-xs);
    cursor: pointer;
  }

  .forget:hover:not(:disabled) {
    color: var(--danger);
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    font-size: var(--text-sm);
  }

  .field input {
    padding: 0.5rem 0.6rem;
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    background: var(--surface);
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
    color: var(--on-accent, #fff);
    font-weight: 600;
    cursor: pointer;
  }

  .primary:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
