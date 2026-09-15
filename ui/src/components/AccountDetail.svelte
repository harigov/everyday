<script lang="ts">
  // One account, in full: its display name, its identities, which services
  // it provides, an attachment cap, signing in again, and what agents may
  // do with it -- everything Settings → Accounts lets you change after the
  // add-account sheet has done its one job of creating the record.
  //
  // Two save rhythms, and they are different on purpose. The fields below --
  // name, identities, services, the cap -- are a draft with an explicit
  // Save, the same as About You: a half-typed signature must never be
  // written mid-keystroke. The permission grid applies each switch the
  // moment it is ticked, the same as MCP's own "let it delete things" --
  // "ticking one switch in Settings → Accounts is one call," per
  // `set_agent_access`'s own doc, not a form waiting to be submitted.

  import { agent } from '../lib/agent.svelte'
  import { api } from '../lib/api'
  import { accounts, beginAccountSignIn, type OAuthSignInHandle } from '../lib/accounts.svelte'
  import { humanBytes } from '../lib/format'
  import { PROVIDER_LABELS, providerLabel, statusLabel } from '../lib/accounts'
  import { errorMessage } from '../lib/errors'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import { openExternal } from '../lib/open-external'
  import type { Account, AccountId, AgentCallerKind, AgentMailAccess, Identity } from '../lib/types'
  import AgentAccessGrid from './AgentAccessGrid.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'

  let { id, onclose }: { id: AccountId; onclose: () => void } = $props()

  void accounts.load()
  void agent.loadSettings()

  const original = $derived(accounts.account(id))
  const providerName = $derived(providerLabel(agent.settings?.providerConfig.baseUrl ?? null))

  interface Draft {
    displayName: string
    identities: Identity[]
    services: { mail: boolean; calendar: boolean }
    capEnabled: boolean
    capMb: number
  }

  let draft = $state<Draft | null>(null)
  // Populated once, the moment the account is first known -- a later change
  // to the underlying record (another window's edit) does not silently
  // discard whatever is half-typed here.
  $effect(() => {
    if (!draft && original) {
      draft = {
        displayName: original.displayName,
        identities: original.identities.map((i) => ({ ...i })),
        services: { ...original.services },
        capEnabled: original.attachmentCapBytes != null,
        capMb: original.attachmentCapBytes
          ? Math.round(original.attachmentCapBytes / 1_048_576)
          : 25,
      }
    }
  })

  let saving = $state(false)
  let notice = $state<string | null>(null)
  let pendingRemove = $state(false)

  async function save() {
    if (!draft || !original) return
    saving = true
    notice = null
    try {
      // `$state.snapshot`: `...original` spreads a reactive record straight
      // from the store's list, and its nested fields (`imap`, `auth`,
      // `status`, the access grids) are Svelte proxies. See
      // `AddAccount.svelte`'s matching note -- unsnapshotted, this account
      // would save fine and then fail the next `list_accounts` silently.
      const account: Account = $state.snapshot({
        ...original,
        displayName: draft.displayName.trim() || original.address,
        identities: draft.identities
          .map((i) => ({ ...i, name: i.name.trim(), address: i.address.trim() }))
          .filter((i) => i.address),
        services: { ...draft.services },
        attachmentCapBytes: draft.capEnabled ? draft.capMb * 1_048_576 : null,
      })
      await api.saveAccount(account)
      await accounts.refresh()
      notice = 'Saved.'
    } catch (e) {
      notice = errorMessage(e)
    } finally {
      saving = false
    }
  }

  function addIdentity() {
    if (!draft || !original) return
    draft.identities = [
      ...draft.identities,
      { name: '', address: original.address, signatureHtml: '' },
    ]
  }

  function removeIdentity(index: number) {
    if (!draft) return
    draft.identities = draft.identities.filter((_, i) => i !== index)
  }

  // ── Signing in again ─────────────────────────────────────────────────

  type SignInPhase = 'idle' | 'clientSecret' | 'waiting' | 'error' | 'cancelled' | 'success'
  let signInPhase = $state<SignInPhase>('idle')
  let signInError = $state<string | null>(null)
  let clientSecretInput = $state('')
  let passwordInput = $state('')
  let signIn = $state<OAuthSignInHandle | null>(null)
  let openedInBrowser = $state(true)

  async function runOAuthSignIn() {
    if (!original || original.auth.type !== 'oAuth') return
    signInPhase = 'waiting'
    signInError = null
    try {
      signIn = await beginAccountSignIn(original.id, {
        authUrl: original.auth.authUrl,
        tokenUrl: original.auth.tokenUrl,
        clientId: original.auth.clientId,
        clientSecret: clientSecretInput.trim() || undefined,
        // A copy, not the reactive array itself -- see the note on `save()`.
        scopes: [...original.auth.scopes],
        loginHint: original.address,
      })
    } catch (e) {
      signInError = errorMessage(e)
      signInPhase = 'error'
      return
    }
    openedInBrowser = await openExternal(signIn.url)
    try {
      await signIn.finished
      await accounts.refresh()
      signInPhase = 'success'
    } catch (e) {
      // See `AddAccount.svelte`'s note: `signInPhase` can move to
      // `'cancelled'` from `cancelSignIn` while this await is in flight.
      if ((signInPhase as SignInPhase) !== 'cancelled') {
        signInError = errorMessage(e)
        signInPhase = 'error'
      }
    }
  }

  function startSignIn() {
    if (!original) return
    if (original.auth.type === 'oAuth') {
      const preset = accounts.preset(original.provider)
      clientSecretInput = ''
      signInPhase = preset?.needsClientSecret ? 'clientSecret' : 'waiting'
      if (signInPhase === 'waiting') void runOAuthSignIn()
    } else {
      passwordInput = ''
      signInPhase = 'waiting'
    }
  }

  function cancelSignIn() {
    signIn?.cancel()
    signInPhase = 'cancelled'
  }

  async function submitPassword() {
    if (!original || !passwordInput.trim()) return
    signInError = null
    try {
      await api.saveAccountPassword(original.id, passwordInput)
      // See `AddAccount.svelte`'s note: `save_account_password` alone does
      // not move the status to `Ok`, so this does the matching write --
      // snapshotted for the same reason as `save()` above.
      await api.saveAccount($state.snapshot({ ...original, status: { type: 'ok' } }))
      await accounts.refresh()
      signInPhase = 'success'
    } catch (e) {
      signInError = errorMessage(e)
      signInPhase = 'error'
    }
  }

  // ── Removing ─────────────────────────────────────────────────────────

  async function confirmRemove() {
    pendingRemove = false
    if (!original) return
    const ok = await accounts.remove(original.id)
    if (ok) onclose()
  }
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape' && signInPhase !== 'waiting') onclose()
  }}
