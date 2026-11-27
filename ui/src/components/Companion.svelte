<script lang="ts">
  // Young Jatayu, drawn into a canvas: the header's, or the preview in
  // Settings.
  //
  // What this owns is the canvas's life rather than the bird's -- the scene
  // is `companion-scene.ts`, loaded the first time a bird is drawn so the
  // rest of the interface does not carry three.js for a header that might
  // have no bird in it. The rest is not drawing when nobody can see it:
  //
  //   - off screen, or in a hidden window, the loop stops, because a header
  //     ornament that kept a GPU busy behind a minimised window would be the
  //     most expensive thing in the application;
  //   - with nothing to do -- idle, listening, asleep -- it settles and is
  //     then held on one frame until something moves it (see `parks`), and
  //     at once in a window that has lost the focus; a turn at work keeps
  //     moving, since the bird is how that shows from across the room;
  //   - with reduced motion asked for, the bird is drawn once per act, still,
  //     in the pose that act ends up in, so it still says what it is doing
  //     without moving to say it;
  //   - with no WebGL at all, nothing is drawn and the space closes up.
  //
  // And two ways it answers back: it watches the pointer, and it can be
  // petted -- a click, or Enter on it -- for a wiggle and some hearts.
  // Neither does anything else; the bird is not a button for anything but
  // itself.
  //
  // The scene is built once the window has drawn what it opened to and has a
  // quiet moment, or as soon as the pointer comes onto the bird: three.js and
  // a WebGL context are the heaviest thing a window starts, and an ornament
  // can wait for the content. Until then the canvas is empty, as it always
  // was while the scene loaded.

  import { onMount } from 'svelte'
  import type { Act, Look } from '../lib/companion'
  import type { CompanionScene } from '../lib/companion-scene'

  interface Props {
    look: Look
    act: Act
    /** In CSS pixels. */
    width: number
    height: number
    /** Who it is, for the "Pet …" a screen reader hears. */
    name?: string
  }

  let { look, act, width, height, name = 'Jatayu' }: Props = $props()

  let canvas = $state<HTMLCanvasElement | null>(null)
  let scene = $state<CompanionScene | null>(null)
  let failed = $state(false)

  const still = window.matchMedia('(prefers-reduced-motion: reduce)')

  /** How far, in CSS pixels, counts as "all the way over there". */
  const REACH = 320
  /** How long after the window loses the focus it counts as nobody there. */
  const UNFOCUSED_MS = 3000
  /** Where the webview offers no idle callback (WebKit), how long to wait instead. */
  const QUIET_MS = 300

  /** Mounted, and not yet torn down. */
  let alive = true
  /** The scene has been asked for; see `begin`. */
  let begun = false
  /** In the viewport, by the IntersectionObserver. */
  let onScreen = false
  /** The window has the focus, or lost it only a moment ago. */
  let focused = document.hasFocus()
  let observer: IntersectionObserver | null = null
  /** The wait for a quiet moment: frames, then an idle callback or a timer. */
  let frames = 0
  let idle = 0
  let quiet: ReturnType<typeof setTimeout> | null = null

  /** Draw, hold still, or stop, for how things stand now. */
  function run() {
    if (!scene) return
    scene.attend(focused)
    if (still.matches) scene.still()
    else if (onScreen && !document.hidden) scene.play()
    else scene.pause()
  }

  function stopWaiting() {
    cancelAnimationFrame(frames)
    frames = 0
    if (idle) cancelIdleCallback(idle)
    idle = 0
    if (quiet) clearTimeout(quiet)
    quiet = null
  }

  /** Build the scene, now: see the note at the top for when. */
  function begin() {
    if (begun || !alive) return
    begun = true
    stopWaiting()
    void import('../lib/companion-scene').then(({ createCompanionScene }) => {
      if (!alive || !canvas) return
      const accent = getComputedStyle(canvas).getPropertyValue('--accent')
      const made = createCompanionScene(canvas, { look, act, accent })
      if (!made) {
        failed = true
        return
      }
      made.resize(width, height)
      scene = made
      observer = new IntersectionObserver((entries) => {
        onScreen = entries.some((e) => e.isIntersecting)
        run()
      })
      observer.observe(canvas)
      run()
    })
  }

  onMount(() => {
    let pending: PointerEvent | null = null
    let raf = 0
    let blurred: ReturnType<typeof setTimeout> | null = null

    // Where the pointer is, relative to the bird, read once a frame at most:
    // a pointer moves far more often than the bird draws. Not at all while it
    // is stopped or still, when there is no head to turn.
    function onPointer(event: PointerEvent) {
      pending = event
      if (raf || !scene?.playing) return
      raf = requestAnimationFrame(() => {
        raf = 0
        if (!pending || !canvas || !scene || still.matches) return
        const box = canvas.getBoundingClientRect()
        const x = (pending.clientX - (box.left + box.width / 2)) / REACH
        const y = (pending.clientY - (box.top + box.height * 0.45)) / REACH
        scene.lookAt(x, y)
      })
    }

    // Losing the focus to a frame inside this window -- a mail's body -- is not
    // losing it, which is why the window is asked again when the wait is up.
    function onBlur() {
      if (blurred) clearTimeout(blurred)
      blurred = setTimeout(() => {
        blurred = null
        focused = document.hasFocus()
        run()
      }, UNFOCUSED_MS)
    }
    function onFocus() {
      if (blurred) clearTimeout(blurred)
      blurred = null
      focused = true
      run()
    }

    // Two frames, so what the window opened to has been drawn; then a quiet
    // moment, or a short wait where there is no way to ask for one.
    frames = requestAnimationFrame(() => {
      frames = requestAnimationFrame(() => {
        frames = 0
        if (typeof requestIdleCallback === 'function') {
          idle = requestIdleCallback(begin, { timeout: 1500 })
        } else {
          quiet = setTimeout(begin, QUIET_MS)
        }
      })
    })

    document.addEventListener('visibilitychange', run)
    still.addEventListener('change', run)
    window.addEventListener('pointermove', onPointer, { passive: true })
    window.addEventListener('blur', onBlur)
    window.addEventListener('focus', onFocus)
    return () => {
      alive = false
      stopWaiting()
      if (blurred) clearTimeout(blurred)
      cancelAnimationFrame(raf)
      observer?.disconnect()
      document.removeEventListener('visibilitychange', run)
      still.removeEventListener('change', run)
      window.removeEventListener('pointermove', onPointer)
      window.removeEventListener('blur', onBlur)
      window.removeEventListener('focus', onFocus)
      scene?.dispose()
      scene = null
    }
  })

  /** A pat. Held still, it shows as a happy face until the pat is over. */
  let unpet: ReturnType<typeof setTimeout> | null = null
  function pet() {
    if (!scene) return
    scene.pet()
    if (!still.matches) return
    scene.still()
    if (unpet) clearTimeout(unpet)
    unpet = setTimeout(() => scene?.still(), 1800)
  }

  $effect(() => {
    // Read out of `look` by its field, so a change of plumage is noticed
    // whether the caller hands in a new object or edits the old one.
    const next = { plumage: look.plumage }
    scene?.setLook(next)
    if (still.matches) scene?.still()
  })

  $effect(() => {
    scene?.setAct(act)
    if (still.matches) scene?.still()
  })

  $effect(() => {
    scene?.resize(width, height)
  })

  $effect(() => () => {
    if (unpet) clearTimeout(unpet)
  })
</script>

{#if !failed}
  <button
    class="companion"
    style="width: {width}px; height: {height}px"
    data-act={act}
    title="Pet {name}"
    aria-label="Pet {name}"
    onclick={pet}
    onpointerenter={begin}
  >
    <canvas bind:this={canvas} style="width: {width}px; height: {height}px"></canvas>
  </button>
{/if}

<style>
  .companion {
    display: block;
    flex: none;
    padding: 0;
    border: 0;
    border-radius: var(--radius);
    background: none;
    cursor: pointer;
  }
  .companion:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  canvas {
    display: block;
  }
</style>
