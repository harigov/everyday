<script lang="ts">
  // Finding something to follow: a directory of podcasts, and a search of
  // feeds, in one dialog.
  //
  //   Podcasts   searched by name, or -- with nothing typed -- the charts, by
  //              genre; each with its artwork, who makes it and how much of
  //              it there is
  //   Feeds      any RSS feed, searched by name, with who follows it
  //
  // Neither asks anything until somebody looks: the dialog opens on the
  // charts because that is what somebody who has not decided what to type
  // wants, and a feed search waits for words. Each search is run a moment
  // after typing stops, and at once on Enter.
  //
  // Every picture is an address on somebody else's server, and the webview
  // may not load one -- the content security policy allows `data:` and
  // `jatayu:` and nothing remote -- so each comes through
  // `api.imagePreview`, as the library's cover search does: asked for only
  // once its row is near the screen, and a few at a time, since fifty charts
  // are fifty requests to strangers' servers and most are never scrolled to.
  //
  // What is typed may be an address rather than a name, and then the usual
  // way to follow it is the add sheet's, which looks at the page and offers
  // what feeds it finds; so the directory says so and hands the address over,
  // beside the search it ran anyway.

  import { onMount } from 'svelte'
  import { api } from '../lib/api'
  import { feed } from '../lib/feed.svelte'
  import { hostOf, looksLikeAddress, sameFeedAddress } from '../lib/feed'
  import { closesOnBack } from '../lib/phone.svelte'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import { compactCount, plural } from '../lib/format'
  import { app, isLocked } from '../lib/state.svelte'
  import { debounce } from '../lib/store/debounce'
  import type { FeedHit, PodcastHit } from '../lib/types'
  import Icon from './Icon.svelte'
  import Loading from './Loading.svelte'

  // A phone's back button puts the sheet away rather than leaving the app.
  closesOnBack(
    () => true,
    () => feed.closeDirectory(),
  )

  type Tab = 'podcasts' | 'feeds'
  const TABS: { id: Tab; label: string }[] = [
    { id: 'podcasts', label: 'Podcasts' },
    { id: 'feeds', label: 'Feeds' },
  ]

  const tab = $derived<Tab>(feed.directoryTab === 'feeds' ? 'feeds' : 'podcasts')

  // What is typed in each, kept apart so a tab keeps its words when the other
  // is looked at.
  let podcastQuery = $state('')
  let feedQuery = $state('')
  /** The charts' genre: a `PodcastGenre.id`, or `null` for all of them. */
  let genre = $state<number | null>(null)

  const query = $derived(tab === 'podcasts' ? podcastQuery : feedQuery)
  const typed = $derived(query.trim())
  const isAddress = $derived(looksLikeAddress(typed))
  /** The host of what was typed, for the row that offers to look there. */
  const addressHost = $derived(hostOf(typed.includes('://') ? typed : `https://${typed}`) || typed)

  // ── Looking ───────────────────────────────────────────────────────────

  function lookPodcasts() {
    const words = podcastQuery.trim()
    if (words) void feed.searchPodcasts(words)
    else void feed.topPodcasts(genre)
  }

  function lookFeeds() {
    const words = feedQuery.trim()
    if (words) void feed.searchFeeds(words)
  }

  const waitPodcasts = debounce(lookPodcasts, 300)
  const waitFeeds = debounce(lookFeeds, 300)

  function typedIn() {
    if (tab === 'podcasts') waitPodcasts.call()
    else waitFeeds.call()
  }

  /** Enter: now, without waiting out the pause. */
  function lookNow() {
    if (tab === 'podcasts') {
      waitPodcasts.cancel()
      lookPodcasts()
    } else {
      waitFeeds.cancel()
      lookFeeds()
    }
  }

  function pickGenre(raw: string) {
    genre = raw === '' ? null : Number(raw)
    void feed.topPodcasts(genre)
  }

  function choose(next: Tab) {
    if (next === tab) return
    waitPodcasts.cancel()
    waitFeeds.cancel()
    feed.openDirectory(next)
    // The charts are what an empty Podcasts tab shows; fetched when it has none.
    if (next === 'podcasts' && !podcastQuery.trim() && feed.podcastHits.length === 0) {
      void feed.topPodcasts(genre)
    }
  }

  onMount(() => {
    void feed.loadGenres()
    if (tab === 'podcasts') void feed.topPodcasts(genre)
    return () => {
      waitPodcasts.cancel()
      waitFeeds.cancel()
    }
  })

  /** A list with each show once: the key a row is drawn by must not repeat. */
  function once<T>(hits: T[], key: (hit: T) => string): T[] {
    const seen = new Set<string>()
    return hits.filter((hit) => {
      const k = key(hit)
      if (seen.has(k)) return false
      seen.add(k)
      return true
    })
  }

  const podcastRows = $derived(once(feed.podcastHits, (h) => h.directoryId || h.feedUrl))
  // Not what a search for something else left behind.
  const feedRows = $derived(typed === '' ? [] : once(feed.feedHits, (h) => h.feedUrl))

  // ── Following ─────────────────────────────────────────────────────────

  /** Followed from here, which the hits from before have no way of knowing. */
  let followedNow = $state<Set<string>>(new Set())
  let following = $state<string | null>(null)

  function followed(url: string, was: boolean): boolean {
    if (was || followedNow.has(url)) return true
    return feed.sources.some((s) => {
      const kind = s.source.kind
      return kind.type === 'web' && sameFeedAddress(kind.url, url)
    })
  }

  /**
   * Follow, and say so only if something was followed. The store reports a
   * failure its own way, by a banner, so what is asked here is the sidebar:
   * a source that was not there is, and the button is a tick; one that was
   * not added leaves the button as it was.
   */
  async function follow(url: string, run: () => unknown) {
    if (following) return
    following = url
    const before = feed.sources.length
    try {
      await run()
    } finally {
      following = null
    }
    if (feed.sources.length > before) followedNow = new Set([...followedNow, url])
  }

  /** Hand an address to the add sheet, which finds what feeds are at it. */
  function lookAtAddress() {
    const address = typed
    feed.closeDirectory()
    feed.follow(address)
  }

  // ── Pictures ──────────────────────────────────────────────────────────
  //
  // A string is a picture to draw, `null` one that would not load, and no
  // entry one not asked for yet.

  const AT_ONCE = 4
  let previews = $state<Record<string, string | null>>({})
  let waiting: string[] = []
  let running = 0
  const asked = new Set<string>()

  /** Already something the webview may load. */
  const loadable = (url: string) => /^(data|blob|jatayu):/i.test(url)

  function want(url: string) {
    if (!url || asked.has(url) || loadable(url)) return
    asked.add(url)
    waiting.push(url)
    pump()
  }

  function pump() {
    while (running < AT_ONCE && waiting.length > 0) {
      const url = waiting.shift()!
      running++
      void api
        .imagePreview(url)
        .then((src) => {
          previews[url] = src
        })
        .catch((e) => {
          if (isLocked(e)) void app.lock()
          else previews[url] = null
        })
        .finally(() => {
          running--
          pump()
        })
    }
  }

  function pictureOf(url: string): string | null {
    if (!url) return null
    return loadable(url) ? url : (previews[url] ?? null)
  }

  /** Ask for a row's picture once it is close to being seen. */
  function nearScreen(node: HTMLElement, url: string) {
    if (!url || loadable(url)) return
    if (typeof IntersectionObserver === 'undefined') {
      want(url)
      return
    }
    const watcher = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          want(url)
          watcher.disconnect()
        }
      },
      { rootMargin: '200px' },
    )
    watcher.observe(node)
    return { destroy: () => watcher.disconnect() }
  }

  // ── Words ─────────────────────────────────────────────────────────────

  function podcastLine(hit: PodcastHit): string {
    const parts = [hit.genre]
    if (hit.episodes) parts.push(plural(hit.episodes, 'episode'))
    return parts.filter((p) => p.trim()).join(' · ')
  }

  function feedLine(hit: FeedHit): string {
    const parts = [hostOf(hit.siteUrl)]
    if (hit.subscribers) parts.push(`${compactCount(hit.subscribers)} readers`)
    return parts.filter(Boolean).join(' · ')
  }
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape') feed.closeDirectory()
  }}
