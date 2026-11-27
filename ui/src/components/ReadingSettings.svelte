<script lang="ts">
  // How an article is set: the face, the size, the spacing, the width of the
  // column and the colour of the page, as a read-it-later app offers them. A
  // popover under the toolbar's "Aa" on a desktop, a sheet from the top on a
  // phone. Every change is kept at once (`reading.svelte.ts`) and the open
  // story is drawn again in it -- there is nothing to save, and the article
  // behind the popover is the preview. Modal, as the album picker is, so the
  // Feed's own keys -- Escape for "back to the list" among them -- wait until
  // it is closed.

  import { dismissable } from '../lib/dismiss'
  import { trapFocus } from '../lib/focus'
  import { closesOnBack, phone } from '../lib/phone.svelte'
  import {
    FONT_LABELS,
    PAGE_LABELS,
    READING_FONTS,
    READING_PAGES,
    READING_SIZES,
    READING_SPACINGS,
    READING_WIDTHS,
    SPACING_LABELS,
    WIDTH_LABELS,
    isDefaultReading,
    pageSwatch,
    stepSize,
    FONT_STACKS,
  } from '../lib/reading'
  import { reading } from '../lib/reading.svelte'

  let { onclose }: { onclose: () => void } = $props()

  const s = $derived(reading.settings)
  const smallest = $derived(s.size <= READING_SIZES[0])
  const largest = $derived(s.size >= READING_SIZES[READING_SIZES.length - 1]!)

  closesOnBack(
    () => true,
    () => onclose(),
  )

  /**
   * A press in the article lands in its frame, whose events never reach this
   * window -- `dismissable` cannot hear it. All the window hears is that it
   * lost focus, and to the frame.
   */
  function blurred() {
    setTimeout(() => {
      if (document.activeElement instanceof HTMLIFrameElement) onclose()
    })
  }
</script>

<svelte:window onkeydown={(e: KeyboardEvent) => e.key === 'Escape' && onclose()} onblur={blurred} />

{#if phone.on}
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div class="scrim" onclick={onclose}></div>
{/if}

<div
  class="pop frost"
  role="dialog"
  aria-label="Reading settings"
  aria-modal="true"
  tabindex="-1"
  use:trapFocus
  use:dismissable={{ onaway: onclose, within: '[data-reading-trigger]' }}
>
  <div class="row">
    <span class="label" id="reading-font">Font</span>
    <div class="seg-group" role="radiogroup" aria-labelledby="reading-font">
      {#each READING_FONTS as font (font)}
        <button
          class="seg-opt"
          class:on={s.font === font}
          role="radio"
          aria-checked={s.font === font}
          style:font-family={FONT_STACKS[font]}
          onclick={() => reading.change({ font })}
        >
          {FONT_LABELS[font]}
        </button>
      {/each}
    </div>
  </div>

  <div class="row">
    <span class="label" id="reading-size">Size</span>
    <div class="size" role="group" aria-labelledby="reading-size">
      <button
        class="step small"
        aria-label="Smaller text"
        disabled={smallest}
        onclick={() => reading.change({ size: stepSize(s.size, -1) })}>A</button
      >
      <span class="px" aria-live="polite">{s.size}px</span>
      <button
        class="step large"
        aria-label="Larger text"
        disabled={largest}
        onclick={() => reading.change({ size: stepSize(s.size, 1) })}>A</button
      >
    </div>
  </div>

  <div class="row">
    <span class="label" id="reading-spacing">Spacing</span>
    <div class="seg-group" role="radiogroup" aria-labelledby="reading-spacing">
      {#each READING_SPACINGS as spacing (spacing)}
        <button
          class="seg-opt"
          class:on={s.spacing === spacing}
          role="radio"
          aria-checked={s.spacing === spacing}
          onclick={() => reading.change({ spacing })}
        >
          {SPACING_LABELS[spacing]}
        </button>
      {/each}
    </div>
  </div>

  {#if !phone.on}
    <!-- A phone's column is the screen. -->
    <div class="row">
      <span class="label" id="reading-width">Width</span>
      <div class="seg-group" role="radiogroup" aria-labelledby="reading-width">
        {#each READING_WIDTHS as width (width)}
          <button
            class="seg-opt"
            class:on={s.width === width}
            role="radio"
            aria-checked={s.width === width}
            onclick={() => reading.change({ width })}
          >
            {WIDTH_LABELS[width]}
          </button>
        {/each}
      </div>
    </div>
  {/if}

  <div class="row">
    <span class="label" id="reading-page">Page</span>
    <div class="pages" role="radiogroup" aria-labelledby="reading-page">
      {#each READING_PAGES as page (page)}
        {@const swatch = pageSwatch(page)}
        <button
          class="page"
          class:on={s.page === page}
          role="radio"
          aria-checked={s.page === page}
          onclick={() => reading.change({ page })}
        >
          <span
            class="paper"
            style:background={swatch?.paper ?? 'var(--bg)'}
            style:color={swatch?.ink ?? 'var(--fg)'}>Aa</span
          >
          <span class="name">{PAGE_LABELS[page]}</span>
        </button>
      {/each}
    </div>
  </div>

  <div class="foot">
    <button class="link" disabled={isDefaultReading(s)} onclick={() => reading.reset()}>
      Reset to defaults
    </button>
  </div>
</div>

<style>
  .pop {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: 50;
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    width: 300px;
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-lg);
  }
  .row {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .label {
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--fg-subtle);
  }

  .size {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: 3px;
    background: var(--bg-sunken);
    border-radius: var(--radius);
  }
  .step {
    flex: 1;
    min-height: 30px;
    border-radius: var(--radius-sm);
    color: var(--fg-muted);
    font-family: var(--font-read);
    font-weight: 600;
    line-height: 1;
    transition: background var(--fast) var(--ease);
  }
  .step:hover:not(:disabled) {
    background: var(--bg-raised);
    color: var(--fg);
  }
  .step:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .step.small {
    font-size: 13px;
  }
  .step.large {
    font-size: 20px;
  }
  .px {
    min-width: 44px;
    text-align: center;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }

  .pages {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: var(--sp-2);
  }
  .page {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 4px;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .paper {
    display: grid;
    place-items: center;
    width: 100%;
    height: 38px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    font-family: var(--font-read);
    font-size: 15px;
    font-weight: 600;
  }
  .page.on .paper {
    box-shadow: 0 0 0 2px var(--accent);
  }
  .page.on .name {
    color: var(--fg);
    font-weight: 600;
  }

  .foot {
    display: flex;
    justify-content: flex-end;
  }
  .link {
    font-size: var(--text-xs);
    color: var(--accent);
  }
  .link:disabled {
    color: var(--fg-faint);
    cursor: default;
  }

  /* ── A phone ──────────────────────────────────────────────────────────
     Hung from the top of the screen like every sheet, over the shared scrim,
     as the album picker is. */
  :global([data-phone]) .pop {
    position: fixed;
    z-index: 66;
    top: calc(var(--safe-top) + max(8dvh, var(--sp-4)));
    left: calc(var(--safe-left) + var(--sp-3));
    right: calc(var(--safe-right) + var(--sp-3));
    width: auto;
  }
  :global([data-phone]) .step {
    min-height: 40px;
  }
</style>
