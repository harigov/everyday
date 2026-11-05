<script lang="ts">
  // The month's spending against each expense category's budget.
  //
  // One row per category, under its group's heading: the name, a bar of what
  // was spent against the limit, and the two figures. A bar in the
  // category's own colour while it is inside its budget and in the danger
  // colour once it is over -- and the words say "over" as well, so the
  // colour is never the only thing carrying it. A category with no budget
  // has no bar to fill; it shows what was spent and an invitation to set
  // one.
  //
  // The budget is edited where it is read: press the figure, type, Enter.
  // Typed as money is written ("400", "$400", "1,250.50") and kept as whole
  // minor units, never a float.
  //
  // Spending with no category gets its own row at the foot, because it is
  // the one row that is a job rather than a fact: it links to exactly those
  // transactions.

  import { money, type CategoryFilter } from '../lib/money.svelte'
  import { formatMoney, monthLabel, parseMoney, plainMoney } from '../lib/money'
  import { DEFAULT_COLORS, colorName } from '../lib/colors'
  import { plural } from '../lib/format'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import type { CategoryKind, CategorySpend, MoneyCategory, MoneyCategoryId } from '../lib/types'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'

  const currency = $derived(money.mainCurrency)
  const summary = $derived(money.summary)

  const spendById = $derived(
    new Map<MoneyCategoryId | null, CategorySpend>(
      (summary?.categories ?? []).map((c): [MoneyCategoryId | null, CategorySpend] => [
        c.categoryId,
        c,
      ]),
    ),
  )
  const uncategorized = $derived(spendById.get(null) ?? null)

  /** Expense categories, grouped under their headings in the order they come. */
  const groups = $derived.by(() => {
    const order: string[] = []
    const by = new Map<string, MoneyCategory[]>()
    for (const c of money.activeCategories) {
      if (c.kind !== 'expense') continue
      const h = c.group.trim()
      if (!by.has(h)) {
        by.set(h, [])
        order.push(h)
      }
      by.get(h)!.push(c)
    }
    return order.map((name) => {
      const categories = by.get(name)!
      const spent = categories.reduce((n, c) => n + spentIn(c.id), 0)
      const budget = categories.reduce((n, c) => n + (c.budget ?? 0), 0)
      return { name, categories, spent, budget }
    })
  })

  const totals = $derived.by(() => {
    let spent = 0
    let budget = 0
    for (const g of groups) {
      spent += g.spent
      budget += g.budget
    }
    return { spent, budget }
  })

  function spentIn(id: MoneyCategoryId): number {
    return Math.abs(spendById.get(id)?.spent ?? 0)
  }

  function fmt(minor: number): string {
    return formatMoney(minor, currency)
  }

  /** How full the bar is, 0..100. Over budget draws a full bar. */
  function fill(spent: number, budget: number): number {
    if (budget <= 0) return 0
    return Math.min(100, (spent / budget) * 100)
  }

  function showTransactions(filter: CategoryFilter) {
    money.setView('transactions')
    void money.setCategory(filter)
  }

  // ── Editing a budget in place ──────────────────────────────────────

  let editing = $state<MoneyCategoryId | null>(null)
  let draft = $state('')
  let invalid = $state(false)

  function startEdit(c: MoneyCategory) {
    editing = c.id
    draft = c.budget != null ? plainMoney(c.budget, currency) : ''
    invalid = false
  }

  async function commit(c: MoneyCategory) {
    if (editing !== c.id) return
    const text = draft.trim()
    if (text === '') {
      editing = null
      if (c.budget != null) await money.setBudget(c.id, null)
      return
    }
    const parsed = parseMoney(text, currency)
    if (parsed === null) {
      invalid = true
      return
    }
    editing = null
    const amount = Math.abs(parsed)
    if (amount === (c.budget ?? null)) return
    await money.setBudget(c.id, amount === 0 ? null : amount)
  }

  function onEditKey(e: KeyboardEvent, c: MoneyCategory) {
    if (e.key === 'Enter') {
      e.preventDefault()
      void commit(c)
    } else if (e.key === 'Escape') {
      e.preventDefault()
      e.stopPropagation()
      editing = null
    }
  }

  // ── Categories themselves ─────────────────────────────────────────

  let editingCategory = $state<MoneyCategory | null>(null)
  let catName = $state('')
  let catGroup = $state('')
  let catKind = $state<CategoryKind>('expense')
  let catColor = $state('')
  /** The name being typed for a new category, while one is being made. */
  let addingName = $state<string | null>(null)
  /** Which heading the new category goes under. */
  let addingTo = $state<string | null>(null)

  const groupNames = $derived(
    [...new Set(money.activeCategories.map((c) => c.group.trim()).filter(Boolean))].sort((a, b) =>
      a.localeCompare(b),
    ),
  )

  function openCategory(c: MoneyCategory) {
    editingCategory = c
    catName = c.name
    catGroup = c.group
    catKind = c.kind
    catColor = c.color
  }

  async function saveCategory() {
    const c = editingCategory
    if (!c || !catName.trim()) return
    editingCategory = null
    await money.saveCategory({
      ...$state.snapshot(c),
      name: catName.trim(),
      group: catGroup.trim(),
      kind: catKind,
      color: catColor,
    })
  }

  async function addCategory(group: string) {
    const name = (addingName ?? '').trim()
    addingName = null
    if (!name) return
    await money.newCategory(name, group)
  }

  function categoryMenu(c: MoneyCategory): MenuItem[] {
    return tidyMenu([
      {
        label: 'Show its transactions',
        icon: 'list',
        run: () => showTransactions(c.id),
      },
      {
        label: c.budget != null ? 'Change the budget' : 'Set a budget',
        icon: 'target',
        run: () => startEdit(c),
      },
      c.budget != null && {
        label: 'No budget',
        icon: 'minus',
        run: () => void money.setBudget(c.id, null),
      },
      SEP,
      { label: 'Edit…', icon: 'pencil', run: () => openCategory(c) },
      SEP,
      // The store asks first, and says what happens to what was in it.
      {
        label: 'Delete…',
        icon: 'trash',
        danger: true,
        run: () => void money.deleteCategory(c.id),
      },
    ])
  }
