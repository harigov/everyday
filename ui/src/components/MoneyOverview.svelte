<script lang="ts">
  // Money at a glance: what you are worth, what came in and went out this
  // month, what needs you, the budgets running over, and what is about to
  // repeat.
  //
  // Figures as figures. Net worth, income, spending and what is left are four
  // single values with no shape to plot, so they are `StatTile`s rather than
  // four one-bar charts. The budgets running over are bars because there the
  // question is "how far over", which is a length against a limit.
  //
  // "Needs you" comes first after the figures, and says so plainly when
  // nothing does: a bank that wants signing in again stops syncing until it
  // is, and spending nobody has sorted is missing from every budget.

  import { money } from '../lib/money.svelte'
  import { CADENCE_LABELS, formatMoney, monthLabel, thisMonth, upcomingCharges } from '../lib/money'
  import { friendlyDate, plural } from '../lib/format'
  import type { MoneyCategory } from '../lib/types'
  import Icon from './Icon.svelte'
  import MoneyAmount from './MoneyAmount.svelte'
  import StatTile from './StatTile.svelte'

  const currency = $derived(money.mainCurrency)
  const summary = $derived(money.summary)
  const month = $derived(money.month || thisMonth())
  const current = $derived(month === thisMonth())

  function fmt(minor: number): string {
    return formatMoney(minor, currency)
  }

  const uncategorized = $derived(summary?.categories.find((c) => c.categoryId === null) ?? null)

  /** Budgets past nine tenths, the furthest over first. */
  const running = $derived.by(() => {
    if (!summary) return []
    const out: { category: MoneyCategory; spent: number; budget: number }[] = []
    for (const s of summary.categories) {
      const category = money.categoryById(s.categoryId)
      const budget = s.budget ?? category?.budget ?? null
      if (!category || !budget || budget <= 0) continue
      const spent = Math.abs(s.spent)
      if (spent >= budget * 0.9) out.push({ category, spent, budget })
    }
    return out.sort((a, b) => b.spent / b.budget - a.spent / a.budget).slice(0, 5)
  })

  /** Charges that repeat and fall due in the next month, soonest first. */
  const upcoming = $derived(upcomingCharges(money.recurring, 31).slice(0, 8))

  const visibleCount = $derived(money.visibleAccounts.length)

  /** Connect a bank -- or, with no Plaid keys yet, go where the Accounts pane says how. */
  function connect() {
    if (money.plaid && !money.plaid.configured) money.setView('accounts')
    else void money.connectBank()
  }

  function sortUncategorized() {
    money.setView('transactions')
    void money.setCategory('none')
  }
</script>

