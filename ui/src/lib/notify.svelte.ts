// The notification service.
//
// One call, from anywhere:
//
//     import { notify } from './lib/notify.svelte'
//
//     notify.success('Calendar refreshed')
//     notify.error('Your journal could not be saved', {
//       body: 'The last few minutes of writing are still in the editor.',
//       reach: 'user',
//     })
//
// The caller says what happened and how far it needs to reach. It does not
// say *where* the message is drawn, because that answer changes with things
// no call site can know -- whether the window is in front of the user, and
// whether this machine will let the app post to the notification centre.
// `notify-policy.ts` makes that decision in one place, and this file carries
// it out.
//
// # Every platform this app runs on
//
// The application is a Tauri shell around a webview, so there are three
// worlds it can find itself in, and each gets the best channel it has:
//
//   * the packaged app on macOS, Linux and Windows -- and iOS and Android
//     when those shells exist -- posts through `tauri-plugin-notification`,
//     which is the platform's own notification centre. Nothing here is
//     conditioned on the operating system: the plugin is the portability
//     layer, and adding a mobile target adds no code in this file.
//   * the interface running in a plain browser on its mock backend
//     (`make ui`) posts through the web Notification API, so the routing
//     above can actually be exercised while the interface is being designed.
//   * anywhere the answer is no -- permission refused, an old webview, a
//     Linux session with no notification daemon -- the toast stack is the
//     whole service, and every notification still arrives. Degrading is the
//     normal case, not the error case.

import { isMock, onShellNotification } from './api'
import { channelFor, push, toToast, type NotifySpec, type Toast } from './notify-policy'

export type { NotifyLevel, NotifyReach, NotifySpec, Toast } from './notify-policy'

/**
 * A way to reach the operating system's own notifications.
 *
 * Both implementations below are lazy about permission on purpose. Asking at
 * startup produces a system prompt over a window the user has not looked at
 * yet, for a capability they have not yet been given a reason to want, and
 * the reflex answer to that prompt is "no" -- which is a decision that
 * sticks. The ask happens the first time something genuinely needs to reach
 * a person who is not looking.
 */
interface OsChannel {
  /** Permission, requested once and remembered. False if it cannot be had. */
  ready(): Promise<boolean>
  send(toast: Toast): Promise<void>
}

const NO_OS: OsChannel = {
  ready: () => Promise.resolve(false),
  send: () => Promise.resolve(),
}

async function tauriChannel(): Promise<OsChannel> {
  const plugin = await import('@tauri-apps/plugin-notification')
  let granted: boolean | null = null
  return {
    async ready() {
      if (granted !== null) return granted
      granted = await plugin.isPermissionGranted()
      if (!granted) granted = (await plugin.requestPermission()) === 'granted'
      return granted
    },
    send(toast) {
      plugin.sendNotification({ title: toast.title, body: toast.body ?? undefined })
      return Promise.resolve()
    },
  }
}

function webChannel(): OsChannel {
  if (!('Notification' in window)) return NO_OS
  let granted: boolean | null = null
  return {
    async ready() {
      if (granted !== null) return granted
      if (Notification.permission === 'denied') return (granted = false)
      granted =
        Notification.permission === 'granted' ||
        (await Notification.requestPermission()) === 'granted'
      return granted
    },
    send(toast) {
      new Notification(toast.title, { body: toast.body ?? undefined, tag: toast.key ?? undefined })
      return Promise.resolve()
    },
  }
}

// Resolved once, at module load, and awaited at the point of use. The import
// is eager because a dynamic import in the middle of handling a failed save
// is a network round trip in development and a chunk load in production --
// either can fail, and the notification that a save is failing is the last
// one that should be lost to a loading error.
const osChannel: Promise<OsChannel> = isMock
  ? Promise.resolve(webChannel())
  : tauriChannel().catch(() => NO_OS)

/** Options a caller may add to the level shorthands. */
type Options = Omit<NotifySpec, 'title' | 'level'>

class Notifications {
  /** What the toast stack is showing, oldest first. */
  toasts = $state<Toast[]>([])

  #nextId = 1
  #timers = new Map<number, ReturnType<typeof setTimeout>>()
  #listening = false

  /**
   * Raise a notification. The general form; most call sites want one of the
   * four shorthands below.
   *
   * Returns the toast's id, which `dismiss` takes -- worth keeping for a
   * notification that describes an ongoing condition the caller will later
   * find to be over. Zero when the notification went to the operating system
   * and nowhere else, since there is nothing on screen to dismiss.
   *
   * Deliberately not `async`. This is called from `catch` blocks and from
   * reactive code all over the interface, and a notification service that
   * makes its callers think about awaiting it -- or that produces an
   * unhandled rejection when they do not -- will not get used.
   */
  post(spec: NotifySpec): number {
    const channel = channelFor(spec, {
      focused: focused(),
      // Routing must be synchronous, so this asks what permission is *known*
      // to be granted rather than awaiting the ask. The first `reach: 'user'`
      // notification of a session therefore shows as a toast while the
      // prompt is still up; that is the right way round, because the
      // alternative is a message held back behind a dialog the user has not
      // answered yet.
      osReady: osGranted,
    })

    if (channel === 'os' || channel === 'both') {
      void deliverToOs(spec)
      if (channel === 'os') return 0
    }

    const toast = toToast(spec, this.#nextId++)
    // A keyed toast replaces one that may have a dwell timer running against
    // its old id. Clearing every timer that is no longer on screen is the
    // only way to be sure of that without tracking the replacement by hand.
    this.toasts = push(this.toasts, toast)
    this.#sweepTimers()
    if (toast.timeout !== null) {
      this.#timers.set(
        toast.id,
        setTimeout(() => this.dismiss(toast.id), toast.timeout),
      )
    }
    return toast.id
  }

