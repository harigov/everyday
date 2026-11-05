<script lang="ts">
  // The rules that sort transactions: "anything from this payee is that".
  //
  // Each rule is written out as the sentence it is, because that is how
  // somebody checks one -- "When the payee contains “TRADER JOE”, put it in
  // Groceries" -- rather than as a row of fields to decode. The first rule
  // that matches wins, so the order matters and can be changed.
  //
  // A rule never overrides a person: a transaction somebody categorised by
  // hand keeps their choice, and running the rules only touches what is
  // still open. Most are made from a transaction ("Always put … in …" in its
  // detail), which is where the need for one is noticed; here they are also
  // written from nothing, corrected, reordered, run and removed. What a run
  // sorted is the store's `note`, shown under the pane.

  import { money } from '../lib/money.svelte'
  import { formatMoney, parseMoney, plainMoney } from '../lib/money'
  import { focusOnMount } from '../lib/focus'
  import type { MoneyCategoryId, MoneyRule, MoneyRuleId } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import MoneyCategoryPicker from './MoneyCategoryPicker.svelte'

  const currency = $derived(money.mainCurrency)
  const rules = $derived(money.rules)

  let running = $state(false)
  let deleting = $state<MoneyRule | null>(null)
  /** A rule being written from nothing, not yet saved. */
  let fresh = $state<MoneyRule | null>(null)

  async function apply(id: MoneyRuleId | null) {
    if (running) return
    running = true
    try {
      await money.applyRules(id)
    } finally {
      running = false
    }
  }

  /** Swap a rule with its neighbour, which is how the order is changed. */
  async function move(index: number, delta: -1 | 1) {
    const a = rules[index]
    const b = rules[index + delta]
    if (!a || !b) return
    const [orderA, orderB] =
      a.sortOrder === b.sortOrder ? [index + delta, index] : [b.sortOrder, a.sortOrder]
    await money.saveRule({ ...$state.snapshot(a), sortOrder: orderA })
    await money.saveRule({ ...$state.snapshot(b), sortOrder: orderB })
  }

  function amountText(rule: MoneyRule): string | null {
    const lo = rule.minAmount
    const hi = rule.maxAmount
    if (lo != null && hi != null) {
      return `between ${formatMoney(lo, currency)} and ${formatMoney(hi, currency)}`
    }
    if (lo != null) return `of ${formatMoney(lo, currency)} or more`
    if (hi != null) return `of ${formatMoney(hi, currency)} or less`
    return null
  }

  // ── Editing one ────────────────────────────────────────────────────

  let editing = $state<MoneyRuleId | null>(null)
  let text = $state('')
  let exact = $state(false)
  let account = $state<string>('')
  let min = $state('')
  let max = $state('')
  let category = $state<MoneyCategoryId | null>(null)
  let rename = $state('')
  let exclude = $state(false)
  let formError = $state<string | null>(null)

  function edit(rule: MoneyRule) {
    editing = rule.id
    text = rule.text
    exact = rule.exact
    account = rule.accountId ?? ''
    min = rule.minAmount != null ? plainMoney(rule.minAmount, currency) : ''
    max = rule.maxAmount != null ? plainMoney(rule.maxAmount, currency) : ''
    category = rule.category ?? null
    rename = rule.rename
    exclude = rule.exclude
    formError = null
    if (fresh && fresh.id !== rule.id) fresh = null
  }

  function startNew() {
    const blank = money.blankRule()
    fresh = blank
    edit(blank)
  }

  function cancel() {
    editing = null
    fresh = null
  }

  /** An amount field: empty is "no limit", anything else must read as money. */
  function amountOf(raw: string): number | null | undefined {
    const t = raw.trim()
    if (t === '') return null
    const parsed = parseMoney(t, currency)
    return parsed === null ? undefined : parsed
  }

  async function save(rule: MoneyRule) {
    if (!text.trim()) {
      formError = 'Say what to look for.'
      return
    }
    const lo = amountOf(min)
    const hi = amountOf(max)
    if (lo === undefined || hi === undefined) {
      formError = 'An amount could not be read.'
      return
    }
    if (lo !== null && hi !== null && lo > hi) {
      formError = 'The smallest amount is larger than the largest.'
      return
    }
    if (!category && !rename.trim() && !exclude) {
      formError = 'Choose something for the rule to do.'
      return
    }
    editing = null
    fresh = null
    await money.saveRule({
      ...$state.snapshot(rule),
      text: text.trim(),
      exact,
      accountId: account || null,
      minAmount: lo,
      maxAmount: hi,
      category,
      rename: rename.trim(),
      exclude,
    })
  }
</script>

