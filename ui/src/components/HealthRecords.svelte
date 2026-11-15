<script lang="ts">
  // Records: the papers and the lab results from them.
  //
  // Two tabs. Documents are what somebody was given -- a lab report, a
  // visit note, a scan, an immunisation card, or the clinical records an
  // Apple Health export carries -- kept sealed in the vault, newest first.
  // Lab results are what was measured, grouped by test so each can be read
  // as a line over time (`HealthLabTrends`).
  //
  // Opening a document replaces the tabs with `HealthDocumentView`; the
  // tabs stay mounted underneath, hidden, so coming back finds the same
  // tab and the same test open.
  //
  // A file can be dropped anywhere over the Documents tab: that opens the
  // form with it already chosen. Nothing is stored until the form is sent.

  import { health } from '../lib/health.svelte'
  import { docKindName } from '../lib/health'
  import { dateFormat, humanBytes, plural } from '../lib/format'
  import { startOfDay } from '../lib/time'
  import type { DocKind, HealthDocument } from '../lib/types'
  import EmptyState from './EmptyState.svelte'
  import HealthDocumentView from './HealthDocumentView.svelte'
  import HealthLabTrends from './HealthLabTrends.svelte'
  import Icon from './Icon.svelte'

  type Tab = 'documents' | 'labs'
  let tab = $state<Tab>('documents')

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
    const at = startOfDay(iso)
    return Number.isNaN(at.getTime()) ? iso : dayFormat.format(at)
  }

  /** Newest first by the date on the paper, or the day it was added when it
   *  has none. */
  const documents = $derived(
    [...health.documents].sort((a, b) => {
      const byDate = (b.date ?? b.createdAt.slice(0, 10)).localeCompare(
        a.date ?? a.createdAt.slice(0, 10),
      )
      return byDate !== 0 ? byDate : b.createdAt.localeCompare(a.createdAt)
    }),
  )

  function toReview(doc: HealthDocument): number {
    return doc.extraction === 'proposed' ? doc.pendingLabs.length : 0
  }

  // ── adding ───────────────────────────────────────────────────────────

  let adding = $state(false)
  /** Not proxied: a `File` is not a plain object. */
  let files = $state.raw<File[]>([])
  let title = $state('')
  let kind = $state<DocKind>('other')
  let date = $state('')
  let saving = $state(false)
  let problem = $state<string | null>(null)
  let dropping = $state(false)
  let picker = $state<HTMLInputElement | null>(null)

  function allowed(f: File): boolean {
    return (
      f.type === 'application/pdf' ||
      f.type.startsWith('image/') ||
      /\.(pdf|png|jpe?g|webp|gif|heic|heif)$/i.test(f.name)
    )
  }

  /** A first guess from the file's name; the person has the last word. */
  function guessKind(name: string): DocKind {
    const n = name.toLowerCase()
    if (/lab|result|panel|a1c|cbc|blood/.test(n)) return 'labReport'
    if (/discharge/.test(n)) return 'discharge'
    if (/x-?ray|mri|ultrasound|imaging|radiolog/.test(n)) return 'imaging'
    if (/prescri/.test(n)) return 'prescription'
    if (/vaccin|immuni/.test(n)) return 'immunization'
    if (/insur|claim|eob/.test(n)) return 'insurance'
    if (/ecg|ekg/.test(n)) return 'ecg'
    if (/visit|note|summary|clinic/.test(n)) return 'visitNote'
    return 'other'
  }

  function titleFrom(name: string): string {
    return name
      .replace(/\.[^.]+$/, '')
      .replace(/[_-]+/g, ' ')
      .trim()
  }

  function same(a: File, b: File): boolean {
    return a.name === b.name && a.size === b.size && a.lastModified === b.lastModified
  }

  /** Take these files into the form, opening it if it is not. */
  function take(list: File[]) {
    if (list.length === 0) return
    const fit = list.filter(allowed)
    problem =
      fit.length < list.length
        ? 'Only PDFs and pictures can be added here. The others were left out.'
        : null
    if (fit.length === 0) return
    const fresh = fit.filter((f) => !files.some((g) => same(f, g)))
    if (fresh.length === 0) return
    const first = files.length === 0
    files = [...files, ...fresh]
    if (first && fresh[0]) {
      if (title.trim() === '') title = titleFrom(fresh[0].name)
      kind = guessKind(fresh[0].name)
    }
    adding = true
  }

  function onPick(e: Event & { currentTarget: HTMLInputElement }) {
    const chosen = Array.from(e.currentTarget.files ?? [])
    // Cleared, so choosing the same file again after a removal still fires.
    e.currentTarget.value = ''
    take(chosen)
  }

  function openForm() {
    adding = true
    problem = null
  }

  function cancel() {
    adding = false
    files = []
    title = ''
    kind = 'other'
    date = ''
    problem = null
  }

  async function submit() {
    if (saving || files.length === 0) return
    const name = title.trim() || titleFrom(files[0]?.name ?? '') || 'Document'
    saving = true
    problem = null
    try {
      const made = await health.addDocument(files, name, kind, date === '' ? null : date)
      if (made) {
        cancel()
        health.openDocument(made.id)
      } else {
        problem = 'The document could not be added.'
      }
    } catch {
      problem = 'The document could not be added.'
    } finally {
      saving = false
    }
  }

  function carriesFiles(e: DragEvent): boolean {
    return e.dataTransfer?.types.includes('Files') ?? false
  }

  function onDrop(e: DragEvent) {
    dropping = false
    if (!carriesFiles(e)) return
    e.preventDefault()
    take(Array.from(e.dataTransfer?.files ?? []))
  }