/>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="scrim" onclick={() => feed.closeDirectory()}></div>
<div
  class="sheet wide"
  role="dialog"
  aria-modal="true"
  aria-label="Browse podcasts and feeds"
  use:trapFocus
>
  <div class="head">
    <h2>Browse</h2>
    <button
      class="shut"
      title="Close (Escape)"
      aria-label="Close"
      onclick={() => feed.closeDirectory()}
    >
      <Icon name="close" size={15} />
    </button>
  </div>

  <div class="tabs" role="tablist" aria-label="What to look for">
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

  <div class="search">
    {#if tab === 'podcasts'}
      <input
        class="field"
        type="search"
        placeholder="Search podcasts"
        aria-label="Search podcasts"
        spellcheck="false"
        autocomplete="off"
        bind:value={podcastQuery}
        use:focusOnMount
        oninput={typedIn}
        onkeydown={(e) => {
          if (e.key === 'Enter') lookNow()
        }}
      />
    {:else}
      <input
        class="field"
        type="search"
        placeholder="Search feeds, or paste an address"
        aria-label="Search feeds, or paste an address"
        spellcheck="false"
        autocomplete="off"
        bind:value={feedQuery}
        use:focusOnMount
        oninput={typedIn}
        onkeydown={(e) => {
          if (e.key === 'Enter') lookNow()
        }}
      />
    {/if}
  </div>

  <div class="results scroll" aria-busy={feed.directoryLoading}>
    {#if isAddress}
      <button class="address" onclick={lookAtAddress}>
        <span class="address-icon"><Icon name="globe" size={16} /></span>
        <span class="who">
          <b>Look for a feed at {addressHost}</b>
          <small>The page is read, and what feeds it has are offered to follow.</small>
        </span>
        <Icon name="chevron" size={14} />
      </button>
    {/if}

    {#if tab === 'podcasts'}
      {#if typed === ''}
        <div class="charts">
          <span class="eyebrow">Top podcasts</span>
          {#if feed.genres.length > 0}
            <select
              class="field genre"
              aria-label="Genre"
              value={genre === null ? '' : String(genre)}
              onchange={(e) => pickGenre(e.currentTarget.value)}
            >
              <option value="">All genres</option>
              {#each feed.genres as g (g.id)}
                <option value={String(g.id)}>{g.name}</option>
              {/each}
            </select>
          {/if}
        </div>
      {/if}

      {#if feed.directoryError}
        <p class="error">{feed.directoryError}</p>
      {:else if feed.directoryLoading && podcastRows.length === 0}
        <Loading label={typed === '' ? 'Loading the charts…' : 'Searching…'} />
      {:else if podcastRows.length === 0}
        <p class="hint">
          {typed === '' ? 'Nothing is on the charts right now.' : `No podcast matches “${typed}”.`}
        </p>
      {:else}
        <ul class="hits" aria-label={typed === '' ? 'Top podcasts' : 'Podcasts found'}>
          {#each podcastRows as hit (hit.directoryId || hit.feedUrl)}
            {@const picture = pictureOf(hit.artwork)}
            {@const done = followed(hit.feedUrl, hit.followed)}
            <li class="hit">
              <span class="art" use:nearScreen={hit.artwork}>
                {#if picture}
                  <img src={picture} alt="" loading="lazy" decoding="async" />
                {:else}
                  <Icon name="podcast" size={22} />
                {/if}
              </span>
              <span class="who">
                <b>
                  {hit.title}
                  {#if hit.explicit}<span class="explicit" title="Explicit">E</span>{/if}
                </b>
                {#if hit.author}<small>{hit.author}</small>{/if}
                <small class="faint">{podcastLine(hit)}</small>
              </span>
              {#if done}
                <span class="hint following"><Icon name="tick" size={14} /> Following</span>
              {:else}
                <button
                  class="btn btn-outline"
                  disabled={following !== null}
                  onclick={() => void follow(hit.feedUrl, () => feed.followPodcast(hit))}
                >
                  {following === hit.feedUrl ? 'Following…' : 'Follow'}
                </button>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
      <p class="hint source">Podcasts are from Apple's directory.</p>
    {:else}
      {#if typed === ''}
        <p class="hint">Search for a feed by name, or paste the address of a site or a feed.</p>
      {:else if feed.directoryError}
        <p class="error">{feed.directoryError}</p>
      {:else if feed.directoryLoading && feedRows.length === 0}
        <Loading label="Searching…" />
      {:else if feedRows.length === 0}
        <p class="hint">No feed matches “{typed}”.</p>
      {:else}
        <ul class="hits" aria-label="Feeds found">
          {#each feedRows as hit (hit.feedUrl)}
            {@const picture = pictureOf(hit.icon)}
            {@const done = followed(hit.feedUrl, hit.followed)}
            <li class="hit">
              <span class="art small" use:nearScreen={hit.icon}>
                {#if picture}
                  <img src={picture} alt="" loading="lazy" decoding="async" />
                {:else}
                  <Icon name="rss" size={18} />
                {/if}
              </span>
              <span class="who">
                <b>{hit.title || hostOf(hit.feedUrl) || hit.feedUrl}</b>
                {#if feedLine(hit)}<small>{feedLine(hit)}</small>{/if}
                {#if hit.description}<small class="about">{hit.description}</small>{/if}
              </span>
              {#if done}
                <span class="hint following"><Icon name="tick" size={14} /> Following</span>
              {:else}
                <button
                  class="btn btn-outline"
                  disabled={following !== null}
                  onclick={() => void follow(hit.feedUrl, () => feed.followFeed(hit))}
                >
                  {following === hit.feedUrl ? 'Following…' : 'Follow'}
                </button>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
      <p class="hint source">Feeds are found with Feedly's search.</p>
    {/if}
  </div>
</div>

<style>
  /* Centred rather than hung from the shared sheet's 22%, and as tall as it can
     be: the results scroll inside it, so the search and the tabs stay put. */
  .wide {
    top: 50%;
    translate: -50% -50%;
    width: min(640px, calc(100vw - var(--sp-8)));
    height: min(700px, calc(100vh - var(--sp-8)));
    display: flex;
    flex-direction: column;
  }

  .head {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin-bottom: var(--sp-3);
  }
  h2 {
    flex: 1;
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
  }
  .shut {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .shut:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .tabs {
    flex: none;
    display: flex;
    gap: 2px;
    padding: 2px;
    margin-bottom: var(--sp-3);
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

  .search {
    flex: none;
    margin-bottom: var(--sp-3);
  }

  .results {
    flex: 1;
    min-height: 0;
    margin: 0 calc(var(--sp-2) * -1);
    padding: 0 var(--sp-2);
  }
  .results > .hint,
  .results > .error {
    margin: var(--sp-3) 0;
  }
  .source {
    margin: var(--sp-3) 0 var(--sp-1);
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }

  .charts {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    margin: var(--sp-1) 0 var(--sp-2);
  }
  .charts .eyebrow {
    flex: 1;
  }
  .genre {
    width: auto;
    min-width: 160px;
    height: 30px;
    font-size: var(--text-sm);
  }

  /* The address row: the way out to the add sheet, above whatever the search
     found. */
  .address {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    width: 100%;
    margin-bottom: var(--sp-2);
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid color-mix(in oklab, var(--accent) 35%, transparent);
    border-radius: var(--radius);
    background: color-mix(in oklab, var(--accent) 7%, transparent);
    color: var(--accent);
    text-align: left;
  }
  .address:hover {
    background: color-mix(in oklab, var(--accent) 12%, transparent);
  }
  .address-icon {
    flex: none;
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
  }

  .hits {
    display: flex;
    flex-direction: column;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  /* Looking again over what is there: the old rows stay, and dim. */
  .results[aria-busy='true'] .hits {
    opacity: 0.6;
  }
  .hit {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-2);
    border-radius: var(--radius);
  }
  .hit:hover {
    background: var(--bg-hover);
  }

  /* The artwork's square; the picture is only drawn once it has come, and the
     mark stands in until then and when it never does. */
  .art {
    flex: none;
    display: grid;
    place-items: center;
    width: 56px;
    height: 56px;
    overflow: hidden;
    border-radius: var(--radius);
    background: var(--bg-sunken);
    box-shadow: inset 0 0 0 1px var(--border);
    color: var(--fg-faint);
  }
  .art.small {
    width: 36px;
    height: 36px;
    border-radius: 50%;
  }
  .art img {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }

  .who {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
    color: inherit;
  }
  .who b {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-base);
    font-weight: 600;
  }
  .who small {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .who small.faint {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  /* A feed's own description: two lines, then gone. */
  .who small.about {
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    white-space: normal;
    font-size: var(--text-xs);
  }
  .address .who small {
    color: inherit;
    opacity: 0.75;
  }
  .explicit {
    display: inline-grid;
    place-items: center;
    width: 14px;
    height: 14px;
    margin-left: 4px;
    border-radius: 3px;
    background: var(--bg-active);
    font-size: 9px;
    font-weight: 700;
    color: var(--fg-subtle);
    vertical-align: 1px;
  }
  .following {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex: none;
  }

  /* ── On a phone ──────────────────────────────────────────────────────── */

  /* As tall as the screen allows between the status bar and the bar of apps,
     hung from the top like every sheet on a phone: the results scroll inside
     it, so the search and the tabs stay put. */
  :global([data-phone]) .sheet.wide {
    top: calc(var(--safe-top) + var(--sp-3));
    translate: -50% 0;
    width: calc(100vw - var(--sp-6) - var(--safe-left) - var(--safe-right));
    height: calc(
      100dvh - var(--safe-top) - var(--sp-3) - var(--bottombar-h) - var(--safe-bottom) - var(--sp-3)
    );
    max-height: none;
  }
  :global([data-phone]) .shut {
    width: 44px;
    height: 44px;
    margin: -8px -8px -8px 0;
  }
  :global([data-phone]) .tab {
    height: 40px;
  }
  :global([data-phone]) .genre {
    height: 40px;
  }
  :global([data-phone]) .hit {
    min-height: 64px;
  }
  :global([data-phone]) .address {
    min-height: 56px;
  }
</style>
