<script lang="ts">
  // A question with more than one way to say yes: one block of a series or
  // all of them, this occurrence or the whole series. `ConfirmDialog` has a
  // single yes, and these have two or three, so this is its shape with a
  // row of answers instead -- the same scrim, sheet and focus rules.
  //
  // Whoever shows it takes it down: an answer runs its own `run`, and the
  // scrim, Escape and Cancel all run `oncancel`.

  import { focusOnMount } from '../lib/focus'
  import Sheet from './Sheet.svelte'

  let {
    title,
    detail = '',
    options,
    oncancel,
  }: {
    title: string
    detail?: string
    options: { label: string; danger?: boolean; run: () => unknown }[]
    oncancel: () => unknown
  } = $props()

  // Shown for as long as it exists, so a phone's back button is a "no"
  // (`Sheet` does that, and Escape).
</script>

<Sheet label={title} role="alertdialog" fit onclose={oncancel}>
  <h2 class="sheet-title">{title}</h2>
  {#if detail}<p class="hint">{detail}</p>{/if}
  <!-- The answers stacked, widest reach last, so the three read as a
       scale -- this one, these, all of them -- rather than as a row of
       buttons whose order has to be puzzled out. -->
  <div class="choices">
    {#each options as option (option.label)}
      <button
        class="btn"
        class:btn-outline={!option.danger}
        class:btn-danger={option.danger}
        onclick={() => void option.run()}>{option.label}</button
      >
    {/each}
  </div>
  <div class="sheet-row">
    <span class="spacer"></span>
    <!-- Focus on Cancel, as in `ConfirmDialog`: this appears because
         something with consequences was asked for, and Enter on a sheet not
         yet read should not be what answers it. -->
    <button class="btn" use:focusOnMount onclick={() => oncancel()}>Cancel</button>
  </div>
</Sheet>

<style>
  .sheet-title {
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
  }
  .sheet-title + .hint {
    margin-top: var(--sp-2);
  }
  .choices {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    margin-top: var(--sp-4);
  }
  .choices .btn {
    justify-content: center;
  }
  /* Each answer a finger tall, and a long one wraps. */
  :global([data-phone]) .choices .btn,
  :global([data-phone]) .sheet-row .btn {
    min-height: 44px;
    height: auto;
    padding-block: var(--sp-2);
    white-space: normal;
    text-align: center;
  }
</style>
