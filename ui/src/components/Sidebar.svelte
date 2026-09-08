<script lang="ts">
  import { app } from '../lib/state.svelte'
  import { DEFAULT_COLORS } from '../lib/colors'
  import { focusOnMount } from '../lib/focus'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { colourItems } from '../lib/menus'
  import Icon from './Icon.svelte'
  import Logo from './Logo.svelte'
  import JournalSettings from './JournalSettings.svelte'
  import TodoNav from './TodoNav.svelte'
  import CalendarNav from './CalendarNav.svelte'
  import LibraryNav from './LibraryNav.svelte'
  import type { Journal } from '../lib/types'

  // The apps themselves are `AppBar`, outside this: they are not one app's
  // navigation, and everything below is.
  let creating = $state(false)
  let draft = $state('')

  async function create() {
    const name = draft.trim()
    draft = ''
    creating = false
    if (!name) return
    // The id, timestamps and defaults come from the core. Minting them here
    // meant depending on `crypto.randomUUID`, which needs a secure context
    // the packaged webview does not always provide -- and when it is missing
    // the journal is silently never created.
    await app.newJournal(
      name,
      DEFAULT_COLORS[app.journals.length % DEFAULT_COLORS.length]!,
      app.journals.length,
    )
  }

  // Which journal's settings are open. A journal used to have none: it was
  // named once at creation, and right-clicking it deleted it -- with a
  // confirmation as the only sign that a right-click was destructive at all.
  // Now that a journal also carries what it tracks there is somewhere for
  // both to live, and deleting is a button inside it rather than a gesture
  // one slip away from a year of entries.
  let settingsFor = $state<Journal | null>(null)

  /** Re-read from the store, so a save inside the dialog is reflected. */
  const settingsJournal = $derived(
    settingsFor ? (app.journals.find((j) => j.id === settingsFor!.id) ?? null) : null,
  )

  /**
   * What a right-click on a journal offers.
   *
   * Note what is *not* in it: delete. That moved inside the settings dialog
   * on purpose -- a journal is a year of entries and it should not be one
   * slip from a menu away -- and putting it back here would undo the point
   * of moving it. Naming and the rest of the journal live in there too, so
   * this offers the two things you would otherwise cross the window for, the
   * colour, and the way in.
   */
  function journalMenu(j: Journal): MenuItem[] {
    const only = app.selectedJournal === j.id && !app.showStarredOnly
    return tidyMenu([
      {
        label: 'New entry here',
        icon: 'plus',
        run: async () => {
          app.showStarredOnly = false
          await app.selectJournal(j.id)
          await app.newEntry()
        },
      },
      {
        label: only ? 'Show every journal' : 'Show only this journal',
        icon: 'layers',
        run: () => {
          app.showStarredOnly = false
          void app.selectJournal(only ? null : j.id)
        },
      },
      SEP,
      {
        label: 'Colour',
        dot: j.color,
        items: colourItems(j.color, (color) => app.saveJournal({ ...$state.snapshot(j), color })),
      },
      { label: 'Journal settings…', icon: 'settings', run: () => (settingsFor = j) },
    ])
  }

  const total = $derived(app.status?.stats?.entries ?? 0)
</script>

<aside class="sidebar">
  <div class="brand">
    <Logo size={20} tile />
    <span class="name">Every Day</span>
  </div>

  {#if app.section === 'todo'}
    <TodoNav />
  {:else if app.section === 'calendar'}
    <CalendarNav />
  {:else if app.section === 'library'}
    <LibraryNav />
  {:else}
    <nav class="scroll nav">
      <button
        class="row"
        class:sel={app.selectedJournal === null && !app.showStarredOnly}
        onclick={() => {
          app.showStarredOnly = false
          void app.selectJournal(null)
        }}
      >
        <span class="icon"><Icon name="layers" /></span>
        <span class="text">All entries</span>
        <span class="count">{total}</span>
      </button>

      <button
        class="row"
        class:sel={app.showStarredOnly}
        onclick={() => {
          app.showStarredOnly = true
          void app.selectJournal(null)
        }}
      >
        <span class="icon star"><Icon name="star" size={15} filled /></span>
        <span class="text">Starred</span>
      </button>

      <div class="head">
        <span class="eyebrow">Journals</span>
        <button
          class="plus"
          title="New journal"
          aria-label="New journal"
          onclick={() => (creating = true)}
        >
          <Icon name="plus" size={15} />
        </button>
      </div>

      {#each app.journals as j (j.id)}
        <div class="slot">
          <button
            class="row"
            class:sel={app.selectedJournal === j.id && !app.showStarredOnly}
            style="--dot: {j.color}"
            onclick={() => {
              app.showStarredOnly = false
              void app.selectJournal(j.id)
            }}
            oncontextmenu={(e) => menu.show(e, journalMenu(j))}
            title={j.description || j.name}
          >
            <span class="icon">{j.icon}</span>
            <span class="text">{j.name}</span>
            <span class="dot" aria-hidden="true"></span>
          </button>
          <button
            class="cog"
            title="{j.name} settings"
            aria-label="{j.name} settings"
            onclick={() => (settingsFor = j)}
          >
            <Icon name="settings" size={13} />
          </button>
        </div>
      {/each}

      {#if creating}
        <input
          class="new"
          placeholder="Journal name"
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
    </nav>
  {/if}
</aside>

{#if settingsJournal}
  <JournalSettings journal={settingsJournal} onclose={() => (settingsFor = null)} />
{/if}

<style>
  .sidebar {
    width: var(--sidebar-w);
    flex: none;
    display: flex;
    flex-direction: column;
    background: var(--bg-sunken);
    border-right: 1px solid var(--border);
  }

  .brand {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: 46px;
    padding: 0 var(--sp-4);
    flex: none;
    /* Room for the traffic lights on macOS. */
    padding-left: max(var(--sp-4), env(titlebar-area-x, var(--sp-4)));
  }
  .name {
    font-weight: 620;
    letter-spacing: -0.006em;
    font-size: var(--text-md);
  }

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

  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    height: 29px;
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

  /* The settings button rides on top of the row rather than inside it: a
     row is a button, and a button inside a button is not a thing HTML has. */
  .slot {
    position: relative;
  }
  .cog {
    position: absolute;
    top: 50%;
    right: 4px;
    transform: translateY(-50%);
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
    background: var(--bg-active);
    opacity: 0;
    transition: opacity var(--fast) var(--ease);
  }
  .slot:hover .cog,
  .cog:focus-visible {
    opacity: 1;
  }
  .cog:hover {
    color: var(--fg);
  }

  /* Holds an inline icon for the fixed rows and an emoji for user journals,
     so it is a centred box of a known size rather than a run of text. */
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
    height: 29px;
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
</style>
