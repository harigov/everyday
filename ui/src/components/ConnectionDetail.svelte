<script lang="ts">
  // One connection: how it is doing, who may use it, where it points, how it
  // signs in, and what the assistant may do with each of its tools.
  //
  // Two kinds of change live here, and they are kept apart on purpose. The
  // switches and each tool's policy are choices about *use*, and apply the
  // moment they are made, from what is saved -- a switch must not carry along
  // a half-typed URL. The name, address and credentials are a draft with a
  // Save, like Models', because saving can drop the live client and a
  // keystroke must not. The draft is copied once, when this opens, and never
  // re-seeded from the store while it is being edited; saving seeds it again.

  import { onMount, untrack } from 'svelte'
  import { connections } from '../lib/connections.svelte'
  import { cleanPatch, statusLabel, usesOAuth } from '../lib/connections'
  import { relativeTime } from '../lib/format'
  import type { McpConnection, ConnectionSecretsPatch } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import ConnectionFields from './ConnectionFields.svelte'
  import ConnectionTools from './ConnectionTools.svelte'
  import Icon from './Icon.svelte'
  import SignInLink from './SignInLink.svelte'

  let {
    id,
    fresh = false,
    onclose,
  }: {
    id: string
    /** Just added: try it once, so its tools are there to look at. */
    fresh?: boolean
    onclose: () => void
  } = $props()

  const view = $derived(connections.find(id))
  const busy = $derived(connections.busy[id])
  const pending = $derived(connections.pending?.id === id ? connections.pending : null)
  const oauth = $derived(view ? usesOAuth(view) : false)
  const label = $derived(view ? statusLabel(view.status) : '')

  // ── The draft ───────────────────────────────────────────────────────

  function copyOf(c: McpConnection): McpConnection {
    return structuredClone($state.snapshot(c))
  }

  let draft = $state<McpConnection | null>(untrack(() => (view ? copyOf(view.connection) : null)))
  let secrets = $state<ConnectionSecretsPatch>({})
  /** Bumped when the draft is seeded again, so the fields start afresh. */
  let epoch = $state(0)
  let saving = $state(false)
  let saved = $state(false)
  let savedTimer: ReturnType<typeof setTimeout> | null = null

  const dirty = $derived.by(() => {
    if (!view || !draft) return false
    const c = view.connection
    return (
      draft.name !== c.name ||
      JSON.stringify($state.snapshot(draft.transport)) !== JSON.stringify(c.transport) ||
      JSON.stringify($state.snapshot(draft.auth)) !== JSON.stringify(c.auth) ||
      Object.keys(cleanPatch($state.snapshot(secrets))).length > 0
    )
  })
  const nameProblem = $derived(
    draft && draft.name.trim() === '' ? 'A connection needs a name.' : null,
  )

  function reseed() {
    if (!view) return
    draft = copyOf(view.connection)
    secrets = {}
    epoch++
  }

  async function save() {
    if (!view || !draft || nameProblem || saving) return
    saving = true
    saved = false
    try {
      // What is saved, with only the draft's three fields laid over it: the
      // switches and policies may have moved since this was opened.
      const next: McpConnection = {
        ...copyOf(view.connection),
        name: draft.name.trim(),
        transport: structuredClone($state.snapshot(draft.transport)),
        auth: structuredClone($state.snapshot(draft.auth)),
      }
      const patch = cleanPatch($state.snapshot(secrets))
      if (!(await connections.update(next))) return
      if (Object.keys(patch).length > 0 && !(await connections.setSecrets(id, patch))) return
      reseed()
      saved = true
      if (savedTimer) clearTimeout(savedTimer)
      savedTimer = setTimeout(() => (saved = false), 3000)
    } finally {
      saving = false
    }
  }

  function discard() {
    reseed()
  }

  // ── Use ─────────────────────────────────────────────────────────────

  function setSwitch(change: Partial<Pick<McpConnection, 'enabled' | 'unattended'>>) {
    if (!view) return
    void connections.update({ ...copyOf(view.connection), ...change })
  }

  async function removeSecret(patch: ConnectionSecretsPatch) {
    await connections.setSecrets(id, patch)
  }

  // ── Checking and signing in ─────────────────────────────────────────

  async function check() {
    await connections.check(id)
  }

  async function signIn() {
    const ok = await connections.signIn(id)
    // A fresh sign-in is the first time its tools can be listed.
    if (ok && (connections.find(id)?.state.tools.length ?? 0) === 0) await connections.check(id)
  }

  async function signOut() {
    await connections.signOut(id)
  }

  onMount(() => {
    // A connection that signs in with a browser waits for the person to
    // press Sign in, which is the next thing on screen; any other can be
    // tried at once.
    if (fresh && view && !usesOAuth(view)) void connections.check(id)
    return () => {
      if (savedTimer) clearTimeout(savedTimer)
    }
  })

  // ── Removing ────────────────────────────────────────────────────────

  let confirmingRemove = $state(false)

  async function remove() {
    confirmingRemove = false
    if (await connections.remove(id)) onclose()
  }

  function when(iso: string | null | undefined): string {
    return iso ? relativeTime(iso) : ''
  }
