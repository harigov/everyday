<script lang="ts">
  // One transaction, in the rail beside the list.
  //
  // Two halves, drawn as two: what the bank says (the date, the amount, its
  // name for the payee, its raw description line), which every sync can
  // rewrite and nothing here edits; and what the person says (the category,
  // their own name for the payee, a note, whether it counts), which no sync
  // ever touches. The bank's description is shown as it came, as plain text
  // -- a transfer memo is the sender's own words, and is treated as such.
  //
  // Text fields save when they lose focus or Enter is pressed, rather than
  // on every key: each save is a write to the vault, and half a payee name
  // is not a name anybody meant.
  //
  // Delete is offered only for what was imported or typed in -- a bank's
  // line would come straight back with the next sync -- and the store asks
  // before it deletes.

  import { money } from '../lib/money.svelte'
  import { shownPayee } from '../lib/money'
  import { longDate } from '../lib/format'
  import type { MoneyCategoryId, Txn } from '../lib/types'
  import Icon from './Icon.svelte'
  import MoneyAmount from './MoneyAmount.svelte'
  import MoneyCategoryPicker from './MoneyCategoryPicker.svelte'

  const { txn }: { txn: Txn } = $props()

  const account = $derived(money.accountById(txn.accountId))
  const category = $derived(money.categoryById(txn.category))
  const payee = $derived(shownPayee(txn))

  /** Who chose the category, in words. */
  const decidedBy = $derived.by(() => {
    switch (txn.categorizedBy.by) {
      case 'person':
        return 'Chosen by you'
      case 'rule':
        return 'Chosen by a rule'
      case 'bank':
        return txn.bankCategory ? `From the bank's “${txn.bankCategory}”` : 'From the bank'
      default:
        return null
    }
  })

  const origin = $derived.by(() => {
    if (txn.origin === 'import') return 'Imported from a file'
    if (txn.origin === 'manual') return 'Typed in by you'
    const link = account?.linkId ? money.links.find((s) => s.link.id === account.linkId) : null
    return link ? `From ${link.link.name}` : 'From the bank'
  })

  function savePayee(value: string) {
    const next = value.trim()
    if (next === txn.payeeOverride) return
    void money.setPayee(txn.id, next)
  }

  function saveNote(value: string) {
    if (value === txn.note) return
    void money.setNote(txn.id, value)
  }

  /** "Always put this payee in this category": the store says what it sorted, under the list. */
  function makeRule() {
    const id = txn.category
    if (id) void money.ruleFromTxn(txn, id)
  }

  /** Enter in a one-line field commits it, the way leaving it does. */
  function commitOnEnter(e: KeyboardEvent & { currentTarget: HTMLInputElement }) {
    if (e.key === 'Enter') {
      e.preventDefault()
      e.currentTarget.blur()
    }
  }
</script>

