<script lang="ts">
  // Where accounts come from: the banks connected through Plaid, and the
  // accounts kept by hand and filled from a bank's export.
  //
  // A bank is one sign-in (Plaid calls it an Item) with one or more accounts
  // under it. Its card says when it last synced, or what went wrong, or that
  // the bank wants the person to sign in again -- in which case nothing syncs
  // until they do, and the button to do it is on the card. Disconnecting
  // comes in two kinds, side by side in the bank's menu: keep the history as
  // accounts kept by hand, or let it go with the connection. Either way the
  // store asks before it does anything, saying exactly what will go.
  //
  // An account can be hidden: it stays synced and in the vault, and leaves
  // the sidebar, the list and net worth. That is how a closed card or a
  // child's account stops being in the way without its history being lost.

  import { money } from '../lib/money.svelte'
  import { formatMoney, isLiability } from '../lib/money'
  import { relativeTime } from '../lib/format'
  import { focusOnMount } from '../lib/focus'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import type { AccountKind, BankAccount, BankAccountId, LinkSummary } from '../lib/types'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import MoneyAmount from './MoneyAmount.svelte'
  import MoneyConnect from './MoneyConnect.svelte'
  import MoneyImport from './MoneyImport.svelte'

  const KIND_LABELS: Record<AccountKind, string> = {
    cash: 'Checking or savings',
    credit: 'Credit card',
    loan: 'Loan',
    investment: 'Investment',
    other: 'Other',
  }

  const keysMissing = $derived(money.plaid !== null && !money.plaid.configured)
  const manual = $derived(money.accounts.filter((a) => !a.linkId))
  const nothing = $derived(money.links.length === 0 && money.accounts.length === 0)

  function accountsOf(s: LinkSummary): BankAccount[] {
    return money.accounts.filter((a) => a.linkId === s.link.id)
  }

  // What a sync found is the store's `note`, which `MoneyView` shows under
  // whichever pane is open.

  function connect() {
    void money.connectBank()
  }

  // ── Renaming in place ──────────────────────────────────────────────

  /** `link:<id>` or `account:<id>` while its name is being edited. */
  let renaming = $state<string | null>(null)
  let draftName = $state('')

  function startRename(key: string, name: string) {
    renaming = key
    draftName = name
  }

  async function finishRename(save: () => Promise<void>) {
    if (renaming === null) return
    renaming = null
    await save()
  }

  function renameKeys(e: KeyboardEvent & { currentTarget: HTMLInputElement }) {
    if (e.key === 'Enter') {
      e.preventDefault()
      e.currentTarget.blur()
    } else if (e.key === 'Escape') {
      e.preventDefault()
      e.stopPropagation()
      renaming = null
    }
  }

  async function renameLink(s: LinkSummary) {
    const name = draftName.trim()
    if (!name || name === s.link.name) return
    await money.saveLink({ ...$state.snapshot(s.link), name })
  }

  async function renameAccount(a: BankAccount) {
    const name = draftName.trim()
    if (!name || name === a.name) return
    await money.saveAccount({ ...$state.snapshot(a), name })
  }

  /** The rarer things to do to a bank, behind its "more" button and a right-click. */
  function linkMenu(s: LinkSummary): MenuItem[] {
    const link = s.link
    return tidyMenu([
      s.needsLogin && {
        label: 'Sign in again',
        icon: 'lock',
        run: () => void money.connectBank(link.id),
      },
      !s.needsLogin &&
        !link.paused && {
          label: 'Sync now',
          icon: 'refresh',
          run: () => void money.syncNow(link.id),
        },
      SEP,
      { label: 'Rename', icon: 'pencil', run: () => startRename(`link:${link.id}`, link.name) },
      {
        label: link.paused ? 'Resume syncing' : 'Pause syncing',
        icon: link.paused ? 'play' : 'stop',
        run: () => void money.saveLink({ ...$state.snapshot(link), paused: !link.paused }),
      },
      SEP,
      // The two kinds of disconnecting, each asked about by the store with
      // what it will do spelt out.
      {
        label: 'Disconnect, keeping the history…',
        icon: 'close',
        run: () => void money.disconnect(link.id, true),
      },
      {
        label: 'Disconnect and delete the history…',
        icon: 'trash',
        danger: true,
        run: () => void money.disconnect(link.id, false),
      },
    ])
  }

  // ── A new manual account ───────────────────────────────────────────

  let creating = $state(false)
  let newName = $state('')
  let newKind = $state<AccountKind>('cash')
  let newCurrency = $state('')
  let createError = $state<string | null>(null)

  function startCreate() {
    creating = true
    newName = ''
    newKind = 'cash'
    newCurrency = money.mainCurrency
    createError = null
  }

  async function create() {
    const name = newName.trim()
    const currency = newCurrency.trim().toUpperCase()
    if (!name) {
      createError = 'Give it a name.'
      return
    }
    if (!/^[A-Z]{3}$/.test(currency)) {
      createError = 'The currency is a three-letter code, like USD or EUR.'
      return
    }
    const made = await money.createManualAccount(name, newKind, currency)
    if (!made) {
      createError = 'It could not be made.'
      return
    }
    creating = false
  }

  // ── Importing ──────────────────────────────────────────────────────

  let importing = $state(false)
  let importInto = $state<BankAccountId | null>(null)

  function openImport(id: BankAccountId | null = null) {
    importInto = id
    importing = true
  }