</script>

<section class="stack">
  <div class="top">
    <button class="back" onclick={onclose}>
      <span class="turn"><Icon name="chevron" size={13} /></span> All connections
    </button>
  </div>

  {#if !view || !draft}
    <p class="hint">This connection is gone.</p>
  {:else}
    {@const st = view.state}
    <div class="titleline">
      <span class="pane-title">{view.connection.name}</span>
      <span class="status tone-{view.status}">
        <span class="dot"></span>{label}
      </span>
      {#if busy}<span class="spinner" role="status" aria-label="Working"></span>{/if}
    </div>

    {#if connections.error}<p class="error" role="alert">{connections.error}</p>{/if}

    <!-- How it is doing -->
    <div class="block">
      {#if st.lastError}
        <p class="problem" role="status">{st.lastError}</p>
      {/if}
      {#if st.server}
        <p class="hint">
          {st.server.name}
          {st.server.version}{#if st.lastOk}, last reached {when(st.lastOk)}{/if}.
        </p>
      {:else if !st.lastError}
        <p class="hint">Not reached yet.</p>
      {/if}
      <div class="row">
        <button class="btn btn-outline" disabled={!!busy || dirty} onclick={() => void check()}>
          {busy === 'checking' ? 'Checking…' : 'Check connection'}
        </button>
        {#if dirty}<span class="hint">Save your changes first.</span>{/if}
      </div>
    </div>

    <!-- Sign-in, for a service that has a browser to sign in with -->
    {#if oauth}
      <section class="block" aria-labelledby="conn-signin">
        <span class="eyebrow" id="conn-signin">Sign in</span>
        {#if pending}
          <p class="hint">Waiting for you to finish signing in, in your browser…</p>
          {#if pending.showUrl}
            <p class="hint">
              The browser could not be opened from here. Open this address yourself:
            </p>
            <SignInLink url={pending.url} />
          {/if}
          <div class="row">
            <button class="btn" onclick={() => void connections.cancelSignIn()}>Cancel</button>
          </div>
        {:else if view.secrets.signedIn}
          <p class="hint">Signed in.</p>
          <div class="row">
            <button
              class="btn btn-outline"
              disabled={!!busy || dirty}
              onclick={() => void signIn()}
            >
              Sign in again
            </button>
            <button class="btn btn-ghost-danger" disabled={!!busy} onclick={() => void signOut()}>
              Sign out
            </button>
          </div>
        {:else}
          <p class="hint">
            {st.needsSignIn
              ? 'Signing in again is needed before this can be used.'
              : 'Not signed in yet.'}
          </p>
          <div class="row">
            <button
              class="btn btn-primary"
              disabled={!!busy || dirty}
              onclick={() => void signIn()}
            >
              Sign in
            </button>
            {#if dirty}<span class="hint">Save your changes first.</span>{/if}
          </div>
        {/if}
      </section>
    {/if}

    <!-- Who may use it -->
    <section class="block" aria-labelledby="conn-use">
      <span class="eyebrow" id="conn-use">Use</span>
      <label class="toggle">
        <input
          type="checkbox"
          checked={view.connection.enabled}
          onchange={(e) => setSwitch({ enabled: e.currentTarget.checked })}
        />
        <span>
          <b>Let the assistant use it</b>
          <small
            >Off, it is not offered to the assistant at all, and its tools are not searched.</small
          >
        </span>
      </label>
      <label class="toggle" class:disabled={!view.connection.enabled}>
        <input
          type="checkbox"
          checked={view.connection.unattended}
          disabled={!view.connection.enabled}
          onchange={(e) => setSwitch({ unattended: e.currentTarget.checked })}
        />
        <span>
          <b>Routines and background jobs may use it too</b>
          <small>
            With nobody there to ask, a tool set to Ask is refused. What a run reads from this
            service can be written by strangers, and it acts as you.
          </small>
        </span>
      </label>
    </section>

    <!-- Where it points and how it signs in -->
    <section class="block" aria-labelledby="conn-settings">
      <span class="eyebrow" id="conn-settings">Settings</span>
      <div class="setting">
        <label class="caption" for="conn-name">Name</label>
        <input id="conn-name" class="field" bind:value={draft.name} autocomplete="off" />
        <span class="hint">The assistant refers to the connection by this name.</span>
      </div>
      {#key epoch}
        <ConnectionFields
          bind:transport={draft.transport}
          bind:auth={draft.auth}
          bind:secrets
          set={view.secrets}
          onremovesecret={(patch: ConnectionSecretsPatch) => void removeSecret(patch)}
        />
      {/key}
      {#if nameProblem}<p class="problem">{nameProblem}</p>{/if}
      <div class="row">
        <button
          class="btn btn-primary"
          disabled={!dirty || !!nameProblem || saving}
          onclick={() => void save()}
        >
          {saving ? 'Saving…' : 'Save'}
        </button>
        {#if dirty}<button class="btn" disabled={saving} onclick={discard}>Discard changes</button
          >{/if}
        {#if saved}<span class="hint" role="status">Saved.</span>{/if}
      </div>
    </section>

    <!-- What it may do -->
    <section class="block" aria-labelledby="conn-tools">
      <span class="eyebrow" id="conn-tools">Tools</span>
      <p class="hint">
        Allow runs a tool without asking, Ask stops for your answer each time, and Off keeps it from
        the assistant altogether. A tool left alone follows its own hints: read-only ones run, the
        rest ask.
      </p>
      <ConnectionTools {view} />
      {#if st.server?.instructions}
        <details class="more">
          <summary>What the server says about itself</summary>
          <p class="instructions">{st.server.instructions}</p>
        </details>
      {/if}
    </section>

    <section class="block danger-zone">
      <button class="link danger" onclick={() => (confirmingRemove = true)}>
        <Icon name="trash" size={13} /> Remove this connection…
      </button>
    </section>
  {/if}
</section>

{#if confirmingRemove && view}
  <ConfirmDialog
    title={'Remove ' + view.connection.name + '?'}
    detail="Its sign-in and every credential stored for it are deleted from this vault. Nothing changes at the service itself; signing in again later starts afresh."
    confirmLabel="Remove connection"
    onconfirm={() => void remove()}
    oncancel={() => (confirmingRemove = false)}
  />
{/if}

<style>
  .stack {
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
    min-width: 0;
  }
  .block {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }
  .block > .hint,
  .block > .problem {
    margin: 0;
  }

  .top {
    display: flex;
  }
  .back {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-muted);
  }
  .back:hover {
    color: var(--fg);
  }
  .back .turn {
    display: grid;
    rotate: 180deg;
  }

  .titleline {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 0;
  }

  .row {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--sp-3);
    min-width: 0;
  }

  .instructions {
    margin: var(--sp-3) 0 0;
    padding: var(--sp-3);
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
    color: var(--fg-muted);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    user-select: text;
  }

  .danger-zone {
    padding-top: var(--sp-4);
    border-top: 1px solid var(--border);
    align-items: flex-start;
  }
  .link.danger {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    color: var(--danger);
  }

  /* A phone: the way back to the list is a finger tall. */
  :global([data-phone]) .back {
    min-height: 44px;
    padding-right: var(--sp-3);
  }
</style>
