<script lang="ts">
  // "Later today, this evening, tomorrow, this weekend, next week, or pick
  // a date and time" -- the small picker `h` opens. The fixed choices come
  // from `mail.ts`'s `snoozeChoices`, pure and tested; this component only
  // draws them (and lets 1-5 pick one without the mouse) and reads a date
  // and time for the custom choice.

  import { focusOnMount } from '../lib/focus'
  import { isTyping } from '../lib/keys'
  import Sheet from './Sheet.svelte'
  import { mail } from '../lib/mail.svelte'
  import { customSnoozeInstant, earliestSnoozeDate, weekdayAndTime } from '../lib/mail'

  let { onchoose, oncancel }: { onchoose: (at: Date) => void; oncancel: () => void } = $props()

  // Called from the template rather than cached in a `const`: `mail.ts`'s
  // own doc on `snoozeChoices` warns that a fixed instant baked in at load
  // time would drift false, and a `const` read once here at mount was
  // exactly that -- correct the moment the sheet opened, quietly wrong (a
  // dropped "Later today", a stale "in 3 hours") the longer it stayed open.

  let customDate = $state('')
  let customTime = $state('08:00')

  function pickCustom() {
    if (!customDate) return
    onchoose(customSnoozeInstant(customDate, customTime))
  }

  /** 1-5 picks a preset while the picker is open -- the digit matching
   *  whichever row that number is, top to bottom, same as the row order
   *  `mail.snoozeOptions()` already draws. Guarded by `isTyping` the same
   *  way `MailQuickReplies.svelte` guards its own digit shortcut: the date
   *  and time fields below are typed into with digits too, and a global
   *  `keydown` would otherwise steal every one of them. */
  function onKeydown(e: KeyboardEvent) {
    if (isTyping(e.target)) return
    const n = Number(e.key)
    if (!Number.isInteger(n) || n < 1 || n > 5) return
    const choice = mail.snoozeOptions()[n - 1]
    if (choice) onchoose(choice.at)
  }
</script>

<svelte:window onkeydown={onKeydown} />

<Sheet label="Snooze this thread" sheetClass="small" onclose={oncancel}>
  <h2>Snooze until…</h2>
  <div class="choices">
    {#each mail.snoozeOptions() as choice, i (choice.key)}
      <button class="choice" use:focusOnMount={i === 0} onclick={() => onchoose(choice.at)}>
        <span class="name"><kbd aria-hidden="true">{i + 1}</kbd>{choice.label}</span>
        <span class="when">{weekdayAndTime(choice.at.toISOString())}</span>
      </button>
    {/each}
  </div>
  <div class="custom">
    <!-- Never today: today's 8am may already be behind `now`, which would
         release the thread almost immediately -- `snoozeChoices`'s own
         "Later today" is what picking "later, today" means instead. -->
    <input type="date" bind:value={customDate} min={earliestSnoozeDate()} aria-label="Date" />
    <input type="time" bind:value={customTime} aria-label="Time" />
    <button class="btn" disabled={!customDate} onclick={pickCustom}>Snooze</button>
  </div>
  <div class="sheet-row">
    <span class="spacer"></span>
    <button class="btn" onclick={oncancel}>Cancel</button>
  </div>
</Sheet>

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
  .name {
    display: inline-flex;
    align-items: baseline;
    gap: var(--sp-2);
  }
  /* The digit that picks the row, quiet; a phone has no keyboard for it. */
  kbd {
    min-width: 1ch;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  :global([data-phone]) kbd {
    display: none;
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
  .custom input[type='time'] {
    flex: none;
    width: 6.5em;
  }

  /* ── On a phone ─────────────────────────────────────────────────────
     Rows to tap, and the date and time on a line of their own with the
     button under them: three side by side were wider than the sheet. */
  :global([data-phone]) .choice {
    min-height: 48px;
    padding: 0 var(--sp-3);
  }
  :global([data-phone]) .custom {
    flex-wrap: wrap;
  }
  :global([data-phone]) .custom input {
    min-height: 44px;
    min-width: 0;
    font-size: 16px;
  }
  :global([data-phone]) .custom input[type='date'] {
    flex: 1 1 100%;
  }
  :global([data-phone]) .custom input[type='time'] {
    flex: 1;
    width: auto;
  }
</style>
