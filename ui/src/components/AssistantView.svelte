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
  import { lookOf } from '../lib/companion'
  import { splitDigest } from '../lib/dream'
  import { panels } from '../lib/panels.svelte'
  import AssistantDog from './AssistantDog.svelte'
  import ChatThread from './ChatThread.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'

  // Each time the page appears: the settings may have changed in the
  // meantime, and the thread list has moved if the rail was used elsewhere.
  // It does not change which conversation is open.
  void agent.load()

  // The dog floats over the top of the conversation, centred on a pill with
  // its name, the way it does in the rail -- see `ChatPanel` -- with the
  // conversation's title to its left and the buttons to its right. Floating
  // while the settings load, because a dog is the default.
  const dog = $derived(agent.settings === null || lookOf(agent.settings.companion) !== null)
  /** How far down the conversation starts: below the dog and its name. */
  const STAGE_H = 154

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

{#snippet sparkle()}<Icon name="sparkle" size={34} weight={1.4} />{/snippet}

<main class="main" class:floating={dog} style="--thread-top: {dog ? STAGE_H : 0}px">
  <header class="head">
    <div class="side left">
      <h1 {title}>{title}</h1>
    </div>
    {#if dog}
      <div class="stage">
        <AssistantDog width={156} height={132} />
        <span class="pill">{agent.displayName}</span>
      </div>
    {/if}
    <div class="side">
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
    </div>
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
      <!-- No sparkle under a dog: the dog, asleep, is the picture. -->
      <EmptyState lead="{agent.displayName} is not set up yet." icon={dog ? undefined : sparkle}>
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
    position: relative;
    display: flex;
    flex: 1;
    flex-direction: column;
    min-width: 0;
    background: var(--bg-raised);
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2);
    height: var(--header-h);
    flex: none;
    padding: 0 var(--sp-3) 0 var(--sp-5);
    border-bottom: 1px solid var(--border);
  }
  .side {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }
  .side.left {
    flex: 1;
  }

  /* Over the conversation: the page's colour at the top, fading out, so a
     reply scrolled up goes under the dog. Clicks fall through the fade to
     the thread; only the title, the buttons and the dog take them. */
  .main.floating .head {
    position: absolute;
    inset: 0 0 auto;
    z-index: 3;
    align-items: flex-start;
    height: calc(var(--thread-top) + var(--sp-4));
    border-bottom: 0;
    background: linear-gradient(var(--bg-raised) 74%, transparent);
    pointer-events: none;
  }
  .main.floating .head > * {
    pointer-events: auto;
  }
  .main.floating .side {
    height: var(--header-h);
  }
  /* The title keeps to its half, clear of the dog. */
  .main.floating .side.left {
    flex: 0 1 auto;
    max-width: calc(50% - 90px);
  }
  .stage {
    position: absolute;
    top: 2px;
    left: 50%;
    display: flex;
    flex-direction: column;
    align-items: center;
    transform: translateX(-50%);
  }
  /* The dog sits on it: tucked up under its paws, and behind it, so a
     laptop or a raised paw comes out in front of the name. */
  .stage :global(.dog) {
    position: relative;
    z-index: 1;
  }
  .pill {
    position: relative;
    max-width: 220px;
    margin-top: -8px;
    padding: 4px var(--sp-4);
    overflow: hidden;
    border-radius: 999px;
    background: var(--bg-raised);
    box-shadow:
      0 0 0 1px var(--border),
      0 2px 10px rgb(0 0 0 / 0.07);
    font-size: var(--text-base);
    font-weight: 620;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  h1 {
    min-width: 0;
    overflow: hidden;
    font-size: var(--text-lg);
    font-weight: 600;
    white-space: nowrap;
    text-overflow: ellipsis;
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
    padding-top: var(--thread-top);
  }
</style>
