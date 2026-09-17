<script lang="ts">
  // Adding a mailbox account: pick a provider, sign in to it, and say what
  // the assistant and MCP may do with it before it is ever saved.
  //
  // Two quite different endings share one sheet. Google and Microsoft sign
  // in over OAuth, in the system browser, and end with tokens sealed onto
  // the account. Everyone else -- iCloud, Fastmail, Yahoo, a self-hosted
  // server -- signs in with a password typed here and stored straight away.
  // The account record itself is saved first, with `NeedsSignIn`, in both
  // cases: a credential that turns out to be wrong should still leave
  // something in the list to fix, not nothing at all.
  //
  // `agent.loadSettings` behind "What agents may do" is not a side effect of
  // opening the sheet touching the assistant -- it only reads the provider
  // sentence needs, so the permission grid can say who the mail would be
  // sent to before it saves anything.

  import { agent } from '../lib/agent.svelte'
  import { api } from '../lib/api'
  import {
    beginAccountSignIn,
    mintAccountId,
    accounts,
    type OAuthSignInHandle,
  } from '../lib/accounts.svelte'
  import {
    DEFAULT_AGENT_ACCESS,
    providerLabel,
    signInArgs,
    validateCustomEndpoint,
  } from '../lib/accounts'
  import { errorMessage } from '../lib/errors'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import { openExternal } from '../lib/open-external'
  import type {
    Account,
    AgentCallerKind,
    AgentMailAccess,
    Endpoint,
    MailProvider,
    Services,
  } from '../lib/types'
  import AgentAccessGrid from './AgentAccessGrid.svelte'
  import OAuthSetupGuide from './OAuthSetupGuide.svelte'
  import Icon from './Icon.svelte'

  let { onclose }: { onclose: () => void } = $props()

  void accounts.loadPresets()
  void agent.loadSettings()

  const providerName = $derived(providerLabel(agent.settings?.providerConfig.baseUrl ?? null))

  // Stable for the life of this sheet, so retrying a failed sign-in saves
  // over the same record rather than minting a second one. See
  // `mintAccountId`'s own doc for why this exists at all.
  const draftId = mintAccountId()

  let provider = $state<MailProvider>('google')
  const preset = $derived(accounts.preset(provider))

  let address = $state('')
  let displayName = $state('')
  let clientId = $state('')
  let clientSecret = $state('')
  let password = $state('')
  let customImap = $state<Endpoint>({ host: '', port: 993, security: 'tls' })
  let customSmtp = $state<Endpoint>({ host: '', port: 587, security: 'startTls' })
  let services = $state<Services>({ mail: true, calendar: false })
  let assistantAccess = $state<AgentMailAccess>({ ...DEFAULT_AGENT_ACCESS })
  let mcpAccess = $state<AgentMailAccess>({ ...DEFAULT_AGENT_ACCESS })
  let acknowledged = $state(false)

  // Provider-specific fields belong to the provider that asked for them; a
  // client id typed for Google must not be submitted for Microsoft because
  // nobody noticed the picker moved.
  $effect(() => {
    void provider
    clientId = ''
    clientSecret = ''
    password = ''
    if (preset) {
      customImap = { ...preset.imap }
      customSmtp = { ...preset.smtp }
    }
  })

  let formError = $state<string | null>(null)
  let busy = $state(false)

  type Phase = 'form' | 'waiting' | 'error' | 'cancelled' | 'success'
  let phase = $state<Phase>('form')
  let signIn = $state<OAuthSignInHandle | null>(null)
  let openedInBrowser = $state(true)

  function buildAccount(): Account {
    const now = new Date().toISOString()
    const trimmedAddress = address.trim()
    const auth: Account['auth'] = preset?.oauth
      ? {
          type: 'oAuth',
          clientId: clientId.trim(),
          authUrl: preset.oauth.authUrl,
          tokenUrl: preset.oauth.tokenUrl,
          scopes: signInArgs(preset.oauth, services, clientId.trim(), trimmedAddress).scopes,
        }
      : { type: 'password', username: trimmedAddress }
    // `$state.snapshot`, not a plain object literal: `imap`/`smtp` (from the
    // chosen preset or the Custom fields) and `assistantAccess`/`mcpAccess`
    // are reactive `$state` values, and a Svelte 5 proxy travels wherever a
    // reference to it goes -- including into this "plain" object, and from
    // there into `api.saveAccount`. The mock's `list_accounts` answers with
    // `structuredClone(accounts)`, and `structuredClone` cannot clone a
    // proxy, so an unsound account here does not fail loudly: it fails the
    // *next* read, silently, inside `handle()`. Snapshotting once, at the
    // one place this record is built, is cheaper than auditing every field.
    return $state.snapshot({
      id: draftId,
      provider,
      address: trimmedAddress,
      displayName: displayName.trim() || trimmedAddress,
      identities: [],
      imap: provider === 'custom' ? customImap : (preset?.imap ?? customImap),
      smtp: provider === 'custom' ? customSmtp : (preset?.smtp ?? customSmtp),
      caldav: preset?.caldav ?? null,
      auth,
      services: { ...services },
      assistantAccess,
      mcpAccess,
      assistantProviderAcknowledged: acknowledged ? providerName : null,
      attachmentCapBytes: null,
      status: { type: 'needsSignIn', reason: 'not yet signed in' },
      lastSyncedAt: null,
      createdAt: now,
      updatedAt: now,
    })
  }

  function addressError(): string | null {
    if (!address.trim().includes('@')) return 'Give it an email address.'
    if (provider === 'custom') return validateCustomEndpoint(customImap, customSmtp)
    return null
  }

  async function startOAuth() {
    const err = addressError()
    if (!err && !clientId.trim()) formError = 'Give it a client id.'
    else formError = err
    if (formError) return
    if (!preset?.oauth) return

    busy = true
    phase = 'waiting'
    const draft = buildAccount()
    try {
      await api.saveAccount(draft)
      await accounts.refresh()
    } catch (e) {
      formError = errorMessage(e)
      phase = 'form'
      busy = false
      return
    }

    try {
      signIn = await beginAccountSignIn(draft.id, {
        authUrl: preset.oauth.authUrl,
        tokenUrl: preset.oauth.tokenUrl,
        clientId: clientId.trim(),
        clientSecret: clientSecret.trim() || undefined,
        scopes: draft.auth.type === 'oAuth' ? draft.auth.scopes : [],
        loginHint: address.trim(),
      })
    } catch (e) {
      formError = errorMessage(e)
      phase = 'error'
      busy = false
      return
    }

    openedInBrowser = await openExternal(signIn.url)
    busy = false

    try {
      await signIn.finished
      await accounts.refresh()
      phase = 'success'
    } catch (e) {
      // `phase` can move to `'cancelled'` from `cancelSignIn`, a different
      // handler, while this await is in flight -- TS's flow narrowing does
      // not see across that, so the comparison is widened back to `Phase`.
      if ((phase as Phase) !== 'cancelled') {
        formError = errorMessage(e)
        phase = 'error'
      }
    }
  }

  function cancelSignIn() {
    signIn?.cancel()
    phase = 'cancelled'
  }

  async function copyUrl() {
    if (!signIn) return
    await navigator.clipboard.writeText(signIn.url)
  }

  async function submitPassword() {
    const err = addressError()
    if (err) {
      formError = err
      return
    }
    if (!password.trim()) {
      formError = 'Give it a password.'
      return
    }
    formError = null
    busy = true
    const draft = buildAccount()
    try {
      await api.saveAccount(draft)
      await api.saveAccountPassword(draft.id, password)
      await accounts.refresh()
      onclose()
    } catch (e) {
      formError = errorMessage(e)
    } finally {
      busy = false
    }
  }

  function submit() {
    if (preset?.oauth) void startOAuth()
    else void submitPassword()
  }
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape' && phase !== 'waiting') onclose()
  }}
