// Where the page's cards come from: the shell, or -- in a development build in
// an ordinary browser, where there is no shell -- sample cards that answer
// presses the way the shell's owners would.
//
// Decided at build time, the way `lib/api.ts` decides its `MOCK`:
// `import.meta.env.DEV` is a literal to Vite, so a release build evaluates
// `MOCK` to `false` and the sample cards are not merely unused but not shipped.
// The runtime check sits inside the dev guard on purpose. A release build that
// cannot reach its shell must show nothing, not cards that were never sent.
import { inShell } from '../lib/platform'
import {
  onPanelLevel,
  onPanelState,
  onPanelTheme,
  panelAction,
  panelDrag,
  panelFocus,
  panelHold,
  panelResetPlace,
  panelResize,
  panelState,
  panelTheme,
  type Panel,
  type PanelLevel,
} from './panel'

/** Everything the page needs from outside it, so the shell and the samples are interchangeable. */
export interface PanelSource {
  /** The stack as it is now, for a page that has only just loaded. */
  state(): Promise<Panel[]>
  /** Called with the whole stack on every change. Resolves to the way to stop listening. */
  onState(handler: (stack: Panel[]) => void): Promise<() => void>
  /** Called about twenty times a second per card with a meter. */
  onLevel(handler: (level: PanelLevel) => void): Promise<() => void>
  /** The theme the main window shows, resolved, or null when it has not said. */
  theme(): Promise<string | null>
  /** Called whenever the main window's theme changes. */
  onTheme(handler: (theme: string | null) => void): Promise<() => void>
  /** A button pressed, a box ticked, or `text` submitted from a box. */
  action(id: string, action: string, copied: boolean, text?: string | null): Promise<void>
  /** How tall the cards are, in CSS pixels. */
  resize(height: number): Promise<void>
  /** The page wants the keyboard for a box (`true`), or is done with it. */
  focus(want: boolean): Promise<void>
  /** The card is being read -- the pointer on it, typing in it -- or no longer. */
  hold(id: string, held: boolean): Promise<void>
  /** A press on a card's head: move the window with the pointer. */
  drag(): Promise<void>
  /** Put the panel back in its corner. */
  resetPlace(): Promise<void>
}

const shell: PanelSource = {
  state: panelState,
  onState: onPanelState,
  onLevel: onPanelLevel,
  theme: panelTheme,
  onTheme: onPanelTheme,
  action: panelAction,
  resize: panelResize,
  focus: panelFocus,
  hold: panelHold,
  drag: panelDrag,
  resetPlace: panelResetPlace,
}

/** In a development build with no shell to talk to: the sample cards in `mock.ts`. */
export const MOCK = import.meta.env.DEV && !inShell

export async function openSource(): Promise<PanelSource> {
  if (MOCK) return (await import('./mock')).mockSource(location.search)
  return shell
}
