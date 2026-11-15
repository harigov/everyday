<script lang="ts" module>
  // A file's picture: its thumbnail when one has been drawn; for a picture
  // with none, the file itself, when a webview can draw it; for a film with
  // none, a poster frame drawn here once and handed back to the vault; and
  // otherwise the icon for what kind of file it is.
  //
  // Lazy on every path. A grid of a few thousand of these is the point of the
  // app, and `loading="lazy"` leaves the ones off screen unfetched -- and,
  // being virtualised by its parent, mostly unmounted as well.
  //
  // The poster is the only thing here that does work. A webview can decode a
  // film and the vault cannot, so a film with no thumbnail is opened in a
  // hidden `<video>`, seeked to about a second in, drawn onto a canvas and
  // sent back as a JPEG (`api.setFileThumbnail`). At most `MOST_AT_ONCE` of
  // those run together, each film is tried once per session, and a file that
  // is not present is never opened: its bytes are not there to read. The
  // frame is kept here until the vault's own thumbnail arrives with the next
  // listing, so the tile does not go blank in between.

  import { api, fileUrl, fileVersion, thumbnailUrl } from '../lib/api'
  import { kindIcon } from '../lib/files'
  import type { FileEntry } from '../lib/types'

  const MOST_AT_ONCE = 2
  /** The longest edge of a poster. */
  const POSTER_EDGE = 480
  const GIVE_UP_MS = 15_000

  /** Films already tried, whatever came of it: once per session. */
  const tried = new Map<string, Promise<string | null>>()
  const waiting: (() => void)[] = []
  let running = 0

  function turn(): Promise<void> {
    if (running < MOST_AT_ONCE) {
      running += 1
      return Promise.resolve()
    }
    return new Promise((resolve) => waiting.push(resolve))
  }

  function done() {
    const next = waiting.shift()
    if (next) next()
    else running -= 1
  }

  /** The poster for a film with no thumbnail, as an address to draw, or null
   *  when it could not be made. The vault is told; its own thumbnail follows
   *  with the next listing. */
  function posterFor(file: FileEntry): Promise<string | null> {
    const known = tried.get(file.id)
    if (known) return known
    const made = (async () => {
      await turn()
      try {
        const jpeg = await frameOf(fileUrl(file.id, fileVersion(file)))
        if (!jpeg) return null
        void api.setFileThumbnail(file.id, jpeg.replace(/^data:[^,]*,/, '')).catch(() => {})
        return jpeg
      } catch {
        return null
      } finally {
        done()
      }
    })()
    tried.set(file.id, made)
    // A long session through a large library: forget the oldest answers.
    if (tried.size > 400) tried.delete(tried.keys().next().value as string)
    return made
  }

  /** One frame of the film at `url`, as a JPEG data address. */
  function frameOf(url: string): Promise<string | null> {
    return new Promise((resolve) => {
      const video = document.createElement('video')
      // In the page, but not to be seen: a webview does not always decode a
      // film that is not.
      video.style.cssText =
        'position:fixed;left:0;top:0;width:1px;height:1px;opacity:0;pointer-events:none'
      video.preload = 'metadata'
      video.muted = true
      video.playsInline = true
      // A frame drawn from another origin taints the canvas it is drawn on
      // unless the film was asked for with CORS.
      video.crossOrigin = 'anonymous'
      let settled = false
      const finish = (jpeg: string | null) => {
        if (settled) return
        settled = true
        clearTimeout(timer)
        video.removeAttribute('src')
        video.load()
        video.remove()
        resolve(jpeg)
      }
      const timer = setTimeout(() => finish(null), GIVE_UP_MS)
      video.onerror = () => finish(null)
      video.onloadedmetadata = () => {
        const length = Number.isFinite(video.duration) ? video.duration : 0
        // About a second in: past a fade from black, but inside a short clip.
        video.currentTime = length > 0 ? Math.min(1, length / 2) : 0
      }
      video.onseeked = () => {
        try {
          const w = video.videoWidth
          const h = video.videoHeight
          if (!w || !h) return finish(null)
          const scale = Math.min(1, POSTER_EDGE / Math.max(w, h))
          const canvas = document.createElement('canvas')
          canvas.width = Math.max(1, Math.round(w * scale))
          canvas.height = Math.max(1, Math.round(h * scale))
          const context = canvas.getContext('2d')
          if (!context) return finish(null)
          context.drawImage(video, 0, 0, canvas.width, canvas.height)
          finish(canvas.toDataURL('image/jpeg', 0.8))
        } catch {
          finish(null)
        }
      }
      document.body.append(video)
      video.src = url
    })
  }
</script>

<script lang="ts">
  import { untrack } from 'svelte'
  import Icon from './Icon.svelte'

  interface Props {
    file: FileEntry
    /** The edge of a square, in pixels. Absent, it fills what holds it. */
    size?: number
    fit?: 'cover' | 'contain'
  }
  let { file, size, fit = 'cover' }: Props = $props()

  /** Pictures a webview draws itself. HEIC and RAW are not among them: those
   *  have the vault's thumbnail or the icon. */
  const DRAWN = /^image\/(png|jpe?g|gif|webp|avif|bmp|svg\+xml)$/i

  const present = $derived(file.status.state === 'present')
  const thumb = $derived(thumbnailUrl(file))
  const original = $derived(
    file.kind === 'image' && present && DRAWN.test(file.mime)
      ? fileUrl(file.id, fileVersion(file))
      : null,
  )
  const wantsPoster = $derived(file.kind === 'video' && !thumb && present)

  /** The frame drawn here, until the vault's thumbnail replaces it. */
  let poster = $state<string | null>(null)
  /** The address that failed to load, so the next one gets its chance. */
  let broken = $state<string | null>(null)

  const src = $derived.by(() => {
    if (thumb && broken !== thumb) return thumb
    if (poster) return poster
    if (!thumb && original && broken !== original) return original
    return null
  })

  /** Compared by value, so a fresh copy of the same file does not start the
   *  poster over. */
  const id = $derived(file.id)

  $effect(() => {
    void id
    poster = null
    if (!wantsPoster) return
    let gone = false
    void posterFor(untrack(() => file)).then((url) => {
      if (!gone && url) poster = url
    })
    return () => {
      gone = true
    }
  })

  const edge = $derived(size ? `${size}px` : '100%')
  const extension = $derived.by(() => {
    const dot = file.name.lastIndexOf('.')
    return dot > 0 && file.name.length - dot <= 6 ? file.name.slice(dot + 1).toUpperCase() : ''
  })
  const icon = $derived(kindIcon(file.kind))
</script>

<div class="thumb" class:contain={fit === 'contain'} style:width={edge} style:height={edge}>
  {#if src}
    <img
      {src}
      alt=""
      loading="lazy"
      decoding="async"
      draggable="false"
      onerror={() => (broken = src)}
    />
  {:else}
    <span class="kind">
      <Icon name={icon} size={size && size < 72 ? Math.round(size * 0.5) : 30} weight={1.3} />
      {#if extension && (!size || size >= 72)}<span class="ext">{extension}</span>{/if}
    </span>
  {/if}
</div>

<style>
  .thumb {
    position: relative;
    display: grid;
    place-items: center;
    flex: none;
    min-width: 0;
    overflow: hidden;
    border-radius: inherit;
    background: var(--bg-sunken);
    color: var(--fg-faint);
  }
  img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
    /* A transparent picture on a pale tile reads as an empty one. */
    background: var(--bg-sunken);
  }
  .thumb.contain img {
    object-fit: contain;
  }
  .kind {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
  }
  .ext {
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.06em;
    color: var(--fg-subtle);
  }
</style>
