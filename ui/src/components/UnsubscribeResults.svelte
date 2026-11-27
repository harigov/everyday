<script lang="ts">
  // What is left after unsubscribing from several senders: the ones who only
  // unsubscribe on a web page, each with a button that opens it in the
  // browser (never fetched here: a page is for the person to finish), and
  // the ones it could not reach, with why. Senders already asked are only
  // counted -- the toast said who.
  //
  // Shown by whoever unsubscribed (Mail's list and thread, Quick Cleanup,
  // the Feed) when `needsAttention(outcomes)`; taken down with `onclose`.

  import { focusOnMount } from '../lib/focus'
  import { openExternal } from '../lib/open-external'
  import { plural } from '../lib/format'
  import { sortOutcomes } from '../lib/unsubscribe'
  import type { UnsubscribeOutcome } from '../lib/types'
  import Sheet from './Sheet.svelte'

  let {
    outcomes,
    onclose,
  }: {
    outcomes: readonly UnsubscribeOutcome[]
    onclose: () => void
  } = $props()

  const sorted = $derived(sortOutcomes(outcomes))
  /** Pages opened, and pages the browser would not take (shown to copy). */
  let opened = $state<ReadonlySet<number>>(new Set())
  let refused = $state<ReadonlySet<number>>(new Set())

  /** The service's reasons are written to follow a colon ("their mail
   *  names no way to unsubscribe"); on a line of their own they start with
   *  a capital and end with a stop. */
  function sentence(reason: string | null | undefined): string {
    const r = (reason ?? '').trim()
    if (!r) return ''
    return r.charAt(0).toUpperCase() + r.slice(1) + (/[.!?]$/.test(r) ? '' : '.')
  }

  // Rows are told apart by place, not by who: in the Feed two newsletters
  // from one address can share a title too (one found, one followed by
  // hand), and a keyed list with two equal keys does not draw at all.
  async function open(o: UnsubscribeOutcome, at: number) {
    if (!o.url) return
    const ok = await openExternal(o.url)
    if (ok) opened = new Set([...opened, at])
    else refused = new Set([...refused, at])
  }

  const title = $derived(
    sorted.pages.length
      ? `${plural(sorted.pages.length, 'sender unsubscribes', 'senders unsubscribe')} on their website`
      : `Couldn't unsubscribe from ${plural(sorted.failed.length, 'sender')}`,
  )
</script>

<Sheet label={title} role="dialog" fit onclose={() => onclose()}>
  <h2 class="sheet-title">{title}</h2>
  {#if sorted.done.length}
    <p class="hint">
      {plural(sorted.done.length, 'sender')} asked to take you off their list.
    </p>
  {/if}

  {#if sorted.pages.length}
    <p class="hint">Open each page and finish there. Nothing is sent until you do.</p>
    <ul class="results" aria-label="Senders with an unsubscribe page">
      {#each sorted.pages as o, i (i)}
        <li>
          <span class="who">
            <b>{o.name || o.email}</b>
            {#if o.name && o.name !== o.email}<small>{o.email}</small>{/if}
            {#if refused.has(i) && o.url}
              <small class="copy"
                >Your browser would not open it. Copy the link:
                <input
                  class="field"
                  readonly
                  value={o.url}
                  onfocus={(e) => e.currentTarget.select()}
                />
              </small>
            {/if}
          </span>
          <button
            class="btn btn-sm"
            class:btn-outline={!opened.has(i)}
            onclick={() => void open(o, i)}>{opened.has(i) ? 'Opened' : 'Open page'}</button
          >
        </li>
      {/each}
    </ul>
  {/if}

  {#if sorted.failed.length}
    <ul class="results" aria-label="Senders it could not reach">
      {#each sorted.failed as o, i (i)}
        <li>
          <span class="who">
            <b>{o.name || o.email}</b>
            <small>{sentence(o.error) || 'Their mail names no way to unsubscribe.'}</small>
          </span>
        </li>
      {/each}
    </ul>
  {/if}

  <div class="sheet-row">
    <span class="spacer"></span>
    <button class="btn" use:focusOnMount onclick={() => onclose()}>Done</button>
  </div>
</Sheet>

<style>
  .sheet-title {
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
  }
  .sheet-title + .hint,
  .hint + .hint {
    margin-top: var(--sp-2);
  }
  .results {
    list-style: none;
    margin: var(--sp-3) 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    max-height: min(50vh, 420px);
    overflow-y: auto;
  }
  .results li {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }
  .who {
    display: flex;
    flex-direction: column;
    min-width: 0;
    flex: 1;
  }
  .who b {
    font-weight: 560;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .who small {
    color: var(--fg-muted);
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
  }
  .copy .field {
    margin-top: var(--sp-1);
    width: 100%;
    font-size: var(--text-xs);
  }
  .results .btn {
    flex: none;
  }
</style>
