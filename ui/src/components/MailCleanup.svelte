<script lang="ts">
  // Quick cleanup: who filled the Inbox most over the last week, month,
  // quarter or year, or ever -- counting only what of theirs is still there --
  // and a way to archive or bin all of it, one sender or several at a time.
  // Or, turned round, who left the most mail older than one of those spans:
  // a sender's back catalogue cleared out, their last month kept. Then only
  // conversations with nothing newer count, since the buttons move whole
  // conversations and a reply from last week is not old mail.
  //
  // Narrowed three ways at once: to one account (it opens on the one the
  // list column was showing, or on every account from the unified Inbox), to
  // one kind of mail -- an inbox tab, or one of the person's own sorted
  // categories -- and to the senders whose name or address holds what was
  // typed. The kind picks conversations rather than senders, so a shop's
  // receipts stay put when its promotions are cleared out.
  //
  // Every sender is asked for at once, and the list is paged and filtered
  // here: the count costs the same however many rows come back, and typing
  // then narrows rows already on hand rather than running the count again
  // for each word.
  //
  // The list is `inbox_senders`' answer as it stands; acting on a sender
  // hands the store exactly the conversations that answer named
  // (`InboxSender.threads`), so what goes is what the row said, never a
  // fresh "from:" search that might have caught something else since. The
  // dialog stays open afterwards -- clearing out a newsletter is rarely the
  // only thing somebody opened this to do -- and asks again quietly behind
  // the rows it has already dropped.

  import { untrack } from 'svelte'
  import { accounts } from '../lib/accounts.svelte'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import { plural } from '../lib/format'
  import { CATEGORY_TABS, senderMatches } from '../lib/mail'
  import * as mailApi from '../lib/mail-api'
  import { mail } from '../lib/mail.svelte'
  import { notify } from '../lib/notify.svelte'
  import { sorting } from '../lib/sorting.svelte'
  import { handle } from '../lib/state.svelte'
  import type {
    AccountId,
    InboxSender,
    InboxSendersArgs,
    MailCategory,
    ThreadId,
  } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'

  let {
    accountId,
    onclose,
  }: {
    /** The account to open on, or `null` for every mail account. */
    accountId: AccountId | null
    onclose: () => void
  } = $props()

  /** `days: null` is all time -- everything still in the Inbox, however old,
   *  which is what clearing out years of newsletters needs. `within` is the
   *  span in words for the sentences that name it, absent for all time. */
  const PERIODS: { days: number | null; label: string; within?: string }[] = [
    { days: 7, label: '7 days', within: 'the last 7 days' },
    { days: 30, label: '30 days', within: 'the last 30 days' },
    { days: 90, label: '90 days', within: 'the last 90 days' },
    { days: 365, label: '1 year', within: 'the last year' },
    { days: null, label: 'All time' },
  ]
  /** `inbox_senders`' most: as many senders as one Inbox can hold. */
  const ALL = 20_000
  /** Senders on one page. */
  const PAGE = 50

  let days = $state<number | null>(30)
  /** Counting mail older than `days` rather than within it. */
  let older = $state(false)
  let account = $state<AccountId | null>(untrack(() => accountId))
  /** `''` for every kind of mail, `tab:<category>` for an inbox tab,
   *  `own:<id>` for one of the person's own sorted categories -- one
   *  string, so one `<select>` can offer both. */
  let sort = $state('')
  let query = $state('')
  let page = $state(0)
  /** Every sender for the period, account and kind. Only ever replaced
   *  whole, never edited in place -- `.raw`, as the store's own `threads`
   *  is. */
  let senders = $state.raw<InboxSender[]>([])
  /** Asking `inbox_senders` -- the period, account or kind just changed.
   *  Not set by the quiet re-ask after an action, which has rows to show. */
  let loading = $state(true)
  /** Has any answer landed yet -- what tells "nothing here" from "not yet". */
  let loaded = $state(false)
  let failed = $state(false)
  /** An archive or a delete is on its way. */
  let busy = $state(false)
  /** By sender address, the key `inbox_senders` groups on. */
  let selected = $state<Set<string>>(new Set())
  /** A bulk action waiting on a yes, because it reaches rows on other pages. */
  let asking = $state<{ move: 'archive' | 'trash'; list: InboxSender[]; unseen: number } | null>(
    null,
  )

  let dialog = $state<HTMLElement>()
  let selectAll = $state<HTMLInputElement>()
  let search = $state<HTMLInputElement>()
  let scroller = $state<HTMLElement>()

  /** Which ask is current, so a slow answer for "1 year" cannot land on top
   *  of the quicker one for "7 days" that was asked for after it. */
  let generation = 0

  const period = $derived(PERIODS.find((p) => p.days === days) ?? PERIODS[1]!)
  const mailAccounts = $derived(accounts.list.filter((a) => a.services.mail))
  /** The person's own categories, as Settings → Mail sorting lists them. */
  const ownCategories = $derived(sorting.sorting?.categories ?? [])
  // Read afresh rather than from whenever Mail first loaded it: whether a
  // decision model is on, which the note below says, may have changed
  // since in Settings.
  void sorting.refresh()

  /** The kind picked, as the menu names it -- `''` for every kind. */
  const sortLabel = $derived(
    CATEGORY_TABS.find((t) => `tab:${t.key}` === sort)?.label ??
      ownCategories.find((c) => `own:${c.id}` === sort)?.name ??
      '',
  )

  /** Who this is looking through, in words, where there is no menu to say
   *  it: one account's address, or every account's -- which, with only
   *  one, is still just that one address. */
  const scopeLabel = $derived.by(() => {
    if (account) return accounts.account(account)?.address ?? 'This account'
    const only = mailAccounts.length === 1 ? mailAccounts[0] : undefined
    return only ? only.address : 'All accounts'
  })

  const words = $derived(query.trim())
  /** Every sender the filters let through, across every page. */
  const rows = $derived(senders.filter((s) => senderMatches(s, words)))
  const pages = $derived(Math.max(1, Math.ceil(rows.length / PAGE)))
  /** `page`, kept inside the list as an archive shortens it. */
  const at = $derived(Math.min(page, pages - 1))
  const shown = $derived(rows.slice(at * PAGE, (at + 1) * PAGE))

  const chosen = $derived(rows.filter((s) => selected.has(s.email)))
  const chosenThreads = $derived(threadsOf(chosen))
  const pageSelected = $derived(shown.length > 0 && shown.every((s) => selected.has(s.email)))

  // A header checkbox's third state is a DOM property with no attribute
  // behind it, so it is set on the element rather than in the markup.
  $effect(() => {
    if (selectAll)
      selectAll.indeterminate = !pageSelected && shown.some((s) => selected.has(s.email))
  })

  /** What the period, account and kind ask `inbox_senders` for. */
  function filters(): InboxSendersArgs {
    return {
      days: older ? null : days,
      olderThanDays: older ? days : null,
      accounts: account ? [account] : null,
      limit: ALL,
      category: sort.startsWith('tab:') ? (sort.slice(4) as MailCategory) : null,
      sortedAs: sort.startsWith('own:') ? sort.slice(4) : null,
    }
  }

  // Asked again whenever the period, the account or the kind changes, and
  // for nothing else: the ask itself is untracked, so neither the rows it
  // writes once the answer lands nor anything the call reads on its way
  // out can send it round a second time.
  $effect(() => {
    const ask = filters()
    untrack(() => void load(ask))
  })

  async function load(ask: InboxSendersArgs, quiet = false) {
    const token = ++generation
    if (!quiet) {
      loading = true
      // A tick was a choice about the conversations a row named under the
      // old filters. Under new ones the same sender's row names others --
      // last year's mail, say, where it named the old -- so no tick
      // carries over.
      selected = new Set()
    }
    try {
      const found = await mailApi.inboxSenders(ask)
      if (token !== generation) return
      senders = found
      // A sender who has dropped out of the new answer cannot stay ticked:
      // "Archive selected" would count conversations no longer on screen.
      selected = new Set([...selected].filter((email) => found.some((s) => s.email === email)))
      loaded = true
      failed = false
    } catch (e) {
      if (token !== generation) return
      failed = true
      await handle(e)
    } finally {
      if (token === generation) loading = false
    }
  }

  function turn(toOlder: boolean) {
    older = toOlder
    // "Older than all time" would be nothing at all.
    if (older && days === null) days = 30
    restart()
  }

  /** Back to the first page, scrolled to its top: under a new filter the
   *  rows are a different list. */
  function restart() {
    turnTo(0)
  }

  function turnTo(next: number) {
    page = next
    scroller?.scrollTo({ top: 0 })
  }

  function typed() {
    restart()
    // A tick the words now hide cannot stay: it would be acted on unseen,
    // or come back ticked once the words are cleared.
    selected = new Set([...selected].filter((email) => rows.some((s) => s.email === email)))
  }

  /** Every conversation a set of senders' rows names, once each -- two
   *  senders in one thread (a reply-all) would otherwise send it twice. */
  function threadsOf(list: readonly InboxSender[]): ThreadId[] {
    return [...new Set(list.flatMap((s) => s.threads))]
  }

  function nameOf(sender: InboxSender): string {
    return sender.name || sender.email
  }

  /** "from Priya Raman, The Economist and 3 others" -- the toast's own line. */
  function fromLine(list: readonly InboxSender[]): string {
    const names = list.map(nameOf)
    if (names.length <= 3) {
      const last = names.pop()
      return names.length > 0 ? `from ${names.join(', ')} and ${last}` : `from ${last ?? ''}`
    }
    return `from ${names.slice(0, 2).join(', ')} and ${names.length - 2} others`
  }

  function toggle(email: string) {
    const next = new Set(selected)
    if (next.has(email)) next.delete(email)
    else next.add(email)
    selected = next
  }

  /** The header checkbox: this page's rows, all on or all off. */
  function togglePage() {
    const next = new Set(selected)
    for (const s of shown) {
      if (pageSelected) next.delete(s.email)
      else next.add(s.email)
    }
    selected = next
  }

  function selectEvery() {
    selected = new Set([...selected, ...rows.map((s) => s.email)])
  }

  /** The bulk buttons: straight on when every ticked row is on this page,
   *  asked first when some are not. "Select all" reaches rows nobody has
   *  looked at, and two clicks should not clear out an Inbox unseen. */
  function bulk(move: 'archive' | 'trash') {
    const onPage = new Set(shown.map((s) => s.email))
    const unseen = chosen.filter((s) => !onPage.has(s.email)).length
    if (unseen > 0) asking = { move, list: chosen, unseen }
    else void act(move, chosen)
  }

  /** Yes to `bulk`'s question. Taken out of `asking` before it is cleared,
   *  which takes the dialog down. */
  function answered() {
    const question = asking
    asking = null
    if (question) void act(question.move, question.list)
  }

  async function act(move: 'archive' | 'trash', list: readonly InboxSender[]) {
    if (busy) return
    const ids = threadsOf(list)
    if (ids.length === 0) return
    busy = true
    let ok = false
    try {
      ok = move === 'archive' ? await mail.archiveThreads(ids) : await mail.trashThreads(ids)
    } finally {
      busy = false
    }
    if (ok) {
      // Gone from the list at once, without waiting on the re-ask below:
      // the senders acted on, and anybody else whose every conversation was
      // among those -- a thread two of them shared, say.
      const gone = new Set(ids)
      const acted = new Set(list.map((s) => s.email))
      senders = senders.filter((s) => !acted.has(s.email) && !s.threads.every((t) => gone.has(t)))
      selected = new Set([...selected].filter((email) => senders.some((s) => s.email === email)))
      notify.success(
        move === 'archive'
          ? `Archived ${plural(ids.length, 'conversation')}`
          : `Moved ${plural(ids.length, 'conversation')} to Trash`,
        { body: fromLine(list) },
      )
      // The button just pressed may have gone with its row; focus would
      // otherwise fall out of the dialog to the page behind it.
      dialog?.focus()
    }
    // Asked again either way -- after a failure partway, some of it did go.
    void load(filters(), true)
  }

  /** The question `bulk` asks, in words. */
  const askingTitle = $derived.by(() => {
    if (!asking) return ''
    const what = plural(threadsOf(asking.list).length, 'conversation')
    const from = plural(asking.list.length, 'sender')
    return asking.move === 'archive'
      ? `Archive ${what} from ${from}?`
      : `Move ${what} from ${from} to Trash?`
  })

  /** What an empty list says, given what it was narrowed to. */
  const emptyHint = $derived.by(() => {
    const within = older
      ? ` older than ${period.label}`
      : period.within
        ? ` from ${period.within}`
        : ''
    const under = sortLabel ? ` under ${sortLabel}` : ''
    if (words) return `Nobody matching “${words}” in your inbox${under}${within}.`
    if (sortLabel) return `Nothing in your inbox${under}${within}.`
    return `Nothing in your inbox${within}.`
  })

  /** Why a list under one of the person's own categories can come up
   *  short: only a thread a decision model has sorted carries one, and it
   *  sorts recent mail unasked, older mail only when asked. `''` when the
   *  period sits inside what it has been sorting. */
  const sortNote = $derived.by(() => {
    if (!sort.startsWith('own:')) return ''
    const view = sorting.view
    if (!view?.decisionModel) {
      return 'Only mail a decision model has sorted is counted, and none is sorting now. Switch one on in Settings → Models.'
    }
    const reach = view.sorting.windowDays
    if (!older && days !== null && days <= reach) return ''
    return `Only mail sorted so far is counted: the last ${plural(reach, 'day')}, and older mail you asked to sort in Settings → Mail sorting.`
  })
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    // The confirmation over the dialog takes its own Escape.
    if (e.key !== 'Escape' || asking) return
    // The first Escape in the filter box empties it, as a search field's
    // does; the next one closes.
    if (e.target === search && query) {
      e.preventDefault()
      query = ''
      typed()
      return
    }
    onclose()
  }}
