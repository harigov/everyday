<script lang="ts">
  // Looking at a file without leaving the list: the picture, the film, the
  // recording, the PDF or the text, over the whole window, with the files
  // either side of it a key away. The mail attachment viewer's sibling -- the
  // same window, focus and keys -- for files in the catalog.
  //
  // What differs from a mail part is that a file's address answers range
  // requests, so a film plays from where it is and seeks without being held
  // whole in memory first, and a very large text file is read for its first
  // megabyte only. A PDF is still read whole: pdf.js wants its bytes.
  //
  // Which files, and which is open, are the caller's: it hands the list and a
  // place in it, and hears of every step (`onindex`) so what it keeps -- the
  // store's `viewing`, a cursor -- follows. The open file is held by id here,
  // as the attachment viewer holds it by part: the list is read again now and
  // then, and can gain or lose files before this one.
  //
  // The information panel is the inspector, the same one as in the rail.

  import { untrack } from 'svelte'
  import { fileUrl, fileVersion, thumbnailUrl } from '../lib/api'
  import { formatSize, kindIcon, previewOf } from '../lib/files'
  import { files as store } from '../lib/files.svelte'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import type { FileEntry } from '../lib/types'
  import FileInspector from './FileInspector.svelte'
  import Icon from './Icon.svelte'
  import PdfView from './PdfView.svelte'

  interface Props {
    files: FileEntry[]
    /** Which of them to open on. */
    index: number
    onclose: () => void
    /** Called with the place in `files` each time the viewer steps. */
    onindex?: (index: number) => void
  }
  let { files, index, onclose, onindex }: Props = $props()

  /** The most of a text file that is read. */
  const TEXT_BYTES = 1024 * 1024

  let openId = $state<string | null>(untrack(() => files[index]?.id ?? null))
  let lastAt = untrack(() => index)
  const position = $derived(openId === null ? -1 : files.findIndex((f) => f.id === openId))
  const current = $derived(position >= 0 ? files[position] : undefined)

  // The caller moved it: another file was opened from outside.
  $effect(() => {
    const i = index
    untrack(() => {
      const next = files[i]
      if (next && next.id !== openId) openId = next.id
    })
  })

  // The open file left the list -- trashed, moved, filtered out. Its
  // neighbour is shown rather than nothing, and the window closes only when
  // nothing is left.
  $effect(() => {
    if (position >= 0) {
      lastAt = position
      return
    }
    const keep = Math.min(lastAt, files.length - 1)
    if (keep >= 0) {
      openId = files[keep]!.id
      onindex?.(keep)
    } else onclose()
  })

  type Shown =
    | { state: 'loading' }
    | { state: 'failed'; reason: string; restorable?: boolean }
    | { state: 'image'; url: string; note?: string }
    | { state: 'media'; url: string; video: boolean }
    | { state: 'pdf'; data: Uint8Array }
    | { state: 'text'; text: string; truncated: boolean }
  let shown = $state.raw<Shown>({ state: 'loading' })

  /** What the load below depends on: the file and its version, and nothing
   *  else about it, so a rename or a tag does not reload what is open. */
  const opened = $derived(current ? `${current.id}:${fileVersion(current)}` : null)

  const COULD_NOT = 'This file could not be shown here.'

  async function bytes(url: string): Promise<ArrayBuffer> {
    const response = await fetch(url)
    if (!response.ok) throw new Error(`${response.status} ${await response.text()}`)
    return response.arrayBuffer()
  }

  /** The start of a text file: asked for with a range, and not read past the
   *  cap even if the server sends the whole of it. */
  async function head(url: string, size: number): Promise<{ text: string; truncated: boolean }> {
    const response = await fetch(url, { headers: { Range: `bytes=0-${TEXT_BYTES - 1}` } })
    if (!response.ok) throw new Error(`${response.status} ${await response.text()}`)
    const reader = response.body?.getReader()
    let got: Uint8Array
    if (reader) {
      const chunks: Uint8Array[] = []
      let total = 0
      while (total < TEXT_BYTES) {
        const { done, value } = await reader.read()
        if (done) break
        chunks.push(value)
        total += value.length
      }
      void reader.cancel().catch(() => {})
      got = new Uint8Array(Math.min(total, TEXT_BYTES))
      let at = 0
      for (const chunk of chunks) {
        const room = got.length - at
        if (room <= 0) break
        got.set(chunk.subarray(0, room), at)
        at += Math.min(chunk.length, room)
      }
    } else {
      got = new Uint8Array(await response.arrayBuffer()).subarray(0, TEXT_BYTES)
    }
    // A file with NULs in its first screenful is not text, whatever its name.
    if (got.subarray(0, 8192).includes(0)) throw new Error('binary')
    return { text: new TextDecoder().decode(got), truncated: size > TEXT_BYTES }
  }

  // Load whichever file is open, and let go of the last one's bytes. Depends
  // on `opened` alone: everything else is read untracked, once.
  $effect(() => {
    if (!opened) return
    const f = untrack(() => (current ? { ...current } : undefined))
    if (!f) return
    let gone = false
    shown = { state: 'loading' }
    void (async () => {
      try {
        let next: Shown
        if (f.status.state === 'trashed') {
          next = {
            state: 'failed',
            reason: 'This file is in the trash. Put it back to look at it.',
            restorable: true,
          }
        } else if (f.status.state === 'missing') {
          next = { state: 'failed', reason: 'This file is missing from the folder.' }
        } else {
          const url = fileUrl(f.id, fileVersion(f))
          switch (previewOf(f)) {
            case 'image':
              next = { state: 'image', url }
              break
            case 'video':
            case 'audio':
              next = { state: 'media', url, video: previewOf(f) === 'video' }
              break
            case 'pdf':
              next = { state: 'pdf', data: new Uint8Array(await bytes(url)) }
              break
            case 'text': {
              const { text, truncated } = await head(url, f.size)
              next = { state: 'text', text, truncated }
              break
            }
            default: {
              // HEIC and the camera RAW formats: the JPEG most cameras embed,
              // as far as the vault drew one.
              const thumb = f.kind === 'image' ? thumbnailUrl(f) : null
              next = thumb
                ? {
                    state: 'image',
                    url: thumb,
                    note: 'A smaller copy: this kind of picture cannot be drawn in full here.',
                  }
                : { state: 'failed', reason: 'This kind of file cannot be looked at here.' }
            }
          }
        }
        if (!gone) shown = next
      } catch {
        if (!gone) shown = { state: 'failed', reason: COULD_NOT }
      }
    })()
    return () => {
      gone = true
    }
  })

  // The next picture is fetched while this one is looked at, so stepping
  // does not wait for it.
  $effect(() => {
    const at = position
    if (at < 0) return
    for (const near of [files[at + 1], files[at - 1]]) {
      if (near && near.status.state === 'present' && previewOf(near) === 'image') {
        new Image().src = fileUrl(near.id, fileVersion(near))
      }
    }
  })

  const name = $derived(current?.name || 'File')
  const facts = $derived.by(() => {
    if (!current) return ''
    const m = current.media
    return [
      formatSize(current.size),
      m.width && m.height ? `${m.width} × ${m.height}` : '',
      files.length > 1 ? `${position + 1} of ${files.length}` : '',
    ]
      .filter(Boolean)
      .join(' · ')
  })

  function go(by: 1 | -1) {
    const to = position + by
    const next = files[to]
    if (next) {
      openId = next.id
      onindex?.(to)
    }
    // The next page of a long list is on its way before it is wanted.
    if (to >= files.length - 3 && store.hasMore) void store.loadMore()
  }

  // ── Information ────────────────────────────────────────────────────

  let info = $state(false)
  // The panel describes whichever file is open.
  $effect(() => {
    if (info && opened && current) {
      const id = current.id
      untrack(() => store.focus(id))
    }
  })

  // ── Keys ───────────────────────────────────────────────────────────

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      // A field in the information panel gives up its edit first.
      if (e.defaultPrevented) return
      e.preventDefault()
      onclose()
      return
    }
    // A film's own controls seek with the arrows, a PDF zoomed wider than the
    // window scrolls sideways with them, and a field takes every key.
    if (
      e.target instanceof HTMLMediaElement ||
      e.target instanceof HTMLInputElement ||
      e.target instanceof HTMLTextAreaElement ||
      e.target instanceof HTMLSelectElement ||
      e.defaultPrevented ||
      e.ctrlKey ||
      e.metaKey ||
      e.altKey ||
      scrollsSideways(e.target)
    ) {
      return
    }
    if (e.key === 'ArrowLeft') go(-1)
    else if (e.key === 'ArrowRight') go(1)
    else if (e.key === 'i' || e.key === 'I') info = !info
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
    // A picture that would not draw falls back to its thumbnail, if it has
    // one, before giving up.
    const thumb = current && shown.state === 'image' ? thumbnailUrl(current) : null
    if (thumb && shown.state === 'image' && shown.url !== thumb) {
      shown = {
        state: 'image',
        url: thumb,
        note: 'A smaller copy: the picture could not be drawn in full.',
      }
      return
    }
    shown = { state: 'failed', reason }
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
  use:focusOnMount
  use:trapFocus
