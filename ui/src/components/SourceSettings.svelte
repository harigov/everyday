<script lang="ts">
  // What can be changed about one source: its name and folder, whether its
  // pictures load, how long its stories are kept, the words that drop a story,
  // and whether it is being fetched at all.
  //
  // Everything here is the source's own and nothing is applied until Save, the
  // way a calendar's or an account's is not -- except that those are switches
  // with one effect each, and this is a form with nine. The record saved is
  // the one the store holds *now* with these fields laid over it, not the copy
  // this dialog opened with: a refresh can have given the source its icon
  // while the dialog was open, and writing the old copy back would take it
  // away again.

  import { untrack } from 'svelte'
  import { feed } from '../lib/feed.svelte'
  import {
    DEFAULT_RETENTION,
    MAX_RETENTION_DAYS,
    clampDays,
    folderNames,
    muteText,
    parseMute,
    sourceLabel,
    unfollowDetail,
  } from '../lib/feed'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import type { NewsSource } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'

  let { source, onclose }: { source: NewsSource; onclose: () => void } = $props()

  // Read once: this dialog is mounted for one source and unmounted when it
  // closes, so what it opened with is what it edits.
  const original: NewsSource = untrack(() => $state.snapshot(source))

  let title = $state(original.title)
  let folder = $state(original.folder)
  let images = $state(original.images)
  let fullText = $state(original.fullText)
  let keepOutOfInbox = $state(original.keepOutOfInbox)
  let paused = $state(original.paused)
  let mute = $state(muteText(original.mute))
  let readDays = $state<number | null>(original.retention?.readDays ?? DEFAULT_RETENTION.readDays)
  let unreadDays = $state<number | null>(
    original.retention?.unreadDays ?? DEFAULT_RETENTION.unreadDays,
  )

  let saving = $state(false)
  let confirming = $state(false)

  const kind = original.kind
  const isWeb = kind.type === 'web'
  const isMail = kind.type === 'mail'

  /** Where it comes from, in words, so the dialog says what it is about. */
  const following =
    kind.type === 'web'
      ? kind.url
      : kind.type === 'mail'
        ? `Newsletters from ${kind.sender}, read in Mail`
        : kind.minPoints > 0
          ? `Hacker News, above ${kind.minPoints} points`
          : 'Hacker News'

  const folders = $derived(folderNames(feed.sources))

  async function save() {
    if (saving) return
    saving = true
    const read = clampDays(readDays, DEFAULT_RETENTION.readDays)
    const unread = clampDays(unreadDays, DEFAULT_RETENTION.unreadDays)
    // The store's copy, if the source is still there, so nothing a refresh
    // wrote in the meantime is lost.
    const current = feed.sources.find((s) => s.source.id === original.id)?.source ?? original
    const base: NewsSource = $state.snapshot(current)
    const next: NewsSource = {
      ...base,
      title: title.trim(),
      folder: folder.trim(),
      images,
      fullText: isWeb ? fullText : false,
      keepOutOfInbox: isMail ? keepOutOfInbox : false,
      mute: parseMute(mute),
      // The defaults are stored as no choice at all, so a source nobody has
      // set anything on keeps following them if they ever change.
      retention:
        read === DEFAULT_RETENTION.readDays && unread === DEFAULT_RETENTION.unreadDays
          ? null
          : { readDays: read, unreadDays: unread },
      paused,
    }
    await feed.saveSource(next)
    saving = false
    onclose()
  }

  async function unfollow() {
    confirming = false
    await feed.deleteSource(original.id)
    onclose()
  }
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape' && !confirming) onclose()
  }}
/>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="scrim" onclick={onclose}></div>
<div
  class="sheet wide"
  role="dialog"
  aria-modal="true"
  aria-label="Settings for {sourceLabel(original)}"
  use:trapFocus
