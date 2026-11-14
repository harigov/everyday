<script lang="ts">
  // Opening this vault in a browser on the tailnet.
  //
  // The switch, the address to open, and what is in the way when something
  // is. It sits in the Vault tab beside SharePanel and McpPanel because it is
  // the same kind of thing: a way into this vault from somewhere other than
  // this window. See `docs/plans/web-access.md`.
  //
  // Not a device list: a browser signs in with the vault password and is
  // signed out when the vault locks, so there is nothing to revoke. Locking
  // the vault is how every browser is sent away at once.
  import { api } from '../lib/api'
  import { app } from '../lib/state.svelte'
  import { notify } from '../lib/notify.svelte'
  import type { WebStatus } from '../lib/types'

  let web = $state<WebStatus | null>(null)
  let busy = $state(false)
  let port = $state('')
  let copied = $state(false)

  async function load() {
    try {
      web = await api.webStatus()
      port = String(web.port)
    } catch {
      // Unavailable on this build, or this window is looking at another
      // computer's vault. The panel does not draw.
      web = null
    }
  }
  void load()

  // While it is on, look again now and then: Tailscale coming up, a
  // certificate arriving and browsers signing in all change what this shows
  // without anybody touching it.
  $effect(() => {
    if (!web?.running) return
    const timer = setInterval(() => void load(), 5000)
    return () => clearInterval(timer)
  })

  async function toggle() {
    busy = true
    try {
      web = web?.running
        ? await api.webStop()
        : await api.webStart({ port: port ? Number(port) : null })
    } catch (e) {
      notify.error(e instanceof Error ? e.message : String(e))
    } finally {
      busy = false
    }
  }

  const url = $derived(web?.urls[0] ?? '')

  async function copy() {
    if (!url) return
    await navigator.clipboard.writeText(url)
    copied = true
    setTimeout(() => (copied = false), 2000)
  }

  const encrypted = $derived(app.status?.encrypted !== false)
</script>

{#if web && !app.remote}
  <section>
    <span class="eyebrow">Open in a browser</span>

    <label class="toggle">
      <input
        type="checkbox"
        checked={web.running}
        disabled={busy || (!web.running && !encrypted)}
        onchange={toggle}
      />
      <span>
        <b>Open this vault in a browser on your tailnet</b>
        <small>
          Your phone or another computer signed in to Tailscale as you can open this vault in a web
          browser, with the vault password. Nothing outside your tailnet can reach it, and every
          browser is signed out when the vault locks.
        </small>
      </span>
    </label>

    {#if !encrypted}
      <p class="hint">
        This vault has no password, so a browser would have nothing to sign in with. Give it one
        below first.
      </p>
    {/if}

    {#if !web.running}
      <label class="field-row">
        <span>Port</span>
        <input
          class="field port"
          type="number"
          min="1"
          max="65535"
          bind:value={port}
          disabled={busy}
        />
      </label>
      <p class="hint">
        Needs Tailscale on this computer with MagicDNS and HTTPS certificates turned on for your
        tailnet, and permission for this app to ask for a certificate: run
        <code>sudo tailscale set --operator=$USER</code> once.
      </p>
    {:else}
      {#if web.problem}
        <p class="problem">{web.problem}</p>
        <p class="hint">It tries again every minute.</p>
      {/if}

      {#if url}
        <div class="row">
          <code class="link">{url}</code>
          <button class="btn" onclick={copy}>{copied ? 'Copied' : 'Copy'}</button>
        </div>
        <p class="hint">
          {#if web.sessions === 0}
            No browser is signed in.
          {:else if web.sessions === 1}
            One browser is signed in.
          {:else}
            {web.sessions} browsers are signed in.
          {/if}
          Lock the vault to sign them all out.
        </p>
      {/if}
    {/if}
  </section>
{/if}

<style>
  /* See McpPanel's identical rule: a child component's section is not a
     literal sibling of SettingsView's, so it brings its own gap. */
  section {
    margin-top: var(--sp-6);
    /* And SettingsView's own column, which its scoped `section` rule does
       not reach into a child component to give. Without it the eyebrow ran
       on into the switch beside it. */
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }

  .field-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: var(--text-sm);
  }

  .field-row .port {
    flex: 1;
    padding: 0.35rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    color: inherit;
    font: inherit;
  }

  .link {
    display: block;
    flex: 1;
    overflow-wrap: anywhere;
    font-size: var(--text-xs);
    color: var(--fg-muted);
    background: var(--bg-sunken);
    padding: 0.35rem 0.45rem;
    border-radius: 4px;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }

  .problem {
    font-size: var(--text-sm);
    color: var(--danger, var(--fg));
  }

  /* The switch and its explanation, as every other settings panel draws
     them (SettingsView's own copy is scoped to that file and does not reach
     this one): the sentence under the title, not run on beside it. */
  .toggle {
    display: flex;
    gap: var(--sp-3);
    align-items: flex-start;
    min-width: 0;
    cursor: pointer;
  }
  .toggle input {
    flex: none;
    margin-top: 3px;
    accent-color: var(--accent);
  }
  .toggle span {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  .toggle b {
    font-size: var(--text-base);
    font-weight: 600;
  }
  .toggle small,
  .hint {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
    overflow-wrap: anywhere;
  }
</style>
