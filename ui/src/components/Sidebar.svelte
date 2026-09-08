<script lang="ts">
  import { app, type Section } from '../lib/state.svelte'
  import { DEFAULT_COLORS } from '../lib/colors'
  import { focusOnMount } from '../lib/focus'
  import Icon from './Icon.svelte'
  import Logo from './Logo.svelte'
  import SettingsMenu from './SettingsMenu.svelte'
  import JournalSettings from './JournalSettings.svelte'
  import TodoNav from './TodoNav.svelte'
  import CalendarNav from './CalendarNav.svelte'
  import LibraryNav from './LibraryNav.svelte'
  import type { Journal } from '../lib/types'
  import type { IconName } from '../lib/icons'

  // One vault, four apps. The switcher is the only chrome above the nav
  // because the apps are peers -- none is a mode of another -- and each tab
  // is hidden on a backend that cannot carry it, so a Markdown vault does
  // not offer a tab that cannot work.
  const APPS: { id: Section; label: string; icon: IconName }[] = [
    { id: 'journal', label: 'Journal', icon: 'quote' },
    { id: 'todo', label: 'Todo', icon: 'check' },
    { id: 'calendar', label: 'Calendar', icon: 'calendar' },
    { id: 'library', label: 'Library', icon: 'book' },
  ]
  const shownApps = $derived(APPS.filter((a) => app.canShow(a.id)))

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

  const total = $derived(app.status?.stats?.entries ?? 0)
</script>

<aside class="sidebar">
  <div class="brand">
    <Logo size={20} tile />
    <span class="name">Every Day</span>
  </div>

  {#if shownApps.length > 1}
    <div class="apps" role="tablist" aria-label="Apps">
      {#each shownApps as a (a.id)}
        <button
          class="app"
          class:on={app.section === a.id}
          role="tab"
          aria-selected={app.section === a.id}
          onclick={() => app.setSection(a.id)}
        >
          <Icon name={a.icon} size={14} />
          {a.label}
        </button>
      {/each}
    </div>
  {/if}

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
            oncontextmenu={(e) => {
              e.preventDefault()
              settingsFor = j
            }}
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

  <div class="foot">
    <SettingsMenu />
    <button class="lock" onclick={() => app.lock()} title="Lock now (Ctrl+L)">
      <Icon name="lock" size={15} />
      Lock
    </button>
  </div>
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

  /* A segmented control rather than two rows in the nav: these switch what
     the whole window is, and a thing that looks like a list item reads as
     "one more place to put a journal". */
  .apps {
    display: flex;
    /* Four of them now. At the sidebar's width the labels no longer fit on
       one row, and a segmented control that wraps to two rows of two reads
       better than one that ellipsises every tab to "Cale…". */
    flex-wrap: wrap;
    gap: 2px;
    flex: none;
    margin: 0 var(--sp-2) var(--sp-1);
    padding: 2px;
    border-radius: var(--radius);
    background: var(--bg-active);
  }
  .app {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 5px;
    /* `1 1 40%` rather than `1`: two per row when four are shown, and still
       one row when a Markdown vault offers only two. */
    flex: 1 1 40%;
    height: 26px;
    border-radius: calc(var(--radius) - 3px);
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-subtle);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .app:hover {
    color: var(--fg);
  }
  .app.on {
    background: var(--bg-raised);
    color: var(--fg);
    box-shadow: var(--shadow-sm);
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

  .foot {
    padding: var(--sp-2);
    border-top: 1px solid var(--border);
  }
  .lock {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    /* Matches the settings trigger above it. */
    width: 100%;
    height: 28px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .lock:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
</style>
