<script lang="ts">
  // A file, as a small card: a thumbnail, its name and what it is. What the
  // other apps draw where a file is attached to something -- a message in the
  // assistant's thread, the composer's tray, a journal entry -- so a file looks
  // the same wherever it turns up.
  //
  // It opens nothing itself. Whoever draws it says what a click does
  // (`onopen`) and whether it can be taken off (`onremove`), because what
  // opening means is the host's: the Files viewer in one place, a preview
  // sheet in another.

  import { formatSize } from '../lib/files'
  import type { FileEntry, FileKind } from '../lib/types'
  import FileThumb from './FileThumb.svelte'
  import Icon from './Icon.svelte'

  interface Props {
    file: FileEntry
    /** One short line, for a tray of them. */
    compact?: boolean
    onopen?: (file: FileEntry) => void
    onremove?: (file: FileEntry) => void
  }
  let { file, compact = false, onopen, onremove }: Props = $props()

  // Kept here rather than imported from the Files app's own helpers, which
  // reach for half the application's stores: every app that shows a file
  // draws this card.
  const KIND_LABELS: Record<FileKind, string> = {
    image: 'Picture',
    video: 'Video',
    audio: 'Audio',
    document: 'Document',
    other: 'File',
  }

  const missing = $derived(file.status.state === 'missing')
  const trashed = $derived(file.status.state === 'trashed')
  const facts = $derived(
    [
      KIND_LABELS[file.kind],
      formatSize(file.size),
      missing ? 'Missing' : trashed ? 'In the trash' : '',
    ]
      .filter(Boolean)
      .join(' · '),
  )
</script>

<div class="card" class:compact class:dim={missing || trashed}>
  <svelte:element
    this={onopen ? 'button' : 'div'}
    class="body"
    class:pressable={!!onopen}
    type={onopen ? 'button' : undefined}
    title={file.name}
    onclick={onopen ? () => onopen?.(file) : undefined}
  >
    <span class="pic"><FileThumb {file} size={compact ? 28 : 44} /></span>
    <span class="text">
      <span class="name">{file.name}</span>
      {#if !compact}<span class="facts">{facts}</span>{/if}
    </span>
  </svelte:element>
  {#if onremove}
    <button
      class="remove"
      aria-label="Remove {file.name}"
      title="Remove"
      onclick={() => onremove?.(file)}
    >
      <Icon name="close" size={13} />
    </button>
  {/if}
</div>

<style>
  .card {
    display: flex;
    align-items: center;
    min-width: 0;
    max-width: 100%;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
  }
  .card.dim {
    opacity: 0.7;
  }
  .body {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-2);
    border-radius: var(--radius);
    color: inherit;
    text-align: left;
  }
  .compact .body {
    gap: var(--sp-2);
    padding: 3px var(--sp-2) 3px 3px;
  }
  .body.pressable:hover {
    background: var(--bg-hover);
  }
  .pic {
    flex: none;
    display: flex;
    border-radius: var(--radius-sm);
    overflow: hidden;
  }
  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    font-weight: 560;
    color: var(--fg);
  }
  .facts {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .remove {
    flex: none;
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    margin-right: var(--sp-1);
    border-radius: 50%;
    color: var(--fg-faint);
  }
  .remove:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
</style>
