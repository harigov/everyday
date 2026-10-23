<script lang="ts">
  // Settings → Mail sorting: the categories a decision model sorts mail into.
  //
  // Each category is a name, what belongs in it in the person's own words,
  // and the inbox tab it files under. The name and the description are what
  // the model is shown -- they are the options it picks between -- so the
  // description is where somebody says what should and should not count.
  // The tab is what the rest of the app understands: Priority, Important,
  // Other, Newsletters, Notifications. See `everyday_core::mail::sorting`.
  //
  // Recent mail is sorted unasked; older mail only from the button at the
  // bottom, because sorting somebody's whole archive on a settings change is
  // a bill and a reshuffle nobody agreed to.

  import { onDestroy } from 'svelte'
  import { accounts } from '../lib/accounts.svelte'
  import { longDate, plural } from '../lib/format'
  import { CATEGORY_TABS } from '../lib/mail'
  import { panels } from '../lib/panels.svelte'
  import { sorting } from '../lib/sorting.svelte'
  import type { MailCategory, MailSorting, SortCategory } from '../lib/types'
  import Icon from './Icon.svelte'

  let draft = $state<MailSorting | null>(null)
  let notice = $state<string | null>(null)
  let saving = $state(false)
  let olderDays = $state<number | null>(365)
  let olderAgain = $state(false)
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

  // A running sort of older mail moves on its own; look again now and then
  // while the tab is open, so its count is not a number from minutes ago.
  const poll = setInterval(() => {
    if (sorting.view?.older.length) void sorting.refresh()
  }, 15_000)
  onDestroy(() => {
    clearInterval(poll)
    if (noticeTimer) clearTimeout(noticeTimer)
  })

  // Fresh every time the tab opens, as the models pane is: a draft seeded
  // from a cached copy would save over what another window changed.
  void sorting.refresh().then((ok) => {
    if (ok && sorting.sorting) draft = structuredClone($state.snapshot(sorting.sorting))
  })
  void accounts.load()

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
    if (!draft) return
    notice = null
    saving = true
    try {
      await sorting.save($state.snapshot(draft))
      draft = structuredClone($state.snapshot(sorting.sorting!))
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
</script>

{#if !draft && sorting.error}
  <p class="hint">Mail sorting could not be read: {sorting.error}</p>
{:else if !draft}
  <p class="hint">Loading…</p>
{:else}
  <div class="panes">
    <p class="hint lead">
      Your own categories for mail. A decision model reads each new message and picks the one it
      fits, going by the descriptions you write here, and the message is filed under that category's
      inbox tab. You only see the category's name beside the message in the list.
    </p>

    {#if !sorting.view?.decisionModel}
      <div class="stored">
        <span>
          <Icon name="info" size={14} />
          No decision model is switched on, so these categories are not used yet: mail is sorted on the
          Small model into the five inbox tabs.
        </span>
        <button class="link" onclick={() => panels.openSettings('models')}>Set one up</button>
      </div>
    {:else}
      <p class="hint">Sorting on {sorting.view.decisionModel}.</p>
    {/if}

    <section class="card" aria-labelledby="sorting-categories">
      <span class="eyebrow" id="sorting-categories">Categories</span>
      <p class="hint">
        The model sees each name and description and picks one, so say what belongs and what does
        not. Keep a catch-all such as Other for mail that fits none of them. A message you file
        yourself always stays where you put it.
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
        New mail is sorted as it arrives, and anything in your inbox from this far back that has not
        been sorted yet. Older mail is left alone unless you ask, below. Each account sorts only if
        you switched on sorting under Settings → Accounts.
      </p>
    </section>

    <div class="save">
      {#if notice}<span class="notice">{notice}</span>{/if}
      <button class="btn btn-primary" onclick={() => void save()} disabled={saving}>
        {saving ? 'Saving…' : 'Save'}
      </button>
    </div>

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
      <label class="switch">
        <input type="checkbox" bind:checked={olderAgain} />
        <span class="switch-text">
          <span class="switch-label">Sort again what was already sorted</span>
          <span class="switch-blurb"
            >For after you change the categories. Mail you filed yourself is still left alone.</span
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
  </div>
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

  .switch {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    cursor: pointer;
  }
  .switch input {
    margin-top: 3px;
    accent-color: var(--accent);
  }
  .switch-text {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .switch-label {
    font-size: var(--text-sm);
  }
  .switch-blurb {
    color: var(--fg-subtle);
    font-size: var(--text-xs);
  }

  .save {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--sp-3);
  }
  .notice {
    flex: 1;
    min-width: 0;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
</style>
