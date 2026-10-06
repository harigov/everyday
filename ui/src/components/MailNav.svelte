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
  import { gmailSystemLabel, mailboxDisplayName } from '../lib/mail'
  import { mail } from '../lib/mail.svelte'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { notify } from '../lib/notify.svelte'
  import { panels } from '../lib/panels.svelte'
  import { proposals } from '../lib/proposals.svelte'
  import { focusSearch } from '../lib/shortcuts.svelte'
  import type { AccountId, Mailbox, MailboxRole } from '../lib/types'
  import Icon from './Icon.svelte'
  import type { IconName } from '../lib/icons'

  /** At least one draft a dream wrote is waiting to be sent. Not scoped to
   *  an account: a `sendMail` proposal names a draft, not an account, and a
   *  hint that something is waiting in Drafts is worth more here than the
   *  plumbing a precise per-account count would cost. */
  const pendingSends = $derived(proposals.forKind('mail').length)

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
    role?: MailboxRole
    accountId: AccountId
    /** The mailbox behind the row -- absent for Scheduled, which is a list of
     *  queued sends rather than a folder. */
    box?: Mailbox
  }

  /** One account's rows: the eight fixed ones (skipping any role the
   *  provider does not send), then Scheduled if it has one, then its
   *  labels and folders. */
  function rowsFor(accountId: string): Row[] {
    const boxes = mail.mailboxes.filter((m) => m.accountId === accountId)
    const fixed: Row[] = []
    for (const role of ROLE_ORDER) {
      const box = boxes.find((b) => b.role === role)
      if (!box) continue
      fixed.push(rowFor(box, ROLE_ICON[role] ?? 'inbox'))
      // Right after Drafts, and only once there is at least one -- see
      // `mail.scheduled`.
      if (role === 'drafts') {
        const count = scheduledCountFor(accountId)
        if (count > 0) fixed.push(scheduledRow(accountId, count))
      }
    }
    // Starred and Snoozed are drawn from the pseudo-mailboxes the mock seeds
    // -- see `mock-mail.ts`'s own note on why there is no server-side
    // "starred, across every folder" query yet.
    // By `pseudo`, not by name: a real folder called "Snoozed" is a folder.
    const starred = boxes.find((b) => b.pseudo === 'starred')
    const snoozed = boxes.find((b) => b.pseudo === 'snoozed')
    const pseudo: Row[] = []
    if (starred) pseudo.push(rowFor(starred, 'star', true))
    if (snoozed) pseudo.push(rowFor(snoozed, 'clock', true))

    // A Gmail label that is really one of Gmail's own system mailboxes --
    // `\Sent`, `\Draft(s)`, `\Starred`, `\Important`, `\Spam`, `\Trash`,
    // `\All` -- duplicates a row already drawn above by `role` or `pseudo`;
    // `\Inbox` is not in this list because `promoteGmailInbox` has already
    // turned it into the Inbox's own `role: 'other'` row, before this ever
    // sees it, wherever nothing else already was.
    const folders = boxes
      .filter((b) => b.role === 'other' && !b.pseudo && gmailSystemLabel(b.remoteName) === null)
      .sort((a, b) => mailboxDisplayName(a).localeCompare(mailboxDisplayName(b)))
      .map((b) => rowFor(b, 'tag'))

    // Inbox, Starred, Snoozed, then the rest -- fixed[0] is always Inbox
    // when there is one, so the pseudo rows slot in right after it.
    const [inboxRow, ...restFixed] = fixed
    return [...(inboxRow ? [inboxRow] : []), ...pseudo, ...restFixed, ...folders]
  }

  function scheduledCountFor(accountId: string): number {
    return mail.scheduled.filter((s) => s.draft.accountId === accountId).length
  }

  function scheduledRow(accountId: string, count: number): Row {
    return {
      id: `scheduled:${accountId}`,
      label: 'Scheduled',
      icon: 'clock',
      onclick: () => mail.selectScheduled(accountId),
      sel: mail.viewingScheduledFor === accountId,
      count,
      accountId,
    }
  }

  function rowFor(box: Mailbox, icon: IconName, pseudo = false): Row {
    return {
      id: box.id,
      label: mailboxDisplayName(box),
      icon,
      onclick: () => void mail.selectMailbox(box.id),
      // Never shown selected at the same time as the Scheduled row: exactly
      // one of the two is ever the list column's content.
      sel: mail.viewingScheduledFor === null && mail.selectedMailbox === box.id,
      count: mail.unreadCounts.get(box.id) ?? 0,
      pseudo,
      role: box.role,
      accountId: box.accountId,
      box,
    }
  }

  // ── What a right-click offers ─────────────────────────────────────
  //
  // The same three layers every other sidebar has: the list's own blank
  // space, a heading, and a row. What is *not* here is anything a folder
  // would need the server's say-so for -- renaming one, emptying Trash --
  // because there is no command for either yet, and a menu row that cannot
  // work is worse than one that is not there.

  /** About the account a row belongs to: offered on its heading and, after
   *  a rule, on every mailbox under it, so nobody has to aim for the
   *  heading to reach them. */
  function accountItems(accountId: AccountId): MenuItem[] {
    const address = accounts.account(accountId)?.address
    return [
      {
        label: address ? `New message from ${address}` : 'New message',
        icon: 'pencil',
        run: () => mail.compose(accountId),
      },
      { label: 'Sync now', icon: 'refresh', run: () => mail.syncNow(accountId) },
      SEP,
      {
        label: 'Account settings…',
        icon: 'settings',
        hint: 'in Settings',
        run: () => panels.openSettings('accounts'),
      },
    ]
  }

  function mailboxMenu(row: Row): MenuItem[] {
    const box = row.box
    return tidyMenu([
      { label: 'Open', icon: 'inbox', disabled: row.sel, run: row.onclick },
      // Starred and Snoozed are views rather than folders, so there is no
      // `in:` for the search to name -- and Scheduled is not even mail that
      // has arrived, so neither a search nor "read" means anything there.
      box &&
        !row.pseudo && {
          label: `Search in ${row.label}`,
          icon: 'search',
          run: () => searchIn(box),
        },
      box && {
        label: 'Mark all as read',
        icon: 'tick',
        disabled: row.count === 0,
        hint: row.count > 0 ? String(row.count) : undefined,
        run: () => markAllRead(box, row.label),
      },
      SEP,
      ...accountItems(row.accountId),
    ])
  }

  /** The blank space under the list. Nothing in it is about one account. */
  function navMenu(): MenuItem[] {
    return tidyMenu([
      mailAccounts.length > 0 && {
        label: 'New message',
        icon: 'pencil',
        run: () => mail.compose(),
      },
      mailAccounts.length > 0 && {
        label: mailAccounts.length === 1 ? 'Sync now' : 'Sync every account',
        icon: 'refresh',
        run: () => Promise.all(mailAccounts.map((a) => mail.syncNow(a.id))),
      },
      SEP,
      { label: 'Add an account…', icon: 'plus', run: () => panels.openSettings('accounts') },
    ])
  }

  /**
   * Open the folder and start a search inside it, in the bar at the top of
   * the window -- `in:` is the same syntax a person would type there, so
   * what this does is visible and can be edited rather than being a mode.
   */
  async function searchIn(box: Mailbox) {
    if (mail.selectedMailbox !== box.id || mail.viewingScheduledFor !== null) {
      await mail.selectMailbox(box.id)
    }
    // The name the server knows it by, which is what `in:` matches -- not
    // the friendlier one the row draws for a Gmail system label.
    const name = /\s/.test(box.remoteName) ? `"${box.remoteName}"` : box.remoteName
    mail.setSearchQuery(`in:${name} `)
    focusSearch()
  }

  async function markAllRead(box: Mailbox, label: string) {
    const done = await mail.markMailboxRead(box.id)
    if (done > 0) {
      notify.success(`Marked ${done === 1 ? '1 conversation' : `${done} conversations`} read`, {
        body: label,
      })
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

<nav class="scroll nav" oncontextmenu={(e) => menu.show(e, navMenu())}>
  {#if mailAccounts.length === 0}
    <p class="hint">
      No mail accounts yet. <button class="link" onclick={() => panels.openSettings('accounts')}
        >Add one in Settings.</button
      >
    </p>
  {/if}

  {#each mailAccounts as account (account.id)}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="head" oncontextmenu={(e) => menu.show(e, tidyMenu(accountItems(account.id)))}>
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
      <button
        class="row"
        class:sel={row.sel}
        onclick={row.onclick}
        oncontextmenu={(e) => menu.show(e, mailboxMenu(row))}
      >
        <span class="icon"
          ><Icon name={row.icon} size={15} filled={row.pseudo && row.icon === 'star'} /></span
        >
        <span class="text">{row.label}</span>
        {#if row.role === 'drafts' && pendingSends > 0}
          <span
            class="ghost-badge"
            title="{pendingSends} {pendingSends === 1 ? 'draft' : 'drafts'} proposed to send"
          >
            <Icon name="sparkle" size={11} />
          </span>
        {/if}
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
  .ghost-badge {
    display: flex;
    color: var(--accent);
    opacity: 0.85;
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
