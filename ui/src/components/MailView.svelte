<script lang="ts">
  // The Mail app's main pane: the thread list, `VirtualList`-backed so a
  // hundred-thousand-message mailbox costs the same DOM as a hundred, and
  // the reading pane beside it -- the journal's `EntryList` + `Editor` shape,
  // folded into one component the way the library and the todo app do.

  import { accounts } from '../lib/accounts.svelte'
  import {
    CATEGORY_TABS,
    categoryTabCount,
    formatSenders,
    isSnoozedMailbox,
    mailboxDisplayName,
    recentActionLine,
    snoozedUntilLabel,
    threadListDate,
    weekdayAndTime,
    withDateSections,
    type ListRow,
  } from '../lib/mail'
  import { mail } from '../lib/mail.svelte'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { compactCount, plural, relativeTime } from '../lib/format'
  import type { Mailbox, MailAddress, MailCategory, Thread, ThreadId } from '../lib/types'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import MailCompose from './MailCompose.svelte'
  import MailLabelPicker from './MailLabelPicker.svelte'
  import MailScheduled from './MailScheduled.svelte'
  import MailSnoozePicker from './MailSnoozePicker.svelte'
  import MailThread from './MailThread.svelte'
  import PaneResizer from './PaneResizer.svelte'
  import VirtualList from './VirtualList.svelte'

  void mail.start()

  const heading = $derived(
    mail.viewingScheduledFor
      ? 'Scheduled'
      : mail.mailbox
        ? mailboxDisplayName(mail.mailbox)
        : 'Mail',
  )

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
    <span class="heading">{heading}</span>
    <button
      class="plus"
      title="Compose (C)"
      aria-label="Compose"
      onclick={() => void mail.compose()}
    >
      <Icon name="plus" size={16} />
    </button>
  </div>

  {#if mail.viewingScheduledFor}
    <MailScheduled accountId={mail.viewingScheduledFor} />
  {:else}
    {#if showTabs && !mail.searchQuery.trim()}
      <!-- (p) TODO: `Tab`/`Shift+Tab` move between these -- see
           `shortcuts.svelte.ts`'s own note on why that key was free to take. -->
      <div class="tabs" role="tablist" aria-label="Mail categories">
        {#each [{ key: null, label: 'All' }, ...CATEGORY_TABS] as tab (tab.key ?? 'all')}
          {@const count = categoryTabCount(mail.categoryCounts, tab.key)}
          <button
            class="tab"
            role="tab"
            aria-selected={mail.category === tab.key}
            class:sel={mail.category === tab.key}
            title={count.threads > 0
              ? `${plural(count.threads, 'conversation')}, ${count.unread.toLocaleString()} unread`
              : undefined}
            onclick={() => mail.setCategory(tab.key)}
          >
            {tab.label}
            {#if count.threads > 0}
              <span class="tab-count" class:has-unread={count.unread > 0}
                >{compactCount(count.threads)}</span
              >
            {/if}
          </button>
        {/each}
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
        {#each searchRows as row (rowKey(row))}
          {#if row.type === 'header'}
            <div class="date-section" aria-hidden="true">{row.label}</div>
          {:else}
            {@const t = threadOf(row)}
            <button
              class="row"
              class:checked={mail.checked.has(t.id)}
              onmousedown={noTextRange}
              onclick={(e) => clickRow(e, t.id)}
              oncontextmenu={(e) => menu.show(e, menuFor(t))}
            >
              {#if mail.checked.has(t.id)}
                <span class="tick" aria-hidden="true"><Icon name="tick" size={10} /></span>
              {:else}
                <span class="dot" aria-hidden="true"></span>
              {/if}
              <div class="body">
                <div class="line1">
                  <span class="from">{formatSenders(t.participants)}</span>
                  {#if t.category === 'priority'}
                    <span class="priority-mark" title="Priority"
                      ><Icon name="flag" size={12} /></span
                    >
                  {/if}
                  {#if t.starred}
                    <Icon name="star" size={12} />
                  {/if}
                  {#if t.hasAttachments}
                    <Icon name="paperclip" size={12} />
                  {/if}
                  <span class="date">{threadListDate(t.lastDate)}</span>
                </div>
                <div class="line2">
                  <span class="subject">{t.subject || '(no subject)'}</span>
                  {#if t.snippet}<span class="snippet">— {t.snippet}</span>{/if}
                </div>
              </div>
            </button>
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
              {@const t = threadOf(row)}
              <button
                class="row"
                class:sel={mail.selectedThread === t.id}
                class:checked={mail.checked.has(t.id)}
                class:unread={t.unreadCount > 0}
                onmousedown={noTextRange}
                onclick={(e) => clickRow(e, t.id)}
                oncontextmenu={(e) => menu.show(e, menuFor(t))}
              >
                {#if mail.checked.has(t.id)}
                  <span class="tick" aria-hidden="true"><Icon name="tick" size={10} /></span>
                {:else}
                  <span class="dot" class:on={t.unreadCount > 0} aria-hidden="true"></span>
                {/if}
                <div class="body">
                  <div class="line1">
                    <span class="from">{formatSenders(t.participants)}</span>
                    {#if t.category === 'priority'}
                      <span class="priority-mark" title="Priority"
                        ><Icon name="flag" size={12} /></span
                      >
                    {/if}
                    {#if t.starred}
                      <Icon name="star" size={12} />
                    {/if}
                    {#if t.hasAttachments}
                      <Icon name="paperclip" size={12} />
                    {/if}
                    <span class="date">
                      {inSnoozedView && t.snoozedUntil
                        ? snoozedUntilLabel(t.snoozedUntil)
                        : threadListDate(t.lastDate)}
                    </span>
                  </div>
                  <div class="line2">
                    <span class="subject">{t.subject || '(no subject)'}</span>
                    {#if t.snippet}<span class="snippet">— {t.snippet}</span>{/if}
                    {#if t.messageCount > 1}<span class="count">{t.messageCount}</span>{/if}
                  </div>
                </div>
              </button>
            {/if}
          {/snippet}
        </VirtualList>
      </div>
    {/if}
  {/if}
  <PaneResizer
    cssVar="--mail-list-w"
    storageKey="pane-w:mail-list"
    defaultWidth={366}
    min={260}
    max={640}
  />
</section>

<main class="main">
  {#if mail.checked.size > 0}
    <div class="bulk">
      <h2>{plural(mail.checked.size, 'conversation')} selected</h2>
      <div class="bulk-actions">
        <button class="bulk-btn" onclick={() => void mail.archiveMany(mail.targets)}>
          <Icon name="layers" size={14} /> Archive <kbd>E</kbd>
        </button>
        <button class="bulk-btn" onclick={() => void mail.trashMany(mail.targets)}>
          <Icon name="trash" size={14} /> Trash <kbd>#</kbd>
        </button>
        {#if pickedThreads.some((t) => t.unreadCount > 0)}
          <button class="bulk-btn" onclick={() => void mail.markReadMany(mail.targets)}>
            <Icon name="check" size={14} /> Mark read <kbd>I</kbd>
          </button>
        {:else}
          <button class="bulk-btn" onclick={() => void mail.markUnreadMany(mail.targets)}>
            <Icon name="check" size={14} /> Mark unread <kbd>U</kbd>
          </button>
        {/if}
        {#if !inSnoozedView}
          <button class="bulk-btn" onclick={() => (mail.wantsSnooze = mail.targets)}>
            <Icon name="clock" size={14} /> Snooze… <kbd>H</kbd>
          </button>
        {/if}
        <button class="bulk-btn" onclick={(e) => menu.show(e, bulkMenu())}> More… </button>
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
    <div class="thread-head">
      <div class="thread-head-row">
        <button class="back" onclick={() => mail.closeThread()} title="Back to the list (Escape)">
          <Icon name="chevron" size={14} />
        </button>
        <h1>{open.thread.subject || '(no subject)'}</h1>
        {#if open.thread.category === 'priority'}
          <span class="priority-mark" title="Priority">
            <Icon name="flag" size={12} />
            Priority
          </span>
        {/if}
        <span class="count">{plural(open.messages.length, 'message')}</span>
        <div class="thread-actions">
          <button
            class="icon-btn"
            title="Archive (E)"
            aria-label="Archive"
            onclick={() => void mail.archive(open.thread.id)}
          >
            <Icon name="layers" size={14} />
          </button>
          <button
            class="icon-btn"
            title="Snooze… (H)"
            aria-label="Snooze"
            onclick={() => (mail.wantsSnooze = [open.thread.id])}
          >
            <Icon name="clock" size={14} />
          </button>
          <button
            class="icon-btn"
            class:on={open.thread.category === 'priority'}
            title={open.thread.category === 'priority'
              ? 'Remove from priority (!)'
              : 'Mark as priority (!)'}
            aria-label="Toggle priority"
            onclick={() =>
              void mail.setPriority(open.thread.id, open.thread.category !== 'priority')}
          >
            <Icon name="flag" size={14} />
          </button>
          <button
            class="icon-btn"
            title="Mark unread (U)"
            aria-label="Mark unread"
            onclick={() => void mail.markUnread(open.thread.id)}
          >
            <Icon name="check" size={14} />
          </button>
          <button
            class="icon-btn"
            title="Trash (#)"
            aria-label="Trash"
            onclick={() => void mail.trash(open.thread.id)}
          >
            <Icon name="trash" size={14} />
          </button>
        </div>
        {#if canSummarize}
          <button
            class="summarize"
            title="Summarise (Z)"
            disabled={mail.summarizing}
            onclick={() => void mail.summarizeOpenThread()}
          >
            <Icon name="sparkle" size={13} />
            {mail.summarizing ? 'Summarising…' : 'Summarise'}
          </button>
        {/if}
      </div>
      {#if open.thread.snoozedUntil && new Date(open.thread.snoozedUntil) > new Date()}
        <p class="snoozed-banner">
          <Icon name="clock" size={13} />
          <span>Snoozed until {weekdayAndTime(open.thread.snoozedUntil)}</span>
          <button class="link" onclick={() => void mail.unsnooze(open.thread.id)}>Unsnooze</button>
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
            <MailCompose draft={mail.composing} inline onclose={() => mail.closeCompose()} />
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

{#if mail.composing && !mail.composeInline}
  <MailCompose draft={mail.composing} onclose={() => mail.closeCompose()} />
{/if}

{#if mail.wantsSnooze}
  <MailSnoozePicker onchoose={snooze} oncancel={() => (mail.wantsSnooze = null)} />
{/if}

{#if mail.wantsLabel}
  <MailLabelPicker onchoose={applyLabel} oncancel={() => (mail.wantsLabel = null)} />
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
    background: var(--bg-panel);
    border-right: 1px solid var(--border);
  }
  .top {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: var(--header-h);
    padding: 0 var(--sp-3) 0 var(--sp-4);
    flex: none;
    border-bottom: 1px solid var(--border);
  }
  .heading {
    flex: 1;
    min-width: 0;
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.006em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .plus {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .plus:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .hint {
    padding: var(--sp-4);
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }
  .operator-hint {
    padding: var(--sp-2) var(--sp-4);
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

  /* A grouping heading over a run of rows -- never a button, never
     focusable: `j`/`k` and a row's own context menu read `mail.threads`
     directly, which never contains one of these, so there is nothing here
     for either to skip over by accident. */
  .date-section {
    padding: var(--sp-2) var(--sp-3) 4px;
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.02em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }

  .row {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    width: 100%;
    padding: var(--sp-2) var(--sp-3);
    text-align: left;
    border-bottom: 1px solid var(--border);
  }
  .row:hover {
    background: var(--bg-hover);
  }
  .row.sel {
    background: var(--bg-active);
  }
  /* Picked, one of several -- tinted with the accent rather than the plain
     selection grey, so a picked row and the cursor row read apart. */
  .row.checked {
    background: color-mix(in oklab, var(--accent) 13%, transparent);
  }
  .row.checked:hover {
    background: color-mix(in oklab, var(--accent) 18%, transparent);
  }
  .tick {
    flex: none;
    display: grid;
    place-items: center;
    width: 13px;
    height: 13px;
    margin: 3px -3px 0;
    border-radius: 4px;
    background: var(--accent);
    color: var(--bg);
  }
  .dot {
    flex: none;
    width: 7px;
    height: 7px;
    margin-top: 6px;
    border-radius: 50%;
    background: transparent;
  }
  .dot.on {
    background: var(--accent);
  }
  .body {
    flex: 1;
    min-width: 0;
    display: grid;
    gap: 2px;
  }
  .line1,
  .line2 {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    overflow: hidden;
  }
  .line1 :global(svg),
  .line2 :global(svg) {
    flex: none;
    color: var(--fg-faint);
  }
  /* Accent-coloured, against the rule above that would otherwise read every
     icon in these two lines as `--fg-faint` -- equal specificity to that
     rule, so this one wins only by being declared after it; moving it
     ahead of `.line1 :global(svg)` would silently lose the colour again. */
  .priority-mark {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    flex: none;
    color: var(--journal-accent, var(--accent));
    font-size: var(--text-xs);
    font-weight: 600;
  }
  .priority-mark :global(svg) {
    color: var(--journal-accent, var(--accent));
  }
  .snippet {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg-faint);
  }
  .from {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .row.unread .from,
  .row.unread .subject {
    color: var(--fg);
    font-weight: 650;
  }
  .date {
    flex: none;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .subject {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .line2 .count {
    flex: none;
    margin-left: auto;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }

  /* Six tabs and their counts are wider than the list column at its
     default width. They wrap to a second row rather than scroll: a strip
     scrolled sideways, with its bar hidden, kept the last two tabs -- and
     their counts -- out of sight with nothing to say they were there. */
  .tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 2px;
    padding: var(--sp-1) var(--sp-3);
    border-bottom: 1px solid var(--border);
  }
  .tab {
    flex: none;
    padding: 5px var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    color: var(--fg-faint);
  }
  .tab:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .tab.sel {
    background: var(--bg-active);
    color: var(--fg);
    font-weight: 550;
  }
  .tab-count {
    margin-left: 4px;
    padding: 0 5px;
    border-radius: 999px;
    background: var(--bg-hover);
    font-size: var(--text-xs);
    font-weight: 500;
    font-variant-numeric: tabular-nums;
    color: var(--fg-faint);
  }
  .tab.sel .tab-count {
    background: var(--bg-panel);
    color: var(--fg-muted);
  }
  /* Something unread under this tab: the accent, the way the unread dot on
     a row is. */
  .tab-count.has-unread {
    color: var(--journal-accent, var(--accent));
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
    padding: 0 var(--sp-4);
    border-bottom: 1px solid var(--border);
  }
  .thread-head-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: var(--header-h);
  }
  .recent-actions {
    display: flex;
    gap: var(--sp-2);
    margin: 0;
    padding-bottom: var(--sp-2);
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .recent-actions .sep {
    opacity: 0.5;
  }
  .back {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
    transform: rotate(180deg);
  }
  .back:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .thread-head h1 {
    flex: 1;
    min-width: 0;
    font-size: var(--text-md);
    font-weight: 620;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .thread-head .count {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  /* The reading pane's own compact action bar -- archive, snooze, priority,
     mark unread, trash -- ahead of Summarise, which keeps its own larger,
     labelled button rather than joining this row: it is the one action
     here that is not also on `h`/`e`/`u`/`!`/`#`'s own list, so it reads as
     a distinct offer rather than a sixth icon indistinguishable from the
     rest. */
  .thread-actions {
    flex: none;
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .icon-btn {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .icon-btn:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  /* The priority toggle's pressed state, in colour alone: `Icon`'s
     `filled` swaps stroke for fill, and the flag's pole is a bare line with
     no area to fill, so the filled flag drew as a floating orange block. */
  .icon-btn.on {
    color: var(--journal-accent, var(--accent));
    background: color-mix(in oklab, var(--accent) 12%, transparent);
  }
  .snoozed-banner {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin: 0;
    padding-bottom: var(--sp-2);
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }
  .snoozed-banner span {
    flex: 1;
  }
  .snoozed-banner .link {
    color: var(--journal-accent, var(--accent));
    font-weight: 600;
  }
  .summarize {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 4px var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-xs);
    color: var(--journal-accent, var(--accent));
  }
  .summarize:hover {
    background: var(--bg-hover);
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
    padding: 0 var(--sp-4) var(--sp-4);
  }

  .thread-rows {
    flex: 1;
    min-height: 0;
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
  .bulk-btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    font-size: var(--text-sm);
    color: var(--fg);
  }
  .bulk-btn:hover {
    background: var(--bg-hover);
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