/>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="scrim" onclick={() => phase !== 'waiting' && onclose()}></div>
<div class="sheet wide" role="dialog" aria-modal="true" aria-label="Add account" use:trapFocus>
  <h2>Add account</h2>

  {#if phase === 'waiting' || phase === 'error' || phase === 'cancelled' || phase === 'success'}
    <div class="oauth-status">
      {#if phase === 'waiting'}
        <p class="hint">
          {#if openedInBrowser}
            Finish signing in in the browser window that just opened.
          {:else}
            This build cannot open a browser itself. Open the link below and finish signing in
            there.
          {/if}
        </p>
        {#if signIn && !openedInBrowser}
          <div class="url-row">
            <code class="link">{signIn.url}</code>
            <button class="btn" onclick={copyUrl}>Copy</button>
          </div>
        {/if}
        <div class="sheet-row">
          <span class="spacer"></span>
          <button class="btn" onclick={cancelSignIn}>Cancel</button>
        </div>
      {:else if phase === 'success'}
        <p class="hint">Signed in. {address} is ready.</p>
        <div class="sheet-row">
          <span class="spacer"></span>
          <button class="btn btn-primary" onclick={onclose}>Done</button>
        </div>
      {:else if phase === 'cancelled'}
        <p class="hint">
          Sign-in cancelled. The account is saved and waiting -- try again, or close and sign in
          later from its details.
        </p>
        <div class="sheet-row">
          <span class="spacer"></span>
          <button class="btn" onclick={onclose}>Close</button>
          <button class="btn btn-primary" onclick={() => (phase = 'form')}>Try again</button>
        </div>
      {:else}
        <p class="error">{formError}</p>
        <div class="sheet-row">
          <span class="spacer"></span>
          <button class="btn" onclick={onclose}>Close</button>
          <button class="btn btn-primary" onclick={() => void startOAuth()}>Try again</button>
        </div>
      {/if}
    </div>
  {:else}
    <div class="providers" role="group" aria-label="Provider">
      {#each accounts.presets as p (p.provider)}
        <button
          class="provider"
          class:on={provider === p.provider}
          onclick={() => (provider = p.provider)}>{p.label}</button
        >
      {/each}
    </div>

    <div class="row">
      <div class="grow">
        <label class="field-label" for="acct-address">Address</label>
        <input
          id="acct-address"
          class="field"
          placeholder="you@example.com"
          spellcheck="false"
          autocomplete="off"
          bind:value={address}
          use:focusOnMount
        />
      </div>
      <div class="grow">
        <label class="field-label" for="acct-name"
          >Display name <span class="opt">optional</span></label
        >
        <input
          id="acct-name"
          class="field"
          placeholder={address || 'Taken from the address'}
          bind:value={displayName}
        />
      </div>
    </div>

    {#if preset?.oauth}
      <div class="row">
        <div class="grow">
          <label class="field-label" for="acct-client-id">Client id</label>
          <input
            id="acct-client-id"
            class="field"
            spellcheck="false"
            autocomplete="off"
            bind:value={clientId}
          />
        </div>
        {#if preset.needsClientSecret}
          <div class="grow">
            <label class="field-label" for="acct-client-secret">Client secret</label>
            <input
              id="acct-client-secret"
              class="field"
              type="password"
              autocomplete="off"
              bind:value={clientSecret}
            />
          </div>
        {/if}
      </div>

      <OAuthSetupGuide
        provider={provider === 'google' ? 'google' : 'microsoft'}
        calendar={services.calendar}
      />
    {:else}
      <div>
        <label class="field-label" for="acct-password">Password</label>
        <input
          id="acct-password"
          class="field"
          type="password"
          autocomplete="new-password"
          bind:value={password}
        />
      </div>
      {#if preset?.appPasswordHelpUrl}
        <p class="hint">
          Use an app password, not your ordinary one -- a second, single-purpose password this
          provider can issue and revoke on its own, without touching the one you sign in with
          everywhere else.
          <a href={preset.appPasswordHelpUrl} target="_blank" rel="noreferrer">How to get one</a>.
        </p>
      {/if}

      {#if provider === 'custom'}
        <div class="row">
          <div class="grow">
            <label class="field-label" for="acct-imap-host">Incoming (IMAP) host</label>
            <input
              id="acct-imap-host"
              class="field"
              spellcheck="false"
              bind:value={customImap.host}
            />
          </div>
          <div>
            <label class="field-label" for="acct-imap-port">Port</label>
            <input
              id="acct-imap-port"
              class="field port"
              type="number"
              bind:value={customImap.port}
            />
          </div>
          <div>
            <label class="field-label" for="acct-imap-sec">Security</label>
            <select id="acct-imap-sec" class="field" bind:value={customImap.security}>
              <option value="tls">TLS</option>
              <option value="startTls">STARTTLS</option>
            </select>
          </div>
        </div>
        <div class="row">
          <div class="grow">
            <label class="field-label" for="acct-smtp-host">Outgoing (SMTP) host</label>
            <input
              id="acct-smtp-host"
              class="field"
              spellcheck="false"
              bind:value={customSmtp.host}
            />
          </div>
          <div>
            <label class="field-label" for="acct-smtp-port">Port</label>
            <input
              id="acct-smtp-port"
              class="field port"
              type="number"
              bind:value={customSmtp.port}
            />
          </div>
          <div>
            <label class="field-label" for="acct-smtp-sec">Security</label>
            <select id="acct-smtp-sec" class="field" bind:value={customSmtp.security}>
              <option value="tls">TLS</option>
              <option value="startTls">STARTTLS</option>
            </select>
          </div>
        </div>
      {/if}
    {/if}

    <div class="services" role="group" aria-label="Services">
      <label class="check">
        <input type="checkbox" bind:checked={services.mail} />
        Mail
      </label>
      <label class="check">
        <input type="checkbox" bind:checked={services.calendar} />
        Calendar
      </label>
    </div>

    <p class="hint mcp-note">
      <Icon name="globe" size={13} />
      <span>
        External agents connected over MCP can read, draft, edit, move to Trash and archive this
        account's mail unless you turn that off below. Sending is off.
      </span>
    </p>

    <AgentAccessGrid
      {assistantAccess}
      {mcpAccess}
      {providerName}
      {acknowledged}
      onchange={(caller: AgentCallerKind, access: AgentMailAccess) => {
        if (caller === 'assistant') assistantAccess = access
        else mcpAccess = access
      }}
      onacknowledge={(ack: boolean) => (acknowledged = ack)}
    />

    {#if formError}<p class="error">{formError}</p>{/if}

    <div class="sheet-row">
      <span class="spacer"></span>
      <button class="btn" onclick={onclose}>Cancel</button>
      <button class="btn btn-primary" disabled={busy || !address.trim()} onclick={submit}>
        {preset?.oauth ? 'Continue to sign in' : 'Add account'}
      </button>
    </div>
  {/if}
</div>

<style>
  /* Centred rather than hung from the shared sheet's 22%: a form this long
     would otherwise run off the bottom of the window before its own
     max-height ever made it scroll. */
  .wide {
    top: 50%;
    translate: -50% -50%;
    width: min(560px, calc(100vw - var(--sp-8)));
    max-height: calc(100vh - var(--sp-8));
    overflow-y: auto;
  }

  h2 {
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
    margin-bottom: var(--sp-3);
  }

  .providers {
    display: flex;
    flex-wrap: wrap;
    gap: 2px;
    padding: 2px;
    margin-bottom: var(--sp-3);
    border-radius: var(--radius-sm);
    background: var(--bg-active);
  }
  .provider {
    flex: 1;
    height: 28px;
    padding: 0 var(--sp-2);
    border-radius: 4px;
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-subtle);
    white-space: nowrap;
  }
  .provider:hover {
    color: var(--fg);
  }
  .provider.on {
    background: var(--bg-raised);
    color: var(--fg);
    box-shadow: var(--shadow-sm);
  }

  .row {
    display: flex;
    gap: var(--sp-3);
    margin-bottom: var(--sp-3);
    align-items: flex-start;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  .port {
    width: 88px;
  }
  .opt {
    font-weight: 400;
    color: var(--fg-faint);
    text-transform: none;
    letter-spacing: 0;
  }

  .services {
    display: flex;
    gap: var(--sp-4);
    margin: var(--sp-3) 0;
  }
  .check {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-sm);
    cursor: pointer;
  }

  .hint {
    margin: 0 0 var(--sp-3);
    color: var(--fg-subtle);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
  }
  .hint a {
    color: var(--accent);
  }

  .mcp-note {
    display: flex;
    gap: var(--sp-2);
    align-items: flex-start;
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
  }
  .mcp-note :global(svg) {
    margin-top: 2px;
    flex: none;
  }

  .error {
    margin-top: var(--sp-3);
  }

  .oauth-status .hint {
    margin-bottom: var(--sp-3);
  }
  .url-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin-bottom: var(--sp-3);
  }
  .link {
    flex: 1;
    overflow-wrap: anywhere;
    font-size: var(--text-xs);
    color: var(--fg-muted);
    background: var(--bg-sunken);
    padding: 0.4rem 0.5rem;
    border-radius: 4px;
  }
</style>