/>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="scrim" onclick={onclose}></div>
<div
  class="sheet cleanup"
  role="dialog"
  aria-modal="true"
  aria-labelledby="mail-cleanup-title"
  aria-describedby="mail-cleanup-lead"
  tabindex="-1"
  bind:this={dialog}
  use:trapFocus
>
  <header>
    <div>
      <h2 id="mail-cleanup-title">Quick cleanup</h2>
      <p class="lead" id="mail-cleanup-lead">
        {#if older}
          Who has the most mail older than {period.label} still in your inbox. A conversation with anything
          newer is left alone.
        {:else}
          Who filled your inbox most{period.within ? ` in ${period.within}` : ''}, counting only
          what is still there.
        {/if}
      </p>
    </div>
    <button class="close" aria-label="Close" onclick={onclose}>
      <Icon name="close" size={14} />
    </button>
  </header>

  <div class="controls">
    <div class="segmented" role="group" aria-label="Which mail">
      <button
        class="seg"
        class:on={!older}
        aria-pressed={!older}
        disabled={busy}
        onclick={() => turn(false)}
      >
        Last
      </button>
      <button
        class="seg"
        class:on={older}
        aria-pressed={older}
        disabled={busy}
        onclick={() => turn(true)}
      >
        Older than
      </button>
    </div>
    <div class="segmented" role="group" aria-label={older ? 'Older than' : 'Over the last'}>
      {#each PERIODS.filter((p) => !older || p.days !== null) as p (p.label)}
        <button
          class="seg"
          class:on={days === p.days}
          aria-pressed={days === p.days}
          disabled={busy}
          onclick={() => {
            days = p.days
            restart()
          }}
        >
          {p.label}
        </button>
      {/each}
    </div>
    {#if mailAccounts.length > 1}
      <label class="scope">
        <Icon name="inbox" size={12} />
        <select bind:value={account} aria-label="Which account" disabled={busy} onchange={restart}>
          <option value={null}>All accounts</option>
          {#each mailAccounts as a (a.id)}
            <option value={a.id}>{a.address}</option>
          {/each}
        </select>
      </label>
    {:else}
      <span class="scope" title="Looking through {scopeLabel}">
        <Icon name="inbox" size={12} />
        {scopeLabel}
      </span>
    {/if}
  </div>

  <div class="controls narrow">
    <label class="find">
      <Icon name="search" size={13} />
      <input
        type="search"
        placeholder="Filter by name or address"
        aria-label="Filter senders by name or address"
        spellcheck="false"
        autocomplete="off"
        bind:this={search}
        bind:value={query}
        oninput={typed}
        use:focusOnMount
      />
    </label>
    <select
      class="kind"
      class:on={sort !== ''}
      bind:value={sort}
      aria-label="Which kind of mail"
      disabled={busy}
      onchange={restart}
    >
      <option value="">All categories</option>
      {#if ownCategories.length > 0}
        <optgroup label="Your categories">
          {#each ownCategories as c (c.id)}
            <option value="own:{c.id}">{c.name}</option>
          {/each}
        </optgroup>
      {/if}
      <optgroup label="Inbox tabs">
        {#each CATEGORY_TABS as tab (tab.key)}
          <option value="tab:{tab.key}">{tab.label}</option>
        {/each}
      </optgroup>
    </select>
  </div>
  {#if sortNote}
    <p class="note">{sortNote}</p>
  {/if}

  <div class="bulk">
    <label class="all">
      <input
        type="checkbox"
        bind:this={selectAll}
        checked={pageSelected}
        disabled={busy || shown.length === 0}
        onchange={togglePage}
      />
      <span>
        {chosen.length > 0 ? `${chosen.length} selected` : pages > 1 ? 'Select page' : 'Select all'}
      </span>
    </label>
    {#if pageSelected && chosen.length < rows.length}
      <button class="every" disabled={busy} onclick={selectEvery}>
        Select all {rows.length.toLocaleString()}
      </button>
    {/if}
    <span class="spacer"></span>
    <button
      class="btn"
      disabled={busy || loading || chosenThreads.length === 0}
      title={chosenThreads.length > 0
        ? `Archive ${plural(chosenThreads.length, 'conversation')}`
        : 'Tick a sender first'}
      onclick={() => bulk('archive')}
    >
      <Icon name="layers" size={13} />
      Archive selected{chosenThreads.length > 0 ? ` (${chosenThreads.length})` : ''}
    </button>
    <button
      class="btn btn-ghost-danger"
      disabled={busy || loading || chosenThreads.length === 0}
      title={chosenThreads.length > 0
        ? `Move ${plural(chosenThreads.length, 'conversation')} to Trash`
        : 'Tick a sender first'}
      onclick={() => bulk('trash')}
    >
      <Icon name="trash" size={13} />
      Delete selected{chosenThreads.length > 0 ? ` (${chosenThreads.length})` : ''}
    </button>
  </div>

  <div class="scroll body" aria-busy={loading} bind:this={scroller}>
    {#if rows.length === 0}
      {#if loading}
        <p class="hint">Loading…</p>
      {:else if failed}
        <p class="hint">Could not ask just now. Try another period, or again in a moment.</p>
      {:else if loaded}
        <p class="hint">{emptyHint}</p>
      {/if}
    {:else}
      <ul class="senders" class:stale={loading}>
        {#each shown as s (s.email)}
          {@const on = selected.has(s.email)}
          <li class="sender" class:on>
            <label class="pick">
              <input
                type="checkbox"
                checked={on}
                disabled={busy}
                onchange={() => toggle(s.email)}
              />
              <span class="who">
                <span class="name">{nameOf(s)}</span>
                {#if s.name}<span class="email">{s.email}</span>{/if}
              </span>
            </label>
            <span class="tally">
              {plural(s.messages, 'email')} · {s.unread.toLocaleString()} unread
            </span>
            <button
              class="act"
              title="Archive {plural(s.threads.length, 'conversation')}"
              disabled={busy || loading}
              onclick={() => void act('archive', [s])}
            >
              <Icon name="layers" size={13} />
              Archive
            </button>
            <button
              class="act danger"
              title="Move to Trash"
              disabled={busy || loading}
              onclick={() => void act('trash', [s])}
            >
              <Icon name="trash" size={13} />
              Delete
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  </div>

  {#if pages > 1}
    <footer class="pager">
      <span>
        {(at * PAGE + 1).toLocaleString()}–{(at * PAGE + shown.length).toLocaleString()} of
        {rows.length >= ALL ? `the top ${ALL.toLocaleString()}` : rows.length.toLocaleString()}
      </span>
      <button
        class="act prev"
        aria-label="Previous page"
        disabled={at === 0}
        onclick={() => turnTo(at - 1)}
      >
        <Icon name="chevron" size={13} />
      </button>
      <button
        class="act"
        aria-label="Next page"
        disabled={at >= pages - 1}
        onclick={() => turnTo(at + 1)}
      >
        <Icon name="chevron" size={13} />
      </button>
    </footer>
  {/if}
</div>

{#if asking}
  <ConfirmDialog
    title={askingTitle}
    detail="{plural(
      asking.unseen,
      'of those senders is',
      'of those senders are',
    )} on other pages, not on the one showing."
    confirmLabel={asking.move === 'archive' ? 'Archive' : 'Move to Trash'}
    danger={asking.move === 'trash'}
    onconfirm={answered}
    oncancel={() => (asking = null)}
  />
{/if}

<style>
  /* Wider and taller than a confirmation, the way `AddWidgetDialog` is: a
     twenty-row list with two buttons a row would be a column of wrapping
     lines in the shared 420px sheet. */
  .cleanup {
    top: 10%;
    display: flex;
    flex-direction: column;
    /* Room for the which-mail toggle, the periods and the account menu on
       one line, so switching between Last and Older than does not move
       the account menu down a row and back. */
    width: min(720px, calc(100vw - var(--sp-8)));
    max-height: 80vh;
    padding: 0;
  }
  .cleanup:focus {
    outline: none;
  }

  header {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    padding: var(--sp-4) var(--sp-4) var(--sp-3);
  }
  header > div {
    flex: 1;
  }
  h2 {
    margin: 0;
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
  }
  .lead {
    margin: var(--sp-1) 0 0;
    color: var(--fg-subtle);
    font-size: var(--text-sm);
  }
  .close {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: var(--radius-sm);
    color: var(--fg-subtle);
  }
  .close:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .controls {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    padding: 0 var(--sp-4) var(--sp-3);
  }
  /* The same segmented control Settings → Meeting notes draws. */
  .segmented {
    display: flex;
    gap: 3px;
    padding: 3px;
    background: var(--bg-sunken);
    border-radius: var(--radius);
  }
  .seg {
    height: 26px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-subtle);
    white-space: nowrap;
  }
  .seg.on {
    background: var(--bg-raised);
    color: var(--fg);
    box-shadow: var(--shadow-sm);
  }
  .seg:disabled {
    opacity: 0.6;
  }
  .scope {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
    margin-left: auto;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .controls select {
    min-width: 0;
    height: 28px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    color: var(--fg-muted);
    font-size: var(--text-sm);
    text-overflow: ellipsis;
    cursor: pointer;
  }
  .scope select {
    max-width: 200px;
  }
  .controls select:disabled {
    opacity: 0.6;
  }

  /* The filter box and the kind of mail, under the period: the two ways
     into a list too long to read down. */
  .narrow {
    flex-wrap: nowrap;
  }
  .find {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: 32px;
    padding: 0 var(--sp-2);
    background: var(--bg-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--fg-faint);
    transition:
      border-color var(--fast) var(--ease),
      box-shadow var(--fast) var(--ease);
  }
  .find:focus-within {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 18%, transparent);
  }
  .find input {
    flex: 1;
    min-width: 0;
    height: 100%;
    padding: 0;
    border: none;
    background: none;
    color: var(--fg);
    font-size: var(--text-sm);
    user-select: text;
  }
  /* The box around it draws the focus instead. */
  .find input:focus-visible {
    outline: none;
  }
  .find input::placeholder {
    color: var(--fg-faint);
  }
  .narrow .kind {
    flex: none;
    max-width: 200px;
    height: 32px;
    border-radius: var(--radius);
  }
  /* A kind picked is a filter in force, and says so. */
  .narrow .kind.on {
    color: var(--fg);
    border-color: var(--accent);
  }

  .bulk {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-2) var(--sp-4);
    border-top: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
  }
  .bulk .spacer {
    flex: 1;
  }
  .bulk .btn {
    height: 30px;
    padding: 0 var(--sp-3);
    font-size: var(--text-sm);
  }
  .note {
    margin: 0;
    padding: 0 var(--sp-4) var(--sp-3);
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }

  .every {
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--accent);
  }
  .every:hover {
    text-decoration: underline;
  }
  .all {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-sm);
    color: var(--fg-muted);
    cursor: pointer;
  }

  input[type='checkbox'] {
    flex: none;
    accent-color: var(--accent);
  }

  .body {
    flex: 1;
    min-height: 0;
  }
  .body .hint {
    margin: 0;
    padding: var(--sp-4);
  }

  .senders {
    margin: 0;
    padding: var(--sp-1) 0 var(--sp-2);
    list-style: none;
    transition: opacity var(--fast) var(--ease);
  }
  /* An answer for another period on its way: the rows shown are the last
     period's, so they recede rather than vanish into "Loading…". */
  .senders.stale {
    opacity: 0.55;
  }
  .sender {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: 6px var(--sp-4);
  }
  .sender:hover,
  .sender.on {
    background: var(--bg-hover);
  }
  .pick {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    cursor: pointer;
  }
  .who {
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .name,
  .email {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .name {
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg);
  }
  .email {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .tally {
    flex: none;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .act {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 26px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-xs);
    color: var(--fg-muted);
  }
  .act:hover {
    background: var(--bg-active);
    color: var(--fg);
  }
  .act.danger:hover {
    background: color-mix(in oklab, var(--danger) 12%, transparent);
    color: var(--danger);
  }
  .act:disabled {
    opacity: 0.45;
    pointer-events: none;
  }

  .pager {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--sp-1);
    padding: var(--sp-2) var(--sp-4);
    border-top: 1px solid var(--border);
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .pager span {
    margin-right: var(--sp-2);
  }
  .prev :global(svg) {
    transform: rotate(180deg);
  }
</style>
