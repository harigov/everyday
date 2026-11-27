<script lang="ts">
  // A note's page: a title, its tags, what it is for, and the text.
  //
  // Deliberately less than the journal's page. There is no date, because a
  // note is not a day; there is no tracker strip, because a reading belongs
  // to a day and this is not one. The text itself is `RichText`, the same
  // component the journal draws, so the editor, the media pipeline and the
  // caret behave identically in both.

  import { onDestroy } from 'svelte'
  import { app, handle } from '../lib/state.svelte'
  import { notes } from '../lib/notes.svelte'
  import { meetings } from '../lib/meetings.svelte'
  import { draftStageLabel, recordingWritingNote } from '../lib/meetings-format'
  import { insertFilled, templates } from '../lib/templates.svelte'
  import { details, phone } from '../lib/phone.svelte'
  import { plural, relativeTime } from '../lib/format'
  import { renderMarkdown } from '../lib/markdown'
  import { mergeTags } from '../lib/tags'
  import { documentToMarkdown, type PMNode } from '../lib/markdown-copy'
  import RichText from './RichText.svelte'
  import Toolbar from './Toolbar.svelte'
  import NotesNav from './NotesNav.svelte'
  import PhoneBack from './PhoneBack.svelte'
  import type { Editor as TipTapEditor } from '@tiptap/core'
  import PurposeField from './PurposeField.svelte'
  import TagEditor from './TagEditor.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import TranscriptPanel from './TranscriptPanel.svelte'
  import NoteProposalPreview from './NoteProposalPreview.svelte'
  import type { Attachment, Purpose, QuickTaskDraft, RichDoc, Template } from '../lib/types'
  import { api } from '../lib/api'
  import { purpose as purposeStore } from '../lib/purpose.svelte'
  import { ask, quick, slot } from '../lib/quick.svelte'
  import { todo } from '../lib/todo.svelte'
  import { agent } from '../lib/agent.svelte'
  import { noteOrigin, ORIGIN_ICONS, originLine } from '../lib/note-list'
  import Suggestions from './Suggestions.svelte'

  let editor = $state<TipTapEditor | null>(null)
  let words = $state(0)
  let dropping = $state(false)
  let storing = $state<string | null>(null)
  let confirmingDelete = $state(false)

  const note = $derived(notes.open)

  // A call's note exists as a draft from the moment the call ends. While its
  // recording is still being worked on -- or has failed -- say so above it.
  const writing = $derived(recordingWritingNote(meetings.recordings, note?.id))
  const drafting = $derived(writing !== null)
  const draftFailed = $derived(writing?.stage.type === 'failed')
  const draftReason = $derived(writing?.stage.type === 'failed' ? writing.stage.reason : null)
  const draftStage = $derived(writing ? (draftStageLabel(writing.stage) ?? 'Working on it') : '')
  let retrying = $state(false)

  async function retryDraft() {
    if (!writing) return
    retrying = true
    try {
      await meetings.retryRecording(writing.id)
    } finally {
      retrying = false
    }
  }

  // ── On a phone ───────────────────────────────────────────────────────
  //
  // The list of notes is the app's page there -- `NotesNav`, drawn in the
  // stage -- and a note is opened over it, with a way back (`details`, and
  // `PhoneBack` in the page's own header). The list opens what the person
  // taps itself; these keep the flag true to what is on screen when
  // something else did.

  // A note opened from elsewhere -- a job's card, a file's "used in" -- is
  // opened over the list too.
  let lastSelected = notes.selected
  $effect(() => {
    const now = notes.selected
    if (now === lastSelected) return
    lastSelected = now
    if (now) details.open('notes')
  })

  // Back to the list: what was typed has to be on disk before the editor
  // goes, and nothing here makes sure of that but this.
  let wasOpen = false
  $effect(() => {
    const open = details.isOpen('notes')
    if (wasOpen && !open) {
      notes.syncBody()
      void notes.flush()
    }
    wasOpen = open
  })

  // The note went -- deleted here, or somewhere else -- or the proposal was
  // answered: there is nothing left to show over the list.
  let had = false
  $effect(() => {
    if (notes.open || notes.selectedProposal) had = true
    else if (had) {
      had = false
      details.close('notes')
    }
  })

  function attach(a: Attachment) {
    const target = notes.open
    if (!target) return
    target.attachments = [...target.attachments, a]
    notes.edited()
  }

  /** A template filled in for today, at the caret -- the journal editor's
   *  `insertTemplate`, for a note: a title only into an empty one, tags added. */
  async function insertTemplate(t: Template) {
    const target = notes.open
    if (!target || !editor) return
    try {
      const filled = await templates.render(t)
      if (notes.open !== target || !editor) return
      insertFilled(editor, target, filled)
      notes.edited()
    } catch (e) {
      await handle(e)
    }
  }

  /** Who started the open note -- read from its tags if it is older than
   *  the field -- and the line saying so over it, none for the person's own. */
  const kind = $derived(note ? noteOrigin(note) : 'own')
  const origin = $derived(originLine(kind, agent.displayName))

  // The tags already in use are offered as you type (`TagEditor`), most used
  // first with their counts. Without them every note grew its own spelling of
  // the same idea -- a real vault had forty tags on thirty-seven notes,
  // thirty-one of them used once -- and a filter over forty one-note tags
  // narrows nothing.

  function setPurpose(next: Purpose | null) {
    notes.setPurpose(next)
  }

  // ── what the quick model read out of the note ────────────────────────
  //
  // Three jobs, and all of them on demand. A note has less privacy weight
  // than a journal entry -- it is a recipe, a reading list, the notes from a
  // call -- so these default on, but they still do not fire as you type: a
  // note being written is a note being changed, and a suggestion about a
  // half-finished sentence is noise.
  //
  // `notes.tasks` is the one that earns its keep. The page of notes from a
  // call becomes five tasks with dates, which is the thing people currently
  // do by retyping.

  let titleIdea = $state('')
  let taskChips = $state<{ key: string; label: string }[]>([])
  let taskDrafts = $state<QuickTaskDraft[]>([])
  let tagChips = $state<{ key: string; label: string }[]>([])
  let scanning = $state(false)
  const taskSlot = slot<QuickTaskDraft[]>()

  /** Get the body onto disk before asking anything to read it back. */
  async function settle() {
    app.syncBody()
    await notes.flush()
  }

  async function readTheNote() {
    const target = notes.open
    if (!target) return
    scanning = true
    await settle()

    // All three at once. They read the same note and there is no reason for
    // somebody to wait through three round trips to find out that a note has
    // no tasks in it.
    const [title, tasks, labels] = await Promise.all([
      target.title.trim()
        ? Promise.resolve(null)
        : ask('notes.title', () => api.quickNoteTitle(target.id)),
      ask('notes.tasks', () => api.quickNoteTasks(target.id)),
      ask('notes.labels', () => api.quickNoteLabels(target.id)),
    ])

    scanning = false
    titleIdea = title ?? ''
    taskDrafts = tasks ?? []
    taskChips = taskDrafts.map((t, i) => ({ key: String(i), label: t.title }))
    tagChips = (labels?.tags ?? []).map((t) => ({ key: t, label: `#${t}` }))
    if (labels?.purpose && !target.purpose) pendingPurpose = labels.purpose
  }

  let pendingPurpose = $state<Purpose | null>(null)

  /** Make one of the proposed tasks. The ordinary create, nothing special. */
  async function acceptTask(key: string) {
    const draft = taskDrafts[Number(key)]
    if (!draft) return
    // Back through the quick-add grammar rather than a second creation path:
    // one place decides what a typed task becomes, and a task made from a
    // note should be indistinguishable from one somebody typed.
    const line = [
      draft.title,
      ...draft.tags.map((t) => `#${t}`),
      draft.priority && draft.priority !== 'none' ? `!${draft.priority}` : '',
      draft.estimateMinutes ? `~${draft.estimateMinutes}m` : '',
      draft.dueDate ? `@${draft.dueDate}` : '',
    ]
      .filter(Boolean)
      .join(' ')
    await todo.add(line)
  }

  function acceptTag(tag: string) {
    const target = notes.open
    if (!target) return
    const next = mergeTags(target.tags, [tag])
    if (next.length !== target.tags.length) void notes.setTags(next)
  }

  onDestroy(() => {
    // `RichText` captures the document on the way out; this makes sure it
    // reaches disk. The journal's page ends the same way.
    notes.syncBody()
    void notes.flush()
  })

  async function remove() {
    confirmingDelete = false
    const id = notes.open?.id
    if (id) await notes.remove(id)
  }

  /**
   * The note as Markdown, for the toolbar's copy button.
   *
   * Read from the live editor, not from `note.body`, which only catches up
   * when a save is due -- so what is copied is what is on the page. An
   * untitled note is copied with no heading rather than a made-up one.
   */
  function noteAsMarkdown(): string {
    const target = notes.open
    if (!target || !editor) return ''
    return documentToMarkdown(target.title, (editor.getJSON().content ?? []) as PMNode[])
  }

  /**
   * "Rewrite summary" replaces the whole body with the model's markdown.
   *
   * The note's body is TipTap JSON, not markdown, and there is no
   * markdown → RichDoc step in this codebase -- so this reuses the one
   * markdown renderer that exists (`renderMarkdown`, built for the
   * assistant's replies) to get HTML, and hands that to the editor exactly
   * the way a paste would arrive. `emitUpdate: true` is what makes this
   * indistinguishable from typing it: `RichText`'s own `onedit` fires and
   * marks the note dirty, same as every other edit on this page.
   */
  function replaceBodyFromMarkdown(markdown: string) {
    if (!editor) return
    editor.commands.setContent(renderMarkdown(markdown), { emitUpdate: true })
  }

  /**
   * A title that wraps, on a phone: a line of a title is too short for most
   * of them, and an input cannot show the rest. Sized to its text.
   */
  function fit(node: HTMLTextAreaElement, _text: string) {
    const size = () => {
      node.style.height = 'auto'
      node.style.height = `${node.scrollHeight}px`
    }
    size()
    node.addEventListener('input', size)
    return {
      update: size,
      destroy: () => node.removeEventListener('input', size),
    }
  }

  /** Enter in the title goes on to the writing, as it would in a field. */
  function titleKeys(e: KeyboardEvent) {
    if (e.key !== 'Enter') return
    e.preventDefault()
    editor?.commands.focus('start')
  }

  function onDragOver(e: DragEvent) {
    if (e.dataTransfer?.types.includes('Files')) {
      e.preventDefault()
      dropping = true
    }
  }
