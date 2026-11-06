<script lang="ts">
  // Read a bank's export into a manual account: an OFX/QFX statement or a
  // CSV, previewed before anything is written.
  //
  // Three steps in one sheet. Pick a file; see what the service made of it
  // -- for a CSV, which column it took for the date, the amount and the
  // payee, with every choice changeable, because a bank's CSV is whatever
  // its developers felt like that year; then choose the account it goes
  // into and import. Lines already there (the same FITID, or the same date,
  // amount and payee) are skipped by the service, so importing the same
  // statement twice is harmless.
  //
  // The file is read here as text and handed to the service whole; nothing
  // in it is ever drawn as anything but plain text. Only manual accounts
  // are offered: a linked one is filled by its bank, and mixing the two
  // would leave the same purchase in it twice.
  //
  // Whoever opens it draws it and takes it down: the Accounts pane, a manual
  // account's menu in the sidebar, the empty screen.

  import { untrack } from 'svelte'
  import { money } from '../lib/money.svelte'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import { plural } from '../lib/format'
  import type {
    AccountKind,
    BankAccountId,
    CsvMapping,
    DateOrder,
    ImportPreview,
    MoneyImportReport,
  } from '../lib/types'
  import MoneyAmount from './MoneyAmount.svelte'

  interface Props {
    /** Chosen to import into, if it is a manual account. */
    accountId?: BankAccountId | null
    onclose: () => void
  }

  const { accountId = null, onclose }: Props = $props()

  /** The most the service reads (the store says the same); past this it is the wrong file. */
  const MAX_BYTES = 5 * 1024 * 1024
  /** Rows of the raw file shown under the column choices. */
  const SHOWN_ROWS = 6

  type Step = 'pick' | 'preview' | 'done'

  let step = $state<Step>('pick')
  let fileName = $state('')
  /** The file's text. Held only while the sheet is open. */
  let content = $state('')
  let preview = $state<ImportPreview | null>(null)
  let mapping = $state<CsvMapping | null>(null)
  /** The column choice the service made, to tell whether its sample still applies. */
  let detected = $state<string>('')
  let error = $state<string | null>(null)
  let busy = $state(false)
  let report = $state<MoneyImportReport | null>(null)

  /** `''` for "a new account". */
  let target = $state<string>('')
  let newName = $state('')
  let newKind = $state<AccountKind>('cash')
  let newCurrency = $state('')
  let importedInto = $state<BankAccountId | null>(null)

  const manual = $derived(money.accounts.filter((a) => !a.linkId))
  const isCsv = $derived(preview?.format === 'csv')
  const mappingChanged = $derived(!!mapping && JSON.stringify(mapping) !== detected)
  const currency = $derived(
    (target ? money.accountById(target)?.currency : null) ??
      preview?.currency ??
      money.mainCurrency,
  )

  const KINDS: { value: AccountKind; label: string }[] = [
    { value: 'cash', label: 'Checking or savings' },
    { value: 'credit', label: 'Credit card' },
    { value: 'loan', label: 'Loan' },
    { value: 'investment', label: 'Investment' },
    { value: 'other', label: 'Other' },
  ]

  const DATE_ORDERS: { value: DateOrder; label: string }[] = [
    { value: 'auto', label: 'Work it out' },
    { value: 'monthFirst', label: 'Month first (11/05/2026)' },
    { value: 'dayFirst', label: 'Day first (05/11/2026)' },
  ]

  // The account it opened with, chosen when it is a manual one; read once,
  // because the account list changes under an open sheet when "A new
  // account…" is made, and the choice must not jump back.
  untrack(() => {
    const wanted = accountId
    target = wanted && manual.some((a) => a.id === wanted) ? wanted : (manual[0]?.id ?? '')
  })

  function close() {
    content = ''
    preview = null
    onclose()
  }

  /** A first guess at a CSV's columns when the service offered none. */
  function guess(columns: string[]): CsvMapping {
    const find = (re: RegExp) => {
      const i = columns.findIndex((c) => re.test(c))
      return i >= 0 ? i : null
    }
    return {
      date: find(/date/i) ?? 0,
      amount: find(/amount/i) ?? (columns.length > 2 ? 2 : null),
      debit: null,
      credit: null,
      payee: find(/payee|description|name|merchant/i) ?? 1,
      description: find(/memo|detail|reference/i),
      negate: false,
      hasHeader: true,
      dateOrder: 'auto',
    }
  }

  async function pickFile(e: Event & { currentTarget: HTMLInputElement }) {
    const file = e.currentTarget.files?.[0]
    // Cleared, so choosing the same file again after a fix still fires.
    e.currentTarget.value = ''
    if (!file) return
    error = null
    if (file.size > MAX_BYTES) {
      error = `${file.name} is larger than a statement can be here (over 5 MB).`
      return
    }
    busy = true
    try {
      const text = await file.text()
      const read = await money.previewImport(text)
      if (!read) {
        error = `${file.name} could not be read as a bank statement.`
        return
      }
      fileName = file.name
      content = text
      preview = read
      mapping = read.format === 'csv' ? (read.mapping ?? guess(read.columns)) : null
      detected = JSON.stringify(mapping)
      if (!newCurrency) newCurrency = read.currency ?? money.mainCurrency
      if (!newName) newName = file.name.replace(/\.[^.]+$/, '')
      step = 'preview'
    } catch {
      error = `${file.name} could not be opened.`
    } finally {
      busy = false
    }
  }

  /** Which role a column has been given, for the header over it. */
  function roleOf(i: number): string | null {
    const m = mapping
    if (!m) return null
    if (m.date === i) return 'Date'
    if (m.payee === i) return 'Payee'
    if (m.amount === i) return 'Amount'
    if (m.debit === i) return 'Money out'
    if (m.credit === i) return 'Money in'
    if (m.description === i) return 'Description'
    return null
  }

  /** The name of column `i`, or its number when the file has no header. */
  function columnName(i: number): string {
    const name = preview?.columns[i]?.trim()
    return name || `Column ${i + 1}`
  }

  // At most the service's own column cap (64), whatever an answer says: each
  // column is an option in six pickers and a cell in every row shown, and a
  // file of nothing but commas must not become a million of each.
  const MAX_COLUMNS = 64
  const columnIndexes = $derived(
    preview ? preview.columns.slice(0, MAX_COLUMNS).map((_, i) => i) : [],
  )
  const amountMode = $derived(mapping && mapping.amount == null ? 'split' : 'one')

  function setColumn(key: 'date' | 'payee', raw: string) {
    if (!mapping) return
    mapping[key] = Number(raw)
  }

  function setOptionalColumn(key: 'amount' | 'debit' | 'credit' | 'description', raw: string) {
    if (!mapping) return
    mapping[key] = raw === '' ? null : Number(raw)
  }

  function setDateOrder(raw: string) {
    if (mapping) mapping.dateOrder = raw as DateOrder
  }

  function setFlag(key: 'hasHeader' | 'negate', on: boolean) {
    if (mapping) mapping[key] = on
  }

  function setAmountMode(mode: 'one' | 'split') {
    if (!mapping) return
    if (mode === 'one') {
      mapping.amount = mapping.amount ?? mapping.debit ?? 0
      mapping.debit = null
      mapping.credit = null
    } else {
      mapping.debit = mapping.debit ?? mapping.amount ?? 0
      mapping.credit = mapping.credit ?? null
      mapping.amount = null
    }
  }

  const ready = $derived.by(() => {
    if (!preview || busy) return false
    if (target === '' && (!newName.trim() || !/^[A-Za-z]{3}$/.test(newCurrency.trim())))
      return false
    if (isCsv) {
      const m = mapping
      if (!m) return false
      if (m.amount == null && m.debit == null && m.credit == null) return false
    }
    return true
  })

  async function run() {
    if (!ready || !preview) return
    busy = true
    error = null
    try {
      let into: BankAccountId | null = target || null
      if (!into) {
        const made = await money.createManualAccount(
          newName.trim(),
          newKind,
          newCurrency.trim().toUpperCase(),
        )
        if (!made) {
          error = 'The new account could not be made.'
          return
        }
        into = made.id
        target = made.id
      }
      const done = await money.importFile(
        into,
        content,
        preview.format,
        isCsv && mapping ? $state.snapshot(mapping) : null,
      )
      if (!done) {
        error = 'Nothing was imported: the file was refused.'
        return
      }
      report = done
      importedInto = into
      content = ''
      step = 'done'
    } finally {
      busy = false
    }
  }

  async function seeThem() {
    const id = importedInto
    close()
    if (!id) return
    money.setView('transactions')
    await money.setAccount(id)
  }
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape' && !busy && !e.defaultPrevented) close()
  }}
