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
  // A recording or a film is not played by the viewer: every one in Files goes
  // through the window's one player (`player.svelte.ts`), so what is playing
  // goes on when the viewer is closed or another file is stepped to, and no
  // two copies of the same sound can start. The viewer shows an audio file's
  // play and pause, where it is and a slider to move it, since the player's
  // own bar is under the viewer; for a film it offers **Play**, which closes
  // the viewer and opens the player's panel, where the picture is. **Up Next**
  // queues a recording, a film or a document.
  //
  // The information panel is the inspector, the same one as in the rail.
  //
  // A PDF is looked at here, and any document the reader can open -- a PDF, an
  // EPUB, Markdown, text, a web page -- can be read instead, with its contents
  // beside it and the assistant's rail; **Read** goes there. An EPUB cannot be
  // drawn here at all, so for one it is the only way on.

  import { untrack } from 'svelte'
  import { fileUrl, fileVersion, thumbnailUrl } from '../lib/api'
  import { isReadable } from '../lib/documents'
  import { formatSize, kindIcon, previewOf } from '../lib/files'
  import { files as store } from '../lib/files.svelte'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import { closesOnBack, phone } from '../lib/phone.svelte'
  import { player } from '../lib/player.svelte'
  import { formatDuration, playRequestForFile } from '../lib/podcast'
  import type { ContentRef, FileEntry } from '../lib/types'
  import { upnext } from '../lib/upnext.svelte'
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
    | { state: 'media'; video: boolean }
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
              next = { state: 'media', video: previewOf(f) === 'video' }
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

  /** Whether the reader can open what is shown: a document that is there, not
   *  one in the trash or missing from the folder. */
  const readable = $derived(current !== undefined && isReadable(current))

  /** Open it in the reader. The viewer comes down first, so the reader is not
   *  opened behind it, and the caller hears of it as of any other close. */
  function read() {
    if (!current || !readable) return
    const id = current.id
    onclose()
    void store.read(id)
  }

  // ── Playing, and Up Next ───────────────────────────────────────────

  /** What the player would play for this file: absent for anything that is
   *  not a recording or a film, or that is not there to be played. */
  const request = $derived(current ? playRequestForFile(current) : null)
  const what = $derived<ContentRef | null>(current ? { type: 'file', id: current.id } : null)
  /** This file is what the player has loaded, playing or paused. */
  const loaded = $derived(what !== null && player.isCurrent(what))
  const playing = $derived(what !== null && player.isPlaying(what))
  /** What Up Next takes from Files: recordings, films and documents. */
  const queueable = $derived(
    current !== undefined && current.status.state === 'present' && (request !== null || readable),
  )
  const queued = $derived(what !== null && upnext.isQueued(what))
  /** Where a recording is, for the slider and the clock beside it. */
  const length = $derived(player.durationMs || request?.durationMs || 0)

  function toggleQueued() {
    if (!what) return
    if (queued) void upnext.removeFor(what)
    else void upnext.add(what)
  }

  /** A recording: play it, or pause it, in the player. */
  function playOrPause() {
    if (!request) return
    if (loaded) player.toggle()
    else void player.play(request)
  }

  /** A film: it plays in the player's panel, which the viewer would cover, so
   *  the viewer comes down first -- as for Read -- and the panel opens. */
  function watch() {
    if (!request) return
    onclose()
    if (!loaded) void player.play(request)
    else if (!player.playing) player.resume()
    player.expanded = true
  }

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

  // On a phone the viewer is a layer back puts away, and the information over
  // it another.
  closesOnBack(
    () => true,
    () => onclose(),
  )
  closesOnBack(
    () => info && phone.on,
    () => (info = false),
  )

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
    // A slider (a recording's position) moves with the arrows, a PDF zoomed
    // wider than the window scrolls sideways with them, and a field takes
    // every key.
    if (
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
    else if ((e.key === 'r' || e.key === 'R') && readable) read()
    // A recording is played and paused with the space bar, as anywhere --
    // unless a button has the keyboard, which takes the space bar for itself.
    else if (
      e.key === ' ' &&
      shown.state === 'media' &&
      !shown.video &&
      !(e.target instanceof HTMLButtonElement)
    ) {
      e.preventDefault()
      playOrPause()
    }
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

<!-- What the viewer's file can be given: stepping to the next and the one
     before, reading, Up Next, a favourite, its information, saving it. A
     desktop has them along the top, and a phone in a dock along the bottom,
     under the thumb. -->
{#snippet controls()}
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
  {#if readable}
    <button
      class="tool labelled"
      aria-label="Read"
      title="Read it, with its contents beside it and the assistant (R)"
      onclick={read}
    >
      <Icon name="book" size={16} />
      <span class="lbl">Read</span>
    </button>
  {/if}
  {#if queueable}
    <button
      class="tool"
      class:on={queued}
      aria-label={queued ? 'Remove from Up Next' : 'Add to Up Next'}
      aria-pressed={queued}
      title={queued ? 'In Up Next: remove it' : 'Add to Up Next'}
      onclick={toggleQueued}
    >
      <Icon name="queue-add" size={16} />
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
    <button
      class="tool labelled"
      aria-label="Save"
      onclick={() => void store.exportFiles([current.id])}
    >
      <Icon name="download" size={16} />
      <span class="lbl">Save</span>
    </button>
  {/if}
{/snippet}

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
    {#if phone.on}
      <button class="tool back" aria-label="Close" onclick={onclose}>
        <Icon name="chevron" size={20} weight={2} />
      </button>
    {/if}
    <div class="what" data-tauri-drag-region>
      <span class="file" title={name} data-tauri-drag-region>{name}</span>
      <span class="facts" data-tauri-drag-region>{facts}</span>
    </div>
    {#if !phone.on}
      {@render controls()}
      <button class="tool" aria-label="Close" title="Close (Esc)" onclick={onclose}>
        <Icon name="close" size={17} />
      </button>
    {/if}
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
            {#if readable}
              <button class="btn btn-primary" onclick={read}>
                <Icon name="book" size={15} /> Read
              </button>
            {/if}
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
              <button
                class="btn"
                class:btn-primary={!readable}
                onclick={() => void store.exportFiles([current.id])}
              >
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
        <div class="recording">
          {#if shown.video}
            <!-- The picture a film has been given, if the catalog drew one;
                 the film itself is the player's. -->
            {#if current && thumbnailUrl(current)}
              <img class="picture poster" src={thumbnailUrl(current)} alt="" draggable="false" />
            {:else}
              <span class="kind"><Icon name="film" size={30} weight={1.3} /></span>
            {/if}
          {:else}
            <span class="kind"><Icon name="mic" size={28} /></span>
          {/if}
          <span class="file">{name}</span>
          {#if !request}
            <p class="quiet">This {shown.video ? 'video' : 'recording'} can't be played here.</p>
          {:else if shown.video}
            <div class="row">
              <button class="btn btn-primary" onclick={watch}>
                <Icon name="play" size={15} filled />
                {loaded ? 'Show the player' : 'Play'}
              </button>
              {#if current && store.local}
                <button class="btn" onclick={() => void store.openExternally(current.id)}>
                  Open in another program
                </button>
              {/if}
            </div>
          {:else}
            <div class="row">
              <button
                class="btn btn-primary"
                aria-label={playing ? 'Pause' : 'Play'}
                title="Play or pause (Space)"
                onclick={playOrPause}
              >
                <Icon name={playing ? 'pause' : 'play'} size={15} filled />
                {playing ? 'Pause' : 'Play'}
              </button>
              {#if loaded}
                <span class="clock">
                  {formatDuration(player.positionMs)}{length > 0
                    ? ` / ${formatDuration(length)}`
                    : ''}
                </span>
              {/if}
              {#if current && store.local}
                <button class="btn" onclick={() => void store.openExternally(current.id)}>
                  Open in another program
                </button>
              {/if}
            </div>
            {#if loaded && length > 0}
              <input
                class="seek"
                type="range"
                min="0"
                max={length}
                step="1000"
                value={Math.min(player.positionMs, length)}
                aria-label="Position"
                aria-valuetext={formatDuration(player.positionMs)}
                onchange={(e) => player.seek(Number(e.currentTarget.value))}
              />
            {/if}
            {#if loaded && player.error}
              <p class="quiet small" role="alert">{player.error}</p>
            {/if}
          {/if}
        </div>
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
      <aside class="info">
        <FileInspector onclose={() => (info = false)} onleave={onclose} />
      </aside>
    {/if}
  </div>

  {#if phone.on}
    <div class="dock" role="toolbar" aria-label="This file">{@render controls()}</div>
  {/if}
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
  .failed .btn:not(.btn-primary),
  .recording .btn:not(.btn-primary) {
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
  .picture {
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
  .recording .row {
    align-items: center;
  }
  .recording .kind {
    display: flex;
    color: rgb(255 255 255 / 0.6);
  }
  /* A film's own picture is shown smaller than a photograph would be: it is
     the doorway to the player, not what is looked at. */
  .poster {
    max-width: min(560px, 80vw);
    max-height: 50vh;
  }
  .clock {
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    color: rgb(255 255 255 / 0.7);
  }
  .seek {
    width: min(480px, 80vw);
    accent-color: currentColor;
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

  /* ── A phone ──────────────────────────────────────────────────────────
     The whole screen, edge to edge, under the status bar and above the
     gesture bar. The name is the header, with the way out at its left; what
     can be done is a dock of icons along the bottom, where a thumb is, and
     the information is a page over all of it. */
  /* Not see-through: the screen is the viewer's, whatever is under it. */
  :global([data-phone]) .backdrop {
    background: rgb(14 13 16);
  }
  :global([data-phone]) .viewer {
    grid-template-columns: minmax(0, 1fr);
    grid-template-rows: auto minmax(0, 1fr) auto;
  }
  :global([data-phone]) .bar {
    gap: var(--sp-1);
    padding: calc(var(--safe-top) + 4px) calc(var(--safe-right) + 12px) 4px
      calc(var(--safe-left) + 4px);
  }
  :global([data-phone]) .what {
    flex-direction: column;
    gap: 0;
  }
  :global([data-phone]) .file {
    font-size: var(--text-base);
  }
  :global([data-phone]) .tool {
    min-width: 44px;
    height: 44px;
  }
  /* A chevron pointing the way back, not on; the dock's own previous button
     is turned over by the same rule. */
  :global([data-phone]) .stage {
    padding: 0;
  }
  :global([data-phone]) .figure {
    width: 100%;
  }
  :global([data-phone]) .picture {
    border-radius: 0;
    box-shadow: none;
  }
  :global([data-phone]) .paper {
    border-radius: 0;
    padding: var(--sp-3) var(--sp-3);
  }
  :global([data-phone]) .recording,
  :global([data-phone]) .failed {
    padding: 0 var(--sp-4);
  }
  .dock {
    display: flex;
    align-items: center;
    justify-content: space-around;
    padding: var(--sp-1) calc(var(--safe-right) + var(--sp-2))
      calc(var(--safe-bottom) + var(--sp-1)) calc(var(--safe-left) + var(--sp-2));
    border-top: 1px solid rgb(255 255 255 / 0.12);
  }
  /* As many as eight across the smallest screen, so they share it. */
  :global([data-phone]) .dock .tool {
    flex: 1 1 0;
    min-width: 40px;
    max-width: 64px;
    padding: 0;
  }
  .dock .lbl {
    display: none;
  }
  /* The information, a page of its own over the viewer. */
  :global([data-phone]) .info {
    position: fixed;
    inset: 0;
    z-index: 66;
    margin: 0;
    padding-top: var(--safe-top);
    border-radius: 0;
    background: var(--bg-panel);
  }
</style>
