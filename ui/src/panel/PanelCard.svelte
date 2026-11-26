<script lang="ts">
  // One card: what the shell said about it, drawn. It decides nothing -- a
  // press goes up to the stack, which knows what is switched off and tells the
  // shell. See `panel.ts` for what a card holds and `cards.ts` for the rules.
  //
  // Top to bottom:
  //
  // - the head, which is also the grip the panel is moved by: the app's mark
  //   and name, so a card is never a stranger's, the kicker in the colour of
  //   the card's state, and the fold button; then the symbol of the app the
  //   card is about, in that app's colour, beside the title -- and a countdown,
  //   a meter or a spinner where the card has one;
  // - what is happening now, never scrolled away: the activity and the clock;
  // - the blocks, which scroll inside the card when a long brief would push
  //   the buttons off the screen;
  // - the box to type in and the answers to pick, over the buttons;
  // - the buttons, the primary one rightmost;
  // - along the bottom edge, the time left before the card goes by itself,
  //   draining, and stopped while the card is held: the pointer on it, someone
  //   typing in it, or words in its box not yet sent.
  import Logo from '../components/Logo.svelte'
  import Symbol from '../components/Symbol.svelte'
  import Glyph from './Glyph.svelte'
  import LevelMeter from './LevelMeter.svelte'
  import ActivityBlock from './blocks/ActivityBlock.svelte'
  import ChoicesBlock from './blocks/ChoicesBlock.svelte'
  import ClockBlock from './blocks/ClockBlock.svelte'
  import ConversationBlock from './blocks/ConversationBlock.svelte'
  import EventBlock from './blocks/EventBlock.svelte'
  import InputBlock from './blocks/InputBlock.svelte'
  import ListBlock from './blocks/ListBlock.svelte'
  import MailBlock from './blocks/MailBlock.svelte'
  import NoteBlock from './blocks/NoteBlock.svelte'
  import PeopleBlock from './blocks/PeopleBlock.svelte'
  import ProposalsBlock from './blocks/ProposalsBlock.svelte'
  import QuoteBlock from './blocks/QuoteBlock.svelte'
  import TaskBlock from './blocks/TaskBlock.svelte'
  import TextBlock from './blocks/TextBlock.svelte'
  import {
    actionGlyph,
    drawnBlocks,
    drawnOrder,
    expiryKey,
    expiryView,
    hasFolded,
    kindLook,
    silence,
    splitBlocks,
    splitCount,
    sticksToBottom,
    toneColour,
    toneMark,
  } from './cards'
  import type { Decision, Panel, PanelAction, PanelCheck, PanelInput, Proposal } from './panel'

  let {
    card,
    levels,
    open,
    folded,
    locked,
    holding,
    budget,
    checkOf,
    proposalOf,
    inputOf,
    onpress,
    oncheck,
    ondecide,
    ondraft,
    onmode,
    onsubmit,
    ontoggle,
    onfold,
  }: {
    card: Panel
    /** The meter's bars, once the first reading has come. */
    levels: readonly number[] | undefined
    /** The folded blocks have been opened ("More"). */
    open: boolean
    /** The card is folded down to its head. */
    folded: boolean
    /** A button was pressed and the card has not changed since: they are off. */
    locked: boolean
    /**
     * The page holds the card -- the pointer is on it, a box in it has the
     * keyboard, or words wait in its box -- and has told the shell so.
     */
    holding: boolean
    /** How tall the blocks may grow before they scroll, in pixels. */
    budget: number
    /** How a box is drawn: ticked or not, and whether it is waiting on the shell. */
    checkOf: (check: PanelCheck) => { done: boolean; locked: boolean }
    /** How an action item is drawn: where it stands, and whether it is waiting on the shell. */
    proposalOf: (item: Proposal) => { state: Decision; locked: boolean }
    /** What is in a box to type in, its mode, and whether sending is off. */
    inputOf: (input: PanelInput) => { draft: string; picked: string | undefined; locked: boolean }
    onpress: (action: PanelAction) => void
    oncheck: (check: PanelCheck) => void
    ondecide: (item: Proposal, to: 'accepted' | 'rejected') => void
    ondraft: (input: PanelInput, text: string) => void
    onmode: (input: PanelInput, mode: string) => void
    onsubmit: (input: PanelInput, text: string) => void
    ontoggle: () => void
    onfold: () => void
  } = $props()

  /** What the meter shows before the first reading. */
  const QUIET = silence()

  const look = $derived(kindLook(card.kind))
  const mark = $derived(toneMark(card.tone))
  const count = $derived(card.count ? splitCount(card.count) : null)
  const parts = $derived(splitBlocks(drawnBlocks(card.blocks, open)))
  const foldable = $derived(hasFolded(card.blocks))
  const actions = $derived(drawnOrder(card.actions))
  const expiry = $derived(expiryView(card.expires))
  const stick = $derived(sticksToBottom(parts.body))

  // The scrolling part keeps to its foot while it ends in a conversation and
  // the person has not scrolled up to read something earlier.
  let scroller = $state<HTMLElement | null>(null)
  let atFoot = true

  function scrolled() {
    if (!scroller) return
    atFoot = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 24
  }

  $effect(() => {
    void card
    const el = scroller
    if (!el || !stick || !atFoot) return
    el.scrollTop = el.scrollHeight
  })