<aside class="detail" aria-label="Transaction">
  <div class="top">
    <span class="when">{longDate(txn.date)}</span>
    <button
      class="close"
      title="Close (Escape)"
      aria-label="Close the transaction"
      onclick={() => money.open(null)}
    >
      <Icon name="close" size={15} />
    </button>
  </div>

  <div class="body scroll">
    <div class="hero" class:excluded={txn.excluded}>
      <MoneyAmount amount={txn.amount} currency={txn.currency} size="xl" struck={txn.excluded} />
      <span class="payee">{payee || 'Unnamed'}</span>
      <span class="meta">
        {#if txn.pending}<span class="tag">Pending</span>{/if}
        {#if account}
          <span class="acct">{account.name}{account.mask ? ` ··${account.mask}` : ''}</span>
        {/if}
      </span>
    </div>

    {#key txn.id}
      <section>
        <span class="field-label">Category</span>
        <MoneyCategoryPicker
          value={txn.category}
          onchange={(id: MoneyCategoryId | null) => money.categorize([txn.id], id)}
          label="Category"
        />
        {#if decidedBy}<p class="hint">{decidedBy}</p>{/if}
        {#if category && payee}
          <button class="link" onclick={makeRule}>Always put {payee} in {category.name}</button>
        {/if}
      </section>

      <section>
        <label class="field-label" for="txn-payee">Payee</label>
        <input
          id="txn-payee"
          class="field small"
          value={txn.payeeOverride}
          placeholder={txn.payee || 'The bank gave no name'}
          autocomplete="off"
          spellcheck="false"
          onchange={(e) => savePayee(e.currentTarget.value)}
          onkeydown={commitOnEnter}
        />
        {#if txn.payeeOverride && txn.payee}
          <p class="hint">
            The bank calls it {txn.payee}.
            <button class="link inline" onclick={() => void money.setPayee(txn.id, '')}>
              Use that
            </button>
          </p>
        {/if}
      </section>

      <section>
        <label class="field-label" for="txn-note">Note</label>
        <textarea
          id="txn-note"
          class="field note"
          rows="3"
          value={txn.note}
          placeholder="Anything worth remembering about it"
          onchange={(e) => saveNote(e.currentTarget.value)}
        ></textarea>
      </section>

      <section>
        <label class="toggle">
          <input
            type="checkbox"
            checked={txn.excluded}
            onchange={(e) => void money.setExcluded([txn.id], e.currentTarget.checked)}
          />
          <span>
            <b>Leave out of reports and budgets</b>
            <small
              >For a transfer between your own accounts, or anything that is not spending.</small
            >
          </span>
        </label>
      </section>
    {/key}

    {#if txn.description && txn.description !== txn.payee}
      <section>
        <span class="field-label">From the bank</span>
        <p class="raw">{txn.description}</p>
      </section>
    {/if}

    <section class="foot">
      <p class="hint">{origin}</p>
      {#if money.canDelete(txn)}
        <button class="btn btn-ghost-danger" onclick={() => void money.deleteTxns([txn.id])}>
          <Icon name="trash" size={14} /> Delete…
        </button>
      {/if}
    </section>
  </div>
</aside>

<style>
  /* The rail every app's detail is: `TaskDetail`'s width and surface. */
  .detail {
    width: 340px;
    flex: none;
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: var(--bg-panel);
    border-left: 1px solid var(--border);
  }
  .top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: var(--header-h);
    padding: 0 var(--sp-2) 0 var(--sp-4);
    flex: none;
  }
  .when {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .close {
    width: 26px;
    height: 26px;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .close:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .body {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 0 var(--sp-4) var(--sp-8);
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
  }

  .hero {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .hero.excluded {
    opacity: 0.7;
  }
  .payee {
    font-size: var(--text-md);
    font-weight: 600;
    color: var(--fg);
    overflow-wrap: anywhere;
    user-select: text;
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    margin-top: 2px;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }

  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    min-width: 0;
  }
  .field-label {
    margin-bottom: 2px;
  }
  .field.small {
    height: 32px;
    font-size: var(--text-sm);
  }
  .note {
    font-size: var(--text-sm);
    resize: vertical;
  }
  .hint {
    margin: 2px 0 0;
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
  }

  .link {
    align-self: flex-start;
    margin-top: var(--sp-1);
    color: var(--accent);
    font-size: var(--text-sm);
    font-weight: 550;
    text-align: left;
    overflow-wrap: anywhere;
  }
  .link:hover {
    text-decoration: underline;
  }
  .link.inline {
    margin: 0 0 0 var(--sp-1);
    font-size: inherit;
  }

  .toggle {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    cursor: pointer;
  }
  .toggle input {
    flex: none;
    margin-top: 3px;
    accent-color: var(--accent);
  }
  .toggle span {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .toggle b {
    font-size: var(--text-sm);
    font-weight: 600;
  }
  .toggle small {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    line-height: var(--leading-snug);
  }

  /* The bank's own line, verbatim: monospaced, because these are often
     fixed-width codes ("POS 1234 SQ *CAFE"), and selectable. */
  .raw {
    margin: 0;
    padding: var(--sp-2);
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    line-height: var(--leading-snug);
    color: var(--fg-muted);
    overflow-wrap: anywhere;
    white-space: pre-wrap;
    user-select: text;
  }

  .foot {
    gap: var(--sp-2);
    align-items: flex-start;
  }
</style>