<div class="overview scroll">
  <div class="page">
    <div class="tiles">
      <StatTile
        label="Net worth"
        value={fmt(money.netWorthNow)}
        note={visibleCount > 0 ? `Across ${plural(visibleCount, 'account')}` : 'No accounts yet'}
      />
      <StatTile
        label={current ? 'In this month' : `In, ${monthLabel(month)}`}
        value={summary ? fmt(summary.income) : '—'}
      />
      <StatTile
        label={current ? 'Out this month' : `Out, ${monthLabel(month)}`}
        value={summary ? fmt(Math.abs(summary.spending)) : '—'}
        note={summary && summary.budgeted > 0 ? `of ${fmt(summary.budgeted)} budgeted` : undefined}
        warn={!!summary && summary.budgeted > 0 && Math.abs(summary.spending) > summary.budgeted}
      />
      <StatTile
        label="Left over"
        value={summary ? formatMoney(summary.net, currency, { sign: true }) : '—'}
        note={summary ? (summary.net < 0 ? 'More went out than came in' : undefined) : undefined}
        warn={!!summary && summary.net < 0}
      />
    </div>

    <section class="card" aria-labelledby="ov-attention">
      <h2 class="eyebrow" id="ov-attention">Needs you</h2>
      {#if money.attention.length === 0 && !(uncategorized && uncategorized.count > 0)}
        <p class="quiet">Nothing needs you. Every bank is syncing and everything is sorted.</p>
      {:else}
        <ul class="attention">
          {#each money.attention as s (s.link.id)}
            <li>
              <span class="mark bad"><Icon name="alert" size={15} /></span>
              <span class="what">
                <b>{s.link.name}</b>
                <span class="why">
                  {s.needsLogin
                    ? 'Wants you to sign in again. It will not sync until you do.'
                    : `Could not sync: ${s.lastError ?? 'unknown error'}`}
                </span>
              </span>
              {#if s.needsLogin}
                <button class="btn btn-outline" onclick={() => void money.connectBank(s.link.id)}>
                  Sign in again
                </button>
              {:else}
                <button
                  class="btn btn-outline"
                  disabled={money.syncing}
                  onclick={() => void money.syncNow(s.link.id)}
                >
                  Try again
                </button>
              {/if}
            </li>
          {/each}
          {#if uncategorized && uncategorized.count > 0}
            <li>
              <span class="mark"><Icon name="tag" size={15} /></span>
              <span class="what">
                <b>{plural(uncategorized.count, 'transaction')} with no category</b>
                <span class="why">
                  {fmt(Math.abs(uncategorized.spent))} of {monthLabel(month)}'s spending is in no
                  budget.
                </span>
              </span>
              <button class="btn btn-outline" onclick={sortUncategorized}>Sort them</button>
            </li>
          {/if}
        </ul>
      {/if}
    </section>

    <div class="pair">
      <section class="card" aria-labelledby="ov-budgets">
        <div class="card-head">
          <h2 class="eyebrow" id="ov-budgets">Budgets running over</h2>
          <button class="link" onclick={() => money.setView('budget')}>The budget</button>
        </div>
        {#if running.length === 0}
          <p class="quiet">
            {summary && summary.budgeted > 0
              ? 'Every budget has room left this month.'
              : 'No budgets set yet. Set them on the Budget, one category at a time.'}
          </p>
        {:else}
          <ul class="budgets">
            {#each running as r (r.category.id)}
              {@const over = r.spent > r.budget}
              <li>
                <span class="b-top">
                  <span class="dot" style:background={r.category.color || 'var(--fg-faint)'}></span>
                  <span class="b-name">{r.category.name}</span>
                  <span class="b-fig" class:over>
                    {over ? `${fmt(r.spent - r.budget)} over` : `${fmt(r.budget - r.spent)} left`}
                  </span>
                </span>
                <span class="track" class:over style:--c={r.category.color || 'var(--accent)'}>
                  <span class="fill" style:width="{Math.min(100, (r.spent / r.budget) * 100)}%"
                  ></span>
                </span>
                <span class="b-note">{fmt(r.spent)} of {fmt(r.budget)}</span>
              </li>
            {/each}
          </ul>
        {/if}
      </section>

      <section class="card" aria-labelledby="ov-upcoming">
        <h2 class="eyebrow" id="ov-upcoming">Coming up</h2>
        {#if upcoming.length === 0}
          <p class="quiet">
            No repeating charge is due in the next month — or there is not enough history yet to
            tell.
          </p>
        {:else}
          <ul class="upcoming">
            {#each upcoming as r (`${r.accountId}:${r.payee}:${r.next}`)}
              <li>
                <span class="u-when">{friendlyDate(r.next)}</span>
                <span class="u-what">
                  <span class="u-payee">{r.payee}</span>
                  <span class="u-cadence">{CADENCE_LABELS[r.cadence]}</span>
                </span>
                <MoneyAmount amount={r.amount} currency={r.currency} size="sm" />
              </li>
            {/each}
          </ul>
        {/if}
      </section>
    </div>

    {#if money.links.length === 0 && money.accounts.length > 0}
      <p class="hint">
        Every account here is kept by hand.
        <button class="link inline" onclick={connect}>Connect a bank</button>
        to have transactions arrive by themselves.
      </p>
    {/if}
  </div>
</div>

<style>
  .overview {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .page {
    max-width: 960px;
    margin: 0 auto;
    padding: var(--sp-2) var(--sp-6) var(--fab-clear);
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
  }

  .tiles {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
    gap: var(--sp-5);
    padding: var(--sp-2) 0 var(--sp-2);
  }

  .card {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg);
  }
  .card-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--sp-3);
  }
  .pair {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
    gap: var(--sp-5);
  }

  .quiet {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }

  ul {
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .attention li {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-2) 0;
  }
  .attention li + li {
    border-top: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }
  .mark {
    flex: none;
    display: grid;
    place-items: center;
    color: var(--fg-subtle);
  }
  .mark.bad {
    color: var(--danger);
  }
  .what {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .what b {
    font-weight: 600;
    font-size: var(--text-base);
    color: var(--fg);
    overflow-wrap: anywhere;
  }
  .why {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    overflow-wrap: anywhere;
  }
  .attention .btn {
    flex: none;
    height: 30px;
    font-size: var(--text-sm);
  }

  .budgets {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .budgets li {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .b-top {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    min-width: 0;
  }
  .dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    align-self: center;
  }
  .b-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-base);
  }
  .b-fig {
    flex: none;
    font-size: var(--text-sm);
    color: var(--fg-muted);
    font-variant-numeric: tabular-nums;
  }
  .b-fig.over {
    color: var(--danger);
    font-weight: 600;
  }
  .b-note {
    font-size: var(--text-xs);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }
  /* As the Budget's own bars: a track in a lighter step of the category's
     colour, the whole bar in the danger colour once it is over. */
  .track {
    display: block;
    width: 100%;
    height: 6px;
    border-radius: 999px;
    background: color-mix(in oklab, var(--c, var(--accent)) 16%, transparent);
    overflow: hidden;
  }
  .fill {
    display: block;
    height: 100%;
    border-radius: 999px;
    background: var(--c, var(--accent));
  }
  .track.over {
    background: color-mix(in oklab, var(--danger) 16%, transparent);
  }
  .track.over .fill {
    background: var(--danger);
  }

  .upcoming li {
    display: flex;
    align-items: baseline;
    gap: var(--sp-3);
    padding: 6px 0;
  }
  .upcoming li + li {
    border-top: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }
  .u-when {
    flex: none;
    width: 6.5em;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .u-what {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .u-payee {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-base);
  }
  .u-cadence {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }

  .link {
    color: var(--accent);
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .link:hover {
    text-decoration: underline;
  }
  .link.inline {
    font-size: inherit;
  }
  .hint {
    margin: 0;
  }
</style>
