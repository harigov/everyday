<script lang="ts">
  // The Feed's reading pane: one story, its toolbar, and its body.
  //
  // The body is drawn the way `MailThread.svelte` draws a message, and for the
  // same reason: it is markup somebody else wrote, and the only safe place
  // for that is a frame that can run nothing and read nothing back. The
  // sandbox is exactly `allow-popups allow-popups-to-escape-sandbox`, nowhere
  // else in this component or its caller, and it stays that way:
  // `allow-scripts` would let a story run code in this window's process, and
  // `allow-same-origin` would let this window be read from inside it. A link
  // in a story still has to open somewhere, which is what the popups
  // permission is for. The document itself arrives already sanitised, once,
  // when the story was fetched, and already drawn for this window's light or
  // dark theme -- see `feed.loadBody`, which is why nothing here themes it a
  // second time: a page that has been themed once reads as having colours of
  // its own, and a second pass would paint it white.

  import { untrack } from 'svelte'
  import { feed } from '../lib/feed.svelte'
  import {
    alsoOn,
    distinctAuthor,
    estimateArticleHeight,
    hostOf,
    looksCutOff,
    sourceLabel,
  } from '../lib/feed'
  import { dateFormat } from '../lib/format'
  import { applyPlatformOrigin } from '../lib/mailview'
  import { openExternal } from '../lib/open-external'
  import type { Story } from '../lib/types'
  import Icon from './Icon.svelte'
  import SourceIcon from './SourceIcon.svelte'
  import SourceSettings from './SourceSettings.svelte'

  let { story }: { story: Story } = $props()

  const source = $derived(feed.sourceOf(story))
  const sourceName = $derived(source ? sourceLabel(source.source) : '')
  const author = $derived(distinctAuthor(story, sourceName))
  const also = $derived(alsoOn(story, feed.sources))
  /** Where "Open original" leads: worth saying for a story whose source is a list of links. */
  const host = $derived(hostOf(story.url))
  const published = $derived.by(() => {
    const at = new Date(story.publishedAt)
    if (Number.isNaN(at.getTime())) return ''
    return dateFormat({
      weekday: 'short',
      day: 'numeric',
      month: 'short',
      year: 'numeric',
      hour: 'numeric',
      minute: '2-digit',
    }).format(at)
  })

  // ── The body ──────────────────────────────────────────────────────────

  let body = $state<{ html: string; imagesHidden: boolean } | null>(null)
  let failed = $state(false)
  /** Which story `body` belongs to, so a new one never shows under the old one's. */
  let shownFor = ''
  /** Counts loads, so one that was overtaken by a newer story is ignored when it lands. */
  let loads = 0

  // Reload when the story changes -- or gains a body, which is what fetching
  // the full article does -- and once more when an extraction ends, since a
  // feed's excerpt that has just been replaced by the article does not change
  // `hasBody`: it was already true.
  //
  // Keyed on these values and not on `story` itself: the store replaces
  // its story objects whenever the list reloads, and a refresh that changed
  // nothing about this one is no reason to fetch its body again.
  const storyId = $derived(story.id)
  const hasBody = $derived(story.hasBody)
  const expired = $derived(story.expired)

  $effect(() => {
    const id = storyId
    void hasBody
    if (feed.extracting) return
    const target = untrack(() => story)
    const mine = ++loads
    if (shownFor !== id) {
      body = null
      failed = false
    }
    if (expired) return
    // Untracked: whatever the store reads on the way to its first `await` is
    // not a reason to load this story again.
    untrack(() => feed.loadBody(target))
      .then((loaded) => {
        if (mine !== loads) return
        shownFor = id
        failed = loaded === null
        body = loaded
      })
      .catch(() => {
        if (mine !== loads) return
        shownFor = id
        failed = true
        body = null
      })
  })

  // ── Drawing it ────────────────────────────────────────────────────────

  /**
   * The document for the frame. A story that is a mail message carries Mail's
   * own picture addresses, which the store does not know about and which need
   * the origin this platform resolves; for any other story this changes
   * nothing.
   */
  const doc = $derived(body ? applyPlatformOrigin(body.html) : null)

  /** Sizes the one iframe inside `node` from the body string, never from
   *  what the iframe renders -- see `estimateArticleHeight`'s own doc. */
  function autoSize(node: HTMLElement, bodyHtml: string) {
    const iframe = node.querySelector('iframe')
    function apply() {
      if (!iframe) return
      iframe.style.height = `${estimateArticleHeight(bodyHtml, node.clientWidth)}px`
    }
    apply()
    const ro = new ResizeObserver(apply)
    ro.observe(node)
    return { destroy: () => ro.disconnect() }
  }

  // ── What can be done with it ──────────────────────────────────────────

  /** Only a web story has an article to fetch, and only when what is here is short of it. */
  const canFetch = $derived(
    story.url !== '' &&
      !story.mailMessage &&
      !story.expired &&
      (!story.hasBody || (body !== null && looksCutOff(body.html))),
  )

  /** Stories put on the Library's shelf this session, so the button can say so. */
  let shelved = $state<Set<string>>(new Set())

  async function addToLibrary() {
    const item = await feed.addToLibrary(story.id)
    if (item) shelved = new Set([...shelved, story.id])
  }

  let settings = $state(false)
