<script lang="ts">
  // The compose sheet: From, To/Cc/Bcc as address chips, subject, a body in
  // TipTap, attachments, and the two ways a message leaves -- send now,
  // through an undo window, or send later.
  //
  // The body is TipTap, the same library `RichText.svelte` wraps for the
  // journal and for notes -- but not that component: a `Draft` stores
  // `bodyHtml`, a string `mail-builder` and `css-inline` turn into the wire
  // format on the way out, where an entry stores ProseMirror JSON.
  // `RichText.svelte`'s contract is `getJSON()`; reaching for it here would
  // mean widening a component every other app already depends on for the
  // sake of one caller. So this builds its own small `Editor`, the same way
  // `RichText.svelte` does internally, and reads `getHTML()` instead.

  import { onDestroy, untrack } from 'svelte'
  import { Editor } from '@tiptap/core'
  import StarterKit from '@tiptap/starter-kit'
  import Link from '@tiptap/extension-link'
  import Placeholder from '@tiptap/extension-placeholder'
  import { TableKit } from '@tiptap/extension-table'
  import { api } from '../lib/api'
  import { accounts } from '../lib/accounts.svelte'
  import { Autosave } from '../lib/autosave'
  import { handle } from '../lib/errors'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import * as mailApi from '../lib/mail-api'
  import { formatSenders, isBlankDraft } from '../lib/mail'
  import {
    escapeHtml,
    htmlToPlainText,
    joinQuoted,
    sendLaterChoices,
    splitQuoted,
  } from '../lib/mailwrite'
  import { mailwrite } from '../lib/mailwrite.svelte'
  import { MarkdownClipboard } from '../lib/markdown-clipboard'
  import { mail } from '../lib/mail.svelte'
  import { menu } from '../lib/menu.svelte'
  import type { MenuItem } from '../lib/menu'
  import { DECLINE_REASONS, proposals } from '../lib/proposals.svelte'
  import { timeOfDay, toLocalInputValue } from '../lib/format'
  import type { Draft, ImproveMode, MailAddress } from '../lib/types'
  import Icon from './Icon.svelte'

  /** Polish, Shorter, Longer, Friendlier, More formal, Fix -- the Improve
   *  menu's own fixed order, a mode and the label its button draws. */
  const IMPROVE_MODES: { mode: ImproveMode; label: string }[] = [
    { mode: 'polish', label: 'Polish' },
    { mode: 'shorter', label: 'Shorter' },
    { mode: 'longer', label: 'Longer' },
    { mode: 'friendlier', label: 'Friendlier' },
    { mode: 'formal', label: 'More formal' },
    { mode: 'fix', label: 'Fix spelling & grammar' },
  ]

  let {
    draft,
    onclose,
    inline = false,
  }: {
    draft: Draft
    onclose: () => void
    /**
     * Reply/Reply all/Forward, and a quick reply chosen under a thread, open
     * in the reading pane rather than a dialog over it -- `MailView.svelte`
     * passes this whenever `mail.composeInline` says so. A brand-new message
     * (`mail.compose()`, with nothing to be a reply to) is never opened this
     * way, so this stays the one prop rather than something read off `draft`
     * itself.
     */
    inline?: boolean
  } = $props()

  /**
   * A dream wrote this and asked to send it. Found by id rather than held as
   * a prop: the sheet reads `draft` once (see `working`, below), so a
   * proposal answered while it is open -- from the Assistant app's own
   * Proposals list, say -- has to be noticed some other way, and the pending
   * list is reactive where the prop is not.
   */
  const proposal = $derived(
    proposals
      .forKind('mail')
      .find((p) => p.payload.type === 'sendMail' && p.payload.draftId === draft.id) ?? null,
  )
  const proposalBusy = $derived(proposal ? proposals.isBusy(proposal.id) : false)
  let choosingReason = $state(false)

  $effect(() => {
    if (proposal) void proposals.markSeen([proposal])
  })

  /**
   * A working copy, so a keystroke redraws chips without a round trip, and
   * `saveDraft` is only ever sent a snapshot.
   *
   * Deliberately reads `draft` once, `untrack`ed: this sheet opens on a
   * fresh `Draft` every time -- `mail.compose`/`reply`/`forward` each mint
   * one, and `undoSend` hands back a new component instance along with it,
   * never a live prop update to an instance already open -- so there is
   * nothing later in `draft` this component is meant to follow.
   */
  let working = $state<Draft>(untrack(() => ({ ...draft })))

  let toText = $state('')
  let ccText = $state('')
  let bccText = $state('')
  let showCcBcc = $state(working.cc.length > 0 || working.bcc.length > 0)
  let suggestions = $state<MailAddress[]>([])
  let suggestingField: 'to' | 'cc' | 'bcc' | null = $state(null)
  /** Guards `addressField`'s await -- see that function's own note. */
  let addressGeneration = 0

  let sendingLater = $state(false)
  /** Within the send-later popover: the fixed presets, or the date/time
   *  field behind "Pick date & time…". Reset to the presets every time the
   *  popover reopens, so it never comes back stuck on the picker. */
  let pickingDateTime = $state(false)
  let sendAt = $state(toLocalInputValue(new Date(Date.now() + 3_600_000)))

  // ── inline, in the reading pane, rather than a dialog over it ──────

  /** This sheet's own root element -- not `host` (the editor's own mount
   *  point): `onKeydown` needs to know whether focus is anywhere in the
   *  *sheet*, chips and buttons included, not only in the prose itself. */
  let sheetEl = $state<HTMLDivElement>()

  /** `working.to`'s first name or address, for the inline header's "Reply
   *  to {name}" -- there is always at least one by the time a reply draft
   *  reaches here (`new_draft` fills it from the parent's `from`/`reply_to`
   *  before this ever mounts), but a forward's `to` starts empty, which is
   *  exactly the case this reads as "Forward" instead. */
  const replyToName = $derived(working.to[0]?.name || working.to[0]?.email || '')

  /** Checked before `onKeydown` acts on anything, but only when `inline`:
   *  a dialog traps focus inside itself already (`trapFocus`, below), so
   *  this is never false while one is open, but an inline reply sits in
   *  normal flow beside the rest of the app, and Escape or Mod+Enter
   *  pressed while reading a *different* thread must not reach across the
   *  pane and act on a draft nobody is looking at. */
  function focusWithinSheet(): boolean {
    return Boolean(sheetEl?.contains(document.activeElement))
  }

  /** `trapFocus` only when this is a dialog -- an inline reply is not
   *  modal, so Tab must stay free to leave it for the rest of the window. */
  function trapFocusUnlessInline(node: HTMLElement) {
    if (inline) return
    return trapFocus(node)
  }

  // ── writing help ─────────────────────────────────────────────────

  const account = $derived(accounts.account(working.accountId))
  const writingEnabled = $derived(account?.mailAi?.writing === true)
  /** Why the sparkle and Improve buttons are disabled, when they are --
   *  read as `title`, so a mouse finds out why without opening Settings. */
  const writingDisabledReason = 'Turn on Writing help in Settings → Accounts'

  /** The sparkle prompt row -- closed by default, except when
   *  `MailQuickReplies`' quiet "Write with AI…" chip asked for it to open
   *  the moment this sheet mounts. Read once, `untrack`ed, for the same
   *  reason `working` itself is: this runs once per fresh compose instance,
   *  never again for a prop update there is none of. */
  let aiPromptOpen = $state(untrack(() => mailwrite.takeOpenPromptOnNextCompose()))
  let aiInstruction = $state('')
  let aiBusy = $state(false)
  let improving = $state(false)
  /** The last AI change's own previous HTML, for the "Rewritten · Undo"
   *  note -- cleared the moment Undo is clicked, not on the next keystroke:
   *  nothing else here asks "is this note still accurate", so it is left up
   *  until the person dismisses it themselves. */
  let aiNote = $state<{ previousHtml: string } | null>(null)

  const saver = new Autosave<string>(async () => {
    await mailApi.saveDraft($state.snapshot(working))
  })

  /**
   * `syncBody()` first: this is TipTap's `onUpdate` handler as well as every
   * other field's, and `saver`'s write is `saveDraft($state.snapshot(working))`
   * -- a snapshot taken whenever the debounce timer fires, not whenever this
   * runs. Without pulling the editor's own HTML into `working.bodyHtml`
   * *here*, every autosave persisted whatever `working.bodyHtml` was seeded
   * with when the sheet opened (blank, or a quoted reply) and never a
   * keystroke that had been typed since.
   */
  function touch() {
    syncBody()
    working.updatedAt = new Date().toISOString()
    saver.touch(working.id)
  }

  /**
   * Generation-guarded the way `mail.svelte.ts`'s own loads are: a keystroke
   * fires this on every change, and a slow answer for an old prefix must not
   * overwrite a newer one that already landed.
   */
  async function addressField(text: string, field: 'to' | 'cc' | 'bcc') {
    suggestingField = field
    const gen = ++addressGeneration
    const results = text.trim() ? await mailApi.suggestAddresses(text.trim()) : []
    if (gen !== addressGeneration) return
    // Never offer an address already a chip in this field -- accepting it
    // would add a second `{a.email}`-keyed row, which is Bug 7's crash.
    const already = new Set(working[field].map((a) => a.email.trim().toLowerCase()))
    suggestions = results.filter((s) => !already.has(s.email.trim().toLowerCase()))
  }

  /** Adds `address` to `field`, unless it is already there -- case-insensitively,
   *  since `{#each working.to as a (a.email)}` keys chips by address and a
   *  second copy of the same one is Svelte's `each_key_duplicate` at runtime,
   *  not a harmless-looking duplicate chip. */
  function addAddress(field: 'to' | 'cc' | 'bcc', address: MailAddress) {
    const email = address.email.trim().toLowerCase()
    const already = working[field].some((a) => a.email.trim().toLowerCase() === email)
    if (!already) {
      working[field] = [...working[field], address]
      touch()
    }
    if (field === 'to') toText = ''
    if (field === 'cc') ccText = ''
    if (field === 'bcc') bccText = ''
    suggestions = []
  }

  function removeAddress(field: 'to' | 'cc' | 'bcc', email: string) {
    working[field] = working[field].filter((a) => a.email !== email)
    touch()
  }

  /** Enter (or a comma) on a bare address turns typed text into a chip
   *  without waiting for a suggestion to be clicked. */
  function commitTyped(field: 'to' | 'cc' | 'bcc', text: string) {
    const email = text.trim().replace(/,$/, '')
    if (!email) return
    addAddress(field, { name: '', email })
  }

  // ── the editor ───────────────────────────────────────────────────

  let host = $state<HTMLDivElement>()
  let editor: Editor | null = null
  // Nothing the editor reads back as content: a `font-weight` here would
  // come back as bold on every header, and a `text-align` as the column's
  // alignment, the next time the draft is opened.
  const CELL = 'border: 1px solid #c8ccd1; padding: 4px 8px; vertical-align: top'

  $effect(() => {
    const el = host
    if (!el) return
    const ed = new Editor({
      element: el,
      extensions: [
        // StarterKit carries a link of its own; this one is configured below.
        StarterKit.configure({ heading: false, link: false }),
        Placeholder.configure({ placeholder: 'Write something…' }),
        Link.configure({
          openOnClick: true,
          autolink: true,
          protocols: ['http', 'https', 'mailto'],
        }),
        // For the table a pasted note or answer brings with it, which would
        // otherwise run its cells together into one line. Styled inline,
        // because the HTML is sent as it is and most mail clients drop a
        // stylesheet; no fill, so it reads the same light or dark.
        TableKit.configure({
          table: { resizable: false, HTMLAttributes: { style: 'border-collapse: collapse' } },
          tableHeader: { HTMLAttributes: { style: CELL } },
          tableCell: { HTMLAttributes: { style: CELL } },
        }),
        // Markdown out on a copy, and in on a paste. A message has no
        // headings and no boxes to tick, so those arrive as a bold line and
        // as `[x]`.
        MarkdownClipboard.configure({ taskLists: false, headings: 0 }),
      ],
      content: working.bodyHtml,
      editorProps: { attributes: { class: 'ed-content', spellcheck: 'true' } },
      // Inline, a reply opens straight into the body rather than the To
      // field it already knows -- `'start'` lands the caret before the
      // quote, which is exactly where the person's own text belongs.
      // Anything else (a dialog, or a forward still waiting on a
      // recipient) keeps TipTap's own default of not stealing focus.
      autofocus: inline && working.inReplyTo ? 'start' : false,
      onUpdate: () => touch(),
    })
    editor = ed
    return () => {
      working.bodyHtml = ed.getHTML()
      ed.destroy()
      editor = null
    }
  })

  function syncBody() {
    if (editor) working.bodyHtml = editor.getHTML()
  }

  // ── writing help ─────────────────────────────────────────────────
  //
  // `draft_with_ai` and `improve_writing` both answer a `WrittenText` meant
  // to replace the person's *own* part of the body, never the quote beneath
  // it -- `splitQuoted`/`joinQuoted` (`mailwrite.ts`) are what keep the two
  // apart. Every replacement below goes through `editor.commands.setContent`
  // rather than a bare `working.bodyHtml = ...`: that is a ProseMirror
  // transaction like any other, so it lands on the same undo stack Mod+Z
  // already walks (`@tiptap/extensions`' `UndoRedo`, wired in by
  // `StarterKit`) and fires the same `onUpdate` a keystroke would, which is
  // what autosaves it and puts it in place before `send()` ever reads
  // `working.bodyHtml`.

  /** The own part of the current body, as plain text -- `currentText` for
   *  `draft_with_ai`, and `text` for a whole-body `improve_writing`. */
  function ownText(): string {
    syncBody()
    return htmlToPlainText(splitQuoted(working.bodyHtml).own)
  }

  /** Replaces the own part of the body with `ownHtml`, keeping the quote
   *  below exactly as it was. Returns the HTML it replaced, for the
   *  "Rewritten · Undo" note. */
  function replaceOwnPart(ed: Editor, ownHtml: string): string {
    const previousHtml = ed.getHTML()
    const { quoted } = splitQuoted(previousHtml)
    ed.commands.setContent(joinQuoted(ownHtml, quoted))
    return previousHtml
  }

  /** Restores the exact HTML an AI change replaced -- not `editor.undo()`,
   *  which would instead undo whatever is now on top of the history stack
   *  if the person kept typing after the change this note is about. */
  function undoAiChange() {
    if (!editor || !aiNote) return
    editor.commands.setContent(aiNote.previousHtml)
    aiNote = null
  }

  function closeAiPrompt() {
    aiPromptOpen = false
    aiInstruction = ''
  }

  async function submitAiPrompt() {
    const instruction = aiInstruction.trim()
    if (!instruction || aiBusy || !writingEnabled) return
    aiBusy = true
    try {
      const result = await mailApi.draftWithAi({
        account: working.accountId,
        inReplyTo: working.inReplyTo ?? null,
        instruction,
        currentText: ownText() || null,
      })
      const ed = editor
      if (ed) aiNote = { previousHtml: replaceOwnPart(ed, result.bodyHtml) }
      closeAiPrompt()
    } catch (e) {
      await handle(e)
    } finally {
      aiBusy = false
    }
  }

  /**
   * Polish/Shorter/.../Fix: rewrites the current selection in place when
   * there is one -- read and replaced as plain text, since a selection is
   * inline within one paragraph and a `<p>` in the middle of it would split
   * the block the selection never asked to leave -- otherwise the whole own
   * part, the same path `submitAiPrompt` takes.
   */
  async function applyImprove(mode: ImproveMode) {
    const ed = editor
    if (!ed || improving || !writingEnabled) return
    const { from, to, empty } = ed.state.selection
    const selected = empty ? '' : ed.state.doc.textBetween(from, to, '\n\n')
    const useSelection = selected.trim().length > 0
    const text = useSelection ? selected : ownText()
    if (!text.trim()) return
    improving = true
    try {
      const result = await mailApi.improveWriting({ account: working.accountId, text, mode })
      if (useSelection) {
        const previousHtml = ed.getHTML()
        const inlineHtml = escapeHtml(result.bodyText).replace(/\n+/g, '<br>')
        ed.chain().focus().insertContentAt({ from, to }, inlineHtml).run()
        aiNote = { previousHtml }
      } else {
        aiNote = { previousHtml: replaceOwnPart(ed, result.bodyHtml) }
      }
    } catch (e) {
      await handle(e)
    } finally {
      improving = false
    }
  }

  function improveMenuItems(): MenuItem[] {
    return IMPROVE_MODES.map(({ mode, label }) => ({
      label,
      disabled: improving,
      run: () => void applyImprove(mode),
    }))
  }

  function openImproveMenu(e: MouseEvent) {
    if (!writingEnabled || improving) return
    const box = (e.currentTarget as HTMLElement).getBoundingClientRect()
    menu.showAt(box.left, box.bottom + 4, improveMenuItems())
  }

  // ── attachments ──────────────────────────────────────────────────

  async function onFiles(files: FileList | null) {
    if (!files) return
    // The same blob-upload path notes and journal entries use --
    // `api.putBlob` -- producing the real `DraftAttachment` shape
    // (`types.ts`) rather than the richer `Attachment` those two domains
    // carry: a draft's attachment is a blob, a filename and a MIME type,
    // nothing else, because `mail-builder` is what turns it into a MIME
    // part on the way out.
    for (const file of Array.from(files)) {
      const blob = await api.putBlob(new Uint8Array(await file.arrayBuffer()))
      working.attachments = [
        ...working.attachments,
        { blob, filename: file.name, mimeType: file.type || 'application/octet-stream' },
      ]
    }
    touch()
  }

  function removeAttachment(blob: string) {
    working.attachments = working.attachments.filter((a) => a.blob !== blob)
    touch()
  }

  // ── sending ──────────────────────────────────────────────────────
  //
  // The undo window itself lives in `mail.svelte.ts`'s `send`/`undoSend` --
  // see that file's own note on why: this sheet is unmounted the moment
  // `onclose()` runs, and a toast whose state lived here would never draw a
  // frame.

  /** Seconds the undo banner stays open. Mirrors the plan's "five to thirty
   *  seconds" outbox window; the middle of that range. */
  const UNDO_WINDOW_S = 8

  /**
   * `sendAt`, given, is "send later": an ISO instant `mail.send` forwards
   * verbatim to `sendDraft`, which the backend then uses exactly as given
   * (`SendDraft::send_at` in `mail.rs`) rather than the undo-send window.
   * Absent, this is an ordinary send with the fixed undo window below.
   *
   * Commits whatever is still sitting, typed but unconfirmed, in each
   * address field first -- Enter/comma is what ordinarily turns typed text
   * into a chip, and a person who types an address and clicks Send without
   * pressing either would otherwise lose it silently, sending to whoever
   * *was* already committed. `commitTyped` no-ops on an empty field.
   */
  async function send(scheduledAt?: string) {
    commitTyped('to', toText)
    commitTyped('cc', ccText)
    commitTyped('bcc', bccText)
    syncBody()
    await saver.flush()
    onclose()
    // Not awaited: `mail.send` handles its own failure (a toast, via
    // `handle`) now that this sheet is already gone, so there is nothing
    // left here to await it for.
    void mail.send($state.snapshot(working), scheduledAt ? undefined : UNDO_WINDOW_S, scheduledAt)
  }

  async function sendLater() {
    await send(new Date(sendAt).toISOString())
  }

  /**
   * Accept the proposal: flush whatever was edited here first -- a typo
   * fixed before sending is still the person's own change, saved through the
   * ordinary draft autosave rather than `edited(...)`, since a `sendMail`
   * proposal carries only the draft's id and never a record to overwrite it
   * with -- then let `accept` hand the draft to the outbox exactly as an
   * ordinary send would.
   */
  async function acceptProposal() {
    const p = proposal
    if (!p) return
    commitTyped('to', toText)
    commitTyped('cc', ccText)
    commitTyped('bcc', bccText)
    syncBody()
    await saver.flush()
    const closed = await proposals.accept(p)
    if (closed) onclose()
  }

  /** Decline, but leave the draft and the sheet exactly as they were --
   *  declining a proposal to send is not a request to throw the draft away. */
  async function declineProposal(reason: (typeof DECLINE_REASONS)[number]['reason'] | null) {
    const p = proposal
    if (!p) return
    choosingReason = false
    await proposals.decline(p, reason)
  }

  async function discard() {
    syncBody()
    if (isBlankDraft(working)) {
      await mailApi.discardDraft(working.id)
      // The draft above no longer exists to write to. Without this,
      // `onDestroy`'s safety-net `flush()` still finds `working.id` dirty
      // from whatever `touch()` ran before this blank check, and writes
      // `saveDraft` for a draft that was just discarded -- resurrecting it
      // locally, and, once that write reaches the outbox, on the server too.
      // `forget()` is `Autosave`'s own answer to "the record it would write
      // is already gone" -- the same call `notes.svelte.ts`'s `remove()`
      // makes for the same reason -- so there is nothing left for that
      // safety net to do.
      saver.forget(working.id)
    } else {
      await saver.flush()
    }
    onclose()
  }

  function onKeydown(e: KeyboardEvent) {
    // An inline reply is not modal: it sits in the reading pane beside
    // whatever else is on screen, so a key meant for the rest of the
    // window -- `j`/`k` over a different thread, another app's own
    // shortcut -- must pass straight through rather than being read as
    // this draft's. A dialog never fails this check while it is open:
    // `trapFocus` keeps focus inside it the whole time.
    if (inline && !focusWithinSheet()) return
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
      e.preventDefault()
      void (proposal ? acceptProposal() : send())
    }
    // Not "Mod+J": `shortcuts.svelte.ts`'s own "the next app" only keys off
    // `anywhere()`, which a *dialog*'s `aria-modal="true"` defeats -- but an
    // inline reply adds no such attribute, and that file's window listener
    // was registered long before this component ever mounts, so it would
    // already have switched apps before this handler got a chance to call
    // `preventDefault()`. Not "Mod+I" either: `@tiptap/extension-italic`
    // (StarterKit, below) binds that inside the prose itself, which is
    // exactly where focus sits the moment an inline reply opens. "Mod+G" is
    // bound by neither table nor any extension in `extensions`, below, in
    // any mode.
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'g') {
      e.preventDefault()
      if (writingEnabled) aiPromptOpen = !aiPromptOpen
    }
    if (e.key === 'Escape') {
      // Inline, Escape is not a way out: there is no scrim it would
      // otherwise match, and a reply sitting quietly in the reading pane
      // must not vanish because the reader pressed Escape to back out of
      // something else entirely. Only the close button -- `discard()`,
      // same as it is here -- removes it.
      if (inline) return
      // Not a bare `onclose()`: that dropped whatever autosave had not yet
      // written, per Bug 1. `discard()` is exactly what the close button and
      // the scrim already do -- flush a draft worth keeping, delete a blank
      // one -- so Escape stops being the one way out of this sheet that
      // loses text.
      e.preventDefault()
      void discard()
    }
  }

  onDestroy(() => {
    // `discard()` and `send()` above already flush -- or, on `discard()`'s
    // blank-draft branch, `forget()` -- before `onclose()` ever runs, so by
    // the time Svelte tears this down through the ordinary close paths there
    // is nothing left dirty. This is the safety net for the
    // other ways the sheet can go away -- `mail.reset()` on a lock, or
    // `undoSend()` swapping in a fresh draft instance over this one -- where
    // nothing upstream called either.
    //
    // `flush()` cannot be awaited here: `onDestroy` cannot suspend teardown.
    // It does not need to be. The write it starts does not depend on this
    // component still being mounted -- `saver` and the
    // `mailApi.saveDraft($state.snapshot(working))` closure it holds are
    // plain objects, kept alive by `Autosave`'s own promise chain (`#queue`)
    // until the write lands, the same as any other in-flight write from a
    // store nothing is currently rendering. `cancel()`, which drops pending
    // edits without writing them, stays reserved for the one case `Autosave`
    // itself says it is for: a lock, with nothing left to write to.
    void saver.flush()
  })