>
  <h2>{sourceLabel(original)}</h2>
  <p class="hint following">{following}</p>

  <label class="field-label" for="src-title">Name</label>
  <input
    id="src-title"
    class="field"
    placeholder={sourceLabel({ ...original, title: '' })}
    bind:value={title}
    use:focusOnMount
  />

  <label class="field-label spaced" for="src-folder">Folder</label>
  <input
    id="src-folder"
    class="field"
    list="src-folders"
    placeholder="None"
    autocomplete="off"
    bind:value={folder}
  />
  <datalist id="src-folders">
    {#each folders as name (name)}
      <option value={name}></option>
    {/each}
  </datalist>

  <div class="switches">
    <label class="toggle">
      <input type="checkbox" bind:checked={images} />
      <span>
        <b>Load pictures</b>
        <small>
          Fetched when a story is opened and kept on this device. The site that published them sees
          the request.
        </small>
      </span>
    </label>

    {#if isWeb}
      <label class="toggle">
        <input type="checkbox" bind:checked={fullText} />
        <span>
          <b>Always fetch the full article</b>
          <small>
            For feeds that carry only a few lines. Each new story's page is fetched and read for
            you; otherwise the button on a story does it when you ask.
          </small>
        </span>
      </label>
    {/if}

    {#if isMail}
      <label class="toggle">
        <input type="checkbox" bind:checked={keepOutOfInbox} />
        <span>
          <b>Keep these out of the inbox</b>
          <small>
            New issues are archived in your mail as they arrive, so they live here and not in your
            inbox. Your mail server sees that move.
          </small>
        </span>
      </label>
    {/if}

    <label class="toggle">
      <input type="checkbox" bind:checked={paused} />
      <span>
        <b>Pause</b>
        <small>A paused source is not refreshed. What it already fetched stays readable.</small>
      </span>
    </label>
  </div>

  <span class="field-label spaced">Keep for</span>
  <div class="keep">
    <input
      class="field days"
      type="number"
      min="1"
      max={MAX_RETENTION_DAYS}
      aria-label="Days to keep a story after it is read"
      bind:value={readDays}
    />
    <span class="hint">days once read, and</span>
    <input
      class="field days"
      type="number"
      min="1"
      max={MAX_RETENTION_DAYS}
      aria-label="Days to keep a story that was never read"
      bind:value={unreadDays}
    />
    <span class="hint">days if not. Saved stories are kept for good.</span>
  </div>

  <label class="field-label spaced" for="src-mute">Mute words</label>
  <input
    id="src-mute"
    class="field"
    placeholder="sponsored, giveaway"
    spellcheck="false"
    bind:value={mute}
  />
  <p class="hint under">
    Separated by commas. A story with one of them in its title is dropped as it arrives.
  </p>

  <div class="sheet-row">
    <button class="btn btn-ghost-danger" onclick={() => (confirming = true)}>Unfollow…</button>
    <span class="spacer"></span>
    <button class="btn" onclick={onclose}>Cancel</button>
    <button class="btn btn-primary" disabled={saving} onclick={() => void save()}>Save</button>
  </div>
</div>

{#if confirming}
  <ConfirmDialog
    title={'Unfollow “' + sourceLabel(original) + '”?'}
    detail={unfollowDetail(original)}
    confirmLabel="Unfollow"
    onconfirm={() => void unfollow()}
    oncancel={() => (confirming = false)}
  />
{/if}

<style>
  /* Centred rather than hung from the shared sheet's 22%: nine fields would
     otherwise run off the bottom of the window before the max-height made
     them scroll. */
  .wide {
    top: 50%;
    translate: -50% -50%;
    width: min(520px, calc(100vw - var(--sp-8)));
    max-height: calc(100vh - var(--sp-8));
    overflow-y: auto;
  }

  h2 {
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
  }
  .following {
    margin: 2px 0 var(--sp-4);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    user-select: text;
  }

  .spaced {
    margin-top: var(--sp-4);
  }
  .under {
    margin-top: var(--sp-2);
    font-size: var(--text-xs);
  }

  .switches {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    margin-top: var(--sp-4);
  }
  .toggle {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    cursor: pointer;
  }
  .toggle input {
    flex: none;
    margin-top: 3px;
    accent-color: var(--accent);
  }
  .toggle span {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .toggle b {
    font-size: var(--text-base);
    font-weight: 600;
  }
  .toggle small {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    line-height: var(--leading-normal);
  }

  .keep {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
  }
  .days {
    width: 76px;
  }
</style>
