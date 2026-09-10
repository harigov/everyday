<script lang="ts">
  // The Assistant app's half of the sidebar: three panes, and the routines
  // under them.
  //
  // The routines are here for the reason the Overview's roles are: a thing is
  // defined where its data is seen, and a routine you have to go to Settings
  // for is a routine you will not edit.

  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { assistant, PANES, PANE_LABELS, type Pane } from '../lib/assistant.svelte'
  import { app } from '../lib/state.svelte'
  import type { IconName } from '../lib/icons'
  import type { RoutineInfo } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'

  let pendingDelete = $state<RoutineInfo | null>(null)

  const PANE_ICONS: Record<Pane, IconName> = {
    runs: 'inbox',
    routines: 'clock',
    memory: 'sparkle',
  }

  const shown = $derived(PANES.filter((p) => p !== 'routines' || app.supportsRoutines))

  function routineMenu(routine: RoutineInfo): MenuItem[] {
    return tidyMenu([
      { label: 'Edit', icon: 'pencil', run: () => assistant.edit(routine) },
      { label: 'Run now', icon: 'play', run: () => void assistant.runNow(routine.id) },
      {
        label: routine.enabled ? 'Switch off' : 'Switch on',
        icon: routine.enabled ? 'stop' : 'play',
        run: () => void assistant.toggle(routine),
      },
      SEP,
      {
        label: 'Delete routine…',
        icon: 'trash',
        danger: true,
        run: () => (pendingDelete = routine),
      },
    ])
  }

  function navMenu(): MenuItem[] {
    return tidyMenu([
      { label: 'New routine', icon: 'plus', hint: 'Ctrl+N', run: () => void assistant.draft() },
      SEP,
      ...shown.map((p) => ({
        label: PANE_LABELS[p],
        icon: PANE_ICONS[p],
        checked: assistant.pane === p,
        run: () => assistant.setPane(p),
      })),
    ])
  }

  async function remove() {
    const routine = pendingDelete
    pendingDelete = null
    if (routine) await assistant.remove(routine.id)
  }
</script>

<nav class="scroll nav" oncontextmenu={(e) => menu.show(e, navMenu())}>
  {#each shown as pane (pane)}
    <button class="row" class:sel={assistant.pane === pane} onclick={() => assistant.setPane(pane)}>
      <span class="icon"><Icon name={PANE_ICONS[pane]} /></span>
      <span class="text">{PANE_LABELS[pane]}</span>
      {#if pane === 'runs' && assistant.unseen > 0}
        <span class="count">{assistant.unseen}</span>
      {/if}
    </button>
  {/each}

  {#if app.supportsRoutines}
    <div class="head">
      <span class="eyebrow">Routines</span>
      <button
        class="plus"
        title="New routine"
        aria-label="New routine"
        onclick={() => void assistant.draft()}
      >
        <Icon name="plus" size={15} />
      </button>
    </div>

    {#if assistant.routines.length === 0}
      <p class="hint">Nothing standing by yet.</p>
    {:else}
      {#each assistant.routines as routine (routine.id)}
        <button
          class="row"
          class:muted={!routine.enabled}
          class:sel={assistant.editing?.id === routine.id}
          onclick={() => assistant.edit(routine)}
          oncontextmenu={(e) => menu.show(e, routineMenu(routine))}
        >
          <span class="icon"><Icon name={routine.enabled ? 'clock' : 'stop'} size={14} /></span>
          <span class="text">
            <span class="name">{routine.name}</span>
            <span class="sub">{routine.when}</span>
          </span>
        </button>
      {/each}
    {/if}
  {/if}
</nav>

{#if pendingDelete}
  <ConfirmDialog
    title="Delete this routine?"
    detail={'“' +
      pendingDelete.name +
      '” and its run log will be removed. Anything it made — notes, tasks — is left alone. This cannot be undone.'}
    confirmLabel="Delete routine"
    onconfirm={remove}
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
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    min-height: var(--row-h);
    padding: var(--sp-1) var(--sp-2);
    border-radius: var(--radius-sm);
    color: var(--fg-muted);
    font-size: var(--text-base);
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

  .row.muted {
    opacity: 0.55;
  }

  .icon {
    display: grid;
    flex: none;
    place-items: center;
    width: 16px;
    height: 16px;
  }

  .text {
    display: grid;
    flex: 1;
    min-width: 0;
    gap: 1px;
  }

  .name,
  .sub {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .sub {
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }

  .count {
    flex: none;
    min-width: 18px;
    padding: 1px 5px;
    border-radius: 999px;
    background: var(--accent);
    color: #fff;
    font-size: 10px;
    font-weight: 700;
    text-align: center;
  }

  .hint {
    padding: var(--sp-2);
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }
</style>
