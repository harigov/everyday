// The chat links Settings → Chat draws: the bot accounts through which the
// assistant can be reached from another device, and where each one stands.
//
// Shaped like `accounts.svelte.ts`: a class singleton, a `#loaded` guard so
// the first open of the tab is the only unconditional fetch, and
// `app.onLock` clearing everything a locked vault must not still be holding.
// The list is a handful of rows at most -- usually one -- so there is no
// paging, and no patching from `live.svelte.ts` either: a link's *status*
// changes without any vault write behind it (signing in, a dropped
// connection), so the panel re-reads the list on a timer while it is open
// (`refresh`, below) and that covers every other window's edit as well.

import { api } from './api'
import { errorMessage, isLocked } from './errors'
import { app, handle } from './state.svelte'
import { latest } from './store/latest'
import { guardedRefresh } from './store/refresh'
import type { ChatLink, ChatLinkId, ChatLinkView } from './types'

/** How a save turned out. The panel shows `error` beside the form itself,
 *  not as the window-wide banner every other store's failure becomes: the
 *  person is looking at the field that is wrong. */
export type SaveResult = { ok: true; view: ChatLinkView } | { ok: false; error: string }

class ChatState {
  list = $state<ChatLinkView[]>([])
  loading = $state(false)

  /** Which load is current, so a slow one cannot land after a lock, a save or
   *  a newer poll. */
  #generation = latest()
  #loaded = false

  constructor() {
    app.onLock(() => this.reset())
  }

  reset() {
    this.#generation.next()
    this.list = []
    this.loading = false
    this.#loaded = false
  }

  /** Load the list, once. Called when the Chat tab is opened. */
  async load(force = false) {
    if (this.#loaded && !force) return
    await guardedRefresh(
      this.#generation,
      async (isCurrent) => {
        const list = await api.chatLinks()
        if (!isCurrent()) return
        this.list = list
        this.#loaded = true
      },
      { setLoading: (v) => (this.loading = v), onError: (e) => handle(e) },
    )
  }

  /**
   * Read the list again, quietly: no loading flag, and a failure is not a
   * banner. This is what the open panel calls every few seconds, so a
   * homeserver that is down must not put a new error over the window each
   * time -- the link's own status already says so. A lock still goes to the
   * lock screen.
   */
  async refresh() {
    const token = this.#generation.next()
    try {
      const list = await api.chatLinks()
      if (!this.#generation.isCurrent(token)) return
      this.list = list
      this.#loaded = true
    } catch (e) {
      if (isLocked(e)) await handle(e)
    }
  }

  view(id: ChatLinkId): ChatLinkView | undefined {
    return this.list.find((v) => v.link.id === id)
  }

  /** A fresh, switched-on Matrix link with nothing filled in, or `null` when
   *  the call failed. Not saved until the form is. */
  async create(): Promise<ChatLink | null> {
    try {
      return await api.newChatLink()
    } catch (e) {
      await handle(e)
      return null
    }
  }

  /**
   * Create or update a link. `password` is the bot's, `null` to keep the
   * sign-in it has.
   *
   * `$state.snapshot`, the same as `accounts.save`: the caller's draft is a
   * Svelte proxy, and the `{ kind: 'matrix', … }` service inside it would
   * not survive the trip over the bridge as one.
   *
   * What comes back is the view the service made on saving -- `connecting`
   * for a link it has just restarted -- and is put into the list at once, so
   * the row says so before the next poll gets to it.
   */
  async save(link: ChatLink, password: string | null): Promise<SaveResult> {
    // A save in flight outranks any poll that started before it.
    this.#generation.next()
    try {
      const view = await api.saveChatLink($state.snapshot(link), password)
      this.#generation.next()
      const at = this.list.findIndex((v) => v.link.id === view.link.id)
      this.list = at >= 0 ? this.list.map((v, i) => (i === at ? view : v)) : [...this.list, view]
      return { ok: true, view }
    } catch (e) {
      if (isLocked(e)) await handle(e)
      return { ok: false, error: errorMessage(e) }
    }
  }

  /** Stop a link and forget it. Answers whether it went. */
  async remove(id: ChatLinkId): Promise<boolean> {
    this.#generation.next()
    try {
      await api.removeChatLink(id)
      this.#generation.next()
      this.list = this.list.filter((v) => v.link.id !== id)
      return true
    } catch (e) {
      await handle(e)
      return false
    }
  }
}

export const chat = new ChatState()
