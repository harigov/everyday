<script lang="ts">
  // The Feed app's panes: the list of stories and the one being read, beside
  // the sidebar -- the shape Mail has, and for the same reason. What it
  // follows is in `FeedNav`; this is what is read.
  //
  // Until there is something followed there is nothing to list, so the whole
  // pane says how to start instead of showing an empty column beside an empty
  // page.

  import { feed } from '../lib/feed.svelte'
  import AddSource from './AddSource.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import StoryList from './StoryList.svelte'
  import StoryView from './StoryView.svelte'

  // Loaded when the view appears rather than when the store is imported, so a
  // vault whose owner never opens this app never reads a source. `start` is
  // idempotent, so coming back refreshes instead of blanking the list.
  //
  // `ready` is the view's own: the store's lists are empty before they are
  // loaded as well as after, and "you follow nothing" for the length of one
  // read is a flash of the wrong sentence.
  let ready = $state(false)
  void feed.start().finally(() => (ready = true))

  const empty = $derived(ready && feed.sources.length === 0)
  const story = $derived(feed.story)

  let followingHackerNews = $state(false)

  /** The front page in one press: the commonest first thing to follow, and it needs no address. */
  async function followHackerNews() {
    if (followingHackerNews) return
    followingHackerNews = true
    await feed.addSource({ type: 'hackerNews', list: 'front', minPoints: 0 })
    followingHackerNews = false
  }
</script>

{#if empty}
  <main class="main">
    <EmptyState lead="Nothing followed yet.">
      {#snippet icon()}<Icon name="rss" size={30} weight={1.3} />{/snippet}
      {#snippet note()}
        Follow a site, Hacker News, or the newsletters already arriving in your mail. Everything is
        kept on this device and can be read with no network.
      {/snippet}
      {#snippet action()}
        <div class="actions">
          <button class="btn btn-primary" onclick={() => feed.openAdd()}>Add a feed</button>
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
  <StoryList />
  <main class="main">
    {#if story}
      <StoryView {story} />
    {:else}
      <EmptyState lead="Select a story">
        {#snippet note()}j and k move, Enter opens.{/snippet}
      </EmptyState>
    {/if}
  </main>
{/if}

{#if feed.adding}
  <AddSource />
{/if}

<style>
  .main {
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
