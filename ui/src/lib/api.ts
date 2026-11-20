// The single boundary between the interface and the Rust core.
//
// Everything the UI can do to a vault goes through here, which means the
// mock backend below is a complete substitute: `KARMAYOGA_MOCK=1 npm run dev`
// runs the entire interface in a browser with no Rust, no vault and no
// native dependencies. That is what makes the design workable on its own.

import type {
  Account,
  AccountId,
  AccountKind,
  BankAccount,
  BankAccountId,
  BankLink,
  BankLinkId,
  CsvMapping,
  IllustrationMarkup,
  ImageShape,
  ImportFormat,
  MoneyCategory,
  MoneyCategoryId,
  MoneyRule,
  MoneyRuleId,
  NewTxn,
  PlaidEnvironment,
  TxnId,
  TxnPatch,
  TxnQuery,
  BrowserConnection,
  ScreenApp,
  ScreenAppId,
  ScreenCategory,
  ScreenCategoryId,
  ScreenQuery,
  ScreenTimeSettings,
  ScreenTimeStatus,
  Album,
  AlbumId,
  FileEntry,
  FileId,
  FileOrigin,
  FileQuery,
  FileUpdate,
  FilesSettings,
  Allergy,
  AllergyId,
  Condition,
  ConditionId,
  DictationEvent,
  DocKind,
  Dose,
  DoseId,
  Food,
  FoodId,
  HealthDocument,
  HealthDocumentId,
  HealthSettings,
  HealthSource,
  HealthSourceId,
  ImportStatus,
  LabResult,
  LabResultId,
  ManualReading,
  Meal,
  MealId,
  Medication,
  MedicationId,
  Metric,
  PushHeaders,
  PushSetup,
  SeriesQuery,
  TemplateChoice,
  TemplateKey,
  Workout,
  WorkoutId,
  DeclineReason,
  AgentCallerKind,
  AgentEvent,
  OnScreen,
  AgentMailAccess,
  Attachment,
  AgentSettings,
  BackupSettings,
  BadgeSources,
  BlockId,
  BlockKind,
  BlockQuery,
  BlockSubject,
  Bootstrap,
  Calendar,
  CalendarId,
  CaptureStatus,
  ChangeEvent,
  ChatLink,
  ChatLinkId,
  Connected,
  Connection,
  ConversationId,
  Destination,
  Draft,
  DraftId,
  Entry,
  EntryId,
  EntryQuery,
  EventDraft,
  EventId,
  EventQuery,
  EventScope,
  Goal,
  GoalId,
  GoalQuery,
  HotkeyStatus,
  ImportMode,
  Item,
  ItemId,
  ItemQuery,
  ItemStatus,
  JobId,
  JobQuery,
  Journal,
  JournalId,
  Kind,
  KeepInTouchSettings,
  KindId,
  ListenEvent,
  ListenSource,
  LogEntry,
  LogEvent,
  LogId,
  LogQuery,
  MailAgentOriginKind,
  MailboxId,
  MailMessageId,
  MailSorting,
  MailWriting,
  McpStatus,
  MeetingSettings,
  Memory,
  MemoryId,
  MemoryOrigin,
  ModelSettings,
  ModelSlot,
  NewSource,
  NewsSource,
  NewsSourceId,
  Note,
  NoteId,
  NoteQuery,
  PickedFile,
  Profile,
  Project,
  ProjectId,
  ProposalId,
  ProposalQuery,
  ProposedRecord,
  Reading,
  ReadingId,
  ReadingQuery,
  Recording,
  RecordingId,
  RecordingQuery,
  Recurrence,
  Role,
  RoleId,
  Routine,
  RoutineId,
  RoutineRunId,
  RunQuery,
  SearchKind,
  SearchRequest,
  SearchResult,
  SeriesScope,
  ShareStatus,
  ShellNotification,
  Skill,
  SkillId,
  StoryId,
  StoryQuery,
  Task,
  TaskId,
  TaskQuery,
  TaskStatus,
  Template,
  TemplateId,
  TemplateUse,
  ThreadFilter,
  InboxSendersArgs,
  ThreadId,
  TimeBlock,
  Tracker,
  TrackerId,
  TrackerKind,
  TrayMenuItem,
  VaultStatus,
  VerificationAnswer,
  VoiceEvent,
  VoiceprintId,
  VoiceprintInfo,
  WebStatus,
  WritingSettings,
  VoiceTypingSettings,
  VoiceTypingKeyMode,
  McpConnection,
  ConnectionAuth,
  ConnectionId,
  ConnectionSecretsPatch,
  ConnectionTransport,
  ToolPolicy,
} from './types'
import { VaultError } from './types'
import { COMMAND_NAMES, SERVICE_COMMANDS } from './generated/commands'
import type { Commands } from './generated/commands'
import type { Section } from './apps'
import type { MockDictationControls } from './mock-dictation'
import type { MockListenControls } from './mock-listen'
import {
  mediaBase as webMediaBase,
  onWebChange,
  onWebLockState,
  onWebNotification,
  webInvoke,
  webSendMessage,
  webUploadChunk,
} from './web'

// Decided at BUILD time, not run time.
//
// `import.meta.env.DEV` is substituted with a literal by Vite, so a
// production bundle evaluates this to `false` and the mock module below is
// statically eliminated -- it is not merely unused, it is not shipped.
//
// The runtime check is deliberately *inside* the dev guard. A release build
// that cannot reach Tauri must fail loudly, not quietly serve a fake journal
// with sample entries in it: for a journal app, that failure mode is
// indistinguishable from having lost everything.
/** Inside the desktop app's webview, with its shell to talk to. */
const SHELL = '__TAURI_INTERNALS__' in window
const MOCK = import.meta.env.DEV && !SHELL
/**
 * A built interface in an ordinary browser: served by the machine holding
 * the vault, over web access (`docs/plans/web-access.md`), and talking to it
 * through `web.ts`.
 */
const WEB = !SHELL && !MOCK

/**
 * `args` is a bag of named arguments for every command but one. `put_blob`
 * passes a bare `Uint8Array`, which Tauri sends as a raw body rather than as
 * JSON -- see `putBlob` for why that distinction is worth the wider type.
 */
type InvokeArgs = Record<string, unknown> | Uint8Array

type Invoke = <T>(cmd: string, args?: InvokeArgs, requestId?: string) => Promise<T>

let invoke: Invoke = async () => {
  throw new VaultError(
    'unavailable',
    'Karma Yoga could not reach its storage backend. Your journal has not been ' +
      'touched; this is a problem with the application, not with your data.',
  )
}

/**
 * Register the handler for the shell's save-before-close request.
 *
 * The window cancels its own close, asks here, and waits for
 * `api.readyToClose`. Outside Tauri there is no such handshake, so this is a
 * no-op and the mock interface closes the way a browser tab does.
 */
export let onSaveAndClose: (handler: () => void | Promise<void>) => void = () => {}

/**
 * Register the handler for notifications raised by the Rust shell.
 *
 * The shell does work the interface never asked for -- refreshing subscribed
 * calendars on a timer -- and this is how it says something about it. The
 * payload is a `NotifySpec` minus the parts only a component could supply,
 * so `notify.svelte.ts` can post it unchanged.
 *
 * Outside Tauri nothing ever emits, so this is a no-op.
 */
export let onShellNotification: (handler: (spec: ShellNotification) => void) => void = () => {}

/**
 * Register the handler for a chosen tray menu item.
 *
 * The payload is the id the interface gave the item in `set_tray_menu`; the
 * routing back to a function lives in `lib/tray.svelte.ts`. A no-op outside
 * Tauri, where there is no tray to choose anything from.
 */
export let onTrayAction: (handler: (id: string) => void) => void = () => {}

/**
 * Register the handler for a write that landed somewhere else.
 *
 * On a local vault, "somewhere else" is another command in this window, and the
 * store that made it ignores its own by origin. Under server mode it is another
 * machine, and this is how a list learns to reload. See `lib/live.svelte.ts`.
 */
export let onChange: (handler: (change: ChangeEvent) => void) => void = () => {}

/**
 * Register the handler for the vault locking or unlocking.
 *
 * Replaces polling for a remote client, where `pollAutoLock` every few seconds
 * would be a round trip every few seconds. The local window still polls,
 * because there the poll is an integer comparison.
 */
export let onLockState: (handler: (locked: boolean) => void) => void = () => {}

/**
 * Register the handler for the OS-wide hotkey.
 *
 * The shell raises the window and emits this; the palette opens here. Outside
 * Tauri there is no desktop to claim a key from, so this is a no-op.
 */
export let onPalette: (handler: () => void) => void = () => {}

/**
 * Register the handler for "open the calendar", raised when the tray's
 * optional meeting line (or its "Open" item) is chosen.
 *
 * The shell raises the window on its own before emitting this -- see
 * `tray.rs`'s `OPEN_MEETING_ID` -- so the handler's whole job is switching
 * to the calendar app section; `lib/tray.svelte.ts` is where that happens.
 * Outside Tauri there is no tray to click, so this is a no-op.
 */
export let onOpenCalendar: (handler: () => void) => void = () => {}

/** What a window of one app hands back to the main window -- see `showInMain`. */
export interface ShowInMain {
  section: Section | null
  settings: string | null
}

/**
 * Register the main window's handler for a window of one app asking it to
 * show something that is not that app: another app, or Settings. The shell
 * has raised the main window already. See `lib/appwindow.ts`.
 */
export let onShowInMain: (handler: (asked: ShowInMain) => void) => void = () => {}

// ── Meeting capture, from the Tauri shell ───────────────────────────────
//
// Three events the native capture code raises, per `docs/plans/meeting-notes.md`
// -- not vault writes, so they do not go through `onChange`. `meeting-status`
// arrives about four times a second while a recording is live and carries
// `null` the moment it stops; `meeting-still-on` is the "are you still on
// the call?" prompt after a long silence; `meeting-offer` is the "take notes
// for this?" banner, whether raised by a press (`automatic: false`) or
// already recording because "Always" is on (`automatic: true`).
//
// Outside Tauri there is no capture to hear from, so these stay the
// default no-ops -- except in mock mode, where `mock.ts` reassigns them to a
// small in-page pub/sub so the pill and the offer banner can be exercised
// with nothing but a browser. See `meetings.svelte.ts`'s "mock capture" for
// the trigger side.

/**
 * What `meeting-offer` carries. Not a stored record -- see the module doc.
 *
 * An offer is pending in the desktop shell until somebody answers it -- from
 * the banner, from the notification the shell raises, or from its tray --
 * so the shell, not this window, owns the list: `meetingOffers` reads it,
 * `meetingOfferAnswer` answers it, and `meeting-offer-settled` says one was
 * answered elsewhere (or the call ended). A calendar offer is about an event
 * with other people in it, about a minute before it starts, whether or not it
 * has a join link; a microphone offer is raised when another application
 * takes the microphone, tied to the calendar meeting on now if there is one
 * (then `eventId` and the rest are set) and standalone if not (then they are
 * null). `ready: false` means meeting notes are not set up yet, so the offer
 * asks to set them up rather than to record.
 *
 * `calendarId`, `uid` and `series` ride along beside `eventId` for
 * the shell's dismissal of a calendar offer to use: `eventId` is a feed event's own id, not
 * stable across a resync, so a "Never for this meeting" click that lands
 * after one would look up the wrong event, or none. `calendarId` and `uid`
 * are what survives that. `series` is needed too, and separately from
 * `uid`: a recurring Google or Microsoft event hands back a fresh `uid` for
 * every occurrence, so without a durable series id of its own, dismissing
 * one occurrence could only ever skip that one, not the series.
 */
export interface MeetingOfferPayload {
  /**
   * `event:{calendarId}:{uid}` for an offer about a calendar event (however
   * it was noticed), `mic:{n}` for a microphone offer with no event. What
   * `meetingOfferAnswer` and the shell's `meeting-offer-settled` event name
   * an offer by.
   */
  key: string
  /** How the meeting was noticed: its calendar event starting, or another app taking the microphone. */
  source: 'calendar' | 'microphone'
  eventId: string | null
  calendarId: string | null
  uid: string | null
  series?: string | null
  title: string
  start: string | null
  end: string | null
  calendarName: string | null
  /** The application using the microphone, for a microphone offer. */
  app: string | null
  /** The event's join link, for "Join & take notes". */
  joinUrl: string | null
  /** False: meeting notes are not set up yet, so the offer asks to set them up. */
  ready: boolean
  /** The shell has already started recording; this is a toast, not an ask. */
  automatic: boolean
  /** The corner card and the countdown, instead of a system notification. */
  autoRecord: boolean
  recordingId?: string | null
}

/** How an offer was answered. `never`: for a calendar offer, never for this
 *  meeting's series; for a microphone offer, stop offering when the
 *  microphone turns on. */
export type MeetingOfferAnswer = 'take' | 'join' | 'notNow' | 'never'

// The other three offer events, besides `meeting-offer` itself.
//
// `meeting-offer-settled` -- the offer was answered somewhere else, or the
// call or event it was about is over; drop it. `meeting-offer-failed` --
// answering from the notification or tray failed (`key` is "" for the
// tray's generic "Take notes now"). `meeting-offer-setup` -- "Set up meeting
// notes" was pressed on a notification, so open Settings -> Meetings.
export let onMeetingOfferSettled: (handler: (key: string) => void) => void = () => {}
export let onMeetingOfferFailed: (
  handler: (key: string, message: string) => void,
) => void = () => {}
export let onMeetingOfferSetup: (handler: (key: string) => void) => void = () => {}

export let onMeetingStatus: (handler: (status: CaptureStatus | null) => void) => void = () => {}
export let onMeetingStillOn: (handler: (recordingId: RecordingId) => void) => void = () => {}
export let onMeetingOffer: (handler: (offer: MeetingOfferPayload) => void) => void = () => {}
/** `{ key, seconds }`. `seconds` is null once the countdown has gone. */
export let onMeetingCountdown: (
  handler: (key: string, seconds: number | null) => void,
) => void = () => {}

/**
 * Say something to the assistant, streaming what it says back.
 *
 * Separate from the `api` object below because it is the one call that is not
 * request/response: a turn takes seconds and runs tools while it does, so the
 * reply arrives on a channel and this resolves only when the turn is over.
 * Rejects with the same `VaultError` every other command does.
 *
 * `attachments` are pictures sent with `prompt`, each made ready by
 * `api.preparePicture` first. `files` are whatever else was handed over, by
 * id: files already in the files catalog (`addFiles` puts them there). A
 * picture can be in both -- its catalogued file, and the small copy a model
 * is shown.
 *
 * Replaced with the mock implementation in dev-without-Tauri, so the panel is
 * as usable in a browser as the rest of the interface.
 */
export let sendMessage: (
  conversationId: ConversationId,
  prompt: string,
  onScreen: OnScreen | null,
  attachments: Attachment[],
  files: FileId[],
  onEvent: (event: AgentEvent) => void,
) => Promise<void> = async () => {
  throw new VaultError('unavailable', 'The assistant could not reach its backend.')
}

/**
 * Talk to the assistant out loud in `conversationId`, streaming what the
 * session does back.
 *
 * Like `sendMessage` it is not request/response: the session is a long run of
 * events on a channel, and the last is always `ended`. Resolves once the
 * session is under way, not when it is over. Stopping, muting and telling it
 * what is on screen are separate calls in `api` (`voiceStop`, `voiceSetMuted`,
 * `voiceSetScreen`), because they reach the shell's microphone, not the vault.
 *
 * Rejects on the web and for a window looking at another computer's vault:
 * the microphone and speakers are this computer's.
 */
export let voiceStart: (
  conversationId: ConversationId,
  onScreen: OnScreen | null,
  onEvent: (event: VoiceEvent) => void,
) => Promise<void> = async () => {
  throw new VaultError('unavailable', 'Talking to the assistant is only in the desktop app.')
}

/**
 * Read `source` aloud at `speed`, streaming what the session does back.
 *
 * As `voiceStart`: a long run of events on a channel, the last always `ended`,
 * and it resolves once the session is under way. Starting while something is
 * being read replaces it. Pausing, skipping, the speed and stopping are
 * separate calls in `api` (`listenPause` and the rest), because they reach the
 * shell's speakers, not the vault.
 *
 * Rejects on the web and for a window looking at another computer's vault.
 */
