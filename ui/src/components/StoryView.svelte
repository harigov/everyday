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
  //
  // An episode is the same pane with a different head: the show's artwork, the
  // length and how far along, and what can be done with it as something to
  // hear (`EpisodeActions`), over its show notes -- which are the body, drawn
  // exactly as an article's is. What only makes sense for an article is left
  // off it: Listen reads the page aloud, and there is no article to fetch
  // behind a recording.
  //
  // What was said in an episode can be written down (a transcript, opened in
  // the reader like any document) and written up (an article, which replaces
  // the body with the telling and keeps the show notes under it). Both are the
  // service's background work, and the buttons say how far along it is. When
  // the body is the telling, a quiet line above it says so, and offers to write
  // it again. An article, for its part, can be saved as audio -- that is
  // `EpisodeActions`, which also plays it.

  import { untrack } from 'svelte'
  import { feed, type LoadedBody } from '../lib/feed.svelte'
  import { listening } from '../lib/listen.svelte'
  import {
    alsoOn,
    distinctAuthor,
    estimateArticleHeight,
    hostOf,
    looksCutOff,
    sourceLabel,
  } from '../lib/feed'
  import { mediaUrl } from '../lib/api'
  import { openTranscript } from '../lib/episodes'
  import { dateFormat } from '../lib/format'
  import { applyPlatformOrigin } from '../lib/mailview'
  import { openExternal } from '../lib/open-external'
  import { episodeProgress, formatDuration, workLine } from '../lib/podcast'
  import { upnext } from '../lib/upnext.svelte'
  import type { Story } from '../lib/types'
  import EpisodeActions from './EpisodeActions.svelte'
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

  // ── An episode ────────────────────────────────────────────────────────

  const media = $derived(story.media)
  const length = $derived(media?.durationMs ?? null)
  /** "Season 2 · Episode 5", for a show that numbers them. */
  const numbering = $derived.by(() => {
    const parts: string[] = []
    if (media?.season) parts.push(`Season ${media.season}`)
    if (media?.episode) parts.push(`Episode ${media.episode}`)
    return parts.join(' · ')
  })
  const progress = $derived(
    media ? episodeProgress(upnext.progressOf({ type: 'story', id: story.id }), length) : null,
  )
  /** The show's own picture, when the first refresh found one; its initials otherwise. */
  const artwork = $derived(source?.source.icon ? mediaUrl(source.source.icon) : null)
  let artBroken = $state(false)
  $effect(() => {
    void artwork
    artBroken = false
  })

  // ── What was said in it ───────────────────────────────────────────────

  /**
   * The transcript, once there is one and while it is in Files: the story
   * links it by id, and somebody may have put the file in the trash since.
   * Looked up as the story is opened; a transcript that is gone is shown as no
   * transcript, which offers to write it down again.
   */
  const transcript = $derived(upnext.linkedFile(story.files?.transcript))
  /** What is passed to the store for "write it up": the story as this pane sees its transcript. */
  const written = $derived({ id: story.id, files: { transcript } })
  /** The transcript was asked for and found gone, so the pane says so. */
  let lost = $state(false)

  $effect(() => {
    void upnext.checkLinks([story.files?.transcript, story.files?.audio])
  })
  $effect(() => {
    void story.id
    lost = false
  })

  async function readTranscript() {
    lost = false
    if (!(await openTranscript(transcript))) lost = true
  }
  const transcribing = $derived(upnext.workFor({ story: story.id }, 'transcript'))
  const writing = $derived(upnext.workFor({ story: story.id }, 'article'))
  const transcribeBusy = $derived(!!transcribing && transcribing.state !== 'failed')
  const writeBusy = $derived(!!writing && writing.state !== 'failed')

  // ── The body ──────────────────────────────────────────────────────────

  let body = $state<LoadedBody | null>(null)
  let failed = $state(false)
  /** Which story `body` belongs to, so a new one never shows under the old one's. */
  let shownFor = ''
  /** Counts loads, so one that was overtaken by a newer story is ignored when it lands. */
  let loads = 0

  /**
   * Whether the body is the telling of the episode and not the feed's own text.
   * The document says so when the service does; when it does not, this window's
   * own memory of having asked for it is what there is.
   */
  const writtenFrom = $derived(
    !!media && (body?.origin ? body.origin === 'transcript' : upnext.wroteFromTranscript(story.id)),
  )

  // Reload when the story changes -- or gains a body, which is what fetching
  // the full article does -- and once more when an extraction ends, since a
  // feed's excerpt that has just been replaced by the article does not change
  // `hasBody`: it was already true.
  //
  // Keyed on these values and not on `story` itself: the store replaces
  // its story objects whenever the list reloads, and a refresh that changed
  // nothing about this one is no reason to fetch its body again. The stamp is
  // the third: an article written from a transcript replaces the body of a
  // story that had one already.
  const storyId = $derived(story.id)
  const hasBody = $derived(story.hasBody)
  const expired = $derived(story.expired)
  const stamp = $derived(feed.bodyStamp(story.id))

  $effect(() => {
    const id = storyId
    void hasBody
    void stamp
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
      !story.media &&
      !story.expired &&
      (!story.hasBody || (body !== null && looksCutOff(body.html))),
  )

  /** Stories put on the Library's shelf this session, so the button can say so. */
  let shelved = $state<Set<string>>(new Set())

  async function addToLibrary() {
    const item = await feed.addToLibrary(story.id)
    if (item) shelved = new Set([...shelved, story.id])
  }

  // ── Reading it aloud ──────────────────────────────────────────────────

  /** Whether this story is what is being read now, in either way. */
  const reading = $derived(listening.isReading({ type: 'story', storyId: story.id }))
  const preparing = $derived(reading && listening.state === 'preparing')

  /**
   * Start reading the story, or stop when it already is. `asWritten` skips
   * the model: the article's own sentences, with only what cannot be said
   * aloud taken out. It is a button of its own beside Listen rather than a
   * hidden modifier, since the choice is one somebody should see they have.
   */
  function listen(asWritten: boolean) {
    if (reading) {
      listening.stop()
      return
    }
    void listening.start(
      asWritten
        ? { type: 'story', storyId: story.id, asWritten: true }
        : { type: 'story', storyId: story.id },
      story.title.trim() || host || 'Untitled',
    )
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
    {#if !story.media}
      <EpisodeActions {story} />
    {/if}
    {#if listening.available && !story.expired && !story.media}
      <button
        class="btn btn-outline"
        class:on={reading}
        title={reading
          ? 'Stop reading it aloud'
          : 'Listen to the article, made fit to hear: tables, lists and links are put into words'}
        onclick={() => listen(false)}
      >
        <Icon name={reading ? 'stop' : 'play'} size={15} />
        {reading ? (preparing ? 'Preparing…' : 'Stop') : 'Listen'}
      </button>
      {#if !reading}
        <button
          class="btn btn-outline"
          title="Listen to it read as written, without the model changing anything"
          onclick={() => listen(true)}
        >
          Read as written
        </button>
      {/if}
    {/if}
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
          <div class="head">
            {#if media}
              <span class="art">
                {#if artwork && !artBroken}
                  <img src={artwork} alt="" decoding="async" onerror={() => (artBroken = true)} />
                {:else if source}
                  <SourceIcon source={source.source} size={88} />
                {/if}
              </span>
            {/if}
            <div class="head-text">
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
                {#if length}
                  <span class="sep">·</span>
                  <span>{formatDuration(length)}</span>
                {/if}
                {#if numbering}
                  <span class="sep">·</span>
                  <span>{numbering}</span>
                {/if}
                {#if host && !media}
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
            </div>
          </div>

          {#if media}
            <div class="episode">
              <EpisodeActions {story} />
              {#if progress}
                <div class="along">
                  <div
                    class="bar"
                    role="progressbar"
                    aria-label="How much has been played"
                    aria-valuemin="0"
                    aria-valuemax="100"
                    aria-valuenow={Math.round(progress.fraction * 100)}
                  >
                    <span style:width="{progress.fraction * 100}%"></span>
                  </div>
                  <span class="along-label">{progress.label}</span>
                </div>
              {/if}
              <div class="words" role="group" aria-label="Transcript and article">
                {#if transcribeBusy}
                  <span class="status" role="status">
                    <span class="spin"><Icon name="quote" size={15} /></span>
                    {workLine(transcribing)}
                  </span>
                {:else if transcript}
                  <button
                    class="btn btn-outline"
                    title="Read what was said, in the reader beside the assistant"
                    onclick={() => void readTranscript()}
                  >
                    <Icon name="quote" size={15} /> Open transcript
                  </button>
                {:else}
                  <button
                    class="btn btn-outline"
                    title="Write down what was said in it"
                    onclick={() => void upnext.transcribe(story.id)}
                  >
                    <Icon name="quote" size={15} />
                    {transcribing ? 'Transcription failed, try again' : 'Transcribe'}
                  </button>
                {/if}
                <!-- Once the body is the telling, writing it again is offered where it says so. -->
                {#if !writtenFrom}
                  {#if writeBusy}
                    <span class="status" role="status">
                      <span class="spin"><Icon name="newspaper" size={15} /></span>
                      {workLine(writing)}
                    </span>
                  {:else}
                    <button
                      class="btn btn-outline"
                      title="Write the episode up as an article from its transcript, which is made first if there is none"
                      onclick={() => void upnext.readAsArticle(written)}
                    >
                      <Icon name="newspaper" size={15} />
                      {writing ? 'Writing failed, try again' : 'Read as an article'}
                    </button>
                  {/if}
                {/if}
              </div>
              {#if lost && !transcript}
                <p class="origin" role="status">
                  The transcript is no longer in Files. It can be written down again.
                </p>
              {/if}
              {#if transcribing?.state === 'failed' && transcribing.error}
                <p class="trouble" role="status">{transcribing.error}</p>
              {:else if writing?.state === 'failed' && writing.error}
                <p class="trouble" role="status">{writing.error}</p>
              {/if}
            </div>
          {/if}
        </header>

        <div class="reading">
          {#if media && !story.expired}
            <h2 class="notes-title">{writtenFrom ? 'Article' : 'Show notes'}</h2>
          {/if}
          {#if writtenFrom && !story.expired}
            <p class="origin">
              {#if writeBusy}
                <span role="status">{workLine(writing)}</span>
              {:else}
                Written from the episode’s transcript, with the show notes kept below it.
                <!-- Written from the transcript, which is made again first if it is gone. -->
                <button class="link" onclick={() => void upnext.readAsArticle(written)}>
                  Write again
                </button>
              {/if}
            </p>
          {/if}
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
  /* An episode's artwork beside its title; an article has no picture, and the
     text takes the whole width. */
  .head {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-4);
  }
  .head-text {
    flex: 1;
    min-width: 0;
  }
  .art {
    flex: none;
    display: flex;
    width: 88px;
    height: 88px;
    overflow: hidden;
    border-radius: var(--radius-lg);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .art img {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .episode {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    margin-top: var(--sp-4);
  }
  .along {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    max-width: 360px;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .bar {
    flex: 1;
    height: 4px;
    border-radius: 2px;
    background: var(--border);
    overflow: hidden;
  }
  .bar > span {
    display: block;
    height: 100%;
    background: var(--accent);
  }
  .along-label {
    flex: none;
  }
  /* Transcript and article: pills, as the episode's other actions are, with the
     progress of what is going where its button was. */
  .words {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px var(--sp-2);
  }
  .status {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  /* The icon breathes while the work goes on; stopped outright under reduced
     motion, where the words beside it still say it is going. */
  .spin {
    display: grid;
    animation: breathe 1.4s ease-in-out infinite;
  }
  @keyframes breathe {
    50% {
      opacity: 0.35;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .spin {
      animation: none;
    }
  }
  .trouble {
    margin: 0;
    font-size: var(--text-xs);
    color: var(--danger);
  }
  /* Where the body came from, said once and quietly over it. */
  .origin {
    margin: 0 0 var(--sp-3);
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .notes-title {
    margin: var(--sp-2) 0 var(--sp-3);
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--fg-faint);
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
