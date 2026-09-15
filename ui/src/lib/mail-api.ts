// A thin wrapper around every mail command the interface calls.
//
// `api.ts` now carries the real generated commands -- every write, every
// draft call, sync, remote images and search all landed in `surface.json`
// while this app was being built against stand-in names. This file is what
// is left of that plan once the rewrite is done: singular, per-thread
// convenience over the real `threads: ThreadId[]` batch commands (nothing
// in the interface multi-selects yet), and two contracts that have not
// landed in Rust at all --
//
//   (i) invitations       -- `respond_to_invite`
//   (p) the Superhuman layer -- `set_thread_category`, `summarize_thread`
//
// each called by name through `callCommand` and marked with a `TODO(i)` or
// `TODO(p)` naming the contract, per `docs/plans/mail.md`'s "Mail for the
// assistant and MCP" and "The Superhuman layer". `mock.ts`/`mock-mail.ts`
// answer both today; when the real commands land, `gen-api.mjs` gives them
// a typed home in `api.ts` and the two functions below become one-line
// forwards to it, same as everything above them already is.

import { api, callCommand } from './api'
import type {
  AccountId,
  Draft,
  DraftId,
  MailAddress,
  MailboxId,
  MailCategory,
  MailMessageId,
  MailSyncProgress,
  Op,
  RemoteImageSettings,
  SearchMailResult,
  ThreadDetail,
  ThreadFilter,
  ThreadId,
  ThreadPage,
} from './types'

// ── Read: mailboxes, threads, accounts ─────────────────────────────────

export const listAccounts = () => api.accounts()
export const listMailboxes = (account: AccountId) => api.mailboxes(account)
export const listThreads = (
  mailbox: MailboxId,
  filter?: ThreadFilter,
  cursor?: string | null,
  limit?: number | null,
): Promise<ThreadPage> => api.threads(mailbox, filter, cursor, limit)
export const getThread = (id: ThreadId): Promise<ThreadDetail> => api.thread(id)

// ── Actions: one outbox op per thread, per "What an action does" ──────
//
// Singular here -- the store and every component select one thread at a
// time -- fanned out to the one-element array the real, batch-shaped
// command takes.

export const markRead = (id: ThreadId): Promise<Op[]> => api.markRead([id])
export const markUnread = (id: ThreadId): Promise<Op[]> => api.markUnread([id])
export const star = (id: ThreadId): Promise<Op[]> => api.star([id])
export const unstar = (id: ThreadId): Promise<Op[]> => api.unstar([id])
export const archive = (id: ThreadId): Promise<Op[]> => api.archive([id])
export const trash = (id: ThreadId): Promise<Op[]> => api.trash([id])
export const moveToMailbox = (id: ThreadId, mailbox: MailboxId): Promise<Op[]> =>
  api.moveToMailbox([id], mailbox)
export const label = (id: ThreadId, labelName: string): Promise<Op[]> => api.label([id], labelName)
export const unlabel = (id: ThreadId, labelName: string): Promise<Op[]> =>
  api.unlabel([id], labelName)
/** `until` is an ISO instant -- when the thread reappears. */
export const snooze = (id: ThreadId, until: string): Promise<Op[]> => api.snooze([id], until)
export const unsnooze = (id: ThreadId): Promise<void> => api.unsnooze([id])

// ── Drafts and sending ───────────────────────────────────────────────

export const newDraft = (opts: {
  account: AccountId
  inReplyTo?: MailMessageId
  forwardOf?: MailMessageId
  replyAll?: boolean
}): Promise<Draft> => api.newDraft(opts)

export const saveDraft = (draft: Draft): Promise<void> => api.saveDraft(draft)
export const discardDraft = (id: DraftId): Promise<void> => api.discardDraft(id)
/** `delaySeconds` is the undo window; absent uses the backend's own default. */
export const sendDraft = (id: DraftId, delaySeconds?: number): Promise<Draft> =>
  api.sendDraft(id, delaySeconds)
/** Only valid inside the undo window `sendDraft` opened. */
export const undoSend = (draftId: DraftId): Promise<Draft> => api.undoSend(draftId)
export const listDrafts = (account: AccountId): Promise<Draft[]> => api.drafts(account)

// ── Sync ─────────────────────────────────────────────────────────────

export const syncStatus = (): Promise<MailSyncProgress[]> => api.syncStatus()
/** Ask an account's task to sync now, out of its ordinary cadence. */
export const syncAccount = (id: AccountId): Promise<void> => api.syncAccount(id)

// ── Remote images ───────────────────────────────────────────────────

/** Allow remote images for a sender, a domain, or once for one message --
 *  name exactly one. See `mailview.ts`'s `loadBody` for where the answer
 *  ("images hidden?") is read back from. */
export const allowRemoteImages = (opts: {
  sender?: string
  domain?: string
  messageId?: MailMessageId
}): Promise<void> => api.allowRemoteImages(opts)
export const listRemoteImageAllowances = (): Promise<RemoteImageSettings> =>
  api.listRemoteImageAllowances()
export const revokeRemoteImageAllowance = (opts: {
  sender?: string
  domain?: string
}): Promise<void> => api.revokeRemoteImageAllowance(opts)

// ── Search and address autocomplete ────────────────────────────────────

/** Gmail-style operators (`from:`, `is:unread`, `has:attachment`, …),
 *  keyset-paged -- pass `next` back as `cursor` for the following page. */
export const searchMail = (
  query: string,
  accountIds?: AccountId[] | null,
  cursor?: string | null,
  limit?: number | null,
): Promise<SearchMailResult> => api.searchMail(query, accountIds, cursor, limit)

export const suggestAddresses = (prefix: string, limit?: number): Promise<MailAddress[]> =>
  api.suggestAddresses(prefix, limit)

// ── (i) Invitations ─────────────────────────────────────────────────
//
// TODO(i): `respond_to_invite { messageId, response, comment? } -> void` is
// the calendars-that-sign-in agent's command, landing alongside
// `MailMessage.invite` (`types.ts`'s `MailInvite`, phase 6 "Invitations in
// mail"). Neither is in `surface.json` yet, so this calls it by name; once
// it lands, this becomes `api.respondToInvite(...)` like everything above.

export const respondToInvite = (
  messageId: MailMessageId,
  response: 'accepted' | 'tentative' | 'declined',
  comment?: string,
): Promise<void> => callCommand<void>('respond_to_invite', { messageId, response, comment })

// ── (p) The Superhuman layer ────────────────────────────────────────
//
// TODO(p): `set_thread_category` and `summarize_thread` are the split-inbox
// agent's commands (`docs/plans/mail.md` phase 7); neither is in
// `surface.json` yet.

export const setThreadCategory = (threads: ThreadId[], category: MailCategory): Promise<void> =>
  callCommand<void>('set_thread_category', { threads, category })

export const summarizeThread = (id: ThreadId): Promise<{ summary: string }> =>
  callCommand<{ summary: string }>('summarize_thread', { id })
