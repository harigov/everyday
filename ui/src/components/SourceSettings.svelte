<script lang="ts">
  // What can be changed about one source: its name and folder, whether its
  // pictures load, what is done by itself with each new story, how long its
  // stories are kept, the words that drop a story, and whether it is being
  // fetched at all.
  //
  // Everything here is the source's own and nothing is applied until Save, the
  // way a calendar's or an account's is not -- except that those are switches
  // with one effect each, and this is a form with a dozen. The record saved is
  // the one the store holds *now* with these fields laid over it, not the copy
  // this dialog opened with: a refresh can have given the source its icon
  // while the dialog was open, and writing the old copy back would take it
  // away again.
  //
  // A podcast has a section more. Each of its choices is either "use the
  // Feed's default" -- saved as no choice at all, so the show goes on
  // following the defaults if they change -- or a choice of its own, and the
  // default is named in the option so nobody has to go to Settings → Feed to
  // learn what "default" is. A show is listed apart from the folders, so it is
  // not asked which folder it is in, and the article fetching a feed of
  // links needs means nothing for a recording.
  //
  // How long stories are kept is the same kind of choice, and so is whether a
  // source summarises what it brings: its kind's default, shown, or its own. A
  // newsletter that was found in Mail says so, quietly, and can be sent away
  // as "not a newsletter"; one from a sender may also say that an issue's
  // email goes to Trash when the issue is cleaned up, which nothing but its own
  // setting can do. Any newsletter can be unsubscribed from here, its sender
  // asked to take you off the list -- the same question, in the same words, as
  // the sidebar asks of one source or of many.

  import { onMount, untrack } from 'svelte'
  import {
    automationForm,
    automationFromForm,
    defaultRetention,
    isMailSender,
    retentionForm,
    retentionFromForm,
    retentionLabel,
    type AutomationForm,
    type RetentionForm,
  } from '../lib/automation'
  import { feed } from '../lib/feed.svelte'
  import {
    MAX_RETENTION_DAYS,
    folderNames,
    muteText,
    notNewsletterChoices,
    notNewslettersDetail,
    notNewslettersTitle,
    parseMute,
    sourceLabel,
    unfollowDetail,
    unsubscribeDetail,
    unsubscribeTitle,
  } from '../lib/feed'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import { closesOnBack } from '../lib/phone.svelte'
  import { PLAYER_SPEEDS } from '../lib/player.svelte'
  import {
    downloadPolicyLabel,
    keepPolicyLabel,
    podcastForm,
    podcastFromForm,
    speedLabel,
    transcribePolicyLabel,
    type PodcastForm,
  } from '../lib/podcast'
  import { upnext } from '../lib/upnext.svelte'
  import type { MailCategory, NewsSource } from '../lib/types'
  import ChoiceDialog from './ChoiceDialog.svelte'
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
  let trashMail = $state(original.trashMail === true)

  let saving = $state(false)
  /** Which question is over the dialog, if one is: each takes the source away. */
  let asking = $state<'unfollow' | 'unsubscribe' | 'notNewsletter' | null>(null)

  const kind = original.kind
  const isWeb = kind.type === 'web'
  const isMail = kind.type === 'mail' || kind.type === 'newsletters'
  /** One sender's mail, followed or found: what an issue's email can be binned for. */
  const fromSender = isMailSender(original)
  const found = original.found === true

  // ── A podcast's own say ────────────────────────────────────────────────

  const show = original.podcast ?? null
  const isShow = show !== null
  let form = $state<PodcastForm>(podcastForm(show ?? { author: '' }))

  // The defaults the options name. Loaded if the Feed's settings have not
  // been yet; until they are, a podcast's option says only that it is the
  // default and the clean-up's names the stock numbers.
  onMount(() => {
    if (!upnext.settings) void upnext.loadSettings()
  })
  const defaults = $derived(upnext.settings?.podcasts ?? null)
  const useDefault = (label: string | null) =>
    label ? `Use the Feed's default (${label})` : "Use the Feed's default"

  // ── What is done with each new story ───────────────────────────────────

  let auto = $state<AutomationForm>(automationForm(original))

  // ── How long stories are kept ──────────────────────────────────────────

  const cleanup = $derived(upnext.settings?.cleanup ?? null)
  /** What the source keeps its stories for when it has not said: its kind's default. */
  const keepDefault = $derived(defaultRetention(original, cleanup))
  let keep = $state<RetentionForm>(untrack(() => retentionForm(original, cleanup)))
  /** The numbers were typed in: they are somebody's, and a default arriving later leaves them. */
  let keepEdited = false
  /** What the select shows, which is a word where the form keeps a flag. */
  const keepChoice = () => (keep.custom ? 'custom' : 'default')
  const chooseKeep = (choice: string) => (keep.custom = choice === 'custom')

  // A source with no retention of its own opens with its kind's default in the
  // boxes, ready to edit; the settings may arrive after the dialog does.
  $effect(() => {
    const fallback = keepDefault
    if (original.retention || keepEdited) return
    keep.readDays = fallback.readDays
    keep.unreadDays = fallback.unreadDays
  })

  /** Where it comes from, in words, so the dialog says what it is about. */
  const following =
    kind.type === 'web'
      ? [show?.author.trim(), kind.url].filter(Boolean).join(' · ')
      : kind.type === 'mail'
        ? `${kind.name?.trim() || 'Newsletters'} from ${kind.sender}, read in Mail`
        : kind.type === 'newsletters'
          ? 'Every newsletter in Mail, but for senders you follow on their own'
          : kind.minPoints > 0
            ? `Hacker News, above ${kind.minPoints} points`
            : 'Hacker News'

  const folders = $derived(folderNames(feed.sources))

  async function save() {
    if (saving) return
    saving = true
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
      retention: retentionFromForm(keep, original, cleanup),
      automation: automationFromForm(original, auto),
      trashMail: fromSender ? trashMail : false,
      paused,
    }
    await feed.saveSource(next)
    // After the source, and only if it changed: the show's settings are their
    // own write, over what the store holds now.
    const stored = current.podcast ?? show
    if (stored) {
      const wanted = podcastFromForm(stored, form)
      const before = podcastFromForm(stored, podcastForm(stored))
      if (JSON.stringify(wanted) !== JSON.stringify(before)) {
        await feed.savePodcastSettings(original.id, wanted)
      }
    }
    saving = false
    onclose()
  }

  async function unfollow() {
    asking = null
    await feed.deleteSource(original.id)
    onclose()
  }

  // These two close at once rather than waiting: the source leaves the screen
  // as soon as it is asked for, and asking a sender can take a while. How it
  // went is the store's to say, after this dialog has gone.
  function unsubscribe() {
    asking = null
    onclose()
    void feed.unsubscribeSources([original.id])
  }

  function notNewsletter(category: MailCategory | null) {
    asking = null
    onclose()
    void feed.markNotNewsletters([original.id], category)
  }

  // A phone's back button is its Escape.
  closesOnBack(
    () => true,
    () => onclose(),
  )

  /**
   * Draw this over the window rather than where it is declared. It is raised
   * from the Feed's sidebar, which on a phone is a drawer, and a drawer that
   * has been moved (`translate`, even to nowhere) is the box that `position:
   * fixed` inside it is measured against -- the sheet came out as wide as the
   * screen, laid from the drawer's left edge, cut off by the screen's.
   *
   * On the desktop too: the sidebar can be that box there as well -- folded
   * and peeking out, it is moved with `translate` just the same -- and the
   * phone check's desktop pass found the sheet centred on the sidebar, a
   * third of it off the window's left edge, in Chromium and WebKit alike.
   */
  function portal(node: HTMLElement) {
    document.body.append(node)
    return {
      destroy() {
        node.remove()
      },
    }
  }
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape' && asking === null) onclose()
  }}
