<script lang="ts">
  // The Money app's half of the sidebar: its six panes, then every bank and
  // the accounts under it, then the accounts kept by hand.
  //
  // An account row is a way into the transactions list narrowed to that
  // account -- the commonest question about one is "what went through it".
  // Its balance sits where every other sidebar puts a count, written short
  // ($12.3k) with the whole figure in its tooltip; a card or a loan shows
  // what is owed as a negative, so the column reads as what each adds to
  // net worth.
  //
  // A bank that needs the person -- to sign in again, or because syncing
  // failed -- carries a dot in the danger colour, and so does Accounts, so
  // it is seen from whichever pane is open. Hidden accounts are left out:
  // that is what hiding one is for.

  import { MONEY_VIEWS, money, type MoneyView } from '../lib/money.svelte'
  import { VIEW_ICONS, VIEW_LABELS, formatMoney, isLiability } from '../lib/money'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import type { BankAccount, BankAccountId, LinkSummary } from '../lib/types'
  import Icon from './Icon.svelte'
  import Symbol from './Symbol.svelte'
  import MoneyImport from './MoneyImport.svelte'

  const manual = $derived(money.visibleAccounts.filter((a) => !a.linkId))
  const hiddenCount = $derived(money.accounts.filter((a) => a.hidden).length)
  const keysMissing = $derived(money.plaid !== null && !money.plaid.configured)

  let importing = $state(false)
  let importInto = $state<BankAccountId | null>(null)

  function accountsOf(s: LinkSummary): BankAccount[] {
    return money.visibleAccounts.filter((a) => a.linkId === s.link.id)
  }

  /** What an account adds to net worth: what is owed counts against it. */
  function signed(a: BankAccount): number | null {
    if (a.balance == null) return null
    return isLiability(a.kind) ? -a.balance : a.balance
  }

  function isView(view: MoneyView): boolean {
    if (money.view !== view) return false
    // With an account picked, its row is the selection, not Transactions.
    return !(view === 'transactions' && money.accountFilter !== null)
  }

  function openView(view: MoneyView) {
    money.setView(view)
    if (view === 'transactions' && money.accountFilter !== null) void money.setAccount(null)
  }

  function openAccount(a: BankAccount) {
    money.setView('transactions')
    void money.setAccount(a.id)
  }

  function isAccount(a: BankAccount): boolean {
    return money.view === 'transactions' && money.accountFilter === a.id
  }

  /** Connect a bank; with no Plaid keys yet, the Accounts pane says where they go. */
  function connect() {
    if (keysMissing) money.setView('accounts')
    else void money.connectBank()
  }

  function openImport(id: BankAccountId | null = null) {
    importInto = id
    importing = true
  }

  function linkTitle(s: LinkSummary): string {
    if (s.needsLogin) return `${s.link.name} — wants you to sign in again`
    if (s.lastError) return `${s.link.name} — could not sync: ${s.lastError}`
    if (s.link.paused) return `${s.link.name} (paused)`
    return s.link.name
  }

  function accountTitle(a: BankAccount): string {
    const value = signed(a)
    const balance = value == null ? '' : ` — ${formatMoney(value, a.currency)}`
    return `${a.name}${a.mask ? ` ··${a.mask}` : ''}${balance}`
  }

  // ── What a right-click offers ─────────────────────────────────────────

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
          disabled: money.syncing,
          run: () => void money.syncNow(link.id),
        },
      {
        label: link.paused ? 'Resume syncing' : 'Pause syncing',
        icon: link.paused ? 'play' : 'stop',
        run: () => void money.saveLink({ ...$state.snapshot(link), paused: !link.paused }),
      },
      SEP,
      { label: 'Manage…', icon: 'settings', run: () => money.setView('accounts') },
      SEP,
      // Two kinds, and the store asks about either, saying what will go.
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

  function accountMenu(a: BankAccount): MenuItem[] {
    return tidyMenu([
      { label: 'Show its transactions', icon: 'list', run: () => openAccount(a) },
      !a.linkId && {
        label: 'Import a statement…',
        icon: 'upload',
        run: () => openImport(a.id),
      },
      SEP,
      {
        label: 'Hide',
        icon: 'hidden',
        run: () => void money.saveAccount({ ...$state.snapshot(a), hidden: true }),
      },
      { label: 'Manage…', icon: 'settings', run: () => money.setView('accounts') },
    ])
  }

  function navMenu(): MenuItem[] {
    return tidyMenu([
      { label: 'Connect a bank…', icon: 'plus', run: connect },
      { label: 'Import a file…', icon: 'upload', run: () => openImport() },
      money.links.length > 0 && {
        label: 'Sync every bank',
        icon: 'refresh',
        disabled: money.syncing,
        run: () => void money.syncNow(),
      },
    ])
  }
</script>

