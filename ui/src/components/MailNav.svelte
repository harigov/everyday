<script lang="ts">
  // The Mail app's half of the sidebar: every account that has mail switched
  // on, each with its mailboxes and a line of sync status, the way the
  // journal's sidebar lists journals and the library's lists shelves.
  //
  // Mailbox order follows the plan: Inbox, Starred, Snoozed, Drafts, Sent,
  // Archive, Spam, Trash, then whatever labels and folders the account has.
  // `role` decides the first eight; anything left over is a label or folder,
  // sorted by name so the list does not reshuffle as new mail arrives.
  //
  // With two or more accounts, an "All accounts" section comes first: every
  // Inbox in one list, and the same for Snoozed and the six fixed roles --
  // see `mail.ts`'s `unifiedMailboxes`.

  import { accounts } from '../lib/accounts.svelte'
  import { accountColor } from '../lib/avatar'
  import {
    gmailSystemLabel,
    mailboxDisplayName,
    syncInProgress,
    unifiedMailboxId,
  } from '../lib/mail'
  import { mail } from '../lib/mail.svelte'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { notify } from '../lib/notify.svelte'
  import { panels } from '../lib/panels.svelte'
  import { pref } from '../lib/prefs'
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
    const active = mail.syncStatus.some(syncInProgress)
    if (active) {
      wasSyncing = true
      const timer = setTimeout(() => void mail.refreshSyncStatus(), 2000)
      return () => clearTimeout(timer)
    }
    // A sync that just finished may have discovered mailboxes that did not
    // exist at the last fetch -- a newly-created label, a folder IMAP only
    // reports once it holds something -- and has very likely brought new
    // mail into the list on screen. Refreshed once, on the falling edge,
    // rather than on every idle tick: the mailboxes, and with them the
    // unread counts (`refreshMailboxes` asks for those itself), and the open
    // list, which `refreshMailboxes` leaves alone once something is open.
    if (wasSyncing) {
      wasSyncing = false
      void mail.refreshMailboxes()
      void mail.refresh()
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
    /** An "All accounts" row -- in no one account, so its menu has nothing
     *  about one to offer. */
    unified?: boolean
    role?: MailboxRole
    accountId: AccountId
    /** The mailbox behind the row -- absent for Scheduled, which is a list of
     *  queued sends rather than a folder. */
    box?: Mailbox
  }

  /** The "All accounts" section's rows, in `unifiedMailboxes`'s own order,
   *  each with the icon its per-account counterpart draws. */
  const unifiedRows = $derived(
    mail.unifiedMailboxes.map((box): Row => ({
      ...rowFor(
        box,
        box.id === unifiedMailboxId('snoozed') ? 'clock' : (ROLE_ICON[box.role] ?? 'inbox'),
      ),
      unified: true,
    })),
  )

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
    // Every account at once: nothing here is about one of them, and `in:`
    // names one account's folder, not a role across all of them.
    if (row.unified && box) {
      return tidyMenu([
        { label: 'Open', icon: 'inbox', disabled: row.sel, run: row.onclick },
        {
          label: 'Mark all as read',
          icon: 'tick',
          disabled: row.count === 0,
          hint: row.count > 0 ? String(row.count) : undefined,
          run: () => markAllRead(box, row.label),
        },
      ])
    }
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
        // `syncAll` rather than one `syncNow` each, so the list column's own
        // Sync now button spins for this too.
        run: () => mail.syncAll(mailAccounts.map((a) => a.id)),
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
  /** In the order their dots take `accountColor`'s palette -- the same
   *  order the thread list reads, so an account's dot matches in both. */
  const accountIds = $derived(mailAccounts.map((a) => a.id))

  // ── Which accounts are unfolded ───────────────────────────────────
  //
  // Only with the "All accounts" section above them: there every inbox is
  // already one click away, and each account folds to one row until its own
  // folders are wanted. Remembered per device, like the sidebar's own fold.

  const openPref = pref<AccountId[]>(
    'everyday.mail.accounts.open',
    (raw) => {
      const parsed: unknown = raw ? JSON.parse(raw) : []
      return Array.isArray(parsed) ? parsed.filter((v): v is string => typeof v === 'string') : []
    },
    [],
    (ids) => JSON.stringify(ids),
  )
  let openAccounts = $state<Set<AccountId>>(new Set(openPref.get()))

  /** The account whose folder is open in the list -- always shown unfolded,
   *  so the selected row is never one hidden inside a closed account. */
  const selectedAccount = $derived(
    mail.mailboxes.find((m) => m.id === mail.selectedMailbox)?.accountId ??
      mail.viewingScheduledFor,
  )

  function accountOpen(id: AccountId): boolean {
    return openAccounts.has(id) || selectedAccount === id
  }

  function toggleAccount(id: AccountId) {
    const next = new Set(openAccounts)
    if (accountOpen(id)) next.delete(id)
    else next.add(id)
    openAccounts = next
    openPref.set([...next])
  }

  /** A folded account's Inbox count, shown on its own row in its place. */
  function inboxCountFor(id: AccountId): number {
    const inbox = mail.mailboxes.find((m) => m.accountId === id && m.role === 'inbox')
    return inbox ? (mail.unreadCounts.get(inbox.id) ?? 0) : 0
  }

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

