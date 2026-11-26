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

  import { untrack } from 'svelte'
  import { agent } from '../lib/agent.svelte'
  import { assistant, PANE_LABELS, PANES, type Pane } from '../lib/assistant.svelte'
  import { lookOf } from '../lib/companion'
  import { splitDigest } from '../lib/dream'
  import { panels } from '../lib/panels.svelte'
  import { details, phone } from '../lib/phone.svelte'
  import AssistantDog from './AssistantDog.svelte'
  import AssistantNav from './AssistantNav.svelte'
  import AssistantPanes from './AssistantPanes.svelte'
  import ChatThread from './ChatThread.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import PhoneBack from './PhoneBack.svelte'
  import SubTabs from './SubTabs.svelte'
  import { jobs } from '../lib/jobs.svelte'
  import { proposals } from '../lib/proposals.svelte'
  import { app } from '../lib/state.svelte'

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

  // ── On a phone ──────────────────────────────────────────────────────
  //
  // There is room for the list of conversations or for one of them, not both
  // (see `details` in `lib/phone.svelte.ts`): the app opens on the list --
  // with what happened while you were away at the top of it, the thing the
  // number on the bar of apps counts -- and a conversation chosen from it
  // is shown over it, with a way back. The dog is a small face in the
  // header's row rather than a figure over the thread: the thread is what
  // the screen is for.

  /** The list, rather than a conversation. */
  const listing = $derived(phone.on && !details.isOpen('assistant'))
  /**
   * One of the standing tabs (what it did, routines, memory, waiting for you)
   * is open in place of the conversation. They are tabs of Settings too; from
   * the sidebar here they open in this app, so the app and its row stay
   * selected and the way back is one press.
   */
  const pane = $derived(listing ? null : assistant.pane)
  /** The dog floats over the thread; on a phone it sits in the header's row. */
  const floats = $derived(dog && !phone.on && pane === null)

  /** The tabs this vault can fill, as Settings offers them. */
  const paneTabs = $derived(
    PANES.filter(
      (p) =>
        (p !== 'proposals' || app.supportsProposals) && (p !== 'routines' || app.supportsRoutines),
    ).map((id) => ({
      id,
      label: PANE_LABELS[id],
      count:
        id === 'runs'
          ? (app.supportsRoutines ? assistant.unseen : 0) + (app.supportsJobs ? jobs.unseen : 0)
          : id === 'proposals'
            ? proposals.unseen
            : 0,
    })),
  )

  // Back at the phone's list, the tab is done with.
  $effect(() => {
    if (listing) untrack(() => (assistant.pane = null))
  })

  // A conversation chosen somewhere that is not this page -- "See what it
  // did" on a job, a file's "Ask about this", a new thread from the command
  // bar -- has to be what is on screen when this page appears, not the list
  // with that conversation highlighted in it. The first thread this page
  // finds is not news: it is whichever one was open last.
  let known: string | null = null
  $effect(() => {
    const id = agent.conversationId
    untrack(() => {
      if (phone.on && id !== null && known !== null && id !== known) details.open('assistant')
      // Another conversation put on screen (from a job, a file, the sidebar)
      // replaces the tab rather than hiding behind it.
      if (known !== null && id !== known) assistant.pane = null
      known = id
    })
  })

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

<main
  class="main"
  class:floating={floats}
  class:listing
  style="--thread-top: {floats ? STAGE_H : 0}px"
