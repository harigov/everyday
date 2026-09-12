// Which window-level panel is open.
//
// Three pieces of state that belong to the window rather than to any one
// app, and that more than one thing needs to set: the app bar opens
// settings, so does a shortcut, and so does the assistant's own "not set up
// yet" screen. Passing a callback down three component trees to say so is
// how a boolean ends up existing twice.
//
// Deliberately not part of `app`: nothing here survives anything, touches
// the vault, or has to be cleared when it locks -- a dialog is closed by the
// lock screen appearing over it.

/**
 * The settings dialog's tabs.
 *
 * Just the type: nothing outside this file ever needed the list itself, only
 * the tab a caller is allowed to ask `openSettings` for, and keeping the
 * array around for that alone was dead weight the moment it stopped being
 * iterated anywhere.
 */
export type SettingsTab = 'general' | 'profile' | 'assistant' | 'data' | 'vault'

class Panels {
  /** Which settings tab is showing, or `null` when the dialog is closed. */
  settings = $state<SettingsTab | null>(null)
  /** The keyboard shortcut sheet. */
  shortcuts = $state(false)
  /**
   * The command palette.
   *
   * Its own flag rather than a mode of the shortcut sheet, though the two read
   * the same table. The sheet answers "what can I press"; the palette answers
   * "do this thing", and one of them is a reference and the other is a verb.
   */
  palette = $state(false)

  openSettings(tab: SettingsTab = 'general') {
    this.shortcuts = false
    this.palette = false
    this.settings = tab
  }

  openPalette() {
    this.shortcuts = false
    this.palette = true
  }

  closePalette() {
    this.palette = false
  }

  closeSettings() {
    this.settings = null
  }

  toggleShortcuts() {
    this.palette = false
    this.shortcuts = !this.shortcuts
  }

  /**
   * Set while the help sheet is asking which shortcuts apply.
   *
   * Without it the sheet is empty of everything interesting. Nearly every
   * binding is gated on "no dialog is over the window", the sheet *is* a
   * dialog, and so the one screen whose entire job is to list the shortcuts
   * was the one screen on which none of them applied. It listed the five
   * with a modifier and nothing else.
   *
   * Deliberately *not* `$state`, and that is not an optimisation. The sheet
   * raises this while it is rendering its rows, and writing reactive state
   * from inside a render is what Svelte answers with
   * `effect_update_depth_exceeded` -- which does not merely fail to draw the
   * sheet: it takes the whole reactive graph down with it, so every button
   * in the window stops responding until the app is reloaded. Nothing draws
   * from this flag, so nothing needs to be told when it moves.
   */
  listing = false

  /** Is anything modal on screen? What the shortcut handler asks. */
  get modal(): boolean {
    return this.settings !== null || (this.shortcuts && !this.listing)
  }
}

export const panels = new Panels()
