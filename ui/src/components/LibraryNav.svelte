<script lang="ts">
  // The library app's half of the sidebar: what is still ahead of you across
  // every shelf, and then the shelves.
  //
  // A shelf -- a `Kind` -- is defined here rather than in Settings, for the
  // reason the Overview's roles are: a thing is defined where its data is
  // seen, and a shelf you have to go to Settings for is a shelf you will not
  // rename. Everything one can have done to it is on its right-click menu.

  import { article } from '../lib/format'
  import { focusOnMount } from '../lib/focus'
  import { library } from '../lib/library.svelte'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { colourItems } from '../lib/menus'
  import type { Kind, KindInfo } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'

  let creating = $state(false)
  let draft = $state('')
  let pendingDelete = $state<KindInfo | null>(null)

  async function create() {
    const name = draft.trim()
    draft = ''
    creating = false
    if (name) await library.newShelf(name)
  }

  function deleteDetail(kind: KindInfo): string {
    const n = kind.items
    return n > 0
      ? `Its ${n} ${n === 1 ? 'item' : 'items'} go with it, along with everything you recorded about them. This cannot be undone.`
      : 'This cannot be undone.'
  }

  const stats = $derived(library.stats)
  /**
   * Shelves somebody has hidden.
   *
   * Listed, faintly, at the foot of the panel rather than left out. A `Kind`
   * has always carried a `visible` flag -- "unticking hides the shelf without
   * deleting it, exactly as unticking a calendar hides it without
   * unsubscribing" -- but nothing in the interface ever drew a tick to untick
   * or a row to find it again. The consequence, if a shelf ever went hidden
   * by any route at all, was the worst kind of missing: the items on it still
   * appear in Everything, still count in the tally at the bottom of this
   * panel, and still turn up in search, while the shelf itself is nowhere and
   * there is no way back. A hidden thing has to be findable.
   */
  const hidden = $derived(library.kinds.filter((k) => !k.visible))
  const counts = $derived(new Map(stats?.byKind.map((c) => [c.kindId, c]) ?? []))
  /**
   * Everything still ahead of you, across every shelf.
   *
   * Summed from the per-shelf counts rather than read off `stats.items`,
   * which is the *total*. One column of numbers has to mean one thing: the
   * shelves below say what is left on them, so the row above them cannot say
   * how many things have ever been on any of them. The total is in the
   * tooltip, exactly as it is for a shelf.
   */
  const openEverywhere = $derived(stats?.byKind.reduce((sum, c) => sum + c.open, 0) ?? 0)

  /**
   * What a right-click on a shelf offers.
   *
   * The gesture used to go straight to the delete confirmation, as the
   * journal and project rows once did. The deletion is still the last line
   * of this -- a shelf takes everything on it with it -- with the things
   * anybody does more than once a year above it.
   */
  function shelfMenu(kind: KindInfo): MenuItem[] {
    const open = library.shelf === kind.id && !library.favouritesOnly
    return tidyMenu([
      {
        label: `Open ${kind.name.toLowerCase()}`,
        icon: 'layers',
        disabled: open,
        run: () => {
          library.favouritesOnly = false
          void library.selectShelf(kind.id)
        },
      },
      {
        label: `Add ${article(kind.singular)} ${kind.singular.toLowerCase()}`,
        icon: 'plus',
        run: async () => {
          library.favouritesOnly = false
          await library.selectShelf(kind.id)
          library.focusCapture()
        },
      },
      SEP,
      {
        label: 'Colour',
        dot: kind.color,
        items: colourItems(kind.color, (color) => library.saveShelf({ ...shelfOnly(kind), color })),
      },
      {
        label: 'Show in this list',
        icon: kind.visible ? 'tick' : 'hidden',
        checked: kind.visible,
        hint: kind.visible ? undefined : `${kind.items} still on it`,
        run: () => library.saveShelf({ ...shelfOnly(kind), visible: !kind.visible }),
      },
      SEP,
      {
        label: `Delete the ${kind.name.toLowerCase()} shelf…`,
        icon: 'trash',
        danger: true,
        run: () => (pendingDelete = kind),
      },
    ])
  }

  /**
   * A shelf as the backend holds it: the record without the counts.
   *
   * `KindInfo` is a shelf plus how many items are on it, and those are
   * counted from the items rather than stored on the shelf -- so writing
   * them back would be handing the backend two of its own answers.
   */
  function shelfOnly(kind: KindInfo): Kind {
    const { items: _items, open: _open, ...shelf } = kind
    return shelf
  }

  /** The panel itself, where there is no shelf under the pointer. */
  function navMenu(): MenuItem[] {
    return tidyMenu([
      { label: 'New shelf', icon: 'plus', run: () => (creating = true) },
      SEP,
      {
        label: 'Everything',
        icon: 'layers',
        checked: library.shelf === null && !library.favouritesOnly,
        run: () => {
          library.favouritesOnly = false
          void library.selectShelf(null)
        },
      },
      {
        label: 'Favourites',
        icon: 'star',
        checked: library.favouritesOnly,
        run: () => {
          library.favouritesOnly = true
          void library.selectShelf(null)
        },
      },
      SEP,
      // Every shelf the vault has, ticked or not, so a hidden one can be
      // brought back from the panel it is missing from as well as from the
      // list of hidden ones at its foot.
      library.kinds.length > 0 && {
        label: 'Show these shelves',
        icon: 'layers',
        items: library.kinds.map((kind) => ({
          label: kind.name,
          dot: kind.color,
          checked: kind.visible,
          run: () => library.saveShelf({ ...shelfOnly(kind), visible: !kind.visible }),
        })),
      },
    ])
  }
