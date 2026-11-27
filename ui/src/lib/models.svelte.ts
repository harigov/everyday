// Which model each size of work runs on: Settings → Models, and what the
// rest of the interface asks about it.
//
// Application settings rather than the assistant's -- `jatayu_core::llm`.
// Large writes up research, Medium is the assistant, Small does the quick
// asks. Each has its own endpoint and model; a key belongs to an endpoint,
// so two tiers at one endpoint share one, and `keyed` lists endpoints rather
// than tiers.
//
// The assistant's rail, the quick asks and the mail acknowledgement all read
// from here, through getters rather than copies, for the reason `quick`
// gives: two loads started together land in either order, and a snapshot
// taken before the second arrives is a feature quietly switched off.
//
// So does the list of decision models that run on this computer -- the
// Decision card's other half -- polled once a second while one downloads.
//
// And the speech card's: the recognisers and the speech kit, which meeting
// notes used to list and poll for themselves when how speech becomes text was
// theirs to choose. It is the platform's now (`jatayu_core::speech`), so
// the list lives here, once, and meeting notes read the speech model from
// their own view rather than keeping a second copy of the catalogue.
//
// And the voice's, the sixth card: the voices that run on this computer, and a
// few seconds of the saved one to listen to before talking to it.

import { endpointOf } from './accounts'
import { isLoopback } from './agent'
import { api } from './api'
import { app, errorMessage, isLocked, quietly } from './state.svelte'
import type {
  DecisionBenchmark,
  LLMConfig,
  LocalModelInfo,
  ModelBenchmark,
  ModelSettings,
  ModelSettingsView,
  ModelSlot,
  ModelTier,
} from './types'

/** How often the decision model list is read again while one downloads. */
const LOCAL_POLL_MS = 1000

/** How often the voice list is read again while one downloads. */
const VOICE_POLL_MS = 500

/** How often the speech model list is read again while one downloads: the
 *  speech kit is small enough that a slower poll would miss most of it. */
const SPEECH_POLL_MS = 500

