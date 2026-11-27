// The panel's half of its contract with the shell, and the only file of the
// panel's page that names a shell command: `crates/jatayu-app/src/acl.rs`
// reads this file to check the window's capability grants exactly what the
// page calls. Mirrors `crates/jatayu-app/src/panel/mod.rs` field for field:
// camelCase, an optional field absent (never null), an empty list absent.
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

/** Which part of the shell a card belongs to, and so who hears its buttons. */
export type PanelOwner = 'meeting' | 'voiceTyping' | 'talk' | 'mail' | 'notes' | 'recording' | 'ask'

/** What a card is about: its mark and its accent (that app's `--app-*`). */
export type PanelKind =
  'notice' | 'meeting' | 'mail' | 'invite' | 'notes' | 'assistant' | 'voiceTyping'

/** What a card is doing: the colour of its state. */
export type PanelTone = 'neutral' | 'listening' | 'working' | 'success' | 'attention' | 'trouble'

/** What an activity is doing. */
export type PanelMode =
  | 'idle'
  | 'starting'
  | 'listening'
  | 'hearing'
  | 'thinking'
  | 'working'
  | 'speaking'
  | 'dictating'
  | 'muted'
  | 'waiting'
  | 'recording'
  | 'sending'
  | 'done'
  | 'failed'

/** The mark a tool gets: what it touches. */
export type ToolIcon =
  | 'generic'
  | 'mail'
  | 'calendar'
  | 'tasks'
  | 'notes'
  | 'journal'
  | 'web'
  | 'files'
  | 'image'
  | 'typing'
  | 'memory'
  | 'money'
  | 'health'
  | 'news'

export interface ToolUse {
  /** The tool's own name: `search_mail`. */
  name: string
  /** "Searching your mail…" */
  label: string
  icon: ToolIcon
}

/** Who said a turn. `heard`: what voice typing heard; `typed`: what was typed elsewhere. */
export type Speaker = 'you' | 'assistant' | 'heard' | 'typed'

export interface Turn {
  who: Speaker
  /** The name over it (the assistant's); the page says "You" for the person. */
  name?: string
  text: string
  /** Still coming: a caret. */
  pending: boolean
}

export type Rsvp = 'accepted' | 'tentative' | 'declined' | 'needsAction'

export interface Person {
  name: string
  email?: string
  /** Who they are, in a line. */
  detail?: string
  status?: Rsvp
  organizer: boolean
  you: boolean
}

export interface EventCard {
  title: string
  /** "10:00–10:30", "All day". */
  when: string
  /** "Today", "Thu 27 Nov". */
  date?: string
  place?: string
  /** The service a join link is for: "Zoom". */
  join?: string
  calendar?: string
  people?: Person[]
  /** The person's own answer, where it is an invitation. */
  rsvp?: Rsvp
}

export interface MailCard {
  from: Person
  subject: string
  snippet?: string
  received?: string
  account?: string
  attachments: number
}

export interface TaskCard {
  title: string
  detail?: string
  due?: string
  done: boolean
  /** A box to tick, sending this action. Absent: no box. */
  check?: string
}

export interface NoteCard {
  title: string
  excerpt?: string
  detail?: string
}

export type Decision = 'pending' | 'accepted' | 'rejected'

/** One of a meeting's action items. A press sends `accept:{id}` or `reject:{id}`. */
export interface Proposal {
  id: string
  title: string
  detail?: string
  due?: string
  state: Decision
}

export interface Suggestion {
  label: string
  text: string
}

export interface InputMode {
  id: string
  label: string
  placeholder: string
  submit: string
}

/** A box to type in. A submit sends the picked mode's id, or `id`, with the text. */
export interface PanelInput {
  id: string
  placeholder: string
  submit: string
  multiline: boolean
  suggestions?: Suggestion[]
  modes?: InputMode[]
  busy: boolean
  /** The last submit failed: the page puts the text back and says this. */
  error?: string
}

export interface PanelCheck {
  action: string
  done: boolean
}

export interface PanelItem {
  text: string
  detail?: string
  check?: PanelCheck
}

export interface PanelSection {
  heading: string
  items: PanelItem[]
  /** Folded under "More" until the card is opened. */
  folded: boolean
}

