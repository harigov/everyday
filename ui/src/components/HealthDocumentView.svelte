<script lang="ts">
  // One open document: the page itself on one side, what is known about it
  // on the other.
  //
  // The page is drawn from the document's own sealed file -- a PDF by the
  // same reader the mail attachments use, a picture as a picture -- and
  // never leaves the vault to be shown. Beside it: its details (title, kind,
  // date, provider, notes, which conditions it is about), saved together
  // with Save changes; the records a clinical export brought, when it has
  // them; and "Read results", which asks the model for the lab results on
  // the page. What the model proposes comes up as `HealthLabReview`, right
  // beside the page it came from, and waits there until it has been checked.
  //
  // The details are held as a draft. The draft follows the document while
  // it is untouched -- a re-read, a sync -- and is left alone once it has
  // been edited, so a background change never eats what was being typed.

  import { untrack } from 'svelte'
  import { mediaUrl } from '../lib/api'
  import { health } from '../lib/health.svelte'
  import { docKindName } from '../lib/health'
  import { dateFormat, humanBytes } from '../lib/format'
  import { startOfDay } from '../lib/time'
  import type { ConditionId, DocFile, DocKind } from '../lib/types'
  import HealthLabReview from './HealthLabReview.svelte'
  import Icon from './Icon.svelte'
  import PdfView from './PdfView.svelte'
  import PhoneBack from './PhoneBack.svelte'

  const KINDS: DocKind[] = [
    'labReport',
    'visitNote',
    'imaging',
    'discharge',
    'prescription',
    'immunization',
    'insurance',
    'fhir',
    'ecg',
    'other',
  ]

  const dayFormat = dateFormat({ day: 'numeric', month: 'short', year: 'numeric' })

  function dateText(iso: string | null | undefined): string {
    if (!iso) return 'No date'
    const at = startOfDay(iso.slice(0, 10))
    return Number.isNaN(at.getTime()) ? iso : dayFormat.format(at)
  }

  const doc = $derived(health.document)
  const pending = $derived(doc?.pendingLabs.length ?? 0)

  // ── the details, as a draft ──────────────────────────────────────────

  let title = $state('')
  let kind = $state<DocKind>('other')
  let date = $state('')
  let provider = $state('')
  let notes = $state('')
  let conditionIds = $state<ConditionId[]>([])
  let saving = $state(false)
  let seededFor = ''

  function sameIds(a: string[], b: string[]): boolean {
    return a.length === b.length && a.every((id) => b.includes(id))
  }

  const dirty = $derived(
    doc !== null &&
      (title !== doc.title ||
        kind !== doc.kind ||
        date !== (doc.date ?? '') ||
        provider !== doc.provider ||
        notes !== doc.notes ||
        !sameIds(conditionIds, doc.conditionIds)),
  )

  function load() {
    const d = doc
    if (!d) return
    title = d.title
    kind = d.kind
    date = d.date ?? ''
    provider = d.provider
    notes = d.notes
    conditionIds = [...d.conditionIds]
  }

  // A different document starts from its own details; the same one is read
  // again only while nothing has been typed over it.
  $effect(() => {
    const d = doc
    if (!d) return
    untrack(() => {
      if (d.id !== seededFor || !dirty) {
        seededFor = d.id
        load()
      }
    })
  })

  function toggle(id: ConditionId) {
    conditionIds = conditionIds.includes(id)
      ? conditionIds.filter((c) => c !== id)
      : [...conditionIds, id]
  }

  async function save() {
    const d = doc
    if (!d || saving || !dirty) return
    saving = true
    try {
      await health.saveDocument({
        ...$state.snapshot(d),
        title: title.trim() || d.title,
        kind,
        date: date === '' ? null : date,
        provider: provider.trim(),
        notes,
        conditionIds: [...conditionIds],
      })
    } finally {
      saving = false
    }
  }

  // ── reading results ──────────────────────────────────────────────────

  let reading = $state(false)
  const busyReading = $derived(reading || doc?.extraction === 'running')
  /** Results proposed and not yet checked: `HealthLabReview` has the floor. */
  const waiting = $derived(doc?.extraction === 'proposed' && pending > 0)
  const readLabel = $derived(
    busyReading
      ? 'Reading…'
      : doc?.extraction === 'none'
        ? 'Read results'
        : doc?.extraction === 'failed'
          ? 'Try again'
          : 'Read again',
  )

  async function read() {
    const d = doc
    if (!d || busyReading) return
    reading = true
    try {
      await health.extractDocument(d.id)
    } finally {
      reading = false
    }
  }

  // ── the page ─────────────────────────────────────────────────────────

  type Shown =
    | { state: 'loading' }
    | { state: 'failed'; reason: string }
    | { state: 'image'; url: string }
    | { state: 'pdf'; data: Uint8Array }
    | { state: 'other' }

  const COULD_NOT = 'This file could not be shown here.'

  let fileIndex = $state(0)
  let shown = $state.raw<Shown>({ state: 'loading' })
  let zoomed = $state(false)
  let openedId = ''

  const file = $derived(
    doc && doc.files.length > 0
      ? (doc.files[Math.min(fileIndex, doc.files.length - 1)] ?? null)
      : null,
  )
  /** Which file is on the page. A string, so it only changes when the file
   *  does, and a re-read of the document does not load it all again. */
  const fileKey = $derived(doc && file ? `${doc.id}/${file.file}` : '')

  $effect(() => {
    const id = doc?.id ?? ''
    if (id === openedId) return
    openedId = id
    fileIndex = 0
    zoomed = false
  })

  function viewKind(f: DocFile): 'pdf' | 'image' | 'other' {
    const mime = f.mime.toLowerCase()
    if (mime === 'application/pdf' || /\.pdf$/i.test(f.name)) return 'pdf'
    if (mime.startsWith('image/') || /\.(png|jpe?g|gif|webp|bmp|avif)$/i.test(f.name)) {
      return 'image'
    }
    return 'other'
  }

  $effect(() => {
    const key = fileKey
    if (key === '') return
    const target = untrack(() => (doc && file ? { d: doc, f: file } : null))
    if (!target) return
    let gone = false
    shown = { state: 'loading' }
    void (async () => {
      try {
        const url = untrack(() => health.documentFileUrl(target.d, target.f.file))
        if (url === '') throw new Error('no address')
        const how = viewKind(target.f)
        let next: Shown
        if (how === 'image') {
          next = { state: 'image', url }
        } else if (how === 'pdf') {
          const response = await fetch(url)
          if (!response.ok) throw new Error(String(response.status))
          next = { state: 'pdf', data: new Uint8Array(await response.arrayBuffer()) }
        } else {
          next = { state: 'other' }
        }
        if (!gone) shown = next
      } catch {
        if (!gone) shown = { state: 'failed', reason: COULD_NOT }
      }
    })()
    return () => {
      gone = true
    }
  })

  function failed(reason: string) {
    shown = { state: 'failed', reason }
  }

  // ── what a clinical record lists ─────────────────────────────────────

  const FEW = 25
  let allEntries = $state(false)
  const entries = $derived(doc ? (allEntries ? doc.fhir : doc.fhir.slice(0, FEW)) : [])

  const conditions = $derived(health.conditions)
