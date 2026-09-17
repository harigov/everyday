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
  import type { NoteSummary } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'
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

<nav class="scroll nav" oncontextmenu={(e) => menu.show(e, navMenu())}>
  <div class="search">
    <Icon name="search" size={14} />
    <input
      class="q"
      data-search
      placeholder="Search notes"
      value={notes.query}
      oninput={(e) => notes.setQuery(e.currentTarget.value)}
      spellcheck="false"
    />
    {#if notes.query}
      <button class="clear" aria-label="Clear search" onclick={() => notes.clearSearch()}>
        <Icon name="close" size={13} />
      </button>
    {/if}
  </div>

  {#if notes.tags.length > 0}
    <div class="tags">
      <button class="chip" class:on={notes.tag === null} onclick={() => void notes.setTag(null)}>
        All
      </button>
      {#each notes.tags as tag (tag)}
        <button class="chip" class:on={notes.tag === tag} onclick={() => void notes.setTag(tag)}>
          {tag}
        </button>
      {/each}
    </div>
  {/if}

  <div class="head">
    <span class="eyebrow">
      {#if notes.query.trim()}
        {plural(notes.results.length, 'result')}
      {:else}
        {plural(notes.list.length, 'note')}
      {/if}
    </span>
    {#if meetings.supported}
      <button
        class="plus"
        title="Record a call"
        aria-label="Record a call"
        onclick={() => (recordSheet = true)}
      >
        <Icon name="mic" size={15} />
      </button>
    {/if}
    <button class="plus" title="New note" aria-label="New note" onclick={() => void notes.create()}>
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
          class="row"
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
  {:else if shown.length === 0}
    <p class="hint">No notes yet.</p>
  {:else}
    {#each shown as note (note.id)}
      <button
        class="row"
        class:sel={notes.selected === note.id}
        onclick={() => void notes.openNote(note.id)}
        oncontextmenu={(e) => menu.show(e, noteMenu(note))}
      >
        {#if note.pinned}
          <span class="pin"><Icon name="pin" size={12} /></span>
        {/if}
        <span class="text">
          <span class="title">{note.title}</span>
          <span class="sub">{note.excerpt}</span>
        </span>
      </button>
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
      class="row"
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
  .nav {
    flex: 1;
    padding: var(--sp-2) var(--sp-2) var(--sp-4);
  }

  .search {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: var(--row-h);
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    background: var(--bg-hover);
    color: var(--fg-faint);
  }

  .q {
    flex: 1;
    min-width: 0;
    border: 0;
    background: none;
    color: var(--fg);
    font-size: var(--text-base);
  }

  .q:focus {
    outline: none;
  }

  .clear {
    display: grid;
    place-items: center;
    color: var(--fg-faint);
  }

  .clear:hover {
    color: var(--fg);
  }

  .tags {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-1);
    padding: var(--sp-3) var(--sp-1) 0;
  }

  .chip {
    padding: 2px var(--sp-2);
    border-radius: 999px;
    background: var(--bg-hover);
    color: var(--fg-muted);
    font-size: var(--text-xs);
  }

  .chip:hover {
    color: var(--fg);
  }

  .chip.on {
    background: var(--bg-active);
    color: var(--fg);
    font-weight: 550;
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--sp-5) var(--sp-2) var(--sp-2);
  }

  .plus {
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }

  .plus:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .row {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    width: 100%;
    padding: var(--sp-2);
    border-radius: var(--radius-sm);
    color: var(--fg-muted);
    text-align: left;
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }

  .row:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .row.sel {
    background: var(--bg-active);
    color: var(--fg);
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

  .sub {
    overflow: hidden;
    color: var(--fg-faint);
    font-size: var(--text-sm);
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .hint {
    padding: var(--sp-3) var(--sp-2);
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }

  .recordings {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding-top: var(--sp-2);
  }

  .row {
    display: flex;
    gap: var(--sp-2);
    margin-top: var(--sp-3);
  }
  .row input {
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