</script>

<article class="story">
  <div class="toolbar-row" role="toolbar" aria-label="Story actions">
    <button
      class="back"
      title="Back to the list (Escape)"
      aria-label="Back to the list"
      onclick={() => feed.close()}
    >
      <Icon name="chevron" size={15} />
    </button>
    {#if story.url}
      <button
        class="btn btn-outline"
        title="Open the original (V)"
        onclick={() => void feed.openOriginal(story.id)}
      >
        <Icon name="link" size={15} /> Open original
      </button>
    {/if}
    {#if story.discussionUrl}
      <button
        class="btn btn-outline"
        title="Open the discussion"
        onclick={() => void feed.openDiscussion(story.id)}
      >
        <Icon name="people" size={15} /> Discussion
      </button>
    {/if}
    <button
      class="btn btn-outline"
      class:on={story.saved}
      aria-pressed={story.saved}
      title={story.saved ? 'Remove from saved (S)' : 'Save, and keep it past its age (S)'}
      onclick={() => void feed.toggleSaved(story.id)}
    >
      <Icon name="star" size={15} filled={story.saved} />
      {story.saved ? 'Saved' : 'Save'}
    </button>
    <button
      class="btn btn-outline"
      title={story.read ? 'Mark unread (U)' : 'Mark read (I)'}
      onclick={() => void feed.toggleRead(story.id)}
    >
      <Icon name="tick" size={15} />
      {story.read ? 'Mark unread' : 'Mark read'}
    </button>
    <button
      class="btn btn-outline"
      class:on={shelved.has(story.id)}
      disabled={shelved.has(story.id)}
      title="Put it on the Library's Articles shelf (L)"
      onclick={() => void addToLibrary()}
    >
      <Icon name="book" size={15} />
      {shelved.has(story.id) ? 'In the Library' : 'Add to Library'}
    </button>
    {#if canFetch}
      <span class="spacer"></span>
      <button
        class="btn btn-outline"
        disabled={feed.extracting}
        title="Fetch the article this links to and read it here"
        onclick={() => void feed.fetchFullArticle(story.id)}
      >
        <Icon name="globe" size={15} />
        {feed.extracting ? 'Fetching…' : 'Fetch the full article'}
      </button>
    {/if}
  </div>

  {#key story.id}
    <div class="scroll">
      <div class="column">
        <header class="subject-block">
          <h1>{story.title.trim() || host || 'Untitled'}</h1>
          <p class="meta">
            {#if source}
              <span class="from">
                <SourceIcon source={source.source} size={18} />
                {sourceName}
              </span>
            {/if}
            {#if author}
              <span class="sep">·</span>
              <span>{author}</span>
            {/if}
            {#if published}
              <span class="sep">·</span>
              <span>{published}</span>
            {/if}
            {#if host}
              <span class="sep">·</span>
              <span class="host" title={story.url}>{host}</span>
            {/if}
          </p>
          {#if also.length > 0}
            <p class="also">
              <span>Also on</span>
              {#each also as place, i (i)}
                {#if i > 0}<span class="sep">·</span>{/if}
                {#if place.url}
                  <button class="link" onclick={() => void openExternal(place.url)}>
                    {place.name}
                  </button>
                {:else}
                  <b>{place.name}</b>
                {/if}
                {#if place.detail}<span>{place.detail}</span>{/if}
              {/each}
            </p>
          {/if}
        </header>

        <div class="reading">
          {#if story.expired}
            <p class="loading">
              This story is past its age, so its text was removed. The original may still be there.
            </p>
          {:else if failed}
            <p class="loading">This story could not be loaded.</p>
          {:else if doc === null}
            <p class="loading">Loading…</p>
          {:else}
            {#if body?.imagesHidden}
              <div class="images-bar">
                <span>Images hidden</span>
                {#if story.mailMessage}
                  <span class="sep">·</span>
                  <span>as in Mail, until they are allowed there</span>
                {:else}
                  <span class="sep">·</span>
                  <button class="link" onclick={() => (settings = true)}>
                    Load pictures for this source…
                  </button>
                {/if}
              </div>
            {/if}
            {#key doc}
              <div class="frame-wrap card" use:autoSize={doc}>
                <iframe
                  title={story.title || 'Story'}
                  sandbox="allow-popups allow-popups-to-escape-sandbox"
                  srcdoc={doc}
                ></iframe>
              </div>
            {/key}
          {/if}
        </div>
      </div>
    </div>
  {/key}
</article>

{#if settings && source}
  <SourceSettings source={source.source} onclose={() => (settings = false)} />
{/if}

<style>
  .story {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  /* Wraps rather than clips: on a narrow window the later buttons drop to a
     second line instead of falling off the edge. Its own name, not the shared
     `.toolbar`, which is the filter bars' row. */
  .toolbar-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px var(--sp-2);
    min-height: 56px;
    flex: none;
    padding: var(--sp-2) var(--sp-4);
    border-bottom: 1px solid var(--border);
  }
  .back {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    border-radius: 999px;
    color: var(--fg-muted);
    transform: rotate(180deg);
    transition: background var(--fast) var(--ease);
  }
  .back:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .spacer {
    flex: 1;
  }
  /* Save and the Library, once done: the accent, in colour alone. */
  .toolbar-row .btn.on {
    color: var(--accent);
  }
  .toolbar-row .btn.on[disabled] {
    opacity: 0.8;
  }

  .scroll {
    flex: 1;
    overflow-y: auto;
  }
  /* A measure to read at: an article across a wide window is a line too long
     for the eye to find the next one. */
  .column {
    width: 100%;
    max-width: 780px;
    margin-inline: auto;
  }

  .subject-block {
    padding: var(--sp-5) var(--sp-6) var(--sp-2);
  }
  h1 {
    margin: 0;
    font-size: var(--text-xl);
    font-weight: 700;
    letter-spacing: -0.02em;
    line-height: var(--leading-tight);
    overflow-wrap: anywhere;
    user-select: text;
  }
  .meta,
  .also {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    margin: 6px 0 0;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .also b {
    font-weight: 600;
    color: var(--fg-muted);
  }
  .from {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--fg-muted);
    font-weight: 560;
  }
  .host {
    color: var(--fg-faint);
  }
  /* A small round dot between the facts, drawn rather than typed: the
     middle-dot glyph sits on whatever baseline the font gives it. */
  .sep {
    width: 3px;
    height: 3px;
    border-radius: 50%;
    background: currentColor;
    opacity: 0.6;
    font-size: 0;
  }

  .reading {
    padding: var(--sp-2) var(--sp-6) var(--sp-8);
  }
  .loading {
    padding: var(--sp-3) 0;
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }

  .images-bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 2px var(--sp-2);
    margin-bottom: var(--sp-2);
    padding: 6px var(--sp-3);
    border-radius: var(--radius);
    background: var(--bg-sunken);
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .link {
    color: var(--accent);
    text-decoration: underline;
  }

  /* No height of its own: the frame is as tall as `estimateArticleHeight`
     says the article is, and the pane around it scrolls, so a story reads as
     one page rather than a box with a scrollbar inside another. The card is
     the page colour the document paints behind plain text, so the frame's
     edge and the card's are one edge. */
  .frame-wrap {
    overflow: hidden;
  }
  .frame-wrap.card {
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg);
  }
  .frame-wrap iframe {
    display: block;
    width: 100%;
    border: 0;
  }
</style>
