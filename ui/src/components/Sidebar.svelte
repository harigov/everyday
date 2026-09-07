<script lang="ts">
  import { app, type Section } from '../lib/state.svelte'
  import { DEFAULT_COLORS } from '../lib/colors'
  import { focusOnMount } from '../lib/focus'
  import Icon from './Icon.svelte'
  import Logo from './Logo.svelte'
  import SettingsMenu from './SettingsMenu.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import TodoNav from './TodoNav.svelte'
  import CalendarNav from './CalendarNav.svelte'
  import type { Journal } from '../lib/types'
  import type { IconName } from '../lib/icons'

  // One vault, three apps. The switcher is the only chrome above the nav
  // because the apps are peers -- none is a mode of another -- and each tab
  // is hidden on a backend that cannot carry it, so a Markdown vault does
  // not offer a tab that cannot work.
  const APPS: { id: Section; label: string; icon: IconName }[] = [
    { id: 'journal', label: 'Journal', icon: 'quote' },
    { id: 'todo', label: 'Todo', icon: 'check' },
    { id: 'calendar', label: 'Calendar', icon: 'calendar' },
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

  let pendingDelete = $state<Journal | null>(null)

  async function remove() {
    const j = pendingDelete
    pendingDelete = null
    if (j) await app.deleteJournal(j.id)
  }

  function deleteDetail(j: Journal): string {
    const n = app.entries.filter((e) => e.journalId === j.id).length
    return n > 0
      ? `Its ${n} ${n === 1 ? 'entry' : 'entries'} will be removed too. This cannot be undone.`
      : 'This cannot be undone.'
  }

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
            pendingDelete = j
          }}
          title={j.description || j.name}
        >
          <span class="icon">{j.icon}</span>
          <span class="text">{j.name}</span>
          <span class="dot" aria-hidden="true"></span>
        </button>
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

{#if pendingDelete}
  <ConfirmDialog
    title={'Delete “' + pendingDelete.name + '”?'}
    detail={deleteDetail(pendingDelete)}
    confirmLabel="Delete journal"
    onconfirm={remove}
    oncancel={() => (pendingDelete = null)}
  />
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
    flex: 1;
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
