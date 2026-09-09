<script lang="ts">
  // What a list says when there is nothing in it.
  //
  // There were five of these before there was one, and they disagreed about
  // everything a reader notices: the journal's sat at the top of the panel
  // in body text, the todo list's a third of the way down in two sizes, the
  // library's centred inside a `34rem` column with its own type ramp, the
  // assistant's flush left in a rail. Same sentence, four voices.
  //
  // The shape here is the one that reads best in every pane the application
  // has: centred across the pane, held a little above the true middle --
  // optical centre, because a block of text centred by arithmetic looks like
  // it has slipped -- with an optional mark above the lead, a quieter second
  // line below it, and room for one action under that.

  import type { Snippet } from 'svelte'

  let {
    lead,
    note,
    icon,
    action,
    /** Pass `false` in a rail or a dropdown, where there is no height to centre in. */
    centred = true,
  }: {
    /** The one sentence. Says what is not here, in the reader's terms. */
    lead: string
    /** What to do about it, or why it is empty. Optional and usually worth it. */
    note?: Snippet
    /** A mark above the lead. Optional: a list of one line does not need one. */
    icon?: Snippet
    /** A button under the note. At most one -- this is a suggestion, not a form. */
    action?: Snippet
    centred?: boolean
  } = $props()
</script>

<div class="empty" class:centred>
  <div class="inner">
    {#if icon}
      <div class="mark" aria-hidden="true">{@render icon()}</div>
    {/if}
    <p class="blank-lead">{lead}</p>
    {#if note}
      <p class="blank-note">{@render note()}</p>
    {/if}
    {#if action}
      <div class="act">{@render action()}</div>
    {/if}
  </div>
</div>

<style>
  .empty {
    display: flex;
    justify-content: center;
    padding: var(--sp-10) var(--sp-6);
  }
  /* Centred in what is left of the pane, and pulled up a touch: a block held
     at the arithmetic middle of a tall column reads as having sunk. */
  .empty.centred {
    flex: 1;
    align-items: center;
    min-height: 0;
    padding-bottom: var(--sp-16);
  }

  .inner {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--sp-2);
    /* Measured in characters, so the second line wraps where a sentence
       wants to rather than wherever the pane happens to end. */
    max-width: 44ch;
    text-align: center;
  }

  .mark {
    display: flex;
    margin-bottom: var(--sp-2);
    color: var(--fg-faint);
  }

  .act {
    margin-top: var(--sp-3);
  }
  /* The house `.btn` is a ghost: no border until the pointer is on it, which
     is right in a toolbar full of them and wrong as the only thing on an
     otherwise empty screen -- there it reads as a bold sentence rather than
     as something to press. Given an outline here, and only here. The primary
     variant already has a fill and is left alone. */
  .act :global(.btn:not(.btn-primary)) {
    border: 1px solid var(--border);
    background: var(--bg-raised);
    color: var(--fg-muted);
    box-shadow: var(--shadow-sm);
  }
  .act :global(.btn:not(.btn-primary):hover) {
    border-color: var(--border-strong);
    color: var(--fg);
  }
</style>
