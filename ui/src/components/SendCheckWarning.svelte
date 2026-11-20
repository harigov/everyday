<script lang="ts">
  // What the checks before sending found, above the row of buttons in the
  // compose sheet: the checks that did not pass, each with what to change
  // when the model said, or why they could not run. Nothing here stops the
  // message -- it is a warning, so Send anyway is as near as Keep editing --
  // but the safe one is the one that has focus and the fill, because the
  // person pressed Send a moment ago and is likely to press it again.
  //
  // The sheet decides when this is shown and what the two buttons do; see
  // `send` in `MailCompose.svelte`. The checks are Settings → Mail writing's
  // (`MailWritingPanel.svelte`).

  import { focusOnMount } from '../lib/focus'
  import type { DraftCheckResult } from '../lib/types'
  import Icon from './Icon.svelte'

  let {
    failed,
    skipped,
    onsend,
    onedit,
  }: {
    /** The checks that did not pass, in Settings' order. */
    failed: DraftCheckResult[]
    /** Why the checks did not run, as a sentence, when they did not. */
    skipped: string | null
    /** Send anyway: as it was asked for, without checking again. */
    onsend: () => void
    /** Keep editing: put this away and go back to the words. */
    onedit: () => void
  } = $props()

  const title = $derived(
    failed.length > 0
      ? `${failed.length} ${failed.length === 1 ? 'check' : 'checks'} didn't pass`
      : "Checks couldn't run",
  )
</script>

<div class="check-warning" role="alert">
  <p class="title">
    <Icon name="alert" size={13} />
    <strong>{title}</strong>
  </p>
  {#if failed.length > 0}
    <ul>
      {#each failed as item, i (i)}
        <li>
          <span class="check">{item.check}</span>
          {#if item.note}<span class="note">{item.note}</span>{/if}
        </li>
      {/each}
    </ul>
  {/if}
  {#if skipped}<p class="skipped">{skipped}</p>{/if}
  <div class="actions">
    <button class="btn btn-primary btn-sm" use:focusOnMount onclick={onedit}>Keep editing</button>
    <button class="btn btn-outline btn-sm" onclick={onsend}>Send anyway</button>
  </div>
</div>

<style>
  /* The same box `.unsent` draws for a send the server refused, in the
     warning colour: this one is a heads-up, not a failure. */
  .check-warning {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid color-mix(in oklab, var(--warning) 45%, var(--border));
    border-radius: var(--radius-sm);
    background: color-mix(in oklab, var(--warning) 7%, transparent);
    font-size: var(--text-sm);
    color: var(--fg);
    overflow-wrap: anywhere;
  }
  .title {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin: 0;
  }
  .title :global(svg) {
    flex: none;
    color: var(--warning);
  }
  ul {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  li {
    display: flex;
    flex-direction: column;
  }
  .note,
  .skipped {
    color: var(--fg-muted);
  }
  .skipped {
    margin: 0;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }
</style>