/>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="scrim" onclick={() => !busy && close()}></div>
<div class="sheet wide" role="dialog" aria-modal="true" aria-label="Import a file" use:trapFocus>
  <h2>Import a file</h2>

  {#if step === 'pick'}
    <p class="hint">
      A statement downloaded from your bank's website: OFX or QFX (often called “Quicken” or “Money”
      format), or a CSV. It is read on this device and goes into an account you keep by hand.
    </p>
    <label class="file btn btn-outline" class:busy>
      <input
        type="file"
        accept=".ofx,.qfx,.csv,text/csv"
        disabled={busy}
        onchange={(e) => void pickFile(e)}
        use:focusOnMount
      />
      {busy ? 'Reading…' : 'Choose a file…'}
    </label>
    {#if error}<p class="error">{error}</p>{/if}
    <div class="sheet-row">
      <span class="spacer"></span>
      <button class="btn" onclick={close}>Cancel</button>
    </div>
  {:else if step === 'preview' && preview}
    <p class="hint file-line">
      <b>{fileName}</b> · {preview.format === 'csv' ? 'CSV' : 'OFX'} · {plural(
        preview.lines,
        'line',
      )}{#if preview.currency}
        · {preview.currency}{/if}
    </p>

    {#if preview.problems.length > 0}
      <ul class="problems">
        {#each preview.problems.slice(0, 5) as problem, i (i)}
          <li>{problem}</li>
        {/each}
        {#if preview.problems.length > 5}
          <li>and {preview.problems.length - 5} more</li>
        {/if}
      </ul>
    {/if}

    {#if isCsv && mapping}
      <fieldset class="mapping">
        <legend class="field-label">Which column is which</legend>
        <div class="grid">
          <label for="imp-date">Date</label>
          <select
            id="imp-date"
            value={String(mapping.date)}
            onchange={(e) => setColumn('date', e.currentTarget.value)}
          >
            {#each columnIndexes as i (i)}<option value={String(i)}>{columnName(i)}</option>{/each}
          </select>

          <label for="imp-payee">Payee</label>
          <select
            id="imp-payee"
            value={String(mapping.payee)}
            onchange={(e) => setColumn('payee', e.currentTarget.value)}
          >
            {#each columnIndexes as i (i)}<option value={String(i)}>{columnName(i)}</option>{/each}
          </select>

          <span class="lab">Amount</span>
          <div class="modes" role="radiogroup" aria-label="How amounts are written">
            <label>
              <input
                type="radio"
                name="imp-mode"
                checked={amountMode === 'one'}
                onchange={() => setAmountMode('one')}
              />
              One column
            </label>
            <label>
              <input
                type="radio"
                name="imp-mode"
                checked={amountMode === 'split'}
                onchange={() => setAmountMode('split')}
              />
              Money out and in apart
            </label>
          </div>

          {#if amountMode === 'one'}
            <label for="imp-amount">Amount column</label>
            <select
              id="imp-amount"
              value={mapping.amount == null ? '' : String(mapping.amount)}
              onchange={(e) => setOptionalColumn('amount', e.currentTarget.value)}
            >
              {#each columnIndexes as i (i)}<option value={String(i)}>{columnName(i)}</option
                >{/each}
            </select>
          {:else}
            <label for="imp-debit">Money out</label>
            <select
              id="imp-debit"
              value={mapping.debit == null ? '' : String(mapping.debit)}
              onchange={(e) => setOptionalColumn('debit', e.currentTarget.value)}
            >
              <option value="">None</option>
              {#each columnIndexes as i (i)}<option value={String(i)}>{columnName(i)}</option
                >{/each}
            </select>
            <label for="imp-credit">Money in</label>
            <select
              id="imp-credit"
              value={mapping.credit == null ? '' : String(mapping.credit)}
              onchange={(e) => setOptionalColumn('credit', e.currentTarget.value)}
            >
              <option value="">None</option>
              {#each columnIndexes as i (i)}<option value={String(i)}>{columnName(i)}</option
                >{/each}
            </select>
          {/if}

          <label for="imp-desc">Description</label>
          <select
            id="imp-desc"
            value={mapping.description == null ? '' : String(mapping.description)}
            onchange={(e) => setOptionalColumn('description', e.currentTarget.value)}
          >
            <option value="">None</option>
            {#each columnIndexes as i (i)}<option value={String(i)}>{columnName(i)}</option>{/each}
          </select>

          <label for="imp-order">Dates</label>
          <select
            id="imp-order"
            value={mapping.dateOrder}
            onchange={(e) => setDateOrder(e.currentTarget.value)}
          >
            {#each DATE_ORDERS as o (o.value)}<option value={o.value}>{o.label}</option>{/each}
          </select>
        </div>

        <div class="checks">
          <label>
            <input
              type="checkbox"
              checked={mapping.hasHeader}
              onchange={(e) => setFlag('hasHeader', e.currentTarget.checked)}
            />
            The first line is column names
          </label>
          {#if amountMode === 'one'}
            <label>
              <input
                type="checkbox"
                checked={mapping.negate}
                onchange={(e) => setFlag('negate', e.currentTarget.checked)}
              />
              This bank writes money out as a positive number
            </label>
          {/if}
        </div>
      </fieldset>

      <div class="table-wrap scroll">
        <table class="raw">
          <thead>
            <tr>
              {#each columnIndexes as i (i)}
                {@const role = roleOf(i)}
                <th scope="col" class:mapped={!!role}>
                  <span class="col-name">{columnName(i)}</span>
                  {#if role}<span class="role">{role}</span>{/if}
                </th>
              {/each}
            </tr>
          </thead>
          <tbody>
            {#each preview.rows.slice(0, SHOWN_ROWS) as row, r (r)}
              <tr>
                {#each columnIndexes as i (i)}
                  <td class:mapped={!!roleOf(i)}>{row[i] ?? ''}</td>
                {/each}
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}

    {#if preview.sample.length > 0 && !mappingChanged}
      <div class="sample">
        <span class="field-label">How it reads</span>
        {#each preview.sample.slice(0, SHOWN_ROWS) as t, i (i)}
          <div class="sample-row">
            <span class="d">{t.date}</span>
            <span class="p">{t.payee || t.description}</span>
            <MoneyAmount amount={t.amount} {currency} size="sm" />
          </div>
        {/each}
      </div>
    {:else if mappingChanged}
      <p class="hint">The columns have changed; the import reads them as chosen above.</p>
    {/if}

    <label class="field-label spaced" for="imp-account">Into</label>
    <select id="imp-account" class="field" bind:value={target}>
      {#each manual as a (a.id)}
        <option value={a.id}>{a.name}{a.mask ? ` ··${a.mask}` : ''} ({a.currency})</option>
      {/each}
      <option value="">A new account…</option>
    </select>

    {#if target === ''}
      <div class="new-account">
        <input
          class="field"
          bind:value={newName}
          placeholder="Account name"
          aria-label="New account name"
          autocomplete="off"
        />
        <select class="field" bind:value={newKind} aria-label="Kind of account">
          {#each KINDS as k (k.value)}<option value={k.value}>{k.label}</option>{/each}
        </select>
        <input
          class="field currency"
          bind:value={newCurrency}
          maxlength="3"
          placeholder="USD"
          aria-label="Currency"
          autocomplete="off"
          spellcheck="false"
        />
      </div>
    {/if}

    {#if error}<p class="error">{error}</p>{/if}

    <div class="sheet-row">
      <button class="btn" disabled={busy} onclick={() => (step = 'pick')}>Another file</button>
      <span class="spacer"></span>
      <button class="btn" disabled={busy} onclick={close}>Cancel</button>
      <button class="btn btn-primary" disabled={!ready} onclick={() => void run()}>
        {busy ? 'Importing…' : 'Import'}
      </button>
    </div>
  {:else if step === 'done' && report}
    <p class="lead">
      {plural(report.added, 'transaction')} added{#if report.skipped > 0}, {report.skipped} already there{/if}.
    </p>
    {#if report.problems.length > 0}
      <ul class="problems">
        {#each report.problems.slice(0, 8) as problem, i (i)}
          <li>{problem}</li>
        {/each}
        {#if report.problems.length > 8}
          <li>and {report.problems.length - 8} more</li>
        {/if}
      </ul>
    {/if}
    <div class="sheet-row">
      <span class="spacer"></span>
      <button class="btn" onclick={close}>Done</button>
      <button class="btn btn-primary" use:focusOnMount onclick={() => void seeThem()}>
        See them
      </button>
    </div>
  {/if}
</div>

<style>
  .wide {
    top: 50%;
    translate: -50% -50%;
    width: min(640px, calc(100vw - var(--sp-8)));
    max-height: calc(100vh - var(--sp-8));
    overflow-y: auto;
  }
  h2 {
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
    margin-bottom: var(--sp-3);
  }
  .hint {
    margin: 0 0 var(--sp-3);
  }
  .file-line b {
    color: var(--fg);
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .lead {
    margin: 0 0 var(--sp-2);
    font-size: var(--text-base);
    color: var(--fg);
  }
  .error {
    margin-top: var(--sp-3);
    overflow-wrap: anywhere;
  }
  .spaced {
    margin-top: var(--sp-4);
  }

  /* The file input itself is hidden inside the pill that labels it: a bare
     `<input type=file>` draws the platform's own button, which looks like
     nothing else in this window. */
  .file {
    position: relative;
    cursor: pointer;
  }
  .file input {
    position: absolute;
    inset: 0;
    opacity: 0;
    cursor: pointer;
  }
  .file.busy {
    opacity: 0.6;
    pointer-events: none;
  }
  .file:focus-within {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .problems {
    margin: 0 0 var(--sp-3);
    padding-left: var(--sp-5);
    font-size: var(--text-sm);
    color: var(--danger);
    overflow-wrap: anywhere;
  }

  .mapping {
    margin: 0;
    padding: 0;
    border: none;
    min-width: 0;
  }
  .grid {
    display: grid;
    grid-template-columns: 110px 1fr;
    align-items: center;
    gap: var(--sp-2);
  }
  .grid label,
  .lab {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .grid select {
    height: 30px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    color: var(--fg);
    font-size: var(--text-sm);
    min-width: 0;
  }
  .modes {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-3);
  }
  .modes label,
  .checks label {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-sm);
    color: var(--fg-muted);
    cursor: pointer;
  }
  .checks {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    margin-top: var(--sp-3);
  }
  input[type='checkbox'],
  input[type='radio'] {
    accent-color: var(--accent);
  }

  .table-wrap {
    margin-top: var(--sp-3);
    max-width: 100%;
    overflow-x: auto;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
  }
  .raw {
    border-collapse: collapse;
    font-size: var(--text-xs);
    min-width: 100%;
  }
  .raw th,
  .raw td {
    padding: 4px var(--sp-2);
    text-align: left;
    white-space: nowrap;
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
    border-bottom: 1px solid var(--border);
    color: var(--fg-subtle);
    user-select: text;
  }
  .raw th {
    font-weight: 550;
    vertical-align: bottom;
  }
  .raw .mapped {
    color: var(--fg);
    background: color-mix(in oklab, var(--accent) 6%, transparent);
  }
  .col-name {
    display: block;
  }
  .role {
    display: block;
    color: var(--accent);
    font-size: 11px;
    font-weight: 650;
  }

  .sample {
    margin-top: var(--sp-4);
  }
  .sample-row {
    display: flex;
    align-items: baseline;
    gap: var(--sp-3);
    padding: 3px 0;
    font-size: var(--text-sm);
    border-bottom: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }
  .sample-row .d {
    flex: none;
    width: 6.5em;
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .sample-row .p {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .new-account {
    display: grid;
    grid-template-columns: 1fr 170px 70px;
    gap: var(--sp-2);
    margin-top: var(--sp-2);
  }
  .currency {
    text-transform: uppercase;
  }
</style>
