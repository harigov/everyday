// The window's own minimise, maximise and close buttons.
//
// On Linux and Windows the window has no title bar of its own: the shell
// builds it undecorated (`lib.rs`), because the system's bar was a strip of
// nothing above the strip the app already draws -- a title, and three
// buttons that fit in the bar beside the search field. So the app draws
// those three itself, at the right end of `TopBar`, and on the screens with
// no top bar (the lock screen, setup) over their top-right corner.
//
// macOS keeps the system's controls: its window is decorated with an
// overlay title bar, so the traffic lights already sit over the left end of
// the top bar and drawing a second set would be wrong.
//
// In the browser preview (`npm run dev`) the buttons are drawn but do
// nothing, so the preview still shows the window the desktop build does.

const mac = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform ?? '')
const shell = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

type TauriWindow = import('@tauri-apps/api/window').Window

class WindowControls {
  /** Whether the app draws the controls itself -- everywhere but macOS. */
  readonly drawn = !mac
  /** Drives the middle button: maximise, or restore once maximised. */
  maximized = $state(false)

  #window: Promise<TauriWindow> | null = null

  /**
   * The Tauri window, imported on first use rather than at load, as
   * `api.ts` does: the module is not there in the browser preview, and the
   * buttons must not be what breaks the preview.
   */
  #current(): Promise<TauriWindow> | null {
    if (!shell) return null
    this.#window ??= import('@tauri-apps/api/window').then((m) => m.getCurrentWindow())
    return this.#window
  }

  /**
   * Follow the window's maximised state -- it changes from outside too, by
   * a double click on the bar, a keyboard shortcut, or the desktop snapping
   * the window to an edge. Returns the stop function for an `$effect`.
   */
  watch(): () => void {
    const current = this.#current()
    if (!current) return () => {}
    let stop: (() => void) | null = null
    let stopped = false
    void current.then(async (w) => {
      const sync = async () => {
        this.maximized = await w.isMaximized()
      }
      await sync()
      const unlisten = await w.onResized(() => void sync())
      if (stopped) unlisten()
      else stop = unlisten
    })
    return () => {
      stopped = true
      stop?.()
    }
  }

  minimize() {
    void this.#current()?.then((w) => w.minimize())
  }

  toggleMaximize() {
    void this.#current()?.then((w) => w.toggleMaximize())
  }

  /**
   * Ask to close, exactly as the system's own button would. The shell
   * decides from there -- it may hide to the tray instead, and otherwise
   * it holds the window for the interface to save first; see the
   * `CloseRequested` handler in `lib.rs`.
   */
  close() {
    void this.#current()?.then((w) => w.close())
  }
}

export const windowControls = new WindowControls()
