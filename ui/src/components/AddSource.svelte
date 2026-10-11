<script lang="ts">
  // Following something: four ways in, one sheet.
  //
  //   Address       paste any address -- a feed, or a page that has one -- and
  //                 pick from what is found there
  //   Hacker News   one of its lists, optionally above a score
  //   From your mail  a sender whose newsletters already arrive in Mail
  //   Import OPML   what another reader exported, and the way back out
  //
  // Nothing here is fetched until somebody asks: the address is looked at when
  // "Find feeds" is pressed, and the senders when their tab is opened. The
  // OPML file is read here, in the webview, and handed over as text, so no
  // path crosses the bridge and "import" cannot be talked into reading a file
  // nobody picked -- the calendar's `.ics` import does the same.

  import { onMount } from 'svelte'
  import { feed } from '../lib/feed.svelte'
  import { HN_LIST_LABELS, HN_LIST_NOTES, clampPoints, folderNames, hostOf } from '../lib/feed'
  import { plural } from '../lib/format'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import {
    HN_LISTS,
    type FoundFeed,
    type HnList,
    type OpmlImportReport,
    type NewsletterSender,
  } from '../lib/types'
  import Icon from './Icon.svelte'

  type Tab = 'address' | 'hackerNews' | 'mail' | 'opml'

  const TABS = $derived<{ id: Tab; label: string }[]>([
    { id: 'address', label: 'Address' },
    { id: 'hackerNews', label: 'Hacker News' },
    ...(feed.mailAvailable ? [{ id: 'mail' as const, label: 'From your mail' }] : []),
    { id: 'opml', label: 'Import OPML' },
  ])

  let tab = $state<Tab>('address')

  /** The folder the next thing followed goes in. Shared: it is the same decision on every tab. */
  let folder = $state('')
  const folders = $derived(folderNames(feed.sources))

  let busy = $state(false)
  let problem = $state<string | null>(null)

  // ── Address ──────────────────────────────────────────────────────────

  // Starts with what was typed in the bar before "Follow" was chosen there.
  let address = $state(feed.addPrefill)
  /** What was found at the address, or `null` before anybody has looked. */
  let found = $state<FoundFeed[] | null>(null)
  let finding = $state(false)
  let chosen = $state('')

  async function find() {
    const url = address.trim()
    if (!url || finding) return
    finding = true
    problem = null
    found = null
    const feeds = await feed.discover(url)
    finding = false
    found = feeds
    chosen = feeds[0]?.url ?? ''
  }

  async function followFound() {
    const pick = found?.find((f) => f.url === chosen)
    if (!pick || busy) return
    busy = true
    problem = null
    const added = await feed.addSource({
      type: 'web',
      url: pick.url,
      title: pick.title || undefined,
      folder: folder.trim() || undefined,
    })
    busy = false
    if (added) feed.closeAdd()
    else problem = 'That could not be followed.'
  }

  // ── Hacker News ──────────────────────────────────────────────────────

  let list = $state<HnList>('front')
  let minPoints = $state<number | null>(0)

  /** The one list that is nothing but a score has to have one. */
  const needsPoints = $derived(list === 'top' && clampPoints(minPoints) === 0)

  function pickList(next: HnList) {
    list = next
    if (next === 'top' && clampPoints(minPoints) === 0) minPoints = 100
  }

  async function followHackerNews() {
    if (needsPoints || busy) return
    busy = true
    problem = null
    const added = await feed.addSource({
      type: 'hackerNews',
      list,
      minPoints: clampPoints(minPoints),
      folder: folder.trim() || undefined,
    })
    busy = false
    if (added) feed.closeAdd()
    else problem = 'That could not be followed.'
  }

  // ── From your mail ───────────────────────────────────────────────────

  /** Who sends newsletters here; `null` until the tab has been opened. */
  let senders = $state<NewsletterSender[] | null>(null)
  let loadingSenders = $state(false)
  let filter = $state('')
  /** Followed just now, which the list from before has no way of knowing. */
  let followedNow = $state<Set<string>>(new Set())
  let following = $state<string | null>(null)

  async function loadSenders() {
    loadingSenders = true
    senders = await feed.newsletterSenders()
    loadingSenders = false
  }

  const shownSenders = $derived.by(() => {
    const word = filter.trim().toLowerCase()
    return (senders ?? []).filter(
      (s) => !word || s.name.toLowerCase().includes(word) || s.email.toLowerCase().includes(word),
    )
  })

  async function followSender(sender: NewsletterSender) {
    if (following) return
    following = sender.email
    problem = null
    const added = await feed.addSource({
      type: 'mail',
      sender: sender.email,
      title: sender.name || undefined,
      folder: folder.trim() || undefined,
    })
    following = null
    if (added) followedNow = new Set([...followedNow, sender.email])
    else problem = 'That could not be followed.'
  }

  function choose(next: Tab) {
    tab = next
    problem = null
    if (next === 'mail' && senders === null) void loadSenders()
  }

  // ── OPML ─────────────────────────────────────────────────────────────

  let file = $state<HTMLInputElement | null>(null)
  let report = $state<OpmlImportReport | null>(null)
  /** The exported text, kept so it can be copied if the download did not land. */
  let exported = $state<string | null>(null)
  let copied = $state(false)

  async function pickFile(e: Event) {
    const input = e.currentTarget as HTMLInputElement
    const chosenFile = input.files?.[0]
    if (!chosenFile) return
    busy = true
    problem = null
    report = null
    const text = await chosenFile.text()
    report = await feed.importOpml(text)
    busy = false
    input.value = ''
    if (!report) problem = 'That file could not be read as OPML.'
  }

  /** Hand a file to the browser, the way the data export does outside the desktop shell. */
  function download(name: string, text: string) {
    const url = URL.createObjectURL(new Blob([text], { type: 'text/x-opml' }))
    const link = document.createElement('a')
    link.href = url
    link.download = name
    link.click()
    // Freed on the next turn rather than at once: revoking in the same tick
    // can cancel the download that was just started.
    setTimeout(() => URL.revokeObjectURL(url), 60_000)
  }

  async function exportFeeds() {
    if (busy) return
    busy = true
    problem = null
    copied = false
    const xml = await feed.exportOpml()
    busy = false
    if (xml === null) {
      problem = 'Nothing could be exported.'
      return
    }
    exported = xml
    download('subscriptions.opml', xml)
  }

  async function copyExport() {
    if (exported === null) return
    try {
      await navigator.clipboard.writeText(exported)
      copied = true
    } catch {
      problem = 'The text could not be copied.'
    }
  }

  /** "Followed 12 new feeds." and what was left out, in one line each. */
  function reportLines(r: OpmlImportReport): string[] {
    const lines = [
      r.added === 0 ? 'Nothing new to follow.' : `Followed ${plural(r.added, 'new feed')}.`,
    ]
    if (r.skipped > 0) {
      lines.push(`${plural(r.skipped, 'entry', 'entries')} already followed, or not a feed.`)
    }
    return lines
  }

  // Choosing "Follow" on an address in the bar is the same as pressing "Find
  // feeds" on it, so the sheet does not wait to be asked.
  onMount(() => {
    if (address.trim()) void find()
  })
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape') feed.closeAdd()
  }}
