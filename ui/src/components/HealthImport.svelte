<script lang="ts">
  // Bringing in the history an iPhone already holds: Apple's own export.
  //
  // Health Auto Export keeps this up to date from now on; the export is how
  // years of readings, every workout and the clinical records in Apple
  // Health come across the first time. The person makes the file on the
  // phone and moves it to this computer, and this picks it -- in a dialog
  // the shell draws, so the path never reaches the page -- and reads it a
  // piece at a time, so a file of several gigabytes needs no more memory
  // than a small one.
  //
  // That is for a window on the vault's own computer. A window anywhere else
  // -- a browser over web access, a desktop paired with a vault elsewhere --
  // has no path to give the vault, so it uploads the file instead: into Files
  // (sealed, Health's), then imported from there and put in the Files trash
  // once it has been read.
  //
  // What is on screen is the store's `importStatus`, which it keeps current
  // while an import runs: how far through the file it is, what has been
  // added so far, and, at the end, the whole report. Nothing is dropped
  // quietly: the kinds of record Karma Yoga does not know are counted and
  // listed. Importing the same file twice is harmless -- a reading already
  // here is recognised and skipped.

  import { untrack } from 'svelte'
  import { files } from '../lib/files.svelte'
  import { health } from '../lib/health.svelte'
  import { dateFormat, humanBytes, plural } from '../lib/format'
  import { app } from '../lib/state.svelte'

  const status = $derived(health.importStatus)
  const running = $derived(status?.state === 'running')
  const report = $derived(status?.report ?? null)

  let starting = $state(false)
  let chooser = $state<HTMLInputElement | null>(null)

  // Whether this window is on the vault's own machine is what the files
  // status says, so it is asked for when this opens, as the assistant's
  // composer does for its paperclip.
  $effect(() => {
    if (app.supportsFiles && files.status === null) void untrack(() => files.refreshStatus())
  })
  /** Not on the vault's machine, and so no picker to use: the file is
   *  uploaded. Until the files status is known the picker is what is offered. */
  const uploading = $derived(app.supportsFiles && files.status !== null && !files.local)

  /** How far through `export.xml` it is, or `null` before the size is known. */
  const percent = $derived(
    status && status.total > 0
      ? Math.min(100, Math.floor((status.read / status.total) * 100))
      : null,
  )

  const skipped = $derived(
    Object.entries(report?.skipped ?? {}).sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0])),
  )
  const skippedTotal = $derived(skipped.reduce((n, [, count]) => n + count, 0))

  const dayFormat = dateFormat({ day: 'numeric', month: 'short', year: 'numeric' })

  const span = $derived.by(() => {
    if (!report?.first || !report.last) return ''
    const first = new Date(report.first)
    const last = new Date(report.last)
    if (Number.isNaN(first.getTime()) || Number.isNaN(last.getTime())) return ''
    return `${dayFormat.format(first)} to ${dayFormat.format(last)}`
  })

  /** `HKQuantityTypeIdentifierDietaryZinc` as "Dietary Zinc". */
  function typeName(raw: string): string {
    return raw
      .replace(/^HK(Quantity|Category|Data|Correlation)Type(Identifier)?/, '')
      .replace(/([a-z])([A-Z])/g, '$1 $2')
      .trim()
  }

  async function start() {
    if (starting || running) return
    // The page's input has to be opened by the press itself.
    if (uploading) return void chooser?.click()
    starting = true
    try {
      await health.importApple()
    } finally {
      starting = false
    }
  }

  async function onUpload(e: Event & { currentTarget: EventTarget & HTMLInputElement }) {
    const file = e.currentTarget.files?.[0]
    e.currentTarget.value = ''
    if (!file || starting || running) return
    starting = true
    try {
      await health.importAppleUpload(file)
    } finally {
      starting = false
    }
  }
</script>

