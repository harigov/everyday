<script lang="ts">
  // The notes app's half of the sidebar: the list, and the tags that narrow
  // it.
  //
  // A list rather than a tree. Notes are not filed in folders here, because a
  // folder is a tag with an opinion about hierarchy and a note is often about
  // two things at once. Pinned ones float to the top, which is the one kind
  // of emphasis this app has.

  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { notes, SORTS } from '../lib/notes.svelte'
  import { plural } from '../lib/format'
  import type { NoteSummary } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'

  // Loaded when the sidebar appears rather than when the store is imported,
  // so a vault whose owner never opens this app never pays for the query.
  // Idempotent, so coming back refreshes instead of blanking.
  void notes.load()

  let pendingDelete = $state<NoteSummary | null>(null)

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
    <button class="plus" title="New note" aria-label="New note" onclick={() => void notes.create()}>
      <Icon name="plus" size={15} />
    </button>
  </div>

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

  .eyebrow {
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--fg-faint);
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
</style>
