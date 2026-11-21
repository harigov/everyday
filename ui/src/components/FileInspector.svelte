<script lang="ts">
  // One file, in the rail beside the list: a preview, its name, and then what
  // the file says about itself and what the person says about it -- two halves,
  // drawn as two, as a transaction's are.
  //
  // What the file says (its type, size, dates, where it is; for a picture the
  // camera and the exposure and where it was taken; for a film its length) is
  // read from the file and never edited here. What the person says -- a
  // caption, tags, a rating, people, a place, albums, and properties of their
  // own -- is kept in the vault and never written into the file: the file in
  // the folder is not touched.
  //
  // Text fields save when they lose focus or Enter is pressed, rather than on
  // every key: each save is a write to the vault, and half a caption is not
  // a caption anybody meant. Chips (tags, people) save as they are added.
  //
  // A property's key is `user.` and a word; the store validates, and the field
  // here says why in advance. Keys other apps and the indexer own are shown,
  // and not edited.
  //
  // In the full-window viewer this is its information panel, and `onclose`
  // closes that instead of un-describing the file.
  //
  // A document -- a PDF, an EPUB, Markdown, text, a web page -- can also be
  // read (the reader, with its contents and the assistant beside it) and put
  // on a library shelf, where it becomes an item with the file as its reading
  // copy. A file that a library item already holds says so under "Used in",
  // and the line opens the item.

  import { untrack } from 'svelte'
  import { docFormatOf, isReadable } from '../lib/documents'
  import { formatSize } from '../lib/files'
  import { files } from '../lib/files.svelte'
  import {
    KIND_LABELS,
    ORIGIN_LABELS,
    PROP_TYPES,
    PROP_WORD,
    USE_LABELS,
    emptyProp,
    filesUi,
    formatDuration,
    gpsText,
    mapUrl,
    menuUnder,
    openUse,
    parseWords,
    propAs,
    propFromText,
    propText,
  } from '../lib/files-ui.svelte'
  import { dateFormat, longDate, relativeTime, timeOfDay } from '../lib/format'
  import { library } from '../lib/library.svelte'
  import { openExternal } from '../lib/open-external'
  import { panels } from '../lib/panels.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { app } from '../lib/state.svelte'
  import type {
    FileEntry,
    FileId,
    FileUpdate,
    FileUse,
    Item,
    ItemId,
    KindId,
    MediaInfo,
    PropValue,
  } from '../lib/types'
  import FileThumb from './FileThumb.svelte'
  import Icon from './Icon.svelte'

  interface Props {
    /** Closes the panel; absent, closing stops describing the file. */
    onclose?: () => void
    /** Brings down the window the panel is in, when that is not the store's
     *  own viewer: the one a chat or a journal entry opened. Called as well as
     *  the store's, before the reader or another app is shown. */
    onleave?: () => void
  }
  let { onclose, onleave }: Props = $props()

  /** Ids are unique to this copy: the viewer's information panel and the
   *  pane's rail can be on screen together. */
  const uid = $props.id()

  const detail = $derived(files.detail)
  const file = $derived(detail?.file)
  /** Nothing is changed in the trash: it is put back first. */
  const locked = $derived(file?.status.state === 'trashed')

  function close() {
    if (onclose) onclose()
    else files.focus(null)
  }

  // ── What the person says ───────────────────────────────────────────

  function change(f: FileEntry, update: FileUpdate) {
    void files.update([f.id], update)
  }

  function saveName(f: FileEntry, input: HTMLInputElement) {
    const name = input.value.trim()
    if (!name) {
      input.value = f.name
      return
    }
    if (name !== f.name) void files.renameFile(f.id, name)
  }

  function saveCaption(f: FileEntry, value: string) {
    if (value !== f.caption) change(f, { caption: value })
  }

  function savePlace(f: FileEntry, value: string) {
    const place = value.trim()
    if (place !== (f.place ?? '')) change(f, { place })
  }

  /** Enter in a one-line field commits it, the way leaving it does. */
  function commitOnEnter(e: KeyboardEvent & { currentTarget: HTMLInputElement }) {
    if (e.key === 'Enter') {
      e.preventDefault()
      e.currentTarget.blur()
    }
  }

  function nameKeys(e: KeyboardEvent & { currentTarget: HTMLInputElement }, f: FileEntry) {
    if (e.key === 'Escape') {
      e.preventDefault()
      e.stopPropagation()
      e.currentTarget.value = f.name
      e.currentTarget.blur()
    } else commitOnEnter(e)
  }

  // Rating: one to five stars, and the one already given clears it.
  let hovered = $state(0)
  function rate(f: FileEntry, n: number) {
    void files.setRating([f.id], f.rating === n ? 0 : n)
  }

  // Tags and people.
  function addWords(f: FileEntry, kind: 'tags' | 'people', input: HTMLInputElement) {
    const have = new Set(kind === 'tags' ? f.tags : f.people)
    const words = parseWords(input.value).filter((w) => !have.has(w))
    input.value = ''
    if (words.length > 0) change(f, kind === 'tags' ? { addTags: words } : { addPeople: words })
  }
  function dropWord(f: FileEntry, kind: 'tags' | 'people', word: string) {
    change(f, kind === 'tags' ? { removeTags: [word] } : { removePeople: [word] })
  }
  function wordKeys(
    e: KeyboardEvent & { currentTarget: HTMLInputElement },
    f: FileEntry,
    kind: 'tags' | 'people',
  ) {
    if (e.key === 'Enter' || e.key === ',') {
      e.preventDefault()
      addWords(f, kind, e.currentTarget)
    }
  }

  // ── Albums ─────────────────────────────────────────────────────────

  let naming = $state(false)
  let albumName = $state('')

  async function albumMenu(e: MouseEvent, f: FileEntry) {
    const button = e.currentTarget as HTMLElement
    const inside = new Set((detail?.albums ?? []).map((a) => a.id))
    await filesUi.ensureAlbums()
    menuUnder(
      button,
      tidyMenu([
        ...files.albums
          .filter((a) => !inside.has(a.album.id))
          .map((a): MenuItem => ({
            label: a.album.name,
            run: () => files.addToAlbum(a.album.id, [f.id]),
          })),
        files.albums.some((a) => !inside.has(a.album.id)) && SEP,
        {
          label: 'New album…',
          run: () => {
            albumName = ''
            naming = true
          },
        },
      ]),
    )
  }

  function makeAlbum(f: FileEntry) {
    const name = albumName.trim()
    naming = false
    if (name) void files.createAlbum(name, [f.id])
  }

  // ── Properties ─────────────────────────────────────────────────────

  const properties = $derived(
    Object.entries(file?.properties ?? {}).sort(([a], [b]) => a.localeCompare(b)),
  )

  function setProperty(f: FileEntry, key: string, value: PropValue | null) {
    change(f, { properties: { [key]: value } })
  }

  /** A property's field changed: the new value, if the text makes one. */
  function editProperty(f: FileEntry, key: string, value: PropValue, input: HTMLInputElement) {
    const next =
      value.type === 'bool'
        ? ({ type: 'bool', value: input.checked } satisfies PropValue)
        : propFromText(value.type, input.value)
    if (!next) {
      // Not a number, not a date: put back what was there.
      input.value = propText(value)
      return
    }
    setProperty(f, key, next)
  }

  let adding = $state(false)
  let newWord = $state('')
  let newType = $state<PropValue['type']>('text')
  let newText = $state('')
  let newFlag = $state(false)
  let newProblem = $state<string | null>(null)

  function startAdding() {
    adding = true
    newWord = ''
    newType = 'text'
    newText = ''
    newFlag = false
    newProblem = null
  }

  function addProperty(f: FileEntry) {
    const word = newWord.trim()
    if (!PROP_WORD.test(word)) {
      newProblem = 'A key is a word: letters, digits, - and _, starting with a letter.'
      return
    }
    const key = `user.${word}`
    if (f.properties && key in f.properties) {
      newProblem = `${key} is already set.`
      return
    }
    const value =
      newType === 'bool'
        ? ({ type: 'bool', value: newFlag } satisfies PropValue)
        : newText.trim() === '' && (newType === 'number' || newType === 'date')
          ? emptyProp(newType)
          : propFromText(newType, newText)
    if (!value) {
      newProblem =
        newType === 'number' ? 'That is not a number.' : 'That is not a date (2026-11-15).'
      return
    }
    adding = false
    setProperty(f, key, value)
  }

  // ── What the file says ─────────────────────────────────────────────

  /** When it was taken: the camera's own wall-clock time when it wrote one. */
  function taken(media: MediaInfo): string | null {
    const local = media.takenLocal
    if (local && /^\d{4}-\d{2}-\d{2}/.test(local)) {
      const time = /[T ](\d{2}:\d{2})/.exec(local)?.[1]
      return time ? `${longDate(local.slice(0, 10))}, ${time}` : longDate(local.slice(0, 10))
    }
    if (media.takenAt) {
      const at = new Date(media.takenAt)
      return `${dateFormat({ dateStyle: 'long' }).format(at)}, ${timeOfDay(at)}`
    }
    return null
  }

  function camera(m: MediaInfo): string | null {
    const make = m.cameraMake?.trim() ?? ''
    const model = m.cameraModel?.trim() ?? ''
    if (!make && !model) return null
    return model.toLowerCase().startsWith(make.toLowerCase()) ? model : `${make} ${model}`.trim()
  }

  function exposure(m: MediaInfo): string | null {
    const bits = [
      m.fNumber ? `ƒ/${+m.fNumber.toFixed(1)}` : '',
      m.exposureTime ? `${m.exposureTime} s` : '',
      m.iso ? `ISO ${m.iso}` : '',
    ].filter(Boolean)
    return bits.length > 0 ? bits.join(' · ') : null
  }

  function where(f: FileEntry): string {
    if (f.storage.kind === 'sealed') return 'Encrypted, kept in the vault'
    const at = f.storage.path.lastIndexOf('/')
    return at < 0 ? 'The top of the files folder' : f.storage.path.slice(0, at)
  }

  function status(f: FileEntry): string | null {
    const s = f.status
    if (s.state === 'missing') return `Missing from the folder since ${longDate(s.since)}.`
    if (s.state === 'trashed') {
      return `In the trash since ${longDate(s.at)}${s.from ? `, from ${s.from}` : ''}.`
    }
    return null
  }

  async function showMap(f: FileEntry) {
    if (f.media.gps) await openExternal(mapUrl(f.media.gps))
  }

  // ── Leaving for another app ────────────────────────────────────────

  /** Whatever is over the window -- the viewer -- comes down first, or the
   *  other app would open behind it. */
  function leave(): void {
    files.closeViewer()
    onleave?.()
  }

  // ── Reading, and the library ───────────────────────────────────────

  /** A document the reader can open, and that is there to be opened: not in
   *  the trash, not missing from the folder. */
  const readable = $derived(file !== undefined && isReadable(file))
  /** A PDF or an EPUB is read whatever opens it, so for those "Open" would be
   *  a second button for the same thing. */
  const readerOnly = $derived.by(() => {
    const format = file ? docFormatOf(file) : null
    return format === 'pdf' || format === 'epub'
  })
  /** A library item holds this file already. */
  const inLibrary = $derived(detail?.uses.some((u) => u.owner.kind === 'item') ?? false)

  function read(f: FileEntry): void {
    leave()
    void files.read(f.id)
  }

  /** The item just made from a file, and the name of the shelf it went on:
   *  said under the buttons, with a way to open it. */
  let made = $state<{ file: FileId; item: Item; shelf: string } | null>(null)
  let making = $state(false)

  async function addToLibrary(f: FileEntry): Promise<void> {
    if (making) return
    making = true
    try {
      const item = await library.createFromFile(f.id)
      if (!item) return
      // The shelves are loaded when the library is opened, and it may not
      // have been: the name is wanted for the sentence.
      if (library.kinds.length === 0) await library.refreshKinds()
      made = {
        file: f.id,
        item,
        shelf: library.kinds.find((k) => k.id === item.kindId)?.name ?? 'your library',
      }
    } finally {
      making = false
    }
  }

  /** Open a library item in its own app. A shelf of its own is opened first
   *  when the library is on another: the panel draws only what the shelf on
   *  screen holds. The everything view holds it already. */
  async function openItem(id: ItemId, shelf: KindId | null): Promise<void> {
    leave()
    if (!(await app.goTo('library'))) return
    if (shelf && library.shelf !== null && library.shelf !== shelf) {
      await library.selectShelf(shelf)
    }
    await library.open(id)
  }

  function openMade(): void {
    if (made) void openItem(made.item.id, made.item.kindId)
  }

  /** What a "Used in" line calls the record, for the library by its shelf. */
  function useLabel(use: FileUse): string {
    if (use.owner.kind !== 'item') return USE_LABELS[use.owner.kind]
    const shelf = library.kinds.find((k) => k.id === use.parent)?.name
    return shelf ? `Library · ${shelf}` : 'Library item'
  }

  function openUsage(use: FileUse): void {
    if (use.owner.kind === 'item') void openItem(use.owner.id, use.parent ?? null)
    else {
      leave()
      void openUse(use)
    }
  }

  // The shelves' names are wanted for "Used in" when something on a shelf holds
  // the file, even if the library has not been opened this session.
  $effect(() => {
    if (inLibrary && library.kinds.length === 0) void library.refreshKinds()
  })

  const preview = $derived(file?.kind === 'video' || file?.kind === 'audio')

  const names = $derived({
    tags: files.facets?.tags.map((t) => t.name) ?? [],
    people: files.facets?.people.map((p) => p.name) ?? [],
  })

  // A different file starts with a closed form and no half-typed words.
  $effect(() => {
    void file?.id
    untrack(() => {
      adding = false
      naming = false
      hovered = 0
    })
  })
