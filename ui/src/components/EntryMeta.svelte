<script lang="ts">
  import type { Entry } from '../lib/types'
  import { app } from '../lib/state.svelte'

  let { entry }: { entry: Entry } = $props()

  let adding = $state(false)
  let draft = $state('')

  function commitTag() {
    const tag = draft.trim().replace(/^#/, '')
    draft = ''
    adding = false
    if (!tag) return
    if (entry.tags.some((t) => t.toLowerCase() === tag.toLowerCase())) return
    entry.tags = [...entry.tags, tag]
    app.scheduleSave()
  }

  function removeTag(tag: string) {
    entry.tags = entry.tags.filter((t) => t !== tag)
    app.scheduleSave()
  }
</script>

<div class="meta">
  {#if entry.location?.placeName || entry.location?.locality}
    <span class="place">◉ {entry.location.placeName ?? entry.location.locality}</span>
  {/if}

  {#each entry.tags as tag (tag)}
    <button class="tag chip" onclick={() => removeTag(tag)} title="Remove tag">
      {tag}<span class="x">×</span>
    </button>
  {/each}

  {#if adding}
    <!-- svelte-ignore a11y_autofocus -->
    <input
      class="tag-input"
      placeholder="tag"
      bind:value={draft}
      autofocus
      onblur={commitTag}
      onkeydown={(e) => {
        if (e.key === 'Enter') commitTag()
        if (e.key === 'Escape') { draft = ''; adding = false }
      }}
    />
  {:else}
    <button class="add" onclick={() => (adding = true)}>+ Tag</button>
  {/if}
</div>

<style>
  .meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    margin-top: var(--sp-3);
  }

  .place { font-size: var(--text-sm); color: var(--fg-subtle); }

  .chip { cursor: pointer; transition: all var(--fast) var(--ease); }
  .chip:hover { border-color: var(--danger); color: var(--danger); }
  .x { margin-left: 4px; opacity: 0; transition: opacity var(--fast) var(--ease); }
  .chip:hover .x { opacity: 1; }

  .add {
    font-size: var(--text-xs);
    font-weight: 500;
    color: var(--fg-faint);
    padding: 0 var(--sp-2);
    height: 20px;
    border-radius: 99px;
    border: 1px dashed var(--border-strong);
  }
  .add:hover { color: var(--fg-muted); border-color: var(--fg-subtle); }

  .tag-input {
    height: 20px;
    width: 90px;
    padding: 0 var(--sp-2);
    border-radius: 99px;
    border: 1px solid var(--accent);
    background: var(--bg-raised);
    font-size: var(--text-xs);
    user-select: text;
  }
  .tag-input:focus { outline: none; }
</style>
