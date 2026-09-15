// A thin wrapper around every mail command the interface calls.
//
// Three sync engine, write-command and body-protocol agents are landing
// their halves of `docs/plans/mail.md` in parallel, and none of their
// commands are in the generated client yet. Rather than sprinkle
// `callCommand('mark_read', {...})` through the store and the components,
// every call the interface needs is named here once, with the argument
// shape this build assumes written beside it. When a command lands for
// real and `gen-api.mjs` is re-run, the fix is a one-line edit to the
// function below that calls it -- nothing that calls *this* file changes.
//
// Four calls already exist in the generated surface --  `list_mailboxes`,
// `list_threads`, `get_thread`, `list_accounts` -- and are called through
// the typed `api` object, exactly as any other domain would. Everything
// else goes through `callCommand`, untyped, with a `// TODO(<agent>)` note
// naming whose branch is expected to add it for real:
//
//   (s) the sync engine       -- sync_status, sync_account
//   (a) writes, drafts, send  -- every thread action, every draft command
//   (b) the body/image proto  -- allow_remote_images, and `bodyDocument`'s
//                                real replacement in `mailview.ts`
//
// `search_mail` and `suggest_addresses` are Phase 4 work nobody has started;
// both are marked TODO(phase-4) rather than pinned to an agent.

import { api, callCommand, isMock } from './api'
// A static import, deliberately, though it is only ever read behind
// `isMock` below: `bodyDocument` has to stay synchronous to match the
// signature `mailview.ts` is expected to keep, and a dynamic `import()`
// cannot be. The cost is sixty threads of seed markup riding along in a
// production bundle for as long as this stand-in exists -- acceptable for
// something meant to be deleted the moment (b) lands the real thing, and
// nothing a bundler cannot tree-shake once it is.
import { mockMessageBodyHtml } from './mock-mail'
import type {
  AccountId,
  AccountView,
  Attachment,
  MailAddress,
  MailboxId,
  MailMessageId,
  ThreadDetail,
  ThreadFilter,
  ThreadId,
  ThreadPage,
} from './types'

// ── Already generated: mailboxes, threads, accounts ───────────────────────

export const listAccounts = (): Promise<AccountView[]> => api.accounts()
export const listMailboxes = (account: AccountId) => api.mailboxes(account)
export const listThreads = (
  mailbox: MailboxId,
  filter?: ThreadFilter,
  cursor?: string | null,
  limit?: number | null,
): Promise<ThreadPage> => api.threads(mailbox, filter, cursor, limit)
export const getThread = (id: ThreadId): Promise<ThreadDetail> => api.thread(id)

// ── Actions: one outbox op each, per "What an action does" in the plan ────
//
// Every one of these assumes the target is a thread id, because that is
// what the list and the open pane both select. A provider whose flags live
// on individual messages (most of them) fans a thread-level op out to every
// message in it server-side; nothing here needs to know that.

/** TODO(a): write commands, drafts and send. */
export const markRead = (id: ThreadId): Promise<void> => callCommand<void>('mark_read', { id })
/** TODO(a) */
export const markUnread = (id: ThreadId): Promise<void> => callCommand<void>('mark_unread', { id })
/** TODO(a) */
export const star = (id: ThreadId): Promise<void> => callCommand<void>('star', { id })
/** TODO(a) */
export const unstar = (id: ThreadId): Promise<void> => callCommand<void>('unstar', { id })
/** TODO(a) */
export const archive = (id: ThreadId): Promise<void> => callCommand<void>('archive', { id })
/** TODO(a) */
export const trash = (id: ThreadId): Promise<void> => callCommand<void>('trash', { id })
/** TODO(a) */
export const moveToMailbox = (id: ThreadId, mailbox: MailboxId): Promise<void> =>
  callCommand<void>('move_to_mailbox', { id, mailbox })
/** TODO(a) */
export const label = (id: ThreadId, label: string): Promise<void> =>
  callCommand<void>('label', { id, label })
/** TODO(a) */
export const unlabel = (id: ThreadId, label: string): Promise<void> =>
  callCommand<void>('unlabel', { id, label })
/** TODO(a). `until` is an ISO instant -- when the thread reappears. */
export const snooze = (id: ThreadId, until: string): Promise<void> =>
  callCommand<void>('snooze', { id, until })
/** TODO(a) */
export const unsnooze = (id: ThreadId): Promise<void> => callCommand<void>('unsnooze', { id })

// ── Drafts and sending ─────────────────────────────────────────────────

