<script lang="ts">
  // The Mail app's main pane: the thread list, `VirtualList`-backed so a
  // hundred-thousand-message mailbox costs the same DOM as a hundred, and
  // the reading pane beside it -- the journal's `EntryList` + `Editor` shape,
  // folded into one component the way the library and the todo app do.

  import { formatSenders, threadListDate } from '../lib/mail'
  import { mail } from '../lib/mail.svelte'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { plural } from '../lib/format'
  import type { Thread } from '../lib/types'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import MailCompose from './MailCompose.svelte'
  import MailSnoozePicker from './MailSnoozePicker.svelte'
  import MailThread from './MailThread.svelte'
  import VirtualList from './VirtualList.svelte'

  void mail.start()

  const heading = $derived(mail.mailbox?.remoteName ?? 'Mail')

  function rowMenu(t: Thread): MenuItem[] {
    return tidyMenu([
      { label: 'Open', icon: 'inbox', run: () => void mail.openThreadById(t.id) },
      {
        label: t.unreadCount > 0 ? 'Mark read' : 'Mark unread',
        icon: 'check',
        run: () => void (t.unreadCount > 0 ? mail.markRead(t.id) : mail.markUnread(t.id)),
      },
      {
        label: t.starred ? 'Remove star' : 'Star',
        icon: 'star',
        run: () => void mail.toggleStar(t.id),
      },
      { label: 'Snooze…', icon: 'clock', run: () => (mail.wantsSnooze = t.id) },
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

  {#if mail.searchQuery.trim()}
    {#if mail.searching && mail.searchResults.length === 0}
      <p class="hint">Searching…</p>
    {:else if mail.searchResults.length === 0}
      <p class="hint">Nothing matched “{mail.searchQuery}”.</p>
    {:else}
      {#each mail.searchResults as hit (hit.threadId)}
        <button class="row" onclick={() => void mail.openThreadById(hit.threadId)}>
          <span class="dot" aria-hidden="true"></span>
          <div class="body">
            <div class="line1">
              <span class="from">{hit.from.name || hit.from.email}</span>
              <span class="date">{threadListDate(hit.date)}</span>
            </div>
            <div class="line2">
              <span class="subject">{hit.subject || '(no subject)'}</span>
              <span class="snippet"> — {hit.snippet}</span>
            </div>
          </div>
        </button>
      {/each}
    {/if}
  {:else if mail.loading && mail.threads.length === 0}
    <p class="hint">Loading…</p>
  {:else if mail.threads.length === 0}
    <EmptyState lead="Nothing here.">
      {#snippet note()}This mailbox has no threads matching what is showing.{/snippet}
    </EmptyState>
  {:else}
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
              <span class="date">{threadListDate(t.lastDate)}</span>
            </div>
            <div class="line2">
              <span class="subject">{t.subject || '(no subject)'}</span>
              {#if t.snippet}<span class="snippet"> — {t.snippet}</span>{/if}
            </div>
            <div class="marks">
              {#if t.starred}<Icon name="star" size={11} filled />
              {/if}
              {#if t.hasAttachments}<Icon name="tag" size={11} />
              {/if}
              {#if t.draftedByAssistant}<span class="assistant-mark"
                  ><Icon name="sparkle" size={11} /> Drafted by the assistant</span
                >{/if}
              {#if t.messageCount > 1}<span class="count">{t.messageCount}</span>{/if}
            </div>
          </div>
        </button>
      {/snippet}
    </VirtualList>
  {/if}
</section>

<main class="main">
  {#if mail.openThread}
    <div class="thread-head">
      <button class="back" onclick={() => mail.closeThread()} title="Back to the list (Escape)">
        <Icon name="chevron" size={14} />
      </button>
      <h1>{mail.openThread.thread.subject || '(no subject)'}</h1>
      <span class="count">{plural(mail.openThread.messages.length, 'message')}</span>
    </div>
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

{#if mail.sendingUndo}
  <div class="undo-toast">
    <span>Sending in {mail.sendingUndo.secondsLeft}s…</span>
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
    gap: var(--sp-2);
    overflow: hidden;
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
    flex: none;
    max-width: 60%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .snippet {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    color: var(--fg-faint);
  }
  .marks {
    display: flex;
    align-items: center;
    gap: 4px;
    color: #e0a92b;
    font-size: var(--text-xs);
  }
  .assistant-mark {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    color: var(--journal-accent, var(--accent));
  }
  .marks .count {
    margin-left: auto;
    color: var(--fg-faint);
  }

  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .thread-head {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: var(--header-h);
    padding: 0 var(--sp-4);
    flex: none;
    border-bottom: 1px solid var(--border);
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
  .scroll {
    flex: 1;
    overflow-y: auto;
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
