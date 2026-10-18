<script lang="ts">
  // Looking for a cover by searching for pictures of the thing.
  //
  // The third way to a cover, beside a file off the disk and an address
  // pasted in: for when no lookup finds the right edition, and nobody has a
  // picture of it to hand. What the search answers is addresses, and the
  // webview may not load an address from the internet -- the content security
  // policy allows `data:` and `everyday:` and nothing remote -- so every
  // thumbnail comes through `api.imagePreview`, fetched by the service on a
  // cover's terms and kept nowhere. Only the picture somebody picks is ever
  // stored, through `library.chooseCoverFrom`.

  import { onMount, untrack } from 'svelte'
  import { api } from '../lib/api'
  import { focusOnMount } from '../lib/focus'
  import { library } from '../lib/library.svelte'
  import { app, errorMessage, isLocked } from '../lib/state.svelte'
  import type { ImageResult, Item } from '../lib/types'
  import Icon from './Icon.svelte'

  let {
    item,
    ratio,
    onclose,
  }: {
    item: Item
    /** The shelf's cover shape, so a tile looks like the card it would make. */
    ratio: string
    onclose: () => void
  } = $props()

  // The title and the byline, which is what tells one "Dune" from another.
  // Read once: the box is somebody's to edit from here on, and a title typed
  // in the panel beside it must not rewrite what they are searching for.
  let query = $state(untrack(() => [item.title, item.creator].filter((w) => w.trim()).join(' ')))
  let results = $state<ImageResult[]>([])
  let searching = $state(false)
  let searched = $state(false)
  let error = $state<string | null>(null)
  /** Bumped by every search; the grid is keyed on it so old tiles go. */
  let round = $state(0)

  // ── previews ─────────────────────────────────────────────────────────
  //
  // A string is a picture to draw, `null` a preview that would not load, and
  // no entry at all one not asked for yet. Asked for only once a tile is
  // near the screen, and no more than a few at a time: a grid of twelve is
  // twelve requests to strangers' servers, and the ones nobody scrolls to
  // need not be made at all.

  const AT_ONCE = 4
  let previews = $state<Record<string, string | null>>({})
  let waiting: string[] = []
  let running = 0
  let asked = new Set<string>()

  const previewOf = (result: ImageResult) => result.thumbnailUrl || result.imageUrl

  function want(url: string) {
    if (!url || asked.has(url)) return
    asked.add(url)
    waiting.push(url)
    pump()
  }

  function pump() {
    while (running < AT_ONCE && waiting.length > 0) {
      const url = waiting.shift()!
      // A preview that arrives after somebody searched again belongs to a
      // grid that is gone, and must not be drawn into the new one.
      const mine = round
      running++
      void api
        .imagePreview(url)
        .then((src) => {
          if (mine === round) previews[url] = src
        })
        .catch((e) => {
          if (isLocked(e)) void app.lock()
          else if (mine === round) previews[url] = null
        })
        .finally(() => {
          running--
          pump()
        })
    }
  }

  /** Ask for a tile's preview once it is close to being seen. */
  function nearScreen(node: HTMLElement, url: string) {
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
      { rootMargin: '160px' },
    )
    watcher.observe(node)
    return { destroy: () => watcher.disconnect() }
  }

  // ── searching ────────────────────────────────────────────────────────

  async function search() {
    const words = query.trim()
    if (!words) return
    round++
    waiting = []
    asked = new Set()
    previews = {}
    results = []
    error = null
    searching = true
    const mine = round
    try {
      const found = await api.imageSearch(words)
      if (mine === round) results = found
    } catch (e) {
      if (isLocked(e)) await app.lock()
      else if (mine === round) error = errorMessage(e)
    } finally {
      if (mine === round) {
        searching = false
        searched = true
      }
    }
  }

  // Straight away, with the words already in the box: somebody pressed
  // "Search for a picture", and the likeliest next thing they would do is
  // press Enter on what is already there.
  onMount(() => void search())

  async function pick(result: ImageResult) {
    // Closed first. The cover itself shows that something is on its way --
    // its buttons are disabled while `library.enriching` -- and a panel left
    // open over a choice already made invites a second click.
    onclose()
    await library.chooseCoverFrom(item.id, result)
  }

  function describe(result: ImageResult): string {
    return result.title ? `${result.title} — ${result.site}` : result.site
  }