</script>

<div class="accounts scroll">
  <div class="page">
    <div class="bar">
      {#if !keysMissing}
        <button class="btn btn-primary" disabled={money.linking !== null} onclick={connect}>
          <Icon name="plus" size={14} /> Connect a bank
        </button>
      {/if}
      {#if money.links.length > 0}
        <button
          class="btn btn-outline"
          disabled={money.syncing}
          onclick={() => void money.syncNow()}
        >
          <span class="turn" class:spinning={money.syncing}><Icon name="refresh" size={14} /></span>
          {money.syncing ? 'Syncing…' : 'Sync all'}
        </button>
      {/if}
      <button class="btn btn-outline" onclick={startCreate}>New manual account</button>
      <button class="btn btn-outline" onclick={() => openImport()}>
        <Icon name="upload" size={14} /> Import a file
      </button>
    </div>

    <MoneyConnect inline />

    {#if nothing && !creating}
      <EmptyState lead="No accounts yet." centred={false}>
        {#snippet note()}
          Connect a bank to have transactions arrive by themselves, or keep an account by hand and
          import the statements your bank lets you download.
        {/snippet}
      </EmptyState>
    {/if}

    {#each money.links as s (s.link.id)}
      {@const link = s.link}
      {@const linkKey = `link:${link.id}`}
      <section
        class="card"
        class:paused={link.paused}
        aria-label={link.name}
        oncontextmenu={(e) => menu.show(e, linkMenu(s))}
      >
        <div class="card-head">
          <span class="bank-mark" aria-hidden="true"><Icon name="landmark" size={16} /></span>
          <div class="titles">
            {#if renaming === linkKey}
              <input
                class="rename"
                bind:value={draftName}
                aria-label="Name of this bank"
                autocomplete="off"
                onkeydown={renameKeys}
                onblur={() => void finishRename(() => renameLink(s))}
                use:focusOnMount
              />
            {:else}
              <span class="name">{link.name}</span>
            {/if}
            <span class="status" class:bad={s.needsLogin || !!s.lastError}>
              {#if s.needsLogin}
                <Icon name="alert" size={13} /> Wants you to sign in again — nothing syncs until you do.
              {:else if s.lastError}
                <Icon name="alert" size={13} /> Could not sync: {s.lastError}
              {:else if link.paused}
                Paused — not syncing.
              {:else if s.lastOk}
                Synced {relativeTime(s.lastOk)}{#if s.nextAt}, next {relativeTime(s.nextAt)}{/if}.
              {:else}
                Not synced yet.
              {/if}
            </span>
          </div>
          <div class="card-actions">
            {#if s.needsLogin}
              <button
                class="btn btn-primary small"
                disabled={money.linking !== null}
                onclick={() => void money.connectBank(link.id)}
              >
                Sign in again
              </button>
            {:else}
              <button
                class="btn btn-outline small"
                disabled={money.syncing || link.paused}
                onclick={() => void money.syncNow(link.id)}
              >
                Sync now
              </button>
            {/if}
            <button
              class="icon-btn"
              aria-label="More for {link.name}"
              title="Rename, pause, disconnect"
              onclick={(e) => menu.show(e, linkMenu(s))}
            >
              <Icon name="more" size={15} />
            </button>
          </div>
        </div>

        <ul class="accts">
          {#each accountsOf(s) as a (a.id)}
            {@render accountRow(a)}
          {:else}
            <li class="empty-accts">No accounts reported by this bank.</li>
          {/each}
        </ul>
      </section>
    {/each}

    {#if manual.length > 0 || creating}
      <section class="card" aria-label="Kept by hand">
        <div class="card-head">
          <span class="bank-mark" aria-hidden="true"><Icon name="pencil" size={15} /></span>
          <div class="titles">
            <span class="name">Kept by hand</span>
            <span class="status">Filled by importing statements, or by typing transactions in.</span
            >
          </div>
        </div>

        <ul class="accts">
          {#each manual as a (a.id)}
            {@render accountRow(a)}
          {/each}
        </ul>

        {#if creating}
          <form
            class="create"
            onsubmit={(e) => {
              e.preventDefault()
              void create()
            }}
          >
            <input
              class="field"
              bind:value={newName}
              placeholder="Account name"
              aria-label="Account name"
              autocomplete="off"
              use:focusOnMount
            />
            <select class="field" bind:value={newKind} aria-label="Kind of account">
              {#each Object.entries(KIND_LABELS) as [value, label] (value)}
                <option {value}>{label}</option>
              {/each}
            </select>
            <input
              class="field currency"
              bind:value={newCurrency}
              maxlength="3"
              aria-label="Currency"
              autocomplete="off"
              spellcheck="false"
            />
            <button type="submit" class="btn btn-primary">Add</button>
            <button type="button" class="btn" onclick={() => (creating = false)}>Cancel</button>
          </form>
          {#if createError}<p class="error">{createError}</p>{/if}
        {/if}
      </section>
    {/if}
  </div>
</div>

{#snippet accountRow(a: BankAccount)}
  {@const key = `account:${a.id}`}
  {@const owed = isLiability(a.kind)}
  <li class="acct" class:is-hidden={a.hidden}>
    <div class="acct-main">
      {#if renaming === key}
        <input
          class="rename"
          bind:value={draftName}
          aria-label="Name of this account"
          autocomplete="off"
          onkeydown={renameKeys}
          onblur={() => void finishRename(() => renameAccount(a))}
          use:focusOnMount
        />
      {:else}
        <button
          class="acct-name"
          title="Show its transactions"
          onclick={() => {
            money.setView('transactions')
            void money.setAccount(a.id)
          }}
        >
          {a.name}
        </button>
      {/if}
      <span class="acct-meta">
        {KIND_LABELS[a.kind]}{a.subtype && a.subtype !== a.kind ? ` · ${a.subtype}` : ''}{a.mask
          ? ` · ··${a.mask}`
          : ''}{a.hidden ? ' · hidden' : ''}
      </span>
    </div>
    <div class="acct-balance">
      {#if a.balance != null}
        <MoneyAmount amount={owed ? -a.balance : a.balance} currency={a.currency} tone={false} />
        <span class="acct-sub">
          {#if owed}
            owed{#if a.limit != null}&nbsp;· {formatMoney(a.limit, a.currency)} limit{/if}
          {:else if a.available != null && a.available !== a.balance}
            {formatMoney(a.available, a.currency)} available
          {/if}
        </span>
      {:else}
        <span class="acct-sub">No balance yet</span>
      {/if}
    </div>
    <div class="acct-actions">
      <button
        class="icon-btn"
        aria-label={a.hidden ? `Show ${a.name}` : `Hide ${a.name}`}
        aria-pressed={a.hidden}
        title={a.hidden ? 'Hidden: show it again' : 'Hide from the sidebar, lists and net worth'}
        onclick={() => void money.saveAccount({ ...$state.snapshot(a), hidden: !a.hidden })}
      >
        <Icon name={a.hidden ? 'hidden' : 'eye'} size={15} />
      </button>
      <button
        class="icon-btn"
        aria-label="Rename {a.name}"
        title="Rename"
        onclick={() => startRename(key, a.name)}
      >
        <Icon name="pencil" size={14} />
      </button>
      {#if !a.linkId}
        <button
          class="icon-btn"
          aria-label="Import into {a.name}"
          title="Import a statement into it"
          onclick={() => openImport(a.id)}
        >
          <Icon name="upload" size={14} />
        </button>
        <button
          class="icon-btn danger"
          aria-label="Delete {a.name}"
          title="Delete"
          onclick={() => void money.deleteAccount(a.id)}
        >
          <Icon name="trash" size={14} />
        </button>
      {/if}
    </div>
  </li>
{/snippet}

{#if importing}
  <MoneyImport accountId={importInto} onclose={() => (importing = false)} />
{/if}

<style>
  .accounts {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .page {
    max-width: 860px;
    margin: 0 auto;
    padding: var(--sp-2) var(--sp-6) var(--fab-clear);
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }

  .bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-2) 0;
  }
  .bar .btn {
    height: 32px;
    font-size: var(--text-sm);
  }
  .turn {
    display: grid;
    place-items: center;
  }
  .turn.spinning {
    animation: spin 1s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  .card {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg);
  }
  .card.paused .name {
    color: var(--fg-subtle);
  }
  .card-head {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
  }
  .bank-mark {
    flex: none;
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    border-radius: 50%;
    background: var(--bg-sunken);
    color: var(--fg-subtle);
  }
  .titles {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .name {
    font-size: var(--text-md);
    font-weight: 600;
    color: var(--fg);
    overflow-wrap: anywhere;
  }
  .status {
    display: inline;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    overflow-wrap: anywhere;
  }
  .status.bad {
    color: var(--danger);
  }
  .status :global(svg) {
    vertical-align: -2px;
  }
  .card-actions {
    flex: none;
    display: flex;
    align-items: center;
    gap: var(--sp-1);
  }
  .btn.small {
    height: 30px;
    padding: 0 var(--sp-3);
    font-size: var(--text-sm);
  }

  .accts {
    margin: var(--sp-2) 0 0;
    padding: 0;
    list-style: none;
  }
  .acct {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-height: 48px;
    padding: var(--sp-2) 0 var(--sp-2) 44px;
    border-top: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }
  .acct.is-hidden .acct-main,
  .acct.is-hidden .acct-balance {
    opacity: 0.55;
  }
  .acct:hover .acct-actions,
  .acct:focus-within .acct-actions {
    opacity: 1;
  }
  .acct-main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .acct-name {
    align-self: flex-start;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    text-align: left;
    font-size: var(--text-base);
    color: var(--fg);
  }
  .acct-name:hover {
    text-decoration: underline;
    text-decoration-color: var(--fg-faint);
    text-underline-offset: 2px;
  }
  .acct-meta {
    font-size: var(--text-xs);
    color: var(--fg-faint);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .acct-balance {
    flex: none;
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 1px;
    color: var(--fg);
  }
  .acct-sub {
    font-size: var(--text-xs);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }
  .acct-actions {
    flex: none;
    display: flex;
    gap: 2px;
    opacity: 0.35;
    transition: opacity var(--fast) var(--ease);
  }
  .empty-accts {
    padding: var(--sp-2) 0 0 44px;
    font-size: var(--text-sm);
    color: var(--fg-faint);
  }

  .icon-btn {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .icon-btn:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .icon-btn.danger:hover {
    background: color-mix(in oklab, var(--danger) 12%, transparent);
    color: var(--danger);
  }

  .rename {
    width: 100%;
    max-width: 320px;
    height: 30px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--accent);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    color: var(--fg);
    font-size: var(--text-base);
    user-select: text;
  }
  .rename:focus {
    outline: none;
  }

  .create {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 180px 70px auto auto;
    gap: var(--sp-2);
    margin-top: var(--sp-2);
    padding-left: 44px;
  }
  .currency {
    text-transform: uppercase;
  }
  .error {
    margin-left: 44px;
  }
</style>
