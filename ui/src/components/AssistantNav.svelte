<script lang="ts">
  // The Assistant app's half of the sidebar: the conversations.
  //
  // It held the four panes and the routines, which are in Settings now; what
  // a sidebar beside a conversation is for is the other conversations. The
  // current one is highlighted, and the list is grouped by when each was
  // last spoken in, because "the one from yesterday" is how anybody goes
  // looking for one.
  //
  // Two rows sit above the list, and only when they have something to say:
  // what the routines did since anybody looked, and what is waiting for an
  // answer. They are the other half of the count on the app bar -- pressing
  // Assistant because of a number has to lead somewhere the number can be
  // cleared -- and they open the Settings tab that holds it.

  import { threadGroups } from '../lib/agent'
  import { agent } from '../lib/agent.svelte'
  import { assistant } from '../lib/assistant.svelte'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { panels } from '../lib/panels.svelte'
  import { proposals } from '../lib/proposals.svelte'
  import { app } from '../lib/state.svelte'
  import type { ConversationSummary } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'

  let pendingDelete = $state<ConversationSummary | null>(null)

  const groups = $derived(threadGroups(agent.threads))

  function threadMenu(thread: ConversationSummary): MenuItem[] {
    return tidyMenu([
      {
        label: 'Open',
        icon: 'quote',
        disabled: agent.busy,
        run: () => void agent.openThread(thread.id),
      },
      SEP,
      {
        label: 'Delete conversation…',
        icon: 'trash',
        danger: true,
        disabled: agent.busy && thread.id === agent.conversationId,
        run: () => (pendingDelete = thread),
      },
    ])
  }

  function navMenu(): MenuItem[] {
    return tidyMenu([
      {
        label: 'New conversation',
        icon: 'plus',
        hint: 'C',
        disabled: agent.busy,
        run: () => void agent.startThread(),
      },
      SEP,
      { label: 'Routines', icon: 'clock', run: () => assistant.setPane('routines') },
      { label: 'What it remembers', icon: 'sparkle', run: () => assistant.setPane('memory') },
      {
        label: 'Assistant settings',
        icon: 'settings',
        run: () => panels.openSettings('assistant'),
      },
    ])
  }

  async function remove() {
    const thread = pendingDelete
    pendingDelete = null
    if (thread) await agent.deleteThread(thread.id)
  }
</script>

<nav class="scroll nav" oncontextmenu={(e) => menu.show(e, navMenu())}>
  <button
    class="row new"
    onclick={() => void agent.startThread()}
    disabled={agent.busy || !agent.ready}
  >
    <span class="icon"><Icon name="plus" /></span>
    <span class="text">New conversation</span>
  </button>

  {#if (app.supportsRoutines && assistant.unseen > 0) || (app.supportsProposals && proposals.unseen > 0)}
    <div class="head"><span class="eyebrow">While you were away</span></div>
    {#if app.supportsRoutines && assistant.unseen > 0}
      <button class="row" onclick={() => assistant.setPane('runs')}>
        <span class="icon"><Icon name="inbox" /></span>
        <span class="text">What it did</span>
        <span class="count">{assistant.unseen}</span>
      </button>
    {/if}
    {#if app.supportsProposals && proposals.unseen > 0}
      <button class="row" onclick={() => assistant.setPane('proposals')}>
        <span class="icon"><Icon name="tick" /></span>
        <span class="text">Waiting for you</span>
        <span class="count">{proposals.unseen}</span>
      </button>
    {/if}
  {/if}

  {#each groups as group (group.label)}
    <div class="head"><span class="eyebrow">{group.label}</span></div>
    {#each group.threads as thread (thread.id)}
      <button
        class="row"
        class:sel={thread.id === agent.conversationId}
        onclick={() => void agent.openThread(thread.id)}
        oncontextmenu={(e) => {
          e.stopPropagation()
          menu.show(e, threadMenu(thread))
        }}
        title={thread.title || 'Untitled'}
      >
        <span class="text">
          <span class="name">{thread.title || 'Untitled'}</span>
        </span>
      </button>
    {/each}
  {:else}
    <p class="hint">Conversations you have with it are kept here.</p>
  {/each}
</nav>

<footer class="foot">
  <button class="row" onclick={() => assistant.setPane('routines')}>
    <span class="icon"><Icon name="clock" size={15} /></span>
    <span class="text">Routines and memory</span>
  </button>
</footer>

{#if pendingDelete}
  <ConfirmDialog
    title="Delete this conversation?"
    detail={'“' +
      (pendingDelete.title || 'Untitled') +
      '” and everything said in it will be removed. Anything it made — notes, tasks — is left alone. This cannot be undone.'}
    confirmLabel="Delete conversation"
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
    padding: var(--sp-5) var(--sp-2) var(--sp-2);
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

  .row:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .row:disabled {
    opacity: 0.5;
  }

  .row.sel {
    background: var(--bg-active);
    color: var(--fg);
    font-weight: 550;
  }

  .row.new {
    color: var(--fg);
    font-weight: 550;
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
  }

  .name {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
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
    padding: var(--sp-4) var(--sp-2);
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }

  .foot {
    flex: none;
    padding: var(--sp-2);
    border-top: 1px solid var(--border);
  }

  .foot .row {
    color: var(--fg-subtle);
    font-size: var(--text-sm);
  }
</style>
