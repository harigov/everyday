<script lang="ts">
  // The compose sheet: From, To/Cc/Bcc as address chips, subject, a body in
  // TipTap, attachments, and the two ways a message leaves -- send now,
  // through an undo window, or send later. Never a dialog: it opens in the
  // reading pane, where a message is read, either filling it (a new message,
  // or a draft reopened) or under the thread it answers -- see `placement`.
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
  import { focusOnMount } from '../lib/focus'
  import * as mailApi from '../lib/mail-api'
  import { estimateQuoteHeight, formatSenders, isBlankDraft, parseTypedAddress } from '../lib/mail'
  import {
    escapeHtml,
    htmlToPlainText,
    joinQuoted,
    sendLaterChoices,
    splitQuoted,
  } from '../lib/mailwrite'
  import { mailwrite } from '../lib/mailwrite.svelte'
  import { quoteDocument } from '../lib/mailview'
  import { MarkdownClipboard } from '../lib/markdown-clipboard'
  import { mail } from '../lib/mail.svelte'
  import { notify } from '../lib/notify.svelte'
  import { menu } from '../lib/menu.svelte'
  import type { MenuItem } from '../lib/menu'
  import { DECLINE_REASONS, proposals } from '../lib/proposals.svelte'
  import { app } from '../lib/state.svelte'
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
    placement = 'pane',
  }: {
    draft: Draft
    onclose: () => void
    /**
     * Where in the reading pane the sheet sits. `'thread'`: under the open
     * thread's messages -- Reply, Reply all, Forward, a suggested reply --
     * whenever `mail.composeInline` says so. `'pane'`: the whole pane, in
     * place of a thread -- a new message (`mail.compose()`), or a draft
     * reopened with no open thread to sit under. A prop rather than read
     * off `draft`, since a reply reopened from the Scheduled list fills the
     * pane all the same.
     */
    placement?: 'pane' | 'thread'
  } = $props()
  const underThread = $derived(placement === 'thread')

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

  /**
   * The quoted parent of a reply or forward -- "On … wrote:" and the
   * message under it -- held apart from the editor for the life of the
   * sheet, and joined back on behind the person's own words whenever the
   * body is read (`syncBody`).
   *
   * Not in the editor, because the editor only keeps what its own schema
   * knows: an HTML email's layout tables, images and colours came out of
   * it mangled, and the person's caret landed inside somebody else's
   * message. Out here it is shown as it was sent, in a sandboxed frame
   * (`quoteDocument`), folded away behind "•••" the way Gmail and
   * Superhuman fold theirs, and sent exactly as it was quoted.
   */
  const initialBody = untrack(() => splitQuoted(working.bodyHtml))
  const quotedHtml = initialBody.quoted
  let showQuote = $state(false)

  function isDarkMode(): boolean {
    if (app.theme === 'dark') return true
    if (app.theme === 'light') return false
    return window.matchMedia('(prefers-color-scheme: dark)').matches
  }
  const quoteSrcdoc = $derived(quotedHtml ? quoteDocument(quotedHtml, isDarkMode()) : '')

  /** Sizes the quote's frame from its HTML, the way `MailThread` sizes a
   *  message's: a sandboxed frame cannot be measured from out here. */
  function sizeQuote(node: HTMLElement) {
    const frame = node.querySelector('iframe')
    function apply() {
      if (frame) frame.style.height = `${estimateQuoteHeight(quotedHtml, node.clientWidth)}px`
    }
    apply()
    const ro = new ResizeObserver(apply)
    ro.observe(node)
    return { destroy: () => ro.disconnect() }
  }

  let toText = $state('')
  let ccText = $state('')
  let bccText = $state('')
  let showCcBcc = $state(working.cc.length > 0 || working.bcc.length > 0)
  let suggestions = $state<MailAddress[]>([])
  let suggestingField: 'to' | 'cc' | 'bcc' | null = $state(null)
  /** The suggestion Enter or Tab would take -- moved by the arrow keys. */
  let activeSuggestion = $state(0)
  /** Guards `addressField`'s await -- see that function's own note. */
  let addressGeneration = 0

  let sendingLater = $state(false)
  /** Within the send-later popover: the fixed presets, or the date/time
   *  field behind "Pick date & time…". Reset to the presets every time the
   *  popover reopens, so it never comes back stuck on the picker. */
  let pickingDateTime = $state(false)
  let sendAt = $state(toLocalInputValue(new Date(Date.now() + 3_600_000)))

  // ── in the reading pane, beside the rest of the app ─────────────

  /** This sheet's own root element -- not `host` (the editor's own mount
   *  point): `onKeydown` needs to know whether focus is anywhere in the
   *  *sheet*, chips and buttons included, not only in the prose itself. */
  let sheetEl = $state<HTMLDivElement>()

  /** `working.to`'s first name or address, for the thread header's "Reply
   *  to {name}" -- there is always at least one by the time a reply draft
   *  reaches here (`new_draft` fills it from the parent's `from`/`reply_to`
   *  before this ever mounts), but a forward's `to` starts empty, which is
   *  exactly the case this reads as "Forward" instead. */
  const replyToName = $derived(working.to[0]?.name || working.to[0]?.email || '')

  /** Checked before `onKeydown` acts on anything: the sheet is not modal,
   *  it sits in normal flow beside the list and the rest of the app, and
   *  Mod+Enter pressed while focus is somewhere else must not reach across
   *  and send a draft nobody is looking at. */
  function focusWithinSheet(): boolean {
    return Boolean(sheetEl?.contains(document.activeElement))
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
  /** Has anything been changed here -- what "Draft saved" waits for, so
   *  putting aside a draft somebody only looked at (the assistant's own,
   *  opened under its thread) says nothing. */
  let edited = false

  function touch() {
    edited = true
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
    activeSuggestion = 0
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
   *  without waiting for a suggestion to be clicked. Several pasted at once
   *  -- `a@x.com, b@y.com` -- become a chip each; one written as
   *  `Ann Lee <ann@x.com>` keeps its name. */
  function commitTyped(field: 'to' | 'cc' | 'bcc', text: string) {
    const parts = text.includes('<') ? [text] : text.split(/[,;]/)
    for (const part of parts) {
      const address = parseTypedAddress(part)
      if (address) addAddress(field, address)
    }
  }

  function typedIn(field: 'to' | 'cc' | 'bcc'): string {
    return field === 'to' ? toText : field === 'cc' ? ccText : bccText
  }

  function setTyped(field: 'to' | 'cc' | 'bcc', text: string) {
    if (field === 'to') toText = text
    if (field === 'cc') ccText = text
    if (field === 'bcc') bccText = text
  }

  /**
   * An address field's keys: the arrows move through the suggestions and
   * Enter or Tab takes the highlighted one, the way Gmail's own list
   * answers; Enter, a comma or a semicolon commit what was typed when
   * nothing is suggested; Backspace in an empty field takes back the last
   * chip; Escape closes the list without reaching the sheet's own Escape.
   */
  function onAddressKeydown(e: KeyboardEvent, field: 'to' | 'cc' | 'bcc') {
    const text = typedIn(field)
    const open = suggestingField === field && suggestions.length > 0
    if (open && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
      e.preventDefault()
      const step = e.key === 'ArrowDown' ? 1 : -1
      activeSuggestion = (activeSuggestion + step + suggestions.length) % suggestions.length
      return
    }
    if (open && (e.key === 'Enter' || (e.key === 'Tab' && text.trim()))) {
      e.preventDefault()
      const chosen = suggestions[activeSuggestion]
      if (chosen) addAddress(field, chosen)
      return
    }
    if (open && e.key === 'Escape') {
      e.preventDefault()
      e.stopPropagation()
      suggestions = []
      return
    }
    if (e.key === 'Enter' || e.key === ',' || e.key === ';') {
      e.preventDefault()
      commitTyped(field, text)
      return
    }
    if (e.key === 'Backspace' && !text && working[field].length > 0) {
      e.preventDefault()
      working[field] = working[field].slice(0, -1)
      touch()
    }
  }

  /** Leaving a field with an address typed in it keeps it, as a chip --
   *  but not a half-typed name, which is still a search. The list closes a
   *  beat later, unless focus has come back to the same field: a click on
   *  a suggestion never takes focus at all (its `mousedown` is cancelled),
   *  so anywhere else focus lands -- Subject, the body -- the list goes. */
  function onAddressBlur(field: 'to' | 'cc' | 'bcc', input: HTMLInputElement) {
    const text = typedIn(field)
    if (text.includes('@')) commitTyped(field, text)
    setTimeout(() => {
      if (suggestingField === field && document.activeElement !== input) suggestions = []
    }, 150)
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
      // Only the person's own part -- see `quotedHtml`.
      content: initialBody.own,
      editorProps: { attributes: { class: 'ed-content', spellcheck: 'true' } },
      // Reply, Reply all, Forward -- and any draft that already has its
      // recipients -- open straight into the body: a fresh reply on the
      // empty first line above the folded quote, so typing can start at
      // once (a forward's recipients can follow); a draft with words of its
      // own already, after the last of them. A new message, with nobody to
      // send it to yet, leaves focus to the To field instead.
      autofocus: underThread
        ? 'start'
        : working.to.length === 0
          ? false
          : initialBody.own.replace(/<[^>]*>/g, '').trim()
            ? 'end'
            : 'start',
      onUpdate: () => touch(),
    })
    editor = ed
    return () => {
      working.bodyHtml = joinQuoted(ed.getHTML(), quotedHtml)
      ed.destroy()
      editor = null
    }
  })

  /** The editor's text, with the quote put back behind it. */
  function syncBody() {
    if (editor) working.bodyHtml = joinQuoted(editor.getHTML(), quotedHtml)
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
    // The sheet is not modal: it sits in the reading pane beside whatever
    // else is on screen, so a key meant for the rest of the window --
    // `j`/`k` over the list, another app's own shortcut -- must pass
    // straight through rather than being read as this draft's.
    if (!focusWithinSheet()) return
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
      e.preventDefault()
      void (proposal ? acceptProposal() : send())
    }
    // Not "Mod+J": `shortcuts.svelte.ts`'s own "the next app" only keys off
    // `anywhere()`, which nothing modal stands in front of here, and that
    // file's window listener was registered long before this component ever
    // mounts, so it would already have switched apps before this handler
    // got a chance to call `preventDefault()`. Not "Mod+I" either:
    // `@tiptap/extension-italic` (StarterKit, below) binds that inside the
    // prose itself, which is exactly where focus sits the moment a reply
    // opens. "Mod+G" is bound by neither table nor any extension in
    // `extensions`, below.
    //
    // No Escape: a draft sitting in the reading pane must not vanish
    // because the reader pressed Escape to back out of something else
    // entirely. Only the close button -- `discard()` -- removes it.
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'g') {
      e.preventDefault()
      if (writingEnabled) aiPromptOpen = !aiPromptOpen
    }
  }

  onDestroy(() => {
    // `discard()` and `send()` above already flush -- or, on `discard()`'s
    // blank-draft branch, `forget()` -- before `onclose()` ever runs, so by
    // the time Svelte tears this down through the ordinary close paths there
    // is nothing left dirty. This is the safety net for the
    // other ways the sheet can go away -- `mail.reset()` on a lock,
    // `undoSend()` swapping in a fresh draft instance over this one, or a
    // thread opened over a draft filling the pane (`mail.parkedDraft`) --
    // where nothing upstream called either.
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

    // Put aside rather than closed: say where it went, and offer it back --
    // when it was written in here. A blank one was never saved, and one
    // only looked at is still in Drafts exactly as it was.
    if (mail.parkedDraft === working.id) {
      mail.parkedDraft = null
      syncBody()
      if (edited && !isBlankDraft(working)) {
        const kept = $state.snapshot(working)
        notify.info('Draft saved', {
          body: kept.subject || undefined,
          key: 'mail-draft-parked',
          action: { label: 'Open', run: () => mail.openDraft(kept) },
        })
      }
    }
  })