/>

<div class="portal" use:portal>
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div class="scrim phone-fit" onclick={onclose}></div>
  <div
    class="sheet wide phone-fit"
    role="dialog"
    aria-modal="true"
    aria-label="Settings for {sourceLabel(original)}"
    use:trapFocus
  >
    <h2>{sourceLabel(original)}</h2>
    <div class="following">
      <p class="hint line">{following}</p>
      {#if found}
        <p class="hint line">Found among your newsletters.</p>
      {/if}
    </div>

    <label class="field-label" for="src-title">Name</label>
    <input
      id="src-title"
      class="field"
      placeholder={sourceLabel({ ...original, title: '' })}
      bind:value={title}
      use:focusOnMount
    />

    {#if !isShow}
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
    {/if}

    <div class="switches">
      <label class="check-row">
        <input type="checkbox" bind:checked={images} />
        <span>
          <b>Load pictures</b>
          <small>
            Fetched when a story is opened and kept on this device. The site that published them
            sees the request.
          </small>
        </span>
      </label>

      {#if isWeb && !isShow}
        <label class="check-row">
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
        <label class="check-row">
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

      <label class="check-row">
        <input type="checkbox" bind:checked={paused} />
        <span>
          <b>Pause</b>
          <small>A paused source is not refreshed. What it already fetched stays readable.</small>
        </span>
      </label>
    </div>

    <hr class="sep spaced" />
    <h3 class="eyebrow section">Automation</h3>

    <div class="switches under">
      <label class="check-row">
        <input type="checkbox" bind:checked={auto.summarize} />
        <span>
          <b>{isShow ? 'Write an article from each transcript' : 'Summarise new stories'}</b>
          <small>
            {#if isShow}
              Once an episode has a transcript, an article is written from it and kept in Files: the
              Summary tab. A show does this unless it is turned off.
            {:else}
              Each new story is summarised as it arrives, and the summary is kept in Files.
              Otherwise the Summary tab makes it when you ask.
            {/if}
          </small>
        </span>
      </label>

      {#if !isShow}
        <label class="check-row">
          <input type="checkbox" bind:checked={auto.audio} />
          <span>
            <b>Make each new article into audio</b>
            <small>
              Each new article is recorded and kept in Files, so it plays from Up Next without
              asking.
            </small>
          </span>
        </label>
      {/if}
    </div>

    {#if isShow}
      <hr class="sep spaced" />
      <h3 class="eyebrow section">Podcast</h3>

      <label class="field-label" for="pod-download">Download episodes</label>
      <div class="choice">
        <select id="pod-download" class="field" bind:value={form.download}>
          <option value="default">
            {useDefault(defaults ? downloadPolicyLabel(defaults.download) : null)}
          </option>
          <option value="off">Off</option>
          <option value="newest">The newest</option>
          <option value="all">Every episode</option>
        </select>
        {#if form.download === 'newest'}
          <input
            class="field days"
            type="number"
            min="1"
            max="999"
            aria-label="How many of the newest episodes to download"
            bind:value={form.downloadCount}
          />
          <span class="hint">episodes</span>
        {/if}
      </div>
      <p class="hint under">An episode is fetched into your files folder, and plays from there.</p>

      <label class="field-label spaced" for="pod-keep">Keep downloaded episodes</label>
      <div class="choice">
        <select id="pod-keep" class="field" bind:value={form.keep}>
          <option value="default">
            {useDefault(defaults ? keepPolicyLabel(defaults.keep) : null)}
          </option>
          <option value="forever">Forever</option>
          <option value="afterPlayed">For a while after they are played</option>
          <option value="newest">Only the newest</option>
        </select>
        {#if form.keep === 'afterPlayed'}
          <input
            class="field days"
            type="number"
            min="1"
            max="365"
            aria-label="Days to keep an episode after it is played to the end"
            bind:value={form.keepNumber}
          />
          <span class="hint">days</span>
        {:else if form.keep === 'newest'}
          <input
            class="field days"
            type="number"
            min="1"
            max="999"
            aria-label="How many of the newest episodes to keep"
            bind:value={form.keepNumber}
          />
          <span class="hint">episodes</span>
        {/if}
      </div>
      <p class="hint under">
        Favourites, saved stories and what is in Up Next are never removed. Transcripts are kept.
      </p>

      <label class="field-label spaced" for="pod-transcribe">Transcribe</label>
      <select id="pod-transcribe" class="field" bind:value={form.transcribe}>
        <option value="default">
          {useDefault(defaults ? transcribePolicyLabel(defaults.transcribe) : null)}
        </option>
        <option value="off">{transcribePolicyLabel('off')}</option>
        <option value="whenDownloaded">{transcribePolicyLabel('whenDownloaded')}</option>
        <option value="whenPlayed">{transcribePolicyLabel('whenPlayed')}</option>
      </select>

      <label class="field-label spaced" for="pod-queue">Put new episodes in Up Next</label>
      <select id="pod-queue" class="field" bind:value={form.queueNew}>
        <option value="default">
          {useDefault(defaults ? (defaults.queueNew ? 'Yes' : 'No') : null)}
        </option>
        <option value="yes">Yes</option>
        <option value="no">No</option>
      </select>

      <label class="field-label spaced" for="pod-speed">Speed</label>
      <select id="pod-speed" class="field" bind:value={form.speed}>
        <option value={null}>The player's own</option>
        {#each PLAYER_SPEEDS as speed (speed)}
          <option value={speed}>{speedLabel(speed)}</option>
        {/each}
      </select>

      <label class="field-label spaced" for="pod-skip">Skip the intro</label>
      <div class="choice">
        <input
          id="pod-skip"
          class="field days"
          type="number"
          min="0"
          max="600"
          bind:value={form.skipIntroS}
        />
        <span class="hint">seconds, in an episode not started yet. 0 plays from the start.</span>
      </div>
    {/if}

    <hr class="sep spaced" />
    <h3 class="eyebrow section">Clean-up</h3>

    <label class="field-label" for="src-clean">Clean up after</label>
    <div class="choice">
      <select id="src-clean" class="field" bind:value={keepChoice, chooseKeep}>
        <option value="default">Use the default ({retentionLabel(keepDefault)})</option>
        <option value="custom">Choose for this source</option>
      </select>
      {#if keep.custom}
        <input
          class="field days"
          type="number"
          min="1"
          max={MAX_RETENTION_DAYS}
          aria-label="Days to keep a story after it is read"
          bind:value={keep.readDays}
          oninput={() => (keepEdited = true)}
        />
        <span class="hint">days once read, and</span>
        <input
          class="field days"
          type="number"
          min="1"
          max={MAX_RETENTION_DAYS}
          aria-label="Days to keep a story that was never read"
          bind:value={keep.unreadDays}
          oninput={() => (keepEdited = true)}
        />
        <span class="hint">days if not.</span>
      {/if}
    </div>
    <p class="hint under">
      {#if fromSender}
        An old issue is removed from the Feed; its email stays in Mail unless you say otherwise
        below.
      {:else if isMail}
        An old issue is removed from the Feed; its email stays in Mail.
      {:else}
        An old story is removed from the Feed.
      {/if}
      Saved stories are kept for good.
    </p>

    {#if fromSender}
      <div class="switches">
        <label class="check-row">
          <input type="checkbox" bind:checked={trashMail} />
          <span>
            <b>Also move each issue’s email to Trash when it is cleaned up</b>
            <small>
              Starred ones stay. The email goes to Trash in Mail like any other you delete, where it
              can be got back.
            </small>
          </span>
        </label>
      </div>
    {/if}

    <hr class="sep spaced" />

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

    <div class="sheet-row ends">
      {#if fromSender}
        <button class="btn btn-ghost-danger" onclick={() => (asking = 'unsubscribe')}>
          Unsubscribe…
        </button>
      {/if}
      <button
        class="btn btn-ghost-danger"
        onclick={() => (asking = found ? 'notNewsletter' : 'unfollow')}
      >
        {found ? 'Not a newsletter…' : 'Unfollow…'}
      </button>
      <span class="spacer"></span>
      <button class="btn" onclick={onclose}>Cancel</button>
      <button class="btn btn-primary" disabled={saving} onclick={() => void save()}>Save</button>
    </div>
  </div>

  {#if asking === 'unfollow'}
    <ConfirmDialog
      title={'Unfollow “' + sourceLabel(original) + '”?'}
      detail={unfollowDetail(original)}
      confirmLabel="Unfollow"
      onconfirm={() => void unfollow()}
      oncancel={() => (asking = null)}
    />
  {:else if asking === 'unsubscribe'}
    <ConfirmDialog
      title={unsubscribeTitle([original])}
      detail={unsubscribeDetail([original])}
      confirmLabel="Unsubscribe"
      danger={false}
      onconfirm={unsubscribe}
      oncancel={() => (asking = null)}
    />
  {:else if asking === 'notNewsletter'}
    <ChoiceDialog
      title={notNewslettersTitle([sourceLabel(original)])}
      detail={notNewslettersDetail(1)}
      options={notNewsletterChoices(1).map((choice) => ({
        label: choice.label,
        run: () => notNewsletter(choice.category),
      }))}
      oncancel={() => (asking = null)}
    />
  {/if}
</div>

<style>
  .portal {
    display: contents;
  }
  /* A newsletter's two ways out beside Cancel and Save can be more than a
     narrow window's width: the row wraps rather than squeezing them. */
  .ends {
    flex-wrap: wrap;
  }
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
  }
  .line {
    margin: 0;
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
  /* Under a section's heading, which has the gap already. */
  .switches.under {
    margin-top: 0;
  }
  .choice {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
  }
  /* The select takes what the number beside it leaves. */
  .choice > select {
    flex: 1;
    min-width: 200px;
    width: auto;
  }
  .section {
    margin: var(--sp-3) 0 var(--sp-3);
  }
  .days {
    width: 76px;
  }

  /* A phone: a tick to hit, and fields at the size that keeps iOS from
     zooming into them. */
  :global([data-phone]) .check-row input {
    width: 24px;
    height: 24px;
  }
  :global([data-phone]) .sheet input:not([type='checkbox']),
  :global([data-phone]) .sheet select {
    font-size: max(16px, 1em);
  }
</style>
