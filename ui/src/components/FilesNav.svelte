<script lang="ts">
  // Files' half of the sidebar: the places to look (the folder itself, what
  // changed lately, photos, albums, favourites, what is encrypted, what no
  // record uses, what has gone missing, the trash), the folders leading to
  // where Browse is and the ones inside it, the files on their way in, and
  // how much is kept.
  //
  // The folder rows are not the whole tree. The store lists one folder at a
  // time, so what is drawn is the way down to the open folder and the
  // folders directly in it -- enough to go in and to go back out, which is
  // what a sidebar is for here; the pane's own path bar does the rest. Every
  // folder row takes files dropped on it, from the pane or from the desktop.
  //
  // Counts only where they are already known (`status.usage`, the albums) --
  // a number that needs a query of its own is not worth a sidebar's
  // round trips. Missing shows only when something is missing: a standing
  // zero there would be a worry with nothing in it.

  import { FILES_VIEWS, files, type FilesView } from '../lib/files.svelte'
  import { formatSize } from '../lib/files'
  import { acceptDrag, crumbsOf, dropInto } from '../lib/files-ui.svelte'
  import { plural } from '../lib/format'
  import { panels } from '../lib/panels.svelte'
  import type { SymbolName } from '../lib/symbols'
  import FilesUploads from './FilesUploads.svelte'
  import Symbol from './Symbol.svelte'

  const LABELS: Record<FilesView, string> = {
    browse: 'Browse',
    recent: 'Recent',
    photos: 'Photos',
    albums: 'Albums',
    favourites: 'Favourites',
    encrypted: 'Encrypted',
    unused: 'Unused',
    missing: 'Missing',
    trash: 'Trash',
  }
  const ICONS: Record<FilesView, SymbolName> = {
    browse: 'folder',
    recent: 'clock',
    photos: 'image',
    albums: 'book',
    favourites: 'star',
    encrypted: 'lock',
    unused: 'archive',
    missing: 'alert',
    trash: 'trash',
  }

  const usage = $derived(files.status?.usage ?? null)

  /** A count to show beside a view, or none. */
  function countOf(view: FilesView): number {
    switch (view) {
      case 'encrypted':
        return usage?.sealedFiles ?? 0
      case 'missing':
        return usage?.missingFiles ?? 0
      case 'trash':
        return usage?.trashedFiles ?? 0
      case 'albums':
        return files.albums.length
      default:
        return 0
    }
  }

  const shown = $derived(
    FILES_VIEWS.filter((v) => v !== 'missing' || files.view === 'missing' || countOf(v) > 0),
  )

  const browsing = $derived(files.view === 'browse')
  const trail = $derived(crumbsOf(files.dir))
  const inside = $derived(files.listing?.folders ?? [])

  /** The folder row a drag is over, to light it. */
  let over = $state<string | null>(null)

  function pick(view: FilesView) {
    // The folder row is the way to the top of Browse once there, so Browse
    // itself only changes the place when it is not already the one shown.
    if (view === 'browse' && browsing) files.openFolder('')
    else files.setView(view)
  }

  function dragOver(e: DragEvent, path: string) {
    if (acceptDrag(e)) over = path
  }
  function drop(e: DragEvent, path: string) {
    over = null
    dropInto(e, path)
  }
</script>

<nav class="scroll side-nav" aria-label="Files">
  {#each shown as view (view)}
    {@const n = countOf(view)}
    <button
      class="side-row"
      class:sel={files.view === view && (view !== 'browse' || files.dir === '')}
      class:drop={view === 'browse' && over === ''}
      aria-current={files.view === view ? 'page' : undefined}
      onclick={() => pick(view)}
      ondragover={view === 'browse' ? (e) => dragOver(e, '') : undefined}
      ondragleave={view === 'browse' ? () => (over = null) : undefined}
      ondrop={view === 'browse' ? (e) => drop(e, '') : undefined}
    >
      <span class="side-icon"><Symbol name={ICONS[view]} size={17} /></span>
      <span class="side-text">{LABELS[view]}</span>
      {#if n > 0}<span class="side-count">{n}</span>{/if}
    </button>

    {#if view === 'browse' && browsing && (trail.length > 0 || inside.length > 0)}
      <div class="tree" role="group" aria-label="Folders">
        {#each trail as crumb, i (crumb.path)}
          {@const here = i === trail.length - 1}
          <button
            class="side-row folder"
            class:sel={here}
            class:drop={over === crumb.path}
            style:--depth={i + 1}
            aria-current={here ? 'page' : undefined}
            onclick={() => files.openFolder(crumb.path)}
            ondragover={(e) => dragOver(e, crumb.path)}
            ondragleave={() => (over = null)}
            ondrop={(e) => drop(e, crumb.path)}
          >
            <span class="side-icon"><Symbol name="folder" size={15} /></span>
            <span class="side-text">{crumb.name}</span>
          </button>
        {/each}
        {#each inside as folder (folder.path)}
          <button
            class="side-row folder"
            class:drop={over === folder.path}
            style:--depth={trail.length + 1}
            onclick={() => files.openFolder(folder.path)}
            ondragover={(e) => dragOver(e, folder.path)}
            ondragleave={() => (over = null)}
            ondrop={(e) => drop(e, folder.path)}
          >
            <span class="side-icon"><Symbol name="folder" size={15} /></span>
            <span class="side-text">{folder.name}</span>
            {#if folder.items > 0}<span class="side-count">{folder.items}</span>{/if}
          </button>
        {/each}
      </div>
    {/if}
  {/each}

  <button class="side-row settings" onclick={() => panels.openSettings('files')}>
    <span class="side-icon"><Symbol name="settings" size={16} /></span>
    <span class="side-text">The files folder…</span>
  </button>
</nav>

<FilesUploads />

{#if usage}
  <p
    class="storage"
    title={usage.sealedFiles > 0 ? `${plural(usage.sealedFiles, 'file')} encrypted` : undefined}
  >
    {#if files.status?.scanning}
      Looking at the folder…
    {:else}
      {formatSize(usage.bytes)} in {plural(usage.files, 'file')}
    {/if}
  </p>
{/if}

<style>
  /* The rows, headings and counts are the shared sidebar's -- see app.css's
     "Sidebars". What is Files' own: the folder rows hung under Browse, each
     one step further in than the one above, the line a drag leaves on the
     row it is over, and the settings row kept apart at the foot. */

  .tree {
    display: flex;
    flex-direction: column;
    margin-bottom: var(--sp-1);
  }
  .folder {
    height: 30px;
    padding-left: calc(var(--sp-3) + var(--depth, 1) * 14px);
    font-size: var(--text-sm);
  }
  .side-row.drop {
    background: var(--bg-selected);
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }

  .settings {
    margin-top: var(--sp-4);
    color: var(--fg-subtle);
  }

  .storage {
    flex: none;
    margin: 0;
    padding: var(--sp-2) var(--sp-4) var(--sp-3);
    border-top: 1px solid var(--border);
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
</style>
