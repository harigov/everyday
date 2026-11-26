<script module lang="ts">
  // Where the phone's list was scrolled to when a note was opened over it
  // and put away -- the list is unmounted meanwhile -- so that back finds it
  // there rather than at the top.
  const memory = { top: 0 }
</script>

<script lang="ts">
  // The notes app's half of the sidebar: the list, and what narrows it.
  //
  // A list rather than a tree. Notes are not filed in folders here, because a
  // folder is a tag with an opinion about hierarchy and a note is often about
  // two things at once. Pinned ones float to the top, which is the one kind
  // of emphasis this app has.
  //
  // Three things keep a long list readable, each a rule in `note-list.ts`:
  // the list is cut into sections by date, as the journal's and the mail
  // list are; a note the assistant wrote or a call produced carries a mark in
  // the row's gutter, and a filter over the list picks one kind; and only
  // the most used tags get a chip, with how many notes carry each, the rest
  // in a menu.

  import { untrack } from 'svelte'
  import { agent } from '../lib/agent.svelte'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import { details, phone } from '../lib/phone.svelte'
  import { sidebar } from '../lib/sidebar.svelte'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { meetings } from '../lib/meetings.svelte'
  import {
    countOrigins,
    ORIGIN_ICONS,
    ORIGINS,
    originLabel,
    originLine,
    rowWhen,
    sameTag,
    shortlistTags,
    toNoteRows,
    type NoteRow,
  } from '../lib/note-list'
  import { notes, SORTS } from '../lib/notes.svelte'
  import { templates } from '../lib/templates.svelte'
  import { plural } from '../lib/format'
  import { proposals, recordAs } from '../lib/proposals.svelte'
  import type { NoteId, NoteOrigin, NoteSummary, TemplateId } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'
  import ProposalGhost from './ProposalGhost.svelte'
  import RecordingCard from './RecordingCard.svelte'

  // On a phone this is drawn twice, and each draws half of it. The list of
  // notes is the app's home, in the stage (`home`, from `NotesView`), and
  // what narrows it -- the kind filter, the tags, the order -- is in the
  // sidebar's drawer, where every other app keeps its navigation. On a desktop
  // it is one component in the sidebar, as it always was.
  let { home = false }: { home?: boolean } = $props()
  const phoneHome = $derived(phone.on && home)
  const phoneDrawer = $derived(phone.on && !home)

  // Loaded when the sidebar appears rather than when the store is imported,
  // so a vault whose owner never opens this app never pays for the query.
  // Idempotent, so coming back refreshes instead of blanking. On a phone by
  // the home, which is on screen whenever a note is not.
  if (untrack(() => !phone.on || home)) void notes.start()

  let scroller = $state<HTMLElement | null>(null)

  // Back where it was, once, when the page is drawn.
  let restored = false
  $effect(() => {
    if (!phoneHome || !scroller || restored) return
    restored = true
    scroller.scrollTop = memory.top
  })

  /** A choice made in the drawer is made to be seen: put the drawer away. */
  function chosen() {
    if (phoneDrawer) sidebar.hide()
  }

  /** Open a note -- over the list, on a phone. */
  async function open(id: NoteId) {
    await notes.openNote(id)
    details.open('notes')
  }

  /** Start a note, blank or from a template, and open it the same way. */
  async function start(templateId: TemplateId | null = null) {
    await notes.create('', templateId)
    details.open('notes')
  }

  /** A proposed note or change, open in place of a note. */
  function openProposal(p: Parameters<typeof notes.openProposal>[0]) {
    notes.openProposal(p)
    details.open('notes')
  }

  let pendingDelete = $state<NoteSummary | null>(null)

  // ── Record a call ────────────────────────────────────────────────────

  let recordSheet = $state(false)
  let recordTitle = $state('')
  let recordError = $state<string | null>(null)

  // `meetings.starting` (not a local flag) gates the Start button below --
  // see that field's own doc: this keeps a press here from racing a press
  // of "Take notes" on `EventDetail`'s panel or `MeetingOfferBanner`'s
  // offer, all three of which call the same `startCapture`.
  async function startRecordCall() {
    recordError = null
    try {
      await meetings.startCapture({ title: recordTitle.trim() || null })
      recordSheet = false
      recordTitle = ''
    } catch (e) {
      recordError = e instanceof Error ? e.message : String(e)
    }
  }

  const searching = $derived(notes.query.trim() !== '')

  /** The list cut into sections, a heading row before each. */
  const rows = $derived(searching ? [] : toNoteRows(notes.visible, notes.sort))

  /** How many of each kind the tag filter leaves, for the kind filter's chips. */
  const counts = $derived(countOrigins(notes.list))

  /** Which kind each note is, for the search results -- a hit carries none. */
  const originOf = $derived(new Map(notes.list.map((n) => [n.id, n.origin])))

  /**
   * Whether the rows keep a gutter for the mark. Only when something in view
   * carries one: a list of nothing but the person's own notes would
   * otherwise give every row an empty column for nothing.
   */
  const marked = $derived(
    searching
      ? notes.results.some((h) => (originOf.get(h.id) ?? 'own') !== 'own')
      : notes.visible.some((n) => n.origin !== 'own'),
  )

  const tagChips = $derived(shortlistTags(notes.tags, notes.tag))
  /** Every tag in the drawer, which scrolls, rather than the most used and a
   *  menu for the rest: a menu of twenty tags is a second, shorter screen. */
  const shownTags = $derived(phoneDrawer ? notes.tags : tagChips.shown)

  /** "27 notes", or "8 notes · Sakha" under the kind filter -- the chips
   *  are icons, so the heading is where the chosen kind is named. */
  const heading = $derived(
    [
      plural(notes.visible.length, 'note'),
      notes.origin ? originLabel(notes.origin, agent.displayName) : null,
    ]
      .filter(Boolean)
      .join(' · '),
  )

  /** The kind filter's chips: every note, then each kind -- all four always,
   *  a kind with none at 0, so no chip moves as the counts change. */
  const kinds = $derived<{ key: NoteOrigin | null; label: string; count: number }[]>([
    { key: null, label: 'All', count: notes.list.length },
    ...ORIGINS.map((o) => ({ key: o, label: originLabel(o, agent.displayName), count: counts[o] })),
  ])

  /** Every tag, A to Z with its count -- the chips' tags among them, so it
   *  is one index to find a tag by name in rather than "the rest". */
  function allTags(e: MouseEvent) {
    const box = (e.currentTarget as HTMLElement).getBoundingClientRect()
    const sorted = [...notes.tags].sort((a, b) =>
      a.tag.localeCompare(b.tag, undefined, { sensitivity: 'base' }),
    )
    menu.showAt(box.left, box.bottom + 4, [
      { kind: 'heading', label: plural(notes.tags.length, 'tag') },
      ...sorted.map((t) => ({
        label: t.tag,
        hint: t.count.toLocaleString(),
        checked: sameTag(t.tag, notes.tag),
        run: () => {
          void notes.setTag(sameTag(t.tag, notes.tag) ? null : t.tag)
          chosen()
        },
      })),
    ])
  }

  /** What an empty list says, under a kind filter that left nothing. */
  function emptyLine(origin: NoteOrigin): string {
    if (origin === 'assistant') return `Nothing ${agent.displayName} has written here.`
    return origin === 'meeting' ? 'No notes from calls here.' : 'Nothing you have written here.'
  }

  // Ghosts are only worth drawing over the plain list -- a search is asking
  // "where did I write about X", and a proposal that has not been saved
  // anywhere yet cannot be an answer to that. A proposal is the assistant's,
  // so it stays under that kind and goes under the other two.
  const ghosts = $derived(
    searching || (notes.origin && notes.origin !== 'assistant') ? [] : notes.ghostNotes,
  )
  $effect(() => {
    if (ghosts.length > 0) void proposals.markSeen(ghosts)
  })

  function noteMenu(note: NoteSummary): MenuItem[] {
    return tidyMenu([
      { label: 'Open', icon: 'quote', run: () => void open(note.id) },
      {
        label: note.pinned ? 'Unpin' : 'Pin to the top',
        icon: 'pin',
        run: async () => {
          await notes.openNote(note.id)
          await notes.togglePinned()
        },
      },
      SEP,
      {
        label: 'Delete note…',
        icon: 'trash',
        danger: true,
        run: () => (pendingDelete = note),
      },
    ])
  }

  void templates.ensure()
  const noteTemplates = $derived(templates.for('notes'))

  /** One row per template offered for notes, each starting a note from it. */
  function templateItems(): MenuItem[] {
    return noteTemplates.map((t) => ({ label: t.name, run: () => void start(t.id) }))
  }

  function pickTemplate(e: MouseEvent) {
    const box = (e.currentTarget as HTMLElement).getBoundingClientRect()
    menu.showAt(box.left, box.bottom + 4, [
      { kind: 'heading', label: 'New note from template' },
      ...templateItems(),
    ])
  }

  function navMenu(): MenuItem[] {
    return tidyMenu([
      { label: 'New note', icon: 'plus', hint: 'Ctrl+N', run: () => void start() },
      ...(noteTemplates.length
        ? [{ label: 'New note from template', icon: 'layers' as const, items: templateItems() }]
        : []),
      ...(meetings.canRecord
        ? [{ label: 'Record a call', icon: 'mic' as const, run: () => (recordSheet = true) }]
        : []),
      SEP,
      ...SORTS.map((s) => ({
        label: s.label,
        checked: notes.sort === s.value,
        run: () => void notes.setSort(s.value),
      })),
    ])
  }

  async function remove() {
    const note = pendingDelete
    pendingDelete = null
    if (note) await notes.remove(note.id)
  }
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (recordSheet && e.key === 'Escape') recordSheet = false
  }}
