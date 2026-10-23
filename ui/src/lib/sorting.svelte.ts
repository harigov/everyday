// Mail sorting: the categories a decision model sorts mail into, as somebody
// wrote them -- `everyday_core::mail::sorting`.
//
// Read by Settings → Mail sorting, which edits them, and by the mail list,
// which draws the name of the category a thread was sorted into beside it.
// A thread carries the category's id (`Thread.sortedAs`), not its name, so a
// rename reaches every row already sorted, and a category deleted since is
// simply not drawn.

import { api } from './api'
import { app, errorMessage, isLocked, quietly } from './state.svelte'
import type { MailSorting, MailSortingView, SortCategory, Thread } from './types'

class SortingState {
  view = $state<MailSortingView | null>(null)
  error = $state<string | null>(null)

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

  async refresh(): Promise<boolean> {
    try {
      this.view = await api.mailSorting()
      this.error = null
      return true
    } catch (e) {
      if (!isLocked(e)) this.error = errorMessage(e)
      await quietly(e)
      return false
    }
  }

  async save(sorting: MailSorting) {
    this.view = await api.saveMailSorting(sorting)
  }

  async sortOlder(days: number | null, again: boolean) {
    this.view = await api.sortOlderMail(days, again)
  }

  async stopOlder(account?: string | null) {
    this.view = await api.stopSortingOlderMail(account)
  }
}

export const sorting = new SortingState()
