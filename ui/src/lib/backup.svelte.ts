// Settings → Backup: where sealed copies of this vault go, how often, how
// many are kept, and how the last one went.
//
// One read answers all of it -- `backup_settings` is the settings, the last
// run's outcome, the run in progress and when the next is due, together --
// and every write answers the same view, so the pane is always drawn from
// what the vault says rather than from what it asked for.
//
// # Following a backup while it runs
//
// "Back up now" returns at once and the copying goes on in the background,
// so the pane reads the view again once a second for as long as `running`
// is set -- the way the decision models' list is polled while one
// downloads -- and stops the moment it is not, the pane closes, or the vault
// locks. Nothing polls while nobody is looking.
//
// A run finishing raises a `settings` change, which `live.svelte.ts` routes
// to the status word. That change is also the only word a pane gets of a
// backup the *clock* started, or one finishing on another machine, so this
// store listens for it too -- see the constructor.

import { api } from './api'
import { registerApply } from './live-apply'
import { app, errorMessage, isLocked, quietly } from './state.svelte'
import type {
  BackupProgress,
  BackupSettings,
  BackupView,
  Destination,
  DestinationCheck,
  SnapshotInfo,
} from './types'

/** How often the view is read again while a backup runs. */
const POLL_MS = 1000

class BackupState {
  view = $state<BackupView | null>(null)
  /**
   * Why the last read failed, or `null`. Kept rather than swallowed, so the
   * pane can say why instead of sitting on "Loading…".
   */
  error = $state<string | null>(null)

  /** How many panes are open on this; polling and listening need one. */
  #watching = 0
  #timer: ReturnType<typeof setInterval> | null = null
  /** A read is on its way: the next tick waits rather than piling up. */
  #reading = false

  constructor() {
    // Where backups go is a credential's worth of trust -- a bucket, a
    // profile name -- and goes with the key, like everything else read out
    // of the vault.
    app.onLock(() => this.reset())
    // A `settings` change belongs to the status word (`RELOADS` in
    // `live.svelte.ts`), and this declines it, so that still reloads exactly
    // as it did. Seeing it first is how an open pane notices a backup
    // finishing that it did not start: one the schedule began, or one on
    // the machine holding the vault. Only while a pane is open -- a settings
    // change is every switch in Settings, and nobody else draws this.
    registerApply(
      'status',
      () => {
        if (this.#watching > 0) void this.refresh()
        return false
      },
      'backup',
    )
  }

  reset() {
    this.view = null
    this.error = null
    this.#stopPolling()
  }

  get settings(): BackupSettings | null {
    return this.view?.settings ?? null
  }

  /** The backup in progress, made by the process holding the vault. */
  get running(): BackupProgress | null {
    return this.view?.running ?? null
  }

  /**
   * Keep this fresh while a pane is open on it. Answers the function that
   * lets go, for `onMount` to call when the pane goes.
   */
  watch(): () => void {
    this.#watching += 1
    this.#poll()
    return () => {
      this.#watching = Math.max(0, this.#watching - 1)
      this.#poll()
    }
  }

  /** Read afresh. Answers whether it worked; the reason is `error`. */
  async refresh(): Promise<boolean> {
    try {
      this.#take(await api.backupSettings())
      this.error = null
      return true
    } catch (e) {
      if (!isLocked(e)) this.error = errorMessage(e)
      await quietly(e)
      return false
    }
  }

  async save(settings: BackupSettings) {
    this.#take(await api.saveBackupSettings(settings))
  }

  async setKeys(accessKeyId: string, secretAccessKey: string) {
    this.#take(await api.setBackupKeys(accessKeyId, secretAccessKey))
  }

  async clearKeys() {
    this.#take(await api.clearBackupKeys())
  }

  /** "Back up now". Rejects with `busy` when one is running already. */
  async run() {
    this.#take(await api.runBackup())
  }

  async cancel() {
    this.#take(await api.cancelBackup())
  }

  /** The copies kept at the saved destination, newest first. */
  snapshots(): Promise<SnapshotInfo[]> {
    return api.backupSnapshots()
  }

  /** Try a destination that has not been saved. Never rejects for one that
   *  does not work -- that is `ok: false` and a message. */
  check(destination: Destination): Promise<DestinationCheck> {
    return api.checkBackupDestination(destination)
  }

  #take(view: BackupView) {
    this.view = view
    this.#poll()
  }

  /** Start or stop the once-a-second read, to match what is on screen. */
  #poll() {
    const wanted = this.#watching > 0 && this.view?.running != null
    if (wanted && !this.#timer) {
      this.#timer = setInterval(() => void this.#tick(), POLL_MS)
    } else if (!wanted) {
      this.#stopPolling()
    }
  }

  async #tick() {
    if (this.#reading) return
    this.#reading = true
    try {
      await this.refresh()
    } finally {
      this.#reading = false
    }
  }

  #stopPolling() {
    if (this.#timer) clearInterval(this.#timer)
    this.#timer = null
  }
}

export const backup = new BackupState()
