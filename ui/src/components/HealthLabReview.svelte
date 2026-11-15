<script lang="ts">
  // Checking what the model read from a document, beside the page.
  //
  // The model proposed these results (`pendingLabs`); a model can misread a
  // digit, a unit or which column a range belongs to, so nothing is kept
  // until the person has looked at each one against the page and pressed
  // Save. Each result is a row that can be corrected or left out. The rows
  // are held here, not in the store: editing them changes nothing anywhere
  // else, and leaving the page loses the edits but not the proposal, which
  // stays on the document until it is saved or discarded.
  //
  // A result outside its own reference range is marked in words as well as
  // colour. That is a note on the page's own numbers -- the range printed
  // beside the result -- not a judgement.

  import { untrack } from 'svelte'
  import { health } from '../lib/health.svelte'
  import { labOutOfRange } from '../lib/health'
  import { plural } from '../lib/format'
  import type { HealthDocument, LabFlag, LabResult } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'

  interface Props {
    doc: HealthDocument
  }
  let { doc }: Props = $props()

  /** A result as it is typed: numbers are text until saved. */
  interface Row {
    id: string
    base: LabResult
    name: string
    code: string
    result: string
    unit: string
    refLow: string
    refHigh: string
    refText: string
    flag: LabFlag
    collected: string
  }

  const FLAGS: { value: LabFlag; label: string }[] = [
    { value: 'none', label: 'No flag' },
    { value: 'normal', label: 'Normal' },
    { value: 'low', label: 'Low' },
    { value: 'high', label: 'High' },
    { value: 'abnormal', label: 'Abnormal' },
    { value: 'critical', label: 'Critical' },
  ]

  const DATE = /^\d{4}-\d{2}-\d{2}$/

  function typed(n: number | null | undefined): string {
    return n === null || n === undefined ? '' : String(n)
  }

  function toRow(lab: LabResult, fallback: string): Row {
    return {
      id: lab.id,
      base: lab,
      name: lab.name,
      code: lab.code,
      result: lab.value !== null && lab.value !== undefined ? String(lab.value) : lab.text,
      unit: lab.unit,
      refLow: typed(lab.refLow),
      refHigh: typed(lab.refHigh),
      refText: lab.refText,
      flag: lab.flag,
      collected: lab.collected || fallback,
    }
  }

  function toRows(d: HealthDocument): Row[] {
    return d.pendingLabs.map((l) => toRow(l, d.date ?? ''))
  }

  /** A number as typed: "5.7", "5,7", "1,500". `null` when it is not one. */
  function parseNumber(raw: string): number | null {
    const s = raw.trim()
    if (s === '') return null
    let plain = s
    if (/^-?\d{1,3}(,\d{3})+(\.\d+)?$/.test(s)) plain = s.replace(/,/g, '')
    else if (/^-?\d+,\d+$/.test(s)) plain = s.replace(',', '.')
    const n = Number(plain)
    return Number.isFinite(n) ? n : null
  }

  function toLab(r: Row): LabResult {
    const result = r.result.trim()
    const n = parseNumber(result)
    return {
      ...r.base,
      name: r.name.trim(),
      code: r.code.trim(),
      value: n,
      // A number typed over a number keeps the text it came with ("> 5.7");
      // a number typed over a word does not.
      text: n === null ? result : r.base.value === n ? r.base.text : '',
      unit: r.unit.trim(),
      refLow: parseNumber(r.refLow),
      refHigh: parseNumber(r.refHigh),
      refText: r.refText.trim(),
      flag: r.flag,
      collected: r.collected,
    }
  }

  /** What is wrong with a row, in a phrase, or `null` when nothing is. */
  function problemWith(r: Row): string | null {
    if (r.name.trim() === '') return 'Needs a name.'
    if (r.result.trim() === '') return 'Needs a result.'
    if (r.refLow.trim() !== '' && parseNumber(r.refLow) === null) {
      return 'The low end is not a number.'
    }
    if (r.refHigh.trim() !== '' && parseNumber(r.refHigh) === null) {
      return 'The high end is not a number.'
    }
    if (!DATE.test(r.collected)) return 'Needs the date it was collected.'
    return null
  }

  function flagWord(lab: LabResult): string {
    switch (lab.flag) {
      case 'high':
        return 'High'
      case 'low':
        return 'Low'
      case 'abnormal':
        return 'Abnormal'
      case 'critical':
        return 'Critical'
      default:
        break
    }
    if (lab.value !== null && lab.value !== undefined) {
      if (lab.refLow !== null && lab.refLow !== undefined && lab.value < lab.refLow) {
        return 'Below the range'
      }
      if (lab.refHigh !== null && lab.refHigh !== undefined && lab.value > lab.refHigh) {
        return 'Above the range'
      }
    }
    return 'Outside the range'
  }

  // The rows are seeded when the document or the set of proposed results
  // changes -- a re-read, a save -- and not when the store merely hands
  // back the same results again, which would throw away what was typed.
  const seedKey = $derived(`${doc.id}|${doc.pendingLabs.map((l) => l.id).join(',')}`)
  let seeded = untrack(() => seedKey)
  let rows = $state<Row[]>(untrack(() => toRows(doc)))

  $effect(() => {
    const key = seedKey
    if (key === seeded) return
    untrack(() => {
      seeded = key
      rows = toRows(doc)
    })
  })

  const invalid = $derived(rows.filter((r) => problemWith(r) !== null).length)

  let saving = $state(false)
  let discarding = $state(false)

  function remove(id: string) {
    rows = rows.filter((r) => r.id !== id)
  }

  async function save() {
    if (saving || rows.length === 0 || invalid > 0) return
    saving = true
    try {
      await health.acceptLabs(
        doc.id,
        rows.map((r) => toLab(r)),
      )
    } finally {
      saving = false
    }
  }

  async function discard() {
    discarding = false
    if (saving) return
    saving = true
    try {
      await health.acceptLabs(doc.id, [])
    } finally {
      saving = false
    }
  }
