<script lang="ts">
  import { app } from '../lib/state.svelte'
  import { DEFAULT_COLORS } from '../lib/colors'
  import SettingsMenu from './SettingsMenu.svelte'
  import type { Journal } from '../lib/types'

  let creating = $state(false)
  let draft = $state('')

  async function create() {
    const name = draft.trim()
    draft = ''
    creating = false
    if (!name) return
    const now = new Date().toISOString()
    const journal: Journal = {
      id: crypto.randomUUID(),
      name,
      color: DEFAULT_COLORS[app.journals.length % DEFAULT_COLORS.length]!,
      icon: '\u{1f4d3}',
      description: '',
      sortOrder: app.journals.length,
      createdAt: now,
      updatedAt: now,
    }
    await app.saveJournal(journal)
    await app.selectJournal(journal.id)
  }

  async function remove(j: Journal) {
    const n = app.entries.filter((e) => e.journalId === j.id).length
    const msg =
      n > 0
        ? `Delete “${j.name}” and its ${n} ${n === 1 ? 'entry' : 'entries'}? This cannot be undone.`
        : `Delete “${j.name}”?`
    if (window.confirm(msg)) await app.deleteJournal(j.id)
  }

  const total = $derived(app.status?.stats?.entries ?? 0)
</script>

<aside class="sidebar">
  <div class="brand">
    <span class="mark">✦</span>
    <span class="name">Every Day</span>
  </div>

  <nav class="scroll nav">
    <button
      class="row"
      class:sel={app.selectedJournal === null && !app.showStarredOnly}
      onclick={() => { app.showStarredOnly = false; app.selectJournal(null) }}
    >
      <span class="icon">◈</span>
      <span class="text">All entries</span>
      <span class="count">{total}</span>
    </button>

    <button
      class="row"
      class:sel={app.showStarredOnly}
      onclick={() => { app.showStarredOnly = true; app.selectJournal(null) }}
    >
      <span class="icon star">★</span>
      <span class="text">Starred</span>
    </button>

    <div class="head">
      <span class="eyebrow">Journals</span>
      <button class="plus" title="New journal" onclick={() => (creating = true)}>+</button>
    </div>

    {#each app.journals as j (j.id)}
      <button
        class="row"
        class:sel={app.selectedJournal === j.id && !app.showStarredOnly}
        style="--dot: {j.color}"
        onclick={() => { app.showStarredOnly = false; app.selectJournal(j.id) }}
        oncontextmenu={(e) => { e.preventDefault(); remove(j) }}
        title={j.description || j.name}
      >
        <span class="icon">{j.icon}</span>
        <span class="text">{j.name}</span>
        <span class="dot" aria-hidden="true"></span>
      </button>
    {/each}

    {#if creating}
      <!-- svelte-ignore a11y_autofocus -->
      <input
        class="new"
        placeholder="Journal name"
        bind:value={draft}
        autofocus
        onblur={create}
        onkeydown={(e) => {
          if (e.key === 'Enter') create()
          if (e.key === 'Escape') { draft = ''; creating = false }
        }}
      />
    {/if}
  </nav>

  <div class="foot">
    <SettingsMenu />
    <button class="lock" onclick={() => app.lock()} title="Lock now (Ctrl+L)">
      <span aria-hidden="true">⌁</span> Lock
    </button>
  </div>
</aside>

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
  .mark { color: var(--accent); font-size: var(--text-md); }
  .name { font-weight: 650; letter-spacing: -0.01em; }

  .nav { flex: 1; padding: var(--sp-2) var(--sp-2) var(--sp-4); }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--sp-5) var(--sp-2) var(--sp-2);
  }
  .plus {
    width: 18px; height: 18px;
    display: grid; place-items: center;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
    font-size: var(--text-md);
    line-height: 1;
  }
  .plus:hover { background: var(--bg-hover); color: var(--fg); }

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
    transition: background var(--fast) var(--ease), color var(--fast) var(--ease);
  }
  .row:hover { background: var(--bg-hover); color: var(--fg); }
  .row.sel { background: var(--bg-active); color: var(--fg); font-weight: 550; }

  .icon { width: 16px; text-align: center; font-size: var(--text-sm); flex: none; }
  .star { color: #e0a92b; }
  .text { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .count { font-size: var(--text-xs); color: var(--fg-faint); font-variant-numeric: tabular-nums; }

  .dot {
    width: 6px; height: 6px; border-radius: 50%;
    background: var(--dot); flex: none; opacity: 0.85;
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
  .new:focus { outline: none; }

  .foot { padding: var(--sp-2); border-top: 1px solid var(--border); }
  .lock {
    display: flex; align-items: center; gap: var(--sp-2);
    width: 100%; height: 28px; padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-sm); color: var(--fg-subtle);
  }
  .lock:hover { background: var(--bg-hover); color: var(--fg); }
</style>
