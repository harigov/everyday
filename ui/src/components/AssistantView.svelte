<script lang="ts">
  // The Assistant app: the conversation, and nothing else.
  //
  // It was four panes -- what it did, its routines, its memory, what it was
  // waiting on -- and the one thing nobody could do here was talk to it. That
  // happened in the rail, a 380px column that started a fresh thread every
  // session, so the app called Assistant was where the assistant was
  // configured and the assistant itself was somewhere else. The four panes
  // are tabs of Settings now (see `AssistantPanes.svelte`), and this is the
  // place to sit down and talk: one long conversation, the whole window, that
  // carries on from wherever it was left until somebody presses New.
  //
  // It is the same thread the rail shows in every other app -- one store,
  // `agent.svelte.ts` -- so a conversation started here can be carried into
  // the todo app and continued there, and the rail is not drawn while this is
  // open, because it would be this page again, narrower.

  import { agent } from '../lib/agent.svelte'
  import { assistant } from '../lib/assistant.svelte'
  import { splitDigest } from '../lib/dream'
  import { panels } from '../lib/panels.svelte'
  import ChatThread from './ChatThread.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'

  // Each time the page appears: the settings may have changed in the
  // meantime, and the thread list has moved if the rail was used elsewhere.
  // It does not change which conversation is open.
  void agent.load()

  // The thread's own title once the list has it, which is not until the
  // first turn has finished; until then, what was asked -- the same words the
  // title will be made of -- rather than a placeholder that changes under
  // the person a few seconds later. A routine's transcript is not in the
  // list at all (runs are left out of it), and is called after its routine.
  const title = $derived.by(() => {
    const saved = agent.current?.title.trim()
    if (saved) return saved
    const run = assistant.runs.find((r) => r.conversationId === agent.conversationId)
    if (run) return run.routineName
    const first = agent.turns.find((t) => t.role === 'user')
    const asked = first ? splitDigest(first.text).text.trim() : ''
    return asked ? asked.split(/\s+/).slice(0, 8).join(' ') : 'New conversation'
  })
</script>

<main class="main">
  <header class="head">
    <h1 {title}>{title}</h1>
    <span class="spacer"></span>
    {#if agent.ready}
      <button
        class="btn"
        onclick={() => void agent.startThread()}
        disabled={agent.busy || agent.turns.length === 0}
        title="New conversation (C)"
      >
        <Icon name="plus" size={14} />
        New
      </button>
    {/if}
    <button
      class="ghost"
      onclick={() => panels.openSettings('assistant')}
      title="Assistant settings, routines and memory"
      aria-label="Assistant settings"
    >
      <Icon name="settings" size={16} />
    </button>
  </header>

  {#if agent.settings === null}
    <!-- The settings have not come back yet. Nothing rather than the "not set
         up" screen, which would flash past on every visit to a configured
         assistant. -->
    <div class="wait"></div>
  {:else if !agent.ready}
    <!-- Supported but not set up. The one thing this state must do is say
         what is missing and where to fix it, rather than presenting a box
         that fails on the first message. -->
    <div class="wait">
      <EmptyState lead="{agent.displayName} is not set up yet.">
        {#snippet icon()}<Icon name="sparkle" size={34} weight={1.4} />{/snippet}
        {#snippet note()}
          Choose a model and add a key in Settings. A model running on this machine — Ollama or LM
          Studio — needs only its address, and nothing you write leaves the machine.
        {/snippet}
        {#snippet action()}
          <button class="btn btn-primary" onclick={() => panels.openSettings('assistant')}>
            Set it up
          </button>
        {/snippet}
      </EmptyState>
    </div>
  {:else}
    <ChatThread variant="page" />
  {/if}
</main>

<style>
  .main {
    display: flex;
    flex: 1;
    flex-direction: column;
    min-width: 0;
    background: var(--bg-raised);
  }

  .head {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: var(--header-h);
    flex: none;
    padding: 0 var(--sp-3) 0 var(--sp-5);
    border-bottom: 1px solid var(--border);
  }

  h1 {
    min-width: 0;
    overflow: hidden;
    font-size: var(--text-lg);
    font-weight: 600;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .spacer {
    flex: 1;
  }

  .head .btn {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .ghost {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: var(--radius-sm);
    color: var(--fg-subtle);
  }
  .ghost:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .wait {
    display: flex;
    flex: 1;
    min-height: 0;
  }
</style>