export let listenStart: (
  source: ListenSource,
  speed: number,
  onEvent: (event: ListenEvent) => void,
) => Promise<void> = async () => {
  throw new VaultError('unavailable', 'Listening is only in the desktop app.')
}

/**
 * Listen for what is said, streaming the words heard so far. As `voiceStart`;
 * ending it is `api.dictationStop` (keep the words) or `api.dictationCancel`.
 */
export let dictationStart: (
  onEvent: (event: DictationEvent) => void,
) => Promise<void> = async () => {
  throw new VaultError('unavailable', 'Dictating is only in the desktop app.')
}

if (SHELL) {
  const { listen } = await import('@tauri-apps/api/event')
  const { getCurrentWebviewWindow } = await import('@tauri-apps/api/webviewWindow')
  onSaveAndClose = (handler) => {
    // On this window, not `listen`'s every window: the shell asks each
    // window by name, and a window of one app closing must not take the
    // main window with it. See `windows.rs`.
    void getCurrentWebviewWindow().listen('karmayoga://save-and-close', () => void handler())
  }
  onShellNotification = (handler) => {
    void listen<ShellNotification>('karmayoga://notify', (event) => handler(event.payload))
  }
  onTrayAction = (handler) => {
    void listen<string>('karmayoga://tray-action', (e) => handler(e.payload))
  }
  onChange = (handler) => {
    void listen<ChangeEvent>('karmayoga://changed', (e) => handler(e.payload))
  }
  onLockState = (handler) => {
    void listen<boolean>('karmayoga://lock-state', (e) => handler(e.payload))
  }
  onPalette = (handler) => {
    void listen('karmayoga://palette', () => handler())
  }
  onOpenCalendar = (handler) => {
    void listen('karmayoga://open-calendar', () => handler())
  }
  onShowInMain = (handler) => {
    void listen<ShowInMain>('karmayoga://show-in-main', (e) => handler(e.payload))
  }
  onMeetingStatus = (handler) => {
    void listen<CaptureStatus | null>('meeting-status', (e) => handler(e.payload))
  }
  onMeetingStillOn = (handler) => {
    // `capture.rs` emits the bare `RecordingId`, not `{ recordingId }` --
    // see `STILL_ON_EVENT`'s one call site.
    void listen<RecordingId>('meeting-still-on', (e) => handler(e.payload))
  }
  onMeetingOffer = (handler) => {
    void listen<MeetingOfferPayload>('meeting-offer', (e) => handler(e.payload))
  }
  onMeetingOfferSettled = (handler) => {
    void listen<{ key: string }>('meeting-offer-settled', (e) => handler(e.payload.key))
  }
  onMeetingOfferFailed = (handler) => {
    void listen<{ key: string; message: string }>('meeting-offer-failed', (e) =>
      handler(e.payload.key, e.payload.message),
    )
  }
  onMeetingOfferSetup = (handler) => {
    void listen<{ key: string }>('meeting-offer-setup', (e) => handler(e.payload.key))
  }
  onMeetingCountdown = (handler) => {
    void listen<{ key: string; seconds: number | null }>('meeting-countdown', (e) =>
      handler(e.payload.key, e.payload.seconds),
    )
  }

  const mod = await import('@tauri-apps/api/core')
  sendMessage = async (conversationId, prompt, onScreen, attachments, files, onEvent) => {
    const channel = new mod.Channel<AgentEvent>()
    channel.onmessage = onEvent
    // By name, not through `call`: a turn answers with a stream rather than a
    // value, so the shell has a command of its own for it. The generated
    // `SERVICE_COMMANDS` leaves the streaming ones out for the same reason.
    await mod.invoke<void>('send_message', {
      conversationId,
      prompt,
      onScreen,
      attachments,
      files,
      channel,
    })
  }
  /**
   * Start one of the shell's sessions that answer on a channel rather than
   * with a value -- talking, listening, dictating -- with `onEvent` hearing
   * all it says. Errors as `invoke` below gives them, which this does not go
   * through.
   */
  const startSession = async <E>(
    cmd: string,
    args: Record<string, unknown>,
    onEvent: (event: E) => void,
  ): Promise<void> => {
    const channel = new mod.Channel<E>()
    channel.onmessage = onEvent
    try {
      await mod.invoke<void>(cmd, { ...args, channel })
    } catch (raw) {
      if (raw && typeof raw === 'object' && 'code' in raw && 'message' in raw) {
        throw new VaultError(String(raw.code), String(raw.message))
      }
      throw new VaultError('unknown', String(raw))
    }
  }
  voiceStart = (conversationId, onScreen, onEvent) =>
    startSession<VoiceEvent>('voice_start', { conversationId, onScreen }, onEvent)
  listenStart = (source, speed, onEvent) =>
    startSession<ListenEvent>('listen_start', { source, speed }, onEvent)
  dictationStart = (onEvent) => startSession<DictationEvent>('dictation_start', {}, onEvent)
  invoke = async <T>(cmd: string, args?: InvokeArgs, requestId?: string): Promise<T> => {
    try {
      // Two kinds of call, and the split is not arbitrary. Almost everything
      // is a *service* command -- one entry in a table the Rust owns -- and
      // goes through the shell's single `call`, which either runs it here or
      // forwards it to whichever machine holds the vault. What is left is the
      // handful the shell alone can do: opening a vault, the tray, the raw
      // attachment body. Those are called by name.
      //
      // The set is generated from the Rust table, so the shell's half is
      // whatever is left over and cannot drift.
      if (!SERVICE_COMMANDS.has(cmd)) return await mod.invoke<T>(cmd, args)
      return await mod.invoke<T>('call', {
        name: cmd,
        args: args ?? {},
        requestId: requestId ?? null,
      })
    } catch (raw) {
      // Commands reject with `{ code, message }`; anything else is a bug in
      // the bridge and should surface as-is rather than be swallowed.
      if (raw && typeof raw === 'object' && 'code' in raw && 'message' in raw) {
        throw new VaultError(String(raw.code), String(raw.message))
      }
      throw new VaultError('unknown', String(raw))
    }
  }
} else if (WEB) {
  // Three of the events above arrive on web access's own stream, and the
  // rest -- the tray, the palette's OS-wide key, windows of one app, meeting
  // capture -- have nothing in a browser to come from.
  invoke = webInvoke
  sendMessage = webSendMessage
  onChange = onWebChange
  onLockState = onWebLockState
  onShellNotification = onWebNotification
} else {
  const {
    mockInvoke,
    mockOnChange,
    mockOnMeetingCountdown,
    mockOnMeetingOffer,
    mockOnMeetingOfferFailed,
    mockOnMeetingOfferSettled,
    mockOnMeetingOfferSetup,
    mockOnMeetingStatus,
    mockOnMeetingStillOn,
  } = await import('./mock')
  invoke = mockInvoke
  // Only the simulated research job raises change events -- every other mock
  // write is read back by the store that made it -- but a job moves on its
  // own, and a card that only changed when somebody reloaded would not show
  // what it is for. See `mock-jobs.ts`.
  onChange = mockOnChange
  // A real backend emits these from native capture code; the mock has none,
  // so it stands in with its own pub/sub, driven by `meetings.svelte.ts`'s
  // "mock capture" simulation rather than by anything crossing a bridge.
  onMeetingStatus = mockOnMeetingStatus
  onMeetingStillOn = mockOnMeetingStillOn
  onMeetingOffer = mockOnMeetingOffer
  onMeetingOfferSettled = mockOnMeetingOfferSettled
  onMeetingOfferFailed = mockOnMeetingOfferFailed
  onMeetingOfferSetup = mockOnMeetingOfferSetup
  onMeetingCountdown = mockOnMeetingCountdown
}

/**
 * An id for one *logical* write, so a retry of it is answered from the first
 * attempt rather than applied twice.
 *
 * The caller mints it, and that is the whole of the contract: an id must be
 * reused across retries of the same write and must not be reused for a
 * different one. Nothing here can decide that, which is why this is not done
 * automatically.
 *
 * It was, briefly, and the mistake is worth recording. Minting one per
 * `invoke` call gave every retry a *fresh* id, so the layer never engaged --
 * protection that looked present and was not. Deriving one by hashing the
 * arguments looks better and is worse: two identical writes are sometimes two
 * writes on purpose -- a second dose recorded at the same minute, the same
 * book added to a shelf twice -- and deduplicating those loses data silently.
 *
 * So: no id unless a caller has reasoned about it. Today that is the entry
 * autosave, which is the one retry loop in the interface whose write is
 * conditional and therefore the one that this failure mode actually bites.
 *
 * A counter behind a random prefix, rather than `crypto.randomUUID`, which
 * needs a secure context the packaged webview does not always provide -- the
 * same reason ids for records are minted in Rust. Uniqueness only has to hold
 * within one connection, and a prefix plus a counter gives that.
 */
const REQUEST_PREFIX = Math.random().toString(36).slice(2, 10)
let requestCounter = 0
export function newRequestId(): string {
  requestCounter += 1
  return `${REQUEST_PREFIX}-${requestCounter}`
}

export const isMock = MOCK
/** A browser on web access, with no desktop shell behind it. */
export const isWeb = WEB
/** The desktop shell is here: a tray, windows, files saved by the shell. */
export const hasShell = SHELL

/**
 * The zone this window reckons days in, for a command that buckets instants
 * into them. Undefined where the runtime cannot name it, which leaves the
 * service to use the machine's -- the same machine, for the desktop app.
 */
function localZone(): string | undefined {
  return Intl.DateTimeFormat().resolvedOptions().timeZone || undefined
}

/**
 * Call a command from the generated surface, typed against it.
 *
 * `gen-api.mjs` exists so that renaming an argument in Rust is a type error
 * here rather than a call that silently sends `undefined`, but every method
 * below used to write its own `invoke<T>('snake_name', {...})` by hand --
 * which meant the generated `Commands` interface had no reader at all, and a
 * rename that changed a field name or a return type compiled anyway, on both
 * sides, until somebody noticed the field come back as `undefined` at run
 * time. `K` is the camelCase key `Commands` and `COMMAND_NAMES` share, so a
 * method reads as `call('saveEntry', ...)` and the wire spelling is resolved
 * for it rather than repeated by hand at every call site.
 *
 * `requestId` is passed straight through to `invoke`, which is the same
 * "nobody attaches one unless they have reasoned about it" contract
 * `newRequestId` documents -- this does not consult `WRITE_COMMANDS` to
 * decide whether to mint or forward one, because today only `saveEntry` and
 * `saveNote` ever pass one at all, and every other write below is content to
 * retry as a fresh call if it is retried.
 */
function call<K extends keyof Commands>(
  name: K,
  args: Commands[K]['args'],
  requestId?: string,
): Promise<Commands[K]['result']> {
  return invoke<Commands[K]['result']>(COMMAND_NAMES[name], args, requestId)
}

/**
 * Call a command that is not yet in the generated surface, by its wire name.
 *
 * Two callers today, for two different reasons. `mail-api.ts` is the
 * "not yet" case: Phase 2-4 of `docs/plans/mail.md` name a couple of
 * commands other agents are still landing, and `gen-api.mjs` cannot
 * generate a typed method for one that does not exist in
 * `crates/karmayoga-service/surface.json` yet -- once it does, that call
 * site becomes a one-line typed method here instead, the same way every
 * meeting-notes command did when the pipeline landed. `meetings.svelte.ts`'s
 * "Dev-only mock triggers" are the other case, and never graduate: no
 * `mock_trigger_meeting_offer` or `mock_trigger_still_on` command exists in
 * a real build, or ever will, so there is nothing for `gen-api.mjs` to
 * generate a typed method from.
 *
 * Mirrors what `call` does for a *known* command: under Tauri it always goes
 * through the shell's `call`, never by name directly, because a command this
 * build does not know about is never one of the handful the shell answers
 * itself.
 */
export function callCommand<T = unknown>(
  name: string,
  args: Record<string, unknown> = {},
): Promise<T> {
  return MOCK ? invoke<T>(name, args) : invoke<T>('call', { name, args, requestId: null })
}

/** The scripted listening and dictating sessions, set in the mock build below
 *  and `null` everywhere else. */
let mockListen: MockListenControls | null = null
let mockDictation: MockDictationControls | null = null

