<script lang="ts">
  // Settings → Mail sorting: the categories a decision model sorts mail into.
  //
  // Each category is a name, what belongs in it in the person's own words,
  // and the inbox tab it files under. The name and the description are what
  // the model is shown -- they are the options it picks between -- so the
  // description is where somebody says what should and should not count.
  // The tab is what the rest of the app understands: Priority, Important,
  // Other, Newsletters, Notifications. See `karmayoga_core::mail::sorting`.
  //
  // Recent mail is sorted unasked; older mail only from the button on its own
  // tab, because sorting somebody's whole archive on a settings change is a
  // bill and a reshuffle nobody agreed to. That tab has no Save: its button
  // does the work and the saved categories are what it uses. It used to sit
  // under the page's Save, which put the Save in the middle of the scroll.
  //
  // Follow-ups are here too, rather than a pane of their own: the same model,
  // under the same per-account switch, reading the same mail, decides which
  // threads wait at the top of the Inbox -- `karmayoga_core::mail::follow_up`.
  // With no decision model it is the Small model that decides, and the card
  // says which, per account how far it has got, and what holds one back: the
  // pass runs in the background and used to say nothing when it could not.

  import { onDestroy } from 'svelte'
  import { accounts } from '../lib/accounts.svelte'
  import { longDate, plural } from '../lib/format'
  import {
    CATEGORY_TABS,
    FOLLOW_UP_DEFAULTS,
    FOLLOW_UP_MAX_WINDOW_DAYS,
    FOLLOW_UP_MIN_WINDOW_DAYS,
    followUpAccountLine,
    followUpHint,
    followUpModelLine,
    followUpSettingsProblems,
  } from '../lib/mail'
  import { panels } from '../lib/panels.svelte'
  import { sorting } from '../lib/sorting.svelte'
  import type {
    FollowUpAccountStatus,
    FollowUpSettings,
    MailCategory,
    MailSorting,
    SortCategory,
  } from '../lib/types'
  import Icon from './Icon.svelte'
  import Loading from './Loading.svelte'
  import SubTabs from './SubTabs.svelte'

  /** `MailSorting` as this pane edits it: with its follow-up settings always
   *  there to bind to, the defaults standing in for a record saved before
   *  they existed. */
  type Editing = MailSorting & { followUp: FollowUpSettings }

  function editable(s: MailSorting): Editing {
    const copy = structuredClone(s)
    return { ...copy, followUp: { ...FOLLOW_UP_DEFAULTS, ...copy.followUp } }
  }

  let draft = $state<Editing | null>(null)
  let notice = $state<string | null>(null)
  let saving = $state(false)
  let olderDays = $state<number | null>(365)
  let olderAgain = $state(false)
  /** Categories and Follow-ups are the draft, saved by the one Save at the
   *  foot of either; Older mail is a button that does the work at once. */
  let sub = $state<'categories' | 'followups' | 'older'>('categories')
  let olderNotice = $state<string | null>(null)

  let noticeTimer: ReturnType<typeof setTimeout> | null = null
  function confirm(message: string) {
    notice = message
    if (noticeTimer) clearTimeout(noticeTimer)
    noticeTimer = setTimeout(() => {
      noticeTimer = null
      notice = null
    }, 4000)
  }

  // A running sort of older mail moves on its own, and so do the follow-up
  // counts as the background pass works; look again now and then while the
  // tab is open, so neither is a number from minutes ago -- the older sort
  // every 15 seconds, the follow-ups every other time. Only what `refresh`
  // puts in `sorting.view` changes: the draft below was copied from it once,
  // and is what the person is editing.
  let ticks = 0
  const poll = setInterval(() => {
    ticks += 1
    if (sorting.view?.older.length || ticks % 2 === 0) void sorting.refresh()
  }, 15_000)
  onDestroy(() => {
    clearInterval(poll)
    if (noticeTimer) clearTimeout(noticeTimer)
  })

  // Fresh every time the tab opens, as the models pane is: a draft seeded
  // from a cached copy would save over what another window changed.
  void sorting.refresh().then((ok) => {
    if (ok && sorting.sorting) draft = editable($state.snapshot(sorting.sorting))
  })
  void accounts.load()

  /** What is wrong with the follow-up numbers, said under each, and what
   *  holds Save back until it is put right. */
  const problems = $derived(
    draft ? followUpSettingsProblems(draft.followUp) : { windowDays: null, nudgeAfterDays: null },
  )
  const followUpsWrong = $derived(problems.windowDays !== null || problems.nudgeAfterDays !== null)

  /** How follow-ups are doing, as the backend last said. Whether they are on
   *  is the saved setting's, not the draft's: what is shown is what runs, and
   *  an unsaved switch changes nothing until it is saved. */
  const followUps = $derived(sorting.followUps)
  const followUpsOn = $derived(sorting.sorting?.followUp?.on !== false)
  /** Why no account is looked at at all, if that is so -- a model problem,
   *  mended under Models. */
  const followUpBlocked = $derived(followUpHint(followUps, followUpsOn))
  const modelLine = $derived(
    followUps ? followUpModelLine(followUps.model, sorting.view?.decisionModel ?? null) : null,
  )

  const WINDOWS: { days: number; label: string }[] = [
    { days: 1, label: 'The last day' },
    { days: 7, label: 'The last week' },
    { days: 30, label: 'The last 30 days' },
    { days: 90, label: 'The last 90 days' },
  ]

  const OLDER: { days: number | null; label: string }[] = [
    { days: 90, label: 'The last 90 days' },
    { days: 365, label: 'The last year' },
    { days: 365 * 3, label: 'The last three years' },
    { days: null, label: 'All of it' },
  ]

  function add() {
    if (!draft) return
    draft.categories.push({
      id: crypto.randomUUID(),
      name: '',
      description: '',
      treatAs: 'other',
    })
  }

  function remove(category: SortCategory) {
    if (!draft) return
    draft.categories = draft.categories.filter((c) => c.id !== category.id)
  }

  function move(index: number, by: -1 | 1) {
    if (!draft) return
    const to = index + by
    if (to < 0 || to >= draft.categories.length) return
    const list = draft.categories
    ;[list[index], list[to]] = [list[to]!, list[index]!]
  }

  async function save() {
    if (!draft || followUpsWrong) return
    notice = null
    saving = true
    try {
      await sorting.save($state.snapshot(draft))
      draft = editable($state.snapshot(sorting.sorting!))
      confirm('Saved.')
    } catch (e) {
      notice = e instanceof Error ? e.message : String(e)
    } finally {
      saving = false
    }
  }

  async function sortOlder() {
    olderNotice = null
    try {
      await sorting.sortOlder(olderDays, olderAgain)
    } catch (e) {
      olderNotice = e instanceof Error ? e.message : String(e)
    }
  }

  async function stopOlder(account: string) {
    olderNotice = null
    try {
      await sorting.stopOlder(account)
    } catch (e) {
      olderNotice = e instanceof Error ? e.message : String(e)
    }
  }

  function accountName(id: string): string {
    const a = accounts.list.find((x) => x.id === id)
    return a ? a.displayName || a.address : 'An account'
  }

  /** Why one account is held back, with the account named -- unless the
   *  sentence already says which, as it does when it names the address. */
  function heldBack(status: FollowUpAccountStatus): string {
    const why = status.blocked ?? ''
    const a = accounts.list.find((x) => x.id === status.accountId)
    const named = [a?.address, a?.displayName].some(
      (name) => !!name && why.toLowerCase().includes(name.toLowerCase()),
    )
    return named ? why : `${accountName(status.accountId)}: ${why}`
  }
