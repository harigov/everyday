// Settings → Connections: the MCP servers the assistant may use.
//
// One read answers the list and the presets; every write answers the view of
// what it touched, which replaces that connection in the list -- so the pane
// is always drawn from what the vault says, never from what it asked for.
// See `docs/plans/connections.md`.
//
// # Secrets only go one way
//
// A token typed into the add form is sent with the add and forgotten: the
// views that come back say which secrets are set and nothing more, so there
// is nothing here to keep or to leak.
//
// # Signing in
//
// `signIn` is three steps the pane would otherwise have to string together:
// begin (the backend starts listening for the browser), open the address
// (or, with no browser to open, leave it in `pending` for the pane to show
// for copying), and wait. Only one is waiting at a time; `cancelSignIn`
// makes the wait reject with `cancelled`, which is the person's own doing
// and so is not reported.

import { api } from './api'
import { registerApply } from './live-apply'
import { openExternal } from './open-external'
import { app, errorMessage, quietly } from './state.svelte'
import type {
  McpConnection,
  ConnectionId,
  ConnectionPreset,
  ConnectionSecretsPatch,
  ConnectionView,
  ConnectionsView,
  ToolPolicy,
} from './types'
import type { ConnectionForm } from './connections'

/** What is in progress on one connection, for its buttons' spinners. */
export type ConnectionBusy = 'saving' | 'checking' | 'signingIn' | 'removing'

/** A sign-in waiting on the browser. */
export interface PendingSignIn {
  id: ConnectionId
  signInId: string
  url: string
  /** `openExternal` could not open it: the pane shows `url` to copy. */
  showUrl: boolean
}

/** What the add form's typed secrets come to once the empty ones are left
 *  out: an empty string would *remove* a secret, which on an add is only
 *  noise. `null` when nothing is left to send. */
function sentSecrets(secrets: ConnectionSecretsPatch): ConnectionSecretsPatch | null {
  const out: ConnectionSecretsPatch = {}
  if (secrets.bearer) out.bearer = secrets.bearer
  if (secrets.clientSecret) out.clientSecret = secrets.clientSecret
  for (const key of ['headers', 'env'] as const) {
    const kept = Object.entries(secrets[key] ?? {}).filter(([, value]) => value !== '')
    if (kept.length > 0) out[key] = Object.fromEntries(kept)
  }
  return Object.keys(out).length > 0 ? out : null
}

class ConnectionsState {
  view = $state<ConnectionsView | null>(null)
  /** Why the last read or write failed, or `null`. */
  error = $state<string | null>(null)
  /** Per connection, what is in progress. */
  busy = $state<Record<ConnectionId, ConnectionBusy>>({})
  /** The sign-in waiting on the browser, if any. One at a time. */
  pending = $state<PendingSignIn | null>(null)

  /** How many panes are open on this; reloading on a change needs one. */
  #watching = 0

