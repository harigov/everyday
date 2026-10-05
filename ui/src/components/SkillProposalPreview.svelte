<script lang="ts">
  // A pending skill proposal, drawn inside the generic ghost row the
  // "Waiting for you" pane gives every proposal kind -- see
  // `ProposalGhost.svelte`'s `children` slot. Skills have no app of their
  // own the way a note or a task does, only Settings → Skills, so there is
  // nowhere richer than this row to open one mid-review: accept or decline
  // is the whole of the decision.
  //
  // Instructions can run to pages, and the slot this sits inside is a
  // single-line caption everywhere else, so the name is shown in full but
  // the instructions are capped to a short, scrollable box rather than laid
  // out -- "collapsed or scrolled", not "spelled out". Nothing here is a
  // button: the slot it sits inside is itself a button when the ghost is
  // given an `onopen`, and a button inside a button is not a thing to add.

  import { recordAs } from '../lib/proposals.svelte'
  import type { Proposal } from '../lib/types'

  let { proposal }: { proposal: Proposal } = $props()

  const skill = $derived(recordAs(proposal, 'skill'))
  // A `Delete` proposal carries no record, only the id it would remove --
  // the caption ("Delete skill: …") is already the whole of what there is
  // to say about it.
  const isReplace = $derived(proposal.payload.type === 'replace')
</script>

<span class="skillpreview">
  {#if skill}
    <span class="name">
      {skill.name}
      {#if isReplace}<span class="tag">changes an existing skill</span>{/if}
    </span>
    {#if skill.description}<span class="desc">{skill.description}</span>{/if}
    {#if skill.instructions}<span class="instructions">{skill.instructions}</span>{/if}
  {:else}
    {proposal.caption}
  {/if}
</span>

<style>
  .skillpreview {
    display: flex;
    flex-direction: column;
    gap: 2px;
    width: 100%;
    min-width: 0;
    /* Undoes the single-line ellipsis `ProposalGhost`'s `.caption`/`.open`
       impose on whatever is passed as children -- this preview is several
       lines on purpose. */
    white-space: normal;
    overflow: visible;
    text-overflow: clip;
  }
  .name {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: var(--sp-2);
    font-weight: 600;
    color: var(--fg);
  }
  .tag {
    font-size: var(--text-xs);
    font-weight: 400;
    color: var(--fg-faint);
  }
  .desc {
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }
  .instructions {
    display: block;
    max-height: 4.5em;
    overflow-y: auto;
    padding: var(--sp-1) var(--sp-2);
    border-radius: var(--radius-sm);
    background: var(--bg-hover);
    color: var(--fg-faint);
    font-size: var(--text-xs);
    white-space: pre-wrap;
  }
</style>
