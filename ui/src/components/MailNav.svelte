<script lang="ts">
  // The Mail app's half of the sidebar: every account that has mail switched
  // on, each with its mailboxes and a line of sync status, the way the
  // journal's sidebar lists journals and the library's lists shelves.
  //
  // Mailbox order follows the plan: Inbox, Starred, Snoozed, Drafts, Sent,
  // Archive, Spam, Trash, then whatever labels and folders the account has.
  // `role` decides the first eight; anything left over is a label or folder,
  // sorted by name so the list does not reshuffle as new mail arrives.

  import { accounts } from '../lib/accounts.svelte'
  import { mail } from '../lib/mail.svelte'
  import { panels } from '../lib/panels.svelte'
  import type { Mailbox, MailboxRole } from '../lib/types'
  import Icon from './Icon.svelte'
  import type { IconName } from '../lib/icons'

  void accounts.load()
  void mail.start()

  // Refreshed whenever the account list itself changes -- a sign-in
  // finishing, a status moving to "needs sign-in" -- and, while any account
  // is mid-sync, again every two seconds: a self-scheduling effect rather
  // than a bare `setInterval`, so it stops polling the moment nothing is
  // syncing instead of ticking forever in the background. See
  // `mail.svelte.ts`'s `refreshSyncStatus`/`syncNow`.
  //
  // Bug 8: `mail.start()` fetches the mailbox list exactly once, and nothing
  // else ever asked for it again -- an account added, or first synced, after
  // Mail had already been opened showed no mailboxes until a lock/unlock.
  // `accounts.list` changing is the same signal `refreshSyncStatus` already
  // reacts to here, so a newly-signed-in account's mailboxes show up the
  // same beat its sync status does.
  $effect(() => {
    void accounts.list
    void mail.refreshSyncStatus()
    void mail.refreshMailboxes()
  })
  let wasSyncing = false
  $effect(() => {
    const active = mail.syncStatus.some((s) => s.phase !== 'idle' && s.phase !== 'idling')
    if (active) {
      wasSyncing = true
      const timer = setTimeout(() => void mail.refreshSyncStatus(), 2000)
      return () => clearTimeout(timer)
    }
    // A sync that just finished may have discovered mailboxes that did not
    // exist at the last fetch -- a newly-created label, a folder IMAP only
    // reports once it holds something. Refreshed once, on the falling edge,
    // rather than on every idle tick.
    if (wasSyncing) {
      wasSyncing = false
      void mail.refreshMailboxes()
    }
  })

  const ROLE_ORDER: MailboxRole[] = ['inbox', 'drafts', 'sent', 'archive', 'spam', 'trash']
  const ROLE_ICON: Partial<Record<MailboxRole, IconName>> = {
    inbox: 'inbox',
    sent: 'arrow-up',
    drafts: 'pencil',
    archive: 'layers',
    spam: 'alert',
    trash: 'trash',
  }

  interface Row {
    id: string
    label: string
    icon: IconName
    onclick: () => void
    sel: boolean
    count: number
    pseudo?: boolean
  }

  /** One account's rows: the eight fixed ones (skipping any role the
   *  provider does not send), then its labels and folders. */
  function rowsFor(accountId: string): Row[] {
    const boxes = mail.mailboxes.filter((m) => m.accountId === accountId)
    const fixed: Row[] = []
    for (const role of ROLE_ORDER) {
      const box = boxes.find((b) => b.role === role)
      if (!box) continue
      fixed.push(rowFor(box, ROLE_ICON[role] ?? 'inbox'))
    }
    // Starred and Snoozed are drawn from the pseudo-mailboxes the mock seeds
    // -- see `mock-mail.ts`'s own note on why there is no server-side
    // "starred, across every folder" query yet.
    const starred = boxes.find((b) => b.remoteName === 'Starred')
    const snoozed = boxes.find((b) => b.remoteName === 'Snoozed')
    const pseudo: Row[] = []
    if (starred) pseudo.push(rowFor(starred, 'star', true))
    if (snoozed) pseudo.push(rowFor(snoozed, 'clock', true))

    const folders = boxes
      .filter((b) => b.role === 'other' && b.remoteName !== 'Starred' && b.remoteName !== 'Snoozed')
      .sort((a, b) => a.remoteName.localeCompare(b.remoteName))
      .map((b) => rowFor(b, 'tag'))

    // Inbox, Starred, Snoozed, then the rest -- fixed[0] is always Inbox
    // when there is one, so the pseudo rows slot in right after it.
    const [inboxRow, ...restFixed] = fixed
    return [...(inboxRow ? [inboxRow] : []), ...pseudo, ...restFixed, ...folders]
  }

  function rowFor(box: Mailbox, icon: IconName, pseudo = false): Row {
    return {
      id: box.id,
      label: box.remoteName,
      icon,
      onclick: () => void mail.selectMailbox(box.id),
      sel: mail.selectedMailbox === box.id,
      count: mail.unreadCounts.get(box.id) ?? 0,
      pseudo,
    }
  }

  const mailAccounts = $derived(accounts.list.filter((a) => a.services.mail))

  /** Phase names as a person reads them, not as the enum spells them --
   *  mirrors `MailSyncPhase` in `types.ts`. */
  const PHASE_LABEL: Record<string, string> = {
    connecting: 'Connecting',
    headers: 'Fetching headers',
    bodies: 'Fetching messages',
    attachments: 'Fetching attachments',
  }

  function statusLine(accountId: string): { text: string; error: boolean } | null {
    const status = mail.syncStatus.find((s) => s.accountId === accountId)
    const account = accounts.account(accountId)
    if (account?.status.type === 'needsSignIn') {
      return { text: 'Sign in again', error: true }
    }
    if (account?.status.type === 'error') {
      return { text: account.status.message, error: true }
    }
    if (status?.lastError) return { text: status.lastError, error: true }
    const label = status ? PHASE_LABEL[status.phase] : undefined
    if (label) {
      const progress =
        status!.total > 0
          ? ` ${status!.done.toLocaleString()}/${status!.total.toLocaleString()}`
          : ''
      return { text: `${label}${progress}…`, error: false }
    }
    return null
  }
