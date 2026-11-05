<script lang="ts">
  // The transactions list: one month, narrowed by account and category,
  // searched from the bar at the top of the window.
  //
  // Rows are calm and dense, because a month is a couple of hundred of them:
  // the date, who it was, its category as a small chip, the account when the
  // list is not already one account's, and the amount on the right in
  // tabular figures so the column reads down. A pending one says so; one left
  // out of reports is dimmed and its amount struck through.
  //
  // Picking works the way the mail list's does. A row's tick box (or
  // Ctrl/Cmd+click) ticks it; Shift+click ticks everything from the last one
  // ticked; and once anything is ticked, the filter bar gives way to what can
  // be done to all of them at once. A plain click opens the row in the rail.
  //
  // j and k step through the list, x ticks the open row and Escape closes
  // it: those are `shortcuts.svelte.ts`'s Money rows. The one key this list
  // keeps for itself is Escape with nothing open, which unticks everything,
  // under the same rules as that table: never while typing, never under a
  // dialog, never with a modifier held.
  //
  // Deleting asks first -- the store does the asking (`money.question`) --
  // and is offered only for what was imported or typed in.

  import { untrack } from 'svelte'
  import { money } from '../lib/money.svelte'
  import { formatMoney, parseMoney, shownPayee } from '../lib/money'
  import { dateFormat } from '../lib/format'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import { isTyping } from '../lib/keys'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import type { MoneyCategory, MoneyCategoryId, NewTxn, Txn } from '../lib/types'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import MoneyAmount from './MoneyAmount.svelte'
  import MoneyCategoryPicker from './MoneyCategoryPicker.svelte'
  import VirtualList from './VirtualList.svelte'

  const rows = $derived(money.page?.txns ?? [])
  const picked = $derived(new Set(money.selected))
  /** The ticked rows that are in the loaded page -- what the bar can say anything about. */
  const pickedRows = $derived(rows.filter((t) => picked.has(t.id)))
  const allPicked = $derived(rows.length > 0 && pickedRows.length === rows.length)
  /** Only what was imported or typed in can be deleted; a bank's own rows come back on sync. */
  const deletable = $derived(pickedRows.length > 0 && pickedRows.every((t) => t.origin !== 'bank'))
  const allExcluded = $derived(pickedRows.length > 0 && pickedRows.every((t) => t.excluded))

  const narrowed = $derived(
    money.accountFilter !== null || money.categoryFilter !== null || money.query.trim() !== '',
  )
  const manualAccounts = $derived(money.visibleAccounts.filter((a) => !a.linkId))

  // ── Filters ─────────────────────────────────────────────────────────

  /** Category headings for the filter's option groups, in the Budget's order. */
  const categoryGroups = $derived.by(() => {
    const order: string[] = []
    const by = new Map<string, MoneyCategory[]>()
    for (const c of money.activeCategories) {
      const h = c.group.trim() || 'Other'
      if (!by.has(h)) {
        by.set(h, [])
        order.push(h)
      }
      by.get(h)!.push(c)
    }
    return order.map((name) => ({ name, categories: by.get(name)! }))
  })

  function pickAccount(raw: string) {
    void money.setAccount(raw === '' ? null : raw)
  }

  function pickCategory(raw: string) {
    void money.setCategory(raw === '' ? null : raw)
  }

  function clearFilters() {
    if (money.query.trim() !== '') money.setQuery('')
    if (money.categoryFilter !== null) void money.setCategory(null)
    if (money.accountFilter !== null) void money.setAccount(null)
  }

  // ── Rows ────────────────────────────────────────────────────────────

  const dayFormat = dateFormat({ month: 'short', day: 'numeric' })

  function rowDate(t: Txn): string {
    return dayFormat.format(new Date(`${t.date}T00:00:00`))
  }

  function rowKey(t: Txn): string {
    return t.id
  }

  function clickRow(e: MouseEvent, t: Txn) {
    if (e.shiftKey) money.toggleSelect(t.id, true)
    else if (e.ctrlKey || e.metaKey) money.toggleSelect(t.id)
    else money.open(money.openId === t.id ? null : t.id)
  }

  /** Shift+click would otherwise also select the text of every row between. */
  function noTextRange(e: MouseEvent) {
    if (e.shiftKey) e.preventDefault()
  }

  function rowMenu(t: Txn): MenuItem[] {
    const ids = picked.has(t.id) && money.selected.length > 1 ? [...money.selected] : [t.id]
    const many = ids.length > 1
    return tidyMenu([
      !many && { label: 'Open', icon: 'eye', run: () => money.open(t.id) },
      {
        label: picked.has(t.id) ? 'Untick' : 'Tick',
        icon: 'tick',
        run: () => money.toggleSelect(t.id),
      },
      SEP,
      {
        label: t.excluded ? 'Count it again' : 'Leave out of reports',
        icon: t.excluded ? 'eye' : 'hidden',
        hint: many ? String(ids.length) : undefined,
        run: () => void money.setExcluded(ids, !t.excluded),
      },
      !!t.category && {
        label: 'Take the category off',
        icon: 'tag',
        hint: many ? String(ids.length) : undefined,
        run: () => void money.categorize(ids, null),
      },
      SEP,
      ids.every((id) => rows.find((r) => r.id === id)?.origin !== 'bank') && {
        label: 'Delete…',
        icon: 'trash',
        danger: true,
        hint: many ? String(ids.length) : undefined,
        run: () => void money.deleteTxns(ids),
      },
    ])
  }

  // ── Many at once ────────────────────────────────────────────────────

  function categorizePicked(id: MoneyCategoryId | null) {
    return money.categorize([...money.selected], id)
  }

  function deletePicked() {
    const ids = pickedRows.filter((t) => t.origin !== 'bank').map((t) => t.id)
    if (ids.length > 0) void money.deleteTxns(ids)
  }

  // ── Keys ────────────────────────────────────────────────────────────

  function onkeydown(e: KeyboardEvent) {
    // Already taken -- by the shortcut table's "close the transaction", or
    // by a picker's own Escape.
    if (e.defaultPrevented || e.ctrlKey || e.metaKey || e.altKey) return
    // The typing-in sheet closes on Escape from anywhere inside it, a field
    // included, as every sheet's does.
    if (money.adding) {
      if (e.key === 'Escape') money.closeAdd()
      return
    }
    if (e.key !== 'Escape' || isTyping(e.target)) return
    if (document.querySelector('[aria-modal="true"]')) return
    if (!money.openId && money.selected.length > 0) {
      e.preventDefault()
      money.clearSelection()
    }
  }

  // ── A transaction typed in ──────────────────────────────────────────

  let newAccount = $state<string>('')
  let newDate = $state('')
  let newPayee = $state('')
  let newAmount = $state('')
  let newIncoming = $state(false)
  let newCategory = $state<MoneyCategoryId | null>(null)
  let newError = $state<string | null>(null)
  let saving = $state(false)

  function today(): string {
    const d = new Date()
    const pad = (n: number) => String(n).padStart(2, '0')
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
  }

  // The sheet is the store's to open -- the Add button, and `c` from the
  // shortcut table, both set `money.adding` -- so the form starts afresh
  // each time it does, wherever the asking came from.
  $effect(() => {
    if (!money.adding) return
    untrack(() => {
      const filtered = manualAccounts.find((a) => a.id === money.accountFilter)
      newAccount = filtered?.id ?? manualAccounts[0]?.id ?? ''
      newDate = today()
      newPayee = ''
      newAmount = ''
      newIncoming = false
      newCategory = null
      newError = null
    })
  })

  async function saveNew() {
    const account = money.accountById(newAccount)
    if (!account) {
      newError = 'Choose an account.'
      return
    }
    const parsed = parseMoney(newAmount, account.currency)
    if (parsed === null || parsed === 0) {
      newError = 'That amount could not be read.'
      return
    }
    if (!newPayee.trim()) {
      newError = 'Say who it was.'
      return
    }
    const amount = newIncoming ? Math.abs(parsed) : -Math.abs(parsed)
    const txn: NewTxn = {
      accountId: account.id,
      date: newDate || today(),
      amount,
      payee: newPayee.trim(),
      category: newCategory,
    }
    saving = true
    const made = await money.addManual(txn)
    saving = false
    if (!made) {
      newError = 'It could not be saved.'
      return
    }
    money.closeAdd()
    money.open(made.id)
  }
