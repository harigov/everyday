// Mail sorting: the categories a decision model sorts mail into, as somebody
// wrote them -- `jatayu_core::mail::sorting`.
//
// Read by Settings → Mail sorting, which edits them, and by the mail list,
// which draws the name of the category a thread was sorted into beside it.
// A thread carries the category's id (`Thread.sortedAs`), not its name, so a
// rename reaches every row already sorted, and a category deleted since is
// simply not drawn.
//
// The same view says whether follow-ups are running (`followUps`), which the
// Inbox's one-line notice and the settings pane both read -- and the notice
// can be dismissed, which is kept here, on this device.

import { api } from './api'
import { app, errorMessage, isLocked, quietly } from './state.svelte'
import type { FollowUpStatus, MailSorting, MailSortingView, SortCategory, Thread } from './types'

/**
 * Where the dismissed follow-up notice is kept, on this device. Written out
 * here rather than through `prefs.ts`, whose keys are a closed list of their
 * own -- as `listen.svelte.ts` does for its speed.
 */
const DISMISSED_KEY = 'jatayu.mail.followups.dismissed'

/**
 * A short fingerprint of a notice's reason, which is what is kept in place of
 * the reason: it names an account's address, and `localStorage` is not sealed
 * the way the vault is. FNV-1a, 32 bits -- this only has to tell one reason
 * from another, not to hide anything from somebody with the disk.
 */
function fingerprint(text: string): string {
  let h = 0x811c9dc5
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i)
    h = Math.imul(h, 0x01000193)
  }
  return (h >>> 0).toString(36)
}

/** How many dismissed reasons are kept, newest first: a reason for each
 *  account and one for All inboxes, with room to spare, so dismissing one
 *  does not bring another back. The oldest are forgotten past this. */
const DISMISSED_KEPT = 8

/**
 * The reasons dismissed so far, as `fingerprint` made them, newest first: a
 * JSON array under `DISMISSED_KEY`. The key first held one fingerprint on its
 * own, which is read as the only one, so a dismissal made before this
 * survives. Anything unreadable is nothing dismissed.
 */
function savedDismissals(): string[] {
  try {
    const raw = localStorage.getItem(DISMISSED_KEY)
    if (!raw) return []
    if (!raw.startsWith('[')) return [raw]
    const kept: unknown = JSON.parse(raw)
    if (!Array.isArray(kept)) return []
    return kept.filter((k): k is string => typeof k === 'string').slice(0, DISMISSED_KEPT)
  } catch {
    return []
  }
}

class SortingState {
  view = $state<MailSortingView | null>(null)
  error = $state<string | null>(null)

  /** Counts a command's own answer being put in `view`, so a `refresh` that
   *  was on its way meanwhile knows it is the older of the two. */
  #answered = 0

  /** The follow-up notice's reasons that were dismissed, newest first: kept
   *  across restarts, and only those -- a different reason shows again. */
  #dismissed = $state<string[]>(savedDismissals())

  constructor() {
    // The names are somebody's own words about their mail.
    app.onLock(() => {
      this.view = null
      this.error = null
    })
  }

  get sorting(): MailSorting | null {
    return this.view?.sorting ?? null
  }

  /** Whether follow-ups can run, on what, and how far each account got --
   *  `null` until the view has been read. */
  get followUps(): FollowUpStatus | null {
    return this.view?.followUps ?? null
  }

  /** Has the person dismissed the follow-up notice for this very reason? */
  stallDismissed(reason: string): boolean {
    return this.#dismissed.includes(fingerprint(reason))
  }

  /** Hide the follow-up notice while its reason is `reason`. */
  dismissStall(reason: string) {
    const kept = fingerprint(reason)
    this.#dismissed = [kept, ...this.#dismissed.filter((k) => k !== kept)].slice(0, DISMISSED_KEPT)
    try {
      localStorage.setItem(DISMISSED_KEY, JSON.stringify(this.#dismissed))
    } catch {
      // Not kept; this session still has it.
    }
  }

  /** The category `thread` was sorted into, if it still exists. */
  categoryOf(thread: Pick<Thread, 'sortedAs'>): SortCategory | null {
    const id = thread.sortedAs?.category
    if (!id) return null
    return this.sorting?.categories.find((c) => c.id === id) ?? null
  }

  /** Read once; quiet, because a row with no label is a fine answer. */
  async load() {
    if (this.view || !app.supportsMail) return
    await this.refresh()
  }

  /**
   * Read the view again, whether or not one is loaded -- for the follow-up
   * status, which moves as the background pass works, and for a running sort
   * of older mail. A copy a person is editing is the pane's own (it seeds a
   * draft from `sorting` once, and a refresh does not reseed it), so this can
   * be called as often as is useful. It does not put an older answer over a
   * newer one: a save or a sort that answered while this was on its way
   * already has the view as it is now.
   */
  async refresh(): Promise<boolean> {
    const before = this.#answered
    try {
      const view = await api.mailSorting()
      if (before === this.#answered) this.view = view
      this.error = null
      return true
    } catch (e) {
      if (!isLocked(e)) this.error = errorMessage(e)
      await quietly(e)
      return false
    }
  }

  async save(sorting: MailSorting) {
    this.#answer(await api.saveMailSorting(sorting))
  }

  async sortOlder(days: number | null, again: boolean) {
    this.#answer(await api.sortOlderMail(days, again))
  }

  async stopOlder(account?: string | null) {
    this.#answer(await api.stopSortingOlderMail(account))
  }

  #answer(view: MailSortingView) {
    this.#answered += 1
    this.view = view
  }
}

export const sorting = new SortingState()
