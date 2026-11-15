<script lang="ts">
  // Files on their way in, one row each, in the sidebar: the name, how far it
  // has got, and a way to stop it. A finished row stays until the person
  // clears them, so a failure is read rather than missed.
  //
  // Progress is drawn as a thin bar and written as a number. A large file
  // goes up in 4 MB chunks and a bar that only moves between chunks would
  // look stuck, which is why the sent size is shown beside it.

  import { formatSize } from '../lib/files'
  import { files } from '../lib/files.svelte'
  import Icon from './Icon.svelte'

  const rows = $derived(files.uploads)
  const finished = $derived(rows.filter((u) => u.state !== 'sending'))
  const sending = $derived(rows.length - finished.length)

  function fraction(sent: number, size: number): number {
    return size > 0 ? Math.min(1, sent / size) : 1
  }
</script>

{#if rows.length > 0}
  <section class="uploads" aria-label="Uploads">
    <div class="side-head">
      <span class="eyebrow">{sending > 0 ? `Adding ${sending}` : 'Added'}</span>
      {#if finished.length > 0}
        <button class="clear" onclick={() => files.clearUploads()}>Clear</button>
      {/if}
    </div>
    <ul>
      {#each rows as u (u.key)}
        <li class:failed={u.state === 'failed'} class:over={u.state === 'cancelled'}>
          <div class="line">
            <span class="name" title={u.name}>{u.name}</span>
            {#if u.state === 'sending'}
              <button
                class="stop"
                aria-label="Cancel {u.name}"
                title="Cancel"
                onclick={() => files.cancelUpload(u.key)}
              >
                <Icon name="close" size={12} />
              </button>
            {:else if u.state === 'done'}
              <span class="state good"><Icon name="tick" size={13} /></span>
            {/if}
          </div>
          {#if u.state === 'sending'}
            <div
              class="bar"
              role="progressbar"
              aria-label="Adding {u.name}"
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={Math.round(fraction(u.sent, u.size) * 100)}
            >
              <span class="fill" style:width="{fraction(u.sent, u.size) * 100}%"></span>
            </div>
            <span class="sub">{formatSize(u.sent)} of {formatSize(u.size)}</span>
          {:else if u.state === 'failed'}
            <span class="sub">{u.error || 'Could not be added.'}</span>
          {:else if u.state === 'cancelled'}
            <span class="sub">Cancelled</span>
          {:else}
            <span class="sub">{formatSize(u.size)}</span>
          {/if}
        </li>
      {/each}
    </ul>
  </section>
{/if}

<style>
  .uploads {
    flex: none;
    padding: 0 var(--sp-3) var(--sp-2);
    border-top: 1px solid var(--border);
  }
  /* The shared heading pads itself for the sidebar's own gutter; here the
     section already has one. */
  .uploads :global(.side-head) {
    padding-inline: 0;
  }
  .clear {
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--accent);
  }
  .clear:hover {
    text-decoration: underline;
  }
  ul {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    margin: 0;
    padding: 0;
    list-style: none;
    max-height: 200px;
    overflow-y: auto;
  }
  li {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  .line {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }
  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    color: var(--fg);
  }
  .stop {
    flex: none;
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    border-radius: 50%;
    color: var(--fg-faint);
  }
  .stop:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .state.good {
    display: flex;
    color: var(--success);
  }
  .bar {
    height: 3px;
    border-radius: 2px;
    background: var(--bg-sunken);
    overflow: hidden;
  }
  .fill {
    display: block;
    height: 100%;
    border-radius: 2px;
    background: var(--accent);
    transition: width var(--fast) var(--ease);
  }
  .sub {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
    overflow-wrap: anywhere;
  }
  li.failed .sub {
    color: var(--danger);
  }
  li.over .name,
  li.over .sub {
    color: var(--fg-faint);
  }
</style>
