<script lang="ts">
  import { onDestroy } from 'svelte'
  import { files } from '../lib/files.svelte'
  import { article } from '../lib/format'
  import { library } from '../lib/library.svelte'
  import { menu } from '../lib/menu.svelte'
  import { tidyMenu, type MenuItem } from '../lib/menu'
  import { onOffPref } from '../lib/prefs'
  import { app } from '../lib/state.svelte'
  import { looksThingsUp, web, type LiveOutcome } from '../lib/websearch'
  import type { FileEntry, KindInfo, SearchResult } from '../lib/types'
  import Cover from './Cover.svelte'
  import FilePicker from './FilePicker.svelte'
  import Icon from './Icon.svelte'
  import Rating from './Rating.svelte'

  let {
    kind,
  }: {
    /** The shelf a new thing goes on. Never null: the view guarantees one. */
    kind: KindInfo
  } = $props()

  // "Add a book", "Add an album" -- the shelf names its own singular, so the
  // article cannot be hard-coded either way.
  const noun = $derived(`Add ${article(kind.singular)} ${kind.singular.toLowerCase()}…`)

  /**
   * Whether the shelf is a choice here, or is simply where you are.
   *
   * On a shelf it is not: you opened Films, so what you type is a film, and
   * offering to file it under Books would be offering to put it somewhere
   * you cannot see. In the everything view it is the whole question -- see
   * `captureShelf` in the store for what used to happen instead.
   */
  const choosable = $derived(library.shelf === null && library.visibleKinds.length > 1)

  function shelfMenu(): MenuItem[] {
    return tidyMenu([
      { kind: 'heading', label: 'Add to' },
      ...library.visibleKinds.map((k) => ({
        label: k.name,
        dot: k.color,
        checked: k.id === kind.id,
        run: () => {
          library.setCaptureShelf(k.id)
          field?.focus()
        },
      })),
    ])
  }

  let draft = $state('')
  let field = $state<HTMLInputElement | null>(null)
  let busy = $state(false)
  const enrichPref = onOffPref('jatayu.library.enrich')
  /** Whether to go and look the title up. Remembered across the session. */
  let enrich = $state(enrichPref.get())
  /**
   * Whether this add will actually look anything up. Never on a shelf that
   * looks nothing up -- people -- whatever the toggle says elsewhere: a
   * friend's name is not a search query.
   */
  const lookingUp = $derived(enrich && looksThingsUp(kind))

  // ── the suggestion list ──────────────────────────────────────────────
  //
  // Typing a title looks it up as you go, and picking a suggestion adds the
  // thing *with its metadata already chosen* -- which is the difference
  // between "add a book and hope the lookup guesses right" and "add this
  // book". Pressing Enter without picking still adds exactly what was typed,
  // so the fast path is never blocked on the network.

  let hits = $state<SearchResult[]>([])
  let searching = $state(false)
  let searchError = $state<string | null>(null)
  let open = $state(false)
  /** Keyboard position in the list. -1 is the "just add what I typed" row. */
  let cursor = $state(-1)

  const search = web.live(
    (outcome: LiveOutcome) => {
      hits = outcome.results
      searching = outcome.searching
      searchError = outcome.error
      // Never move the cursor off the typed row on the strength of results
      // arriving: the person is still typing, and having Enter suddenly mean
      // something else is how a capture field loses a title.
      if (outcome.results.length === 0) cursor = -1
    },
    // A vault that locked while somebody was typing a title is a screen to
    // go to, not a red line in a dropdown they are about to be taken away
    // from.
    () => void app.lock(),
  )
  onDestroy(() => search.stop())

  // The store holds the focus hook so a quick action from the menu bar can
  // reach it. Same arrangement as the todo app's capture line.
  $effect(() => {
    library.bindCapture(() => field?.focus())
    return () => library.bindCapture(null)
  })

  function onInput(value: string) {
    draft = value
    open = value.trim().length > 1
    if (!lookingUp || !open) {
      hits = []
      searching = false
      search.stop()
      return
    }
    search.lookup(kind.id, value, 6)
  }

  function setEnrich(on: boolean) {
    enrich = on
    enrichPref.set(on)
    if (!on) {
      hits = []
      search.stop()
    } else if (lookingUp && draft.trim().length > 1) {
      search.lookup(kind.id, draft, 6)
    }
  }

  /** Add exactly what was typed. The path that never waits on a network. */
  async function addTyped() {
    const title = draft.trim()
    if (!title || busy) return
    busy = true
    // Cleared *before* the await, and focus kept, so a burst of titles costs
    // a burst of Enters. The todo app's capture line makes the same trade
    // and for the same reason.
    draft = ''
    hits = []
    open = false
    cursor = -1
    search.stop()
    try {
      await library.add(kind.id, title, lookingUp)
    } finally {
      busy = false
      field?.focus()
    }
  }

  /** Add the thing the person picked out of the list, metadata and all. */
  async function addHit(hit: SearchResult) {
    if (busy) return
    busy = true
    const title = hit.title || draft.trim()
    draft = ''
    hits = []
    open = false
    cursor = -1
    search.stop()
    try {
      // Added without a lookup -- we already have the answer -- and then
      // given the chosen result, which is also what fetches the cover.
      const item = await library.add(kind.id, title, false)
      if (item) await library.applyMetadata(item.id, hit, false)
    } finally {
      busy = false
      field?.focus()
    }
  }

  // ── from a file ──────────────────────────────────────────────────────
  //
  // A document in the files folder -- a paper's PDF, a book's EPUB -- is a
  // thing to put on a shelf as it is: the item is made from what the file
  // says about itself, and holds the file as its reading copy. On the
  // Articles shelf this is the natural way in; a journal article has no
  // title worth searching for until it has been read.

  let picking = $state(false)

  async function addFromFile(picked: FileEntry[]) {
    picking = false
    const file = picked[0]
    if (!file || busy) return
    busy = true
    try {
      // The store opens what it makes, so that what the file said about
      // itself can be seen and put right.
      await library.createFromFile(file.id, kind.id)
    } finally {
      busy = false
      field?.focus()
    }
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      if (open) {
        event.stopPropagation()
        open = false
        cursor = -1
      } else {
        draft = ''
      }
      return
    }
    if (event.key === 'Enter') {
      event.preventDefault()
      const hit = cursor >= 0 ? hits[cursor] : undefined
      void (hit ? addHit(hit) : addTyped())
      return
    }
    if (!open || hits.length === 0) return
    if (event.key === 'ArrowDown') {
      event.preventDefault()
      cursor = cursor + 1 >= hits.length ? -1 : cursor + 1
    } else if (event.key === 'ArrowUp') {
      event.preventDefault()
      cursor = cursor - 1 < -1 ? hits.length - 1 : cursor - 1
    }
  }
