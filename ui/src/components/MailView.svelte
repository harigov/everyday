<script lang="ts">
  // The Mail app's main pane: the thread list, `VirtualList`-backed so a
  // hundred-thousand-message mailbox costs the same DOM as a hundred, and
  // the reading pane beside it -- the journal's `EntryList` + `Editor` shape,
  // folded into one component the way the library and the todo app do.

  import { onMount, untrack } from 'svelte'
  import { userAgentMac } from '../lib/platform'
  import { keysLabel } from '../lib/keys'
  import { accounts } from '../lib/accounts.svelte'
  import { accountColor } from '../lib/avatar'
  import {
    CATEGORY_TABS,
    MAIL_ONLY,
    categoryTabCount,
    followUpPill,
    followUpScope,
    followUpStall,
    isSnoozedMailbox,
    isUnifiedMailbox,
    mailboxDisplayName,
    newestSyncedAt,
    mailboxEmptyCopy,
    onlyEmptyLead,
    recentActionLine,
    snippetText,
    snoozedUntilLabel,
    syncInProgress,
    threadListDate,
    threadPeople,
    threadRowDate,
    unifiedMailboxId,
    weekdayAndTime,
    searchPlace,
    withDateSections,
    withFollowUpSections,
    type ListRow,
    type MailOnly,
  } from '../lib/mail'
  import { listening } from '../lib/listen.svelte'
  import { mail } from '../lib/mail.svelte'
  import * as mailApi from '../lib/mail-api'
  import { notify } from '../lib/notify.svelte'
  import { openExternal } from '../lib/open-external'
  import { panels } from '../lib/panels.svelte'
  import { closesOnBack, details, phone } from '../lib/phone.svelte'
  import { sorting } from '../lib/sorting.svelte'
  import { app, handle } from '../lib/state.svelte'
  import {
    askedLine,
    manyDetail,
    needsAttention,
    offerDetail,
    summarize,
    unsubscribeLabel,
  } from '../lib/unsubscribe'
  import { todo } from '../lib/todo.svelte'
  import { calendar } from '../lib/calendar.svelte'
  import { notes } from '../lib/notes.svelte'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { compactCount, plural, relativeTime } from '../lib/format'
  import { todayIso } from '../lib/time'
  import type { IconName } from '../lib/icons'
  import type {
    AccountId,
    FollowUpKind,
    Mailbox,
    MailAddress,
    MailCategory,
    MailMessageDetail,
    Thread,
    ThreadDetail,
    ThreadId,
    UnsubscribeOffer,
    UnsubscribeOutcome,
  } from '../lib/types'
  import ChoiceDialog from './ChoiceDialog.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import EmptyState from './EmptyState.svelte'
  import Loading from './Loading.svelte'
  import Icon from './Icon.svelte'
  import MailCleanup from './MailCleanup.svelte'
  import MailCompose from './MailCompose.svelte'
  import MailFollowUpBanner from './MailFollowUpBanner.svelte'
  import MailLabelPicker from './MailLabelPicker.svelte'
  import MailScheduled from './MailScheduled.svelte'
  import MailOutbox from './MailOutbox.svelte'
  import MailSnoozePicker from './MailSnoozePicker.svelte'
  import MailThread from './MailThread.svelte'
  import MailThreadDrafts from './MailThreadDrafts.svelte'
  import PaneResizer from './PaneResizer.svelte'
  import PhoneBack from './PhoneBack.svelte'
  import UnsubscribeResults from './UnsubscribeResults.svelte'
  import VirtualList from './VirtualList.svelte'

  void mail.start()
  // What arrived in the open thread while nobody could see it is read once
  // somebody can: see `mail.seeOpenThread`.
  onMount(() => {
    mail.setOnScreen(true)
    return () => mail.setOnScreen(false)
  })
  // The names of the categories a decision model sorted threads into, for
  // the label a sorted row carries.
  void sorting.load()

  /** Is the list column one of the "All accounts" rows -- every account's
   *  threads in one list, so each row says whose it is. */
  const unifiedView = $derived(isUnifiedMailbox(mail.mailbox))

  const heading = $derived(
    mail.sendsView
      ? mail.sendsView.list === 'scheduled'
        ? 'Scheduled'
        : mail.sendsView.account === null
          ? 'Outbox, all accounts'
          : 'Outbox'
      : mail.mailbox
        ? unifiedView && mail.mailbox.id !== unifiedMailboxId('inbox')
          ? // "All inboxes" already says it; "Sent" alone would read as one
            // account's Sent.
            `${mailboxDisplayName(mail.mailbox)}, all accounts`
          : mailboxDisplayName(mail.mailbox)
        : 'Mail',
  )

  const mailAccounts = $derived(accounts.list.filter((a) => a.services.mail))

  /** Every mail account, in the order their dots take `accountColor`'s
   *  palette -- the order Settings and the sidebar list them in. */
  const accountIds = $derived(mailAccounts.map((a) => a.id))

  /** The line under the heading: how much of what is listed is unread, from
   *  the same count the sidebar row shows -- nothing until that has loaded,
   *  rather than a "0 unread" that is not yet true. */
  const unreadLine = $derived.by(() => {
    const sends = mail.sendsView
    if (sends?.list === 'scheduled') {
      const waiting = mail.scheduled.filter((s) => s.draft.accountId === sends.account).length
      return waiting > 0 ? `${waiting.toLocaleString()} waiting to send` : ''
    }
    if (sends?.list === 'outbox') {
      const stuck = mail.outbox.filter(
        (i) => sends.account === null || i.draft.accountId === sends.account,
      ).length
      return stuck > 0 ? `${stuck.toLocaleString()} not sent` : ''
    }
    const box = mail.mailbox
    const unread = box ? mail.unreadCounts.get(box.id) : undefined
    if (unread === undefined) return ''
    return unread > 0 ? `${unread.toLocaleString()} unread` : 'Nothing unread'
  })

  /** Each category chip's mark. `user` for Important, which is mail a
   *  person wrote to you; `more` for whatever the rules could not place. */
  const TAB_ICONS: Record<MailCategory | 'all', IconName> = {
    all: 'inbox',
    priority: 'flag',
    important: 'user',
    other: 'more',
    newsletter: 'newspaper',
    notification: 'bell',
  }

  const ONLY_ICONS: Record<MailOnly, IconName> = {
    unread: 'mail',
    starred: 'star',
    attachments: 'paperclip',
  }

  /** What a follow-up row's pill means, for its title. */
  const WAITING: Record<FollowUpKind, string> = {
    reply: 'Waiting on your reply',
    nudge: 'Nobody has answered your last message',
  }

  /**
   * The number on a tab: its conversations, or with Unread on, the unread
   * ones. `null` with another toggle on, since `category_counts` does not
   * know how many of a tab's threads are starred, and a total beside a
   * narrowed list reads as how many it holds.
   */
  function tabNumber(count: { threads: number; unread: number }): number | null {
    if (mail.only.size === 0) return count.threads
    return mail.only.size === 1 && mail.only.has('unread') ? count.unread : null
  }

  /** The account a list row belongs to, as compactly as it can be named. */
  function accountLabel(id: AccountId): string {
    const account = accounts.account(id)
    return account ? account.displayName || account.address : ''
  }

  /**
   * The one account the list column is about, or `null` for every one --
   * an "All accounts" row, or nothing open yet. What Sync now syncs and
   * what Quick cleanup looks through. The Scheduled list is one account's,
   * and is checked first: `selectedMailbox` stays set underneath it.
   */
  const scopeAccount = $derived<AccountId | null>(
    mail.sendsView
      ? mail.sendsView.account
      : mail.mailbox && !unifiedView
        ? mail.mailbox.accountId
        : null,
  )
  const syncScope = $derived<AccountId[]>(
    scopeAccount ? [scopeAccount] : mailAccounts.map((a) => a.id),
  )

  const syncing = $derived(
    mail.syncRequested ||
      mail.syncStatus.some((s) => syncScope.includes(s.accountId) && syncInProgress(s)),
  )
  const syncTitle = $derived.by(() => {
    if (syncing) return 'Syncing…'
    const at = newestSyncedAt(syncScope, mail.syncStatus, accounts.list)
    return at ? `Sync now · last synced ${relativeTime(at)}` : 'Sync now'
  })

  // A slow status read for as long as Mail is on screen, so the "last
  // synced" in Sync now's title stays true while nothing is syncing --
  // `MailNav`'s own two-second poll only runs while something is.
  $effect(() => {
    const timer = setInterval(() => void mail.refreshSyncStatus(), 30_000)
    return () => clearInterval(timer)
  })

  let cleaning = $state(false)
  /** What kind of mail Quick cleanup opens on: everything, or -- from the
   *  Newsletters tab's bar -- that tab's (`'tab:newsletter'`). */
  let cleaningKind = $state<string | undefined>(undefined)

  /** Open Quick cleanup, on one kind of mail or on all of it. */
  function openCleanup(kind?: string) {
    cleaningKind = kind
    cleaning = true
  }

  /** Where the Newsletters tab's bar remembers being put away, per device. */
  const NEWSLETTER_BAR_KEY = 'jatayu.mail.newsletterBarHidden'
  function readBarHidden(): boolean {
    try {
      return localStorage.getItem(NEWSLETTER_BAR_KEY) === '1'
    } catch {
      return false
    }
  }
  let newsletterBarHidden = $state(readBarHidden())
  const NEWSLETTER_TIP =
    "Too many newsletters? Unsubscribe from their senders all at once. Some aren't newsletters? Select them and choose Not newsletters."
  function hideNewsletterBar() {
    newsletterBarHidden = true
    try {
      localStorage.setItem(NEWSLETTER_BAR_KEY, '1')
    } catch {
      // Storage refused (a private window): put away for this visit only.
    }
  }

  /** What the platform calls the key Ctrl+click is held with. */
  const modKey = userAgentMac ? '⌘' : 'Ctrl'

  /** Is the list column currently the Snoozed view -- the one place a row's
   *  own date column means "comes back" rather than "last arrived", and the
   *  one place "Unsnooze" belongs in a row's own menu (an ordinary mailbox
   *  never lists a currently-snoozed thread at all, per `mail.refresh`'s own
   *  `snoozed` filter). */
  const inSnoozedView = $derived(isSnoozedMailbox(mail.mailbox))

  /** The follow-ups drawn over an Inbox's own rows, for the tab and toggles
   *  on -- nothing for any other list, search included: see
   *  `mail.followUpRows`. */
  const followUpRows = $derived(mail.followUpRows)
  const followUpCount = $derived(followUpRows.reply.length + followUpRows.nudge.length)

  /** The list column's own rows, headings interleaved, an Inbox's follow-ups
   *  in sections of their own above the dates -- never grouped in the
   *  Snoozed view, whose rows already read "comes back", not "arrived",
   *  which `dateSection`'s own headings have nothing to say about. */
  const listRows = $derived<ListRow[]>(
    inSnoozedView
      ? mail.threads.map((thread) => ({ type: 'thread', thread }))
      : withFollowUpSections(followUpRows, mail.threads, (t) => t.lastDate),
  )
  /** Nothing to draw at all: no rows of the mailbox's own, and nothing
   *  waiting at the top -- which a tab with no mail of its own can still
   *  have, in a nudge. */
  const listEmpty = $derived(mail.threads.length === 0 && followUpCount === 0)
  const searchRows = $derived<ListRow[]>(withDateSections(mail.searchResults, (t) => t.lastDate))

  /**
   * Why follow-ups cannot run for the list on screen, or `null`: an Inbox
   * (the one `followUpScope` is asked for, so not a search, not Snoozed, not
   * a list of sends) with follow-ups switched on that nothing is looking
   * for -- no model to ask, or this account (every account, under All
   * inboxes) not set up for it. Running and finding nothing is not this: the
   * empty top of the list is then the answer. `sorting.view` is read with
   * `load`, so there is nothing until it has been.
   */
  const followUpsStalled = $derived.by(() => {
    if (mail.sendsView || mail.searchQuery.trim()) return null
    // Follow-ups on the list say for themselves that something is running;
    // a notice above them saying nothing is would only argue with them.
    if (followUpCount > 0) return null
    const scope = followUpScope(mail.mailbox)
    if (!scope) return null
    const on = sorting.sorting?.followUp?.on !== false
    return followUpStall(sorting.followUps, on, scope.account)
  })
  /** The same, unless the person hid the notice for this very reason -- a
   *  different one shows it again. */
  const followUpsNotice = $derived(
    followUpsStalled !== null && !sorting.stallDismissed(followUpsStalled)
      ? followUpsStalled
      : null,
  )

  // The notice is as true as the last read of the status, which nothing else
  // here repeats: so look again every minute while there is one to be wrong,
  // and as Settings closes, which is where it is put right.
  $effect(() => {
    if (followUpsStalled === null) return
    const timer = setInterval(() => void sorting.refresh(), 60_000)
    return () => clearInterval(timer)
  })
  let settingsWasOpen = false
  $effect(() => {
    const open = panels.settings !== null
    if (settingsWasOpen && !open) void sorting.refresh()
    settingsWasOpen = open
  })

  function rowKey(row: ListRow): string {
    return row.type === 'header' ? `header:${row.key}` : row.thread.id
  }

  /** A thread row's thread. Spelled out rather than read as `row.thread`
   *  under the template's `{:else}`: the type-aware lint cannot carry the
   *  `row.type === 'header'` narrowing into an `{:else}` branch, and reads
   *  every field of an un-narrowed `row.thread` as an unsafe `any`. */
  function threadOf(row: Extract<ListRow, { type: 'thread' }>): Thread {
    return row.thread
  }

  /** Which follow-up section a thread row is drawn in, if it is in one --
   *  spelled out for the same reason `threadOf` is. */
  function followUpOf(row: Extract<ListRow, { type: 'thread' }>): FollowUpKind | null {
    return row.followUp ?? null
  }

  /** (p) TODO: only the inbox has a split to tab through -- see
   *  `docs/plans/mail.md`'s "Split inbox". Every other mailbox (Sent,
   *  Archive, a label) shows everything in it, uncategorised or not. */
  const showTabs = $derived(mail.mailbox?.role === 'inbox')

  /** The Newsletters tab's tip: over that tab, not over a search, and not
   *  once it has been put away on this device. */
  const newsletterTipShown = $derived(
    showTabs && mail.category === 'newsletter' && !mail.searchQuery.trim() && !newsletterBarHidden,
  )

  const openAccount = $derived(
    mail.openThread ? accounts.account(mail.openThread.thread.accountId) : undefined,
  )

  /** The lines worth showing under the open thread's subject -- non-person
   *  origins and failures, per `recentActionLine`'s own rule; oldest of the
   *  handful the backend sends last, so the most recent reads first. */
  const recentActionLines = $derived(
    (mail.openThread?.recentActions ?? [])
      .map(recentActionLine)
      .filter((l): l is string => l !== null),
  )
  /** (p) TODO: `summarize_thread` stays out of reach until the account's
   *  own `mailAi.summaries` switch is on -- see `mail-api.ts`'s TODO(p). */
  const canSummarize = $derived(openAccount?.mailAi?.summaries === true)
  /** Listening is gated as summaries are -- the same mail goes to the same
   *  model -- and needs a speaker on this computer. */
  const canListen = $derived(canSummarize && listening.available)
  /** Whether the open thread is the one being read aloud now. */
  const listeningToOpen = $derived(
    mail.openThread
      ? listening.isReading({ type: 'mailThread', threadId: mail.openThread.thread.id })
      : false,
  )

  /** Start reading the open thread, or stop doing so when it already is. */
  function toggleListen() {
    const thread = mail.openThread?.thread
    if (!thread) return
    if (listeningToOpen) listening.stop()
    else
      void listening.start(
        { type: 'mailThread', threadId: thread.id },
        thread.subject || '(no subject)',
      )
  }
  /** The open thread's drafts, but the one being written below, which is the
   *  composer itself rather than a card. */
  const draftCards = $derived(
    (mail.openThread?.drafts ?? []).filter((d) => !d.draft || d.draft.id !== mail.composing?.id),
  )

  function categoryMoveItems(ids: ThreadId[], current: MailCategory | null): MenuItem[] {
    return CATEGORY_TABS.map((tab) => ({
      label: tab.label,
      checked: current === tab.key,
      run: () => void mail.moveToCategory(ids, tab.key),
    }))
  }

  /** Where mail filed as a newsletter by mistake usually belongs: the tabs a
   *  "Not a newsletter" offers, in the strip's own order. Priority is left
   *  to its own "Always priority from…", and Newsletters is where it is. */
  const NOT_NEWSLETTER_TABS = CATEGORY_TABS.filter(
    (tab) => tab.key === 'important' || tab.key === 'other' || tab.key === 'notification',
  )

  /** "Not a newsletter"'s choices, each `prefix` + the tab's name. */
  function notNewsletterItems(ids: ThreadId[], prefix = ''): MenuItem[] {
    return NOT_NEWSLETTER_TABS.map((tab) => ({
      label: `${prefix}${tab.label}`,
      icon: TAB_ICONS[tab.key],
      run: () => void refile(ids, tab.key, tab.label),
    }))
  }

  /**
   * Move threads out of Newsletters, and say so. It is a rule for each
   * sender rather than a move of these threads alone (`set_thread_category`
   * files all their mail), which the toast says, since otherwise their
   * other newsletters vanishing from the tab would be a surprise.
   */
  async function refile(ids: ThreadId[], category: MailCategory, label: string) {
    const count = new Set(ids).size
    if (!(await mail.moveToCategory(ids, category))) return
    notify.success(
      count === 1 ? `Moved to ${label}` : `Moved ${plural(count, 'conversation')} to ${label}`,
      {
        key: 'mail-refile',
        body:
          count === 1
            ? 'New mail from this sender goes there too.'
            : 'New mail from their senders goes there too.',
      },
    )
  }

  function moveMailboxItems(ids: ThreadId[], accountId: string): MenuItem[] {
    return mail.mailboxes
      .filter((m) => m.accountId === accountId && m.role !== 'other')
      .map((box: Mailbox) => ({
        label: mailboxDisplayName(box),
        run: () => void mail.moveManyTo(ids, box.id),
      }))
  }

  /**
   * Who "Always priority from …" would mean, for this row's own menu: the
   * last participant who is not one of the account's own addresses --
   * `Thread.participants` carries no per-message order the way
   * `set_thread_category`'s own "the thread's last message's sender" reads,
   * but a thread grows this array in the order a new voice first joins it,
   * so the last one standing once this account's own addresses are read out
   * is, in the ordinary two-person case, exactly that sender. `null` when
   * every participant is this account's own -- a thread with nobody else in
   * it, which the VIP rule has nothing to point at.
   */
  function vipSenderFor(t: Thread): MailAddress | null {
    const mine = accounts.ownAddresses(t.accountId)
    const other = [...t.participants].reverse().find((p) => !mine.has(p.email.toLowerCase()))
    return other ?? t.participants.at(-1) ?? null
  }

  /** A row's date column. Snoozed rows have no heading to name the day, and say
   *  when they come back. */
  function rowDate(t: Thread): string {
    if (!inSnoozedView) return threadRowDate(t.lastDate)
    return t.snoozedUntil ? snoozedUntilLabel(t.snoozedUntil) : threadListDate(t.lastDate)
  }

  function rowMenu(t: Thread): MenuItem[] {
    const vip = vipSenderFor(t)
    return tidyMenu([
      { label: 'Open', icon: 'inbox', run: () => void mail.openThreadById(t.id) },
      // No Ctrl-click on a phone: a long press picks the first, and a tap
      // picks each one after it -- see `clickRow`.
      phone.on && { label: 'Select', icon: 'check', run: () => mail.toggleChecked(t.id) },
      // First after Open on a follow-up: being done with what it is waiting
      // on is the one thing its place at the top asks of somebody.
      t.followUp && {
        label: 'Not needed',
        icon: 'tick',
        run: () => void mail.dismissFollowUp(t.id),
      },
      {
        label: t.unreadCount > 0 ? 'Mark read' : 'Mark unread',
        icon: 'check',
        run: () => void (t.unreadCount > 0 ? mail.markRead(t.id) : mail.markUnread(t.id)),
      },
      { label: t.starred ? 'Unstar' : 'Star', icon: 'star', run: () => void mail.toggleStar(t.id) },
      {
        label: t.category === 'priority' ? 'Remove from priority' : 'Mark as priority',
        icon: 'flag',
        run: () => void mail.setPriority(t.id, t.category !== 'priority'),
      },
      vip && {
        label: `Always priority from ${vip.name || vip.email}`,
        icon: 'flag',
        run: () => void mail.setCategoryFor(t.id, 'priority'),
      },
      inSnoozedView
        ? { label: 'Unsnooze', icon: 'clock', run: () => void mail.unsnooze(t.id) }
        : { label: 'Snooze…', icon: 'clock', run: () => (mail.wantsSnooze = [t.id]) },
      { label: 'Label…', icon: 'tag', run: () => (mail.wantsLabel = [t.id]) },
      { label: 'Move to…', icon: 'layers', items: moveMailboxItems([t.id], t.accountId) },
      {
        label: 'Move to category',
        icon: 'inbox',
        items: categoryMoveItems([t.id], t.category ?? null),
      },
      t.category === 'newsletter' && {
        label: 'Not a newsletter',
        icon: 'newspaper',
        items: notNewsletterItems([t.id]),
      },
      SEP,
      unsubscribable && {
        label: 'Unsubscribe…',
        icon: 'mail-x',
        run: () => (mail.wantsUnsubscribe = [t.id]),
      },
      { label: 'Archive', icon: 'layers', run: () => void mail.archive(t.id) },
      ...deleteItems([t.id]),
    ])
  }

  /** Is there anybody else's mail to unsubscribe from in this list? Not in
   *  Sent or Drafts, whose threads are the person's own words. */
  const unsubscribable = $derived(mail.mailbox?.role !== 'sent' && mail.mailbox?.role !== 'drafts')

  /**
   * The end of a row's menu and the bulk one: to Trash, and -- in Trash or
   * Spam -- for good. Trash itself offers only the second, since trashing
   * what is already there changes nothing.
   */
  function deleteItems(ids: ThreadId[]): (MenuItem | false)[] {
    const role = mail.emptiableRole
    const box = mail.mailbox
    return [
      role !== 'trash' && {
        label: 'Trash',
        icon: 'trash',
        danger: true,
        run: () => void mail.trashMany(ids),
      },
      role !== null &&
        !!box && {
          label: 'Delete forever…',
          icon: 'trash',
          danger: true,
          run: () => (mail.wantsForever = { ids, mailbox: box }),
        },
    ]
  }

  function confirmForever() {
    // Copied out before clearing: the dialog's own reading of it is reactive.
    const ask = mail.wantsForever
    mail.wantsForever = null
    if (ask) void mail.deleteForeverMany(ask.ids, ask.mailbox)
  }

  /** "every account's Trash", or the one account's address it is in. */
  function emptyScope(box: Mailbox): string {
    if (isUnifiedMailbox(box)) return `every account's ${mailboxDisplayName(box)}`
    const address = accounts.account(box.accountId)?.address
    return address ? `${mailboxDisplayName(box)} for ${address}` : mailboxDisplayName(box)
  }

  async function confirmEmpty() {
    const box = mail.wantsEmpty
    mail.wantsEmpty = null
    if (!box) return
    const count = await mail.emptyMailbox(box)
    if (count === null) return
    const name = mailboxDisplayName(box)
    if (count === 0) notify.success(`${name} was already empty`)
    else notify.success(`Deleted ${plural(count, 'conversation')} for good`, { body: name })
  }

  /** The picked threads, in list order -- see `mail.checked`. The follow-ups
   *  at the top count: a nudge is in no other list to be found in. */
  const pickedThreads = $derived(mail.shownThreads.filter((t) => mail.checked.has(t.id)))

  /** Everything the bulk pane and a picked row's own menu offer, for every
   *  picked thread at once. Move to… only lists one account's folders, so
   *  it is left out when the picked threads span two accounts. */
  function bulkMenu(): MenuItem[] {
    const ids = mail.targets
    const accounts = new Set(pickedThreads.map((t) => t.accountId))
    const anyUnread = pickedThreads.some((t) => t.unreadCount > 0)
    const allStarred = pickedThreads.every((t) => t.starred)
    const allPriority = pickedThreads.every((t) => t.category === 'priority')
    return tidyMenu([
      {
        label: anyUnread ? 'Mark read' : 'Mark unread',
        icon: 'check',
        run: () => void (anyUnread ? mail.markReadMany(ids) : mail.markUnreadMany(ids)),
      },
      {
        label: allStarred ? 'Unstar' : 'Star',
        icon: 'star',
        run: () => void mail.toggleStarMany(ids),
      },
      {
        label: allPriority ? 'Remove from priority' : 'Mark as priority',
        icon: 'flag',
        run: () => void mail.setPriorityMany(ids, !allPriority),
      },
      inSnoozedView
        ? { label: 'Unsnooze', icon: 'clock', run: () => void mail.unsnoozeMany(ids) }
        : { label: 'Snooze…', icon: 'clock', run: () => (mail.wantsSnooze = ids) },
      { label: 'Label…', icon: 'tag', run: () => (mail.wantsLabel = ids) },
      accounts.size === 1 && {
        label: 'Move to…',
        icon: 'layers',
        items: moveMailboxItems(ids, [...accounts][0]!),
      },
      { label: 'Move to category', icon: 'inbox', items: categoryMoveItems(ids, null) },
      pickedNewsletters && {
        label: 'Not newsletters',
        icon: 'newspaper',
        items: notNewsletterItems(ids),
      },
      SEP,
      unsubscribable && {
        label: 'Unsubscribe…',
        icon: 'mail-x',
        run: () => (mail.wantsUnsubscribe = ids),
      },
      { label: 'Archive', icon: 'layers', run: () => void mail.archiveMany(ids) },
      ...deleteItems(ids),
    ])
  }

  /** Are the picked threads newsletters -- the Newsletters tab's, or every
   *  one of them anywhere else? What offers "Not newsletters" for them. */
  const pickedNewsletters = $derived(
    (showTabs && mail.category === 'newsletter' && !mail.searchQuery.trim()) ||
      (pickedThreads.length > 0 && pickedThreads.every((t) => t.category === 'newsletter')),
  )
  /** Is any picked thread the kind of mail one unsubscribes from? Then the
   *  bulk bar offers it as a button, not only under More. */
  const pickedBulkMail = $derived(
    pickedThreads.some((t) => t.category === 'newsletter' || t.category === 'notification'),
  )

  /** A row's own menu -- the bulk one when it is one of several picked. */
  function menuFor(t: Thread): MenuItem[] {
    return mail.checked.has(t.id) && mail.checked.size > 1 ? bulkMenu() : rowMenu(t)
  }

  /** A row clicked: Shift and Ctrl/Cmd pick, a plain click opens -- see
   *  `mail.clickThread`. */
  function clickRow(e: MouseEvent, id: ThreadId) {
    if (phone.on) {
      // With some picked, a tap picks or puts back; with none, it opens --
      // and the open thread is on screen at once, before it has loaded.
      if (mail.checked.size > 0) {
        mail.toggleChecked(id)
        return
      }
      opening = id
    }
    mail.clickThread(id, { shift: e.shiftKey, toggle: e.ctrlKey || e.metaKey })
  }

  /** Shift+click would otherwise also select the text of every row between
   *  the two -- the browser's own range selection, not ours. */
  function noTextRange(e: MouseEvent) {
    if (e.shiftKey) e.preventDefault()
  }

  /** Scrolls an inline reply into view the moment it mounts -- it draws
   *  itself right after the thread's own messages, which may already be
   *  scrolled well past the fold by the time a reply opens under them.
   *  Paired with `{#key mail.composeKey}` in the template, so opening a
   *  *second* reply into the same thread (send, then reply again) remounts
   *  the element and this runs again, rather than firing only once per
   *  thread. */
  function scrollIntoViewOnMount(el: HTMLElement) {
    // A phone's reply is a screen of its own, not something under the thread.
    if (phone.on) return
    el.scrollIntoView({ behavior: 'smooth', block: 'nearest' })
  }

  function snooze(at: Date) {
    const ids = mail.wantsSnooze
    mail.wantsSnooze = null
    if (ids) void mail.snoozeMany(ids, at)
  }

  function applyLabel(labelName: string) {
    const ids = mail.wantsLabel
    mail.wantsLabel = null
    if (ids) void mail.labelMany(ids, labelName)
  }

  // ── Unsubscribing ────────────────────────────────────────────────────
  //
  // Always asked first, since it writes to somebody in the person's name and
  // cannot be taken back: one conversation's sender, with what their mail
  // offers said in words (`offerDetail`), or the senders of several. The
  // question is raised from the toolbar here, or through
  // `mail.wantsUnsubscribe` by a row's menu, the bulk bar and Mod+U.

  /** The question on screen: one sender, from what their mail offers, or the
   *  senders of several conversations -- counted, and named when they turn
   *  out to be one. */
  type UnsubscribeAsk =
    | { kind: 'one'; ids: ThreadId[]; offer: UnsubscribeOffer }
    | { kind: 'many'; ids: ThreadId[]; senders: number; name: string | null }
  let unsubscribeAsk = $state.raw<UnsubscribeAsk | null>(null)
  /** What is left once it is done -- pages to open, senders it could not
   *  reach -- for `UnsubscribeResults`. */
  let unsubscribeLeft = $state.raw<readonly UnsubscribeOutcome[] | null>(null)

  // Taken off the store at once, so the same request is never asked twice,
  // and turned into the question here.
  $effect(() => {
    const ids = mail.wantsUnsubscribe
    if (ids === null) return
    untrack(() => {
      mail.wantsUnsubscribe = null
      void askToUnsubscribe(ids)
    })
  })

  /**
   * Who sent these conversations, as far as their rows tell: each thread's
   * sender is the last voice in it that is not the account's own, as for
   * "Always priority from…". A thread with no row on screen counts as a
   * sender of its own -- more said than fewer.
   */
  function sendersOf(ids: readonly ThreadId[]): { senders: number; name: string | null } {
    const rows = new Map(mail.shownThreads.map((t): [ThreadId, Thread] => [t.id, t]))
    const named = new Map<string, MailAddress>()
    let unknown = 0
    for (const id of ids) {
      const row = rows.get(id)
      const who = row ? vipSenderFor(row) : null
      if (!row || !who) unknown += 1
      else named.set(`${row.accountId}\n${who.email.toLowerCase()}`, who)
    }
    const [only] = named.values()
    const senders = named.size + unknown
    return { senders, name: senders === 1 && only ? only.name || only.email : null }
  }

  /** Raise the question for `ids`: what one conversation's sender offers,
   *  read from their mail first, or the many-senders one. */
  async function askToUnsubscribe(ids: readonly ThreadId[]) {
    const unique = [...new Set(ids)]
    const [only] = unique
    if (only === undefined) return
    if (unique.length > 1) {
      unsubscribeAsk = { kind: 'many', ids: unique, ...sendersOf(unique) }
      return
    }
    let offer = mail.openThread?.thread.id === only ? mail.openOffer : null
    if (!offer) {
      try {
        offer = await mailApi.unsubscribeOffer(only)
      } catch (e) {
        await handle(e)
        return
      }
    }
    if (!offer) {
      notify.info('This conversation names no way to unsubscribe.', { key: 'mail-unsubscribe' })
      return
    }
    unsubscribeAsk = { kind: 'one', ids: [only], offer }
  }

  /** The toolbar's Unsubscribe: the open thread's own offer, already read. */
  function unsubscribeOpen(id: ThreadId, offer: UnsubscribeOffer) {
    unsubscribeAsk = { kind: 'one', ids: [id], offer }
  }

  /** The question's words and answers, for `ChoiceDialog`. The narrower
   *  answer first, the one that also archives after it, as that dialog
   *  stacks them. */
  const unsubscribeDialog = $derived.by(() => {
    const ask = unsubscribeAsk
    if (!ask) return null
    if (ask.kind === 'one') {
      const { ids, offer } = ask
      const who = offer.name || offer.email
      const web = offer.via === 'web'
      return {
        title: offer.askedAt ? `Unsubscribe from ${who} again?` : `Unsubscribe from ${who}?`,
        detail: [askedLine(offer.askedAt), offerDetail(offer)].filter(Boolean).join('. '),
        options: [
          {
            label: web ? 'Open their page' : unsubscribeLabel(offer.via),
            run: () => void unsubscribeOne(ids, offer, false),
          },
          {
            label: web ? 'Open their page and archive' : 'Unsubscribe and archive',
            run: () => void unsubscribeOne(ids, offer, true),
          },
        ],
      }
    }
    const { ids, senders, name } = ask
    return {
      title: name
        ? `Unsubscribe from ${name}?`
        : `Unsubscribe from the senders of ${plural(ids.length, 'conversation')}?`,
      detail: manyDetail(senders),
      options: [
        { label: 'Unsubscribe only', run: () => void unsubscribeMany(ids, false) },
        { label: 'Unsubscribe and archive', run: () => void unsubscribeMany(ids, true) },
      ],
    }
  })

  /**
   * One sender, as their offer said. A page is opened in the browser as the
   * answer promised -- shown to copy when the browser will not take it --
   * and anything else is said in a toast, with what is left to do after.
   */
  async function unsubscribeOne(ids: ThreadId[], offer: UnsubscribeOffer, archive: boolean) {
    unsubscribeAsk = null
    const who = offer.name || offer.email
    const pending =
      offer.via === 'web'
        ? null
        : notify.info(`Unsubscribing from ${who}…`, { key: 'mail-unsubscribe', timeout: null })
    const outcomes = await mail.unsubscribeThreads(ids, { archive, senders: [offer.email] })
    if (pending !== null) notify.dismiss(pending)
    const [page] = outcomes
    // Opened straight away only when a page was the way out the question
    // named. After a refused one-click request it is listed instead: the
    // question said nothing would open.
    const promised = offer.via === 'web'
    if (promised && outcomes.length === 1 && page && !page.done && page.url) {
      if (await openExternal(page.url)) {
        notify.info('Their page is open in your browser', {
          key: 'mail-unsubscribe',
          body: 'Finish unsubscribing there.',
        })
      } else {
        unsubscribeLeft = [page]
      }
      return
    }
    report(outcomes)
  }

  /** The senders of several conversations at once, then what came of it. */
  async function unsubscribeMany(ids: ThreadId[], archive: boolean) {
    unsubscribeAsk = null
    const pending = notify.info('Unsubscribing…', { key: 'mail-unsubscribe', timeout: null })
    const outcomes = await mail.unsubscribeThreads(ids, { archive })
    notify.dismiss(pending)
    // Done with, so let go -- unless the ask failed, when the same pick is
    // there to try again.
    if (outcomes.length > 0) mail.clearChecked()
    report(outcomes)
  }

  /** How it went, in a toast, and what is left in a sheet. Nothing when the
   *  ask itself failed: that has been said already. */
  function report(outcomes: readonly UnsubscribeOutcome[]) {
    if (outcomes.length === 0) return
    const { title, body, ok } = summarize(outcomes)
    const opts = { key: 'mail-unsubscribe', body }
    if (ok) notify.success(title, opts)
    else notify.warn(title, opts)
    if (needsAttention(outcomes)) unsubscribeLeft = outcomes
  }

  /** Is the reading pane there at all? Only with one thread open, or a new
   *  message being written in it. With nothing open, or with threads picked
   *  -- whose actions are a bar over the list instead -- the list takes the
   *  whole width, and its rows go to one line with more of each message. */
  const paneOpen = $derived(
    (mail.composing !== null && !mail.composeInline) ||
      (mail.checked.size === 0 && mail.openThread !== null),
  )

  // ── On a phone ───────────────────────────────────────────────────────
  //
  // There is room for the list or for what was opened from it, never both:
  // see `details` in `lib/phone.svelte.ts`. The reading pane -- a thread, or
  // a new message -- is then drawn over the whole list (which stays where it
  // is, scrolled as it was, only hidden), and back puts it away.

  /** The row tapped and not yet loaded: the thread's screen opens at once,
   *  with its way back, rather than the tap doing nothing for as long as
   *  the thread takes to arrive. */
  let opening = $state<ThreadId | null>(null)
  $effect(() => {
    if (opening !== null && (mail.openThread !== null || mail.selectedThread !== opening)) {
      opening = null
    }
  })

  /** Is the reading pane over the list? Always `false` beside it. */
  const phoneDetail = $derived(
    phone.on &&
      (paneOpen ||
        (opening !== null &&
          mail.checked.size === 0 &&
          mail.selectedThread === opening &&
          mail.openThread === null)),
  )

  // `details` is what `App.svelte`'s back reads, so it says so when the
  // pane opens and when it goes. Back (Android's, or the browser's) closes
  // it from there: a flag that went false with the pane still up is a back.
  $effect(() => {
    if (!phone.on) return
    if (phoneDetail) details.open('mail')
    else details.close('mail')
  })
  let detailWasOpen = false
  $effect(() => {
    const open = details.isOpen('mail')
    if (detailWasOpen && !open && untrack(() => phoneDetail)) closeDetail()
    detailWasOpen = open
  })

  /** The writing screen, to close it as its own close button does -- the
   *  draft kept, a blank one thrown away. */
  let composeSheet: { close(): void } | undefined = $state()

  /** Put the pane away: the message being written, kept as a draft, or the
   *  thread. */
  function closeDetail() {
    if (mail.composing && !mail.composeInline) {
      if (composeSheet) composeSheet.close()
      else mail.closeCompose()
    } else mail.closeThread()
  }

  // A reply or a forward is a screen of its own over the thread; back puts
  // it away -- kept in the thread as a draft -- and shows the thread again.
  closesOnBack(
    () => phone.on && mail.composing !== null && mail.composeInline,
    () => (composeSheet ? composeSheet.close() : mail.closeCompose()),
  )

  /**
   * Mail into the other apps, through each one's own public door: a task
   * in the inbox, an event as the calendar's own new-event
   * draft, a note as a note. Only offered where the vault carries the app.
   */
  function bridgeItems(
    open: ThreadDetail,
    newest: MailMessageDetail | undefined,
  ): (MenuItem | false)[] {
    const subject = open.thread.subject.trim() || '(no subject)'
    const own = accounts.ownAddresses(open.thread.accountId)
    // Who it is from, for a note made of it: the newest message not sent by
    // the person themself.
    const from = [...open.messages]
      .reverse()
      .find((m) => !own.has(m.from.email) && !own.has(m.from.email.toLowerCase()))?.from
    return [
      app.canShow('todo') && {
        label: 'Make a task',
        icon: 'check',
        run: async () => {
          // The subject as written, into the inbox with no date: see
          // `todo.addFrom`. Not "raise with" the sender -- that makes it a
          // talking point for the next meeting with them, which a task from
          // a newsletter or an invoice is not.
          const task = await todo.addFrom(subject)
          if (!task) return
          notify.info('Task added', {
            key: 'mail-task',
            action: {
              label: 'Open',
              run: () => {
                void app.goTo('todo').then((went) => {
                  if (went) void todo.open(task.id)
                })
              },
            },
          })
        },
      },
      app.canShow('calendar') && {
        label: 'Add to calendar',
        icon: 'calendar',
        run: async () => {
          if (!(await app.goTo('calendar'))) return
          await calendar.newEvent({
            day: todayIso(),
            // The next whole hour, kept inside today.
            startMinutes: Math.min((new Date().getHours() + 1) * 60, 23 * 60),
            minutes: 60,
            title: subject,
          })
        },
      },
      app.canShow('notes') && {
        label: 'Save to a note',
        icon: 'pencil',
        run: async () => {
          const lines = [
            from ? `From ${from.name ? `${from.name} <${from.email}>` : from.email}` : '',
            newest ? snippetText(newest.snippet) : '',
          ].filter(Boolean)
          // Made before the Notes app is on screen, so its editor opens
          // already holding the text rather than being told afterwards.
          await notes.create(subject)
          if (notes.open && lines.length > 0) {
            notes.open.body = {
              type: 'doc',
              content: lines.map((text) => ({
                type: 'paragraph',
                content: [{ type: 'text', text }],
              })),
            }
            notes.edited()
          }
          await app.goTo('notes')
        },
      },
    ]
  }

  /** What a thread's toolbar has no room for on a phone. */
  function threadMenu(open: ThreadDetail, newest: MailMessageDetail | undefined): MenuItem[] {
    const priority = open.thread.category === 'priority'
    const offer = mail.openOffer
    return tidyMenu([
      newest && {
        label: 'Reply all',
        icon: 'reply-all',
        run: () => void mail.reply(newest.id, true),
      },
      newest && { label: 'Forward', icon: 'forward', run: () => void mail.forward(newest.id) },
      SEP,
      {
        label: priority ? 'Remove from priority' : 'Mark as priority',
        icon: 'flag',
        run: () => void mail.setPriority(open.thread.id, !priority),
      },
      {
        label: 'Mark unread',
        icon: 'mail',
        run: () => void mail.markUnread(open.thread.id),
      },
      { label: 'Label…', icon: 'tag', run: () => (mail.wantsLabel = [open.thread.id]) },
      !!offer && {
        label: offer.askedAt ? 'Unsubscribed' : 'Unsubscribe',
        icon: 'mail-x',
        run: () => unsubscribeOpen(open.thread.id, offer),
      },
      SEP,
      ...bridgeItems(open, newest),
      SEP,
      canListen && {
        label: listeningToOpen ? 'Stop reading aloud' : 'Listen',
        icon: listeningToOpen ? 'stop' : 'play',
        run: toggleListen,
      },
      canSummarize && {
        label: 'Summarise',
        icon: 'sparkle',
        disabled: mail.summarizing,
        run: () => void mail.summarizeOpenThread(),
      },
    ])
  }

  /** The category chips, which on a phone are a strip that scrolls sideways
   *  when the open one's name is longer than the room it is left. */
  let tabStrip: HTMLElement | undefined = $state()
  $effect(() => {
    void mail.category
    const strip = tabStrip
    const on = strip?.querySelector<HTMLElement>('[aria-selected="true"]')
    if (!strip || !on || !phone.on) return
    // After the chip has taken its name, so its width is the one to fit.
    const room = strip.getBoundingClientRect()
    const chip = on.getBoundingClientRect()
    if (chip.left < room.left) strip.scrollLeft += chip.left - room.left - 8
    else if (chip.right > room.right) strip.scrollLeft += chip.right - room.right + 8
  })

  /** The mailbox's own list, to bring the open thread's row back into view
   *  once the list has narrowed under it -- see `showSelected`. */
  let threadList: { scrollToIndex(index: number): void } | undefined = $state()

  /** Scroll the selected row into view. Opening a thread from the full-width
   *  list narrows it, its one-line rows go to two, and a row clicked near
   *  the bottom would end up below the fold; `VirtualList` only scrolls to
   *  its selection when the selection changes, which was before that. */
  function showSelected() {
    const at = listRows.findIndex((row) => rowKey(row) === mail.selectedThread)
    if (at >= 0) threadList?.scrollToIndex(at)
  }

  const still = window.matchMedia('(prefers-reduced-motion: reduce)')
  // With no motion the pane is there at once: bring the row back as soon as
  // the narrowed list has measured its rows.
  $effect(() => {
    if (paneOpen && still.matches) requestAnimationFrame(() => requestAnimationFrame(showSelected))
  })

  /**
   * The reading pane coming and going, one bidirectional transition with `t`
   * as how open it is, so a close begun mid-open turns back from where it
   * is rather than starting over.
   *
   * The pane is pinned at its full size the whole time and only uncovered,
   * from its left edge, while the list beside it narrows by exactly as much
   * -- so the list's edge is the pane's, frame for frame, and the thread
   * inside never lays itself out at an in-between width (each mail body is
   * a frame that measures its own height whenever its width moves). Both
   * are driven from this one `tick` rather than a CSS transition on each,
   * which could drift a frame apart. The list's in-between width is a
   * `flex-grow` between 0 (its own width) and 1 (all of it): a lone item
   * growing by less than 1 takes that share of the free space, so the width
   * moves in step with `t`. A dragged width is never animated, since only
   * this moves it.
   *
   * The list's contents are laid out once, at the width the list is going
   * to, and the list's own box clips them while it moves (`.sliding`):
   * otherwise every frame would lay out every rendered row again, and the
   * rows crossing the one-line/two-line width halfway would all be
   * measured again by the virtual list.
   */
  function slide(node: HTMLElement) {
    // A phone's pane is over the list rather than beside it: nothing for
    // the list to give way to, so it is simply there.
    if (phone.on) return { duration: 0 }
    const list = node.parentElement?.querySelector<HTMLElement>(':scope > .list')
    const box = node.parentElement
    /** Which way the list's contents were last laid out for. */
    let laidFor: boolean | null = null
    return {
      duration: still.matches ? 0 : 260,
      easing: (t: number) => 1 - Math.pow(1 - t, 4),
      tick: (t: number) => {
        if (!list || !box) return
        // Fully open, or gone: CSS has it from here.
        if (t === 1 || (t === 0 && !paneOpen)) {
          node.style.clipPath = ''
          list.style.flexGrow = ''
          list.style.removeProperty('--slide-w')
          list.classList.remove('sliding')
          laidFor = null
          if (t === 1) showSelected()
          return
        }
        if (laidFor !== paneOpen) {
          laidFor = paneOpen
          const to = paneOpen ? box.clientWidth - node.offsetWidth : box.clientWidth
          list.style.setProperty('--slide-w', `${to}px`)
          list.classList.add('sliding')
        }
        node.style.clipPath = `inset(0 0 0 ${(1 - t) * 100}%)`
        list.style.flexGrow = String(1 - t)
      },
    }
  }