>
  <header class="head">
    <div class="side left">
      <!-- A conversation is over the list, on a phone: the way back to it. -->
      {#if !listing}
        <PhoneBack
          onclick={() => {
            assistant.pane = null
            details.close('assistant')
          }}
        />
      {/if}
      <div class="pane-titles">
        <h1 class="pane-title" title={listing ? undefined : pane ? PANE_LABELS[pane] : title}>
          {listing ? agent.displayName : pane ? PANE_LABELS[pane] : title}
        </h1>
      </div>
    </div>
    {#if dog && pane === null}
      <div class="stage">
        <AssistantDog width={phone.on ? 64 : 156} height={phone.on ? 54 : 132} />
        <span class="pill desktop-only">{agent.displayName}</span>
      </div>
    {/if}
    <div class="side">
      {#if pane !== null}
        <button class="btn btn-sm back desktop-only" onclick={() => (assistant.pane = null)}>
          Back to conversation
        </button>
      {/if}
      {#if agent.ready && !listing && pane === null}
        <button
          class="btn new"
          onclick={() => void agent.startThread()}
          disabled={agent.busy || agent.turns.length === 0}
          title="New conversation (C)"
          aria-label="New conversation"
        >
          <Icon name="plus" size={14} />
          <span class="label">New</span>
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

  {#if listing}
    <!-- The conversations, as the sidebar's drawer lists them: one list, in
         two places, so a row means the same in both. -->
    <div class="list">
      <AssistantNav />
    </div>
  {:else if pane !== null}
    <!-- The same tabs Settings holds, drawn here beside the conversations. -->
    <div class="tab scroll">
      <div class="tab-body">
        <SubTabs
          tabs={paneTabs}
          bind:value={() => pane, (v: Pane) => (assistant.pane = v)}
          label="What the assistant has done and remembers"
        />
        <AssistantPanes {pane} />
      </div>
    </div>
  {:else if agent.settings === null}
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
  /* No background of its own: the raised surface every app sits on is
     App.svelte's, and the floating header's fade below is drawn in it. */
  .main {
    /* As tall as the stage by a stated height, not stretched: see
       `.shell-stage` in app.css. */
    height: 100%;
    position: relative;
    display: flex;
    flex: 1;
    flex-direction: column;
    min-width: 0;
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
    border-radius: var(--radius-pill);
    background: var(--bg-raised);
    box-shadow:
      0 0 0 1px var(--border),
      0 2px 10px rgb(0 0 0 / 0.07);
    font-size: var(--text-base);
    font-weight: 620;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  /* Round, as an icon on its own is everywhere now that the text buttons
     beside it are pills. */
  .ghost {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .ghost:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .tab {
    flex: 1;
    min-height: 0;
    padding: var(--sp-6) var(--sp-8) 0;
  }
  .tab-body {
    display: grid;
    align-content: start;
    gap: var(--sp-4);
    width: 100%;
    max-width: calc(var(--measure) + var(--sp-8) * 2);
    margin: 0 auto;
    padding-bottom: var(--sp-8);
  }
  :global([data-phone]) .tab {
    padding: var(--sp-4) var(--sp-4) 0;
  }
  :global([data-phone]) .tab-body {
    padding-bottom: var(--fab-clear);
  }

  .wait {
    display: flex;
    flex: 1;
    min-height: 0;
    padding-top: var(--thread-top);
  }

  /* ── A phone ────────────────────────────────────────────────────────
     One row of header -- back, the conversation's name, the dog's face, New
     and Settings, each a thumb's width -- over either the list or the
     thread. */
  .list {
    display: flex;
    flex: 1;
    flex-direction: column;
    min-height: 0;
  }
  :global([data-phone]) .head {
    gap: var(--sp-1);
    height: 56px;
    padding: 0 var(--sp-1) 0 var(--sp-1);
  }
  :global([data-phone]) .main.listing .head {
    padding-left: var(--sp-4);
  }
  :global([data-phone]) .side {
    gap: 0;
  }
  :global([data-phone]) .side.left {
    flex: 1;
    min-width: 0;
  }
  :global([data-phone]) .side.left .pane-titles {
    flex: 1;
    min-width: 0;
  }
  :global([data-phone]) .pane-title {
    font-size: var(--text-md);
  }
  /* The dog is one of the row's own items rather than over it. */
  :global([data-phone]) .stage {
    position: static;
    flex: none;
    transform: none;
  }
  :global([data-phone]) .new {
    width: 44px;
    padding: 0;
    border-color: transparent;
    background: none;
  }
  :global([data-phone]) .new .label {
    display: none;
  }
  :global([data-phone]) .ghost {
    width: 44px;
    height: 44px;
    border-radius: 50%;
  }
</style>
