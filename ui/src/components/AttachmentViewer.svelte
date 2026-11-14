<script lang="ts">
  // Looking at an attachment without saving it: the picture, the film, the
  // recording, the PDF or the page, over the whole window, with the message's
  // other attachments a key away. Which ones can be looked at, and as what, is
  // the backend's call (`mailpreview::kind_of`), made once for the chips and
  // for this.
  //
  // A page -- a web page, Markdown, a CSV, a forwarded message, text -- comes
  // back from `preview_attachment` as a whole document and goes into the same
  // frame a message does, with the same sandbox and for the same reasons (see
  // the top of `MailThread.svelte`): no scripts, an opaque origin, and links
  // that leave for the browser. Everything else is drawn here from the part's
  // own bytes at `everyday://mail/part/…`.

  import { untrack } from 'svelte'
  import { humanBytes } from '../lib/format'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import * as mailApi from '../lib/mail-api'
  import {
    applyBodyTheme,
    applyPlatformOrigin,
    mediaType,
    mockPartUrl,
    partIdentifier,
    partUrl,
  } from '../lib/mailview'
  import type { MailAttachment, MailMessageDetail } from '../lib/types'
  import { app } from '../lib/state.svelte'
  import Icon from './Icon.svelte'
  import PdfView from './PdfView.svelte'

  interface Props {
    message: MailMessageDetail
    /** The message's attachments that can be looked at, in their order. */
    attachments: MailAttachment[]
    /** Which of them to open on, by its place in the message's parts. */
    index: number
    dark: boolean
    /** Saves one, answering whether it was. */
    onsave: (attachment: MailAttachment) => Promise<boolean>
    onclose: () => void
  }
  let { message, attachments, index, dark, onsave, onclose }: Props = $props()

  /** The open file, by its place in the message's parts rather than in
   *  `attachments`: the thread is read again now and then -- a sync, a flag,
   *  a part fetched -- and the list can gain or lose files before this one. */
  let open = $state(untrack(() => index))
  const position = $derived(attachments.findIndex((a) => a.index === open))
  const current = $derived(position >= 0 ? attachments[position] : undefined)
  /** Which file is open. Re-reading the thread hands this new copies of the
   *  same attachments and the same message; that is not a reason to load the
   *  open one again, so this is a string, which only changes when the file
   *  does. */
  const opened = $derived(current ? `${message.id}/${partIdentifier(current)}` : null)

  // Gone from the message, or no longer one that can be looked at: there is
  // nothing to show, and showing its neighbour instead would be a surprise.
  $effect(() => {
    if (position < 0) onclose()
  })

  type Shown =
    | { state: 'loading' }
    | { state: 'failed'; reason: string }
    | { state: 'image'; url: string }
    | { state: 'media'; url: string; video: boolean }
    | { state: 'pdf'; data: Uint8Array }
    | { state: 'page'; html: string }
  let shown = $state.raw<Shown>({ state: 'loading' })
  /** Which files a save is under way for, and which were saved, by place in
   *  the parts: a save outlives stepping to the next file. */
  let saving = $state<ReadonlySet<number>>(new Set())
  let saved = $state<ReadonlySet<number>>(new Set())

  const COULD_NOT = 'This file could not be shown here. Save it to open it.'

  function address(messageId: string, a: MailAttachment): string {
    const url =
      partUrl(messageId, partIdentifier(a)) ?? mockPartUrl(a.mimeType, `${messageId}:${a.index}`)
    if (!url) throw new Error('no address for this part')
    return url
  }

  async function bytes(url: string): Promise<ArrayBuffer> {
    const response = await fetch(url)
    if (!response.ok) throw new Error(`${response.status} ${await response.text()}`)
    return response.arrayBuffer()
  }

  // Load whichever attachment is open, and let go of the last one's bytes.
  // Depends on `opened` alone: everything else is read untracked, once.
  $effect(() => {
    if (!opened) return
    const { a, messageId } = untrack(() => ({
      a: current ? { ...current } : undefined,
      messageId: message.id,
    }))
    if (!a) return
    let gone = false
    let blob: string | null = null
    shown = { state: 'loading' }
    void (async () => {
      try {
        let next: Shown
        switch (a.preview) {
          case 'image':
            next = { state: 'image', url: address(messageId, a) }
            break
          case 'video':
          case 'audio': {
            // Held whole in a `blob:` address rather than played from the
            // part's own, which answers no range requests: a film has to be
            // able to seek, and one whose index is at the end cannot start.
            const data = await bytes(address(messageId, a))
            if (gone) return
            blob = URL.createObjectURL(new Blob([data], { type: mediaType(a) }))
            next = { state: 'media', url: blob, video: a.preview === 'video' }
            break
          }
          case 'pdf':
            next = { state: 'pdf', data: new Uint8Array(await bytes(address(messageId, a))) }
            break
          case 'page': {
            const { html } = await mailApi.previewAttachment(messageId, a.index)
            next = { state: 'page', html: applyPlatformOrigin(html) }
            break
          }
          default:
            throw new Error('not an attachment that can be looked at')
        }
        if (!gone) shown = next
      } catch {
        if (!gone) shown = { state: 'failed', reason: COULD_NOT }
      }
    })()
    return () => {
      gone = true
      if (blob) URL.revokeObjectURL(blob)
    }
  })

  const themed = $derived(shown.state === 'page' ? applyBodyTheme(shown.html, dark, app.frame) : '')
  const name = $derived(current?.filename || 'Attachment')
  const isSaving = $derived(current !== undefined && saving.has(current.index))
  const isSaved = $derived(current !== undefined && saved.has(current.index))

  function go(by: 1 | -1) {
    const next = attachments[position + by]
    if (next) open = next.index
  }

  async function save() {
    const a = current
    if (!a || saving.has(a.index)) return
    saving = new Set([...saving, a.index])
    try {
      if (await onsave(a)) saved = new Set([...saved, a.index])
    } finally {
      saving = new Set([...saving].filter((i) => i !== a.index))
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault()
      onclose()
      return
    }
    // A film's own controls seek with the arrows, and a PDF zoomed wider
    // than the window scrolls sideways with them.
    if (e.target instanceof HTMLMediaElement || e.defaultPrevented || scrollsSideways(e.target)) {
      return
    }
    if (e.key === 'ArrowLeft') go(-1)
    else if (e.key === 'ArrowRight') go(1)
  }

  function scrollsSideways(target: EventTarget | null): boolean {
    for (let el = target instanceof Element ? target : null; el; el = el.parentElement) {
      const overflow = getComputedStyle(el).overflowX
      if ((overflow === 'auto' || overflow === 'scroll') && el.scrollWidth > el.clientWidth) {
        return true
      }
    }
    return false
  }

  function failed(reason: string) {
    shown = { state: 'failed', reason }
  }

  let viewer = $state<HTMLElement | null>(null)

  /** A click into a page hands the keyboard to the frame -- so its text can
   *  be selected and copied -- and a frame with no scripts can tell this
   *  window nothing about the keys pressed in it, Escape included. Leaving
   *  the frame with the pointer hands the keyboard back. Tab never goes in:
   *  the frame is `tabindex="-1"`. */
  function reclaimKeys() {
    if (document.activeElement instanceof HTMLIFrameElement) viewer?.focus()
  }
