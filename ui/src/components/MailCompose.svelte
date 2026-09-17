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
  import { api } from '../lib/api'
  import { Autosave } from '../lib/autosave'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import * as mailApi from '../lib/mail-api'
  import { isBlankDraft } from '../lib/mail'
  import { mail } from '../lib/mail.svelte'
  import { DECLINE_REASONS, proposals } from '../lib/proposals.svelte'
  import { toLocalInputValue } from '../lib/format'
  import type { Draft, MailAddress } from '../lib/types'
  import Icon from './Icon.svelte'

  let { draft, onclose }: { draft: Draft; onclose: () => void } = $props()

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
  let sendAt = $state(toLocalInputValue(new Date(Date.now() + 3_600_000)))

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

  $effect(() => {
    const el = host
    if (!el) return
    const ed = new Editor({
      element: el,
      extensions: [
        StarterKit.configure({ heading: false }),
        Placeholder.configure({ placeholder: 'Write something…' }),
        Link.configure({
          openOnClick: true,
          autolink: true,
          protocols: ['http', 'https', 'mailto'],
        }),
      ],
      content: working.bodyHtml,
      editorProps: { attributes: { class: 'ed-content', spellcheck: 'true' } },
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
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
      e.preventDefault()
      void (proposal ? acceptProposal() : send())
    }
    if (e.key === 'Escape') {
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

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="scrim" onclick={discard}></div>
<div class="sheet compose" role="dialog" aria-modal="true" aria-label="Compose" use:trapFocus>
  <div class="head">
    <h2>{working.subject || 'New message'}</h2>
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
        use:focusOnMount
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
    {:else if sendingLater}
      <input type="datetime-local" bind:value={sendAt} />
      <button class="btn" onclick={() => void sendLater()}>Schedule</button>
      <button class="btn" onclick={() => (sendingLater = false)}>Cancel</button>
    {:else}
      <button class="btn" onclick={() => (sendingLater = true)}>Send later</button>
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