class ModelsState {
  view = $state<ModelSettingsView | null>(null)
  /** The decision models that run on this computer: `decision_models`. */
  local = $state<LocalModelInfo[]>([])
  #localTimer: ReturnType<typeof setInterval> | null = null
  /** The speech recognisers and the speech kit: `speech_models`. */
  speech = $state<LocalModelInfo[]>([])
  #speechTimer: ReturnType<typeof setInterval> | null = null
  /** The voices that run on this computer: `voice_models`. */
  voiceModels = $state<LocalModelInfo[]>([])
  #voiceTimer: ReturnType<typeof setInterval> | null = null
  /** The picture models and the image engine: `image_models`. */
  imageModels = $state<LocalModelInfo[]>([])
  #imageTimer: ReturnType<typeof setInterval> | null = null
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
    this.local = []
    if (this.#localTimer) clearInterval(this.#localTimer)
    this.#localTimer = null
    this.speech = []
    if (this.#speechTimer) clearInterval(this.#speechTimer)
    this.#speechTimer = null
    this.voiceModels = []
    if (this.#voiceTimer) clearInterval(this.#voiceTimer)
    this.#voiceTimer = null
    this.imageModels = []
    if (this.#imageTimer) clearInterval(this.#imageTimer)
    this.#imageTimer = null
  }

  /** Whether this copy of Jatayu can run a decision model itself. */
  get localDecisions(): boolean {
    return this.view?.localDecisions === true
  }

  /** Whether this copy can run a speech recogniser itself. Assumed until the
   *  first read says otherwise, so the choice does not flicker in. */
  get localSpeech(): boolean {
    return this.view?.localSpeech ?? true
  }

  /** Whether this copy can speak with a voice of its own. Assumed until the
   *  first read says otherwise, as `localSpeech` is. */
  get localVoice(): boolean {
    return this.view?.localVoice ?? true
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

  /** Whether a key is stored for the address a decision model posts to. */
  hasKeyAt(endpoint: string): boolean {
    return this.view?.keyed.includes(endpoint) ?? false
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

  async setKey(tier: ModelSlot, key: string) {
    this.view = await api.setModelKey(tier, key)
  }

  async clearKey(tier: ModelSlot) {
    this.view = await api.clearModelKey(tier)
  }

  // ── Decision models on this computer ─────────────────────────────

  async refreshLocal() {
    this.local = await api.decisionModels()
    const downloading = this.local.some((m) => m.progress != null)
    if (downloading && !this.#localTimer) {
      this.#localTimer = setInterval(() => void this.refreshLocal().catch(quietly), LOCAL_POLL_MS)
    } else if (!downloading && this.#localTimer) {
      clearInterval(this.#localTimer)
      this.#localTimer = null
    }
  }

  async downloadLocal(id: string) {
    await api.downloadDecisionModel(id)
    await this.refreshLocal()
  }

  async cancelLocal(id: string) {
    await api.cancelDecisionModelDownload(id)
    await this.refreshLocal()
  }

  async deleteLocal(id: string) {
    await api.deleteDecisionModel(id)
    await this.refreshLocal()
  }

  benchmarkLocal(id: string): Promise<DecisionBenchmark> {
    return api.benchmarkDecisionModel(id)
  }

  // ── Speech models on this computer ───────────────────────────────

  async refreshSpeech() {
    this.speech = await api.speechModels()
    const downloading = this.speech.some((m) => m.progress != null)
    if (downloading && !this.#speechTimer) {
      this.#speechTimer = setInterval(
        () => void this.refreshSpeech().catch(quietly),
        SPEECH_POLL_MS,
      )
    } else if (!downloading && this.#speechTimer) {
      clearInterval(this.#speechTimer)
      this.#speechTimer = null
    }
  }

  async downloadSpeech(id: string) {
    await api.downloadSpeechModel(id)
    await this.refreshSpeech()
  }

  async cancelSpeech(id: string) {
    await api.cancelSpeechModelDownload(id)
    await this.refreshSpeech()
  }

  async deleteSpeech(id: string) {
    await api.deleteSpeechModel(id)
    await this.refreshSpeech()
  }

  benchmarkSpeech(id: string): Promise<ModelBenchmark> {
    return api.benchmarkSpeechModel(id)
  }

  /** Ask the saved speech model to prove it works. Rejects with why not. */
  testSpeech(): Promise<void> {
    return api.testTranscriber()
  }

  // ── Voices on this computer ──────────────────────────────────────

  async refreshVoice() {
    this.voiceModels = await api.voiceModels()
    const downloading = this.voiceModels.some((m) => m.progress != null)
    if (downloading && !this.#voiceTimer) {
      this.#voiceTimer = setInterval(() => void this.refreshVoice().catch(quietly), VOICE_POLL_MS)
    } else if (!downloading && this.#voiceTimer) {
      clearInterval(this.#voiceTimer)
      this.#voiceTimer = null
    }
  }

  async downloadVoice(id: string) {
    await api.downloadVoiceModel(id)
    await this.refreshVoice()
  }

  async cancelVoice(id: string) {
    await api.cancelVoiceModelDownload(id)
    await this.refreshVoice()
  }

  async deleteVoice(id: string) {
    await api.deleteVoiceModel(id)
    await this.refreshVoice()
  }

  // ── Pictures on this computer ────────────────────────────────────

  async refreshImages() {
    this.imageModels = await api.imageModels()
    const downloading = this.imageModels.some((m) => m.progress != null)
    if (downloading && !this.#imageTimer) {
      this.#imageTimer = setInterval(() => void this.refreshImages().catch(quietly), VOICE_POLL_MS)
    } else if (!downloading && this.#imageTimer) {
      clearInterval(this.#imageTimer)
      this.#imageTimer = null
    }
  }

  async downloadImage(id: string) {
    await api.downloadImageModel(id)
    await this.refreshImages()
  }

  async cancelImage(id: string) {
    await api.cancelImageModelDownload(id)
    await this.refreshImages()
  }

  async deleteImage(id: string) {
    await api.deleteImageModel(id)
    await this.refreshImages()
  }

  /** The saved image model, as an object URL of the PNG. The caller revokes it. */
  async generateImage(prompt: string): Promise<string> {
    const { png } = await api.generateImage(prompt)
    const binary = atob(png)
    const bytes = new Uint8Array(binary.length)
    for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i)
    return URL.createObjectURL(new Blob([bytes], { type: 'image/png' }))
  }

  /**
   * A few seconds of the saved voice, as an object URL to play. The caller
   * revokes it when it is done with it.
   */
  async sampleVoice(): Promise<string> {
    const { wav } = await api.voiceSample()
    const binary = atob(wav)
    const bytes = new Uint8Array(binary.length)
    for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i)
    return URL.createObjectURL(new Blob([bytes], { type: 'audio/wav' }))
  }
}

export const models = new ModelsState()