</script>

<section
  class="card"
  class:folded
  class:held={(expiry?.held ?? false) || holding}
  data-tone={card.tone}
  style:--kind={look.colour}
  style:--tone={toneColour(card.tone)}
  style:--blocks-max="{budget}px"
  aria-label="{card.kicker}: {card.title}"
>
  <header class="head" data-grip>
    <div class="mast">
      <span class="brand" title="Drag to move the panel · double-click to put it back">
        <Logo size={14} />
        <span>Karma Yoga</span>
      </span>
      <span class="kicker">
        {#if mark === 'dot'}
          <span class="dot" aria-hidden="true"></span>
        {:else if mark === 'check'}
          <Glyph name="tick" size={12} weight={2.25} />
        {:else if mark === 'alert'}
          <Glyph name="alert" size={12} weight={2.25} />
        {/if}
        <span class="said">{card.kicker}</span>
      </span>
      {#if card.busy}
        <span class="pc-spinner busy" role="status" aria-label="Working"></span>
      {/if}
      <button
        type="button"
        class="fold"
        aria-expanded={!folded}
        aria-label={folded ? 'Unfold the card' : 'Fold the card'}
        title={folded ? 'Unfold' : 'Fold'}
        onclick={onfold}
      >
        <Glyph name="chevron" size={14} weight={2} />
      </button>
    </div>
    <div class="lead">
      <span class="symbol" aria-hidden="true"><Symbol name={look.symbol} size={20} /></span>
      <div class="titles">
        <h2 class="title">{card.title}</h2>
        {#if card.body && !folded}<p class="body">{card.body}</p>{/if}
      </div>
      {#if card.meter}
        <LevelMeter levels={levels ?? QUIET} />
      {/if}
      {#if count}
        <p class="count">
          <span class="num">{count.value}</span>
          {#if count.unit}<span class="unit">{count.unit}</span>{/if}
        </p>
      {/if}
    </div>
  </header>

  {#if !folded}
    {#if parts.top.length > 0}
      <div class="now">
        {#each parts.top as { key, block } (key)}
          {#if block.type === 'activity'}
            <ActivityBlock {block} />
          {:else if block.type === 'clock'}
            <ClockBlock {block} />
          {/if}
        {/each}
      </div>
    {/if}

    {#if parts.body.length > 0}
      <div class="scroll" bind:this={scroller} onscroll={scrolled}>
        {#each parts.body as { key, block } (key)}
          {#if block.type === 'conversation'}
            <ConversationBlock {block} />
          {:else if block.type === 'event'}
            <EventBlock {block} cardTitle={card.title} />
          {:else if block.type === 'mail'}
            <MailBlock {block} cardTitle={card.title} />
          {:else if block.type === 'people'}
            <PeopleBlock {block} {open} {ontoggle} />
          {:else if block.type === 'task'}
            {@const action = block.check}
            <TaskBlock
              {block}
              view={action === undefined ? null : checkOf({ action, done: block.done })}
              oncheck={() => {
                if (action !== undefined) oncheck({ action, done: block.done })
              }}
            />
          {:else if block.type === 'note'}
            <NoteBlock {block} cardTitle={card.title} />
          {:else if block.type === 'text'}
            <TextBlock {block} />
          {:else if block.type === 'quote'}
            <QuoteBlock {block} />
          {:else if block.type === 'list'}
            <ListBlock {block} {checkOf} {oncheck} />
          {:else if block.type === 'proposals'}
            <ProposalsBlock {block} viewOf={proposalOf} {ondecide} />
          {/if}
        {/each}
      </div>
    {/if}

    {#if parts.pinned.length > 0}
      <div class="pinned">
        {#each parts.pinned as { key, block } (key)}
          {#if block.type === 'choices'}
            <ChoicesBlock {block} {locked} {onpress} />
          {:else if block.type === 'input'}
            {@const box = inputOf(block)}
            <InputBlock
              {block}
              draft={box.draft}
              picked={box.picked}
              locked={box.locked}
              ondraft={(text: string) => ondraft(block, text)}
              onmode={(mode: string) => onmode(block, mode)}
              onsubmit={(text: string) => onsubmit(block, text)}
            />
          {/if}
        {/each}
      </div>
    {/if}

    {#if foldable || actions.length > 0}
      <footer class="foot">
        {#if foldable}
          <button type="button" class="more" aria-expanded={open} onclick={ontoggle}>
            {open ? 'Less' : 'More'}
            <span class="turn" class:open><Glyph name="chevron" size={12} weight={2.25} /></span>
          </button>
        {/if}
        <span class="actions">
          {#each actions as action (action.id)}
            {@const glyph = actionGlyph(action.icon)}
            <button
              type="button"
              class="act"
              class:primary={action.primary}
              class:danger={action.danger}
              disabled={locked}
              onclick={() => onpress(action)}
            >
              {#if glyph}<Glyph name={glyph} size={15} weight={1.9} />{/if}
              <span>{action.label}</span>
            </button>
          {/each}
        </span>
      </footer>
    {/if}
  {/if}

  {#if expiry}
    {#key expiryKey(card.expires)}
      <span class="expiry" aria-hidden="true">
        <i
          style:animation-duration="{expiry.durationMs}ms"
          style:animation-delay="{expiry.delayMs}ms"
        ></i>
      </span>
    {/key}
  {/if}
</section>

<style>
  /* The card: the theme's floating fill -- nearly opaque in every theme, so
     whatever is behind the window never shows through the words -- washed at
     the top with a breath of its app's colour; a hairline rim, the theme's
     highlight along it where the theme is glass, and a shadow that fits in
     the window's margin (`--shadow`: the larger `--shadow-lg` would be cut
     off square at the window's edge). */
  .card {
    position: relative;
    isolation: isolate;
    overflow: hidden;
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    padding: 12px 16px 14px;
    border-radius: var(--radius-xl);
    background-color: var(--float);
    background-image: linear-gradient(
      to bottom,
      color-mix(in oklab, var(--kind) 9%, transparent),
      transparent 96px
    );
    color: var(--fg);
    box-shadow:
      var(--edge),
      0 0 0 1px var(--border-strong),
      var(--shadow);
    font-family: var(--font-ui);
    -webkit-user-select: none;
    user-select: none;
    cursor: default;
  }

  /* Frosted where the theme is glass: what is under the card blurred, as the
     main window's menus are (`.frost` in `app.css`) -- on a layer behind the
     content, so nothing inside becomes its containing block. Where the
     webview draws no backdrop filter, the fill alone carries the card. */
  .card::before {
    content: '';
    position: absolute;
    inset: 0;
    z-index: -1;
    border-radius: inherit;
    -webkit-backdrop-filter: var(--glass-blur);
    backdrop-filter: var(--glass-blur);
    pointer-events: none;
  }

  /* A card that wants something of the person, or has failed, says so at its
     rim as well as in its kicker. */
  .card[data-tone='attention'],
  .card[data-tone='trouble'] {
    box-shadow:
      var(--edge),
      0 0 0 1px color-mix(in oklab, var(--tone) 55%, var(--border-strong)),
      var(--shadow);
  }

  /* ── the head ─────────────────────────────────────────────────────────── */

  .head {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }

  .mast {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
    height: 20px;
    font-size: var(--text-xs);
    font-weight: 600;
    line-height: 1;
  }

  .brand {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    color: var(--fg-muted);
  }

  /* A dot between the app's name and the kicker. */
  .brand::after {
    content: '';
    width: 3px;
    height: 3px;
    margin-left: 3px;
    border-radius: 50%;
    background: var(--fg-faint);
  }

  .kicker {
    flex: 1;
    min-width: 0;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    color: color-mix(in oklab, var(--tone) 72%, var(--fg));
  }

  .kicker .said {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .dot {
    flex: none;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--tone);
  }

  /* Listening with no meter beside it: the dot is the only sign the
     microphone is open, so it breathes. */
  .card[data-tone='listening'] .dot {
    animation: pc-breathe 2s ease-in-out infinite;
  }

  .busy {
    width: 12px;
    height: 12px;
    color: var(--tone);
  }

  .fold {
    flex: none;
    width: 24px;
    height: 24px;
    margin: -2px -6px -2px 0;
    padding: 0;
    display: grid;
    place-items: center;
    border: 0;
    border-radius: var(--radius-pill);
    background: transparent;
    color: var(--fg-subtle);
    cursor: pointer;
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }

  .fold:hover {
    background: color-mix(in oklab, var(--fg) 8%, transparent);
    color: var(--fg);
  }

  /* Pointing up while there is something to fold away, down when folded. */
  .fold :global(svg) {
    transform: rotate(-90deg);
    transition: transform var(--med) var(--ease);
  }

  .folded .fold :global(svg) {
    transform: rotate(90deg);
  }

  .lead {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 0;
  }

  .symbol {
    flex: none;
    align-self: flex-start;
    width: 36px;
    height: 36px;
    display: grid;
    place-items: center;
    border-radius: var(--radius-lg);
    background: color-mix(in oklab, var(--kind) 15%, transparent);
    box-shadow: inset 0 0 0 1px color-mix(in oklab, var(--kind) 18%, transparent);
    color: var(--kind);
  }

  .titles {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 2px;
    min-height: 36px;
  }

  .title {
    font-size: var(--text-md);
    font-weight: 620;
    line-height: var(--leading-tight);
    letter-spacing: -0.005em;
    overflow: hidden;
    overflow-wrap: anywhere;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
  }

  .folded .title {
    -webkit-line-clamp: 1;
    line-clamp: 1;
  }

  .body {
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
    color: var(--fg-muted);
    white-space: pre-line;
    overflow-wrap: anywhere;
    overflow: hidden;
    display: -webkit-box;
    -webkit-line-clamp: 4;
    line-clamp: 4;
    -webkit-box-orient: vertical;
  }

  .count {
    flex: none;
    display: flex;
    align-items: baseline;
    gap: 2px;
    line-height: 1;
    font-variant-numeric: tabular-nums;
    color: color-mix(in oklab, var(--tone) 80%, var(--fg));
  }

  .num {
    font-size: var(--text-2xl);
    font-weight: 650;
    letter-spacing: -0.02em;
  }

  .unit {
    font-size: var(--text-md);
    font-weight: 550;
    color: var(--fg-muted);
  }

  /* ── what it holds ────────────────────────────────────────────────────── */

  /* What is happening now, on one line: the activity at the left, the clock
     at the right. */
  .now {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    min-width: 0;
  }

  /* The one part of a card that scrolls, so its box and buttons stay where
     they are. It reaches out to the card's edges, so its scroll bar sits at
     the rim rather than over the words. */
  .scroll {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    max-height: var(--blocks-max);
    margin: 0 -16px;
    padding: 0 16px;
    overflow-y: auto;
    overscroll-behavior: contain;
    scrollbar-width: thin;
  }

  .pinned {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }

  /* ── the buttons ──────────────────────────────────────────────────────── */

  .foot {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }

  .actions {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: 6px;
  }

  .act {
    height: 32px;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 0 13px;
    border: 0;
    border-radius: var(--radius-pill);
    background: color-mix(in oklab, var(--fg) 6%, transparent);
    color: var(--fg);
    font-size: var(--text-sm);
    font-weight: 600;
    white-space: nowrap;
    cursor: pointer;
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }

  .act:has(:global(svg)) {
    padding-left: 11px;
  }

  .act :global(svg) {
    color: var(--fg-muted);
  }

  .act:hover {
    background: color-mix(in oklab, var(--fg) 11%, transparent);
  }

  .act:active {
    background: color-mix(in oklab, var(--fg) 15%, transparent);
  }

  .act.danger {
    background: color-mix(in oklab, var(--danger) 10%, transparent);
    color: var(--danger);
  }

  .act.danger :global(svg) {
    color: currentColor;
  }

  .act.danger:hover {
    background: color-mix(in oklab, var(--danger) 16%, transparent);
  }

  .act.primary {
    background: var(--accent);
    color: var(--fg-on-accent);
    box-shadow: var(--shadow-sm);
  }

  .act.primary :global(svg) {
    color: currentColor;
  }

  .act.primary:hover {
    background: var(--accent-hover);
  }

  .act.primary.danger {
    background: var(--danger);
    color: var(--fg-on-danger);
  }

  .act:disabled {
    opacity: 0.5;
    pointer-events: none;
  }

  .more {
    flex: none;
    height: 28px;
    display: inline-flex;
    align-items: center;
    gap: 3px;
    padding: 0 8px 0 4px;
    margin-left: -4px;
    border: 0;
    border-radius: var(--radius-pill);
    background: transparent;
    color: var(--fg-muted);
    font-size: var(--text-xs);
    font-weight: 600;
    cursor: pointer;
  }

  .more:hover {
    color: var(--fg);
  }

  .turn {
    display: grid;
    transform: rotate(90deg);
    transition: transform var(--med) var(--ease);
  }

  .turn.open {
    transform: rotate(-90deg);
  }

  /* ── the time left ────────────────────────────────────────────────────── */

  .expiry {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    height: 3px;
    pointer-events: none;
  }

  .expiry i {
    display: block;
    height: 100%;
    background: color-mix(in oklab, var(--kind) 75%, transparent);
    transform-origin: left center;
    animation-name: pc-drain;
    animation-timing-function: linear;
    animation-fill-mode: both;
  }

  /* Held: the time stops where it is. The shell stops its own clock too
     (`panel_hold`); `holding` is the page not waiting to be told. From the
     page's own idea of what is held, not `:hover` -- which a macOS panel that
     is not the key window leaves set after a press, as it hears nothing of
     the pointer going -- nor `:focus-within`, which a button can have without
     the card being held. */
  .card.held .expiry i {
    animation-play-state: paused;
  }

  @media (prefers-reduced-motion: reduce) {
    .card[data-tone='listening'] .dot {
      animation: none;
    }

    .fold :global(svg),
    .turn {
      transition: none;
    }
  }
</style>