</script>

<div class="add">
  <div class="line" class:busy>
    {#if choosable}
      <!-- The shelf, as a button, at the head of the line it decides. It is
           where the shelf's emoji already was, so nothing moved: what was a
           label is now the control it always looked like. -->
      <button
        class="lead pick"
        data-menu-trigger
        title="Adding to {kind.name} — click to change"
        aria-label="Adding to {kind.name}. Change the shelf."
        aria-haspopup="menu"
        aria-expanded={menu.at !== null}
        onclick={(e) => (menu.at ? menu.close() : menu.show(e, shelfMenu()))}
      >
        <span aria-hidden="true">{kind.icon}</span>
        <Icon name="chevron" size={11} weight={2} />
      </button>
    {:else}
      <span class="lead" aria-hidden="true">{kind.icon}</span>
    {/if}
    <input
      bind:this={field}
      class="field"
      type="text"
      placeholder={noun}
      aria-label={noun}
      autocomplete="off"
      spellcheck="false"
      value={draft}
      oninput={(e) => onInput(e.currentTarget.value)}
      onkeydown={onKeydown}
      onfocus={() => (open = draft.trim().length > 1 && hits.length > 0)}
    />

    {#if looksThingsUp(kind)}
      <button
        class="toggle"
        class:on={enrich}
        title={enrich
          ? 'Looking up covers and details online. Click to stop.'
          : 'Adding exactly what you type. Click to look things up online.'}
        aria-pressed={enrich}
        onclick={() => setEnrich(!enrich)}
      >
        <Icon name="sparkle" size={14} filled={enrich} />
      </button>
    {/if}

    {#if files.supported}
      <button
        class="from-file"
        title="From a file in Files…"
        aria-label="Add from a file in Files"
        disabled={busy}
        onclick={() => (picking = true)}
      >
        <Icon name="file" size={14} />
      </button>
    {/if}

    <button class="go" disabled={!draft.trim() || busy} onclick={addTyped} title="Add (Enter)">
      <Icon name="plus" size={15} />
    </button>
  </div>

  {#if picking}
    <FilePicker
      title="Add from a file"
      note="Choose a document in your files. It goes on the {kind.name} shelf, filled in from what the file says about itself."
      onchoose={addFromFile}
      onclose={() => (picking = false)}
    />
  {/if}

  {#if open && (searching || hits.length > 0 || searchError)}
    <div class="results" role="listbox" aria-label="Suggestions">
      {#if searching && hits.length === 0}
        <div class="status">Looking up “{draft.trim()}”…</div>
      {/if}
      {#each hits as hit, i (hit.url || hit.title + i)}
        <button
          class="hit"
          class:on={cursor === i}
          role="option"
          aria-selected={cursor === i}
          onmousemove={() => (cursor = i)}
          onclick={() => void addHit(hit)}
        >
          <span class="thumb">
            <!-- Deliberately the placeholder, not the remote image: nothing
                 in the interface loads a picture off the internet, and a
                 suggestion has not been chosen yet, so nothing has been
                 downloaded. The cover arrives when the thing is added. -->
            <Cover title={hit.title} icon={kind.icon} color={kind.color} ratio="1 / 1" />
          </span>
          <span class="body">
            <span class="name">{hit.title}</span>
            {#if hit.creator || hit.year}
              <span class="sub"
                >{hit.creator}{#if hit.creator && hit.year}&nbsp;·&nbsp;{/if}{hit.year ?? ''}</span
              >
            {:else if hit.subtitle}
              <span class="sub">{hit.subtitle}</span>
            {/if}
          </span>
          {#if hit.rating !== null && hit.rating !== undefined}
            <span class="score"><Rating value={hit.rating} size={11} readonly muted /></span>
          {/if}
        </button>
      {/each}
      {#if searchError}
        <!-- Said plainly and never as a dialog. Nothing here is blocked by
             it: Enter still adds what was typed. -->
        <div class="status err">{searchError}</div>
      {:else if !searching && hits.length === 0 && draft.trim().length > 1}
        <div class="status">Nothing found. Press Enter to add it anyway.</div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .add {
    position: relative;
  }

  .line {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    height: 36px;
    padding: 0 var(--sp-1) 0 var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    transition:
      border-color var(--fast) var(--ease),
      box-shadow var(--fast) var(--ease);
  }
  .line:focus-within {
    border-color: var(--journal-accent, var(--accent));
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--journal-accent, var(--accent)) 16%, transparent);
  }
  .line.busy {
    opacity: 0.7;
  }

  .lead {
    font-size: var(--text-base);
    line-height: 1;
    opacity: 0.85;
  }
  /* The chevron points down when it is a menu, which is the only difference
     between the two states a reader should have to notice. */
  .pick {
    display: flex;
    align-items: center;
    gap: 1px;
    height: 24px;
    padding: 0 3px 0 4px;
    margin-left: -2px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
    transition: background var(--fast) var(--ease);
  }
  .pick:hover {
    background: var(--bg-hover);
    color: var(--fg-muted);
  }
  .pick :global(svg) {
    rotate: 90deg;
  }

  .field {
    flex: 1;
    min-width: 0;
    height: 100%;
    border: 0;
    background: none;
    font-size: var(--text-base);
    color: var(--fg);
    user-select: text;
  }
  .field:focus {
    outline: none;
  }
  .field::placeholder {
    color: var(--fg-faint);
  }

  .toggle,
  .from-file,
  .go {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    flex: none;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .toggle:hover,
  .from-file:not(:disabled):hover,
  .go:not(:disabled):hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .toggle.on {
    color: var(--journal-accent, var(--accent));
  }
  .go:disabled,
  .from-file:disabled {
    opacity: 0.35;
  }

  .results {
    position: absolute;
    z-index: 30;
    top: calc(100% + 6px);
    left: 0;
    right: 0;
    padding: var(--sp-1);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--float);
    box-shadow: var(--shadow-lg);
    max-height: 340px;
    overflow-y: auto;
  }

  .hit {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    padding: var(--sp-1) var(--sp-2);
    border-radius: var(--radius-sm);
    text-align: left;
  }
  .hit.on {
    background: var(--bg-active);
  }
  .thumb {
    display: block;
    width: 34px;
    flex: none;
  }
  .body {
    display: flex;
    flex-direction: column;
    gap: 1px;
    flex: 1;
    min-width: 0;
  }
  .name {
    font-size: var(--text-base);
    font-weight: 550;
    color: var(--fg);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sub {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .score {
    flex: none;
  }

  .status {
    padding: var(--sp-2) var(--sp-2);
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .status.err {
    color: var(--danger);
  }

  /* On a phone the line is as tall as a button, across the screen, with a
     button of each kind at its end a finger can hit. */
  :global([data-phone]) .line {
    height: 48px;
  }
  :global([data-phone]) .pick {
    height: 40px;
    padding: 0 6px;
  }
  :global([data-phone]) .toggle,
  :global([data-phone]) .from-file,
  :global([data-phone]) .go {
    width: 40px;
    height: 40px;
  }
  /* The line is the ring: the shared field's own, around the text alone,
     was a box in a box. */
  :global([data-phone]) .field:focus {
    border-color: transparent;
    box-shadow: none;
  }
  :global([data-phone]) .hit {
    padding: var(--sp-2);
  }
  :global([data-phone]) .results {
    max-height: min(340px, 50dvh);
  }
</style>