/>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="scrim" onclick={() => signInPhase !== 'waiting' && onclose()}></div>
<div class="sheet wide" role="dialog" aria-modal="true" aria-label="Account" use:trapFocus>
  {#if !original || !draft}
    <p class="hint">Loading…</p>
  {:else}
    {@const st = statusLabel(original.status)}
    <header>
      <div>
        <h2>{original.address}</h2>
        <p class="sub">
          {accounts.preset(original.provider)?.label ?? PROVIDER_LABELS[original.provider]}
        </p>
      </div>
      <button class="ghost" onclick={onclose} title="Close"><Icon name="close" size={16} /></button>
    </header>

    <p class="status tone-{st.tone}">
      <span class="dot"></span>
      {st.label}{#if st.detail}<span> — {st.detail}</span>{/if}
    </p>

    <section>
      <label class="field-label" for="detail-name">Display name</label>
      <input id="detail-name" class="field" bind:value={draft.displayName} />
    </section>

    <section>
      <span class="eyebrow">Identities</span>
      <p class="hint">A second address this account can send as, with its own signature.</p>
      {#each draft.identities as identity, i (i)}
        <div class="identity">
          <div class="row">
            <input class="field" placeholder="Name" bind:value={identity.name} />
            <input class="field" placeholder="Address" bind:value={identity.address} />
            <button class="ghost" title="Remove this identity" onclick={() => removeIdentity(i)}>
              <Icon name="trash" size={14} />
            </button>
          </div>
          <textarea
            class="field sig"
            placeholder="Signature"
            rows="3"
            bind:value={identity.signatureHtml}
          ></textarea>
        </div>
      {/each}
      <div>
        <button class="btn" onclick={addIdentity}
          ><Icon name="plus" size={13} /> Add an identity</button
        >
      </div>
    </section>

    <section>
      <span class="eyebrow">Services</span>
      <div class="services">
        <label class="check">
          <input type="checkbox" bind:checked={draft.services.mail} />
          Mail
        </label>
        <label class="check">
          <input type="checkbox" bind:checked={draft.services.calendar} />
          Calendar
        </label>
      </div>
    </section>

    <section>
      <span class="eyebrow">Attachments</span>
      <label class="toggle">
        <input type="checkbox" bind:checked={draft.capEnabled} />
        <span>
          <b>Cap how large an attachment is kept</b>
          <small>
            Off by default: every attachment is kept in full. On, anything larger than the limit is
            left on the server instead of stored here.
          </small>
        </span>
      </label>
      {#if draft.capEnabled}
        <div class="row cap-row">
          <input class="field cap" type="number" min="1" bind:value={draft.capMb} />
          <span class="hint">MB ({humanBytes(draft.capMb * 1_048_576)})</span>
        </div>
      {/if}
    </section>

    {#if notice}<p class="notice">{notice}</p>{/if}
    <div class="sheet-row">
      <span class="spacer"></span>
      <button class="btn btn-primary" disabled={saving} onclick={save}>
        {saving ? 'Saving…' : 'Save changes'}
      </button>
    </div>

    <section>
      <span class="eyebrow">Signing in</span>
      {#if signInPhase === 'idle'}
        <div>
          <button class="btn" onclick={startSignIn}>Sign in again</button>
        </div>
      {:else if signInPhase === 'clientSecret'}
        <p class="hint">
          Google and Microsoft do not hand a client secret back once it is set the first time --
          type it again to continue.
        </p>
        <input
          class="field"
          type="password"
          placeholder="Client secret"
          bind:value={clientSecretInput}
          use:focusOnMount
        />
        <div class="sheet-row">
          <span class="spacer"></span>
          <button class="btn" onclick={() => (signInPhase = 'idle')}>Cancel</button>
          <button class="btn btn-primary" onclick={() => void runOAuthSignIn()}>Continue</button>
        </div>
      {:else if signInPhase === 'waiting' && original.auth.type === 'password'}
        <input
          class="field"
          type="password"
          placeholder="Password"
          bind:value={passwordInput}
          autocomplete="new-password"
          use:focusOnMount
        />
        <div class="sheet-row">
          <span class="spacer"></span>
          <button class="btn" onclick={() => (signInPhase = 'idle')}>Cancel</button>
          <button class="btn btn-primary" disabled={!passwordInput.trim()} onclick={submitPassword}>
            Save password
          </button>
        </div>
      {:else if signInPhase === 'waiting'}
        <p class="hint">
          {#if openedInBrowser}
            Finish signing in in the browser window that just opened.
          {:else if signIn}
            This build cannot open a browser itself. Open this link and finish signing in there:
          {/if}
        </p>
        {#if signIn && !openedInBrowser}
          <code class="link">{signIn.url}</code>
        {/if}
        <div class="sheet-row">
          <span class="spacer"></span>
          <button class="btn" onclick={cancelSignIn}>Cancel</button>
        </div>
      {:else if signInPhase === 'success'}
        <p class="hint">Signed in.</p>
      {:else if signInPhase === 'cancelled'}
        <p class="hint">Cancelled.</p>
        <div>
          <button class="btn" onclick={startSignIn}>Try again</button>
        </div>
      {:else if signInPhase === 'error'}
        <p class="error">{signInError}</p>
        <div>
          <button class="btn" onclick={startSignIn}>Try again</button>
        </div>
      {/if}
    </section>

    <section>
      <span class="eyebrow">What agents may do</span>
      <AgentAccessGrid
        assistantAccess={original.assistantAccess}
        mcpAccess={original.mcpAccess}
        {providerName}
        acknowledged={original.assistantProviderAcknowledged === providerName}
        onchange={(caller: AgentCallerKind, access: AgentMailAccess) =>
          void accounts.setAccess(original.id, caller, access)}
        onacknowledge={(ack: boolean) =>
          void api
            .saveAccount(
              $state.snapshot({
                ...original,
                assistantProviderAcknowledged: ack ? providerName : null,
              }),
            )
            .then(() => accounts.refresh())}
      />
    </section>

    <section>
      <span class="eyebrow">Remove</span>
      <p class="hint">
        Removes this account and deletes the local copy of its mail. Nothing changes on the server
        itself.
      </p>
      <div>
        <button class="btn btn-danger" onclick={() => (pendingRemove = true)}
          >Remove account…</button
        >
      </div>
    </section>
  {/if}
</div>

{#if pendingRemove && original}
  <ConfirmDialog
    title={'Remove ' + original.address + '?'}
    detail="The local copy of its mail is deleted from this vault. Nothing changes on the server itself -- signing in again later starts a fresh copy."
    confirmLabel="Remove account"
    onconfirm={confirmRemove}
    oncancel={() => (pendingRemove = false)}
  />
{/if}

<style>
  .wide {
    width: min(560px, calc(100vw - var(--sp-8)));
    max-height: calc(100vh - var(--sp-8));
    overflow-y: auto;
  }

  header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--sp-3);
    margin-bottom: var(--sp-3);
  }
  h2 {
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
    overflow-wrap: anywhere;
  }
  .sub {
    margin: 2px 0 0;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .ghost {
    display: grid;
    place-items: center;
    flex: none;
    width: 28px;
    height: 28px;
    border-radius: var(--radius-sm);
    color: var(--fg-subtle);
  }
  .ghost:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .status {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0 0 var(--sp-4);
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .status .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--fg-faint);
  }
  .status.tone-warn .dot,
  .status.tone-error .dot {
    background: var(--danger);
  }
  .status.tone-ok .dot {
    background: var(--accent);
  }

  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    margin-bottom: var(--sp-4);
  }

  .row {
    display: flex;
    gap: var(--sp-2);
    align-items: center;
  }
  .row .field {
    flex: 1;
  }

  .identity {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
  }
  .sig {
    min-height: 60px;
    padding: var(--sp-2) var(--sp-3);
    resize: vertical;
    font: inherit;
  }

  .services {
    display: flex;
    gap: var(--sp-4);
  }
  .check {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-sm);
    cursor: pointer;
  }

  .toggle {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
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
  }
  .toggle b {
    font-size: var(--text-base);
    font-weight: 600;
  }
  .toggle small {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    line-height: var(--leading-normal);
  }

  .cap-row {
    margin-top: var(--sp-1);
  }
  .cap {
    width: 88px;
  }

  .hint {
    margin: 0;
    color: var(--fg-subtle);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
  }

  .notice {
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }

  .link {
    display: block;
    overflow-wrap: anywhere;
    font-size: var(--text-xs);
    color: var(--fg-muted);
    background: var(--bg-sunken);
    padding: 0.4rem 0.5rem;
    border-radius: 4px;
  }
</style>
