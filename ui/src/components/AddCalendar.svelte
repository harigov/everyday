<script lang="ts">
  // Adding a calendar.
  //
  // Google, Outlook and Apple all publish a calendar as an iCalendar feed at
  // a secret URL, and all three let you revoke that URL without touching the
  // account. Subscribing to one needs no OAuth client registered with a
  // vendor, no redirect server and no token to refresh — which is the only
  // arrangement that keeps working for an application that is a binary
  // somebody built themselves rather than a product with a client id.
  //
  // The hard part is not the code. It is that "Integrate calendar → Secret
  // address in iCal format" is buried five clicks deep in a settings page
  // nobody has opened, so most of this sheet is the instruction for finding
  // it, shown for the provider you picked and nothing else.
  //
  // Two things are said plainly rather than discovered later: the sync is
  // one-way, and the address is a secret.

  import { calendar } from '../lib/calendar.svelte'
  import { api } from '../lib/api'
  import { accounts } from '../lib/accounts.svelte'
  import { DEFAULT_COLORS } from '../lib/colors'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import Icon from './Icon.svelte'
  import type { AccountId, CalendarProvider, ProviderInfo, RemoteCalendarInfo } from '../lib/types'

  let { onclose }: { onclose: () => void } = $props()

  /** By address, or from an account already signed in with calendar on. */
  let mode = $state<'address' | 'account'>('address')

  let providers = $state<ProviderInfo[]>([])
  let provider = $state<CalendarProvider>('google')
  let url = $state('')
  let name = $state('')
  let color = $state(DEFAULT_COLORS[calendar.calendars.length % DEFAULT_COLORS.length]!)
  let busy = $state(false)
  let problem = $state<string | null>(null)
  let file = $state<HTMLInputElement | null>(null)

  // The provider list and its instructions come from the core, because the
  // guess that picks a provider from a pasted URL lives there too and the
  // two have to agree about what the set is.
  void api
    .calendarProviders()
    .then((p) => (providers = p))
    .catch(() => (providers = []))

  const hint = $derived(providers.find((p) => p.id === provider)?.hint ?? '')

  // ── From an account ─────────────────────────────────────────────────

  void accounts.load()
  /** Only an account with calendar switched on has anything to offer here. */
  const calendarAccounts = $derived(accounts.list.filter((a) => a.services.calendar))

  let selectedAccount = $state<AccountId | null>(null)
  let remoteCalendars = $state<RemoteCalendarInfo[]>([])
  let loadingRemote = $state(false)
  let subscribingRemoteId = $state<string | null>(null)

  async function pickAccount(id: AccountId) {
    selectedAccount = id
    remoteCalendars = []
    problem = null
    loadingRemote = true
    try {
      remoteCalendars = await api.listAccountCalendars(id)
    } catch {
      problem = 'Could not list this account’s calendars.'
    } finally {
      loadingRemote = false
    }
  }

  async function subscribeRemote(remote: RemoteCalendarInfo) {
    if (!selectedAccount || remote.subscribed || subscribingRemoteId) return
    subscribingRemoteId = remote.remoteId
    problem = null
    const failed = await calendar.subscribeFromAccount(selectedAccount, remote.remoteId)
    subscribingRemoteId = null
    if (failed) {
      problem = failed
      return
    }
    const picked = remoteCalendars.find((r) => r.remoteId === remote.remoteId)
    if (picked) picked.subscribed = true
    onclose()
  }

  async function subscribe() {
    const address = url.trim()
    if (!address || busy) return
    busy = true
    problem = null
    problem = await calendar.subscribe(name, address, color)
    busy = false
    if (!problem) onclose()
  }

  async function pickFile(e: Event) {
    const input = e.currentTarget as HTMLInputElement
    const chosen = input.files?.[0]
    if (!chosen) return
    busy = true
    problem = null
    // Read here, in the webview, rather than handing a path to the backend:
    // no path crosses the bridge, so "import a calendar" cannot be talked
    // into reading a file that was never picked.
    const text = await chosen.text()
    problem = await calendar.importFile(
      name || chosen.name.replace(/\.ics$/i, ''),
      chosen.name,
      color,
      text,
    )
    busy = false
    input.value = ''
    if (!problem) onclose()
  }
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape') onclose()
  }}
