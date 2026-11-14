<script lang="ts">
  // Settings → Money: the Plaid keys banks are linked with.
  //
  // Plaid is the service between this app and a bank: it signs in to the
  // bank on the person's behalf and answers with accounts and transactions.
  // Karma Yoga has no Plaid account of its own -- the person brings theirs,
  // so their bank data is never routed through anybody else's -- and these
  // are its two credentials and which of Plaid's environments they belong
  // to.
  //
  // The secret is held in this component's own field until Save, sent once,
  // and the field is emptied whatever the answer: it is never put in the
  // store, never shown again, and nothing the service answers contains it.
  // What can be said about the keys afterwards is `PlaidSetup`: whether they
  // are set, for which environment, and the client id's last four
  // characters, which is enough to tell two sets of keys apart and no use to
  // anybody else.

  import { onDestroy } from 'svelte'
  import { money } from '../lib/money.svelte'
  import { openExternal } from '../lib/open-external'
  import type { PlaidEnvironment } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'

  /** Where Plaid lists an account's keys. A constant: nothing here opens an address from data. */
  const DASHBOARD = 'https://dashboard.plaid.com/developers/keys'

  let clientId = $state('')
  let secret = $state('')
  let environment = $state<PlaidEnvironment>('production')
  let saving = $state(false)
  let confirming = $state(false)
  let notice = $state<{ text: string; bad: boolean } | null>(null)
  let noticeTimer: ReturnType<typeof setTimeout> | null = null

  function say(text: string, bad = false) {
    notice = { text, bad }
    if (noticeTimer) clearTimeout(noticeTimer)
    noticeTimer = setTimeout(() => {
      noticeTimer = null
      notice = null
    }, 5000)
  }

  onDestroy(() => {
    if (noticeTimer) clearTimeout(noticeTimer)
    // Belt and braces: the field is component state and goes with the
    // component, but there is no reason to leave it to the collector.
    secret = ''
  })

  // Fresh each time the tab opens, and the environment picker starts at
  // whatever the saved keys are for.
  void money.loadPlaid().then(() => {
    const saved = money.plaid?.environment
    if (saved) environment = saved
  })

  const setup = $derived(money.plaid)
  const canSave = $derived(!saving && clientId.trim() !== '' && secret.trim() !== '')

  async function save() {
    if (!canSave) return
    saving = true
    notice = null
    try {
      const ok = await money.savePlaid(clientId.trim(), secret.trim(), environment)
      if (ok) {
        clientId = ''
        say('Saved. Banks can be connected from the Money app.')
      } else {
        // The store has already said why, without the secret in it.
        say('The keys were not saved.', true)
      }
    } catch {
      say('The keys could not be saved.', true)
    } finally {
      // Whatever happened, the secret does not stay in the field.
      secret = ''
      saving = false
    }
  }

  async function remove() {
    confirming = false
    await money.clearPlaid()
    say('Keys removed.')
  }
</script>

<div class="panes">
  <p class="hint lead">
    Banks are connected through <b>Plaid</b>, the service many budgeting apps use to read accounts
    and transactions from a bank. Karma Yoga uses keys from your own Plaid account, so your bank
    data goes from Plaid to this vault and nowhere else. Plaid's free personal-use plan covers ten
    bank sign-ins. Connecting is read-only: nothing here can move money.
  </p>

  <section class="card" aria-labelledby="plaid-head">
    <div class="head">
      <span class="eyebrow" id="plaid-head">Plaid keys</span>
      <button class="link" onclick={() => void openExternal(DASHBOARD)}>
        Plaid's dashboard <Icon name="link" size={13} />
      </button>
    </div>

    {#if setup === null}
      <p class="hint">Loading…</p>
    {:else if setup.configured}
      <div class="stored">
        <span>
          <Icon name="lock" size={14} />
          Keys are set for
          {setup.environment === 'sandbox' ? 'the sandbox' : 'production'}{setup.clientIdHint
            ? `, client id …${setup.clientIdHint}`
            : ''}.
        </span>
        <button class="link danger" onclick={() => (confirming = true)}>Remove keys</button>
      </div>
    {:else}
      <p class="hint">
        No keys yet. In Plaid's dashboard, under Developers → Keys, copy the client id and the
        secret for the environment you want.
      </p>
    {/if}

    <form
      class="form"
      onsubmit={(e) => {
        e.preventDefault()
        void save()
      }}
    >
      <label class="setting">
        <span>Client id</span>
        <input
          bind:value={clientId}
          placeholder={setup?.configured ? 'Replace the client id' : 'From Plaid’s dashboard'}
          spellcheck="false"
          autocomplete="off"
        />
      </label>
      <label class="setting">
        <span>Secret</span>
        <input
          type="password"
          bind:value={secret}
          placeholder={setup?.configured ? 'Replace the secret' : 'The secret for this environment'}
          spellcheck="false"
          autocomplete="new-password"
        />
      </label>
      <label class="setting">
        <span>Environment</span>
        <select bind:value={environment}>
          <option value="production">Production — your real banks</option>
          <option value="sandbox">Sandbox — Plaid's test banks</option>
        </select>
      </label>
      <p class="hint">
        Each environment has its own secret. A bank stays with the environment it was connected in,
        so switching does not move existing connections.
      </p>
      <div class="save">
        {#if notice}
          <span class="notice" class:bad={notice.bad} role="status">{notice.text}</span>
        {/if}
        <button class="btn btn-primary" type="submit" disabled={!canSave}>
          {saving ? 'Saving…' : 'Save keys'}
        </button>
      </div>
    </form>
  </section>

  <p class="hint">
    The keys are sealed in the vault with everything else, so they are unreadable while it is
    locked, and they are only ever sent to Plaid — never to the assistant, an export, or anywhere
    else. Each bank's own sign-in token is sealed the same way.
  </p>
</div>

{#if confirming}
  <ConfirmDialog
    title="Remove the Plaid keys?"
    detail="Connected banks stop syncing until keys are set again. Nothing already synced is deleted."
    confirmLabel="Remove keys"
    onconfirm={() => void remove()}
    oncancel={() => (confirming = false)}
  />
{/if}

<style>
  /* The same card, field and stored-key shapes as Settings → Models, which
     is where somebody has already typed a key into this app once. */
  .panes {
    display: flex;
    flex-direction: column;
    gap: var(--sp-6);
    min-width: 0;
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
  }
  .hint {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
    overflow-wrap: anywhere;
  }
  .hint.lead {
    color: var(--fg-muted);
  }
  .hint b {
    font-weight: 600;
    color: var(--fg);
  }

  .form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }
  .setting {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    min-width: 0;
  }
  .setting > span {
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-muted);
  }
  input,
  select {
    width: 100%;
    min-width: 0;
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-base);
    user-select: text;
  }
  input:focus,
  select:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 16%, transparent);
  }

  .stored {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-sunken);
    font-size: var(--text-sm);
    min-width: 0;
  }
  .stored span {
    display: flex;
    flex: 1;
    flex-wrap: wrap;
    align-items: baseline;
    gap: var(--sp-1) var(--sp-2);
    min-width: 0;
    line-height: var(--leading-normal);
    color: var(--fg-muted);
  }
  .link {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: var(--sp-1);
    color: var(--accent);
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .link:hover {
    text-decoration: underline;
  }
  .link.danger {
    color: var(--danger);
  }

  .save {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--sp-3);
  }
  .notice {
    flex: 1;
    min-width: 0;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .notice.bad {
    color: var(--danger);
  }
</style>
