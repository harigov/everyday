<script lang="ts">
  // A source's mark: the site's own icon from the blob store, or its initials
  // on a colour when it has none.
  //
  // The icon is fetched once, by the first refresh, so a source added a
  // moment ago has none yet -- and a blob id can outlive its blob (a vault
  // restored from an older backup), so a picture that fails to load falls
  // back to the initials too, as `Cover` does, rather than leaving the
  // browser's broken-image glyph in the middle of the sidebar.

  import { mediaUrl } from '../lib/api'
  import { sourceLabel } from '../lib/feed'
  import type { NewsSource } from '../lib/types'
  import Avatar from './Avatar.svelte'

  let { source, size = 18 }: { source: NewsSource; size?: number } = $props()

  let broken = $state(false)
  // A new blob deserves a new attempt.
  $effect(() => {
    void source.icon
    broken = false
  })

  const src = $derived(source.icon && !broken ? mediaUrl(source.icon) : null)
</script>

{#if src}
  <img
    class="icon"
    {src}
    alt=""
    style:--size="{size}px"
    loading="lazy"
    decoding="async"
    onerror={() => (broken = true)}
  />
{:else}
  <Avatar name={sourceLabel(source)} {size} />
{/if}

<style>
  /* A white tile under the picture, in both themes: most site icons are drawn
     for a light page, and a dark glyph on the dark theme would vanish. */
  .icon {
    flex: none;
    display: block;
    width: var(--size);
    height: var(--size);
    padding: calc(var(--size) * 0.12);
    border-radius: 50%;
    background: #fff;
    object-fit: contain;
    box-shadow: inset 0 0 0 1px rgb(0 0 0 / 0.08);
  }
</style>