/**
 * A draft, as this build assumes it until agent (a)'s real record lands.
 *
 * Not in `types.ts`: nothing outside the compose sheet needs to know its
 * shape, and declaring it there would be a guess at another agent's schema
 * sitting in a file several stores read. Kept close to `docs/plans/mail.md`'s
 * `Draft` record, minus the fields the interface never touches directly
 * (`pack`, the outbox `op` pointer).
 */
export interface Draft {
  id: string
  accountId: AccountId
  identity?: string | null
  inReplyTo?: MailMessageId | null
  forwardOf?: MailMessageId | null
  to: MailAddress[]
  cc: MailAddress[]
  bcc: MailAddress[]
  subject: string
  bodyHtml: string
  attachments: Attachment[]
  origin: 'person' | 'assistant' | 'routine' | 'mcp'
  state: 'editing' | 'queued' | 'sent' | 'discarded'
  createdAt: string
  updatedAt: string
}

/** TODO(a). Mints a blank draft, prefilled the way reply/reply-all/forward
 *  need -- see `mail.ts`'s `replyAllRecipients` for the reply-all half of
 *  that, computed here rather than trusted to the backend since the
 *  interface has to show the recipients before the round trip returns. */
export const newDraft = (opts: {
  account: AccountId
  inReplyTo?: MailMessageId
  forwardOf?: MailMessageId
  replyAll?: boolean
}): Promise<Draft> => callCommand<Draft>('new_draft', opts)

/** TODO(a) */
export const saveDraft = (draft: Draft): Promise<void> => callCommand<void>('save_draft', { draft })
/** TODO(a) */
export const discardDraft = (id: string): Promise<void> =>
  callCommand<void>('discard_draft', { id })
/** TODO(a). `delaySeconds` is the undo window; absent sends at once. */
export const sendDraft = (id: string, delaySeconds?: number): Promise<void> =>
  callCommand<void>('send_draft', { id, delaySeconds })
/** TODO(a). Only valid inside the undo window `sendDraft` opened. */
export const undoSend = (draftId: string): Promise<void> =>
  callCommand<void>('undo_send', { draftId })
/** TODO(a) */
export const listDrafts = (): Promise<Draft[]> => callCommand<Draft[]>('list_drafts')

// ── Sync and images ─────────────────────────────────────────────────────

export interface SyncStatus {
  accountId: AccountId
  /** `null` once the first sync has finished; otherwise "how far in". */
  progress: { done: number; total: number } | null
  error: string | null
}

/** TODO(s): the sync engine. One row per account with a task running. */
export const syncStatus = (): Promise<SyncStatus[]> => callCommand<SyncStatus[]>('sync_status')
/** TODO(s). Ask an account's task to sync now, out of its ordinary cadence. */
export const syncAccount = (id: AccountId): Promise<void> =>
  callCommand<void>('sync_account', { id })

/**
 * Allow remote images for a sender, or once for one message.
 *
 * TODO(b): the body/image protocol. `forever: true` remembers the sender
 * account-wide (the settings screen's allow-list); `forever: false` fetches
 * once for this message only, matching the "Images hidden · Show · Always
 * from sender" bar the plan asks for.
 */
export const allowRemoteImages = (
  messageId: MailMessageId,
  opts: { forever: boolean },
): Promise<void> => callCommand<void>('allow_remote_images', { messageId, ...opts })

// ── Search and address autocomplete: Phase 4, nobody's started yet ────────

export interface MailSearchHit {
  threadId: ThreadId
  subject: string
  from: MailAddress
  snippet: string
  date: string
}

/** TODO(phase-4): the mail search index (`everyday-mailindex`). Until it
 *  exists, `mock-mail.ts`'s mock implementation does a simple substring
 *  search over the seeded threads. */
export const searchMail = (
  query: string,
  cursor?: string | null,
): Promise<{ hits: MailSearchHit[]; nextCursor: string | null }> =>
  callCommand<{ hits: MailSearchHit[]; nextCursor: string | null }>('search_mail', {
    query,
    cursor,
  })

/** TODO(phase-4): address autocomplete from headers, ranked by `frizbee`.
 *  The mock ranks by how often a seeded thread's messages use the address. */
export const suggestAddresses = (prefix: string): Promise<MailAddress[]> =>
  callCommand<MailAddress[]>('suggest_addresses', { prefix })

// ── Rendering a message body ───────────────────────────────────────────

