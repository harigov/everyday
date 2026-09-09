<script lang="ts">
  // Serving this vault to other computers.
  //
  // The switch, the link somebody carries to the other machine, and the list of
  // what has been let in. It is in the Vault tab rather than General because it
  // is a property of the vault and not of this desktop: another machine opening
  // the same Postgres vault has its own answer to all of it.
  //
  // The trade is stated on screen, not in a document. A hosted Postgres vault
  // sees ciphertext; a server sees plaintext, because it is the thing holding
  // the key. Somebody turning this on should be told that by the switch.
  import { api } from '../lib/api'
  import { app } from '../lib/state.svelte'
  import { notify } from '../lib/notify.svelte'
  import { friendlyDate } from '../lib/format'
  import type { ShareStatus } from '../lib/types'
  import Icon from './Icon.svelte'

  let share = $state<ShareStatus | null>(null)
  let busy = $state(false)
  let address = $state('')
  let copied = $state(false)

  async function load() {
    try {
      share = await api.shareStatus()
      address = share.addresses[0] ?? ''
    } catch {
      // Sharing is unavailable on this build or this window is remote. The
      // panel simply does not draw; there is nothing useful to say about it.
      share = null
    }
  }
  void load()

  async function run(work: () => Promise<ShareStatus>) {
    busy = true
    try {
      share = await work()
    } catch (e) {
      notify.error(e instanceof Error ? e.message : String(e))
    } finally {
      busy = false
    }
  }

  const toggle = () =>
    run(() =>
      share?.sharing
        ? api.shareStop()
        : api.shareStart({ address: address || null, port: share?.port ?? null }),
    )

  async function copyLink() {
    const url = share?.invitation?.url
    if (!url) return
    await navigator.clipboard.writeText(url)
    copied = true
    setTimeout(() => (copied = false), 2000)
  }
</script>

{#if share && !app.remote}
  <section>
    <span class="eyebrow">Share on the network</span>

    <label class="toggle">
      <input type="checkbox" checked={share.sharing} disabled={busy} onchange={toggle} />
      <span>
        <b>Let other computers use this vault</b>
        <small>
          Every Day on another machine connects to this one instead of opening a vault of its own.
          Nothing is copied there.
        </small>
      </span>
    </label>

    {#if !share.sharing && share.addresses.length > 1}
      <label class="field-row">
        <span>Answer on</span>
        <select bind:value={address} disabled={busy}>
          <option value="">Every network on this computer</option>
          {#each share.addresses as a (a)}
            <option value={a}>{a}</option>
          {/each}
        </select>
      </label>
    {/if}

    {#if share.sharing}
      <p class="hint">
        Answering on <code>{share.address}</code>.
        {#if share.listeners > 0}
          {share.listeners}
          {share.listeners === 1 ? 'computer is' : 'computers are'} connected.
        {/if}
      </p>

      {#if share.invitation}
        <div class="invite">
          <!-- eslint-disable-next-line svelte/no-at-html-tags -->
          <div class="qr">{@html share.invitation.qrSvg}</div>
          <div class="invite-what">
            <p class="hint">
              Paste this into Every Day on the other computer, or scan it. It works once, for five
              minutes.
            </p>
            <code class="link">{share.invitation.url}</code>
            <div class="row">
              <button class="btn" onclick={copyLink}>{copied ? 'Copied' : 'Copy link'}</button>
              <button class="btn" disabled={busy} onclick={() => run(api.cancelPairing)}>
                Cancel
              </button>
            </div>
          </div>
        </div>
      {:else}
        <div>
          <button class="btn" disabled={busy} onclick={() => run(api.newPairingCode)}>
            <Icon name="share" /> Add a computer…
          </button>
        </div>
      {/if}

      <label class="toggle">
        <input
          type="checkbox"
          checked={share.allowRemoteUnlock}
          disabled={busy}
          onchange={(e) => run(() => api.setRemoteUnlock(e.currentTarget.checked))}
        />
        <span>
          <b>Let a connected computer unlock this vault</b>
          <small>
            With this off, the password has to be typed on this machine. Turn it off for a laptop
            you leave at home and unlock in the morning.
          </small>
        </span>
      </label>
    {/if}

    {#if share.devices.length > 0}
      <div class="devices">
        <span class="eyebrow">Computers that have paired</span>
        {#each share.devices as device (device.id)}
          <div class="device" class:stale={device.expired}>
            <Icon name="monitor" />
            <span class="what">
              <b>{device.name}</b>
              <small>
                {device.expired
                  ? 'Not used for a month — it must pair again'
                  : `Last used ${friendlyDate(device.lastSeen)}`}
              </small>
            </span>
            <button
              class="btn danger"
              disabled={busy}
              onclick={() => run(() => api.revokeDevice(device.id))}
            >
              Revoke
            </button>
          </div>
        {/each}
      </div>
    {/if}

    <p class="caveat">
      A computer serving a vault can read it, because it is the one holding the key. That is a
      different trade from keeping a vault on a Postgres server, which only ever sees ciphertext.
      What crosses the network is encrypted with a certificate the other computer pins when it
      pairs.
    </p>
  </section>
{/if}

<style>
  .field-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: var(--text-sm);
  }

  .field-row select {
    flex: 1;
    padding: 0.35rem 0.5rem;
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    background: var(--surface);
    color: inherit;
    font: inherit;
  }

  .invite {
    display: flex;
    gap: 0.9rem;
    align-items: flex-start;
    padding: 0.75rem;
    border: 1px solid var(--rule);
    border-radius: var(--radius);
  }

  .qr {
    flex: none;
    width: 132px;
    height: 132px;
    border-radius: 6px;
    overflow: hidden;
  }

  .qr :global(svg) {
    width: 100%;
    height: 100%;
    display: block;
  }

  .invite-what {
    display: flex;
    flex-direction: column;
    gap: 0.45rem;
    min-width: 0;
  }

  .link {
    display: block;
    overflow-wrap: anywhere;
    font-size: var(--text-xs);
    color: var(--text-muted);
    background: var(--surface-sunken, transparent);
    padding: 0.35rem 0.45rem;
    border-radius: 4px;
  }

  .devices {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }

  .device {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.45rem 0.55rem;
    border: 1px solid var(--rule);
    border-radius: var(--radius);
  }

  .device.stale {
    opacity: 0.65;
  }

  .device .what {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .device small {
    color: var(--text-muted);
    font-size: var(--text-xs);
  }

  .caveat {
    margin: 0;
    padding-top: 0.6rem;
    border-top: 1px solid var(--rule);
    color: var(--text-muted);
    font-size: var(--text-xs);
    line-height: 1.55;
  }

  .btn.danger:hover:not(:disabled) {
    color: var(--danger);
  }

  .row {
    display: flex;
    gap: 0.4rem;
  }
</style>