</script>

<svelte:window onkeydown={onKeydown} />

{#if !inline}
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div class="scrim" onclick={discard}></div>
{/if}
<div
  bind:this={sheetEl}
  class={inline ? 'inline-compose' : 'sheet compose'}
  role={inline ? undefined : 'dialog'}
  aria-modal={inline ? undefined : true}
  aria-label="Compose"
  use:trapFocusUnlessInline
>
  <div class="head" class:inline-head={inline}>
    {#if inline}
      <h2>{working.inReplyTo ? `Reply to ${replyToName}` : 'Forward'}</h2>
      {#if working.to.length > 0}
        <span class="inline-to">To: {formatSenders(working.to, 3)}</span>
      {/if}
    {:else}
      <h2>{working.subject || 'New message'}</h2>
    {/if}
    {#if proposal}
      <span class="assistant-mark proposal-mark" title={proposal.why || proposal.caption}
        ><Icon name="sparkle" size={12} /> Proposed to send — review before accepting</span
      >
    {:else if working.origin.type === 'assistant'}
      <span class="assistant-mark"
        ><Icon name="sparkle" size={12} /> Drafted by the assistant — review before sending</span
      >
    {/if}
    <button class="close" aria-label="Close" onclick={discard}
      ><Icon name="close" size={15} /></button
    >
  </div>

  <div class="field">
    <span class="label">To</span>
    <div class="chips">
      {#each working.to as a (a.email)}
        <span class="chip"
          >{a.name || a.email}<button onclick={() => removeAddress('to', a.email)}
            ><Icon name="close" size={10} /></button
          ></span
        >
      {/each}
      <input
        use:focusOnMount={!(inline && working.inReplyTo)}
        value={toText}
        oninput={(e) => {
          toText = e.currentTarget.value
          void addressField(toText, 'to')
        }}
        onkeydown={(e) => {
          if (e.key === 'Enter' || e.key === ',') {
            e.preventDefault()
            commitTyped('to', toText)
          }
        }}
        placeholder={working.to.length === 0 ? 'Recipients' : ''}
      />
    </div>
    {#if !showCcBcc}
      <button class="textlink" onclick={() => (showCcBcc = true)}>Cc/Bcc</button>
    {/if}
  </div>

  {#if showCcBcc}
    <div class="field">
      <span class="label">Cc</span>
      <div class="chips">
        {#each working.cc as a (a.email)}
          <span class="chip"
            >{a.name || a.email}<button onclick={() => removeAddress('cc', a.email)}
              ><Icon name="close" size={10} /></button
            ></span
          >
        {/each}
        <input
          value={ccText}
          oninput={(e) => {
            ccText = e.currentTarget.value
            void addressField(ccText, 'cc')
          }}
          onkeydown={(e) => {
            if (e.key === 'Enter' || e.key === ',') {
              e.preventDefault()
              commitTyped('cc', ccText)
            }
          }}
        />
      </div>
    </div>
    <div class="field">
      <span class="label">Bcc</span>
      <div class="chips">
        {#each working.bcc as a (a.email)}
          <span class="chip"
            >{a.name || a.email}<button onclick={() => removeAddress('bcc', a.email)}
              ><Icon name="close" size={10} /></button
            ></span
          >
        {/each}
        <input
          value={bccText}
          oninput={(e) => {
            bccText = e.currentTarget.value
            void addressField(bccText, 'bcc')
          }}
          onkeydown={(e) => {
            if (e.key === 'Enter' || e.key === ',') {
              e.preventDefault()
              commitTyped('bcc', bccText)
            }
          }}
        />
      </div>
    </div>
  {/if}

  {#if suggestingField && suggestions.length > 0}
    <div class="suggestions">
      {#each suggestions as s (s.email)}
        <button
          class="suggestion"
          onclick={() => suggestingField && addAddress(suggestingField, s)}
        >
          <span class="s-name">{s.name || s.email}</span>
          {#if s.name}<span class="s-email">{s.email}</span>{/if}
        </button>
      {/each}
    </div>
  {/if}

  <div class="field">
    <input
      class="subject"
      value={working.subject}
      oninput={(e) => {
        working.subject = e.currentTarget.value
        touch()
      }}
      placeholder="Subject"
    />
  </div>

  {#if aiPromptOpen}
    <div class="ai-prompt">
      <Icon name="sparkle" size={13} />
      <input
        use:focusOnMount
        value={aiInstruction}
        oninput={(e) => (aiInstruction = e.currentTarget.value)}
        onkeydown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault()
            void submitAiPrompt()
          }
          if (e.key === 'Escape') {
            // Not `onKeydown`'s own Escape -- that discards the whole
            // sheet, and this row closing is not that.
            e.preventDefault()
            e.stopPropagation()
            closeAiPrompt()
          }
        }}
        placeholder="What should it say? e.g. yes to Thursday, ask for the agenda"
        disabled={aiBusy}
      />
      <button
        class="ai-icon-btn"
        aria-label="Write"
        disabled={aiBusy || !aiInstruction.trim()}
        onclick={() => void submitAiPrompt()}
      >
        <Icon name="check" size={13} />
      </button>
      <button
        class="ai-icon-btn"
        aria-label="Close the prompt"
        disabled={aiBusy}
        onclick={closeAiPrompt}
      >
        <Icon name="close" size={12} />
      </button>
    </div>
  {/if}

  {#if aiNote}
    <p class="ai-note">
      Rewritten
      <button class="textlink" onclick={undoAiChange}>Undo</button>
    </p>
  {/if}

  <div class="prose" bind:this={host}></div>

  {#if working.attachments.length > 0}
    <div class="attachments">
      {#each working.attachments as a (a.blob)}
        <span class="chip"
          >{a.filename}<button onclick={() => removeAttachment(a.blob)}
            ><Icon name="close" size={10} /></button
          ></span
        >
      {/each}
    </div>
  {/if}

  <div class="foot">
    <label class="attach">
      <Icon name="plus" size={14} />
      <input type="file" multiple hidden onchange={(e) => void onFiles(e.currentTarget.files)} />
    </label>
    <button
      class="attach"
      class:active={aiPromptOpen}
      aria-label="Write with AI"
      title={writingEnabled ? 'Write with AI (Mod+G)' : writingDisabledReason}
      disabled={!writingEnabled}
      onclick={() => (aiPromptOpen = !aiPromptOpen)}
    >
      <Icon name="sparkle" size={14} />
    </button>
    <button
      class="btn"
      title={writingEnabled ? 'Improve what you wrote' : writingDisabledReason}
      disabled={!writingEnabled || improving}
      onclick={openImproveMenu}
    >
      <Icon name="pencil" size={13} />
      {improving ? 'Improving…' : 'Improve'}
    </button>
    <span class="spacer"></span>
    {#if proposal}
      <!-- A dream's send, not the person's own: the two ordinary send paths
           give way to accept and decline, the same pair every ghost in the
           app offers, because a proposal answered from in here is still
           answered through `proposals.accept`/`decline` and not `mail.send`. -->
      <div class="decline-wrap">
        <button
          class="btn"
          disabled={proposalBusy}
          onclick={() => (choosingReason = !choosingReason)}
        >
          Decline
        </button>
        {#if choosingReason}
          <div class="reasons" role="menu">
            <button role="menuitem" onclick={() => declineProposal(null)}>Just decline</button>
            {#each DECLINE_REASONS as r (r.label)}
              <button role="menuitem" onclick={() => declineProposal(r.reason)}>{r.label}</button>
            {/each}
          </div>
        {/if}
      </div>
      <button class="btn btn-primary" disabled={proposalBusy} onclick={() => void acceptProposal()}>
        Send <span class="hint">(Mod+Enter)</span>
      </button>
    {:else}
      <div class="later-wrap">
        <button
          class="btn"
          onclick={() => {
            sendingLater = !sendingLater
            pickingDateTime = false
          }}
        >
          Send later
        </button>
        {#if sendingLater}
          <div class="later-pop" role="menu">
            {#if pickingDateTime}
              <input
                type="datetime-local"
                bind:value={sendAt}
                min={toLocalInputValue(new Date())}
              />
              <button class="btn" onclick={() => void sendLater()}>Schedule</button>
            {:else}
              {#each sendLaterChoices() as choice (choice.key)}
                <button role="menuitem" onclick={() => void send(choice.at.toISOString())}>
                  <span>{choice.label}</span>
                  <span class="when">{timeOfDay(choice.at)}</span>
                </button>
              {/each}
              <button role="menuitem" onclick={() => (pickingDateTime = true)}>
                Pick date &amp; time…
              </button>
            {/if}
          </div>
        {/if}
      </div>
      <button class="btn btn-primary" onclick={() => void send()}>
        Send <span class="hint">(Mod+Enter)</span>
      </button>
    {/if}
  </div>
</div>

<style>
  .compose {
    width: 620px;
    max-width: calc(100vw - var(--sp-8));
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  /* In normal flow, not over it: no `position`, no `z-index`, a width that
     follows its parent rather than naming one of its own -- the opposite of
     every rule `.sheet`/`.compose` set for a reason that was always "this
     floats over the window", which an inline reply does not. */
  .inline-compose {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    width: 100%;
    padding: var(--sp-3) var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
  }
  .head {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .head h2 {
    flex: 1;
    min-width: 0;
    font-size: var(--text-md);
    font-weight: 620;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* The compact header: the title names what this is rather than growing to
     fill the row, so there is still room for the recipients line beside it. */
  .head.inline-head h2 {
    flex: none;
  }
  .inline-to {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .assistant-mark {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    padding: 2px var(--sp-2);
    border-radius: 999px;
    background: var(--bg-hover);
    color: var(--accent);
    font-size: var(--text-xs);
  }
  .proposal-mark {
    border: 1px dashed color-mix(in oklab, var(--accent) 55%, var(--border));
    background: color-mix(in oklab, var(--accent) 8%, transparent);
  }
  .close {
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .close:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .field {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    border-bottom: 1px solid var(--border);
    padding: var(--sp-1) 0;
  }
  .label {
    flex: none;
    width: 36px;
    font-size: var(--text-sm);
    color: var(--fg-faint);
  }
  .chips {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    align-items: center;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px var(--sp-2);
    border-radius: 999px;
    background: var(--bg-hover);
    font-size: var(--text-sm);
  }
  .chip button {
    display: grid;
    place-items: center;
    color: var(--fg-faint);
  }
  .chips input,
  .subject {
    flex: 1;
    min-width: 80px;
    border: 0;
    background: none;
    color: var(--fg);
    font-size: var(--text-base);
  }
  .chips input:focus,
  .subject:focus {
    outline: none;
  }
  .subject {
    width: 100%;
  }
  .textlink {
    flex: none;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .textlink:hover {
    color: var(--fg);
  }

  .suggestions {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    max-height: 160px;
    overflow-y: auto;
  }
  .suggestion {
    display: flex;
    gap: var(--sp-2);
    padding: var(--sp-1) var(--sp-2);
    text-align: left;
  }
  .suggestion:hover {
    background: var(--bg-hover);
  }
  .s-email {
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }

  .prose {
    min-height: 140px;
    max-height: 40vh;
    overflow-y: auto;
    padding: var(--sp-2) 0;
  }
  .prose :global(.ed-content) {
    outline: none;
    font-size: var(--text-base);
    line-height: var(--leading-normal);
  }
  /* The message being answered, quoted under the reply: set apart the way
     mail clients draw a quote, so it never reads as the person's own words. */
  .prose :global(blockquote) {
    margin: var(--sp-2) 0;
    padding-left: var(--sp-3);
    border-left: 2px solid var(--border);
    color: var(--fg-muted);
  }
  .prose :global(table) {
    margin: var(--sp-2) 0;
  }
  .prose :global(td p),
  .prose :global(th p) {
    margin: 0;
  }
  .prose :global(.selectedCell) {
    background: color-mix(in oklab, var(--accent) 14%, transparent);
  }
  .prose :global(p.is-editor-empty:first-child::before) {
    content: attr(data-placeholder);
    float: left;
    height: 0;
    pointer-events: none;
    color: var(--fg-faint);
  }

  .attachments {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }

  .foot {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding-top: var(--sp-2);
    border-top: 1px solid var(--border);
  }
  .attach {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .attach:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .attach.active {
    background: color-mix(in oklab, var(--accent) 14%, transparent);
    color: var(--journal-accent, var(--accent));
  }
  .attach:disabled {
    opacity: 0.5;
  }
  .foot .hint {
    opacity: 0.7;
    font-size: var(--text-xs);
  }
  .foot input[type='datetime-local'] {
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 4px var(--sp-2);
    background: var(--bg-raised);
    color: var(--fg);
  }

  .ai-prompt {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-1) var(--sp-2);
    border: 1px solid color-mix(in oklab, var(--accent) 40%, var(--border));
    border-radius: var(--radius-sm);
    background: color-mix(in oklab, var(--accent) 6%, transparent);
    color: var(--journal-accent, var(--accent));
  }
  .ai-prompt input {
    flex: 1;
    min-width: 0;
    border: 0;
    background: none;
    color: var(--fg);
    font-size: var(--text-sm);
  }
  .ai-prompt input:focus {
    outline: none;
  }
  .ai-icon-btn {
    display: grid;
    place-items: center;
    flex: none;
    width: 22px;
    height: 22px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .ai-icon-btn:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .ai-icon-btn:disabled {
    opacity: 0.5;
  }
  .ai-note {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    margin: 0;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .ai-note .textlink {
    color: var(--journal-accent, var(--accent));
  }

  .decline-wrap {
    position: relative;
  }
  .later-wrap {
    position: relative;
  }
  .later-pop {
    position: absolute;
    right: 0;
    bottom: 100%;
    z-index: 20;
    display: flex;
    flex-direction: column;
    min-width: 200px;
    margin-bottom: 4px;
    padding: var(--sp-1);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    box-shadow: var(--shadow);
  }
  .later-pop button[role='menuitem'] {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2);
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
  .later-pop button[role='menuitem']:hover {
    background: var(--bg-hover);
  }
  .later-pop .when {
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }
  .later-pop input[type='datetime-local'] {
    margin-bottom: var(--sp-1);
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
