// The meeting-notes half of the in-memory backend.
//
// Split out from `mock.ts` the way `mock-mail.ts` is, and for the same
// reason: this is a whole domain's worth of state -- settings, a recording
// history in every stage, transcripts, voiceprints, a model catalogue with a
// download that actually progresses -- and it deserves its own file rather
// than four hundred more lines in the one everything else already shares.
//
// One thing here that mail's mock does not need: a *capture* simulation.
// There is no native shell in a browser, so `meeting_start` / `meeting_stop`
// / `meeting_status` and the three events they drive
// (`meeting-status`, `meeting-still-on`, `meeting-offer`) are all reproduced
// here in JavaScript -- a `setInterval` standing in for the real four-times-
// a-second status poll, and `setTimeout`s standing in for a pipeline that
// would otherwise take minutes. `mock.ts` wires the wire-name commands to
// the functions below; `api.ts` wires the three event names to
// `mockOnMeetingStatus` and its two siblings.
//
// Dev-only triggers -- `mock_trigger_meeting_offer` and
// `mock_trigger_still_on` -- are mock-only commands, called through
// `meetings.svelte.ts`'s own use of the untyped `callCommand` escape hatch
// in `api.ts`, the same one a real build never has a reason to reach for.
// There is no real command of either name; they exist so the offer banner
// and the "still on the call?" prompt can be exercised from a browser with
// nothing running underneath. `MeetingsPanel.svelte` draws the buttons for
// them, guarded on `isMock`.

import { VaultError } from './types'
import type {
  CalendarFilter,
  CaptureStatus,
  EventRef,
  MeetingSettings,
  MeetingSettingsView,
  ModelBenchmark,
  Note,
  NoteTemplate,
  Recording,
  RecordingId,
  RecordingQuery,
  Segment,
  Speaker,
  SpeechModelInfo,
  Stage,
  Transcript,
  VoiceprintId,
  VoiceprintInfo,
} from './types'
import type { MeetingOfferPayload } from './api'

// ── Small local helpers, independent of mock.ts's own ──────────────────

let counter = 0
function newId(prefix: string): string {
  counter += 1
  return `${prefix}-${counter}`
}

function minutesAgo(n: number): string {
  return new Date(Date.now() - n * 60_000).toISOString()
}

function minutesFromNow(n: number): string {
  return new Date(Date.now() + n * 60_000).toISOString()
}

function para(...lines: string[]) {
  return lines.map((text) => ({
    type: 'paragraph',
    content: text ? [{ type: 'text', text }] : [],
  }))
}

// ── The placeholder cheat sheet, per docs/plans/meeting-notes.md ───────

const DEFAULT_TEMPLATE_BODY = `# {{title}}
{{date}}, {{start}}–{{end}} · {{present}}

## Summary
Three sentences: what the call was for and where it landed.

## Decisions
Bullets. Only what was actually agreed, with who agreed it.

## Action items
- [ ] Owner — what — by when, only if a date was said.
`

const BRIEF_TEMPLATE_BODY = `# {{title}}
{{date}} · {{present}}

## Summary
Two sentences.

## Action items
- [ ] Owner — what — by when, only if a date was said.
`

// ── Templates ────────────────────────────────────────────────────────

const templates: NoteTemplate[] = [
  { id: 'tpl-default', name: 'Default', body: DEFAULT_TEMPLATE_BODY },
  { id: 'tpl-brief', name: 'Brief', body: BRIEF_TEMPLATE_BODY },
]

// ── Settings ─────────────────────────────────────────────────────────

let settings: MeetingSettings = {
  enabled: false,
  offer: 'ask',
  calendars: { calendarIds: [], roleIds: [] } satisfies CalendarFilter,
  skippedSeries: [],
  transcriber: null,
  useAssistantKey: false,
  language: null,
  templates,
  defaultTemplate: 'tpl-default',
  voiceprints: false,
  autoStop: true,
  summaryBudget: null,
}

let hasKey = false
/** Set true the first time an OpenAI-shaped transcriber has ever had a key. */
const ASSISTANT_KEY_AVAILABLE = true