>
  <!-- The window's own title bar is under this, so the bar stands in for it
       as somewhere to drag the window by. -->
  <header class="bar" data-tauri-drag-region>
    <div class="what" data-tauri-drag-region>
      <span class="file" title={name} data-tauri-drag-region>{name}</span>
      <span class="facts" data-tauri-drag-region>{facts}</span>
    </div>
    {#if files.length > 1}
      <button
        class="tool back"
        aria-label="Previous"
        title="Previous (←)"
        disabled={position <= 0}
        onclick={() => go(-1)}
      >
        <Icon name="chevron" size={17} />
      </button>
      <button
        class="tool"
        aria-label="Next"
        title="Next (→)"
        disabled={position >= files.length - 1}
        onclick={() => go(1)}
      >
        <Icon name="chevron" size={17} />
      </button>
    {/if}
    {#if current}
      <button
        class="tool"
        class:on={current.favourite}
        aria-label={current.favourite ? 'Remove from favourites' : 'Add to favourites'}
        aria-pressed={current.favourite}
        title="Favourite"
        onclick={() => void store.toggleFavourite(current.id)}
      >
        <Icon name="star" size={16} filled={current.favourite} />
      </button>
    {/if}
    <button
      class="tool"
      class:on={info}
      aria-label="Information"
      aria-pressed={info}
      title="Information (I)"
      onclick={() => (info = !info)}
    >
      <Icon name="info" size={16} />
    </button>
    {#if store.local && current}
      <button
        class="tool"
        aria-label="Open in another program"
        title="Open in another program"
        onclick={() => void store.openExternally(current.id)}
      >
        <Icon name="share" size={16} />
      </button>
    {/if}
    {#if current}
      <button class="tool labelled" onclick={() => void store.exportFiles([current.id])}>
        <Icon name="download" size={16} />
        Save
      </button>
    {/if}
    <button class="tool" aria-label="Close" title="Close (Esc)" onclick={onclose}>
      <Icon name="close" size={17} />
    </button>
  </header>

  <div class="body">
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="stage" onclick={(e) => e.target === e.currentTarget && onclose()}>
      {#if shown.state === 'loading'}
        <p class="quiet">Loading…</p>
      {:else if shown.state === 'failed'}
        <div class="failed">
          {#if current}<span class="kind"
              ><Icon name={kindIcon(current.kind)} size={30} weight={1.3} /></span
            >{/if}
          <p>{shown.reason}</p>
          <div class="row">
            {#if shown.restorable && current}
              <button class="btn btn-primary" onclick={() => void store.restore([current.id])}>
                Put it back
              </button>
            {/if}
            {#if store.local && current && current.status.state === 'present'}
              <button class="btn" onclick={() => void store.openExternally(current.id)}>
                Open in another program
              </button>
            {/if}
            {#if current && current.status.state === 'present'}
              <button class="btn btn-primary" onclick={() => void store.exportFiles([current.id])}>
                Save
              </button>
            {/if}
          </div>
        </div>
      {:else if shown.state === 'image'}
        <div class="figure">
          <img
            class="picture"
            src={shown.url}
            alt={name}
            draggable="false"
            onerror={() => failed('This picture could not be shown here.')}
          />
          {#if shown.note}<p class="quiet small">{shown.note}</p>{/if}
        </div>
      {:else if shown.state === 'media'}
        {#if shown.video}
          <!-- A stranger's film comes with no captions to offer. -->
          <!-- svelte-ignore a11y_media_has_caption -->
          <video
            class="film"
            src={shown.url}
            controls
            preload="metadata"
            onerror={() => failed("This video can't be played here.")}
          ></video>
        {:else}
          <div class="recording">
            <Icon name="mic" size={28} />
            <span class="file">{name}</span>
            <audio
              src={shown.url}
              controls
              preload="metadata"
              onerror={() => failed("This recording can't be played here.")}
            ></audio>
          </div>
        {/if}
      {:else if shown.state === 'pdf'}
        <div class="reader">
          <PdfView data={shown.data} onfail={failed} />
        </div>
      {:else}
        <div class="paper scroll">
          <pre>{shown.text}</pre>
          {#if shown.truncated}
            <p class="cut">Only the first megabyte is shown. Save the file to read the rest.</p>
          {/if}
        </div>
      {/if}
    </div>

    {#if info}
      <aside class="info"><FileInspector onclose={() => (info = false)} /></aside>
    {/if}
  </div>
</div>

<style>
  /* A lightbox is dark whatever the theme: a picture is judged against it. The
     same literals the attachment viewer's lightbox uses. */
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
  .tool.on {
    background: rgb(255 255 255 / 0.16);
  }
  .tool.labelled {
    padding: 0 14px 0 10px;
  }
  .back :global(.icon) {
    transform: scaleX(-1);
  }

  .body {
    display: flex;
    min-height: 0;
  }
  .stage {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0 24px 24px;
  }
  /* The rail is the inspector's, drawn on its own surface over the dark. */
  .info {
    flex: none;
    display: flex;
    min-height: 0;
    margin: 0 12px 12px 0;
    border-radius: var(--radius-lg);
    overflow: hidden;
    color: var(--fg);
  }

  .quiet {
    margin: 0;
    color: rgb(255 255 255 / 0.7);
  }
  .quiet.small {
    font-size: var(--text-sm);
  }
  .failed {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 14px;
    max-width: 30em;
    text-align: center;
  }
  .failed p {
    margin: 0;
  }
  .failed .kind {
    display: flex;
    color: rgb(255 255 255 / 0.6);
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: 8px;
  }
  .failed .btn:not(.btn-primary) {
    color: inherit;
    border: 1px solid rgb(255 255 255 / 0.3);
  }
  .figure {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    max-width: 100%;
    max-height: 100%;
    min-height: 0;
  }
  .picture,
  .film {
    max-width: 100%;
    max-height: 100%;
    min-height: 0;
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
    padding: var(--sp-4) var(--sp-5);
    border-radius: 8px;
    overflow: auto;
    background: var(--card);
    color: var(--fg);
    box-shadow: 0 4px 24px rgb(0 0 0 / 0.45);
    user-select: text;
  }
  .paper pre {
    margin: 0;
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    tab-size: 4;
  }
  .cut {
    margin: var(--sp-4) 0 0;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
</style>
