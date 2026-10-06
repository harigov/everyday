<script lang="ts">
  // The conversation itself: what was said, what ran while it was said, and
  // the box to say the next thing in.
  //
  // Drawn in two places. The Assistant app gives it the whole window, because
  // that is where somebody sits down to talk; the rail gives it a column
  // beside whichever app is open, because that is where somebody asks about
  // what they are looking at. It is the same thread in both -- see
  // `agent.svelte.ts` -- so it is the same component, and the two differ
  // only in how much room they have and what they say when empty.
  //
  // The part that matters most here is what a turn shows *while it runs*. A
  // reply that searches twice, reads a page and checks a forecast takes the
  // better part of a minute, and the panel used to show three dots until the
  // first tool card appeared and then nothing at all -- so a turn that was
  // working and a turn that had wedged looked the same. Now a running turn
  // always ends in one line saying what it is doing and for how long, with a
  // Stop beside the composer; its plan, when it made one, ticks itself off
  // as it goes; and once it is over, a long list of steps folds away under
  // one line so the answer is what is left to read.

  import { onDestroy } from 'svelte'
  import { activity, elapsed, isPlan, planOf, workSummary, type Turn } from '../lib/agent'
  import { agent } from '../lib/agent.svelte'
  import { APPS } from '../lib/apps'
  import { lookOf } from '../lib/companion'
  import { splitDigest } from '../lib/dream'
  import { renderMarkdown } from '../lib/markdown'
  import { headline, toWire } from '../lib/onscreen'
  import { panels } from '../lib/panels.svelte'
  import { onScreenNow } from '../lib/screen.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import ToolCardView from './ToolCard.svelte'

  let {
    variant,
  }: {
    /** `page` is the Assistant app; `rail` is the column beside any other. */
    variant: 'page' | 'rail'
  } = $props()

  // ── What goes along with a message ─────────────────────────────────
  //
  // References to whatever is on screen -- the open thread, the selected
  // task -- which the service looks up and describes for the model; see
  // `lib/onscreen.ts`. The rail says which in a strip above the box, because
  // a message that quietly carries "and this email" should not be a
  // surprise, and lets it be left out of one message with a click.
  //
  // On the page there is nothing to say: the conversation is the whole
  // window, so the only thing on screen is the Assistant app itself.

  const screen = $derived(onScreenNow())
  const seenNow = $derived(headline(screen.showing))
  /** Whether the next message carries the screen. On by default, and back
   *  on after every send and whenever what is on screen changes -- leaving
   *  out one email is not leaving out the next one. */
  let withScreen = $state(true)
  /** Which record the strip names, as a string: `seenNow` is a fresh object
   *  whenever anything in the open app's store moves -- a sync, an autosave
   *  -- and an effect on it would turn the switch back on under somebody who
   *  had just turned it off. A string only changes when the record does. */
  const seenKey = $derived(seenNow ? `${seenNow.kind}:${seenNow.id}` : screen.app)
  $effect(() => {
    void seenKey
    withScreen = true
  })
  const appName = $derived(screen.app === 'settings' ? 'Settings' : APPS[screen.app].label)

  let draft = $state('')
  let box = $state<HTMLTextAreaElement | null>(null)
  let scroller = $state<HTMLDivElement | null>(null)

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

  // ── The clock a running turn is on ──────────────────────────────────
  //
  // Ticks once a second while something is running and not at all
  // otherwise. The seconds are not decoration: "Searching the web · 4s" and
  // "Searching the web · 48s" are different situations, and the second is
  // the one where somebody reaches for Stop.

  let now = $state(Date.now())
  $effect(() => {
    if (!agent.busy) return
    now = Date.now()
    const timer = setInterval(() => (now = Date.now()), 1000)
    return () => clearInterval(timer)
  })

  // ── Folding a finished turn's steps ────────────────────────────────
  //
  // Three cards are a sentence and twelve are a wall. Once a turn is over,
  // more than a couple of steps fold under one line -- "Worked for 18s · 5
  // steps" -- and the answer is what the eye lands on. While it runs they
  // are all open, because then they *are* the answer so far.

  /** Turns somebody has unfolded, by id. */
  let unfolded = $state<Record<string, boolean>>({})
  const FOLD_OVER = 2

  function foldable(turn: Turn, steps: number): boolean {
    return turn.phase === 'done' && steps > FOLD_OVER
  }

  // ── Following the reply as it streams ──────────────────────────────
  //
  // Only from the bottom: a person who has scrolled up to read something is
  // reading it, and yanking them back down every few tokens is the single
  // most irritating thing a chat panel can do.

  let pinned = $state(true)
  function onScroll() {
    if (!scroller) return
    pinned = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 60
  }
  $effect(() => {
    // Touch what should retrigger this: the turns, the text growing, a card
    // arriving, the status line appearing.
    agent.turns.map((t) => t.text.length + t.cards.length + t.thinking.length)
    void agent.busy
    if (pinned && scroller) scroller.scrollTop = scroller.scrollHeight
  })

  // Opening a different thread starts at its foot, wherever the last one
  // was scrolled to.
  $effect(() => {
    void agent.conversationId
    pinned = true
  })

  // ── The composer ────────────────────────────────────────────────────

  // The panel is opened in order to type in it, so the caret starts here
  // rather than making the first thing anyone does be a click -- and moves
  // back here whenever something asks for it (`agent.focusComposer`).
  //
  // Depends on the textarea and the tick and nothing else. It used to read
  // `agent.ready`, which reads the settings -- so every save in the
  // Assistant settings re-ran it and pulled focus out of the dialog into the
  // composer behind it, and the next thing typed went to the wrong box.
  $effect(() => {
    void agent.focusTick
    box?.focus()
  })

  /** Grow with what is typed, up to a limit, rather than scroll a slit. */
  function fit() {
    if (!box) return
    box.style.height = 'auto'
    const max = variant === 'page' ? 240 : 160
    box.style.height = `${Math.min(box.scrollHeight, max)}px`
  }
  $effect(() => {
    void draft
    fit()
  })

  // The dog listens while something is typed and not yet sent. Cleared on
  // the way out, so a rail closed mid-sentence does not leave it listening.
  $effect(() => {
    agent.composing = draft.trim() !== ''
  })
  onDestroy(() => {
    agent.composing = false
  })

  async function send(text = draft) {
    if (agent.busy || !text.trim()) return
    // Cleared optimistically, because leaving the question in the box reads
    // as though nothing was sent. Put back if the store refuses it -- which
    // it does when there is no thread open -- so a paragraph is never
    // silently eaten.
    const was = draft
    draft = ''
    const accepted = await agent.send(
      text,
      variant === 'rail' && withScreen ? toWire(screen.app, screen.showing) : null,
    )
    withScreen = true
    if (!accepted) draft = was
    box?.focus()
  }

  function onKeydown(e: KeyboardEvent) {
    // Enter sends, Shift+Enter breaks the line. The other way round is
    // defensible and is not what anyone's fingers expect here. While a turn
    // is running Enter does nothing -- the box stays open so the next
    // question can be written while this one is answered, and Escape stops
    // the one running, the way it stops everything else.
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault()
      void send()
    } else if (e.key === 'Escape' && agent.busy) {
      e.preventDefault()
      e.stopPropagation()
      void agent.stop()
    }
  }

  // ── When there is nothing yet ──────────────────────────────────────

  const web = $derived(agent.settings?.web === true)

  /** A dog floats over the top of the thread, and is its face: no sparkle. */
  const dog = $derived(agent.settings !== null && lookOf(agent.settings.companion) !== null)

  /**
   * Somewhere to start, on the page. Each is a thing it can actually do in
   * this vault with these settings -- a suggestion that ends in "web access
   * is off" is a demonstration of a limitation, not of the assistant.
   */
  const suggestions = $derived(
    web
      ? [
          'What is on my plate today?',
          'What is the weather looking like this weekend?',
          'Plan my week around what is already on the calendar',
          'Find a quick recipe for dinner tonight and add what I need to my list',
        ]
      : [
          'What is on my plate today?',
          'Plan my week around what is already on the calendar',
          'What have I been writing about in my journal lately?',
          'Which of my goals have I not touched this month?',
        ],
  )
