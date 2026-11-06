<script lang="ts">
  // Where the money went: a year of cash flow, the month's spending by
  // category, and net worth over time.
  //
  // Three questions, three forms, and never a donut:
  //
  //   cash flow     paired columns per month, money in beside money out --
  //                 two magnitudes compared month by month, from one baseline
  //   by category   a ranked table with a bar per row (`RankTable`), because
  //                 every row is a name worth reading and carries two figures
  //   net worth     a line, because it is a level that exists on every day in
  //                 between, unlike the counts `ColumnChart` draws
  //
  // The colours follow `MoneyAmount`: money in is the theme's green, money out
  // the neutral text colour. Neutral rather than a second hue on purpose --
  // it keeps the two apart by lightness as well as hue, so they still
  // separate for somebody who cannot tell green from orange, and the fixed
  // order (in on the left, out on the right), the legend and the table under
  // each chart mean colour is never the only way to tell them apart.
  //
  // Labels are sparse. Each column says its month and nothing else; the
  // figures are in the tooltip and in the table, which is the chart's
  // accessible twin rather than an extra.

  import { money } from '../lib/money.svelte'
  import { formatMoney, monthLabel, thisMonth } from '../lib/money'
  import { dateFormat } from '../lib/format'
  import type { MoneyCategoryId } from '../lib/types'
  import RankTable from './RankTable.svelte'

  const currency = $derived(money.mainCurrency)
  const month = $derived(money.month || thisMonth())

  function fmt(minor: number, compact = false): string {
    return formatMoney(minor, currency, { compact })
  }

  // ── Cash flow ──────────────────────────────────────────────────────

  const flow = $derived(
    money.cashFlow.map((m) => ({
      month: m.month,
      // Bars cannot go below the axis; a month whose refunds outweighed
      // its spending draws none, and the totals below still count it.
      income: Math.max(0, m.income),
      spending: Math.max(0, m.spending),
    })),
  )
  const flowCeiling = $derived(Math.max(1, ...flow.map((m) => Math.max(m.income, m.spending))))
  const flowTotals = $derived(
    money.cashFlow.reduce(
      (t, m) => ({ income: t.income + m.income, spending: t.spending + m.spending }),
      {
        income: 0,
        spending: 0,
      },
    ),
  )

  const shortMonth = dateFormat({ month: 'short' })
  const shortMonthYear = dateFormat({ month: 'short', year: '2-digit' })

  /** "Nov", or "Jan 26" where a year turns, so twelve columns say where they are. */
  function columnLabel(m: string, i: number): string {
    const at = new Date(`${m}-01T00:00:00`)
    return i === 0 || m.endsWith('-01') ? shortMonthYear.format(at) : shortMonth.format(at)
  }

  function pct(value: number): number {
    return (value / flowCeiling) * 100
  }

  // ── Spending by category ───────────────────────────────────────────

  const spendRows = $derived.by(() => {
    const summary = money.summary
    if (!summary) return []
    return summary.categories
      .filter((c) => c.spent !== 0)
      .map((c) => {
        const category = money.categoryById(c.categoryId)
        // Signed: a category refunded more than it spent shows below zero.
        const spent = c.spent
        const budget = c.budget ?? category?.budget ?? null
        return {
          id: c.categoryId ?? 'none',
          name: category?.name ?? 'No category',
          dot: category?.color || undefined,
          detail: c.count === 1 ? '1 transaction' : `${c.count} transactions`,
          value: Math.max(0, spent),
          cells: [fmt(spent), budget ? fmt(budget) : '—'],
          onpick: () => showCategory(c.categoryId),
          pickLabel: `Show ${category?.name ?? 'uncategorised'} transactions`,
        }
      })
      .sort((a, b) => b.value - a.value)
  })

  function showCategory(id: MoneyCategoryId | null) {
    money.setView('transactions')
    void money.setCategory(id ?? 'none')
  }

  // ── Net worth ──────────────────────────────────────────────────────

  const worth = $derived(
    money.netWorth.map((p) => ({
      day: p.day,
      value: p.assets - Math.abs(p.liabilities),
      assets: p.assets,
      liabilities: Math.abs(p.liabilities),
    })),
  )
  const worthRange = $derived.by(() => {
    if (worth.length === 0) return { lo: 0, hi: 1 }
    let lo = Math.min(...worth.map((p) => p.value))
    let hi = Math.max(...worth.map((p) => p.value))
    if (hi === lo) {
      hi += Math.max(1, Math.abs(hi) * 0.05)
      lo -= Math.max(1, Math.abs(lo) * 0.05)
    }
    const pad = (hi - lo) * 0.08
    return { lo: lo - pad, hi: hi + pad }
  })

  function x(i: number): number {
    return worth.length > 1 ? (i / (worth.length - 1)) * 100 : 50
  }
  function y(v: number): number {
    const { lo, hi } = worthRange
    return 100 - ((v - lo) / (hi - lo)) * 100
  }

  const linePoints = $derived(worth.map((p, i) => `${x(i)},${y(p.value)}`).join(' '))
  const zeroY = $derived(worthRange.lo < 0 && worthRange.hi > 0 ? y(0) : null)
  const worthFirst = $derived(worth[0])
  const worthLast = $derived(worth[worth.length - 1])
  const worthHigh = $derived(
    worth.reduce<(typeof worth)[number] | null>(
      (best, p) => (!best || p.value > best.value ? p : best),
      null,
    ),
  )

  const dayFormat = dateFormat({ day: 'numeric', month: 'short', year: 'numeric' })
  function dayLabel(iso: string): string {
    return dayFormat.format(new Date(`${iso}T00:00:00`))
  }

  /** The point under the pointer, for the crosshair. */
  let hover = $state<number | null>(null)

  function track(e: PointerEvent & { currentTarget: HTMLElement }) {
    if (worth.length === 0) return
    const box = e.currentTarget.getBoundingClientRect()
    const at = Math.min(1, Math.max(0, (e.clientX - box.left) / Math.max(1, box.width)))
    hover = Math.round(at * (worth.length - 1))
  }

  const hovered = $derived(hover !== null ? (worth[hover] ?? null) : null)