</script>

<svelte:window {onkeydown} />

<div class="backdrop"></div>
<div
  class="viewer"
  role="dialog"
  aria-modal="true"
  aria-label="Preview of {name}"
  tabindex="-1"
  bind:this={viewer}
  use:focusOnMount
  use:trapFocus
>
  <!-- The window's own title bar is under this, so the bar stands in for it
       as somewhere to drag the window by. -->
  <header class="bar" data-tauri-drag-region>
    <div class="what" data-tauri-drag-region>
      <span class="file" title={name} data-tauri-drag-region>{name}</span>
      <span class="facts" data-tauri-drag-region>
        {humanBytes(current?.size ?? 0)}{attachments.length > 1
          ? ` · ${position + 1} of ${attachments.length}`
          : ''}
      </span>
    </div>
    {#if attachments.length > 1}
      <button
        class="tool back"
        aria-label="Previous attachment"
        title="Previous attachment"
        disabled={position <= 0}
        onclick={() => go(-1)}
      >
        <Icon name="chevron" size={17} />
      </button>
      <button
        class="tool"
        aria-label="Next attachment"
        title="Next attachment"
        disabled={position >= attachments.length - 1}
        onclick={() => go(1)}
      >
        <Icon name="chevron" size={17} />
      </button>
    {/if}
    <button class="tool labelled" disabled={isSaving} onclick={() => void save()}>
      <Icon name="download" size={16} />
      {isSaved ? 'Saved' : 'Save'}
    </button>
    <button class="tool" aria-label="Close" title="Close" onclick={onclose}>
      <Icon name="close" size={17} />
    </button>
  </header>

  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="stage" onclick={(e) => e.target === e.currentTarget && onclose()}>
    {#if shown.state === 'loading'}
      <p class="quiet">Loading…</p>
    {:else if shown.state === 'failed'}
      <div class="failed">
        <p>{shown.reason}</p>
        <button class="btn btn-primary" disabled={isSaving} onclick={() => void save()}>
          {isSaved ? 'Saved' : 'Save'}
        </button>
      </div>
    {:else if shown.state === 'image'}
      <img
        class="picture"
        src={shown.url}
        alt={name}
        onerror={() => failed('This picture could not be shown. Save it to open it.')}
      />
    {:else if shown.state === 'media'}
      {#if shown.video}
        <!-- A stranger's film comes with no captions to offer. -->
        <!-- svelte-ignore a11y_media_has_caption -->
        <video
          class="film"
          src={shown.url}
          controls
          onerror={() => failed("This video can't be played here. Save it to open it.")}
        ></video>
      {:else}
        <div class="recording">
          <Icon name="mic" size={28} />
          <span class="file">{name}</span>
          <audio
            src={shown.url}
            controls
            onerror={() => failed("This recording can't be played here. Save it to open it.")}
          ></audio>
        </div>
      {/if}
    {:else if shown.state === 'pdf'}
      <div class="reader">
        <PdfView data={shown.data} onfail={failed} />
      </div>
    {:else}
      <div class="paper" onpointerleave={reclaimKeys}>
        <iframe
          title={name}
          tabindex="-1"
          sandbox="allow-popups allow-popups-to-escape-sandbox"
          srcdoc={themed}
        ></iframe>
      </div>
    {/if}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 64;
    background: rgb(14 13 16 / 0.88);
  }
  .viewer {
    position: fixed;
    inset: 0;
    z-index: 65;
    display: grid;
    grid-template-rows: auto minmax(0, 1fr);
    color: #f2f1ee;
    outline: none;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 10px 12px 10px 20px;
  }
  .what {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: baseline;
    gap: 10px;
  }
  .file {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 560;
  }
  .facts {
    flex: none;
    color: rgb(255 255 255 / 0.6);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
  }
  .tool {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    min-width: 34px;
    height: 34px;
    padding: 0 8px;
    border-radius: 999px;
    color: inherit;
  }
  .tool:hover:not(:disabled) {
    background: rgb(255 255 255 / 0.12);
  }
  .tool:disabled {
    opacity: 0.4;
  }
  .tool.labelled {
    padding: 0 14px 0 10px;
  }
  .back :global(.icon) {
    transform: scaleX(-1);
  }
  .stage {
    min-height: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0 24px 24px;
  }
  .quiet {
    color: rgb(255 255 255 / 0.7);
  }
  .failed {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 14px;
    max-width: 30em;
    text-align: center;
  }
  .picture,
  .film {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
    border-radius: 4px;
    box-shadow: 0 4px 24px rgb(0 0 0 / 0.45);
  }
  .picture {
    /* A transparent picture is drawn on what it was most likely made for. */
    background: #ffffff;
  }
  .recording {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 14px;
    max-width: 100%;
  }
  .recording audio {
    width: min(480px, 80vw);
  }
  .reader {
    align-self: stretch;
    width: 100%;
  }
  .paper {
    align-self: stretch;
    width: min(100%, 940px);
    border-radius: 8px;
    overflow: hidden;
    box-shadow: 0 4px 24px rgb(0 0 0 / 0.45);
  }
  .paper iframe {
    display: block;
    width: 100%;
    height: 100%;
    border: 0;
  }
</style>
