<script lang="ts">
  // The Files app's main pane: the toolbar over whichever view the sidebar has
  // chosen, and the file in the rail beside it when one is selected.
  //
  // Photos and Albums are drawn by their own components, under the same
  // toolbar. Every other view is the same two things -- a grid or a list of
  // what the store loaded -- and says what to do about it when it is empty.
  //
  // Before any of that, whether there is a folder to look at. A first run has
  // none yet, and says what the folder is and where it is suggested to be; a
  // folder that is not there at the moment -- a drive not mounted, a volume
  // locked -- is said to be away and not empty, because nothing is done to
  // the catalog while it is: nothing is marked missing, nothing is swept, and
  // everything the person added is as it was.
  //
  // Files dragged in from the desktop are added to the folder on screen. The
  // desktop window has no drop of paths of its own, so a drop arrives as
  // `File`s and goes up in chunks like any upload; what a drop of files
  // already in the catalog does is the grid's and the sidebar's, onto a
  // folder.

  import { formatSize } from '../lib/files'
  import { files } from '../lib/files.svelte'
  import { dragIsFromOutside, droppedFiles, folderName } from '../lib/files-ui.svelte'
  import { plural } from '../lib/format'
  import { panels } from '../lib/panels.svelte'
  import AlbumsView from './AlbumsView.svelte'
  import EmptyState from './EmptyState.svelte'
  import FileInspector from './FileInspector.svelte'
  import FileViewer from './FileViewer.svelte'
  import FilesGrid from './FilesGrid.svelte'
  import FilesList from './FilesList.svelte'
  import FilesToolbar from './FilesToolbar.svelte'
  import Icon from './Icon.svelte'
  import PhotosView from './PhotosView.svelte'

  // Loaded when the view appears rather than when the store is imported, so a
  // vault whose owner never opens Files never reads a folder. `ready` is the
  // view's own: the lists are empty before they load as well as after, and
  // "this folder is empty" for the length of one read is a flash of the wrong
  // sentence.
  let ready = $state(false)
  void files.start().finally(() => (ready = true))

  const view = $derived(files.view)
  const folder = $derived(files.folderState)
  const own = $derived(view !== 'photos' && view !== 'albums')

  // The rail stays while anything is selected or described, empty for the
  // moment between one file and the next, so choosing another does not
  // resize the grid beside it each time.
  const railed = $derived(files.selection.length > 0 || files.focused !== null)
  const several = $derived(files.selection.length > 1)
  const chosen = $derived(files.selectedFiles)
  const chosenBytes = $derived(chosen.reduce((sum, f) => sum + f.size, 0))

  // ── The viewer, for the views this draws ───────────────────────────

  const list = $derived(files.visibleFiles)
  const viewing = $derived(
    own && files.viewing !== null ? list.findIndex((f) => f.id === files.viewing) : -1,
  )

  // ── Nothing to show ────────────────────────────────────────────────

  const listed = $derived(view === 'browse' ? files.listing !== null : files.page !== null)
  const empty = $derived(
    listed &&
      list.length === 0 &&
      (view !== 'browse' || (files.listing?.folders.length ?? 0) === 0),
  )
  const searching = $derived(files.search.trim() !== '')

  const EMPTY: Record<string, { lead: string; note: string }> = {
    browse: {
      lead: 'This folder is empty.',
      note: 'Add files with the button, or drag them in from your desktop. They are kept as ordinary files in the folder, so other programs can use them too.',
    },
    recent: {
      lead: 'Nothing has been added yet.',
      note: 'The files you add, or that turn up in the folder, are listed here newest first.',
    },
    favourites: {
      lead: 'No favourites yet.',
      note: 'Open a file and press the star on it, and it is kept here.',
    },
    encrypted: {
      lead: 'Nothing is encrypted.',
      note: 'A file you encrypt, and the files of an app set to keep them encrypted in Settings → Files, are kept inside the vault, and listed here. Only Karma Yoga can open them.',
    },
    unused: {
      lead: 'Every file is in use.',
      note: 'A file that no journal entry, note or conversation refers to is listed here -- a way to find what could be cleared out. Nothing is ever deleted for you.',
    },
    missing: {
      lead: 'Nothing is missing.',
      note: 'A file that was in the folder and is not any more is listed here. What you added to it is kept until it comes back or you forget it.',
    },
    trash: {
      lead: 'The trash is empty.',
      note: 'Files you move to the trash wait here, and can be put back.',
    },
  }

  // ── Dropping files in ──────────────────────────────────────────────

  /** What is dragged over, as enters minus leaves: the pointer crossing the
   *  panes inside this one fires both. */
  let depth = $state(0)
  const dropping = $derived(depth > 0)
  const takesFiles = $derived(view !== 'trash' && view !== 'missing' && view !== 'albums')
  const into = $derived(view === 'browse' ? files.dir : undefined)
  const where = $derived(
    view === 'browse' ? (files.dir ? folderName(files.dir) : 'Files') : 'your files',
  )

  function enter(e: DragEvent) {
    if (!takesFiles || !dragIsFromOutside(e)) return
    e.preventDefault()
    depth += 1
  }
  function over(e: DragEvent) {
    if (!takesFiles || !dragIsFromOutside(e)) return
    e.preventDefault()
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy'
  }
  function leave(e: DragEvent) {
    if (!takesFiles || !dragIsFromOutside(e)) return
    depth = Math.max(0, depth - 1)
  }
  function drop(e: DragEvent) {
    if (!takesFiles || !e.dataTransfer || !dragIsFromOutside(e)) return
    e.preventDefault()
    const dropped = droppedFiles(e.dataTransfer)
    if (dropped.length === 0) return
    void files.uploadFiles(dropped, {
      dir: into,
      encrypted: view === 'encrypted' ? true : undefined,
    })
  }