function view(): MeetingSettingsView {
  const t = settings.transcriber
  let usable = false
  let problem: string | null = 'Choose how speech becomes text.'
  if (t) {
    if (t.type === 'local') {
      const model = speechModels.find((m) => m.id === t.model)
      const kit = speechModels.find((m) => m.id === 'speechKit')
      if (model?.installed && kit?.installed) {
        usable = true
        problem = null
      } else {
        problem = 'Download the speech kit and this model first.'
      }
    } else if (t.type === 'compatible') {
      if (t.baseUrl.trim() && t.model.trim()) {
        usable = true
        problem = null
      } else {
        problem = 'Set a base URL and a model.'
      }
    } else {
      // openAi or google
      if (hasKey || (settings.useAssistantKey && ASSISTANT_KEY_AVAILABLE)) {
        usable = true
        problem = null
      } else {
        problem = 'Add a key, or use the assistant’s.'
      }
    }
  }
  return {
    settings,
    hasKey,
    usable,
    problem,
    assistantKeyAvailable: ASSISTANT_KEY_AVAILABLE,
    remote: t != null && t.type !== 'local',
    localSpeech: true,
  }
}

export function mockMeetingSettings(): MeetingSettingsView {
  return view()
}

export function mockSaveMeetingSettings(next: MeetingSettings): MeetingSettingsView {
  const previous = settings
  settings = { ...next, templates: next.templates.length ? next.templates : templates }
  const v = view()
  if (next.enabled && !v.usable) {
    // Mirrors the real command's own refusal: enabling with nothing to
    // transcribe with is rejected rather than silently accepted and left
    // dark. The attempted write does not stick.
    settings = previous
    throw new VaultError('unusable', v.problem ?? 'Meeting notes are not set up yet.')
  }
  return v
}

export function mockSetTranscriberKey(key: string | null): MeetingSettingsView {
  hasKey = key != null && key.trim().length > 0
  return view()
}

export function mockNewMeetingTemplate(): NoteTemplate {
  const t: NoteTemplate = {
    id: newId('tpl'),
    name: 'Untitled template',
    body: DEFAULT_TEMPLATE_BODY,
  }
  return t
}

const KNOWN_PLACEHOLDERS = [
  'title',
  'date',
  'start',
  'end',
  'duration',
  'organizer',
  'attendees',
  'present',
  'calendar',
]

export function mockLintMeetingTemplate(body: string): string[] {
  const warnings: string[] = []
  const used = [...body.matchAll(/\{\{(\w+)\}\}/g)].map((m) => m[1]!)
  for (const name of used) {
    if (!KNOWN_PLACEHOLDERS.includes(name)) {
      warnings.push(`{{${name}}} is not a placeholder this template understands.`)
    }
  }
  if (!body.includes('#')) warnings.push('No heading at all — the note will have no title line.')
  // A heading with nothing under it before the next heading (or the end).
  const lines = body.split('\n')
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]!
    if (!line.startsWith('#')) continue
    const rest = lines.slice(i + 1)
    const next = rest.findIndex((l) => l.startsWith('#'))
    const body_ = (next === -1 ? rest : rest.slice(0, next)).join('').trim()
    if (!body_) warnings.push(`“${line.replace(/^#+\s*/, '')}” has nothing under it.`)
  }
  return warnings
}

function fillPlaceholders(body: string): string {
  const values: Record<string, string> = {
    title: 'Design sync',
    date: 'Wednesday 16 September',
    start: '10:00',
    end: '10:30',
    duration: '30 minutes',
    organizer: 'Priya Iyer',
    attendees: 'You, Priya Iyer, Sam Okafor',
    present: 'You, Priya Iyer',
    calendar: 'Priya — Work',
  }
  return body.replace(/\{\{(\w+)\}\}/g, (m, name: string) => values[name] ?? m)
}

export async function mockPreviewMeetingTemplate(body: string): Promise<string> {
  // The real thing calls the assistant's model and can be slow or fail; the
  // mock just takes a moment, so the "Preview" button's loading state is
  // exercised without an actual model to ask.
  await new Promise((r) => setTimeout(r, 500))
  const filled = fillPlaceholders(body)
  // Replace an instruction paragraph -- the non-empty text under a heading
  // that is not itself a placeholder line -- with a canned sentence, the
  // same shape the real render produces: the heading survives, the
  // instruction does not.
  return filled
    .split('\n')
    .map((line) => {
      if (
        line.startsWith('#') ||
        line.startsWith('{{') ||
        line.trim() === '' ||
        line.startsWith('-')
      ) {
        return line
      }
      return 'Sample content the model would have written here, from the actual call.'
    })
    .join('\n')
}