/**
 * A `srcdoc` for one message's sandboxed `<iframe>`.
 *
 * The real thing is agent (b)'s `mailview.ts`, which builds this from a body
 * already sanitised once in Rust at sync time -- `lol_html` rewriting `src`
 * to `everyday://mail/img/…`, `ammonia` stripping everything with behaviour.
 * That file does not exist yet, so this is a same-named, same-signature
 * stand-in that builds a plausible `srcdoc` straight from the mock seed
 * data, entirely client-side. It is deliberately synchronous, matching the
 * signature `mailview.ts` is expected to keep: the real one reads an
 * already-decrypted, already-sanitised string out of the vault's row, which
 * costs nothing worth awaiting for either.
 *
 * Delete this function, and the `mock-mail` import it needs, the moment
 * `mailview.ts` lands -- every caller already imports `bodyDocument` from
 * here by name, so the only edit anywhere else is the import path.
 */
export function bodyDocument(messageId: MailMessageId): string {
  if (!isMock) {
    // Nothing to build from: the real protocol is not wired up yet in a
    // packaged build, and there is no mock data to fall back on outside dev.
    return srcdocFor('<p style="color:#888">This message has not been synced yet.</p>')
  }
  return srcdocFor(mockMessageBodyHtml(messageId))
}

/**
 * Wrap a body fragment as a full HTML document with its own CSP.
 *
 * Mirrors what the real sync-time sanitiser is expected to hand back
 * (`docs/plans/mail.md`, "Rendering a message"): a `<meta>` CSP that allows
 * no script and no network of its own, belt-and-braces beside the iframe's
 * `sandbox` attribute which is the wall that actually matters -- see
 * `MailThread.svelte` for why `allow-scripts` is never added even here.
 */
/**
 * Guess a message's rendered height in pixels, from its HTML *string* --
 * never by touching what the iframe renders.
 *
 * `MailThread.svelte` sizes each reused `<iframe sandbox>` from this before
 * the first paint. The plan asks for a `ResizeObserver`-sized frame, and the
 * honest way to read that here is: a `ResizeObserver` earns its keep on the
 * *container*, reacting to the column getting narrower or wider as the
 * assistant's rail opens and closes, which changes how many characters fit
 * a line and so how tall the same text needs to be. It cannot tell us the
 * *content* height, because that would mean reading out of the iframe --
 * `contentDocument`, `scrollHeight` -- which needs `allow-same-origin`, and
 * the sandbox on this iframe is deliberately `allow-popups
 * allow-popups-to-escape-sandbox` and nothing else. `allow-same-origin`
 * without `allow-scripts` is arguably safe on its own -- there is no script
 * in there to abuse the access -- but "arguably safe" is a harder property
 * to keep true forever than "never granted", and the second one is what
 * this app promises in its no-telemetry, no-surprises voice.
 *
 * So this is an estimate, not a measurement: strip the tags, count the
 * characters, and divide by a plausible line length at the width the reader
 * actually has. It is wrong on the first paint of anything unusual -- a
 * table, a deeply nested quote, an image that has not loaded -- and the
 * container underneath keeps `overflow-y: auto` for exactly that case: the
 * frame never clips a message, it occasionally scrolls one a few pixels
 * more than it needed to. That is the trade-off, stated plainly rather than
 * hidden behind a permission this app said it would not ask for.
 */
export function estimateBodyHeight(bodyHtml: string, widthPx: number): number {
  const text = bodyHtml
    .replace(/<[^>]*>/g, ' ')
    .replace(/\s+/g, ' ')
    .trim()
  const charsPerLine = Math.max(20, Math.floor(widthPx / 8.2))
  const lines = Math.max(3, Math.ceil(text.length / charsPerLine))
  const blocks = (bodyHtml.match(/<(p|blockquote|div|li)[ >]/gi) ?? []).length
  const lineHeightPx = 21
  const blockGapPx = 12
  return Math.min(2400, lines * lineHeightPx + blocks * blockGapPx + 24)
}

function srcdocFor(bodyHtml: string): string {
  return (
    `<!doctype html><html><head><meta charset="utf-8">` +
    `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data: everyday:; style-src 'unsafe-inline'">` +
    `<style>body{font:14px/1.5 -apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;color:#1a1a1a;margin:0;padding:2px 4px;word-wrap:break-word}` +
    `img{max-width:100%}a{color:#2563eb}blockquote{margin:0.6em 0;padding-left:0.8em;border-left:2px solid #d0d0d0;color:#555}</style>` +
    `</head><body>${bodyHtml}</body></html>`
  )
}