export const api = {
  bootstrap: () => invoke<Bootstrap>('bootstrap'),

  /**
   * `settings` is whatever the chosen backend asked for in its spec — a
   * connection URL, a schema name — and is omitted for a backend that needs
   * only a folder. It is sealed under the vault password on the way in, so
   * a database credential does not end up readable in the vault header.
   */
  createVault: (opts: {
    path: string
    name: string
    backend: string
    settings?: Record<string, string>
    password: string | null
  }) => invoke<VaultStatus>('create_vault', opts),

  openVault: (path: string) => invoke<VaultStatus>('open_vault', { path }),

  // ── Another computer's vault ───────────────────────────────────────
  //
  // A remote client is this same application, with its own window, its own
  // tray and its own keyboard; only the machine the commands run on differs.
  // Everything below `api` is unchanged when one of these is in effect --
  // which is the whole claim, and why there is nothing here but connecting.

  /** Every server this copy has paired with. */
  remotes: () => invoke<Connection[]>('list_remotes'),

  /**
   * Pair from a link somebody copied, and connect.
   *
   * One call rather than pair-then-connect: a pairing that succeeded and a
   * connection that then failed would leave a row in the picker whose token
   * there is no way to know is good.
   */
  connectRemote: (link: string) => invoke<Connected>('connect_remote', { link }),

  /** Reconnect to one already paired with. */
  reconnectRemote: (id: string) => invoke<Connected>('reconnect_remote', { id }),

  /** Look at a vault in this process again. The pairing survives. */
  disconnectRemote: () => invoke<void>('disconnect_remote'),

  /** Forget a pairing: the connection and its token. */
  forgetRemote: (id: string) => invoke<void>('forget_remote', { id }),

  // ── Sharing this vault ─────────────────────────────────────────────
  //
  // The other half: this window's vault, served to other machines. Every one
  // of these answers with the whole pane's state, because every one of them
  // changes more than the thing it was asked about -- starting the server
  // fixes the address, pairing adds a device, revoking removes one.

  shareStatus: () => invoke<ShareStatus>('share_status'),
  shareStart: (opts: { address?: string | null; port?: number | null }) =>
    invoke<ShareStatus>('share_start', opts),
  /**
   * Whether a connected computer may unlock this vault.
   *
   * Its own call rather than an argument to `shareStart`, because it is not a
   * reason to restart the server -- and restarting rebinds the port, which can
   * fail and leave sharing off because somebody moved a switch.
   */
  setRemoteUnlock: (allow: boolean) => invoke<ShareStatus>('set_remote_unlock', { allow }),
  shareStop: () => invoke<ShareStatus>('share_stop'),
  /** Offer to pair, for the next five minutes. */
  newPairingCode: () => invoke<ShareStatus>('new_pairing_code'),
  cancelPairing: () => invoke<ShareStatus>('cancel_pairing'),
  /** Take a computer's access away. It has to pair again to get it back. */
  revokeDevice: (id: string) => invoke<ShareStatus>('revoke_device', { id }),

  // ── Letting an MCP client use this vault ────────────────────────────
  //
  // Claude Code, Claude Desktop or anything else that speaks the Model
  // Context Protocol, reading this vault's tools over `POST /mcp`. Off by
  // default and loopback by default; see `McpPanel.svelte`. Revoking an
  // issued token is `revokeDevice` above -- it is a row in the same list a
  // paired computer is.

  mcpStatus: () => invoke<McpStatus>('mcp_status'),
  mcpStart: (opts: { address?: string | null; port?: number | null }) =>
    invoke<McpStatus>('mcp_start', opts),
  mcpStop: () => invoke<McpStatus>('mcp_stop'),
  /** Whether a destructive tool -- one that deletes something -- is offered
   * at all. Off by default; see `docs/plans/mcp.md`'s "Destructive tools
   * are absent, not refused". */
  mcpSetDestructive: (allow: boolean) => invoke<McpStatus>('mcp_set_destructive', { allow }),
  /**
   * Mint a token scoped to `scopes` and hand it back once. There is no
   * second call that shows it again -- copy it now, or issue a new one.
   *
   * Never empty: `mcp_issue_token` refuses that outright rather than
   * quietly widening it to everything, so `McpPanel` disables the button
   * before it can ask.
   */
  mcpIssueToken: (scopes: string[]) => invoke<string>('mcp_issue_token', { scopes }),

  // ── Opening this vault in a browser ────────────────────────────────
  //
  // The desktop half of web access: the switch in Settings -> Vault. The
  // browser half is `web.ts`. See `docs/plans/web-access.md`.

  webStatus: () => invoke<WebStatus>('web_status'),
  webStart: (opts: { port?: number | null }) => invoke<WebStatus>('web_start', opts),
  webStop: () => invoke<WebStatus>('web_stop'),

  // ── The OS-wide hotkey ─────────────────────────────────────────────

  /**
   * Raise a desktop notification. By name, not through `call`: it is the
   * shell's to post, and only `notify.svelte.ts` should be deciding to. See
   * `crates/karmayoga-app/src/notice.rs` for why the plugin does not do it.
   */
  postNotification: (title: string, body: string | null, key: string | null) =>
    invoke<void>('post_notification', { title, body, key }),

  /**
   * The picture on the system clipboard, as the bytes of the file it was
   * offered as -- PNG, JPEG, WebP or GIF, told apart by `sniffPicture` in
   * `clipboard-picture.ts` -- or `null` when there is none. That is the
   * answer on every desktop but Linux's, in a browser, and in mock mode,
   * because everywhere else a pasted picture reaches the page as a file. By name, not through `call`: WebKitGTK gives
   * a paste of a screenshot no files at all, and this is the shell reading
   * the clipboard on the page's behalf. Asked only after a paste that looked
   * like a picture (`clipboard-picture.ts`). See
   * `crates/karmayoga-app/src/clipboard.rs`.
   *
   * The shell answers raw bytes, which arrive as an `ArrayBuffer`; the array
   * of numbers is the slow path `put_blob` also keeps for a webview that
   * blocks Tauri's custom protocol. Rejects, as every command does, when
   * there was a picture and it could not be had -- too large, or its owner
   * would not hand it over.
   */
  clipboardImage: async (): Promise<Uint8Array | null> => {
    const raw = await invoke<ArrayBuffer | Uint8Array | number[] | null>('clipboard_image')
    if (!raw) return null
    const bytes =
      raw instanceof Uint8Array
        ? raw
        : Array.isArray(raw)
          ? Uint8Array.from(raw)
          : new Uint8Array(raw)
    return bytes.length > 0 ? bytes : null
  },

  /**
   * Whether the desktop granted the key that raises the palette.
   *
   * False is an ordinary answer, not an error: Wayland has no protocol for an
   * application to claim a global key without the desktop's portal, and not
   * every compositor implements one. The settings pane says so and offers the
   * tray instead.
   */
  hotkeyStatus: () => invoke<HotkeyStatus>('hotkey_status'),
  setHotkey: (on: boolean) => invoke<HotkeyStatus>('set_hotkey', { on }),

  /**
   * Writing help: a second OS-wide key, over text selected in any
   * application. Per machine, like the palette's key, and kept by the shell
   * rather than the vault -- see `crates/karmayoga-app/src/writing/mod.rs`.
   * Unanswered in mock mode, where the pane leaves the row out.
   */
  writingSettings: () => invoke<WritingSettings>('writing_settings'),
  setWritingSettings: (enabled: boolean, shortcut: string | null = null) =>
    invoke<WritingSettings>('set_writing_settings', { enabled, shortcut }),

  /**
   * Voice typing: a third OS-wide key, over the microphone instead of a
   * selection. Per machine, kept by the shell -- see
   * `crates/karmayoga-app/src/voicetyping/mod.rs`. Unanswered in mock mode,
   * where the pane leaves the section out. `null` leaves a field as it is.
   */
  voiceTypingSettings: () => invoke<VoiceTypingSettings>('voice_typing_settings'),
  setVoiceTypingSettings: (
    enabled: boolean,
    shortcut: string | null = null,
    mode: VoiceTypingKeyMode | null = null,
    clean: boolean | null = null,
  ) => invoke<VoiceTypingSettings>('set_voice_typing_settings', { enabled, shortcut, mode, clean }),

  // ── Meeting capture, in the Tauri shell ────────────────────────────
  //
  // Native, not a service command: the webview has no microphone
  // permission, and `capabilities/default.json` does not change that. These
  // three are called by name, the same way `hotkeyStatus` above is, rather
  // than through `call` -- see `docs/plans/meeting-notes.md`'s "Recording is
  // native, in the Tauri shell". In mock mode `mock.ts` answers all three,
  // so the pill still has something to poll in a browser.

  meetingStart: (opts: {
    eventId?: string | null
    title?: string | null
    templateId?: string | null
  }) => invoke<Recording>('meeting_start', opts),
  meetingStop: (discard: boolean) => invoke<void>('meeting_stop', { discard }),
  meetingStatus: () => invoke<CaptureStatus | null>('meeting_status'),

  /**
   * Answer a pending offer, in the shell, which owns them -- it also raises
   * the notification and tray item for each, and settles all three together.
   * Resolves with the recording a `take` or `join` started, or null for
   * `notNow` and `never`.
   */
  meetingOfferAnswer: (key: string, answer: MeetingOfferAnswer) =>
    invoke<Recording | null>('meeting_offer_answer', { key, answer }),
  /** The offers pending in the shell, oldest first. */
  meetingOffers: () => invoke<MeetingOfferPayload[]>('meeting_offers', {}),

  /**
   * Record a 20-second sample and turn it into a voiceprint, natively.
   *
   * Rejects if the capture engineer's build of this command has not landed
   * yet -- `meetings.svelte.ts` turns that into "Voice enrolment isn't
   * available in this build" rather than a raw Tauri error, per
   * `docs/plans/meeting-notes.md`'s instruction to handle it gracefully.
   */
  voiceEnrol: (seconds = 20) => invoke<VoiceprintInfo>('voice_enrol', { seconds }),

  // Talking to the assistant, in the shell: `voiceStart` above begins a
  // session, and these three reach it. By name, like the capture commands --
  // they are the microphone's, not the vault's.
  voiceStop: () => invoke<void>('voice_stop'),
  voiceSetMuted: (muted: boolean) => invoke<void>('voice_set_muted', { muted }),
  /** What is on screen now, for the next thing the person says. */
  voiceSetScreen: (onScreen: OnScreen | null) => invoke<void>('voice_set_screen', { onScreen }),

  // Listening and dictating, in the shell: `listenStart` and `dictationStart`
  // above begin a session, and these reach it. By name, like the voice ones.
  // In the mock they go to the scripted sessions instead (`mockListen`), as
  // the mock `invoke` knows no such commands.
  listenPause: () => (mockListen ? mockListen.pause() : invoke<void>('listen_pause')),
  listenResume: () => (mockListen ? mockListen.resume() : invoke<void>('listen_resume')),
  /** Skip `by` sentences: -1 back one, 1 forward one. */
  listenSkip: (by: number) =>
    mockListen ? mockListen.skip(by) : invoke<void>('listen_skip', { by }),
  listenSpeed: (speed: number) =>
    mockListen ? mockListen.speed(speed) : invoke<void>('listen_speed', { speed }),
  listenStop: () => (mockListen ? mockListen.stop() : invoke<void>('listen_stop')),
  /** Stop listening and finish with the words heard. */
  dictationStop: () => (mockDictation ? mockDictation.stop() : invoke<void>('dictation_stop')),
  /** Stop listening and throw the words away. */
  dictationCancel: () =>
    mockDictation ? mockDictation.cancel() : invoke<void>('dictation_cancel'),

  // ── Meeting notes -- the service commands ──────────────────────────
  //
  // Everything about meeting notes that is an ordinary vault write, unlike
  // the three above: settings, the recording history, transcripts, local
  // speech models and voiceprints. This used to be `meetings-api.ts`'s own
  // hand-rolled `callCommand` wrappers, kept apart from `surface.json`
  // while the pipeline was still landing; now that it has, these are
  // one-line forwards to `call` like everything else in this object.

  meetingSettings: () => call('meetingSettings', {}),
  saveMeetingSettings: (settings: MeetingSettings) => call('saveMeetingSettings', { settings }),
  // `setTranscriberKey` is gone from here, though the command stays for older
  // clients: the speech model's key is set on its card in Settings → Models,
  // through `setModelKey('speech', …)`.
  /** Renders with the real model -- may be slow, and may fail. */
  previewMeetingTemplate: (body: string) => call('previewMeetingTemplate', { body }),
  /** Probe the platform's speech model, as saved in Settings → Models. Rejects
   *  with the failure text. Still named for the pane it began in. */
  testTranscriber: () => call('testTranscriber', {}),

  listRecordings: (query: RecordingQuery = {}) => call('listRecordings', { query }),
  getRecording: (id: RecordingId) => call('getRecording', { id }),
  activeRecording: () => call('activeRecording', {}),
  deleteRecording: (id: RecordingId) => call('deleteRecording', { id }),
  retryRecording: (id: RecordingId) => call('retryRecording', { id }),
  discardRecording: (id: RecordingId) => call('discardRecording', { id }),

  getTranscript: (noteId: NoteId) => call('getTranscript', { noteId }),
  nameSpeaker: (opts: {
    noteId: NoteId
    speakerKey: number
    name: string
    email?: string | null
  }) => call('nameSpeaker', opts),
  /** Proposed markdown body. The caller shows a diff and applies it itself. */
  rewriteMeetingNote: (noteId: NoteId, templateId: TemplateId) =>
    call('rewriteMeetingNote', { noteId, templateId }),

  listVoiceprints: () => call('listVoiceprints', {}),
  deleteVoiceprint: (id: VoiceprintId) => call('deleteVoiceprint', { id }),
  deleteAllVoiceprints: () => call('deleteAllVoiceprints', {}),

  // The speech card's downloads, in Settings → Models: the recognisers and the
  // speech kit. Under the meetings scope they were born in.
  speechModels: () => call('speechModels', {}),
  downloadSpeechModel: (id: string) => call('downloadSpeechModel', { id }),
  cancelSpeechModelDownload: (id: string) => call('cancelSpeechModelDownload', { id }),
  deleteSpeechModel: (id: string) => call('deleteSpeechModel', { id }),
  benchmarkSpeechModel: (id: string) => call('benchmarkSpeechModel', { id }),

  // The voice card's downloads, and a sample of the saved voice.
  voiceModels: () => call('voiceModels', {}),
  downloadVoiceModel: (id: string) => call('downloadVoiceModel', { id }),
  cancelVoiceModelDownload: (id: string) => call('cancelVoiceModelDownload', { id }),
  deleteVoiceModel: (id: string) => call('deleteVoiceModel', { id }),
  /** A few seconds of the saved voice, as a base64 WAV. */
  voiceSample: () => call('voiceSample', {}),

  // The image card: downloads, and one picture from the saved model.
  imageModels: () => call('imageModels', {}),
  downloadImageModel: (id: string) => call('downloadImageModel', { id }),
  cancelImageModelDownload: (id: string) => call('cancelImageModelDownload', { id }),
  deleteImageModel: (id: string) => call('deleteImageModel', { id }),
  generateImage: (prompt: string, shape?: ImageShape) =>
    call('generateImage', { prompt, shape: shape ?? null }),
  // A drawing from an SVG or an HTML page; no model needed. See
  // `illustrate` in the service.
  renderIllustration: (
    markup: IllustrationMarkup,
    source: string,
    width?: number,
    height?: number,
  ) => call('renderIllustration', { markup, source, width: width ?? null, height: height ?? null }),

  dismissMeetingOffer: (calendarId: string, uid: string, series: string | null, never: boolean) =>
    call('dismissMeetingOffer', { calendarId, uid, series, never }),

  unlock: (password: string) => call('unlock', { password }),
  /**
   * Check a password without opening or closing anything.
   *
   * What a window asks when its own screen was locked and the vault behind it
   * never was. Costs the same Argon2 derivation an unlock costs and counts
   * against the same lockout, deliberately.
   */
  verifyPassword: (password: string) => call('verifyPassword', { password }),
  lock: () => call('lock', {}),
  status: () => call('status', {}),
  changePassword: (current: string, next: string) => call('changePassword', { current, next }),
  /** How long before a client hides what it is showing. */
  setAutoLock: (seconds: number) => call('setAutoLock', { seconds }),
  /** How long before the machine holding the vault drops its key. 0 is never. */
  setForgetKey: (seconds: number) => call('setForgetKey', { seconds }),
  /**
   * Keep this vault's key in this machine's keychain, or take it out again.
   *
   * The password is asked for on the way *on*, and only then: this is the one
   * switch whose whole effect is that the password stops being needed, so
   * pressing it should cost the password once from somebody who knows it,
   * rather than being available to anybody who wandered past an unlocked
   * screen. Answers with what the setting now is.
   */
  setOpensItself: (on: boolean, password: string | null) =>
    invoke<boolean>('set_opens_itself', { on, password }),
  /** Defers the moment the key is dropped; called on real user interaction. */
  touch: () => call('touch', {}),
  /** Returns true if the vault has just dropped its key. Polled on a timer. */
  pollAutoLock: () => call('pollAutoLock', {}),

  journals: () => call('listJournals', {}),
  newJournal: (name: string) => call('newJournal', { name }),
  saveJournal: (journal: Journal) => call('saveJournal', { journal }),
  deleteJournal: (id: JournalId) => call('deleteJournal', { id }),

  entries: (query: EntryQuery) => call('listEntries', { query }),
  entry: (id: EntryId) => call('getEntry', { id }),
  newEntry: (journalId: JournalId) => call('newEntry', { journalId }),
  /**
   * Save an entry, refusing to overwrite a change made since it was loaded.
   *
   * `expect` is the `updatedAt` this window last read for the entry, or
   * `null` for one it has just created. A mismatch rejects with code
   * `conflict` and writes nothing.
   *
   * `requestId` identifies one logical write. Pass the *same* one when
   * retrying, so a save that landed and whose answer was lost is answered from
   * the record rather than refused as a conflict against itself. See
   * `newRequestId`.
   */
  saveEntry: (entry: Entry, expect: string | null, requestId?: string) =>
    call('saveEntry', { entry, expect }, requestId),

  /** Save regardless of what is stored. The "keep mine" on a conflict. */
  saveEntryForce: (entry: Entry) => call('saveEntryForce', { entry }),
  deleteEntry: (id: EntryId) => call('deleteEntry', { id }),

  /**
   * Search entries and notes.
   *
   * `kind` narrows to one of them; naming a journal narrows to entries
   * whatever `kind` says, because a note is in no journal. Both absent means
   * both kinds, which is what the palette wants.
   */
  search: (
    query: string,
    journalId: JournalId | null,
    limit: number,
    kind: SearchKind | null = null,
  ) => call('search', { query, journalId, kind, limit }),

  /**
   * Who the vault belongs to.
   *
   * Read into every prompt the assistant sends. Written from Settings, and by
   * the assistant's `update_profile` only when the person says so in chat --
   * a fact that changes is otherwise a memory, and this is for the ones that
   * do not.
   */
  profile: () => call('profile', {}),
  saveProfile: (profile: Profile) => call('saveProfile', { profile }),

  /** The assistant's standing work, with when each next runs. */
  routines: () => call('listRoutines', {}),
  /** A blank routine with an id. The core allocates it; see `newEntry`. */
  newRoutine: () => call('newRoutine', {}),
  saveRoutine: (routine: Routine) => call('saveRoutine', { routine }),
  deleteRoutine: (id: RoutineId) => call('deleteRoutine', { id }),
  /**
   * Ask for a routine to run now.
   *
   * Queued rather than run: the scheduler carries it out on its next tick, so
   * that runs stay serial however many times the button is pressed. Pressing it
   * twice returns the same queued run rather than paying for two model calls.
   */
  runRoutine: (id: RoutineId) => call('runRoutine', { id }),
  runs: (query: RunQuery = {}) => call('listRuns', { query }),
  run: (id: RoutineRunId) => call('getRun', { id }),
  deleteRun: (id: RoutineRunId) => call('deleteRun', { id }),
  /** Mark runs as looked at. An empty list means all of them. */
  markRunsSeen: (ids: RoutineRunId[] = []) => call('markRunsSeen', { ids }),
  /** How many runs nobody has looked at. The number on the app bar. */
  unseenRuns: () => call('unseenRuns', {}),
  routineTemplates: () => call('routineTemplates', {}),

  /**
   * Work the assistant took on in the background. See `jobs.svelte.ts`.
   *
   * Newest first, like the run log beside it.
   */
  jobs: (query: JobQuery = {}) => call('listJobs', { query }),
  job: (id: JobId) => call('getJob', { id }),
  /**
   * Start research by hand, rather than the assistant starting it.
   *
   * Queued rather than run: the worker picks it up. `conversationId` is where
   * the answer comes back to; leave it off and the job just makes its note.
   */
  startResearch: (question: string, brief?: string, conversationId?: ConversationId) =>
    call('startResearch', { question, brief, conversationId }),
  /** Stop a queued or running job. Whatever it wrote is kept. */
  cancelJob: (id: JobId) => call('cancelJob', { id }),
  /** Delete a finished job and its transcript. The note it made stays. */
  deleteJob: (id: JobId) => call('deleteJob', { id }),
  /** Mark jobs as looked at. An empty list means all of them. */
  markJobsSeen: (ids: JobId[] = []) => call('markJobsSeen', { ids }),
  /** How many jobs nobody has looked at. Added to the number on the app bar. */
  unseenJobs: () => call('unseenJobs', {}),
  /** Have the follow-up in the conversation that asked run again. */
  answerJob: (id: JobId) => call('answerJob', { id }),

  /** Work the assistant prepared. See `proposals.svelte.ts`. */
  proposals: (query: ProposalQuery = {}) => call('listProposals', { query }),
  proposal: (id: ProposalId) => call('getProposal', { id }),
  /**
   * Say yes. `edited` is the record as changed before accepting; `confirm`
   * is for a memory accepted as true from the memory list.
   */
  acceptProposal: (id: ProposalId, edited: ProposedRecord | null = null, confirm = false) =>
    call('acceptProposal', { id, edited, confirm }),
  declineProposal: (id: ProposalId, reason: DeclineReason | null = null) =>
    call('declineProposal', { id, reason }),
  /** Mark proposals as looked at. An empty list means every pending one. */
  markProposalsSeen: (ids: ProposalId[] = []) => call('markProposalsSeen', { ids }),
  /** Pending proposals nobody has looked at. Half the number on the app bar. */
  unseenProposals: () => call('unseenProposals', {}),
  /** Confirm an inferred memory, or strike it out as rejected. */
  setMemoryOrigin: (id: MemoryId, origin: MemoryOrigin) => call('setMemoryOrigin', { id, origin }),

  notes: (query: NoteQuery = {}) => call('listNotes', { query }),
  note: (id: NoteId) => call('getNote', { id }),
  /** A note to start: blank, or from a template filled in for today. */
  newNote: (templateId: TemplateId | null = null) => call('newNote', { templateId }),
  /**
   * Save a note, refusing to overwrite an edit made since `expect` was read.
   *
   * The same contract `saveEntry` has, for the same reason: a note is typed
   * into and autosaved, so two windows on one vault find each other -- and
   * `requestId` is the caller's for the same reason again. This minted its own
   * with `newRequestId()` here, which is the one thing that function's own
   * doc comment says not to do: a fresh id per attempt is an id the retry
   * cannot be recognised by.
   */
  saveNote: (note: Note, expect: string | null, requestId?: string) =>
    call('saveNote', { note, expect }, requestId),
  saveNoteForce: (note: Note) => call('saveNoteForce', { note }),
  deleteNote: (id: NoteId) => call('deleteNote', { id }),
  noteTags: () => call('noteTags', {}),

  // ── Templates ──
  //
  // One list for the journal, notes and meeting notes. Applying one is not
  // here: `newEntry` and `newNote` fill a template in themselves.

  templates: () => call('listTemplates', {}),
  /** A template with an id and starting text, not saved. */
  newTemplate: (uses: TemplateUse[] = []) => call('newTemplate', { uses }),
  saveTemplate: (template: Template) => call('saveTemplate', { template }),
  deleteTemplate: (id: TemplateId) => call('deleteTemplate', { id }),
  /** What is wrong with it, in words. Never a reason a save is refused. */
  lintTemplate: (template: Template) => call('lintTemplate', { template }),
  /** Filled in for `date` (today when null), as "Insert template" puts it. */
  renderTemplate: (
    template: Template,
    date: string | null = null,
    journalId: JournalId | null = null,
  ) => call('renderTemplate', { template, date, journalId }),

  /**
   * Import a file the user dropped or picked; returns its content address.
   *
   * The buffer is the whole payload rather than a field inside one, which
   * looks like a slip and is not: Tauri sends a top-level `ArrayBuffer` as
   * raw bytes and anything else as JSON. Wrapped in an object, a 100 MB
   * video became a hundred million JSON numbers -- hundreds of megabytes of
   * text to build, post and parse -- and froze the window while it did.
   * `put_blob` reads the request body to match.
   */
  putBlob: (bytes: Uint8Array) => invoke<string>('put_blob', bytes),

  /** Tells the shell that pending writes have landed and it may close. */
  readyToClose: () => invoke<void>('ready_to_close'),

  /**
   * Put these items in the tray menu, raising the icon if it is not up.
   *
   * False means this desktop has nowhere to put one -- a Linux session with
   * no StatusNotifier host -- rather than that something went wrong.
   */
  setTrayMenu: (items: TrayMenuItem[]) => invoke<boolean>('set_tray_menu', { items }),
  /** Take the tray icon down. */
  hideTray: () => invoke<void>('hide_tray'),
  /**
   * Turn the tray's optional "show the next meeting" line on or off.
   *
   * A machine preference rather than a vault one -- see
   * `lib/tray.svelte.ts`'s `meetingPref` -- so this is called with the
   * *effective* value (the preference, collapsed with the tray switch
   * itself being on) rather than mirroring a vault write.
   */
  setTrayMeeting: (on: boolean) => invoke<void>('set_tray_meeting', { on }),

  /**
   * Open an app in a window of its own, or bring forward the one already
   * showing it. See `lib/appwindow.ts`.
   */
  openAppWindow: (section: Section) => invoke<void>('open_app_window', { section }),
  /**
   * From a window of one app: bring the main window forward showing another
   * app, or Settings at a tab.
   */
  showInMain: (asked: { section?: Section; settings?: string }) =>
    invoke<void>('show_in_main', {
      section: asked.section ?? null,
      settings: asked.settings ?? null,
    }),
  /**
   * Choose what the number on the application's icon counts; nothing chosen
   * takes it off. The shell does the counting, and keeps it current with the
   * window hidden -- see `crates/karmayoga-app/src/badge.rs`.
   */
  setBadge: (sources: BadgeSources) => invoke<void>('set_badge', { sources }),

  /** All tags in use, most frequent first. */
  tags: () => call('listTags', {}),

  // ── The task domain ────────────────────────────────────────────────
  //
  // Available only when `status.capabilities.tasks` is true; a
  // vault stores journals and nothing else, and the interface hides the
  // todo app rather than letting these fail at click time.

  projects: () => call('listProjects', {}),
  newProject: (name: string) => call('newProject', { name }),
  saveProject: (project: Project) => call('saveProject', { project }),
  /** Deletes the project, its tasks and their time blocks. */
  deleteProject: (id: ProjectId) => call('deleteProject', { id }),

  tasks: (query: TaskQuery) => call('listTasks', { query }),
  task: (id: TaskId) => call('getTask', { id }),
  /** Mints an unsaved task; fill it in and pass it to `saveTask`. */
  newTask: (opts: {
    projectId: ProjectId | null
    parentId: TaskId | null
    status: TaskStatus | null
  }) => call('newTask', opts),
  saveTask: (task: Task) => call('saveTask', { task }),
  /** One write for many tasks: what a board reorder is. */
  saveTasks: (tasks: Task[]) => call('saveTasks', { tasks }),
  /** Deletes the task, its subtasks and their time blocks. */
  deleteTask: (id: TaskId) => call('deleteTask', { id }),

  blocks: (query: BlockQuery) => call('listBlocks', { query }),
  /** Mints an unsaved block, with the machine's own time zone resolved. */
  newBlock: (opts: {
    subject: BlockSubject
    start: string
    minutes: number
    kind: BlockKind | null
  }) => call('newBlock', opts),
  saveBlock: (block: TimeBlock) => call('saveBlock', { block }),
  deleteBlock: (id: BlockId) => call('deleteBlock', { id }),
  /**
   * Save `block` as the first of a repeating series -- or, with `null`, as
   * one that stops repeating here -- and write the rest of it out, two
   * years ahead. Every *later* block of whatever series it was already in
   * is replaced, which is also how "apply this change to the following
   * ones" is said: the same call, with the rule it already has. Answers
   * every block written, `block` first.
   */
  saveBlockSeries: (block: TimeBlock, recurrence: Recurrence | null) =>
    call('saveBlockSeries', { block, recurrence }),
  /** Delete `id` and the blocks of its series after it, or the whole series. */
  deleteBlockSeries: (id: BlockId, scope: SeriesScope) => call('deleteBlockSeries', { id, scope }),

  /** Every tag in the task domain with its usage count, most used first. */
  taskTags: () => call('taskTags', {}),
  taskStats: () => call('taskStats', {}),

  // ── The calendar domain ────────────────────────────────────────────
  //
  // Subscribed calendars and their events, and the two ways an appointment
  // of your own is written. On this computer it is a time block, through
  // the task commands above. On an account's calendar -- Google, Microsoft,
  // a CalDAV server -- it is written to that server by `createEvent` and its
  // siblings below, and comes back the way every other event does: through
  // the calendar's own sync. Nothing here edits a stored event in place;
  // the server's copy is the event, and this vault's is a reading of it.

  calendars: () => call('listCalendars', {}),
  saveCalendar: (calendar: Calendar) => call('saveCalendar', { calendar }),
  /** Unsubscribe: the calendar and every event that came from it. */
  deleteCalendar: (id: CalendarId) => call('deleteCalendar', { id }),

  /**
   * Subscribe to a feed and fetch it once.
   *
   * One call rather than save-then-fetch, because the two are not
   * independent: if the address turns out not to be a calendar, the honest
   * outcome is that nothing was added.
   */
  subscribeCalendar: (opts: { name: string; url: string; color: string }) =>
    call('subscribeCalendar', opts),

  /** Add a calendar from a `.ics` file the browser read for us. */
  importCalendar: (opts: { name: string; label: string; color: string; ics: string }) =>
    call('importCalendar', opts),

  /** Refetch one feed and replace its events with what comes back. */
  syncCalendar: (id: CalendarId) => call('syncCalendar', { id }),
  /** Refetch every feed whose interval has elapsed. `force` ignores it. */
  syncDueCalendars: (force: boolean) => call('syncDueCalendars', { force }),

  events: (query: EventQuery) => call('listEvents', { query }),
  event: (id: EventId) => call('getEvent', { id }),

  /**
   * Where new events go when nobody names a calendar: one of an account's
   * writable calendars, or `null` for this computer's own time blocks.
   * `saveCalendar` keeps whatever is stored for this, so it is moved only
   * here -- and the list has to be read again afterwards to see it moved.
   */
  setDefaultCalendar: (id: CalendarId | null) => call('setDefaultCalendar', { id }),
  setThisComputer: (on: boolean) => call('setThisComputer', { on }),
  /**
   * Put a new event on an account's calendar. A network call, and one that
   * sends invitations when the draft has guests. Answers the event as the
   * calendar's sync brought it back, or `null` when the server took it and
   * the sync has not caught up yet -- a refresh a moment later finds it.
   */
  createEvent: (calendarId: CalendarId, draft: EventDraft) =>
    call('createEvent', { calendarId, draft }),
  /**
   * An account calendar's event read fresh from its server, ready to be
   * changed: the full description, guests by address and answer, and the
   * series' rule. What the stored copy holds is a reading for the grid and
   * is not enough to write back from.
   */
  loadEvent: (id: EventId) => call('loadEvent', { id }),
  /** Change one occurrence, or the whole series. The calendar syncs afterwards. */
  updateEvent: (id: EventId, draft: EventDraft, scope: EventScope) =>
    call('updateEvent', { id, draft, scope }),
  /** Delete one occurrence, or the whole series. Guests are sent a cancellation. */
  deleteEvent: (id: EventId, scope: EventScope) => call('deleteEvent', { id, scope }),

  /** The providers the add sheet offers, with where to find each address. */
  calendarProviders: () => call('calendarProviders', {}),

  /**
   * The calendars `account` offers, marking which ones this vault already
   * subscribes to. What "From an account" in the add-calendar sheet, and
   * the calendar list in Settings → Accounts, both read.
   */
  listAccountCalendars: (account: AccountId) => call('listAccountCalendars', { account }),
  /** Subscribe to one of an account's own calendars and fetch it once. */
  subscribeAccountCalendar: (account: AccountId, remoteId: string) =>
    call('subscribeAccountCalendar', { account, remoteId }),

  // ── The library domain ─────────────────────────────────────────────
  //
  // Shelves, the things on them, and the log of what you did with them.
  // Available only when `status.capabilities.library` is true.

  /**
   * Every shelf, with its counts.
   *
   * Also what seeds the built-in shelves into an empty library, which is why
   * the library store calls this before anything else — see `list_kinds` in
   * the Rust shell for why the seeding hangs off a read.
   */
  kinds: () => call('listKinds', {}),
  /** Mints an unsaved shelf, with a slug derived from the name. */
  newKind: (name: string, singular: string) => call('newKind', { name, singular }),
  saveKind: (kind: Kind) => call('saveKind', { kind }),
  /** Deletes the shelf, everything on it, and those items' log rows. */
  deleteKind: (id: KindId) => call('deleteKind', { id }),

  items: (query: ItemQuery) => call('listItems', { query }),
  item: (id: ItemId) => call('getItem', { id }),

  /**
   * Add something to a shelf, and optionally go and find out what it is.
   *
   * One call rather than create-then-enrich: what the interface wants back is
   * the finished card. The lookup is best-effort — a network that is off
   * never stops something being added — and `lookedUp` says whether anything
   * was found, so the interface can offer to search again rather than
   * silently implying it tried.
   */
  addItem: (kindId: KindId, title: string, lookup: boolean) =>
    call('addItem', { kindId, title, lookup }),
  saveItem: (item: Item) => call('saveItem', { item }),
  /** One write for many items: what a re-ordered shelf is. */
  saveItems: (items: Item[]) => call('saveItems', { items }),
  /** Deletes the item and its whole log. */
  deleteItem: (id: ItemId) => call('deleteItem', { id }),

  /**
   * Move an item to a status, dating it and logging it in one act.
   *
   * The dates and the log row are coupled in the backend on purpose: marking
   * a book read is the moment "finished on" is known *and* the moment the
   * log gains the row that makes "what did I read this year" answerable.
   */
  setItemStatus: (id: ItemId, status: ItemStatus, log: boolean) =>
    call('setItemStatus', { id, status, log }),
  /** Record where you have got to. Starts the item if it was only wished for. */
  setItemProgress: (id: ItemId, position: number, total: number | null, log: boolean) =>
    call('setItemProgress', { id, position, total, log }),

  logs: (query: LogQuery) => call('listLogs', { query }),
  /** Mints an unsaved log row dated today on the machine's own calendar. */
  newLog: (itemId: ItemId, event: LogEvent) => call('newLog', { itemId, event }),
  saveLog: (log: LogEntry) => call('saveLog', { log }),
  deleteLog: (id: LogId) => call('deleteLog', { id }),

  libraryStats: () => call('libraryStats', {}),
  keepInTouch: () => call('keepInTouchStatus', {}),
  saveKeepInTouch: (settings: KeepInTouchSettings) => call('saveKeepInTouchSettings', { settings }),
  peopleDue: () => call('listPeopleDue', {}),
  runKeepInTouch: () => call('runKeepInTouch', {}),

  // ── Web search ─────────────────────────────────────────────────────
  //
  // A facility rather than a feature of one app. `lib/websearch.ts` wraps
  // these with debouncing, cancellation and a cache; prefer that over
  // calling them directly.

  /** Search the web. The general entry point; anything may call it. */
  webSearch: (request: SearchRequest) => call('webSearch', { request }),
  /** The sources a search can be run against, for the picker. */
  searchSources: () => call('searchSources', {}),
  /** Look a title up using whatever source a shelf prefers. */
  lookupMetadata: (kindId: KindId, query: string, limit?: number) =>
    call('lookupMetadata', { kindId, query, limit: limit ?? null }),
  /** Apply a chosen result to an item, downloading its cover on the way. */
  applyMetadata: (id: ItemId, result: SearchResult, overwrite: boolean) =>
    call('applyMetadata', { id, result, overwrite }),
  /**
   * Download a picture into the vault as a cover -- no more than 800 pixels
   * on its longer side, as a JPEG -- and return its blob id.
   *
   * Nothing in the interface ever loads a remote image directly: the content
   * security policy allows images from `'self'` and `karmayoga:` and nowhere
   * else, so a cover has to come home before it can be drawn.
   */
  fetchImage: (url: string) => call('fetchImage', { url }),
  /**
   * Make a picture already in the vault into a cover -- shrunk and stored as
   * a JPEG, the way `fetchImage` stores one -- and return the cover's blob
   * id, which is the same id when it was one already. For a file chosen off
   * the disk, which arrives through `putBlob` like any attachment.
   */
  shrinkCover: (blob: string) => call('shrinkCover', { blob }),
  /**
   * Make a picture already in the vault into one for the assistant -- the
   * right way up, no larger than a model is shown, and stripped of where it
   * was taken -- and return it as the attachment `sendMessage` takes. For a
   * file chosen, pasted or dropped into the composer, which arrives through
   * `putBlob` like any attachment. Rejects, with a sentence to show, a file
   * that is not a picture the assistant can be sent.
   */
  preparePicture: (blob: string, filename: string) => call('preparePicture', { blob, filename }),
  /**
   * Look for pictures of something: Bing, then Wikimedia Commons when Bing
   * has nothing. Addresses only; nothing is fetched or kept.
   */
  imageSearch: (query: string, limit?: number) =>
    call('imageSearch', { query, limit: limit ?? null }),
  /**
   * One picture, small, as a `data:` address a grid can draw -- fetched on a
   * cover's terms and kept nowhere. The webview cannot load the address
   * itself, for `fetchImage`'s reason.
   */
  imagePreview: (url: string) => call('imagePreview', { url }),

  // ── Weather ────────────────────────────────────────────────────────
  //
  // The Overview's Weather widget reads this directly -- see
  // `overview.svelte.ts` -- rather than through a wrapper the way
  // `lib/websearch.ts` wraps the search calls above, because the service
  // already caches a result per place for a few minutes; a second cache on
  // this side would just be two clocks to keep straight. `place` left out,
  // or blank, asks for the profile's own location; a vault with neither
  // answers with a `VaultError` coded `no_location` rather than a forecast
  // -- `overview.svelte.ts`'s `fetchWeather` is what tells that apart from
  // every other failure.
  weather: (place?: string) => call('weather', { place: place?.trim() || null }),

  // ── Roles and goals ────────────────────────────────────────────────
  //
  // Available only when `status.capabilities.purpose` is true. The two records
  // are small; the interesting call is `balance`, which is the whole reason
  // the purpose pointer exists.

  roles: () => call('listRoles', {}),

  /** Mints an unsaved role; fill it in and pass it to `saveRole`. */
  newRole: (name: string) => call('newRole', { name }),
  saveRole: (role: Role) => call('saveRole', { role }),

  /**
   * Delete a role. Refused, with a message naming the count, while goals
   * still point at it — unlike a project, which takes its tasks with it.
   */
  deleteRole: (id: RoleId) => call('deleteRole', { id }),

  /**
   * Offer a starting set of roles, and answer 0 if there are any already.
   *
   * Never called on unlock, unlike the library's shelves: a list of what a
   * life is made of is a claim, and writing one unasked would be this
   * application telling somebody who they are.
   */
  seedRoles: () => call('seedRoles', {}),

  goals: (query: GoalQuery = {}) => call('listGoals', { query }),
  goal: (id: GoalId) => call('getGoal', { id }),
  newGoal: (roleId: RoleId, title: string) => call('newGoal', { roleId, title }),
  saveGoal: (goal: Goal) => call('saveGoal', { goal }),
  saveGoals: (goals: Goal[]) => call('saveGoals', { goals }),
  deleteGoal: (id: GoalId) => call('deleteGoal', { id }),

  /**
   * Minutes per purpose over a window, and the meetings somebody else
   * booked, in one call — the Overview draws them together, and two round
   * trips would let one arrive without the other.
   */
  balance: (from: string, to: string) => call('timeByPurpose', { from, to }),

  goalActivity: (id: GoalId) => call('goalActivity', { id }),

  /**
   * A window of mail and of meetings, counted, for the Overview. Each is a
   * decrypt of every record in the window, so a window is a quarter at most
   * from the interface. `tz` is ours, so a day here is the day on screen.
   */
  mailActivity: (from: string, to: string) => call('mailActivity', { from, to, tz: localZone() }),
  meetingActivity: (from: string, to: string) =>
    call('meetingActivity', { from, to, tz: localZone() }),

  // ── The tracking domain ────────────────────────────────────────────
  //
  // Available only when `status.capabilities.trackers` is true. Both halves
  // are records: a definition is its own row, and which journals draw its
  // chip is a list of ids on each journal.

  /** Mints an unsaved tracker; fill it in and pass it to `saveTracker`. */
  newTracker: (name: string, kind: TrackerKind) => call('newTracker', { name, kind }),

  /**
   * Every tracker in the vault.
   *
   * Also where a vault written before trackers became records has its old
   * definitions moved out of its journals — once, on the first call.
   */
  trackers: () => call('listTrackers', {}),
  saveTracker: (tracker: Tracker) => call('saveTracker', { tracker }),

  /**
   * Fold one tracker into another, keeping both histories, and answer how
   * many readings moved. The tidy-up for a name typed two ways.
   */
  mergeTrackers: (from: TrackerId, into: TrackerId) => call('mergeTrackers', { from, into }),

  readings: (query: ReadingQuery) => call('listReadings', { query }),

  /** One row per tracker per day: the aggregate a chart is built from. */
  trackerDays: (query: ReadingQuery) => call('trackerDays', { query }),

  /**
   * Karma: every day from `from` to `to` that came to anything, and the
   * total since Karma started. Worked out from the other records on each
   * call -- see `karmayoga_core::karma`.
   */
  karma: (from: string, to: string) => call('karma', { from, to }),

  /**
   * Record one value, and let the backend decide what "when" means.
   *
   * Pass `at` to state the time outright. Otherwise a reading on today's
   * date takes the current minute, and one on a past date takes no time at
   * all — writing up Tuesday on Thursday says nothing about 23:04, and a
   * defaulted timestamp would put a mark on the calendar at an hour nothing
   * happened.
   */
  logReading: (opts: {
    trackerId: TrackerId
    value: number
    date: string
    at?: string | null
    /** Where it was ticked. Both absent for a reading logged from anywhere else. */
    journalId?: JournalId | null
    entryId?: EntryId | null
  }) => call('logReading', opts),

  /** Update a reading that exists: a corrected dose, a note, a time. */
  saveReading: (reading: Reading) => call('saveReading', { reading }),
  deleteReading: (id: ReadingId) => call('deleteReading', { id }),

  /**
   * Delete a tracker *and* every reading it ever made, returning how many
   * went. Archiving — a flag on the definition — is the non-destructive
   * half of this pair, and the usual answer.
   */
  deleteTracker: (id: TrackerId) => call('deleteTracker', { id }),

  // ── The assistant ──────────────────────────────────────────────────
  //
  // Available only when `status.capabilities.agent` is true. Small, because
  // almost everything the assistant can do it does through its *tools*,
  // which live in the Rust core and never cross this boundary. What is here
  // is configuring it, reading its threads back, and the two halves of one
  // exchange: `sendMessage` (exported separately, because it streams) and
  // the confirmation that answers it.

  agentSettings: () => call('agentSettings', {}),

  /** Returns what was actually stored. */
  saveAgentSettings: (settings: AgentSettings) => call('saveAgentSettings', { settings }),

  // ── The models ────────────────────────────────────────────────────────
  //
  // Application settings, not the assistant's: which model each size of work
  // runs on, and where. Every call answers the whole view, so the pane is
  // told what was stored rather than what it asked for.

  modelSettings: () => call('modelSettings', {}),
  saveModelSettings: (settings: ModelSettings) => call('saveModelSettings', { settings }),

  /**
   * Store the key for the endpoint `tier`'s card points at -- every card
   * there shares it, the decision and speech models' included, so OpenAI
   * speech uses an OpenAI language model's key. There is deliberately no
   * call that reads one back.
   */
  setModelKey: (tier: ModelSlot, key: string) => call('setModelKey', { tier, key }),
  clearModelKey: (tier: ModelSlot) => call('clearModelKey', { tier }),

  /** The decision models that run on this computer, and whether each is here. */
  decisionModels: () => call('decisionModels', {}),
  downloadDecisionModel: (id: string) => call('downloadDecisionModel', { id }),
  cancelDecisionModelDownload: (id: string) => call('cancelDecisionModelDownload', { id }),
  deleteDecisionModel: (id: string) => call('deleteDecisionModel', { id }),
  /** Loads it if it is not in memory, then times a few made-up messages. */
  benchmarkDecisionModel: (id: string) => call('benchmarkDecisionModel', { id }),

  conversations: (limit?: number) => call('listConversations', { limit }),

  /** Mints an unsaved thread; the first message is what saves it. */
  newConversation: () => call('newConversation', {}),

  conversationMessages: (id: ConversationId) => call('conversationMessages', { id }),

  deleteConversation: (id: ConversationId) => call('deleteConversation', { id }),

  /**
   * Answer a confirmation the assistant is waiting on. False means nothing
   * was waiting any more -- a turn cancelled between the question and the
   * click -- which the panel treats as a dismissal rather than an error.
   *
   * `later` parks the call as a proposal instead of running or refusing it;
   * `approved` keeps its old meaning either way, for a card that never
   * offered "later" at all. See `docs/plans/dreaming.md`'s Phase 5.
   */
  confirmToolCall: (callId: string, approved: boolean, later = false) =>
    call('confirmToolCall', { callId, approved, later }),

  /**
   * Stop the turn running in a thread. What it had said and done so far is
   * kept; a question it was waiting on is withdrawn, as though declined.
   * False means nothing was running -- it finished on its own between the
   * press and the request -- which is not worth telling anybody about.
   */
  cancelTurn: (conversationId: ConversationId) => call('cancelTurn', { conversationId }),

  memories: () => call('listMemories', {}),

  /** A blank memory with an id, pinned. The core allocates it. */
  newMemory: () => call('newMemory', {}),

  /**
   * Saving by hand also pins: a fact somebody typed is not one the
   * assistant's own housekeeping may drop. Returns what it evicted.
   */
  saveMemory: (memory: Memory) => call('saveMemory', { memory }),
  deleteMemory: (id: MemoryId) => call('deleteMemory', { id }),

  skills: () => call('listSkills', {}),

  /** A blank skill with an id, switched on. The core allocates it. */
  newSkill: () => call('newSkill', {}),

  saveSkill: (skill: Skill) => call('saveSkill', { skill }),
  deleteSkill: (id: SkillId) => call('deleteSkill', { id }),

  /**
   * What one kind of agent has done with mail -- newest first, resolved
   * into an address and a subject rather than the bare ids `Op` itself
   * carries. What Settings → Sharing draws for each connected MCP client,
   * and the assistant's own settings draw for itself. `cursor` is the
   * previous page's own last `opId`; omit it for the first page.
   */
  mailActionsByOrigin: (kind: MailAgentOriginKind, limit?: number, cursor?: string | null) =>
    call('mailActionsByOrigin', { kind, limit: limit ?? null, cursor: cursor ?? null }),

  // ── The quick model ────────────────────────────────────────────────
  //
  // One call per flow that has a use for a small, fast model. Every one is a
  // read: nothing here writes a record. What comes back is a *proposal* that
  // some component draws beside a field, and it is the ordinary `updateTask`,
  // `saveItem` or `addReading` that applies it when somebody taps.
  //
  // Components should not call these directly -- go through `quick.svelte.ts`,
  // which owns the switch check, the cancellation and the rule that a failure
  // here is silence rather than an error.

  quickAsks: () => call('quickAsks', {}),
  setQuickAsk: (args: { name: string; on: boolean }) => call('setQuickAsk', args),

  /** The shelf's own declared fields, out of what a search found. */
  quickItemFields: (itemId: ItemId) => call('quickItemFields', { itemId }),

  /** Which result is the thing, or null when none of them is. */
  quickPickResult: (args: { kindId: KindId; query: string; results: SearchResult[] }) =>
    call('quickPickResult', args),

  /** A whole shelf, drafted from its name. */
  quickKindDraft: (name: string) => call('quickKindDraft', { name }),

  quickImportColumns: (args: { kindId: KindId; columns: string[]; sample?: string[] }) =>
    call('quickImportColumns', args),

  quickTaskLabels: (title: string) => call('quickTaskLabels', { title }),
  quickTaskFromLine: (line: string) => call('quickTaskFromLine', { line }),
  quickSubtasks: (taskId: TaskId) => call('quickSubtasks', { taskId }),
  quickEstimate: (taskId: TaskId) => call('quickEstimate', { taskId }),

  quickEventFromLine: (line: string) => call('quickEventFromLine', { line }),
  /** Display only: the feed's own record is never rewritten. */
  quickEventTitle: (title: string) => call('quickEventTitle', { title }),

  quickEntryReadings: (entryId: EntryId) => call('quickEntryReadings', { entryId }),
  quickEntryTitle: (entryId: EntryId) => call('quickEntryTitle', { entryId }),
  quickEntryLabels: (entryId: EntryId) => call('quickEntryLabels', { entryId }),

  quickNoteTitle: (noteId: NoteId) => call('quickNoteTitle', { noteId }),
  quickNoteTasks: (noteId: NoteId) => call('quickNoteTasks', { noteId }),
  quickNoteLabels: (noteId: NoteId) => call('quickNoteLabels', { noteId }),

  quickReadingFromLine: (line: string) => call('quickReadingFromLine', { line }),
  quickTrackerDraft: (args: { name: string; line?: string }) => call('quickTrackerDraft', args),

  quickGoalWording: (args: { title: string; roleId: RoleId }) => call('quickGoalWording', args),
  quickGoalBackfill: (goalId: GoalId) => call('quickGoalBackfill', { goalId }),

  quickWeekNote: (args: { thisWeek: string; lastWeek?: string }) => call('quickWeekNote', args),

  quickFrontMatter: (args: { ours: string[]; theirs: string[] }) => call('quickFrontMatter', args),

  // ── Taking your data out, and putting it back ────────────────────────
  //
  // Nine calls rather than two because an archive does not fit in a reply:
  // both directions are a handle and a series of chunks, exactly as an
  // attachment is. `lib/transfer.svelte.ts` owns both loops; nothing else in
  // the interface should be calling these by hand.

  /** What this vault can hand over, and how much of it there is. */
  exportableParts: () => call('listParts', {}),

  /** Build the archive. The bytes stay on the vault's machine until read. */
  startExport: (parts: string[], media: boolean) => call('startExport', { parts, media }),
  readExport: (handle: string, offset: number) => call('readExport', { handle, offset }),
  /** For a download that was abandoned; a finished one drops itself. */
  endExport: (handle: string) => call('endExport', { handle }),

  startImport: (name: string, bytes: number) => call('startImport', { name, bytes }),
  writeImport: (handle: string, offset: number, data: string) =>
    call('writeImport', { handle, offset, data }),
  /** What is in it, changing nothing. The dry run the dialog shows. */
  readImport: (handle: string) => call('readImport', { handle }),
  runImport: (handle: string, parts: string[], mode: ImportMode) =>
    call('runImport', { handle, parts, mode }),
  endImport: (handle: string) => call('endImport', { handle }),

  /**
   * Ask the shell to save an already-built export where the user picks.
   *
   * Desktop only, and the reason it exists is that the bytes then never come
   * through here at all: the shell pulls the chunks from the session straight
   * into the file. A browser attached to a paired vault assembles them itself
   * instead -- see `transfer.svelte.ts`. Answers false when the dialog was
   * dismissed, which is not an error.
   */
  saveExport: (handle: string, name: string) => invoke<boolean>('save_export', { handle, name }),

  /**
   * Ask the shell where to put one part of a message, and write it there. The
   * bytes never come through here. Answers false when the dialog was dismissed,
   * which is not an error.
   */
  saveMailPart: (messageId: string, identifier: string, name: string) =>
    invoke<boolean>('save_mail_part', { messageId, identifier, name }),

  /**
   * Ask the shell for a file to import, and hand it to the vault.
   *
   * Nothing has been read into the vault when this returns: what comes back
   * is the handle to inspect and then, separately, to import with. `null`
   * means the picker was dismissed.
   */
  openImport: () => invoke<PickedFile | null>('open_import'),

  // ── Backups ──────────────────────────────────────────────────────────
  //
  // Settings → Backup: sealed, date-stamped copies of the vault, to a folder
  // or a bucket, on a schedule. Every call but the last three answers the
  // whole `BackupView`, so the pane is told what was stored and what is
  // running rather than what it asked for. `lib/backup.svelte.ts` is the
  // one caller.

  backupSettings: () => call('backupSettings', {}),
  saveBackupSettings: (settings: BackupSettings) => call('saveBackupSettings', { settings }),

  /**
   * Store the access key an S3 destination with `credentials: 'stored'`
   * signs with, sealed in the vault. There is deliberately no call that
   * reads it back: `BackupView.hasKeys` says only that one is there.
   */
  setBackupKeys: (accessKeyId: string, secretAccessKey: string) =>
    call('setBackupKeys', { accessKeyId, secretAccessKey }),
  clearBackupKeys: () => call('clearBackupKeys', {}),

  /**
   * "Back up now": starts one in the background and answers at once with
   * `running` set. Rejects with `busy` when one is running already.
   */
  runBackup: () => call('runBackup', {}),
  /** What it had sent stays at the destination and is used by the next one. */
  cancelBackup: () => call('cancelBackup', {}),

  /** The copies kept at the saved destination, newest first. Asks the
   *  destination, so it can fail the way a backup would. */
  backupSnapshots: () => call('backupSnapshots', {}),

  /**
   * Try a destination before it is saved: write a small file there, read it
   * back, delete it, and say whether this vault's backups are there already.
   * A destination that does not work is `ok: false` and a message, not a
   * rejection.
   */
  checkBackupDestination: (destination: Destination) =>
    call('checkBackupDestination', { destination }),

  /**
   * Ask the shell for a folder: the platform's own picker, titled `title`.
   * Desktop only -- a browser on web access has no shell to ask. Answers
   * `null` when the dialog was dismissed, which is not an error. Only the
   * path comes back; nothing is read or written there.
   */
  pickFolder: (title: string) => invoke<string | null>('pick_folder', { title }),

  // ── Connections ──────────────────────────────────────────────────────
  //
  // Settings → Connections: MCP servers the assistant may use, each signed
  // in as one account. Every write answers the view of what it touched.
  // Secrets go in and never come back out: `ConnectionView.secrets` says
  // only which are set. `lib/connections.svelte.ts` is the one caller. See
  // `docs/plans/connections.md`.

  /** Every connection, and the presets to add one from. */
  connections: () => call('connections', {}),
  /** Add a connection, with any secrets it came with. Does not connect. */
  addConnection: (opts: {
    preset?: string
    name: string
    transport: ConnectionTransport
    auth: ConnectionAuth
    secrets?: ConnectionSecretsPatch
  }) => call('addConnection', opts),
  /** Save an edited connection: name, transport, sign-in, the two switches,
   *  the per-tool policies. */
  updateConnection: (connection: McpConnection) => call('updateConnection', { connection }),
  /** Forget a connection, what was learned about it, and its secrets. */
  removeConnection: (id: ConnectionId) => call('removeConnection', { id }),
  /** Change some secrets; a field left out is left alone, `''` removes. */
  setConnectionSecrets: (id: ConnectionId, patch: ConnectionSecretsPatch) =>
    call('setConnectionSecrets', { id, ...patch }),
  /** Connect and list the tools afresh. Answers the view either way; its
   *  `state.lastError` says what went wrong. */
  checkConnection: (id: ConnectionId) => call('checkConnection', { id }),
  /** Set one tool's policy, or with `null` go back to its own hints. */
  setConnectionToolPolicy: (id: ConnectionId, tool: string, policy: ToolPolicy | null) =>
    call('setConnectionToolPolicy', policy === null ? { id, tool } : { id, tool, policy }),
  /**
   * Start an OAuth sign-in. Open the answered `url` yourself with
   * `openExternal` (or show it to copy where there is no browser to open),
   * then wait on `awaitConnectionSignIn`.
   */
  beginConnectionSignIn: (id: ConnectionId) => call('beginConnectionSignIn', { id }),
  /** Resolves once the browser came back and the tools were listed; rejects
   *  with `cancelled`, `timed_out`, `invalid_client` or `provider`. */
  awaitConnectionSignIn: (signInId: string) => call('awaitConnectionSignIn', { signInId }),
  cancelConnectionSignIn: (signInId: string) => call('cancelConnectionSignIn', { signInId }),
  /** Forget the OAuth grant, keeping the connection. */
  signOutConnection: (id: ConnectionId) => call('signOutConnection', { id }),

  // ── Mail: mailboxes and threads ───────────────────────────────────────
  //
  // See `mail-api.ts` for the domain-shaped wrapper every Mail component
  // actually calls -- singular thread ids where this is the plural `threads`
  // the real commands take, and a couple of names (`respond_to_invite`,
  // `set_thread_category`, `summarize_thread`) that are not in
  // `surface.json` yet and so are not here either; see that file's own
  // TODOs for the two contracts they belong to.

  mailboxes: (account: AccountId) => call('listMailboxes', { account }),
  threads: (
    mailbox: MailboxId,
    filter?: ThreadFilter,
    cursor?: string | null,
    limit?: number | null,
  ) => call('listThreads', { mailbox, filter, cursor, limit }),
  /** `threads`, across several mailboxes at once -- the "All accounts"
   *  rows' one list -- newest activity first. A thread filed in two of them
   *  can come back on two different pages, so an appending caller dedupes. */
  threadsAcross: (
    mailboxes: MailboxId[],
    filter?: ThreadFilter,
    cursor?: string | null,
    limit?: number | null,
  ) => call('listThreadsAcross', { mailboxes, filter, cursor, limit }),
  threadsListed: (mailboxes: MailboxId[], filter: ThreadFilter, threads: ThreadId[]) =>
    call('threadsListed', { mailboxes, filter, threads }),
  thread: (id: ThreadId) => call('getThread', { id }),
  /** `mailbox`'s threads counted by category -- the inbox tabs' badges. */
  categoryCounts: (mailbox: MailboxId) => call('categoryCounts', { mailbox }),

  // ── Mail writing: what writing help is told ──────────────────────────

  mailWriting: () => call('mailWriting', {}),
  saveMailWriting: (writing: MailWriting) => call('saveMailWriting', { writing }),
  /** What writing help has learned from the replies they sent. */
  voiceLearning: () => call('voiceLearning', {}),
  /** The notes as the person left them; empty forgets everything learned. */
  saveVoiceLearning: (notes: string) => call('saveVoiceLearning', { notes }),
  /** A learning pass now, on the medium model -- several seconds, and refused,
   *  saying why, when there is nothing it may read. */
  learnFromReplies: () => call('learnFromReplies', {}),

  // ── Follow-ups: the top of the Inbox ─────────────────────────────────

  /** Threads waiting on a reply, and threads to nudge: one account's, or
   *  every mail account's when `account` is omitted. */
  mailFollowUps: (account?: AccountId | null) =>
    call('mailFollowUps', { account: account ?? null }),
  dismissFollowUp: (id: ThreadId) => call('dismissFollowUp', { id }),

  // ── Mail sorting: the categories a decision model sorts into ─────────

  mailSorting: () => call('mailSorting', {}),
  saveMailSorting: (sorting: MailSorting) => call('saveMailSorting', { sorting }),
  /** Sort older mail too, `days` back (`null`: all of it), in the background.
   *  `again` re-asks about threads already sorted. */
  sortOlderMail: (days: number | null, again: boolean, account?: AccountId | null) =>
    call('sortOlderMail', { account: account ?? null, days, again }),
  stopSortingOlderMail: (account?: AccountId | null) =>
    call('stopSortingOlderMail', { account: account ?? null }),
  /** A part left `available: false` on a `MailMessageDetail` -- over the
   *  attachment cap, with no blob yet. */
  fetchAttachment: (messageId: MailMessageId, index: number) =>
    call('fetchAttachment', { messageId, index }),
  /** A part whose `preview` is `page`, drawn as a document for a sandboxed
   *  frame. */
  previewAttachment: (messageId: MailMessageId, index: number) =>
    call('previewAttachment', { messageId, index }),

  // ── Mail: batch thread actions ──────────────────────────────────────
  //
  // Each is one outbox op per thread -- see `docs/plans/mail.md`'s "What an
  // action does" -- and answers with the `Op[]` it enqueued, which the
  // interface does not currently read back (the row already changed
  // optimistically); kept typed rather than `void` because that is what the
  // command answers.

  markRead: (threads: ThreadId[]) => call('markRead', { threads }),
  markUnread: (threads: ThreadId[]) => call('markUnread', { threads }),
  star: (threads: ThreadId[]) => call('star', { threads }),
  unstar: (threads: ThreadId[]) => call('unstar', { threads }),
  archive: (threads: ThreadId[]) => call('archive', { threads }),
  trash: (threads: ThreadId[]) => call('trash', { threads }),
  /** For good, and only what each thread has in `from` -- Trash or Spam
   *  mailboxes, which the backend checks -- nothing it has anywhere else. */
  deleteForever: (threads: ThreadId[], from: MailboxId[]) =>
    call('deleteForever', { threads, from }),
  /** Every thread in one Trash or Spam, for good. Answers how many. */
  emptyMailbox: (mailbox: MailboxId) => call('emptyMailbox', { mailbox }),
  moveToMailbox: (threads: ThreadId[], to: MailboxId) => call('moveToMailbox', { threads, to }),
  label: (threads: ThreadId[], label: string) => call('label', { threads, label }),
  unlabel: (threads: ThreadId[], label: string) => call('unlabel', { threads, label }),
  /** `until` is an ISO instant -- when the thread reappears. */
  snooze: (threads: ThreadId[], until: string) => call('snooze', { threads, until }),
  /** No outbox op -- see `Vault::release_snooze`'s own docs: snooze never
   *  told the server anything, so there is nothing to tell it is over. */
  unsnooze: (threads: ThreadId[]) => call('unsnooze', { threads }),

  // ── Mail: drafts and sending ────────────────────────────────────────

  /** Mints a blank draft, prefilled for a reply, a reply-all or a forward. */
  newDraft: (opts: {
    account: AccountId
    inReplyTo?: MailMessageId | null
    forwardOf?: MailMessageId | null
    replyAll?: boolean | null
  }) => call('newDraft', opts),
  saveDraft: (draft: Draft) => call('saveDraft', { draft }),
  discardDraft: (id: DraftId) => call('discardDraft', { id }),
  restoreDraft: (id: DraftId) => call('restoreDraft', { id }),
  editThreadDraft: (copies: MailMessageId[]) => call('editThreadDraft', { copies }),
  moveDraft: (draft: Draft, account: AccountId, identity: string) =>
    call('moveDraft', { draft, account, identity }),
  deleteThreadDraft: (draft: DraftId | null, copies: MailMessageId[]) =>
    call('deleteThreadDraft', { draft: draft ?? undefined, copies }),
  /** `delaySeconds` is the undo window (5-30s, clamped by the backend);
   *  `sendAt` queues for a specific, possibly distant, moment instead --
   *  "send later". Passing neither uses the backend's own default window. */
  sendDraft: (id: DraftId, delaySeconds?: number | null, sendAt?: string | null) =>
    call('sendDraft', { id, delaySeconds, sendAt }),
  /** Only valid inside the undo window `sendDraft` opened. */
  undoSend: (draftId: DraftId) => call('undoSend', { draftId }),
  /** Stops retrying a send that failed and is waiting to be tried again. */
  stopSend: (draftId: DraftId) => call('stopSend', { draftId }),
  drafts: (account: AccountId) => call('listDrafts', { account }),

  // ── Mail: sync ───────────────────────────────────────────────────────

  syncStatus: () => call('syncStatus', {}),
  /** Ask an account's task to sync now, out of its ordinary cadence. */
  syncAccount: (id: AccountId) => call('syncAccount', { id }),
  rebuildMailIndex: (id?: AccountId) => call('rebuildMailIndex', { id }),

  // ── Mail: remote images ─────────────────────────────────────────────
  //
  // Name exactly one of `sender`, `domain` or `messageId` -- the last is a
  // one-off grant kept in memory for this session only, the other two join
  // the standing, sealed allow-list. See `mailview.ts` for how a body's own
  // `X-Mail-Images-Hidden` decides whether to offer this at all.

  allowRemoteImages: (opts: {
    sender?: string | null
    domain?: string | null
    messageId?: MailMessageId | null
  }) => call('allowRemoteImages', opts),
  listRemoteImageAllowances: () => call('listRemoteImageAllowances', {}),
  revokeRemoteImageAllowance: (opts: { sender?: string | null; domain?: string | null }) =>
    call('revokeRemoteImageAllowance', opts),

  // ── Mail: search and address autocomplete ───────────────────────────

  /** Gmail-style operators (`from:`, `is:unread`, …), keyset-paged. */
  searchMail: (
    query: string,
    accountIds?: AccountId[] | null,
    cursor?: string | null,
    limit?: number | null,
  ) => call('searchMail', { query, accountIds, cursor, limit }),
  suggestAddresses: (prefix: string, limit?: number | null) =>
    call('suggestAddresses', { prefix, limit }),

  // ── Mail: Quick cleanup ─────────────────────────────────────────────

  /** Who sent the most of what is still in the Inbox over the last `days`
   *  (1..365, `null` for all of it), most first -- narrowed by account, by
   *  kind of mail and by name, see `InboxSendersArgs`. */
  inboxSenders: (args: InboxSendersArgs) => call('inboxSenders', args),

  // ── The Feed ─────────────────────────────────────────────────────────
  //
  // What somebody follows, and the stories it brings. "Feed" is the app and
  // `news` is the domain -- `feed` already meant a subscribed calendar in the
  // Rust -- so the wire names say news and these say what the screen does.
  // Available only when `status.capabilities.news` is true. A story's body is
  // not a command: it is served at `karmayoga://news/body/{id}`, which
  // `feed.svelte.ts` fetches the way `mailview.ts` fetches a message.

  newsSources: () => call('listNewsSources', {}),
  /** Every feed at, or advertised by, an address somebody pasted. */
  discoverFeeds: (url: string) => call('discoverFeeds', { url }),
  addNewsSource: (source: NewSource) => call('addNewsSource', { source }),
  saveNewsSource: (source: NewsSource) => call('saveNewsSource', { source }),
  /** Unfollows, and takes the source's stories with it. */
  deleteNewsSource: (id: NewsSourceId) => call('deleteNewsSource', { id }),

  stories: (query: StoryQuery) => call('listStories', { query }),
  story: (id: StoryId) => call('getStory', { id }),
  markStoriesRead: (ids: StoryId[], read: boolean) => call('markStoriesRead', { ids, read }),
  /**
   * Everything published up to `upTo` in these sources, read. Answers the
   * ids it changed, which is what makes an undo possible: marking them
   * unread again touches exactly those and nothing that was read already.
   */
  markAllStoriesRead: (sources: NewsSourceId[], upTo: string) =>
    call('markAllStoriesRead', { sources, upTo }),
  saveStories: (ids: StoryId[], saved: boolean) => call('saveStories', { ids, saved }),
  /** Ask the network: one source, or every source when `null`. */
  refreshNews: (source: NewsSourceId | null = null) => call('refreshNews', { source }),
  /** Fetch and extract the linked article, keeping it as the story's body. */
  fetchFullArticle: (id: StoryId) => call('fetchFullArticle', { id }),
  /** Writes an Articles item first and enriches it after, as the library does. */
  addStoryToLibrary: (id: StoryId) => call('addStoryToLibrary', { id }),
  importOpml: (xml: string) => call('importOpml', { xml }),
  exportOpml: () => call('exportOpml', {}),
  /** Who sent newsletters to this vault's inboxes over the last `days`. */
  newsletterSenders: (days: number) => call('newsletterSenders', { days }),

  // ── Money ──────────────────────────────────────────────────────────
  //
  // Linked banks, accounts kept by hand, their transactions, categories,
  // budgets and rules -- see `docs/plans/money.md`. Available only when
  // `status.capabilities.money` is true. Named as the commands are, since
  // nothing here needs a friendlier word than the wire's own. Amounts are
  // integers of minor units throughout.
  //
  // The Plaid secret crosses once, in `savePlaidCredentials`, and nothing
  // answers it back: `plaidSetup` says whether keys are set, which
  // environment, and the client id's last four characters. A bank sign-in
  // is an opaque `session` and Plaid's page address; no token of any kind
  // reaches this side.

  plaidSetup: () => call('plaidSetup', {}),
  savePlaidCredentials: (clientId: string, secret: string, environment: PlaidEnvironment) =>
    call('savePlaidCredentials', { clientId, secret, environment }),
  clearPlaidCredentials: () => call('clearPlaidCredentials', {}),

  /** Begin a Hosted Link sign-in -- or, with `linkId`, sign in to that bank again. */
  startBankLink: (linkId: BankLinkId | null = null) => call('startBankLink', { linkId }),
  /** Ask how a sign-in is going; `waiting` until the person finishes in the browser. */
  checkBankLink: (session: string) => call('checkBankLink', { session }),
  cancelBankLink: (session: string) => call('cancelBankLink', { session }),
  listBankLinks: () => call('listBankLinks', {}),
  /** Name, paused and order only; the provider half is kept as stored. */
  saveBankLink: (link: BankLink) => call('saveBankLink', { link }),
  /** Disconnect. `keepHistory` keeps the accounts and their transactions, as manual ones. */
  deleteBankLink: (id: BankLinkId, keepHistory: boolean) =>
    call('deleteBankLink', { id, keepHistory }),
  /** Sync one bank, or every bank when `null`. */
  syncBankLinks: (linkId: BankLinkId | null = null) => call('syncBankLinks', { linkId }),

  listBankAccounts: () => call('listBankAccounts', {}),
  saveBankAccount: (account: BankAccount) => call('saveBankAccount', { account }),
  createManualAccount: (name: string, kind: AccountKind, currency: string) =>
    call('createManualAccount', { name, kind, currency }),
  /** An account kept by hand only; a bank's is refused. */
  deleteBankAccount: (id: BankAccountId) => call('deleteBankAccount', { id }),

  listTransactions: (query: TxnQuery) => call('listTransactions', { query }),
  getTransaction: (id: TxnId) => call('getTransaction', { id }),
  /** A person's change to many at once; answers the ids that changed. */
  updateTransactions: (ids: TxnId[], patch: TxnPatch) => call('updateTransactions', { ids, patch }),
  addManualTransaction: (txn: NewTxn) => call('addManualTransaction', { txn }),
  /** Imported or typed-in only; a bank's is refused. */
  deleteTransactions: (ids: TxnId[]) => call('deleteTransactions', { ids }),
  previewImport: (
    content: string,
    format: ImportFormat | null = null,
    currency: string | null = null,
  ) => call('previewImport', { content, format, currency }),
  importTransactions: (
    accountId: BankAccountId,
    content: string,
    format: ImportFormat | null = null,
    mapping: CsvMapping | null = null,
  ) => call('importTransactions', { accountId, content, format, mapping }),

  listMoneyCategories: () => call('listMoneyCategories', {}),
  saveMoneyCategory: (category: MoneyCategory) => call('saveMoneyCategory', { category }),
  deleteMoneyCategory: (id: MoneyCategoryId) => call('deleteMoneyCategory', { id }),
  listMoneyRules: () => call('listMoneyRules', {}),
  saveMoneyRule: (rule: MoneyRule) => call('saveMoneyRule', { rule }),
  deleteMoneyRule: (id: MoneyRuleId) => call('deleteMoneyRule', { id }),
  /** Run the rules -- or one -- over everything still open; answers how many changed. */
  applyMoneyRules: (ruleId: MoneyRuleId | null = null) => call('applyMoneyRules', { ruleId }),

  /** One month (`YYYY-MM`) against its budgets, in the main currency. */
  moneyMonth: (month: string) => call('moneyMonth', { month }),
  /** Income and spending for `months` months ending with `end`'s, oldest first. */
  moneyCashFlow: (end: string, months: number) => call('moneyCashFlow', { end, months }),
  moneyNetWorth: (from: string, to: string) => call('moneyNetWorth', { from, to }),
  moneyRecurring: () => call('moneyRecurring', {}),

  // ── Screen time ────────────────────────────────────────────────────
  //
  // Which app or site had somebody's attention, on which machine, and for
  // how long -- see `docs/plans/screentime.md`. Available only when
  // `status.capabilities.screentime` is true. Named as the commands are.
  // `record_screen_time` and `screen_time_collector` are left out: they are
  // the collectors' (the shell's own, the GNOME and browser extensions,
  // over the heartbeats routes), and nothing in this window sends one.

  /** Totals for a range of days, by item, category, day, hour and device. */
  screenTimeReport: (query: ScreenQuery) => call('screenTimeReport', { query }),
  /** One day's runs, in order. `devices` absent or empty is every device. */
  screenTimeTimeline: (date: string, tz: string, devices?: string[]) =>
    call('screenTimeTimeline', { date, tz, devices }),
  /** Every item seen in the last 30 days and every labelled one. */
  listScreenApps: () => call('listScreenApps', {}),
  saveScreenApp: (app: ScreenApp) => call('saveScreenApp', { app }),
  deleteScreenApp: (id: ScreenAppId) => call('deleteScreenApp', { id }),
  /** The categories, in order; the first call writes the starter ones. */
  listScreenCategories: () => call('listScreenCategories', {}),
  saveScreenCategory: (category: ScreenCategory) => call('saveScreenCategory', { category }),
  deleteScreenCategory: (id: ScreenCategoryId) => call('deleteScreenCategory', { id }),
  listScreenDevices: () => call('listScreenDevices', {}),
  screenTimeSettings: () => call('screenTimeSettings', {}),
  saveScreenTimeSettings: (settings: ScreenTimeSettings) =>
    call('saveScreenTimeSettings', { settings }),
  /** Delete every span of one item (by key); answers how many went. */
  forgetScreenItem: (key: string) => call('forgetScreenItem', { key }),
  /** Delete everything recorded between two instants; answers how many spans went. */
  deleteScreenTime: (from: string, to: string) => call('deleteScreenTime', { from, to }),

  // This machine's half: whether it records, what it is called, how it
  // watches focus. By name, not through `call`: these are the shell's own,
  // answered by the computer this window runs on -- a window looking at a
  // vault elsewhere still says whether *this* computer is tracked. Each
  // answers the status as it is afterwards, except the browser connection,
  // which is shown once and never again.
  screentimeStatus: () => invoke<ScreenTimeStatus>('screentime_status'),
  screentimeSetTracking: (on: boolean) =>
    invoke<ScreenTimeStatus>('screentime_set_tracking', { on }),
  screentimeSetDevice: (name: string) =>
    invoke<ScreenTimeStatus>('screentime_set_device', { name }),
  /** Copy the GNOME Shell extension into place, enable it, and write its token. */
  screentimeInstallGnomeExtension: () =>
    invoke<ScreenTimeStatus>('screentime_install_gnome_extension'),
  /** Mint a heartbeats token for a browser extension, as one code to paste. */
  screentimeConnectBrowser: (browser: string) =>
    invoke<BrowserConnection>('screentime_connect_browser', { browser }),

  // ── Files ──────────────────────────────────────────────────────────
  //
  // The files catalog and the files folder -- see `docs/plans/files.md`.
  // Available when `status.capabilities.files` is true. Named as the
  // commands are, bar the shell's own at the end.

  filesStatus: () => call('filesStatus', {}),
  saveFilesSettings: (settings: FilesSettings) => call('saveFilesSettings', { settings }),
  /** Set the folder up at `path` (absolute), or at the default when absent. */
  setUpFilesFolder: (path?: string) => call('setUpFilesFolder', { path }),
  listFiles: (query: FileQuery) => call('listFiles', { query }),
  /** One folder: subfolders from the disk, files from the catalog. */
  listFolder: (dir: string) => call('listFolder', { dir }),
  getFile: (id: FileId) => call('getFile', { id }),
  filesFacets: () => call('filesFacets', {}),
  /**
   * Copy files from paths on the vault's own machine. Only the shell on that
   * machine may (`FilesStatus.local`); everything else uploads.
   */
  importFiles: (
    paths: string[],
    opts: { dir?: string; origin?: FileOrigin; encrypted?: boolean } = {},
  ) => call('importFiles', { paths, ...opts }),
  beginUpload: (
    name: string,
    size: number,
    opts: { dir?: string; origin?: FileOrigin; encrypted?: boolean } = {},
  ) => call('beginUpload', { name, size, ...opts }),
  /**
   * One chunk of an upload, as raw bytes. Answers the bytes received so far.
   * Not a service command (its body is bytes, as `put_blob`'s is): the shell
   * has `upload_chunk`, reading the upload and the offset from headers; web
   * access posts to `/v1/files/upload/{upload}?offset=`.
   */
  uploadChunk: (upload: string, offset: number, bytes: Uint8Array) =>
    uploadChunk(upload, offset, bytes),
  finishUpload: (upload: string) => call('finishUpload', { upload }),
  cancelUpload: (upload: string) => call('cancelUpload', { upload }),
  createFolder: (dir: string, name: string) => call('createFolder', { dir, name }),
  renameFile: (id: FileId, name: string) => call('renameFile', { id, name }),
  moveFiles: (ids: FileId[], dir: string) => call('moveFiles', { ids, dir }),
  renameFolder: (path: string, name: string) => call('renameFolder', { path, name }),
  moveFolder: (path: string, into: string) => call('moveFolder', { path, into }),
  updateFiles: (ids: FileId[], update: FileUpdate) => call('updateFiles', { ids, update }),
  stackFiles: (top: FileId, others: FileId[]) => call('stackFiles', { top, others }),
  unstackFiles: (ids: FileId[]) => call('unstackFiles', { ids }),
  trashFiles: (ids: FileId[]) => call('trashFiles', { ids }),
  /** Answers how many files went to the trash. */
  trashFolder: (path: string) => call('trashFolder', { path }),
  restoreFiles: (ids: FileId[]) => call('restoreFiles', { ids }),
  deleteFilesForever: (ids: FileId[]) => call('deleteFilesForever', { ids }),
  emptyFilesTrash: () => call('emptyFilesTrash', {}),
  forgetMissingFiles: (ids: FileId[]) => call('forgetMissingFiles', { ids }),
  setFileEncrypted: (id: FileId, encrypted: boolean) => call('setFileEncrypted', { id, encrypted }),
  /** A film's poster frame, drawn here: a JPEG or PNG, base64. */
  setFileThumbnail: (id: FileId, image: string) => call('setFileThumbnail', { id, image }),
  rescanFiles: () => call('rescanFiles', {}),
  listAlbums: () => call('listAlbums', {}),
  saveAlbum: (album: Album) => call('saveAlbum', { album }),
  deleteAlbum: (id: AlbumId) => call('deleteAlbum', { id }),
  addToAlbum: (id: AlbumId, files: FileId[]) => call('addToAlbum', { id, files }),
  removeFromAlbum: (id: AlbumId, files: FileId[]) => call('removeFromAlbum', { id, files }),
  /** Move pictures kept inside entries, notes and messages into the folder. */
  bringInAttachments: () => call('bringInAttachments', {}),
  saveMailPartToFiles: (messageId: string, part: string, dir?: string) =>
    call('saveMailPartToFiles', { messageId, part, dir }),
  restoreFileFromBackup: (id: FileId) => call('restoreFileFromBackup', { id }),

  // This machine's half: pickers, a drop's paths, opening a file in another
  // program. By name, not through `call`: they are the shell's own, and only
  // a window on the vault's machine (`FilesStatus.local`) can use them.
  /** Ask the system file picker for files and import them; `[]` if cancelled. */
  pickAndImportFiles: (opts: { dir?: string; origin?: FileOrigin; encrypted?: boolean } = {}) =>
    invoke<FileEntry[]>('pick_and_import_files', opts),
  /** Ask the system folder picker for a folder; null if cancelled. */
  pickFilesFolder: () => invoke<string | null>('pick_files_folder'),
  /** Open a file in the program the system uses for it. */
  openFileExternally: (id: FileId) => invoke<void>('open_file_externally', { id }),
  /** Show a plain file in the system file manager. */
  revealFile: (id: FileId) => invoke<void>('reveal_file', { id }),
  /** Ask where to save copies of these files, and save them; how many were saved. */
  exportFiles: (ids: FileId[]) => invoke<number>('export_files', { ids }),

  // ── Health ─────────────────────────────────────────────────────────
  //
  // What a watch and a phone measured, what somebody takes, eats and lives
  // with, and the papers their clinicians gave them -- see
  // `docs/plans/health.md`. Available only when `status.capabilities.health`
  // is true. Named as the commands are; every value is in its metric's
  // canonical unit, and `lib/health.ts` converts for display. `from` and `to`
  // are local dates (`YYYY-MM-DD`) except where a comment says instants, and
  // `tz` is the reader's IANA zone.
  //
  // `record_health_push` has a method here for completeness and no caller:
  // the phone's automations post to it through the web-access route, with a
  // write-only token, and no window sends one.

  healthCatalog: () => call('healthCatalog', {}),
  healthSettings: () => call('healthSettings', {}),
  saveHealthSettings: (settings: HealthSettings) => call('saveHealthSettings', { settings }),
  listHealthSources: () => call('listHealthSources', {}),
  /** Name and paused only; what arrival learned (`status`) is kept as stored. */
  saveHealthSource: (source: HealthSource) => call('saveHealthSource', { source }),
  /** What the source brought stays. */
  deleteHealthSource: (id: HealthSourceId) => call('deleteHealthSource', { id }),
  recordHealthPush: (sourceId: HealthSourceId, headers: PushHeaders, body: unknown) =>
    call('recordHealthPush', { sourceId, headers, body }),
  /** Start importing an Apple `export.zip` by path. The shell's picker
   *  (`healthImportApple`) is how a window gets one; this is the vault's half. */
  importAppleHealth: (path: string, sourceId: HealthSourceId | null = null) =>
    call('importAppleHealth', { path, sourceId: sourceId ?? undefined }),
  /** Start importing an `export.zip` that is in Files: one a sync tool
   *  brought from the phone, or one uploaded from a window on another
   *  machine. `trashAfter` puts it in the Files trash once imported. */
  importAppleHealthFile: (
    file: FileId,
    sourceId: HealthSourceId | null = null,
    trashAfter = false,
  ) => call('importAppleHealthFile', { file, sourceId: sourceId ?? undefined, trashAfter }),
  appleImportStatus: () => call('appleImportStatus', {}),
  /** Answers whether there was an import to stop. */
  cancelAppleImport: () => call('cancelAppleImport', {}),
  /** Whether the food-lookup key is set, and its last four characters. */
  healthKeys: () => call('healthKeys', {}),
  saveHealthKeys: (fdcKey: string) => call('saveHealthKeys', { fdcKey }),
  clearHealthKeys: () => call('clearHealthKeys', {}),

  healthToday: (date: string, tz: string) => call('healthToday', { date, tz }),
  healthSeries: (query: SeriesQuery) => call('healthSeries', { query }),
  /** Every sample of a metric between two instants -- at most three days. */
  healthSamples: (metric: Metric, from: string, to: string) =>
    call('healthSamples', { metric, from, to }),
  /** The latest value of each metric at or before the end of `on`. */
  healthLatest: (metrics: Metric[], on: string) => call('healthLatest', { metrics, on }),
  addHealthReading: (reading: ManualReading) => call('addHealthReading', { reading }),
  /** Between two instants, only `source`'s when given; answers how many went. */
  deleteHealthSamples: (metric: Metric, from: string, to: string, source: string | null = null) =>
    call('deleteHealthSamples', { metric, from, to, source: source ?? undefined }),
  healthActivity: (from: string, to: string, tz: string) =>
    call('healthActivity', { from, to, tz }),
  healthSleep: (from: string, to: string, tz: string) => call('healthSleep', { from, to, tz }),
  healthGlucose: (from: string, to: string, tz: string) => call('healthGlucose', { from, to, tz }),
  healthNutrition: (from: string, to: string, tz: string) =>
    call('healthNutrition', { from, to, tz }),
  healthDayTimeline: (date: string, tz: string) => call('healthDayTimeline', { date, tz }),

  /** Between two instants. */
  listWorkouts: (from: string, to: string) => call('listWorkouts', { from, to }),
  getWorkout: (id: WorkoutId) => call('getWorkout', { id }),
  saveWorkout: (workout: Workout) => call('saveWorkout', { workout }),
  deleteWorkout: (id: WorkoutId) => call('deleteWorkout', { id }),

  listMedications: () => call('listMedications', {}),
  saveMedication: (medication: Medication) => call('saveMedication', { medication }),
  /** Its doses go with it. */
  deleteMedication: (id: MedicationId) => call('deleteMedication', { id }),
  /** Between two instants. */
  listDoses: (from: string, to: string, medicationId: MedicationId | null = null) =>
    call('listDoses', { from, to, medicationId: medicationId ?? undefined }),
  logDose: (dose: Dose) => call('logDose', { dose }),
  deleteDose: (id: DoseId) => call('deleteDose', { id }),
  /** Every due time on a day, and the dose that answered it. */
  dosesDue: (date: string, tz: string) => call('dosesDue', { date, tz }),
  medicationAlerts: (from: string, to: string, tz: string) =>
    call('medicationAlerts', { from, to, tz }),
  medicationAdherence: (medicationId: MedicationId | null, from: string, to: string, tz: string) =>
    call('medicationAdherence', { medicationId: medicationId ?? undefined, from, to, tz }),
  listAllergies: () => call('listAllergies', {}),
  saveAllergy: (allergy: Allergy) => call('saveAllergy', { allergy }),
  deleteAllergy: (id: AllergyId) => call('deleteAllergy', { id }),

  listConditions: () => call('listConditions', {}),
  saveCondition: (condition: Condition) => call('saveCondition', { condition }),
  /** `removeTrackers` also deletes the trackers its plan made. */
  deleteCondition: (id: ConditionId, removeTrackers: boolean) =>
    call('deleteCondition', { id, removeTrackers }),
  conditionTemplates: () => call('conditionTemplates', {}),
  applyConditionTemplate: (template: TemplateKey, choice: TemplateChoice) =>
    call('applyConditionTemplate', { template, choice }),
  conditionReport: (id: ConditionId, date: string, tz: string) =>
    call('conditionReport', { id, date, tz }),

  /** Every result, or the results of one test (`key`, as `LabTrend.key`). */
  listLabs: (key: string | null = null) => call('listLabs', { key: key ?? undefined }),
  saveLab: (lab: LabResult) => call('saveLab', { lab }),
  deleteLab: (id: LabResultId) => call('deleteLab', { id }),
  labTrends: () => call('labTrends', {}),
  listHealthDocuments: () => call('listHealthDocuments', {}),
  getHealthDocument: (id: HealthDocumentId) => call('getHealthDocument', { id }),
  /** The files are added through Files first (`addFiles`, sealed, origin
   *  `health`); this makes one document of them. */
  addHealthDocument: (title: string, kind: DocKind, date: string | null, files: FileId[]) =>
    call('addHealthDocument', { title, kind, date: date ?? undefined, files }),
  saveHealthDocument: (document: HealthDocument) => call('saveHealthDocument', { document }),
  /** `deleteLabs` also deletes the results accepted from it. */
  deleteHealthDocument: (id: HealthDocumentId, deleteLabs: boolean) =>
    call('deleteHealthDocument', { id, deleteLabs }),
  /** Read lab results out of it with the model. `text` is a PDF's own text
   *  layer, read here; without it the model reads the pages. */
  extractHealthDocument: (id: HealthDocumentId, text?: string) =>
    call('extractHealthDocument', { id, text }),
  acceptDocumentLabs: (id: HealthDocumentId, labs: LabResult[]) =>
    call('acceptDocumentLabs', { id, labs }),

  listFoods: (query: string) => call('listFoods', { query }),
  saveFood: (food: Food) => call('saveFood', { food }),
  deleteFood: (id: FoodId) => call('deleteFood', { id }),
  /** Only the search text leaves the machine. */
  searchFoods: (query: string) => call('searchFoods', { query }),
  lookupBarcode: (code: string) => call('lookupBarcode', { code }),
  /** Between two instants. */
  listMeals: (from: string, to: string) => call('listMeals', { from, to }),
  saveMeal: (meal: Meal) => call('saveMeal', { meal }),
  deleteMeal: (id: MealId) => call('deleteMeal', { id }),
  /** A picture added through Files (sealed, origin `health`), sent to the
   *  model: foods and grams, every item an estimate. */
  estimateMealPhoto: (photo: FileId) => call('estimateMealPhoto', { photo }),
  mealResponse: (id: MealId) => call('mealResponse', { id }),
  /** A summary for a clinic visit, from the vault's own numbers. */
  healthVisitBrief: (conditionId: ConditionId | null, since: string, tz: string) =>
    call('healthVisitBrief', { conditionId: conditionId ?? undefined, since, tz }),

  // This machine's half: the phone's push address and token, and the file
  // dialog an Apple export is picked in. By name, not through `call`: they
  // are the shell's own, and the path of the chosen file never reaches the
  // webview.

  /** Mint (or re-mint, revoking the old) the write-only token for a source.
   *  The token is shown once. */
  healthPushSetup: (sourceId: HealthSourceId) =>
    invoke<PushSetup>('health_push_setup', { sourceId }),
  /** Answers whether there was a token to revoke. */
  healthPushRevoke: (sourceId: HealthSourceId) =>
    invoke<boolean>('health_push_revoke', { sourceId }),
  /** Ask for an `export.zip` and start importing it; `null` when nothing was picked. */
  healthImportApple: () => invoke<ImportStatus | null>('health_import_apple'),

  // ── Accounts ───────────────────────────────────────────────────────
  //
  // A mailbox provider signed in to -- not owned by the mail app, see
  // `karmayoga_core::account`'s module doc. `AccountView` never carries a
  // secret; `hasPassword` and `signedIn` are the only things this interface
  // is ever told about one.

  /** Every account this vault knows about, whole -- but never a secret. */
  accounts: () => call('listAccounts', {}),
  account: (id: AccountId) => call('getAccount', { id }),
  /** Create or update an account record. Never the credential -- see below. */
  saveAccount: (account: Account) => call('saveAccount', { account }),
  deleteAccount: (id: AccountId) => call('deleteAccount', { id }),
  /** Host, port, security and OAuth endpoints each well-known provider
   *  publishes, for the add-account sheet to draw before anybody has typed
   *  an address. */
  accountPresets: () => call('accountPresets', {}),
  /** Connect to the account's outgoing server and sign in, sending nothing.
   *  Throws with the server's own words when it refuses. */
  checkSending: (id: AccountId) => call('checkSending', { id }),
  /** Store or replace a password-authenticated account's credential. */
  saveAccountPassword: (id: AccountId, password: string) =>
    call('saveAccountPassword', { id, password }),
  /** Flip one caller's -- the assistant's, or MCP's -- switches on one account. */
  setAgentAccess: (id: AccountId, caller: AgentCallerKind, access: AgentMailAccess) =>
    call('setAgentAccess', { id, caller, access }),

  /**
   * Start an OAuth sign-in: open the answered `url` yourself, with
   * `openExternal` (see `ui/src/lib/open-external.ts`) on the desktop, or by
   * showing it to copy when there is no browser this process can open --
   * remote and server mode. Remember `signInId` for the two calls below.
   */
  // Named `Oauth`, not `OAuth`: `gen-api.mjs`'s `snake_case` -> `camelCase`
  // conversion only capitalises the letter after each underscore, the same
  // way `mcp_status` became `mcpStatus` rather than `mcpStatus`'s all-caps
  // cousin. Matching it here is what keeps `call('beginOauthSignIn', ...)`
  // a compile-time-checked key into `Commands` rather than a string that
  // merely looks right.
  beginOauthSignIn: (opts: {
    authUrl: string
    tokenUrl: string
    clientId: string
    clientSecret?: string
    scopes: string[]
    loginHint?: string
  }) => call('beginOauthSignIn', opts),
  /**
   * Wait for that sign-in to land. Resolves once the browser has come back
   * and the code has been exchanged, or rejects with a `CommandError` whose
   * `code` is one of `invalidGrant`, `invalidClient`, `provider`,
   * `timedOut` or `cancelled` -- see `crates/karmayoga-service/src/signin.rs`.
   * Safe to call again after a dropped connection; it does not consume
   * anything.
   */
  awaitOauthSignIn: (signInId: string) => call('awaitOauthSignIn', { signInId }),
  /** Withdraw a sign-in nobody is going to finish. */
  cancelOauthSignIn: (signInId: string) => call('cancelOauthSignIn', { signInId }),
  /**
   * Move a finished sign-in's tokens onto an account, sealed. The last step
   * of the flow above -- see `crates/karmayoga-service/src/domains/
   * accounts.rs`'s module doc for the hand-off this closes.
   */
  /**
   * `scopes` are the ones the sign-in asked for, recorded on the account --
   * what tells "signed in to read calendars" apart from "signed in to write
   * them" afterwards, and so whether its calendars can take a new event.
   */
  attachOauthSignIn: (id: AccountId, signInId: string, clientSecret?: string, scopes?: string[]) =>
    call('attachOauthSignIn', { id, signInId, clientSecret, scopes }),

  // ── Chat links ─────────────────────────────────────────────────────
  //
  // The assistant as a bot on a chat service -- see `docs/plans/chat.md`.
  // Like an account, a link's secrets (the bot's password, its session)
  // never come back: a view says only whether each is there.

  /** Every link, each with where it stands right now. */
  chatLinks: () => call('chatLinks', {}),
  /** A fresh, switched-on Matrix link with nothing filled in. Not saved. */
  newChatLink: () => call('newChatLink', {}),
  /**
   * Create or update a link, and restart it so the change takes effect at
   * once. `password` is the bot's, given once to sign in; `null` or absent
   * keeps whatever sign-in the link already has. Rejects with the server's
   * own message when the link is invalid.
   */
  saveChatLink: (link: ChatLink, password?: string | null) =>
    call('saveChatLink', { link, password }),
  /** Stop a link and forget it, its sign-in and what it kept on this machine. */
  removeChatLink: (id: ChatLinkId) => call('removeChatLink', { id }),
  /**
   * Answer the emoji comparison a link's bot is in: they match, they do not,
   * or stop. `flow` is `ChatVerification.flow`, so an answer cannot land on a
   * newer verification than the one on screen.
   */
  answerChatVerification: (id: ChatLinkId, flow: string, answer: VerificationAnswer) =>
    call('answerChatVerification', { id, flow, answer }),
  /** Have a link's bot ask its owner to verify it; their chat app shows the
   *  request. Rejects when the bot is not running here. */
  startChatVerification: (id: ChatLinkId) => call('startChatVerification', { id }),
  /**
   * The owner's devices as a link's running bot sees them, each with whether
   * it is trusted. Rejects when the bot is not running here.
   */
  chatDevices: (id: ChatLinkId) => call('chatDevices', { id }),
  /**
   * Choose, by hand, which of the owner's devices the bot trusts: exactly
   * `devices`, replacing the list. It believes an encrypted message only from
   * one of them and gives its keys to no other, and it takes effect at once.
   * Rejects with `forbidden` from anywhere but this computer (its own window or
   * its local socket).
   */
  setChatTrustedDevices: (id: ChatLinkId, devices: string[]) =>
    call('setChatTrustedDevices', { id, devices }),
}