</script>

{#if doc}
  <div class="doc">
    <header class="top">
      <PhoneBack onclick={() => health.openDocument(null)} label="Records" />
      <button class="back desktop-only" onclick={() => health.openDocument(null)}>
        <span class="flip"><Icon name="chevron" size={13} /></span>
        Records
      </button>
      <div class="pane-titles">
        <h2 class="pane-title">{doc.title || 'Untitled document'}</h2>
        <span class="pane-sub">
          {docKindName(doc.kind)} · {dateText(doc.date)}{#if doc.provider}&nbsp;·
            {doc.provider}{/if}
        </span>
      </div>
      <button
        class="btn btn-ghost-danger btn-outline btn-sm"
        onclick={() => health.deleteDocument(doc)}
      >
        <Icon name="trash" size={13} />
        Delete…
      </button>
    </header>

    <div class="cols" class:review={pending > 0} class:single={!file}>
      {#if file}
        <section class="page" aria-label="The document">
          {#if doc.files.length > 1}
            <div class="filters files" role="tablist" aria-label="Files in this document">
              {#each doc.files as f, i (f.file)}
                <button
                  class="filter"
                  class:on={file.file === f.file}
                  role="tab"
                  aria-selected={file.file === f.file}
                  title="{f.name} · {humanBytes(f.size)}"
                  onclick={() => (fileIndex = i)}
                >
                  <span class="file-name">{f.name}</span>
                </button>
              {/each}
            </div>
          {/if}

          <div class="stage">
            {#if shown.state === 'loading'}
              <p class="quiet">Opening…</p>
            {:else if shown.state === 'failed'}
              <p class="quiet">{shown.reason}</p>
            {:else if shown.state === 'other'}
              <p class="quiet">This kind of file can’t be shown here.</p>
            {:else if shown.state === 'image'}
              <div class="picture-wrap">
                <img
                  class="picture"
                  class:zoomed
                  src={shown.url}
                  alt={file.name}
                  onerror={() => failed('This picture could not be shown here.')}
                />
              </div>
              <button class="pill" onclick={() => (zoomed = !zoomed)}>
                {zoomed ? 'Fit to the window' : 'Actual size'}
              </button>
            {:else}
              <div class="reader">
                <PdfView data={shown.data} onfail={failed} />
              </div>
            {/if}
          </div>
        </section>
      {/if}

      <div class="side">
        {#if pending > 0}
          <HealthLabReview {doc} />
        {/if}

        {#if !waiting && doc.files.length > 0}
          <section class="block" aria-labelledby="read-head">
            <span class="eyebrow" id="read-head">Lab results</span>
            {#if doc.extraction === 'failed' && !busyReading}
              <p class="error" role="alert">
                {doc.extractionError || 'The document could not be read.'}
              </p>
            {:else if busyReading}
              <p class="state" role="status">
                <Icon name="refresh" size={14} />
                Reading the document. This can take a minute.
              </p>
            {:else if doc.extraction === 'proposed'}
              <p class="state">
                <Icon name="info" size={14} />
                The model found no lab results in this document.
              </p>
            {:else if doc.extraction === 'done'}
              <p class="state">
                <Icon name="tick" size={14} />
                Results from this document are saved under Lab results.
              </p>
            {:else}
              <p class="hint">
                The model can read lab results off the page and propose them. You check them against
                the page before anything is kept.
              </p>
            {/if}
            <div>
              <button class="btn btn-outline" disabled={busyReading} onclick={() => void read()}>
                <Icon name="sparkle" size={14} />
                {readLabel}
              </button>
            </div>
            <p class="hint small">
              Reading sends this document’s text, or its pages as pictures, to your large model.
            </p>
          </section>
        {/if}

        <form
          class="block"
          aria-labelledby="details-head"
          onsubmit={(e) => {
            e.preventDefault()
            void save()
          }}
        >
          <span class="eyebrow" id="details-head">Details</span>
          <label>
            <span class="field-label">Title</span>
            <input class="field" bind:value={title} autocomplete="off" />
          </label>
          <div class="two">
            <label>
              <span class="field-label">Kind</span>
              <select class="field" bind:value={kind}>
                {#each KINDS as k (k)}
                  <option value={k}>{docKindName(k)}</option>
                {/each}
              </select>
            </label>
            <label>
              <span class="field-label">Date on it</span>
              <input class="field" type="date" bind:value={date} />
            </label>
          </div>
          <label>
            <span class="field-label">Provider</span>
            <input
              class="field"
              bind:value={provider}
              placeholder="Clinic, hospital or lab"
              autocomplete="off"
            />
          </label>
          <label>
            <span class="field-label">Notes</span>
            <textarea class="field" rows="3" bind:value={notes}></textarea>
          </label>

          {#if conditions.length > 0}
            <fieldset class="conds">
              <legend class="field-label">About these conditions</legend>
              {#each conditions as c (c.id)}
                <label class="check">
                  <input
                    type="checkbox"
                    checked={conditionIds.includes(c.id)}
                    onchange={() => toggle(c.id)}
                  />
                  <span>{c.name}</span>
                </label>
              {/each}
            </fieldset>
          {/if}

          <div class="save">
            <button type="button" class="btn" disabled={!dirty || saving} onclick={load}>
              Revert
            </button>
            <button class="btn btn-primary" type="submit" disabled={!dirty || saving}>
              {saving ? 'Saving…' : 'Save changes'}
            </button>
          </div>
        </form>

        {#if doc.fhir.length > 0}
          <section class="block" aria-labelledby="fhir-head">
            <span class="eyebrow" id="fhir-head">In this record</span>
            <p class="hint">
              What the provider’s record lists, as it arrived. It is shown here, not interpreted.
            </p>
            <ul class="fhir">
              {#each entries as e, i (i)}
                <li>
                  <span class="fhir-type">{e.resourceType}</span>
                  <span class="fhir-name">{e.display}</span>
                  {#if e.value}<span class="fhir-value">{e.value}</span>{/if}
                  {#if e.date}<span class="fhir-date">{dateText(e.date)}</span>{/if}
                </li>
              {/each}
            </ul>
            {#if doc.fhir.length > FEW}
              <div>
                <button class="btn btn-sm" onclick={() => (allEntries = !allEntries)}>
                  {allEntries ? 'Show fewer' : `Show all ${doc.fhir.length.toLocaleString()}`}
                </button>
              </div>
            {/if}
          </section>
        {/if}

        <!-- What a connector sent (a FHIR resource's JSON, an ECG's CSV) is
             the record's own data, not a page: nothing here draws it, but it
             can be saved and opened elsewhere, as it arrived. -->
        {#if doc.source}
          <section class="block" aria-labelledby="source-head">
            <span class="eyebrow" id="source-head">As it arrived</span>
            <p class="hint">
              <a href={mediaUrl(doc.source.blob)} download={doc.source.name}>{doc.source.name}</a>
              · {humanBytes(doc.source.size)}
            </p>
          </section>
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
  .doc {
    height: 100%;
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    container-type: inline-size;
  }

  .top {
    flex: none;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-2) var(--sp-4) var(--sp-3) var(--sp-4);
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

  .cols {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    grid-template-rows: minmax(0, 1fr);
    gap: var(--sp-5);
    padding: 0 var(--sp-4) var(--sp-4);
  }
  .cols.review {
    grid-template-columns: minmax(0, 1fr) minmax(0, 1.2fr);
  }
  .cols.single {
    grid-template-columns: minmax(0, 720px);
    justify-content: center;
  }

  .page {
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .files {
    align-self: flex-start;
    max-width: 100%;
    justify-content: flex-start;
  }
  .file-name {
    max-width: 14em;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .stage {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
    border-radius: var(--radius);
    background: var(--bg-sunken);
    overflow: hidden;
  }
  .stage :global(.wait) {
    color: var(--fg-subtle);
  }
  .quiet {
    margin: auto;
    padding: var(--sp-6);
    text-align: center;
    color: var(--fg-subtle);
  }
  .reader {
    flex: 1;
    min-width: 0;
    padding-top: var(--sp-2);
  }
  .picture-wrap {
    flex: 1;
    min-width: 0;
    overflow: auto;
    padding: var(--sp-3) var(--sp-3) 64px;
  }
  .picture {
    display: block;
    max-width: 100%;
    margin: 0 auto;
    background: #ffffff;
    border-radius: 2px;
    box-shadow: var(--shadow);
  }
  .picture.zoomed {
    max-width: none;
  }
  .pill {
    position: absolute;
    left: 50%;
    bottom: var(--sp-3);
    translate: -50% 0;
    height: 32px;
    padding: 0 var(--sp-4);
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--float);
    color: var(--fg);
    font-size: var(--text-sm);
    font-weight: 550;
    box-shadow: var(--shadow);
  }
  .pill:hover {
    border-color: var(--border-strong);
  }

  .side {
    min-width: 0;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: var(--sp-6);
    padding-right: var(--sp-2);
    padding-bottom: var(--fab-clear);
    container-type: inline-size;
  }
  .block {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }
  .hint {
    margin: 0;
  }
  .hint.small {
    font-size: var(--text-xs);
  }
  .state {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-muted);
  }
  .state :global(svg) {
    translate: 0 2px;
  }
  .error {
    margin: 0;
  }

  label {
    min-width: 0;
  }
  textarea.field {
    resize: vertical;
  }
  .two {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--sp-3);
  }
  .conds {
    margin: 0;
    padding: 0;
    border: 0;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .conds legend {
    padding: 0;
  }
  .check {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-base);
    cursor: pointer;
  }
  .check input {
    accent-color: var(--accent);
  }
  .save {
    display: flex;
    justify-content: flex-end;
    gap: var(--sp-2);
  }

  .fhir {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
  }
  .fhir li {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: var(--sp-1) var(--sp-3);
    padding: var(--sp-2) 0;
    border-top: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
    font-size: var(--text-sm);
  }
  .fhir-type {
    flex: none;
    min-width: 7em;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .fhir-name {
    flex: 1 1 12em;
    min-width: 0;
    color: var(--fg);
    overflow-wrap: anywhere;
  }
  .fhir-value {
    color: var(--fg-muted);
    font-variant-numeric: tabular-nums;
  }
  .fhir-date {
    color: var(--fg-subtle);
    font-size: var(--text-xs);
  }

  /* Narrow: the page above, the details under it, the whole column
     scrolling rather than each half. */
  @container (max-width: 760px) {
    .cols,
    .cols.review {
      grid-template-columns: minmax(0, 1fr);
      grid-template-rows: none;
      grid-auto-rows: auto;
      align-content: start;
      overflow-y: auto;
    }
    .page {
      height: min(70vh, 640px);
    }
    .side {
      overflow: visible;
    }
  }

  /* ── A phone ────────────────────────────────────────────────────────
     A page of its own: the way back and Delete across the top, the title
     under them, the document as tall as a phone's screen allows, and the
     details a finger can fill in. */
  :global([data-phone]) .top {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    gap: 0 var(--sp-2);
    padding: var(--sp-1) var(--sp-3) var(--sp-2);
  }
  :global([data-phone]) .top .pane-titles {
    grid-column: 1 / -1;
    grid-row: 2;
    padding-inline: var(--sp-2);
  }
  :global([data-phone]) .top .btn {
    grid-column: 3;
    grid-row: 1;
  }
  :global([data-phone]) .top .pane-title {
    white-space: normal;
  }
  :global([data-phone]) .cols {
    padding-inline: var(--sp-3);
    gap: var(--sp-4);
  }
  :global([data-phone]) .page {
    height: min(62dvh, 640px);
  }
  :global([data-phone]) .two {
    grid-template-columns: minmax(0, 1fr);
  }
  :global([data-phone]) .check {
    min-height: 44px;
  }
  :global([data-phone]) .check input {
    width: 20px;
    height: 20px;
  }
  :global([data-phone]) .pill {
    height: 40px;
  }
  :global([data-phone]) .save {
    flex-wrap: wrap;
  }
</style>
