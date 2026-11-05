<script lang="ts">
  // The Money app's main pane: a heading, the month it is looking at, and
  // whichever of the six panes the sidebar has chosen -- with a
  // transaction's detail as a rail on the right when one is open.
  //
  // The heading is drawn here once for every pane, so moving between them
  // never moves the title or the month: the Overview, the list, the Budget
  // and the Reports all look at one month, and the arrows beside the title
  // step it for all four. Accounts and Rules are not about a month and show
  // no arrows.
  //
  // Until there is anything at all -- no bank, no account -- the panes that
  // would only be empty say how to start instead. Budget, Accounts and Rules
  // stay reachable: categories and budgets can be set before the first
  // transaction arrives.

  import { money, type MoneyView as Pane } from '../lib/money.svelte'
  import { formatMoney, monthLabel, thisMonth } from '../lib/money'
  import { plural } from '../lib/format'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import MoneyAccounts from './MoneyAccounts.svelte'
  import MoneyBudget from './MoneyBudget.svelte'
  import MoneyConnect from './MoneyConnect.svelte'
  import MoneyImport from './MoneyImport.svelte'
  import MoneyOverview from './MoneyOverview.svelte'
  import MoneyReports from './MoneyReports.svelte'
  import MoneyRules from './MoneyRules.svelte'
  import MoneyTransactions from './MoneyTransactions.svelte'
  import MoneyTxnDetail from './MoneyTxnDetail.svelte'

  // Loaded when the view appears rather than when the store is imported, so
  // a vault whose owner never opens Money never reads a bank. `ready` is the
  // view's own: the lists are empty before they load as well as after, and
  // "connect a bank" for the length of one read is a flash of the wrong
  // sentence.
  let ready = $state(false)
  void money.start().finally(() => (ready = true))

  let importing = $state(false)

  const nothing = $derived(ready && money.links.length === 0 && money.accounts.length === 0)
  const view = $derived(money.view)
  const blank = $derived(
    nothing && (view === 'overview' || view === 'transactions' || view === 'reports'),
  )
  const monthly = $derived(view !== 'accounts' && view !== 'rules')
  const month = $derived(money.month || thisMonth())
  const atNow = $derived(month >= thisMonth())
  const openTxn = $derived(view === 'transactions' ? money.txn : null)
  const keysMissing = $derived(money.plaid !== null && !money.plaid.configured)

  const TITLES: Record<Pane, string> = {
    overview: 'Overview',
    transactions: 'Transactions',
    budget: 'Budget',
    reports: 'Reports',
    accounts: 'Accounts',
    rules: 'Rules',
  }

  /** The pane's name; the list is named for what it is narrowed to. */
  const title = $derived.by(() => {
    if (view !== 'transactions') return TITLES[view]
    const account = money.accountById(money.accountFilter)
    if (account) return account.name
    const filter = money.categoryFilter
    if (filter === 'none') return 'No category'
    if (filter) return money.categoryById(filter)?.name ?? TITLES.transactions
    return TITLES.transactions
  })

  const sub = $derived.by(() => {
    const currency = money.mainCurrency
    switch (view) {
      case 'transactions': {
        const page = money.page
        if (!page) return ''
        const sums = page.sums
          .filter((s) => s.amount !== 0)
          .map((s) => formatMoney(s.amount, s.currency, { sign: true }))
        return [plural(page.total, 'transaction'), ...sums].join(' · ')
      }
      case 'budget': {
        const s = money.summary
        if (!s) return ''
        return s.budgeted > 0
          ? `${formatMoney(Math.abs(s.spending), currency)} spent of ${formatMoney(s.budgeted, currency)} budgeted`
          : `${formatMoney(Math.abs(s.spending), currency)} spent`
      }
      case 'accounts': {
        const banks = money.links.length
        const accounts = money.accounts.length
        return banks > 0
          ? `${plural(banks, 'bank')} · ${plural(accounts, 'account')}`
          : plural(accounts, 'account')
      }
      case 'rules':
        return money.rules.length > 0 ? plural(money.rules.length, 'rule') : ''
      case 'reports':
        return 'The last twelve months'
      default:
        return ''
    }
  })

  function connect() {
    if (keysMissing) money.setView('accounts')
    else void money.connectBank()
  }
</script>

