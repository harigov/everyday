<script lang="ts">
  // The notes app's half of the sidebar: the list, and the tags that narrow
  // it.
  //
  // A list rather than a tree. Notes are not filed in folders here, because a
  // folder is a tag with an opinion about hierarchy and a note is often about
  // two things at once. Pinned ones float to the top, which is the one kind
  // of emphasis this app has.

  import { focusOnMount, trapFocus } from '../lib/focus'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { meetings } from '../lib/meetings.svelte'
  import { notes, SORTS } from '../lib/notes.svelte'
  import { plural } from '../lib/format'
  import { proposals, recordAs } from '../lib/proposals.svelte'
  import type { NoteSummary } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'
  import ProposalGhost from './ProposalGhost.svelte'
  import RecordingCard from './RecordingCard.svelte'

  // Loaded when the sidebar appears rather than when the store is imported,
  // so a vault whose owner never opens this app never pays for the query.
  // Idempotent, so coming back refreshes instead of blanking.
  void notes.start()

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

  const shown = $derived(notes.query.trim() ? [] : notes.list)

  // Ghosts are only worth drawing over the plain list -- a search is asking
  // "where did I write about X", and a proposal that has not been saved
  // anywhere yet cannot be an answer to that.
  const ghosts = $derived(notes.query.trim() ? [] : notes.ghostNotes)
  $effect(() => {
    if (ghosts.length > 0) void proposals.markSeen(ghosts)
  })

  function noteMenu(note: NoteSummary): MenuItem[] {
    return tidyMenu([
      { label: 'Open', icon: 'quote', run: () => void notes.openNote(note.id) },
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

  function navMenu(): MenuItem[] {
    return tidyMenu([
      { label: 'New note', icon: 'plus', hint: 'Ctrl+N', run: () => void notes.create() },
      ...(meetings.supported
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

<nav class="scroll side-nav" oncontextmenu={(e) => menu.show(e, navMenu())}>
  {#if notes.tags.length > 0}
    <!-- The shared filter bar (app.css's "Filter bars"): one tag at a time,
         or All, which is the same single choice every other filter row
         makes. -->
    <div class="filters tags">
      <button class="filter" class:on={notes.tag === null} onclick={() => void notes.setTag(null)}>
        All
      </button>
      {#each notes.tags as tag (tag)}
        <button class="filter" class:on={notes.tag === tag} onclick={() => void notes.setTag(tag)}>
          {tag}
        </button>
      {/each}
    </div>
  {/if}

  <div class="side-head">
    <span class="eyebrow">
      {#if notes.query.trim()}
        {plural(notes.results.length, 'result')}
      {:else}
        {plural(notes.list.length, 'note')}
      {/if}
    </span>
    {#if meetings.supported}
      <button
        class="side-add"
        title="Record a call"
        aria-label="Record a call"
        onclick={() => (recordSheet = true)}
      >
        <Icon name="mic" size={15} />
      </button>
    {/if}
    <button
      class="side-add"
      title="New note"
      aria-label="New note"
      onclick={() => void notes.create()}
    >
      <Icon name="plus" size={15} />
    </button>
  </div>

  {#if !notes.query.trim() && meetings.recordingsError}
    <p class="error recordings-error">
      Couldn’t refresh call recordings: {meetings.recordingsError}
    </p>
  {/if}

  {#if !notes.query.trim() && meetings.recordings.length > 0}
    <div class="recordings">
      {#each meetings.recordings as r (r.id)}
        <RecordingCard recording={r} />
      {/each}
    </div>
  {/if}

  {#if notes.query.trim()}
    {#if notes.searching && notes.results.length === 0}
      <p class="hint">Searching…</p>
    {:else if notes.results.length === 0}
      <p class="hint">Nothing matched.</p>
    {:else}
      {#each notes.results as hit (hit.id)}
        <button
          class="side-row hit"
          class:sel={notes.selected === hit.id}
          onclick={() => void notes.openNote(hit.id)}
        >
          <span class="text">
            <span class="title">{hit.title}</span>
            <span class="sub">{hit.snippet}</span>
          </span>
        </button>
      {/each}
    {/if}
  {:else if shown.length === 0 && ghosts.length === 0}
    <p class="hint">No notes yet.</p>
  {:else}
    <!-- Ghosts at the top: a note is written forward, so there is no due
         date or column for a proposed one to join the foot of the way a
         task or a block does. See `notes.ghostNotes`. -->
    {#each ghosts as p (p.id)}
      {@const note = recordAs(p, 'note')}
      {#if note}
        <ProposalGhost proposal={p} color="var(--accent)" onopen={() => notes.openProposal(p)}>
          <span class="text">
            <span class="title">{note.title || 'Untitled note'}</span>
          </span>
        </ProposalGhost>
      {/if}
    {/each}
    {#each shown as note (note.id)}
      {@const replaceProposal = notes.replaceProposalFor(note.id)}
      {@const deleteProposal = notes.deleteProposalFor(note.id)}
      <!-- A `div`, not a button: with a proposed deletion on it this row
           carries two controls (open, and the chip's own accept/decline),
           and a button cannot contain a button. -->
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="side-row noterow"
        class:sel={notes.selected === note.id}
        oncontextmenu={(e) => menu.show(e, noteMenu(note))}
      >
        <button class="rowbody" onclick={() => void notes.openNote(note.id)}>
          {#if note.pinned}
            <span class="pin"><Icon name="pin" size={12} /></span>
          {/if}
          <span class="text">
            <span class="title">{note.title}</span>
            <span class="sub">{note.excerpt}</span>
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
          onopen={() => notes.openProposal(replaceProposal)}
        />
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
     shared sidebar's -- see app.css's "Sidebars". What is the notes app's
     own: the tag filter's place in a sidebar, rows two lines tall, the pin
     and the proposed-deletion chip on a row, and the record-a-call sheet. */

  /* The shared filter bar, set a little apart from the heading above. Its
     track is the sidebar's own sunken colour, so here it takes a shade of
     the hover tint instead -- otherwise the chips float as loose words. */
  .tags {
    margin-top: var(--sp-3);
    background: var(--bg-active);
  }

  /* Two lines -- the title, and a line of the note under it -- so these rows
     are as tall as what is in them rather than the sidebar's one line. */
  .side-row.hit {
    height: auto;
    align-items: flex-start;
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
    padding: var(--sp-2) var(--sp-3);
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

  .pin {
    display: grid;
    flex: none;
    place-items: center;
    height: 18px;
    color: var(--fg-faint);
  }

  .text {
    display: grid;
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
</style>
