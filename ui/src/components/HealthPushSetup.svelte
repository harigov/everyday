<script lang="ts">
  // Connecting an iPhone: the address and the key Health Auto Export needs,
  // and what to put where in the app.
  //
  // Health Auto Export is an iOS app that posts the phone's health data to
  // an address on a schedule. Here that address is on this computer, served
  // by web access over the tailnet, and the key is a bearer token that can
  // add readings to this one source and do nothing else.
  //
  // Opening this makes a token -- a new one each time, which retires the
  // last -- and shows it once. It is held in this component's own state and
  // nowhere else: not the store, not a log, not the clipboard until the
  // person presses Copy. Closing this, or leaving the page, forgets it, and
  // the only way to see a token again is to make another.

  import { onDestroy } from 'svelte'
  import { health } from '../lib/health.svelte'
  import { panels } from '../lib/panels.svelte'
  import type { HealthSource, PushSetup } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'

  interface Props {
    source: HealthSource
    onclose: () => void
  }
  let { source, onclose }: Props = $props()

  let setup = $state<PushSetup | null>(null)
  let minting = $state(true)
  let revoked = $state(false)
  let copied = $state<string | null>(null)
  let copyFailed = $state(false)
  let confirming = $state(false)
  let timer: ReturnType<typeof setTimeout> | null = null

  async function mint() {
    minting = true
    revoked = false
    copyFailed = false
    try {
      setup = await health.pushSetup(source.id)
    } catch {
      setup = null
    } finally {
      minting = false
    }
  }
  void mint()

  onDestroy(() => {
    if (timer) clearTimeout(timer)
    // Component state goes with the component; there is no reason to leave
    // a token for the collector to find.
    setup = null
  })

  async function copy(what: string, text: string) {
    try {
      await navigator.clipboard.writeText(text)
      copied = what
      copyFailed = false
      if (timer) clearTimeout(timer)
      timer = setTimeout(() => (copied = null), 2000)
    } catch {
      copyFailed = true
    }
  }

  async function revoke() {
    confirming = false
    await health.revokePush(source.id)
    setup = null
    revoked = true
  }

  const TYPES = ['Health Metrics', 'Workouts', 'Medications']
  const AUTOMATIONS = [
    ...TYPES.map((type) => ({
      name: type,
      type,
      range: 'Since Last Sync',
      every: '1 hour',
    })),
    ...TYPES.map((type) => ({
      name: `${type}, catch-up`,
      type,
      range: 'Previous 7 Days',
      every: '1 day',
    })),
  ]
</script>