/**
 * URL for an attachment.
 *
 * Media is served by a custom protocol handler in the Rust shell rather than
 * as a data: URL, so a 400 MB video streams and seeks instead of being
 * base64-encoded into the document.
 */
export function mediaUrl(blob: string): string {
  if (MOCK) return mockMediaUrl(blob)
  if (WEB) return `${webMediaBase()}/v1/blob/${blob}`
  // Tauri maps custom schemes to an http origin on Windows and Android.
  const base =
    navigator.userAgent.includes('Windows') || navigator.userAgent.includes('Android')
      ? 'http://karmayoga.localhost'
      : 'karmayoga://localhost'
  return `${base}/${blob}`
}

/**
 * URL for a file in the files catalog: `karmayoga://files/file/{id}` in the
 * desktop window, answered with ranges from the folder (or the vault, for a
 * sealed file); `/v1/files/file/{id}` over web access.
 *
 * Unlike a blob, a file's bytes can change under the same id -- something
 * else may edit it in the folder -- so `version` (its hash, or its modified
 * time when not yet hashed) is added for the webview's cache to tell them
 * apart. `fileVersion` gives it.
 */
export function fileUrl(id: string, version = ''): string {
  const v = version ? `?v=${encodeURIComponent(version)}` : ''
  if (MOCK) return mockFileUrl(id)
  if (WEB) return `${webMediaBase()}/v1/files/file/${id}${v}`
  const base =
    navigator.userAgent.includes('Windows') || navigator.userAgent.includes('Android')
      ? 'http://karmayoga.localhost/files'
      : 'karmayoga://files'
  return `${base}/file/${id}${v}`
}