export async function mockTestTranscriber(): Promise<void> {
  const v = view()
  if (!v.usable) throw new VaultError('unusable', v.problem ?? 'Nothing is configured yet.')
  await new Promise((r) => setTimeout(r, 400))
}

// ── Speech models ────────────────────────────────────────────────────

const speechModels: (SpeechModelInfo & { installed: boolean })[] = [
  {
    id: 'speechKit',
    name: 'Speech kit',
    description:
      'Voice detection and speaker separation. Needed by every transcriber, local or not.',
    languages: '',
    bytes: 42_000_000,
    installed: false,
    progress: null,
    error: null,
  },
  {
    id: 'parakeetV3',
    name: 'Parakeet TDT v3',
    description: 'English & European languages, faster',
    languages: 'English & the major European languages',
    bytes: 660_000_000,
    installed: false,
    progress: null,
    error: null,
  },
  {
    id: 'whisperTurbo',
    name: 'Whisper large-v3 turbo',
    description: 'Any language, slower',
    languages: 'Any language',
    bytes: 1_620_000_000,
    installed: false,
    progress: null,
    error: null,
  },
]

const downloadTimers = new Map<string, ReturnType<typeof setInterval>>()

export function mockSpeechModels(): SpeechModelInfo[] {
  return speechModels.map((m) => ({ ...m }))
}

export function mockDownloadSpeechModel(id: string): void {
  const model = speechModels.find((m) => m.id === id)
  if (!model || model.installed) return
  const existing = downloadTimers.get(id)
  if (existing) return
  model.error = null
  model.progress = { done: 0, total: model.bytes }
  const timer = setInterval(() => {
    if (!model.progress) return
    model.progress = {
      done: Math.min(model.bytes, model.progress.done + model.bytes / 8),
      total: model.bytes,
    }
    if (model.progress.done >= model.bytes) {
      clearInterval(timer)
      downloadTimers.delete(id)
      model.installed = true
      model.progress = null
    }
  }, 300)
  downloadTimers.set(id, timer)
}

export function mockCancelSpeechModelDownload(id: string): void {
  const timer = downloadTimers.get(id)
  if (timer) clearInterval(timer)
  downloadTimers.delete(id)
  const model = speechModels.find((m) => m.id === id)
  if (model) model.progress = null
}

export function mockDeleteSpeechModel(id: string): void {
  const model = speechModels.find((m) => m.id === id)
  if (!model) return
  mockCancelSpeechModelDownload(id)
  model.installed = false
}

export async function mockBenchmarkSpeechModel(id: string): Promise<ModelBenchmark> {
  await new Promise((r) => setTimeout(r, 600))
  const factor = id === 'parakeetV3' ? 4.2 : id === 'whisperTurbo' ? 1.1 : 2.5
  return { realtimeFactor: factor }
}

// ── Voiceprints ──────────────────────────────────────────────────────

let voiceprints: VoiceprintInfo[] = [
  {
    id: 'vp-you',
    name: 'You',
    isOwner: true,
    model: 'v1',
    samples: 4,
    createdAt: minutesAgo(60 * 24 * 20),
    updatedAt: minutesAgo(60 * 24 * 2),
  },
  {
    id: 'vp-priya',
    name: 'Priya Iyer',
    email: 'priya@example.com',
    isOwner: false,
    model: 'v1',
    samples: 2,
    createdAt: minutesAgo(60 * 24 * 10),
    updatedAt: minutesAgo(60 * 24 * 10),
  },
]

export function mockListVoiceprints(): VoiceprintInfo[] {
  return voiceprints
}

export function mockDeleteVoiceprint(id: VoiceprintId): void {
  voiceprints = voiceprints.filter((v) => v.id !== id)
}

export function mockDeleteAllVoiceprints(): void {
  voiceprints = []
}

