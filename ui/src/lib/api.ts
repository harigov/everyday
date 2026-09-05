// The single boundary between the interface and the Rust core.
//
// Everything the UI can do to a vault goes through here, which means the
// mock backend below is a complete substitute: `EVERYDAY_MOCK=1 npm run dev`
// runs the entire interface in a browser with no Rust, no vault and no
// native dependencies. That is what makes the design workable on its own.

import type {
  Bootstrap,
  Entry,
  EntryId,
  EntryQuery,
  EntrySummary,
  Journal,
  JournalId,
  SearchHit,
  VaultStatus,
} from './types'
import { VaultError } from './types'

// Decided at BUILD time, not run time.
//
// `import.meta.env.DEV` is substituted with a literal by Vite, so a
// production bundle evaluates this to `false` and the mock module below is
// statically eliminated -- it is not merely unused, it is not shipped.
//
// The runtime check is deliberately *inside* the dev guard. A release build
// that cannot reach Tauri must fail loudly, not quietly serve a fake journal
// with sample entries in it: for a journal app, that failure mode is
// indistinguishable from having lost everything.
const MOCK = import.meta.env.DEV && !('__TAURI_INTERNALS__' in window)

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>

let invoke: Invoke = async () => {
  throw new VaultError(
    'unavailable',
    'Every Day could not reach its storage backend. Your journal has not been ' +
      'touched; this is a problem with the application, not with your data.',
  )
}

if (!MOCK) {
  const mod = await import('@tauri-apps/api/core')
  invoke = async <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
    try {
      return await mod.invoke<T>(cmd, args)
    } catch (raw) {
      // Commands reject with `{ code, message }`; anything else is a bug in
      // the bridge and should surface as-is rather than be swallowed.
      if (raw && typeof raw === 'object' && 'code' in raw && 'message' in raw) {
        throw new VaultError(String(raw.code), String(raw.message))
      }
      throw new VaultError('unknown', String(raw))
    }
  }
} else {
  const { mockInvoke } = await import('./mock')
  invoke = mockInvoke
}

export const isMock = MOCK

export const api = {
  bootstrap: () => invoke<Bootstrap>('bootstrap'),

  createVault: (opts: {
    path: string
    name: string
    backend: string
    password: string | null
  }) => invoke<VaultStatus>('create_vault', opts),

  openVault: (path: string) => invoke<VaultStatus>('open_vault', { path }),
  unlock: (password: string) => invoke<VaultStatus>('unlock', { password }),
  lock: () => invoke<VaultStatus>('lock'),
  status: () => invoke<VaultStatus>('status'),
  changePassword: (current: string, next: string) =>
    invoke<void>('change_password', { current, next }),
  setAutoLock: (seconds: number) => invoke<void>('set_auto_lock', { seconds }),
  /** Defers the idle auto-lock; called on real user interaction. */
  touch: () => invoke<void>('touch'),
  /** Returns true if the vault locked itself. Polled on a timer. */
  pollAutoLock: () => invoke<boolean>('poll_auto_lock'),

  journals: () => invoke<Journal[]>('list_journals'),
  newJournal: (name: string) => invoke<Journal>('new_journal', { name }),
  saveJournal: (journal: Journal) => invoke<void>('save_journal', { journal }),
  deleteJournal: (id: JournalId) => invoke<void>('delete_journal', { id }),

  entries: (query: EntryQuery) => invoke<EntrySummary[]>('list_entries', { query }),
  entry: (id: EntryId) => invoke<Entry>('get_entry', { id }),
  newEntry: (journalId: JournalId) => invoke<Entry>('new_entry', { journalId }),
  saveEntry: (entry: Entry) => invoke<void>('save_entry', { entry }),
  deleteEntry: (id: EntryId) => invoke<void>('delete_entry', { id }),

  search: (query: string, journalId: JournalId | null, limit: number) =>
    invoke<SearchHit[]>('search', { query, journalId, limit }),

  /** Import a file the user dropped or picked; returns its content address. */
  putBlob: (bytes: Uint8Array) =>
    invoke<string>('put_blob', { bytes: Array.from(bytes) }),

  /** All tags in use, most frequent first. */
  tags: () => invoke<string[]>('list_tags'),
}

/**
 * URL for an attachment.
 *
 * Media is served by a custom protocol handler in the Rust shell rather than
 * as a data: URL, so a 400 MB video streams and seeks instead of being
 * base64-encoded into the document.
 */
export function mediaUrl(blob: string): string {
  if (MOCK) return mockMediaUrl(blob)
  // Tauri maps custom schemes to an http origin on Windows and Android.
  const base =
    navigator.userAgent.includes('Windows') || navigator.userAgent.includes('Android')
      ? 'http://everyday.localhost'
      : 'everyday://localhost'
  return `${base}/${blob}`
}

let mockMediaUrl: (blob: string) => string = () => ''
if (MOCK) {
  const { mockMediaUrl: f } = await import('./mock')
  mockMediaUrl = f
}
