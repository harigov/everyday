<script lang="ts">
  // How a connection reaches its server and signs in: the URL or the command,
  // the kind of sign-in, and every secret that goes with them.
  //
  // One set of fields for two places -- the "add" form and an existing
  // connection's detail -- because they are the same fields and a connection
  // that can be added one way and not edited the same way is a trap. The only
  // difference is what is already stored: an existing connection tells which
  // secrets are set (`set`), and then a field is "Set. Type to replace" and
  // never shows a value, since a value is never read back.
  //
  // The parent owns `transport`, `auth` and `secrets`, all bound: this edits
  // them in place. Names (headers, variables) go in the transport and values
  // in `secrets`, because the two are stored apart and only the names may
  // ever be read again.
  //
  // Styles are `ConnectionsPanel`'s, reached through its `.conn` wrapper.

  import { untrack } from 'svelte'
  import { joinArgs, splitArgs } from '../lib/connections'
  import type {
    ConnectionAuth,
    ConnectionPresetNeed,
    ConnectionSecretsPatch,
    ConnectionSecretsSet,
    ConnectionTransport,
  } from '../lib/types'

  let {
    transport = $bindable(),
    auth = $bindable(),
    secrets = $bindable(),
    set = null,
    needs = [],
    onremovesecret,
  }: {
    transport: ConnectionTransport
    auth: ConnectionAuth
    secrets: ConnectionSecretsPatch
    /** Which secrets are stored already, for an existing connection. */
    set?: ConnectionSecretsSet | null
    /** What the chosen preset asks for, to label and mark required. */
    needs?: ConnectionPresetNeed[]
    /** Remove one stored secret at once (an existing connection only). */
    onremovesecret?: (patch: ConnectionSecretsPatch) => void
  } = $props()

  const uid = $props.id()

  // ── Needs the preset names ──────────────────────────────────────────

  const clientIdNeed = $derived(needs.find((n) => n.kind === 'clientId') ?? null)
  const clientSecretNeed = $derived(needs.find((n) => n.kind === 'clientSecret') ?? null)
  const tokenNeed = $derived(needs.find((n) => n.kind === 'token') ?? null)
  /** The header or variable ones are drawn with the named values below. */
  const namedNeeds = $derived(
    needs.filter((n) => (n.kind === 'header' || n.kind === 'env') && !!n.name),
  )
  const needNames = $derived(namedNeeds.map((n) => n.name as string))

  // ── The transport ───────────────────────────────────────────────────

  // Typed text kept apart from what it parses to: re-joining the arguments
  // as they are typed would eat a trailing space the moment it was typed.
  let argsText = $state(untrack(() => (transport.type === 'stdio' ? joinArgs(transport.args) : '')))
  let scopesText = $state(untrack(() => (auth.type === 'oAuth' ? auth.scopes.join(' ') : '')))
  let portText = $state(
    untrack(() => (auth.type === 'oAuth' && auth.redirectPort ? String(auth.redirectPort) : '')),
  )

  function setUrl(url: string) {
    if (transport.type === 'http') transport.url = url
  }
  function setCommand(command: string) {
    if (transport.type === 'stdio') transport.command = command
  }
  function setArgs(text: string) {
    argsText = text
    if (transport.type === 'stdio') transport.args = splitArgs(text)
  }
  function setCwd(cwd: string) {
    if (transport.type === 'stdio') transport.cwd = cwd.trim() === '' ? null : cwd
  }

  // ── The sign-in ─────────────────────────────────────────────────────

  const SIGN_INS: { value: ConnectionAuth['type']; label: string }[] = [
    { value: 'none', label: 'None' },
    { value: 'bearer', label: 'Token' },
    { value: 'oAuth', label: 'OAuth (sign in with a browser)' },
  ]

  function chooseSignIn(type: string) {
    if (type === 'oAuth') {
      auth = { type: 'oAuth', clientId: null, scopes: [], redirectPort: null }
      scopesText = ''
      portText = ''
    } else if (type === 'bearer') {
      auth = { type: 'bearer' }
    } else {
      auth = { type: 'none' }
    }
  }
  function setClientId(id: string) {
    if (auth.type === 'oAuth') auth.clientId = id.trim() === '' ? null : id.trim()
  }
  function setScopes(text: string) {
    scopesText = text
    if (auth.type === 'oAuth') auth.scopes = text.split(/[\s,]+/).filter(Boolean)
  }
  function setPort(text: string) {
    portText = text
    if (auth.type !== 'oAuth') return
    const n = Number.parseInt(text, 10)
    auth.redirectPort = Number.isFinite(n) && n > 0 && n < 65536 ? n : null
  }

  // ── Headers or variables ────────────────────────────────────────────

  /** One typed row: `saved` rows are the names the connection already has,
   *  whose name cannot be edited, only its value replaced or the row removed. */
  type Row = { key: number; name: string; value: string; saved: boolean }

  const http = $derived(transport.type === 'http')
  let nextKey = 0

  function namesNow(): string[] {
    return transport.type === 'http' ? transport.headers : transport.env
  }
  function storedNames(): string[] {
    return (transport.type === 'http' ? set?.headers : set?.env) ?? []
  }
  function valuesNow(): Record<string, string> {
    return (transport.type === 'http' ? secrets.headers : secrets.env) ?? {}
  }

  let rows = $state<Row[]>(
    untrack(() =>
      namesNow()
        .filter((n) => !needNames.includes(n))
        .map((name) => ({ key: nextKey++, name, value: '', saved: storedNames().includes(name) })),
    ),
  )
  /** Saved names taken out of the list, to be emptied when the patch is sent. */
  let removed = $state<string[]>([])

  /** Write the rows back into the transport's names and the secrets' values,
   *  leaving alone whatever the preset's own needs wrote there. */
  function sync() {
    const fixedNames = namesNow().filter((n) => needNames.includes(n))
    const fixedValues = Object.fromEntries(
      Object.entries(valuesNow()).filter(([n]) => needNames.includes(n)),
    )
    const typed = rows.filter((r) => r.name.trim() !== '')
    const names = [...fixedNames, ...typed.map((r) => r.name.trim())]
    const values: Record<string, string> = { ...fixedValues }
    for (const r of typed) if (r.value !== '') values[r.name.trim()] = r.value
    for (const n of removed) if (!(n in values)) values[n] = ''
    if (transport.type === 'http') {
      transport.headers = names
      secrets.headers = values
    } else {
      transport.env = names
      secrets.env = values
    }
  }

  function addRow() {
    rows.push({ key: nextKey++, name: '', value: '', saved: false })
  }
  function removeRow(row: Row) {
    rows = rows.filter((r) => r.key !== row.key)
    if (row.saved) removed.push(row.name)
    sync()
  }

  /** A needed header or variable: its value, and its name kept in the list
   *  only while there is a value to send under it. */
  function setNeed(need: ConnectionPresetNeed, value: string) {
    const name = need.name as string
    const stored = storedNames().includes(name)
    const names = namesNow().filter((n) => n !== name)
    if (value !== '' || stored) names.push(name)
    const values = { ...valuesNow() }
    if (value === '') delete values[name]
    else values[name] = value
    if (transport.type === 'http') {
      transport.headers = names
      secrets.headers = values
    } else {
      transport.env = names
      secrets.env = values
    }
  }
  function needValue(need: ConnectionPresetNeed): string {
    return valuesNow()[need.name as string] ?? ''
  }
  function needStored(need: ConnectionPresetNeed): boolean {
    return storedNames().includes(need.name as string)
  }
