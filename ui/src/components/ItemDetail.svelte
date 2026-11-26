<script lang="ts">
  // The detail panel of one library item: what it is, where you are in it,
  // your notes and history, and what it was looked up as.
  //
  // An item can hold files -- a book's EPUB, an article's PDF -- and then it
  // is read from here: **Read** opens the first of them in the reader, **Ask**
  // opens it with the assistant's rail beside it, and progress the reader
  // keeps (the page you are at) is shown under them. The first file is the
  // reading copy; any other can be made it. Files are never copied in: they
  // are chosen from the files folder (see `docs/plans/reading.md`).
  //
  // Any item can be put in **Up Next**, the Feed's queue of what to read and
  // hear next -- an article saved with nothing but its link included, which
  // Up Next opens where it lives; the button says when it is already there.

  import { tick } from 'svelte'
  import { api } from '../lib/api'
  import { formatName, isReadable } from '../lib/documents'
  import { lookupFiles } from '../lib/file-lookup'
  import { formatSize } from '../lib/files'
  import { files } from '../lib/files.svelte'
  import { friendlyDate, plural, pluralWord } from '../lib/format'
  import { ask, quick, slot } from '../lib/quick.svelte'
  import { focusOnMount } from '../lib/focus'
  import { coverRatio, library } from '../lib/library.svelte'
  import { phone } from '../lib/phone.svelte'
  import { ratingLabel } from '../lib/rating'
  import { reader } from '../lib/reader.svelte'
  import { app } from '../lib/state.svelte'
  import { upnext } from '../lib/upnext.svelte'
  import { looksThingsUp, sourceLabel } from '../lib/websearch'
  import { ITEM_STATUSES, VIAS } from '../lib/types'
  import type {
    FieldDef,
    FileEntry,
    FileId,
    Item,
    ItemId,
    KindInfo,
    LogEvent,
    QuickFields,
    CatchUpVia,
    SearchResult,
  } from '../lib/types'
  import {
    VIA_LABELS,
    every,
    isContactShelf,
    lastLine,
    catchUpSource,
    lateness,
  } from '../lib/keepintouch'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Cover from './Cover.svelte'
  import CoverSearch from './CoverSearch.svelte'
  import FilePicker from './FilePicker.svelte'
  import Icon from './Icon.svelte'
  import PhoneBack from './PhoneBack.svelte'
  import Rating from './Rating.svelte'
  import Suggestions from './Suggestions.svelte'

  let {
    item,
    kind,
  }: {
    item: Item
    kind: KindInfo | null
  } = $props()

  const color = $derived(kind?.color ?? 'var(--accent)')
  /** What the shelf calls one of these, for a sentence. */
  const noun = $derived(kind?.singular.toLowerCase() ?? 'item')
  const unit = $derived(item.progress?.unit || kind?.progressUnit || '')
  /** A person, whose log is catch-ups and who may be due one. */
  const person = $derived(isContactShelf(kind))
  const due = $derived(person ? library.dueOf(item.id) : null)

  let confirming = $state(false)
  let tagDraft = $state('')

  /** Where the way back on a phone goes: the shelf the item was opened from. */
  const shelfName = $derived(
    library.favouritesOnly ? 'Favourites' : (library.kind?.name ?? 'Library'),
  )

  // ── choosing a cover by hand ─────────────────────────────────────────
  //
  // For when no lookup finds the right edition, or finds nothing at all: a
  // file off the disk, the address of a picture somebody has open in a
  // browser, or a search for pictures of the thing. All three are stored in
  // the vault; none is ever hot-linked.

  let coverInput = $state<HTMLInputElement | null>(null)
  let coverUrlDraft = $state<string | null>(null)
  /**
   * Which item the picture search is open for. An id rather than a flag so
   * that moving on to another item closes it: a picture searched for one
   * book must not be one click from landing on the next.
   */
  let coverSearchFor = $state<ItemId | null>(null)

  /** The address form and the search share a place; one at a time. */
  function toggleCoverUrl() {
    coverSearchFor = null
    coverUrlDraft = coverUrlDraft === null ? '' : null
  }

  function toggleCoverSearch() {
    coverUrlDraft = null
    coverSearchFor = coverSearchFor === item.id ? null : item.id
  }

  function pickedCover(e: Event & { currentTarget: HTMLInputElement }) {
    const file = e.currentTarget.files?.[0]
    e.currentTarget.value = ''
    if (file) void library.chooseCover(item.id, file)
  }

  async function useCoverUrl() {
    const url = coverUrlDraft?.trim()
    if (!url) return
    coverUrlDraft = null
    await library.chooseCover(item.id, url)
  }

  // ── its files, and reading it ────────────────────────────────────────
  //
  // `item.files` is ids; what a row says -- a name, a size, whether it is
  // still there -- is the catalog's, asked for again whenever the list
  // changes. A file that is no longer in the catalog is said to be gone
  // rather than left out, so it can still be taken off.

  /** The catalog's record of each file the item holds, by id. */
  let held = $state.raw<Record<FileId, FileEntry>>({})
  /** The record above is for the files the item holds now. */
  let heldReady = $state(false)
  let picking = $state(false)

  $effect(() => {
    const ids = [...item.files]
    heldReady = false
    if (ids.length === 0) {
      held = {}
      return
    }
    let stale = false
    lookupFiles(ids, { fresh: true })
      .then((found) => {
        if (stale) return
        held = Object.fromEntries(found.map((f) => [f.id, f]))
        heldReady = true
      })
      .catch(() => {
        // The catalog could not be asked: the rows say what they can.
        if (!stale) heldReady = true
      })
    return () => {
      stale = true
    }
  })

  const readableFile = (id: FileId): boolean => {
    const file = held[id]
    return file !== undefined && isReadable(file)
  }
  /** The file Read opens: the reading copy, or -- if that is not something
   *  the reader can open -- the first that is. Until the catalog has said,
   *  the reading copy, which is what it will be nearly always. */
  const readingFile = $derived(heldReady ? item.files.find(readableFile) : item.files[0])

  /** Where the reader left off, in the shelf's own unit: "Page 47 of 312". */
  const readerProgress = $derived.by(() => {
    const p = item.progress
    if (!p || p.position <= 0 || item.files.length === 0) return ''
    const word = p.unit ? p.unit.charAt(0).toUpperCase() + p.unit.slice(1) : ''
    return `${word ? word + ' ' : ''}${p.position}${p.total ? ` of ${p.total}` : ''}`
  })

  /** The item is in Up Next. Up Next is loaded with the window (`PlayerHost`),
   *  so this is right on the first draw. */
  const queued = $derived(upnext.isQueued({ type: 'item', id: item.id }))

  function toggleQueued() {
    const what = { type: 'item' as const, id: item.id }
    if (queued) void upnext.removeFor(what)
    else void upnext.add(what)
  }

  function read(id: FileId | undefined = readingFile) {
    if (id) void reader.openFile(id, { item: item.id })
  }

  /** Open it with the assistant's rail beside it, ready for a question. */
  function askAbout() {
    const id = readingFile
    if (id) void reader.openFile(id, { item: item.id }).then(() => reader.ask())
  }

  function factsOf(file: FileEntry): string {
    return [
      formatName(file),
      formatSize(file.size),
      file.status.state === 'trashed' ? 'In the trash' : '',
      file.status.state === 'missing' ? 'Missing from the folder' : '',
    ]
      .filter(Boolean)
      .join(' · ')
  }

  function attach(picked: FileEntry[]) {
    picking = false
    if (picked.length > 0) {
      void library.attachFiles(
        item.id,
        picked.map((f) => f.id),
      )
    }
  }

  /** Move a file to the front: it is the one Read opens and the assistant
   *  reads. Saved as any other edit to the item is. */
  function makeReadingCopy(id: FileId) {
    edit(() => (item.files = [id, ...item.files.filter((f) => f !== id)]))
  }

  /** Show it in Files, where a missing or trashed file can be put back. */
  async function showInFiles(id: FileId) {
    if (!(await app.goTo('files'))) return
    await files.start()
    files.focus(id)
  }

  // ── re-looking something up ──────────────────────────────────────────
  //
  // Offered rather than automatic. The metadata on an item is only ever
  // refreshed because somebody asked, so a shelf never changes under you
  // while you are reading it.

  let matches = $state<SearchResult[]>([])
  let matching = $state(false)
  let matchError = $state<string | null>(null)
  let showMatches = $state(false)

  // ── what the quick model found ───────────────────────────────────────
  //
  // Two jobs, and they sit at opposite ends of the same lookup. `library.pick`
  // decides which of six results is the thing; `library.fields` reads the top
  // few and fills in the shelf's own declared fields, which is the only way
  // an article, a recipe or a shelf somebody invented ever gets anything but
  // a blurb -- `SearchResult::facts` is empty for a plain web search.
  //
  // Neither writes. The fields arrive as chips, one per field, because
  // accepting "the author is Frank Herbert" and rejecting "the year is 2021"
  // has to be two decisions.

  let fieldChips = $state<{ key: string; label: string }[]>([])
  let fieldValues = $state<Record<string, string>>({})
  let fieldsBusy = $state(false)
  const fieldSlot = slot<QuickFields>()

  async function suggestFields() {
    fieldsBusy = true
    await fieldSlot.run(
      'library.fields',
      () => api.quickItemFields(item.id),
      (found) => {
        fieldsBusy = false
        if (!found) return
        const chips: { key: string; label: string }[] = []
        const values: Record<string, string> = {}
        // Gaps only. What is already on the record survives a lookup that
        // disagrees with it -- the same rule `websearch::apply` enforces in
        // Rust, applied here so the chip is never even offered.
        if (found.creator && !item.creator.trim()) {
          chips.push({ key: 'creator', label: found.creator })
          values.creator = found.creator
        }
        if (found.year != null && item.year == null) {
          chips.push({ key: 'year', label: String(found.year) })
          values.year = String(found.year)
        }
        for (const [key, value] of Object.entries(found.facts)) {
          if ((item.facts[key] ?? '').trim()) continue
          const label = kind?.fields.find((f) => f.key === key)?.label ?? key
          chips.push({ key: `fact:${key}`, label: `${label}: ${value}` })
          values[`fact:${key}`] = value
        }
        fieldChips = chips
        fieldValues = values
      },
    )
    fieldsBusy = false
  }

  function acceptField(key: string) {
    const value = fieldValues[key]
    if (value == null) return
    edit(() => {
      if (key === 'creator') item.creator = value
      else if (key === 'year') item.year = Number(value)
      else item.facts[key.slice('fact:'.length)] = value
    })
  }

  async function findMatches() {
    if (!kind) return
    showMatches = true
    matching = true
    matchError = null
    try {
      // Through the store, not `web` directly: it is what routes a vault
      // that locked mid-lookup to the lock screen rather than leaving an
      // unhandled rejection and "Nothing found" on screen.
      const outcome = await library.lookup(kind.id, item.title, 6)
      matches = outcome.results
      matchError = outcome.error
      // Which of them is the thing. A hint drawn on one row rather than a
      // choice made for anybody: the list is on screen and somebody is
      // looking at it, so the most this is allowed to do is point.
      suggested = null
      if (matches.length > 1) {
        void ask('library.pick', () =>
          api.quickPickResult({ kindId: kind.id, query: item.title, results: matches }),
        ).then((index) => {
          suggested = index
        })
      }
    } finally {
      matching = false
    }
  }

  /** Which result the quick model thinks is the thing. Null is a real answer. */
  let suggested = $state<number | null>(null)

  async function choose(hit: SearchResult) {
    showMatches = false
    // `true`: this is the explicit re-fetch, so it may replace the byline and
    // the facts a previous lookup filled in. It still cannot touch the notes,
    // the rating or the status -- that rule lives in the Rust core.
    await library.applyMetadata(item.id, hit, true)
  }

  // ── editing ──────────────────────────────────────────────────────────

  function edit(patch: () => void) {
    patch()
    library.touch(item.id)
  }

  function addTag() {
    const tag = tagDraft.trim().replace(/^#/, '')
    tagDraft = ''
    if (!tag || item.tags.some((t) => t.toLowerCase() === tag.toLowerCase())) return
    edit(() => item.tags.push(tag))
  }

  function setFact(key: string, value: string) {
    edit(() => {
      if (value.trim()) item.facts[key] = value
      // A field cleared to nothing is a fact removed, not a fact that is the
      // empty string -- otherwise the detail panel keeps drawing a blank row.
      else delete item.facts[key]
    })
  }

  function setYear(raw: string) {
    const year = Number.parseInt(raw, 10)
    edit(() => (item.year = Number.isFinite(year) ? year : null))
  }

  async function commitProgress(raw: string) {
    const position = Number.parseInt(raw, 10)
    if (!Number.isFinite(position) || position < 0) return
    await library.setProgress(item.id, position, item.progress?.total ?? null, true)
  }

  function setTotal(raw: string) {
    const total = Number.parseInt(raw, 10)
    edit(() => {
      const position = item.progress?.position ?? 0
      item.progress = {
        position,
        total: Number.isFinite(total) && total > 0 ? total : null,
        unit: unit || 'step',
      }
    })
  }

  /**
   * What a log row says, in this shelf's language.
   *
   * `verbs.log` is the past participle -- "read", "watched", "ate at" -- so
   * it lands mid-sentence and the row reads as a sentence rather than as a
   * database event name.
   */
  function logLabel(event: LogEvent): string {
    const verb = kind?.verbs.log ?? ''
    switch (event) {
      case 'started':
        return 'Started'
      // The verb alone: "Read", "Watched", "Ate at". Saying "Finished — read
      // it" puts the same fact twice on a line that is already narrow.
      case 'finished':
        return verb ? verb[0]!.toUpperCase() + verb.slice(1) : 'Finished'
      // "Again" rather than the verb again, because "Ate at again" is a
      // sentence no shelf's wording makes read well.
      case 'revisited':
        return 'Again'
      case 'contact':
        return 'Caught up'
      case 'progress':
        return 'Progress'
      case 'stopped':
        return 'Set aside'
      default:
        return 'Note'
    }
  }

  const shownFields = $derived(kind?.fields ?? [])
  /** The one-line fields, drawn as rows of the details. */
  const rowFields = $derived(shownFields.filter((f) => f.fieldType !== 'list'))

  // ── lists: ingredients, a method ─────────────────────────────────────
  //
  // A list wants the panel's width and as many lines as it has, so it is a
  // section of its own rather than a row squeezed beside a label. One with
  // nothing in it is only a button, because the Drinks shelf has a method
  // for a cocktail and none for a beer, and two empty boxes on every beer
  // would be the panel asking a question that does not apply.

  /** `item:key` for each empty list somebody has opened, or is typing in. */
  let opened = $state<string[]>([])
  const listFields = $derived(shownFields.filter((f) => f.fieldType === 'list'))
  const isOpen = (field: FieldDef) =>
    (item.facts[field.key] ?? '').trim() !== '' || opened.includes(`${item.id}:${field.key}`)
  const openLists = $derived(listFields.filter(isOpen))
  const closedLists = $derived(listFields.filter((f) => !isOpen(f)))

  /** Keep a list on screen while it is being written in, even emptied. */
  function holdOpen(field: FieldDef) {
    const key = `${item.id}:${field.key}`
    if (!opened.includes(key)) opened = [...opened, key]
  }

  async function openList(field: FieldDef) {
    holdOpen(field)
    await tick()
    document.getElementById(`list-${field.key}`)?.focus()
  }

  /**
   * What a text field offers as you type: the shelf's suggestions, then
   * whatever has already been written in that field on this shelf -- so the
   * "Waterfall" you typed on one place is offered on the next.
   */
  function choicesFor(field: FieldDef): string[] {
    if (field.fieldType !== 'text') return []
    const seen = new Set<string>()
    const out: string[] = []
    const used = library.items
      .filter((i) => i.kindId === item.kindId)
      .map((i) => i.facts[field.key]?.trim() ?? '')
    for (const value of [...(field.suggestions ?? []), ...used.sort()]) {
      const folded = value.toLowerCase()
      if (!value || seen.has(folded)) continue
      seen.add(folded)
      out.push(value)
    }
    return out
  }
  /**
   * Facts with no field definition behind them.
   *
   * They arise when a field is removed from a shelf: the value is kept
   * deliberately (see `Kind::field_label`), so putting the field back brings
   * four hundred books' worth of it with it. Shown greyed, with a way to
   * delete, rather than hidden — a value you cannot see or remove is worse
   * than one you can do both to.
   */
  const orphanFacts = $derived(
    Object.keys(item.facts)
      .filter((key) => !shownFields.some((f) => f.key === key))
      .sort(),
  )
</script>

<aside class="detail" style="--tint: {color}">
  <header class="top">
    <PhoneBack onclick={() => library.close()} label={shelfName} />
    <span class="where">{kind?.icon ?? ''} {kind?.singular ?? 'Item'}</span>
    <span class="spacer"></span>
    <button
      class="icon-btn"
      class:on={item.favourite}
      title={item.favourite ? 'Remove from favourites' : 'Add to favourites'}
      aria-label="Favourite"
      onclick={() => void library.toggleFavourite(item.id)}
    >
      <Icon name="star" size={15} filled={item.favourite} />
    </button>
    <button
      class="icon-btn danger"
      title="Delete"
      aria-label="Delete"
      onclick={() => (confirming = true)}
    >
      <Icon name="trash" size={15} />
    </button>
    <button
      class="icon-btn desktop-only"
      title="Close"
      aria-label="Close"
      onclick={() => library.close()}
    >
      <Icon name="close" size={15} />
    </button>
  </header>

  <div class="scroll body">
    <div class="hero">
      <div class="art">
        <button
          class="cover-pick"
          title="Choose a picture"
          disabled={library.enriching}
          onclick={() => coverInput?.click()}
        >
          <Cover
            blob={item.cover}
            title={item.title}
            icon={kind?.icon ?? ''}
            {color}
            ratio={coverRatio(kind)}
          />
        </button>
        <input
          bind:this={coverInput}
          class="hidden"
          type="file"
          accept="image/*"
          aria-label="Cover picture"
          onchange={pickedCover}
        />
        <div class="cover-actions" class:crowded={!!item.cover}>
          <button
            class="cover-act"
            title="Choose a picture file"
            aria-label="Choose a picture"
            disabled={library.enriching}
            onclick={() => coverInput?.click()}
          >
            <Icon name="image" size={12} /> <span class="act-word">Choose</span>
          </button>
          <button
            class="cover-act"
            class:on={coverSearchFor === item.id}
            title="Search for a picture"
            aria-label="Search for a picture"
            aria-expanded={coverSearchFor === item.id}
            disabled={library.enriching}
            onclick={toggleCoverSearch}
          >
            <Icon name="search" size={12} />
          </button>
          <button
            class="cover-act"
            class:on={coverUrlDraft !== null}
            title="Use the address of a picture"
            aria-label="Use the address of a picture"
            aria-expanded={coverUrlDraft !== null}
            disabled={library.enriching}
            onclick={toggleCoverUrl}
          >
            <Icon name="link" size={12} />
          </button>
          {#if item.cover}
            <button
              class="cover-act"
              title="Remove the cover"
              aria-label="Remove the cover"
              onclick={() => library.removeCover(item.id)}
            >
              <Icon name="close" size={12} />
            </button>
          {/if}
        </div>
        {#if !item.cover && item.coverUrl}
          <!-- The address is known and the picture is not here: a lookup that
               found the details while the network was flaky. One button
               rather than a second search. -->
          <button
            class="cover-fetch"
            disabled={library.enriching}
            onclick={() => void library.fetchCover(item.id)}
          >
            <Icon name="image" size={13} /> Get the cover
          </button>
        {/if}
      </div>

      <div class="head">
        <input
          class="title"
          value={item.title}
          placeholder="Title"
          aria-label="Title"
          oninput={(e) => edit(() => (item.title = e.currentTarget.value))}
        />
        <input
          class="creator subtitle"
          value={item.subtitle}
          placeholder="Subtitle"
          aria-label="Subtitle"
          oninput={(e) => edit(() => (item.subtitle = e.currentTarget.value))}
        />
        <input
          class="creator"
          value={item.creator}
          placeholder="Who made it"
          aria-label="Creator"
          oninput={(e) => edit(() => (item.creator = e.currentTarget.value))}
        />

        <div class="mine">
          <Rating
            value={item.rating}
            size={phone.on ? 28 : 19}
            showNumber
            label="Your rating"
            onchange={(v: number | null) => void library.rate(item.id, v)}
          />
        </div>

        {#each item.external as rating (rating.source)}
          <div class="theirs" title="{rating.source}, not your rating">
            <Rating value={rating.score} size={11} readonly muted />
            <!-- The separator is interpolated rather than written as text
                 between the tags: Svelte collapses the newline the formatter
                 puts there, so a literal "·" loses the space before it. -->
            <span>
              {ratingLabel(rating.score)} on {rating.source}{rating.count
                ? ` \u00b7 ${plural(rating.count, 'rating')}`
                : ''}
            </span>
          </div>
        {/each}

        <!-- Reading it, when it has a file the reader can open: Read, or
             ask about it; and putting it in Up Next, whatever it is --
             then where the reader left off. -->
        <div class="reading">
          {#if readingFile}
            <button
              class="btn btn-sm btn-primary"
              title="Read it, with its contents beside it"
              onclick={() => read()}
            >
              <Icon name="book" size={14} /> Read
            </button>
            <button
              class="btn btn-sm btn-outline"
              title="Read it with the assistant beside it, ready for a question"
              onclick={askAbout}
            >
              <Icon name="sparkle" size={14} /> Ask
            </button>
          {/if}
          <button
            class="btn btn-sm btn-outline"
            aria-pressed={queued}
            title={queued ? 'In Up Next: take it out' : 'Add it to Up Next'}
            onclick={toggleQueued}
          >
            <Icon name="queue-add" size={14} />
            {queued ? 'In Up Next' : 'Add to Up Next'}
          </button>
        </div>
        {#if readingFile && readerProgress}<span class="read-at">{readerProgress}</span>{/if}
      </div>
    </div>

    {#if coverUrlDraft !== null}
      <form
        class="cover-url"
        onsubmit={(e) => {
          e.preventDefault()
          void useCoverUrl()
        }}
      >
        <input
          type="url"
          placeholder="https://… address of a picture"
          aria-label="Picture address"
          bind:value={coverUrlDraft}
          use:focusOnMount
          onkeydown={(e) => {
            if (e.key === 'Escape') coverUrlDraft = null
          }}
        />
        <button class="mini" type="submit" disabled={!coverUrlDraft.trim()}>Use</button>
      </form>
    {:else if coverSearchFor === item.id}
      <CoverSearch {item} ratio={coverRatio(kind)} onclose={() => (coverSearchFor = null)} />
    {/if}

    <!-- Status: five buttons, in this shelf's own words. A select would be
         one click shorter to build and one click longer to use, and this is
         the control the app is really about. -->
    <div class="statuses" role="group" aria-label="Status">
      {#each ITEM_STATUSES as status (status)}
        <button
          class="status"
          class:on={item.status === status}
          onclick={() => void library.setStatus(item.id, status)}
        >
          {library.label(kind, status)}
        </button>
      {/each}
    </div>

    {#if person}
      <!-- Keeping in touch, in one line: how often, how it stands, and when
           last. Somebody with no circle and no frequency is never suggested,
           and says so rather than leaving the line out. -->
      {#if due}
        <p class="touch" class:late={due.daysOver > 0}>
          <span class="lead">{lateness(due)}</span>
          · {every(due.everyDays)} · {lastLine(due, friendlyDate)}
        </p>
      {:else}
        <p class="touch">
          Give them a circle, or a number of days, to be reminded to keep in touch.
        </p>
      {/if}
    {/if}

    {#if item.startedOn || item.finishedOn}
      <!-- `friendlyDate`, not the long form: two full weekday-and-month dates
           on one line wrap in a 380px panel, and the same shortening is
           already what the history below uses. -->
      <p class="dates">
        {#if item.startedOn}Started {friendlyDate(item.startedOn)}{/if}
        {#if item.startedOn && item.finishedOn}
          ·
        {/if}
        {#if item.finishedOn}Finished {friendlyDate(item.finishedOn)}{/if}
      </p>
    {/if}

    {#if kind?.progressUnit}
      <section>
        <h3>Where you are</h3>
        <div class="progress">
          <input
            class="num"
            type="number"
            min="0"
            value={item.progress?.position ?? ''}
            aria-label="Position"
            onchange={(e) => void commitProgress(e.currentTarget.value)}
          />
          <span class="of">of</span>
          <input
            class="num"
            type="number"
            min="0"
            placeholder="?"
            value={item.progress?.total ?? ''}
            aria-label="Total"
            onchange={(e) => setTotal(e.currentTarget.value)}
          />
          <span class="unit">{pluralWord(item.progress?.total ?? 2, unit || 'step')}</span>
        </div>
        {#if item.progress?.total}
          <div class="track" aria-hidden="true">
            <span
              style="width: {Math.min(100, (item.progress.position / item.progress.total) * 100)}%"
            ></span>
          </div>
        {/if}
      </section>
    {/if}

    {#if files.supported}
      {#if item.files.length > 0}
        <section>
          <div class="head-row">
            <h3>Files</h3>
            <button class="mini" onclick={() => (picking = true)}>
              <Icon name="paperclip" size={12} /> Attach a file…
            </button>
          </div>
          <ul class="held">
            {#each item.files as id, i (id)}
              {@const file = held[id]}
              <li class="held-row" class:gone={heldReady && !file}>
                <span class="held-icon"><Icon name="file" size={15} /></span>
                <span class="held-text">
                  {#if file && readableFile(id)}
                    <button class="held-name" title="Read it" onclick={() => read(id)}>
                      {file.name}
                    </button>
                  {:else}
                    <span class="held-name">
                      {file ? file.name : heldReady ? 'No longer in your files' : '…'}
                    </span>
                  {/if}
                  <span class="held-facts">
                    {#if i === 0}<span class="copy">Reading copy</span>{/if}
                    {file ? factsOf(file) : ''}
                  </span>
                </span>
                <span class="held-acts">
                  {#if i > 0}
                    <button
                      class="held-act"
                      title="Make it the reading copy"
                      aria-label="Make {file?.name ?? 'this file'} the reading copy"
                      onclick={() => makeReadingCopy(id)}
                    >
                      <Icon name="book" size={12} />
                    </button>
                  {/if}
                  <button
                    class="held-act"
                    title="Show it in Files"
                    aria-label="Show {file?.name ?? 'this file'} in Files"
                    onclick={() => void showInFiles(id)}
                  >
                    <Icon name="folder" size={12} />
                  </button>
                  <button
                    class="held-act"
                    title="Take it off this {noun}. The file stays in Files."
                    aria-label="Take {file?.name ?? 'this file'} off"
                    onclick={() => void library.detachFile(item.id, id)}
                  >
                    <Icon name="close" size={12} />
                  </button>
                </span>
              </li>
            {/each}
          </ul>
        </section>
      {:else}
        <!-- Nothing attached, so only a way to: most shelves never hold a file,
             and an empty heading on every film would be asking a question that
             does not apply. -->
        <div class="add-lists">
          <button class="mini" onclick={() => (picking = true)}>
            <Icon name="paperclip" size={12} /> Attach a file…
          </button>
        </div>
      {/if}
    {/if}

    <section>
      <h3>About</h3>
      <!-- The blurb, usually in the source's words, and editable because a
           source gets it wrong. What you think still goes in Notes: keeping
           the two apart is what lets a re-fetch replace this and never that. -->
      <textarea
        class="summary"
        rows="3"
        placeholder="What it is"
        value={item.summary}
        oninput={(e) => edit(() => (item.summary = e.currentTarget.value))}
      ></textarea>
    </section>

    {#each openLists as field (field.key)}
      <section>
        <h3><label for="list-{field.key}">{field.label}</label></h3>
        <textarea
          id="list-{field.key}"
          class="list"
          rows="3"
          placeholder={field.placeholder || 'One to a line'}
          value={item.facts[field.key] ?? ''}
          onfocus={() => holdOpen(field)}
          oninput={(e) => setFact(field.key, e.currentTarget.value)}
        ></textarea>
      </section>
    {/each}
    {#if closedLists.length > 0}
      <div class="add-lists">
        {#each closedLists as field (field.key)}
          <button class="mini" onclick={() => void openList(field)}>
            <Icon name="plus" size={12} />
            {field.label}
          </button>
        {/each}
      </div>
    {/if}

    <section>
      <h3>Notes</h3>
      <textarea
        class="notes"
        rows="4"
        placeholder="What you thought…"
        value={item.notes}
        oninput={(e) => edit(() => (item.notes = e.currentTarget.value))}
      ></textarea>
    </section>

    <section>
      <h3>Details</h3>
      <dl class="facts">
        <dt>Year</dt>
        <dd>
          <input
            type="number"
            value={item.year ?? ''}
            aria-label="Year"
            oninput={(e) => setYear(e.currentTarget.value)}
          />
        </dd>
        {#each rowFields as field (field.key)}
          <dt>{field.label}</dt>
          <dd>
            {#if field.fieldType === 'multiline'}
              <textarea
                rows="2"
                placeholder={field.placeholder}
                value={item.facts[field.key] ?? ''}
                oninput={(e) => setFact(field.key, e.currentTarget.value)}
              ></textarea>
            {:else}
              {@const choices = choicesFor(field)}
              <input
                type={field.fieldType === 'number'
                  ? 'number'
                  : field.fieldType === 'date'
                    ? 'date'
                    : 'text'}
                placeholder={field.placeholder}
                value={item.facts[field.key] ?? ''}
                list={choices.length > 0 ? `fact-choices-${field.key}` : undefined}
                oninput={(e) => setFact(field.key, e.currentTarget.value)}
              />
              {#if choices.length > 0}
                <datalist id="fact-choices-{field.key}">
                  {#each choices as choice (choice)}
                    <option value={choice}></option>
                  {/each}
                </datalist>
              {/if}
            {/if}
          </dd>
        {/each}
        {#each orphanFacts as key (key)}
          <dt class="orphan" title="This shelf no longer has a field called “{key}”">{key}</dt>
          <dd>
            <input
              value={item.facts[key] ?? ''}
              oninput={(e) => setFact(key, e.currentTarget.value)}
            />
          </dd>
        {/each}
      </dl>
    </section>

    <section>
      <h3>Tags</h3>
      <div class="tags">
        {#each item.tags as tag (tag)}
          <button
            class="tag"
            title="Remove"
            onclick={() => edit(() => (item.tags = item.tags.filter((t) => t !== tag)))}
          >
            {tag}<Icon name="close" size={11} />
          </button>
        {/each}
        <input
          class="tag-add"
          placeholder="Add a tag"
          bind:value={tagDraft}
          onblur={addTag}
          onkeydown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault()
              addTag()
            }
          }}
        />
      </div>
    </section>

    {#if item.links.length > 0}
      <section>
        <h3>Links</h3>
        <ul class="links">
          {#each item.links as link (link.url)}
            <li>
              <!-- Opened in the system browser by the shell, never here: the
                   webview has no network permission at all. -->
              <a href={link.url} target="_blank" rel="noreferrer noopener">
                <Icon name="link" size={13} />{link.label || link.url}
              </a>
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    <section>
      <div class="head-row">
        <h3>History</h3>
        <div class="log-actions">
          <button class="mini" onclick={() => void library.addLog(item.id, 'note')}>
            <Icon name="plus" size={12} /> Note
          </button>
          <button
            class="mini"
            onclick={() => void library.addLog(item.id, person ? 'contact' : 'revisited')}
          >
            <Icon name="refresh" size={12} />
            {person ? 'Caught up' : 'Again'}
          </button>
        </div>
      </div>

      {#if library.logs.length === 0}
        <p class="empty">
          Nothing recorded yet. Marking this {kind?.singular.toLowerCase() ?? 'item'} as
          {library.label(kind, 'done').toLowerCase()} will add the first line.
        </p>
      {:else}
        <ol class="log">
          {#each library.logs as entry (entry.id)}
            <li>
              <span class="when">{friendlyDate(entry.date)}</span>
              <span class="what">
                {#if person && (entry.event === 'contact' || entry.event === 'revisited')}
                  <select
                    class="via"
                    aria-label="How you were in touch"
                    title={catchUpSource(entry.source) || 'How you were in touch'}
                    value={entry.via ?? ''}
                    onchange={(e) => {
                      entry.via = (e.currentTarget.value || null) as CatchUpVia | null
                      void library.saveLogEntry(entry)
                    }}
                  >
                    <option value="">Caught up</option>
                    {#each VIAS as via (via)}
                      <option value={via}>{VIA_LABELS[via]}</option>
                    {/each}
                  </select>
                {:else}
                  <span class="event">{logLabel(entry.event)}</span>
                {/if}
                {#if entry.position !== null && entry.position !== undefined}
                  <!-- `plural`, not the bare unit: "412 page" is a typo the
                     eye trips over every time it reads the column. -->
                  <span class="at">{plural(entry.position, unit || 'step')}</span>
                {/if}
                <input
                  class="log-note"
                  placeholder="Add a note"
                  value={entry.note}
                  oninput={(e) => (entry.note = e.currentTarget.value)}
                  onblur={() => void library.saveLogEntry(entry)}
                />
              </span>
              <button
                class="icon-btn small"
                title="Remove this line"
                aria-label="Remove this line"
                onclick={() => void library.removeLog(entry.id)}
              >
                <Icon name="close" size={12} />
              </button>
            </li>
          {/each}
        </ol>
      {/if}
    </section>

    <!-- Not on a shelf that looks nothing up: a person's card is filled in
         by the person who knows them, not by a search for their name. -->
    {#if !kind || looksThingsUp(kind)}
      <section>
        <div class="head-row">
          <h3>Metadata</h3>
          <div class="head-actions">
            {#if quick.enabled('library.fields')}
              <button class="mini" disabled={fieldsBusy} onclick={() => void suggestFields()}>
                <Icon name="sparkle" size={12} />
                Fill in the gaps
              </button>
            {/if}
            <button class="mini" disabled={library.enriching || matching} onclick={findMatches}>
              <Icon name="sparkle" size={12} />
              {item.source ? 'Look up again' : 'Look this up'}
            </button>
          </div>
        </div>

        <!-- One chip per field, because accepting the author and rejecting the
             year has to be two decisions. Only fields that are empty are ever
             offered: what you typed survives a lookup that disagrees. -->
        <Suggestions
          scope={item.id}
          items={fieldChips}
          busy={fieldsBusy}
          label="Found:"
          onaccept={acceptField}
          ondismiss={() => fieldSlot.dismiss(() => (fieldChips = []))}
        />

        {#if showMatches}
          {#if matching}
            <p class="empty">Searching…</p>
          {:else if matchError}
            <p class="empty err">{matchError}</p>
          {:else if matches.length === 0}
            <p class="empty">Nothing found for “{item.title}”.</p>
          {:else}
            <p class="empty">
              Picking one replaces the title, byline and details. Your rating, your notes, the
              history and any list you have written in are never touched.
            </p>
            <div class="matches">
              {#each matches as hit, i (hit.url || hit.title + i)}
                <button
                  class="match"
                  class:picked={suggested === i}
                  onclick={() => void choose(hit)}
                >
                  <span class="m-title">
                    {hit.title}
                    {#if suggested === i}
                      <span class="hint-tag"><Icon name="sparkle" size={10} /> likely</span>
                    {/if}
                  </span>
                  {#if hit.creator || hit.year}
                    <span class="m-sub"
                      >{hit.creator}{#if hit.creator && hit.year}&nbsp;·&nbsp;{/if}{hit.year ??
                        ''}</span
                    >
                  {/if}
                </button>
              {/each}
            </div>
          {/if}
        {:else if item.source}
          <p class="empty">Filled in from {sourceLabel(item.source)}.</p>
        {/if}
      </section>
    {/if}
  </div>
</aside>

{#if picking}
  <FilePicker
    title="Attach a file"
    note="Choose documents from your files. The first is the reading copy: the one Read opens."
    multiple
    confirm="Attach"
    taken={item.files}
    onchoose={attach}
    onclose={() => (picking = false)}
  />
{/if}

{#if confirming}
  <ConfirmDialog
    title={'Delete “' + item.title + '”?'}
    detail="Its notes, its rating and its whole history go with it. This cannot be undone."
    confirmLabel="Delete"
    onconfirm={() => {
      confirming = false
      void library.remove(item.id)
    }}
    oncancel={() => (confirming = false)}
  />
{/if}

<style>
  .head-actions {
    display: flex;
    gap: var(--sp-2);
  }

  /* A hint on one row, not a choice made for anybody: the list is on screen
     and somebody is looking at it, so the most this may do is point. */
  .match.picked {
    border-color: color-mix(in oklab, var(--accent) 40%, transparent);
    background: color-mix(in oklab, var(--accent) 7%, transparent);
  }

  .hint-tag {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    margin-left: var(--sp-2);
    color: var(--fg-subtle);
    font-size: var(--text-xs);
  }

  .detail {
    width: 380px;
    flex: none;
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: var(--bg-panel);
    border-left: 1px solid var(--border);
  }

  /* The same height as every other header in the window, and for the reason
     `--header-h` exists: this row is the top of the rightmost pane, so it sits
     on the same line as the brand row and the shelf header beside it. It was a
     bare 42px, which put it twelve pixels shy of both. */
  .top {
    display: flex;
    align-items: center;
    gap: 2px;
    height: var(--header-h);
    flex: none;
    padding: 0 var(--sp-2) 0 var(--sp-4);
    border-bottom: 1px solid var(--border);
  }
  .where {
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }
  .spacer {
    flex: 1;
  }

  .icon-btn {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: var(--radius-sm);
    color: var(--fg-subtle);
  }
  .icon-btn:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .icon-btn.on {
    color: var(--star);
  }
  .icon-btn.danger:hover {
    color: var(--danger);
  }
  .icon-btn.small {
    width: 20px;
    height: 20px;
    opacity: 0;
  }

  .body {
    flex: 1;
    padding: var(--sp-4);
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
  }

  .hero {
    display: flex;
    gap: var(--sp-3);
  }
  .art {
    width: 116px;
    flex: none;
  }
  .cover-pick {
    display: block;
    width: 100%;
    padding: 0;
    border-radius: var(--radius-sm);
  }
  .cover-pick:hover:not(:disabled) {
    opacity: 0.85;
  }
  .hidden {
    display: none;
  }
  .cover-actions {
    display: flex;
    gap: 2px;
    margin-top: var(--sp-1);
  }
  .cover-act {
    flex: 1 1 auto;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 3px;
    height: 22px;
    padding: 0 4px;
    border-radius: var(--radius-sm);
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    background: var(--bg-hover);
  }
  /* Only "Choose" carries a word: four labelled buttons do not fit under a
     116px cover, and the others say what they are on hover. */
  .cover-act + .cover-act {
    flex: none;
    width: 24px;
  }
  /* And not even "Choose" once a cover is there to be removed: four buttons
     leave it too narrow for the word, which would spill under its neighbour.
     The picture icon and the hover title still say what it does. */
  .cover-actions.crowded .act-word {
    display: none;
  }
  .cover-act:hover:not(:disabled),
  .cover-act.on {
    color: var(--fg);
    background: var(--bg-active);
  }
  .cover-act:disabled {
    opacity: 0.45;
  }
  .cover-url {
    display: flex;
    gap: var(--sp-2);
    margin-top: calc(var(--sp-3) * -1);
  }
  .cover-url input {
    flex: 1;
    min-width: 0;
    height: 26px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    font-size: var(--text-sm);
    color: var(--fg);
    user-select: text;
  }
  .cover-url input:focus {
    outline: none;
    border-color: var(--tint);
  }
  .cover-url .mini {
    height: 26px;
  }
  .cover-fetch {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 4px;
    width: 100%;
    margin-top: var(--sp-1);
    padding: 3px 0;
    border-radius: var(--radius-sm);
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    background: var(--bg-hover);
  }
  .cover-fetch:hover {
    color: var(--fg);
  }

  .head {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .title,
  .creator {
    width: 100%;
    border: 0;
    background: none;
    border-radius: var(--radius-sm);
    padding: 2px 4px;
    margin-left: -4px;
    user-select: text;
  }
  .title {
    font-family: var(--font-read);
    font-size: var(--text-lg);
    font-weight: 650;
    line-height: var(--leading-tight);
    color: var(--fg);
  }
  .creator {
    font-size: var(--text-base);
    color: var(--fg-muted);
  }
  /* A faint rule under each, so they read as fields you can type in and not
     as a heading: drawn borderless, nobody found out the title was editable. */
  .title,
  .creator {
    box-shadow: inset 0 -1px 0 var(--border);
  }
  .subtitle {
    font-size: var(--text-sm);
  }
  .title::placeholder,
  .creator::placeholder {
    color: var(--fg-faint);
  }
  .title:hover,
  .creator:hover {
    background: var(--bg-hover);
  }
  .title:focus,
  .creator:focus {
    outline: none;
    background: var(--bg-hover);
  }

  .mine {
    margin-top: var(--sp-2);
  }
  .theirs {
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }

  /* Read and Ask, under the rating, with where the reader left off. */
  .reading {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-1);
    margin-top: var(--sp-2);
  }
  .read-at {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }

  .statuses {
    display: flex;
    flex-wrap: wrap;
    gap: 3px;
    padding: 3px;
    border-radius: var(--radius);
    background: var(--bg-active);
  }
  .status {
    flex: 1 1 auto;
    height: 26px;
    padding: 0 var(--sp-2);
    border-radius: calc(var(--radius) - 3px);
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-subtle);
    white-space: nowrap;
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .status:hover {
    color: var(--fg);
  }
  .status.on {
    background: var(--tint);
    color: #fff;
    box-shadow: var(--shadow-sm);
  }

  .dates {
    margin: calc(var(--sp-4) * -1 + 2px) 0 0;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }

  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  h3 {
    margin: 0;
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }
  .head-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2);
  }
  .log-actions {
    display: flex;
    gap: var(--sp-1);
  }
  .mini {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 22px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-xs);
    font-weight: 550;
    color: var(--fg-subtle);
    background: var(--bg-hover);
  }
  .mini:hover:not(:disabled) {
    color: var(--fg);
    background: var(--bg-active);
  }
  .mini:disabled {
    opacity: 0.45;
  }

  .progress {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .num {
    width: 68px;
    height: 28px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    font-size: var(--text-base);
    font-variant-numeric: tabular-nums;
    color: var(--fg);
    user-select: text;
  }
  .of,
  .unit {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .track {
    height: 5px;
    border-radius: 999px;
    background: var(--bg-active);
    overflow: hidden;
  }
  .track span {
    display: block;
    height: 100%;
    background: var(--tint);
    transition: width var(--med) var(--ease);
  }

  .summary {
    width: 100%;
    padding: var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    resize: vertical;
    user-select: text;
    font-family: var(--font-read);
    font-size: var(--text-base);
    line-height: var(--leading-normal);
    color: var(--fg-muted);
  }

  .notes,
  .facts textarea,
  .facts input {
    width: 100%;
    padding: var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    font: inherit;
    font-size: var(--text-base);
    color: var(--fg);
    resize: vertical;
    user-select: text;
  }
  .notes {
    font-family: var(--font-read);
    line-height: var(--leading-normal);
  }
  /* As tall as what is in it, so a method is read rather than scrolled. */
  .list {
    width: 100%;
    min-height: 4.5em;
    padding: var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    field-sizing: content;
    resize: vertical;
    user-select: text;
    font-family: var(--font-read);
    font-size: var(--text-base);
    line-height: var(--leading-normal);
    color: var(--fg);
  }
  .add-lists {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-1);
  }

  /* The files an item holds: one line each, its actions shown when the row is
     pointed at or focused -- as the history's are. */
  .held {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .held-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-1) var(--sp-2);
    border-radius: var(--radius-sm);
    background: var(--bg-hover);
  }
  .held-icon {
    flex: none;
    display: grid;
    place-items: center;
    color: var(--fg-faint);
  }
  .held-text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .held-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    text-align: left;
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg);
  }
  button.held-name:hover {
    color: var(--tint);
    text-decoration: underline;
  }
  .held-row.gone .held-name {
    font-style: italic;
    font-weight: 400;
    color: var(--fg-faint);
  }
  .held-facts {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .copy {
    margin-right: var(--sp-1);
    font-weight: 600;
    color: var(--tint);
  }
  .held-acts {
    flex: none;
    display: flex;
    opacity: 0;
    transition: opacity var(--fast) var(--ease);
  }
  .held-row:hover .held-acts,
  .held-row:focus-within .held-acts {
    opacity: 1;
  }
  .held-act {
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: var(--radius-sm);
    color: var(--fg-subtle);
  }
  .held-act:hover {
    background: var(--bg-active);
    color: var(--fg);
  }
  .notes:focus,
  .list:focus,
  .summary:focus,
  .facts textarea:focus,
  .facts input:focus {
    outline: none;
    border-color: var(--tint);
  }

  .facts {
    display: grid;
    grid-template-columns: 96px 1fr;
    align-items: center;
    gap: var(--sp-2);
    margin: 0;
  }
  dt {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  dt.orphan {
    font-style: italic;
    color: var(--fg-faint);
  }
  dd {
    margin: 0;
    min-width: 0;
  }

  .tags {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-1);
    align-items: center;
  }
  .tag {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 22px;
    padding: 0 var(--sp-2);
    border-radius: 999px;
    font-size: var(--text-xs);
    color: var(--fg-muted);
    background: var(--bg-active);
  }
  .tag:hover {
    color: var(--danger);
  }
  .tag-add {
    height: 22px;
    min-width: 90px;
    flex: 1;
    padding: 0 var(--sp-2);
    border: 1px dashed var(--border-strong);
    border-radius: 999px;
    background: none;
    font-size: var(--text-xs);
    color: var(--fg);
    user-select: text;
  }
  .tag-add:focus {
    outline: none;
    border-style: solid;
    border-color: var(--tint);
  }

  .links {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .links a {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: var(--text-sm);
    color: var(--tint);
    text-decoration: none;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .links a:hover {
    text-decoration: underline;
  }

  .log {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }
  .log li {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    padding: var(--sp-1) 0;
    /* The rail: a history reads as one column of days, not as a table. */
    border-left: 2px solid var(--bg-active);
    padding-left: var(--sp-3);
    margin-left: 3px;
  }
  .log li:hover .icon-btn.small {
    opacity: 1;
  }
  .when {
    flex: none;
    width: 76px;
    font-size: var(--text-xs);
    color: var(--fg-faint);
    padding-top: 3px;
  }
  .what {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .touch {
    margin: var(--sp-2) 0 0;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .touch .lead {
    color: var(--fg);
    font-weight: 500;
  }
  .touch.late .lead {
    color: var(--tint, var(--accent));
  }
  .via {
    font: inherit;
    font-size: var(--text-sm);
    color: var(--fg-muted);
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius);
    padding: 0 var(--sp-1);
    max-width: 9rem;
  }
  .via:hover,
  .via:focus-visible {
    border-color: var(--border);
  }

  .event {
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-muted);
  }
  .at {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .log-note {
    width: 100%;
    border: 0;
    background: none;
    padding: 1px 0;
    font-size: var(--text-sm);
    color: var(--fg);
    user-select: text;
  }
  .log-note:focus {
    outline: none;
  }
  .log-note::placeholder {
    color: var(--fg-faint);
  }

  .empty {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
    color: var(--fg-subtle);
  }
  .empty.err {
    color: var(--danger);
  }

  .matches {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .match {
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding: var(--sp-2);
    border-radius: var(--radius-sm);
    text-align: left;
    background: var(--bg-hover);
  }
  .match:hover {
    background: var(--bg-active);
  }
  .m-title {
    font-size: var(--text-base);
    font-weight: 550;
    color: var(--fg);
  }
  .m-sub {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }

  /* ── On a phone ──────────────────────────────────────────────────────── */

  /* A page over the shelf (see `.layered` in `LibraryView`), not a column
     beside it. */
  :global([data-phone]) .detail {
    position: absolute;
    inset: 0;
    z-index: 3;
    width: auto;
    border-left: 0;
  }
  :global([data-phone]) .top {
    gap: 0;
    padding: 0 var(--sp-1);
  }
  /* The way back says where it goes; the shelf's name stands for the rest. */
  :global([data-phone]) .where {
    display: none;
  }
  :global([data-phone]) .top .icon-btn {
    width: 44px;
    height: 44px;
  }
  :global([data-phone]) .body {
    padding: var(--sp-4) var(--sp-4) var(--fab-clear);
  }

  /* Under the cover: two across, a finger's height, where a desktop has a
     row of small buttons that say what they are on hover. */
  :global([data-phone]) .cover-actions {
    flex-wrap: wrap;
    gap: 4px;
  }
  :global([data-phone]) .cover-act,
  :global([data-phone]) .cover-act + .cover-act {
    flex: 1 1 calc(50% - 2px);
    width: auto;
    height: 40px;
  }
  :global([data-phone]) .cover-actions:not(.crowded) .cover-act:first-child {
    flex-basis: 100%;
  }
  :global([data-phone]) .cover-fetch {
    height: 40px;
  }
  :global([data-phone]) .cover-url input,
  :global([data-phone]) .cover-url .mini {
    height: 40px;
  }

  :global([data-phone]) .statuses {
    gap: 4px;
  }
  :global([data-phone]) .status {
    flex: 1 1 28%;
    height: 40px;
  }

  :global([data-phone]) .mini,
  :global([data-phone]) .tag,
  :global([data-phone]) .tag-add {
    height: 36px;
  }
  :global([data-phone]) .mini {
    padding: 0 var(--sp-3);
  }
  :global([data-phone]) .tag {
    padding: 0 var(--sp-3);
  }
  :global([data-phone]) .num {
    height: 40px;
  }

  /* Actions that wait for a pointer to come near are shown. */
  :global([data-phone]) .held-acts {
    opacity: 1;
  }
  :global([data-phone]) .held-act {
    width: 40px;
    height: 40px;
  }
  :global([data-phone]) .icon-btn.small {
    width: 40px;
    height: 40px;
    margin: -6px -8px -6px 0;
    opacity: 0.6;
  }
  :global([data-phone]) .links a {
    min-height: 36px;
  }
  :global([data-phone]) .when {
    padding-top: 10px;
  }
  :global([data-phone]) .log-note {
    min-height: 36px;
  }
  :global([data-phone]) .event {
    padding-top: 6px;
  }
  :global([data-phone]) .via {
    min-height: 36px;
  }
  :global([data-phone]) .facts {
    grid-template-columns: 88px 1fr;
  }
  :global([data-phone]) .match {
    padding: var(--sp-3);
  }

  :global([data-phone]) button.held-name {
    padding: 8px 0;
  }
</style>
