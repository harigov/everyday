// State for meeting notes: the settings pane, the live capture pill, the
// offer banner, and the two lists that poll while something is moving --
// recordings still in the pipeline, and a model still downloading.
//
// Deliberately one store for all of it rather than one per surface. The
// settings pane, the pill and the notes app all need to know the same two
// things -- is a call being recorded right now, and what is in the pipeline
// -- and a press in one of them (Stop, Retry, Download) has to be seen by
// the other two without a page reload. A vault-wide `onChange` would do that
// for an ordinary record, but recordings, transcripts and voiceprints are
// not in `ChangeKind` yet (see `meetings-api.ts`'s own `TODO(meetings)`), so
// this store polls instead -- narrowly, and only while there is something
// worth polling for.

import { api, callCommand, isMock, onMeetingOffer, onMeetingStatus, onMeetingStillOn } from './api'
import type { MeetingOfferPayload } from './api'
import * as meetingsApi from './meetings-api'
import { stageIsActive } from './meetings-format'
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

/** The stages worth a card in the notes list and a poll. */
const ACTIVE_STAGES = ['recording', 'transcribing', 'identifying', 'summarising', 'failed']

/** How often the recordings list is re-read while something is moving. */
const RECORDINGS_POLL_MS = 4000
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

  #recordingsTimer: ReturnType<typeof setInterval> | null = null
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
    if (this.#recordingsTimer) clearInterval(this.#recordingsTimer)
    if (this.#modelsTimer) clearInterval(this.#modelsTimer)
    this.#recordingsTimer = null
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
      this.view = await meetingsApi.meetingSettings()
    } catch (e) {
      if (!isLocked(e)) await handle(e)
    } finally {
      this.loading = false
    }
  }

  /** Throws on failure -- the settings pane shows the message beside Save. */
  async save(settings: MeetingSettings): Promise<MeetingSettingsView> {
    const v = await meetingsApi.saveMeetingSettings(settings)
    this.view = v
    return v
  }

  async setKey(key: string | null): Promise<MeetingSettingsView> {
    const v = await meetingsApi.setTranscriberKey(key)
    this.view = v
    return v
  }

  test(): Promise<void> {
    return meetingsApi.testTranscriber()
  }

  newTemplate(): Promise<NoteTemplate> {
    return meetingsApi.newMeetingTemplate()
  }

  lintTemplate(body: string): Promise<string[]> {
    return meetingsApi.lintMeetingTemplate(body)
  }

  previewTemplate(body: string): Promise<string> {
    return meetingsApi.previewMeetingTemplate(body)
  }

  // ── Recordings, polled while any of them is still moving ───────────

  async refreshRecordings() {
    try {
      this.recordings = await meetingsApi.listRecordings({ stages: ACTIVE_STAGES, limit: 50 })
    } catch (e) {
      if (!isLocked(e)) return
    }
    this.#ensureRecordingsPoll()
  }

  #ensureRecordingsPoll() {
    const moving = this.recordings.some((r) => stageIsActive(r.stage)) || this.capture != null
    if (moving && !this.#recordingsTimer) {
      this.#recordingsTimer = setInterval(() => void this.refreshRecordings(), RECORDINGS_POLL_MS)
    } else if (!moving && this.#recordingsTimer) {
      clearInterval(this.#recordingsTimer)
      this.#recordingsTimer = null
    }
  }

  async retryRecording(id: RecordingId) {
    await meetingsApi.retryRecording(id)
    await this.refreshRecordings()
  }

  async discardRecording(id: RecordingId) {
    await meetingsApi.discardRecording(id)
    await this.refreshRecordings()
  }

  async deleteRecording(id: RecordingId) {
    await meetingsApi.deleteRecording(id)
    await this.refreshRecordings()
  }

  // ── Local speech models ──────────────────────────────────────────

  async refreshModels() {
    this.models = await meetingsApi.speechModels()
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
    await meetingsApi.downloadSpeechModel(id)
    await this.refreshModels()
  }

  async cancelDownload(id: string) {
    await meetingsApi.cancelSpeechModelDownload(id)
    await this.refreshModels()
  }

  async deleteModel(id: string) {
    await meetingsApi.deleteSpeechModel(id)
    await this.refreshModels()
  }

  benchmark(id: string): Promise<ModelBenchmark> {
    return meetingsApi.benchmarkSpeechModel(id)
  }

  // ── Voiceprints ───────────────────────────────────────────────────

  async loadVoiceprints() {
    this.voiceprints = await meetingsApi.listVoiceprints()
  }

  async deleteVoiceprint(id: VoiceprintId) {
    await meetingsApi.deleteVoiceprint(id)
    await this.loadVoiceprints()
  }

  async deleteAllVoiceprints() {
    await meetingsApi.deleteAllVoiceprints()
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

  #dropOffer(eventId: string) {
    this.offers = this.offers.filter((o) => o.eventId !== eventId)
  }

  async dismissOffer(eventId: string, never: boolean) {
    this.#dropOffer(eventId)
    await meetingsApi.dismissMeetingOffer(eventId, never)
  }

  async acceptOffer(offer: MeetingOfferPayload): Promise<Recording> {
    this.#dropOffer(offer.eventId)
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
