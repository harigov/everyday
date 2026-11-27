<script lang="ts">
  // The journal: its list of days and the page of the one open, side by side
  // -- or on a phone one at a time, the page over the list (`details` in
  // `phone.svelte.ts`).
  //
  // A component of its own so it can be loaded like every other app's view
  // (`lib/views.svelte.ts`), and the editor, and TipTap under it, with it.
  import { app } from '../lib/state.svelte'
  import { details, phone } from '../lib/phone.svelte'
  import EntryList from './EntryList.svelte'
  import Editor from './Editor.svelte'

  // The list and the newest page are read when the journal is first shown
  // after an unlock, not at the unlock: a window that opens on another app
  // has no use for them yet. Once only; see `ensureEntries`.
  void app.ensureEntries()
</script>

{#if phone.on}
  {#if details.isOpen('journal')}
    <main class="main"><Editor /></main>
  {:else}
    <EntryList />
  {/if}
{:else}
  <EntryList />
  <main class="main"><Editor /></main>
{/if}

<style>
  .main {
    /* As tall as the stage by a stated height, not stretched: see
       `.shell-stage` in app.css. */
    height: 100%;
    flex: 1;
    min-width: 0;
  }
</style>