</script>

<section class="review" aria-labelledby="review-head">
  <div class="head">
    <span class="eyebrow" id="review-head">Results to check</span>
    <span class="count">{plural(rows.length, 'result')}</span>
  </div>

  <p class="lead">
    These were read from the page by the model, which can misread a number, a unit or a range. Check
    each one against the page. Nothing is saved until you press Save results.
  </p>

  {#if rows.length === 0}
    <p class="hint">
      Every result has been left out. Save nothing by discarding them, or read the document again.
    </p>
  {/if}

  <ul class="rows">
    {#each rows as row (row.id)}
      {@const lab = toLab(row)}
      {@const out = labOutOfRange(lab)}
      {@const why = problemWith(row)}
      <li class="row" class:out>
        <div class="row-head">
          <input
            class="name"
            bind:value={row.name}
            aria-label="Test name"
            aria-invalid={row.name.trim() === ''}
            placeholder="Test name"
            autocomplete="off"
          />
          {#if out}
            <span class="mark">
              <Icon name="alert" size={13} />
              {flagWord(lab)}
            </span>
          {/if}
          <button
            class="remove"
            aria-label="Leave out {row.name || 'this result'}"
            title="Leave out"
            onclick={() => remove(row.id)}
          >
            <Icon name="close" size={14} />
          </button>
        </div>

        <div class="grid">
          <label class="f wide">
            <span>Result</span>
            <input
              bind:value={row.result}
              aria-invalid={row.result.trim() === ''}
              autocomplete="off"
              inputmode="decimal"
            />
          </label>
          <label class="f">
            <span>Unit</span>
            <input bind:value={row.unit} autocomplete="off" />
          </label>
          <label class="f">
            <span>Flag</span>
            <select bind:value={row.flag}>
              {#each FLAGS as f (f.value)}
                <option value={f.value}>{f.label}</option>
              {/each}
            </select>
          </label>
          <label class="f">
            <span>Range low</span>
            <input bind:value={row.refLow} autocomplete="off" inputmode="decimal" />
          </label>
          <label class="f">
            <span>Range high</span>
            <input bind:value={row.refHigh} autocomplete="off" inputmode="decimal" />
          </label>
          <label class="f wide">
            <span>Range as printed</span>
            <input bind:value={row.refText} autocomplete="off" placeholder="e.g. 3.5 – 5.0" />
          </label>
          <label class="f">
            <span>Collected</span>
            <input type="date" bind:value={row.collected} />
          </label>
          <label class="f wider">
            <span>LOINC code</span>
            <input bind:value={row.code} autocomplete="off" placeholder="If the page prints one" />
          </label>
        </div>

        {#if why}<p class="why" role="status">{why}</p>{/if}
      </li>
    {/each}
  </ul>

  {#if invalid > 0}
    <p class="hint">
      {plural(invalid, 'result')}
      {invalid === 1 ? 'needs' : 'need'} a look before these can be saved.
    </p>
  {/if}

  <div class="foot">
    <button class="btn" disabled={saving} onclick={() => (discarding = true)}>Discard</button>
    <button
      class="btn btn-primary"
      disabled={saving || rows.length === 0 || invalid > 0}
      onclick={() => void save()}
    >
      {saving ? 'Saving…' : 'Save results'}
    </button>
  </div>
</section>

{#if discarding}
  <ConfirmDialog
    title="Discard these results?"
    detail="Nothing has been saved from them. You can read the document again at any time."
    confirmLabel="Discard"
    onconfirm={() => void discard()}
    oncancel={() => (discarding = false)}
  />
{/if}

<style>
  .review {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }
  .head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--sp-3);
  }
  .count {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .lead {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-muted);
  }
  .hint {
    margin: 0;
  }

  .rows {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .row {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
  }
  /* Out of range: the card's edge says so, and so do the words on it. */
  .row.out {
    border-color: color-mix(in oklab, var(--warning) 60%, var(--border));
    box-shadow: inset 3px 0 0 var(--warning);
  }
  .row-head {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .mark {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: var(--sp-1);
    height: 22px;
    padding: 0 var(--sp-2);
    border-radius: 99px;
    background: color-mix(in oklab, var(--warning) 16%, transparent);
    color: color-mix(in oklab, var(--warning) 55%, var(--fg));
    font-size: var(--text-xs);
    font-weight: 600;
    white-space: nowrap;
  }
  .remove {
    flex: none;
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .remove:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: var(--sp-2);
  }
  .f {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  .f.wide {
    grid-column: span 2;
  }
  .f.wider {
    grid-column: span 3;
  }
  .f > span {
    font-size: var(--text-xs);
    font-weight: 550;
    color: var(--fg-subtle);
  }

  input,
  select {
    width: 100%;
    min-width: 0;
    padding: 5px var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    user-select: text;
  }
  input:focus,
  select:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 16%, transparent);
  }
  input[aria-invalid='true'] {
    border-color: var(--danger);
  }
  .name {
    flex: 1;
    font-weight: 600;
    font-size: var(--text-base);
  }

  .why {
    margin: 0;
    font-size: var(--text-xs);
    color: var(--danger);
  }

  .foot {
    display: flex;
    justify-content: flex-end;
    gap: var(--sp-2);
  }

  @container (max-width: 460px) {
    .grid {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
    .f.wider {
      grid-column: span 2;
    }
  }
</style>