<section class="setup" aria-labelledby="push-head-{source.id}">
  <div class="head">
    <span class="eyebrow" id="push-head-{source.id}">Connect {source.name}</span>
    <button class="btn btn-sm" onclick={onclose}>Done</button>
  </div>

  {#if minting && setup === null}
    <p class="hint" role="status">Making a token…</p>
  {:else if revoked}
    <p class="state">
      <Icon name="tick" size={14} />
      The token is revoked. The phone can’t send anything until you make a new one and put it in its automations.
    </p>
    <div>
      <button class="btn btn-outline" onclick={() => void mint()}>Make a new token</button>
    </div>
  {:else if setup === null}
    <p class="error" role="alert">This could not be set up just now.</p>
    <div>
      <button class="btn btn-outline" onclick={() => void mint()}>Try again</button>
    </div>
  {:else if setup.token === '' || setup.url === ''}
    <p class="error" role="alert">
      {setup.problem ||
        (setup.webOn
          ? 'This computer has no address the phone can reach yet.'
          : 'Web access is off, and the phone reaches this computer through it.')}
    </p>
    <p class="hint">
      Turn on web access in Settings, under Vault. It needs Tailscale on this computer. Then try
      again.
    </p>
    <div class="row">
      <button class="btn btn-outline" onclick={() => panels.openSettings('vault')}>
        Open the Vault settings
      </button>
      <button class="btn btn-outline" onclick={() => void mint()}>Try again</button>
    </div>
  {:else}
    {@const s = setup}
    <p class="lead">
      Health Auto Export sends your Apple Watch and iPhone data to this computer. Copy the address
      and the key into its automations, as below. The key can only add readings to this vault: it
      cannot read anything. <b>It is shown once</b>, so copy it now. If you lose it, make another.
    </p>

    <div class="creds">
      <div class="cred">
        <span class="label" id="cred-url-{source.id}">Address</span>
        <div class="cred-row">
          <input
            class="mono"
            readonly
            value={s.url}
            aria-labelledby="cred-url-{source.id}"
            onfocus={(e) => e.currentTarget.select()}
          />
          <button class="btn btn-outline btn-sm" onclick={() => void copy('url', s.url)}>
            <Icon name="copy" size={13} />
            {copied === 'url' ? 'Copied' : 'Copy'}
          </button>
        </div>
      </div>
      <div class="cred">
        <span class="label" id="cred-name-{source.id}">Header name</span>
        <div class="cred-row">
          <input
            class="mono"
            readonly
            value="Authorization"
            aria-labelledby="cred-name-{source.id}"
            onfocus={(e) => e.currentTarget.select()}
          />
          <button class="btn btn-outline btn-sm" onclick={() => void copy('name', 'Authorization')}>
            <Icon name="copy" size={13} />
            {copied === 'name' ? 'Copied' : 'Copy'}
          </button>
        </div>
      </div>
      <div class="cred">
        <span class="label" id="cred-value-{source.id}">Header value (this is the key)</span>
        <div class="cred-row">
          <input
            class="mono"
            readonly
            value="Bearer {s.token}"
            aria-labelledby="cred-value-{source.id}"
            onfocus={(e) => e.currentTarget.select()}
          />
          <button
            class="btn btn-outline btn-sm"
            onclick={() => void copy('value', `Bearer ${s.token}`)}
          >
            <Icon name="copy" size={13} />
            {copied === 'value' ? 'Copied' : 'Copy'}
          </button>
        </div>
      </div>
      {#if copyFailed}
        <p class="error" role="alert">
          Copying didn’t work here. Click a field to select it, then copy it yourself.
        </p>
      {/if}
    </div>

    <h3 class="steps-head">On your iPhone</h3>
    <ol class="steps">
      <li>
        <b>Install Health Auto Export</b> (a paid app from the App Store) and let it read the Health data
        you want to send.
      </li>
      <li>
        <b>Be on the same Tailscale network as this computer.</b> Sign in to Tailscale on the phone with
        the same account, and keep it on. The address above only answers devices on your tailnet. Nothing
        goes over the open internet.
      </li>
      <li>
        <b>Make an automation.</b> In Health Auto Export open Automations, add one, and choose the
        type <code>REST API</code>.
      </li>
      <li>
        <b>Pick one kind of data.</b> The app sends one kind per automation: Health Metrics, then Workouts,
        then Medications (Medications needs iOS 26). Make one automation for each.
      </li>
      <li>
        <b>URL:</b> paste the address above.
      </li>
      <li>
        <b>Headers:</b> add one. The name is <code>Authorization</code> and the value is
        <code>Bearer</code> then a space then the key, which is what “Header value” above holds.
      </li>
      <li>
        <b>Export Format</b> <code>JSON</code>, <b>Export Version</b> <code>2</code>.
      </li>
      <li>
        <b>Summarize Data: turn it OFF.</b> This one matters. With it on, the app sends daily or hourly
        totals instead of the readings, and Karma Yoga refuses them, because the totals would be counted
        twice beside the readings it already has.
      </li>
      <li><b>Batch Requests:</b> on.</li>
      <li>
        <b>Date Range:</b> <code>Since Last Sync</code>. <b>Sync Frequency:</b> <code>1 hour</code>.
      </li>
      <li>
        <b>Save it, then run it once.</b> A moment later this source says it heard from the phone.
      </li>
      <li>
        <b>Make a catch-up set.</b> Do all of that again for each kind of data, but with Date Range
        <code>Previous 7 Days</code> and a daily sync. If this computer was asleep or the vault was locked
        when an hourly one ran, the catch-up fills the gap. A reading that arrives twice is kept once.
      </li>
      <li>
        <b>Allow Background App Refresh.</b> In the iPhone’s Settings, General, Background App Refresh,
        turn it on for Health Auto Export. Without it iOS may not run the hourly sync.
      </li>
    </ol>

    <div class="table-wrap">
      <table>
        <caption>The six automations</caption>
        <thead>
          <tr>
            <th scope="col">Name (yours to choose)</th>
            <th scope="col">Data type</th>
            <th scope="col">Date Range</th>
            <th scope="col">Sync Frequency</th>
          </tr>
        </thead>
        <tbody>
          {#each AUTOMATIONS as a (a.name)}
            <tr>
              <td>{a.name}</td>
              <td>{a.type}</td>
              <td>{a.range}</td>
              <td>{a.every}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>

    <p class="hint">
      Deleting a reading on the phone does not delete it here. Delete it here by hand if you want it
      gone.
    </p>

    <div class="row foot">
      <button class="btn btn-ghost-danger btn-outline btn-sm" onclick={() => (confirming = true)}>
        Revoke this token
      </button>
      <span class="spacer"></span>
      <button class="btn btn-primary" onclick={onclose}>Done</button>
    </div>
  {/if}
</section>

{#if confirming}
  <ConfirmDialog
    title="Revoke this token?"
    detail="The phone stops being able to send at once. Make a new token and put it in the automations to start again."
    confirmLabel="Revoke"
    onconfirm={() => void revoke()}
    oncancel={() => (confirming = false)}
  />
{/if}

<style>
  .setup {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-sunken);
  }
  .head,
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2);
  }
  .row {
    justify-content: flex-start;
    flex-wrap: wrap;
  }
  .foot {
    padding-top: var(--sp-2);
  }
  .spacer {
    flex: 1;
  }
  .hint,
  .lead {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
    overflow-wrap: anywhere;
  }
  .lead {
    color: var(--fg-muted);
  }
  .lead b {
    font-weight: 600;
    color: var(--fg);
  }
  .error {
    margin: 0;
  }
  .state {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-muted);
  }
  .state :global(svg) {
    translate: 0 2px;
  }

  .creds {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .cred {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    min-width: 0;
  }
  .label {
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-muted);
  }
  .cred-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }
  .mono {
    flex: 1;
    min-width: 0;
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    color: var(--fg);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    user-select: text;
  }
  .mono:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 16%, transparent);
  }

  .steps-head {
    margin: var(--sp-2) 0 0;
    font-size: var(--text-md);
    font-weight: 620;
  }
  .steps {
    margin: 0;
    padding-left: 1.4em;
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    font-size: var(--text-base);
    line-height: var(--leading-normal);
    color: var(--fg-muted);
  }
  .steps b {
    font-weight: 600;
    color: var(--fg);
  }
  code {
    padding: 1px 4px;
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg-muted);
    font-size: var(--text-xs);
  }

  .table-wrap {
    overflow-x: auto;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--text-sm);
  }
  caption {
    padding-bottom: var(--sp-2);
    text-align: left;
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--fg-faint);
  }
  th {
    padding: var(--sp-1) var(--sp-3) var(--sp-1) 0;
    text-align: left;
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--fg-faint);
    border-bottom: 1px solid var(--border);
  }
  td {
    padding: var(--sp-1) var(--sp-3) var(--sp-1) 0;
    color: var(--fg-muted);
    border-bottom: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }

  /* A phone: the address and the keys are fields a finger can select, each
     with its Copy button at a finger's size and the table's cells wrapping
     rather than scrolling; the steps read a size down on the narrow page. */
  :global([data-phone]) .setup {
    padding: var(--sp-3);
  }
  :global([data-phone]) .steps {
    font-size: var(--text-sm);
    padding-left: 1.2em;
  }
  :global([data-phone]) .foot {
    flex-wrap: wrap;
  }
  :global([data-phone]) th,
  :global([data-phone]) td {
    padding-right: var(--sp-2);
  }
</style>