/** What `fileUrl` is versioned by. */
export function fileVersion(file: { hash: string; modifiedAt: string }): string {
  return file.hash || file.modifiedAt
}

/** A file's thumbnail, when one has been drawn: a blob like any other. */
export function thumbnailUrl(file: { thumbnail?: string }): string | null {
  return file.thumbnail ? mediaUrl(file.thumbnail) : null
}

async function uploadChunk(upload: string, offset: number, bytes: Uint8Array): Promise<number> {
  if (MOCK) return mockUploadChunk(upload, offset, bytes)
  if (WEB) return webUploadChunk(upload, offset, bytes)
  const mod = await import('@tauri-apps/api/core')
  try {
    return await mod.invoke<number>('upload_chunk', bytes, {
      headers: { 'x-karmayoga-upload': upload, 'x-karmayoga-offset': String(offset) },
    })
  } catch (raw) {
    if (raw && typeof raw === 'object' && 'code' in raw && 'message' in raw) {
      throw new VaultError(String(raw.code), String(raw.message))
    }
    throw new VaultError('unknown', String(raw))
  }
}

let mockFileUrl: (id: string) => string = () => ''
let mockUploadChunk: (
  upload: string,
  offset: number,
  bytes: Uint8Array,
) => Promise<number> = async () => 0
if (MOCK) {
  const { mockFileUrl: f, mockUploadChunk: u } = await import('./mock-files')
  mockFileUrl = f
  mockUploadChunk = u
}

let mockMediaUrl: (blob: string) => string = () => ''
if (MOCK) {
  const { mockMediaUrl: f, mockSendMessage } = await import('./mock')
  mockMediaUrl = f
  sendMessage = mockSendMessage
  voiceStart = (await import('./mock-voice')).mockVoiceStart
  const listen = await import('./mock-listen')
  listenStart = listen.mockListenStart
  mockListen = listen.mockListenControls
  const dictation = await import('./mock-dictation')
  dictationStart = dictation.mockDictationStart
  mockDictation = dictation.mockDictationControls
}
