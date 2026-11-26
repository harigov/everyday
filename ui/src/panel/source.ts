// Where the page's cards come from: the shell, or -- in a development build in
// an ordinary browser, where there is no shell -- a few sample cards.
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
  panelAction,
  panelResize,
  panelState,
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
  /** A button pressed or a box ticked. */
  action(id: string, action: string, copied: boolean): Promise<void>
  /** How tall the cards are, in CSS pixels. */
  resize(height: number): Promise<void>
}

const shell: PanelSource = {
  state: panelState,
  onState: onPanelState,
  onLevel: onPanelLevel,
  action: panelAction,
  resize: panelResize,
}

/** In a development build with no shell to talk to: the sample cards in `mock.ts`. */
export const MOCK = import.meta.env.DEV && !inShell

export async function openSource(): Promise<PanelSource> {
  if (MOCK) return (await import('./mock')).mockSource(location.search)
  return shell
}