</script>

{#if !draft && sorting.error}
  <p class="hint">Mail sorting could not be read: {sorting.error}</p>
{:else if !draft}
  <Loading />
{:else}
  <SubTabs
    tabs={[
      { id: 'categories', label: 'Categories' },
      { id: 'followups', label: 'Follow-ups' },
      { id: 'older', label: 'Older mail' },
    ]}
    bind:value={sub}
    label="Mail sorting"
  />

  <div class="panes">
    {#if sub === 'categories'}
      <p class="hint lead">
        Your own categories for mail. Each new message is read and filed under the category it fits,
        going by the descriptions you write here, and it lands in that category's inbox tab. You
        only see the category's name beside the message in the list.
      </p>
    {/if}

    {#if !sorting.view?.decisionModel}
      <div class="stored">
        <span>
          <Icon name="info" size={14} />
          No model is set up for sorting yet, so these categories are not used: mail goes into the five
          inbox tabs, sorted by your smaller model, which also decides follow-ups.
        </span>
        <button class="link" onclick={() => panels.openSettings('models')}>Set up a model</button>
      </div>
    {:else if sub !== 'followups'}
      <p class="hint">Sorting on {sorting.view.decisionModel}.</p>
    {/if}

    {#if sub === 'categories'}
      <section class="card" aria-labelledby="sorting-categories">
        <span class="eyebrow" id="sorting-categories">Categories</span>
        <p class="hint">
          Each message is filed by the names and descriptions here, so say what belongs and what
          does not. Keep a catch-all such as Other for mail that fits none of them. A message you
          file yourself always stays where you put it.
        </p>

        <ol class="categories">
          {#each draft.categories as category, i (category.id)}
            <li class="category">
              <div class="row">
                <label class="setting grow">
                  <span>Name</span>
                  <input bind:value={category.name} placeholder="Receipts" spellcheck="false" />
                </label>
                <label class="setting">
                  <span>Files under</span>
                  <select
                    value={category.treatAs}
                    onchange={(e) => (category.treatAs = e.currentTarget.value as MailCategory)}
                  >
                    {#each CATEGORY_TABS as tab (tab.key)}
                      <option value={tab.key}>{tab.label}</option>
                    {/each}
                  </select>
                </label>
                <div class="actions">
                  <button
                    class="icon-btn"
                    aria-label="Move {category.name || 'this category'} up"
                    disabled={i === 0}
                    onclick={() => move(i, -1)}
                  >
                    <Icon name="chevron" size={14} />
                  </button>
                  <button
                    class="icon-btn down"
                    aria-label="Move {category.name || 'this category'} down"
                    disabled={i === draft.categories.length - 1}
                    onclick={() => move(i, 1)}
                  >
                    <Icon name="chevron" size={14} />
                  </button>
                  <button
                    class="icon-btn"
                    aria-label="Remove {category.name || 'this category'}"
                    disabled={draft.categories.length === 1}
                    onclick={() => remove(category)}
                  >
                    <Icon name="trash" size={14} />
                  </button>
                </div>
              </div>
              <label class="setting">
                <span>What belongs here</span>
                <textarea
                  bind:value={category.description}
                  rows="2"
                  placeholder="Orders, receipts and invoices. Not: marketing from the same shops."
                ></textarea>
              </label>
              {#if category.treatAs === 'priority'}
                <p class="hint">Mail sorted here shows under Priority, ahead of everything else.</p>
              {/if}
            </li>
          {/each}
        </ol>
        <div class="row-end">
          <button class="btn" onclick={add}><Icon name="plus" size={14} /> Add a category</button>
        </div>
      </section>

      <section class="card" aria-labelledby="sorting-window">
        <span class="eyebrow" id="sorting-window">Which mail</span>
        <label class="setting">
          <span>Sort without asking</span>
          <select
            value={draft.windowDays}
            onchange={(e) => draft && (draft.windowDays = Number(e.currentTarget.value))}
          >
            {#each WINDOWS as w (w.days)}
              <option value={w.days}>{w.label}</option>
            {/each}
          </select>
        </label>
        <p class="hint">
          New mail is sorted as it arrives, and anything in your inbox from this far back that has
          not been sorted yet. Older mail is left alone unless you ask, under Older mail. Each
          account sorts only while sorting is switched on for it under
          <button class="link inline" onclick={() => panels.openSettings('ai')}>AI features</button
          >.
        </p>
      </section>
    {:else if sub === 'followups'}
      <section class="card" aria-labelledby="sorting-follow-ups">
        <span class="eyebrow" id="sorting-follow-ups">Follow-ups</span>
        <label class="check-row">
          <input type="checkbox" bind:checked={draft.followUp.on} />
          <span>
            <b>Show mail that needs your reply, and mail to nudge, at the top of the Inbox</b>
            <small>
              Somebody waiting on an answer from you, and mail you sent that nobody has answered.
            </small>
          </span>
        </label>
        <div class="row numbers">
          <label class="setting">
            <span>Look back</span>
            <span class="days">
              <input
                type="number"
                min={FOLLOW_UP_MIN_WINDOW_DAYS}
                max={FOLLOW_UP_MAX_WINDOW_DAYS}
                step="1"
                aria-invalid={problems.windowDays !== null}
                aria-describedby={problems.windowDays ? 'follow-up-window-problem' : undefined}
                bind:value={draft.followUp.windowDays}
              />
              days
            </span>
            {#if problems.windowDays}
              <span class="problem" id="follow-up-window-problem">{problems.windowDays}</span>
            {/if}
          </label>
          <label class="setting">
            <span>Offer a nudge after</span>
            <span class="days">
              <input
                type="number"
                min="1"
                max={Math.max(1, draft.followUp.windowDays - 1)}
                step="1"
                aria-invalid={problems.nudgeAfterDays !== null}
                aria-describedby={problems.nudgeAfterDays ? 'follow-up-nudge-problem' : undefined}
                bind:value={draft.followUp.nudgeAfterDays}
              />
              days
            </span>
            {#if problems.nudgeAfterDays}
              <span class="problem" id="follow-up-nudge-problem">{problems.nudgeAfterDays}</span>
            {/if}
          </label>
        </div>
        <p class="hint">
          It runs for each account whose sorting is switched on under
          <button class="link inline" onclick={() => panels.openSettings('ai')}>AI features</button
          >, using your sorting model, or your smaller model when none is set up. It reads the same
          parts of a message sorting does, and for a nudge your own last message as well. Cold
          outreach, newsletters and automated mail are left out: a stranger's pitch ends in a
          question too, but it is not waiting on you.
        </p>

        {#if followUps && followUpsOn}
          {#if followUpBlocked}
            <div class="stored">
              <span><Icon name="info" size={14} />{followUpBlocked}</span>
              <button class="link" onclick={() => panels.openSettings('models')}>Open Models</button
              >
            </div>
          {:else}
            <div class="follow-status">
              {#if modelLine}<p class="hint">{modelLine}</p>{/if}
              {#each followUps.accounts as account (account.accountId)}
                {#if account.blocked}
                  <div class="stored">
                    <span><Icon name="info" size={14} />{heldBack(account)}</span>
                    <button class="link" onclick={() => panels.openSettings('accounts')}>
                      Open Accounts
                    </button>
                  </div>
                {:else}
                  <p class="hint">
                    <span class="who">{accountName(account.accountId)}</span>:
                    {followUpAccountLine(account)}
                  </p>
                  {#if account.error}<p class="hint error">{account.error}</p>{/if}
                {/if}
              {/each}
            </div>
          {/if}
        {/if}
      </section>
    {:else}
      <section class="card" aria-labelledby="sorting-older">
        <span class="eyebrow" id="sorting-older">Older mail</span>
        <p class="hint">
          Sort inbox mail from further back, a few hundred messages a minute, in the background. It
          uses the saved categories, so save first.
        </p>
        <div class="row">
          <label class="setting grow">
            <span>How far back</span>
            <select
              value={olderDays == null ? '' : String(olderDays)}
              onchange={(e) =>
                (olderDays = e.currentTarget.value === '' ? null : Number(e.currentTarget.value))}
            >
              {#each OLDER as o (o.label)}
                <option value={o.days == null ? '' : String(o.days)}>{o.label}</option>
              {/each}
            </select>
          </label>
        </div>
        <label class="check-row">
          <input type="checkbox" bind:checked={olderAgain} />
          <span>
            <b>Sort again what was already sorted</b>
            <small
              >For after you change the categories. Mail you filed yourself is still left alone.</small
            >
          </span>
        </label>
        <div class="row-end">
          <button
            class="btn"
            onclick={() => void sortOlder()}
            disabled={!sorting.view?.decisionModel}
          >
            Sort older mail
          </button>
        </div>
        {#if olderNotice}<p class="hint error">{olderNotice}</p>{/if}
        {#each sorting.view?.older ?? [] as older (older.accountId)}
          <div class="stored">
            <span>
              <Icon name="refresh" size={14} />
              {accountName(older.accountId)}: {plural(older.asked, 'message')} sorted so far, back to
              {older.since ? longDate(older.since) : 'the beginning'}.
            </span>
            <button class="link" onclick={() => void stopOlder(older.accountId)}>Stop</button>
          </div>
        {/each}
      </section>
    {/if}
  </div>

  <!-- Under the two tabs that are the draft, and not under Older mail. The
       draft is this component's, so it is one Save for both: switching tabs
       keeps what was typed on the other. With the follow-up numbers wrong
       the button says where, since the reason may be on the other tab. -->
  {#if sub !== 'older'}
    <div class="save save-row">
      {#if notice}<span class="notice" role="status">{notice}</span>{/if}
      <button
        class="btn btn-primary"
        onclick={() => void save()}
        disabled={saving || followUpsWrong}
        title={followUpsWrong ? 'Put the numbers under Follow-ups right first' : undefined}
      >
        {saving ? 'Saving…' : 'Save'}
      </button>
    </div>
  {/if}
{/if}

<style>
  .panes {
    display: flex;
    flex-direction: column;
    gap: var(--sp-6);
    min-width: 0;
  }
  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }
  .card {
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
  }
  .hint {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
    overflow-wrap: anywhere;
  }
  .hint.lead {
    color: var(--fg-muted);
  }
  .hint.error {
    color: var(--danger, var(--fg));
  }

  /* Follow-ups' status under the card's own prose: what runs, then one line
     for each account that does, in the quiet hint voice -- the ones held back
     are the notice boxes the pane already has. */
  .follow-status {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    min-width: 0;
  }
  .who {
    font-weight: 550;
    color: var(--fg-muted);
  }

  .categories {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .category {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-sunken);
    min-width: 0;
  }
  .row {
    display: flex;
    align-items: flex-end;
    gap: var(--sp-3);
    min-width: 0;
  }
  .row-end {
    display: flex;
    justify-content: flex-end;
  }
  .grow {
    flex: 1;
  }
  .actions {
    display: flex;
    gap: var(--sp-1);
    padding-bottom: 4px;
  }
  .icon-btn {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: var(--radius);
    color: var(--fg-muted);
  }
  .icon-btn:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .icon-btn:disabled {
    opacity: 0.35;
  }
  /* The chevron points up; the second one is turned over. */
  .icon-btn :global(svg) {
    transform: rotate(-90deg);
  }
  .icon-btn.down :global(svg) {
    transform: rotate(90deg);
  }
  .actions .icon-btn:last-child :global(svg) {
    transform: none;
  }

  .setting {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    min-width: 0;
  }
  .setting > span {
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-muted);
  }
  input,
  select,
  textarea {
    width: 100%;
    min-width: 0;
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-base);
    user-select: text;
  }
  textarea {
    resize: vertical;
    line-height: var(--leading-normal);
  }
  input:focus,
  select:focus,
  textarea:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 16%, transparent);
  }
  input[type='checkbox'] {
    width: auto;
    padding: 0;
  }

  /* The follow-up numbers: two short fields side by side, each with its unit
     after it and what is wrong with it under it -- so the row lines up by its
     tops, not by whichever field has a line under it. */
  .row.numbers {
    align-items: flex-start;
    flex-wrap: wrap;
    gap: var(--sp-3) var(--sp-5);
  }
  .setting > .days {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-sm);
    font-weight: 400;
    color: var(--fg-muted);
  }
  .days input {
    width: 5.5em;
    font-variant-numeric: tabular-nums;
  }
  .days input[aria-invalid='true'] {
    border-color: var(--danger, var(--border));
  }
  .setting > .problem {
    font-size: var(--text-xs);
    font-weight: 400;
    color: var(--danger, var(--fg));
  }

  .stored {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-sunken);
    font-size: var(--text-sm);
    min-width: 0;
  }
  .stored span {
    display: flex;
    flex: 1;
    align-items: baseline;
    gap: var(--sp-2);
    min-width: 0;
    line-height: var(--leading-normal);
    color: var(--fg-muted);
  }
  .link {
    flex: none;
    color: var(--accent);
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .link:hover {
    text-decoration: underline;
  }

  /* A link in the middle of a sentence stays on its line. */
  .link.inline {
    font-size: inherit;
  }

  /* A phone: a category's name on a line of its own, then where it files and
     its buttons, each button a finger wide -- three fields in a row left the
     name eighteen pixels. */
  :global([data-phone]) .category > .row {
    flex-wrap: wrap;
  }
  :global([data-phone]) .category > .row > .grow {
    flex: 1 1 100%;
  }
  :global([data-phone]) .category > .row > .setting:not(.grow) {
    flex: 1 1 10rem;
  }
  :global([data-phone]) .actions {
    padding-bottom: 0;
  }
  :global([data-phone]) .icon-btn {
    width: 44px;
    height: 44px;
  }
</style>