{#snippet editor(rule: MoneyRule)}
  <form
    class="editor"
    onsubmit={(e) => {
      e.preventDefault()
      void save(rule)
    }}
  >
    <div class="grid">
      <label for="rule-text">When the payee</label>
      <div class="inline">
        <select
          value={exact ? 'exact' : 'contains'}
          aria-label="How it matches"
          onchange={(e) => (exact = e.currentTarget.value === 'exact')}
        >
          <option value="contains">contains</option>
          <option value="exact">is exactly</option>
        </select>
        <input
          id="rule-text"
          class="field small"
          bind:value={text}
          placeholder="TRADER JOE"
          autocomplete="off"
          spellcheck="false"
          use:focusOnMount
        />
      </div>

      <label for="rule-account">In</label>
      <select id="rule-account" class="field small" bind:value={account}>
        <option value="">Any account</option>
        {#each money.accounts as a (a.id)}
          <option value={a.id}>{a.name}{a.mask ? ` ··${a.mask}` : ''}</option>
        {/each}
      </select>

      <span class="lab">Amount</span>
      <div class="inline">
        <input
          class="field small"
          bind:value={min}
          placeholder="Smallest"
          aria-label="Smallest amount"
          inputmode="decimal"
          autocomplete="off"
        />
        <span class="to">to</span>
        <input
          class="field small"
          bind:value={max}
          placeholder="Largest"
          aria-label="Largest amount"
          inputmode="decimal"
          autocomplete="off"
        />
      </div>
      <span></span>
      <p class="hint small">
        Optional. Money out is negative, as in the list: −50 to −10 is a spend of ten to fifty.
      </p>

      <span class="lab">Put it in</span>
      <MoneyCategoryPicker
        value={category}
        onchange={(id: MoneyCategoryId | null) => (category = id)}
        placeholder="Leave the category alone"
        label="Category"
      />

      <label for="rule-rename">Name it</label>
      <input
        id="rule-rename"
        class="field small"
        bind:value={rename}
        placeholder="Leave the name alone"
        autocomplete="off"
      />

      <span></span>
      <label class="check">
        <input type="checkbox" bind:checked={exclude} />
        Leave it out of reports and budgets
      </label>
    </div>

    {#if formError}<p class="error">{formError}</p>{/if}

    <div class="editor-row">
      <span class="spacer"></span>
      <button type="button" class="btn" onclick={cancel}>Cancel</button>
      <button type="submit" class="btn btn-primary">Save</button>
    </div>
  </form>
{/snippet}

<div class="rules scroll">
  {#if rules.length === 0 && !fresh}
    <EmptyState lead="No rules yet.">
      {#snippet note()}
        Open a transaction and choose “Always put … in …” under its category, and everything from
        that payee is sorted the same way from then on — or write one here.
      {/snippet}
      {#snippet action()}
        <button class="btn" onclick={startNew}>New rule</button>
      {/snippet}
    </EmptyState>
  {:else}
    <div class="page">
      <div class="intro">
        <p class="hint">
          New transactions are sorted by the first rule that matches. A category you choose by hand
          always stands; rules only fill in what is still open.
        </p>
        <button class="btn btn-outline" disabled={!!fresh} onclick={startNew}>
          <Icon name="plus" size={14} /> New rule
        </button>
        <button
          class="btn btn-outline"
          disabled={running || rules.length === 0}
          onclick={() => void apply(null)}
        >
          <Icon name="refresh" size={14} />
          {running ? 'Sorting…' : 'Run on everything unsorted'}
        </button>
      </div>

      <ol class="list">
        {#if fresh && editing === fresh.id}
          <li class="rule open">{@render editor(fresh)}</li>
        {/if}
        {#each rules as rule, i (rule.id)}
          {@const cat = money.categoryById(rule.category)}
          {@const acct = money.accountById(rule.accountId)}
          {@const amount = amountText(rule)}
          <li class="rule" class:open={editing === rule.id}>
            {#if editing === rule.id}
              {@render editor(rule)}
            {:else}
              <div class="order">
                <button
                  class="icon-btn"
                  disabled={i === 0}
                  aria-label="Move up"
                  title="Move up: tried sooner"
                  onclick={() => void move(i, -1)}
                >
                  <span class="up"><Icon name="chevron" size={12} /></span>
                </button>
                <button
                  class="icon-btn"
                  disabled={i === rules.length - 1}
                  aria-label="Move down"
                  title="Move down: tried later"
                  onclick={() => void move(i, 1)}
                >
                  <span class="down"><Icon name="chevron" size={12} /></span>
                </button>
              </div>

              <p class="sentence">
                When the payee {rule.exact ? 'is' : 'contains'}
                <q class="match">{rule.text}</q>{#if acct}
                  in {acct.name}{/if}{#if amount}, {amount}{/if}:
                {#if cat}
                  put it in
                  <span class="chip">
                    <span class="dot" style:background={cat.color || 'var(--fg-faint)'}></span>
                    {cat.name}
                  </span>
                {/if}
                {#if rule.rename}
                  {cat ? ' and ' : ''}name it <q class="match">{rule.rename}</q>
                {/if}
                {#if rule.exclude}
                  {cat || rule.rename ? ' and ' : ''}leave it out of reports
                {/if}
                {#if rule.category && !cat}
                  <span class="gone">(its category is gone)</span>
                {/if}
              </p>

              <div class="actions">
                <button
                  class="icon-btn"
                  disabled={running}
                  aria-label="Run this rule"
                  title="Run this rule on everything unsorted"
                  onclick={() => void apply(rule.id)}
                >
                  <Icon name="refresh" size={14} />
                </button>
                <button
                  class="icon-btn"
                  aria-label="Edit this rule"
                  title="Edit"
                  onclick={() => edit(rule)}
                >
                  <Icon name="pencil" size={14} />
                </button>
                <button
                  class="icon-btn danger"
                  aria-label="Delete this rule"
                  title="Delete"
                  onclick={() => (deleting = rule)}
                >
                  <Icon name="trash" size={14} />
                </button>
              </div>
            {/if}
          </li>
        {/each}
      </ol>
    </div>
  {/if}
</div>

{#if deleting}
  <ConfirmDialog
    title={`Delete the rule for “${deleting.text}”?`}
    detail="What it already sorted stays sorted. New transactions from this payee will not be."
    onconfirm={() => {
      const doomed = deleting
      deleting = null
      if (doomed) void money.deleteRule(doomed.id)
    }}
    oncancel={() => (deleting = null)}
  />
{/if}

<style>
  .rules {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
  }
  .page {
    width: 100%;
    max-width: 860px;
    margin: 0 auto;
    padding: var(--sp-2) var(--sp-6) var(--fab-clear);
  }

  .intro {
    display: flex;
    align-items: center;
    gap: var(--sp-4);
    padding: var(--sp-2) 0 var(--sp-4);
  }
  .intro .hint {
    flex: 1;
    margin: 0;
  }
  .intro .btn {
    flex: none;
    height: 32px;
    font-size: var(--text-sm);
  }

  .list {
    margin: 0;
    padding: 0;
    list-style: none;
    border-top: 1px solid var(--border);
  }
  .rule {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-height: 48px;
    padding: var(--sp-2) 0;
    border-bottom: 1px solid var(--border);
  }
  .rule:hover .actions,
  .rule:focus-within .actions {
    opacity: 1;
  }
  .rule.open {
    display: block;
    padding: var(--sp-4) 0;
  }

  .order {
    flex: none;
    display: flex;
    flex-direction: column;
  }
  .up,
  .down {
    display: grid;
    place-items: center;
  }
  .up {
    rotate: -90deg;
  }
  .down {
    rotate: 90deg;
  }

  .sentence {
    flex: 1;
    min-width: 0;
    margin: 0;
    font-size: var(--text-base);
    line-height: var(--leading-snug);
    color: var(--fg-muted);
    overflow-wrap: anywhere;
  }
  .match {
    color: var(--fg);
    font-weight: 550;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 22px;
    padding: 0 var(--sp-2);
    border-radius: 999px;
    background: var(--bg-sunken);
    font-size: var(--text-sm);
    color: var(--fg);
    vertical-align: middle;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
  }
  .gone {
    color: var(--danger);
    font-size: var(--text-sm);
  }

  .actions {
    flex: none;
    display: flex;
    gap: 2px;
    opacity: 0.35;
    transition: opacity var(--fast) var(--ease);
  }
  .icon-btn {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .order .icon-btn {
    width: 22px;
    height: 18px;
    border-radius: var(--radius-sm);
  }
  .icon-btn:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .icon-btn.danger:hover:not(:disabled) {
    background: color-mix(in oklab, var(--danger) 12%, transparent);
    color: var(--danger);
  }
  .icon-btn:disabled {
    opacity: 0.3;
  }

  .editor {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .grid {
    display: grid;
    grid-template-columns: 120px minmax(0, 1fr);
    align-items: center;
    gap: var(--sp-2) var(--sp-3);
  }
  .grid > label,
  .lab {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .inline {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }
  .inline select {
    flex: none;
    height: 32px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    color: var(--fg);
    font-size: var(--text-sm);
  }
  .field.small {
    height: 32px;
    font-size: var(--text-sm);
  }
  .to {
    font-size: var(--text-sm);
    color: var(--fg-faint);
  }
  .hint.small {
    margin: 0;
    font-size: var(--text-xs);
  }
  .check {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-sm);
    color: var(--fg-muted);
    cursor: pointer;
  }
  .check input {
    accent-color: var(--accent);
  }
  .editor-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .spacer {
    flex: 1;
  }
</style>
