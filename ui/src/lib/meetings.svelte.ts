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

import {
  api,
  callCommand,
  isMock,
  onMeetingOffer,
  onMeetingOfferFailed,
  onMeetingOfferSettled,
  onMeetingOfferSetup,
  onMeetingStatus,
  onMeetingStillOn,
} from './api'
import type { MeetingOfferAnswer, MeetingOfferPayload } from './api'
import { notify } from './notify.svelte'
import { panels } from './panels.svelte'
import { app, errorMessage, handle, isLocked } from './state.svelte'
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
  /**
   * True from the moment any surface calls `startCapture` until
   * `api.meetingStart` settles -- set here, once, rather than by each of
   * `MeetingOfferBanner`, `EventDetail` and `NotesNav` in their own local
   * state, so the three cannot race each other: a press in one disables the
   * "Take notes"/"Start" control in all three, rather than leaving the
   * other two clickable while a start (and the service round trip behind
   * it) is already under way. The shell's own reservation
   * (`meeting.rs`'s `begin_headless`) is what actually keeps two starts
   * from both succeeding; this is the UI's half, so a second press reads as
   * disabled rather than as a race that resolves into a confusing refusal.
   */
  starting = $state(false)
  /** Set while the "still on the call?" prompt should be showing. */
  stillOn = $state<RecordingId | null>(null)
  /**
   * Offers awaiting a decision -- "Take notes for X?" -- oldest first, each
   * keyed by its `key`. The desktop shell owns the real list (it raises a
   * notification and a tray item for each, and can be answered from any of
   * the three); this is the window's copy of it, kept in step by the
   * `meeting-offer` and `meeting-offer-settled` events and seeded from
   * `meetingOffers` at start.
   */
  offers = $state<MeetingOfferPayload[]>([])
  /**
   * Why answering an offer failed, by offer key -- set when the shell reports
   * a failure from the notification or the tray, which the banner could not
   * see for itself. `MeetingOfferBanner` shows it beside the offer.
   */
  offerErrors = $state<Record<string, string>>({})

  /** Recordings still in the pipeline, or failed -- the notes list's cards. */
  recordings = $state<Recording[]>([])
  /**
   * Set when `refreshRecordings` fails for a reason worth telling somebody
   * about -- everything except the vault locking, which is its own screen,
   * not a message over this one. Cleared the moment a refresh succeeds
   * again. `recordings` itself is left exactly as it was on a failure: the
   * cards on screen stay put (stale, but not wrong) rather than vanishing,
   * and `NotesNav` shows this beside them so a stale list is at least a
   * visible one.
   */
  recordingsError = $state<string | null>(null)
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
    this.offerErrors = {}
    this.recordings = []
    this.recordingsError = null
    this.models = []
    this.voiceprints = []
    this.starting = false
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
        this.#upsertOffer(offer)
      }
    })
    onMeetingOfferSettled((key) => this.#dropOffer(key))
    onMeetingOfferFailed((key, message) => {
      if (key) this.offerErrors = { ...this.offerErrors, [key]: message }
      // The banner shows a keyed failure beside its offer, but the window
      // may be behind another, or showing a different offer first; a toast
      // makes sure it is seen. The tray's generic "Take notes now" has no
      // offer to show it beside, so it is the only place, and offers the
      // way to fix the usual cause.
      notify.error('Couldn’t start taking notes', {
        body: message,
        key: `meeting-offer-failed:${key}`,
        action: key
          ? undefined
          : { label: 'Open settings', run: () => panels.openSettings('meetings') },
      })
    })
    onMeetingOfferSetup(() => panels.openSettings('meetings'))

    void api
      .meetingStatus()
      .then((status) => (this.capture = status))
      .catch(() => {
        // No shell to ask (a plain browser build, or a remote client) --
        // the pill simply never shows, which is the right degrading.
      })
    void this.refreshRecordings()

    // Offers raised before this window was listening -- it was opened from
    // the tray, say, or reloaded. Merged rather than assigned, so one that
    // arrived by event while this was in flight is not lost.
    void api
      .meetingOffers()
      .then((pending) => {
        for (const offer of pending) this.#upsertOffer(offer)
      })
      .catch(() => {
        // No shell, so no offers to recover.
      })
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
      this.recordingsError = null
    } catch (e) {
      // A lock is its own screen, not a message over this one -- the same
      // policy `handle` itself applies, but without `handle`'s global
      // banner: a background refresh failing is worth a line beside the
      // cards it could not update, not a report over the whole window.
      if (isLocked(e)) return
      this.recordingsError = errorMessage(e)
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
    this.starting = true
    try {
      const r = await api.meetingStart(opts)
      this.capture = await api.meetingStatus().catch(() => null)
      await this.refreshRecordings()
      return r
    } finally {
      this.starting = false
    }
  }

  async stopCapture(discard: boolean) {
    await api.meetingStop(discard)
    this.capture = null
    this.stillOn = null
    await this.refreshRecordings()
  }

  // ── Offers ────────────────────────────────────────────────────────

  /** A new offer joins the end; one with a key already here replaces it in
   *  place, so a re-raised offer (its readiness changed, say) does not jump
   *  the queue or show twice. */
  #upsertOffer(offer: MeetingOfferPayload) {
    const at = this.offers.findIndex((o) => o.key === offer.key)
    if (at === -1) {
      this.offers = [...this.offers, offer]
    } else {
      this.offers = this.offers.map((o, i) => (i === at ? offer : o))
    }
  }

  #dropOffer(key: string) {
    this.offers = this.offers.filter((o) => o.key !== key)
    if (key in this.offerErrors) {
      const rest = { ...this.offerErrors }
      delete rest[key]
      this.offerErrors = rest
    }
  }

  /**
   * Could taking notes start right now? The offer says so itself when the
   * shell raised it, but an offer made before meeting notes were set up
   * becomes takeable the moment the person finishes setting them up -- the
   * settings view this store already holds says so -- without the offer
   * having to be raised again.
   */
  offerReady(offer: MeetingOfferPayload): boolean {
    return offer.ready || (this.view?.settings.enabled === true && this.view.usable)
  }

  /**
   * Answer an offer, by asking the shell to -- it owns the offer, and
   * settles its notification and tray item along with this banner. The offer
   * is dropped from the list only once the shell has actually answered:
   * previously it was dropped first, so a failed start (the mic already
   * claimed, the vault refusing to begin) lost the offer along with the
   * error, leaving nothing on screen to retry. `MeetingOfferBanner` shows
   * what is thrown; this just has to not hide the offer behind it.
   *
   * `take` and `join` share `startCapture`'s `starting` flag, so every
   * "Take notes" control in the window disables together.
   */
  async answerOffer(
    offer: MeetingOfferPayload,
    answer: MeetingOfferAnswer,
  ): Promise<Recording | null> {
    const begins = answer === 'take' || answer === 'join'
    if (begins) this.starting = true
    try {
      const recording = await api.meetingOfferAnswer(offer.key, answer)
      this.#dropOffer(offer.key)
      if (begins) {
        this.capture = await api.meetingStatus().catch(() => null)
        await this.refreshRecordings()
      }
      return recording
    } finally {
      if (begins) this.starting = false
    }
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

  async mockTriggerMicOffer(withEvent: boolean) {
    if (!isMock) return
    await callCommand('mock_trigger_mic_offer', { withEvent })
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
