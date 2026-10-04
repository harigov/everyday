<script lang="ts">
  // The assistant, down the right-hand side.
  //
  // Deliberately a rail rather than a screen. Everything it does is *to* what
  // is on the left — a task you can see, the entry you are writing — so
  // taking the window away from that to talk about it would be the wrong
  // shape. It is also why the composer sends what you are looking at along
  // with what you typed: "file this under tomorrow" means nothing without it.
  //
  // It is the same conversation the Assistant app shows across the whole
  // window, carried into whichever app somebody went to next -- so the rail
  // is the narrow view of one thread, not a second assistant with a memory of
  // its own. The conversation itself is `ChatThread`, drawn by both; what is
  // here is only what a rail has and a page does not: a width to drag, and
  // the history folded into a header button rather than a sidebar. What the
  // person is looking at -- the line sent with every message so "this"
  // resolves -- is worked out by the thread itself, from whichever app is
  // open.
  //
  // Three states, and the middle one matters most. The panel is not offered
  // at all on a backend with no assistant storage; it is offered but shows a
  // way into settings when it is not configured; and it talks when it is. A
  // panel that looked ready and then failed on the first message would be the
  // worst of the three.

  import { agent } from '../lib/agent.svelte'
  import { panels } from '../lib/panels.svelte'
  import { pref } from '../lib/prefs'
  import { app } from '../lib/state.svelte'
  import ChatThread from './ChatThread.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'

  let showHistory = $state(false)

  // ── How wide the rail is ─────────────────────────────────────────────
  //
  // It was 340px and nothing else. That is a reasonable width for a question
  // and a two-line answer, and the wrong one for everything else this panel
  // now draws: a table of tasks, a fenced block of configuration, a numbered
  // list of eleven things. Any of those in a 340px column is a column of
  // wrapped fragments.
  //
  // Remembered across launches, like whether the rail is open at all, and
  // for the same reason: it is a working preference rather than a mood.

  const MIN_WIDTH = 300
  const DEFAULT_WIDTH = 380
  /** Never more than this share of the window: the app is the point. */
  const MAX_SHARE = 0.62

  const widthPref = pref<number>(
    'everyday:assistant-width',
    (raw) => Number(raw) || DEFAULT_WIDTH,
    DEFAULT_WIDTH,
  )

  /**
   * The width somebody asked for, and the width they get.
   *
   * Two values rather than one, because the preference outlives the window it
   * was set in. Storing the clamped figure meant a rail dragged wide on a
   * desktop display came back at 62% of a laptop's -- and, worse, that
   * *became* the preference, so plugging the big screen back in did not
   * restore it. `width` is the wish and is what is written down; `applied` is
   * what the rail is actually given, recomputed whenever the window changes
   * shape.
   */
  let width = $state(widthPref.get())
  let viewport = $state(window.innerWidth)
  let dragging = $state(false)

  /** Clamp to the window, so a rail dragged wide on a big display comes back. */
  function clamp(px: number, within = viewport): number {
    return Math.max(MIN_WIDTH, Math.min(px, Math.round(within * MAX_SHARE)))
  }

  const applied = $derived(clamp(width))

  function startResize(event: PointerEvent) {
    // Prevented so the drag does not paint a selection across the reply it
    // passes over -- and, because preventing a pointer press also suppresses
    // the focus that would have followed it, the handle is focused by hand.
    // Without that, a resizer you can drag is one you cannot click on to
    // then nudge with the arrow keys.
    event.preventDefault()
    dragging = true
    const startX = event.clientX
    const startWidth = applied
    // Captured on the handle, so the drag survives the pointer outrunning it
    // -- which it does immediately, because the panel is what moves.
    const handle = event.currentTarget as HTMLElement
    handle.focus()
    handle.setPointerCapture(event.pointerId)

    const move = (e: PointerEvent) => {
      // Leftwards is wider: the rail is on the right-hand edge. Measured
      // from `applied` rather than from `width`, so a drag that begins on a
      // rail the window has narrowed starts from where the edge actually is.
      width = clamp(startWidth + (startX - e.clientX))
    }
    const end = () => {
      dragging = false
      handle.releasePointerCapture(event.pointerId)
      handle.removeEventListener('pointermove', move)
      handle.removeEventListener('pointerup', end)
      handle.removeEventListener('pointercancel', end)
      widthPref.set(width)
    }
    handle.addEventListener('pointermove', move)
    handle.addEventListener('pointerup', end)
    handle.addEventListener('pointercancel', end)
  }

  /** The keyboard's version of the drag. A rail nobody can resize by hand. */
  function nudge(event: KeyboardEvent) {
    const step = event.shiftKey ? 48 : 16
    if (event.key === 'ArrowLeft') width = clamp(applied + step)
    else if (event.key === 'ArrowRight') width = clamp(applied - step)
    else return
    // Taken here, so the same arrow key does not also page the calendar
    // behind the rail: the window's shortcut handler checks this first.
    event.preventDefault()
    widthPref.set(width)
  }

  // The rail can be on screen without anyone having clicked it this session
  // -- restored at startup, or still open after a lock cleared its state --
  // so it loads its own settings rather than relying on the toggle. Without
  // this a configured assistant draws its "not set up yet" screen until the
  // panel is closed and reopened.
  $effect(() => {
    void agent.ensureLoaded()
  })
</script>