  constructor() {
    // Tools and tokens go with the key, like everything else read out of the
    // vault.
    app.onLock(() => this.reset())
    // Read again, quietly, when something in settings changes while a pane
    // is open: a sign-in finished in another window, the assistant's own
    // use recording a refused token. Only while somebody is looking. A
    // `settings` change belongs to the status word (`RELOADS` in
    // `live.svelte.ts`), and this declines it, so that still reloads as it
    // did.
    registerApply(
      'status',
      () => {
        if (this.#watching > 0) void this.load()
        return false
      },
      'connections',
    )
  }

  get connections(): ConnectionView[] {
    return this.view?.connections ?? []
  }

  get presets(): ConnectionPreset[] {
    return this.view?.presets ?? []
  }

  find(id: ConnectionId): ConnectionView | undefined {
    return this.connections.find((v) => v.connection.id === id)
  }

  /** Read the list and the presets. */
  async load(): Promise<void> {
    try {
      this.view = await api.connections()
      this.error = null
    } catch (e) {
      this.error = errorMessage(e)
      await quietly(e)
    }
  }

  /** A pane opened: load, and reload on a `settings` change while open.
   *  Returns the function the pane calls when it closes. */
  watch(): () => void {
    this.#watching += 1
    void this.load()
    return () => {
      this.#watching = Math.max(0, this.#watching - 1)
    }
  }

  /** Add a connection from a filled-in form. Answers the new view, or
   *  `null` with `error` set. */
  async add(form: ConnectionForm): Promise<ConnectionView | null> {
    this.error = null
    try {
      const secrets = sentSecrets(form.secrets)
      const added = await api.addConnection({
        ...(form.preset ? { preset: form.preset } : {}),
        name: form.name.trim(),
        transport: form.transport,
        auth: form.auth,
        ...(secrets ? { secrets } : {}),
      })
      this.#put(added)
      return added
    } catch (e) {
      this.error = errorMessage(e)
      await quietly(e)
      return null
    }
  }

  /** Save an edited connection. */
  async update(connection: McpConnection): Promise<boolean> {
    return this.#work(connection.id, 'saving', async () => {
      this.#put(await api.updateConnection(connection))
    })
  }

  async remove(id: ConnectionId): Promise<boolean> {
    return this.#work(id, 'removing', async () => {
      this.view = await api.removeConnection(id)
    })
  }

  async setSecrets(id: ConnectionId, patch: ConnectionSecretsPatch): Promise<boolean> {
    return this.#work(id, 'saving', async () => {
      this.#put(await api.setConnectionSecrets(id, patch))
    })
  }

  /** Connect and list the tools afresh. A server that cannot be reached is
   *  not a failure of the call: the view says so in `state.lastError`. */
  async check(id: ConnectionId): Promise<void> {
    await this.#work(id, 'checking', async () => {
      this.#put(await api.checkConnection(id))
    })
  }

  async setPolicy(id: ConnectionId, tool: string, policy: ToolPolicy | null): Promise<void> {
    await this.#work(id, 'saving', async () => {
      this.#put(await api.setConnectionToolPolicy(id, tool, policy))
    })
  }

  /** Begin an OAuth sign-in, open the browser, and wait for it. Answers
   *  whether it finished. */
  async signIn(id: ConnectionId): Promise<boolean> {
    if (this.pending) return false
    this.busy[id] = 'signingIn'
    this.error = null
    try {
      const begun = await api.beginConnectionSignIn(id)
      // Pending before the browser opens, so a cancel is possible from the
      // moment the sign-in exists; the address is shown for copying only
      // once opening it has failed.
      this.pending = { id, signInId: begun.signInId, url: begun.url, showUrl: false }
      const opened = await openExternal(begun.url)
      if (this.pending?.signInId === begun.signInId) this.pending.showUrl = !opened
      this.#put(await api.awaitConnectionSignIn(begun.signInId))
      return true
    } catch (e) {
      // Withdrawing it is the person's own doing, not a fault.
      if ((e as { code?: unknown } | null)?.code !== 'cancelled') this.error = errorMessage(e)
      await quietly(e)
      return false
    } finally {
      this.pending = null
      delete this.busy[id]
    }
  }

  /** Withdraw the pending sign-in. */
  async cancelSignIn(): Promise<void> {
    const pending = this.pending
    if (!pending) return
    try {
      await api.cancelConnectionSignIn(pending.signInId)
    } catch (e) {
      this.error = errorMessage(e)
      await quietly(e)
    }
  }

  async signOut(id: ConnectionId): Promise<void> {
    await this.#work(id, 'saving', async () => {
      this.#put(await api.signOutConnection(id))
    })
  }

  /** Forget everything: the vault locked. */
  reset(): void {
    this.view = null
    this.error = null
    this.busy = {}
    this.pending = null
  }

  /** Run one write on `id` with its spinner showing. Answers whether it
   *  worked; the reason, when it did not, is `error`. */
  async #work(id: ConnectionId, busy: ConnectionBusy, run: () => Promise<void>): Promise<boolean> {
    this.busy[id] = busy
    this.error = null
    try {
      await run()
      return true
    } catch (e) {
      this.error = errorMessage(e)
      await quietly(e)
      return false
    } finally {
      delete this.busy[id]
    }
  }

  /** Put `fresh` where its connection is in the list, or at the end. */
  #put(fresh: ConnectionView) {
    const view = this.view ?? { connections: [], presets: [] }
    const at = view.connections.findIndex((v) => v.connection.id === fresh.connection.id)
    const connections =
      at < 0 ? [...view.connections, fresh] : view.connections.map((v, i) => (i === at ? fresh : v))
    this.view = { ...view, connections }
  }
}

export const connections = new ConnectionsState()