export async function mockEnrolVoice(): Promise<VoiceprintInfo> {
  await new Promise((r) => setTimeout(r, 300))
  const existing = voiceprints.find((v) => v.isOwner)
  if (existing) {
    existing.samples += 1
    existing.updatedAt = new Date().toISOString()
    return existing
  }
  const v: VoiceprintInfo = {
    id: newId('vp'),
    name: 'You',
    isOwner: true,
    model: 'v1',
    samples: 1,
    createdAt: new Date().toISOString(),
    updatedAt: new Date().toISOString(),
  }
  voiceprints = [...voiceprints, v]
  return v
}

// ── The seed recordings and their notes/transcripts ─────────────────

function sampleEvent(overrides: Partial<EventRef> = {}): EventRef {
  return {
    calendarId: 'c-work',
    uid: 'evt-design-sync',
    title: 'Design sync',
    start: minutesAgo(45),
    end: minutesAgo(15),
    tz: Intl.DateTimeFormat().resolvedOptions().timeZone,
    organizer: 'priya@example.com',
    attendees: ['you@example.com', 'priya@example.com', 'sam@example.com'],
    joinUrl: 'https://meet.google.com/abc-defg-hij',
    calendarName: 'Priya — Work',
    ...overrides,
  }
}

const DONE_NOTE_ID = 'n-meeting-done'

const doneNote: Note = {
  id: DONE_NOTE_ID,
  title: 'Design sync',
  body: {
    type: 'doc',
    content: [
      { type: 'heading', attrs: { level: 2 }, content: [{ type: 'text', text: 'Summary' }] },
      ...para(
        'Priya and Sam walked through the new onboarding flow. The empty state was the main sticking point; everything else is close to ready.',
      ),
      { type: 'heading', attrs: { level: 2 }, content: [{ type: 'text', text: 'Decisions' }] },
      ...para('Ship the simplified empty state — agreed by Priya and you.'),
      { type: 'heading', attrs: { level: 2 }, content: [{ type: 'text', text: 'Action items' }] },
      ...para('Sam — mock up the empty state — by Friday.'),
    ],
  },
  tags: ['meetings'],
  pinned: false,
  attachments: [],
  createdAt: minutesAgo(15),
  updatedAt: minutesAgo(15),
}

function doneTranscript(): Transcript {
  const speakers: Speaker[] = [
    { key: 0, label: 'You', how: { type: 'owner' } },
    {
      key: 1,
      label: 'Priya Iyer',
      email: 'priya@example.com',
      voiceprintId: 'vp-priya',
      how: { type: 'matched', score: 0.91 },
    },
    { key: 2, label: 'Unknown 1', how: { type: 'unknown' } },
  ]
  const segments: Segment[] = [
    { startMs: 0, endMs: 4200, speaker: 1, text: "Let's start with the onboarding flow." },
    {
      startMs: 4300,
      endMs: 9800,
      speaker: 0,
      text: 'Sure — the main thing I want to get to is the empty state.',
    },
    {
      startMs: 9900,
      endMs: 15600,
      speaker: 2,
      text: 'I think the current one is a bit bare, honestly.',
    },
    { startMs: 15700, endMs: 22000, speaker: 1, text: 'Agreed. Can we simplify it before Friday?' },
    {
      startMs: 22100,
      endMs: 26000,
      speaker: 0,
      text: "Let's do that — I'll get Sam to mock it up.",
    },
  ]
  return {
    id: 'tr-done',
    noteId: DONE_NOTE_ID,
    recordingId: 'rec-done',
    language: 'en',
    backend: 'openAi',
    speakers,
    segments,
    createdAt: minutesAgo(14),
    updatedAt: minutesAgo(14),
  }
}

const transcripts = new Map<string, Transcript>([[DONE_NOTE_ID, doneTranscript()]])

