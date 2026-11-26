<script lang="ts">
  // A titled list -- an agenda, the points to raise, your notes so far -- each
  // item a line with a quieter second line under it, and a box to tick where
  // the shell gave it one. A tick shows at once and waits, switched off, until
  // the shell has answered (`checkView` in `cards.ts`).
  import type { PanelBlock, PanelCheck } from '../panel'

  let {
    block,
    checkOf,
    oncheck,
  }: {
    block: Extract<PanelBlock, { type: 'list' }>
    /** How a box is drawn: ticked or not, and whether it is waiting on the shell. */
    checkOf: (check: PanelCheck) => { done: boolean; locked: boolean }
    oncheck: (check: PanelCheck) => void
  } = $props()
</script>

<section class="list">
  <h3 class="pc-label">{block.heading}</h3>
  <ul>
    {#each block.items as item, i (i)}
      {#if item.check}
        {@const check = item.check}
        {@const box = checkOf(check)}
        <li>
          <button
            type="button"
            class="row"
            class:done={box.done}
            role="checkbox"
            aria-checked={box.done}
            disabled={box.locked}
            onclick={() => oncheck(check)}
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

<style>
  ul {
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .row {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    width: 100%;
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--fg);
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
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: color-mix(in oklab, var(--kind) 55%, var(--fg-subtle));
  }

  .box {
    width: 16px;
    height: 16px;
    display: grid;
    place-items: center;
    border: 1.5px solid var(--fg-subtle);
    border-radius: 5px;
    color: var(--fg-on-accent);
    transition:
      background var(--fast) var(--ease),
      border-color var(--fast) var(--ease);
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
</style>
