// An app in a window of its own.
//
// The main window shows every app, one at a time. A second window can show
// one of them alone -- the todo list beside the editor you are writing in --
// opened from the app's button in the app bar or from the palette. It is the
// same interface in a second webview, with its own stores, talking to the
// same vault; see `crates/everyday-app/src/windows.rs` for the shell's half.
//
// Which window this is is known before anything is drawn, from the window's
// own label (`app-todo`), so every module can read it as a constant rather
// than wait for a store. What follows from it:
//
//   - the app bar, the assistant's rail and the meeting banner are not drawn;
//   - the tray, the badge, shell notifications and the palette's hotkey are
//     the main window's alone -- one of each per process;
//   - anything that is not this window's app -- "Go to Mail", Settings, a
//     link from a task to its event -- is shown in the main window instead
//     (`api.showInMain`), which comes forward to show it. The app, that is:
//     what a link meant to open there has no store here to open it in, so
//     `app.goTo` answers false and the caller stops rather than open it
//     unseen in this window. A main window on its lock screen shows it once
//     it is unlocked;
//   - which app was last open is not remembered from here, so the main
//     window comes back where it was left.
//
// Only for a vault in this process. Each window's writes reach the others
// because the shell stamps them with the window they came from, and a vault
// on another computer stamps them with this machine instead -- and sends
// nothing back to the machine that wrote. So the way to open one is not
// offered on a remote vault, and the shell refuses it there.
//
// In the browser preview, `?window=todo` stands in for the label.

import { APP_ORDER, type Section } from './apps'

/** Every app window's label starts with this -- see `windows.rs`'s `PREFIX`. */
const PREFIX = 'app-'

function sectionOf(raw: string | null | undefined): Section | null {
  return APP_ORDER.includes(raw as Section) ? (raw as Section) : null
}

const shell = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

async function find(): Promise<Section | null> {
  if (shell) {
    const { getCurrentWebviewWindow } = await import('@tauri-apps/api/webviewWindow')
    const label = getCurrentWebviewWindow().label
    return label.startsWith(PREFIX) ? sectionOf(label.slice(PREFIX.length)) : null
  }
  if (typeof location === 'undefined') return null
  return sectionOf(new URLSearchParams(location.search).get('window'))
}

/** The app this window shows alone, or `null` in the main window. */
export const pinned: Section | null = await find()

/**
 * What this window's own writes come back stamped with, so it can tell them
 * from everybody else's -- see `live.svelte.ts`, and `ctx_for` in the shell.
 * The main window's are `local`, as they always were.
 */
export const ownOrigin: string = pinned
  ? `window:${PREFIX}${pinned}`
  : // A browser on web access: the server already leaves this session's own
    // writes out of its stream, and `local` is the desktop window's -- whose
    // writes this page must not mistake for its own.
    shell || import.meta.env.DEV
    ? 'local'
    : 'web'

/**
 * Can an app be opened in a window of its own from here? Not from a window
 * that is already one app's. (In the browser preview it opens a tab, on the
 * preview's own sample vault.)
 */
export const canOpenWindows: boolean = pinned === null
