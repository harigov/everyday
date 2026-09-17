<script lang="ts">
  // A pending note proposal, read-only, in place of the editor.
  //
  // Unlike a task or a block, a note is not a handful of fields -- it is a
  // page of prose, and there is no draft-editing story for prose that is not
  // just the editor itself. So this pane shows what the note would read as
  // and offers only accept and decline; the plan's own words are "edit after
  // accepting", and accepting hands the record straight to the ordinary
  // editor the way any other note does.

  import { DECLINE_REASONS, proposals, recordAs } from '../lib/proposals.svelte'
  import { notes, previewText } from '../lib/notes.svelte'
  import { plural } from '../lib/format'
  import Icon from './Icon.svelte'
  import type { Proposal } from '../lib/types'

  let { proposal }: { proposal: Proposal } = $props()

  const note = $derived(recordAs(proposal, 'note'))
  const text = $derived(note ? previewText(note.body) : '')
  const busy = $derived(proposals.isBusy(proposal.id))
  let choosing = $state(false)

  async function accept() {
    if (!note) return
    // Nothing has been edited here -- there is nothing to edit -- so this is
    // always the plain accept, never `edited(...)`.
    const closed = await proposals.accept(proposal)
    if (closed) notes.closeProposal()
  }

  async function decline(reason: (typeof DECLINE_REASONS)[number]['reason'] | null) {
    choosing = false
    await proposals.decline(proposal, reason)
    notes.closeProposal()
  }
</script>

{#if note}
  <div class="editor proposal">
    <div class="scroll canvas">
      <div class="page">
        <header class="head">
          <span class="badge"><Icon name="sparkle" size={12} /> Proposed note</span>
          <h1 class="title">{note.title || 'Untitled note'}</h1>
          {#if proposal.why}<p class="why">{proposal.why}</p>{/if}
          {#if note.tags.length > 0}
            <div class="tags">
              {#each note.tags as tag (tag)}<span class="chip">{tag}</span>{/each}
            </div>
          {/if}
        </header>

        <!-- Plain text, not the rich editor: there is nothing to edit here,
             and the editor's whole job is editing. `previewText` is the same
             walk the list's own excerpt is built from. -->
        <p class="body">{text || 'Nothing written yet.'}</p>
        <p class="wordcount">{plural(text.split(/\s+/).filter(Boolean).length, 'word')}</p>
      </div>
    </div>

    <footer class="status">
      <span class="spacer"></span>
      <div class="decline-wrap">
        <button class="btn" disabled={busy} onclick={() => (choosing = !choosing)}>
          Decline
        </button>
        {#if choosing}
          <div class="reasons" role="menu">
            <button role="menuitem" onclick={() => decline(null)}>Just decline</button>
            {#each DECLINE_REASONS as r (r.label)}
              <button role="menuitem" onclick={() => decline(r.reason)}>{r.label}</button>
            {/each}
          </div>
        {/if}
      </div>
      <button class="btn btn-primary" disabled={busy} onclick={accept}>Accept</button>
    </footer>
  </div>
{/if}

<style>
  .editor {
    position: relative;
    display: flex;
    flex: 1;
    min-width: 0;
    flex-direction: column;
    height: 100%;
    background: var(--bg-raised);
  }
  .canvas {
    flex: 1;
  }
  .page {
    max-width: calc(var(--measure) + var(--sp-8) * 2);
    margin: 0 auto;
    padding: var(--sp-10) var(--sp-8) 30vh;
  }

  .head {
    margin-bottom: var(--sp-6);
  }
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    margin-bottom: var(--sp-2);
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--accent);
    text-transform: lowercase;
  }
  .title {
    font-family: var(--font-read);
    font-size: var(--text-page-title);
    font-weight: 650;
    line-height: var(--leading-tight);
    letter-spacing: -0.018em;
    color: var(--fg);
  }
  .why {
    margin-top: var(--sp-2);
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--radius-sm);
    background: color-mix(in oklab, var(--accent) 8%, transparent);
    color: var(--fg-muted);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
  }
  .tags {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-1);
    margin-top: var(--sp-3);
  }
  .chip {
    padding: 2px var(--sp-2);
    border-radius: 999px;
    background: var(--bg-hover);
    color: var(--fg-muted);
    font-size: var(--text-xs);
  }

  .body {
    font-family: var(--font-read);
    font-size: var(--text-base);
    line-height: var(--leading-normal);
    color: var(--fg);
    white-space: pre-wrap;
  }
  .wordcount {
    margin-top: var(--sp-4);
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }

  .status {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: 46px;
    padding: 0 var(--fab-clear) 0 var(--sp-4);
    border-top: 1px solid var(--border);
    flex: none;
  }
  .status .spacer {
    flex: 1;
  }
  .decline-wrap {
    position: relative;
  }
  .reasons {
    position: absolute;
    right: 0;
    bottom: 100%;
    z-index: 20;
    display: flex;
    flex-direction: column;
    min-width: 160px;
    margin-bottom: 4px;
    padding: var(--sp-1);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    box-shadow: var(--shadow);
  }
  .reasons button {
    padding: var(--sp-1) var(--sp-2);
    border: none;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    text-align: left;
    cursor: pointer;
  }
  .reasons button:hover {
    background: var(--bg-hover);
  }
</style>