/>

<nav
  class="scroll side-nav"
  class:home={phoneHome}
  bind:this={scroller}
  onscroll={phoneHome ? () => scroller && (memory.top = scroller.scrollTop) : undefined}
  oncontextmenu={(e) => menu.show(e, navMenu())}
>
  {#if phoneHome}
    <!-- The app's page on a phone: its name, how many, and the buttons that
         start a note. What narrows the list is in the drawer. -->
    <header class="phonetop">
      <div class="pane-titles">
        <h1 class="pane-title">Notes</h1>
        <span class="pane-sub">
          {#if searching}
            {plural(notes.results.length, 'result')}
          {:else}
            {heading}
          {/if}
        </span>
      </div>
      {#if meetings.canRecord}
        <button
          class="side-add"
          title="Record a call"
          aria-label="Record a call"
          onclick={() => (recordSheet = true)}
        >
          <Icon name="mic" size={17} />
        </button>
      {/if}
      {#if noteTemplates.length}
        <button
          class="side-add"
          title="New note from template"
          aria-label="New note from template"
          aria-haspopup="menu"
          onclick={pickTemplate}
        >
          <Icon name="layers" size={17} />
        </button>
      {/if}
      <button
        class="side-add primary"
        title="New note"
        aria-label="New note"
        onclick={() => void start()}
      >
        <Icon name="plus" size={20} weight={2} />
      </button>
    </header>

    <!-- What is narrowing the list, with a way to let it go: the choosing is
         in the drawer, and a list that is short for a reason nobody can see
         reads as a list that has lost notes. -->
    {#if !searching && (notes.origin || notes.tag)}
      <div class="active" role="group" aria-label="Narrowed to">
        {#if notes.origin}
          <button
            class="tagchip on"
            aria-label="Showing {originLabel(notes.origin, agent.displayName)}: show every kind"
            onclick={() => notes.setOrigin(null)}
          >
            <Icon name={ORIGIN_ICONS[notes.origin]} size={13} />
            {originLabel(notes.origin, agent.displayName)}
            <Icon name="close" size={12} weight={1.8} />
          </button>
        {/if}
        {#if notes.tag}
          {@const tag = notes.tag}
          <button
            class="tagchip on"
            aria-label="Showing notes tagged {tag}: show every note"
            onclick={() => void notes.setTag(null)}
          >
            <span class="hash">#</span>{tag}
            <Icon name="close" size={12} weight={1.8} />
          </button>
        {/if}
      </div>
    {/if}
  {:else}
    <div class="side-head">
      <span class="eyebrow">
        {#if searching}
          {plural(notes.results.length, 'result')}
        {:else}
          {heading}
        {/if}
      </span>
      {#if !phoneDrawer}
        {#if meetings.canRecord}
          <button
            class="side-add"
            title="Record a call"
            aria-label="Record a call"
            onclick={() => (recordSheet = true)}
          >
            <Icon name="mic" size={15} />
          </button>
        {/if}
        {#if noteTemplates.length}
          <button
            class="side-add"
            title="New note from template"
            aria-label="New note from template"
            aria-haspopup="menu"
            onclick={pickTemplate}
          >
            <Icon name="layers" size={15} />
          </button>
        {/if}
        <button
          class="side-add"
          title="New note"
          aria-label="New note"
          onclick={() => void start()}
        >
          <Icon name="plus" size={15} />
        </button>
      {/if}
    </div>
  {/if}

  {#if !phoneHome && !searching && (notes.mixed || notes.origin)}
    <!-- The shared filter bar: each kind an icon and how many there are,
         the chosen one named in the heading above rather than on its chip
         -- in a sidebar this narrow a chip naming itself clipped the
         others' counts to one digit. The icons are the marks the rows carry,
         so the bar is also their legend. Every chip is still named, in its
         title and to assistive technology. -->
    <div class="filters kinds" role="tablist" aria-label="Show notes by">
      {#each kinds as kind (kind.key ?? 'all')}
        {@const on = notes.origin === kind.key}
        <button
          class="filter"
          role="tab"
          aria-selected={on}
          aria-label={kind.label}
          class:on
          title={`${kind.label}: ${plural(kind.count, 'note')}`}
          onclick={() => {
            notes.setOrigin(kind.key)
            chosen()
          }}
        >
          <Icon name={kind.key ? ORIGIN_ICONS[kind.key] : 'list'} size={14} />
          <span class="n">{kind.count.toLocaleString()}</span>
        </button>
      {/each}
    </div>
  {/if}

  {#if !phoneHome && !searching && (notes.tags.length > 0 || notes.tag)}
    <!-- The most used tags, each with how many notes carry it -- the order
         and the number together say which matter. One at a time, like
         every other filter; picking the one in force again lets it go. -->
    <div class="tagrow" role="group" aria-label="Tags">
      {#each shownTags as t (t.tag)}
        {@const on = sameTag(t.tag, notes.tag)}
        <button
          class="tagchip"
          class:on
          aria-pressed={on}
          title={on ? 'Show every note' : plural(t.count, 'note')}
          onclick={() => {
            void notes.setTag(on ? null : t.tag)
            chosen()
          }}
        >
          <span class="hash">#</span>{t.tag}
          {#if on}
            <Icon name="close" size={11} weight={1.8} />
          {:else}
            <span class="n">{t.count.toLocaleString()}</span>
          {/if}
        </button>
      {/each}
      {#if tagChips.rest > 0 && !phoneDrawer}
        <button
          class="tagchip more"
          aria-haspopup="menu"
          title="Every tag, A to Z"
          onclick={allTags}
        >
          All {plural(notes.tags.length, 'tag')}
          <Icon name="chevron" size={11} weight={1.8} />
        </button>
      {/if}
    </div>
  {/if}

  {#if !phoneHome && !searching && meetings.recordingsError}
    <p class="error recordings-error">
      Couldn’t refresh call recordings: {meetings.recordingsError}
    </p>
  {/if}

  {#if !phoneHome && !searching && meetings.recordings.length > 0}
    <div class="recordings">
      {#each meetings.recordings as r (r.id)}
        <RecordingCard recording={r} />
      {/each}
    </div>
  {/if}

  {#if phoneDrawer}
    <!-- The order, which on a desktop is in the menu a right-click raises. A
         row, so choosing one puts the drawer away. -->
    <div class="side-head"><span class="eyebrow">Sort by</span></div>
    {#each SORTS as sort (sort.value)}
      <button
        class="side-row"
        class:sel={notes.sort === sort.value}
        aria-pressed={notes.sort === sort.value}
        onclick={() => void notes.setSort(sort.value)}
      >
        <span class="side-text">{sort.label}</span>
        {#if notes.sort === sort.value}<Icon name="check" size={15} weight={2} />{/if}
      </button>
    {/each}
  {:else if searching}
    {#if notes.searching && notes.results.length === 0}
      <p class="hint">Searching…</p>
    {:else if notes.results.length === 0}
      <p class="hint">Nothing matched.</p>
    {:else}
      {#each notes.results as hit (hit.id)}
        {@const origin = originOf.get(hit.id) ?? 'own'}
        <button
          class="side-row hit"
          class:sel={!phoneHome && notes.selected === hit.id}
          onclick={() => void open(hit.id)}
        >
          {#if marked}
            <span class="mark {origin}">
              {#if origin !== 'own'}<Icon name={ORIGIN_ICONS[origin]} size={12} />{/if}
            </span>
          {/if}
          <span class="text">
            <span class="title">{hit.title}</span>
            <span class="sub">{hit.snippet}</span>
          </span>
        </button>
      {/each}
    {/if}
  {:else if rows.length === 0 && ghosts.length === 0}
    <p class="hint">
      {#if notes.origin && notes.list.length > 0}
        {emptyLine(notes.origin)}
      {:else if notes.tag}
        No notes tagged {notes.tag}.
      {:else}
        No notes yet.
      {/if}
    </p>
  {:else}
    <!-- Ghosts at the top: a note is written forward, so there is no due
         date or column for a proposed one to join the foot of the way a
         task or a block does. See `notes.ghostNotes`. -->
    {#each ghosts as p (p.id)}
      {@const note = recordAs(p, 'note')}
      {#if note}
        <ProposalGhost proposal={p} color="var(--accent)" onopen={() => openProposal(p)}>
          <span class="text">
            <span class="title">{note.title || 'Untitled note'}</span>
          </span>
        </ProposalGhost>
      {/if}
    {/each}
    {#each rows as row (row.key)}
      {#if row.kind === 'header'}
        <h3 class="section">{row.label}</h3>
      {:else}
        {@const note: NoteSummary = (row as Extract<NoteRow, { kind: 'note' }>).note}
        {@const replaceProposal = notes.replaceProposalFor(note.id)}
        {@const deleteProposal = notes.deleteProposalFor(note.id)}
        <!-- A `div`, not a button: with a proposed deletion on it this row
             carries two controls (open, and the chip's own accept/decline),
             and a button cannot contain a button. -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div
          class="side-row noterow"
          class:sel={!phoneHome && notes.selected === note.id}
          oncontextmenu={(e) => menu.show(e, noteMenu(note))}
        >
          <button class="rowbody" onclick={() => void open(note.id)}>
            {#if marked}
              <span
                class="mark {note.origin}"
                title={originLine(note.origin, agent.displayName) ?? undefined}
              >
                {#if note.origin !== 'own'}
                  <Icon name={ORIGIN_ICONS[note.origin]} size={12} />
                {/if}
              </span>
            {/if}
            <!-- The date opens the second line rather than closing the first,
                 as Apple Notes has it: in a sidebar this narrow a date beside
                 the title took a third of the room the title had. -->
            <span class="text">
              <span class="title">{note.title}</span>
              <span class="sub">
                <span class="when">{rowWhen(note, notes.sort)}</span>
                {note.excerpt}
              </span>
            </span>
          </button>
          {#if deleteProposal}
            <span class="delete-chip">
              <ProposalGhost proposal={deleteProposal} compact color="var(--danger)">
                Proposed: delete
              </ProposalGhost>
            </span>
          {/if}
        </div>
        {#if replaceProposal}
          <ProposalGhost
            proposal={replaceProposal}
            color="var(--accent)"
            onopen={() => openProposal(replaceProposal)}
          />
        {/if}
      {/if}
    {/each}
  {/if}
</nav>

{#if pendingDelete}
  <ConfirmDialog
    title="Delete this note?"
    detail={'“' +
      pendingDelete.title +
      '” and anything attached to it will be removed. This cannot be undone.'}
    confirmLabel="Delete note"
    onconfirm={remove}
    oncancel={() => (pendingDelete = null)}
  />
{/if}

{#if recordSheet}
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div class="scrim" onclick={() => (recordSheet = false)}></div>
  <div class="sheet small" role="dialog" aria-modal="true" aria-label="Record a call" use:trapFocus>
    <h2>Record a call</h2>
    <p class="hint">
      Starts recording now, on this computer. There is no calendar event behind it, so the note it
      writes is titled whatever you give it here.
    </p>
    <form
      class="record-form"
      onsubmit={(e) => {
        e.preventDefault()
        void startRecordCall()
      }}
    >
      <input
        use:focusOnMount
        bind:value={recordTitle}
        placeholder="Title (optional)"
        aria-label="Title"
        spellcheck="false"
      />
      <button class="btn btn-primary" type="submit" disabled={meetings.starting}>
        {meetings.starting ? 'Starting…' : 'Start'}
      </button>
    </form>
    {#if recordError}<p class="error">{recordError}</p>{/if}
    <div class="sheet-row">
      <span class="spacer"></span>
      <button class="btn" onclick={() => (recordSheet = false)}>Cancel</button>
    </div>
  </div>
{/if}

<style>
  /* The heading, its buttons and the rows' hover and selection are the
     shared sidebar's -- see app.css's "Sidebars" -- and the kind filter is
     the shared filter bar. What is the notes app's own: the tag chips, the
     section headings, rows two lines tall with a gutter for the mark, the
     proposed-deletion chip on a row, and the record-a-call sheet. */

  /* Spread across the sidebar as Mail's categories are across its column:
     four equal chips sharing the width rather than wrapping, none narrower
     than its icon and count. */
  .kinds {
    flex-wrap: nowrap;
    justify-content: space-between;
    margin-top: var(--sp-1);
    background: var(--bg-active);
  }
  .kinds .filter {
    flex: 1 1 0;
    justify-content: center;
    gap: 5px;
    padding: 0 6px;
  }
  .kinds .filter :global(svg) {
    flex: none;
  }

  /* The tags: left to right in order of use, each saying how many notes
     carry it. Quieter than the kind filter above, which says what sort of
     writing this is; these say what it is about. */
  .tagrow {
    display: flex;
    flex-wrap: wrap;
    gap: 2px;
    padding: var(--sp-2) 0 0;
  }
  .tagchip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    max-width: 100%;
    height: 24px;
    padding: 0 7px;
    border-radius: 999px;
    color: var(--fg-muted);
    font-size: var(--text-xs);
    font-weight: 550;
    white-space: nowrap;
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .tagchip:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .tagchip .hash {
    margin-right: -2px;
    color: var(--fg-faint);
    font-weight: 500;
  }
  .tagchip .n {
    color: var(--fg-faint);
    font-weight: 500;
    font-variant-numeric: tabular-nums;
  }
  .tagchip.on {
    background: color-mix(in oklab, var(--accent) 16%, transparent);
    color: var(--accent);
  }
  .tagchip.on .hash {
    color: inherit;
    opacity: 0.7;
  }
  .tagchip.more {
    color: var(--fg-subtle);
  }

  /* A heading over a run of rows -- Pinned, Today, a month. Never a button
     and never focusable, as the mail list's date headings are not. Lined up
     with the rows' titles. */
  .section {
    margin: 0;
    padding: var(--sp-4) var(--sp-3) var(--sp-1);
    color: var(--fg-subtle);
    font-size: var(--text-xs);
    font-weight: 550;
  }

  /* Two lines -- the title, and a line of the note under it -- so these rows
     are as tall as what is in them rather than the sidebar's one line. */
  .side-row.hit {
    height: auto;
    align-items: flex-start;
    gap: var(--sp-2);
    padding: var(--sp-2) var(--sp-3);
  }

  /* The note row proper is a `div`, not a button -- see the template's own
     comment -- so its padding moves to the button inside it and the row
     itself keeps only the shared row's hover and selected backgrounds. */
  .side-row.noterow {
    height: auto;
    padding: 0;
  }
  .rowbody {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    padding: 7px var(--sp-3);
    background: none;
    border: none;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .delete-chip {
    display: flex;
    align-items: center;
    flex: none;
    padding-right: var(--sp-3);
  }

  /* The gutter the mark sits in, kept on every row once any row in view has
     one so the titles stay in one column. Each mark in the colour of where
     the note came from -- the assistant's own, the calendar's for a call --
     which is the colour that app's icon has in the app bar. */
  .mark {
    display: grid;
    flex: none;
    place-items: center;
    width: 14px;
    height: 19px;
  }
  .mark.assistant {
    color: var(--app-assistant);
  }
  .mark.meeting {
    color: var(--app-calendar);
  }
  .side-row.sel .mark {
    color: inherit;
  }

  .text {
    display: grid;
    flex: 1;
    min-width: 0;
    gap: 1px;
  }

  .title {
    overflow: hidden;
    color: var(--fg);
    font-size: var(--text-base);
    font-weight: 550;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  /* The open note's title takes the accent, as every sidebar's open row's
     name does. */
  .side-row.sel .title {
    color: inherit;
  }

  .when {
    margin-right: 6px;
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }

  /* Its own weight, so the open row's heavier one -- meant for a row's one
     name -- does not embolden a line of the note's text. */
  .sub {
    overflow: hidden;
    color: var(--fg-faint);
    font-size: var(--text-sm);
    font-weight: 400;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .hint {
    padding: var(--sp-3);
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }

  .recordings {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding-top: var(--sp-2);
  }

  .record-form {
    display: flex;
    gap: var(--sp-2);
    margin-top: var(--sp-3);
  }
  .record-form input {
    flex: 1;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 6px var(--sp-2);
    background: var(--bg-raised);
    color: var(--fg);
  }
  .error {
    margin-top: var(--sp-2);
    color: var(--danger);
    font-size: var(--text-sm);
  }

  /* ── On a phone ──────────────────────────────────────────────────────── */

  /* The list is the app's page. Its tail clears the floating assistant
     button, and a row is the width of the screen. */
  .home {
    padding: 0 var(--sp-2) var(--fab-clear);
  }
  .phonetop {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    padding: var(--sp-2) var(--sp-1) var(--sp-2) var(--sp-2);
  }
  .side-add.primary {
    width: 40px;
    height: 40px;
    background: color-mix(in oklab, var(--accent) 14%, transparent);
    color: var(--accent);
  }
  .side-add.primary:active {
    background: color-mix(in oklab, var(--accent) 24%, transparent);
  }
  .home .side-row:active {
    background: var(--bg-hover);
  }
  .home .section {
    padding-inline: var(--sp-2);
  }
  .home .hint {
    padding-inline: var(--sp-2);
  }
  .active {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
    padding: 0 var(--sp-2) var(--sp-1);
  }
  .active .tagchip {
    height: 34px;
  }

  /* Chips a finger can hit, in the drawer and on the page. */
  :global([data-phone]) .tagchip {
    height: 34px;
    padding: 0 var(--sp-3);
    font-size: var(--text-sm);
  }
  :global([data-phone]) .tagrow {
    gap: var(--sp-1);
    padding-top: var(--sp-3);
  }
  :global([data-phone]) .rowbody {
    padding-block: var(--sp-2);
  }

  /* The drawer reaches the foot of the screen, over the bar of apps: what is
     last in it has to clear the gesture bar and be reachable. */
  :global([data-phone]) .side-nav:not(.home) {
    padding-bottom: calc(var(--bottombar-h) + var(--safe-bottom));
  }
</style>
