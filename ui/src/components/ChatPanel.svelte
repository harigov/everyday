<script lang="ts">
  // The assistant, down the right-hand side.
  //
  // Deliberately a rail rather than a screen. Everything it does is *to* what
  // is on the left — a task you can see, the entry you are writing — so
  // taking the window away from that to talk about it would be the wrong
  // shape. It is also why the composer sends what you are looking at along
  // with what you typed: "file this under tomorrow" means nothing without it.
  //
  // Three states, and the middle one matters most. The panel is not offered
  // at all on a backend with no assistant storage; it is offered but shows a
  // way into settings when it is not configured; and it talks when it is. A
  // panel that looked ready and then failed on the first message would be the
  // worst of the three.

  import { agent } from '../lib/agent.svelte'
  import { app } from '../lib/state.svelte'
  import { todo } from '../lib/todo.svelte'
  import { library } from '../lib/library.svelte'
  import Icon from './Icon.svelte'
  import ToolCardView from './ToolCard.svelte'

  let draft = $state('')
  let box = $state<HTMLTextAreaElement | null>(null)
  let scroller = $state<HTMLDivElement | null>(null)
  let showHistory = $state(false)

  /**
   * What the person is looking at, in one line.
   *
   * Sent with every message so "this", "here" and "that one" resolve. It is
   * prose rather than ids on purpose: the assistant has tools for finding
   * records and does not need to be handed one, but it does need to know
   * which app is open and what is selected in it.
   */
  const context = $derived.by(() => {
    switch (app.section) {
      case 'todo':
        return todo.project ? `the todo app, project "${todo.project.name}"` : 'the todo app'
      case 'calendar':
        return 'the calendar'
      case 'library':
        return library.kind ? `the library, shelf "${library.kind.name}"` : 'the library'
      default: {
        const entry = app.entry
        if (!entry) return 'the journal'
        const title = entry.title?.trim()
        return title
          ? `the journal, entry "${title}" dated ${entry.localDate}`
          : `the journal, an entry dated ${entry.localDate}`
      }
    }
  })

  // Follow the reply as it streams, but only from the bottom: a person who
  // has scrolled up to read something is reading it, and yanking them back
  // down every few tokens is the single most irritating thing a chat panel
  // can do.
  let pinned = $state(true)
  function onScroll() {
    if (!scroller) return
    pinned = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 60
  }
  $effect(() => {
    // Touch what should retrigger this: the turns and the text growing.
    agent.turns.map((t) => t.text.length)
    if (pinned && scroller) scroller.scrollTop = scroller.scrollHeight
  })

  // The rail can be on screen without anyone having clicked it this session
  // -- restored at startup, or still open after a lock cleared its state --
  // so it loads its own settings rather than relying on the toggle. Without
  // this a configured assistant draws its "not set up yet" screen until the
  // panel is closed and reopened.
  $effect(() => {
    void agent.ensureLoaded()
  })

  // The panel is opened in order to type in it, so the caret starts here
  // rather than making the first thing anyone does be a click.
  //
  // Depends on the textarea existing and on nothing else. It used to read
  // `agent.ready`, which reads the settings -- so every save in the Assistant
  // dialog re-ran this and pulled focus out of the dialog into the composer
  // behind it, and the next thing typed went to the wrong box.
  $effect(() => {
    box?.focus()
  })

  async function send() {
    const text = draft
    // Cleared optimistically, because the box is disabled for the whole turn
    // and leaving the question in it reads as though nothing was sent. Put
    // back if the store refuses it -- which it does when there is no thread
    // open -- so a paragraph is never silently eaten.
    draft = ''
    const accepted = await agent.send(text, context)
    if (!accepted) draft = text
    // A turn takes seconds with the box disabled, which drops focus. Putting
    // it back is what makes a second question as cheap to ask as the first.
    box?.focus()
  }

  function onKeydown(e: KeyboardEvent) {
    // Enter sends, Shift+Enter breaks the line. The other way round is
    // defensible and is not what anyone's fingers expect here.
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      void send()
    }
  }
</script>

