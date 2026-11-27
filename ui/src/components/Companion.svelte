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
  //   - with reduced motion asked for, the bird is drawn once per act, still,
  //     in the pose that act ends up in, so it still says what it is doing
  //     without moving to say it;
  //   - with no WebGL at all, nothing is drawn and the space closes up.
  //
  // And two ways it answers back: it watches the pointer, and it can be
  // petted -- a click, or Enter on it -- for a wiggle and some hearts.
  // Neither does anything else; the bird is not a button for anything but
  // itself.

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

  onMount(() => {
    let alive = true
    let onScreen = false
    let observer: IntersectionObserver | null = null
    let pending: PointerEvent | null = null
    let raf = 0

    function run() {
      if (!scene) return
      if (still.matches) scene.still()
      else if (onScreen && !document.hidden) scene.play()
      else scene.pause()
    }

    // Where the pointer is, relative to the bird, read once a frame at most:
    // a pointer moves far more often than the bird draws.
    function onPointer(event: PointerEvent) {
      pending = event
      if (raf) return
      raf = requestAnimationFrame(() => {
        raf = 0
        if (!pending || !canvas || !scene || still.matches) return
        const box = canvas.getBoundingClientRect()
        const x = (pending.clientX - (box.left + box.width / 2)) / REACH
        const y = (pending.clientY - (box.top + box.height * 0.45)) / REACH
        scene.lookAt(x, y)
      })
    }

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

    document.addEventListener('visibilitychange', run)
    still.addEventListener('change', run)
    window.addEventListener('pointermove', onPointer, { passive: true })
    return () => {
      alive = false
      cancelAnimationFrame(raf)
      observer?.disconnect()
      document.removeEventListener('visibilitychange', run)
      still.removeEventListener('change', run)
      window.removeEventListener('pointermove', onPointer)
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
