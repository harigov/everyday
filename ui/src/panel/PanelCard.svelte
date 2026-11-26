<script lang="ts">
  // One card: what the shell said about it, drawn. It decides nothing -- a
  // press goes up to the stack, which knows what is switched off and tells the
  // shell. See `panel.ts` for what a card holds and `cards.ts` for the rules.
  import LevelMeter from './LevelMeter.svelte'
  import {
    drawnOrder,
    drawnSections,
    hasFolded,
    lastLines,
    silence,
    splitCount,
    toneColour,
    toneMark,
  } from './cards'
  import type { Panel, PanelAction, PanelCheck, PanelItem } from './panel'

  let {
    card,
    levels,
    open,
    locked,
    lists,
    checkOf,
    onpress,
    oncheck,
    ontoggle,
  }: {
    card: Panel
    /** The meter's bars, once the first reading has come. */
    levels: readonly number[] | undefined
    /** The folded lists have been opened. */
    open: boolean
    /** A button was pressed and the card has not changed since: they are off. */
    locked: boolean
    /** How tall the lists may grow before they scroll, in pixels. */
    lists: number
    /** How a box is drawn: ticked or not, and whether it is waiting on the shell. */
    checkOf: (check: PanelCheck) => { done: boolean; locked: boolean }
    onpress: (action: PanelAction) => void
    oncheck: (item: PanelItem) => void
    ontoggle: () => void
  } = $props()

  /** What the meter shows before the first reading. */
  const QUIET = silence()

  const lines = $derived(lastLines(card.lines))
  const sections = $derived(drawnSections(card.sections, open))
  const foldable = $derived(hasFolded(card.sections))
  const count = $derived(card.count ? splitCount(card.count) : null)
  const mark = $derived(toneMark(card.tone))
  const actions = $derived(drawnOrder(card.actions))
</script>

<section
  class="card"
  class:metered={card.meter}
  data-tone={card.tone}
  style:--tone={toneColour(card.tone)}
  style:--lists-max="{lists}px"
  aria-label={card.kicker}