</script>

<nav class="scroll nav">
  <div class="search">
    <Icon name="search" size={14} />
    <input
      class="q"
      data-search
      placeholder="Search mail"
      value={mail.searchQuery}
      oninput={(e) => mail.setSearchQuery(e.currentTarget.value)}
      spellcheck="false"
    />
    {#if mail.searchQuery}
      <button class="clear" aria-label="Clear search" onclick={() => mail.clearSearch()}>
        <Icon name="close" size={13} />
      </button>
    {/if}
  </div>

  {#if mailAccounts.length === 0}
    <p class="hint">
      No mail accounts yet. <button class="link" onclick={() => panels.openSettings('accounts')}
        >Add one in Settings.</button
      >
    </p>
  {/if}

  {#each mailAccounts as account (account.id)}
    <div class="head">
      <span class="eyebrow">{account.displayName || account.address}</span>
      <button
        class="sync-now"
        title="Sync now"
        aria-label="Sync now"
        onclick={() => void mail.syncNow(account.id)}
      >
        <Icon name="refresh" size={12} />
      </button>
    </div>
    {#each rowsFor(account.id) as row (row.id)}
      <button class="row" class:sel={row.sel} onclick={row.onclick}>
        <span class="icon"
          ><Icon name={row.icon} size={15} filled={row.pseudo && row.icon === 'star'} /></span
        >
        <span class="text">{row.label}</span>
        {#if row.count > 0}<span class="count">{row.count}</span>{/if}
      </button>
    {/each}
    {@const line = statusLine(account.id)}
    {#if line}
      <p class="status" class:error={line.error} title={line.text}>
        {#if line.error}
          <button class="link status-link" onclick={() => panels.openSettings('accounts')}
            >{line.text}</button
          >
        {:else}
          {line.text}
        {/if}
      </p>
    {/if}
  {/each}
</nav>

<style>
  .nav {
    flex: 1;
    padding: var(--sp-2) var(--sp-2) var(--sp-4);
  }

  .search {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: var(--row-h);
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    background: var(--bg-hover);
    color: var(--fg-faint);
  }
  .q {
    flex: 1;
    min-width: 0;
    border: 0;
    background: none;
    color: var(--fg);
    font-size: var(--text-base);
  }
  .q:focus {
    outline: none;
  }
  .clear {
    display: grid;
    place-items: center;
    color: var(--fg-faint);
  }
  .clear:hover {
    color: var(--fg);
  }

  .head {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    padding: var(--sp-5) var(--sp-2) var(--sp-1);
  }
  .eyebrow {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.02em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }
  .sync-now {
    flex: none;
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
    opacity: 0;
  }
  .head:hover .sync-now,
  .sync-now:focus-visible {
    opacity: 1;
  }
  .sync-now:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    height: var(--row-h);
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-base);
    color: var(--fg-muted);
    text-align: left;
  }
  .row:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .row.sel {
    background: var(--bg-active);
    color: var(--fg);
    font-weight: 550;
  }
  .icon {
    width: 16px;
    height: 16px;
    flex: none;
    display: grid;
    place-items: center;
  }
  .text {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .count {
    font-size: var(--text-xs);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }
  .row.sel .count {
    color: var(--fg-muted);
  }

  .status {
    padding: 2px var(--sp-2) var(--sp-1);
    font-size: var(--text-xs);
    color: var(--fg-faint);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .status.error {
    color: var(--danger, #c0392b);
  }
  .status-link {
    color: inherit;
    text-decoration: underline;
  }

  .hint {
    padding: var(--sp-3) var(--sp-2);
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }
  .link {
    color: var(--accent);
    text-decoration: underline;
  }
</style>