</script>

<div class="fields">
  {#if transport.type === 'http'}
    <div class="setting">
      <label class="caption" for="{uid}-url">URL</label>
      <input
        id="{uid}-url"
        class="field"
        type="url"
        value={transport.url}
        oninput={(e) => setUrl(e.currentTarget.value)}
        placeholder="https://mcp.example.com/mcp"
        spellcheck="false"
        autocomplete="off"
      />
    </div>
  {:else}
    <div class="setting">
      <label class="caption" for="{uid}-command">Command</label>
      <input
        id="{uid}-command"
        class="field mono"
        value={transport.command}
        oninput={(e) => setCommand(e.currentTarget.value)}
        placeholder="npx"
        spellcheck="false"
        autocomplete="off"
      />
    </div>
    <div class="setting">
      <label class="caption" for="{uid}-args">Arguments</label>
      <input
        id="{uid}-args"
        class="field mono"
        value={argsText}
        oninput={(e) => setArgs(e.currentTarget.value)}
        placeholder="-y some-mcp-server@latest"
        spellcheck="false"
        autocomplete="off"
      />
      <span class="hint">Separated by spaces; put quotes around one that contains a space.</span>
    </div>
    <div class="setting">
      <label class="caption" for="{uid}-cwd">Working folder (optional)</label>
      <input
        id="{uid}-cwd"
        class="field mono"
        value={transport.cwd ?? ''}
        oninput={(e) => setCwd(e.currentTarget.value)}
        placeholder="Where the program starts"
        spellcheck="false"
        autocomplete="off"
      />
    </div>
  {/if}

  <!-- A program on this computer has no browser to sign in with: it is given
       what it needs as variables, below. -->
  {#if http}
    <div class="setting">
      <label class="caption" for="{uid}-signin">Sign-in</label>
      <select
        id="{uid}-signin"
        class="field"
        value={auth.type}
        onchange={(e) => chooseSignIn(e.currentTarget.value)}
      >
        {#each SIGN_INS as s (s.value)}
          <option value={s.value}>{s.label}</option>
        {/each}
      </select>
    </div>

    {#if auth.type === 'bearer'}
      <div class="setting">
        <label class="caption" for="{uid}-token">
          {tokenNeed?.label ?? 'Token'}
          {#if tokenNeed && !tokenNeed.optional}<span class="tag">Required</span>{/if}
        </label>
        <div class="with-link">
          <input
            id="{uid}-token"
            class="field"
            type="password"
            bind:value={secrets.bearer}
            placeholder={set?.bearer ? 'Set. Type to replace.' : ''}
            spellcheck="false"
            autocomplete="off"
          />
          {#if set?.bearer && onremovesecret}
            <button class="link" onclick={() => onremovesecret({ bearer: '' })}>Remove</button>
          {/if}
        </div>
        {#if tokenNeed}<span class="hint">{tokenNeed.help}</span>{/if}
      </div>
    {:else if auth.type === 'oAuth'}
      <div class="setting">
        <label class="caption" for="{uid}-clientid">
          {clientIdNeed?.label ?? 'Client ID (optional)'}
          {#if clientIdNeed && !clientIdNeed.optional}<span class="tag">Required</span>{/if}
        </label>
        <input
          id="{uid}-clientid"
          class="field mono"
          value={auth.clientId ?? ''}
          oninput={(e) => setClientId(e.currentTarget.value)}
          spellcheck="false"
          autocomplete="off"
        />
        <span class="hint">
          {clientIdNeed?.help ??
            'Leave empty and the service registers this app itself, where it allows that.'}
        </span>
      </div>
      <div class="setting">
        <label class="caption" for="{uid}-clientsecret">
          {clientSecretNeed?.label ?? 'Client secret (optional)'}
          {#if clientSecretNeed && !clientSecretNeed.optional}<span class="tag">Required</span>{/if}
        </label>
        <div class="with-link">
          <input
            id="{uid}-clientsecret"
            class="field"
            type="password"
            bind:value={secrets.clientSecret}
            placeholder={set?.clientSecret ? 'Set. Type to replace.' : ''}
            spellcheck="false"
            autocomplete="off"
          />
          {#if set?.clientSecret && onremovesecret}
            <button class="link" onclick={() => onremovesecret({ clientSecret: '' })}>
              Remove
            </button>
          {/if}
        </div>
        {#if clientSecretNeed}<span class="hint">{clientSecretNeed.help}</span>{/if}
      </div>
      <div class="pair">
        <div class="setting">
          <label class="caption" for="{uid}-scopes">Scopes (optional)</label>
          <input
            id="{uid}-scopes"
            class="field mono"
            value={scopesText}
            oninput={(e) => setScopes(e.currentTarget.value)}
            placeholder="Separated by spaces"
            spellcheck="false"
            autocomplete="off"
          />
        </div>
        <div class="setting">
          <label class="caption" for="{uid}-port">Redirect port (optional)</label>
          <input
            id="{uid}-port"
            class="field"
            type="number"
            min="1"
            max="65535"
            value={portText}
            oninput={(e) => setPort(e.currentTarget.value)}
            placeholder="Any free port"
          />
        </div>
      </div>
      <span class="hint">
        Empty scopes ask for what the service suggests. Some services match the redirect address
        exactly, and then need the same port you registered for it.
      </span>
    {/if}
  {/if}

  {#each namedNeeds as need (need.kind + ':' + need.name)}
    {@const stored = needStored(need)}
    <div class="setting">
      <label class="caption" for="{uid}-need-{need.name}">
        {need.label}
        {#if !need.optional}<span class="tag">Required</span>{/if}
      </label>
      <input
        id="{uid}-need-{need.name}"
        class="field"
        type="password"
        value={needValue(need)}
        oninput={(e) => setNeed(need, e.currentTarget.value)}
        placeholder={stored ? 'Set. Type to replace.' : ''}
        spellcheck="false"
        autocomplete="off"
      />
      <span class="hint">{need.help}</span>
    </div>
  {/each}

  <details class="more" open={rows.length > 0}>
    <summary>{http ? 'Headers' : 'Environment variables'}</summary>
    <div class="more-body">
      <span class="hint">
        {#if http}
          Extra headers sent with every request. Values are stored sealed and never shown again.
        {:else}
          Variables the program is started with. Values are stored sealed and never shown again.
        {/if}
      </span>
      {#each rows as row (row.key)}
        <div class="kv">
          {#if row.saved}
            <span class="kv-name mono" title={row.name}>{row.name}</span>
          {:else}
            <input
              class="field mono"
              aria-label={http ? 'Header name' : 'Variable name'}
              value={row.name}
              oninput={(e) => {
                row.name = e.currentTarget.value
                sync()
              }}
              placeholder={http ? 'X-Api-Key' : 'API_KEY'}
              spellcheck="false"
              autocomplete="off"
            />
          {/if}
          <input
            class="field"
            type="password"
            aria-label="Value of {row.name || (http ? 'header' : 'variable')}"
            value={row.value}
            oninput={(e) => {
              row.value = e.currentTarget.value
              sync()
            }}
            placeholder={row.saved ? 'Set. Type to replace.' : 'Value'}
            spellcheck="false"
            autocomplete="off"
          />
          <button class="link" onclick={() => removeRow(row)}>Remove</button>
        </div>
      {/each}
      <div>
        <button class="btn btn-sm btn-outline" onclick={addRow}>
          Add {http ? 'a header' : 'a variable'}
        </button>
      </div>
    </div>
  </details>
</div>

<style>
  .fields {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    min-width: 0;
  }
  .with-link {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 0;
  }
  .kv {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }
  .kv > :global(.field) {
    flex: 1;
    min-width: 0;
  }
  .kv-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg-muted);
  }
  .caption .tag {
    margin-left: var(--sp-2);
  }
</style>
