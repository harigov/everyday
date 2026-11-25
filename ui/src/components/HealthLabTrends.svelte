<script lang="ts">
  // Lab results, a test at a time.
  //
  // The list has one row per test -- TSH, haemoglobin A1c, LDL -- with its
  // latest result, when that was, and how many there are. A test is whatever
  // shares a LOINC code, or failing that a name. Opening one shows its
  // results as dots over time against the reference range of the latest
  // result that printed one, and under the chart a table of every result,
  // which is the chart's accessible twin and where a result is corrected or
  // deleted. A result can be added by hand, for a number read off a phone
  // call or a paper nobody scanned.
  //
  // A result outside its own printed range is marked in words as well as
  // colour. The range is the lab's, printed beside the number; this does not
  // decide what is healthy.

  import { health } from '../lib/health.svelte'
  import { HEALTH_ACCENT, formatNumber, labKey, labOutOfRange, newId } from '../lib/health'
  import { dateFormat, plural } from '../lib/format'
  import { closesOnBack, phone } from '../lib/phone.svelte'
  import { startOfDay, todayIso } from '../lib/time'
  import type { LabFlag, LabResult, LabTrend } from '../lib/types'
  import EmptyState from './EmptyState.svelte'
  import HealthLine from './HealthLine.svelte'
  import Icon from './Icon.svelte'
  import PhoneBack from './PhoneBack.svelte'

  const FLAGS: { value: LabFlag; label: string }[] = [
    { value: 'none', label: 'No flag' },
    { value: 'normal', label: 'Normal' },
    { value: 'low', label: 'Low' },
    { value: 'high', label: 'High' },
    { value: 'abnormal', label: 'Abnormal' },
    { value: 'critical', label: 'Critical' },
  ]

  const DATE = /^\d{4}-\d{2}-\d{2}$/
  const dayFormat = dateFormat({ day: 'numeric', month: 'short', year: 'numeric' })

  function dateText(iso: string | null | undefined): string {
    if (!iso) return ''
    const at = startOfDay(iso.slice(0, 10))
    return Number.isNaN(at.getTime()) ? iso : dayFormat.format(at)
  }

  function figure(n: number): string {
    return formatNumber(n, 4)
  }

  function resultText(lab: LabResult): string {
    if (lab.value !== null && lab.value !== undefined) return figure(lab.value)
    return lab.text || '—'
  }

  /** The reference range as it should be read: its numbers when it has
   *  them, else what the paper said. */
  function rangeText(lab: LabResult): string {
    const low = lab.refLow ?? null
    const high = lab.refHigh ?? null
    if (low !== null && high !== null) return `${figure(low)} – ${figure(high)}`
    if (high !== null) return `up to ${figure(high)}`
    if (low !== null) return `${figure(low)} or more`
    return lab.refText
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

  function originWord(lab: LabResult): string {
    if (lab.lab) return lab.lab
    switch (lab.origin) {
      case 'extracted':
        return 'Read from a document'
      case 'fhir':
        return 'Clinical record'
      default:
        return 'Typed in'
    }
  }

  /** Newest first. */
  function newest(a: LabResult, b: LabResult): number {
    return (
      b.collected.localeCompare(a.collected) ||
      (b.collectedAt ?? b.createdAt).localeCompare(a.collectedAt ?? a.createdAt)
    )
  }

  function latestOf(t: LabTrend): LabResult | null {
    return [...t.results].sort(newest)[0] ?? null
  }

  /** Where a result sits on the time axis: its moment when it has one, else
   *  its day -- a bare date, which the chart reads as that local day. */
  function atOf(lab: LabResult): string {
    return lab.collectedAt ?? lab.collected
  }

  // ── the list ─────────────────────────────────────────────────────────

  const rows = $derived(health.labTrends.map((t) => ({ trend: t, latest: latestOf(t) })))

  // ── one test open ────────────────────────────────────────────────────

  let openKey = $state<string | null>(null)
  const trend = $derived(
    openKey === null ? null : (health.labTrends.find((t) => t.key === openKey) ?? null),
  )
  const results = $derived(trend ? [...trend.results].sort(newest) : [])
  /** The numeric results, oldest first, for the chart. */
  const points = $derived(
    results
      .flatMap((lab) =>
        lab.value === null || lab.value === undefined ? [] : [{ lab, value: lab.value }],
      )
      .reverse(),
  )

  /** The latest result that printed numbers for its range. */
  const ranged = $derived(
    results.find((r) => (r.refLow ?? null) !== null || (r.refHigh ?? null) !== null) ?? null,
  )
  const band = $derived.by(() => {
    if (!ranged) return null
    const low = ranged.refLow ?? null
    const high = ranged.refHigh ?? null
    const values = points.map((p) => p.value)
    if (low !== null && high !== null) return { low, high }
    // One-sided ranges ("under 5.7"): shade down to zero, or up past the
    // highest result, so the band has two edges to draw.
    if (high !== null) return { low: Math.min(0, ...values), high }
    if (low !== null) return { low, high: Math.max(low, ...values) * 1.2 }
    return null
  })
  const bandNote = $derived(
    band && ranged ? `${rangeText(ranged)}${trend?.unit ? ` ${trend.unit}` : ''}` : '',
  )
  const bounds = $derived.by(() => {
    const values = points.map((p) => p.value)
    if (band) values.push(band.low, band.high)
    if (values.length === 0) return null
    const lo = Math.min(...values)
    const hi = Math.max(...values)
    const pad = (hi - lo) * 0.15 || Math.abs(hi) * 0.1 || 1
    return { min: lo >= 0 && lo - pad < 0 ? 0 : lo - pad, max: hi + pad }
  })
  const series = $derived(points.map((p) => ({ at: atOf(p.lab), value: p.value })))

  // ── adding and correcting by hand ────────────────────────────────────

  interface Draft {
    /** The result being corrected; `null` for a new one. */
    base: LabResult | null
    name: string
    code: string
    result: string
    unit: string
    refLow: string
    refHigh: string
    refText: string
    flag: LabFlag
    collected: string
    lab: string
    note: string
  }

  let draft = $state<Draft | null>(null)
  let saving = $state(false)
  let problem = $state<string | null>(null)

  function typed(n: number | null | undefined): string {
    return n === null || n === undefined ? '' : String(n)
  }

  function parseNumber(raw: string): number | null {
    const s = raw.trim()
    if (s === '') return null
    let plain = s
    if (/^-?\d{1,3}(,\d{3})+(\.\d+)?$/.test(s)) plain = s.replace(/,/g, '')
    else if (/^-?\d+,\d+$/.test(s)) plain = s.replace(',', '.')
    const n = Number(plain)
    return Number.isFinite(n) ? n : null
  }

  function startAdd() {
    const t = trend
    const last = t ? latestOf(t) : null
    problem = null
    draft = {
      base: null,
      name: t?.name ?? '',
      code: last?.code ?? '',
      result: '',
      unit: t?.unit ?? '',
      refLow: typed(last?.refLow),
      refHigh: typed(last?.refHigh),
      refText: last?.refText ?? '',
      flag: 'none',
      collected: todayIso(),
      lab: last?.lab ?? '',
      note: '',
    }
  }

  function startEdit(lab: LabResult) {
    problem = null
    draft = {
      base: lab,
      name: lab.name,
      code: lab.code,
      result: lab.value !== null && lab.value !== undefined ? String(lab.value) : lab.text,
      unit: lab.unit,
      refLow: typed(lab.refLow),
      refHigh: typed(lab.refHigh),
      refText: lab.refText,
      flag: lab.flag,
      collected: lab.collected,
      lab: lab.lab,
      note: lab.note,
    }
  }

  function stop() {
    draft = null
    problem = null
  }

  // On a phone one test opened from the list is a page over it, and the form
  // that adds or corrects a result a layer over that: back closes each.
  closesOnBack(
    () => phone.on && openKey !== null,
    () => (openKey = null),
  )
  closesOnBack(
    () => phone.on && draft !== null,
    () => stop(),
  )

  /** A name that is already a test fills in its code and unit, once. */
  function nameChosen() {
    const d = draft
    if (!d || d.base) return
    const known = health.labTrends.find((t) => t.name.toLowerCase() === d.name.trim().toLowerCase())
    if (!known) return
    const last = latestOf(known)
    if (d.code === '') d.code = last?.code ?? ''
    if (d.unit === '') d.unit = known.unit
  }

  function problemWith(d: Draft): string | null {
    if (d.name.trim() === '') return 'Give the test a name.'
    if (d.result.trim() === '') return 'Give the result.'
    if (d.refLow.trim() !== '' && parseNumber(d.refLow) === null) {
      return 'The low end of the range is not a number.'
    }
    if (d.refHigh.trim() !== '' && parseNumber(d.refHigh) === null) {
      return 'The high end of the range is not a number.'
    }
    if (!DATE.test(d.collected)) return 'Give the date it was collected.'
    return null
  }

  async function submit() {
    const d = draft
    if (!d || saving) return
    const why = problemWith(d)
    if (why) {
      problem = why
      return
    }
    saving = true
    problem = null
    try {
      const now = new Date().toISOString()
      const result = d.result.trim()
      const n = parseNumber(result)
      const fields = {
        name: d.name.trim(),
        code: d.code.trim(),
        value: n,
        text: n === null ? result : d.base && d.base.value === n ? d.base.text : '',
        unit: d.unit.trim(),
        refLow: parseNumber(d.refLow),
        refHigh: parseNumber(d.refHigh),
        refText: d.refText.trim(),
        flag: d.flag,
        collected: d.collected,
        lab: d.lab.trim(),
        note: d.note.trim(),
      }
      const lab: LabResult = d.base
        ? {
            ...$state.snapshot(d.base),
            ...fields,
            collectedAt: d.collected === d.base.collected ? (d.base.collectedAt ?? null) : null,
            updatedAt: now,
          }
        : {
            id: newId(),
            ...fields,
            collectedAt: null,
            panel: '',
            documentId: null,
            origin: 'manual',
            createdAt: now,
            updatedAt: now,
          }
      const saved = await health.saveLab(lab)
      if (saved) {
        draft = null
        openKey = labKey(saved)
      } else {
        problem = 'The result could not be saved.'
      }
    } catch {
      problem = 'The result could not be saved.'
    } finally {
      saving = false
    }
  }

  function documentOf(lab: LabResult) {
    return lab.documentId ? health.documents.find((d) => d.id === lab.documentId) : undefined
  }
</script>

{#snippet editor()}
  {#if draft}
    <form
      class="editor"
      aria-label={draft.base ? 'Correct this result' : 'Add a result'}
      onsubmit={(e) => {
        e.preventDefault()
        void submit()
      }}
    >
      <span class="eyebrow">{draft.base ? 'Correct this result' : 'Add a result'}</span>
      <div class="fields">
        <label class="f wide">
          <span>Test</span>
          <input
            bind:value={draft.name}
            list="lab-names"
            autocomplete="off"
            placeholder="e.g. TSH"
            onchange={nameChosen}
          />
        </label>
        <label class="f">
          <span>Result</span>
          <input bind:value={draft.result} autocomplete="off" inputmode="decimal" />
        </label>
        <label class="f">
          <span>Unit</span>
          <input bind:value={draft.unit} autocomplete="off" />
        </label>
        <label class="f">
          <span>Range low</span>
          <input bind:value={draft.refLow} autocomplete="off" inputmode="decimal" />
        </label>
        <label class="f">
          <span>Range high</span>
          <input bind:value={draft.refHigh} autocomplete="off" inputmode="decimal" />
        </label>
        <label class="f wide">
          <span>Range as printed</span>
          <input bind:value={draft.refText} autocomplete="off" placeholder="e.g. 0.4 – 4.0" />
        </label>
        <label class="f">
          <span>Flag</span>
          <select bind:value={draft.flag}>
            {#each FLAGS as f (f.value)}
              <option value={f.value}>{f.label}</option>
            {/each}
          </select>
        </label>
        <label class="f">
          <span>Collected</span>
          <input type="date" bind:value={draft.collected} />
        </label>
        <label class="f">
          <span>LOINC code</span>
          <input bind:value={draft.code} autocomplete="off" />
        </label>
        <label class="f wide">
          <span>Lab</span>
          <input bind:value={draft.lab} autocomplete="off" placeholder="Who ran it" />
        </label>
        <label class="f wider">
          <span>Note</span>
          <input bind:value={draft.note} autocomplete="off" />
        </label>
      </div>
      {#if problem}<p class="error" role="alert">{problem}</p>{/if}
      <div class="foot">
        <button type="button" class="btn" disabled={saving} onclick={stop}>Cancel</button>
        <button class="btn btn-primary" type="submit" disabled={saving}>
          {saving ? 'Saving…' : 'Save result'}
        </button>
      </div>
    </form>
  {/if}
{/snippet}

<datalist id="lab-names">
  {#each health.labTrends as t (t.key)}
    <option value={t.name}></option>
  {/each}
</datalist>

<div class="trends scroll">
  <div class="page">
    {#if trend}
      <div class="d-head">
        <PhoneBack onclick={() => (openKey = null)} label="All tests" />
        <button class="back desktop-only" onclick={() => (openKey = null)}>
          <span class="flip"><Icon name="chevron" size={13} /></span>
          All tests
        </button>
        <div class="pane-titles">
          <h2 class="pane-title">{trend.name}</h2>
          <span class="pane-sub">
            {trend.unit || 'No unit'} · {plural(results.length, 'result')}
          </span>
        </div>
        <button class="btn btn-outline btn-sm" onclick={startAdd}>
          <Icon name="plus" size={13} />
          Add a result
        </button>
      </div>

      {@render editor()}

      {#if series.length > 0}
        <figure class="chart" aria-label="{trend.name} over time">
          <HealthLine
            {series}
            {band}
            unit={trend.unit}
            color={HEALTH_ACCENT}
            height={200}
            dots
            yMin={bounds?.min}
            yMax={bounds?.max}
          />
          <figcaption class="hint">
            {#if bandNote}
              The shaded band is the reference range of the latest result that printed one: {bandNote}.
            {/if}
            Every result is in the table below.
          </figcaption>
        </figure>
      {/if}

      <div class="table-wrap">
        <table>
          <caption class="sr-only">Results for {trend.name}, newest first</caption>
          <thead>
            <tr>
              <th scope="col">Date</th>
              <th scope="col">Result</th>
              <th scope="col">Reference</th>
              <th scope="col">From</th>
              <th scope="col"><span class="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            {#each results as lab (lab.id)}
              {@const out = labOutOfRange(lab)}
              {@const source = documentOf(lab)}
              <tr class:editing={draft?.base?.id === lab.id}>
                <td class="when">{dateText(lab.collected)}</td>
                <td>
                  <div class="what">
                    <span class="value">{resultText(lab)}</span>
                    {#if lab.unit}<span class="unit">{lab.unit}</span>{/if}
                    {#if out}
                      <span class="mark">
                        <Icon name="alert" size={12} />
                        {flagWord(lab)}
                      </span>
                    {/if}
                  </div>
                </td>
                <td class="ref">{rangeText(lab) || '—'}</td>
                <td class="from">
                  {#if source}
                    <button class="link" onclick={() => health.openDocument(source.id)}>
                      {source.title || 'Open the document'}
                    </button>
                  {:else}
                    {originWord(lab)}
                  {/if}
                </td>
                <td class="acts">
                  <button
                    class="icon"
                    aria-label="Correct the result from {dateText(lab.collected)}"
                    title="Correct"
                    onclick={() => startEdit(lab)}
                  >
                    <Icon name="pencil" size={14} />
                  </button>
                  <button
                    class="icon"
                    aria-label="Delete the result from {dateText(lab.collected)}"
                    title="Delete"
                    onclick={() => health.deleteLab(lab)}
                  >
                    <Icon name="trash" size={14} />
                  </button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {:else}
      {@render editor()}

      {#if rows.length === 0 && !draft}
        <EmptyState lead="No lab results yet.">
          {#snippet icon()}<Icon name="chart" size={30} weight={1.3} />{/snippet}
          {#snippet note()}
            Open a lab report under Documents and choose Read results, and what the model finds
            comes here once you have checked it. Or add a result by hand.
          {/snippet}
          {#snippet action()}
            <button class="btn btn-primary" onclick={startAdd}>Add a result</button>
          {/snippet}
        </EmptyState>
      {:else if rows.length > 0}
        <div class="l-head">
          <span class="eyebrow">Tests</span>
          {#if !draft}
            <button class="btn btn-outline btn-sm" onclick={startAdd}>
              <Icon name="plus" size={13} />
              Add a result
            </button>
          {/if}
        </div>
        <div class="cols" aria-hidden="true">
          <span>Test</span>
          <span>Latest</span>
          <span>Collected</span>
          <span class="end">Results</span>
        </div>
        <ul class="tests" aria-label="Tests">
          {#each rows as { trend: t, latest } (t.key)}
            {@const out = latest ? labOutOfRange(latest) : false}
            <li>
              <button class="list-row test" onclick={() => (openKey = t.key)}>
                <span class="t-name">{t.name}</span>
                <span class="t-latest">
                  {#if latest}
                    <span class="value">{resultText(latest)}</span>
                    {#if t.unit}<span class="unit">{t.unit}</span>{/if}
                    {#if out}
                      <span class="mark">
                        <Icon name="alert" size={12} />
                        {flagWord(latest)}
                      </span>
                    {/if}
                  {/if}
                </span>
                <span class="t-date">{latest ? dateText(latest.collected) : ''}</span>
                <span class="t-count">{plural(t.results.length, 'result')}</span>
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    {/if}
  </div>
</div>

<style>
  .trends {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow-y: auto;
    container-type: inline-size;
  }
  .page {
    flex: 1 0 auto;
    width: 100%;
    max-width: 860px;
    margin: 0 auto;
    padding: var(--sp-2) var(--sp-6) var(--fab-clear);
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }

  .l-head,
  .d-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
  }
  .back {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: var(--sp-1);
    height: 28px;
    padding: 0 var(--sp-3) 0 var(--sp-2);
    border-radius: 999px;
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-subtle);
  }
  .back:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .flip {
    display: grid;
    place-items: center;
    rotate: 180deg;
  }

  /* The list: a quiet header over rows of the same four columns. */
  .cols,
  .test {
    display: grid;
    grid-template-columns: minmax(0, 1.4fr) minmax(0, 1.3fr) 7.5em 6em;
    align-items: center;
    gap: var(--sp-3);
  }
  .cols {
    padding: 0 var(--sp-5);
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--fg-faint);
  }
  .end,
  .t-count {
    text-align: right;
  }
  .tests {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .test {
    padding: var(--sp-3);
  }
  .t-name {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .t-latest {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: var(--sp-1) var(--sp-2);
    min-width: 0;
  }
  .t-date,
  .t-count {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .value {
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .unit {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .mark {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-1);
    height: 20px;
    padding: 0 var(--sp-2);
    border-radius: 99px;
    background: color-mix(in oklab, var(--warning) 16%, transparent);
    color: color-mix(in oklab, var(--warning) 55%, var(--fg));
    font-size: var(--text-xs);
    font-weight: 600;
    white-space: nowrap;
  }

  .chart {
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .chart .hint {
    margin: 0;
  }

  .table-wrap {
    overflow-x: auto;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--text-sm);
  }
  th {
    padding: var(--sp-2) var(--sp-3);
    text-align: left;
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--fg-faint);
    border-bottom: 1px solid var(--border);
  }
  td {
    padding: var(--sp-2) var(--sp-3);
    vertical-align: baseline;
    border-bottom: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }
  tr.editing td {
    background: var(--bg-selected);
  }
  .when,
  .ref {
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .what {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: var(--sp-1) var(--sp-2);
  }
  .from {
    color: var(--fg-subtle);
    max-width: 14em;
    overflow-wrap: anywhere;
  }
  .link {
    color: var(--accent);
    font-weight: 550;
    text-align: left;
  }
  .link:hover {
    text-decoration: underline;
  }
  .acts {
    white-space: nowrap;
    text-align: right;
  }
  .icon {
    display: inline-grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .icon:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .editor {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg);
  }
  .fields {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: var(--sp-2) var(--sp-3);
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
  .editor input,
  .editor select {
    width: 100%;
    min-width: 0;
    padding: 6px var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    user-select: text;
  }
  .editor input:focus,
  .editor select:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 16%, transparent);
  }
  .editor .error {
    margin: 0;
  }
  .foot {
    display: flex;
    justify-content: flex-end;
    gap: var(--sp-2);
  }

  @container (max-width: 640px) {
    .cols {
      display: none;
    }
    .test {
      grid-template-columns: minmax(0, 1fr) auto;
      row-gap: var(--sp-1);
    }
    .t-latest {
      grid-column: 1 / -1;
      grid-row: 2;
    }
    .t-date {
      grid-column: 1;
      grid-row: 3;
    }
    .t-count {
      grid-column: 2;
      grid-row: 1;
    }
    .fields {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
    .f.wider {
      grid-column: span 2;
    }
  }

  /* ── A phone ────────────────────────────────────────────────────────
     A test opened is a page with its way back and Add a result across the
     top and its name under them. Its table is laid out as a card per result
     -- date, value, then what the reference and the source were -- rather
     than five columns in a box that scrolls sideways; the form's fields
     are a finger tall. */
  :global([data-phone]) .page {
    padding-inline: var(--sp-3);
  }
  :global([data-phone]) .d-head {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    gap: 0 var(--sp-2);
  }
  :global([data-phone]) .d-head .pane-titles {
    grid-column: 1 / -1;
    grid-row: 2;
    padding-inline: var(--sp-2);
  }
  :global([data-phone]) .d-head .btn {
    grid-column: 3;
    grid-row: 1;
  }
  :global([data-phone]) .d-head .pane-title {
    white-space: normal;
  }
  :global([data-phone]) .test {
    min-height: 64px;
  }
  :global([data-phone]) table,
  :global([data-phone]) tbody,
  :global([data-phone]) tr,
  :global([data-phone]) td {
    display: block;
  }
  :global([data-phone]) thead {
    display: none;
  }
  :global([data-phone]) tr {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    column-gap: var(--sp-2);
    padding: var(--sp-2) 0;
    border-bottom: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }
  :global([data-phone]) td {
    padding: 1px 0;
    border: 0;
    background: none;
  }
  :global([data-phone]) tr.editing {
    background: var(--bg-selected);
  }
  :global([data-phone]) td.when {
    grid-column: 1;
    grid-row: 1;
  }
  :global([data-phone]) td:nth-child(2) {
    grid-column: 1;
    grid-row: 2;
  }
  :global([data-phone]) td.ref {
    grid-column: 1;
    grid-row: 3;
    white-space: normal;
  }
  :global([data-phone]) td.ref::before {
    content: 'Reference: ';
    color: var(--fg-faint);
  }
  :global([data-phone]) td.from {
    grid-column: 1;
    grid-row: 4;
    max-width: none;
  }
  :global([data-phone]) td.from::before {
    content: 'From: ';
    color: var(--fg-faint);
  }
  :global([data-phone]) td.acts {
    grid-column: 2;
    grid-row: 1 / span 2;
    align-self: center;
  }
  :global([data-phone]) .icon {
    width: 40px;
    height: 40px;
  }
  :global([data-phone]) .link {
    display: inline-flex;
    align-items: center;
    min-height: 40px;
  }
  :global([data-phone]) .editor {
    padding: var(--sp-3);
  }
  :global([data-phone]) .editor input,
  :global([data-phone]) .editor select {
    min-height: 40px;
  }
  :global([data-phone]) .foot {
    flex-wrap: wrap;
  }
</style>