<nav class="scroll side-nav" aria-label="Money" oncontextmenu={(e) => menu.show(e, navMenu())}>
  {#each MONEY_VIEWS as view (view)}
    <button
      class="side-row"
      class:sel={isView(view)}
      aria-current={isView(view) ? 'page' : undefined}
      onclick={() => openView(view)}
    >
      <span class="side-icon"><Symbol name={VIEW_ICONS[view]} size={17} /></span>
      <span class="side-text">{VIEW_LABELS[view]}</span>
      {#if view === 'accounts' && money.attention.length > 0}
        <span
          class="side-dot"
          style:--dot="var(--danger)"
          role="img"
          aria-label="{money.attention.length} {money.attention.length === 1
            ? 'bank needs'
            : 'banks need'} you"
          title="{money.attention.length} {money.attention.length === 1
            ? 'bank needs'
            : 'banks need'} you"
        ></span>
      {/if}
    </button>
  {/each}

  <div class="side-head">
    <span class="eyebrow">Banks</span>
    <button
      class="side-add"
      title="Connect a bank"
      aria-label="Connect a bank"
      disabled={money.linking !== null}
      onclick={connect}
    >
      <Icon name="plus" size={15} />
    </button>
  </div>

  {#if money.links.length === 0}
    <p class="hint">
      None connected.
      <button class="inline" onclick={connect}>Connect a bank</button>
    </p>
  {/if}

  {#each money.links as s (s.link.id)}
    {@const bad = s.needsLogin || !!s.lastError}
    <button
      class="side-row bank"
      class:paused={s.link.paused}
      title={linkTitle(s)}
      onclick={() => money.setView('accounts')}
      oncontextmenu={(e) => menu.show(e, linkMenu(s))}
    >
      <span class="side-text">{s.link.name}</span>
      {#if s.needsLogin}
        <span class="badge">Sign in</span>
      {:else if bad}
        <span
          class="side-dot"
          style:--dot="var(--danger)"
          role="img"
          aria-label="Could not sync: {s.lastError}"
        ></span>
      {/if}
    </button>
    {#each accountsOf(s) as a (a.id)}
      {@render accountRow(a)}
    {/each}
  {/each}

  {#if manual.length > 0}
    <div class="side-head">
      <span class="eyebrow">Kept by hand</span>
      <button
        class="side-add"
        title="Import a file"
        aria-label="Import a file"
        onclick={() => openImport()}
      >
        <Icon name="upload" size={14} />
      </button>
    </div>
    {#each manual as a (a.id)}
      {@render accountRow(a)}
    {/each}
  {/if}

  {#if hiddenCount > 0}
    <button class="side-row quiet" onclick={() => money.setView('accounts')}>
      <span class="side-icon"><Symbol name="hidden" size={15} /></span>
      <span class="side-text">{hiddenCount} hidden</span>
    </button>
  {/if}
</nav>

{#snippet accountRow(a: BankAccount)}
  {@const value = signed(a)}
  <button
    class="side-row nested"
    class:sel={isAccount(a)}
    aria-current={isAccount(a) ? 'page' : undefined}
    title={accountTitle(a)}
    onclick={() => openAccount(a)}
    oncontextmenu={(e) => menu.show(e, accountMenu(a))}
  >
    <span class="side-text">{a.name}</span>
    {#if value != null}
      <span class="side-count balance" class:owed={value < 0}>
        {formatMoney(value, a.currency, { compact: true })}
      </span>
    {/if}
  </button>
{/snippet}

{#if importing}
  <MoneyImport accountId={importInto} onclose={() => (importing = false)} />
{/if}

<style>
  /* The rows, headings, counts and dots are the shared sidebar's -- see
     app.css's "Sidebars". What is Money's own: a bank's row as a quiet
     heading over its accounts, the accounts set in under it, a balance in
     the count's place, and the "sign in" badge. */

  .side-row.bank {
    margin-top: 2px;
    font-weight: 600;
    color: var(--fg);
  }
  .side-row.bank.paused {
    opacity: 0.55;
  }

  /* An account under its bank, set in so the bank reads as its heading. */
  .side-row.nested {
    padding-left: 24px;
  }

  .balance {
    font-size: var(--text-xs);
  }
  .balance.owed {
    color: var(--fg-subtle);
  }

  /* Words, not only a colour: a bank that wants signing in says so. */
  .badge {
    flex: none;
    padding: 1px 6px;
    border-radius: 999px;
    background: color-mix(in oklab, var(--danger) 12%, transparent);
    color: var(--danger);
    font-size: 11px;
    font-weight: 650;
  }

  .side-row.quiet {
    margin-top: var(--sp-2);
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }

  .side-add:disabled {
    opacity: 0.4;
  }

  .hint {
    padding: var(--sp-1) var(--sp-3) var(--sp-2);
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }
  .inline {
    color: var(--accent);
    font-size: inherit;
  }
  .inline:hover {
    text-decoration: underline;
  }
</style>
