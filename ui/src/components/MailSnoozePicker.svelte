<script lang="ts">
  // "Later today, tomorrow, next week, or pick a date" -- the small picker
  // `h` opens. The three fixed choices come from `mail.ts`'s `snoozeChoices`,
  // pure and tested; this component only draws them and reads a date.

  import { focusOnMount, trapFocus } from '../lib/focus'
  import { mail } from '../lib/mail.svelte'
  import { toLocalInputValue } from '../lib/format'

  let { onchoose, oncancel }: { onchoose: (at: Date) => void; oncancel: () => void } = $props()

  const choices = mail.snoozeOptions()

  let customDate = $state('')

  function pickCustom() {
    if (!customDate) return
    const at = new Date(customDate)
    at.setHours(8, 0, 0, 0)
    onchoose(at)
  }
</script>

<svelte:window onkeydown={(e: KeyboardEvent) => e.key === 'Escape' && oncancel()} />

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="scrim" onclick={oncancel}></div>
<div
  class="sheet small"
  role="dialog"
  aria-modal="true"
  aria-label="Snooze this thread"
  use:trapFocus
>
  <h2>Snooze until…</h2>
  <div class="choices">
    {#each choices as choice, i (choice.key)}
      <button class="choice" use:focusOnMount={i === 0} onclick={() => onchoose(choice.at)}>
        {choice.label}
        <span class="when">{toLocalInputValue(choice.at).replace('T', ' ')}</span>
      </button>
    {/each}
  </div>
  <div class="custom">
    <input type="date" bind:value={customDate} min={toLocalInputValue(new Date()).slice(0, 10)} />
    <button class="btn" disabled={!customDate} onclick={pickCustom}>Snooze</button>
  </div>
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
  .choices {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    margin-top: var(--sp-3);
  }
  .choice {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--radius-sm);
    text-align: left;
  }
  .choice:hover {
    background: var(--bg-hover);
  }
  .when {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .custom {
    display: flex;
    gap: var(--sp-2);
    margin-top: var(--sp-3);
    padding-top: var(--sp-3);
    border-top: 1px solid var(--border);
  }
  .custom input {
    flex: 1;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 6px var(--sp-2);
    background: var(--bg-raised);
    color: var(--fg);
  }
</style>
