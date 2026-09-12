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

  import { onDestroy } from 'svelte'
  import { agent } from '../lib/agent.svelte'
  import { renderMarkdown } from '../lib/markdown'
  import { panels } from '../lib/panels.svelte'
  import { pref } from '../lib/prefs'
  import { app } from '../lib/state.svelte'
  import { todo } from '../lib/todo.svelte'
  import { assistant, PANE_LABELS } from '../lib/assistant.svelte'
  import { overview } from '../lib/overview.svelte'
  import { purpose } from '../lib/purpose.svelte'
  import { specOf } from '../lib/dashboard'
  import { library } from '../lib/library.svelte'
  import { notes } from '../lib/notes.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import ToolCardView from './ToolCard.svelte'

  let draft = $state('')
  let box = $state<HTMLTextAreaElement | null>(null)
  let scroller = $state<HTMLDivElement | null>(null)
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

  /** Which turn's copy button has just been pressed, for the tick. */
  let copied = $state<string | null>(null)
  let copiedTimer: ReturnType<typeof setTimeout> | null = null

  /**
   * Put a reply on the clipboard as the Markdown it arrived as.
   *
   * Deliberately the source rather than the rendered text: what comes back
   * is usually going somewhere that understands Markdown -- a note, an
   * issue, the entry you were writing -- and flattening the list you are
   * copying is not a kindness. Selecting by hand still gets the plain text,
   * which is the other half of why the log is selectable at all.
   */
  async function copy(id: string, text: string) {
    try {
      await navigator.clipboard.writeText(text)
      copied = id
      if (copiedTimer) clearTimeout(copiedTimer)
      copiedTimer = setTimeout(() => {
        copiedTimer = null
        copied = null
      }, 1600)
    } catch {
      /* a clipboard the webview refuses is not worth a dialog */
    }
  }
  onDestroy(() => {
    if (copiedTimer) clearTimeout(copiedTimer)
  })

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
        if (todo.showingGoals) {
          const goal = purpose.selected ? purpose.goal(purpose.selected) : undefined
          return goal
            ? `the todo app's goals, the goal "${goal.title}"`
            : "the todo app's goals, grouped by role"
        }
        return todo.project ? `the todo app, project "${todo.project.name}"` : 'the todo app'
      case 'calendar':
        return 'the calendar'
      case 'library':
        return library.kind ? `the library, shelf "${library.kind.name}"` : 'the library'
      case 'notes':
        return notes.open ? `the notes app, the note "${notes.title}"` : 'the notes app'
      case 'overview': {
        // Named rather than described: it is a page of whatever cards its
        // owner put on it now, so "where the week adds up by role" would be
        // a claim about somebody else's page. An empty page is one somebody
        // is allowed to have, and saying "showing" followed by nothing at all
        // would be the assistant told a sentence that stops mid-word.
        const cards = overview.widgets.map((w) => specOf(w.type).label.toLowerCase()).slice(0, 6)
        return cards.length > 0
          ? `their overview page, showing ${cards.join(', ')}`
          : 'their overview page, which they have not put anything on yet'
      }
      case 'assistant':
        return `your own routines and what they did, on the "${PANE_LABELS[assistant.pane]}" page`
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
    <div class="log" bind:this={scroller} onscroll={onScroll}>
      {#if agent.turns.length === 0}
        <EmptyState lead="Ask about anything in this vault.">
          {#snippet icon()}<Icon name="sparkle" size={28} weight={1.4} />{/snippet}
          {#snippet note()}
            It can read and change your journal, tasks, calendar, shelves and trackers. Deletions
            stop and ask first.
          {/snippet}
        </EmptyState>
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
              <!-- Rendered rather than shown as it arrived. A model answers
                   in Markdown whatever it is asked, so `white-space:
                   pre-wrap` meant literal asterisks around every bold
                   phrase, numbered lists run together and shell commands in
                   the same face as the sentence around them. See
                   `lib/markdown.ts` -- in particular why the renderer is in
                   this repository and what it escapes before it does
                   anything else. -->
              <!-- The rule below is right in general and this is the case
                   it does not cover: `renderMarkdown` HTML-escapes its input
                   before a single pattern runs, so every tag in what comes
                   back was written by that module. It is checked from five
                   directions in `scripts/markdown.test.mjs`, including a raw
                   `<script>`, an `onerror` attribute and a `javascript:`
                   link. -->
              <!-- eslint-disable-next-line svelte/no-at-html-tags -->
              <div class="reply md">{@html renderMarkdown(turn.text)}</div>
              <!-- Under the reply rather than over it, and quiet until the
                   turn is hovered: a copy button is wanted after reading. -->
              <div class="acts">
                <button
                  class="copy"
                  onclick={() => void copy(turn.id, turn.text)}
                  title="Copy this reply"
                >
                  <Icon name={copied === turn.id ? 'tick' : 'copy'} size={13} weight={1.8} />
                  {copied === turn.id ? 'Copied' : 'Copy'}
                </button>
              </div>
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

  .log {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: var(--sp-4) var(--sp-4) var(--sp-6);
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
    /* The window sets `user-select: none` so that dragging across a list of
       rows does not paint them blue. A conversation is the opposite case:
       it is prose, and an answer you cannot select is an answer you have to
       retype. Everything inside the log -- and the composer -- opts back in. */
    user-select: text;
    cursor: auto;
  }

  .unset {
    display: flex;
    flex: 1;
    min-height: 0;
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
    max-width: 88%;
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
    overflow-wrap: anywhere;
  }

  /* Quiet, and only there once the pointer is on the turn: a copy button on
     every reply, permanently, is a column of grey buttons down the rail. */
  .acts {
    display: flex;
    opacity: 0;
    transition: opacity var(--fast) var(--ease);
  }
  .turn:hover .acts,
  .acts:focus-within {
    opacity: 1;
  }
  .copy {
    display: flex;
    align-items: center;
    gap: 5px;
    height: 24px;
    padding: 0 var(--sp-2);
    margin-left: -6px;
    border-radius: var(--radius-sm);
    font-size: var(--text-xs);
    font-weight: 550;
    color: var(--fg-faint);
  }
  .copy:hover {
    background: var(--bg-hover);
    color: var(--fg-muted);
  }

  /* ── A rendered reply ──────────────────────────────────────────────
     The markup all comes from `lib/markdown.ts`, so this is the complete
     list of what can appear. Sized down from the journal's prose: this is a
     rail beside the thing being talked about, not the page itself. */

  .md :global(> * + *) {
    margin-top: 0.7em;
  }
  .md :global(h1),
  .md :global(h2),
  .md :global(h3),
  .md :global(h4),
  .md :global(h5),
  .md :global(h6) {
    font-size: var(--text-base);
    font-weight: 650;
    line-height: var(--leading-snug);
    margin-top: 1.3em;
  }
  .md :global(h1) {
    font-size: var(--text-md);
  }
  .md :global(strong) {
    font-weight: 650;
  }
  .md :global(em) {
    font-style: italic;
  }
  .md :global(del) {
    color: var(--fg-subtle);
  }
  .md :global(a) {
    color: var(--accent);
  }
  .md :global(ul),
  .md :global(ol) {
    padding-left: 1.35em;
  }
  .md :global(li + li) {
    margin-top: 0.25em;
  }
  .md :global(li > p) {
    margin: 0;
  }
  .md :global(li > p + p) {
    margin-top: 0.5em;
  }
  .md :global(blockquote) {
    margin-left: 0;
    padding-left: 0.9em;
    border-left: 2px solid var(--border-strong);
    color: var(--fg-muted);
  }
  .md :global(hr) {
    border: none;
    border-top: 1px solid var(--border);
    margin: 1.2em 0;
  }
  .md :global(code) {
    font-family: var(--font-mono);
    font-size: 0.88em;
    padding: 0.1em 0.35em;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-sunken);
  }
  .md :global(pre) {
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-sunken);
    /* Scrolled rather than wrapped: a wrapped command is a command that
       cannot be copied and pasted. */
    overflow-x: auto;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
  }
  .md :global(pre code) {
    padding: 0;
    border: none;
    background: none;
    font-size: inherit;
    white-space: pre;
  }
  .md :global(table) {
    border-collapse: collapse;
    width: 100%;
    font-size: var(--text-sm);
  }
  .md :global(th),
  .md :global(td) {
    padding: 4px var(--sp-2);
    border: 1px solid var(--border);
    text-align: left;
  }
  .md :global(th) {
    background: var(--bg-sunken);
    font-weight: 650;
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
    user-select: text;
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
  .none {
    margin: 0;
    padding: var(--sp-3);
    font-size: var(--text-sm);
    color: var(--fg-faint);
  }
</style>