>
  <header class="head">
    <div class="heading">
      <p class="kicker">
        {#if mark === 'dot'}
          <span class="dot" aria-hidden="true"></span>
        {:else if mark === 'check'}
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 12.5l4.5 4.5L19 7.5" /></svg>
        {:else if mark === 'alert'}
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 6.5v7M12 17.4v.1" /></svg>
        {/if}
        <span>{card.kicker}</span>
      </p>
      <h2 class="title">{card.title}</h2>
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
    {#if card.busy}
      <span class="spinner" role="status" aria-label="Working"></span>
    {/if}
  </header>

  {#if card.body}
    <p class="body">{card.body}</p>
  {/if}

  {#if lines.length > 0}
    <ul class="lines" aria-live="polite">
      {#each lines as line, i (i)}
        <li>
          {#if line.who}<strong class="who">{line.who}</strong>{/if}
          <span class="said">{line.text}</span>
          {#if line.pending}<span class="caret" aria-hidden="true"></span>{/if}
        </li>
      {/each}
    </ul>
  {/if}

  {#if sections.length > 0}
    <div class="lists">
      {#each sections as section, s (s)}
        <section class="list">
          <h3>{section.heading}</h3>
          <ul class="items">
            {#each section.items as item, i (i)}
              {#if item.check}
                {@const box = checkOf(item.check)}
                <li>
                  <button
                    type="button"
                    class="row"
                    class:done={box.done}
                    role="checkbox"
                    aria-checked={box.done}
                    disabled={box.locked}
                    onclick={() => oncheck(item)}
                  >
                    <span class="lead">
                      <span class="box" aria-hidden="true">
                        <svg viewBox="0 0 24 24"><path d="M5 12.5l4.5 4.5L19 7.5" /></svg>
                      </span>
                    </span>
                    <span class="words">
                      <span class="text">{item.text}</span>
                      {#if item.detail}<span class="detail">{item.detail}</span>{/if}
                    </span>
                  </button>
                </li>
              {:else}
                <li class="row">
                  <span class="lead"><span class="bullet" aria-hidden="true"></span></span>
                  <span class="words">
                    <span class="text">{item.text}</span>
                    {#if item.detail}<span class="detail">{item.detail}</span>{/if}
                  </span>
                </li>
              {/if}
            {/each}
          </ul>
        </section>
      {/each}
    </div>
  {/if}

  {#if foldable || actions.length > 0}
    <div class="foot">
      {#if foldable}
        <button type="button" class="toggle" aria-expanded={open} onclick={ontoggle}>
          {open ? 'Less' : 'More'}
          <svg class:flip={open} viewBox="0 0 24 24" aria-hidden="true">
            <path d="M6 9l6 6 6-6" />
          </svg>
        </button>
      {/if}
      {#each actions as action (action.id)}
        <button
          type="button"
          class="btn"
          class:primary={action.primary}
          disabled={locked}
          onclick={() => onpress(action)}
        >
          {action.label}
        </button>
      {/each}
    </div>
  {/if}
</section>

<style>
  .card {
    box-sizing: border-box;
    padding: 14px 16px 12px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    background: var(--float);
    color: var(--fg);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-xl);
    box-shadow: var(--edge), var(--shadow);
    font-family: var(--font-ui);
    -webkit-user-select: none;
    user-select: none;
    cursor: default;
  }

  /* A card that wants something of the person, or has failed, shows it at its
     rim as well as in its kicker. */
  .card[data-tone='attention'],
  .card[data-tone='trouble'] {
    border-color: color-mix(in oklab, var(--tone) 45%, var(--border-strong));
  }

  /* ── the top: kicker and title, then whatever counts or moves ─────────── */

  .head {
    display: flex;
    align-items: flex-start;
    gap: 12px;
  }

  .heading {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }

  .kicker {
    margin: 0;
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--tone);
  }

  .dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: currentColor;
  }

  /* Listening with no meter beside it: the dot is the only sign the
     microphone is open, so it breathes. Opacity alone, on one small layer. */
  .card[data-tone='listening']:not(.metered) .dot {
    animation: pulse 2s ease-in-out infinite;
  }

  .kicker svg {
    flex: none;
    width: 14px;
    height: 14px;
    fill: none;
    stroke: currentColor;
    stroke-width: 2.5;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .title {
    margin: 0;
    font-size: var(--text-md);
    font-weight: 600;
    line-height: var(--leading-tight);
    overflow: hidden;
    overflow-wrap: anywhere;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
  }

  .count {
    flex: none;
    align-self: center;
    margin: 0;
    display: flex;
    align-items: baseline;
    gap: 2px;
    line-height: 1;
    font-variant-numeric: tabular-nums;
  }

  .num {
    font-size: var(--text-2xl);
    font-weight: 650;
  }

  .unit {
    font-size: var(--text-md);
    color: var(--fg-muted);
  }

  .spinner {
    flex: none;
    align-self: center;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    border: 2px solid var(--border-strong);
    border-top-color: var(--tone);
    animation: spin 0.9s linear infinite;
  }

  /* ── what it says ─────────────────────────────────────────────────────── */

  .body {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
    color: var(--fg-muted);
    white-space: pre-line;
    overflow-wrap: anywhere;
    overflow: hidden;
    display: -webkit-box;
    -webkit-line-clamp: 6;
    line-clamp: 6;
    -webkit-box-orient: vertical;
  }

  /* The newest line is the last, and when there is more than there is room
     for it is the top that is cut, not the end of what is being said. */
  .lines {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    justify-content: flex-end;
    gap: 3px;
    max-height: calc(var(--text-sm) * 10);
    overflow: hidden;
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
    overflow-wrap: anywhere;
  }

  .lines li {
    color: var(--fg-muted);
  }

  .lines li:last-child {
    color: var(--fg);
  }

  .who {
    font-weight: 650;
    color: var(--fg);
  }

  .caret {
    display: inline-block;
    width: 2px;
    height: 1.05em;
    margin-left: 1px;
    vertical-align: text-bottom;
    background: var(--tone);
    animation: blink 1.1s steps(1) infinite;
  }

  /* ── lists ────────────────────────────────────────────────────────────── */

  /* The one part of a card that scrolls, so its buttons stay where they are. */
  .lists {
    display: flex;
    flex-direction: column;
    gap: 10px;
    max-height: var(--lists-max);
    margin-right: -8px;
    padding-right: 8px;
    overflow-y: auto;
    overscroll-behavior: contain;
    scrollbar-width: thin;
  }

  .list h3 {
    margin: 0 0 4px;
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--fg-muted);
  }

  .items {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 5px;
  }

  .row {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    width: 100%;
    margin: 0;
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
    text-align: left;
  }

  button.row {
    cursor: pointer;
  }

  button.row:disabled {
    cursor: default;
  }

  /* One column for the box or the bullet, the height of a line of text, so
     the words line up whichever an item has. */
  .lead {
    flex: none;
    width: 16px;
    height: calc(var(--text-sm) * 1.45);
    display: grid;
    place-items: center;
  }

  .bullet {
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: var(--fg-subtle);
  }

  .box {
    width: 16px;
    height: 16px;
    box-sizing: border-box;
    display: grid;
    place-items: center;
    border: 1.5px solid var(--fg-subtle);
    border-radius: 5px;
    color: var(--fg-on-accent);
  }

  .box svg {
    width: 12px;
    height: 12px;
    fill: none;
    stroke: currentColor;
    stroke-width: 3;
    stroke-linecap: round;
    stroke-linejoin: round;
    opacity: 0;
  }

  button.row:hover .box {
    border-color: var(--accent);
  }

  .done .box {
    background: var(--accent);
    border-color: var(--accent);
  }

  .done .box svg {
    opacity: 1;
  }

  .words {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    overflow-wrap: anywhere;
  }

  .done .text {
    color: var(--fg-muted);
    text-decoration: line-through;
  }

  .detail {
    font-size: var(--text-xs);
    color: var(--fg-muted);
  }

  /* ── buttons ──────────────────────────────────────────────────────────── */

  .foot {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: flex-end;
    gap: 4px 6px;
    margin-top: 2px;
  }

  .btn {
    height: 32px;
    padding: 0 14px;
    border: 0;
    border-radius: var(--radius-pill);
    background: transparent;
    color: var(--fg-muted);
    font: inherit;
    font-size: var(--text-sm);
    font-weight: 550;
    white-space: nowrap;
    cursor: pointer;
  }

  .btn:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .btn.primary {
    background: var(--accent);
    color: var(--fg-on-accent);
  }

  .btn.primary:hover {
    background: var(--accent-hover);
    color: var(--fg-on-accent);
  }

  .btn:disabled {
    opacity: 0.5;
    pointer-events: none;
  }

  .toggle {
    margin-right: auto;
    height: 28px;
    padding: 0 8px 0 4px;
    display: inline-flex;
    align-items: center;
    gap: 2px;
    border: 0;
    border-radius: var(--radius-pill);
    background: transparent;
    color: var(--fg-muted);
    font: inherit;
    font-size: var(--text-xs);
    font-weight: 550;
    cursor: pointer;
  }

  .toggle:hover {
    color: var(--fg);
  }

  .toggle svg {
    width: 14px;
    height: 14px;
    fill: none;
    stroke: currentColor;
    stroke-width: 2.5;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .toggle svg.flip {
    transform: rotate(180deg);
  }

  button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  /* ── motion: opacity and transform only ───────────────────────────────── */

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  @keyframes pulse {
    0%,
    100% {
      opacity: 0.4;
    }
    50% {
      opacity: 1;
    }
  }

  @keyframes blink {
    50% {
      opacity: 0;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .spinner {
      animation: none;
      border-top-color: var(--border-strong);
      border-right-color: var(--tone);
    }

    .card[data-tone='listening']:not(.metered) .dot,
    .caret {
      animation: none;
    }
  }
</style>
