<script lang="ts" module>
  // The sheets that are open, oldest first. Escape closes the top one only:
  // each sheet listens on the window, so without this a confirmation raised
  // over a dialog went down with it on the one key press.
  const open: object[] = []
</script>

<script lang="ts">
  // The modal shell most dialogs share: a scrim that closes it when pressed,
  // and a sheet that traps focus, closes on Escape and, on a phone, is put
  // away by the back button. What goes inside is the dialog's own, so its
  // scoped styles keep applying to it; this draws only the two elements
  // around it and carries no styles of its own (the shared `.scrim` and
  // `.sheet` are in `app.css`, the phone's `.phone-fit` in `ConfirmDialog`).
  //
  // A dialog whose sheet has a class its own scoped styles select (`.wide`)
  // does not use this: that class would no longer carry the dialog's scope.

  import type { Snippet } from 'svelte'
  import { trapFocus } from '../lib/focus'
  import { closesOnBack } from '../lib/phone.svelte'

  let {
    label,
    onclose,
    role = 'dialog',
    sheetClass = '',
    fit = false,
    backWhen = () => true,
    children,
  }: {
    /** The sheet's accessible name. */
    label: string
    /** The scrim, Escape and (on a phone) back all run this. */
    onclose: () => unknown
    role?: 'dialog' | 'alertdialog'
    /** Classes after `sheet` on the sheet, such as `small`. */
    sheetClass?: string
    /** Add `phone-fit` to the scrim and the sheet. */
    fit?: boolean
    /** Whether a phone's back button closes it; shown for as long as it exists by default. */
    backWhen?: () => boolean
    children: Snippet
  } = $props()

  closesOnBack(
    () => backWhen(),
    () => void onclose(),
  )

  const me = {}
  $effect(() => {
    open.push(me)
    return () => {
      const at = open.indexOf(me)
      if (at >= 0) open.splice(at, 1)
    }
  })
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape' && open[open.length - 1] === me) void onclose()
  }}
/>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class={['scrim', fit && 'phone-fit']} onclick={() => void onclose()}></div>
<div
  class={['sheet', sheetClass, fit && 'phone-fit']}
  {role}
  aria-modal="true"
  aria-label={label}
  use:trapFocus
>
  {@render children()}
</div>
