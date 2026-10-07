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

<nav class="scroll side-nav" oncontextmenu={(e) => menu.show(e, navMenu())}>
  <button
    class="side-row new"
    onclick={() => void agent.startThread()}
    disabled={agent.busy || !agent.ready}
  >
    <span class="side-icon"><Icon name="plus" /></span>
    <span class="side-text">New conversation</span>
  </button>

  {#if (app.supportsRoutines && assistant.unseen > 0) || (app.supportsProposals && proposals.unseen > 0)}
    <div class="side-head"><span class="eyebrow">While you were away</span></div>
    <!-- Badged counts: these rows are here only because something is
         unseen, and they are the app bar's count broken down -- the number
         somebody pressed Assistant to find. -->
    {#if app.supportsRoutines && assistant.unseen > 0}
      <button class="side-row" onclick={() => assistant.setPane('runs')}>
        <span class="side-icon"><Icon name="inbox" /></span>
        <span class="side-text">What it did</span>
        <span class="side-count strong">{assistant.unseen}</span>
      </button>
    {/if}
    {#if app.supportsProposals && proposals.unseen > 0}
      <button class="side-row" onclick={() => assistant.setPane('proposals')}>
        <span class="side-icon"><Icon name="tick" /></span>
        <span class="side-text">Waiting for you</span>
        <span class="side-count strong">{proposals.unseen}</span>
      </button>
    {/if}
  {/if}

  {#each groups as group (group.label)}
    <div class="side-head"><span class="eyebrow">{group.label}</span></div>
    {#each group.threads as thread (thread.id)}
      <button
        class="side-row"
        class:sel={thread.id === agent.conversationId}
        onclick={() => void agent.openThread(thread.id)}
        oncontextmenu={(e) => {
          e.stopPropagation()
          menu.show(e, threadMenu(thread))
        }}
        title={thread.title || 'Untitled'}
      >
        <span class="side-text">{thread.title || 'Untitled'}</span>
      </button>
    {/each}
  {:else}
    <p class="hint">Conversations you have with it are kept here.</p>
  {/each}
</nav>

<footer class="foot">
  <button class="side-row" onclick={() => assistant.setPane('routines')}>
    <span class="side-icon"><Icon name="clock" size={15} /></span>
    <span class="side-text">Routines and memory</span>
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
  /* The headings, rows, counts and selection are the shared sidebar's --
     see app.css's "Sidebars". What is the assistant's own: the New row, and
     the foot under the list. */

  /* New conversation: the one row here that makes something rather than
     going somewhere, so it is drawn in full ink rather than the rows'
     muted grey. */
  .side-row.new {
    color: var(--fg);
    font-weight: 550;
  }
  /* Not while a reply is still coming in, or before there is a model to
     talk to -- and no hover then, which would say it could be pressed. */
  .side-row.new:disabled {
    opacity: 0.5;
    background: none;
  }

  .hint {
    padding: var(--sp-4) var(--sp-3);
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }

  /* Under the list rather than in it, and quieter than it: the way into
     Settings, not one of the conversations. Inset by the same amount as the
     list above, so its row lines up with theirs. */
  .foot {
    flex: none;
    padding: var(--sp-2) var(--sp-3);
    border-top: 1px solid var(--border);
  }

  .foot .side-row {
    color: var(--fg-subtle);
    font-size: var(--text-sm);
  }
</style>