<aside class="panel" aria-label="Assistant">
  <header class="head">
    <button
      class="ghost"
      onclick={() => (showHistory = !showHistory)}
      aria-expanded={showHistory}
      title="Past conversations"
    >
      <Icon name="layers" size={16} />
    </button>
    <span class="title">Assistant</span>
    <button
      class="ghost"
      onclick={() => void agent.startThread()}
      disabled={agent.busy}
      title="New conversation"
    >
      <Icon name="plus" size={16} />
    </button>
    <button class="ghost" onclick={() => void agent.toggle()} title="Close">
      <Icon name="close" size={16} />
    </button>
  </header>

  {#if showHistory}
    <div class="history">
      {#if agent.threads.length === 0}
        <p class="empty">Nothing yet.</p>
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
      <p class="lede">The assistant is not set up yet.</p>
      <p class="note">
        Choose a model and add a key in Settings. A model running on this machine — Ollama or LM
        Studio — needs only its address, and nothing you write leaves the machine.
      </p>
    </div>
  {:else}
    <div class="log" bind:this={scroller} onscroll={onScroll}>
      {#if agent.turns.length === 0}
        <div class="opening">
          <p class="lede">Ask about anything in this vault.</p>
          <p class="note">
            It can read and change your journal, tasks, calendar, shelves and trackers. Deletions
            stop and ask first.
          </p>
        </div>
      {/if}

      {#each agent.turns as turn (turn.id)}
        <article class="turn {turn.role}">
          {#if turn.role === 'user'}
            <p class="said">{turn.text}</p>
          {:else}
            {#each turn.cards as card (card.callId)}
              <ToolCardView
                {card}
                onanswer={(ok: boolean) => void agent.confirm(card.callId, ok)}
              />
            {/each}
            {#if turn.text}
              <div class="reply">{turn.text}</div>
            {/if}
            {#if turn.error}
              <p class="failed">{turn.error}</p>
            {/if}
            {#if !turn.text && !turn.error && agent.busy && turn.cards.length === 0}
              <div class="thinking" aria-label="Thinking"><i></i><i></i><i></i></div>
            {/if}
          {/if}
        </article>
      {/each}
    </div>

    {#if agent.error}
      <p class="failed banner">{agent.error}</p>
    {/if}

    <div class="composer">
      <textarea
        bind:this={box}
        bind:value={draft}
        onkeydown={onKeydown}
        placeholder="Ask, or tell it what to do…"
        rows="2"
        disabled={agent.busy}
      ></textarea>
      <button
        class="send"
        onclick={() => void send()}
        disabled={agent.busy || draft.trim() === ''}
        title="Send"
      >
        <Icon name="arrow-up" size={16} />
      </button>
    </div>
  {/if}
</aside>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    width: 340px;
    flex: none;
    min-height: 0;
    border-left: 1px solid var(--border);
    background: var(--bg-panel);
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

  .log {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: var(--sp-3);
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }

  .opening,
  .unset {
    padding: var(--sp-4) var(--sp-3);
  }
  .unset {
    flex: 1;
  }
  .lede {
    margin: 0 0 var(--sp-2);
    font-size: var(--text-base);
    color: var(--fg);
  }
  .note {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }

  .turn {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  /* The person's turn is a bubble and the assistant's is not, which is the
     cheapest way to make a long exchange scannable: your own words are the
     landmarks you scroll to find. */
  .said {
    align-self: flex-end;
    max-width: 85%;
    margin: 0;
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--radius-lg);
    background: var(--bg-selected);
    font-size: var(--text-base);
    line-height: var(--leading-snug);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .reply {
    font-size: var(--text-base);
    line-height: var(--leading-normal);
    color: var(--fg);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .failed {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--danger);
  }
  .banner {
    padding: var(--sp-2) var(--sp-3);
    border-top: 1px solid var(--border);
  }

  .thinking {
    display: flex;
    gap: 4px;
    padding: var(--sp-1) 0;
  }
  .thinking i {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--fg-faint);
    animation: blink 1.2s ease-in-out infinite;
  }
  .thinking i:nth-child(2) {
    animation-delay: 0.15s;
  }
  .thinking i:nth-child(3) {
    animation-delay: 0.3s;
  }
  @keyframes blink {
    0%,
    100% {
      opacity: 0.25;
    }
    50% {
      opacity: 1;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .thinking i {
      animation: none;
      opacity: 0.5;
    }
  }

  .composer {
    display: flex;
    gap: var(--sp-2);
    align-items: flex-end;
    padding: var(--sp-2);
    border-top: 1px solid var(--border);
  }
  textarea {
    flex: 1;
    resize: none;
    padding: var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-base);
    line-height: var(--leading-snug);
  }
  textarea:focus {
    outline: none;
    border-color: var(--accent);
  }
  .send {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    border: 0;
    border-radius: var(--radius);
    background: var(--accent);
    color: var(--fg-on-accent);
    cursor: pointer;
  }
  .send:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .empty {
    margin: 0;
    padding: var(--sp-3);
    font-size: var(--text-sm);
    color: var(--fg-faint);
  }
</style>