let recordings: Recording[] = [
  {
    id: 'rec-transcribing',
    event: sampleEvent({
      uid: 'evt-standup',
      title: 'Morning standup',
      start: minutesAgo(8),
      end: minutesFromNow(7),
    }),
    title: 'Morning standup',
    startedAt: minutesAgo(8),
    endedAt: minutesAgo(0),
    stage: { type: 'transcribing' } satisfies Stage,
    templateId: 'tpl-default',
    automatic: true,
    chunks: [],
    partial: [],
    noteId: null,
    updatedAt: minutesAgo(0),
  },
  {
    id: 'rec-identifying',
    event: sampleEvent({
      uid: 'evt-1-1',
      title: '1:1 with Sam',
      start: minutesAgo(35),
      end: minutesAgo(5),
    }),
    title: '1:1 with Sam',
    startedAt: minutesAgo(35),
    endedAt: minutesAgo(4),
    stage: { type: 'identifying' } satisfies Stage,
    templateId: 'tpl-default',
    automatic: false,
    chunks: [],
    partial: [],
    noteId: null,
    updatedAt: minutesAgo(4),
  },
  {
    id: 'rec-summarising',
    event: sampleEvent({
      uid: 'evt-retro',
      title: 'Sprint retro',
      start: minutesAgo(70),
      end: minutesAgo(20),
    }),
    title: 'Sprint retro',
    startedAt: minutesAgo(70),
    endedAt: minutesAgo(19),
    stage: { type: 'summarising' } satisfies Stage,
    templateId: 'tpl-brief',
    automatic: false,
    chunks: [],
    partial: [],
    noteId: null,
    updatedAt: minutesAgo(19),
  },
  {
    id: 'rec-failed',
    event: sampleEvent({
      uid: 'evt-planning',
      title: 'Quarterly planning',
      start: minutesAgo(150),
      end: minutesAgo(90),
    }),
    title: 'Quarterly planning',
    startedAt: minutesAgo(150),
    endedAt: minutesAgo(89),
    stage: {
      type: 'failed',
      reason: 'The transcription service returned an error: rate limited. Try again in a minute.',
      at: { type: 'transcribing' },
    } satisfies Stage,
    templateId: 'tpl-default',
    automatic: false,
    chunks: [],
    partial: [],
    noteId: null,
    updatedAt: minutesAgo(89),
  },
  {
    id: 'rec-done',
    event: sampleEvent(),
    title: 'Design sync',
    startedAt: minutesAgo(45),
    endedAt: minutesAgo(15),
    stage: { type: 'done' } satisfies Stage,
    templateId: 'tpl-default',
    automatic: false,
    chunks: [],
    partial: [],
    noteId: DONE_NOTE_ID,
    updatedAt: minutesAgo(15),
  },
]

export function mockMeetingsSeedNotes(): Note[] {
  return [doneNote]
}

// ── The note sink: how the live simulation's pipeline writes a note ────
//
// `notes` is `mock.ts`'s own array, private to that module. Rather than
// export it, `mock.ts` hands this file one closure at load time that can add
// to it -- the smallest surface that lets a pipeline finishing here produce
// a note there.

interface NoteSink {
  addNote(note: Note): void
}
let sink: NoteSink | null = null
export function mockMeetingsInit(s: NoteSink) {
  sink = s
}

export function mockListRecordings(query: RecordingQuery): Recording[] {
  return recordings
    .filter((r) => {
      if (query.stages?.length && !query.stages.includes(r.stage.type)) return false
      if (query.calendarId && r.event?.calendarId !== query.calendarId) return false
      if (query.from && r.startedAt < query.from) return false
      if (query.to && r.startedAt > query.to) return false
      return true
    })
    .sort((a, b) => b.startedAt.localeCompare(a.startedAt))
    .slice(0, query.limit ?? undefined)
}

export function mockGetRecording(id: RecordingId): Recording {
  const r = recordings.find((x) => x.id === id)
  if (!r) throw new VaultError('notFound', 'no such recording')
  return r
}

export function mockActiveRecording(): Recording | null {
  if (!activeCapture) return null
  return recordings.find((r) => r.id === activeCapture!.recordingId) ?? null
}

export function mockDeleteRecording(id: RecordingId): void {
  recordings = recordings.filter((r) => r.id !== id)
}

export function mockRetryRecording(id: RecordingId): Recording {
  const r = recordings.find((x) => x.id === id)
  if (!r) throw new VaultError('notFound', 'no such recording')
  if (r.stage.type !== 'failed') return r
  // Back into the pipeline. The real backend resumes at the stage that
  // failed; the mock always restarts from transcribing, which is close
  // enough to exercise "Retry" without a second copy of the stage table.
  r.stage = { type: 'transcribing' }
  r.updatedAt = new Date().toISOString()
  runPipeline(r)
  return r
}

