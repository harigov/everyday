<script lang="ts">
  // "Label this thread as…" -- the proper picker `l` opens, in place of the
  // `window.prompt` this app used to fall back to. Shaped exactly like
  // `MailSnoozePicker.svelte`, which is the "small sheet with one field and
  // a primary button" this app already has a pattern for; there is no
  // `list_labels` command yet for a real "choose an existing one" picker to
  // read from (see `shortcuts.svelte.ts`'s own note on the same gap), so
  // this is a text field rather than a list -- worth revisiting once real
  // accounts have real label lists to offer.

  import { focusOnMount, trapFocus } from '../lib/focus'

  let { onchoose, oncancel }: { onchoose: (label: string) => void; oncancel: () => void } = $props()

  let text = $state('')

  function submit() {
    const label = text.trim()
    if (label) onchoose(label)
  }
</script>

<svelte:window onkeydown={(e: KeyboardEvent) => e.key === 'Escape' && oncancel()} />

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="scrim" onclick={oncancel}></div>
<div
  class="sheet small"
  role="dialog"
  aria-modal="true"
  aria-label="Label this thread"
  use:trapFocus
>
  <h2>Label this thread as…</h2>
  <form
    class="row"
    onsubmit={(e) => {
      e.preventDefault()
      submit()
    }}
  >
    <input
      use:focusOnMount
      bind:value={text}
      placeholder="Label"
      aria-label="Label"
      autocomplete="off"
      spellcheck="false"
    />
    <button class="btn btn-primary" disabled={!text.trim()} type="submit">Apply</button>
  </form>
  <div class="sheet-row">
    <span class="spacer"></span>
    <button class="btn" onclick={oncancel}>Cancel</button>
  </div>
</div>

<style>
  h2 {
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
  }
  .row {
    display: flex;
    gap: var(--sp-2);
    margin-top: var(--sp-3);
  }
  .row input {
    flex: 1;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 6px var(--sp-2);
    background: var(--bg-raised);
    color: var(--fg);
  }
</style>
