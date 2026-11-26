<script lang="ts">
  // The bar over the Files list, and over Photos and Albums too: where this is
  // (the folder's path, with every step a way back), how it is sorted and laid
  // out, whether hidden files show, and the ways to add. When files are
  // selected a second row gives way to what can be done to them -- a different
  // set in the trash and under Missing, where what the files need is getting
  // back, not tagging.
  //
  // There is no search here: the bar at the top of the window searches Files
  // (`apps.ts`), and a second field over the list would be two boxes for one
  // thing.
  //
  // It also draws the two small questions the pane asks over itself, because
  // a menu, a key and a button all reach them: a name (a new folder, an
  // album, some tags) and a folder to move into. The pane's own menus raise
  // them through `filesUi` and this is where they appear.

  import { untrack } from 'svelte'
  import { api, mobileShell } from '../lib/api'
  import { isReadable } from '../lib/documents'
  import { formatSize } from '../lib/files'
  import { files, type FilesView } from '../lib/files.svelte'
  import {
    acceptDrag,
    clearAll,
    crumbsOf,
    dropInto,
    filesUi,
    menuUnder,
    newAlbumAsk,
    parseWords,
  } from '../lib/files-ui.svelte'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import { plural } from '../lib/format'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { closesOnBack, details, phone } from '../lib/phone.svelte'
  import type { FileSort, FolderInfo } from '../lib/types'
  import Icon from './Icon.svelte'
  import PhoneBack from './PhoneBack.svelte'

  const view = $derived(files.view)
  const browsing = $derived(view === 'browse')

  const TITLES: Record<FilesView, string> = {
    browse: 'Files',
    recent: 'Recent',
    photos: 'Photos',
    albums: 'Albums',
    favourites: 'Favourites',
    encrypted: 'Encrypted',
    unused: 'Unused',
    missing: 'Missing',
    trash: 'Trash',
  }

  // ── Where this is ──────────────────────────────────────────────────

  const trail = $derived(crumbsOf(files.dir))
  /** The last few steps; a deep path keeps its end and a way to the rest. */
  const shownTrail = $derived(trail.slice(-3))
  const hiddenAbove = $derived(trail.length > shownTrail.length ? trail[trail.length - 4] : null)
  /** An album is open: it heads itself, with its name to rename in place, so
   *  this bar says nothing over it. */
  const inAlbum = $derived(view === 'albums' && files.album !== null)
  const title = $derived(
    browsing ? (trail.length > 0 ? trail[trail.length - 1]!.name : TITLES.browse) : TITLES[view],
  )

  const sub = $derived.by(() => {
    if (browsing) {
      const folders = files.listing?.folders.length ?? 0
      const n = files.visibleFiles.length
      return [folders > 0 ? plural(folders, 'folder') : '', plural(n, 'file')]
        .filter(Boolean)
        .join(' · ')
    }
    const total = files.page?.total
    const count = total === undefined ? '' : plural(total, 'file')
    const days = files.settings?.trashDays ?? 0
    switch (view) {
      case 'photos':
        return total === undefined ? '' : plural(total, 'photo')
      case 'albums':
        return files.albums.length > 0 ? plural(files.albums.length, 'album') : ''
      case 'trash':
        return [count, days > 0 ? `emptied after ${plural(days, 'day')}` : '']
          .filter(Boolean)
          .join(' · ')
      case 'missing':
        return [count, 'gone from the folder; what you added is kept'].filter(Boolean).join(' · ')
      case 'unused':
        return [count, 'in no entry, note or message'].filter(Boolean).join(' · ')
      default:
        return count
    }
  })

  /** The crumb a drag is over. */
  let over = $state<string | null>(null)

  // ── Sort, layout, adding ───────────────────────────────────────────

  const SORTS: { id: FileSort; label: string }[] = [
    { id: 'when', label: 'Date' },
    { id: 'added', label: 'Added' },
    { id: 'name', label: 'Name' },
    { id: 'size', label: 'Size' },
  ]

  const order = $derived.by(() => {
    const down = files.descending
    switch (files.sort) {
      case 'name':
        return down ? 'Z to A' : 'A to Z'
      case 'size':
        return down ? 'Largest first' : 'Smallest first'
      default:
        return down ? 'Newest first' : 'Oldest first'
    }
  })

  /** Recent, Photos and Albums keep their own order, and only the lists are
   *  laid out as a choice of grid or rows. */
  const ordered = $derived(view !== 'recent' && view !== 'photos' && view !== 'albums')
  const laidOut = $derived(view !== 'photos' && view !== 'albums')
  const canAdd = $derived(view !== 'trash' && view !== 'missing' && view !== 'albums')

  /** Where the way back goes on a phone: the folder above this one. */
  const parentName = $derived(trail.length > 1 ? trail[trail.length - 2]!.name : TITLES.browse)

  /** What a phone keeps behind one button: the sort, the layout, hidden
   *  files and a new folder -- a desktop has each in the bar itself. */
  function optionsMenu(e: MouseEvent) {
    menuUnder(
      e.currentTarget as HTMLElement,
      tidyMenu([
        ordered && { kind: 'heading', label: 'Sort by' },
        ...(ordered
          ? SORTS.map((s): MenuItem => ({
              label: s.label,
              checked: files.sort === s.id,
              run: () => files.setSort(s.id, files.descending),
            }))
          : []),
        ordered && {
          label: order,
          icon: 'refresh',
          hint: 'Reverse',
          run: () => files.setSort(files.sort, !files.descending),
        },
        ordered && SEP,
        laidOut && {
          label: 'Grid',
          icon: 'grid',
          checked: files.layout === 'grid',
          run: () => files.setLayout('grid'),
        },
        laidOut && {
          label: 'List',
          icon: 'list',
          checked: files.layout === 'list',
          run: () => files.setLayout('list'),
        },
        laidOut && SEP,
        {
          label: 'Show hidden files',
          icon: files.showHidden ? 'eye' : 'hidden',
          checked: files.showHidden,
          run: () => files.setShowHidden(!files.showHidden),
        },
        browsing && SEP,
        browsing && { label: 'New folder', icon: 'plus', run: newFolder },
      ]),
    )
  }

  function newFolder() {
    filesUi.askName({
      title: 'New folder',
      label: 'Name',
      confirm: 'Create',
      run: (name) => files.createFolder(name.trim()),
    })
  }

  // ── What is selected ───────────────────────────────────────────────

  const ids = $derived(files.selection)
  const chosen = $derived(files.selectedFiles)
  const bytes = $derived(chosen.reduce((sum, f) => sum + f.size, 0))
  const allFavourite = $derived(chosen.length > 0 && chosen.every((f) => f.favourite))
  const allSealed = $derived(chosen.length > 0 && chosen.every((f) => f.storage.kind === 'sealed'))
  const one = $derived(ids.length === 1 ? chosen[0] : undefined)
  /** The one file is a document the reader can open, and is there to open. */
  const readable = $derived(one !== undefined && isReadable(one))

  function rateMenu(e: MouseEvent) {
    const rated = one?.rating ?? 0
    menuUnder(e.currentTarget as HTMLElement, [
      ...[1, 2, 3, 4, 5].map((n): MenuItem => ({
        label: n === 1 ? '1 star' : `${n} stars`,
        checked: rated === n,
        run: () => files.setRating(ids, n),
      })),
      SEP,
      { label: 'No rating', run: () => files.setRating(ids, 0) },
    ])
  }

  function tag() {
    filesUi.askName({
      title: ids.length > 1 ? `Tag ${ids.length} files` : 'Tag this file',
      label: 'Tags, separated by commas',
      confirm: 'Add',
      run: (text) => files.update(ids, { addTags: parseWords(text) }),
    })
  }

  async function albumMenu(e: MouseEvent) {
    const button = e.currentTarget as HTMLElement
    await filesUi.ensureAlbums()
    menuUnder(
      button,
      tidyMenu([
        ...files.albums.map((a): MenuItem => ({
          label: a.album.name,
          run: () => files.addToAlbum(a.album.id, ids),
        })),
        files.albums.length > 0 && SEP,
        { label: 'New album…', run: () => filesUi.askName(newAlbumAsk(ids)) },
      ]),
    )
  }

  async function moreMenu(e: MouseEvent) {
    const button = e.currentTarget as HTMLElement
    // A phone's bar has room for the few that are asked for most; the rest
    // are here.
    if (phone.on) await filesUi.ensureAlbums()
    menuUnder(
      button,
      tidyMenu([
        phone.on && {
          label: 'Rate',
          icon: 'star',
          items: [
            ...[1, 2, 3, 4, 5].map((n): MenuItem => ({
              label: n === 1 ? '1 star' : `${n} stars`,
              checked: (one?.rating ?? 0) === n,
              run: () => files.setRating(ids, n),
            })),
            SEP,
            { label: 'No rating', run: () => files.setRating(ids, 0) },
          ],
        },
        phone.on && { label: 'Tag…', icon: 'tag', run: tag },
        phone.on && {
          label: 'Add to album',
          icon: 'image',
          items: tidyMenu([
            ...files.albums.map((a): MenuItem => ({
              label: a.album.name,
              run: () => files.addToAlbum(a.album.id, ids),
            })),
            files.albums.length > 0 && SEP,
            { label: 'New album…', run: () => filesUi.askName(newAlbumAsk(ids)) },
          ]),
        },
        phone.on &&
          !mobileShell && {
            label: 'Export…',
            icon: 'download',
            run: () => void files.exportFiles(ids),
          },
        phone.on && SEP,
        one && { label: 'Open', icon: 'eye', run: () => files.open(one.id) },
        // A document is also read, in the reader with its contents beside it.
        // For a PDF or an EPUB "Open" goes there too, and this is the same.
        one && readable && { label: 'Read', icon: 'book', run: () => files.read(one.id) },
        one &&
          files.local && {
            label: 'Open in another program',
            icon: 'share',
            run: () => files.openExternally(one.id),
          },
        one &&
          files.local &&
          one.storage.kind === 'plain' && {
            label: 'Show in folder',
            icon: 'folder',
            run: () => files.reveal(one.id),
          },
        SEP,
        {
          label: allSealed ? 'Store as a plain file' : 'Encrypt',
          icon: 'lock',
          run: async () => {
            for (const f of chosen) await files.setEncrypted(f.id, !allSealed)
          },
        },
      ]),
    )
  }

  // ── Questions ──────────────────────────────────────────────────────

  let nameText = $state('')
  $effect(() => {
    const ask = filesUi.name
    if (ask) nameText = ask.initial ?? ''
  })

  function submitName(e: SubmitEvent) {
    e.preventDefault()
    const ask = filesUi.name
    const text = nameText.trim()
    if (!ask || !text) return
    filesUi.name = null
    void Promise.resolve(ask.run(text))
  }

  /** The folder being looked into, and what is in it. */
  let into = $state('')
  let inside = $state<FolderInfo[]>([])
  let reading = $state(false)
  let readError = $state<string | null>(null)

  $effect(() => {
    if (filesUi.moving === null) return
    // Opened on the folder the pane is showing.
    into = untrack(() => (files.view === 'browse' ? files.dir : ''))
  })

  $effect(() => {
    const dir = into
    if (filesUi.moving === null) return
    let gone = false
    reading = true
    readError = null
    void api
      .listFolder(dir)
      .then((listing) => {
        if (!gone) inside = listing.folders
      })
      .catch((e: unknown) => {
        if (!gone) {
          inside = []
          readError = e instanceof Error ? e.message : String(e)
        }
      })
      .finally(() => {
        if (!gone) reading = false
      })
    return () => {
      gone = true
    }
  })

  const intoTrail = $derived(crumbsOf(into))

  function moveHere() {
    const moving = filesUi.moving
    filesUi.moving = null
    if (moving && moving.length > 0) void files.moveFiles(moving, into)
  }

  function closeQuestions() {
    if (filesUi.name) filesUi.name = null
    else if (filesUi.moving) filesUi.moving = null
  }

  // On a phone, back puts a question away.
  closesOnBack(
    () => filesUi.name !== null,
    () => (filesUi.name = null),
  )
  closesOnBack(
    () => filesUi.moving !== null,
    () => (filesUi.moving = null),
  )

  /** The selection's details, as a page over the list. */
  function showDetails() {
    details.open('files')
  }
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape' && (filesUi.name || filesUi.moving)) {
      e.preventDefault()
      closeQuestions()
    }
  }}