</script>

<svelte:window onfocus={() => mail.seeOpenThread()} />
<svelte:document onvisibilitychange={() => mail.seeOpenThread()} />

<!-- The list and the reading pane, as one positioned box: the pane stands
     in it at a fixed place, out of the flow, so the list can widen under it
     while it goes -- see `slide`. -->
<div class="mail">
  <section
    class="list"
    class:wide={!paneOpen}
    class:behind={phoneDetail}
    inert={phoneDetail}
    data-thread-list
    data-mail-pane
  >
    <div class="top">
      <div class="pane-titles">
        <span class="pane-title">{heading}</span>
        {#if unreadLine}<span class="pane-sub">{unreadLine}</span>{/if}
      </div>
      <button
        class="plus"
        class:spinning={syncing}
        title={syncTitle}
        aria-label={syncing ? 'Syncing' : 'Sync now'}
        aria-busy={syncing}
        onclick={() => void mail.syncAll(syncScope.length > 0 ? syncScope : undefined)}
      >
        <Icon name="refresh" size={17} />
      </button>
      <button
        class="plus"
        title="Quick cleanup"
        aria-label="Quick cleanup"
        aria-haspopup="dialog"
        onclick={() => openCleanup()}
      >
        <Icon name="broom" size={17} />
      </button>
      <button
        class="plus compose"
        title="Compose (C)"
        aria-label="Compose"
        onclick={() => void mail.compose()}
      >
        <Icon name="pencil" size={16} />
      </button>
    </div>

    {#if mail.sendsView?.list === 'scheduled' && mail.sendsView.account}
      <MailScheduled accountId={mail.sendsView.account} />
    {:else if mail.sendsView?.list === 'outbox'}
      <MailOutbox accountId={mail.sendsView.account} />
    {:else}
      {#if mail.checked.size > 0}
        <!-- What can be done to every picked thread at once, over the list
           they were picked from rather than in a reading pane of its own:
           the list keeps the whole width while they are. -->
        <div class="bulk" role="toolbar" aria-label="Picked conversations">
          <span class="bulk-count">
            {plural(mail.checked.size, 'conversation')} selected
          </span>
          <!-- No box of its own beside a pointer; a phone's strip of buttons
               that scrolls sideways under the count. -->
          <div class="bulk-actions">
            <button
              class="btn btn-sm btn-outline"
              onclick={() => void mail.archiveMany(mail.targets)}
            >
              <Icon name="layers" size={14} /> Archive <kbd>E</kbd>
            </button>
            {#if mail.emptiableRole !== 'trash'}
              <button
                class="btn btn-sm btn-outline"
                onclick={() => void mail.trashMany(mail.targets)}
              >
                <Icon name="trash" size={14} /> Trash <kbd>#</kbd>
              </button>
            {/if}
            {#if mail.emptiableRole && mail.mailbox}
              {@const box = mail.mailbox}
              <button
                class="btn btn-sm btn-outline"
                onclick={() => (mail.wantsForever = { ids: mail.targets, mailbox: box })}
              >
                <Icon name="trash" size={14} /> Delete forever…
                {#if mail.emptiableRole === 'trash'}<kbd>#</kbd>{/if}
              </button>
            {/if}
            {#if pickedThreads.some((t) => t.unreadCount > 0)}
              <button
                class="btn btn-sm btn-outline"
                onclick={() => void mail.markReadMany(mail.targets)}
              >
                <Icon name="check" size={14} /> Mark read <kbd>I</kbd>
              </button>
            {:else}
              <button
                class="btn btn-sm btn-outline"
                onclick={() => void mail.markUnreadMany(mail.targets)}
              >
                <Icon name="check" size={14} /> Mark unread <kbd>U</kbd>
              </button>
            {/if}
            {#if !inSnoozedView}
              <button
                class="btn btn-sm btn-outline"
                onclick={() => (mail.wantsSnooze = mail.targets)}
              >
                <Icon name="clock" size={14} /> Snooze… <kbd>H</kbd>
              </button>
            {/if}
            {#if pickedNewsletters}
              <!-- Filed as newsletters by mistake: out of the tab, and their
                   senders' new mail with them. -->
              <button
                class="btn btn-sm btn-outline"
                aria-haspopup="menu"
                onclick={(e) => menu.show(e, notNewsletterItems(mail.targets, 'Move to '))}
              >
                <Icon name="newspaper" size={14} /> Not newsletters
              </button>
            {/if}
            {#if unsubscribable && pickedBulkMail}
              <button
                class="btn btn-sm btn-outline"
                aria-haspopup="dialog"
                onclick={() => (mail.wantsUnsubscribe = mail.targets)}
              >
                <Icon name="mail-x" size={14} /> Unsubscribe…
                <kbd>{keysLabel('mod+u', userAgentMac)}</kbd>
              </button>
            {/if}
            <button class="btn btn-sm btn-outline" onclick={(e) => menu.show(e, bulkMenu())}>
              More…
            </button>
          </div>
          <button
            class="bulk-clear"
            title="Clear selection (Esc)"
            aria-label="Clear selection"
            onclick={() => mail.clearChecked()}
          >
            <Icon name="close" size={14} />
          </button>
          <p class="bulk-hint">
            Shift-click or <kbd>Shift</kbd>+<kbd>↓</kbd> picks a run of threads, {modKey}-click adds
            or removes one, and <kbd>X</kbd> picks the one under the cursor.
          </p>
        </div>
      {/if}
      {#if showTabs && !mail.searchQuery.trim()}
        <!-- The shared filter bar (app.css's "Filter bars"), as Todo's and the
           library's are: the open category named, the rest an icon and its
           count -- six labelled tabs and their counts are wider than the
           list column, and wrapped onto two rows they read as a paragraph
           rather than a control. Every chip is still named, in its title
           and to assistive technology, and its count turns the accent when
           anything under it is unread. -->
        <div class="tabs">
          <div class="filters" role="tablist" aria-label="Mail categories" bind:this={tabStrip}>
            {#each [{ key: null, label: 'All' }, ...CATEGORY_TABS] as tab (tab.key ?? 'all')}
              {@const count = categoryTabCount(mail.categoryCounts, tab.key)}
              {@const number = tabNumber(count)}
              {@const on = mail.category === tab.key}
              <button
                class="filter"
                role="tab"
                aria-selected={on}
                aria-label={tab.label}
                class:on
                title={count.threads > 0
                  ? `${tab.label}: ${plural(count.threads, 'conversation')}, ${count.unread.toLocaleString()} unread`
                  : tab.label}
                onclick={() => mail.setCategory(tab.key)}
              >
                <Icon name={TAB_ICONS[tab.key ?? 'all']} size={15} />
                {#if on}<span>{tab.label}</span>{/if}
                {#if number}
                  <span class="n" class:has-unread={count.unread > 0}>{compactCount(number)}</span>
                {/if}
              </button>
            {/each}
          </div>
        </div>
      {/if}

      {#if mail.selectedMailbox && !mail.searchQuery.trim()}
        <div class="only" role="group" aria-label="Show only">
          {#each MAIL_ONLY as { key, label } (key)}
            {@const on = mail.only.has(key)}
            <button
              class="only-toggle"
              class:on
              aria-pressed={on}
              title={key === 'unread'
                ? `Show only ${label.toLowerCase()} (${keysLabel('g r', userAgentMac)})`
                : `Show only ${label.toLowerCase()}`}
              onclick={() => mail.toggleOnly(key)}
            >
              <Icon name={ONLY_ICONS[key]} size={13} filled={on && key === 'starred'} />
              {label}
            </button>
          {/each}
        </div>
      {/if}

      {#if newsletterTipShown}
        <!-- The two things worth doing with a tab full of newsletters, said
           once where they are wanted, in the follow-up notice's quiet shape:
           stop the ones nobody reads, and take out the ones that are not
           newsletters at all. Put away for good with its cross. -->
        <div class="follow-note">
          <Icon name="newspaper" size={13} />
          <span class="follow-note-text" title={NEWSLETTER_TIP}>{NEWSLETTER_TIP}</span>
          <!-- "Clean up", not the list's own "Quick cleanup" again: one name
               for one button, to a screen reader and to the phone checks. -->
          <button
            class="follow-note-link"
            aria-haspopup="dialog"
            title="Open Quick cleanup on your newsletters"
            onclick={() => openCleanup('tab:newsletter')}
          >
            Clean up
          </button>
          <button
            class="follow-note-dismiss"
            aria-label="Hide this tip"
            title="Hide this tip"
            onclick={hideNewsletterBar}
          >
            <Icon name="close" size={12} />
          </button>
        </div>
      {/if}

      {#if mail.emptiableRole && mail.mailbox && (mail.threads.length > 0 || mail.only.size > 0)}
        {@const box = mail.mailbox}
        <div class="empty-bar">
          <span>Emptying can't be undone.</span>
          <button class="empty-btn" onclick={() => (mail.wantsEmpty = box)}>
            <Icon name="trash" size={13} /> Empty {mailboxDisplayName(box)}
          </button>
        </div>
      {/if}

      {#if followUpsNotice !== null}
        <!-- Where the follow-up sections would be, one quiet line saying why
           there are none: an Inbox with nothing at the top looked the same
           whether nothing was waiting or nothing was being asked. Outside the
           virtual list, so its keyed rows stay as they are. -->
        <div class="follow-note" role="status">
          <Icon name="info" size={13} />
          <span class="follow-note-text" title={followUpsNotice}>
            Follow-ups aren't running. {followUpsNotice}
          </span>
          <button class="follow-note-link" onclick={() => panels.openSettings('sorting')}>
            Open settings
          </button>
          <button
            class="follow-note-dismiss"
            aria-label="Dismiss"
            title="Hide this until the reason changes"
            onclick={() => sorting.dismissStall(followUpsNotice)}
          >
            <Icon name="close" size={12} />
          </button>
        </div>
      {/if}

      {#if mail.searchQuery.trim()}
        <p class="hint operator-hint">
          Try <code>from:</code>, <code>to:</code>, <code>subject:</code>,
          <code>has:attachment</code>, <code>before:</code>, <code>after:</code>, <code>in:</code>
          or <code>is:unread</code>.
        </p>
        {#if mail.searching && mail.searchResults.length === 0}
          <Loading label="Searching…" centred />
        {:else if mail.searchResults.length === 0}
          <EmptyState lead="Nothing matched “{mail.searchQuery}”">
            {#snippet note()}Try fewer words, or a different spelling.{/snippet}
          </EmptyState>
        {:else}
          <!-- `SearchMailResult.threads` is the same `Thread` shape every other
             row already draws from -- see `types.ts`'s own doc on why a
             search hit is not a narrower type of its own. -->
          <div class="scroll thread-rows">
            {#each searchRows as row (rowKey(row))}
              {#if row.type === 'header'}
                <div class="date-section" aria-hidden="true">{row.label}</div>
              {:else}
                {@render threadRow(threadOf(row), null)}
              {/if}
            {/each}
            {#if mail.searchCursor}
              <button
                class="load-more"
                disabled={mail.searching}
                onclick={() => void mail.loadMoreSearchResults()}
              >
                {mail.searching ? 'Loading…' : 'Load more'}
              </button>
            {/if}
          </div>
        {/if}
      {:else if mail.loading && listEmpty}
        <Loading centred />
      {:else if listEmpty && mail.only.size > 0}
        <EmptyState lead={onlyEmptyLead(mail.only)}>
          {#snippet action()}
            <button class="btn btn-outline" onclick={() => mail.setOnly(new Set())}>
              Show all mail
            </button>
          {/snippet}
        </EmptyState>
      {:else if listEmpty}
        {@const copy = mailboxEmptyCopy(mail.mailbox, inSnoozedView)}
        <EmptyState lead={copy.lead}>
          {#snippet note()}{copy.note}{/snippet}
        </EmptyState>
      {:else}
        <!-- The scrolling parent `VirtualList` needs: `virtua` watches its
           container's parent for scroll, and `section.list` itself never
           scrolls, so without this the rows past the first screen were
           unreachable and `onEndReached` never fired. -->
        <div class="scroll thread-rows">
          <VirtualList
            bind:this={threadList}
            items={listRows}
            getKey={rowKey}
            selectedId={mail.selectedThread}
            onEndReached={() => void mail.loadMore()}
          >
            {#snippet children(row: ListRow)}
              {#if row.type === 'header'}
                <div class="date-section" aria-hidden="true">{row.label}</div>
              {:else}
                {@const kind = followUpOf(row)}
                {#if kind}
                  {@render followUpRow(threadOf(row), kind)}
                {:else}
                  {@render threadRow(threadOf(row), null)}
                {/if}
              {/if}
            {/snippet}
          </VirtualList>
        </div>
      {/if}
    {/if}
    {#if paneOpen && !phone.on}
      <PaneResizer
        cssVar="--mail-list-w"
        storageKey="pane-w:mail-list"
        defaultWidth={380}
        min={280}
        max={640}
      />
    {/if}
  </section>

  <!-- One thread in a list -- the mailbox's own and a search's draw the same
     row. Whether it is unread is said once, down the left edge, and by how
     bright its words are, and everything else on the row keeps quiet: most of
     an inbox can be unread at once, so anything louder repeats on every line.
     The tick takes the dot's place once the row is picked. `followUp` is the
     section at the top it is drawn in, if any, which a pill names. -->
  {#snippet threadRow(t: Thread, followUp: FollowUpKind | null)}
    {@const picked = mail.checked.has(t.id)}
    {@const preview = snippetText(t.snippet)}
    {@const sorted = sorting.categoryOf(t)}
    <button
      class="row list-row"
      data-thread={t.id}
      class:sel={mail.selectedThread === t.id}
      class:checked={picked}
      class:unread={t.unreadCount > 0}
      class:follow={followUp !== null}
      aria-current={mail.selectedThread === t.id ? 'true' : undefined}
      aria-pressed={picked ? true : undefined}
      onmousedown={noTextRange}
      onclick={(e) => clickRow(e, t.id)}
      oncontextmenu={(e) => menu.show(e, menuFor(t))}
    >
      {#if picked}
        <span class="tick" aria-hidden="true"><Icon name="tick" size={13} weight={2.5} /></span>
      {:else}
        <span class="dot" class:on={t.unreadCount > 0} aria-hidden="true"></span>
      {/if}
      <span class="who">
        {#if t.unreadCount > 0}<span class="sr-only">Unread, </span>{/if}
        <span class="from">{threadPeople(t.participants, accounts.ownAddresses(t.accountId))}</span>
        {#if t.messageCount > 1}<span class="count">{t.messageCount}</span>{/if}
      </span>
      <span class="gist">
        <span class="words">
          <span class="subject">{t.subject || '(no subject)'}</span>
          {#if preview}<span class="snippet">{preview}</span>{/if}
        </span>
        {#if followUp && t.followUp}
          <span class="sorted waiting" title={WAITING[followUp]}>{followUpPill(t.followUp)}</span>
        {/if}
        {#if sorted}
          <span class="sorted" title="Sorted as {sorted.name}">{sorted.name}</span>
        {/if}
        {#if mail.searchQuery.trim()}
          {@const place = searchPlace(mail.searchPlaces[t.id])}
          {#if place}<span class="sorted place" title="Where this conversation is">{place}</span
            >{/if}
        {/if}
      </span>
      <span class="side">
        {#if t.category === 'priority'}
          <span class="mark priority" title="Priority"
            ><Icon name="flag" size={12} /><span class="sr-only">Priority</span></span
          >
        {/if}
        {#if t.starred}
          <span class="mark starred" title="Starred"
            ><Icon name="star" size={12} filled /><span class="sr-only">Starred</span></span
          >
        {/if}
        {#if t.hasAttachments}
          <span class="mark" title="Has an attachment"
            ><Icon name="paperclip" size={12} /><span class="sr-only">Has an attachment</span></span
          >
        {/if}
        <span class="date">{rowDate(t)}</span>
        {#if unifiedView}
          <span
            class="acct"
            style:background={accountColor(t.accountId, accountIds)}
            title={accountLabel(t.accountId)}
            ><span class="sr-only">{accountLabel(t.accountId)}</span></span
          >
        {/if}
      </span>
    </button>
  {/snippet}

  <!-- A row in a follow-up section at the top of the Inbox: the ordinary row,
     and beside it -- not inside it, a button cannot hold another -- the tick
     that says it is not needed, in a gutter the row leaves for it. -->
  {#snippet followUpRow(t: Thread, kind: FollowUpKind)}
    <div class="follow-row">
      {@render threadRow(t, kind)}
      <button
        class="follow-done"
        title="Not needed — take it off the top"
        aria-label="Not needed — take it off the top"
        onclick={() => void mail.dismissFollowUp(t.id)}
      >
        <Icon name="close" size={13} />
      </button>
    </div>
  {/snippet}

  <!-- The open thread's subject and what is said about it: who, how many,
     whether it is snoozed or waiting on somebody. -->
  {#snippet subjectBlock(open: ThreadDetail)}
    <div class="subject-block">
      <h1>{open.thread.subject || '(no subject)'}</h1>
      <p class="meta">
        <span>{plural(open.messages.length, 'message')}</span>
        {#if openAccount && mailAccounts.length > 1}
          <span class="sep">·</span>
          <span class="meta-acct">
            <span
              class="acct"
              style:background={accountColor(openAccount.id, accountIds)}
              aria-hidden="true"
            ></span>
            {openAccount.displayName || openAccount.address}
          </span>
        {/if}
        {#if open.thread.category === 'priority'}
          <span class="sep">·</span>
          <span class="priority-mark"><Icon name="flag" size={12} /> Priority</span>
        {/if}
        {#if sorting.categoryOf(open.thread)}
          <span class="sep">·</span>
          <span class="sorted-mark" title="Sorted by the decision model">
            <Icon name="tag" size={12} />
            {sorting.categoryOf(open.thread)?.name}
          </span>
        {/if}
        {#if mail.openOffer}
          <!-- Only when the sender's mail names a way out, and in the line
               rather than the toolbar, where one more pill pushed Summarise
               onto a second row at an ordinary window width. Once asked it
               says so, and pressing it offers to ask again. -->
          {@const offer = mail.openOffer}
          <span class="sep">·</span>
          <button
            class="meta-link unsub"
            aria-haspopup="dialog"
            title={askedLine(offer.askedAt) ??
              `${unsubscribeLabel(offer.via)} (${keysLabel('mod+u', userAgentMac)})`}
            onclick={() => unsubscribeOpen(open.thread.id, offer)}
          >
            <Icon name="mail-x" size={12} />
            {offer.askedAt ? 'Unsubscribed' : 'Unsubscribe'}
          </button>
        {/if}
        {#if open.thread.category === 'newsletter'}
          <!-- Filed as a newsletter, which plenty of mail is by mistake: the
               way out is a word away rather than under Move to category. -->
          <span class="sep">·</span>
          <button
            class="meta-link"
            aria-haspopup="menu"
            title="File this sender's mail somewhere else"
            onclick={(e) => menu.show(e, notNewsletterItems([open.thread.id], 'Move to '))}
          >
            Not a newsletter?
          </button>
        {/if}
      </p>
      {#if open.thread.snoozedUntil && new Date(open.thread.snoozedUntil) > new Date()}
        <p class="snoozed-banner">
          <Icon name="clock" size={13} />
          <span>Snoozed until {weekdayAndTime(open.thread.snoozedUntil)}</span>
          <button class="link" onclick={() => void mail.unsnooze(open.thread.id)}>Unsnooze</button>
        </p>
      {/if}
      <MailFollowUpBanner detail={open} />
      {#if recentActionLines.length > 0}
        <p class="recent-actions">
          {#each recentActionLines as line, i (i)}
            {#if i > 0}<span class="sep">·</span>{/if}<span>{line}</span>
          {/each}
        </p>
      {/if}
    </div>
  {/snippet}

  {#if paneOpen || phoneDetail}
    <main
      class="main"
      class:replying={phone.on && mail.composing !== null && mail.composeInline}
      data-mail-pane
      transition:slide
    >
      {#if mail.composing && !mail.composeInline}
        <!-- A new message, or a draft reopened, where a thread is read rather
         than in a dialog over everything: the list stays in reach beside
         it. Keyed, so a second draft opened over the first is a fresh sheet
         rather than the first one's working copy under another name. -->
        {#key mail.composeKey}
          <MailCompose
            bind:this={composeSheet}
            draft={mail.composing}
            onclose={() => mail.closeCompose()}
          />
        {/key}
      {:else if mail.openThread}
        <!-- Captured once, rather than re-read as `mail.openThread.thread.id`
         inside every button below: a closure does not inherit the
         narrowing this `{#if}` gives the expression directly above it, so
         without this each one would have to re-assert past `| null` by
         hand. `open` is itself never reassigned for the life of the block,
         so every closure below closes over the same non-null value. -->
        {@const open = mail.openThread}
        {@const newest = open.messages.at(-1)}
        <div class="thread-head">
          {#if phone.on}
            <!-- A phone's toolbar: the way back, then the four things done
                 most -- answer it, put it away, put it off, throw it out --
                 and the rest in a menu. Each a finger wide. -->
            <div class="phone-bar" role="toolbar" aria-label="Thread actions">
              <PhoneBack onclick={() => mail.closeThread()} />
              <span class="spacer"></span>
              {#if newest}
                <button
                  class="tool"
                  aria-label="Reply"
                  title="Reply"
                  onclick={() => mail.reply(newest.id, false)}
                >
                  <Icon name="reply" size={20} />
                </button>
              {/if}
              <button
                class="tool"
                aria-label="Archive"
                title="Archive"
                onclick={() => void mail.archive(open.thread.id)}
              >
                <Icon name="archive" size={20} />
              </button>
              <button
                class="tool"
                aria-label="Snooze"
                title="Snooze"
                onclick={() => (mail.wantsSnooze = [open.thread.id])}
              >
                <Icon name="clock" size={20} />
              </button>
              {#if mail.emptiableRole === 'trash' && mail.mailbox}
                {@const box = mail.mailbox}
                <button
                  class="tool"
                  aria-label="Delete forever"
                  title="Delete forever"
                  onclick={() => (mail.wantsForever = { ids: [open.thread.id], mailbox: box })}
                >
                  <Icon name="trash" size={20} />
                </button>
              {:else}
                <button
                  class="tool"
                  aria-label="Trash"
                  title="Trash"
                  onclick={() => void mail.trash(open.thread.id)}
                >
                  <Icon name="trash" size={20} />
                </button>
              {/if}
              <button
                class="tool"
                aria-label="More actions"
                title="More actions"
                aria-haspopup="menu"
                onclick={(e) => menu.show(e, threadMenu(open, newest))}
              >
                <Icon name="more" size={20} />
              </button>
            </div>
          {:else}
            <!-- The toolbar: what to do with this thread, grouped the way the
           hands reach for it -- answer it, put it away, mark it -- each group
           one pill, so a row of twelve icons reads as three decisions. -->
            <div class="thread-toolbar" role="toolbar" aria-label="Thread actions">
              <button
                class="tool back"
                onclick={() => mail.closeThread()}
                title="Back to the list (Escape)"
                aria-label="Back to the list"
              >
                <Icon name="chevron" size={15} />
              </button>
              {#if newest}
                <div class="group">
                  <button
                    class="tool"
                    title="Reply (R)"
                    aria-label="Reply"
                    onclick={() => mail.reply(newest.id, false)}
                  >
                    <Icon name="reply" size={17} />
                  </button>
                  <button
                    class="tool"
                    title="Reply all (W)"
                    aria-label="Reply all"
                    onclick={() => mail.reply(newest.id, true)}
                  >
                    <Icon name="reply-all" size={17} />
                  </button>
                  <button
                    class="tool"
                    title="Forward (F)"
                    aria-label="Forward"
                    onclick={() => mail.forward(newest.id)}
                  >
                    <Icon name="forward" size={17} />
                  </button>
                </div>
              {/if}
              <div class="group">
                <button
                  class="tool"
                  title="Archive (E)"
                  aria-label="Archive"
                  onclick={() => void mail.archive(open.thread.id)}
                >
                  <Icon name="archive" size={17} />
                </button>
                <button
                  class="tool"
                  title="Snooze… (H)"
                  aria-label="Snooze"
                  onclick={() => (mail.wantsSnooze = [open.thread.id])}
                >
                  <Icon name="clock" size={17} />
                </button>
                {#if mail.emptiableRole === 'trash' && mail.mailbox}
                  {@const box = mail.mailbox}
                  <button
                    class="tool"
                    title="Delete forever… (#)"
                    aria-label="Delete forever"
                    onclick={() => (mail.wantsForever = { ids: [open.thread.id], mailbox: box })}
                  >
                    <Icon name="trash" size={17} />
                  </button>
                {:else}
                  <button
                    class="tool"
                    title="Trash (#)"
                    aria-label="Trash"
                    onclick={() => void mail.trash(open.thread.id)}
                  >
                    <Icon name="trash" size={17} />
                  </button>
                {/if}
              </div>
              <div class="group">
                <button
                  class="tool"
                  class:on={open.thread.category === 'priority'}
                  title={open.thread.category === 'priority'
                    ? 'Remove from priority (!)'
                    : 'Mark as priority (!)'}
                  aria-label="Toggle priority"
                  aria-pressed={open.thread.category === 'priority'}
                  onclick={() =>
                    void mail.setPriority(open.thread.id, open.thread.category !== 'priority')}
                >
                  <Icon name="flag" size={17} />
                </button>
                <button
                  class="tool"
                  title="Mark unread (U)"
                  aria-label="Mark unread"
                  onclick={() => void mail.markUnread(open.thread.id)}
                >
                  <Icon name="mail" size={17} />
                </button>
                <button
                  class="tool"
                  title="Label… (L)"
                  aria-label="Label"
                  onclick={() => (mail.wantsLabel = [open.thread.id])}
                >
                  <Icon name="tag" size={17} />
                </button>
                <button
                  class="tool"
                  title="Make a task, add to the calendar, save to a note"
                  aria-label="Send to another app"
                  aria-haspopup="menu"
                  onclick={(e) => menu.show(e, tidyMenu(bridgeItems(open, newest)))}
                >
                  <Icon name="more" size={17} />
                </button>
              </div>
              <span class="spacer"></span>
              {#if canListen}
                <button
                  class="summarize"
                  title={listeningToOpen
                    ? 'Stop reading it aloud'
                    : 'Listen to a briefing of this thread'}
                  onclick={toggleListen}
                >
                  <Icon name={listeningToOpen ? 'stop' : 'play'} size={14} />
                  {#if !listeningToOpen}
                    Listen
                  {:else if listening.state === 'preparing'}
                    Preparing…
                  {:else}
                    Stop
                  {/if}
                </button>
              {/if}
              {#if canSummarize}
                <button
                  class="summarize"
                  title="Summarise (Z)"
                  disabled={mail.summarizing}
                  onclick={() => void mail.summarizeOpenThread()}
                >
                  <Icon name="sparkle" size={14} />
                  {mail.summarizing ? 'Summarising…' : 'Summarise'}
                </button>
              {/if}
            </div>
          {/if}
          <!-- Beside a pointer the subject is pinned with the toolbar; on a phone
               it scrolls away with the thread -- see the same block in
               `.scroll` below -- so the messages have the screen. -->
          {#if !phone.on}
            {@render subjectBlock(open)}
          {/if}
        </div>
        {#if mail.summary && mail.summary.threadId === open.thread.id}
          <div class="summary-panel">
            <Icon name="sparkle" size={14} />
            <p>{mail.summary.text}</p>
            <button
              class="close"
              aria-label="Dismiss the summary"
              title="Dismiss the summary"
              onclick={() => mail.dismissSummary()}><Icon name="close" size={13} /></button
            >
          </div>
        {/if}
        {#if listeningToOpen && listening.sentences.length > 0}
          <!-- What the voice has said so far, in the summary's place and style.
               The sentence being said is not marked yet. -->
          <div class="summary-panel">
            <Icon name="play" size={14} />
            <p>{listening.sentences.join(' ')}</p>
            <button
              class="close"
              aria-label="Stop reading aloud"
              title="Stop reading aloud"
              onclick={() => listening.stop()}><Icon name="close" size={13} /></button
            >
          </div>
        {/if}
        <div class="scroll">
          {#if phone.on}
            {@render subjectBlock(open)}
          {/if}
          <MailThread messages={open.messages} expanded={mail.expanded} sending={open.sending} />
          {#if draftCards.length > 0}
            <MailThreadDrafts drafts={draftCards} />
          {/if}
          {#if mail.composing && mail.composeInline}
            {#key mail.composeKey}
              <div class="inline-reply" use:scrollIntoViewOnMount>
                <MailCompose
                  bind:this={composeSheet}
                  draft={mail.composing}
                  placement="thread"
                  onclose={() => mail.closeCompose()}
                />
              </div>
            {/key}
          {/if}
        </div>
      {:else}
        <!-- A phone's tap on a row, before the thread has come: the screen
             it will be on, with the way back. -->
        <div class="thread-head">
          <div class="phone-bar">
            <PhoneBack onclick={() => mail.closeThread()} />
          </div>
          <Loading label="Opening…" centred />
        </div>
      {/if}
    </main>
  {/if}
</div>

{#if mail.wantsSnooze}
  <MailSnoozePicker onchoose={snooze} oncancel={() => (mail.wantsSnooze = null)} />
{/if}

{#if mail.wantsLabel}
  <MailLabelPicker onchoose={applyLabel} oncancel={() => (mail.wantsLabel = null)} />
{/if}

{#if mail.wantsForever}
  {@const count = mail.wantsForever.ids.length}
  {@const where = mailboxDisplayName(mail.wantsForever.mailbox)}
  <ConfirmDialog
    title={count === 1
      ? 'Delete this conversation forever?'
      : `Delete ${plural(count, 'conversation')} forever?`}
    detail={`${count === 1 ? 'Its' : 'Their'} messages in ${where} are deleted for good, on the server too. This can't be undone.`}
    confirmLabel="Delete forever"
    onconfirm={confirmForever}
    oncancel={() => (mail.wantsForever = null)}
  />
{/if}

{#if mail.wantsEmpty}
  <ConfirmDialog
    title={`Empty ${mailboxDisplayName(mail.wantsEmpty)}?`}
    detail={`Everything in ${emptyScope(mail.wantsEmpty)} is deleted for good, on the server too. This can't be undone.`}
    confirmLabel={`Empty ${mailboxDisplayName(mail.wantsEmpty)}`}
    onconfirm={() => void confirmEmpty()}
    oncancel={() => (mail.wantsEmpty = null)}
  />
{/if}

{#if unsubscribeDialog}
  <ChoiceDialog
    title={unsubscribeDialog.title}
    detail={unsubscribeDialog.detail}
    options={unsubscribeDialog.options}
    oncancel={() => (unsubscribeAsk = null)}
  />
{/if}

{#if unsubscribeLeft}
  <UnsubscribeResults outcomes={unsubscribeLeft} onclose={() => (unsubscribeLeft = null)} />
{/if}

{#if cleaning}
  <MailCleanup accountId={scopeAccount} kind={cleaningKind} onclose={() => (cleaning = false)} />
{/if}

<style>
  /* The list and the reading pane. Stated heights, not stretched, here and
     in both: see `.shell-stage` in app.css. */
  .mail {
    position: relative;
    display: flex;
    flex: 1;
    min-width: 0;
    height: 100%;
    /* The list's width beside the pane: the dragged one, short of leaving
       the pane less than 360px. The two always tile, never overlap -- the
       pane has no background of its own to hide the list with. Percentages
       here resolve where it is used, against this box, for both. */
    --list-at: max(220px, min(var(--mail-list-w), 100% - 360px));
  }
  .list {
    height: 100%;
    position: relative;
    /* Mail's own variable -- `--list-w` is shared with the journal's and
       the calendar's lists, which a drag here must leave alone. */
    width: var(--list-at);
    flex: 0 0 auto;
    display: flex;
    flex-direction: column;
    border-right: 1px solid var(--border);
  }
  /* Nothing open beside it: all of the width. `slide` moves it between. */
  .list.wide {
    flex-grow: 1;
    border-right-color: transparent;
  }
  /* Moving: its contents stay at the width it is going to -- see `slide`,
     which sets the class (so global, or it would be pruned as unused). */
  .list:global(.sliding) {
    overflow: hidden;
  }
  .list:global(.sliding) > * {
    width: var(--slide-w);
    flex-shrink: 0;
  }
  .top {
    display: flex;
    align-items: center;
    gap: 2px;
    min-height: var(--header-h);
    padding: var(--sp-2) var(--sp-3) var(--sp-1) var(--sp-5);
    flex: none;
  }
  .plus {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    border-radius: 50%;
    color: var(--fg-muted);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .plus:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  /* Compose is the one thing this column is for besides reading, so it is
     the one filled button: the accent, the way the assistant's own is. */
  .plus.compose {
    margin-left: var(--sp-1);
    background: var(--accent);
    color: var(--fg-on-accent);
    box-shadow: var(--shadow-sm);
  }
  .plus.compose:hover {
    background: var(--accent-hover);
    color: var(--fg-on-accent);
  }
  /* Sync now, while a pass is running: the icon turns, the button around it
     stays put. Stopped outright under reduced motion -- the title still
     says "Syncing…" -- rather than left to the global rule, which would
     still play one very short turn. */
  .plus.spinning :global(svg) {
    animation: spin 1s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .plus.spinning :global(svg) {
      animation: none;
    }
  }

  .hint {
    padding: var(--sp-4) var(--sp-5);
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }
  .operator-hint {
    padding: var(--sp-2) var(--sp-5);
    font-size: var(--text-xs);
    line-height: var(--leading-normal);
  }
  .operator-hint code {
    padding: 1px 4px;
    border-radius: 4px;
    background: var(--bg-hover);
    font-size: inherit;
  }
  .load-more {
    width: 100%;
    padding: var(--sp-3);
    text-align: center;
    color: var(--journal-accent, var(--accent));
    font-size: var(--text-sm);
  }
  .load-more:hover {
    background: var(--bg-hover);
  }

  /* ── The category filter ────────────────────────────────────────── */

  /* The shared bar, spread across the column: six chips share its width
     rather than wrapping, the unchosen ones narrowed to an icon and a
     count. */
  .tabs {
    flex: none;
    padding: var(--sp-1) var(--sp-4) var(--sp-3);
  }
  .tabs .filters {
    flex-wrap: nowrap;
    justify-content: space-between;
  }
  .tabs .filter {
    flex: 1 1 auto;
    justify-content: center;
    gap: 5px;
    padding: 0 6px;
  }
  .tabs .filter.on {
    padding: 0 var(--sp-3);
  }
  .tabs .filter .n.has-unread {
    color: var(--accent);
    font-weight: 650;
  }
  .tabs .filter.on .n.has-unread {
    color: inherit;
  }

  /* ── The "show only" toggles ─────────────────────────────────────── */

  /* Quieter than the tabs above them, which say what kind of mail this is:
     these say how much of it to show. */
  .only {
    flex: none;
    display: flex;
    gap: var(--sp-1);
    padding: 0 var(--sp-4) var(--sp-2);
  }
  .only-toggle {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 26px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-pill);
    font-size: var(--text-xs);
    font-weight: 550;
    color: var(--fg-subtle);
    white-space: nowrap;
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .only-toggle:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .only-toggle.on {
    background: color-mix(in oklab, var(--accent) 16%, transparent);
    color: color-mix(in oklab, var(--accent) 72%, var(--fg));
  }

  /* Trash and Spam: the one thing to do with all of it, said once. */
  .empty-bar {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2);
    margin: 0 var(--sp-4) var(--sp-2);
    padding: var(--sp-1) var(--sp-1) var(--sp-1) var(--sp-3);
    border-radius: var(--radius);
    background: var(--bg-hover);
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .empty-bar > span {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .empty-btn {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 26px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-pill);
    font-weight: 600;
    color: var(--danger);
    white-space: nowrap;
    transition: background var(--fast) var(--ease);
  }
  .empty-btn:hover {
    background: color-mix(in oklab, var(--danger) 14%, transparent);
  }

  /* Why there are no follow-ups at the top: the follow-up banner's quiet
     accent tint, one line (two at most, the whole of it in the title), the
     way to mend it as words, and a cross. */
  .follow-note {
    flex: none;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin: 0 var(--sp-4) var(--sp-2);
    padding: var(--sp-1) var(--sp-1) var(--sp-1) var(--sp-3);
    border-radius: var(--radius-lg);
    background: color-mix(in oklab, var(--accent) 10%, transparent);
    color: var(--fg-muted);
    font-size: var(--text-xs);
    line-height: var(--leading-normal);
  }
  .follow-note > :global(svg) {
    flex: none;
    color: var(--journal-accent, var(--accent));
  }
  .follow-note-text {
    flex: 1;
    min-width: 0;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .follow-note-link {
    flex: none;
    color: var(--journal-accent, var(--accent));
    font-weight: 600;
    white-space: nowrap;
  }
  .follow-note-link:hover {
    text-decoration: underline;
  }
  .follow-note-dismiss {
    flex: none;
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .follow-note-dismiss:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  /* ── Rows ───────────────────────────────────────────────────────── */

  /* What each row's width query below measures. */
  .thread-rows {
    flex: 1;
    min-height: 0;
    padding-bottom: var(--sp-2);
    container-type: inline-size;
  }

  /* A grouping heading over a run of rows -- a date, or a follow-up section
     -- never a button, never focusable: `j`/`k` and a row's own context menu
     read `mail.shownThreads`, which never contains one of these, so there is
     nothing here for either to skip over by accident. Lined up with the
     row's text. */
  .date-section {
    padding: var(--sp-4) var(--sp-5) var(--sp-1) calc(var(--sp-2) + var(--sp-5));
    font-size: var(--text-xs);
    font-weight: 500;
    color: var(--fg-subtle);
  }

  /* Each row is the shared `.list-row` card (app.css's "Lists"); what is
     here is the thread row's own layout inside it. Two lines in the column's
     usual width, who and when over what it is about; one line once the column
     is dragged wide enough to hold all three side by side. */
  .row {
    display: grid;
    grid-template:
      'who side' auto
      'gist gist' auto
      / minmax(0, 1fr) auto;
    gap: 1px var(--sp-3);
    align-items: baseline;
    padding: var(--sp-2) var(--sp-3) var(--sp-2) var(--sp-5);
  }
  @container (min-width: 560px) {
    .row {
      /* The gist stops at a reading width and the date follows it, rather than
         sitting at the far edge of a 1440px-wide list, a long way from the
         name it belongs to. The row's card still runs the full width. */
      grid-template: 'who gist side' auto / 168px minmax(0, var(--measure)) auto;
      justify-content: start;
      column-gap: var(--sp-4);
    }
  }
  /* Picked, one of several: the same tint as the open row, a shade deeper. */
  .row.checked {
    background: color-mix(in oklab, var(--accent) 18%, transparent);
  }
  .dot,
  .tick {
    position: absolute;
    color: var(--accent);
  }
  .dot {
    left: 7px;
    top: calc(var(--sp-2) + 7px);
    width: 6px;
    height: 6px;
    border-radius: 50%;
  }
  .dot.on {
    background: var(--accent);
  }
  .tick {
    left: 4px;
    top: calc(var(--sp-2) + 4px);
    display: grid;
  }
  .who {
    grid-area: who;
    display: flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
  }
  .from {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-base);
    color: var(--fg-muted);
  }
  /* How many messages: a bare number beside the names, which it counts. */
  .count {
    flex: none;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .gist {
    grid-area: gist;
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    min-width: 0;
  }
  /* The subject with the start of the message running on after it, on the one
     line, so a short subject shows more and a long one simply hides the rest. */
  .words {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
  }
  .subject {
    color: var(--fg-muted);
  }
  .snippet {
    margin-left: var(--sp-2);
    color: var(--fg-subtle);
  }
  /* Unread is the words at full brightness and a little heavier; read mail
     stays a step back, so the two can be told apart from across the list. */
  .row.unread .from {
    font-weight: 600;
    color: var(--fg);
  }
  .row.unread .subject {
    font-weight: 500;
    color: var(--fg);
  }
  .side {
    grid-area: side;
    display: flex;
    align-items: center;
    gap: 6px;
    align-self: center;
  }
  .mark {
    flex: none;
    display: grid;
    place-items: center;
    color: var(--fg-subtle);
  }
  .mark.priority,
  .mark.starred {
    color: var(--journal-accent, var(--accent));
  }
  .date {
    flex: none;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .row.unread .date {
    color: var(--fg-muted);
  }
  /* Which account, in the unified Inbox: a short bar at the far edge. A dot
     beside the name read as a second way of saying unread. */
  .acct {
    flex: none;
    width: 3px;
    height: 12px;
    border-radius: 2px;
  }
  /* The category a decision model sorted the thread into: a quiet word -- the
     tab it is filed under already says what kind of mail it is; this says which
     of the person's own names for it. */
  .sorted {
    flex: none;
    max-width: 40%;
    overflow: hidden;
    font-size: 11px;
    font-weight: 500;
    color: var(--fg-subtle);
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  /* A follow-up's pill: the same quiet word, in the accent, since why it is at
     the top is the one thing on the row worth saying first. */
  .sorted.waiting {
    color: var(--journal-accent, var(--accent));
    font-weight: 600;
  }

  /* A follow-up row and its tick. The row keeps its own card; the tick sits
     in a gutter the row leaves at its right edge, so it covers nothing. */
  .follow-row {
    position: relative;
  }
  .row.follow {
    padding-right: calc(var(--sp-3) + 28px);
  }
  /* Hovering the tick is still hovering the row it belongs to. */
  .follow-row:hover .row:not(.sel):not(.checked) {
    background: var(--bg-hover);
  }
  .follow-done {
    position: absolute;
    top: 50%;
    right: calc(var(--sp-2) + 6px);
    translate: 0 -50%;
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: 50%;
    color: var(--fg-faint);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .follow-done:hover,
  .follow-done:focus-visible {
    background: color-mix(in oklab, var(--accent) 14%, transparent);
    color: var(--journal-accent, var(--accent));
    outline: none;
  }

  .sorted-mark {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    flex: none;
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--fg-muted);
  }

  /* The open thread's Priority mark in the reading pane's header. */
  .priority-mark {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    flex: none;
    color: var(--journal-accent, var(--accent));
    font-size: var(--text-xs);
    font-weight: 600;
  }

  /* Beside the list, out of the flow so the list can widen under it as it
     goes (see `slide`), and as wide as what the list leaves. */
  .main {
    position: absolute;
    top: 0;
    right: 0;
    height: 100%;
    width: calc(100% - var(--list-at));
    display: flex;
    flex-direction: column;
  }
  .thread-head {
    display: flex;
    flex-direction: column;
    flex: none;
  }
  /* Wraps rather than clips: on a narrow window the third group drops to a
     second line instead of losing its last button off the edge. Its own
     name, not the shared `.toolbar`, which is the filter bars' row. */
  .thread-toolbar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px var(--sp-2);
    min-height: 56px;
    padding: var(--sp-2) var(--sp-4);
    border-bottom: 1px solid var(--border);
  }
  .group {
    display: flex;
    align-items: center;
    gap: 1px;
    padding: 2px;
    border-radius: var(--radius-pill);
    background: var(--bg-sunken);
  }
  .tool {
    display: grid;
    place-items: center;
    width: 36px;
    height: 32px;
    border-radius: var(--radius-pill);
    color: var(--fg-muted);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .tool:hover {
    background: var(--bg-raised);
    color: var(--fg);
    box-shadow: var(--shadow-sm);
  }
  .tool.back {
    width: 32px;
    transform: rotate(180deg);
  }
  .tool.back:hover {
    background: var(--bg-hover);
    box-shadow: none;
  }
  /* The priority toggle's pressed state, in colour alone: `Icon`'s
     `filled` swaps stroke for fill, and the flag's pole is a bare line with
     no area to fill, so the filled flag drew as a floating orange block. */
  .tool.on {
    color: var(--journal-accent, var(--accent));
    background: var(--bg-raised);
  }
  .spacer {
    flex: 1;
  }
  .subject-block {
    padding: var(--sp-5) var(--sp-6) var(--sp-2);
  }
  .subject-block h1 {
    margin: 0;
    font-size: var(--text-xl);
    font-weight: 700;
    letter-spacing: -0.02em;
    line-height: var(--leading-tight);
    overflow-wrap: anywhere;
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    margin: 6px 0 0;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  /* A small round dot between the facts, drawn rather than typed: the
     middle-dot glyph sits on whatever baseline the font gives it, which
     beside an 8px account dot and an icon read as a stray full stop. */
  .meta .sep {
    width: 3px;
    height: 3px;
    border-radius: 50%;
    background: currentColor;
    opacity: 0.6;
    font-size: 0;
  }
  .meta-acct {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .meta .acct {
    width: 8px;
    height: 8px;
    border-radius: 50%;
  }
  /* "Not a newsletter?": a word in the line, in the accent, not a button's
     box -- the snoozed banner's Unsnooze is the same kind of thing. */
  .meta-link {
    color: var(--journal-accent, var(--accent));
    font-size: var(--text-xs);
    font-weight: 600;
  }
  .meta-link:hover {
    text-decoration: underline;
  }
  .meta-link.unsub {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  .recent-actions {
    display: flex;
    gap: var(--sp-2);
    margin: var(--sp-2) 0 0;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .recent-actions .sep {
    opacity: 0.5;
  }
  .snoozed-banner {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    margin: var(--sp-2) 0 0;
    padding: 4px var(--sp-3);
    border-radius: var(--radius-pill);
    background: color-mix(in oklab, var(--accent) 10%, transparent);
    color: var(--fg-muted);
    font-size: var(--text-xs);
  }
  .snoozed-banner .link {
    color: var(--journal-accent, var(--accent));
    font-weight: 600;
  }
  .summarize {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 34px;
    padding: 0 var(--sp-3);
    border-radius: var(--radius-pill);
    background: color-mix(in oklab, var(--accent) 10%, transparent);
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--journal-accent, var(--accent));
  }
  .summarize:hover {
    background: color-mix(in oklab, var(--accent) 16%, transparent);
  }
  .summarize:disabled {
    opacity: 0.6;
  }
  .summary-panel {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    padding: var(--sp-2) var(--sp-4);
    border-bottom: 1px solid var(--border);
    background: color-mix(in oklab, var(--accent) 6%, transparent);
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }
  .summary-panel p {
    flex: 1;
    min-width: 0;
    margin: 0;
    line-height: var(--leading-normal);
  }
  .summary-panel .close {
    flex: none;
    display: grid;
    place-items: center;
    color: var(--fg-faint);
  }
  .summary-panel .close:hover {
    color: var(--fg);
  }
  .scroll {
    flex: 1;
    overflow-y: auto;
  }

  /* Inset the way `MailThread`'s own messages are, so the reply lines up
     under the message it answers rather than running edge to edge. */
  /* Room at the end for the floating assistant button, which would otherwise
     sit on the reply's own tools once it is scrolled all the way down. */
  .inline-reply {
    padding: 0 var(--sp-5) var(--fab-clear);
    container-type: inline-size;
  }

  /* The picked threads' actions, a row of small pills over the list. */
  .bulk {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    margin: 0 var(--sp-3) var(--sp-2);
    padding: var(--sp-2) var(--sp-2) var(--sp-2) var(--sp-3);
    border-radius: var(--radius-lg);
    background: color-mix(in oklab, var(--accent) 10%, transparent);
    flex: none;
  }
  .bulk-count {
    margin-right: auto;
    font-size: var(--text-sm);
    font-weight: 620;
  }
  .bulk-actions {
    display: contents;
  }
  .bulk-clear {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    color: var(--fg-muted);
  }
  .bulk-clear:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .bulk kbd {
    padding: 0 4px;
    border-radius: 4px;
    background: var(--bg-hover);
    font-family: inherit;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .bulk-hint {
    flex-basis: 100%;
    margin: 0;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    line-height: var(--leading-normal);
  }

  /* ── On a phone ─────────────────────────────────────────────────────
     One column. The list has the whole width; a thread, or a message being
     written, is a screen over it (`phoneDetail`). The list stays under it,
     scrolled where it was, only hidden -- so back finds it as it was left.
     Everything here is under `data-phone`: the desktop is as it was. */
  :global([data-phone]) .list {
    width: 100%;
    border-right: 0;
  }
  :global([data-phone]) .list.behind {
    visibility: hidden;
  }
  :global([data-phone]) .main {
    inset: 0;
    width: 100%;
  }

  /* Six categories and the open one's name do not always fit across: the
     strip scrolls sideways, as a row of chips may. */
  :global([data-phone]) .tabs .filters {
    justify-content: flex-start;
    overflow-x: auto;
    overflow-y: hidden;
    scrollbar-width: none;
  }
  :global([data-phone]) .tabs .filters::-webkit-scrollbar {
    display: none;
  }
  :global([data-phone]) .tabs .filter {
    flex: 1 0 auto;
  }

  /* The heading's buttons, a finger wide. */
  :global([data-phone]) .top {
    gap: 0;
    padding: var(--sp-2) var(--sp-2) var(--sp-1) var(--sp-4);
  }
  :global([data-phone]) .plus {
    width: 44px;
    height: 44px;
  }
  :global([data-phone]) .plus.compose {
    margin-left: var(--sp-1);
  }

  /* Chips for what to show, tall enough to hit. */
  :global([data-phone]) .only {
    gap: var(--sp-2);
    padding-bottom: var(--sp-3);
  }
  :global([data-phone]) .only-toggle {
    height: 34px;
    padding: 0 var(--sp-3);
    font-size: var(--text-sm);
  }
  :global([data-phone]) .empty-bar {
    padding: var(--sp-1) var(--sp-1) var(--sp-1) var(--sp-3);
  }
  :global([data-phone]) .empty-btn {
    height: 36px;
    padding: 0 var(--sp-3);
  }
  :global([data-phone]) .follow-note {
    padding: var(--sp-1) var(--sp-1) var(--sp-1) var(--sp-3);
  }
  :global([data-phone]) .follow-note-link {
    min-height: 36px;
    padding: 0 var(--sp-1);
  }
  :global([data-phone]) .follow-note-dismiss {
    width: 36px;
    height: 36px;
  }
  :global([data-phone]) .load-more {
    min-height: 48px;
  }

  /* The last row clears the assistant's button, which floats over the end
     of the list. */
  :global([data-phone]) .thread-rows {
    padding-bottom: var(--fab-clear);
  }
  /* "Not needed" on a follow-up, in a gutter wide enough to hit. */
  :global([data-phone]) .row.follow {
    padding-right: calc(var(--sp-2) + 44px);
  }
  :global([data-phone]) .follow-done {
    width: 44px;
    height: 44px;
    right: 0;
  }

  /* Picked rows' bar: the keyboard's hints have no place, and each action
     is a button of its own to press. */
  :global([data-phone]) .bulk {
    gap: var(--sp-1) var(--sp-2);
    margin: 0 var(--sp-2) var(--sp-2);
  }
  :global([data-phone]) .bulk-hint {
    display: none;
  }
  :global([data-phone]) .bulk-count {
    order: 0;
    padding-left: var(--sp-1);
  }
  :global([data-phone]) .bulk-clear {
    order: 1;
    width: 44px;
    height: 44px;
    margin: -8px -4px -8px 0;
  }
  /* Under the count, one strip of buttons, scrolled sideways if there are
     more than fit -- a strip meant to. */
  :global([data-phone]) .bulk-actions {
    order: 2;
    display: flex;
    flex: 1 1 100%;
    gap: var(--sp-2);
    margin: 0 calc(var(--sp-2) * -1);
    padding: 0 var(--sp-2) var(--sp-1);
    overflow-x: auto;
    scrollbar-width: none;
  }
  :global([data-phone]) .bulk-actions .btn {
    flex: none;
  }

  /* The open thread: its own bar, then the subject, then the messages. */
  :global([data-phone]) .phone-bar {
    display: flex;
    align-items: center;
    min-height: 52px;
    padding: var(--sp-1) var(--sp-2);
    border-bottom: 1px solid var(--border);
  }
  :global([data-phone]) .phone-bar .tool {
    width: 44px;
    height: 44px;
    border-radius: 50%;
  }
  :global([data-phone]) .phone-bar .tool:active {
    background: var(--bg-hover);
  }
  :global([data-phone]) .subject-block {
    padding: var(--sp-4) var(--sp-4) var(--sp-2);
  }
  :global([data-phone]) .subject-block h1 {
    font-size: var(--text-lg);
  }
  :global([data-phone]) .snoozed-banner .link,
  :global([data-phone]) .meta-link {
    min-height: 32px;
    padding: 0 var(--sp-1);
  }
  :global([data-phone]) .summary-panel {
    max-height: 28vh;
    overflow-y: auto;
    padding: var(--sp-2) var(--sp-2) var(--sp-2) var(--sp-4);
  }
  :global([data-phone]) .summary-panel .close {
    width: 40px;
    height: 40px;
    margin: -8px 0;
  }
  :global([data-phone]) .main .scroll {
    padding-bottom: var(--fab-clear);
    overscroll-behavior: contain;
  }

  /* Writing a reply or a forward: a screen over the thread, as a new message
     is over the list. The thread under it is hidden, not taken out, so it is
     as it was when the reply goes. */
  :global([data-phone]) .main.replying .thread-head,
  :global([data-phone]) .main.replying .summary-panel,
  :global([data-phone]) .main.replying .scroll > :global(:not(.inline-reply)) {
    visibility: hidden;
  }
  :global([data-phone]) .inline-reply {
    position: absolute;
    inset: 0;
    z-index: 5;
    display: flex;
    flex-direction: column;
    padding: 0;
  }

  /* A phone's row is three lines, not the desktop's two squeezed: who and
     when, then the subject, then the start of the message, with a category
     or follow-up word wrapping under rather than taking width from them. */
  :global([data-phone]) .gist {
    flex-wrap: wrap;
  }
  :global([data-phone]) .words {
    display: flex;
    flex: 1 1 100%;
    flex-direction: column;
    gap: 1px;
    overflow: visible;
    white-space: normal;
  }
  :global([data-phone]) .subject,
  :global([data-phone]) .snippet {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  :global([data-phone]) .snippet {
    margin-left: 0;
  }
</style>
