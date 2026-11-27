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
  //
  // Two more things can be done with a sender than moving their mail. They
  // can be asked to stop (`unsubscribe`), with their conversations archived
  // as well or left where they are; the row then says "Unsubscribed" rather
  // than offering it again as new. And they can be filed under another kind
  // of mail (`set_sender_category`), their new mail with them -- how the
  // newsletters that are not newsletters get out of that tab in bulk. Both
  // hand over the row's own conversations and accounts, never a bare
  // address, for the reason the archive does.

  import { untrack } from 'svelte'
  import { accounts } from '../lib/accounts.svelte'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import { plural } from '../lib/format'
  import { CATEGORY_TABS, senderMatches } from '../lib/mail'
  import * as mailApi from '../lib/mail-api'
  import { mail } from '../lib/mail.svelte'
  import type { MenuItem } from '../lib/menu'
  import { menu } from '../lib/menu.svelte'
  import { notify } from '../lib/notify.svelte'
  import { openExternal } from '../lib/open-external'
  import { closesOnBack } from '../lib/phone.svelte'
  import { sorting } from '../lib/sorting.svelte'
  import { handle } from '../lib/state.svelte'
  import type {
    AccountId,
    InboxSender,
    InboxSendersArgs,
    MailCategory,
    SenderRef,
    ThreadId,
    UnsubscribeOutcome,
    UnsubscribeVia,
  } from '../lib/types'
  import {
    askedLine,
    manyDetail,
    needsAttention,
    sortOutcomes,
    summarize,
  } from '../lib/unsubscribe'
  import ChoiceDialog from './ChoiceDialog.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'
  import Loading from './Loading.svelte'
  import UnsubscribeResults from './UnsubscribeResults.svelte'

  let {
    accountId,
    kind = '',
    onclose,
  }: {
    /** The account to open on, or `null` for every mail account. */
    accountId: AccountId | null
    /** The kind of mail to open on, spelt as `sort` spells it --
     *  `tab:newsletter` from Mail's Newsletters tab -- or every kind when
     *  absent. Only where it starts: the menu changes it from there. */
    kind?: string
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
  /** How many of a sender's conversations, newest first, are handed over
   *  to find how to leave their list. The header that counts is on their
   *  newest mail, and a sender with years of it would otherwise send
   *  thousands; "and archive" still takes every one. */
  const LOOK_IN = 20
  /** Senders asked to stop in one `unsubscribe`, and senders re-filed in
   *  one `set_sender_category`: well inside what one call takes, so "Select
   *  all" over a long list goes in turns rather than being refused. */
  const ASK_AT_ONCE = 100
  const MOVE_AT_ONCE = 1000

  let days = $state<number | null>(30)
  /** Counting mail older than `days` rather than within it. */
  let older = $state(false)
  let account = $state<AccountId | null>(untrack(() => accountId))
  /** `''` for every kind of mail, `tab:<category>` for an inbox tab,
   *  `own:<id>` for one of the person's own sorted categories -- one
   *  string, so one `<select>` can offer both. */
  let sort = $state(untrack(() => kind))
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
  /** An archive, a delete, an unsubscribe or a move is on its way. */
  let busy = $state(false)
  /** By sender address, the key `inbox_senders` groups on. */
  let selected = $state<Set<string>>(new Set())
  /** A bulk action waiting on a yes, because it reaches rows on other
   *  pages: an archive, a trash, or a move to another kind of mail. */
  type Asking =
    | { move: 'archive' | 'trash'; list: InboxSender[]; unseen: number }
    | { move: 'file'; to: MailCategory; list: InboxSender[]; unseen: number }
  let asking = $state<Asking | null>(null)
  /** An unsubscribe waiting on how: with the senders' conversations
   *  archived too, or left where they are. Always asked -- it writes to
   *  them in the person's name, and cannot be taken back. `unseen` counts
   *  the senders on other pages, which the question mentions. */
  let unsubscribing = $state.raw<{ list: InboxSender[]; unseen: number } | null>(null)
  /** What an unsubscribe left over: pages to open, and senders it could not
   *  reach. Listed until the person says they are done. */
  let leftOver = $state.raw<UnsubscribeOutcome[] | null>(null)

  // A phone's back button closes the question over the dialog first, then
  // the dialog, as Escape does.
  closesOnBack(
    () => true,
    () => onclose(),
  )
  closesOnBack(
    () => asking !== null,
    () => (asking = null),
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
  /** The ticked senders whose mail says how to leave their list. */
  const unsubscribable = $derived(chosen.filter((s) => !!s.unsubscribe))
  /** The inbox tab the list is narrowed to, which "Move to…" leaves out. */
  const inTab = $derived(sort.startsWith('tab:') ? (sort.slice(4) as MailCategory) : null)
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

  /** How many of `list` are on other pages than the one showing. */
  function offPage(list: readonly InboxSender[]): number {
    const onPage = new Set(shown.map((s) => s.email))
    return list.filter((s) => !onPage.has(s.email)).length
  }

  /** The bulk buttons: straight on when every ticked row is on this page,
   *  asked first when some are not. "Select all" reaches rows nobody has
   *  looked at, and two clicks should not clear out an Inbox unseen. */
  function bulk(move: 'archive' | 'trash') {
    const unseen = offPage(chosen)
    if (unseen > 0) asking = { move, list: chosen, unseen }
    else void act(move, chosen)
  }

  /** "Move to…"'s choice, asked first on the same terms as `bulk`. */
  function fileChosen(to: MailCategory) {
    const unseen = offPage(chosen)
    if (unseen > 0) asking = { move: 'file', to, list: chosen, unseen }
    else void file(chosen, to)
  }

  /** Yes to `bulk`'s question. Taken out of `asking` before it is cleared,
   *  which takes the dialog down. */
  function answered() {
    const question = asking
    asking = null
    if (!question) return
    if (question.move === 'file') void file(question.list, question.to)
    else void act(question.move, question.list)
  }

  /** An inbox tab's name, as the tabs and the kind menu say it. */
  function labelOf(category: MailCategory): string {
    return CATEGORY_TABS.find((t) => t.key === category)?.label ?? category
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

  // ── Unsubscribing ───────────────────────────────────────────────────

  /** One row's Unsubscribe, or the ticked rows': always asked first. */
  function askUnsubscribe(list: readonly InboxSender[]) {
    if (busy || list.length === 0) return
    unsubscribing = { list: [...list], unseen: offPage(list) }
  }

  /** An answer to `askUnsubscribe`'s question, taken out of it before the
   *  question goes, as `answered` does. */
  function unsubscribeAnswered(archive: boolean) {
    const question = unsubscribing
    unsubscribing = null
    if (question) void unsubscribeFrom(question.list, archive)
  }

  /** The row's Unsubscribe, in a few words: how they would be asked. */
  function wayTitle(via: UnsubscribeVia): string {
    switch (via) {
      case 'oneClick':
        return 'Unsubscribe in one click'
      case 'mail':
        return 'Unsubscribe by sending them an email'
      case 'web':
        return 'Unsubscribe on their website'
    }
  }

  /** What unsubscribing from one sender does, by the way their mail
   *  offers. */
  function wayDetail(via: UnsubscribeVia | null | undefined, name: string): string {
    switch (via) {
      case 'oneClick':
        return `${name} is asked to take you off their list, in one click. Nothing opens in your browser.`
      case 'mail':
        return `An email asking ${name} to take you off their list goes from your address.`
      case 'web':
        return `${name} only unsubscribes on their website, so their page opens in your browser for you to finish there.`
      default:
        return `${name}'s mail names no way to unsubscribe.`
    }
  }

  /** `unsubscribing`'s question in words, and its two answers -- the one
   *  that reaches further second, as `ChoiceDialog` orders them. */
  const unsubscribeQuestion = $derived.by(() => {
    if (!unsubscribing) return null
    const { list, unseen } = unsubscribing
    const [only] = list
    if (list.length === 1 && only) {
      const name = nameOf(only)
      const asked = askedLine(only.unsubscribedAt)
      const how = `${wayDetail(only.unsubscribe, name)} Their emails already in your inbox stay there unless you archive them too.`
      const web = only.unsubscribe === 'web'
      return {
        title: asked ? `Unsubscribe from ${name} again?` : `Unsubscribe from ${name}?`,
        detail: asked ? `${asked}. ${how}` : how,
        plain: web ? 'Open their page' : 'Unsubscribe only',
        archive: web ? 'Open their page and archive' : 'Unsubscribe and archive',
      }
    }
    // Senders whose mail names no way out are still handed over: the
    // answer lists them afterwards, and "and archive" clears them out too.
    const without = list.filter((s) => !s.unsubscribe).length
    const parts = [manyDetail(list.length - without)]
    if (without > 0) {
      parts.push(
        `${plural(without, 'other sender offers', 'other senders offer')} no way to unsubscribe.`,
      )
    }
    if (unseen > 0) {
      parts.push(
        `${plural(unseen, 'of those senders is', 'of those senders are')} on other pages, not on the one showing.`,
      )
    }
    return {
      title: `Unsubscribe from ${plural(list.length, 'sender')}?`,
      detail: parts.join(' '),
      plain: 'Unsubscribe only',
      archive: 'Unsubscribe and archive',
    }
  })

  /**
   * Ask `list`'s senders to take the person off their lists, then archive
   * their conversations when that was the answer -- only once the asking
   * went: if it failed outright, their mail stays where it was, to try
   * again from. One sender's failure is theirs alone; the outcomes say who
   * was asked, whose page is left to open and who could not be reached.
   * The rows asked say "Unsubscribed" straight away, or leave with their
   * conversations, as `act` drops them. A long selection is asked in turns
   * (`ASK_AT_ONCE`), each sender by their newest conversations
   * (`LOOK_IN`).
   */
  async function unsubscribeFrom(list: readonly InboxSender[], archive: boolean) {
    if (busy) return
    const ids = threadsOf(list)
    if (ids.length === 0) return
    busy = true
    // The rows to mark are the ones the press was on -- unless another
    // answer replaced them meanwhile, which then has the say.
    const token = generation
    const outcomes: UnsubscribeOutcome[] = []
    let failed = false
    let archived = false
    try {
      for (let at = 0; at < list.length; at += ASK_AT_ONCE) {
        const part = list.slice(at, at + ASK_AT_ONCE)
        const look = [...new Set(part.flatMap((s) => s.threads.slice(0, LOOK_IN)))]
        outcomes.push(
          ...(await mailApi.unsubscribe(
            look,
            part.map((s) => s.email),
          )),
        )
      }
      if (archive) archived = await mail.archiveThreads(ids)
    } catch (e) {
      failed = true
      await handle(e)
    } finally {
      busy = false
    }
    // After a failure partway, the senders already asked still were.
    const heard = !failed || outcomes.length > 0
    if (heard) {
      // Every sender pressed for -- or, after a failure partway, the ones
      // it reached.
      const acted = new Set(failed ? outcomes.map((o) => o.email) : list.map((s) => s.email))
      if (archived) {
        const gone = new Set(ids)
        senders = senders.filter((s) => !acted.has(s.email) && !s.threads.every((t) => gone.has(t)))
      } else if (token === generation) {
        const asked = new Set(outcomes.filter((o) => o.done).map((o) => o.email))
        const now = new Date().toISOString()
        senders = senders.map((s) => (asked.has(s.email) ? { ...s, unsubscribedAt: now } : s))
      }
      // Done with: a tick left on would only invite the same press again.
      const kept = (email: string) => !acted.has(email) && senders.some((s) => s.email === email)
      selected = new Set([...selected].filter(kept))
      // The button just pressed may have gone with its row.
      dialog?.focus()
    }
    void load(filters(), true)
    // A page is opened straight away only when it was the way out the
    // question named for a single sender: after a refused one-click request
    // it is listed instead, since the question said nothing would open.
    const promised = list.length === 1 && list[0]?.unsubscribe === 'web'
    if (heard) await report(outcomes, archived ? ids.length : 0, promised)
  }

  /**
   * Say how an unsubscribe went. A single page left to open is opened
   * straight away when `promised` (the question said a page would open) --
   * the person pressed Unsubscribe on that one sender, and finishing there
   * is the rest of the same press -- and anything else left
   * over (several pages, a sender it could not reach, a page the browser
   * would not take) is listed, with a button for each page.
   */
  async function report(outcomes: UnsubscribeOutcome[], archived: number, promised: boolean) {
    const tail = archived > 0 ? `Archived ${plural(archived, 'conversation')}.` : ''
    const [web] = sortOutcomes(outcomes).pages
    if (promised && outcomes.length === 1 && web?.url && (await openExternal(web.url))) {
      notify.success(`Finish unsubscribing from ${web.name || web.email} on their page`, {
        body: ['It is open in your browser.', tail].filter(Boolean).join(' '),
      })
      return
    }
    const said = summarize(outcomes)
    const body = [said.body, tail].filter(Boolean).join(' ')
    const opts = body ? { body } : {}
    if (said.ok) notify.success(said.title, opts)
    else notify.warn(said.title, opts)
    if (needsAttention(outcomes)) leftOver = outcomes
  }

  // ── Filing senders under another kind of mail ───────────────────────

  /** "Move to…"'s menu: every inbox tab but the one the list is narrowed
   *  to, which is where these senders already are. */
  function moveItems(): MenuItem[] {
    return [
      { kind: 'heading', label: 'Move their mail to' },
      ...CATEGORY_TABS.filter((t) => t.key !== inTab).map((t): MenuItem => ({
        label: t.label,
        run: () => fileChosen(t.key),
      })),
    ]
  }

  /**
   * File every message from `list`'s senders under `to`, in each account
   * they write to, and keep their new mail there -- the newsletters that
   * are not, moved where they belong. Under a tab, their rows leave at
   * once; the quiet re-ask settles the rest.
   */
  async function file(list: readonly InboxSender[], to: MailCategory) {
    if (busy || list.length === 0) return
    const refs: SenderRef[] = list.flatMap((s) =>
      s.accounts.map((account) => ({ account, email: s.email })),
    )
    busy = true
    let moved = false
    try {
      for (let at = 0; at < refs.length; at += MOVE_AT_ONCE) {
        await mailApi.setSenderCategory(refs.slice(at, at + MOVE_AT_ONCE), to)
      }
      moved = true
    } catch (e) {
      await handle(e)
    } finally {
      busy = false
    }
    if (moved) {
      const acted = new Set(list.map((s) => s.email))
      if (inTab) senders = senders.filter((s) => !acted.has(s.email))
      selected = new Set([...selected].filter((email) => !acted.has(email)))
      notify.success(`Moved ${plural(list.length, 'sender')} to ${labelOf(to)}`, {
        body: 'Their new mail goes there too.',
      })
      dialog?.focus()
    }
    // Asked again either way -- after a failure partway, some of them did
    // move -- and Mail's own list and tab counts, behind this dialog, too.
    void mail.refresh()
    void mail.refreshCategoryCounts()
    void load(filters(), true)
  }

  /** The question `bulk` and "Move to…" ask, in words. */
  const askingTitle = $derived.by(() => {
    if (!asking) return ''
    const from = plural(asking.list.length, 'sender')
    if (asking.move === 'file') return `Move ${from} to ${labelOf(asking.to)}?`
    const what = plural(threadsOf(asking.list).length, 'conversation')
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
    // A question or a list over the dialog takes its own Escape.
    if (e.key !== 'Escape' || asking || unsubscribing || leftOver) return
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
          what is still there. Archived mail is not counted, but the buttons move whole
          conversations.
        {/if}
      </p>
    </div>
    <button class="close" aria-label="Close" title="Close" onclick={onclose}>
      <Icon name="close" size={14} />
    </button>
  </header>

  <div class="controls">
    <div class="seg-group" role="radiogroup" aria-label="Which mail">
      <button
        class="seg-opt"
        class:on={!older}
        role="radio"
        aria-checked={!older}
        disabled={busy}
        onclick={() => turn(false)}
      >
        Last
      </button>
      <button
        class="seg-opt"
        class:on={older}
        role="radio"
        aria-checked={older}
        disabled={busy}
        onclick={() => turn(true)}
      >
        Older than
      </button>
    </div>
    <div class="seg-group" role="radiogroup" aria-label={older ? 'Older than' : 'Over the last'}>
      {#each PERIODS.filter((p) => !older || p.days !== null) as p (p.label)}
        <button
          class="seg-opt"
          class:on={days === p.days}
          role="radio"
          aria-checked={days === p.days}
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
      disabled={busy || loading || unsubscribable.length === 0}
      title={unsubscribable.length > 0
        ? `Ask ${plural(unsubscribable.length, 'sender')} to take you off their list`
        : chosen.length > 0
          ? 'None of these senders says how to unsubscribe'
          : 'Tick a sender first'}
      onclick={() => askUnsubscribe(chosen)}
    >
      <Icon name="mail-x" size={13} />
      Unsubscribe
    </button>
    <button
      class="btn"
      aria-haspopup="menu"
      disabled={busy || loading || chosen.length === 0}
      title={chosen.length > 0
        ? `File ${plural(chosen.length, 'sender')} under another kind of mail`
        : 'Tick a sender first'}
      onclick={(e) => menu.show(e, moveItems())}
    >
      <Icon name="tag" size={13} />
      Move to…
    </button>
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
      Trash selected{chosenThreads.length > 0 ? ` (${chosenThreads.length})` : ''}
    </button>
  </div>

  <div class="scroll body" aria-busy={loading} bind:this={scroller}>
    {#if rows.length === 0}
      {#if loading}
        <Loading label="Reading your mail…" centred />
      {:else if failed}
        <p class="hint">Could not ask just now. Try another period, or again in a moment.</p>
      {:else if loaded}
        <p class="hint">{emptyHint}</p>
      {/if}
    {:else}
      <ul class="senders" class:stale={loading}>
        {#each shown as s (s.email)}
          {@const on = selected.has(s.email)}
          <li class="sender" class:on class:plain={!s.unsubscribe && !s.unsubscribedAt}>
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
            <!-- Asked already: said quietly, and pressed again to ask again
                 (a list that kept writing, or a page never finished). -->
            {#if s.unsubscribedAt}
              {@const asked = askedLine(s.unsubscribedAt) ?? 'You asked to unsubscribe'}
              {#if s.unsubscribe}
                <button
                  class="act out asked"
                  title="{asked}. Press to ask again."
                  disabled={busy || loading}
                  onclick={() => askUnsubscribe([s])}
                >
                  <Icon name="check" size={13} />
                  Unsubscribed
                </button>
              {:else}
                <span class="mark out" title={asked}>
                  <Icon name="check" size={13} />
                  Unsubscribed
                </span>
              {/if}
            {:else if s.unsubscribe}
              <button
                class="act out"
                title={wayTitle(s.unsubscribe)}
                disabled={busy || loading}
                onclick={() => askUnsubscribe([s])}
              >
                <Icon name="mail-x" size={13} />
                Unsubscribe
              </button>
            {/if}
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
              Trash
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
        title="Previous page"
        disabled={at === 0}
        onclick={() => turnTo(at - 1)}
      >
        <Icon name="chevron" size={13} />
      </button>
      <button
        class="act"
        aria-label="Next page"
        title="Next page"
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
    confirmLabel={asking.move === 'archive'
      ? 'Archive'
      : asking.move === 'trash'
        ? 'Move to Trash'
        : 'Move'}
    danger={asking.move === 'trash'}
    onconfirm={answered}
    oncancel={() => (asking = null)}
  />
{/if}

{#if unsubscribeQuestion}
  <ChoiceDialog
    title={unsubscribeQuestion.title}
    detail={unsubscribeQuestion.detail}
    options={[
      { label: unsubscribeQuestion.plain, run: () => unsubscribeAnswered(false) },
      { label: unsubscribeQuestion.archive, run: () => unsubscribeAnswered(true) },
    ]}
    oncancel={() => (unsubscribing = null)}
  />
{/if}

{#if leftOver}
  <UnsubscribeResults
    outcomes={leftOver}
    onclose={() => {
      leftOver = null
      dialog?.focus()
    }}
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
  /* The shared `.seg-group`, kept to its words on the one line. */
  .controls .seg-opt {
    white-space: nowrap;
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

  /* Four buttons and the count beside them fill the line; with "Select
     all" as well, the buttons go on to a second rather than off the edge. */
  .bulk {
    display: flex;
    flex-wrap: wrap;
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
  /* Already asked: said, not offered, until it is hovered. */
  .act.asked {
    color: var(--fg-faint);
  }
  .act.asked:hover {
    color: var(--fg);
  }
  .mark {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 26px;
    padding: 0 var(--sp-2);
    font-size: var(--text-xs);
    color: var(--fg-faint);
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

  /* ── On a phone ─────────────────────────────────────────────────────
     The whole screen between the bars, not a box in the middle of it: the
     controls stacked, each sender a name over a line of its numbers and two
     buttons, every one a finger tall. (The bar of apps stays: it is above a
     sheet, and the question this asks over itself has to be above this.)
     Under `data-phone`; the desktop's sheet is as it was. */
  :global([data-phone]) .cleanup {
    inset: calc(var(--topbar-h) + var(--safe-top)) var(--safe-right)
      calc(var(--bottombar-h) + var(--safe-bottom)) var(--safe-left);
    translate: none;
    width: auto;
    max-height: none;
    border: 0;
    border-radius: 0;
  }
  :global([data-phone]) .cleanup header {
    align-items: center;
    padding: var(--sp-2) var(--sp-2) var(--sp-2) var(--sp-4);
  }
  :global([data-phone]) .cleanup .close {
    align-self: flex-start;
    width: 44px;
    height: 44px;
    border-radius: 50%;
  }
  :global([data-phone]) .controls {
    padding: 0 var(--sp-4) var(--sp-2);
  }
  :global([data-phone]) .controls .seg-group {
    max-width: 100%;
  }
  :global([data-phone]) .controls .seg-opt {
    min-width: 0;
    padding: 0 var(--sp-2);
  }
  /* The periods share the line; the one-line choice of Last or Older than
     keeps to its own words. */
  :global([data-phone]) .controls > .seg-group:nth-child(2) {
    flex: 1 1 100%;
  }
  :global([data-phone]) .scope {
    flex: 1 1 100%;
    margin-left: 0;
  }
  :global([data-phone]) .scope select {
    flex: 1;
    max-width: none;
  }
  :global([data-phone]) .controls select {
    height: 44px;
    font-size: 16px;
  }
  :global([data-phone]) .narrow {
    flex-wrap: wrap;
  }
  :global([data-phone]) .find {
    flex: 1 1 100%;
    height: 44px;
  }
  :global([data-phone]) .find input {
    font-size: 16px;
  }
  :global([data-phone]) .narrow .kind {
    flex: 1 1 100%;
    max-width: none;
    height: 44px;
  }
  :global([data-phone]) .bulk {
    flex-wrap: wrap;
    gap: var(--sp-1) var(--sp-2);
    padding: var(--sp-1) var(--sp-4) var(--sp-2);
  }
  :global([data-phone]) .bulk .all {
    min-height: 44px;
    flex: 1 1 100%;
  }
  :global([data-phone]) .bulk .spacer {
    display: none;
  }
  :global([data-phone]) .bulk .btn {
    flex: 1 1 calc(50% - var(--sp-1));
    height: auto;
    min-height: 44px;
    padding: var(--sp-1) var(--sp-2);
    line-height: 1.25;
  }
  :global([data-phone]) .every {
    min-height: 44px;
  }
  :global([data-phone]) input[type='checkbox'] {
    width: 22px;
    height: 22px;
  }
  :global([data-phone]) .body {
    overscroll-behavior: contain;
  }
  /* The way out of a sender's list sits beside their name: the line under
     it already holds the numbers and two buttons. A sender with none has
     the whole line for their name. */
  :global([data-phone]) .sender {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto auto;
    grid-template-areas:
      'pick pick unsub'
      'tally archive trash';
    align-items: center;
    gap: 0 var(--sp-2);
    padding: var(--sp-1) var(--sp-4) var(--sp-2);
    border-bottom: 1px solid var(--border);
  }
  :global([data-phone]) .pick {
    grid-area: pick;
    min-height: 48px;
  }
  :global([data-phone]) .sender.plain .pick {
    grid-column: 1 / -1;
  }
  :global([data-phone]) .name,
  :global([data-phone]) .email {
    font-size: var(--text-base);
  }
  :global([data-phone]) .email {
    font-size: var(--text-sm);
  }
  :global([data-phone]) .tally {
    grid-area: tally;
    font-size: var(--text-sm);
  }
  :global([data-phone]) .sender .act {
    grid-area: archive;
    height: 40px;
    padding: 0 var(--sp-3);
    border: 1px solid var(--border);
    font-size: var(--text-sm);
  }
  /* Each row is a grid of its own, so the last column is as wide as the
     wider of Trash and the way out above it; both keep to their own width
     at its right-hand edge rather than stretching to fill it. */
  :global([data-phone]) .sender .act.danger {
    grid-area: trash;
    justify-self: end;
  }
  :global([data-phone]) .sender .out {
    grid-area: unsub;
    justify-self: end;
    padding: 0 var(--sp-2);
  }
  :global([data-phone]) .sender .mark {
    height: 40px;
    font-size: var(--text-sm);
  }
  :global([data-phone]) .pager {
    padding: var(--sp-1) var(--sp-3);
  }
  :global([data-phone]) .pager .act {
    width: 44px;
    height: 44px;
    justify-content: center;
    padding: 0;
  }
</style>
