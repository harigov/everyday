<script lang="ts">
  // A question with more than one way to say yes: one block of a series or
  // all of them, this occurrence or the whole series. `ConfirmDialog` has a
  // single yes, and these have two or three, so this is its shape with a
  // row of answers instead -- the same scrim, sheet and focus rules.
  //
  // Whoever shows it takes it down: an answer runs its own `run`, and the
  // scrim, Escape and Cancel all run `oncancel`.

  import { focusOnMount, trapFocus } from '../lib/focus'

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
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape') oncancel()
  }}
/>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="scrim" onclick={() => oncancel()}></div>
<div class="sheet" role="alertdialog" aria-modal="true" aria-label={title} use:trapFocus>
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
</div>

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
</style>
