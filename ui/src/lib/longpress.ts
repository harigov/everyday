// A long press is a right-click, on iOS too.
//
// Every list's row menu, the calendar's and mail's among them, opens on the
// `contextmenu` event, which a desktop raises for a right-click and Android's
// webview for a finger held still on something. iOS's WKWebView raises it
// for neither, so on an iPhone those menus -- and what is only in them --
// would be out of reach. This raises it there: a single finger held still
// for half a second on an element that is not a text field becomes a
// `contextmenu` event at that point, and the tap that would follow the lift
// is swallowed, as Android swallows it.
//
// Only on iOS: anywhere else the platform raises the event itself, and doing
// it twice would open a menu over a menu.

const HOLD_MS = 500
/** How far a finger may drift and still be holding, not scrolling. */
const SLOP_PX = 10

/** An iPhone or an iPad. An iPad's webview calls itself a Mac; a Mac has no
 *  touch screen. */
export function isIOS(): boolean {
  const agent = navigator.userAgent
  return /iPhone|iPad|iPod/.test(agent) || (/Macintosh/.test(agent) && navigator.maxTouchPoints > 1)
}

export function installLongPress(target: Document = document) {
  let timer: ReturnType<typeof setTimeout> | null = null
  let start: { x: number; y: number } | null = null
  let fired = false

  const cancel = () => {
    if (timer) clearTimeout(timer)
    timer = null
    start = null
  }

  target.addEventListener(
    'touchstart',
    (e) => {
      cancel()
      fired = false
      const touch = e.touches[0]
      if (e.touches.length !== 1 || !touch) return
      const element = e.target
      if (!(element instanceof Element)) return
      // A field's own long press selects text and shows the system's menu.
      if (element.closest('input, textarea, [contenteditable="true"]')) return
      start = { x: touch.clientX, y: touch.clientY }
      timer = setTimeout(() => {
        timer = null
        if (!start) return
        fired = true
        element.dispatchEvent(
          new MouseEvent('contextmenu', {
            bubbles: true,
            cancelable: true,
            clientX: start.x,
            clientY: start.y,
            button: 2,
          }),
        )
      }, HOLD_MS)
    },
    { passive: true },
  )

  target.addEventListener(
    'touchmove',
    (e) => {
      const touch = e.touches[0]
      if (!start || !touch) return
      if (Math.hypot(touch.clientX - start.x, touch.clientY - start.y) > SLOP_PX) cancel()
    },
    { passive: true },
  )

  target.addEventListener('touchend', cancel, { passive: true })
  target.addEventListener('touchcancel', cancel, { passive: true })

  // The lift after a menu opened is not also a tap on what was held.
  target.addEventListener(
    'click',
    (e) => {
      if (!fired) return
      fired = false
      e.preventDefault()
      e.stopPropagation()
    },
    true,
  )
}
