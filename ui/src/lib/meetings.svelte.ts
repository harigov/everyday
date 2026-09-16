// State for meeting notes: the settings pane, the live capture pill, the
// offer banner, and the recordings/voiceprints lists and the model catalogue
// they share this store with.
//
// Deliberately one store for all of it rather than one per surface. The
// settings pane, the pill and the notes app all need to know the same two
// things -- is a call being recorded right now, and what is in the pipeline
// -- and a press in one of them (Stop, Retry, Download) has to be seen by
// the other two without a page reload. `recording` and `voiceprint` are
// ordinary `ChangeKind`s now, so `live.svelte.ts` routes a change of either
// to this store's own `liveRefresh` the way every other domain's changes
// route to its store -- see that module's `RELOADS`. The model catalogue is
// the one list here that still polls: a byte counter mid-download is not a
// vault write, so nothing raises a change for it, and `MODELS_POLL_MS`
// below is that gap's whole fix, same as `speech.rs`'s own doc explains.

import { api, callCommand, isMock, onMeetingOffer, onMeetingStatus, onMeetingStillOn } from './api'
import type { MeetingOfferPayload } from './api'
import { notify } from './notify.svelte'
import { app, handle, isLocked } from './state.svelte'
import type {
  CaptureStatus,
  MeetingSettings,
  MeetingSettingsView,
  ModelBenchmark,
  NoteTemplate,
  Recording,
  RecordingId,
  SpeechModelInfo,
  VoiceprintId,
  VoiceprintInfo,
} from './types'

/** The stages worth a card in the notes list. */
const ACTIVE_STAGES = ['recording', 'transcribing', 'identifying', 'summarising', 'failed']

/** How often the model catalogue is re-read while a download is running. */
const MODELS_POLL_MS = 500

class MeetingsState {
  /** The settings pane's whole answer: the settings, and what they add up to. */
  view = $state<MeetingSettingsView | null>(null)
  loading = $state(false)

  /** Non-null exactly while the shell (or its mock stand-in) is recording. */
  capture = $state<CaptureStatus | null>(null)
  /** Set while the "still on the call?" prompt should be showing. */
  stillOn = $state<RecordingId | null>(null)
  /** Offers awaiting a decision -- "Take notes for X?" -- oldest first. */
  offers = $state<MeetingOfferPayload[]>([])

  /** Recordings still in the pipeline, or failed -- the notes list's cards. */
  recordings = $state<Recording[]>([])
  models = $state<SpeechModelInfo[]>([])
  voiceprints = $state<VoiceprintInfo[]>([])

  #modelsTimer: ReturnType<typeof setInterval> | null = null
  #started = false

  constructor() {
    // A recording's title, a transcript's text -- decrypted contents of the
    // vault, same as everything else that clears on lock.
    app.onLock(() => this.reset())
  }

