<script lang="ts">
  // Settings → Feed: what the Feed does with the podcasts it follows, how
  // Recommended is ordered, Up Next and the player, what it remembers, and how
  // long it keeps stories. See `docs/plans/podcasts.md` and
  // `docs/plans/feed-usability.md`.
  //
  // One draft for the whole page, saved whole -- the service keeps one sealed
  // row (`save_feed_settings`) -- with Save pinned at the foot of the pane
  // (`SettingsView`'s `.save`, which has to be the last thing on the page).
  // What is chosen here is the Feed's default for every show, feed and
  // newsletter; a source's own page overrides any of it for that source alone.
  //
  // One thing on it is not the draft's: "Allow again" on a sender that was
  // taken out of the Feed as not being a newsletter saves at once, and saves
  // only that, over what is stored. A click on a button beside a name is not
  // a wish to save whatever else is half-edited on the page.
  //
  // Nothing applies until it is saved, and that includes the criteria: each
  // one is a question the decision model is asked about every new story, so
  // editing them ranks everything again, and "Rank again" is held back while
  // there is an unsaved edit rather than asking the old questions.

  import { untrack, tick } from 'svelte'
  import { sameNewsletter } from '../lib/automation'
  import { MAX_RETENTION_DAYS } from '../lib/feed'
  import { panels } from '../lib/panels.svelte'
  import { upnext } from '../lib/upnext.svelte'
  import type {
    Criterion,
    DownloadPolicy,
    FeedSettings,
    KeepPolicy,
    NewsletterRef,
    TranscribePolicy,
  } from '../lib/types'
  import Icon from './Icon.svelte'

  void upnext.loadSettings()
  // For the status line under the criteria: which model ranks, and how far
  // along it is. Read with Recommended, as the service answers them together.
  void upnext.loadRecommended()

  // ── The draft ────────────────────────────────────────────────────────

  let draft = $state<FeedSettings | null>(null)
  /** What the draft was when the edit began, as text: the thing to compare. */
  let kept = $state('')
  const dirty = $derived(draft !== null && JSON.stringify(draft) !== kept)

  function adopt(settings: FeedSettings) {
    const copy = $state.snapshot(settings)
    draft = copy
    kept = JSON.stringify(copy)
  }

  // The store's copy arrives, and arrives again after a save or a change made
  // somewhere else. A draft nobody has touched takes it; one somebody is in
  // the middle of is left alone.
  $effect(() => {
    const settings = upnext.settings
    if (!settings) return
    untrack(() => {
      if (draft !== null && JSON.stringify(draft) !== kept) return
      adopt(settings)
    })
  })

  // ── Choices ──────────────────────────────────────────────────────────

  const DOWNLOADS: { id: DownloadPolicy['type']; label: string }[] = [
    { id: 'off', label: 'Off' },
    { id: 'newest', label: 'Newest' },
    { id: 'all', label: 'Every episode' },
  ]
  const KEEPS: { id: KeepPolicy['type']; label: string }[] = [
    { id: 'forever', label: 'Forever' },
    { id: 'afterPlayed', label: 'After played' },
    { id: 'newest', label: 'Newest' },
  ]
  const TRANSCRIBES: { id: TranscribePolicy; label: string; blurb: string }[] = [
    { id: 'off', label: 'Only when asked', blurb: 'A transcript is made when you ask for one.' },
    {
      id: 'whenDownloaded',
      label: 'When downloaded',
      blurb: 'As soon as an episode is here, so it can be read and searched before you play it.',
    },
    {
      id: 'whenPlayed',
      label: 'When played',
      blurb: 'The first time you start an episode, so what you listen to is what is remembered.',
    },
  ]

  /** The skips the buttons offer, the odd one a person once chose included. */
  const SKIP_BACK = [5, 10, 15, 30, 45, 60]
  const SKIP_FORWARD = [10, 15, 30, 45, 60, 90]
  function skips(choices: number[], now: number): number[] {
    return choices.includes(now) ? choices : [...choices, now].sort((a, b) => a - b)
  }

  /** A whole number inside a range, or `null` for what is not one yet --
   *  an empty box on the way to being typed into. */
  function whole(raw: string, least: number, most: number): number | null {
    if (raw.trim() === '') return null
    const n = Math.round(Number(raw))
    return Number.isFinite(n) ? Math.min(most, Math.max(least, n)) : null
  }

  // ── Podcasts ─────────────────────────────────────────────────────────

  function setDownload(type: DownloadPolicy['type']) {
    if (!draft) return
    const now = draft.podcasts.download
    draft.podcasts.download =
      type === 'newest' ? { type, count: now.type === 'newest' ? now.count : 1 } : { type }
  }
  function setDownloadCount(raw: string) {
    const count = whole(raw, 1, 99)
    if (draft && count !== null && draft.podcasts.download.type === 'newest') {
      draft.podcasts.download = { type: 'newest', count }
    }
  }

  function setKeep(type: KeepPolicy['type']) {
    if (!draft) return
    const now = draft.podcasts.keep
    draft.podcasts.keep =
      type === 'afterPlayed'
        ? { type, days: now.type === 'afterPlayed' ? now.days : 7 }
        : type === 'newest'
          ? { type, count: now.type === 'newest' ? now.count : 5 }
          : { type }
  }
  function setKeepNumber(raw: string) {
    if (!draft) return
    const keep = draft.podcasts.keep
    if (keep.type === 'afterPlayed') {
      // The service keeps the window between a day and sixty.
      const days = whole(raw, 1, 60)
      if (days !== null) draft.podcasts.keep = { type: 'afterPlayed', days }
    } else if (keep.type === 'newest') {
      const count = whole(raw, 1, 99)
      if (count !== null) draft.podcasts.keep = { type: 'newest', count }
    }
  }

  // ── Recommended ──────────────────────────────────────────────────────

  /** What a weight means, in the words somebody would use for it. */
  const WEIGHT_WORDS: Record<number, string> = {
    [-3]: 'Avoid',
    [-2]: 'Rather not',
    [-1]: 'A little less',
    0: 'No effect',
    1: 'A little more',
    2: 'More',
    3: 'Want more',
  }
  function weightWord(weight: number): string {
    return WEIGHT_WORDS[weight] ?? String(weight)
  }

  /** The newest criterion's box, to put the caret in it when it is added. */
  let list = $state<HTMLElement | null>(null)

  async function addCriterion() {
    if (!draft) return
    const added: Criterion = { id: crypto.randomUUID(), text: '', weight: 2 }
    draft.ranking.criteria.push(added)
    await tick()
    const boxes = list?.querySelectorAll<HTMLInputElement>('input[type="text"]')
    boxes?.[boxes.length - 1]?.focus()
  }

  function removeCriterion(id: string) {
    if (draft) draft.ranking.criteria = draft.ranking.criteria.filter((c) => c.id !== id)
  }

  function setWindow(raw: string) {
    const days = whole(raw, 1, 60)
    if (draft && days !== null) draft.ranking.windowDays = days
  }

  const status = $derived(upnext.ranking)
  let reranking = $state(false)

  async function rerank() {
    reranking = true
    try {
      await upnext.rerank()
      await upnext.loadRecommended()
    } finally {
      reranking = false
    }
  }

  // ── Up Next ──────────────────────────────────────────────────────────

  function setAutofill(on: boolean) {
    if (draft) draft.autofill = on ? 5 : null
  }
  function setAutofillCount(raw: string) {
    const count = whole(raw, 1, 50)
    if (draft && count !== null) draft.autofill = count
  }

  // ── Clean-up ─────────────────────────────────────────────────────────

  function setDays(kind: 'feeds' | 'newsletters', field: 'readDays' | 'unreadDays', raw: string) {
    const days = whole(raw, 1, MAX_RETENTION_DAYS)
    if (draft && days !== null) draft.cleanup[kind][field] = days
  }

  /** `settings` without `ref` among the not-newsletters; no list at all when none is left. */
  function without(settings: FeedSettings, ref: NewsletterRef) {
    const left = (settings.notNewsletters ?? []).filter((n) => !sameNewsletter(n, ref))
    if (left.length > 0) settings.notNewsletters = left
    else delete settings.notNewsletters
  }

  let allowing = $state(false)

  async function allowAgain(ref: NewsletterRef) {
    const stored = upnext.settings
    if (!draft || !stored || allowing) return
    allowing = true
    notice = null
    problem = null
    // Only this change is saved, over what is stored: the page's other edits are
    // still the draft's, and Save is still theirs.
    const only: FeedSettings = $state.snapshot(stored)
    without(only, ref)
    const saved = await upnext.saveSettings(only)
    allowing = false
    if (!saved) {
      problem = upnext.error ?? 'The change could not be saved.'
      return
    }
    // The draft and what it is compared with move together, so the page is not
    // made to look edited by what was saved.
    without(draft, ref)
    const base = JSON.parse(kept) as FeedSettings
    without(base, ref)
    kept = JSON.stringify(base)
  }

  // ── Saving ───────────────────────────────────────────────────────────

  let saving = $state(false)
  let notice = $state<string | null>(null)
  let problem = $state<string | null>(null)

  async function save() {
    if (!draft) return
    const plain = $state.snapshot(draft)
    saving = true
    // What is not a newsletter is added to by the service -- an unfollow in a
    // source's own settings while this page is open -- and taken from only by
    // "Allow again", which saves at once; so it goes back as it is stored
    // now, not as it was when the page opened.
    await upnext.loadSettings()
    // A question with no words in it asks nothing: left out rather than
    // saved blank, and its row goes with it.
    const next: FeedSettings = {
      ...plain,
      notNewsletters: upnext.settings?.notNewsletters ?? plain.notNewsletters,
      ranking: {
        ...plain.ranking,
        criteria: plain.ranking.criteria
          .map((c) => ({ ...c, text: c.text.trim() }))
          .filter((c) => c.text !== ''),
      },
    }
    notice = null
    problem = null
    const saved = await upnext.saveSettings(next)
    saving = false
    if (saved) {
      // What was stored, which the service may have tidied -- a window
      // clamped, a criterion's weight kept within -3 to 3 -- not the draft.
      adopt(upnext.settings ?? next)
      notice = 'Saved.'
    } else {
      problem = upnext.error ?? 'The settings could not be saved.'
    }
  }
