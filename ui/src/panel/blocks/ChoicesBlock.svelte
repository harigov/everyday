<script lang="ts">
  // Answers to pick from, one press each: a question's choices, as chips. A
  // press is the card's like any of its buttons -- they all go off together
  // until the shell answers (`isLocked` in `cards.ts`).
  import Glyph from '../Glyph.svelte'
  import { actionGlyph } from '../cards'
  import type { PanelAction, PanelBlock } from '../panel'

  let {
    block,
    locked,
    onpress,
  }: {
    block: Extract<PanelBlock, { type: 'choices' }>
    locked: boolean
    onpress: (action: PanelAction) => void
  } = $props()
</script>

<div class="choices">
  {#if block.prompt}<p class="prompt">{block.prompt}</p>{/if}
  <div class="row">
    {#each block.choices as choice (choice.id)}
      {@const glyph = actionGlyph(choice.icon)}
      <button
        type="button"
        class="pc-chip"
        class:primary={choice.primary}
        class:danger={choice.danger && !choice.primary}
        disabled={locked}
        onclick={() => onpress(choice)}
      >
        {#if glyph}<Glyph name={glyph} size={14} />{/if}
        <span>{choice.label}</span>
      </button>
    {/each}
  </div>
</div>

<style>
  .choices {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }

  .prompt {
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }

  .row {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
</style>