/>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="scrim" onclick={onclose}></div>
<div class="sheet wide" role="dialog" aria-modal="true" aria-label="Add a calendar" use:trapFocus>
  <h2>Add a calendar</h2>

  {#if calendarAccounts.length > 0}
    <div class="providers" role="group" aria-label="How to add it">
      <button class="provider" class:on={mode === 'address'} onclick={() => (mode = 'address')}>
        By address
      </button>
      <button class="provider" class:on={mode === 'account'} onclick={() => (mode = 'account')}>
        From an account
      </button>
    </div>
  {/if}

  {#if mode === 'address'}
    <div class="providers" role="group" aria-label="Where from">
      {#each providers as p (p.id)}
        <button class="provider" class:on={provider === p.id} onclick={() => (provider = p.id)}
          >{p.label}</button
        >
      {/each}
    </div>

    {#if hint}
      <p class="hint where">{hint}</p>
    {/if}

    <label class="field-label" for="cal-url">Calendar address</label>
    <input
      id="cal-url"
      class="field"
      placeholder="https://… or webcal://…"
      spellcheck="false"
      autocomplete="off"
      bind:value={url}
      use:focusOnMount
      oninput={() => (problem = null)}
      onkeydown={(e) => {
        if (e.key === 'Enter') void subscribe()
      }}
    />

    <div class="row">
      <div class="grow">
        <label class="field-label" for="cal-name">Name <span class="opt">optional</span></label>
        <input
          id="cal-name"
          class="field"
          placeholder="Taken from the calendar itself"
          bind:value={name}
        />
      </div>
      <div>
        <span class="field-label">Colour</span>
        <div class="swatches">
          {#each DEFAULT_COLORS as c (c)}
            <button
              class="swatch"
              class:on={color === c}
              style="--c: {c}"
              aria-label="Use this colour"
              onclick={() => (color = c)}
            ></button>
          {/each}
        </div>
      </div>
    </div>

    {#if problem}<p class="error">{problem}</p>{/if}

    <p class="hint terms">
      <Icon name="lock" size={13} />
      <span>
        The address is a key: anyone holding it can read that calendar, so it is encrypted with
        everything else in your vault. Events are fetched
        <strong>read-only</strong> — nothing you do here is ever written back to
        {providers.find((p) => p.id === provider)?.label ?? 'the calendar'}.
      </span>
    </p>

    <div class="sheet-row">
      <button class="btn ghost" onclick={() => file?.click()} disabled={busy}>
        <Icon name="upload" size={14} />
        Import a .ics file
      </button>
      <input
        class="vh"
        type="file"
        accept=".ics,text/calendar"
        bind:this={file}
        onchange={pickFile}
      />
      <span class="spacer"></span>
      <button class="btn" onclick={onclose}>Cancel</button>
      <button class="btn btn-primary" disabled={!url.trim() || busy} onclick={subscribe}>
        {busy ? 'Fetching…' : 'Subscribe'}
      </button>
    </div>
  {:else}
    <div class="providers" role="group" aria-label="Which account">
      {#each calendarAccounts as a (a.id)}
        <button
          class="provider"
          class:on={selectedAccount === a.id}
          onclick={() => pickAccount(a.id)}
        >
          {a.address}
        </button>
      {/each}
    </div>

    {#if loadingRemote}
      <p class="hint">Looking…</p>
    {:else if selectedAccount}
      {#if remoteCalendars.length === 0}
        <p class="hint">This account has no calendars to offer.</p>
      {:else}
        <ul class="remotelist">
          {#each remoteCalendars as r (r.remoteId)}
            <li class="remoterow">
              <span class="remotedot" style="--c: {r.color ?? '#64748b'}" aria-hidden="true"></span>
              <span class="remotename">{r.name}</span>
              {#if r.subscribed}
                <span class="hint subscribed">Subscribed</span>
              {:else}
                <button
                  class="btn"
                  disabled={subscribingRemoteId === r.remoteId}
                  onclick={() => subscribeRemote(r)}
                >
                  {subscribingRemoteId === r.remoteId ? 'Adding…' : 'Add'}
                </button>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    {:else}
      <p class="hint">Pick an account above to see its calendars.</p>
    {/if}

    {#if problem}<p class="error">{problem}</p>{/if}

    <div class="sheet-row">
      <span class="spacer"></span>
      <button class="btn" onclick={onclose}>Close</button>
    </div>
  {/if}
</div>

<style>
  .wide {
    width: min(520px, calc(100vw - var(--sp-8)));
  }

  h2 {
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
    margin-bottom: var(--sp-3);
  }

  .providers {
    display: flex;
    gap: 2px;
    padding: 2px;
    margin-bottom: var(--sp-2);
    border-radius: var(--radius-sm);
    background: var(--bg-active);
  }
  .provider {
    flex: 1;
    height: 26px;
    border-radius: 4px;
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-subtle);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .provider:hover {
    color: var(--fg);
  }
  .provider.on {
    background: var(--bg-raised);
    color: var(--fg);
    box-shadow: var(--shadow-sm);
  }

  .where {
    margin-bottom: var(--sp-3);
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
  }

  .row {
    display: flex;
    gap: var(--sp-3);
    margin-top: var(--sp-3);
    align-items: flex-start;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  .opt {
    font-weight: 400;
    color: var(--fg-faint);
    text-transform: none;
    letter-spacing: 0;
  }

  .swatches {
    display: flex;
    gap: 4px;
    height: 36px;
    align-items: center;
  }
  .swatch {
    width: 18px;
    height: 18px;
    border-radius: 5px;
    background: var(--c);
    box-shadow: 0 0 0 1px rgb(0 0 0 / 0.08) inset;
    transition: box-shadow var(--fast) var(--ease);
  }
  .swatch.on {
    box-shadow:
      0 0 0 2px var(--bg-raised),
      0 0 0 4px var(--c);
  }

  .error {
    margin-top: var(--sp-3);
  }

  .terms {
    display: flex;
    gap: var(--sp-2);
    align-items: flex-start;
    margin-top: var(--sp-3);
    font-size: var(--text-xs);
  }
  .terms :global(svg) {
    margin-top: 2px;
    flex: none;
  }

  .ghost {
    color: var(--fg-subtle);
  }

  .remotelist {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-top: var(--sp-2);
  }
  .remoterow {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-2);
    border-radius: var(--radius-sm);
  }
  .remoterow:hover {
    background: var(--bg-hover);
  }
  .remotedot {
    width: 10px;
    height: 10px;
    flex: none;
    border-radius: 3px;
    background: var(--c);
  }
  .remotename {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
  }
  .subscribed {
    margin: 0;
  }

  /* Visually hidden, still reachable: the real file input behind the button. */
  .vh {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }
</style>
