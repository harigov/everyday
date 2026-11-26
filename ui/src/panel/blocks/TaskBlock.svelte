<script lang="ts">
  // A task, as the tasks app draws one: a round box, the title, a quieter line
  // under it and when it is due -- in a tile of the tasks app's colour. With a
  // box to tick (`check`), ticking it shows at once and waits, switched off,
  // until the shell has answered (`checkView` in `cards.ts`).
  import type { PanelBlock } from '../panel'

  let {
    block,
    view,
    oncheck,
  }: {
    block: Extract<PanelBlock, { type: 'task' }>
    /** How its box is drawn, where it has one: ticked or not, and waiting on the shell or not. */
    view: { done: boolean; locked: boolean } | null
    oncheck: () => void
  } = $props()

  const done = $derived(view ? view.done : block.done)
</script>

<div class="pc-tile task" class:done style:--tile="var(--app-todo)">
  {#if block.check !== undefined && view}
    <button
      type="button"
      class="box"
      role="checkbox"
      aria-checked={done}
      aria-label={block.title}
      disabled={view.locked}
      onclick={oncheck}
    >
      <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6.5 12.5l3.5 3.5 7.5-8" /></svg>
    </button>
  {:else}
    <span class="box" aria-hidden="true">
      <svg viewBox="0 0 24 24"><path d="M6.5 12.5l3.5 3.5 7.5-8" /></svg>
    </span>
  {/if}
  <span class="words">
    <span class="title">{block.title}</span>
    {#if block.detail}<span class="detail">{block.detail}</span>{/if}
  </span>
  {#if block.due}<span class="due">{block.due}</span>{/if}
</div>

<style>
  .task {
    display: flex;
    align-items: flex-start;
    gap: 10px;
  }

  .box {
    flex: none;
    width: 18px;
    height: 18px;
    margin-top: 1px;
    padding: 0;
    display: grid;
    place-items: center;
    border: 1.5px solid color-mix(in oklab, var(--tile) 70%, var(--fg-subtle));
    border-radius: 50%;
    background: transparent;
    color: var(--float);
    transition:
      background var(--fast) var(--ease),
      border-color var(--fast) var(--ease);
  }

  button.box {
    cursor: pointer;
  }

  button.box:hover {
    background: color-mix(in oklab, var(--tile) 18%, transparent);
  }

  button.box:disabled {
    cursor: default;
  }

  .box svg {
    width: 13px;
    height: 13px;
    fill: none;
    stroke: currentColor;
    stroke-width: 3;
    stroke-linecap: round;
    stroke-linejoin: round;
    opacity: 0;
  }

  .done .box {
    border-color: var(--tile);
    background: var(--tile);
  }

  .done .box svg {
    opacity: 1;
  }

  .words {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
    overflow-wrap: anywhere;
  }

  .title {
    font-size: var(--text-sm);
    font-weight: 600;
    line-height: var(--leading-snug);
  }

  .done .title {
    color: var(--fg-muted);
    text-decoration: line-through;
    text-decoration-color: color-mix(in oklab, var(--fg-muted) 60%, transparent);
  }

  .detail {
    font-size: var(--text-xs);
    color: var(--fg-muted);
  }

  .due {
    flex: none;
    padding: 0 8px;
    border-radius: var(--radius-pill);
    background: color-mix(in oklab, var(--tile) 15%, transparent);
    color: color-mix(in oklab, var(--tile) 68%, var(--fg));
    font-size: var(--text-xs);
    font-weight: 650;
    line-height: 20px;
  }
</style>
