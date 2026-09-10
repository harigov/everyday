<script lang="ts">
  // Letting another program use this vault's tools, over MCP.
  //
  // The switch, the endpoint to paste into a client, and the token minted for
  // one. It sits in the Vault tab beside SharePanel, for the same reason that
  // one is there: this is a property of the vault rather than of this
  // desktop, and the trade belongs on this screen rather than in a document
  // somebody has to go looking for.
  //
  // What this panel deliberately is not: a device manager. An issued token is
  // a row in the same paired-device list a computer that shared this vault
  // appears in -- see the panel above -- and revoking it is that panel's
  // "Revoke" button, not a second one here. This draws only the switch, the
  // endpoint, and the one-time token, and leaves "who is still in" to the
  // panel that already answers it.
  import { api } from '../lib/api'
  import { app } from '../lib/state.svelte'
  import { notify } from '../lib/notify.svelte'
  import type { McpStatus } from '../lib/types'
  import Icon from './Icon.svelte'

  // The domains a token can be narrowed to, in `everyday_service::domains::
  // meta::scope_of`'s order, labelled the way the app bar names them rather
  // than with the raw scope string a settings pane has no business showing.
  // `all`, `admin` and `any` are not here on purpose: `all` is what ticking
  // every box below already means, and the other two are never something a
  // token is issued.
  const DOMAIN_SCOPES: { scope: string; label: string }[] = [
    { scope: 'journals', label: 'Journal' },
    { scope: 'notes', label: 'Notes' },
    { scope: 'tasks', label: 'Tasks' },
    { scope: 'calendars', label: 'Calendar' },
    { scope: 'library', label: 'Library' },
    { scope: 'trackers', label: 'Tracking' },
    { scope: 'purpose', label: 'Overview' },
    { scope: 'agent', label: 'Assistant' },
  ]

  let mcp = $state<McpStatus | null>(null)
  let busy = $state(false)
  let address = $state('')
  let port = $state('')
  // What the next token issued may reach. Every box ticked to start with --
  // that is what issuing a token has always done, and narrowing it is a
  // choice somebody makes on purpose, not the default they have to notice
  // and opt out of.
  let scopes = $state<Set<string>>(new Set(DOMAIN_SCOPES.map((d) => d.scope)))
  const allScopesTicked = $derived(scopes.size === DOMAIN_SCOPES.length)
  const noScopesTicked = $derived(scopes.size === 0)

  function toggleScope(scope: string, checked: boolean) {
    const next = new Set(scopes)
    if (checked) next.add(scope)
    else next.delete(scope)
    scopes = next
  }

  // The plaintext token, held only in this component's memory for as long as
  // this window stays open. It came back from one call, is shown once, and
  // is never asked for again -- reloading this pane loses it, which is the
  // point.
  let token = $state('')
  let copiedToken = $state(false)
  let copiedUrl = $state(false)

  async function load() {
    try {
      mcp = await api.mcpStatus()
      port = String(mcp.port)
    } catch {
      // MCP is unavailable on this build, or this window is looking at
      // another computer's vault. The panel simply does not draw; there is
      // nothing useful to say about it.
      mcp = null
    }
  }
  void load()

  async function run(work: () => Promise<McpStatus>) {
    busy = true
    try {
      mcp = await work()
    } catch (e) {
      notify.error(e instanceof Error ? e.message : String(e))
    } finally {
      busy = false
    }
  }

  const toggle = () =>
    run(() =>
      mcp?.running
        ? api.mcpStop()
        : api.mcpStart({ address: address || null, port: port ? Number(port) : null }),
    )

  const endpoint = $derived(mcp?.running && mcp.address ? `http://${mcp.address}/mcp` : '')

  async function copyEndpoint() {
    if (!endpoint) return
    await navigator.clipboard.writeText(endpoint)
    copiedUrl = true
    setTimeout(() => (copiedUrl = false), 2000)
  }

  async function issueToken() {
    if (noScopesTicked) return
    busy = true
    try {
      // `all` when every box is ticked, rather than the same eight scopes
      // spelt out -- that is the grant a token has always been issued, and
      // leaving every box ticked should read as "everything", not as a list
      // that happens to cover everything today. Otherwise exactly what is
      // ticked: `mcp_issue_token` never widens a narrower list back to
      // `all` on its own.
      token = await api.mcpIssueToken(allScopesTicked ? ['all'] : Array.from(scopes))
      mcp = await api.mcpStatus()
    } catch (e) {
      notify.error(e instanceof Error ? e.message : String(e))
    } finally {
      busy = false
    }
  }

  async function copyToken() {
    if (!token) return
    await navigator.clipboard.writeText(token)
    copiedToken = true
    setTimeout(() => (copiedToken = false), 2000)
  }
