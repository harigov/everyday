// What is on screen right now, and what the bar at the top searches.
//
// Two questions every app answers for itself through `apps.ts` -- see
// `AppModule.onScreen` and `AppModule.search` -- asked from the one module
// that may import every store to hand them over. `apps.ts` cannot (see its
// header on the import cycle that would close), and neither the composer
// nor the bar should have to know which stores exist.
//
// Settings is the one thing answered here rather than by an app: it is drawn
// where the open app would be, not over it, so while it is showing the app
// underneath is not on screen at all -- even though `app.section` still
// names it, so that closing Settings goes back there.

import { APPS, type AppDeps, type AppSearch } from './apps'
import { calendar } from './calendar.svelte'
import { library } from './library.svelte'
import { mail } from './mail.svelte'
import { notes } from './notes.svelte'
import type { Showing } from './onscreen'
import { overview } from './overview.svelte'
import { panels, type SettingsTab } from './panels.svelte'
import { app } from './state.svelte'
import { todo } from './todo.svelte'
import type { OnScreenApp } from './types'

const deps: AppDeps = { app, notes, todo, calendar, library, mail, overview }

/** Each Settings tab, as the model is told it. Not the drawn labels, which
 *  live with their icons in `SettingsView.svelte`; these only need to say
 *  which page it is. */
const SETTINGS_PAGES: Record<SettingsTab, string> = {
  general: 'the General tab',
  profile: 'the About You tab, their profile and roles',
  accounts: 'the Accounts tab',
  assistant: "the Assistant tab, the assistant's own settings",
  runs: 'the list of what the assistant did on its own',
  routines: "the assistant's routines",
  memory: 'what the assistant remembers about them',
  proposals: 'the proposals waiting for them to accept or decline',
  skills: "the assistant's skills",
  meetings: 'the Meetings tab',
  data: 'the Data tab, import and export',
  vault: 'the Vault tab',
}

/** Which app is in front of them, and what it shows. */
export function onScreenNow(): { app: OnScreenApp; showing: Showing } {
  if (panels.settings !== null) {
    return { app: 'settings', showing: { view: SETTINGS_PAGES[panels.settings] } }
  }
  return { app: app.section, showing: APPS[app.section].onScreen(deps) }
}

/** What the bar searches: the open app's own search, or `null` -- Settings
 *  included, which has nothing to search. */
export function searchNow(): AppSearch | null {
  if (panels.settings !== null) return null
  return APPS[app.section].search(deps)
}
