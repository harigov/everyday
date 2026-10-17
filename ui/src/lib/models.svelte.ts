// Which model each size of work runs on: Settings → Models, and what the
// rest of the interface asks about it.
//
// Application settings rather than the assistant's -- `everyday_core::llm`.
// Large writes up research, Medium is the assistant, Small does the quick
// asks. Each has its own endpoint and model; a key belongs to an endpoint,
// so two tiers at one endpoint share one, and `keyed` lists endpoints rather
// than tiers.
//
// The assistant's rail, the quick asks and the mail acknowledgement all read
// from here, through getters rather than copies, for the reason `quick`
// gives: two loads started together land in either order, and a snapshot
// taken before the second arrives is a feature quietly switched off.

import { endpointOf } from './accounts'
import { isLoopback } from './agent'
import { api } from './api'
import { app, errorMessage, isLocked, quietly } from './state.svelte'
import type { LLMConfig, ModelSettings, ModelSettingsView, ModelTier } from './types'

class ModelsState {
  view = $state<ModelSettingsView | null>(null)
  /**
   * Why the last read failed, or `null`. Kept rather than swallowed: the pane
   * would otherwise sit on "Loading…" forever, and an account's "I
   * understand" would have no endpoints to agree to without saying why.
   */
  error = $state<string | null>(null)

  constructor() {
    // Endpoints can be a company's private gateway; they go when the key
    // does, like everything else read out of the vault.
    app.onLock(() => this.reset())
  }

  reset() {
    this.view = null
    this.error = null
  }

  get settings(): ModelSettings | null {
    return this.view?.settings ?? null
  }

  /** What a mail acknowledgement stores -- the server's own string. */
  get acknowledgement(): string | null {
    return this.view?.acknowledgement ?? null
  }

  /** Whether a key is stored for the endpoint `config` points at. */
  hasKey(config: Pick<LLMConfig, 'baseUrl'>): boolean {
    return this.view?.keyed.includes(endpointOf(config.baseUrl)) ?? false
  }

  /**
   * Whether `tier`'s work could be asked for right now: configured, named,
   * and keyed unless the model is on this machine. Mirrors
   * `Vault::model_access` -- Large answers with Medium when it is not set,
   * Small with nothing. If the two ever disagree the backend decides, and
   * says so.
   */
  usable(tier: ModelTier): boolean {
    const s = this.settings
    if (!s) return false
    const config = tier === 'large' ? (s.large ?? s.medium) : s[tier]
    if (!config || !config.model.trim()) return false
    return this.hasKey(config) || isLoopback(config.baseUrl)
  }

  /** Whether this vault's backend can hold them. Every backend that can
   *  carry the assistant can, and nothing else asks a model. */
  get supported(): boolean {
    return app.status?.capabilities?.agent === true
  }

  /** Read once; a no-op when already loaded. Quiet, because every caller
   *  but the pane draws a suggestion or nothing. */
  async load() {
    if (this.view || !this.supported) return
    await this.refresh()
  }

  /** Read afresh. Answers whether it worked; the reason is `error`. */
  async refresh(): Promise<boolean> {
    try {
      this.view = await api.modelSettings()
      this.error = null
      return true
    } catch (e) {
      if (!isLocked(e)) this.error = errorMessage(e)
      await quietly(e)
      return false
    }
  }

  async save(settings: ModelSettings) {
    this.view = await api.saveModelSettings(settings)
  }

  async setKey(tier: ModelTier, key: string) {
    this.view = await api.setModelKey(tier, key)
  }

  async clearKey(tier: ModelTier) {
    this.view = await api.clearModelKey(tier)
  }
}

export const models = new ModelsState()
