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
import type { Section } from './apps'

const QUERY = '(max-width: 640px), (max-height: 500px) and (max-width: 960px)'

class Phone {
  /** Laid out for a phone. */
  on = $state(false)

  constructor() {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return
    const query = window.matchMedia(QUERY)
    // Only on a change: a page starts without `data-phone`, so a desktop
    // never touches <html> for it.
    const apply = () => {
      const on = query.matches || (mobileShell && window.innerWidth <= 960)
      if (on === this.on) return
      this.on = on
      document.documentElement.toggleAttribute('data-phone', on)
    }
    apply()
    query.addEventListener('change', apply)
    // The phone shell's own clause is a width the query does not ask about:
    // a tablet or an unfolded phone turned across 960 px changes it without
    // the query's answer changing.
    if (mobileShell) window.addEventListener('resize', apply)
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
  /** Its entry is on the history. A layer opened while a traversal is still
   *  landing is not yet. */
  pushed: boolean
}

class Back {
  #layers: Layer[] = []
  #next = 0
  /** Going back that this module asked for itself, so `popstate` ignores it. */
  #quiet = 0
  /** Layers opened while that was still landing: their entries are pushed
   *  when it has, in the order they were opened. */
  #waiting: Layer[] = []
  #timer: ReturnType<typeof setTimeout> | undefined
  #listening = false

  /** From the first layer on: a desktop never listens. */
  #listen() {
    if (this.#listening) return
    this.#listening = true
    window.addEventListener('popstate', () => {
      if (this.#quiet > 0) {
        this.#quiet--
        if (this.#quiet === 0) this.#landed()
        return
      }
      // The entry gone is the topmost layer's that has one: a layer still
      // waiting for its entry to be pushed is not what back took away.
      for (let at = this.#layers.length - 1; at >= 0; at--) {
        const layer = this.#layers[at]
        if (!layer?.pushed) continue
        this.#layers.splice(at, 1)
        layer.close()
        return
      }
    })
  }

  /**
   * A layer has opened: `close` closes it when back is pressed. Answers what
   * to call when it closes some other way.
   */
  open(close: () => void): () => void {
    if (!phone.on) return () => {}
    this.#listen()
    const layer: Layer = { id: ++this.#next, close, pushed: false }
    this.#layers.push(layer)
    // One layer closing as another opens -- a menu's item raising a dialog --
    // is the usual way in here, and `go(-1)` for the first only takes effect
    // after the push for the second: the history then lands one entry too
    // far back, and the next back leaves the app. So the second waits for the
    // first to land.
    if (this.#quiet > 0) this.#waiting.push(layer)
    else this.#push(layer)
    return () => this.#closed(layer.id)
  }

  /** How many layers are open, for a test. */
  get depth(): number {
    return this.#layers.length
  }

  #push(layer: Layer) {
    history.pushState({ layer: layer.id }, '')
    layer.pushed = true
  }

  #landed() {
    clearTimeout(this.#timer)
    // Not those closed while they waited.
    for (const layer of this.#waiting.splice(0)) {
      if (this.#layers.includes(layer)) this.#push(layer)
    }
  }

  #closed(id: number) {
    const at = this.#layers.findIndex((l) => l.id === id)
    if (at < 0) return
    // Only this layer: one opened over it that closes with it -- a sheet in
    // an item put away -- says so itself as its component goes, and one that
    // stays -- a dialog opened from the drawer as the drawer slides away --
    // keeps its entry, so the next back closes it rather than the app. The
    // entries are alike, so taking any one off the end is taking this one.
    const [layer] = this.#layers.splice(at, 1)
    if (!layer?.pushed) return
    this.#quiet++
    history.go(-1)
    // A traversal that cannot happen -- already at the first entry -- never
    // says so; the layers waiting on it are not left waiting for ever.
    clearTimeout(this.#timer)
    this.#timer = setTimeout(() => {
      this.#quiet = 0
      this.#landed()
    }, 500)
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

/**
 * Tell the Android shell whether the theme on screen is dark, so the status
 * bar's clock and icons over the page are light over it and dark over a
 * light one (`MainActivity.kt`). Nothing elsewhere: iOS and a browser follow
 * the page's own `color-scheme`.
 */
export function tellShellTheme(dark: boolean) {
  if (typeof window === 'undefined') return
  ;(window as { KarmaYogaWindow?: { setDark(dark: boolean): void } }).KarmaYogaWindow?.setDark(dark)
}

// ── An app's list, and what was opened from it ───────────────────────────
//
// A desktop app shows its list and the thing opened from it side by side. A
// phone has room for one, so it shows the list until something in it is
// opened, then that thing over the whole screen with a way back
// (`PhoneBack`), and back -- the button, Android's -- shows the list again.
// Whether an app is showing the opened thing is kept here, one flag per app,
// so leaving an app and coming back finds it as it was; `App.svelte` makes
// back close whichever is open. An app's view decides what "opened" means
// (an entry, a thread, an event) and calls `open` when the person opens one
// and `close` when it goes. On a desktop the flag is kept and ignored.

class Details {
  #open = $state<Partial<Record<Section, boolean>>>({})

  /** The app is showing something opened from its list, over it. */
  isOpen(section: Section): boolean {
    return phone.on && this.#open[section] === true
  }

  open(section: Section) {
    this.#open[section] = true
  }

  close(section: Section) {
    this.#open[section] = false
  }
}

export const details = new Details()
