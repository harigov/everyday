<script lang="ts">
  import type { Entry, Purpose } from '../lib/types'
  import { app } from '../lib/state.svelte'
  import Icon from './Icon.svelte'
  import PurposeField from './PurposeField.svelte'
  import TagEditor from './TagEditor.svelte'

  let { entry }: { entry: Entry } = $props()
</script>

<div class="meta">
  {#if entry.location?.placeName || entry.location?.locality}
    <span class="place">
      <Icon name="place" size={13} weight={1.6} />
      {entry.location.placeName ?? entry.location.locality}
    </span>
  {/if}

  <TagEditor
    tags={entry.tags}
    onchange={(next: string[]) => {
      entry.tags = next
      app.scheduleSave()
    }}
    id="entry-tag-options"
  />
</div>

{#if app.supportsOverview}
  <!-- The same field, worded the same, as a note's: what this day was for.
       Written through the store's own setter, which saves at once and keeps
       the list behind the page in step. -->
  <div class="purpose-row">
    <span class="purpose-label">Goal or area</span>
    <PurposeField
      value={entry.purpose}
      onchange={(next: Purpose | null) => void app.setEntryPurpose(entry.id, next)}
    />
  </div>
{/if}

<style>
  .meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    margin-top: var(--sp-3);
  }

  .place {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }

  .purpose-row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    margin-top: var(--sp-3);
  }
  .purpose-label {
    color: var(--fg-subtle);
    font-size: var(--text-xs);
    font-weight: 550;
  }

  /* ── On a phone ──────────────────────────────────────────────────────── */

  :global([data-phone]) .meta {
    gap: var(--sp-2);
  }
  :global([data-phone]) .place {
    min-height: 34px;
    font-size: var(--text-base);
  }
</style>