/>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="scrim" onclick={() => feed.closeAdd()}></div>
<div class="sheet wide" role="dialog" aria-modal="true" aria-label="Follow something" use:trapFocus>
  <h2>Follow something</h2>

  <div class="tabs" role="tablist" aria-label="How to follow it">
    {#each TABS as t (t.id)}
      <button
        class="tab"
        class:on={tab === t.id}
        role="tab"
        aria-selected={tab === t.id}
        onclick={() => choose(t.id)}
      >
        {t.label}
      </button>
    {/each}
  </div>

  {#snippet folderField()}
    <label class="field-label spaced" for="follow-folder">
      Folder <span class="opt">optional</span>
    </label>
    <input
      id="follow-folder"
      class="field"
      list="follow-folders"
      placeholder="None"
      autocomplete="off"
      bind:value={folder}
    />
    <datalist id="follow-folders">
      {#each folders as name (name)}
        <option value={name}></option>
      {/each}
    </datalist>
  {/snippet}

  {#if tab === 'address'}
    <label class="field-label" for="follow-address">Address</label>
    <div class="find">
      <input
        id="follow-address"
        class="field"
        placeholder="A feed, or any page that has one"
        spellcheck="false"
        autocomplete="off"
        bind:value={address}
        use:focusOnMount
        oninput={() => {
          found = null
          problem = null
        }}
        onkeydown={(e) => {
          if (e.key === 'Enter') void find()
        }}
      />
      <button
        class="btn btn-outline"
        disabled={!address.trim() || finding}
        onclick={() => void find()}
      >
        {finding ? 'Looking…' : 'Find feeds'}
      </button>
    </div>
    <p class="hint under">
      A site's address is enough: its feed is found from the page. Substack, YouTube, Mastodon,
      GitHub and most blogs and news sites have one.
    </p>

    {#if found !== null}
      {#if found.length === 0}
        <p class="hint none">
          No feed was found there. Try the address of the page itself, or a link ending in
          <code>.xml</code>, <code>.rss</code> or <code>.atom</code>.
        </p>
      {:else}
        <ul class="choices" role="radiogroup" aria-label="Feeds found">
          {#each found as f (f.url)}
            <li>
              <label class="choice" class:on={chosen === f.url}>
                <input type="radio" name="found" value={f.url} bind:group={chosen} />
                <span class="text">
                  <b>{f.title || hostOf(f.url) || f.url}</b>
                  <small>{f.url}</small>
                </span>
              </label>
            </li>
          {/each}
        </ul>
        {@render folderField()}
      {/if}
    {/if}

    {#if problem}<p class="error spaced">{problem}</p>{/if}

    <div class="sheet-row">
      <span class="spacer"></span>
      <button class="btn" onclick={() => feed.closeAdd()}>Cancel</button>
      <button
        class="btn btn-primary"
        disabled={!found?.some((f) => f.url === chosen) || busy}
        onclick={() => void followFound()}
      >
        {busy ? 'Following…' : 'Follow'}
      </button>
    </div>
  {:else if tab === 'hackerNews'}
    <ul class="choices" role="radiogroup" aria-label="Which list">
      {#each HN_LISTS as l (l)}
        <li>
          <label class="choice" class:on={list === l}>
            <input
              type="radio"
              name="hn-list"
              value={l}
              checked={list === l}
              onchange={() => pickList(l)}
            />
            <span class="text">
              <b>{HN_LIST_LABELS[l]}</b>
              <small>{HN_LIST_NOTES[l]}</small>
            </span>
          </label>
        </li>
      {/each}
    </ul>

    <label class="field-label spaced" for="follow-points">
      Only stories above <span class="opt">{list === 'top' ? 'points' : 'points, optional'}</span>
    </label>
    <input
      id="follow-points"
      class="field points"
      type="number"
      min="0"
      step="10"
      bind:value={minPoints}
    />
    {#if needsPoints}
      <p class="hint under">This list is every story above a score, so it needs one.</p>
    {/if}

    {@render folderField()}

    {#if problem}<p class="error spaced">{problem}</p>{/if}

    <div class="sheet-row">
      <span class="spacer"></span>
      <button class="btn" onclick={() => feed.closeAdd()}>Cancel</button>
      <button
        class="btn btn-primary"
        disabled={needsPoints || busy}
        onclick={() => void followHackerNews()}
      >
        {busy ? 'Following…' : 'Follow'}
      </button>
    </div>
  {:else if tab === 'mail'}
    <p class="hint lead">
      Newsletters that already arrive in your mail. The messages stay in Mail and the Feed only
      points at them, so reading one here marks it read there. A source can be set to keep its
      issues out of the inbox in its settings.
    </p>

    {#if loadingSenders && senders === null}
      <p class="hint none">Looking through your mail…</p>
    {:else if senders !== null && senders.length === 0}
      <p class="hint none">No newsletters have arrived in the last 90 days.</p>
    {:else if senders !== null}
      {#if senders.length > 8}
        <input
          class="field find-sender"
          type="search"
          placeholder="Search senders"
          aria-label="Search senders"
          bind:value={filter}
        />
      {/if}
      <ul class="senders">
        {#each shownSenders as s (s.email)}
          {@const done = s.followed || followedNow.has(s.email)}
          <li class="sender">
            <span class="who">
              <b>{s.name || s.email}</b>
              <small>{s.name ? `${s.email} · ` : ''}{plural(s.messages, 'message')}</small>
            </span>
            {#if done}
              <span class="hint following">
                <Icon name="tick" size={14} /> Following
              </span>
            {:else}
              <button
                class="btn btn-outline"
                disabled={following !== null}
                onclick={() => void followSender(s)}
              >
                {following === s.email ? 'Following…' : 'Follow'}
              </button>
            {/if}
          </li>
        {:else}
          <li class="hint none">Nobody matches “{filter.trim()}”.</li>
        {/each}
      </ul>
      {@render folderField()}
    {/if}

    {#if problem}<p class="error spaced">{problem}</p>{/if}

    <div class="sheet-row">
      <span class="spacer"></span>
      <button class="btn btn-primary" onclick={() => feed.closeAdd()}>Done</button>
    </div>
  {:else}
    <p class="hint lead">
      Every feed reader can export what it follows as an OPML file. Choose one here to follow the
      same feeds.
    </p>

    <div class="sheet-row top">
      <button class="btn btn-outline" disabled={busy} onclick={() => file?.click()}>
        <Icon name="upload" size={14} />
        {busy ? 'Reading…' : 'Choose an OPML file'}
      </button>
      <input
        class="vh"
        type="file"
        accept=".opml,.xml,text/xml,application/xml,text/x-opml"
        bind:this={file}
        onchange={(e) => void pickFile(e)}
      />
    </div>

    {#if report}
      <div class="report" role="status">
        {#each reportLines(report) as line (line)}
          <p>{line}</p>
        {/each}
      </div>
    {/if}

    <hr class="sep spaced" />

    <p class="hint lead">
      Or take yours with you: everything you follow, as a file any other reader will open.
    </p>
    <div class="sheet-row top">
      <button
        class="btn btn-outline"
        disabled={busy || feed.sources.length === 0}
        onclick={() => void exportFeeds()}
      >
        Export OPML
      </button>
      {#if exported !== null}
        <span class="hint">Saved as subscriptions.opml.</span>
        <button class="link" onclick={() => void copyExport()}>
          {copied ? 'Copied' : 'Nothing saved? Copy it instead'}
        </button>
      {/if}
    </div>

    {#if problem}<p class="error spaced">{problem}</p>{/if}

    <div class="sheet-row">
      <span class="spacer"></span>
      <button class="btn btn-primary" onclick={() => feed.closeAdd()}>Done</button>
    </div>
  {/if}
</div>

<style>
  /* Centred rather than hung from the shared sheet's 22%: the longest tab is
     a list of senders, and it would otherwise run off the bottom of the
     window before the max-height made it scroll. */
  .wide {
    top: 50%;
    translate: -50% -50%;
    width: min(540px, calc(100vw - var(--sp-8)));
    max-height: calc(100vh - var(--sp-8));
    overflow-y: auto;
  }

  h2 {
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
    margin-bottom: var(--sp-3);
  }

  .tabs {
    display: flex;
    gap: 2px;
    padding: 2px;
    margin-bottom: var(--sp-4);
    border-radius: var(--radius-sm);
    background: var(--bg-active);
  }
  .tab {
    flex: 1;
    height: 28px;
    padding: 0 var(--sp-2);
    border-radius: 4px;
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-subtle);
    white-space: nowrap;
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .tab:hover {
    color: var(--fg);
  }
  .tab.on {
    background: var(--bg-raised);
    color: var(--fg);
    box-shadow: var(--shadow-sm);
  }

  .spaced {
    margin-top: var(--sp-4);
  }
  .opt {
    font-weight: 400;
    color: var(--fg-faint);
    text-transform: none;
    letter-spacing: 0;
  }
  .under {
    margin-top: var(--sp-2);
    font-size: var(--text-xs);
  }
  .lead {
    margin-bottom: var(--sp-3);
  }
  .none {
    margin-top: var(--sp-3);
    padding: var(--sp-3);
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
  }
  code {
    font-size: 0.92em;
  }

  .find {
    display: flex;
    gap: var(--sp-2);
  }
  .points {
    width: 120px;
  }
  .find-sender {
    margin-bottom: var(--sp-2);
  }

  /* One choice to make from several: a radio and what it means, as a row. */
  .choices {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: var(--sp-3) 0 0;
    padding: 0;
    list-style: none;
  }
  .choice {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid transparent;
    border-radius: var(--radius);
    cursor: pointer;
  }
  .choice:hover {
    background: var(--bg-hover);
  }
  .choice.on {
    border-color: color-mix(in oklab, var(--accent) 45%, transparent);
    background: color-mix(in oklab, var(--accent) 8%, transparent);
  }
  .choice input {
    flex: none;
    margin-top: 3px;
    accent-color: var(--accent);
  }
  .text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .text b {
    font-size: var(--text-base);
    font-weight: 600;
  }
  .text small {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    user-select: text;
  }

  .senders {
    display: flex;
    flex-direction: column;
    max-height: 300px;
    margin: 0;
    padding: 0;
    overflow-y: auto;
    list-style: none;
  }
  .sender {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-2);
    border-radius: var(--radius-sm);
  }
  .sender:hover {
    background: var(--bg-hover);
  }
  .who {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .who b,
  .who small {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .who b {
    font-size: var(--text-base);
    font-weight: 600;
  }
  .who small {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .following {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex: none;
  }

  .sheet-row.top {
    margin-top: 0;
    flex-wrap: wrap;
  }
  .report {
    margin-top: var(--sp-3);
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .link {
    color: var(--accent);
    font-size: var(--text-sm);
    text-decoration: underline;
  }

  /* Visually hidden, still reachable: the real file input behind the button. */
  .vh {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }
</style>
