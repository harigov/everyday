// The account list Settings → Accounts draws, and the small amount of state
// a mailbox provider's sign-in needs while it is in flight.
//
// Shaped like `purpose.svelte.ts`: a class singleton, a `#loaded` guard so
// the first open of the tab is the only unconditional fetch, and
// `app.onLock` clearing everything a locked vault must not still be holding.
// The list is small -- a handful of accounts, not a hundred thousand
// messages -- so there is no paging here, unlike what mail itself will need.

import { api } from './api'
import { registerApply, singleId, type ChangeWithIds } from './live-apply'
import { app, handle, isLocked } from './state.svelte'
import type {
  Account,
  AccountId,
  AccountView,
  AgentCallerKind,
  AgentMailAccess,
  MailProvider,
  MailProviderInfo,
} from './types'

class AccountsState {
  list = $state<AccountView[]>([])
  presets = $state<MailProviderInfo[]>([])
  loading = $state(false)

  /** Which load is current, so a slow one cannot land after a lock or a newer call. */
  #generation = 0
  #loaded = false
  #presetsLoaded = false

  constructor() {
    app.onLock(() => this.reset())
    // `live.svelte.ts`'s opt-in: a change from another window patches this
    // list in place rather than reloading it whole. First user beside
    // `notes`, and the same shape -- a single-record change re-fetches that
    // one row; anything wider asks for the ordinary `refresh()` instead.
    registerApply('accounts', (changes) => this.#applyChanges(changes))
  }

  reset() {
    this.#generation += 1
    this.list = []
    this.presets = []
    this.loading = false
    this.#loaded = false
    this.#presetsLoaded = false
  }

  /** Load the list, once. Called when the Accounts tab is opened. */
  async load(force = false) {
    if (this.#loaded && !force) return
    const mine = ++this.#generation
    this.loading = true
    try {
      const list = await api.accounts()
      if (mine !== this.#generation) return
      this.list = list
      this.#loaded = true
    } catch (e) {
      await handle(e)
    } finally {
      if (mine === this.#generation) this.loading = false
    }
  }

  /** Reload after a write, without the loading flag -- the list is already on screen. */
  refresh(): Promise<void> {
    return this.load(true)
  }

  /** Every well-known provider's preset, loaded once -- pure data, so it never goes stale. */
  async loadPresets() {
    if (this.#presetsLoaded) return
    try {
      this.presets = await api.accountPresets()
      this.#presetsLoaded = true
    } catch (e) {
      await handle(e)
    }
  }

  account(id: AccountId): AccountView | undefined {
    return this.list.find((a) => a.id === id)
  }

  preset(provider: MailProvider): MailProviderInfo | undefined {
    return this.presets.find((p) => p.provider === provider)
  }

  /**
   * Create or update an account record. Answers whether it went.
   *
   * `$state.snapshot`, the same as `saveRole` and `saveGoal` in
   * `purpose.svelte.ts`: a caller often hands this a record read straight
   * out of `this.list` or built from a sheet's own `$state` fields, and its
   * nested objects -- `imap`, `auth`, the access grids -- are Svelte
   * proxies. Sealing that at the store boundary means a caller never has to
   * remember to.
   */
  async save(account: Account): Promise<boolean> {
    try {
      await api.saveAccount($state.snapshot(account))
      await this.refresh()
      return true
    } catch (e) {
      await handle(e)
      return false
    }
  }

  async savePassword(id: AccountId, password: string): Promise<boolean> {
    try {
      await api.saveAccountPassword(id, password)
      await this.refresh()
      return true
    } catch (e) {
      await handle(e)
      return false
    }
  }

  /** Remove an account. The backend deletes its local mail with it. */
  async remove(id: AccountId): Promise<boolean> {
    try {
      await api.deleteAccount(id)
      await this.refresh()
      return true
    } catch (e) {
      await handle(e)
      return false
    }
  }

  async setAccess(
    id: AccountId,
    caller: AgentCallerKind,
    access: AgentMailAccess,
  ): Promise<boolean> {
    try {
      await api.setAgentAccess(id, caller, $state.snapshot(access))
      await this.refresh()
      return true
    } catch (e) {
      await handle(e)
      return false
    }
  }

  #applyChanges(changes: ChangeWithIds[]): boolean {
    if (changes.length !== 1) return false
    const change = changes[0]!
    const id = singleId(change)
    if (!id) return false
    if (change.op === 'deleted') {
      this.list = this.list.filter((a) => a.id !== id)
      return true
    }
    if (change.op === 'created' || change.op === 'updated') {
      void this.#patchOne(id)
      return true
    }
    return false
  }

  async #patchOne(id: AccountId) {
    try {
      const view = await api.account(id)
      const at = this.list.findIndex((a) => a.id === id)
      this.list = at >= 0 ? this.list.map((a, i) => (i === at ? view : a)) : [...this.list, view]
    } catch (e) {
      if (isLocked(e)) return
      await this.refresh()
    }
  }
}

export const accounts = new AccountsState()

/**
 * A fresh id for an account that has not been saved yet.
 *
 * Every other record with a client-minted id gets one from a `new_X`
 * command instead of `crypto.randomUUID` -- see `assistant.svelte.ts`'s own
 * note on why: a v4 id where the rest of the vault is v7, and a secure
 * context the packaged webview does not always have. There is no
 * `new_account` command for this to defer to -- `save_account` is a plain
 * upsert, and nothing in the surface mints a blank `Account` the way
 * `new_role` and `new_task` mint a blank record of theirs. Until one exists,
 * this is the accepted stopgap: `localhost` and the packaged app's own
 * origin are both secure contexts, and a v4 id round-trips through the
 * wire's `Uuid` type exactly as well as a v7 one -- it only sorts
 * differently, which nothing here relies on.
 */
export function mintAccountId(): AccountId {
  return crypto.randomUUID()
}

/**
 * One OAuth sign-in in flight, as a sheet drives it.
 *
 * Deliberately not part of `AccountsState`: a sign-in belongs to whichever
 * sheet started it and ends when that sheet closes, while the list survives
 * across every one of them. `finished` resolves once the tokens are sealed
 * onto the account -- `attach_oauth_sign_in`'s job, called here so a caller
 * only has to await one thing rather than repeat the three-call sequence
 * `docs/plans/mail.md`'s Phase 1 lays out.
 */
export interface OAuthSignInHandle {
  url: string
  signInId: string
  finished: Promise<void>
  /** Withdraw it. `finished` then rejects with the backend's own "cancelled". */
  cancel: () => void
}

/**
 * Begin a sign-in for `id`, an account already saved with `NeedsSignIn`.
 *
 * Errors from `begin_oauth_sign_in` reject this call directly; errors from
 * the wait or the attach step reject `finished` instead, once the URL has
 * already been handed back for the sheet to open. Not wrapped in `handle`:
 * the caller shows the message inline, beside the sheet's own Cancel and
 * Try again, rather than as the application-wide banner every other store's
 * failure becomes.
 */
export async function beginAccountSignIn(
  id: AccountId,
  args: {
    authUrl: string
    tokenUrl: string
    clientId: string
    clientSecret?: string
    scopes: string[]
    loginHint?: string
  },
): Promise<OAuthSignInHandle> {
  const begun = await api.beginOauthSignIn(args)
  const finished = (async () => {
    const awaited = await api.awaitOauthSignIn(begun.signInId)
    await api.attachOauthSignIn(id, awaited.tokensSavedUnder, args.clientSecret)
  })()
  return {
    url: begun.url,
    signInId: begun.signInId,
    finished,
    cancel: () => void api.cancelOauthSignIn(begun.signInId),
  }
}
