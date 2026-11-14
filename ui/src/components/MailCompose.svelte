<script lang="ts">
  // The compose sheet: From, To/Cc/Bcc as address chips, subject, a body in
  // TipTap, attachments, and the two ways a message leaves -- send now,
  // through an undo window, or send later. Never a dialog: it opens in the
  // reading pane, where a message is read, either filling it (a new message,
  // or a draft reopened) or under the thread it answers -- see `placement`.
  // Under a thread it reads as the next message rather than a form: a line
  // saying who it is to, the words, and a row of quiet actions, with the
  // address fields and the subject folded behind that first line.
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
  import { dismissable } from '../lib/dismiss'
  import { handle } from '../lib/errors'
  import { focusOnMount } from '../lib/focus'
  import * as mailApi from '../lib/mail-api'
  import { keysLabel } from '../lib/keys'
  import {
    estimateQuoteHeight,
    formatAddresses,
    formatSenders,
    isBlankDraft,
    parseTypedAddress,
  } from '../lib/mail'
  import {
    escapeHtml,
    htmlToPlainText,
    joinQuoted,
    sendLaterChoices,
    signatureFor,
    splitQuoted,
    splitSignature,
    withOwnWords,
    withSignatureSwapped,
    withoutEmptySignature,
  } from '../lib/mailwrite'
  import { mailwrite } from '../lib/mailwrite.svelte'
  import { quoteDocument } from '../lib/mailview'
  import { MarkdownClipboard } from '../lib/markdown-clipboard'
  import { ListAfterBreak } from '../lib/list-after-break'
  import { MailPicture, picturesIn } from '../lib/mail-picture'
  import { Signature, signatureRange } from '../lib/signature'
  import { mail } from '../lib/mail.svelte'
  import { notify } from '../lib/notify.svelte'
  import { menu } from '../lib/menu.svelte'
  import type { MenuItem } from '../lib/menu'
  import { declineItems, proposals } from '../lib/proposals.svelte'
  import { app } from '../lib/state.svelte'
  import { timeOfDay, toLocalInputValue } from '../lib/format'
  import type {
    AccountView,
    DeclineReason,
    Draft,
    DraftAttachment,
    DraftId,
    ImproveMode,
    MailAddress,
  } from '../lib/types'
  import Avatar from './Avatar.svelte'
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

  const FIELD_LABELS = { to: 'To', cc: 'Cc', bcc: 'Bcc' }

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
  const reasons = Symbol('decline reasons')
  const choosingReason = $derived(menu.owner === reasons)

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

  const quoteSrcdoc = $derived(quotedHtml ? quoteDocument(quotedHtml, app.dark, app.frame) : '')

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

  /** Are To, Cc, Bcc and Subject showing? A reply already knows all four, so
   *  they start folded behind its first line -- open when there is nobody to
   *  send to yet, and whenever there is something in them to check first: a
   *  draft anything but the person wrote, recipients anything but the person
   *  changed, and a Bcc, a second From address or another account than the
   *  thread's, which the line it folds behind has no way to flag. */
  let showFields = $state(
    untrack(
      () =>
        placement === 'pane' ||
        working.to.length === 0 ||
        working.bcc.length > 0 ||
        (accounts.account(working.accountId)?.address ?? working.identity) !== working.identity ||
        (placement === 'thread' &&
          !!mail.openThread &&
          mail.openThread.thread.accountId !== working.accountId) ||
        working.origin.type !== 'person' ||
        !!working.recipientsChangedBy,
    ),
  )
  /** Unfolded by hand, which is asking to change something in there. */
  let focusFields = $state(false)

  function toggleFields() {
    focusFields = !showFields
    showFields = !showFields
  }

  /** Everybody this goes to, Bcc included: folded away, nobody should be a
   *  surprise. */
  const recipients = $derived(formatSenders([...working.to, ...working.cc, ...working.bcc], 3))
  const recipientsTitle = $derived(
    (['to', 'cc', 'bcc'] as const)
      .filter((field) => working[field].length > 0)
      .map((field) => `${FIELD_LABELS[field]}: ${formatAddresses(working[field])}`)
      .join('\n'),
  )

  /** A chord as this machine spells it, for the words on screen. */
  function keyName(keys: string): string {
    return keysLabel(keys, navigator.userAgent.includes('Mac'))
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

  /** Checked before `onKeydown` acts on anything: the sheet is not modal,
   *  it sits in normal flow beside the list and the rest of the app, and
   *  Mod+Enter pressed while focus is somewhere else must not reach across
   *  and send a draft nobody is looking at. */
  function focusWithinSheet(): boolean {
    return Boolean(sheetEl?.contains(document.activeElement))
  }

  // ── writing help ─────────────────────────────────────────────────

  const account = $derived(accounts.account(working.accountId))

  /** Who this goes out as, for the avatar beside it. */
  const sender = $derived(
    account?.identities.find((i) => i.address === working.identity)?.name ||
      account?.displayName ||
      working.identity,
  )
  /** Every address this draft can go out from, account by account: each
   *  mail account's own address and its identities', and whichever this one
   *  is from now, which may be one since removed. One address is no choice,
   *  and is not shown as one. A draft the assistant proposed sending stays on
   *  its account: moving makes a new draft, and the proposal names this one. */
  const fromGroups = $derived.by(() => {
    const seen = new Set<string>()
    const groups: { account: AccountView; addresses: string[] }[] = []
    for (const a of accounts.list) {
      if (a.id !== working.accountId && (!a.services.mail || proposal)) continue
      const here = [a.address, ...a.identities.map((i) => i.address)]
      if (a.id === working.accountId) here.push(working.identity)
      const addresses = here.filter((address) => {
        const key = `${a.id}\n${address.trim().toLowerCase()}`
        if (!address.trim() || seen.has(key)) return false
        seen.add(key)
        return true
      })
      if (addresses.length > 0) groups.push({ account: a, addresses })
    }
    return groups
  })
  const fromCount = $derived(fromGroups.reduce((n, g) => n + g.addresses.length, 0))
  /** The `<select>`'s value for an address: which account, and which of its
   *  addresses -- the same address can belong to two accounts. */
  const fromValue = (accountId: string, address: string) => `${accountId}\n${address}`

  /** While a draft is on its way to another account the sheet is inert: a
   *  keystroke then would be saved to the record being thrown away. */
  let moving = $state(false)

  /** The signature in the editor, swapped from `from`'s to `to`'s. Answers
   *  what the editor held before, to put back. */
  function swapSignature(
    from: { account: AccountView; address: string },
    to: { account: AccountView; address: string },
  ): string | null {
    const ed = editor
    if (!ed) return null
    const html = ed.getHTML()
    const swapped = withSignatureSwapped(
      html,
      signatureFor(from.account, from.address),
      signatureFor(to.account, to.address),
    )
    if (swapped !== html) ed.commands.setContent(swapped)
    return html
  }

  async function setFrom(select: HTMLSelectElement) {
    const [accountId, address] = select.value.split('\n')
    if (!account || !accountId || !address) return
    if (accountId === working.accountId && address === working.identity) return
    const target = accounts.account(accountId)
    if (!target) return
    const from = { account, address: working.identity }
    if (accountId === working.accountId) {
      swapSignature(from, { account, address })
      working.identity = address
      touch()
      return
    }
    // Another account: a new draft there, carrying what is written, and this
    // one thrown away with its copy on the old account's server.
    moving = true
    const old = working.id
    let before: string | null = null
    let moved: Draft | null = null
    try {
      // Stored as it stands, so no save of the old one is still on its way
      // to land after the move throws it away; nothing can be typed until
      // the move is done.
      saver.touch(old)
      if (!(await written())) return
      saver.forget(old)
      before = swapSignature(from, { account: target, address })
      syncBody()
      moved = await mailApi.moveDraft($state.snapshot(working), accountId, address)
      working.id = moved.id
      working.accountId = moved.accountId
      working.identity = moved.identity
      working.state = moved.state
      working.createdAt = moved.createdAt
      mail.composingMoved(moved)
      touch()
    } catch (e) {
      await handle(e)
    } finally {
      if (!moved) {
        // Still the old draft, from the old address, with its own signature.
        if (before !== null) editor?.commands.setContent(before)
        syncBody()
        saver.touch(old)
        select.value = fromValue(working.accountId, working.identity)
      }
      moving = false
    }
  }

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

  /** `email` taken out of `from` and put at the end of `to`: a chip dragged
   *  between To, Cc and Bcc, or moved from its menu. Already in `to`, it is
   *  only taken out of `from`. */
  function moveAddress(from: 'to' | 'cc' | 'bcc', to: 'to' | 'cc' | 'bcc', email: string) {
    if (from === to) return
    const address = working[from].find((a) => a.email === email)
    if (!address) return
    working[from] = working[from].filter((a) => a.email !== email)
    const lower = email.trim().toLowerCase()
    if (!working[to].some((a) => a.email.trim().toLowerCase() === lower)) {
      working[to] = [...working[to], address]
    }
    if (to !== 'to') showCcBcc = true
    touch()
  }

  /** The chip being dragged, while it is. Not read from `dataTransfer`,
   *  which a drag only lets a drop see. */
  let dragging = $state<{ field: 'to' | 'cc' | 'bcc'; email: string } | null>(null)
  /** The row a dragged chip is over, which says it will land there. */
  let dropInto = $state<'to' | 'cc' | 'bcc' | null>(null)

  function onChipDragStart(e: DragEvent, field: 'to' | 'cc' | 'bcc', email: string) {
    dragging = { field, email }
    // Somewhere to drop it: Cc and Bcc show while a chip is in the air.
    showCcBcc = true
    if (e.dataTransfer) {
      e.dataTransfer.effectAllowed = 'move'
      e.dataTransfer.setData('text/plain', email)
    }
  }

  function onRowDragOver(e: DragEvent, field: 'to' | 'cc' | 'bcc') {
    if (!dragging || dragging.field === field) return
    e.preventDefault()
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move'
    dropInto = field
  }

  function onRowDrop(e: DragEvent, field: 'to' | 'cc' | 'bcc') {
    if (!dragging) return
    e.preventDefault()
    moveAddress(dragging.field, field, dragging.email)
    dragging = null
    dropInto = null
  }

  function onChipDragEnd() {
    dragging = null
    dropInto = null
  }

  function chipMenu(e: MouseEvent, field: 'to' | 'cc' | 'bcc', email: string) {
    menu.show(
      e,
      (['to', 'cc', 'bcc'] as const)
        .filter((other) => other !== field)
        .map((other) => ({
          label: `Move to ${FIELD_LABELS[other]}`,
          run: () => moveAddress(field, other, email),
        })),
    )
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
    // A message with words in it opens after the last of them, which is not the
    // end of the document: the signature is under them, and what is typed into
    // it stays there, unseen by Write and Improve.
    const afterWords =
      !underThread &&
      working.to.length > 0 &&
      Boolean(
        splitSignature(initialBody.own)
          .words.replace(/<[^>]*>/g, '')
          .trim(),
      )
    const ed = new Editor({
      element: el,
      extensions: [
        // StarterKit carries a link of its own; this one is configured below.
        // No trailing paragraph under a signature: there is nothing to type
        // there, and it would go out as a blank line above the quote.
        StarterKit.configure({
          heading: false,
          link: false,
          trailingNode: { notAfter: ['paragraph', 'signature'] },
        }),
        Signature,
        Placeholder.configure({
          // `untrack`: read while the editor is being built, inside this very
          // effect, it would otherwise rebuild the editor when Settings
          // loaded -- and throw away what had been typed.
          placeholder: () =>
            untrack(() => writingEnabled)
              ? `Write something, or press ${keyName('mod+g')} for AI`
              : 'Write something…',
        }),
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
        ListAfterBreak,
        MailPicture,
      ],
      // Only the person's own part -- see `quotedHtml`.
      content: initialBody.own,
      editorProps: {
        attributes: { class: 'ed-content', spellcheck: 'true' },
        // A picture pasted or dropped goes where it was put; any other file
        // is attached. Text is left to the editor (and `MarkdownClipboard`).
        // Words come first: a spreadsheet or a document copies a picture of
        // what was copied alongside its text, and the text is what was meant.
        handlePaste: (_view, event) => {
          const data = event.clipboardData
          if (data?.getData('text/plain').trim()) return false
          return addFiles(Array.from(data?.files ?? []))
        },
        handleDrop: (view, event, _slice, moved) => {
          if (moved) return false
          const at = view.posAtCoords({ left: event.clientX, top: event.clientY })?.pos
          return addFiles(Array.from(event.dataTransfer?.files ?? []), at)
        },
      },
      // Reply, Reply all, Forward -- and any draft that already has its
      // recipients -- open straight into the body: a fresh reply on the
      // empty first line above the folded quote, so typing can start at
      // once (a forward's recipients can follow); a draft with words of its
      // own already, after the last of them. A new message, with nobody to
      // send it to yet, leaves focus to the To field instead.
      autofocus: underThread || (working.to.length > 0 && !afterWords) ? 'start' : false,
      onCreate: ({ editor: created }) => {
        inlineBlobs = new Set(picturesIn(created.state.doc).keys())
        if (!afterWords) return
        const signature = signatureRange(created.state.doc)
        created.commands.focus(signature && signature.from > 0 ? signature.from - 1 : 'end')
      },
      onUpdate: ({ editor: updated }) => {
        syncPictures(updated.state.doc)
        touch()
      },
    })
    editor = ed
    return () => {
      working.bodyHtml = joinQuoted(withoutEmptySignature(ed.getHTML()), quotedHtml)
      ed.destroy()
      editor = null
    }
  })

  /** The editor's text, with the quote put back behind it. */
  function syncBody() {
    if (editor) working.bodyHtml = joinQuoted(withoutEmptySignature(editor.getHTML()), quotedHtml)
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

  /** What the person wrote, as plain text, without the quote or the signature
   *  -- `currentText` for `draft_with_ai`, and `text` for a whole-body
   *  `improve_writing`. */
  function ownText(): string {
    syncBody()
    return htmlToPlainText(splitSignature(splitQuoted(working.bodyHtml).own).words)
  }

  /** Replaces what the person wrote with `ownHtml`, keeping the signature and
   *  the quote below exactly as they were. Returns the HTML it replaced, for
   *  the "Rewritten · Undo" note. */
  function replaceOwnPart(ed: Editor, ownHtml: string): string {
    const previousHtml = ed.getHTML()
    const { own, quoted } = splitQuoted(previousHtml)
    ed.commands.setContent(joinQuoted(withOwnWords(own, ownHtml), quoted))
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
        identity: working.identity,
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
   *
   * A selection running into the signature stops at it: Select All and Polish
   * means their words, and replacing the range as it stood would take the block
   * with it. One entirely inside the signature was asked for, and is rewritten
   * there. All of their words selected is the same as nothing selected, and
   * keeps their paragraphs.
   */
  async function applyImprove(mode: ImproveMode) {
    const ed = editor
    if (!ed || improving || !writingEnabled) return
    const { doc, selection } = ed.state
    const signature = signatureRange(doc)
    let { from, to } = selection
    let allWords = false
    if (signature && from < signature.from && to > signature.from) {
      to = signature.from - 1
      allWords = from <= 1
    } else if (signature && from > signature.from && from < signature.to && to > signature.to) {
      from = signature.to
    }
    const selected = selection.empty ? '' : doc.textBetween(from, to, '\n\n')
    const useSelection = selected.trim().length > 0 && !allWords
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

  /** The blobs of the pictures in the message itself. Each has an entry of
   *  its own among `working.attachments` -- so the backend has its bytes, and
   *  nothing sweeps them away as unreferenced -- which is not drawn as a chip. */
  let inlineBlobs = $state<Set<string>>(new Set())

  /**
   * Which entries of `attachments` are pictures' own: the last with each
   * picture's blob. Blobs are named by their bytes, so a file attached with
   * the paperclip and the same picture pasted share one; the file's entry
   * comes first and stays a chip, and the backend sends the last one inline
   * (`outbox::outgoing`).
   */
  function pictureEntries(attachments: readonly DraftAttachment[]): Set<number> {
    const last = new Map<string, number>()
    attachments.forEach((a, i) => {
      if (inlineBlobs.has(a.blob)) last.set(a.blob, i)
    })
    return new Set(last.values())
  }

  /** The chips, with where each sits in `working.attachments`. */
  const fileAttachments = $derived.by(() => {
    const pictures = pictureEntries(working.attachments)
    return working.attachments
      .map((attachment, index) => ({ attachment, index }))
      .filter(({ index }) => !pictures.has(index))
  })

  /** Keep `working.attachments` in step with the pictures in `doc`: one
   *  taken out of the message loses its entry (a file of the same bytes
   *  stays), and one put back (an undo, a paste of a copied one) gets one. */
  function syncPictures(doc: Parameters<typeof picturesIn>[0]) {
    const pictures = picturesIn(doc)
    const own = pictureEntries(working.attachments)
    const kept = working.attachments.filter((a, i) => !own.has(i) || pictures.has(a.blob))
    const missing = [...pictures.values()]
      .filter((p) => !inlineBlobs.has(p.blob) || !kept.some((a) => a.blob === p.blob))
      .map((p) => ({ blob: p.blob, filename: p.filename, mimeType: p.mime || 'image/png' }))
    if (kept.length !== working.attachments.length || missing.length > 0) {
      working.attachments = [...kept, ...missing]
    }
    const same =
      pictures.size === inlineBlobs.size && [...pictures.keys()].every((b) => inlineBlobs.has(b))
    if (!same) inlineBlobs = new Set(pictures.keys())
  }

  /** Is there a file being dragged over the sheet -- what lights it up. */
  let dropping = $state(false)

  function carriesFiles(e: DragEvent): boolean {
    return e.dataTransfer?.types.includes('Files') ?? false
  }

  /** Files dropped on the sheet but not into the words -- on the address
   *  fields, the subject, the foot: pictures go in at the caret, and the rest
   *  are attached, as dropped into the words. */
  function onSheetDrop(e: DragEvent) {
    dropping = false
    if (e.defaultPrevented || !carriesFiles(e)) return
    e.preventDefault()
    addFiles(Array.from(e.dataTransfer?.files ?? []))
  }

  /**
   * Store `files`, putting each picture into the message -- at `at`, a
   * position in the editor, or the caret -- and attaching everything else.
   * Answers whether there was anything to take, so a paste or a drop of
   * text is left to the editor.
   */
  function addFiles(files: File[], at?: number): boolean {
    if (files.length === 0) return false
    void (async () => {
      let place = at
      const others: File[] = []
      for (const file of files) {
        if (!file.type.startsWith('image/') || !editor) {
          others.push(file)
          continue
        }
        try {
          const blob = await api.putBlob(new Uint8Array(await file.arrayBuffer()))
          const attrs = { blob, mime: file.type, filename: file.name || 'picture' }
          // Gone while it was being stored: kept as an attachment instead,
          // so nothing pasted is lost.
          if (!editor) {
            working.attachments = [
              ...working.attachments,
              { blob, filename: attrs.filename, mimeType: attrs.mime },
            ]
            continue
          }
          editor.chain().focus().insertPicture(attrs, place).run()
          // The next one after it, not in front of it.
          if (place !== undefined) place += 1
        } catch (e) {
          await handle(e)
        }
      }
      if (others.length > 0) await attachFiles(others)
    })()
    return true
  }

  async function onFiles(files: FileList | null) {
    if (files) await attachFiles(Array.from(files))
  }

  async function attachFiles(files: File[]) {
    if (!files) return
    // The same blob-upload path notes and journal entries use --
    // `api.putBlob` -- producing the real `DraftAttachment` shape
    // (`types.ts`) rather than the richer `Attachment` those two domains
    // carry: a draft's attachment is a blob, a filename and a MIME type,
    // nothing else, because `mail-builder` is what turns it into a MIME
    // part on the way out.
    for (const file of files) {
      const blob = await api.putBlob(new Uint8Array(await file.arrayBuffer()))
      // In front of the pictures' own entries, which stay last -- see
      // `pictureEntries`.
      const own = pictureEntries(working.attachments)
      working.attachments = [
        ...working.attachments.filter((_, i) => !own.has(i)),
        { blob, filename: file.name, mimeType: file.type || 'application/octet-stream' },
        ...working.attachments.filter((_, i) => own.has(i)),
      ]
    }
    touch()
  }

  /** Take off the chip at `index` in `working.attachments`, and no other
   *  entry with its blob: a picture in the words keeps its own. */
  function removeAttachment(index: number) {
    working.attachments = working.attachments.filter((_, i) => i !== index)
    touch()
  }

  // ── sending ──────────────────────────────────────────────────────
  //
  // The undo window itself lives in `mail.svelte.ts`'s `send`/`undoSend` --
  // see that file's own note on why: this sheet is unmounted the moment
  // `onclose()` runs, and a toast whose state lived here would never draw a
  // frame.

  /** Seconds the undo banner stays open: the short end of the plan's "five
   *  to thirty seconds" outbox window (`undo_send_delay` clamps to it), so a
   *  message is on its way and in the thread soon after Send. */
  const UNDO_WINDOW_S = 5

  /** The error `written` is showing, if it is. */
  let unsavedNotice = 0

  /**
   * Is what is on screen what is stored? `flush` resolves whether or not the
   * write landed and keeps trying on its own, which is right for autosave and
   * wrong for what is about to go out: whatever was last stored would be sent,
   * or nothing at all if nothing ever was. Stays here, and says so, if not.
   */
  async function written(): Promise<boolean> {
    await saver.flush()
    if (!saver.retrying) {
      // An error stays until dismissed, and this one is no longer true.
      notify.dismiss(unsavedNotice)
      return true
    }
    unsavedNotice = notify.error('This message could not be saved, so it has not been sent', {
      key: 'mail-draft-not-saved',
      body: 'It is still here, and saving it is being tried again.',
    })
    return false
  }

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
    // Written whatever `saver` thinks: a reply nobody has typed in has never
    // been saved, and there is no draft to send until it has.
    saver.touch(working.id)
    if (!(await written())) return
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
    if (!(await written())) return
    const closed = await proposals.accept(p)
    if (closed) onclose()
  }

  /** Decline, but leave the draft and the sheet exactly as they were --
   *  declining a proposal to send is not a request to throw the draft away. */
  async function declineProposal(reason: DeclineReason | null) {
    const p = proposal
    if (!p) return
    await proposals.decline(p, reason)
  }

  function toggleReasons(e: MouseEvent) {
    if (choosingReason) menu.close()
    else menu.show(e, declineItems(declineProposal), reasons)
  }

  /**
   * The stored draft gone. The order is what matters: nothing further is
   * written, whatever is already on its way lands first so it cannot follow
   * the discard and bring the draft back -- locally, and, once that write
   * reaches the outbox, on the server too -- and anything typed meanwhile is
   * dropped after. `forget()` is `Autosave`'s own answer to "the record it
   * would write is already gone", the same call `notes.svelte.ts`'s `remove()`
   * makes for the same reason. False, and said so, if it did not happen, with
   * the draft still here and still to be saved.
   */
  async function discardStored(id: DraftId): Promise<boolean> {
    saver.forget(id)
    try {
      await saver.flush()
      await mailApi.discardDraft(id)
    } catch (e) {
      saver.touch(id)
      await handle(e)
      return false
    }
    saver.forget(id)
    return true
  }

  async function discard() {
    syncBody()
    if (isBlankDraft(working)) {
      if (!(await discardStored(working.id))) return
    } else {
      await saver.flush()
    }
    onclose()
  }

  /** Guards the bin against a second click while the first is on its way. */
  let throwingAway = $state(false)

  /** The bin: gone, where the close button keeps it. Words went with it, so
   *  it offers them back. */
  async function throwAway() {
    const worthKeeping = edited || ownText().trim().length > 0
    const kept = $state.snapshot(working)
    const thread = underThread ? mail.selectedThread : null
    throwingAway = true
    if (!(await discardStored(kept.id))) {
      throwingAway = false
      return
    }
    onclose()
    if (!worthKeeping) return
    notify.info('Draft discarded', {
      key: 'mail-draft-discarded',
      action: { label: 'Undo', run: () => void bringBack(kept, thread) },
    })
  }

  /** The same draft back, with what was typed since it last autosaved. */
  async function bringBack(kept: Draft, thread: string | null) {
    try {
      await mailApi.restoreDraft(kept.id)
      await mailApi.saveDraft(kept)
      if (thread && mail.selectedThread === thread) mail.openInlineDraft(kept)
      else mail.openDraft(kept)
    } catch (e) {
      await handle(e)
    }
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
  inert={moving}
  class={underThread ? 'thread-compose' : 'pane-compose'}
  class:dropping
  role="region"
  aria-label="Compose"
  ondragover={(e) => {
    if (!carriesFiles(e)) return
    e.preventDefault()
    dropping = true
  }}
  ondragleave={(e) => {
    if (!(e.currentTarget as HTMLElement).contains(e.relatedTarget as Node | null)) dropping = false
  }}
  ondrop={onSheetDrop}
>
  {#if underThread && account}
    <span class="own-avatar">
      <Avatar name={sender} email={working.identity} size={40} />
    </span>
  {/if}
  <div class="head">
    {#if underThread}
      <button
        class="draft-to"
        aria-expanded={showFields}
        title={recipientsTitle || 'Show the address fields'}
        onclick={toggleFields}
      >
        <span class="draft-word">{working.inReplyTo ? 'Draft' : 'Forward'}</span>
        {#if recipients}<span class="draft-names">to {recipients}</span>{/if}
        <span class="draft-fold" class:open={showFields}>
          <Icon name="chevron" size={11} weight={2} />
        </span>
      </button>
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
    <button class="close" aria-label="Close" title="Close and keep the draft" onclick={discard}
      ><Icon name="close" size={15} /></button
    >
  </div>

  {#snippet addressRow(field: 'to' | 'cc' | 'bcc')}
    {@const label = FIELD_LABELS[field]}
    <div
      class="line address"
      class:drop-into={dropInto === field}
      role="group"
      aria-label={label}
      ondragover={(e) => onRowDragOver(e, field)}
      ondragleave={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null) && dropInto === field) {
          dropInto = null
        }
      }}
      ondrop={(e) => onRowDrop(e, field)}
    >
      <span class="label">{label}</span>
      <div class="chips">
        {#each working[field] as a (a.email)}
          <span
            class="chip"
            class:lifted={dragging?.field === field && dragging.email === a.email}
            title={`${a.email} · drag to To, Cc or Bcc, or right-click to move`}
            role="listitem"
            draggable="true"
            ondragstart={(e) => onChipDragStart(e, field, a.email)}
            ondragend={onChipDragEnd}
            oncontextmenu={(e) => chipMenu(e, field, a.email)}
            >{a.name || a.email}<button
              aria-label={`Remove ${a.name || a.email}`}
              onclick={() => removeAddress(field, a.email)}><Icon name="close" size={10} /></button
            ></span
          >
        {/each}
        <input
          use:focusOnMount={field === 'to' &&
            (focusFields || (!underThread && working.to.length === 0))}
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

  {#if working.sendError && working.state.type === 'editing'}
    <p class="unsent" role="status">
      <Icon name="alert" size={13} />
      <span><strong>Not sent.</strong> {working.sendError}</span>
    </p>
  {/if}

  {#if showFields}
    <div class="fields">
      {#if fromCount > 1}
        <div class="line">
          <span class="label">From</span>
          <select
            class="from"
            aria-label="From"
            disabled={moving}
            value={fromValue(working.accountId, working.identity)}
            onchange={(e) => void setFrom(e.currentTarget)}
          >
            {#each fromGroups as group (group.account.id)}
              {#if fromGroups.length > 1}
                <optgroup label={group.account.displayName || group.account.address}>
                  {#each group.addresses as address (address)}
                    <option value={fromValue(group.account.id, address)}>{address}</option>
                  {/each}
                </optgroup>
              {:else}
                {#each group.addresses as address (address)}
                  <option value={fromValue(group.account.id, address)}>{address}</option>
                {/each}
              {/if}
            {/each}
          </select>
        </div>
      {/if}
      {@render addressRow('to')}
      {#if showCcBcc}
        {@render addressRow('cc')}
        {@render addressRow('bcc')}
      {/if}

      <div class="line">
        <span class="label">Subject</span>
        <input
          class="subject"
          aria-label="Subject"
          value={working.subject}
          oninput={(e) => {
            working.subject = e.currentTarget.value
            touch()
          }}
        />
      </div>
    </div>
  {/if}

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

  {#if fileAttachments.length > 0}
    <div class="attachments">
      {#each fileAttachments as { attachment: a, index } (index)}
        <span class="chip"
          >{a.filename}<button onclick={() => removeAttachment(index)}
            ><Icon name="close" size={10} /></button
          ></span
        >
      {/each}
    </div>
  {/if}

  <!-- The two ways out on the left, where the eye finishes the last line
       written; the tools on the right, out of the way until they are wanted. -->
  <div class="foot">
    {#if proposal}
      <!-- A dream's send, not the person's own: the two ordinary send paths
           give way to accept and decline, the same pair every ghost in the
           app offers, because a proposal answered from in here is still
           answered through `proposals.accept`/`decline` and not `mail.send`. -->
      <button
        class="btn btn-primary btn-sm"
        title={`Send (${keyName('mod+Enter')})`}
        disabled={proposalBusy}
        onclick={() => void acceptProposal()}
      >
        Send
      </button>
      <button
        class="btn btn-outline btn-sm"
        disabled={proposalBusy}
        data-menu-trigger
        aria-haspopup="menu"
        aria-expanded={choosingReason}
        onclick={toggleReasons}
      >
        Decline
      </button>
    {:else}
      <button
        class="btn btn-primary btn-sm"
        title={`Send (${keyName('mod+Enter')})`}
        onclick={() => void send()}
      >
        Send
      </button>
      <div class="later-wrap">
        <button
          class="btn btn-outline btn-sm"
          aria-expanded={sendingLater}
          onclick={() => {
            sendingLater = !sendingLater
            pickingDateTime = false
          }}
        >
          <Icon name="clock" size={14} /> Send later
        </button>
        {#if sendingLater}
          <div
            class="later-pop"
            role="menu"
            use:dismissable={{ onaway: () => (sendingLater = false), within: '.later-wrap' }}
          >
            {#if pickingDateTime}
              <input
                type="datetime-local"
                bind:value={sendAt}
                min={toLocalInputValue(new Date())}
              />
              <button class="btn btn-primary btn-sm" onclick={() => void sendLater()}>
                Schedule
              </button>
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
    {/if}
    <span class="spacer"></span>
    <button
      class="tool"
      class:active={aiPromptOpen}
      aria-label="Write with AI"
      title={writingEnabled ? `Write with AI (${keyName('mod+g')})` : writingDisabledReason}
      disabled={!writingEnabled}
      onclick={() => (aiPromptOpen = !aiPromptOpen)}
    >
      <Icon name="sparkle" size={16} />
    </button>
    <button
      class="tool"
      aria-label="Improve what you wrote"
      aria-haspopup="menu"
      aria-busy={improving}
      title={writingEnabled
        ? improving
          ? 'Improving…'
          : 'Improve what you wrote'
        : writingDisabledReason}
      disabled={!writingEnabled || improving}
      onclick={openImproveMenu}
    >
      <Icon name="pencil" size={15} />
    </button>
    <label class="tool" title="Attach files" aria-label="Attach files">
      <Icon name="paperclip" size={16} />
      <input type="file" multiple hidden onchange={(e) => void onFiles(e.currentTarget.files)} />
    </label>
    {#if !proposal}
      <button
        class="tool"
        aria-label="Discard the draft"
        title="Discard the draft"
        disabled={throwingAway}
        onclick={() => void throwAway()}
      >
        <Icon name="trash" size={16} />
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
  /* Past the floating assistant button, which sits on this corner. */
  .pane-compose > .foot {
    padding-right: var(--fab-clear);
  }
  /* The next message in the thread it answers: no box, a hairline above, and
     its text on the same line as the messages' names and bodies, the writer's
     avatar in the gutter they keep theirs in. */
  /* A file held over the sheet: somewhere to let go of it. */
  .dropping {
    outline: 2px dashed color-mix(in oklab, var(--accent) 60%, transparent);
    outline-offset: -2px;
  }
  .thread-compose {
    --gutter: calc(40px + var(--sp-3) + var(--sp-1));
    position: relative;
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    width: 100%;
    padding: var(--sp-4) 0 0 var(--gutter);
    border-top: 1px solid var(--border);
  }
  .own-avatar {
    position: absolute;
    top: var(--sp-3);
    left: var(--sp-1);
  }
  /* A narrow pane needs the width more than the alignment, as the messages
     above do. */
  @container (max-width: 520px) {
    .thread-compose {
      padding-left: 0;
    }
    .own-avatar {
      display: none;
    }
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
  /* Who it is to, which is also what unfolds the fields -- a message's own
     name line, with one word of colour in it. */
  .draft-to {
    flex: 0 1 auto;
    min-width: 0;
    display: flex;
    align-items: baseline;
    gap: 0.35em;
    padding: 2px 0;
    font-size: var(--text-md);
    font-weight: 620;
    color: var(--fg);
    text-align: left;
  }
  .draft-word {
    flex: none;
    color: var(--journal-accent, var(--accent));
  }
  .draft-names {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .draft-fold {
    flex: none;
    align-self: center;
    display: grid;
    color: var(--fg-faint);
    rotate: 90deg;
    transition: rotate var(--fast) var(--ease);
  }
  .draft-fold.open {
    rotate: -90deg;
  }
  .draft-to:hover .draft-fold {
    color: var(--fg);
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
    flex: none;
    display: grid;
    place-items: center;
    margin-left: auto;
    width: 22px;
    height: 22px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .close:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  /* Not `.field`: `app.css` draws that as a whole bordered input box, which
     is what put a box around each of these. */
  .fields {
    display: flex;
    flex-direction: column;
  }
  .line {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-height: 36px;
    padding: var(--sp-1) 0;
    border-bottom: 1px solid var(--border);
  }
  /* What the suggestions hang from. */
  .line.address {
    position: relative;
  }
  .label {
    flex: none;
    width: 52px;
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
  .chip[draggable='true'] {
    cursor: grab;
  }
  .chip.lifted {
    opacity: 0.45;
  }
  /* The line a dragged chip will land on. */
  .line.address.drop-into {
    background: var(--bg-hover);
    box-shadow: inset 2px 0 0 var(--journal-accent, var(--accent));
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
  .chips input::placeholder {
    color: var(--fg-faint);
  }
  .subject {
    width: 100%;
  }
  .from {
    flex: 1;
    min-width: 0;
    border: 0;
    background: none;
    color: var(--fg);
    font-size: var(--text-base);
  }
  .from:focus-visible {
    outline: 1px solid var(--accent);
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
    left: 60px;
    z-index: 30;
    display: flex;
    flex-direction: column;
    width: min(420px, calc(100% - 60px));
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
    min-height: 72px;
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
  /* A picture pasted into the message: as wide as the words at most, and
     outlined when picked, so Delete visibly has something to take. */
  .prose :global(img[data-blob]) {
    max-width: 100%;
    height: auto;
    vertical-align: bottom;
    border-radius: var(--radius-sm);
  }
  .prose :global(img[data-blob].ProseMirror-selectednode) {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .prose :global(td p),
  .prose :global(th p) {
    margin: 0;
  }
  .prose :global(.selectedCell) {
    background: color-mix(in oklab, var(--accent) 14%, transparent);
  }
  /* Quieter than what is being written, and a line clear of it. Only here: it
     goes out as ordinary text. */
  .prose :global([data-signature]) {
    margin-top: var(--sp-4);
    color: var(--fg-muted);
  }
  /* `is-empty` rather than `is-editor-empty`: a new message has its signature
     in it, so the editor as a whole is never empty, and the line to write on
     still is. */
  .prose :global(.ed-content > p.is-empty:first-child::before) {
    content: attr(data-placeholder);
    float: left;
    height: 0;
    pointer-events: none;
    color: var(--fg-faint);
  }

  /* The whole quote, folded into three dots, and nothing around them until
     the pointer is on them. */
  .quote-toggle {
    align-self: flex-start;
    margin-left: -6px;
    padding: 0 6px;
    height: 18px;
    line-height: 16px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
    font-size: 11px;
    letter-spacing: 1px;
  }
  .quote-toggle:hover,
  .quote-toggle.open {
    background: var(--bg-hover);
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
  }
  /* The tools, to the far end of the row. */
  .foot .spacer {
    flex: 1;
  }
  .tool {
    flex: none;
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    margin-left: calc(-1 * var(--sp-2));
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
    cursor: pointer;
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .tool:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .tool.active {
    background: color-mix(in oklab, var(--accent) 14%, transparent);
    color: var(--journal-accent, var(--accent));
  }
  .tool:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .foot input[type='datetime-local'] {
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 4px var(--sp-2);
    background: var(--bg-raised);
    color: var(--fg);
  }

  /* Why the last send did not go out -- the server's own words. */
  .unsent {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    margin: 0;
    padding: var(--sp-1) var(--sp-2);
    border: 1px solid color-mix(in oklab, var(--danger) 40%, var(--border));
    border-radius: var(--radius-sm);
    background: color-mix(in oklab, var(--danger) 6%, transparent);
    font-size: var(--text-sm);
    color: var(--fg);
    overflow-wrap: anywhere;
  }
  .unsent :global(svg) {
    flex: none;
    align-self: center;
    color: var(--danger);
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

  .later-wrap {
    position: relative;
  }
  .later-pop {
    position: absolute;
    left: 0;
    bottom: 100%;
    z-index: 20;
    display: flex;
    flex-direction: column;
    min-width: 240px;
    margin-bottom: 4px;
    padding: var(--sp-1);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    box-shadow: var(--shadow-lg);
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
  .later-pop button[role='menuitem'] span {
    white-space: nowrap;
  }
  .later-pop .when {
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }
  .later-pop .btn {
    margin-top: var(--sp-1);
  }
  .later-pop input[type='datetime-local'] {
    margin-bottom: var(--sp-1);
  }
</style>
