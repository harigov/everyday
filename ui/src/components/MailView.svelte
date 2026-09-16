<script lang="ts">
  // The Mail app's main pane: the thread list, `VirtualList`-backed so a
  // hundred-thousand-message mailbox costs the same DOM as a hundred, and
  // the reading pane beside it -- the journal's `EntryList` + `Editor` shape,
  // folded into one component the way the library and the todo app do.

  import { accounts } from '../lib/accounts.svelte'
  import { CATEGORY_TABS, formatSenders, recentActionLine, threadListDate } from '../lib/mail'
  import { mail } from '../lib/mail.svelte'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { plural, relativeTime } from '../lib/format'
  import type { Mailbox, Thread } from '../lib/types'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import MailCompose from './MailCompose.svelte'
  import MailLabelPicker from './MailLabelPicker.svelte'
  import MailSnoozePicker from './MailSnoozePicker.svelte'
  import MailThread from './MailThread.svelte'
  import VirtualList from './VirtualList.svelte'

  void mail.start()

  const heading = $derived(mail.mailbox?.remoteName ?? 'Mail')

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

  function categoryMoveItems(t: Thread): MenuItem[] {
    return CATEGORY_TABS.map((tab) => ({
      label: tab.label,
      checked: t.category === tab.key,
      run: () => void mail.setCategoryFor(t.id, tab.key),
    }))
  }

  function moveMailboxItems(t: Thread): MenuItem[] {
    return mail.mailboxes
      .filter((m) => m.accountId === t.accountId && m.role !== 'other')
      .map((box: Mailbox) => ({ label: box.remoteName, run: () => void mail.moveTo(t.id, box.id) }))
  }

  function rowMenu(t: Thread): MenuItem[] {
    return tidyMenu([
      { label: 'Open', icon: 'inbox', run: () => void mail.openThreadById(t.id) },
      {
        label: t.unreadCount > 0 ? 'Mark read' : 'Mark unread',
        icon: 'check',
        run: () => void (t.unreadCount > 0 ? mail.markRead(t.id) : mail.markUnread(t.id)),
      },
      { label: t.starred ? 'Unstar' : 'Star', icon: 'star', run: () => void mail.toggleStar(t.id) },
      { label: 'Snooze…', icon: 'clock', run: () => (mail.wantsSnooze = t.id) },
      { label: 'Label…', icon: 'tag', run: () => (mail.wantsLabel = t.id) },
      { label: 'Move to…', icon: 'layers', items: moveMailboxItems(t) },
      { label: 'Move to category', icon: 'inbox', items: categoryMoveItems(t) },
      SEP,
      { label: 'Archive', icon: 'layers', run: () => void mail.archive(t.id) },
      { label: 'Trash', icon: 'trash', danger: true, run: () => void mail.trash(t.id) },
    ])
  }

  function snooze(at: Date) {
    const id = mail.wantsSnooze
    mail.wantsSnooze = null
    if (id) void mail.snooze(id, at)
  }

  function applyLabel(labelName: string) {
    const id = mail.wantsLabel
    mail.wantsLabel = null
    if (id) void mail.label(id, labelName)
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

  {#if showTabs && !mail.searchQuery.trim()}
    <!-- (p) TODO: `Tab`/`Shift+Tab` move between these -- see
         `shortcuts.svelte.ts`'s own note on why that key was free to take. -->
    <div class="tabs" role="tablist" aria-label="Mail categories">
      <button
        class="tab"
        role="tab"
        aria-selected={mail.category === null}
        class:sel={mail.category === null}
        onclick={() => mail.setCategory(null)}
      >
        All
      </button>
      {#each CATEGORY_TABS as tab (tab.key)}
        <button
          class="tab"
          role="tab"
          aria-selected={mail.category === tab.key}
          class:sel={mail.category === tab.key}
          onclick={() => mail.setCategory(tab.key)}
        >
          {tab.label}
        </button>
      {/each}
    </div>
  {/if}

  {#if mail.searchQuery.trim()}
    <p class="hint operator-hint">
      Try <code>from:</code>, <code>to:</code>, <code>subject:</code>, <code>has:attachment</code>,
      <code>before:</code>, <code>after:</code>, <code>in:</code> or <code>is:unread</code>.
    </p>
    {#if mail.searching && mail.searchResults.length === 0}
      <p class="hint">Searching…</p>
    {:else if mail.searchResults.length === 0}
      <p class="hint">Nothing matched “{mail.searchQuery}”.</p>
    {:else}
      <!-- `SearchMailResult.threads` is the same `Thread` shape every other
           row already draws from -- see `types.ts`'s own doc on why a
           search hit is not a narrower type of its own. -->
      {#each mail.searchResults as t (t.id)}
        <button class="row" onclick={() => void mail.openThreadById(t.id)}>
          <span class="dot" aria-hidden="true"></span>
          <div class="body">
            <div class="line1">
              <span class="from">{formatSenders(t.participants)}</span>
              {#if t.starred}
                <Icon name="star" size={12} />
              {/if}
              {#if t.hasAttachments}
                <Icon name="tag" size={12} />
              {/if}
              <span class="date">{threadListDate(t.lastDate)}</span>
            </div>
            <div class="line2">
              <span class="subject">{t.subject || '(no subject)'}</span>
              {#if t.snippet}<span class="snippet">— {t.snippet}</span>{/if}
            </div>
          </div>
        </button>
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
        items={mail.threads}
        selectedId={mail.selectedThread}
        onEndReached={() => void mail.loadMore()}
      >
        {#snippet children(t: Thread)}
          <button
            class="row"
            class:sel={mail.selectedThread === t.id}
            class:unread={t.unreadCount > 0}
            onclick={() => void mail.openThreadById(t.id)}
            oncontextmenu={(e) => menu.show(e, rowMenu(t))}
          >
            <span class="dot" class:on={t.unreadCount > 0} aria-hidden="true"></span>
            <div class="body">
              <div class="line1">
                <span class="from">{formatSenders(t.participants)}</span>
                {#if t.starred}
                  <Icon name="star" size={12} />
                {/if}
                {#if t.hasAttachments}
                  <Icon name="tag" size={12} />
                {/if}
                <span class="date">{threadListDate(t.lastDate)}</span>
              </div>
              <div class="line2">
                <span class="subject">{t.subject || '(no subject)'}</span>
                {#if t.snippet}<span class="snippet">— {t.snippet}</span>{/if}
                {#if t.messageCount > 1}<span class="count">{t.messageCount}</span>{/if}
              </div>
            </div>
          </button>
        {/snippet}
      </VirtualList>
    </div>
  {/if}
</section>

<main class="main">
  {#if mail.openThread}
    <div class="thread-head">
      <div class="thread-head-row">
        <button class="back" onclick={() => mail.closeThread()} title="Back to the list (Escape)">
          <Icon name="chevron" size={14} />
        </button>
        <h1>{mail.openThread.thread.subject || '(no subject)'}</h1>
        <span class="count">{plural(mail.openThread.messages.length, 'message')}</span>
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
      {#if recentActionLines.length > 0}
        <p class="recent-actions">
          {#each recentActionLines as line, i (i)}
            {#if i > 0}<span class="sep">·</span>{/if}<span>{line}</span>
          {/each}
        </p>
      {/if}
    </div>
    {#if mail.summary && mail.summary.threadId === mail.openThread.thread.id}
      <div class="summary-panel">
        <Icon name="sparkle" size={14} />
        <p>{mail.summary.text}</p>
        <button class="close" aria-label="Dismiss the summary" onclick={() => mail.dismissSummary()}
          ><Icon name="close" size={13} /></button
        >
      </div>
    {/if}
    <div class="scroll">
      <MailThread messages={mail.openThread.messages} expanded={mail.expanded} />
    </div>
  {:else}
    <EmptyState lead="Select a thread">
      {#snippet note()}j and k move, Enter opens.{/snippet}
    </EmptyState>
  {/if}
</main>

{#if mail.composing}
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
    width: var(--list-w);
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

  .tabs {
    display: flex;
    gap: 2px;
    padding: var(--sp-1) var(--sp-3);
    border-bottom: 1px solid var(--border);
    overflow-x: auto;
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

  .main {
    flex: 1;
    min-width: 0;
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

  .thread-rows {
    flex: 1;
    min-height: 0;
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
