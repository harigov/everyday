<script lang="ts">
  // The journal's page: the day, the title, what the day recorded in numbers,
  // and a Delete button. The text itself is `RichText`, which the notes app
  // draws too -- so a photograph dropped into an entry and a photograph
  // dropped into a note go through one media path and one set of caret bugs.
  import { onDestroy } from 'svelte'
  import { app } from '../lib/state.svelte'
  import { longDate, plural, relativeTime } from '../lib/format'
  import RichText from './RichText.svelte'
  import EntryMeta from './EntryMeta.svelte'
  import TrackerStrip from './TrackerStrip.svelte'
  import Logo from './Logo.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import type { Attachment, RichDoc } from '../lib/types'

  let words = $state(0)
  let dropping = $state(false)
  /** The file being written to the vault, if any. See `RichText`. */
  let storing = $state<string | null>(null)
  let confirmingDelete = $state(false)

  const entry = $derived(app.entry)

  function attach(a: Attachment) {
    const target = app.entry
    if (!target) return
    target.attachments = [...target.attachments, a]
    app.scheduleSave()
  }

  onDestroy(() => {
    // `RichText` captures the document on the way out; this makes sure it
    // reaches disk.
    app.syncBody()
    void app.flush()
  })

  async function deleteEntry() {
    confirmingDelete = false
    const id = app.entry?.id
    if (id) await app.deleteEntry(id)
  }

  function onDragOver(e: DragEvent) {
    if (e.dataTransfer?.types.includes('Files')) {
      e.preventDefault()
      dropping = true
    }
  }
</script>

{#if !entry}
  <div class="empty">
    <EmptyState lead="Nothing open">
      {#snippet icon()}<Logo size={40} />{/snippet}
      {#snippet note()}Choose a day from the list, or start today's entry.{/snippet}
      {#snippet action()}
        <button class="btn btn-primary" onclick={() => app.newEntry()}>Today's entry</button>
      {/snippet}
    </EmptyState>
  </div>
{:else}
  <div
    class="editor"
    class:dropping
    ondragover={onDragOver}
    ondragleave={() => (dropping = false)}
    ondrop={() => (dropping = false)}
    role="region"
    aria-label="Entry editor"
  >
    <div class="scroll canvas">
      <div class="page">
        <header class="head">
          <div class="date">{longDate(entry.localDate)}</div>
          <input
            class="title"
            placeholder="Title"
            bind:value={entry.title}
            oninput={() => {
              app.scheduleSave()
              app.touch()
            }}
            spellcheck="false"
          />
          <EntryMeta {entry} />
          <!-- What the day recorded in numbers, under what it recorded in
               prose. Keyed on the journal and the date rather than on the
               entry, because a reading belongs to the day: it survives this
               entry being deleted, and it can be recorded on a day nothing
               was written at all. -->
          <TrackerStrip journalId={entry.journalId} date={entry.localDate} />
        </header>

        <RichText
          docId={entry.id}
          doc={() => app.entry?.body}
          placeholder="What happened today?"
          bindBody={(get: (() => RichDoc) | null) => app.bindBody(get)}
          syncBody={() => app.syncBody()}
          onedit={() => app.scheduleSave()}
          onattach={attach}
          onwords={(n: number) => (words = n)}
          onstoring={(f: string | null) => (storing = f)}
        />
      </div>
    </div>

    <footer class="status">
      <span>{plural(words, 'word')}</span>
      <span class="dot">·</span>
      <span>
        {#if storing}Storing {storing}…
        {:else if app.saving}Saving…
        {:else if app.lastSaved}Saved {relativeTime(app.lastSaved)}
        {:else}Edited {relativeTime(entry.updatedAt)}{/if}
      </span>
      <span class="spacer"></span>
      <button class="delete" onclick={() => (confirmingDelete = true)} title="Delete this entry">
        <Icon name="trash" size={14} weight={1.5} />
        Delete
      </button>
    </footer>

    {#if dropping}
      <div class="dropzone"><span>Drop to add to this entry</span></div>
    {/if}
  </div>

  {#if confirmingDelete}
    <ConfirmDialog
      title="Delete this entry?"
      detail={'“' +
        (entry.title || 'Untitled entry') +
        '” and anything attached to it will be removed. This cannot be undone.'}
      confirmLabel="Delete entry"
      onconfirm={deleteEntry}
      oncancel={() => (confirmingDelete = false)}
    />
  {/if}
{/if}

<style>
  .editor {
    position: relative;
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--bg-raised);
  }
  .canvas {
    flex: 1;
  }

  /* --measure is the text column; the padding sits outside it. Setting it as
     the box width instead cost two thirds of an inch of line on every side. */
  .page {
    max-width: calc(var(--measure) + var(--sp-8) * 2);
    margin: 0 auto;
    padding: var(--sp-10) var(--sp-8) 30vh;
  }

  .head {
    margin-bottom: var(--sp-6);
  }

  .date {
    font-size: var(--text-sm);
    font-weight: 600;
    letter-spacing: 0.02em;
    color: var(--journal-accent, var(--accent));
    margin-bottom: var(--sp-2);
  }

  .title {
    width: 100%;
    border: none;
    background: none;
    padding: 0;
    font-family: var(--font-read);
    font-size: var(--text-3xl);
    font-weight: 650;
    line-height: var(--leading-tight);
    letter-spacing: -0.018em;
    color: var(--fg);
    user-select: text;
  }
  .title::placeholder {
    color: var(--fg-faint);
    font-weight: 500;
  }

  .status {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: 30px;
    /* Room on the right for the floating assistant button, which is drawn
       over this corner of the window: without it, Delete sits underneath a
       48px disc and the only way to reach it is to open the rail. */
    padding: 0 var(--fab-clear) 0 var(--sp-4);
    border-top: 1px solid var(--border);
    font-size: var(--text-xs);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }
  .dot {
    opacity: 0.5;
  }
  .status .spacer {
    flex: 1;
  }

  /* Quiet until wanted: deleting an entry should be findable, not inviting. */
  .delete {
    display: flex;
    align-items: center;
    gap: 5px;
    height: 22px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
    font-size: var(--text-xs);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .delete:hover {
    background: color-mix(in oklab, var(--danger) 12%, transparent);
    color: var(--danger);
  }

  .dropzone {
    position: absolute;
    inset: var(--sp-3);
    display: grid;
    place-items: center;
    border: 2px dashed var(--accent);
    border-radius: var(--radius-lg);
    background: color-mix(in oklab, var(--accent) 8%, transparent);
    backdrop-filter: blur(2px);
    font-weight: 600;
    color: var(--accent);
    pointer-events: none;
  }

  .empty {
    display: flex;
    height: 100%;
    background: var(--bg-raised);
  }

  /* ── Prose ───────────────────────────────────────────────────────────
     Typography for the entry body. This is the part of the app people
     actually look at, so it gets the reading face, a generous measure and a
     line height chosen for continuous reading rather than for UI density.

     The face is a humanist sans rather than a serif. It still contrasts with
     the interface around it -- different drawing, different colour on the
     page -- without the serif's mismatch against a screen-first UI. */
</style>
