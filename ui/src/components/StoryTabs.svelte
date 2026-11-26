<script lang="ts">
  // The tabs of a story's pane and the document under each: an episode's Notes,
  // Transcript and Summary, an article's Article and Summary. See
  // `docs/plans/feed-usability.md`.
  //
  // Every tab is a document, drawn the way the body always was, and for the
  // same reason: it is markup somebody else wrote, and the only safe place for
  // that is a frame that can run nothing and read nothing back. The sandbox
  // is exactly `allow-popups allow-popups-to-escape-sandbox`, nowhere else in
  // this component or its callers, and it stays that way: `allow-scripts`
  // would let a story run code in this window's process, and
  // `allow-same-origin` would let this window be read from inside it. A link
  // in a story still has to open somewhere, which is what the popups
  // permission is for. A document arrives already sanitised, once, and already
  // drawn for this window's light or dark theme -- see `feed.loadBody`, which is
  // why nothing here themes it a second time: a page that has been themed once
  // reads as having colours of its own, and a second pass would paint it white.
  //
  // An article's own tab is the body its pane has loaded and hands down; every
  // other tab is one `part` of the body route, asked for when the tab is
  // shown and asked for again when a file it depends on arrives -- a
  // transcript, a summary -- or the body is replaced.
  //
  // A tab whose document does not exist yet says so and offers to make it, or,
  // once that is asked for, how far along it is, in words that come from
  // `story-tabs.ts`: whether a Summary says "Summarise" or "Write the article",
  // and whether a failure is offered again, is decided there where it can be
  // tested. Which tab is open is remembered per kind on this device.

  import { untrack } from 'svelte'
  import { feed, type LoadedBody } from '../lib/feed.svelte'
  import { estimateArticleHeight } from '../lib/feed'
  import { applyPlatformOrigin } from '../lib/mailview'
  import {
    offerLabel,
    offerText,
    panelFor,
    partOf,
    recallTab,
    rememberTab,
    tabKind,
    tabsOf,
    workStatus,
    type StoryTab,
    type TabAction,
    type TabKind,
  } from '../lib/story-tabs'
  import { upnext } from '../lib/upnext.svelte'
  import type { Story } from '../lib/types'
  import Icon from './Icon.svelte'
  import Loading from './Loading.svelte'
  import SubTabs from './SubTabs.svelte'

  let {
    story,
    body,
    failed,
    telling,
    onsettings,
  }: {
    story: Story
    /** The story's whole body, which the pane loads: the Article tab is it. */
    body: LoadedBody | null
    /** The body could not be loaded. */
    failed: boolean
    /** An episode's article has been written, so its Summary is there to read. */
    telling: boolean
    /** Open the source's settings, where pictures are allowed. */
    onsettings: () => void
  } = $props()

  const kind = $derived(tabKind(story))
  const tabs = $derived(tabsOf(story))

  // What was last picked, per kind, read once as the pane opens.
  let picked = $state<Record<TabKind, StoryTab>>({
    episode: recallTab('episode'),
    article: recallTab('article'),
  })
  const tab = $derived(picked[kind])
  const label = $derived(tabs.find((t) => t.id === tab)?.label ?? '')
  const heading = $derived(kind === 'episode' ? 'Episode' : 'Story')

  function pick(next: StoryTab) {
    picked[kind] = next
    rememberTab(kind, next)
  }

  // ── What there is ─────────────────────────────────────────────────────

  /**
   * The files, as the story links them and while they are in Files: somebody
   * may have trashed one since, and a tab that went by the link alone would
   * offer to read what is not there and never to make it again.
   */
  const transcriptFile = $derived(story.files?.transcript)
  const summaryFile = $derived(story.files?.summary)
  const transcript = $derived(upnext.linkedFile(transcriptFile) !== null)
  const summary = $derived(kind === 'episode' ? telling : upnext.linkedFile(summaryFile) !== null)
  const transcribing = $derived(upnext.workFor({ story: story.id }, 'transcript'))
  const writing = $derived(upnext.workFor({ story: story.id }, 'article'))
  const summarising = $derived(upnext.workFor({ story: story.id }, 'summary'))

  const panel = $derived(
    panelFor(kind, tab, { transcript, summary, transcribing, writing, summarising }),
  )

  function start(action: TabAction) {
    if (action === 'transcribe') void upnext.transcribe(story.id)
    else if (action === 'summarise') void upnext.summarize(story.id)
    else {
      // From the transcript as this pane sees it: one that is gone is made first.
      void upnext.readAsArticle({
        id: story.id,
        files: { transcript: upnext.linkedFile(transcriptFile) },
      })
    }
  }

  /** What stands for each kind of work and each thing offered. */
  const ICONS = {
    transcript: 'quote',
    article: 'newspaper',
    summary: 'sparkle',
    audio: 'play',
  } as const
  const OFFERED = { transcribe: 'quote', write: 'newspaper', summarise: 'sparkle' } as const

  // ── A part of the body ────────────────────────────────────────────────

  const part = $derived(partOf(tab))
  /** Whether there is a document to fetch: not the article's own tab, nor one still to be made. */
  const fetching = $derived(part !== undefined && panel.type === 'content')
  const stamp = $derived(feed.bodyStamp(story.id))

  /** The document the pane wants: this story's, this tab's. */
  const wanted = $derived(`${story.id}:${part ?? ''}`)
  /** What was loaded, and for which of those: a tab never shows under another's document. */
  let loaded = $state<{ key: string; doc: LoadedBody } | null>(null)
  /** The document that could not be loaded, if the last try was for one. */
  let brokenKey = $state<string | null>(null)
  /** Counts loads, so one that was overtaken by a newer tab or story is ignored when it lands. */
  let loads = 0

  // Keyed on the values below and not on `story` itself: the store replaces its
  // story objects whenever the list reloads, and a refresh that changed nothing
  // about this one is no reason to fetch its documents again. The two files and
  // the stamp are the reasons: a transcript or a summary has arrived, or an
  // article has been written over the body.
  $effect(() => {
    const key = wanted
    const asked = part
    void transcriptFile
    void summaryFile
    void stamp
    const mine = ++loads
    if (!fetching || asked === undefined) return
    brokenKey = null
    const target = untrack(() => story)
    // Untracked: whatever the store reads on the way to its first `await` is not
    // a reason to load this document again.
    untrack(() => feed.loadBody(target, asked))
      .then((got) => {
        if (mine !== loads) return
        brokenKey = got === null ? key : null
        loaded = got === null ? null : { key, doc: got }
      })
      .catch(() => {
        if (mine !== loads) return
        brokenKey = key
        loaded = null
      })
  })

  // ── Drawing it ────────────────────────────────────────────────────────

  /** Sizes the one iframe inside `node` from the document string, never from
   *  what the iframe renders -- see `estimateArticleHeight`'s own doc. */
  function autoSize(node: HTMLElement, html: string) {
    const iframe = node.querySelector('iframe')
    function apply() {
      if (!iframe) return
      iframe.style.height = `${estimateArticleHeight(html, node.clientWidth)}px`
    }
    apply()
    const ro = new ResizeObserver(apply)
    ro.observe(node)
    return { destroy: () => ro.disconnect() }
  }