</script>

<div class="cover-search">
  <form
    class="bar"
    onsubmit={(e) => {
      e.preventDefault()
      void search()
    }}
  >
    <input
      type="text"
      placeholder="What is it a picture of?"
      aria-label="Search for a picture"
      bind:value={query}
      use:focusOnMount
      onkeydown={(e) => {
        if (e.key === 'Escape') {
          e.preventDefault()
          onclose()
        }
      }}
    />
    <button class="mini" type="submit" disabled={searching || !query.trim()}>Search</button>
    <button class="mini icon" type="button" title="Close" aria-label="Close" onclick={onclose}>
      <Icon name="close" size={12} />
    </button>
  </form>

  {#if searching}
    <p class="hint">Searching…</p>
  {:else if error}
    <p class="error">{error}</p>
  {:else if searched && results.length === 0}
    <p class="hint">No pictures found — try other words.</p>
  {:else if results.length > 0}
    {#key round}
      <div class="grid" role="list" style="--ratio: {ratio}">
        {#each results as result, i (i)}
          {@const url = previewOf(result)}
          <div role="listitem">
            <button
              class="tile"
              title={describe(result)}
              aria-label="Use {describe(result)}"
              disabled={library.enriching}
              onclick={() => void pick(result)}
              use:nearScreen={url}
            >
              <span class="thumb">
                {#if previews[url]}
                  <img src={previews[url]} alt="" />
                {:else if previews[url] === null}
                  <!-- A preview that would not load still has a picture
                       behind it, which may well come home when chosen:
                       the tile stays, plainly, rather than vanishing. -->
                  <span class="none"><Icon name="image" size={16} /></span>
                {/if}
              </span>
              <span class="site">{result.site}</span>
            </button>
          </div>
        {/each}
      </div>
    {/key}
    <p class="hint small">Picking one saves it as the cover. Each belongs to the site under it.</p>
  {/if}
</div>

<style>
  .cover-search {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    /* Snug under the cover it is choosing for, as the address form is. */
    margin-top: calc(var(--sp-3) * -1);
  }

  .bar {
    display: flex;
    gap: var(--sp-1);
  }
  .bar input {
    flex: 1;
    min-width: 0;
    height: 26px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    font-size: var(--text-sm);
    color: var(--fg);
    user-select: text;
  }
  .bar input:focus {
    outline: none;
    border-color: var(--tint, var(--accent));
  }

  /* The detail panel's own small button, for this panel's two. */
  .mini {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 4px;
    height: 26px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-xs);
    font-weight: 550;
    color: var(--fg-subtle);
    background: var(--bg-hover);
  }
  .mini.icon {
    width: 26px;
    padding: 0;
  }
  .mini:hover:not(:disabled) {
    color: var(--fg);
    background: var(--bg-active);
  }
  .mini:disabled {
    opacity: 0.45;
  }

  .hint,
  .error {
    margin: 0;
  }
  .hint.small {
    font-size: var(--text-xs);
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: var(--sp-2);
  }
  .tile {
    display: flex;
    flex-direction: column;
    gap: 2px;
    width: 100%;
    padding: 0;
    text-align: left;
    border-radius: var(--radius-sm);
  }
  .tile:disabled {
    opacity: 0.6;
  }
  /* The shelf's own shape, and the whole picture inside it: this is where
     somebody decides whether the edition is right, and cropping the title off
     a cover is the way to make that decision wrong. */
  .thumb {
    display: grid;
    place-items: center;
    aspect-ratio: var(--ratio);
    overflow: hidden;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-hover);
  }
  .tile:hover:not(:disabled) .thumb {
    border-color: var(--tint, var(--accent));
  }
  .tile:focus-visible {
    outline: none;
  }
  .tile:focus-visible .thumb {
    border-color: var(--tint, var(--accent));
    box-shadow: 0 0 0 2px color-mix(in oklab, var(--tint, var(--accent)) 30%, transparent);
  }
  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: contain;
  }
  .none {
    color: var(--fg-faint);
  }
  .site {
    overflow: hidden;
    font-size: var(--text-xs);
    color: var(--fg-faint);
    white-space: nowrap;
    text-overflow: ellipsis;
  }
</style>
