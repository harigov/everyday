<script lang="ts">
  import { focusOnMount, trapFocus } from '../lib/focus'
  import { closesOnBack } from '../lib/phone.svelte'

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

  // Shown for as long as it exists, so a phone's back button is a "no".
  closesOnBack(
    () => true,
    () => oncancel(),
  )
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape') oncancel()
  }}
/>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="scrim phone-fit" onclick={oncancel}></div>
<div class="sheet phone-fit" role="alertdialog" aria-modal="true" aria-label={title} use:trapFocus>
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
  /* ── Sheets on a phone ───────────────────────────────────────────────
     `class="… phone-fit"` on a scrim and its sheet, here once for every one
     that says so (this file's styles are in the one stylesheet whichever
     dialog is open). The bar of apps along the bottom is above the shared
     `.scrim` and `.sheet` in the stack, so a dialog was drawn under it, its
     buttons with it, and the screen not dimmed where the bar is: a dialog is
     put over the bar, and a form is kept to the room above it -- it hangs
     from the top, under the status bar, rather than being centred in the
     whole window, which sent its foot under the bar -- or, with the keyboard
     up and the bar away, to the room the page has left. This is what
     app.css's own phone rule for `.sheet` should say, for every dialog. */
  :global([data-phone] .scrim.phone-fit) {
    z-index: 64;
  }
  :global([data-phone] .sheet.phone-fit) {
    z-index: 65;
    max-height: calc(100dvh - 12% - var(--bottombar-h) - var(--safe-bottom) - var(--sp-3));
  }
  :global([data-phone] .sheet.wide.phone-fit) {
    top: calc(var(--safe-top) + var(--sp-3));
    translate: -50% 0;
    max-height: calc(
      100dvh - var(--safe-top) - var(--sp-3) - var(--bottombar-h) - var(--safe-bottom) - var(--sp-3)
    );
  }
  :global(:root[data-phone][data-keyboard] .sheet.phone-fit) {
    max-height: calc(100dvh - 12% - var(--sp-3));
  }
  :global(:root[data-phone][data-keyboard] .sheet.wide.phone-fit) {
    max-height: calc(100dvh - var(--safe-top) - var(--sp-3) - var(--sp-3));
  }

  /* Two answers side by side, each a finger wide; a long label wraps rather
     than pushing the other off the sheet. */
  :global([data-phone]) .sheet-row {
    flex-wrap: wrap;
    justify-content: flex-end;
  }
  :global([data-phone]) .sheet-row .btn {
    min-height: 44px;
    white-space: normal;
  }
</style>