</script>

<main class="files" style:--tint={files.accent}>
  {#if !files.supported}
    <EmptyState lead="Files are not kept in this vault.">
      {#snippet icon()}<Icon name="folder" size={30} weight={1.3} />{/snippet}
      {#snippet note()}
        This vault cannot hold a catalog of files. Open one on a backend that can, or update the
        application.
      {/snippet}
    </EmptyState>
  {:else if !ready || (!files.status && !files.error)}
    <EmptyState lead="Looking at your files…" />
  {:else if !files.status}
    <EmptyState lead="Files could not be read.">
      {#snippet icon()}<Icon name="alert" size={30} weight={1.3} />{/snippet}
      {#snippet note()}{files.error}{/snippet}
      {#snippet action()}
        <button class="btn" onclick={() => void files.reload()}>Try again</button>
      {/snippet}
    </EmptyState>
  {:else if folder?.state === 'notSetUp'}
    <EmptyState lead="Choose where your files live.">
      {#snippet icon()}<Icon name="folder" size={30} weight={1.3} />{/snippet}
      {#snippet note()}
        Karma Yoga keeps your files in an ordinary folder, so a photo viewer, a file manager or a
        sync tool such as Syncthing can use them where they lie. Pictures, films and documents you
        add from any app land there, in order, and you can move the whole folder later and point
        Settings at it. Whatever you mark as private is the exception: it is kept encrypted inside
        the vault instead.
      {/snippet}
      {#snippet action()}
        <div class="suggest">
          <span class="faint">Suggested</span>
          <code>{folder.suggested}</code>
        </div>
        <div class="actions">
          <button
            class="btn btn-primary"
            disabled={files.busy}
            onclick={() => void files.setUpFolder()}
          >
            Use this folder
          </button>
          {#if files.local}
            <button class="btn" disabled={files.busy} onclick={() => void files.chooseFolder()}>
              Choose another…
            </button>
          {/if}
        </div>
        {#if !files.local}
          <p class="small">
            To keep them somewhere else, choose the folder in Settings on the computer that holds
            the vault.
          </p>
        {/if}
      {/snippet}
    </EmptyState>
  {:else if folder?.state === 'unavailable'}
    <EmptyState lead="The files folder is not available.">
      {#snippet icon()}<Icon name="alert" size={30} weight={1.3} />{/snippet}
      {#snippet note()}
        {folder.reason}
        <br />
        <code>{folder.path}</code>
        <br />
        Nothing has been changed while it is away, and nothing is marked missing. What you added to your
        files -- captions, tags, albums -- is safe in the vault.
      {/snippet}
      {#snippet action()}
        <div class="actions">
          <button class="btn btn-primary" onclick={() => void files.reload()}>Try again</button>
          {#if files.local}
            <button class="btn" onclick={() => void files.chooseFolder()}>Choose another…</button>
          {/if}
          <button class="btn" onclick={() => panels.openSettings('files')}>Settings</button>
        </div>
      {/snippet}
    </EmptyState>
  {:else}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="pane"
      ondragenter={enter}
      ondragover={over}
      ondragleave={leave}
      ondropcapture={() => (depth = 0)}
      ondrop={drop}
    >
      <FilesToolbar />

      {#if view === 'photos'}
        <PhotosView />
      {:else if view === 'albums'}
        <AlbumsView />
      {:else if !listed}
        <p class="loading" role="status">Loading…</p>
      {:else if empty}
        {@const words = EMPTY[view] ?? EMPTY.browse!}
        {#if searching}
          <EmptyState lead="No files match.">
            {#snippet note()}Nothing was found for “{files.search.trim()}”.{/snippet}
            {#snippet action()}
              <button class="btn" onclick={() => files.setSearch('')}>Clear the search</button>
            {/snippet}
          </EmptyState>
        {:else}
          <EmptyState lead={words.lead}>
            {#snippet icon()}
              <Icon
                name={view === 'trash' ? 'trash' : view === 'missing' ? 'alert' : 'folder'}
                size={30}
                weight={1.3}
              />
            {/snippet}
            {#snippet note()}{words.note}{/snippet}
            {#snippet action()}
              {#if view === 'browse' || view === 'recent' || view === 'encrypted'}
                <button class="btn btn-primary" onclick={() => files.create()}>Add files</button>
              {/if}
            {/snippet}
          </EmptyState>
        {/if}
      {:else if files.layout === 'list'}
        <FilesList />
      {:else}
        <FilesGrid />
      {/if}

      {#if dropping}
        <div class="drop" aria-hidden="true">
          <span class="drop-label"><Icon name="upload" size={18} /> Drop to add to {where}</span>
        </div>
      {/if}
    </div>

    {#if railed}
      <div class="rail">
        {#if several}
          <aside class="several" aria-label="Selection">
            <p class="several-count">{plural(files.selection.length, 'file')} selected</p>
            {#if chosenBytes > 0}<p class="several-size">{formatSize(chosenBytes)}</p>{/if}
            <p class="several-hint">
              Favourite, tag, move, export or trash them together from the bar above.
            </p>
            <button class="btn btn-outline" onclick={() => files.clearSelection()}>
              Clear the selection
            </button>
          </aside>
        {:else}
          <FileInspector />
        {/if}
      </div>
    {/if}
  {/if}
</main>

{#if viewing >= 0}
  <FileViewer
    files={list}
    index={viewing}
    onclose={() => files.closeViewer()}
    onindex={(i: number) => {
      const next = list[i]
      if (next) files.open(next.id)
    }}
  />
{/if}

<style>
  .files {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
  }
  .pane {
    position: relative;
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  /* The details rail: as wide as the inspector, there before it has anything
     to show. */
  .rail {
    flex: none;
    display: flex;
    width: 340px;
    min-height: 0;
  }
  .several {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--sp-2);
    padding: var(--sp-6) var(--sp-4);
    background: var(--bg-panel);
    border-left: 1px solid var(--border);
  }
  .several p {
    margin: 0;
  }
  .several-count {
    font-size: var(--text-md);
    font-weight: 620;
  }
  .several-size {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .several-hint {
    margin-bottom: var(--sp-2);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }

  .loading {
    margin: var(--sp-6);
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }

  .suggest {
    display: flex;
    align-items: baseline;
    justify-content: center;
    gap: var(--sp-2);
    margin-bottom: var(--sp-3);
    font-size: var(--text-sm);
  }
  .faint {
    color: var(--fg-faint);
  }
  code {
    padding: 1px 6px;
    border-radius: 4px;
    background: var(--bg-sunken);
    color: var(--fg-muted);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
    user-select: text;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: var(--sp-2);
  }
  .small {
    margin: var(--sp-3) 0 0;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }

  /* What a drag from the desktop is over: the pane outlined, one line saying
     what the drop will do. Flat -- no blur over a pane of thumbnails. */
  .drop {
    position: absolute;
    inset: var(--sp-2);
    z-index: 5;
    display: grid;
    place-items: center;
    border: 2px dashed var(--accent);
    border-radius: var(--radius-lg);
    background: color-mix(in oklab, var(--bg) 70%, transparent);
    pointer-events: none;
  }
  .drop-label {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-2) var(--sp-4);
    border-radius: 999px;
    background: var(--accent);
    color: var(--fg-on-accent);
    font-weight: 600;
  }
</style>