/** A button. A card lists its buttons primary first; they are drawn with the primary rightmost. */
export interface PanelAction {
  id: string
  label: string
  primary: boolean
  danger: boolean
  /** A mark from a fixed set: reply, archive, read, open, close, check, x, copy, join, mic, micOff, stop, send, sparkle, later. */
  icon?: string
  /** Text to put on the clipboard when pressed, before reporting the press. */
  copy?: string
}

/** One structured part of a card. */
export type PanelBlock =
  | { type: 'activity'; mode: PanelMode; label?: string; tool?: ToolUse }
  | { type: 'conversation'; turns: Turn[] }
  | ({ type: 'event' } & EventCard)
  | ({ type: 'mail' } & MailCard)
  | { type: 'people'; heading?: string; people: Person[]; folded: boolean }
  | ({ type: 'task' } & TaskCard)
  | ({ type: 'note' } & NoteCard)
  | { type: 'text'; heading?: string; text: string; folded: boolean }
  | { type: 'quote'; label?: string; text: string }
  | ({ type: 'list' } & PanelSection)
  | { type: 'proposals'; heading?: string; items: Proposal[] }
  | { type: 'choices'; prompt?: string; choices: PanelAction[] }
  | ({ type: 'input' } & PanelInput)
  | { type: 'clock'; sinceMs: number; label?: string }

/**
 * When a card goes by itself. `remainingMs` and `held` are the shell's, as of
 * when the stack was sent: the page drains a bar from there, and stops while
 * `held`.
 */
export interface PanelExpiry {
  afterMs: number
  action: string
  remainingMs: number
  held: boolean
}

/** One card. The shell sends the whole stack on every change. */
export interface Panel {
  id: string
  owner: PanelOwner
  kind: PanelKind
  tone: PanelTone
  kicker: string
  title: string
  count?: string
  body?: string
  blocks?: PanelBlock[]
  meter: boolean
  actions?: PanelAction[]
  busy: boolean
  expires?: PanelExpiry
}

/** How loud the microphone is for one card, from 0 to 1. */
export interface PanelLevel {
  id: string
  level: number
}

/** The events the shell sends this page. */
export const STATE_EVENT = 'jatayu://panel/state'
export const LEVEL_EVENT = 'jatayu://panel/level'
export const THEME_EVENT = 'jatayu://panel/theme'

/** The stack as it is now, for a page that has only just loaded. */
export function panelState(): Promise<Panel[]> {
  return invoke<Panel[]>('panel_state')
}

/**
 * A button pressed, a box ticked or text submitted. `copied`: this page put
 * the button's text on the clipboard. `text`: what was typed, for a submit.
 */
export function panelAction(
  id: string,
  action: string,
  copied = false,
  text: string | null = null,
): Promise<void> {
  return invoke<void>('panel_action', { id, action, copied, text })
}

/** How tall the cards are, in CSS pixels, so the window can be that tall. */
export function panelResize(height: number): Promise<void> {
  return invoke<void>('panel_resize', { height })
}

/** The page wants the keyboard for a box on a card, or is done with it. */
export function panelFocus(want: boolean): Promise<void> {
  return invoke<void>('panel_focus', { want })
}

/** The card is being read (pointer on it, typing in it), or no longer: it does not go by itself while held. */
export function panelHold(id: string, held: boolean): Promise<void> {
  return invoke<void>('panel_hold', { id, held })
}

/** A press on a card's grip: move the window with the pointer. */
export function panelDrag(): Promise<void> {
  return invoke<void>('panel_drag')
}

/** Put the panel back in its corner. */
export function panelResetPlace(): Promise<void> {
  return invoke<void>('panel_reset_place')
}

/** The theme to draw in (resolved: never "system"), for a page that has only just loaded. */
export function panelTheme(): Promise<string | null> {
  return invoke<string | null>('panel_theme')
}

export function onPanelState(handler: (stack: Panel[]) => void): Promise<UnlistenFn> {
  return listen<Panel[]>(STATE_EVENT, (event) => handler(event.payload))
}

export function onPanelLevel(handler: (level: PanelLevel) => void): Promise<UnlistenFn> {
  return listen<PanelLevel>(LEVEL_EVENT, (event) => handler(event.payload))
}

export function onPanelTheme(handler: (theme: string | null) => void): Promise<UnlistenFn> {
  return listen<string | null>(THEME_EVENT, (event) => handler(event.payload))
}
