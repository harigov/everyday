// The panel's half of its contract with the shell, and the only file of the
// panel's page that names a shell command: `crates/karmayoga-app/src/acl.rs`
// reads this file to check the window's capability grants exactly what the
// page calls. Mirrors `crates/karmayoga-app/src/panel/mod.rs` field for field.
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

/** Which part of the shell a card belongs to. */
export type PanelOwner = 'meeting' | 'voiceTyping' | 'talk'

/** What a card is doing: its accent colour and the mark beside its kicker. */
export type PanelTone = 'neutral' | 'listening' | 'working' | 'success' | 'attention' | 'trouble'

/** A line of speech, or of anything else said in turn. */
export interface PanelLine {
  who?: string
  text: string
  /** Still coming: drawn with a caret. */
  pending: boolean
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
  /** Text to put on the clipboard when pressed, before reporting the press. */
  copy?: string
}

/** One card. The shell sends the whole stack on every change. */
export interface Panel {
  id: string
  owner: PanelOwner
  tone: PanelTone
  kicker: string
  title: string
  count?: string
  body?: string
  lines?: PanelLine[]
  meter: boolean
  sections?: PanelSection[]
  actions?: PanelAction[]
  busy: boolean
}

/** How loud the microphone is for one card, from 0 to 1. */
export interface PanelLevel {
  id: string
  level: number
}

/** The events the shell sends this page. */
export const STATE_EVENT = 'karmayoga://panel/state'
export const LEVEL_EVENT = 'karmayoga://panel/level'

/** The stack as it is now, for a page that has only just loaded. */
export function panelState(): Promise<Panel[]> {
  return invoke<Panel[]>('panel_state')
}

/** A button pressed or a box ticked. `copied`: this page put the button's text on the clipboard. */
export function panelAction(id: string, action: string, copied = false): Promise<void> {
  return invoke<void>('panel_action', { id, action, copied })
}

/** How tall the cards are, in CSS pixels, so the window can be that tall. */
export function panelResize(height: number): Promise<void> {
  return invoke<void>('panel_resize', { height })
}

export function onPanelState(handler: (stack: Panel[]) => void): Promise<UnlistenFn> {
  return listen<Panel[]>(STATE_EVENT, (event) => handler(event.payload))
}

export function onPanelLevel(handler: (level: PanelLevel) => void): Promise<UnlistenFn> {
  return listen<PanelLevel>(LEVEL_EVENT, (event) => handler(event.payload))
}