<!-- The rail is re-fitted when the window changes shape, without the stored
     preference being rewritten: see `width` and `applied`. -->
<svelte:window onresize={() => (viewport = window.innerWidth)} />

<aside class="panel" class:dragging style="--panel-w: {applied}px" aria-label={agent.displayName}>
  <!-- The rail's own left edge, as a control. `separator` with an
       orientation and a value is what a resizer is called in ARIA, and it
       takes the arrow keys for the same reason every other control here
       does: a panel only the pointer can size is a panel a keyboard user
       cannot read a table in. -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions, a11y_no_noninteractive_tabindex -->
  <div
    class="resizer"
    role="separator"
    aria-orientation="vertical"
    aria-label="Width of the assistant"
    aria-valuenow={applied}
    tabindex="0"
    onpointerdown={startResize}
    onkeydown={nudge}
    ondblclick={() => {
      width = DEFAULT_WIDTH
      widthPref.set(width)
    }}
  ></div>

  <header class="head">
    <button
      class="ghost"
      onclick={() => (showHistory = !showHistory)}
      aria-expanded={showHistory}
      title="Past conversations"
    >
      <Icon name="layers" size={16} />
    </button>
    <span class="title">{agent.displayName}</span>
    <button
      class="ghost"
      onclick={() => void agent.startThread()}
      disabled={agent.busy}
      title="New conversation"
    >
      <Icon name="plus" size={16} />
    </button>
    <!-- The way from the rail to the page, for a conversation that has
         outgrown a column. The thread comes with it: they are one store. -->
    {#if app.canShow('assistant')}
      <button
        class="ghost"
        onclick={() => void app.goTo('assistant')}
        title="Open in the Assistant app"
      >
        <Icon name="monitor" size={16} />
      </button>
    {/if}
    <button class="ghost" onclick={() => void agent.toggle()} title="Close">
      <Icon name="close" size={16} />
    </button>
  </header>

  {#if showHistory}
    <div class="history">
      {#if agent.threads.length === 0}
        <p class="none">Nothing yet.</p>
      {:else}
        {#each agent.threads as thread (thread.id)}
          <div class="thread" class:on={thread.id === agent.conversationId}>
            <button class="threadname" onclick={() => void agent.openThread(thread.id)}>
              <span class="threadtitle">{thread.title || 'Untitled'}</span>
              <span class="count">{thread.messages}</span>
            </button>
            <button
              class="ghost"
              onclick={() => void agent.deleteThread(thread.id)}
              title="Delete conversation"
            >
              <Icon name="trash" size={14} />
            </button>
          </div>
        {/each}
      {/if}
    </div>
  {/if}

  {#if !agent.ready}
    <!-- Supported but not set up. The one thing this state must do is say
         what is missing and where to fix it, rather than presenting a box
         that fails on the first message. -->
    <div class="unset">
      <EmptyState lead="{agent.displayName} is not set up yet.">
        {#snippet icon()}<Icon name="sparkle" size={28} weight={1.4} />{/snippet}
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
    <ChatThread variant="rail" />
  {/if}
</aside>

<style>
  .panel {
    position: relative;
    display: flex;
    flex-direction: column;
    width: var(--panel-w);
    flex: none;
    min-height: 0;
    border-left: 1px solid var(--border);
    background: var(--bg-panel);
  }

  /* Four pixels wide and eleven to grab: a resizer you can hit is wider than
     a resizer you can see, so the target is padded outwards over the pane
     beside it rather than drawn thicker. */
  .resizer {
    position: absolute;
    top: 0;
    bottom: 0;
    left: -4px;
    width: 9px;
    z-index: 5;
    cursor: col-resize;
    touch-action: none;
  }
  .resizer::after {
    content: '';
    position: absolute;
    inset: 0 auto 0 4px;
    width: 2px;
    background: var(--accent);
    opacity: 0;
    transition: opacity var(--fast) var(--ease);
  }
  .resizer:hover::after,
  .resizer:focus-visible::after,
  .panel.dragging .resizer::after {
    opacity: 1;
  }
  /* Text selection must not fight the drag: without this, pulling the rail
     wider highlights every reply it passes over. */
  .panel.dragging {
    user-select: none;
  }

  .head {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    padding: var(--sp-2) var(--sp-2) var(--sp-2) var(--sp-3);
    border-bottom: 1px solid var(--border);
  }
  .title {
    flex: 1;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg-muted);
  }
  .ghost {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--fg-subtle);
    cursor: pointer;
  }
  .ghost:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .ghost:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .history {
    max-height: 200px;
    overflow-y: auto;
    padding: var(--sp-1);
    border-bottom: 1px solid var(--border);
  }
  .thread {
    display: flex;
    align-items: center;
    border-radius: var(--radius-sm);
  }
  .thread:hover {
    background: var(--bg-hover);
  }
  .thread.on {
    background: var(--bg-selected);
  }
  .threadname {
    display: flex;
    flex: 1;
    gap: var(--sp-2);
    align-items: baseline;
    min-width: 0;
    padding: var(--sp-2);
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
    font-size: var(--text-sm);
    text-align: left;
    cursor: pointer;
  }
  .threadtitle {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .count {
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }

  .unset {
    display: flex;
    flex: 1;
    min-height: 0;
  }

  .none {
    margin: 0;
    padding: var(--sp-3);
    font-size: var(--text-sm);
    color: var(--fg-faint);
  }
</style>