<nav class="scroll side-nav" oncontextmenu={(e) => menu.show(e, navMenu())}>
  {#if mailAccounts.length === 0}
    <p class="hint">
      No mail accounts yet. <button class="link" onclick={() => panels.openSettings('accounts')}
        >Add one in Settings.</button
      >
    </p>
  {/if}

  {#snippet mailboxRow(row: Row, nested = false)}
    <button
      class="side-row"
      class:sel={row.sel}
      class:nested
      onclick={row.onclick}
      oncontextmenu={(e) => menu.show(e, mailboxMenu(row))}
    >
      <span class="side-icon"
        ><Icon name={row.icon} size={16} filled={row.pseudo && row.icon === 'star'} /></span
      >
      <span class="side-text">{row.label}</span>
      {#if row.role === 'drafts' && pendingSends > 0}
        <span
          class="ghost-badge"
          title="{pendingSends} {pendingSends === 1 ? 'draft' : 'drafts'} proposed to send"
        >
          <Icon name="sparkle" size={11} />
        </span>
      {/if}
      {#if row.count > 0}
        <span class="side-count" class:strong={row.role === 'inbox'}>{row.count}</span>
      {/if}
    </button>
  {/snippet}

  {#snippet statusFor(accountId: AccountId)}
    {@const line = statusLine(accountId)}
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
  {/snippet}

  {#if unifiedRows.length > 0}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="side-head" oncontextmenu={(e) => menu.show(e, navMenu())}>
      <span class="eyebrow">All accounts</span>
      <button
        class="side-add sync-now"
        title="Sync every account"
        aria-label="Sync every account"
        onclick={() => void mail.syncAll(mailAccounts.map((a) => a.id))}
      >
        <Icon name="refresh" size={12} />
      </button>
    </div>
    {#each unifiedRows as row (row.id)}
      {@render mailboxRow(row)}
    {/each}

    <!-- With every inbox already above, each account folds to one row --
         its own folders and labels a click away rather than three copies of
         Inbox, Drafts and Sent stacked down the sidebar. -->
    <div class="side-head">
      <span class="eyebrow">Accounts</span>
    </div>
    {#each mailAccounts as account (account.id)}
      {@const open = accountOpen(account.id)}
      {@const inbox = inboxCountFor(account.id)}
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="account"
        class:open
        oncontextmenu={(e) => menu.show(e, tidyMenu(accountItems(account.id)))}
      >
        <button
          class="account-toggle"
          aria-expanded={open}
          onclick={() => toggleAccount(account.id)}
          title={account.address}
        >
          <span class="chev" class:down={open}><Icon name="chevron" size={12} /></span>
          <span class="acct-dot" style:background={accountColor(account.id, accountIds)}></span>
          <span class="side-text">{account.displayName || account.address}</span>
          {#if !open && inbox > 0}<span class="side-count strong">{inbox}</span>{/if}
        </button>
        <button
          class="side-add sync-now"
          title="Sync now"
          aria-label="Sync {account.displayName || account.address} now"
          onclick={() => void mail.syncNow(account.id)}
        >
          <Icon name="refresh" size={12} />
        </button>
      </div>
      {#if open}
        {#each rowsFor(account.id) as row (row.id)}
          {@render mailboxRow(row, true)}
        {/each}
      {/if}
      {@render statusFor(account.id)}
    {/each}
  {:else}
    {#each mailAccounts as account (account.id)}
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="side-head"
        oncontextmenu={(e) => menu.show(e, tidyMenu(accountItems(account.id)))}
      >
        <span class="eyebrow">{account.displayName || account.address}</span>
        <button
          class="side-add sync-now"
          title="Sync now"
          aria-label="Sync now"
          onclick={() => void mail.syncNow(account.id)}
        >
          <Icon name="refresh" size={12} />
        </button>
      </div>
      {#each rowsFor(account.id) as row (row.id)}
        {@render mailboxRow(row)}
      {/each}
      {@render statusFor(account.id)}
    {/each}
  {/if}
</nav>

<style>
  /* The rows, headings and counts are the shared sidebar's -- see app.css's
     "Sidebars". What is Mail's own: a nested account's folders set in under
     its name, the per-account sync button that shows on hover, and the
     account rows that fold. */

  /* An account's own folders, set in under its name. */
  .side-row.nested {
    padding-left: 30px;
  }
  .ghost-badge {
    display: flex;
    color: var(--accent);
    opacity: 0.85;
  }

  /* Sync now, per heading and per account: shown on hover, since every
     account already syncs on its own and this is the impatient path. */
  .sync-now {
    opacity: 0;
    transition: opacity var(--fast) var(--ease);
  }
  .side-head:hover .sync-now,
  .account:hover .sync-now,
  .sync-now:focus-visible {
    opacity: 1;
  }

  /* ── An account, folded or not ──────────────────────────────────── */

  .account {
    display: flex;
    align-items: center;
    gap: 2px;
    padding-right: var(--sp-1);
    border-radius: var(--radius);
  }
  .account:hover {
    background: var(--bg-hover);
  }
  .account-toggle {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: var(--side-row-h);
    padding: 0 var(--sp-2);
    font-size: var(--text-base);
    font-weight: 600;
    color: var(--fg);
    text-align: left;
  }
  .chev {
    flex: none;
    display: grid;
    place-items: center;
    width: 14px;
    color: var(--fg-faint);
    transition: transform var(--fast) var(--ease);
  }
  .chev.down {
    transform: rotate(90deg);
  }
  .acct-dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
  }

  .status {
    padding: 2px var(--sp-3) var(--sp-1);
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
    padding: var(--sp-3);
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }
  .link {
    color: var(--accent);
    text-decoration: underline;
  }
</style>