</script>

<div class="reports scroll">
  <div class="page">
    <!-- Cash flow -->
    <section class="card" aria-labelledby="rep-flow">
      <div class="card-head">
        <h2 class="eyebrow" id="rep-flow">Cash flow</h2>
        <div class="legend" aria-hidden="true">
          <span class="key"><span class="swatch in"></span>In</span>
          <span class="key"><span class="swatch out"></span>Out</span>
        </div>
      </div>

      {#if flow.length === 0}
        <p class="none">Nothing to show yet. A month appears here once money has moved in it.</p>
      {:else}
        <p class="sub">
          {fmt(flowTotals.income)} in and {fmt(flowTotals.spending)} out over {flow.length}
          {flow.length === 1 ? 'month' : 'months'},
          {flowTotals.income >= flowTotals.spending
            ? `${fmt(flowTotals.income - flowTotals.spending)} kept`
            : `${fmt(flowTotals.spending - flowTotals.income)} more out than in`}.
        </p>
        <div
          class="flow"
          role="img"
          aria-label="Money in and out for each of the last {flow.length} months; the table below has the figures"
        >
          {#each flow as m, i (m.month)}
            <div
              class="slot"
              class:current={m.month === month}
              title="{monthLabel(m.month)} — in {fmt(m.income)}, out {fmt(m.spending)}"
            >
              <div class="bars">
                {#if m.income > 0}
                  <span class="bar in" style:height="max(2px, {pct(m.income)}%)"></span>
                {:else}
                  <span class="bar gap"></span>
                {/if}
                {#if m.spending > 0}
                  <span class="bar out" style:height="max(2px, {pct(m.spending)}%)"></span>
                {:else}
                  <span class="bar gap"></span>
                {/if}
              </div>
              <span class="mlabel">{columnLabel(m.month, i)}</span>
            </div>
          {/each}
        </div>
        <div class="axis">
          <span>{fmt(flowCeiling, true)} at most</span>
        </div>
        <details class="twin">
          <summary>As a table</summary>
          <table>
            <thead>
              <tr>
                <th scope="col">Month</th>
                <th scope="col" class="num">In</th>
                <th scope="col" class="num">Out</th>
                <th scope="col" class="num">Kept</th>
              </tr>
            </thead>
            <tbody>
              {#each flow as m (m.month)}
                <tr>
                  <th scope="row">{monthLabel(m.month)}</th>
                  <td class="num">{fmt(m.income)}</td>
                  <td class="num">{fmt(m.spending)}</td>
                  <td class="num">{formatMoney(m.income - m.spending, currency, { sign: true })}</td
                  >
                </tr>
              {/each}
            </tbody>
          </table>
        </details>
      {/if}
    </section>

    <!-- Spending by category -->
    <section class="card" aria-labelledby="rep-cats">
      <div class="card-head">
        <h2 class="eyebrow" id="rep-cats">Spending in {monthLabel(month)}</h2>
        {#if money.summary}
          <span class="card-figure">{fmt(money.summary.spending)}</span>
        {/if}
      </div>
      <RankTable
        head="Category"
        columns={['Spent', 'Budget']}
        rows={spendRows}
        limit={12}
        empty="Nothing spent this month yet."
        color="var(--fg-muted)"
        figureWidth="7em"
      />
    </section>

    <!-- Net worth -->
    <section class="card" aria-labelledby="rep-worth">
      <div class="card-head">
        <h2 class="eyebrow" id="rep-worth">Net worth</h2>
        {#if worthLast}
          <span class="card-figure">{fmt(worthLast.value)}</span>
        {/if}
      </div>

      {#if worth.length === 0}
        <p class="none">
          Net worth is drawn from account balances as they sync. It fills in from the first one.
        </p>
      {:else}
        <div
          class="worth"
          role="img"
          aria-label="Net worth from {dayLabel(worthFirst?.day ?? '')} to {dayLabel(
            worthLast?.day ?? '',
          )}, now {fmt(worthLast?.value ?? 0)}; the table below has the figures"
          onpointermove={track}
          onpointerleave={() => (hover = null)}
        >
          <svg viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
            {#if zeroY !== null}
              <line class="zero" x1="0" x2="100" y1={zeroY} y2={zeroY} />
            {/if}
            {#if worth.length > 1}
              <polyline class="line" points={linePoints} />
            {/if}
          </svg>
          {#if worth.length === 1}
            <span class="lone" style:left="50%" style:top="{y(worth[0]?.value ?? 0)}%"></span>
          {/if}
          {#if hover !== null && hovered}
            <span class="rule" style:left="{x(hover)}%"></span>
            <span class="marker" style:left="{x(hover)}%" style:top="{y(hovered.value)}%"></span>
            <span class="tip" class:flip={x(hover) > 60} style:left="{x(hover)}%">
              <b>{fmt(hovered.value)}</b>
              <span>{dayLabel(hovered.day)}</span>
            </span>
          {/if}
        </div>
        <div class="axis split">
          <span>{worthFirst ? dayLabel(worthFirst.day) : ''}</span>
          {#if worthHigh && worthHigh !== worthLast}
            <span>Highest {fmt(worthHigh.value)}, {dayLabel(worthHigh.day)}</span>
          {/if}
          <span>{worthLast ? dayLabel(worthLast.day) : ''}</span>
        </div>
        {#if worthLast}
          <p class="sub">
            {fmt(worthLast.assets)} in what you have, less {fmt(worthLast.liabilities)} owed.
          </p>
        {/if}
        <details class="twin">
          <summary>As a table</summary>
          <table>
            <thead>
              <tr>
                <th scope="col">Day</th>
                <th scope="col" class="num">Assets</th>
                <th scope="col" class="num">Owed</th>
                <th scope="col" class="num">Net worth</th>
              </tr>
            </thead>
            <tbody>
              {#each worth.slice(-60).reverse() as p (p.day)}
                <tr>
                  <th scope="row">{dayLabel(p.day)}</th>
                  <td class="num">{fmt(p.assets)}</td>
                  <td class="num">{fmt(p.liabilities)}</td>
                  <td class="num">{fmt(p.value)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
          {#if worth.length > 60}<p class="none">The most recent 60 days.</p>{/if}
        </details>
      {/if}
    </section>
  </div>
</div>

<style>
  .reports {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    /* Money in and money out, as `MoneyAmount` colours them. */
    --money-in: color-mix(in oklab, var(--aqi-1) 82%, var(--fg));
    --money-out: var(--fg-muted);
  }
  .page {
    max-width: 960px;
    margin: 0 auto;
    padding: var(--sp-2) var(--sp-6) var(--fab-clear);
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
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
  .card-figure {
    font-size: var(--text-md);
    font-weight: 600;
    color: var(--fg);
  }
  .sub {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    line-height: var(--leading-snug);
  }
  .none {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--fg-faint);
    line-height: var(--leading-normal);
  }

  .legend {
    display: flex;
    gap: var(--sp-3);
    font-size: var(--text-xs);
    color: var(--fg-muted);
  }
  .key {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .swatch {
    width: 10px;
    height: 10px;
    border-radius: 2px;
  }
  .swatch.in {
    background: var(--money-in);
  }
  .swatch.out {
    background: var(--money-out);
  }

  /* ── Cash flow ─────────────────────────────────────────────────── */

  /* One slot per month, whether or not anything moved, so the spacing is
     time rather than data. The plot has a real height for the bars'
     percentages to resolve against -- see the house notes on the bar chart
     that drew nothing. */
  .flow {
    display: flex;
    align-items: stretch;
    gap: var(--sp-2);
    padding-top: var(--sp-2);
  }
  .slot {
    flex: 1 1 0;
    min-width: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--sp-1);
    border-radius: var(--radius-sm);
  }
  .slot:hover {
    background: var(--bg-hover);
  }
  .bars {
    display: flex;
    align-items: flex-end;
    justify-content: center;
    gap: 2px;
    width: 100%;
    height: 160px;
    border-bottom: 1px solid var(--border);
  }
  /* Thin, 4px rounded at the data end, square on the baseline, and a 2px
     surface gap between the pair. */
  .bar {
    flex: 0 1 14px;
    min-width: 3px;
    border-radius: 4px 4px 0 0;
  }
  .bar.in {
    background: var(--money-in);
  }
  .bar.out {
    background: var(--money-out);
  }
  .bar.gap {
    height: 0;
  }
  .mlabel {
    font-size: var(--text-xs);
    color: var(--fg-faint);
    white-space: nowrap;
  }
  .slot.current .mlabel {
    color: var(--fg);
    font-weight: 600;
  }

  .axis {
    display: flex;
    justify-content: flex-end;
    gap: var(--sp-2);
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
  .axis.split {
    justify-content: space-between;
  }

  /* ── Net worth ─────────────────────────────────────────────────── */

  .worth {
    position: relative;
    height: 180px;
    border-bottom: 1px solid var(--border);
    cursor: crosshair;
  }
  .worth svg {
    display: block;
    width: 100%;
    height: 100%;
    overflow: visible;
  }
  .line {
    fill: none;
    stroke: var(--accent);
    stroke-width: 2px;
    stroke-linejoin: round;
    stroke-linecap: round;
    vector-effect: non-scaling-stroke;
  }
  /* A solid hairline at zero, only when the line crosses it. */
  .zero {
    stroke: var(--border-strong);
    stroke-width: 1px;
    vector-effect: non-scaling-stroke;
  }
  .lone,
  .marker {
    position: absolute;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    translate: -50% -50%;
    background: var(--accent);
    box-shadow: 0 0 0 2px var(--bg);
    pointer-events: none;
  }
  .rule {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 1px;
    background: var(--border-strong);
    pointer-events: none;
  }
  .tip {
    position: absolute;
    top: 0;
    margin-left: var(--sp-2);
    display: flex;
    flex-direction: column;
    padding: 4px var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    box-shadow: var(--shadow-sm);
    font-size: var(--text-xs);
    color: var(--fg-muted);
    white-space: nowrap;
    pointer-events: none;
  }
  .tip.flip {
    translate: calc(-100% - var(--sp-4)) 0;
  }
  .tip b {
    color: var(--fg);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }

  /* ── The tables under the charts ───────────────────────────────── */

  .twin summary {
    cursor: pointer;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .twin table {
    width: 100%;
    margin-top: var(--sp-2);
    border-collapse: collapse;
    font-size: var(--text-sm);
  }
  .twin th,
  .twin td {
    padding: 4px 0;
    text-align: left;
    font-weight: 400;
    color: var(--fg-muted);
    border-bottom: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }
  .twin thead th {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .twin .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
</style>
