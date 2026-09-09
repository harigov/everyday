<script lang="ts">
  import { focusOnMount } from '../lib/focus'

  let {
    title,
    detail = '',
    confirmLabel = 'Delete',
    danger = true,
    onconfirm,
    oncancel,
  }: {
    title: string
    detail?: string
    confirmLabel?: string
    /** Style the confirming button as destructive. */
    danger?: boolean
    onconfirm: () => void
    oncancel: () => void
  } = $props()
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape') oncancel()
  }}
/>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="scrim" onclick={oncancel}></div>
<div class="sheet" role="alertdialog" aria-modal="true" aria-label={title}>
  <h2>{title}</h2>
  {#if detail}<p class="hint">{detail}</p>{/if}
  <div class="sheet-row">
    <span class="spacer"></span>
    <!-- Focus lands here, not on the destructive button beside it: this
         dialog appears *because* something irreversible was asked for, and
         Enter on a dialog you have not finished reading should not be the
         thing that deletes an entry. Tab reaches the other one in one step. -->
    <button class="btn" use:focusOnMount onclick={oncancel}>Cancel</button>
    <button class="btn" class:btn-danger={danger} class:btn-primary={!danger} onclick={onconfirm}
      >{confirmLabel}</button
    >
  </div>
</div>

<style>
  h2 {
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
  }
  h2 + :global(.hint) {
    margin-top: var(--sp-2);
  }
</style>