</script>

<nav class="scroll nav" oncontextmenu={(e) => menu.show(e, navMenu())}>
  <button
    class="row"
    class:sel={library.shelf === null && !library.favouritesOnly}
    title="Everything — {stats?.items ?? 0} {stats?.items === 1 ? 'item' : 'items'}"
    onclick={() => {
      library.favouritesOnly = false
      void library.selectShelf(null)
    }}
  >
    <span class="icon"><Icon name="layers" /></span>
    <span class="text">Everything</span>
    {#if openEverywhere > 0}<span class="count">{openEverywhere}</span>{/if}
  </button>

  <button
    class="row"
    class:sel={library.favouritesOnly}
    onclick={() => {
      library.favouritesOnly = true
      void library.selectShelf(null)
    }}
  >
    <span class="icon star"><Icon name="star" size={15} filled /></span>
    <span class="text">Favourites</span>
  </button>

  <div class="head">
    <span class="eyebrow">Shelves</span>
    <button class="plus" title="New shelf" aria-label="New shelf" onclick={() => (creating = true)}>
      <Icon name="plus" size={15} />
    </button>
  </div>

  {#each library.visibleKinds as kind (kind.id)}
    {@const count = counts.get(kind.id)}
    <button
      class="row"
      class:sel={library.shelf === kind.id && !library.favouritesOnly}
      style="--dot: {kind.color}"
      onclick={() => {
        library.favouritesOnly = false
        void library.selectShelf(kind.id)
      }}
      oncontextmenu={(e) => menu.show(e, shelfMenu(kind))}
      title="{kind.name} — {kind.items} {kind.items === 1 ? 'item' : 'items'}"
    >
      <span class="icon">{kind.icon}</span>
      <span class="text">{kind.name}</span>
      <!-- What is still ahead of you, not the total: a shelf of four hundred
           read books is not four hundred things to do something about. The
           total is in the tooltip, where it belongs. -->
      {#if count && count.open > 0}
        <span class="count">{count.open}</span>
      {/if}
      <span class="dot" aria-hidden="true"></span>
    </button>
  {/each}

  {#if creating}
    <input
      class="new"
      placeholder="Shelf name"
      bind:value={draft}
      use:focusOnMount
      onblur={create}
      onkeydown={(e) => {
        if (e.key === 'Enter') void create()
        if (e.key === 'Escape') {
          draft = ''
          creating = false
        }
      }}
    />
  {/if}

  {#if hidden.length > 0}
    <div class="head">
      <span class="eyebrow">Hidden</span>
    </div>
    {#each hidden as kind (kind.id)}
      <button
        class="row muted"
        style="--dot: {kind.color}"
        title="{kind.name} — hidden, {kind.items} {kind.items === 1 ? 'item' : 'items'} still on it"
        onclick={() => library.saveShelf({ ...shelfOnly(kind), visible: true })}
        oncontextmenu={(e) => menu.show(e, shelfMenu(kind))}
      >
        <span class="icon">{kind.icon}</span>
        <span class="text">{kind.name}</span>
        {#if kind.items > 0}<span class="count">{kind.items}</span>{/if}
        <span class="reveal"><Icon name="hidden" size={13} /></span>
      </button>
    {/each}
  {/if}

  {#if stats && (stats.finishedThisYear > 0 || stats.active > 0)}
    <!-- The one number worth a permanent place. "Eleven this year" is what
         makes somebody open a reading list again in November. -->
    <div class="tally">
      {#if stats.active > 0}
        <div class="line"><b>{stats.active}</b> underway</div>
      {/if}
      {#if stats.finishedThisYear > 0}
        <div class="line"><b>{stats.finishedThisYear}</b> finished this year</div>
      {/if}
    </div>
  {/if}
</nav>

{#if pendingDelete}
  <ConfirmDialog
    title={'Delete the “' + pendingDelete.name + '” shelf?'}
    detail={deleteDetail(pendingDelete)}
    confirmLabel="Delete shelf"
    onconfirm={() => {
      const kind = pendingDelete
      pendingDelete = null
      if (kind) void library.deleteShelf(kind.id)
    }}
    oncancel={() => (pendingDelete = null)}
  />
{/if}

<style>
  .nav {
    flex: 1;
    padding: var(--sp-2) var(--sp-2) var(--sp-4);
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--sp-5) var(--sp-2) var(--sp-2);
  }
  .plus {
    width: 20px;
    height: 20px;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .plus:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  /* A hidden shelf is still a shelf: legible, one click from coming back,
     and never mistaken for a live one. */
  .row.muted {
    opacity: 0.55;
  }

  .reveal {
    display: grid;
    flex: none;
    place-items: center;
    width: 14px;
    color: var(--fg-faint);
  }

  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    height: var(--row-h);
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-base);
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
    font-weight: 550;
  }

  .icon {
    width: 16px;
    height: 16px;
    flex: none;
    display: grid;
    place-items: center;
    font-size: var(--text-sm);
    line-height: 1;
  }
  .star {
    color: #e0a92b;
  }
  .text {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .count {
    font-size: var(--text-xs);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--dot);
    flex: none;
    opacity: 0.85;
  }

  .new {
    width: 100%;
    height: var(--row-h);
    margin-top: 2px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--accent);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    font-size: var(--text-base);
    user-select: text;
  }
  .new:focus {
    outline: none;
  }

  .tally {
    margin-top: var(--sp-6);
    padding: var(--sp-3) var(--sp-2) 0;
    border-top: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .line {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .line b {
    font-weight: 650;
    color: var(--fg-muted);
    font-variant-numeric: tabular-nums;
  }
</style>
