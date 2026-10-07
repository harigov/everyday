<script lang="ts">
  // The Mail app's main pane: the thread list, `VirtualList`-backed so a
  // hundred-thousand-message mailbox costs the same DOM as a hundred, and
  // the reading pane beside it -- the journal's `EntryList` + `Editor` shape,
  // folded into one component the way the library and the todo app do.

  import { accounts } from '../lib/accounts.svelte'
  import { accountColor } from '../lib/avatar'
  import {
    CATEGORY_TABS,
    categoryTabCount,
    formatSenders,
    isSnoozedMailbox,
    isUnifiedMailbox,
    mailboxDisplayName,
    newestSyncedAt,
    recentActionLine,
    snippetText,
    snoozedUntilLabel,
    syncInProgress,
    threadListDate,
    unifiedMailboxId,
    weekdayAndTime,
    withDateSections,
    type ListRow,
  } from '../lib/mail'
  import { mail } from '../lib/mail.svelte'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { compactCount, plural, relativeTime } from '../lib/format'
  import type { IconName } from '../lib/icons'
  import type {
    AccountId,
    Mailbox,
    MailAddress,
    MailCategory,
    Thread,
    ThreadId,
  } from '../lib/types'
  import Avatar from './Avatar.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import MailCleanup from './MailCleanup.svelte'
  import MailCompose from './MailCompose.svelte'
  import MailLabelPicker from './MailLabelPicker.svelte'
  import MailScheduled from './MailScheduled.svelte'
  import MailSnoozePicker from './MailSnoozePicker.svelte'
  import MailThread from './MailThread.svelte'
  import PaneResizer from './PaneResizer.svelte'
  import VirtualList from './VirtualList.svelte'

  void mail.start()

  /** Is the list column one of the "All accounts" rows -- every account's
   *  threads in one list, so each row says whose it is. */
  const unifiedView = $derived(isUnifiedMailbox(mail.mailbox))

  const heading = $derived(
    mail.viewingScheduledFor
      ? 'Scheduled'
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
    if (mail.viewingScheduledFor) {
      const waiting = mail.scheduled.filter(
        (s) => s.draft.accountId === mail.viewingScheduledFor,
      ).length
      return waiting > 0 ? `${waiting.toLocaleString()} waiting to send` : ''
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
    mail.viewingScheduledFor ?? (mail.mailbox && !unifiedView ? mail.mailbox.accountId : null),
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

  /** Is the list column currently the Snoozed view -- the one place a row's
   *  own date column means "comes back" rather than "last arrived", and the
   *  one place "Unsnooze" belongs in a row's own menu (an ordinary mailbox
   *  never lists a currently-snoozed thread at all, per `mail.refresh`'s own
   *  `snoozed` filter). */
  const inSnoozedView = $derived(isSnoozedMailbox(mail.mailbox))

  /** The list column's own rows, headings interleaved -- never grouped in
   *  the Snoozed view, whose rows already read "comes back", not "arrived",
   *  which `dateSection`'s own headings have nothing to say about. */
  const listRows = $derived<ListRow[]>(
    inSnoozedView
      ? mail.threads.map((thread) => ({ type: 'thread', thread }))
      : withDateSections(mail.threads, (t) => t.lastDate),
  )
  const searchRows = $derived<ListRow[]>(withDateSections(mail.searchResults, (t) => t.lastDate))

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

  /** (p) TODO: only the inbox has a split to tab through -- see
   *  `docs/plans/mail.md`'s "Split inbox". Every other mailbox (Sent,
   *  Archive, a label) shows everything in it, uncategorised or not. */
  const showTabs = $derived(mail.mailbox?.role === 'inbox')

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

  function categoryMoveItems(ids: ThreadId[], current: MailCategory | null): MenuItem[] {
    return CATEGORY_TABS.map((tab) => ({
      label: tab.label,
      checked: current === tab.key,
      run: () => void mail.setCategoryForMany(ids, tab.key),
    }))
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
    const account = accounts.account(t.accountId)
    const mine = new Set(
      [account?.address, ...(account?.identities.map((i) => i.address) ?? [])]
        .filter((a): a is string => !!a)
        .map((a) => a.toLowerCase()),
    )
    const other = [...t.participants].reverse().find((p) => !mine.has(p.email.toLowerCase()))
    return other ?? t.participants.at(-1) ?? null
  }

  function rowMenu(t: Thread): MenuItem[] {
    const vip = vipSenderFor(t)
    return tidyMenu([
      { label: 'Open', icon: 'inbox', run: () => void mail.openThreadById(t.id) },
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
      SEP,
      { label: 'Archive', icon: 'layers', run: () => void mail.archive(t.id) },
      { label: 'Trash', icon: 'trash', danger: true, run: () => void mail.trash(t.id) },
    ])
  }

  /** The picked threads, in list order -- see `mail.checked`. */
  const pickedThreads = $derived(
    (mail.searchQuery.trim() ? mail.searchResults : mail.threads).filter((t) =>
      mail.checked.has(t.id),
    ),
  )

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
      SEP,
      { label: 'Archive', icon: 'layers', run: () => void mail.archiveMany(ids) },
      { label: 'Trash', icon: 'trash', danger: true, run: () => void mail.trashMany(ids) },
    ])
  }

  /** A row's own menu -- the bulk one when it is one of several picked. */
  function menuFor(t: Thread): MenuItem[] {
    return mail.checked.has(t.id) && mail.checked.size > 1 ? bulkMenu() : rowMenu(t)
  }

  /** A row clicked: Shift and Ctrl/Cmd pick, a plain click opens -- see
   *  `mail.clickThread`. */
  function clickRow(e: MouseEvent, id: ThreadId) {
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
   *  Paired with `{#key mail.composing.id}` in the template, so opening a
   *  *second* reply into the same thread (send, then reply again) remounts
   *  the element and this runs again, rather than firing only once per
   *  thread. */
  function scrollIntoViewOnMount(el: HTMLElement) {
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
</script>

<section class="list">
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
      onclick={() => (cleaning = true)}
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

  {#if mail.viewingScheduledFor}
    <MailScheduled accountId={mail.viewingScheduledFor} />
  {:else}
    {#if showTabs && !mail.searchQuery.trim()}
      <!-- The shared filter bar (app.css's "Filter bars"), as Todo's and the
           library's are: the open category named, the rest an icon and its
           count -- six labelled tabs and their counts are wider than the
           list column, and wrapped onto two rows they read as a paragraph
           rather than a control. Every chip is still named, in its title
           and to assistive technology, and its count turns the accent when
           anything under it is unread. -->
      <div class="tabs">
        <div class="filters" role="tablist" aria-label="Mail categories">
          {#each [{ key: null, label: 'All' }, ...CATEGORY_TABS] as tab (tab.key ?? 'all')}
            {@const count = categoryTabCount(mail.categoryCounts, tab.key)}
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
              {#if count.threads > 0}
                <span class="n" class:has-unread={count.unread > 0}
                  >{compactCount(count.threads)}</span
                >
              {/if}
            </button>
          {/each}
        </div>
      </div>
    {/if}

    {#if mail.searchQuery.trim()}
      <p class="hint operator-hint">
        Try <code>from:</code>, <code>to:</code>, <code>subject:</code>,
        <code>has:attachment</code>, <code>before:</code>, <code>after:</code>, <code>in:</code>
        or <code>is:unread</code>.
      </p>
      {#if mail.searching && mail.searchResults.length === 0}
        <p class="hint">Searching…</p>
      {:else if mail.searchResults.length === 0}
        <p class="hint">Nothing matched “{mail.searchQuery}”.</p>
      {:else}
        <!-- `SearchMailResult.threads` is the same `Thread` shape every other
             row already draws from -- see `types.ts`'s own doc on why a
             search hit is not a narrower type of its own. -->
        <div class="scroll thread-rows">
          {#each searchRows as row (rowKey(row))}
            {#if row.type === 'header'}
              <div class="date-section" aria-hidden="true">{row.label}</div>
            {:else}
              {@render threadRow(threadOf(row))}
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
    {:else if mail.loading && mail.threads.length === 0}
      <p class="hint">Loading…</p>
    {:else if mail.threads.length === 0}
      <EmptyState lead="Nothing here.">
        {#snippet note()}This mailbox has no threads matching what is showing.{/snippet}
      </EmptyState>
    {:else}
      <!-- The scrolling parent `VirtualList` needs: `virtua` watches its
           container's parent for scroll, and `section.list` itself never
           scrolls, so without this the rows past the first screen were
           unreachable and `onEndReached` never fired. -->
      <div class="scroll thread-rows">
        <VirtualList
          items={listRows}
          getKey={rowKey}
          selectedId={mail.selectedThread}
          onEndReached={() => void mail.loadMore()}
        >
          {#snippet children(row: ListRow)}
            {#if row.type === 'header'}
              <div class="date-section" aria-hidden="true">{row.label}</div>
            {:else}
              {@render threadRow(threadOf(row))}
            {/if}
          {/snippet}
        </VirtualList>
      </div>
    {/if}
  {/if}
  <PaneResizer
    cssVar="--mail-list-w"
    storageKey="pane-w:mail-list"
    defaultWidth={380}
    min={280}
    max={640}
  />
</section>

<!-- One thread in a list -- the mailbox's own and a search's draw the same
     row. The avatar is whoever the thread is with (the newest voice that is
     not this account's own), a tick in its place once the row is picked;
     the unread dot sits in the gutter left of it, where the eye starts. -->
{#snippet threadRow(t: Thread)}
  {@const picked = mail.checked.has(t.id)}
  {@const other = vipSenderFor(t)}
  {@const preview = snippetText(t.snippet)}
  <button
    class="row list-row"
    class:sel={mail.selectedThread === t.id}
    class:checked={picked}
    class:unread={t.unreadCount > 0}
    onmousedown={noTextRange}
    onclick={(e) => clickRow(e, t.id)}
    oncontextmenu={(e) => menu.show(e, menuFor(t))}
  >
    <span class="dot" class:on={t.unreadCount > 0} aria-hidden="true"></span>
    {#if picked}
      <span class="tick" aria-hidden="true"><Icon name="tick" size={16} weight={2} /></span>
    {:else}
      <Avatar name={other?.name ?? ''} email={other?.email ?? ''} size={38} />
    {/if}
    <div class="body">
      <div class="line1">
        {#if unifiedView}
          <span
            class="acct"
            style:background={accountColor(t.accountId, accountIds)}
            title={accountLabel(t.accountId)}
          ></span>
        {/if}
        <span class="from">{formatSenders(t.participants)}</span>
        {#if t.hasAttachments}
          <span class="mark" title="Has an attachment"><Icon name="paperclip" size={13} /></span>
        {/if}
        <span class="date">
          {inSnoozedView && t.snoozedUntil
            ? snoozedUntilLabel(t.snoozedUntil)
            : threadListDate(t.lastDate)}
        </span>
      </div>
      <div class="line2">
        <span class="subject">{t.subject || '(no subject)'}</span>
        {#if t.category === 'priority'}
          <span class="mark priority" title="Priority"><Icon name="flag" size={13} /></span>
        {/if}
        {#if t.starred}
          <span class="mark starred" title="Starred"><Icon name="star" size={13} filled /></span>
        {/if}
        {#if t.messageCount > 1}<span class="count">{t.messageCount}</span>{/if}
      </div>
      {#if preview}<p class="snippet">{preview}</p>{/if}
    </div>
  </button>
{/snippet}

<main class="main">
  {#if mail.composing && !mail.composeInline}
    <!-- A new message, or a draft reopened, where a thread is read rather
         than in a dialog over everything: the list stays in reach beside
         it. Keyed, so a second draft opened over the first is a fresh sheet
         rather than the first one's working copy under another name. -->
    {#key mail.composing.id}
      <MailCompose draft={mail.composing} onclose={() => mail.closeCompose()} />
    {/key}
  {:else if mail.checked.size > 0}
    <div class="bulk">
      <h2>{plural(mail.checked.size, 'conversation')} selected</h2>
      <div class="bulk-actions">
        <button class="btn btn-outline" onclick={() => void mail.archiveMany(mail.targets)}>
          <Icon name="layers" size={14} /> Archive <kbd>E</kbd>
        </button>
        <button class="btn btn-outline" onclick={() => void mail.trashMany(mail.targets)}>
          <Icon name="trash" size={14} /> Trash <kbd>#</kbd>
        </button>
        {#if pickedThreads.some((t) => t.unreadCount > 0)}
          <button class="btn btn-outline" onclick={() => void mail.markReadMany(mail.targets)}>
            <Icon name="check" size={14} /> Mark read <kbd>I</kbd>
          </button>
        {:else}
          <button class="btn btn-outline" onclick={() => void mail.markUnreadMany(mail.targets)}>
            <Icon name="check" size={14} /> Mark unread <kbd>U</kbd>
          </button>
        {/if}
        {#if !inSnoozedView}
          <button class="btn btn-outline" onclick={() => (mail.wantsSnooze = mail.targets)}>
            <Icon name="clock" size={14} /> Snooze… <kbd>H</kbd>
          </button>
        {/if}
        <button class="btn btn-outline" onclick={(e) => menu.show(e, bulkMenu())}> More… </button>
      </div>
      <p class="bulk-hint">
        Shift-click picks a run of threads, {navigator.userAgent.includes('Mac')
          ? '⌘'
          : 'Ctrl'}-click adds or removes one, and <kbd>X</kbd> picks the one under the cursor.
      </p>
      <button class="link" onclick={() => mail.clearChecked()}>Clear selection (Esc)</button>
    </div>
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
          <button
            class="tool"
            title="Trash (#)"
            aria-label="Trash"
            onclick={() => void mail.trash(open.thread.id)}
          >
            <Icon name="trash" size={17} />
          </button>
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
        </div>
        <span class="spacer"></span>
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
        </p>
        {#if open.thread.snoozedUntil && new Date(open.thread.snoozedUntil) > new Date()}
          <p class="snoozed-banner">
            <Icon name="clock" size={13} />
            <span>Snoozed until {weekdayAndTime(open.thread.snoozedUntil)}</span>
            <button class="link" onclick={() => void mail.unsnooze(open.thread.id)}>Unsnooze</button
            >
          </p>
        {/if}
        {#if recentActionLines.length > 0}
          <p class="recent-actions">
            {#each recentActionLines as line, i (i)}
              {#if i > 0}<span class="sep">·</span>{/if}<span>{line}</span>
            {/each}
          </p>
        {/if}
      </div>
    </div>
    {#if mail.summary && mail.summary.threadId === open.thread.id}
      <div class="summary-panel">
        <Icon name="sparkle" size={14} />
        <p>{mail.summary.text}</p>
        <button class="close" aria-label="Dismiss the summary" onclick={() => mail.dismissSummary()}
          ><Icon name="close" size={13} /></button
        >
      </div>
    {/if}
    <div class="scroll">
      <MailThread messages={open.messages} expanded={mail.expanded} />
      {#if mail.composing && mail.composeInline}
        {#key mail.composing.id}
          <div class="inline-reply" use:scrollIntoViewOnMount>
            <MailCompose
              draft={mail.composing}
              placement="thread"
              onclose={() => mail.closeCompose()}
            />
          </div>
        {/key}
      {/if}
    </div>
  {:else}
    <EmptyState lead={mail.viewingScheduledFor ? 'Nothing open' : 'Select a thread'}>
      {#snippet note()}
        {mail.viewingScheduledFor
          ? 'Edit, reschedule, send now or cancel a message from the list.'
          : 'j and k move, Enter opens.'}
      {/snippet}
    </EmptyState>
  {/if}
</main>

{#if mail.wantsSnooze}
  <MailSnoozePicker onchoose={snooze} oncancel={() => (mail.wantsSnooze = null)} />
{/if}

{#if mail.wantsLabel}
  <MailLabelPicker onchoose={applyLabel} oncancel={() => (mail.wantsLabel = null)} />
{/if}

{#if cleaning}
  <MailCleanup accountId={scopeAccount} onclose={() => (cleaning = false)} />
{/if}

{#if mail.sendingUndo}
  <div class="undo-toast">
    <span>
      {#if mail.sendingUndo.scheduled}
        <!-- A "send later" window can be hours out -- a countdown in seconds
             would either read as an absurd number or, worse, look wrong the
             instant it started (Bug 11). `relativeTime` gives the same
             "in 3 hours" shape the rest of the app already uses. -->
        Scheduled — sending {relativeTime(new Date(mail.sendingUndo.at).toISOString())}
      {:else}
        Sending in {mail.sendingUndo.secondsLeft}s…
      {/if}
    </span>
    <button class="link" onclick={() => void mail.undoSend()}>Undo</button>
  </div>
{/if}

<style>
  .list {
    position: relative;
    /* Mail's own variable -- `--list-w` is shared with the journal's and
       the calendar's lists, which a drag here must leave alone. */
    width: var(--mail-list-w);
    flex: none;
    display: flex;
    flex-direction: column;
    border-right: 1px solid var(--border);
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

  /* ── Rows ───────────────────────────────────────────────────────── */

  .thread-rows {
    flex: 1;
    min-height: 0;
    padding-bottom: var(--sp-2);
  }

  /* A grouping heading over a run of rows -- never a button, never
     focusable: `j`/`k` and a row's own context menu read `mail.threads`
     directly, which never contains one of these, so there is nothing here
     for either to skip over by accident. */
  .date-section {
    padding: var(--sp-4) var(--sp-5) 6px;
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }

  /* Each row is the shared `.list-row` card (app.css's "Lists"); what is
     here is the thread row's own layout inside it. */
  .row {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    padding: var(--sp-3) var(--sp-3) var(--sp-3) var(--sp-5);
  }
  /* The hairline between rows, starting under the text rather than the
     avatar -- the column of faces stays one unbroken edge. */
  .row::after {
    content: '';
    position: absolute;
    left: calc(var(--sp-5) + 38px + var(--sp-3));
    right: var(--sp-3);
    bottom: 0;
    height: 1px;
    background: var(--border);
  }
  .row.sel::after,
  .row:hover::after {
    opacity: 0;
  }
  /* Picked, one of several: the same tint as the open row, a shade deeper,
     with the tick standing in for the avatar. */
  .row.checked {
    background: color-mix(in oklab, var(--accent) 18%, transparent);
  }
  .row.checked::after {
    opacity: 0;
  }
  .dot {
    position: absolute;
    left: 7px;
    top: calc(var(--sp-3) + 16px);
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: transparent;
  }
  .dot.on {
    background: var(--accent);
  }
  .tick {
    flex: none;
    display: grid;
    place-items: center;
    width: 38px;
    height: 38px;
    border-radius: 50%;
    background: var(--accent);
    color: var(--fg-on-accent);
  }
  .body {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .line1,
  .line2 {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .acct {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
  }
  .from {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-base);
    font-weight: 560;
    color: var(--fg);
  }
  .row.unread .from {
    font-weight: 700;
  }
  .mark {
    flex: none;
    display: grid;
    place-items: center;
    color: var(--fg-faint);
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
    color: var(--accent);
    font-weight: 650;
  }
  .subject {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    font-weight: 500;
    color: var(--fg-muted);
  }
  .row.unread .subject {
    font-weight: 650;
    color: var(--fg);
  }
  /* How many messages, as a quiet pill rather than a bare number that
     could be read as a count of something else. */
  .line2 .count {
    flex: none;
    min-width: 20px;
    padding: 1px 6px;
    border-radius: 999px;
    background: var(--bg-sunken);
    font-size: 11px;
    font-weight: 600;
    text-align: center;
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .row.sel .line2 .count {
    background: var(--bg-raised);
  }
  /* Two lines of the message, not one squeezed in beside the subject: enough
     to tell a question from an announcement without opening either. */
  .snippet {
    margin: 2px 0 0;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    font-size: var(--text-sm);
    line-height: 1.4;
    color: var(--fg-subtle);
    overflow-wrap: anywhere;
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

  .main {
    flex: 1;
    /* Never crushed to nothing by the list column beside it growing --
       `PaneResizer`'s own `max` on `.list` already stops short of most
       window widths, but a narrow window plus a list dragged wide should
       still leave a reading pane rather than squeeze it to a sliver. */
    min-width: 360px;
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
    border-radius: 999px;
    background: var(--bg-sunken);
  }
  .tool {
    display: grid;
    place-items: center;
    width: 36px;
    height: 32px;
    border-radius: 999px;
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
    border-radius: 999px;
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
    border-radius: 999px;
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
  .inline-reply {
    padding: 0 var(--sp-6) var(--sp-6);
  }

  .bulk {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--sp-3);
    margin: auto;
    padding: var(--sp-6);
    max-width: 520px;
    text-align: center;
  }
  .bulk h2 {
    font-size: var(--text-lg);
    font-weight: 620;
  }
  .bulk-actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: var(--sp-2);
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
    margin: 0;
    font-size: var(--text-xs);
    color: var(--fg-faint);
    line-height: var(--leading-normal);
  }
  .bulk .link {
    font-size: var(--text-sm);
    color: var(--journal-accent, var(--accent));
    font-weight: 600;
  }

  .undo-toast {
    position: fixed;
    left: 50%;
    bottom: var(--sp-8);
    translate: -50% 0;
    z-index: 60;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-2) var(--sp-4);
    border-radius: 999px;
    background: var(--bg-raised);
    border: 1px solid var(--border);
    box-shadow: var(--shadow-lg);
    font-size: var(--text-sm);
  }
  .undo-toast .link {
    color: var(--journal-accent, var(--accent));
    font-weight: 650;
  }
</style>
