<script lang="ts">
  // A note's page: a title, its tags, what it is for, and the text.
  //
  // Deliberately less than the journal's page. There is no date, because a
  // note is not a day; there is no tracker strip, because a reading belongs
  // to a day and this is not one. The text itself is `RichText`, the same
  // component the journal draws, so the editor, the media pipeline and the
  // caret behave identically in both.

  import { onDestroy } from 'svelte'
  import { app } from '../lib/state.svelte'
  import { notes } from '../lib/notes.svelte'
  import { focusOnMount } from '../lib/focus'
  import { plural, relativeTime } from '../lib/format'
  import RichText from './RichText.svelte'
  import PurposeField from './PurposeField.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import type { Attachment, Purpose, RichDoc } from '../lib/types'

  let words = $state(0)
  let dropping = $state(false)
  let storing = $state<string | null>(null)
  let confirmingDelete = $state(false)
  let addingTag = $state(false)
  let tagDraft = $state('')

  const note = $derived(notes.open)

  /**
   * The getter the editor is bound to, and the sync that runs on teardown.
   *
   * Kept here rather than in the store because the store has no business
   * knowing that a ProseMirror view exists; it is handed a document.
   */
  let body: (() => RichDoc) | null = null

  function syncBody() {
    if (notes.open && body) notes.open.body = body()
  }

  function attach(a: Attachment) {
    const target = notes.open
    if (!target) return
    target.attachments = [...target.attachments, a]
    notes.edited(target.body)
  }

  function commitTag() {
    const tag = tagDraft.trim().replace(/^#/, '')
    tagDraft = ''
    addingTag = false
    const target = notes.open
    if (!tag || !target) return
    if (target.tags.some((t) => t.toLowerCase() === tag.toLowerCase())) return
    void notes.setTags([...target.tags, tag])
  }

  function removeTag(tag: string) {
    const target = notes.open
    if (!target) return
    void notes.setTags(target.tags.filter((t) => t !== tag))
  }

  function setPurpose(next: Purpose | null) {
    const target = notes.open
    if (!target) return
    target.purpose = next
    void notes.setTags(target.tags)
  }

  onDestroy(() => {
    syncBody()
    void notes.flush()
  })

  async function remove() {
    confirmingDelete = false
    const id = notes.open?.id
    if (id) await notes.remove(id)
  }

  function onDragOver(e: DragEvent) {
    if (e.dataTransfer?.types.includes('Files')) {
      e.preventDefault()
      dropping = true
    }
  }
</script>

{#if !note}
  <div class="empty">
    <EmptyState lead="Nothing open">
      {#snippet icon()}<Icon name="quote" size={34} weight={1.4} />{/snippet}
      {#snippet note()}
        A note is a recipe, a reading list, the notes from a call — anything that is not a day.
      {/snippet}
      {#snippet action()}
        <button class="btn btn-primary" onclick={() => void notes.create()}>New note</button>
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
    aria-label="Note editor"
  >
    {#if notes.conflict}
      <div class="conflict">
        <span>
          This note was changed somewhere else while you were editing it. Keep which one?
        </span>
        <button class="btn" onclick={() => void notes.takeTheirs()}>Take theirs</button>
        <button class="btn btn-primary" onclick={() => void notes.keepMine()}>Keep mine</button>
      </div>
    {/if}

    <div class="scroll canvas">
      <div class="page">
        <header class="head">
          <input
            class="title"
            placeholder="Title"
            value={note.title}
            oninput={(e) => notes.setTitle(e.currentTarget.value)}
            spellcheck="false"
          />

          <div class="meta">
            <button
              class="pin"
              class:on={note.pinned}
              title={note.pinned ? 'Unpin' : 'Pin to the top'}
              onclick={() => void notes.togglePinned()}
            >
              <Icon name="pin" size={13} weight={1.6} />
              {note.pinned ? 'Pinned' : 'Pin'}
            </button>

            {#each note.tags as tag (tag)}
              <button class="tag chip" onclick={() => removeTag(tag)} title="Remove tag">
                {tag}<span class="x"><Icon name="close" size={11} weight={1.8} /></span>
              </button>
            {/each}

            {#if addingTag}
              <input
                class="tag-input"
                placeholder="tag"
                bind:value={tagDraft}
                use:focusOnMount
                onblur={commitTag}
                onkeydown={(e) => {
                  if (e.key === 'Enter') commitTag()
                  if (e.key === 'Escape') {
                    tagDraft = ''
                    addingTag = false
                  }
                }}
              />
            {:else}
              <button class="add" onclick={() => (addingTag = true)}>
                <Icon name="plus" size={11} weight={1.8} /> Tag
              </button>
            {/if}
          </div>

          {#if app.supportsOverview}
            <PurposeField value={note.purpose} onchange={setPurpose} />
          {/if}
        </header>

        <RichText
          docId={note.id}
          doc={() => notes.open?.body}
          placeholder="Write…"
          bindBody={(get: (() => RichDoc) | null) => (body = get)}
          {syncBody}
          onedit={() => {
            syncBody()
            if (notes.open) notes.edited(notes.open.body)
          }}
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
        {:else}Edited {relativeTime(note.updatedAt)}{/if}
      </span>
      <span class="spacer"></span>
      <button class="delete" onclick={() => (confirmingDelete = true)} title="Delete this note">
        <Icon name="trash" size={14} weight={1.5} />
        Delete
      </button>
    </footer>

    {#if dropping}
      <div class="dropzone"><span>Drop to add to this note</span></div>
    {/if}
  </div>

  {#if confirmingDelete}
    <ConfirmDialog
      title="Delete this note?"
      detail={'“' +
        (note.title || 'Untitled note') +
        '” and anything attached to it will be removed. This cannot be undone.'}
      confirmLabel="Delete note"
      onconfirm={remove}
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

  .conflict {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-3) var(--sp-5);
    border-bottom: 1px solid var(--line);
    background: color-mix(in oklab, var(--warning) 12%, var(--bg-raised));
    color: var(--fg);
    font-size: var(--text-sm);
  }

  .conflict span {
    flex: 1;
    min-width: 14rem;
  }

  .meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    margin-top: var(--sp-3);
  }

  .chip,
  .add,
  .pin {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    height: 22px;
    padding: 0 var(--sp-2);
    border-radius: 999px;
    background: var(--bg-hover);
    color: var(--fg-muted);
    font-size: var(--text-xs);
  }

  .chip:hover,
  .add:hover,
  .pin:hover {
    color: var(--fg);
  }

  .pin.on {
    background: var(--bg-active);
    color: var(--fg);
    font-weight: 550;
  }

  .tag .x {
    display: none;
  }

  .tag:hover .x {
    display: inline-flex;
  }

  .tag-input {
    width: 6rem;
    height: 22px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--line);
    border-radius: 999px;
    background: var(--bg);
    color: var(--fg);
    font-size: var(--text-xs);
  }
</style>
