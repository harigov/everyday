<script lang="ts">
  // Settings → Data: leaving with your writing, and coming back with it.
  //
  // The pane is deliberately two halves with a rule between them, and they
  // are not symmetrical. Export is one press away: it is a read, it cannot
  // lose anything, and asking somebody to confirm a copy of their own diary
  // would be theatre. Import is three steps -- choose a file, be told what is
  // in it, then decide -- because `replace` overwrites records somebody
  // wrote, and a dialog that only says "are you sure?" is a dialog that gets
  // clicked through.
  //
  // Neither half knows what an app is. The list of things to tick comes from
  // `list_parts`, which every app answers for itself; adding a sixth app puts
  // a row here without this file being opened.

  import { dateFormat, humanBytes, plural } from '../lib/format'
  import { hasShell, transfer } from '../lib/transfer.svelte'
  import { app } from '../lib/state.svelte'
  import Icon from './Icon.svelte'

  let picker = $state<HTMLInputElement | null>(null)

  // Loaded once when the tab is first opened rather than on every mount:
  // `list_parts` counts records in every app, which is a handful of queries
  // and no reason to run them again for somebody switching tabs.
  if (transfer.parts === null) void transfer.load()

  const parts = $derived(transfer.parts ?? [])
  const chosen = $derived(parts.filter((p) => transfer.chosen.has(p.id)))
  const records = $derived(chosen.reduce((sum, p) => sum + p.records, 0))
  const anyMedia = $derived(chosen.some((p) => p.media))
  const mediaBytes = $derived(app.status?.stats?.blobBytes ?? 0)
  const stage = $derived(transfer.stage)
  const incoming = $derived(transfer.incoming)

  function offer(e: Event) {
    const file = (e.currentTarget as HTMLInputElement).files?.[0]
    if (file) void transfer.offerFile(file)
    // Cleared so that choosing the same file twice in a row still fires.
    ;(e.currentTarget as HTMLInputElement).value = ''
  }

  function startImport() {
    if (hasShell()) void transfer.chooseFile()
    else picker?.click()
  }

  /** `12 entries` — the count with the word an app would use for it. */
  function count(n: number, label: string): string {
    return `${n.toLocaleString()} ${label.toLowerCase()}`
  }
</script>

