// Which window-level panel is open.
//
// Three pieces of state that belong to the window rather than to any one
// app, and that more than one thing needs to set: the app bar opens
// settings, so does a shortcut, and so does the assistant's own "not set up
// yet" screen. Passing a callback down three component trees to say so is
// how a boolean ends up existing twice.
//
// Deliberately not part of `app`: nothing here survives anything or touches
// the vault. Settings *does* have to be cleared when the vault locks, though
// -- it used to be a dialog that simply unmounted along with everything else
// behind the lock screen, but it is a page drawn in the panes now, the way
// any other app is (see `App.svelte`), and relying on an incidental unmount
// to also forget which tab was open is how somebody unlocks and lands back
// in Settings instead of whatever they were doing before. `App.svelte`
// clears it explicitly with `app.onLock`.

/**
 * The settings page's tabs.
 *
 * Just the type: nothing outside this file ever needed the list itself, only
 * the tab a caller is allowed to ask `openSettings` for, and keeping the
 * array around for that alone was dead weight the moment it stopped being
 * iterated anywhere.
 */
export type SettingsTab =
  | 'general'
  | 'profile'
  | 'accounts'
  | 'assistant'
  // The assistant's standing work, drawn under its own tab -- see
  // `assistant.svelte.ts`'s `Pane`, which is these four by name.
  | 'runs'
  | 'routines'
  | 'memory'
  | 'proposals'
  // Not one of `assistant.svelte.ts`'s four panes -- skills are managed only
  // from Settings, never drawn in the Assistant app's own nav -- so it gets
  // its own branch in `SettingsView.svelte` rather than joining `Pane`.
  | 'skills'
  | 'meetings'
  | 'data'
  | 'vault'

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

  /**
   * Is anything modal on screen?
   *
   * Settings does not count any more -- it is a page, not a dialog, so it
   * must not stop the app-switching keys or the palette the way a real
   * dialog does. Only the shortcut sheet is left here, and only while it is
   * not the one asking this question itself (`panels.listing`); see
   * `shortcuts.svelte.ts`'s own `dialogOpen`, which is what the shortcut
   * handler actually asks, for the rest of what counts -- the confirmation
   * sheets and the like that this store has never known about.
   */
  get modal(): boolean {
    return this.shortcuts && !this.listing
  }
}

export const panels = new Panels()