</script>

{#snippet sparkle()}<Icon name="sparkle" size={28} weight={1.4} />{/snippet}

<div class="thread {variant}">
  <div class="log scroll" bind:this={scroller} onscroll={onScroll}>
    <div class="column">
      {#if agent.turns.length === 0}
        {#if variant === 'page'}
          <div class="hello">
            {#if !dog}
              <span class="glyph"><Icon name="sparkle" size={30} weight={1.4} /></span>
            {/if}
            <h2>What can I do for you?</h2>
            <p>
              It can read and change your journal, notes, tasks, calendar, library and trackers{web
                ? ', and look things up on the web'
                : ''}. Deletions stop and ask first.
            </p>
            <div class="starts">
              {#each suggestions as s (s)}
                <button class="start" onclick={() => void send(s)}>{s}</button>
              {/each}
            </div>
            {#if !web}
              <p class="aside">
                It cannot reach the web yet, so searching, reading pages and the weather are off.
                <button class="link" onclick={() => panels.openSettings('assistant')}>
                  Turn them on
                </button>
              </p>
            {/if}
          </div>
        {:else}
          <EmptyState lead="Ask about anything in this vault." icon={dog ? undefined : sparkle}>
            {#snippet note()}
              It can read and change your journal, tasks, calendar, shelves and trackers{web
                ? ', and look things up on the web'
                : ''}. Deletions stop and ask first.
            {/snippet}
          </EmptyState>
        {/if}
      {/if}

      {#each agent.turns as turn (turn.id)}
        <article class="turn {turn.role}">
          {#if turn.role === 'user'}
            <!-- A dream's opening message is its digest wearing a sentence.
                 The words ahead of the marker are drawn as any other turn;
                 what follows it is data the person did not write and would
                 not otherwise see, so it is folded rather than dropped -- see
                 `splitDigest` and docs/plans/dreaming.md. -->
            {@const { text, digest } = splitDigest(turn.text)}
            {#if text}<p class="said">{text}</p>{/if}
            {#if digest}
              <details class="digest">
                <summary>What it looked at</summary>
                <!-- Safe by the same construction as the reply below: see the
                     note on `renderMarkdown` there. -->
                <!-- eslint-disable-next-line svelte/no-at-html-tags -->
                <div class="reply md">{@html renderMarkdown(digest)}</div>
              </details>
            {/if}
          {:else}
            {@const plan = planOf(turn)}
            {@const steps = turn.cards.filter((c) => !isPlan(c))}
            {@const status = activity(turn)}

            {#if turn.thinking}
              <!-- The model's own reasoning, when its provider streams any.
                   Folded, because it is how the answer was reached rather
                   than the answer, and kept only for as long as the window
                   is open -- the vault does not store it. -->
              <details class="reasoning">
                <summary>
                  {turn.phase === 'thinking' && !turn.text ? 'Thinking…' : 'How it reasoned'}
                </summary>
                <p>{turn.thinking}</p>
              </details>
            {/if}

            {#if plan}
              <!-- The plan, as one checklist that updates in place rather
                   than a card per revision: see `planOf`. -->
              <ol class="plan" aria-label="Plan">
                {#each plan as step, i (i)}
                  <li class={step.status}>
                    <span class="box" aria-hidden="true">
                      {#if step.status === 'done'}
                        <Icon name="tick" size={12} weight={2.2} />
                      {:else if step.status === 'active'}
                        <span class="spin"></span>
                      {/if}
                    </span>
                    <span>{step.text}</span>
                  </li>
                {/each}
              </ol>
            {/if}

            {#if steps.length > 0}
              {#if foldable(turn, steps.length)}
                <button
                  class="worked"
                  aria-expanded={unfolded[turn.id] === true}
                  onclick={() => (unfolded[turn.id] = !unfolded[turn.id])}
                >
                  <span class="chev" class:open={unfolded[turn.id]}>
                    <Icon name="chevron" size={12} weight={2} />
                  </span>
                  {workSummary(turn)}
                </button>
              {/if}
              {#if !foldable(turn, steps.length) || unfolded[turn.id]}
                <div class="steps">
                  {#each steps as card (card.callId)}
                    <ToolCardView
                      {card}
                      onanswer={(answer: 'confirm' | 'decline' | 'later') =>
                        void agent.confirm(card.callId, answer)}
                    />
                  {/each}
                </div>
              {/if}
            {/if}

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
            {/if}

            {#if turn.error}
              <p class="failed">{turn.error}</p>
            {/if}

            {#if status}
              <!-- Always the last thing in a running turn, whatever else it
                   has drawn: this is the line that says it is still going. -->
              <div class="status" role="status" aria-live="polite">
                <span class="pulse" aria-hidden="true"><i></i><i></i><i></i></span>
                <span class="doing">{status}…</span>
                {#if turn.startedAt !== null}
                  <span class="clock">{elapsed(now - turn.startedAt)}</span>
                {/if}
              </div>
            {:else if turn.stopped}
              <p class="note">Stopped.</p>
            {/if}

            {#if turn.text && turn.phase === 'done'}
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
          {/if}
        </article>
      {/each}
    </div>
  </div>

  {#if agent.error}
    <p class="failed banner">{agent.error}</p>
  {/if}

  <div class="composer">
    {#if variant === 'rail'}
      <button
        class="seen"
        class:off={!withScreen}
        onclick={() => (withScreen = !withScreen)}
        aria-pressed={withScreen}
        title={withScreen
          ? 'Sent with your message so “this” means what you are looking at. Click to leave it out.'
          : 'Left out of your next message. Click to send it.'}
      >
        <Icon name={withScreen ? 'eye' : 'hidden'} size={13} />
        <span class="seen-app">{appName}</span>
        {#if seenNow}
          <span class="seen-sep" aria-hidden="true">›</span>
          <span class="seen-what">{seenNow.label}</span>
        {/if}
      </button>
    {/if}
    <div class="compose">
      <textarea
        bind:this={box}
        bind:value={draft}
        onkeydown={onKeydown}
        placeholder={variant === 'page'
          ? 'Ask anything, or tell it what to do…'
          : 'Ask, or tell it what to do…'}
        rows={variant === 'page' ? 1 : 2}
        aria-label="Message the assistant"
      ></textarea>
      {#if agent.busy}
        <!-- Stop where Send was, so the thing to press is where the hand
             already is. The box stays writable: the next question can be
             typed while this one is answered. -->
        <button class="send stop" onclick={() => void agent.stop()} title="Stop (Esc)">
          <Icon name="stop" size={14} weight={2} />
        </button>
      {:else}
        <button
          class="send"
          onclick={() => void send()}
          disabled={draft.trim() === ''}
          title="Send (Enter)"
        >
          <Icon name="arrow-up" size={16} />
        </button>
      {/if}
    </div>
  </div>
</div>

<style>
  .thread {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
  }

  .log {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    /* The window sets `user-select: none` so that dragging across a list of
       rows does not paint them blue. A conversation is the opposite case:
       it is prose, and an answer you cannot select is an answer you have to
       retype. Everything inside the log -- and the composer -- opts back in. */
    user-select: text;
    cursor: auto;
  }
  /* `--thread-top` is room for whatever floats over the top of the thread
     -- the dog and its name -- which the conversation scrolls under. */
  .column {
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
    padding: calc(var(--thread-top, 0px) + var(--sp-4)) var(--sp-4) var(--sp-6);
  }
  /* On the page, a reading column rather than the window's whole width: a
     line of prose 1400px long is a line nobody can find the start of the
     next one from. */
  .page .column {
    max-width: 760px;
    margin: 0 auto;
    padding: calc(var(--thread-top, 0px) + var(--sp-8)) var(--sp-6) var(--sp-10);
    gap: var(--sp-6);
  }

  /* ── Empty, on the page ───────────────────────────────────────────── */

  .hello {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--sp-3);
    /* The same drop from the top of the page with a dog over it or not: the
       dog's room counts towards it, so the greeting sits under the dog. */
    margin-top: max(var(--sp-4), calc(12vh - var(--thread-top, 0px)));
    text-align: center;
  }
  .glyph {
    color: var(--accent);
  }
  .hello h2 {
    margin: 0;
    font-size: var(--text-xl);
    font-weight: 650;
  }
  .hello p {
    max-width: 46ch;
    margin: 0;
    color: var(--fg-muted);
    line-height: var(--leading-normal);
  }
  .starts {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--sp-2);
    width: 100%;
    margin-top: var(--sp-4);
  }
  .start {
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    color: var(--fg-muted);
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
    text-align: left;
    transition:
      border-color var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .start:hover {
    border-color: var(--border-strong);
    color: var(--fg);
  }
  .hello .aside {
    font-size: var(--text-sm);
    color: var(--fg-faint);
  }
  .link {
    color: var(--accent);
    font: inherit;
  }
  .link:hover {
    text-decoration: underline;
  }

  /* ── A turn ───────────────────────────────────────────────────────── */

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
  .page .said {
    max-width: 80%;
    padding: var(--sp-2) var(--sp-4);
  }
  .reply {
    font-size: var(--text-base);
    line-height: var(--leading-normal);
    color: var(--fg);
    overflow-wrap: anywhere;
  }

  /* The digest a dream was given, folded under its opening message. Aligned
     with the reply below rather than with the person's own bubble, since it
     is the assistant's material even though it arrived on a `user` turn. */
  .digest {
    align-self: flex-start;
    max-width: 100%;
    font-size: var(--text-sm);
  }
  .digest summary,
  .reasoning summary {
    color: var(--fg-faint);
    cursor: pointer;
    user-select: none;
  }
  .digest summary:hover,
  .reasoning summary:hover {
    color: var(--fg-muted);
  }
  .digest[open] summary {
    margin-bottom: var(--sp-2);
  }
  .digest .reply {
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }

  .reasoning {
    font-size: var(--text-sm);
  }
  .reasoning p {
    max-height: 220px;
    overflow-y: auto;
    margin: var(--sp-2) 0 0;
    padding-left: var(--sp-3);
    border-left: 2px solid var(--border);
    color: var(--fg-subtle);
    white-space: pre-wrap;
    line-height: var(--leading-snug);
  }

  /* ── The plan ─────────────────────────────────────────────────────── */

  .plan {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin: 0;
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    list-style: none;
    font-size: var(--text-sm);
  }
  .plan li {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    color: var(--fg-muted);
    line-height: var(--leading-snug);
  }
  .plan li.active {
    color: var(--fg);
    font-weight: 550;
  }
  .plan li.done {
    color: var(--fg-faint);
  }
  .plan li.done > span:last-child {
    text-decoration: line-through;
    text-decoration-color: color-mix(in oklab, var(--fg-faint) 60%, transparent);
  }
  .box {
    display: grid;
    flex: none;
    place-items: center;
    width: 15px;
    height: 15px;
    margin-top: 1px;
    border: 1.5px solid var(--border-strong);
    border-radius: 4px;
  }
  .plan li.done .box {
    border-color: var(--accent);
    background: var(--accent);
    color: var(--fg-on-accent);
  }
  .plan li.active .box {
    border-color: var(--accent);
  }
  .spin {
    width: 8px;
    height: 8px;
    border: 1.5px solid color-mix(in oklab, var(--accent) 25%, transparent);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: turn 0.8s linear infinite;
  }
  @keyframes turn {
    to {
      rotate: 360deg;
    }
  }

  /* ── Steps, and the line they fold under ──────────────────────────── */

  .steps {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .worked {
    display: flex;
    align-items: center;
    align-self: flex-start;
    gap: 6px;
    padding: 2px var(--sp-2) 2px 4px;
    margin-left: -4px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }
  .worked:hover {
    background: var(--bg-hover);
    color: var(--fg-muted);
  }
  .chev {
    display: grid;
    place-items: center;
    transition: rotate var(--fast) var(--ease);
  }
  .chev.open {
    rotate: 90deg;
  }

  /* ── Still going ──────────────────────────────────────────────────── */

  .status {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-height: 22px;
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }
  .doing {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    /* A sheen across the words, so the line reads as live even in the
       stretch where nothing about it changes. */
    background: linear-gradient(
        90deg,
        var(--fg-muted) 0%,
        var(--fg-muted) 40%,
        var(--fg) 50%,
        var(--fg-muted) 60%,
        var(--fg-muted) 100%
      )
      0 0 / 250% 100%;
    -webkit-background-clip: text;
    background-clip: text;
    color: transparent;
    animation: sheen 2.2s linear infinite;
  }
  @keyframes sheen {
    from {
      background-position: 100% 0;
    }
    to {
      background-position: -150% 0;
    }
  }
  .clock {
    flex: none;
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }
  .pulse {
    display: flex;
    flex: none;
    gap: 3px;
  }
  .pulse i {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--accent);
    animation: blink 1.2s ease-in-out infinite;
  }
  .pulse i:nth-child(2) {
    animation-delay: 0.15s;
  }
  .pulse i:nth-child(3) {
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
    .pulse i,
    .spin {
      animation: none;
      opacity: 0.6;
    }
    .doing {
      animation: none;
      background: none;
      color: var(--fg-muted);
    }
  }

  .note {
    margin: 0;
    color: var(--fg-faint);
    font-size: var(--text-sm);
    font-style: italic;
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
     conversation beside the things being talked about, not the page itself. */

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

  /* ── The composer ─────────────────────────────────────────────────── */

  .composer {
    padding: var(--sp-2);
    border-top: 1px solid var(--border);
  }

  /* What goes along with the message: one quiet line over the box, the
     size of a caption, struck through when it is being left out. */
  .seen {
    display: flex;
    align-items: center;
    gap: 5px;
    max-width: 100%;
    margin: 0 0 var(--sp-2);
    padding: 2px var(--sp-2);
    border-radius: 999px;
    background: var(--bg-hover);
    color: var(--fg-muted);
    font-size: var(--text-xs);
    line-height: 1.6;
  }
  .seen:hover {
    color: var(--fg);
  }
  .seen.off {
    background: none;
    color: var(--fg-faint);
  }
  .seen.off .seen-app,
  .seen.off .seen-what {
    text-decoration: line-through;
  }
  .seen-app {
    flex: none;
    font-weight: 600;
  }
  .seen-sep {
    flex: none;
    color: var(--fg-faint);
  }
  .seen-what {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* On the page it floats at the foot of the reading column instead of
     spanning the window under a rule: the box is the one control on the
     page, and it sits where the conversation is. */
  .page .composer {
    width: 100%;
    max-width: 760px;
    margin: 0 auto;
    padding: 0 var(--sp-6) var(--sp-5);
    border-top: 0;
  }
  .compose {
    display: flex;
    gap: var(--sp-2);
    align-items: flex-end;
  }
  .page .compose {
    padding: var(--sp-2) var(--sp-2) var(--sp-2) var(--sp-3);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-xl);
    background: var(--bg-raised);
    box-shadow: var(--shadow);
  }
  .page .compose:focus-within {
    border-color: var(--accent);
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
  .page textarea {
    padding: 6px 0;
    border: 0;
    background: none;
  }
  .send {
    display: grid;
    flex: none;
    place-items: center;
    width: 32px;
    height: 32px;
    border: 0;
    border-radius: var(--radius);
    background: var(--accent);
    color: var(--fg-on-accent);
    cursor: pointer;
  }
  .page .send {
    border-radius: 50%;
  }
  .send:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .send.stop {
    background: var(--fg);
    color: var(--bg);
  }
</style>