</script>

{#if mcp && !app.remote}
  <section>
    <span class="eyebrow">Let another agent in</span>

    <label class="toggle">
      <input type="checkbox" checked={mcp.running} disabled={busy} onchange={toggle} />
      <span>
        <b>Let an AI agent use this vault</b>
        <small>
          Claude Code, Claude Desktop or anything else that speaks the Model Context Protocol can
          read a day, add a task or write a note. The agent on the other end is somebody else's
          program, and what it reads goes into that program's context.
        </small>
      </span>
    </label>

    {#if !mcp.running}
      <label class="field-row">
        <span>Answer on</span>
        <select bind:value={address} disabled={busy}>
          <option value="">This computer only</option>
          {#each mcp.addresses as a (a)}
            <option value={a}>{a}</option>
          {/each}
        </select>
      </label>
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
    {/if}

    {#if mcp.running}
      <p class="hint">
        Answering on <code>{mcp.address}</code>. A locked vault offers it nothing --
        <code>tools/list</code> comes back empty until this vault is unlocked, and a client that asked
        to hear about changes picks the catalogue up the moment it is, with no restart.
      </p>

      <div class="row">
        <code class="link">{endpoint}</code>
        <button class="btn" onclick={copyEndpoint}>{copiedUrl ? 'Copied' : 'Copy'}</button>
      </div>

      {#if token}
        <div class="token">
          <p class="hint">
            This is shown once. Paste it into the client's configuration now -- it will not be shown
            again, and there is no way to ask for it a second time except issuing a new one.
          </p>
          <code class="link">{token}</code>
          <div class="row">
            <button class="btn" onclick={copyToken}>{copiedToken ? 'Copied' : 'Copy token'}</button>
          </div>
        </div>
      {:else}
        <div class="scopes">
          <span class="eyebrow">What it can reach</span>
          <div class="scope-grid">
            {#each DOMAIN_SCOPES as d (d.scope)}
              <label class="scope">
                <input
                  type="checkbox"
                  checked={scopes.has(d.scope)}
                  disabled={busy}
                  onchange={(e) => toggleScope(d.scope, e.currentTarget.checked)}
                />
                {d.label}
              </label>
            {/each}
          </div>
          <div>
            <button class="btn" disabled={busy || noScopesTicked} onclick={issueToken}>
              <Icon name="sparkle" /> Issue a token…
            </button>
            {#if noScopesTicked}
              <p class="hint">
                Tick at least one app -- a token has to reach something, and an empty list is
                refused rather than quietly handed the whole vault.
              </p>
            {/if}
            {#if mcp.hasToken}
              <p class="hint">
                A token has already been issued for this vault. It is the row named "MCP client" in
                the paired list above; issuing another adds a second one rather than replacing it.
              </p>
            {/if}
          </div>
        </div>
      {/if}

      <label class="toggle">
        <input
          type="checkbox"
          checked={mcp.allowDestructive}
          disabled={busy}
          onchange={(e) => run(() => api.mcpSetDestructive(e.currentTarget.checked))}
        />
        <span>
          <b>Let it delete things</b>
          <small>
            Off by default. A tool that removes something is left out of the list entirely rather
            than offered and refused. Turn this on and deletes are approved in the client's own
            interface instead of this one -- Claude Code and Claude Desktop both ask before running
            a tool that removes something.
          </small>
        </span>
      </label>
    {/if}
  </section>
{/if}

<style>
  .field-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: var(--text-sm);
  }

  .field-row select,
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

  .token {
    display: flex;
    flex-direction: column;
    gap: 0.45rem;
    padding: 0.75rem;
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .scopes {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .scope-grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 0.3rem 0.75rem;
  }

  .scope {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    font-size: var(--text-sm);
  }

  .row {
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }
</style>
