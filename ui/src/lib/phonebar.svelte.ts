// The apps along the bottom of a phone's screen: four of them, chosen by the
// person, with "More" beside them for the rest. Kept on this device rather
// than in the vault, as the sidebar's folds are: what goes in a phone's
// thumb's reach is a fact about the phone.

import { APP_ORDER, type Section } from './apps'
import { pref } from './prefs'

/** How many apps the bar holds beside "More". Five tabs is what a 360 px
 *  screen fits at a finger's width each. */
export const BAR_SIZE = 4

/** Home, the inbox, the day and the assistant: what a phone is reached for. */
export const DEFAULT_BAR: Section[] = ['overview', 'mail', 'calendar', 'assistant']

const barPref = pref<Section[]>(
  'jatayu.phone.bar',
  (raw) => {
    if (raw === null) return DEFAULT_BAR
    const list: unknown = JSON.parse(raw)
    if (!Array.isArray(list)) return DEFAULT_BAR
    const known = list.filter((s): s is Section => (APP_ORDER as readonly unknown[]).includes(s))
    return [...new Set(known)].slice(0, BAR_SIZE)
  },
  DEFAULT_BAR,
  (list) => JSON.stringify(list),
)

class PhoneBar {
  apps = $state<Section[]>(barPref.get())

  has(section: Section): boolean {
    return this.apps.includes(section)
  }

  /** On the bar if it is off, off if it is on. A full bar refuses another:
   *  the person takes one off first, which is a choice we should not make
   *  for them by dropping the oldest. */
  toggle(section: Section): boolean {
    if (this.has(section)) {
      this.apps = this.apps.filter((s) => s !== section)
    } else if (this.apps.length < BAR_SIZE) {
      this.apps = [...this.apps, section]
    } else {
      return false
    }
    barPref.set(this.apps)
    return true
  }

  reset() {
    this.apps = [...DEFAULT_BAR]
    barPref.set(this.apps)
  }
}

export const phoneBar = new PhoneBar()
