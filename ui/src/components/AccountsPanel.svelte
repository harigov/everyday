<script lang="ts">
  // Settings → Accounts: the mailbox providers this vault has signed in to.
  //
  // An account is not mail's own settings screen -- see
  // `everyday_core::account`'s module doc -- so this is a tab of its own
  // beside About You and the Assistant, not a section tucked under either.
  // The list, an "Add account" sheet, and a detail sheet to edit or remove
  // one are the whole of it; reading and sending mail is a later app.

  import { accounts } from '../lib/accounts.svelte'
  import { PROVIDER_LABELS, statusLabel } from '../lib/accounts'
  import { app } from '../lib/state.svelte'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import type { AccountId, AccountView } from '../lib/types'
  import AddAccount from './AddAccount.svelte'
  import AccountDetail from './AccountDetail.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'

  void accounts.load()
  void accounts.loadPresets()

  let adding = $state(false)
  let detailId = $state<AccountId | null>(null)
  let pendingDelete = $state<AccountView | null>(null)

  const writable = $derived(app.status?.writable !== false)

  function providerLabel(account: AccountView): string {
    return accounts.preset(account.provider)?.label ?? PROVIDER_LABELS[account.provider]
  }

  function rowMenu(account: AccountView): MenuItem[] {
    return tidyMenu([
      { label: 'Edit…', icon: 'pencil', run: () => (detailId = account.id) },
      SEP,
      {
        label: 'Remove account…',
        icon: 'trash',
        danger: true,
        run: () => (pendingDelete = account),
      },
    ])
  }

  async function confirmRemove() {
    const account = pendingDelete
    pendingDelete = null
    if (account) await accounts.remove(account.id)
  }
</script>

<section>
  <span class="eyebrow">Mailboxes signed in</span>
  <p class="hint">
    An account holds how to reach a mailbox and how to sign in to it. Mail and the calendar both ask
    it for a service rather than holding a credential of their own, so signing in once here is
    enough for both.
  </p>

  {#if accounts.loading && accounts.list.length === 0}
    <p class="hint">Loading…</p>
  {:else if accounts.list.length === 0}
    <p class="hint">
      No mailboxes yet. Add one to read its mail offline and see its calendar alongside your own.
    </p>
  {/if}

  <ul class="rows">
    {#each accounts.list as account (account.id)}
      {@const st = statusLabel(account.status)}
      <li>
        <div
          class="row"
          role="button"
          tabindex="0"
          onclick={() => (detailId = account.id)}
          onkeydown={(e) => e.key === 'Enter' && (detailId = account.id)}
          oncontextmenu={(e) => menu.show(e, rowMenu(account))}
        >
          <div class="main">
            <span class="address">{account.address}</span>
            <span class="provider">{providerLabel(account)}</span>
          </div>

          <div class="services">
            {#if account.services.mail}<span class="tag">Mail</span>{/if}
            {#if account.services.calendar}<span class="tag">Calendar</span>{/if}
          </div>

          <span class="status tone-{st.tone}" title={st.detail ?? undefined}>
            <span class="dot"></span>
            {st.label}{#if st.detail}<span class="detail">— {st.detail}</span>{/if}
          </span>

          <button
            class="more"
            title="What can be done to this account"
            aria-label="More, for {account.address}"
            onclick={(e) => {
              e.stopPropagation()
              menu.show(e, rowMenu(account))
            }}
          >
            <Icon name="chevron" size={13} />
          </button>
        </div>
      </li>
    {/each}
  </ul>

  <div>
    <button class="btn" disabled={!writable} onclick={() => (adding = true)}>
      <Icon name="plus" size={13} /> Add account
    </button>
  </div>
</section>

{#if adding}
  <AddAccount onclose={() => (adding = false)} />
{/if}

{#if detailId}
  <AccountDetail id={detailId} onclose={() => (detailId = null)} />
{/if}

{#if pendingDelete}
  <ConfirmDialog
    title={'Remove ' + pendingDelete.address + '?'}
    detail="The local copy of its mail is deleted from this vault. Nothing changes on the server itself -- signing in again later starts a fresh copy."
    confirmLabel="Remove account"
    onconfirm={confirmRemove}
    oncancel={() => (pendingDelete = null)}
  />
{/if}

<style>
  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }

  .rows {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-height: var(--row-h);
    padding: var(--sp-2);
    border-radius: var(--radius-sm);
    cursor: pointer;
  }
  .row:hover {
    background: var(--bg-hover);
  }

  .main {
    display: flex;
    flex-direction: column;
    min-width: 0;
    flex: 1;
  }
  .address {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-base);
    font-weight: 550;
    color: var(--fg);
  }
  .provider {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }

  .services {
    display: flex;
    gap: var(--sp-1);
    flex: none;
  }

  .status {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: none;
    max-width: 220px;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .status .detail {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg-faint);
  }
  .status .dot {
    flex: none;
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

  .more {
    display: grid;
    flex: none;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
    rotate: 90deg;
  }
  .more:hover {
    background: var(--bg-active);
    color: var(--fg);
  }

  .hint {
    margin: 0;
    color: var(--fg-subtle);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
  }
</style>
