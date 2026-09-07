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
  onkeydown={(e) => {
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
    <button class="btn" onclick={oncancel}>Cancel</button>
    <!-- Focused rather than the destructive one: Enter should not delete. -->
    <button
      class="btn"
      class:btn-danger={danger}
      class:btn-primary={!danger}
      use:focusOnMount
      onclick={onconfirm}>{confirmLabel}</button
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
