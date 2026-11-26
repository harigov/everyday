<script lang="ts">
  // Who said what, never run together. The person's words sit on the right in
  // a bubble tinted with the accent -- the same bubble as their turns in the
  // main window's conversation -- and the assistant's on the left, on the card
  // itself, under its name. What voice typing heard, or what was typed where
  // the person was typing, is neither: a quiet running line with a mark of
  // which it is. The newest is at the bottom, and a screen reader hears each
  // as it comes.
  import Symbol from '../../components/Symbol.svelte'
  import Glyph from '../Glyph.svelte'
  import { lastTurns, showsName } from '../cards'
  import type { PanelBlock } from '../panel'

  let { block }: { block: Extract<PanelBlock, { type: 'conversation' }> } = $props()

  const turns = $derived(lastTurns(block.turns))
</script>

<ol class="turns" aria-live="polite">
  {#each turns as turn, i (i)}
    {@const named = showsName(turns, i)}
    {#if turn.who === 'you'}
      <li class="you" class:named>
        {#if named}<span class="who">You</span>{/if}
        <p class="bubble">
          {turn.text}{#if turn.pending}<span class="pc-caret" aria-hidden="true"></span>{/if}
        </p>
      </li>
    {:else if turn.who === 'assistant'}
      <li class="assistant" class:named>
        {#if named}
          <span class="who pc-ink" style:--ink="var(--app-assistant)">
            <Symbol name="sparkle" size={12} />
            {turn.name ?? 'Assistant'}
          </span>
        {/if}
        <p class="said">
          {#if turn.text.trim() === ''}
            <span class="pc-dots" role="img" aria-label="Writing"><i></i><i></i><i></i></span>
          {:else}
            {turn.text}{#if turn.pending}<span class="pc-caret" aria-hidden="true"></span>{/if}
          {/if}
        </p>
      </li>
    {:else}
      <li class="line">
        <span class="cue" title={turn.who === 'heard' ? 'Heard' : 'Typed'}>
          <Glyph name={turn.who === 'heard' ? 'mic' : 'pencil'} size={13} weight={1.75} />
        </span>
        <p>
          {turn.text}{#if turn.pending}<span class="pc-caret" aria-hidden="true"></span>{/if}
        </p>
      </li>
    {/if}
  {/each}
</ol>

<style>
  .turns {
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: var(--text-base);
    line-height: var(--leading-snug);
    overflow-wrap: anywhere;
  }

  /* A new speaker is a new paragraph; the same one carrying on is not. */
  .named:not(:first-child) {
    margin-top: 6px;
  }

  .who {
    display: flex;
    align-items: center;
    gap: 5px;
    margin-bottom: 2px;
    font-size: var(--text-xs);
    font-weight: 650;
    color: var(--fg-subtle);
  }

  .you {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
  }

  .bubble {
    max-width: 86%;
    padding: 6px 11px 7px;
    border-radius: 14px 14px 4px 14px;
    background: var(--bg-selected);
    color: var(--fg);
    white-space: pre-wrap;
  }

  .assistant {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    padding-right: 8%;
  }

  .said {
    color: var(--fg);
    white-space: pre-wrap;
  }

  .said .pc-dots {
    color: var(--fg-subtle);
  }

  /* Heard, or typed elsewhere: the earlier lines quieter, the newest in ink. */
  .line {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    color: var(--fg-muted);
  }

  .line:last-child {
    color: var(--fg);
  }

  .cue {
    flex: none;
    display: grid;
    place-items: center;
    width: 18px;
    height: calc(var(--text-base) * 1.45);
    color: var(--fg-subtle);
  }

  .line p {
    min-width: 0;
    white-space: pre-wrap;
  }
</style>