  info(title: string, opts: Options = {}): number {
    return this.post({ ...opts, title, level: 'info' })
  }

  success(title: string, opts: Options = {}): number {
    return this.post({ ...opts, title, level: 'success' })
  }

  warn(title: string, opts: Options = {}): number {
    return this.post({ ...opts, title, level: 'warning' })
  }

  error(title: string, opts: Options = {}): number {
    return this.post({ ...opts, title, level: 'error' })
  }

  /**
   * Cancel the dwell timers of toasts that are no longer on screen.
   *
   * A keyed toast replaces its predecessor rather than dismissing it, so the
   * predecessor's timer would otherwise still be pending -- and would fire
   * `dismiss` on an id that no longer exists. Harmless today, and exactly the
   * kind of leak that stops being harmless once toasts carry anything heavier
   * than a string.
   */
  #sweepTimers() {
    const live = new Set(this.toasts.map((t) => t.id))
    for (const [id, timer] of this.#timers) {
      if (live.has(id)) continue
      clearTimeout(timer)
      this.#timers.delete(id)
    }
  }

  /** Take one toast off the screen. Safe to call for an id already gone. */
  dismiss(id: number) {
    const timer = this.#timers.get(id)
    if (timer) clearTimeout(timer)
    this.#timers.delete(id)
    this.toasts = this.toasts.filter((t) => t.id !== id)
  }

  /**
   * Take everything off the screen.
   *
   * Called when the vault locks: a toast can be quoting an entry title or a
   * project name, and those are decrypted contents of the vault. They must
   * go when the key does, for the same reason the search index does.
   */
  clear() {
    for (const timer of this.#timers.values()) clearTimeout(timer)
    this.#timers.clear()
    this.toasts = []
  }

  /** Run a toast's action, then take it off the screen. */
  act(toast: Toast) {
    toast.action?.run()
    this.dismiss(toast.id)
  }

  /**
   * Start relaying notifications raised by the Rust shell.
   *
   * The shell does background work the interface never sees -- refreshing a
   * subscribed calendar on a timer, most of all -- and before this there was
   * no way for it to say anything about it except a log line. It emits, this
   * consumes, and the routing above applies to a notification from the shell
   * exactly as it does to one from a component. That is the point of having
   * one service rather than two: the rule about not putting a banner over a
   * focused window is written once.
   */
  listenToShell() {
    if (this.#listening) return
    this.#listening = true
    onShellNotification((n) =>
      this.post({
        title: n.title,
        // `Option<String>` crosses the bridge as null; the spec's absent
        // field is `undefined`. Normalising here keeps every other call site
        // free of a distinction that only exists because of serde.
        body: n.body ?? undefined,
        level: n.level,
        reach: n.reach,
        key: n.key ?? undefined,
      }),
    )
  }
}

/** Is the window in front of the user? */
function focused(): boolean {
  return document.visibilityState === 'visible' && document.hasFocus()
}

/**
 * Whether the OS channel is known to be usable, cached for the synchronous
 * routing decision in `post`. Starts false and is settled by the first
 * `reach: 'user'` notification of the session.
 */
let osGranted = false

async function deliverToOs(spec: NotifySpec) {
  try {
    const channel = await osChannel
    if (!(await channel.ready())) {
      osGranted = false
      return
    }
    osGranted = true
    await channel.send(toToast(spec, 0))
  } catch {
    // A notification that cannot be delivered is not worth an error about a
    // notification. The toast copy has already been queued by the router in
    // every case except a plain `os`, and one lost banner is a better outcome
    // than an unhandled rejection in a `catch` block somewhere else.
    osGranted = false
  }
}

/**
 * The service. One instance, imported anywhere:
 *
 *     import { notify } from '../lib/notify.svelte'
 */
export const notify = new Notifications()

/**
 * Ask for the OS notification permission ahead of the first time it is
 * needed.
 *
 * Not called at startup, deliberately -- see `OsChannel`. This exists for
 * the moment a user turns something on that will need it, so the prompt
 * arrives attached to a thing they just asked for.
 */
export async function requestOsNotifications(): Promise<boolean> {
  try {
    osGranted = await (await osChannel).ready()
  } catch {
    osGranted = false
  }
  return osGranted
}