</script>

{#snippet page(doc: LoadedBody, title: string)}
  {@const html = applyPlatformOrigin(doc.html)}
  {#if doc.imagesHidden}
    <div class="images-bar">
      <span>Images hidden</span>
      {#if story.mailMessage}
        <span class="sep">·</span>
        <span>as in Mail, until they are allowed there</span>
      {:else}
        <span class="sep">·</span>
        <button class="link" onclick={onsettings}>Load pictures for this source…</button>
      {/if}
    </div>
  {/if}
  <!-- Re-keyed on the document, so the sizing is measured from the new one. -->
  {#key html}
    <div class="frame-wrap card" use:autoSize={html}>
      <iframe {title} sandbox="allow-popups allow-popups-to-escape-sandbox" srcdoc={html}></iframe>
    </div>
  {/key}
{/snippet}

<div class="tabs">
  <SubTabs {tabs} bind:value={() => tab, pick} label={heading} />
</div>

<div role="tabpanel" aria-label={label}>
  {#if panel.type === 'working'}
    <p class="status" role="status">
      <span class="spin"><Icon name={ICONS[panel.kind]} size={15} /></span>
      {panel.line}
    </p>
  {:else if panel.type === 'offer'}
    <div class="offer">
      <p class="blurb">{offerText(panel.action)}</p>
      <div>
        <button class="btn btn-outline" onclick={() => start(panel.action)}>
          <Icon name={OFFERED[panel.action]} size={15} />
          {offerLabel(panel.action, panel.failed)}
        </button>
      </div>
      {#if panel.error}
        <p class="trouble" role="status">{panel.error}</p>
      {/if}
    </div>
  {:else}
    {#if tab === 'summary' && kind === 'episode'}
      <!-- Where it was read: what is going, or what went wrong, and the way to ask again. -->
      <p class="origin">
        {#if writing && writing.state !== 'failed'}
          <span role="status">{workStatus(writing)}</span>
        {:else}
          <button class="link" onclick={() => start('write')}>Write it again</button>
          {#if writing?.state === 'failed'}
            <span class="trouble" role="status">{writing.error ?? workStatus(writing)}</span>
          {/if}
        {/if}
      </p>
    {/if}
    {#if tab === 'article'}
      {#if failed}
        <p class="loading">This story could not be loaded.</p>
      {:else if body === null}
        <Loading label="Loading the story" />
      {:else}
        {@render page(body, story.title || 'Story')}
      {/if}
    {:else if brokenKey === wanted}
      <p class="loading">The {label.toLowerCase()} could not be loaded.</p>
    {:else if loaded === null || loaded.key !== wanted}
      <Loading label={`Loading the ${label.toLowerCase()}`} />
    {:else}
      {@render page(loaded.doc, `${label}: ${story.title || 'Story'}`)}
    {/if}
  {/if}
</div>

<style>
  /* The shared strip's own gap is sized for a page of settings; under a
     story's heading it would push the document half a screen down. */
  .tabs :global(.subtabs) {
    margin-bottom: var(--sp-3);
  }

  .loading {
    padding: var(--sp-3) 0;
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }

  /* What an empty tab says, and its one button. */
  .offer {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--sp-3);
    padding: var(--sp-3) 0;
  }
  .blurb {
    margin: 0;
    max-width: 52ch;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }
  .status {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    margin: var(--sp-3) 0;
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
  /* What is said once and quietly over a document. */
  .origin {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    margin: 0 0 var(--sp-3);
    font-size: var(--text-xs);
    color: var(--fg-subtle);
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
  .link {
    color: var(--accent);
    text-decoration: underline;
  }

  /* No height of its own: the frame is as tall as `estimateArticleHeight`
     says the document is, and the pane around it scrolls, so a story reads as
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