<section>
  <span class="eyebrow">Export</span>
  <p class="hint">
    A zip of plain files: your journal and notes as Markdown, your calendars as
    <code>.ics</code>, your shelves and your tracking as CSV. Nothing in it needs Every Day to open,
    and nothing in it is encrypted — keep it where you would keep the diary itself.
  </p>

  {#if transfer.parts === null}
    <p class="hint">Counting…</p>
  {:else}
    <ul class="parts">
      {#each parts as part (part.id)}
        <li>
          <label class="tick">
            <input
              type="checkbox"
              checked={transfer.chosen.has(part.id)}
              onchange={() => transfer.toggle(part.id)}
            />
            <span>
              <b>
                {part.label}
                <small class="records">{part.records.toLocaleString()}</small>
              </b>
              <small>{part.summary}</small>
              <small class="format">{part.format}</small>
            </span>
          </label>
        </li>
      {/each}
    </ul>

    {#if anyMedia}
      <label class="toggle">
        <input
          type="checkbox"
          checked={transfer.media}
          onchange={(e) => (transfer.media = e.currentTarget.checked)}
        />
        <span>
          <b>Include photographs, video and cover art</b>
          <small>
            {#if mediaBytes > 0}
              About {humanBytes(mediaBytes)} of attachments. Without them you get the words and not the
              pictures.
            {:else}
              Without them you get the words and not the pictures.
            {/if}
          </small>
        </span>
      </label>
    {/if}

    <div class="row">
      <button
        class="btn btn-primary"
        disabled={transfer.busy || chosen.length === 0}
        onclick={() => transfer.exportNow()}
      >
        Export {records ? count(records, 'records') : 'nothing'}…
      </button>
    </div>
  {/if}
</section>

<hr />

<section>
  <span class="eyebrow">Import</span>
  <p class="hint">
    An archive Every Day exported, or a folder of Markdown zipped up by anything else. You will be
    told what is in it before anything is read.
  </p>

  <!-- The browser's own picker, for a window with no shell behind it. Hidden
       rather than absent: it is what the button presses when there is no
       platform dialog to open. -->
  <input
    class="hidden"
    type="file"
    accept=".zip,application/zip"
    bind:this={picker}
    onchange={offer}
  />

  {#if incoming}
    <div class="found">
      <p class="file"><Icon name="upload" size={14} /> {incoming.name}</p>
      {#if incoming.manifest.vault}
        <p class="hint">
          Exported from <b>{incoming.manifest.vault}</b>
          {#if incoming.manifest.exportedAt}
            on {dateFormat({}).format(new Date(incoming.manifest.exportedAt))}
          {/if}.
        </p>
      {/if}

      <ul class="parts">
        {#each incoming.manifest.parts as part (part.id)}
          <li>
            <label class="tick" class:off={!part.imports}>
              <input
                type="checkbox"
                disabled={!part.imports}
                checked={transfer.importing.has(part.id)}
                onchange={() => transfer.toggleImport(part.id)}
              />
              <span>
                <b>
                  {part.label}
                  <small class="records">
                    {part.records
                      ? part.records.toLocaleString()
                      : plural(part.files, 'file', 'files')}
                  </small>
                </b>
                <small>
                  {part.imports ? part.format : 'This one is a reading copy and is not read back.'}
                </small>
              </span>
            </label>
          </li>
        {/each}
      </ul>

      <div class="segmented">
        <button
          class="seg"
          class:on={transfer.mode === 'skip'}
          onclick={() => (transfer.mode = 'skip')}
        >
          Only add what is missing
        </button>
        <button
          class="seg"
          class:on={transfer.mode === 'replace'}
          onclick={() => (transfer.mode = 'replace')}
        >
          Replace what is here
        </button>
      </div>
      <p class="hint">
        {#if transfer.mode === 'skip'}
          Anything this vault already has is left exactly as it is. Nothing you have written can be
          lost.
        {:else}
          A record this vault already has is overwritten from the file. This is what you want after
          editing an export in a text editor, and it will replace what is here.
        {/if}
      </p>

      <div class="row">
        <button class="btn" disabled={transfer.busy} onclick={() => transfer.cancelImport()}>
          Cancel
        </button>
        <button
          class="btn"
          class:btn-primary={transfer.mode === 'skip'}
          class:btn-danger={transfer.mode === 'replace'}
          disabled={transfer.busy || transfer.importing.size === 0}
          onclick={() => transfer.importNow()}
        >
          {transfer.mode === 'replace' ? 'Replace and import' : 'Import'}
        </button>
      </div>
    </div>
  {:else}
    <div class="row">
      <button class="btn" disabled={transfer.busy} onclick={startImport}>Choose a file…</button>
    </div>
  {/if}

  {#if transfer.result}
    <div class="report">
      <p>
        <b>
          {transfer.result.added.toLocaleString()} added,
          {transfer.result.replaced.toLocaleString()} replaced,
          {transfer.result.skipped.toLocaleString()} left alone.
        </b>
      </p>
      <!-- One unreadable file in four hundred should not have cost the other
           three hundred and ninety-nine, and it must not pass unmentioned
           either. Both halves of that are here. -->
      {#each transfer.result.reports.filter((r) => r.problems.length) as report (report.part)}
        <p class="problem">
          <b>{report.part}</b>
          {#each report.problems.slice(0, 5) as problem (problem)}
            <small>{problem}</small>
          {/each}
          {#if report.problems.length > 5}
            <small>…and {report.problems.length - 5} more.</small>
          {/if}
        </p>
      {/each}
    </div>
  {/if}
</section>

{#if stage.at === 'working'}
  <p class="working">
    {stage.what}…
    {#if stage.total > 0}
      <span>{humanBytes(stage.done)} of {humanBytes(stage.total)}</span>
    {/if}
  </p>
{:else if stage.at === 'failed'}
  <p class="notice">{stage.message}</p>
{:else if transfer.outcome}
  <p class="notice good">{transfer.outcome}</p>
{/if}

<hr />

<section>
  <span class="eyebrow">This is not a backup</span>
  <p class="hint">
    An export is your writing in a form anything can read, which means it is your writing with the
    encryption taken off. A <i>backup</i> is the vault copied as it is, still sealed, still opening
    with the same password — and it keeps the things an export deliberately leaves out: your
    assistant's key, the devices you have paired, and the key the vault itself is locked with. That
    is <code>everyday backup</code> on the command line.
  </p>
</section>

<style>
  hr {
    margin: var(--sp-5) 0;
    border: 0;
    border-top: 1px solid var(--border);
  }

  .parts {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 0 0 var(--sp-3);
    padding: 0;
    list-style: none;
  }

  .tick {
    display: flex;
    gap: var(--sp-3);
    align-items: start;
    padding: var(--sp-2) var(--sp-2);
    border-radius: var(--radius-sm);
    cursor: pointer;
  }
  .tick:hover {
    background: var(--bg-hover);
  }
  .tick.off {
    cursor: default;
    opacity: 0.6;
  }
  .tick input {
    margin-top: 3px;
  }
  .tick span {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .tick b {
    display: flex;
    gap: var(--sp-2);
    align-items: baseline;
    font-size: var(--text-sm);
    font-weight: 600;
  }
  .tick small {
    color: var(--fg-subtle);
    font-size: var(--text-xs);
    line-height: 1.4;
  }
  /* The count reads as a fact about the row rather than as part of its name,
     which is what stops "Journal 412" being read as a title. */
  .records {
    padding: 0 6px;
    border-radius: 999px;
    background: var(--bg-hover);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
    font-weight: 500;
  }
  .format {
    font-style: italic;
  }

  .found {
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-panel);
  }
  .file {
    display: flex;
    gap: var(--sp-2);
    align-items: center;
    margin: 0 0 var(--sp-2);
    font-size: var(--text-sm);
    font-weight: 600;
  }

  .hidden {
    display: none;
  }

  .working {
    display: flex;
    gap: var(--sp-2);
    align-items: baseline;
    margin: var(--sp-3) 0 0;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .working span {
    font-variant-numeric: tabular-nums;
  }

  .report {
    margin-top: var(--sp-3);
    font-size: var(--text-sm);
  }
  .report p {
    margin: 0 0 var(--sp-2);
  }
  .problem {
    display: flex;
    flex-direction: column;
    gap: 2px;
    color: var(--danger);
  }
  .problem small {
    font-size: var(--text-xs);
  }

  .good {
    color: var(--fg-subtle);
  }

  code {
    font-size: 0.92em;
  }
</style>
