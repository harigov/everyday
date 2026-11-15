<script lang="ts">
  // Settings → Files: where the folder is and whether it can be reached, what
  // happens to files other apps add, how long the trash keeps things, what the
  // assistant and the network may be told, and how much is kept.
  //
  // The folder is the person's -- ordinary files, in an ordinary place, that
  // other software can use -- so this says where it is in full, says plainly
  // when it is not there, and offers no way to move files around inside it:
  // moving the whole folder is a file manager's job, and pointing Settings at
  // the new place is all it takes (paths in the catalog are relative).
  //
  // Everything here but the folder saves as it is changed. The two things that
  // take a while -- looking at the folder again and bringing old pictures in --
  // are buttons that say what they are doing and cannot be pressed twice.
  //
  // Choosing a folder with the system picker is for a window on the computer
  // that holds the vault (`files.local`): anywhere else the picker would offer
  // the wrong machine's folders. A browser or a paired desktop sets the
  // suggested place up and leaves the rest to the vault's own computer.

  import { files } from '../lib/files.svelte'
  import { formatSize } from '../lib/files'
  import { plural, relativeTime } from '../lib/format'
  import type { FileKind, FileOrigin, FilesSettings, ScanReport } from '../lib/types'
  import Icon from './Icon.svelte'

  // The panel needs the status and nothing else, whether or not Files was
  // ever opened.
  void files.refreshStatus()

  const status = $derived(files.status)
  const settings = $derived(files.settings)
  const folder = $derived(files.folderState)
  const usage = $derived(status?.usage ?? null)
  const report = $derived(status?.lastReport ?? null)

  /** The apps whose files can be kept encrypted, in the order they are drawn.
   *  `found` is not one: those are put in the folder by something else. */
  const ORIGINS: { id: Exclude<FileOrigin, 'found'>; label: string; words: string }[] = [
    { id: 'files', label: 'The Files app', words: 'Files added here, by hand.' },
    { id: 'journal', label: 'Journal', words: 'Pictures and films in entries.' },
    { id: 'notes', label: 'Notes', words: 'Pictures and films in notes.' },
    { id: 'assistant', label: 'Assistant', words: 'Files it is handed or makes.' },
    { id: 'chat', label: 'Chat', words: 'Files sent to it in Matrix.' },
    { id: 'mail', label: 'Mail', words: 'Attachments saved from messages.' },
    { id: 'jobs', label: 'Background jobs', words: 'Files a job brings back.' },
  ]

  const KINDS: Record<FileKind, string> = {
    image: 'Pictures',
    video: 'Films',
    audio: 'Recordings',
    document: 'Documents',
    other: 'Other',
  }

  const TRASH: { days: number; label: string }[] = [
    { days: 7, label: '7 days' },
    { days: 14, label: '14 days' },
    { days: 30, label: '30 days' },
    { days: 60, label: '60 days' },
    { days: 90, label: '90 days' },
    { days: 0, label: 'Until I empty it' },
  ]
  const trashOptions = $derived(
    settings && !TRASH.some((t) => t.days === settings.trashDays)
      ? [{ days: settings.trashDays, label: `${settings.trashDays} days` }, ...TRASH]
      : TRASH,
  )

  function save(patch: Partial<FilesSettings>) {
    if (!settings) return
    void files.saveSettings({ ...$state.snapshot(settings), ...patch })
  }

  function setSealed(origin: FileOrigin, on: boolean) {
    if (!settings) return
    const rest = settings.encrypted.filter((o) => o !== origin)
    save({ encrypted: on ? [...rest, origin] : rest })
  }

  /** What a scan found, in a line. */
  function summary(r: ScanReport): string {
    return [
      r.added > 0 ? `${r.added} new` : '',
      r.changed > 0 ? `${r.changed} changed` : '',
      r.moved > 0 ? `${r.moved} moved` : '',
      r.returned > 0 ? `${r.returned} came back` : '',
      r.missing > 0 ? `${r.missing} missing` : '',
      `${r.unchanged.toLocaleString()} unchanged`,
    ]
      .filter(Boolean)
      .join(', ')
  }

  const scanning = $derived(files.busy || status?.scanning === true)
  const reachable = $derived(folder?.state === 'available')
  const legacy = $derived(status?.legacy ?? null)
  const mostBytes = $derived(Math.max(1, ...(usage?.byKind.map((k) => k.bytes) ?? [])))
  const kinds = $derived(
    [...(usage?.byKind ?? [])].filter((k) => k.files > 0).sort((a, b) => b.bytes - a.bytes),
  )