/>

<header class="top">
  {#if browsing && trail.length > 0}
    <PhoneBack onclick={() => files.up()} label={parentName} />
  {/if}
  <div class="pane-titles">
    {#if browsing}
      <nav class="crumbs" aria-label="Folder">
        {#if trail.length > 0}
          <button
            class="crumb"
            class:drop={over === ''}
            onclick={() => files.openFolder('')}
            ondragover={(e) => {
              if (acceptDrag(e)) over = ''
            }}
            ondragleave={() => (over = null)}
            ondrop={(e) => {
              over = null
              dropInto(e, '')
            }}>{TITLES.browse}</button
          >
          <span class="crumb-sep" aria-hidden="true"><Icon name="chevron" size={12} /></span>
        {/if}
        {#if hiddenAbove}
          <button class="crumb" onclick={() => files.openFolder(hiddenAbove.path)}>…</button>
          <span class="crumb-sep" aria-hidden="true"><Icon name="chevron" size={12} /></span>
        {/if}
        {#each shownTrail.slice(0, -1) as c (c.path)}
          <button
            class="crumb"
            class:drop={over === c.path}
            onclick={() => files.openFolder(c.path)}
            ondragover={(e) => {
              if (acceptDrag(e)) over = c.path
            }}
            ondragleave={() => (over = null)}
            ondrop={(e) => {
              over = null
              dropInto(e, c.path)
            }}>{c.name}</button
          >
          <span class="crumb-sep" aria-hidden="true"><Icon name="chevron" size={12} /></span>
        {/each}
        <h1 class="pane-title">{title}</h1>
      </nav>
    {:else if !inAlbum}
      <h1 class="pane-title">{title}</h1>
    {/if}
    {#if sub && !inAlbum}<span class="pane-sub">{sub}</span>{/if}
  </div>

  <div class="tools">
    {#if ordered}
      <select
        class="sort desktop-only"
        aria-label="Sort by"
        value={files.sort}
        onchange={(e) => files.setSort(e.currentTarget.value as FileSort, files.descending)}
      >
        {#each SORTS as s (s.id)}
          <option value={s.id}>{s.label}</option>
        {/each}
      </select>
      <button
        class="round desktop-only"
        title={order}
        aria-label="{order}. Reverse the order"
        onclick={() => files.setSort(files.sort, !files.descending)}
      >
        <span class="arrow" class:down={files.descending}><Icon name="chevron" size={14} /></span>
      </button>
    {/if}

    {#if laidOut}
      <div class="layout desktop-only" role="group" aria-label="Layout">
        <button
          class:on={files.layout === 'grid'}
          aria-pressed={files.layout === 'grid'}
          title="Grid"
          aria-label="Grid"
          onclick={() => files.setLayout('grid')}
        >
          <Icon name="grid" size={15} />
        </button>
        <button
          class:on={files.layout === 'list'}
          aria-pressed={files.layout === 'list'}
          title="List"
          aria-label="List"
          onclick={() => files.setLayout('list')}
        >
          <Icon name="list" size={15} />
        </button>
      </div>
    {/if}

    <button
      class="round desktop-only"
      class:on={files.showHidden}
      aria-pressed={files.showHidden}
      title={files.showHidden ? 'Hide the files marked hidden' : 'Show the files marked hidden'}
      aria-label="Show hidden files"
      onclick={() => files.setShowHidden(!files.showHidden)}
    >
      <Icon name={files.showHidden ? 'eye' : 'hidden'} size={15} />
    </button>

    {#if browsing}
      <button class="btn desktop-only" onclick={newFolder}>
        <Icon name="plus" size={14} /> New folder
      </button>
    {/if}
    {#if view === 'trash' && (files.status?.usage.trashedFiles ?? 0) > 0}
      <button class="btn btn-outline btn-ghost-danger" onclick={() => files.emptyTrash()}>
        Empty trash…
      </button>
    {/if}
    {#if canAdd}
      <!-- The store picks the way: the system picker on the vault's own
           machine, an input anywhere else, into the folder on screen. -->
      <button class="btn btn-primary" onclick={() => files.create()}>
        <Icon name="upload" size={15} /> Add
      </button>
    {/if}
    <button
      class="round options phone-only"
      aria-label="Options"
      title="Sort, layout and more"
      onclick={optionsMenu}
    >
      <Icon name="more" size={18} />
    </button>
  </div>
</header>

{#if ids.length > 0}
  <div class="selbar" role="toolbar" aria-label="Selected files">
    <span class="count">
      {plural(ids.length, 'file')} selected{bytes > 0 ? ` · ${formatSize(bytes)}` : ''}
    </span>
    <button class="clear" onclick={clearAll}>Clear</button>
    <span class="grow"></span>

    {#if view === 'trash'}
      <button class="btn btn-sm" onclick={() => void files.restore(ids)}>
        <Icon name="refresh" size={14} /> Restore
      </button>
      <button class="btn btn-sm btn-ghost-danger" onclick={() => files.deleteForever(ids)}>
        <Icon name="trash" size={14} /> Delete forever…
      </button>
    {:else if view === 'missing'}
      {#if ids.length === 1}
        <button class="btn btn-sm" onclick={() => void files.restoreFromBackup(ids[0]!)}>
          <Icon name="archive" size={14} /> Restore from backup
        </button>
      {/if}
      <button class="btn btn-sm btn-ghost-danger" onclick={() => files.forgetMissing(ids)}>
        <Icon name="trash" size={14} /> Forget…
      </button>
    {:else if phone.on}
      <!-- A bar of icons: the few asked for most, and More for the rest. -->
      <button
        class="tool"
        class:on={allFavourite}
        aria-label={allFavourite ? 'Unfavourite' : 'Favourite'}
        aria-pressed={allFavourite}
        onclick={() => void files.update(ids, { favourite: !allFavourite })}
      >
        <Icon name="star" size={19} filled={allFavourite} />
      </button>
      <button class="tool" aria-label="Move" onclick={() => filesUi.pickFolder([...ids])}>
        <Icon name="folder" size={19} />
      </button>
      {#if ids.length === 1}
        <button class="tool" aria-label="Details" onclick={showDetails}>
          <Icon name="info" size={19} />
        </button>
      {/if}
      <button class="tool" aria-label="More" onclick={(e) => void moreMenu(e)}>
        <Icon name="more" size={19} />
      </button>
      <button class="tool danger" aria-label="Trash" onclick={() => void files.trash(ids)}>
        <Icon name="trash" size={19} />
      </button>
    {:else}
      <button
        class="btn btn-sm"
        aria-pressed={allFavourite}
        onclick={() => void files.update(ids, { favourite: !allFavourite })}
      >
        <Icon name="star" size={14} filled={allFavourite} />
        {allFavourite ? 'Unfavourite' : 'Favourite'}
      </button>
      <button class="btn btn-sm" onclick={rateMenu}>Rate</button>
      <button class="btn btn-sm" onclick={tag}><Icon name="tag" size={14} /> Tag</button>
      <button class="btn btn-sm" onclick={(e) => void albumMenu(e)}>
        <Icon name="image" size={14} /> Album
      </button>
      <button class="btn btn-sm" onclick={() => filesUi.pickFolder([...ids])}>
        <Icon name="folder" size={14} /> Move
      </button>
      {#if !mobileShell}
        <button class="btn btn-sm" onclick={() => void files.exportFiles(ids)}>
          <Icon name="download" size={14} /> Export
        </button>
      {/if}
      <button class="btn btn-sm" aria-label="More" title="More" onclick={moreMenu}>
        <Icon name="more" size={14} />
      </button>
      <button class="btn btn-sm btn-ghost-danger" onclick={() => void files.trash(ids)}>
        <Icon name="trash" size={14} /> Trash
      </button>
    {/if}
  </div>
{/if}

{#if filesUi.name}
  {@const ask = filesUi.name}
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div class="scrim" onclick={() => (filesUi.name = null)}></div>
  <div class="sheet" role="dialog" aria-modal="true" aria-label={ask.title} use:trapFocus>
    <form onsubmit={submitName}>
      <h2>{ask.title}</h2>
      <label class="field-label asked" for="files-name">{ask.label}</label>
      <input
        id="files-name"
        class="field"
        bind:value={nameText}
        placeholder={ask.placeholder ?? ''}
        autocomplete="off"
        spellcheck="false"
        use:focusOnMount
      />
      <div class="sheet-row">
        <span class="spacer"></span>
        <button type="button" class="btn" onclick={() => (filesUi.name = null)}>Cancel</button>
        <button type="submit" class="btn btn-primary" disabled={nameText.trim() === ''}>
          {ask.confirm}
        </button>
      </div>
    </form>
  </div>
{/if}

{#if filesUi.moving}
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div class="scrim" onclick={() => (filesUi.moving = null)}></div>
  <div class="sheet" role="dialog" aria-modal="true" aria-label="Move to a folder" use:trapFocus>
    <h2>Move {plural(filesUi.moving.length, 'file')} to</h2>
    <nav class="where" aria-label="Folder to move into">
      <button class="crumb" onclick={() => (into = '')} disabled={into === ''}>Files</button>
      {#each intoTrail as c (c.path)}
        <span class="crumb-sep" aria-hidden="true"><Icon name="chevron" size={12} /></span>
        <button class="crumb" disabled={c.path === into} onclick={() => (into = c.path)}>
          {c.name}
        </button>
      {/each}
    </nav>
    <ul class="folders scroll">
      {#each inside as f (f.path)}
        <li>
          <button class="side-row" onclick={() => (into = f.path)}>
            <span class="side-icon"><Icon name="folder" size={15} /></span>
            <span class="side-text">{f.name}</span>
            <span class="side-count"><Icon name="chevron" size={12} /></span>
          </button>
        </li>
      {/each}
      {#if readError}
        <li class="error">{readError}</li>
      {:else if !reading && inside.length === 0}
        <li class="none">No folders in here.</li>
      {/if}
    </ul>
    <div class="sheet-row">
      <span class="spacer"></span>
      <button class="btn" onclick={() => (filesUi.moving = null)}>Cancel</button>
      <button class="btn btn-primary" use:focusOnMount onclick={moveHere}>Move here</button>
    </div>
  </div>
{/if}

<style>
  .top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    height: var(--header-h);
    padding: 0 var(--sp-4) 0 var(--sp-6);
    flex: none;
  }
  .tools {
    flex: none;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }

  /* The path: every step but the last is a way back, the last is the heading. */
  .crumbs {
    display: flex;
    align-items: baseline;
    gap: 2px;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
  }
  .crumbs .pane-title {
    flex: 0 1 auto;
    min-width: 0;
  }
  .crumb {
    flex: none;
    max-width: 14em;
    overflow: hidden;
    text-overflow: ellipsis;
    padding: 0 var(--sp-1);
    border-radius: var(--radius-sm);
    font-size: var(--text-lg);
    font-weight: 560;
    letter-spacing: -0.015em;
    line-height: var(--leading-tight);
    color: var(--fg-subtle);
  }
  .crumb:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .crumb.drop {
    background: var(--bg-selected);
    color: var(--accent);
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .crumb-sep {
    flex: none;
    align-self: center;
    display: flex;
    color: var(--fg-faint);
  }

  .sort {
    height: 32px;
    padding: 0 var(--sp-2);
    border: 0;
    border-radius: 999px;
    background: var(--bg-sunken);
    color: var(--fg-muted);
    font: inherit;
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .round {
    display: grid;
    place-items: center;
    flex: none;
    width: 32px;
    height: 32px;
    border-radius: 50%;
    color: var(--fg-muted);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .round:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .round.on {
    background: var(--bg-selected);
    color: var(--accent);
  }
  .arrow {
    display: grid;
    place-items: center;
    rotate: -90deg;
  }
  .arrow.down {
    rotate: 90deg;
  }

  /* Grid and list in the chips' rounded track, as Money's month is. */
  .layout {
    display: flex;
    gap: 2px;
    padding: 2px;
    border-radius: 999px;
    background: var(--bg-sunken);
  }
  .layout button {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .layout button:hover {
    color: var(--fg);
  }
  .layout button.on {
    background: var(--bg-raised);
    color: var(--accent);
  }

  .selbar {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--sp-2);
    flex: none;
    margin: 0 var(--sp-4) var(--sp-2) var(--sp-6);
    padding: var(--sp-1) var(--sp-2) var(--sp-1) var(--sp-3);
    border-radius: var(--radius-lg);
    background: var(--bg-selected);
  }
  .count {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--accent);
    font-variant-numeric: tabular-nums;
  }
  .clear {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .clear:hover {
    color: var(--fg);
    text-decoration: underline;
  }
  .grow {
    flex: 1;
  }

  /* The sheets. */
  .sheet h2 {
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
    margin-bottom: var(--sp-3);
  }
  .asked {
    margin-bottom: var(--sp-2);
  }
  .where {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 2px;
    margin-bottom: var(--sp-2);
  }
  .where .crumb {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg-muted);
  }
  .where .crumb:disabled {
    color: var(--fg);
  }
  .folders {
    list-style: none;
    margin: 0;
    padding: 0;
    height: 14rem;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    overflow-y: auto;
  }
  .none,
  .error {
    margin: var(--sp-3);
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .error {
    color: var(--danger);
  }

  /* ── A phone ────────────────────────────────────────────────────────────
     The title takes what is left of one row, with the way up in front of it
     when a folder is open; the sort, the layout and hidden files are behind
     the options button, and what a selection can be given is a bar of
     icons. */
  .tool {
    display: grid;
    place-items: center;
    flex: none;
    width: 44px;
    height: 44px;
    border-radius: var(--radius);
    color: var(--fg-muted);
  }
  .tool:active {
    background: var(--bg-hover);
  }
  .tool.on {
    color: var(--star);
  }
  .tool.danger {
    color: var(--danger);
  }

  :global([data-phone]) .top {
    height: auto;
    min-height: 56px;
    padding: var(--sp-1) var(--sp-2) var(--sp-1) var(--sp-3);
    gap: var(--sp-1);
  }
  /* The way up is its chevron; the folder above is named to a screen reader. */
  :global([data-phone]) .top > :global(.phone-back) {
    margin-left: calc(var(--sp-2) * -1);
  }
  :global([data-phone]) .top > :global(.phone-back .label) {
    display: none;
  }
  :global([data-phone]) .crumbs .crumb,
  :global([data-phone]) .crumbs .crumb-sep {
    display: none;
  }
  :global([data-phone]) .tools {
    gap: var(--sp-1);
  }
  :global([data-phone]) .where .crumb {
    min-height: 36px;
    padding: 0 var(--sp-2);
  }
  :global([data-phone]) .round {
    width: 44px;
    height: 44px;
  }

  :global([data-phone]) .selbar {
    margin: 0 var(--sp-3) var(--sp-2);
    padding: 0 var(--sp-1) 0 var(--sp-3);
    row-gap: 0;
  }
  :global([data-phone]) .selbar .count {
    padding: var(--sp-2) 0;
  }
  :global([data-phone]) .selbar .clear {
    min-height: 44px;
    margin-left: auto;
    padding: 0 var(--sp-3);
  }
  /* The line the icons go on. */
  :global([data-phone]) .selbar .grow {
    flex: 1 0 100%;
    height: 0;
  }
  :global([data-phone]) .selbar .tool {
    flex: 1 1 0;
  }
  :global([data-phone]) .selbar .btn {
    flex: 1 1 auto;
    justify-content: center;
  }
</style>