</script>

<div class="records" class:away={health.document !== null}>
  <div class="toolbar">
    <div class="toolbar-end"></div>
    <div class="filters" role="tablist" aria-label="Records">
      <button
        class="filter"
        class:on={tab === 'documents'}
        role="tab"
        aria-selected={tab === 'documents'}
        onclick={() => (tab = 'documents')}
      >
        Documents
        <span class="n">{health.documents.length}</span>
      </button>
      <button
        class="filter"
        class:on={tab === 'labs'}
        role="tab"
        aria-selected={tab === 'labs'}
        onclick={() => (tab = 'labs')}
      >
        Lab results
        <span class="n">{health.labTrends.length}</span>
      </button>
    </div>
    <div class="toolbar-end right">
      {#if tab === 'documents' && !adding}
        <button class="btn btn-primary btn-sm" onclick={openForm}>
          <Icon name="plus" size={14} />
          Add a document
        </button>
      {/if}
    </div>
  </div>

  {#if tab === 'documents'}
    <section
      class="body scroll"
      class:dropping
      aria-label="Documents"
      ondragover={(e) => {
        if (!carriesFiles(e)) return
        e.preventDefault()
        dropping = true
      }}
      ondragleave={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) dropping = false
      }}
      ondrop={onDrop}
    >
      {#if adding}
        <form
          class="add"
          onsubmit={(e) => {
            e.preventDefault()
            void submit()
          }}
        >
          <span class="eyebrow">Add a document</span>

          <div class="drop">
            <button type="button" class="btn btn-outline" onclick={() => picker?.click()}>
              <Icon name="upload" size={14} />
              {files.length === 0 ? 'Choose files' : 'Add more files'}
            </button>
            <span class="hint">or drop PDFs and pictures here. A scan can be several pages.</span>
          </div>
          <input
            bind:this={picker}
            type="file"
            multiple
            accept="application/pdf,image/*"
            hidden
            onchange={onPick}
          />

          {#if files.length > 0}
            <ul class="chosen" aria-label="Files chosen">
              {#each files as f (`${f.name}/${f.size}/${f.lastModified}`)}
                <li>
                  <Icon name="paperclip" size={13} />
                  <span class="chosen-name">{f.name}</span>
                  <span class="chosen-size">{humanBytes(f.size)}</span>
                  <button
                    type="button"
                    class="chosen-remove"
                    aria-label="Leave out {f.name}"
                    title="Leave out"
                    onclick={() => (files = files.filter((g) => !same(f, g)))}
                  >
                    <Icon name="close" size={13} />
                  </button>
                </li>
              {/each}
            </ul>
          {/if}

          <div class="fields">
            <label class="grow">
              <span class="field-label">Title</span>
              <input
                class="field"
                bind:value={title}
                placeholder="What it is, in your words"
                autocomplete="off"
              />
            </label>
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

          {#if problem}<p class="error" role="alert">{problem}</p>{/if}

          <div class="add-foot">
            <button type="button" class="btn" disabled={saving} onclick={cancel}>Cancel</button>
            <button class="btn btn-primary" type="submit" disabled={saving || files.length === 0}>
              {saving ? 'Adding…' : 'Add'}
            </button>
          </div>
        </form>
      {/if}

      {#if documents.length === 0 && !adding}
        <EmptyState lead="No documents yet.">
          {#snippet icon()}<Icon name="paperclip" size={30} weight={1.3} />{/snippet}
          {#snippet note()}
            Lab reports, visit notes, scans and the records in an Apple Health export are kept here,
            sealed in this vault. Drop a PDF or a picture anywhere on this page, or add one.
          {/snippet}
          {#snippet action()}
            <button class="btn btn-primary" onclick={openForm}>Add a document</button>
          {/snippet}
        </EmptyState>
      {:else if documents.length > 0}
        <div class="page">
          {#if problem && !adding}<p class="error" role="alert">{problem}</p>{/if}
          <ul class="docs" aria-label="Documents, newest first">
            {#each documents as doc (doc.id)}
              {@const review = toReview(doc)}
              <li>
                <button class="list-row doc" onclick={() => health.openDocument(doc.id)}>
                  <span class="doc-mark" aria-hidden="true">
                    <Icon name="paperclip" size={16} />
                  </span>
                  <span class="doc-main">
                    <span class="doc-title">{doc.title || 'Untitled document'}</span>
                    <span class="doc-meta">
                      {docKindName(doc.kind)} · {dateText(doc.date)}{#if doc.provider}&nbsp;·
                        {doc.provider}{/if}
                    </span>
                  </span>
                  {#if review > 0}
                    <span class="badge">{plural(review, 'result')} to review</span>
                  {:else if doc.extraction === 'running'}
                    <span class="badge quiet">Reading…</span>
                  {/if}
                </button>
              </li>
            {/each}
          </ul>
        </div>
      {/if}
    </section>
  {:else}
    <HealthLabTrends />
  {/if}
</div>

{#if health.document}
  <HealthDocumentView />
{/if}

<style>
  .records {
    height: 100%;
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .records.away {
    display: none;
  }

  .body {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow-y: auto;
    padding: var(--sp-2) var(--sp-6) var(--fab-clear);
  }
  .body.dropping {
    outline: 2px dashed color-mix(in oklab, var(--accent) 60%, transparent);
    outline-offset: -6px;
  }
  .page {
    width: 100%;
    max-width: 860px;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }

  .docs {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .doc {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-3);
  }
  .doc-mark {
    flex: none;
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    border-radius: 50%;
    background: var(--bg-sunken);
    color: var(--fg-subtle);
  }
  .doc-main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .doc-title {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .doc-meta {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .badge {
    flex: none;
    height: 22px;
    display: inline-flex;
    align-items: center;
    padding: 0 var(--sp-2);
    border-radius: 99px;
    background: color-mix(in oklab, var(--accent) 16%, transparent);
    color: color-mix(in oklab, var(--accent) 72%, var(--fg));
    font-size: var(--text-xs);
    font-weight: 600;
  }
  .badge.quiet {
    background: var(--bg-sunken);
    color: var(--fg-subtle);
  }

  .add {
    width: 100%;
    max-width: 860px;
    margin: 0 auto var(--sp-4);
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg);
  }
  .drop {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--sp-3);
    padding: var(--sp-4);
    border: 1px dashed var(--border-strong);
    border-radius: var(--radius);
    background: var(--bg-sunken);
  }
  .chosen {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
  }
  .chosen li {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .chosen-name {
    min-width: 0;
    flex: 0 1 auto;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg);
  }
  .chosen-size {
    flex: 1;
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }
  .chosen-remove {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .chosen-remove:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .fields {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-3);
  }
  .fields label {
    flex: 0 1 11rem;
    min-width: 0;
  }
  .fields label.grow {
    flex: 1 1 16rem;
  }
  .add-foot {
    display: flex;
    justify-content: flex-end;
    gap: var(--sp-2);
  }
</style>