</script>

<svelte:window onkeydown={onKeydown} />

<div
  bind:this={sheetEl}
  class={underThread ? 'thread-compose' : 'pane-compose'}
  role="region"
  aria-label="Compose"
>
  <div class="head" class:thread-head={underThread}>
    {#if underThread}
      <h2>{working.inReplyTo ? `Reply to ${replyToName}` : 'Forward'}</h2>
      {#if working.to.length > 0}
        <span class="thread-to">To: {formatSenders(working.to, 3)}</span>
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

  {#snippet addressRow(field: 'to' | 'cc' | 'bcc', label: string)}
    <div class="field address">
      <span class="label">{label}</span>
      <div class="chips">
        {#each working[field] as a (a.email)}
          <span class="chip" title={a.email}
            >{a.name || a.email}<button
              aria-label={`Remove ${a.name || a.email}`}
              onclick={() => removeAddress(field, a.email)}><Icon name="close" size={10} /></button
            ></span
          >
        {/each}
        <input
          use:focusOnMount={field === 'to' && !underThread && working.to.length === 0}
          value={typedIn(field)}
          aria-label={label}
          aria-autocomplete="list"
          aria-expanded={suggestingField === field && suggestions.length > 0}
          autocomplete="off"
          spellcheck="false"
          oninput={(e) => {
            setTyped(field, e.currentTarget.value)
            void addressField(e.currentTarget.value, field)
          }}
          onkeydown={(e) => onAddressKeydown(e, field)}
          onblur={(e) => onAddressBlur(field, e.currentTarget)}
          placeholder={field === 'to' && working.to.length === 0 ? 'Name or email address' : ''}
        />
      </div>
      {#if field === 'to' && !showCcBcc}
        <button class="textlink" onclick={() => (showCcBcc = true)}>Cc/Bcc</button>
      {/if}
      {#if suggestingField === field && suggestions.length > 0}
        <div class="suggestions" role="listbox" aria-label="Suggested people">
          {#each suggestions as s, i (s.email)}
            <button
              class="suggestion"
              class:active={i === activeSuggestion}
              role="option"
              aria-selected={i === activeSuggestion}
              onmousedown={(e) => e.preventDefault()}
              onmouseenter={() => (activeSuggestion = i)}
              onclick={() => addAddress(field, s)}
            >
              <span class="avatar" aria-hidden="true"
                >{(s.name || s.email).trim().charAt(0).toUpperCase()}</span
              >
              <span class="s-text">
                <span class="s-name">{s.name || s.email}</span>
                {#if s.name}<span class="s-email">{s.email}</span>{/if}
              </span>
            </button>
          {/each}
        </div>
      {/if}
    </div>
  {/snippet}

  {@render addressRow('to', 'To')}
  {#if showCcBcc}
    {@render addressRow('cc', 'Cc')}
    {@render addressRow('bcc', 'Bcc')}
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

  <div class="body">
    <div class="prose" class:fill={!quotedHtml} bind:this={host}></div>
    {#if quotedHtml}
      <button
        class="quote-toggle"
        class:open={showQuote}
        title={showQuote ? 'Hide the quoted message' : 'Show the quoted message'}
        aria-label={showQuote ? 'Hide the quoted message' : 'Show the quoted message'}
        aria-expanded={showQuote}
        onclick={() => (showQuote = !showQuote)}>•••</button
      >
      {#if showQuote}
        <div class="quote" use:sizeQuote>
          <iframe
            title="The quoted message"
            sandbox="allow-popups allow-popups-to-escape-sandbox"
            srcdoc={quoteSrcdoc}
          ></iframe>
        </div>
      {/if}
    {/if}
  </div>

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
    <label class="attach" title="Attach files" aria-label="Attach files">
      <Icon name="paperclip" size={15} />
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
  /* The whole reading pane, where a thread would otherwise be: a header the
     height of the thread's own, then the fields, then the body taking
     whatever height is left and scrolling inside it, the footer always in
     view at the bottom. */
  .pane-compose {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: 0 var(--sp-4) var(--sp-3);
  }
  .pane-compose > .head {
    flex: none;
    height: var(--header-h);
    margin: 0 calc(-1 * var(--sp-4));
    padding: 0 var(--sp-4);
    border-bottom: 1px solid var(--border);
  }
  /* Under the thread it answers, in its flow: a card the width of the
     messages above it. */
  .thread-compose {
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
  .head.thread-head h2 {
    flex: none;
  }
  .thread-to {
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
  /* What the suggestions hang from. */
  .field.address {
    position: relative;
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

  /* Under the field being typed in, over whatever is below it -- the
     subject, the body -- rather than pushing it all down a row at a time
     as the list fills. */
  .suggestions {
    position: absolute;
    top: calc(100% + 2px);
    left: 44px;
    z-index: 30;
    display: flex;
    flex-direction: column;
    width: min(420px, calc(100% - 44px));
    max-height: 280px;
    overflow-y: auto;
    padding: 4px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    box-shadow: var(--shadow-lg);
  }
  .suggestion {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: 6px var(--sp-2);
    border-radius: var(--radius-sm);
    text-align: left;
  }
  .suggestion.active {
    background: var(--bg-hover);
  }
  .avatar {
    flex: none;
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: 50%;
    background: color-mix(in oklab, var(--accent) 22%, var(--bg-hover));
    color: var(--fg);
    font-size: var(--text-xs);
    font-weight: 650;
  }
  .s-text {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .s-name,
  .s-email {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .s-name {
    font-size: var(--text-sm);
    color: var(--fg);
  }
  .s-email {
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }

  /* The editor and the folded quote under it. Filling the pane, this is
     what takes the height the fields leave, and what scrolls; under a
     thread, the reading pane around it already scrolls. */
  .body {
    display: flex;
    flex-direction: column;
  }
  .pane-compose .body {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .prose {
    display: flex;
    flex-direction: column;
    min-height: 120px;
    padding: var(--sp-2) 0;
  }
  /* A message quoting nothing: the editor fills the body, so a click
     anywhere in the empty space below the last line still lands in it. */
  .pane-compose .prose.fill {
    flex: 1 0 auto;
  }
  .prose :global(.ed-content) {
    flex: 1;
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

  /* Gmail's "•••": the whole quote, folded into one small pill. */
  .quote-toggle {
    align-self: flex-start;
    padding: 0 8px;
    height: 16px;
    line-height: 14px;
    border-radius: 999px;
    background: var(--bg-hover);
    color: var(--fg-muted);
    font-size: 11px;
    letter-spacing: 1px;
  }
  .quote-toggle:hover,
  .quote-toggle.open {
    background: var(--bg-active);
    color: var(--fg);
  }
  .quote {
    margin-top: var(--sp-2);
  }
  .quote iframe {
    display: block;
    width: 100%;
    border: 0;
    background: transparent;
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
  /* Send and Send later to the far end of the row, away from the tools. */
  .foot .spacer {
    flex: 1;
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