</script>

{#if detail && file}
  {@const f = file}
  <aside class="detail" aria-label="File">
    <div class="top">
      <span class="when">{KIND_LABELS[f.kind]} · {formatSize(f.size)}</span>
      <button class="close" title="Close" aria-label="Close the details" onclick={close}>
        <Icon name="close" size={15} />
      </button>
    </div>

    <div class="body scroll">
      {#key f.id}
        <button class="preview" title="Open" onclick={() => files.open(f.id)}>
          <FileThumb file={f} fit="contain" />
          {#if preview}
            <span class="play"><Icon name={f.kind === 'video' ? 'play' : 'mic'} size={22} /></span>
          {/if}
        </button>

        <section>
          <label class="field-label" for="{uid}-name">Name</label>
          <input
            id="{uid}-name"
            class="field small"
            value={f.name}
            disabled={locked}
            autocomplete="off"
            spellcheck="false"
            onchange={(e) => saveName(f, e.currentTarget)}
            onkeydown={(e) => nameKeys(e, f)}
          />
        </section>

        {#if status(f)}
          <p class="note" class:bad={f.status.state === 'missing'}>
            {status(f)}
            {#if f.status.state === 'trashed'}
              <button class="link inline" onclick={() => void files.restore([f.id])}>
                Put it back
              </button>
            {:else if f.status.state === 'missing'}
              <button class="link inline" onclick={() => void files.restoreFromBackup(f.id)}>
                Restore from a backup
              </button>
            {/if}
          </p>
        {/if}

        <section class="quick">
          <button
            class="fav"
            class:on={f.favourite}
            aria-pressed={f.favourite}
            disabled={locked}
            onclick={() => void files.toggleFavourite(f.id)}
          >
            <Icon name="star" size={16} filled={f.favourite} />
            {f.favourite ? 'Favourite' : 'Favourite?'}
          </button>
          <div
            class="stars"
            role="radiogroup"
            aria-label="Rating"
            onmouseleave={() => (hovered = 0)}
          >
            {#each [1, 2, 3, 4, 5] as n (n)}
              <button
                role="radio"
                aria-checked={f.rating === n}
                aria-label="{n} {n === 1 ? 'star' : 'stars'}"
                title={f.rating === n ? 'Clear the rating' : `${n} ${n === 1 ? 'star' : 'stars'}`}
                disabled={locked}
                onmouseenter={() => (hovered = n)}
                onclick={() => rate(f, n)}
              >
                <Icon name="star" size={17} filled={n <= (hovered || f.rating || 0)} />
              </button>
            {/each}
          </div>
        </section>

        <section>
          <label class="field-label" for="{uid}-caption">Caption</label>
          <textarea
            id="{uid}-caption"
            class="field note-field"
            rows="3"
            value={f.caption}
            disabled={locked}
            placeholder="A line about it"
            onchange={(e) => saveCaption(f, e.currentTarget.value)}
          ></textarea>
        </section>

        <section>
          <span class="field-label">Tags</span>
          <div class="chips">
            {#each f.tags ?? [] as tag (tag)}
              <span class="chip">
                <span class="chip-text">{tag}</span>
                {#if !locked}
                  <button
                    class="chip-x"
                    aria-label="Remove {tag}"
                    onclick={() => dropWord(f, 'tags', tag)}
                  >
                    <Icon name="close" size={11} />
                  </button>
                {/if}
              </span>
            {/each}
            <input
              class="chip-input"
              list="{uid}-tags"
              placeholder={(f.tags?.length ?? 0) > 0 ? 'Add' : 'Add a tag'}
              aria-label="Add a tag"
              disabled={locked}
              autocomplete="off"
              spellcheck="false"
              onkeydown={(e) => wordKeys(e, f, 'tags')}
              onblur={(e) => addWords(f, 'tags', e.currentTarget)}
            />
          </div>
          <datalist id="{uid}-tags">
            {#each names.tags as name (name)}<option value={name}></option>{/each}
          </datalist>
        </section>

        <section>
          <span class="field-label">People</span>
          <div class="chips">
            {#each f.people ?? [] as person (person)}
              <span class="chip">
                <span class="chip-text">{person}</span>
                {#if !locked}
                  <button
                    class="chip-x"
                    aria-label="Remove {person}"
                    onclick={() => dropWord(f, 'people', person)}
                  >
                    <Icon name="close" size={11} />
                  </button>
                {/if}
              </span>
            {/each}
            <input
              class="chip-input"
              list="{uid}-people"
              placeholder={(f.people?.length ?? 0) > 0 ? 'Add' : 'Who is in it'}
              aria-label="Add a person"
              disabled={locked}
              autocomplete="off"
              spellcheck="false"
              onkeydown={(e) => wordKeys(e, f, 'people')}
              onblur={(e) => addWords(f, 'people', e.currentTarget)}
            />
          </div>
          <datalist id="{uid}-people">
            {#each names.people as name (name)}<option value={name}></option>{/each}
          </datalist>
        </section>

        <section>
          <label class="field-label" for="{uid}-place">Place</label>
          <input
            id="{uid}-place"
            class="field small"
            list="{uid}-places"
            value={f.place ?? ''}
            disabled={locked}
            placeholder="Where it was taken"
            autocomplete="off"
            onchange={(e) => savePlace(f, e.currentTarget.value)}
            onkeydown={commitOnEnter}
          />
          <datalist id="{uid}-places">
            {#each files.facets?.places ?? [] as p (p.name)}<option value={p.name}></option>{/each}
          </datalist>
          {#if f.media.gps && !files.settings?.namePlaces && !f.place}
            <p class="hint">
              This picture knows where it was taken.
              <button class="link inline" onclick={() => panels.openSettings('files')}>
                Name places
              </button> in Settings to fill this in.
            </p>
          {/if}
        </section>

        <section>
          <span class="field-label">Albums</span>
          <div class="chips">
            {#each detail.albums as album (album.id)}
              <span class="chip">
                <button
                  class="chip-text link-chip"
                  title="Open the album"
                  onclick={() => {
                    leave()
                    files.openAlbum(album.id)
                  }}>{album.name}</button
                >
                <button
                  class="chip-x"
                  aria-label="Take out of {album.name}"
                  onclick={() => void files.removeFromAlbum(album.id, [f.id])}
                >
                  <Icon name="close" size={11} />
                </button>
              </span>
            {/each}
            {#if !naming}
              <button class="btn btn-sm add" onclick={(e) => void albumMenu(e, f)}>
                <Icon name="plus" size={12} /> Album
              </button>
            {/if}
          </div>
          {#if naming}
            <form
              class="inline-form"
              onsubmit={(e) => {
                e.preventDefault()
                makeAlbum(f)
              }}
            >
              <input
                class="field small"
                bind:value={albumName}
                placeholder="Name the new album"
                aria-label="Name of the new album"
                autocomplete="off"
                onkeydown={(e) => {
                  if (e.key === 'Escape') {
                    e.stopPropagation()
                    naming = false
                  }
                }}
              />
              <button class="btn btn-sm btn-primary" type="submit" disabled={!albumName.trim()}>
                Create
              </button>
            </form>
          {/if}
        </section>

        <section>
          <span class="field-label">Properties</span>
          {#if properties.length > 0}
            <ul class="props">
              {#each properties as [key, value] (key)}
                {@const own = key.startsWith('user.')}
                <li>
                  <span class="pkey" title={key}>{key}</span>
                  {#if own && !locked}
                    <span class="pvalue">
                      {#if value.type === 'bool'}
                        <input
                          type="checkbox"
                          checked={value.value}
                          aria-label={key}
                          onchange={(e) => editProperty(f, key, value, e.currentTarget)}
                        />
                      {:else}
                        <input
                          class="field small"
                          type={value.type === 'number'
                            ? 'number'
                            : value.type === 'date'
                              ? 'date'
                              : 'text'}
                          step={value.type === 'number' ? 'any' : undefined}
                          value={value.type === 'date' ? value.value.slice(0, 10) : propText(value)}
                          placeholder={value.type === 'list' ? 'a, b, c' : ''}
                          aria-label={key}
                          autocomplete="off"
                          onchange={(e) => editProperty(f, key, value, e.currentTarget)}
                          onkeydown={commitOnEnter}
                        />
                      {/if}
                      <select
                        class="ptype"
                        aria-label="Type of {key}"
                        value={value.type}
                        onchange={(e) =>
                          setProperty(
                            f,
                            key,
                            propAs(value, e.currentTarget.value as PropValue['type']),
                          )}
                      >
                        {#each PROP_TYPES as t (t.id)}
                          <option value={t.id}>{t.label}</option>
                        {/each}
                      </select>
                      <button
                        class="chip-x"
                        aria-label="Remove {key}"
                        onclick={() => setProperty(f, key, null)}
                      >
                        <Icon name="close" size={12} />
                      </button>
                    </span>
                  {:else}
                    <span class="pvalue plain">{propText(value)}</span>
                  {/if}
                </li>
              {/each}
            </ul>
          {/if}
          {#if adding && !locked}
            <form
              class="newprop"
              onsubmit={(e) => {
                e.preventDefault()
                addProperty(f)
              }}
            >
              <div class="keyrow">
                <span class="prefix">user.</span>
                <input
                  class="field small"
                  bind:value={newWord}
                  placeholder="key"
                  aria-label="Property key"
                  autocomplete="off"
                  spellcheck="false"
                />
                <select class="ptype" bind:value={newType} aria-label="Type">
                  {#each PROP_TYPES as t (t.id)}
                    <option value={t.id}>{t.label}</option>
                  {/each}
                </select>
              </div>
              {#if newType === 'bool'}
                <label class="toggle">
                  <input type="checkbox" bind:checked={newFlag} />
                  <span><b>Yes</b></span>
                </label>
              {:else}
                <input
                  class="field small"
                  type={newType === 'number' ? 'number' : newType === 'date' ? 'date' : 'text'}
                  step={newType === 'number' ? 'any' : undefined}
                  bind:value={newText}
                  placeholder={newType === 'list' ? 'a, b, c' : 'Value'}
                  aria-label="Property value"
                  autocomplete="off"
                />
              {/if}
              {#if newProblem}<p class="error">{newProblem}</p>{/if}
              <div class="row">
                <button class="btn btn-sm btn-primary" type="submit">Add</button>
                <button class="btn btn-sm" type="button" onclick={() => (adding = false)}>
                  Cancel
                </button>
              </div>
            </form>
          {:else if !locked}
            <button class="btn btn-sm add" onclick={startAdding}>
              <Icon name="plus" size={12} /> Add a property
            </button>
          {/if}
        </section>

        <section>
          <span class="field-label">Details</span>
          <dl class="facts">
            <dt>Type</dt>
            <dd>{KIND_LABELS[f.kind]}<span class="faint"> · {f.mime || 'unknown'}</span></dd>
            <dt>Size</dt>
            <dd title="{f.size.toLocaleString()} bytes">{formatSize(f.size)}</dd>
            {#if f.media.width && f.media.height}
              <dt>Dimensions</dt>
              <dd>{f.media.width} × {f.media.height}</dd>
            {/if}
            {#if f.media.durationMs}
              <dt>Length</dt>
              <dd>{formatDuration(f.media.durationMs)}</dd>
            {/if}
            {#if f.media.pages}
              <dt>Pages</dt>
              <dd>{f.media.pages}</dd>
            {/if}
            {#if f.media.codec}
              <dt>Codec</dt>
              <dd>{f.media.codec}</dd>
            {/if}
            {#if taken(f.media)}
              <dt>Taken</dt>
              <dd>{taken(f.media)}</dd>
            {/if}
            <dt>Modified</dt>
            <dd title={new Date(f.modifiedAt).toLocaleString()}>{relativeTime(f.modifiedAt)}</dd>
            <dt>Added</dt>
            <dd title={new Date(f.addedAt).toLocaleString()}>{relativeTime(f.addedAt)}</dd>
            <dt>From</dt>
            <dd>{ORIGIN_LABELS[f.origin]}</dd>
            <dt>Where</dt>
            <dd class="path">{where(f)}</dd>
          </dl>
        </section>

        {#if camera(f.media) || f.media.lens || exposure(f.media) || f.media.focalLengthMm}
          <section>
            <span class="field-label">Camera</span>
            <dl class="facts">
              {#if camera(f.media)}
                <dt>Camera</dt>
                <dd>{camera(f.media)}</dd>
              {/if}
              {#if f.media.lens}
                <dt>Lens</dt>
                <dd>{f.media.lens}</dd>
              {/if}
              {#if exposure(f.media)}
                <dt>Exposure</dt>
                <dd>{exposure(f.media)}</dd>
              {/if}
              {#if f.media.focalLengthMm}
                <dt>Focal length</dt>
                <dd>{+f.media.focalLengthMm.toFixed(1)} mm</dd>
              {/if}
            </dl>
          </section>
        {/if}

        {#if f.media.gps}
          <section>
            <span class="field-label">Location</span>
            <p class="gps">{gpsText(f.media.gps)}</p>
            <button class="link" onclick={() => void showMap(f)}>Open in OpenStreetMap</button>
          </section>
        {/if}

        {#if detail.uses.length > 0}
          <section>
            <span class="field-label">Used in</span>
            <ul class="uses">
              {#each detail.uses as usage (`${usage.owner.kind}:${usage.owner.id}`)}
                <li>
                  <button class="use" onclick={() => openUsage(usage)}>
                    <span class="use-kind">{useLabel(usage)}</span>
                    <span class="use-title">{usage.title || 'Untitled'}</span>
                    {#if usage.when}<span class="use-when">{relativeTime(usage.when)}</span>{/if}
                  </button>
                </li>
              {/each}
            </ul>
          </section>
        {/if}

        <section class="actions">
          {#if readable}
            <button class="btn btn-sm btn-primary" onclick={() => read(f)}>
              <Icon name="book" size={14} /> Read
            </button>
          {/if}
          {#if !onclose && !(readable && readerOnly)}
            <button class="btn btn-sm btn-outline" onclick={() => files.open(f.id)}>
              <Icon name="eye" size={14} /> Open
            </button>
          {/if}
          {#if readable && app.supportsLibrary && !inLibrary && made?.file !== f.id}
            <button
              class="btn btn-sm btn-outline"
              disabled={making}
              onclick={() => void addToLibrary(f)}
            >
              <Icon name="plus" size={14} />
              {making ? 'Adding…' : 'Add to library'}
            </button>
          {/if}
          {#if files.local}
            <button class="btn btn-sm btn-outline" onclick={() => void files.openExternally(f.id)}>
              <Icon name="share" size={14} /> Open in another program
            </button>
            {#if f.storage.kind === 'plain'}
              <button class="btn btn-sm btn-outline" onclick={() => void files.reveal(f.id)}>
                <Icon name="folder" size={14} /> Show in folder
              </button>
            {/if}
          {/if}
          <button class="btn btn-sm btn-outline" onclick={() => void files.exportFiles([f.id])}>
            <Icon name="download" size={14} /> Export
          </button>
          {#if !locked}
            <button
              class="btn btn-sm btn-outline"
              onclick={() => void files.setEncrypted(f.id, f.storage.kind !== 'sealed')}
            >
              <Icon name="lock" size={14} />
              {f.storage.kind === 'sealed' ? 'Store as a plain file' : 'Encrypt'}
            </button>
          {/if}
          {#if made && made.file === f.id}
            <p class="note made" role="status">
              Added to {made.shelf}.
              <button class="link inline" onclick={openMade}>Open it</button>
            </p>
          {/if}
        </section>

        <section class="foot">
          <label class="toggle">
            <input
              type="checkbox"
              checked={f.hidden}
              disabled={locked}
              onchange={(e) => change(f, { hidden: e.currentTarget.checked })}
            />
            <span>
              <b>Hide it</b>
              <small>Left out of Photos and the lists, unless hidden files are shown.</small>
            </span>
          </label>
          {#if f.status.state === 'trashed'}
            <button class="btn btn-sm" onclick={() => void files.restore([f.id])}>
              <Icon name="refresh" size={14} /> Restore
            </button>
            <button class="btn btn-sm btn-ghost-danger" onclick={() => files.deleteForever([f.id])}>
              <Icon name="trash" size={14} /> Delete forever…
            </button>
          {:else if f.status.state === 'missing'}
            <button class="btn btn-sm btn-ghost-danger" onclick={() => files.forgetMissing([f.id])}>
              <Icon name="trash" size={14} /> Forget…
            </button>
          {:else}
            <button class="btn btn-sm btn-ghost-danger" onclick={() => void files.trash([f.id])}>
              <Icon name="trash" size={14} /> Move to the trash
            </button>
          {/if}
        </section>
      {/key}
    </div>
  </aside>
{/if}

<style>
  /* The rail every app's detail is: `TaskDetail`'s width and surface. */
  .detail {
    width: 340px;
    flex: none;
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: var(--bg-panel);
    border-left: 1px solid var(--border);
  }
  .top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: var(--header-h);
    padding: 0 var(--sp-2) 0 var(--sp-4);
    flex: none;
  }
  .when {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .close {
    width: 26px;
    height: 26px;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .close:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .body {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 0 var(--sp-4) var(--sp-8);
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
  }

  .preview {
    position: relative;
    display: block;
    flex: none;
    width: 100%;
    height: 200px;
    border-radius: var(--radius-lg);
    overflow: hidden;
    background: var(--bg-sunken);
  }
  .play {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    color: var(--fg);
  }
  .play :global(svg) {
    padding: 12px;
    box-sizing: content-box;
    border-radius: 50%;
    background: color-mix(in oklab, var(--bg) 80%, transparent);
  }

  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    min-width: 0;
  }
  .field-label {
    margin-bottom: 2px;
  }
  .field.small {
    height: 32px;
    font-size: var(--text-sm);
  }
  .note-field {
    font-size: var(--text-sm);
    resize: vertical;
  }
  .hint {
    margin: 2px 0 0;
    font-size: var(--text-xs);
  }
  .note {
    margin: 0;
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
    color: var(--fg-muted);
  }
  .note.bad {
    color: var(--danger);
  }

  .link {
    align-self: flex-start;
    color: var(--accent);
    font-size: var(--text-sm);
    font-weight: 550;
    text-align: left;
  }
  .link:hover {
    text-decoration: underline;
  }
  .link.inline {
    display: inline;
    font-size: inherit;
  }

  /* Favourite and the stars, side by side. */
  .quick {
    flex-direction: row;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
  }
  .fav {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 30px;
    padding: 0 var(--sp-3) 0 var(--sp-2);
    border-radius: 999px;
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-muted);
  }
  .fav:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .fav.on {
    color: var(--star);
  }
  .stars {
    display: flex;
    gap: 1px;
    color: var(--star);
  }
  .stars button {
    display: grid;
    place-items: center;
    width: 24px;
    height: 28px;
    border-radius: var(--radius-sm);
  }
  .stars button:disabled,
  .fav:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .chips {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-1);
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    height: 24px;
    padding: 0 4px 0 var(--sp-2);
    border-radius: 99px;
    background: var(--bg-sunken);
    border: 1px solid var(--border);
    font-size: var(--text-xs);
    font-weight: 500;
    color: var(--fg-muted);
    min-width: 0;
  }
  .chip-text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 14em;
  }
  .link-chip:hover {
    color: var(--accent);
    text-decoration: underline;
  }
  .chip-x {
    flex: none;
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    color: var(--fg-faint);
  }
  .chip-x:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .chip-input {
    flex: 1;
    min-width: 7em;
    height: 24px;
    padding: 0 var(--sp-1);
    border: 0;
    background: transparent;
    font-size: var(--text-sm);
    color: var(--fg);
    user-select: text;
  }
  .chip-input:focus {
    outline: none;
  }
  .add {
    align-self: flex-start;
    color: var(--accent);
  }
  .inline-form {
    display: flex;
    gap: var(--sp-2);
    margin-top: var(--sp-1);
  }
  .inline-form .field {
    flex: 1;
    min-width: 0;
  }

  .props {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .props li {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  .pkey {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .pvalue {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    min-width: 0;
  }
  .pvalue .field {
    flex: 1;
    min-width: 0;
  }
  .pvalue.plain {
    font-size: var(--text-sm);
    color: var(--fg-muted);
    overflow-wrap: anywhere;
    user-select: text;
  }
  .pvalue input[type='checkbox'],
  .toggle input {
    flex: none;
    accent-color: var(--accent);
  }
  .ptype {
    flex: none;
    height: 32px;
    padding: 0 var(--sp-1);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    font-size: var(--text-xs);
    color: var(--fg-muted);
  }
  .newprop {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-sunken);
  }
  .keyrow,
  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
  }
  .keyrow .field {
    flex: 1;
    min-width: 0;
  }
  .prefix {
    flex: none;
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .newprop .error {
    margin: 0;
  }

  .facts {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    gap: 4px var(--sp-3);
    margin: 0;
    font-size: var(--text-sm);
  }
  .facts dt {
    color: var(--fg-subtle);
  }
  .facts dd {
    margin: 0;
    color: var(--fg);
    overflow-wrap: anywhere;
    user-select: text;
  }
  .faint {
    color: var(--fg-faint);
  }
  .path {
    font-family: var(--font-mono);
    font-size: var(--text-xs);
  }
  .gps {
    margin: 0;
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    user-select: text;
  }

  .uses {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .use {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    column-gap: var(--sp-2);
    width: 100%;
    padding: var(--sp-2);
    border-radius: var(--radius-sm);
    text-align: left;
  }
  .use:hover {
    background: var(--bg-hover);
  }
  .use-kind {
    grid-column: 1 / -1;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .use-title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--accent);
  }
  .use-when {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }

  .actions {
    flex-direction: row;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }
  /* The sentence after "Add to library" takes the row to itself. */
  .actions .made {
    flex: 1 1 100%;
  }
  .foot {
    gap: var(--sp-2);
    align-items: flex-start;
  }
  .toggle {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    cursor: pointer;
  }
  .toggle input {
    margin-top: 3px;
  }
  .toggle span {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .toggle b {
    font-size: var(--text-sm);
    font-weight: 600;
  }
  .toggle small {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    line-height: var(--leading-snug);
  }
</style>