export function mockDiscardRecording(id: RecordingId): void {
  recordings = recordings.filter((r) => r.id !== id)
}

export function mockGetTranscript(noteId: string): Transcript | null {
  return transcripts.get(noteId) ?? null
}

export function mockNameSpeaker(opts: {
  noteId: string
  speakerKey: number
  name: string
  email?: string | null
}): Transcript {
  const t = transcripts.get(opts.noteId)
  if (!t) throw new VaultError('notFound', 'no transcript for that note')
  const speaker = t.speakers.find((s) => s.key === opts.speakerKey)
  if (!speaker) throw new VaultError('notFound', 'no such speaker')
  speaker.label = opts.name
  speaker.email = opts.email ?? speaker.email ?? null
  speaker.how = { type: 'named' }
  t.updatedAt = new Date().toISOString()
  return t
}

export async function mockRewriteMeetingNote(noteId: string, templateId: string): Promise<string> {
  const tpl = templates.find((t) => t.id === templateId) ?? templates[0]!
  await new Promise((r) => setTimeout(r, 500))
  const t = transcripts.get(noteId)
  const present = t
    ? [
        ...new Set(
          t.segments.map((s) => t.speakers.find((sp) => sp.key === s.speaker)?.label ?? 'Unknown'),
        ),
      ].join(', ')
    : 'You'
  return fillPlaceholders(tpl.body)
    .replace('{{present}}', present)
    .split('\n')
    .map((line) => {
      if (line.startsWith('#') || line.trim() === '' || line.startsWith('-')) return line
      return 'Rewritten from the transcript, now that a speaker has been named.'
    })
    .join('\n')
}

export function mockDismissMeetingOffer(uid: string, never: boolean): void {
  if (never && !settings.skippedSeries.includes(uid)) {
    settings = { ...settings, skippedSeries: [...settings.skippedSeries, uid] }
  }
}

// ── Capture simulation ──────────────────────────────────────────────

let activeCapture: CaptureStatus | null = null
let captureTimer: ReturnType<typeof setInterval> | null = null

const statusHandlers = new Set<(s: CaptureStatus | null) => void>()
const stillOnHandlers = new Set<(id: RecordingId) => void>()
const offerHandlers = new Set<(o: MeetingOfferPayload) => void>()

export function mockOnMeetingStatus(handler: (s: CaptureStatus | null) => void): void {
  statusHandlers.add(handler)
}
export function mockOnMeetingStillOn(handler: (id: RecordingId) => void): void {
  stillOnHandlers.add(handler)
}
export function mockOnMeetingOffer(handler: (o: MeetingOfferPayload) => void): void {
  offerHandlers.add(handler)
}

function emitStatus() {
  const snapshot = activeCapture ? { ...activeCapture } : null
  for (const h of statusHandlers) h(snapshot)
}

export function mockMeetingStatus(): CaptureStatus | null {
  return activeCapture ? { ...activeCapture } : null
}

export function mockMeetingStart(opts: {
  eventId?: string | null
  title?: string | null
  templateId?: string | null
  automatic?: boolean
}): Recording {
  if (activeCapture) throw new VaultError('busy', 'Already recording a call.')
  const event = opts.eventId
    ? sampleEvent({ uid: opts.eventId, title: opts.title ?? undefined })
    : null
  const r: Recording = {
    id: newId('rec'),
    event,
    title: opts.title ?? event?.title ?? 'Recording',
    startedAt: new Date().toISOString(),
    endedAt: null,
    stage: { type: 'recording' },
    templateId: opts.templateId ?? settings.defaultTemplate ?? templates[0]!.id,
    automatic: opts.automatic ?? false,
    chunks: [],
    partial: [],
    noteId: null,
    updatedAt: new Date().toISOString(),
  }
  recordings = [r, ...recordings]
  activeCapture = {
    recordingId: r.id,
    title: r.title,
    automatic: r.automatic,
    elapsedMs: 0,
    micLevel: 0,
    systemLevel: 0,
    systemSilentMs: 0,
    systemUnavailable: null,
    // Armed against the event's own end, when there is one to arm against.
    autoStopAt: settings.autoStop && event ? event.end : null,
  }
  if (captureTimer) clearInterval(captureTimer)
  captureTimer = setInterval(() => {
    if (!activeCapture) return
    activeCapture = {
      ...activeCapture,
      elapsedMs: activeCapture.elapsedMs + 300,
      micLevel: 0.15 + Math.random() * 0.5,
      systemLevel: 0.1 + Math.random() * 0.45,
    }
    emitStatus()
  }, 300)
  emitStatus()
  return r
}

