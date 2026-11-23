<script lang="ts">
  // The Feed app's panes: the list of stories and the one being read, beside
  // the sidebar -- the shape Mail has, and for the same reason. What it
  // follows is in `FeedNav`; this is what is read.
  //
  // Until there is something followed there is nothing to list, so the whole
  // pane says how to start instead of showing an empty column beside an empty
  // page.
  //
  // Which list is in the column follows the sidebar. A source, a folder and
  // All -- narrowed to one kind of feed when a chip is on -- are stories,
  // newest first: `StoryList`. Recommended is stories too, but chosen and
  // ordered by the Feed: the same two panes, with its own column. Up Next is
  // not a list of stories at all -- it is episodes, files and articles in the
  // order they will be taken in -- so it is a page of its own, whole width,
  // with nothing to read beside it. So is History, what was read, heard and
  // watched, by day: a record, not a list to read down, and its rows open a
  // story in its own source's list.

  import { feed } from '../lib/feed.svelte'
  import { upnext } from '../lib/upnext.svelte'
  import AddSource from './AddSource.svelte'
  import EmptyState from './EmptyState.svelte'
  import HistoryView from './HistoryView.svelte'
  import Icon from './Icon.svelte'
  import PodcastDirectory from './PodcastDirectory.svelte'
  import RecommendedView from './RecommendedView.svelte'
  import StoryList from './StoryList.svelte'
  import StoryView from './StoryView.svelte'
  import UpNextView from './UpNextView.svelte'

  // Loaded when the view appears rather than when the store is imported, so a
  // vault whose owner never opens this app never reads a source. `start` is
  // idempotent, so coming back refreshes instead of blanking the list.
  //
  // `ready` is the view's own: the store's lists are empty before they are
  // loaded as well as after, and "you follow nothing" for the length of one
  // read is a flash of the wrong sentence.
  let ready = $state(false)
  void feed.start().finally(() => (ready = true))
  // Up Next's count is in the sidebar, which is there before any page is.
  void upnext.load()
  // Background work -- transcripts, articles, audio -- is looked at once, so
  // a row that is being transcribed says so the first time it is drawn.
  void upnext.ensureWork()

  const empty = $derived(ready && feed.sources.length === 0)
  const story = $derived(feed.story)
  const scope = $derived(feed.scope.kind)

  let followingHackerNews = $state(false)

  /** The front page in one press: the commonest first thing to follow, and it needs no address. */
  async function followHackerNews() {
    if (followingHackerNews) return
    followingHackerNews = true
    await feed.addSource({ type: 'hackerNews', list: 'front', minPoints: 0 })
    followingHackerNews = false
  }
</script>

{#if scope === 'upnext'}
  <!-- Whether or not anything is followed: files and library items queue too. -->
  <UpNextView />
{:else if scope === 'history'}
  <!-- Whether or not anything is followed: files and library items are taken in too. -->
  <HistoryView />
{:else if empty}
  <main class="main">
    <EmptyState lead="Nothing followed yet.">
      {#snippet icon()}<Icon name="rss" size={30} weight={1.3} />{/snippet}
      {#snippet note()}
        Follow a site, a podcast, Hacker News, or the newsletters already arriving in your mail.
        Everything is kept on this device and can be read with no network.
      {/snippet}
      {#snippet action()}
        <div class="actions">
          <button class="btn btn-primary" onclick={() => feed.openAdd()}>Add a feed</button>
          <button class="btn" onclick={() => feed.openDirectory('podcasts')}>
            Browse podcasts
          </button>
          <button
            class="btn"
            disabled={followingHackerNews}
            onclick={() => void followHackerNews()}
          >
            Follow Hacker News
          </button>
        </div>
      {/snippet}
    </EmptyState>
  </main>
{:else}
  {#if scope === 'recommended'}
    <RecommendedView />
  {:else}
    <StoryList />
  {/if}
  <main class="main">
    {#if story}
      <StoryView {story} />
    {:else}
      <EmptyState lead="Select a story">
        {#snippet note()}
          {scope === 'recommended'
            ? 'The arrow keys move, Enter opens.'
            : 'j and k move, Enter opens.'}
        {/snippet}
      </EmptyState>
    {/if}
  </main>
{/if}

{#if feed.adding}
  <AddSource />
{/if}

{#if feed.directoryOpen}
  <PodcastDirectory />
{/if}

<style>
  .main {
    /* As tall as the stage by a stated height, not stretched: see
       `.shell-stage` in app.css. */
    height: 100%;
    flex: 1;
    /* Never crushed to nothing by the list column beside it growing:
       `PaneResizer`'s own `max` stops short of most window widths, but a
       narrow window plus a list dragged wide should still leave a reading
       pane rather than squeeze it to a sliver. */
    min-width: 360px;
    display: flex;
    flex-direction: column;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: var(--sp-2);
  }
</style>