</script>

<p class="hint lead">
  What the Feed does with podcasts, how Recommended is chosen, how Up Next and the player behave,
  and how long stories are kept. A source's own page can say otherwise for that source.
</p>

{#if !draft}
  {#if upnext.error}
    <p class="hint err" role="alert">{upnext.error}</p>
  {:else}
    <p class="hint">Loading…</p>
  {/if}
{:else}
  {@const d = draft}
  <div class="sections">
    <section class="block">
      <h3 class="eyebrow">Podcasts</h3>

      <div class="field-row">
        <span class="label" id="feed-download">Download new episodes</span>
        <div class="when">
          <div class="segmented" role="group" aria-labelledby="feed-download">
            {#each DOWNLOADS as choice (choice.id)}
              <button
                class="seg"
                class:on={d.podcasts.download.type === choice.id}
                aria-pressed={d.podcasts.download.type === choice.id}
                onclick={() => setDownload(choice.id)}
              >
                {choice.label}
              </button>
            {/each}
          </div>
          {#if d.podcasts.download.type === 'newest'}
            <input
              class="field num"
              type="number"
              min="1"
              max="99"
              aria-label="How many of the newest episodes"
              value={d.podcasts.download.count}
              oninput={(e) => setDownloadCount(e.currentTarget.value)}
            />
            <span class="hint">of each show</span>
          {/if}
        </div>
        <span class="hint">
          {#if d.podcasts.download.type === 'off'}
            Only what you ask for, or put in Up Next, is downloaded.
          {:else if d.podcasts.download.type === 'newest'}
            The newest {d.podcasts.download.count === 1
              ? 'episode'
              : `${d.podcasts.download.count} episodes`} of each show, as they arrive. Episodes are files
            in your files folder, under Feed, so they are searched and backed up with the rest.
          {:else}
            Every new episode of every show you follow. They can add up: set how long they are kept
            below.
          {/if}
        </span>
      </div>

      <div class="field-row">
        <span class="label" id="feed-keep">Keep a downloaded episode</span>
        <div class="when">
          <div class="segmented" role="group" aria-labelledby="feed-keep">
            {#each KEEPS as k (k.id)}
              <button
                class="seg"
                class:on={d.podcasts.keep.type === k.id}
                aria-pressed={d.podcasts.keep.type === k.id}
                onclick={() => setKeep(k.id)}
              >
                {k.label}
              </button>
            {/each}
          </div>
          {#if d.podcasts.keep.type === 'afterPlayed'}
            <input
              class="field num"
              type="number"
              min="1"
              max="365"
              aria-label="Days after it is played"
              value={d.podcasts.keep.days}
              oninput={(e) => setKeepNumber(e.currentTarget.value)}
            />
            <span class="hint">{d.podcasts.keep.days === 1 ? 'day' : 'days'}</span>
          {:else if d.podcasts.keep.type === 'newest'}
            <input
              class="field num"
              type="number"
              min="1"
              max="99"
              aria-label="How many episodes to keep"
              value={d.podcasts.keep.count}
              oninput={(e) => setKeepNumber(e.currentTarget.value)}
            />
            <span class="hint">of each show</span>
          {/if}
        </div>
        <span class="hint">
          {#if d.podcasts.keep.type === 'forever'}
            Until you delete it.
          {:else if d.podcasts.keep.type === 'afterPlayed'}
            Moved to the files trash that long after you have played it to the end.
          {:else}
            Older episodes of a show are moved to the files trash.
          {/if}
          Only episodes the Feed downloaded are ever cleaned up, and never a favourite, a saved story,
          or something in Up Next. Transcripts and kept articles are never removed.
        </span>
      </div>

      <div class="field-row">
        <span class="label" id="feed-transcribe">Transcribe episodes</span>
        <div class="segmented" role="group" aria-labelledby="feed-transcribe">
          {#each TRANSCRIBES as t (t.id)}
            <button
              class="seg"
              class:on={d.podcasts.transcribe === t.id}
              aria-pressed={d.podcasts.transcribe === t.id}
              onclick={() => (d.podcasts.transcribe = t.id)}
            >
              {t.label}
            </button>
          {/each}
        </div>
        <span class="hint">
          {TRANSCRIBES.find((t) => t.id === d.podcasts.transcribe)?.blurb} A transcript is what lets an
          episode be read as an article, asked about, and found by what was said.
        </span>
      </div>

      <label class="toggle">
        <input type="checkbox" bind:checked={d.podcasts.queueNew} />
        <span>
          <b>Put new episodes in Up Next</b>
          <small>
            Each new episode of a show you follow joins the end of Up Next as it arrives, downloaded
            ahead so it plays without the network.
          </small>
        </span>
      </label>
    </section>

    <section class="block">
      <h3 class="eyebrow">Recommended</h3>

      <label class="toggle">
        <input type="checkbox" bind:checked={d.ranking.enabled} />
        <span>
          <b>Rank what is new for me</b>
          <small>
            Recommended shows the unread stories of the sources you follow, best first. Off, it is
            newest first.
          </small>
        </span>
      </label>

      <div class="field-row" class:quiet={!d.ranking.enabled}>
        <span class="label" id="feed-criteria">What I want more or less of</span>
        <span class="hint">
          Each is a question the decision model asks of every story, in your words: “about building
          software with AI”, “a long interview with a founder”. Slide it from avoid to want more.
          Changing these ranks everything again.
        </span>
        <ul class="criteria" bind:this={list} aria-labelledby="feed-criteria">
          {#each d.ranking.criteria as c (c.id)}
            <li class="criterion">
              <input
                class="field words"
                type="text"
                aria-label="What it is about"
                placeholder="about building software with AI"
                disabled={!d.ranking.enabled}
                bind:value={c.text}
              />
              <div class="weight">
                <input
                  class="slider"
                  type="range"
                  min="-3"
                  max="3"
                  step="1"
                  aria-label="How much I want it"
                  aria-valuetext={weightWord(c.weight)}
                  disabled={!d.ranking.enabled}
                  bind:value={c.weight}
                />
                <span class="word" class:neg={c.weight < 0} class:pos={c.weight > 0}>
                  {weightWord(c.weight)}
                </span>
              </div>
              <button
                class="icon-btn"
                aria-label="Remove “{c.text.trim() || 'this criterion'}”"
                title="Remove"
                onclick={() => removeCriterion(c.id)}
              >
                <Icon name="close" size={15} />
              </button>
            </li>
          {/each}
        </ul>
        <div>
          <button
            class="btn btn-sm btn-outline"
            disabled={!d.ranking.enabled}
            onclick={() => void addCriterion()}
          >
            <Icon name="plus" size={14} /> Add a criterion
          </button>
        </div>
      </div>

      <label class="toggle" class:disabled={!d.ranking.enabled}>
        <input type="checkbox" disabled={!d.ranking.enabled} bind:checked={d.ranking.goals} />
        <span>
          <b>Also ask about my goals</b>
          <small>A story that helps with one of your active goals ranks higher.</small>
        </span>
      </label>

      <div class="field-row" class:quiet={!d.ranking.enabled}>
        <div class="when">
          <label class="label" for="feed-window">Look back</label>
          <input
            id="feed-window"
            class="field num"
            type="number"
            min="1"
            max="60"
            disabled={!d.ranking.enabled}
            value={d.ranking.windowDays}
            oninput={(e) => setWindow(e.currentTarget.value)}
          />
          <span class="hint">{d.ranking.windowDays === 1 ? 'day' : 'days'}</span>
        </div>
        <span class="hint">
          Only stories this recent are recommended, and ranked: older ones are left alone.
        </span>
      </div>

      <div class="status" role="status">
        <div class="status-text">
          {#if !status}
            <span class="hint">Checking what ranks Recommended…</span>
          {:else if status.judge == null}
            <span class="hint">
              Nothing to rank with, so Recommended is newest first. Set up a decision model or the
              Small model in
              <button class="link" onclick={() => panels.openSettings('models')}
                >Settings → Models</button
              >.
            </span>
          {:else}
            <span class="hint">
              Ranked by {status.judge === 'decision' ? 'the decision model' : 'the Small model'} ·
              {status.ranked.toLocaleString()} ranked · {status.waiting.toLocaleString()} waiting
            </span>
          {/if}
          {#if status?.problem}<span class="hint err">{status.problem}</span>{/if}
        </div>
        <button
          class="btn btn-sm btn-outline"
          disabled={reranking || dirty || !d.ranking.enabled || status?.judge == null}
          title={dirty ? 'Save first' : 'Forget every rank and ask again'}
          onclick={() => void rerank()}
        >
          <Icon name="refresh" size={14} />
          {reranking ? 'Ranking again…' : 'Rank again'}
        </button>
      </div>
    </section>

    <section class="block">
      <h3 class="eyebrow">Up Next and the player</h3>

      <div class="field-row">
        <label class="toggle">
          <input
            type="checkbox"
            checked={d.autofill != null}
            onchange={(e) => setAutofill(e.currentTarget.checked)}
          />
          <span>
            <b>Keep Up Next filled</b>
            <small>
              When it runs short, the best of Recommended is added, so there is always something to
              play next.
            </small>
          </span>
        </label>
        {#if d.autofill != null}
          <div class="when sub">
            <label class="hint" for="feed-autofill">At least</label>
            <input
              id="feed-autofill"
              class="field num"
              type="number"
              min="1"
              max="50"
              value={d.autofill}
              oninput={(e) => setAutofillCount(e.currentTarget.value)}
            />
            <span class="hint">{d.autofill === 1 ? 'thing' : 'things'} long</span>
          </div>
        {/if}
      </div>

      <label class="toggle">
        <input type="checkbox" bind:checked={d.autoplay} />
        <span>
          <b>Play the next one when one ends</b>
          <small>
            Work down Up Next in order, an article heard aloud in the same way, until it is empty.
          </small>
        </span>
      </label>

      <label class="toggle">
        <input type="checkbox" bind:checked={d.resumeAfterVoice} />
        <span>
          <b>Pick up where it was when a conversation ends</b>
          <small>
            Talking to the assistant pauses what is playing, an episode or an article read aloud.
            When the conversation ends, it carries on from the same place.
          </small>
        </span>
      </label>

      <div class="when">
        <label class="label" for="feed-skip-back">Skip back</label>
        <select
          id="feed-skip-back"
          class="field sec"
          value={d.skipBackS}
          onchange={(e) => (d.skipBackS = Number(e.currentTarget.value))}
        >
          {#each skips(SKIP_BACK, d.skipBackS) as s (s)}
            <option value={s}>{s} seconds</option>
          {/each}
        </select>
        <label class="label" for="feed-skip-forward">Skip forward</label>
        <select
          id="feed-skip-forward"
          class="field sec"
          value={d.skipForwardS}
          onchange={(e) => (d.skipForwardS = Number(e.currentTarget.value))}
        >
          {#each skips(SKIP_FORWARD, d.skipForwardS) as s (s)}
            <option value={s}>{s} seconds</option>
          {/each}
        </select>
      </div>
    </section>

    <section class="block">
      <h3 class="eyebrow">Memory</h3>
      <label class="toggle">
        <input type="checkbox" bind:checked={d.keepRead} />
        <span>
          <b>Keep what I read or hear</b>
          <small>
            Every article you read or have read to you is also kept as Markdown under Feed, Articles
            in your files folder. It stays there after the story has expired, searched with your
            other files, so the Feed remembers what you took in.
          </small>
        </span>
      </label>
    </section>

    <section class="block">
      <h3 class="eyebrow">Clean-up</h3>

      <div class="field-row">
        <span class="label" id="feed-clean-feeds">Web feeds, Hacker News and podcasts</span>
        <div class="when" role="group" aria-labelledby="feed-clean-feeds">
          <input
            class="field num"
            type="number"
            min="1"
            max={MAX_RETENTION_DAYS}
            aria-label="Days to keep a story from a feed after it is read"
            value={d.cleanup.feeds.readDays}
            oninput={(e) => setDays('feeds', 'readDays', e.currentTarget.value)}
          />
          <span class="hint">days once read, and</span>
          <input
            class="field num"
            type="number"
            min="1"
            max={MAX_RETENTION_DAYS}
            aria-label="Days to keep a story from a feed that was never read"
            value={d.cleanup.feeds.unreadDays}
            oninput={(e) => setDays('feeds', 'unreadDays', e.currentTarget.value)}
          />
          <span class="hint">days if not.</span>
        </div>
      </div>

      <div class="field-row">
        <span class="label" id="feed-clean-newsletters">Newsletters</span>
        <div class="when" role="group" aria-labelledby="feed-clean-newsletters">
          <input
            class="field num"
            type="number"
            min="1"
            max={MAX_RETENTION_DAYS}
            aria-label="Days to keep a newsletter after it is read"
            value={d.cleanup.newsletters.readDays}
            oninput={(e) => setDays('newsletters', 'readDays', e.currentTarget.value)}
          />
          <span class="hint">days once read, and</span>
          <input
            class="field num"
            type="number"
            min="1"
            max={MAX_RETENTION_DAYS}
            aria-label="Days to keep a newsletter that was never read"
            value={d.cleanup.newsletters.unreadDays}
            oninput={(e) => setDays('newsletters', 'unreadDays', e.currentTarget.value)}
          />
          <span class="hint">days if not.</span>
        </div>
        <span class="hint">
          Newsletters are only removed from the Feed — their emails stay in Mail. To bin a
          newsletter’s emails too, turn that on in its own settings.
        </span>
      </div>

      <p class="hint">
        Saved stories are kept for good. A source’s own settings can choose another time for just
        that source.
      </p>

      {#if (d.notNewsletters ?? []).length > 0}
        <div class="field-row">
          <span class="label" id="feed-not-newsletters">Not newsletters</span>
          <span class="hint">
            Taken out of the Feed as not being newsletters, so they are not found again. Their
            emails are still in Mail.
          </span>
          <ul class="refs" aria-labelledby="feed-not-newsletters">
            {#each d.notNewsletters ?? [] as n (`${n.sender}\n${n.name ?? ''}`)}
              <li class="ref">
                <span class="who">
                  <b>{n.name?.trim() || n.sender}</b>
                  {#if n.name?.trim()}<small>{n.sender}</small>{/if}
                </span>
                <button
                  class="btn btn-sm btn-outline"
                  disabled={allowing}
                  onclick={() => void allowAgain(n)}
                >
                  Allow again
                </button>
              </li>
            {/each}
          </ul>
        </div>
      {/if}
    </section>
  </div>

  <div class="save">
    {#if problem}
      <span class="notice err" role="alert">{problem}</span>
    {:else if notice && !dirty}
      <span class="notice" role="status">{notice}</span>
    {:else}
      <span class="notice"></span>
    {/if}
    <button class="btn btn-primary" disabled={saving || !dirty} onclick={() => void save()}>
      {saving ? 'Saving…' : 'Save'}
    </button>
  </div>
{/if}

<style>
  .lead {
    margin: 0 0 var(--sp-6);
  }

  .sections {
    display: flex;
    flex-direction: column;
    gap: var(--sp-8);
    min-width: 0;
  }

  .block {
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
    min-width: 0;
  }
  .eyebrow {
    margin: 0;
  }

  .field-row {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    min-width: 0;
  }
  /* A group that does not apply while ranking is off: still readable, so
     what it holds is there for when it is turned back on. */
  .field-row.quiet {
    opacity: 0.55;
  }
  .label {
    font-size: var(--text-base);
    font-weight: 600;
    color: var(--fg);
  }

  .toggle {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    min-width: 0;
    cursor: pointer;
  }
  .toggle.disabled {
    opacity: 0.55;
    cursor: default;
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
    min-width: 0;
  }
  .toggle b {
    font-size: var(--text-base);
    font-weight: 600;
  }
  .toggle small,
  .hint {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }
  .hint.err,
  .notice.err {
    color: var(--danger);
  }

  .when {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-3);
  }
  /* What belongs to the switch above it, lined up with its words. */
  .sub {
    margin-left: calc(13px + var(--sp-3));
  }
  .num {
    width: 5rem;
  }
  .sec {
    width: auto;
    min-width: 8rem;
  }

  .segmented {
    display: flex;
    gap: 3px;
    padding: 3px;
    background: var(--bg-sunken);
    border-radius: var(--radius);
  }
  .seg {
    flex: 1;
    height: 30px;
    padding: 0 var(--sp-3);
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-subtle);
    white-space: nowrap;
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .seg:hover {
    color: var(--fg);
  }
  .seg.on {
    background: var(--bg-raised);
    color: var(--fg);
    box-shadow: var(--shadow-sm);
  }

  /* The questions Recommended asks: the words, how much, and a way out. */
  .criteria {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    margin: var(--sp-1) 0 0;
    padding: 0;
    list-style: none;
  }
  .criterion {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2) var(--sp-3);
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .words {
    flex: 1 1 100%;
  }
  .weight {
    flex: 1;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 0;
  }
  .slider {
    flex: 1;
    min-width: 6rem;
    accent-color: var(--accent);
  }
  .word {
    flex: none;
    width: 6.5rem;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg-muted);
  }
  .word.pos {
    color: var(--accent);
  }
  .word.neg {
    color: var(--danger);
  }

  /* The senders taken out as not being newsletters, and the way back. */
  .refs {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    margin: var(--sp-1) 0 0;
    padding: 0;
    list-style: none;
  }
  .ref {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .who {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .who b {
    font-size: var(--text-base);
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .who small {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .icon-btn {
    flex: none;
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: var(--radius);
    color: var(--fg-muted);
  }
  .icon-btn:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .status {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    padding: var(--sp-3);
    border-radius: var(--radius);
    background: var(--bg-sunken);
  }
  .status-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    flex: 1;
  }

  .link {
    color: var(--accent);
    font-size: inherit;
  }
  .link:hover {
    text-decoration: underline;
  }

  .save {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--sp-3);
    margin-top: var(--sp-6);
    padding-top: var(--sp-4);
    border-top: 1px solid var(--border);
  }
  .notice {
    flex: 1;
    min-width: 0;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
</style>