</script>

<svelte:window {onkeydown} />

<section class="list" style:--tint={money.accent}>
  {#if money.selected.length > 0}
    <div class="toolbar bulk" role="toolbar" aria-label="Ticked transactions">
      <button
        class="tickbox"
        role="checkbox"
        aria-checked={allPicked ? 'true' : 'mixed'}
        aria-label={allPicked ? 'Untick all' : 'Tick all'}
        onclick={() => (allPicked ? money.clearSelection() : money.selectAll())}
      >
        <Icon name={allPicked ? 'tick' : 'minus'} size={12} weight={2.5} />
      </button>
      <span class="count">{money.selected.length} ticked</span>
      <span class="spacer"></span>
      <MoneyCategoryPicker
        value={null}
        onchange={categorizePicked}
        buttonLabel="Categorise"
        pill
        align="right"
        allowNone
      />
      <button class="btn" onclick={() => void money.setExcluded([...money.selected], !allExcluded)}>
        <Icon name={allExcluded ? 'eye' : 'hidden'} size={14} />
        {allExcluded ? 'Count again' : 'Leave out'}
      </button>
      {#if deletable}
        <button class="btn btn-ghost-danger" onclick={deletePicked}>
          <Icon name="trash" size={14} /> Delete…
        </button>
      {/if}
      <button class="btn" onclick={() => money.clearSelection()} title="Untick all (Escape)">
        Done
      </button>
    </div>
  {:else}
    <div class="toolbar">
      <div class="toolbar-end">
        <select
          class="select"
          aria-label="Account"
          value={money.accountFilter ?? ''}
          onchange={(e) => pickAccount(e.currentTarget.value)}
        >
          <option value="">All accounts</option>
          {#each money.visibleAccounts as a (a.id)}
            <option value={a.id}>{a.name}{a.mask ? ` ··${a.mask}` : ''}</option>
          {/each}
        </select>
        <select
          class="select"
          aria-label="Category"
          value={money.categoryFilter ?? ''}
          onchange={(e) => pickCategory(e.currentTarget.value)}
        >
          <option value="">Any category</option>
          <option value="none">No category</option>
          {#each categoryGroups as g (g.name)}
            <optgroup label={g.name}>
              {#each g.categories as c (c.id)}<option value={c.id}>{c.name}</option>{/each}
            </optgroup>
          {/each}
        </select>
      </div>
      <div class="toolbar-end right">
        {#if money.query.trim()}
          <span class="query" title="Searching payee, description and note">
            <Icon name="search" size={12} />
            <span class="q">{money.query.trim()}</span>
          </span>
        {/if}
        {#if narrowed}
          <button class="clear" onclick={clearFilters}>Clear</button>
        {/if}
        {#if manualAccounts.length > 0}
          <button class="btn add" onclick={() => money.openAdd()} title="Type in a transaction (C)">
            <Icon name="plus" size={14} /> Add
          </button>
        {/if}
      </div>
    </div>
  {/if}

  {#if money.loading && rows.length === 0}
    <p class="hint">Loading…</p>
  {:else if rows.length === 0}
    {#if narrowed}
      <EmptyState lead="Nothing matches.">
        {#snippet note()}Nothing this month fits these filters.{/snippet}
        {#snippet action()}
          <button class="btn" onclick={clearFilters}>Clear the filters</button>
        {/snippet}
      </EmptyState>
    {:else}
      <EmptyState lead="Nothing this month.">
        {#snippet note()}
          No money moved in or out of your accounts this month, or it has not synced yet.
        {/snippet}
        {#snippet action()}
          <button class="btn" onclick={() => void money.shiftMonth(-1)}>The month before</button>
        {/snippet}
      </EmptyState>
    {/if}
  {:else}
    <!-- The scrolling parent `VirtualList` measures; see `StoryList`. -->
    <div class="scroll txn-rows">
      <VirtualList
        items={rows}
        getKey={rowKey}
        selectedId={money.openId}
        onEndReached={() => void money.loadMore()}
      >
        {#snippet children(t: Txn)}
          {@render txnRow(t)}
        {/snippet}
      </VirtualList>
      {#if money.page?.nextOffset != null}
        <button class="load-more" disabled={money.loading} onclick={() => void money.loadMore()}>
          {money.loading ? 'Loading…' : 'Load more'}
        </button>
      {/if}
    </div>
  {/if}
</section>

{#snippet txnRow(t: Txn)}
  {@const isPicked = picked.has(t.id)}
  {@const category = money.categoryById(t.category)}
  {@const account = money.accountFilter ? null : money.accountById(t.accountId)}
  {@const name = shownPayee(t) || 'Unnamed'}
  <div
    class="row list-row"
    class:sel={money.openId === t.id}
    class:picked={isPicked}
    class:excluded={t.excluded}
  >
    <button
      class="tickbox"
      role="checkbox"
      aria-checked={isPicked}
      aria-label="Tick {name}"
      onmousedown={noTextRange}
      onclick={(e) => money.toggleSelect(t.id, e.shiftKey)}
    >
      {#if isPicked}<Icon name="tick" size={12} weight={2.5} />{/if}
    </button>
    <button
      class="main"
      aria-current={money.openId === t.id ? 'true' : undefined}
      onmousedown={noTextRange}
      onclick={(e) => clickRow(e, t)}
      oncontextmenu={(e) => menu.show(e, rowMenu(t))}
    >
      <span class="date">{rowDate(t)}</span>
      <span class="who">
        <span class="payee">{name}</span>
        {#if t.pending}<span class="tag pending">Pending</span>{/if}
        {#if t.note}<span class="has-note" title={t.note}><Icon name="pencil" size={11} /></span
          >{/if}
      </span>
      {#if category}
        <span class="chip" title={category.name}>
          <span class="dot" style:background={category.color || 'var(--fg-faint)'}></span>
          <span class="chip-name">{category.name}</span>
        </span>
      {:else}
        <span class="chip none">No category</span>
      {/if}
      {#if account}<span class="acct" title={account.name}>{account.name}</span>{/if}
      <span class="amt">
        <MoneyAmount amount={t.amount} currency={t.currency} struck={t.excluded} />
      </span>
    </button>
  </div>
{/snippet}

{#if money.adding}
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div class="scrim" onclick={() => money.closeAdd()}></div>
  <div
    class="sheet"
    role="dialog"
    aria-modal="true"
    aria-label="Type in a transaction"
    use:trapFocus
  >
    <h2>Type in a transaction</h2>
    <form
      class="new"
      onsubmit={(e) => {
        e.preventDefault()
        void saveNew()
      }}
    >
      <label class="field-label" for="new-account">Account</label>
      <select id="new-account" class="field" bind:value={newAccount} use:focusOnMount>
        {#each manualAccounts as a (a.id)}<option value={a.id}>{a.name}</option>{/each}
      </select>

      <div class="pair">
        <div>
          <label class="field-label" for="new-date">Date</label>
          <input id="new-date" class="field" type="date" bind:value={newDate} />
        </div>
        <div>
          <label class="field-label" for="new-amount">Amount</label>
          <input
            id="new-amount"
            class="field"
            inputmode="decimal"
            autocomplete="off"
            placeholder={formatMoney(
              0,
              money.accountById(newAccount)?.currency ?? money.mainCurrency,
            )}
            bind:value={newAmount}
          />
        </div>
      </div>

      <div class="direction" role="radiogroup" aria-label="Which way">
        <label>
          <input
            type="radio"
            name="new-dir"
            checked={!newIncoming}
            onchange={() => (newIncoming = false)}
          />
          Money out
        </label>
        <label>
          <input
            type="radio"
            name="new-dir"
            checked={newIncoming}
            onchange={() => (newIncoming = true)}
          />
          Money in
        </label>
      </div>

      <label class="field-label" for="new-payee">Payee</label>
      <input id="new-payee" class="field" autocomplete="off" bind:value={newPayee} />

      <span class="field-label">Category</span>
      <MoneyCategoryPicker
        value={newCategory}
        onchange={(id: MoneyCategoryId | null) => (newCategory = id)}
        label="Category"
      />

      {#if newError}<p class="error">{newError}</p>{/if}

      <div class="sheet-row">
        <span class="spacer"></span>
        <button type="button" class="btn" onclick={() => money.closeAdd()}>Cancel</button>
        <button type="submit" class="btn btn-primary" disabled={saving}>Add</button>
      </div>
    </form>
  </div>
{/if}

<style>
  .list {
    flex: 1;
    min-height: 0;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }

  /* The shared filter bar, with its ends allowed a real width so the two
     selects wrap rather than paint over each other in a narrow pane (see
     `TodoView`'s note on the same row). */
  .toolbar {
    flex-wrap: wrap;
  }
  .toolbar-end {
    min-width: max-content;
  }
  .select {
    height: 28px;
    max-width: 200px;
    padding: 0 var(--sp-2);
    border: none;
    border-radius: 999px;
    background: none;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    cursor: pointer;
  }
  .select:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .select:focus {
    outline: none;
  }
  .clear {
    height: 26px;
    padding: 0 var(--sp-2);
    border-radius: 999px;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .clear:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .add {
    height: 28px;
    padding: 0 var(--sp-3);
    font-size: var(--text-sm);
  }
  .query {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-1);
    max-width: 180px;
    height: 24px;
    padding: 0 var(--sp-2);
    border-radius: 999px;
    background: var(--bg-sunken);
    font-size: var(--text-xs);
    color: var(--fg-muted);
  }
  .q {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* What can be done to every ticked row. */
  .bulk {
    gap: var(--sp-2);
    padding-left: var(--sp-5);
  }
  .bulk .count {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg);
  }
  .bulk .btn {
    height: 30px;
    padding: 0 var(--sp-3);
    font-size: var(--text-sm);
  }
  .spacer {
    flex: 1;
  }

  .hint {
    padding: var(--sp-4) var(--sp-5);
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }

  .txn-rows {
    flex: 1;
    min-height: 0;
    padding-bottom: var(--sp-6);
  }
  .load-more {
    width: 100%;
    padding: var(--sp-3);
    text-align: center;
    color: var(--accent);
    font-size: var(--text-sm);
  }
  .load-more:hover:not(:disabled) {
    background: var(--bg-hover);
  }

  /* ── Rows ───────────────────────────────────────────────────────── */

  /* The shared `.list-row` card; inside it, the tick box in the gutter and
     one button for the rest of the row. Two buttons side by side rather than
     a box inside a button, which is not allowed to hold one. */
  .row {
    display: flex;
    align-items: center;
    min-height: 40px;
  }
  .row.picked {
    background: color-mix(in oklab, var(--accent) 7%, transparent);
  }
  .row.excluded .main {
    color: var(--fg-subtle);
  }

  .tickbox {
    flex: none;
    display: grid;
    place-items: center;
    width: 16px;
    height: 16px;
    margin-left: var(--sp-3);
    border: 1px solid var(--border-strong);
    border-radius: 4px;
    color: var(--fg-on-accent);
    opacity: 0;
    transition:
      opacity var(--fast) var(--ease),
      background var(--fast) var(--ease);
  }
  /* Out of the way until wanted: on the row under the pointer, on a ticked
     row, on every row once anything is ticked, and on keyboard focus. */
  .row:hover .tickbox,
  .row.picked .tickbox,
  .tickbox:focus-visible,
  .list:has(.row.picked) .tickbox,
  .bulk .tickbox {
    opacity: 1;
  }
  .row.picked .tickbox,
  .bulk .tickbox {
    background: var(--accent);
    border-color: var(--accent);
  }

  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-2) var(--sp-4) var(--sp-2) var(--sp-3);
    text-align: left;
    color: var(--fg);
    font-size: var(--text-base);
  }
  .date {
    flex: none;
    width: 3.6em;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .who {
    flex: 1 1 0;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .payee {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .pending {
    flex: none;
    height: 18px;
    font-size: 11px;
  }
  .has-note {
    flex: none;
    display: grid;
    place-items: center;
    color: var(--fg-faint);
  }

  .chip {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    max-width: 150px;
    height: 22px;
    padding: 0 var(--sp-2);
    border-radius: 999px;
    background: var(--bg-sunken);
    font-size: var(--text-xs);
    color: var(--fg-muted);
  }
  .chip.none {
    background: none;
    border: 1px dashed var(--border-strong);
    color: var(--fg-faint);
  }
  .chip-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
  }
  .acct {
    flex: none;
    max-width: 120px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .amt {
    flex: none;
    min-width: 6.5em;
    text-align: right;
  }

  /* A narrow pane keeps the date, the payee and the amount; the chip and the
     account are what can be read in the rail. */
  @container (max-width: 520px) {
    .acct {
      display: none;
    }
  }
  .list {
    container-type: inline-size;
  }

  /* ── Typing one in ─────────────────────────────────────────────── */

  .sheet h2 {
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
    margin-bottom: var(--sp-3);
  }
  .new {
    display: flex;
    flex-direction: column;
  }
  .new .field-label {
    margin-top: var(--sp-3);
  }
  .pair {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--sp-2);
  }
  .direction {
    display: flex;
    gap: var(--sp-4);
    margin-top: var(--sp-3);
  }
  .direction label {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-sm);
    color: var(--fg-muted);
    cursor: pointer;
  }
  .direction input {
    accent-color: var(--accent);
  }
  .error {
    margin-top: var(--sp-3);
  }
</style>
