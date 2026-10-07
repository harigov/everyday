<script lang="ts">
  // Quick cleanup: who filled the Inbox most over the last week, month,
  // quarter or year, or ever -- counting only what of theirs is still there --
  // and a way to archive or bin all of it, one sender or several at a time.
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
  import * as mailApi from '../lib/mail-api'
  import { mail } from '../lib/mail.svelte'
  import { notify } from '../lib/notify.svelte'
  import { handle } from '../lib/state.svelte'
  import type { AccountId, InboxSender, ThreadId } from '../lib/types'
  import Icon from './Icon.svelte'

  let {
    accountId,
    onclose,
  }: {
    /** The one account to look through, or `null` for every mail account. */
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
  const TOPS = [10, 20]

  let days = $state<number | null>(30)
  let limit = $state(20)
  /** Only ever replaced whole, never edited in place -- `.raw`, as the
   *  store's own `threads` is. */
  let senders = $state.raw<InboxSender[]>([])
  /** Asking `inbox_senders` -- the period or the cutoff just changed. Not
   *  set by the quiet re-ask after an action, which has rows to show. */
  let loading = $state(true)
  /** Has any answer landed yet -- what tells "nothing here" from "not yet". */
  let loaded = $state(false)
  let failed = $state(false)
  /** An archive or a delete is on its way. */
  let busy = $state(false)
  /** By sender address, the key `inbox_senders` groups on. */
  let selected = $state<Set<string>>(new Set())

  let dialog = $state<HTMLElement>()
  let selectAll = $state<HTMLInputElement>()

  /** Which ask is current, so a slow answer for "1 year" cannot land on top
   *  of the quicker one for "7 days" that was asked for after it. */
  let generation = 0

  const period = $derived(PERIODS.find((p) => p.days === days) ?? PERIODS[1]!)

  /** Who this is looking through, in words: one account's address, or every
   *  account's -- which, with only one, is still just that one address. */
  const scopeLabel = $derived.by(() => {
    if (accountId) return accounts.account(accountId)?.address ?? 'This account'
    const withMail = accounts.list.filter((a) => a.services.mail)
    const only = withMail.length === 1 ? withMail[0] : undefined
    return only ? only.address : 'All accounts'
  })

  const chosen = $derived(senders.filter((s) => selected.has(s.email)))
  const chosenThreads = $derived(threadsOf(chosen))
  const allSelected = $derived(senders.length > 0 && chosen.length === senders.length)

  // A header checkbox's third state is a DOM property with no attribute
  // behind it, so it is set on the element rather than in the markup.
  $effect(() => {
    if (selectAll) selectAll.indeterminate = chosen.length > 0 && !allSelected
  })

  // Asked again whenever the period or the cutoff changes, and for nothing
  // else: the ask itself is untracked, so neither the rows it writes once
  // the answer lands nor anything the call reads on its way out can send it
  // round a second time.
  $effect(() => {
    const span = days
    const top = limit
    untrack(() => void load(span, top))
  })

  async function load(span: number | null, top: number, quiet = false) {
    const token = ++generation
    if (!quiet) loading = true
    try {
      const found = await mailApi.inboxSenders(span, accountId ? [accountId] : null, top)
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

  function toggleAll() {
    selected = allSelected ? new Set() : new Set(senders.map((s) => s.email))
  }

  async function act(kind: 'archive' | 'trash', list: readonly InboxSender[]) {
    if (busy) return
    const ids = threadsOf(list)
    if (ids.length === 0) return
    busy = true
    let ok = false
    try {
      ok = kind === 'archive' ? await mail.archiveThreads(ids) : await mail.trashThreads(ids)
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
        kind === 'archive'
          ? `Archived ${plural(ids.length, 'conversation')}`
          : `Moved ${plural(ids.length, 'conversation')} to Trash`,
        { body: fromLine(list) },
      )
      // The button just pressed may have gone with its row; focus would
      // otherwise fall out of the dialog to the page behind it.
      dialog?.focus()
    }
    // Asked again either way -- after a failure partway, some of it did go.
    void load(days, limit, true)
  }
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape') onclose()
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
  use:focusOnMount
  use:trapFocus
>
  <header>
    <div>
      <h2 id="mail-cleanup-title">Quick cleanup</h2>
      <p class="lead" id="mail-cleanup-lead">
        Who filled your inbox most{period.within ? ` in ${period.within}` : ''}, counting only what
        is still there.
      </p>
    </div>
    <button class="close" aria-label="Close" onclick={onclose}>
      <Icon name="close" size={14} />
    </button>
  </header>

  <div class="controls">
    <div class="segmented" role="group" aria-label="Over the last">
      {#each PERIODS as p (p.label)}
        <button
          class="seg"
          class:on={days === p.days}
          aria-pressed={days === p.days}
          disabled={busy}
          onclick={() => (days = p.days)}
        >
          {p.label}
        </button>
      {/each}
    </div>
    <div class="segmented" role="group" aria-label="How many senders">
      {#each TOPS as n (n)}
        <button
          class="seg"
          class:on={limit === n}
          aria-pressed={limit === n}
          disabled={busy}
          onclick={() => (limit = n)}
        >
          Top {n}
        </button>
      {/each}
    </div>
    <span class="scope" title="Looking through {scopeLabel}">
      <Icon name="inbox" size={12} />
      {scopeLabel}
    </span>
  </div>

  <div class="bulk">
    <label class="all">
      <input
        type="checkbox"
        bind:this={selectAll}
        checked={allSelected}
        disabled={busy || senders.length === 0}
        onchange={toggleAll}
      />
      <span>{chosen.length > 0 ? `${chosen.length} selected` : 'Select all'}</span>
    </label>
    <span class="spacer"></span>
    <button
      class="btn"
      disabled={busy || loading || chosenThreads.length === 0}
      title={chosenThreads.length > 0
        ? `Archive ${plural(chosenThreads.length, 'conversation')}`
        : 'Tick a sender first'}
      onclick={() => void act('archive', chosen)}
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
      onclick={() => void act('trash', chosen)}
    >
      <Icon name="trash" size={13} />
      Delete selected{chosenThreads.length > 0 ? ` (${chosenThreads.length})` : ''}
    </button>
  </div>

  <div class="scroll body" aria-busy={loading}>
    {#if senders.length === 0}
      {#if loading}
        <p class="hint">Loading…</p>
      {:else if failed}
        <p class="hint">Could not ask just now. Try another period, or again in a moment.</p>
      {:else if loaded}
        <p class="hint">Nothing in your inbox{period.within ? ` from ${period.within}` : ''}.</p>
      {/if}
    {:else}
      <ul class="senders" class:stale={loading}>
        {#each senders as s (s.email)}
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
</div>

<style>
  /* Wider and taller than a confirmation, the way `AddWidgetDialog` is: a
     twenty-row list with two buttons a row would be a column of wrapping
     lines in the shared 420px sheet. */
  .cleanup {
    top: 10%;
    display: flex;
    flex-direction: column;
    width: min(640px, calc(100vw - var(--sp-8)));
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
</style>