</script>

<div class="panes">
  {#if !files.supported}
    <section>
      <span class="eyebrow">Files</span>
      <p class="hint">This vault cannot hold a catalog of files.</p>
    </section>
  {:else if !status || !settings}
    <section>
      <span class="eyebrow">Folder</span>
      <p class="hint">{files.error ?? 'Looking at the folder…'}</p>
    </section>
  {:else}
    <section aria-labelledby="fs-folder">
      <span class="eyebrow" id="fs-folder">Folder</span>

      {#if folder?.state === 'notSetUp'}
        <p class="hint">
          No folder yet. Files are kept as ordinary files in a folder you can see, so a photo
          viewer, a file manager or a sync tool can use them where they lie. Suggested:
          <code>{folder.suggested}</code>.
        </p>
        <div class="buttons">
          <button
            class="btn btn-primary"
            disabled={files.busy}
            onclick={() => void files.setUpFolder()}
          >
            Use this folder
          </button>
          {#if files.local}
            <button
              class="btn btn-outline"
              disabled={files.busy}
              onclick={() => void files.chooseFolder()}
            >
              Choose another…
            </button>
          {/if}
        </div>
      {:else if folder}
        <div class="fact">
          <span class="fact-label">Where it is</span>
          <code class="path">{folder.path}</code>
        </div>
        {#if folder.state === 'available'}
          <p class="state good"><Icon name="tick" size={14} /> Available</p>
        {:else if folder.state === 'unavailable'}
          <p class="state bad">
            <Icon name="alert" size={14} /> Not available: {folder.reason}
          </p>
          <p class="hint">
            Nothing is changed while it is away: no file is marked missing and nothing is cleared
            out. What you added to your files is kept in the vault.
          </p>
        {/if}
        <p class="hint">
          To move the folder, move it with your file manager and then point this at its new place:
          the catalog keeps paths relative to it.
        </p>
        <div class="buttons">
          {#if files.local}
            <button
              class="btn btn-outline"
              disabled={files.busy}
              onclick={() => void files.chooseFolder()}
            >
              Choose another…
            </button>
          {/if}
          {#if folder.state === 'unavailable'}
            <button class="btn btn-outline" onclick={() => void files.reload()}>Try again</button>
          {/if}
        </div>

        <div class="row-setting scan">
          <span>
            <b>Look at the folder again</b>
            <small>
              It is checked as things change in it and every five minutes. This does it now, for
              something a sync tool just brought in.
            </small>
          </span>
          <button
            class="btn btn-outline"
            disabled={scanning || !reachable}
            onclick={() => void files.rescan()}
          >
            {scanning ? 'Looking…' : 'Rescan now'}
          </button>
        </div>
        {#if status.lastScan}
          <p class="hint">
            Last looked at {relativeTime(status.lastScan)}{report ? `: ${summary(report)}.` : '.'}
          </p>
        {/if}
        {#if report?.heldBack}
          <p class="error">
            So many files vanished at once that none were marked missing. That looks like a drive
            that is not mounted rather than files that were deleted. Check the folder, then rescan.
          </p>
        {/if}
        {#if report?.errors && report.errors.length > 0}
          <details class="problems">
            <summary>{plural(report.errors.length, 'file')} could not be read</summary>
            <ul>
              {#each report.errors.slice(0, 20) as problem, i (i)}<li>{problem}</li>{/each}
            </ul>
          </details>
        {/if}
      {/if}
    </section>

    <section aria-labelledby="fs-new">
      <span class="eyebrow" id="fs-new">New files</span>

      <div class="fact">
        <span class="fact-label">Keep these encrypted</span>
        <p class="hint">
          Files from the apps ticked here are kept encrypted inside the vault, readable only by
          Karma Yoga, instead of in the folder. Other programs and sync tools cannot see them. Any
          file can be switched either way later, from its own details.
        </p>
      </div>
      {#each ORIGINS as o (o.id)}
        <label class="toggle">
          <input
            type="checkbox"
            checked={settings.encrypted.includes(o.id)}
            onchange={(e) => setSealed(o.id, e.currentTarget.checked)}
          />
          <span>
            <b>{o.label}</b>
            <small>{o.words}</small>
          </span>
        </label>
      {/each}

      <label class="row-setting">
        <span>
          <b>Keep the trash for</b>
          <small>
            A file moved to the trash waits there, and can be put back, until then. Nothing in the
            folder is ever deleted any other way.
          </small>
        </span>
        <select
          value={settings.trashDays}
          onchange={(e) => save({ trashDays: Number(e.currentTarget.value) })}
        >
          {#each trashOptions as t (t.days)}
            <option value={t.days}>{t.label}</option>
          {/each}
        </select>
      </label>
    </section>

    <section aria-labelledby="fs-privacy">
      <span class="eyebrow" id="fs-privacy">Privacy</span>

      <label class="toggle">
        <input
          type="checkbox"
          checked={settings.assistantAccess}
          onchange={(e) => save({ assistantAccess: e.currentTarget.checked })}
        />
        <span>
          <b>Let the assistant find and read your files</b>
          <small>
            It can look for files, read them and organise them. What you hand it yourself, in the
            message box, it always sees. Reading a file counts as reading mail: for the rest of that
            turn, fetching a web page asks you first.
          </small>
        </span>
      </label>

      <label class="toggle">
        <input
          type="checkbox"
          checked={settings.namePlaces}
          onchange={(e) => save({ namePlaces: e.currentTarget.checked })}
        />
        <span>
          <b>Name the places photos were taken</b>
          <small>
            Sends a photo’s coordinates -- not the photo -- to OpenStreetMap to find the town or
            city. That puts where you were on the network, so it is off until you switch it on.
          </small>
        </span>
      </label>
    </section>

    <section aria-labelledby="fs-old">
      <span class="eyebrow" id="fs-old">Old pictures</span>
      {#if legacy && legacy.files > 0}
        <div class="row-setting">
          <span>
            <b>Bring old pictures into Files</b>
            <small>
              {plural(legacy.files, 'picture or film', 'pictures and films')}
              ({formatSize(legacy.bytes)}) are still kept inside
              {[
                legacy.entries > 0
                  ? plural(legacy.entries, 'journal entry', 'journal entries')
                  : '',
                legacy.notes > 0 ? plural(legacy.notes, 'note') : '',
                legacy.messages > 0 ? plural(legacy.messages, 'assistant message') : '',
              ]
                .filter(Boolean)
                .join(', ')}. Bringing them in puts each in the folder -- or encrypts it, for an app
              ticked above -- and points where it was used at it. Nothing is lost.
            </small>
          </span>
          <button
            class="btn btn-outline"
            disabled={files.busy || !reachable}
            onclick={() => void files.bringInAttachments()}
          >
            {files.busy ? 'Working…' : 'Bring them in'}
          </button>
        </div>
      {:else}
        <p class="hint">
          Nothing to bring in. Pictures and films added from now on are kept in Files.
        </p>
      {/if}
    </section>

    <section aria-labelledby="fs-storage">
      <span class="eyebrow" id="fs-storage">Storage</span>
      {#if usage}
        <p class="total">
          {formatSize(usage.bytes)}
          <span class="faint">in {plural(usage.files, 'file')}</span>
        </p>
        {#if kinds.length > 0}
          <ul class="kinds">
            {#each kinds as k (k.kind)}
              <li>
                <span class="kind">{KINDS[k.kind]}</span>
                <span
                  class="bar"
                  role="img"
                  aria-label="{formatSize(k.bytes)} of {formatSize(usage.bytes)}"
                >
                  <span class="fill" style:width="{Math.max(2, (k.bytes / mostBytes) * 100)}%"
                  ></span>
                </span>
                <span class="size">{formatSize(k.bytes)}</span>
                <span class="n">{k.files.toLocaleString()}</span>
              </li>
            {/each}
          </ul>
        {/if}
        <p class="hint">
          {#if usage.sealedFiles > 0}
            {plural(usage.sealedFiles, 'file')} ({formatSize(usage.sealedBytes)}) kept encrypted
            inside the vault.
          {/if}
          {#if usage.trashedFiles > 0}
            {plural(usage.trashedFiles, 'file')} ({formatSize(usage.trashedBytes)}) in the trash.
          {/if}
          {#if usage.missingFiles > 0}
            {plural(usage.missingFiles, 'file')} missing from the folder.
          {/if}
        </p>
      {/if}
    </section>
  {/if}
</div>

<style>
  .panes {
    display: flex;
    flex-direction: column;
    gap: var(--sp-8);
    min-width: 0;
  }
  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
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
  code {
    padding: 1px 4px;
    border-radius: 4px;
    background: var(--bg-sunken);
    color: var(--fg-muted);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
    user-select: text;
  }
  .path {
    align-self: flex-start;
    padding: var(--sp-1) var(--sp-2);
  }
  .faint {
    color: var(--fg-faint);
  }

  .toggle {
    display: flex;
    gap: var(--sp-3);
    align-items: flex-start;
    min-width: 0;
    cursor: pointer;
  }
  .toggle input {
    flex: none;
    margin-top: 3px;
    accent-color: var(--accent);
  }
  .toggle > span,
  .row-setting > span:first-child {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  b {
    font-size: var(--text-base);
    font-weight: 600;
  }
  small {
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }

  .fact {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    min-width: 0;
  }
  .fact-label {
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-muted);
  }

  .row-setting {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-4);
    min-width: 0;
  }
  .row-setting.scan {
    padding-top: var(--sp-3);
    border-top: 1px solid var(--border);
  }
  .row-setting .btn,
  .row-setting select {
    flex: none;
  }
  .buttons {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }

  select {
    min-width: 0;
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-base);
  }
  select:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 16%, transparent);
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
  .state.good {
    color: var(--fg);
    font-weight: 550;
  }
  .state.bad {
    color: var(--danger);
    font-weight: 550;
  }

  .problems {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .problems summary {
    cursor: pointer;
  }
  .problems ul {
    margin: var(--sp-2) 0 0;
    padding-left: var(--sp-5);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
    user-select: text;
  }

  .total {
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 650;
    letter-spacing: -0.01em;
    font-variant-numeric: tabular-nums;
  }
  .total .faint {
    font-size: var(--text-sm);
    font-weight: 400;
  }
  /* One hue, the accent: the length is the figure, and the kind is its label. */
  .kinds {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .kinds li {
    display: grid;
    grid-template-columns: 6.5em minmax(0, 1fr) 5.5em 4em;
    align-items: center;
    gap: var(--sp-3);
    font-size: var(--text-sm);
  }
  .kind {
    color: var(--fg-muted);
  }
  .bar {
    height: 8px;
    border-radius: 4px;
    background: var(--bg-sunken);
    overflow: hidden;
  }
  .fill {
    display: block;
    height: 100%;
    border-radius: 4px;
    background: var(--accent);
  }
  .size,
  .n {
    text-align: right;
    font-variant-numeric: tabular-nums;
    color: var(--fg);
  }
  .n {
    color: var(--fg-faint);
  }
</style>