<main class="money" style:--tint={money.accent}>
  <div class="pane">
    <header class="top">
      <div class="pane-titles">
        <h1 class="pane-title">{title}</h1>
        {#if sub && !blank}<span class="pane-sub">{sub}</span>{/if}
      </div>

      <div class="tools">
        {#if monthly && !blank}
          <div class="months" role="group" aria-label="Month">
            <button
              class="step"
              aria-label="The month before"
              title="The month before (←)"
              onclick={() => void money.shiftMonth(-1)}
            >
              <span class="back"><Icon name="chevron" size={13} /></span>
            </button>
            <button
              class="month"
              title={atNow ? monthLabel(month) : 'Back to this month (T)'}
              disabled={atNow}
              onclick={() => void money.setMonth(thisMonth())}
            >
              {monthLabel(month)}
            </button>
            <button
              class="step"
              aria-label="The month after"
              title="The month after (→)"
              disabled={atNow}
              onclick={() => void money.shiftMonth(1)}
            >
              <Icon name="chevron" size={13} />
            </button>
          </div>
        {/if}
        {#if money.links.length > 0}
          <button
            class="round"
            class:spinning={money.syncing}
            title={money.syncing ? 'Syncing…' : 'Sync every bank now (R)'}
            aria-label={money.syncing ? 'Syncing' : 'Sync every bank now'}
            aria-busy={money.syncing}
            disabled={money.syncing}
            onclick={() => void money.syncNow()}
          >
            <Icon name="refresh" size={16} />
          </button>
        {/if}
      </div>
    </header>

    {#if blank}
      <EmptyState lead="Connect a bank or import a file.">
        {#snippet icon()}<Icon name="landmark" size={30} weight={1.3} />{/snippet}
        {#snippet note()}
          {#if keysMissing}
            Banks are connected through Plaid, with keys from your own Plaid account — add them
            under Settings → Money first. A statement downloaded from your bank (OFX, QFX or CSV)
            can be imported without them.
          {:else}
            Transactions from a connected bank arrive by themselves. Or keep an account by hand and
            import the statements your bank lets you download. Everything stays in this vault.
          {/if}
        {/snippet}
        {#snippet action()}
          <div class="actions">
            <button class="btn btn-primary" disabled={money.linking !== null} onclick={connect}>
              {keysMissing ? 'Set up a bank' : 'Connect a bank'}
            </button>
            <button class="btn" onclick={() => (importing = true)}>Import a file</button>
          </div>
        {/snippet}
      </EmptyState>
    {:else if view === 'overview'}
      <MoneyOverview />
    {:else if view === 'transactions'}
      <MoneyTransactions />
    {:else if view === 'budget'}
      <MoneyBudget />
    {:else if view === 'reports'}
      <MoneyReports />
    {:else if view === 'accounts'}
      <MoneyAccounts />
    {:else if view === 'rules'}
      <MoneyRules />
    {/if}

    <!-- What the last thing done came to: a sync, an import, rules run, a
         bank connected. The store clears it on the next move. -->
    {#if money.note}
      <p class="note" role="status">{money.note}</p>
    {/if}
  </div>

  {#if openTxn}
    <MoneyTxnDetail txn={openTxn} />
  {/if}
</main>

<!-- The sign-in in flight, over whichever pane started it. -->
<MoneyConnect />

{#if importing}
  <MoneyImport onclose={() => (importing = false)} />
{/if}

<style>
  .money {
    flex: 1;
    min-width: 0;
    display: flex;
  }
  .pane {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }

  .top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    height: var(--header-h);
    padding: 0 var(--sp-4) 0 var(--sp-6);
    flex: none;
  }

  .tools {
    flex: none;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }

  /* The month, between its two arrows, in the same rounded track as the
     filter chips so it reads as one control. */
  .months {
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 2px;
    border-radius: 999px;
    background: var(--bg-sunken);
  }
  .step {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .step:hover:not(:disabled) {
    background: var(--bg-raised);
    color: var(--fg);
  }
  .step:disabled {
    opacity: 0.35;
  }
  .back {
    display: grid;
    place-items: center;
    rotate: 180deg;
  }
  .month {
    min-width: 8.5em;
    height: 26px;
    padding: 0 var(--sp-2);
    border-radius: 999px;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg);
    font-variant-numeric: tabular-nums;
  }
  .month:hover:not(:disabled) {
    background: var(--bg-raised);
  }
  .month:disabled {
    cursor: default;
  }

  .round {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    border-radius: 50%;
    color: var(--fg-muted);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .round:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .round:disabled {
    opacity: 0.6;
  }
  /* A sync in progress: the icon turns, the button stays put. Stopped under
     reduced motion; the title still says "Syncing…". */
  .round.spinning :global(svg) {
    animation: spin 1s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .round.spinning :global(svg) {
      animation: none;
    }
  }

  .note {
    flex: none;
    margin: 0;
    padding: var(--sp-2) var(--sp-6);
    border-top: 1px solid var(--border);
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: var(--sp-2);
  }
</style>