<section class="import" aria-labelledby="import-head">
  <span class="eyebrow" id="import-head">Import an Apple Health export</span>

  <p class="hint">
    Do this once to bring in your history. After that, the iPhone connection keeps things up to
    date. Everything in the export comes across: readings, workouts, and any clinical records, such
    as lab results your providers shared with Health. A reading already here is skipped, so
    importing the same file twice is safe.
  </p>

  <details class="how">
    <summary>How to make the export on your iPhone</summary>
    <ol>
      <li>Open the Health app and tap your picture at the top right.</li>
      <li>
        Scroll down and tap <b>Export All Health Data</b>, then <b>Export</b>. A large history can
        take several minutes to prepare.
      </li>
      <li>
        Send the file somewhere you can reach from this computer: AirDrop, Files, a cloud drive. It
        is called <code>export.zip</code>.
      </li>
      <li>
        {#if uploading}
          Get it onto the computer you are using now, then press the button below and choose it. It
          is uploaded to the vault, imported, and put in the trash in Files.
        {:else}
          Get it onto this computer, then press the button below and choose it.
        {/if}
      </li>
    </ol>
  </details>

  <div class="row">
    <button class="btn btn-outline" disabled={starting || running} onclick={() => void start()}>
      {#if running}
        Importing…
      {:else if uploading}
        {starting ? 'Uploading…' : 'Upload export.zip…'}
      {:else}
        {starting ? 'Choosing…' : 'Choose export.zip…'}
      {/if}
    </button>
    <input
      class="hidden"
      type="file"
      accept=".zip,application/zip"
      tabindex="-1"
      aria-hidden="true"
      bind:this={chooser}
      onchange={(e) => void onUpload(e)}
    />
    {#if running}
      <button class="btn" onclick={() => void health.cancelImport()}>Cancel</button>
    {/if}
  </div>

  {#if status && status.state !== 'idle'}
    {#if running}
      <div class="progress">
        <div
          class="bar"
          role="progressbar"
          aria-label="Reading {status.fileName || 'the export'}"
          aria-valuemin="0"
          aria-valuemax="100"
          aria-valuenow={percent ?? undefined}
        >
          <div
            class="fill"
            class:unknown={percent === null}
            style:width={percent === null ? undefined : `${percent}%`}
          ></div>
        </div>
        <p class="hint" role="status">
          {#if percent !== null}
            Reading {status.fileName || 'the export'}: {percent}% ({humanBytes(status.read)} of {humanBytes(
              status.total,
            )}).
          {:else}
            Opening {status.fileName || 'the export'}…
          {/if}
          You can keep using the app.
        </p>
      </div>
    {:else if status.state === 'failed'}
      <p class="error" role="alert">
        The import stopped: {status.error || 'it could not be read.'}
      </p>
    {:else if status.state === 'cancelled'}
      <p class="hint" role="status">
        Cancelled. What was read before it stopped is kept, and importing the same file again adds
        only what is missing.
      </p>
    {:else if status.state === 'done'}
      <p class="done" role="status">
        Imported{status.fileName ? ` ${status.fileName}` : ''}{span ? `, covering ${span}` : ''}.
      </p>
    {/if}

    {#if report}
      <dl class="report" aria-label={running ? 'So far' : 'What came in'}>
        <div>
          <dt>Readings added</dt>
          <dd>{report.samplesAdded.toLocaleString()}</dd>
        </div>
        <div>
          <dt>Already here</dt>
          <dd>{report.samplesDuplicate.toLocaleString()}</dd>
        </div>
        {#if report.samplesRejected > 0}
          <div>
            <dt>Not usable</dt>
            <dd>{report.samplesRejected.toLocaleString()}</dd>
          </div>
        {/if}
        <div>
          <dt>Workouts</dt>
          <dd>
            {report.workoutsAdded.toLocaleString()}
            {#if report.workoutsUpdated > 0}
              <span class="more">+ {report.workoutsUpdated.toLocaleString()} updated</span>
            {/if}
          </dd>
        </div>
        {#if report.dosesAdded > 0 || report.dosesUnmatched > 0}
          <div>
            <dt>Doses</dt>
            <dd>
              {report.dosesAdded.toLocaleString()}
              {#if report.dosesUnmatched > 0}
                <span class="more"
                  >{report.dosesUnmatched.toLocaleString()} not on your medication list</span
                >
              {/if}
            </dd>
          </div>
        {/if}
        {#if report.documentsAdded > 0}
          <div>
            <dt>Documents</dt>
            <dd>
              {report.documentsAdded.toLocaleString()}
              <span class="more">lab results waiting for you under Records</span>
            </dd>
          </div>
        {/if}
      </dl>
    {/if}

    {#if skipped.length > 0}
      <details class="skipped">
        <summary>
          {plural(skipped.length, 'kind')} of record skipped ({skippedTotal.toLocaleString()} in all)
        </summary>
        <p class="hint">
          Karma Yoga doesn’t know these yet, so they were counted and left out, not lost: they are
          still in the export file.
        </p>
        <ul>
          {#each skipped as [name, count] (name)}
            <li>
              <span class="skip-name" title={name}>{typeName(name) || name}</span>
              <span class="skip-count">{count.toLocaleString()}</span>
            </li>
          {/each}
        </ul>
      </details>
    {/if}
  {/if}
</section>

<style>
  .import {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }
  .hint {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
    overflow-wrap: anywhere;
  }
  .error {
    margin: 0;
  }
  .done {
    margin: 0;
    font-size: var(--text-base);
    font-weight: 550;
  }
  .hidden {
    display: none;
  }
  .row {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }

  .how summary,
  .skipped summary {
    cursor: pointer;
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--accent);
  }
  .how ol {
    margin: var(--sp-2) 0 0;
    padding-left: 1.4em;
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-muted);
  }
  .how b {
    font-weight: 600;
    color: var(--fg);
  }
  code {
    padding: 1px 4px;
    border-radius: 4px;
    background: var(--bg-sunken);
    color: var(--fg-muted);
    font-size: var(--text-xs);
  }

  .progress {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .bar {
    height: 6px;
    border-radius: 3px;
    background: var(--bg-sunken);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    border-radius: 3px;
    background: var(--accent);
    transition: width var(--fast) var(--ease);
  }
  /* Size not known yet: a short bar that slides, so it does not read as 0%. */
  .fill.unknown {
    width: 30%;
    animation: slide 1.4s ease-in-out infinite;
  }
  @keyframes slide {
    from {
      transform: translateX(-100%);
    }
    to {
      transform: translateX(340%);
    }
  }

  .report {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-3) var(--sp-6);
    margin: 0;
  }
  .report div {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  dt {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  dd {
    margin: 0;
    font-size: var(--text-md);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .more {
    font-size: var(--text-xs);
    font-weight: 400;
    color: var(--fg-subtle);
  }

  .skipped ul {
    margin: var(--sp-2) 0 0;
    padding: 0;
    list-style: none;
    max-height: 16em;
    overflow-y: auto;
  }
  .skipped li {
    display: flex;
    justify-content: space-between;
    gap: var(--sp-3);
    padding: var(--sp-1) 0;
    border-top: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
    font-size: var(--text-sm);
  }
  .skipped .hint {
    margin-top: var(--sp-2);
  }
  .skip-name {
    min-width: 0;
    overflow-wrap: anywhere;
    color: var(--fg-muted);
  }
  .skip-count {
    flex: none;
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }

  /* A phone: the two disclosures are a finger tall, and the button that
     chooses the export (with Cancel beside it while it runs) takes the
     width. */
  :global([data-phone]) .how summary,
  :global([data-phone]) .skipped summary {
    display: flex;
    align-items: center;
    min-height: 44px;
  }
  :global([data-phone]) .row .btn {
    flex: 1 1 10em;
  }
  :global([data-phone]) .skipped li {
    padding-block: var(--sp-2);
  }
</style>
