// A cursor asked for before there was anywhere to put it, once.
//
// The todo app's capture line and the library's kept byte-identical copies of
// this: a private slot for the view's focus method, a private flag for
// "somebody asked before the view was there to take it", and a `bind` that
// both registers the method and serves the flag if it had been raised. This
// is the ordinary case for the tray and the menu bar's quick actions -- the
// request arrives while some other app is on screen, and the view that owns
// the field mounts a frame later, with no reference to hand it down through.

export class FocusRequest {
  #handler: (() => void) | null = null
  #pending = false

  /** Register (or with `null`, retire) the field's focus method. */
  bind(handler: (() => void) | null): void {
    this.#handler = handler
    if (handler && this.#pending) {
      this.#pending = false
      handler()
    }
  }

  /** Put the cursor there now, or as soon as something is bound to do it. */
  request(): void {
    if (this.#handler) this.#handler()
    else this.#pending = true
  }
}