</script>

<div class="budget scroll">
  {#if groups.length === 0}
    <EmptyState lead="No spending categories yet.">
      {#snippet note()}
        Categories are what a budget is set on. Make the first one, and the rest as transactions
        need them.
      {/snippet}
      {#snippet action()}
        <button class="btn" onclick={() => (addingName = '')}>New category</button>
      {/snippet}
    </EmptyState>
  {:else}
    <div class="sheet-of-rows">
      <div class="total">
        <div class="total-figures">
          <span class="total-spent">{fmt(totals.spent)}</span>
          <span class="total-of">
            spent in {monthLabel(money.month)}{#if totals.budget > 0}&nbsp;of
              {fmt(totals.budget)} budgeted{/if}
          </span>
        </div>
        {#if totals.budget > 0}
          {@const over = totals.spent > totals.budget}
          <span class="track wide" class:over>
            <span class="fill" style:width="{fill(totals.spent, totals.budget)}%"></span>
          </span>
          <span class="total-left" class:over>
            {over
              ? `${fmt(totals.spent - totals.budget)} over`
              : `${fmt(totals.budget - totals.spent)} left`}
          </span>
        {/if}
      </div>

      {#each groups as g (g.name)}
        <section class="group" aria-label={g.name || 'Ungrouped'}>
          <div class="group-head">
            <span class="eyebrow">{g.name || 'Other'}</span>
            <span class="group-figures">
              {fmt(g.spent)}{#if g.budget > 0}<span class="of">&nbsp;of {fmt(g.budget)}</span>{/if}
            </span>
          </div>

          {#each g.categories as c (c.id)}
            {@const spent = spentIn(c.id)}
            {@const budget = c.budget ?? 0}
            {@const over = budget > 0 && spent > budget}
            {@const count = spendById.get(c.id)?.count ?? 0}
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div class="row" oncontextmenu={(e) => menu.show(e, categoryMenu(c))}>
              <button
                class="name"
                title={count > 0
                  ? `${plural(count, 'transaction')} in ${c.name}`
                  : `Nothing in ${c.name} this month`}
                onclick={() => showTransactions(c.id)}
              >
                <span class="dot" style:background={c.color || 'var(--fg-faint)'}></span>
                <span class="label">{c.name}</span>
              </button>

              <span class="bar">
                {#if budget > 0}
                  <span class="track" class:over style:--c={c.color || 'var(--accent)'}>
                    <span class="fill" style:width="{fill(spent, budget)}%"></span>
                  </span>
                {/if}
              </span>

              <span class="spent" class:quiet={spent === 0}>{fmt(spent)}</span>

              <span class="limit">
                {#if editing === c.id}
                  <input
                    class="edit"
                    class:invalid
                    bind:value={draft}
                    aria-label="Monthly budget for {c.name}"
                    placeholder="No budget"
                    inputmode="decimal"
                    autocomplete="off"
                    use:focusOnMount
                    onkeydown={(e) => onEditKey(e, c)}
                    onblur={() => void commit(c)}
                  />
                {:else if budget > 0}
                  <button
                    class="figure"
                    class:over
                    title="Change the budget"
                    aria-label="Budget for {c.name}: {fmt(budget)}. Change it"
                    onclick={() => startEdit(c)}
                  >
                    {over ? `${fmt(spent - budget)} over` : `of ${fmt(budget)}`}
                  </button>
                {:else}
                  <button
                    class="figure set"
                    aria-label="Set a budget for {c.name}"
                    onclick={() => startEdit(c)}
                  >
                    Set budget
                  </button>
                {/if}
              </span>

              <button
                class="more"
                title="More"
                aria-label="More for {c.name}"
                onclick={(e) => menu.show(e, categoryMenu(c))}
              >
                <Icon name="more" size={15} />
              </button>
            </div>
          {/each}

          {#if addingName !== null && addingTo === g.name}
            <input
              class="side-new new-cat"
              bind:value={addingName}
              placeholder="New category in {g.name || 'Other'}"
              aria-label="New category name"
              use:focusOnMount
              onkeydown={(e) => {
                if (e.key === 'Enter') void addCategory(g.name)
                else if (e.key === 'Escape') {
                  e.stopPropagation()
                  addingName = null
                }
              }}
              onblur={() => void addCategory(g.name)}
            />
          {:else}
            <button
              class="add"
              onclick={() => {
                addingTo = g.name
                addingName = ''
              }}
            >
              <Icon name="plus" size={13} /> New category
            </button>
          {/if}
        </section>
      {/each}

      {#if uncategorized && uncategorized.spent !== 0}
        <section class="group">
          <div class="row none">
            <button class="name" onclick={() => showTransactions('none')}>
              <span class="dot empty"></span>
              <span class="label">No category</span>
            </button>
            <span class="bar">
              <span class="nudge">
                {plural(uncategorized.count, 'transaction')} to sort
              </span>
            </span>
            <span class="spent">{fmt(Math.abs(uncategorized.spent))}</span>
            <span class="limit">
              <button class="figure set" onclick={() => showTransactions('none')}>Sort them</button>
            </span>
            <span class="more-gap"></span>
          </div>
        </section>
      {/if}

      {#if summary && summary.otherCurrencies.length > 0}
        <p class="hint">
          Counted in {summary.currency}. Spending in {summary.otherCurrencies.join(', ')} is not in these
          figures.
        </p>
      {/if}
    </div>
  {/if}
</div>

{#if groups.length === 0 && addingName !== null}
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div class="scrim" onclick={() => (addingName = null)}></div>
  <div class="sheet" role="dialog" aria-modal="true" aria-label="New category" use:trapFocus>
    <h2>New category</h2>
    <input
      class="field"
      bind:value={addingName}
      placeholder="Groceries"
      aria-label="Category name"
      use:focusOnMount
      onkeydown={(e) => {
        if (e.key === 'Enter') void addCategory('')
        else if (e.key === 'Escape') addingName = null
      }}
    />
    <div class="sheet-row">
      <span class="spacer"></span>
      <button class="btn" onclick={() => (addingName = null)}>Cancel</button>
      <button class="btn btn-primary" onclick={() => void addCategory('')}>Add</button>
    </div>
  </div>
{/if}

{#if editingCategory}
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div class="scrim" onclick={() => (editingCategory = null)}></div>
  <div
    class="sheet"
    role="dialog"
    aria-modal="true"
    aria-label="Edit {editingCategory.name}"
    use:trapFocus
  >
    <h2>{editingCategory.name}</h2>
    <form
      class="cat-form"
      onsubmit={(e) => {
        e.preventDefault()
        void saveCategory()
      }}
    >
      <label class="field-label" for="cat-name">Name</label>
      <input id="cat-name" class="field" bind:value={catName} use:focusOnMount autocomplete="off" />

      <label class="field-label" for="cat-group">Heading</label>
      <input
        id="cat-group"
        class="field"
        list="cat-groups"
        bind:value={catGroup}
        placeholder="None"
        autocomplete="off"
      />
      <datalist id="cat-groups">
        {#each groupNames as name (name)}<option value={name}></option>{/each}
      </datalist>

      <label class="field-label" for="cat-kind">Counts as</label>
      <select id="cat-kind" class="field" bind:value={catKind}>
        <option value="expense">Spending</option>
        <option value="income">Income</option>
        <option value="transfer">A transfer between your accounts</option>
      </select>

      <span class="field-label">Colour</span>
      <div class="swatches" role="radiogroup" aria-label="Colour">
        {#each DEFAULT_COLORS as color (color)}
          <button
            type="button"
            class="swatch"
            class:on={catColor.toLowerCase() === color}
            style:background={color}
            role="radio"
            aria-checked={catColor.toLowerCase() === color}
            aria-label={colorName(color)}
            onclick={() => (catColor = color)}
          ></button>
        {/each}
      </div>

      <div class="sheet-row">
        <span class="spacer"></span>
        <button type="button" class="btn" onclick={() => (editingCategory = null)}>Cancel</button>
        <button type="submit" class="btn btn-primary" disabled={!catName.trim()}>Save</button>
      </div>
    </form>
  </div>
{/if}

<style>
  .budget {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
  }
  .sheet-of-rows {
    width: 100%;
    max-width: 860px;
    margin: 0 auto;
    padding: var(--sp-2) var(--sp-6) var(--fab-clear);
  }

  .total {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: var(--sp-3) 0 var(--sp-5);
  }
  .total-figures {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }
  .total-spent {
    font-size: var(--text-xl);
    font-weight: 600;
    color: var(--fg);
  }
  .total-of {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .total-left {
    font-size: var(--text-sm);
    color: var(--fg-muted);
    font-variant-numeric: tabular-nums;
  }
  .total-left.over {
    color: var(--danger);
  }

  .group {
    margin-bottom: var(--sp-5);
  }
  .group-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--sp-3);
    padding: 0 0 var(--sp-1);
    border-bottom: 1px solid var(--border);
  }
  .group-figures {
    font-size: var(--text-xs);
    color: var(--fg-muted);
    font-variant-numeric: tabular-nums;
  }
  .of {
    color: var(--fg-faint);
  }

  /* Name, bar, spent, budget, menu: a grid per row so the figures make
     columns down the whole group. */
  .row {
    display: grid;
    grid-template-columns: minmax(120px, 1.1fr) minmax(80px, 1.4fr) 7.5em 9em 28px;
    align-items: center;
    gap: var(--sp-3);
    min-height: 38px;
    border-bottom: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }
  .row:hover .more {
    opacity: 1;
  }

  .name {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
    text-align: left;
    color: var(--fg);
    font-size: var(--text-base);
  }
  .name:hover .label {
    text-decoration: underline;
    text-decoration-color: var(--fg-faint);
    text-underline-offset: 2px;
  }
  .label {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
  }
  .dot.empty {
    border: 1px dashed var(--border-strong);
  }

  /* A real width for the fill's percentage -- see the house chart notes on
     the bar that drew nothing inside a shrink-to-fit box. The track is a
     lighter step of the category's own colour, so an empty bar still says
     whose it is; over budget, the whole bar turns to the danger colour. */
  .bar {
    display: block;
    min-width: 0;
  }
  .track {
    display: block;
    width: 100%;
    height: 6px;
    border-radius: 999px;
    background: color-mix(in oklab, var(--c, var(--accent)) 16%, transparent);
    overflow: hidden;
  }
  .track.wide {
    height: 8px;
  }
  .fill {
    display: block;
    height: 100%;
    border-radius: 999px;
    background: var(--c, var(--accent));
    transition: width var(--med) var(--ease);
  }
  .track.over {
    background: color-mix(in oklab, var(--danger) 16%, transparent);
  }
  .track.over .fill {
    background: var(--danger);
  }

  .spent {
    text-align: right;
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    color: var(--fg);
  }
  .spent.quiet {
    color: var(--fg-faint);
  }

  .limit {
    display: flex;
    justify-content: flex-end;
    min-width: 0;
  }
  .figure {
    height: 26px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .figure:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .figure.over {
    color: var(--danger);
    font-weight: 600;
  }
  .figure.set {
    color: var(--fg-faint);
  }
  .figure.set:hover {
    color: var(--accent);
  }
  .edit {
    width: 100%;
    height: 28px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--accent);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    color: var(--fg);
    font-size: var(--text-sm);
    text-align: right;
    font-variant-numeric: tabular-nums;
    user-select: text;
  }
  .edit:focus {
    outline: none;
  }
  .edit.invalid {
    border-color: var(--danger);
  }

  .more {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    color: var(--fg-faint);
    opacity: 0;
    transition: opacity var(--fast) var(--ease);
  }
  .more:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .more:focus-visible {
    opacity: 1;
  }

  .nudge {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }

  .add {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-1);
    margin-top: var(--sp-1);
    height: 28px;
    padding: 0 var(--sp-2);
    border-radius: 999px;
    font-size: var(--text-sm);
    color: var(--fg-faint);
  }
  .add:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .new-cat {
    max-width: 320px;
  }

  .hint {
    margin-top: var(--sp-2);
  }

  .sheet h2 {
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
    margin-bottom: var(--sp-3);
  }
  .cat-form {
    display: flex;
    flex-direction: column;
  }
  .cat-form .field-label {
    margin-top: var(--sp-3);
  }
  .swatches {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }
  .swatch {
    width: 22px;
    height: 22px;
    border-radius: 50%;
    border: 2px solid transparent;
    box-shadow: inset 0 0 0 2px var(--bg-raised);
  }
  .swatch.on {
    border-color: var(--fg);
  }
</style>
