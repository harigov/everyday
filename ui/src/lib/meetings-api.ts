// A thin wrapper around every meeting-notes service command.
//
// `docs/plans/meeting-notes.md` names about two dozen commands other agents
// are landing in parallel, in `crates/everyday-service` and
// `crates/everyday-service/surface.json`. Neither `surface.json` nor
// `ui/src/lib/generated/commands.ts` can be touched from here -- they are
// generated, and another agent owns them -- so `gen-api.mjs` cannot give any
// of these a typed home in `api.ts` yet. This file is `mail-api.ts`'s own
// pattern, applied to the meetings surface: each command called by its wire
// name through `callCommand`, the untyped escape hatch `api.ts` exists for
// exactly this. Every line below is marked so the supervisor can find and
// remove the whole file in one pass once the real commands land -- at that
// point `gen-api.mjs` gives each of these a typed method on `api`, the
// callers in `meetings.svelte.ts` and the components switch to calling that
// instead, and this file goes away.
//
// TODO(meetings): generated after merge

import { callCommand } from './api'
import type {
  ModelBenchmark,
  NoteId,
  NoteTemplate,
  Recording,
  RecordingId,
  RecordingQuery,
  SpeechModelInfo,
  Transcript,
  TranscriberConfig,
  VoiceprintId,
  VoiceprintInfo,
  MeetingSettings,
  MeetingSettingsView,
} from './types'

// ── Settings ──────────────────────────────────────────────────────────

export const meetingSettings = (): Promise<MeetingSettingsView> =>
  callCommand('meeting_settings', {})

export const saveMeetingSettings = (settings: MeetingSettings): Promise<MeetingSettingsView> =>
  callCommand('save_meeting_settings', { settings })

export const setTranscriberKey = (key: string | null): Promise<MeetingSettingsView> =>
  callCommand('set_transcriber_key', { key })

export const newMeetingTemplate = (): Promise<NoteTemplate> =>
  callCommand('new_meeting_template', {})

export const lintMeetingTemplate = (body: string): Promise<string[]> =>
  callCommand('lint_meeting_template', { body })

/** Renders with the real model -- may be slow, and may fail. */
export const previewMeetingTemplate = (body: string): Promise<string> =>
  callCommand('preview_meeting_template', { body })

/** Probe the configured transcriber backend. Rejects with the failure text. */
export const testTranscriber = (): Promise<void> => callCommand('test_transcriber', {})

// ── Recordings ────────────────────────────────────────────────────────

export const listRecordings = (query: RecordingQuery): Promise<Recording[]> =>
  callCommand('list_recordings', { query })

export const getRecording = (id: RecordingId): Promise<Recording> =>
  callCommand('get_recording', { id })

export const activeRecording = (): Promise<Recording | null> => callCommand('active_recording', {})

export const deleteRecording = (id: RecordingId): Promise<void> =>
  callCommand('delete_recording', { id })

export const retryRecording = (id: RecordingId): Promise<Recording> =>
  callCommand('retry_recording', { id })

export const discardRecording = (id: RecordingId): Promise<void> =>
  callCommand('discard_recording', { id })

// ── Transcripts ───────────────────────────────────────────────────────

export const getTranscript = (noteId: NoteId): Promise<Transcript | null> =>
  callCommand('get_transcript', { noteId })

export const nameSpeaker = (opts: {
  noteId: NoteId
  speakerKey: number
  name: string
  email?: string | null
}): Promise<Transcript> => callCommand('name_speaker', opts)

/** Proposed markdown body. The caller shows a diff and applies it itself. */
export const rewriteMeetingNote = (noteId: NoteId, templateId: string): Promise<string> =>
  callCommand('rewrite_meeting_note', { noteId, templateId })

// ── Voiceprints ───────────────────────────────────────────────────────

export const listVoiceprints = (): Promise<VoiceprintInfo[]> => callCommand('list_voiceprints', {})

export const deleteVoiceprint = (id: VoiceprintId): Promise<void> =>
  callCommand('delete_voiceprint', { id })

export const deleteAllVoiceprints = (): Promise<void> => callCommand('delete_all_voiceprints', {})

/** From a base64 PCM sample. The desktop capture path is `api.voiceEnrol` instead. */
export const enrolVoice = (pcm: string): Promise<VoiceprintInfo> =>
  callCommand('enrol_voice', { pcm })

// ── Local speech models ──────────────────────────────────────────────

export const speechModels = (): Promise<SpeechModelInfo[]> => callCommand('speech_models', {})

export const downloadSpeechModel = (id: string): Promise<void> =>
  callCommand('download_speech_model', { id })

export const cancelSpeechModelDownload = (id: string): Promise<void> =>
  callCommand('cancel_speech_model_download', { id })

export const deleteSpeechModel = (id: string): Promise<void> =>
  callCommand('delete_speech_model', { id })

export const benchmarkSpeechModel = (id: string): Promise<ModelBenchmark> =>
  callCommand('benchmark_speech_model', { id })

// ── Offers ────────────────────────────────────────────────────────────

export const dismissMeetingOffer = (eventId: string, never: boolean): Promise<void> =>
  callCommand('dismiss_meeting_offer', { eventId, never })

/** Re-exported so a caller that only wants the config type does not also
 *  need `./types` for it. */
export type { TranscriberConfig }