  reset() {
    this.view = null
    this.capture = null
    this.stillOn = null
    this.offers = []
    this.recordings = []
    this.models = []
    this.voiceprints = []
    if (this.#modelsTimer) clearInterval(this.#modelsTimer)
    this.#modelsTimer = null
  }

  /** Does this vault's backend carry notes, so a meeting note has somewhere
   *  to live? Meeting notes are filed as ordinary notes -- see the plan's
   *  "Purpose" -- so this rides the same capability rather than a new one. */
  get supported(): boolean {
    return app.status?.capabilities?.notes === true
  }

  /**
   * Begin listening for the shell's capture events, and take the first
   * reading of what it is doing. Called once, from `App.svelte`, the way
   * `live.start()` and `tray.start()` are.
   */
  start() {
    if (this.#started || !this.supported) return
    this.#started = true

    onMeetingStatus((status) => (this.capture = status))
    onMeetingStillOn((id) => (this.stillOn = id))
    onMeetingOffer((offer) => {
      if (offer.automatic) {
        // The shell has already started recording; this is a toast, not an
        // ask. `key` replaces rather than stacks, in case more than one
        // automatic recording starts before the last toast is dismissed.
        notify.info(`Recording ${offer.title} automatically`, {
          key: 'meeting-automatic',
          timeout: null,
          action: { label: 'Stop', run: () => void this.stopCapture(false) },
        })
        void this.refreshRecordings()
      } else {
        this.offers = [...this.offers, offer]
      }
    })

    void api
      .meetingStatus()
      .then((status) => (this.capture = status))
      .catch(() => {
        // No shell to ask (a plain browser build, or a remote client) --
        // the pill simply never shows, which is the right degrading.
      })
    void this.refreshRecordings()
  }

  // ── Settings ──────────────────────────────────────────────────────

  async load() {
    if (this.view) return
    await this.reload()
  }

  async reload() {
    this.loading = true
    try {
      this.view = await api.meetingSettings()
    } catch (e) {
      if (!isLocked(e)) await handle(e)
    } finally {
      this.loading = false
    }
  }

  /** Throws on failure -- the settings pane shows the message beside Save. */
  async save(settings: MeetingSettings): Promise<MeetingSettingsView> {
    const v = await api.saveMeetingSettings(settings)
    this.view = v
    return v
  }

  async setKey(key: string | null): Promise<MeetingSettingsView> {
    const v = await api.setTranscriberKey(key)
    this.view = v
    return v
  }

  test(): Promise<void> {
    return api.testTranscriber()
  }

  newTemplate(): Promise<NoteTemplate> {
    return api.newMeetingTemplate()
  }

  lintTemplate(body: string): Promise<string[]> {
    return api.lintMeetingTemplate(body)
  }

  previewTemplate(body: string): Promise<string> {
    return api.previewMeetingTemplate(body)
  }

  // ── Recordings, refreshed by a `recording` change from `live.svelte.ts` ──

  async refreshRecordings() {
    try {
      this.recordings = await api.listRecordings({ stages: ACTIVE_STAGES, limit: 50 })
    } catch (e) {
      if (!isLocked(e)) return
    }
  }

  /**
   * What `live.svelte.ts`'s `RELOAD['meetings']` calls for a `recording` or
   * `voiceprint` change -- another window's write, or this vault's own
   * background pipeline finishing or failing a call it started. One target
   * for both kinds, the same "an app, not a table" granularity every other
   * domain's live refresh uses: neither list is ever large enough that
   * telling the two kinds apart would save anything worth the extra code.
   */
  async liveRefresh() {
    await Promise.all([this.refreshRecordings(), this.loadVoiceprints()])
  }

  async retryRecording(id: RecordingId) {
    await api.retryRecording(id)
    await this.refreshRecordings()
  }

  async discardRecording(id: RecordingId) {
    await api.discardRecording(id)
    await this.refreshRecordings()
  }

  async deleteRecording(id: RecordingId) {
    await api.deleteRecording(id)
    await this.refreshRecordings()
  }

  // ── Local speech models ──────────────────────────────────────────

  async refreshModels() {
    this.models = await api.speechModels()
    this.#ensureModelsPoll()
  }

  #ensureModelsPoll() {
    const downloading = this.models.some((m) => m.progress != null)
    if (downloading && !this.#modelsTimer) {
      this.#modelsTimer = setInterval(() => void this.refreshModels(), MODELS_POLL_MS)
    } else if (!downloading && this.#modelsTimer) {
      clearInterval(this.#modelsTimer)
      this.#modelsTimer = null
    }
  }

  async downloadModel(id: string) {
    await api.downloadSpeechModel(id)
    await this.refreshModels()
  }

  async cancelDownload(id: string) {
    await api.cancelSpeechModelDownload(id)
    await this.refreshModels()
  }

  async deleteModel(id: string) {
    await api.deleteSpeechModel(id)
    await this.refreshModels()
  }

  benchmark(id: string): Promise<ModelBenchmark> {
    return api.benchmarkSpeechModel(id)
  }

  // ── Voiceprints ───────────────────────────────────────────────────

  async loadVoiceprints() {
    this.voiceprints = await api.listVoiceprints()
  }

  async deleteVoiceprint(id: VoiceprintId) {
    await api.deleteVoiceprint(id)
    await this.loadVoiceprints()
  }

  async deleteAllVoiceprints() {
    await api.deleteAllVoiceprints()
    await this.loadVoiceprints()
  }

  /**
   * Record a 20-second sample, natively, and turn it into a voiceprint.
   *
   * `api.voiceEnrol` is a Tauri command the capture engineer may not have
   * built yet; a build without it rejects with something that is not a
   * `VaultError` at all (Tauri's own "command not found"), so this turns
   * *any* failure of the native call into one plain sentence rather than
   * whatever that rejection happens to stringify to.
   */
  async enrolVoice(): Promise<VoiceprintInfo> {
    let vp: VoiceprintInfo
    try {
      vp = await api.voiceEnrol(20)
    } catch {
      throw new Error(
        'Recording your voice needs the desktop app’s microphone access, which is not in this build yet.',
      )
    }
    await this.loadVoiceprints()
    return vp
  }

  // ── Capture ───────────────────────────────────────────────────────

  async startCapture(opts: {
    eventId?: string | null
    title?: string | null
    templateId?: string | null
  }): Promise<Recording> {
    const r = await api.meetingStart(opts)
    this.capture = await api.meetingStatus().catch(() => null)
    await this.refreshRecordings()
    return r
  }

  async stopCapture(discard: boolean) {
    await api.meetingStop(discard)
    this.capture = null
    this.stillOn = null
    await this.refreshRecordings()
  }

  // ── Offers ────────────────────────────────────────────────────────

  // Keyed by calendarId + uid, not eventId -- the same durable pair
  // `dismissMeetingOffer` itself is keyed by, so dropping an offer here and
  // dismissing it on the service side agree about which one even after a
  // feed resync has changed the eventId. See `MeetingOfferPayload`'s own
  // doc in `api.ts`.
  #dropOffer(calendarId: string, uid: string) {
    this.offers = this.offers.filter((o) => !(o.calendarId === calendarId && o.uid === uid))
  }

  async dismissOffer(calendarId: string, uid: string, never: boolean) {
    this.#dropOffer(calendarId, uid)
    await api.dismissMeetingOffer(calendarId, uid, never)
  }

  async acceptOffer(offer: MeetingOfferPayload): Promise<Recording> {
    this.#dropOffer(offer.calendarId, offer.uid)
    return this.startCapture({ eventId: offer.eventId, title: offer.title })
  }

  // ── Dev-only mock triggers ───────────────────────────────────────
  //
  // No-ops on a real backend: `isMock` gates every one of these, and
  // `MeetingsPanel` gates the buttons that call them on the same flag, so
  // neither the command nor the section is reachable outside `make ui`.

  async mockTriggerOffer(automatic: boolean) {
    if (!isMock) return
    await callCommand('mock_trigger_meeting_offer', { automatic })
  }

  async mockTriggerStillOn() {
    if (!isMock) return
    await callCommand('mock_trigger_still_on', {})
  }

  async mockTriggerSystemSilent() {
    if (!isMock) return
    await callCommand('mock_trigger_system_silent', {})
  }
}

export const meetings = new MeetingsState()
