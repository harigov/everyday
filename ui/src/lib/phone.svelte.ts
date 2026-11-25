// Whether the interface is laid out for a phone, and the phone's back button.
//
// One question, asked of the screen and nowhere else: is it narrow, or short
// and not wide (a phone on its side)? The answer is `phone.on` for script and
// `data-phone` on <html> for CSS -- `:global([data-phone]) .thing { ... }` in
// a component, `[data-phone] .thing` in app.css -- so every rule a phone
// needs is keyed off the same breakpoint, and nothing compares widths of its
// own. It is the screen's question, not the shell's: a phone's browser on web
// access gets the phone's layout too, and the desktop app in a narrow window
// would. Which shell commands exist is `mobileShell` in `api.ts`, a separate
// matter.
//
// The breakpoint: every phone held upright is under 640 px wide (the widest,
// 440), and every tablet is over it; a phone on its side is under 500 px
// tall, which no laptop window that anybody works in is.

import { mobileShell } from './api'

const QUERY = '(max-width: 640px), (max-height: 500px) and (max-width: 960px)'

class Phone {
  /** Laid out for a phone. */
  on = $state(false)

  constructor() {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return
    const query = window.matchMedia(QUERY)
    const apply = () => {
      this.on = query.matches || (mobileShell && window.innerWidth <= 960)
      document.documentElement.toggleAttribute('data-phone', this.on)
    }
    apply()
    query.addEventListener('change', apply)
  }
}

export const phone = new Phone()

// ── The screen's edges and the keyboard ──────────────────────────────────
//
// How far in from each edge is clear of the status bar, the gesture bar and
// a notch is CSS's `env(safe-area-inset-*)`, which iOS gives in full. The
// Android shell measures them itself and says so -- its webview leaves some
// out -- through `window.KarmaYogaInsets` and a `karmayoga:insets` event
// (`MainActivity.kt`); they become `--edge-*` here, and app.css's
// `--safe-*` takes whichever is larger. `data-keyboard` on <html> is set
// while the on-screen keyboard is up, so what a phone keeps along the bottom
// -- the bar of apps -- can step aside for it.

interface Edges {
  top: number
  right: number
  bottom: number
  left: number
  keyboard: number
}

function applyEdges(edges: Edges) {
  const style = document.documentElement.style
  for (const side of ['top', 'right', 'bottom', 'left'] as const) {
    style.setProperty(`--edge-${side}`, `${edges[side]}px`)
  }
  document.documentElement.toggleAttribute('data-keyboard', edges.keyboard > 0)
}

if (typeof window !== 'undefined') {
  const bridge = (window as { KarmaYogaInsets?: { get(): string } }).KarmaYogaInsets
  if (bridge) {
    try {
      applyEdges(JSON.parse(bridge.get()) as Edges)
    } catch {
      // A shell too old to say: env() is all there is.
    }
    window.addEventListener('karmayoga:insets', (e) => applyEdges((e as CustomEvent<Edges>).detail))
  } else if (window.visualViewport) {
    // Elsewhere -- iOS, a phone's browser -- the keyboard is noticed by the
    // visible part of the page shrinking well short of the window.
    const viewport = window.visualViewport
    const watch = () => {
      const up = phone.on && window.innerHeight - viewport.height > 150
      document.documentElement.toggleAttribute('data-keyboard', up)
    }
    viewport.addEventListener('resize', watch)
  }
}

// ── Back ──────────────────────────────────────────────────────────────────
//
// Android's back button asks the webview to go back a page, and a page with
// no history to go back to leaves the app. So each layer the interface opens
// on a phone that back ought to close -- the sidebar's drawer, a sheet, an
// item opened from its list, the assistant over an app -- leaves an entry in
// the page's history while it is open, and going back takes the entry and
// closes the layer. Closed any other way (its own button, a choice made in
// it), it takes its entry back off quietly, so the next back closes the layer
// under it rather than nothing. iOS has no back button; its layers close
// from their own buttons, which is this module's quiet path.
//
// Only on a phone: the desktop's history is left alone.

interface Layer {
  id: number
  close: () => void
}

class Back {
  #layers: Layer[] = []
  #next = 0
  /** Going back that this module asked for itself, so `popstate` ignores it. */
  #quiet = 0

  constructor() {
    if (typeof window === 'undefined') return
    window.addEventListener('popstate', () => {
      if (this.#quiet > 0) {
        this.#quiet--
        return
      }
      this.#layers.pop()?.close()
    })
  }

  /**
   * A layer has opened: `close` closes it when back is pressed. Answers what
   * to call when it closes some other way.
   */
  open(close: () => void): () => void {
    if (!phone.on) return () => {}
    const id = ++this.#next
    this.#layers.push({ id, close })
    history.pushState({ layer: id }, '')
    return () => this.#closed(id)
  }

  /** How many layers are open, for a test. */
  get depth(): number {
    return this.#layers.length
  }

  #closed(id: number) {
    const at = this.#layers.findIndex((l) => l.id === id)
    if (at < 0) return
    // Whatever was opened over it goes with it: a sheet over an item closed
    // by its list closing is not left behind on the history.
    const gone = this.#layers.splice(at)
    this.#quiet++
    history.go(-gone.length)
  }
}

export const back = new Back()

/**
 * Call while a component sets up: for as long as `open()` is true, back
 * closes it by calling `close`. `open` is read in an effect, so a store's
 * field or a `$state` works.
 */
export function closesOnBack(open: () => boolean, close: () => void) {
  // Through a derived boolean, so going from one open item to another -- a
  // second thread picked from the list -- is not a close and an open, each
  // moving the history.
  const isOpen = $derived(open())
  $effect(() => {
    if (!isOpen) return
    return back.open(close)
  })
}