</script>

{#if phone.on && !details.isOpen('notes')}
  <!-- A phone's page for the app: the notes themselves. -->
  <div class="phonehome"><NotesNav home /></div>
{:else if notes.selectedProposal}
  {#if phone.on}
    <div class="phonepage">
      <div class="phonehead">
        <PhoneBack onclick={() => details.close('notes')} label="Notes" />
      </div>
      <NoteProposalPreview proposal={notes.selectedProposal} />
    </div>
  {:else}
    <NoteProposalPreview proposal={notes.selectedProposal} />
  {/if}
{:else if !note}
  {#if phone.on}
    <!-- Only for the moment between a note being asked for and arriving. -->
    <div class="phonepage">
      <div class="phonehead">
        <PhoneBack onclick={() => details.close('notes')} label="Notes" />
      </div>
    </div>
  {:else}
    <div class="empty">
      <EmptyState lead="Pick a note">
        {#snippet icon()}<Icon name="pencil" size={34} weight={1.4} />{/snippet}
        {#snippet note()}
          A note is a recipe, a reading list, the notes from a call — anything that is not a day.
        {/snippet}
        {#snippet action()}
          <button class="btn btn-primary" onclick={() => void notes.create()}>New note</button>
        {/snippet}
      </EmptyState>
    </div>
  {/if}
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

    <!-- A phone shows the note over its list: the way back to it, and the
         one thing the foot of the page keeps for a desktop. -->
    <div class="phonehead phone-only">
      <PhoneBack onclick={() => details.close('notes')} label="Notes" />
      <span class="grow"></span>
      <button
        class="headbtn"
        aria-label="Delete this note"
        onclick={() => (confirmingDelete = true)}
      >
        <Icon name="trash" size={19} weight={1.5} />
      </button>
    </div>

    <Toolbar
      {editor}
      templateFor="notes"
      oninsert={(t: Template) => void insertTemplate(t)}
      markdown={noteAsMarkdown}
    />

    <div class="scroll canvas">
      <div class="page">
        <header class="head">
          {#if phone.on}
            <textarea
              class="title"
              rows="1"
              placeholder="Title"
              aria-label="Title"
              value={note.title}
              use:fit={note.title}
              oninput={(e) => notes.setTitle(e.currentTarget.value)}
              onkeydown={titleKeys}
              spellcheck="false"
            ></textarea>
          {:else}
            <input
              class="title"
              placeholder="Title"
              value={note.title}
              oninput={(e) => notes.setTitle(e.currentTarget.value)}
              spellcheck="false"
            />
          {/if}

          <!-- Into an empty title only. A title you wrote is not a gap. -->
          {#if titleIdea && !note.title.trim()}
            <Suggestions
              scope={note.id}
              items={[{ key: 'title', label: titleIdea }]}
              label="Call it:"
              onaccept={() => {
                notes.setTitle(titleIdea)
                titleIdea = ''
              }}
              ondismiss={() => (titleIdea = '')}
            />
          {/if}

          <div class="meta">
            {#if origin}
              <span class="origin {kind}">
                <Icon name={ORIGIN_ICONS[kind]} size={12} />
                {origin}
              </span>
            {/if}
            <button
              class="pin"
              class:on={note.pinned}
              title={note.pinned ? 'Unpin' : 'Pin to the top'}
              onclick={() => void notes.togglePinned()}
            >
              <Icon name="pin" size={13} weight={1.6} />
              {note.pinned ? 'Pinned' : 'Pin'}
            </button>

            <TagEditor
              tags={note.tags}
              known={notes.tags}
              onchange={(next: string[]) => void notes.setTags(next)}
              id="note-tag-options"
            />
          </div>

          {#if app.supportsOverview}
            <div class="purpose-row">
              <span class="purpose-label">Goal or area</span>
              <PurposeField value={note.purpose} onchange={setPurpose} />
            </div>
          {/if}

          {#if quick.enabled('notes.tasks') || quick.enabled('notes.labels')}
            <div class="quick-row">
              <button
                class="quick-btn"
                disabled={scanning}
                title="Your quick model reads this note and offers a title, tasks and tags. Nothing changes until you accept."
                onclick={() => void readTheNote()}
              >
                <Icon name="sparkle" size={12} />
                What is in here?
              </button>
            </div>
          {/if}

          <!-- The tasks a page of notes actually commits somebody to. Each
               one goes through the same quick-add grammar a typed task does,
               so a task made from a note is indistinguishable from one
               somebody wrote by hand. -->
          <Suggestions
            scope={note.id}
            items={taskChips}
            busy={scanning}
            label="Make a task:"
            onaccept={(key: string) => void acceptTask(key)}
            ondismiss={() => taskSlot.dismiss(() => (taskChips = []))}
          />
          <Suggestions
            scope={note.id}
            items={tagChips}
            label="Tag it:"
            onaccept={acceptTag}
            ondismiss={() => (tagChips = [])}
          />
          {#if pendingPurpose && !note.purpose}
            <Suggestions
              scope={note.id}
              items={[{ key: 'p', label: purposeStore.describe(pendingPurpose).name }]}
              label="File it under:"
              onaccept={() => {
                setPurpose(pendingPurpose)
                pendingPurpose = null
              }}
              ondismiss={() => (pendingPurpose = null)}
            />
          {/if}
        </header>

        {#if drafting}
          <div class="draft" class:failed={draftFailed} role="status">
            {#if draftFailed}
              <Icon name="alert" size={14} />
              <span class="draft-text">
                The summary could not be written{draftReason ? `: ${draftReason}` : '.'}
              </span>
              <button class="btn" disabled={retrying} onclick={() => void retryDraft()}>
                Retry
              </button>
            {:else}
              <span class="spinner" aria-hidden="true"></span>
              <span class="draft-text">{draftStage}…</span>
            {/if}
          </div>
        {/if}

        <RichText
          oneditor={(ed: TipTapEditor | null) => (editor = ed)}
          docId={note.id}
          doc={() => notes.open?.body}
          placeholder="Write…"
          bindBody={(get: (() => RichDoc) | null) => notes.bindBody(get)}
          syncBody={() => notes.syncBody()}
          onedit={() => notes.edited()}
          onattach={attach}
          origin="notes"
          onwords={(n: number) => (words = n)}
          onstoring={(f: string | null) => (storing = f)}
        />

        <TranscriptPanel noteId={note.id} onreplacebody={replaceBodyFromMarkdown} />
      </div>
    </div>

    <footer class="status">
      <span>{plural(words, 'word')}</span>
      <span class="dot">·</span>
      <span>
        {#if storing}Storing {storing}…
        {:else if notes.saving}Saving…
        {:else if notes.lastSaved}Saved {relativeTime(notes.lastSaved)}
        {:else}Edited {relativeTime(note.updatedAt)}{/if}
      </span>
      <span class="spacer"></span>
      <button
        class="delete desktop-only"
        onclick={() => (confirmingDelete = true)}
        title="Delete this note"
      >
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
  /* Quiet by default: these are offers, and a page you write on should not be
     a page of buttons. Same treatment as the journal's. */
  .quick-row {
    display: flex;
    gap: var(--sp-2);
    margin-top: var(--sp-2);
  }

  .quick-btn {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-1);
    padding: 2px var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-pill);
    background: none;
    color: var(--fg-subtle);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
  }

  .quick-btn:hover:not(:disabled) {
    border-color: var(--border-strong);
    color: var(--fg-muted);
  }

  .quick-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  /* `flex: 1` and `min-width: 0`, because this component *is* the pane.
     The journal wraps its editor in a `<main class="main">` that carries
     both; this one is dropped straight into the window's flex row, so
     without them it took its width from its own content -- about half a
     wide window, with the rest of the row left empty to the right of it.
     No background of its own: the raised surface every app sits on is
     App.svelte's. */
  .editor {
    position: relative;
    display: flex;
    flex: 1;
    min-width: 0;
    flex-direction: column;
    height: 100%;
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
    font-size: var(--text-page-title);
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

  /* Quiet until wanted: deleting an entry should be findable, not inviting.
     A pill, small, like every other button. */
  .delete {
    display: flex;
    align-items: center;
    gap: 5px;
    height: 22px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-pill);
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

  /* The same, for the pane with no note open: an empty state centred in
     half the window reads as a layout that has gone wrong. */
  .empty {
    display: flex;
    flex: 1;
    min-width: 0;
    height: 100%;
  }

  .conflict {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-3) var(--sp-5);
    border-bottom: 1px solid var(--border);
    background: color-mix(in oklab, var(--accent) 12%, var(--bg-raised));
    color: var(--fg);
    font-size: var(--text-sm);
  }

  /* The slim line above a call's note while the pipeline is still filling
     it in -- the same spinner and failed colouring `RecordingCard` has. */
  .draft {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    margin-bottom: var(--sp-3);
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }
  .draft.failed {
    border-color: color-mix(in oklab, var(--danger) 30%, var(--border));
    background: color-mix(in oklab, var(--danger) 6%, var(--bg));
    color: var(--danger);
  }
  .draft-text {
    flex: 1;
    min-width: 12rem;
    color: var(--fg-muted);
  }
  .spinner {
    flex: none;
    width: 9px;
    height: 9px;
    border-radius: 50%;
    border: 1.5px solid var(--fg-faint);
    border-top-color: var(--accent);
    animation: ky-spin 0.8s linear infinite;
  }
  @media (prefers-reduced-motion: reduce) {
    .spinner {
      animation: none;
    }
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

  /* `PurposeField` carries no margin of its own -- inside `TaskDetail` it
     sits in a label/field grid that already spaces its rows. Here, under
     the tag row, it needs the same breathing room the journal gives the
     tracker strip under its own tag row (see `.strip` in `TrackerStrip`):
     without it, the field sat flush against Pin/tags with no gap at all. */
  .purpose-row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    margin-top: var(--sp-4);
  }
  .purpose-label {
    color: var(--fg-subtle);
    font-size: var(--text-xs);
    font-weight: 550;
  }

  .pin {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    height: 22px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-pill);
    background: var(--bg-hover);
    color: var(--fg-muted);
    font-size: var(--text-xs);
  }

  .pin:hover {
    color: var(--fg);
  }

  .pin.on {
    background: var(--bg-active);
    color: var(--fg);
    font-weight: 550;
  }

  /* Where the note came from: a word, not a control, in the colour of the
     app it came from -- the sidebar's mark beside it says the same. */
  .origin {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 22px;
    margin-right: var(--sp-1);
    color: var(--fg-subtle);
    font-size: var(--text-xs);
    font-weight: 550;
  }
  .origin.assistant :global(svg) {
    color: color-mix(in oklab, var(--app-assistant) 70%, var(--fg));
  }
  .origin.meeting :global(svg) {
    color: color-mix(in oklab, var(--app-calendar) 70%, var(--fg));
  }

  /* ── On a phone ──────────────────────────────────────────────────────── */

  /* The list, and a note over it: each the whole of the page. */
  .phonehome,
  .phonepage {
    display: flex;
    flex: 1;
    min-width: 0;
    flex-direction: column;
    height: 100%;
  }
  /* A proposed note under the way back: the preview is as tall as its pane
     by its own rule, which here is the pane less the way back. */
  .phonepage :global(.editor) {
    height: auto;
    min-height: 0;
  }
  .phonehead {
    display: flex;
    flex: none;
    align-items: center;
    padding: var(--sp-1) var(--sp-2) 0 var(--sp-1);
  }
  .phonehead .grow {
    flex: 1;
  }
  .headbtn {
    display: grid;
    place-items: center;
    width: 44px;
    height: 44px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .headbtn:active {
    background: color-mix(in oklab, var(--danger) 12%, transparent);
    color: var(--danger);
  }

  /* The page is the width of the screen, and the title is a title: the
     shared rule that keeps iOS from zooming into a field would otherwise
     set it at the size of the text under it. */
  :global([data-phone]) .page {
    padding: var(--sp-3) var(--sp-4) 40vh;
  }
  :global([data-phone]) .head {
    margin-bottom: var(--sp-4);
  }
  :global([data-phone]) .title {
    display: block;
    resize: none;
    overflow: hidden;
    font-size: max(16px, var(--text-page-title));
  }
  :global([data-phone]) .status {
    height: 32px;
    padding: 0 var(--sp-4);
  }
  /* The room above the keyboard is for the writing. */
  :global(:root[data-keyboard]) .status {
    display: none;
  }

  /* Tags, pin and the offers under the title, for a finger -- and a tag's
     cross always there, since there is no hover to show it and a touch on a
     chip takes the tag off. */
  :global([data-phone]) .meta {
    gap: var(--sp-2);
  }
  :global([data-phone]) .pin {
    height: 34px;
    padding: 0 var(--sp-3);
    font-size: var(--text-sm);
    gap: 5px;
  }
  :global([data-phone]) .origin {
    height: 34px;
    font-size: var(--text-sm);
  }
  :global([data-phone]) .quick-btn {
    min-height: 34px;
    padding: 0 var(--sp-3);
    font-size: var(--text-sm);
  }
  :global([data-phone]) .conflict {
    padding: var(--sp-3) var(--sp-4);
  }
</style>