export function mockMeetingStop(discard: boolean): void {
  if (!activeCapture) return
  const id = activeCapture.recordingId
  if (captureTimer) clearInterval(captureTimer)
  captureTimer = null
  activeCapture = null
  emitStatus()
  const r = recordings.find((x) => x.id === id)
  if (!r) return
  if (discard) {
    recordings = recordings.filter((x) => x.id !== id)
    return
  }
  r.endedAt = new Date().toISOString()
  r.stage = { type: 'transcribing' }
  r.updatedAt = new Date().toISOString()
  runPipeline(r)
}

/** Walk a recording through the rest of the pipeline, a second or so a stage. */
function runPipeline(r: Recording) {
  const STAGE_MS = 900
  const advance = (next: Stage) => {
    const live = recordings.find((x) => x.id === r.id)
    if (!live) return
    live.stage = next
    live.updatedAt = new Date().toISOString()
  }
  setTimeout(() => {
    advance({ type: 'identifying' })
    setTimeout(() => {
      advance({ type: 'summarising' })
      setTimeout(() => {
        const live = recordings.find((x) => x.id === r.id)
        if (!live) return
        const noteId = newId('n-meeting')
        const speakers: Speaker[] = [
          { key: 0, label: 'You', how: { type: 'owner' } },
          { key: 1, label: 'Unknown 1', how: { type: 'unknown' } },
        ]
        const segments: Segment[] = [
          { startMs: 0, endMs: 3000, speaker: 1, text: 'Right, shall we get started?' },
          { startMs: 3100, endMs: 7000, speaker: 0, text: "Yes — let's go through the agenda." },
        ]
        transcripts.set(noteId, {
          id: newId('tr'),
          noteId,
          recordingId: live.id,
          language: 'en',
          backend: 'local',
          speakers,
          segments,
          createdAt: new Date().toISOString(),
          updatedAt: new Date().toISOString(),
        })
        const note: Note = {
          id: noteId,
          title: live.title,
          body: {
            type: 'doc',
            content: [
              {
                type: 'heading',
                attrs: { level: 2 },
                content: [{ type: 'text', text: 'Summary' }],
              },
              ...para('Written from the recording just finished.'),
            ],
          },
          tags: ['meetings'],
          pinned: false,
          attachments: [],
          createdAt: new Date().toISOString(),
          updatedAt: new Date().toISOString(),
        }
        sink?.addNote(note)
        live.noteId = noteId
        live.stage = { type: 'done' }
        live.updatedAt = new Date().toISOString()
      }, STAGE_MS)
    }, STAGE_MS)
  }, STAGE_MS)
}

// ── Dev-only triggers ────────────────────────────────────────────────
//
// `mock_trigger_meeting_offer` and `mock_trigger_still_on` are not real
// commands -- see the module doc. Called by `MeetingsPanel`'s "Simulate"
// section, shown only when `isMock`.

export function mockTriggerMeetingOffer(automatic: boolean): void {
  const eventId = newId('evt-offer')
  const payload: MeetingOfferPayload = {
    eventId,
    calendarId: newId('cal-offer'),
    uid: `evt-offer-${eventId}`,
    title: 'Design sync',
    start: new Date().toISOString(),
    end: minutesFromNow(30),
    calendarName: 'Priya — Work',
    automatic,
  }
  if (automatic) {
    const r = mockMeetingStart({ eventId, title: payload.title, automatic: true })
    payload.recordingId = r.id
  }
  for (const h of offerHandlers) h(payload)
}

export function mockTriggerStillOn(): void {
  if (!activeCapture) return
  for (const h of stillOnHandlers) h(activeCapture.recordingId)
}

/** So a screenshot can show the warning without waiting a real minute. */
export function mockTriggerSystemSilent(): void {
  if (!activeCapture) return
  activeCapture = { ...activeCapture, systemSilentMs: 70_000 }
  emitStatus()
}
