<script lang="ts">
  import { friendlyDate, plural, pluralWord } from '../lib/format'
  import { coverRatio, library } from '../lib/library.svelte'
  import { ratingLabel } from '../lib/rating'
  import { sourceLabel } from '../lib/websearch'
  import { ITEM_STATUSES } from '../lib/types'
  import type { Item, KindInfo, LogEvent, SearchResult } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Cover from './Cover.svelte'
  import Icon from './Icon.svelte'
  import Rating from './Rating.svelte'

  let {
    item,
    kind,
  }: {
    item: Item
    kind: KindInfo | null
  } = $props()

  const color = $derived(kind?.color ?? 'var(--accent)')
  const unit = $derived(item.progress?.unit || kind?.progressUnit || '')

  let confirming = $state(false)
  let tagDraft = $state('')

  // ── re-looking something up ──────────────────────────────────────────
  //
  // Offered rather than automatic. The metadata on an item is only ever
  // refreshed because somebody asked, so a shelf never changes under you
  // while you are reading it.

  let matches = $state<SearchResult[]>([])
  let matching = $state(false)
  let matchError = $state<string | null>(null)
  let showMatches = $state(false)

  async function findMatches() {
    if (!kind) return
    showMatches = true
    matching = true
    matchError = null
    try {
      // Through the store, not `web` directly: it is what routes a vault
      // that locked mid-lookup to the lock screen rather than leaving an
      // unhandled rejection and "Nothing found" on screen.
      const outcome = await library.lookup(kind.id, item.title, 6)
      matches = outcome.results
      matchError = outcome.error
    } finally {
      matching = false
    }
  }

  async function choose(hit: SearchResult) {
    showMatches = false
    // `true`: this is the explicit re-fetch, so it may replace the byline and
    // the facts a previous lookup filled in. It still cannot touch the notes,
    // the rating or the status -- that rule lives in the Rust core.
    await library.applyMetadata(item.id, hit, true)
  }

  // ── editing ──────────────────────────────────────────────────────────

  function edit(patch: () => void) {
    patch()
    library.touch(item.id)
  }

  function addTag() {
    const tag = tagDraft.trim().replace(/^#/, '')
    tagDraft = ''
    if (!tag || item.tags.some((t) => t.toLowerCase() === tag.toLowerCase())) return
    edit(() => item.tags.push(tag))
  }

  function setFact(key: string, value: string) {
    edit(() => {
      if (value.trim()) item.facts[key] = value
      // A field cleared to nothing is a fact removed, not a fact that is the
      // empty string -- otherwise the detail panel keeps drawing a blank row.
      else delete item.facts[key]
    })
  }

  async function commitProgress(raw: string) {
    const position = Number.parseInt(raw, 10)
    if (!Number.isFinite(position) || position < 0) return
    await library.setProgress(item.id, position, item.progress?.total ?? null, true)
  }

  function setTotal(raw: string) {
    const total = Number.parseInt(raw, 10)
    edit(() => {
      const position = item.progress?.position ?? 0
      item.progress = {
        position,
        total: Number.isFinite(total) && total > 0 ? total : null,
        unit: unit || 'step',
      }
    })
  }

  /**
   * What a log row says, in this shelf's language.
   *
   * `verbs.log` is the past participle -- "read", "watched", "ate at" -- so
   * it lands mid-sentence and the row reads as a sentence rather than as a
   * database event name.
   */
  function logLabel(event: LogEvent): string {
    const verb = kind?.verbs.log ?? ''
    switch (event) {
      case 'started':
        return 'Started'
      // The verb alone: "Read", "Watched", "Ate at". Saying "Finished — read
      // it" puts the same fact twice on a line that is already narrow.
      case 'finished':
        return verb ? verb[0]!.toUpperCase() + verb.slice(1) : 'Finished'
      // "Again" rather than the verb again, because "Ate at again" is a
      // sentence no shelf's wording makes read well.
      case 'revisited':
        return 'Again'
      case 'progress':
        return 'Progress'
      case 'stopped':
        return 'Set aside'
      default:
        return 'Note'
    }
  }

  const shownFields = $derived(kind?.fields ?? [])
  /**
   * Facts with no field definition behind them.
   *
   * They arise when a field is removed from a shelf: the value is kept
   * deliberately (see `Kind::field_label`), so putting the field back brings
   * four hundred books' worth of it with it. Shown greyed, with a way to
   * delete, rather than hidden — a value you cannot see or remove is worse
   * than one you can do both to.
   */
  const orphanFacts = $derived(
    Object.keys(item.facts)
      .filter((key) => !shownFields.some((f) => f.key === key))
      .sort(),
  )
</script>

<aside class="detail" style="--tint: {color}">
  <header class="top">
    <span class="where">{kind?.icon ?? ''} {kind?.singular ?? 'Item'}</span>
    <span class="spacer"></span>
    <button
      class="icon-btn"
      class:on={item.favourite}
      title={item.favourite ? 'Remove from favourites' : 'Add to favourites'}
      aria-label="Favourite"
      onclick={() => void library.toggleFavourite(item.id)}
    >
      <Icon name="star" size={15} filled={item.favourite} />
    </button>
    <button
      class="icon-btn danger"
      title="Delete"
      aria-label="Delete"
      onclick={() => (confirming = true)}
    >
      <Icon name="trash" size={15} />
    </button>
    <button class="icon-btn" title="Close" aria-label="Close" onclick={() => library.close()}>
      <Icon name="close" size={15} />
    </button>
  </header>

  <div class="scroll body">
    <div class="hero">
      <div class="art">
        <Cover
          blob={item.cover}
          title={item.title}
          icon={kind?.icon ?? ''}
          {color}
          ratio={coverRatio(kind)}
        />
        {#if !item.cover && item.coverUrl}
          <!-- The address is known and the picture is not here: a lookup that
               found the details while the network was flaky. One button
               rather than a second search. -->
          <button
            class="cover-fetch"
            disabled={library.enriching}
            onclick={() => void library.fetchCover(item.id)}
          >
            <Icon name="image" size={13} /> Get the cover
          </button>
        {/if}
      </div>

      <div class="head">
        <input
          class="title"
          value={item.title}
          aria-label="Title"
          oninput={(e) => edit(() => (item.title = e.currentTarget.value))}
        />
        <input
          class="creator"
          value={item.creator}
          placeholder="Who made it"
          aria-label="Creator"
          oninput={(e) => edit(() => (item.creator = e.currentTarget.value))}
        />

        <div class="mine">
          <Rating
            value={item.rating}
            size={19}
            showNumber
            label="Your rating"
            onchange={(v: number | null) => void library.rate(item.id, v)}
          />
        </div>

        {#each item.external as rating (rating.source)}
          <div class="theirs" title="{rating.source}, not your rating">
            <Rating value={rating.score} size={11} readonly muted />
            <!-- The separator is interpolated rather than written as text
                 between the tags: Svelte collapses the newline the formatter
                 puts there, so a literal "·" loses the space before it. -->
            <span>
              {ratingLabel(rating.score)} on {rating.source}{rating.count
                ? ` \u00b7 ${plural(rating.count, 'rating')}`
                : ''}
            </span>
          </div>
        {/each}
      </div>
    </div>

    <!-- Status: five buttons, in this shelf's own words. A select would be
         one click shorter to build and one click longer to use, and this is
         the control the app is really about. -->
    <div class="statuses" role="group" aria-label="Status">
      {#each ITEM_STATUSES as status (status)}
        <button
          class="status"
          class:on={item.status === status}
          onclick={() => void library.setStatus(item.id, status)}
        >
          {library.label(kind, status)}
        </button>
      {/each}
    </div>

    {#if item.startedOn || item.finishedOn}
      <!-- `friendlyDate`, not the long form: two full weekday-and-month dates
           on one line wrap in a 380px panel, and the same shortening is
           already what the history below uses. -->
      <p class="dates">
        {#if item.startedOn}Started {friendlyDate(item.startedOn)}{/if}
        {#if item.startedOn && item.finishedOn}
          ·
        {/if}
        {#if item.finishedOn}Finished {friendlyDate(item.finishedOn)}{/if}
      </p>
    {/if}

    {#if kind?.progressUnit}
      <section>
        <h3>Where you are</h3>
        <div class="progress">
          <input
            class="num"
            type="number"
            min="0"
            value={item.progress?.position ?? ''}
            aria-label="Position"
            onchange={(e) => void commitProgress(e.currentTarget.value)}
          />
          <span class="of">of</span>
          <input
            class="num"
            type="number"
            min="0"
            placeholder="?"
            value={item.progress?.total ?? ''}
            aria-label="Total"
            onchange={(e) => setTotal(e.currentTarget.value)}
          />
          <span class="unit">{pluralWord(item.progress?.total ?? 2, unit || 'step')}</span>
        </div>
        {#if item.progress?.total}
          <div class="track" aria-hidden="true">
            <span
              style="width: {Math.min(100, (item.progress.position / item.progress.total) * 100)}%"
            ></span>
          </div>
        {/if}
      </section>
    {/if}

    {#if item.summary}
      <section>
        <h3>About</h3>
        <!-- Their words, so it is not editable here. What you think goes in
             Notes, and keeping the two apart is what lets a re-fetch replace
             one without touching the other. -->
        <p class="summary">{item.summary}</p>
      </section>
    {/if}

    <section>
      <h3>Notes</h3>
      <textarea
        class="notes"
        rows="4"
        placeholder="What you thought…"
        value={item.notes}
        oninput={(e) => edit(() => (item.notes = e.currentTarget.value))}
      ></textarea>
    </section>

    {#if shownFields.length > 0 || orphanFacts.length > 0}
      <section>
        <h3>Details</h3>
        <dl class="facts">
          {#each shownFields as field (field.key)}
            <dt>{field.label}</dt>
            <dd>
              {#if field.fieldType === 'multiline'}
                <textarea
                  rows="2"
                  placeholder={field.placeholder}
                  value={item.facts[field.key] ?? ''}
                  oninput={(e) => setFact(field.key, e.currentTarget.value)}
                ></textarea>
              {:else}
                <input
                  type={field.fieldType === 'number'
                    ? 'number'
                    : field.fieldType === 'date'
                      ? 'date'
                      : 'text'}
                  placeholder={field.placeholder}
                  value={item.facts[field.key] ?? ''}
                  oninput={(e) => setFact(field.key, e.currentTarget.value)}
                />
              {/if}
            </dd>
          {/each}
          {#each orphanFacts as key (key)}
            <dt class="orphan" title="This shelf no longer has a field called “{key}”">{key}</dt>
            <dd>
              <input
                value={item.facts[key] ?? ''}
                oninput={(e) => setFact(key, e.currentTarget.value)}
              />
            </dd>
          {/each}
        </dl>
      </section>
    {/if}

    <section>
      <h3>Tags</h3>
      <div class="tags">
        {#each item.tags as tag (tag)}
          <button
            class="tag"
            title="Remove"
            onclick={() => edit(() => (item.tags = item.tags.filter((t) => t !== tag)))}
          >
            {tag}<Icon name="close" size={11} />
          </button>
        {/each}
        <input
          class="tag-add"
          placeholder="Add a tag"
          bind:value={tagDraft}
          onblur={addTag}
          onkeydown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault()
              addTag()
            }
          }}
        />
      </div>
    </section>

    {#if item.links.length > 0}
      <section>
        <h3>Links</h3>
        <ul class="links">
          {#each item.links as link (link.url)}
            <li>
              <!-- Opened in the system browser by the shell, never here: the
                   webview has no network permission at all. -->
              <a href={link.url} target="_blank" rel="noreferrer noopener">
                <Icon name="link" size={13} />{link.label || link.url}
              </a>
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    <section>
      <div class="head-row">
        <h3>History</h3>
        <div class="log-actions">
          <button class="mini" onclick={() => void library.addLog(item.id, 'note')}>
            <Icon name="plus" size={12} /> Note
          </button>
          <button class="mini" onclick={() => void library.addLog(item.id, 'revisited')}>
            <Icon name="refresh" size={12} /> Again
          </button>
        </div>
      </div>

      {#if library.logs.length === 0}
        <p class="empty">
          Nothing recorded yet. Marking this {kind?.singular.toLowerCase() ?? 'item'} as
          {library.label(kind, 'done').toLowerCase()} will add the first line.
        </p>
      {:else}
        <ol class="log">
          {#each library.logs as entry (entry.id)}
            <li>
              <span class="when">{friendlyDate(entry.date)}</span>
              <span class="what">
                <span class="event">{logLabel(entry.event)}</span>
                {#if entry.position !== null && entry.position !== undefined}
                  <!-- `plural`, not the bare unit: "412 page" is a typo the
                     eye trips over every time it reads the column. -->
                  <span class="at">{plural(entry.position, unit || 'step')}</span>
                {/if}
                <input
                  class="log-note"
                  placeholder="Add a note"
                  value={entry.note}
                  oninput={(e) => (entry.note = e.currentTarget.value)}
                  onblur={() => void library.saveLogEntry(entry)}
                />
              </span>
              <button
                class="icon-btn small"
                title="Remove this line"
                aria-label="Remove this line"
                onclick={() => void library.removeLog(entry.id)}
              >
                <Icon name="close" size={12} />
              </button>
            </li>
          {/each}
        </ol>
      {/if}
    </section>

    <section>
      <div class="head-row">
        <h3>Metadata</h3>
        <button class="mini" disabled={library.enriching || matching} onclick={findMatches}>
          <Icon name="sparkle" size={12} />
          {item.source ? 'Look up again' : 'Look this up'}
        </button>
      </div>

      {#if showMatches}
        {#if matching}
          <p class="empty">Searching…</p>
        {:else if matchError}
          <p class="empty err">{matchError}</p>
        {:else if matches.length === 0}
          <p class="empty">Nothing found for “{item.title}”.</p>
        {:else}
          <p class="empty">
            Picking one replaces the title, byline and details. Your rating, your notes and the
            history are never touched.
          </p>
          <div class="matches">
            {#each matches as hit, i (hit.url || hit.title + i)}
              <button class="match" onclick={() => void choose(hit)}>
                <span class="m-title">{hit.title}</span>
                {#if hit.creator || hit.year}
                  <span class="m-sub"
                    >{hit.creator}{#if hit.creator && hit.year}&nbsp;·&nbsp;{/if}{hit.year ??
                      ''}</span
                  >
                {/if}
              </button>
            {/each}
          </div>
        {/if}
      {:else if item.source}
        <p class="empty">Filled in from {sourceLabel(item.source)}.</p>
      {/if}
    </section>
  </div>
</aside>

{#if confirming}
  <ConfirmDialog
    title={'Delete “' + item.title + '”?'}
    detail="Its notes, its rating and its whole history go with it. This cannot be undone."
    confirmLabel="Delete"
    onconfirm={() => {
      confirming = false
      void library.remove(item.id)
    }}
    oncancel={() => (confirming = false)}
  />
{/if}

<style>
  .detail {
    width: 380px;
    flex: none;
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: var(--bg-panel);
    border-left: 1px solid var(--border);
  }

  .top {
    display: flex;
    align-items: center;
    gap: 2px;
    height: 42px;
    flex: none;
    padding: 0 var(--sp-2) 0 var(--sp-4);
    border-bottom: 1px solid var(--border);
  }
  .where {
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }
  .spacer {
    flex: 1;
  }

  .icon-btn {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: var(--radius-sm);
    color: var(--fg-subtle);
  }
  .icon-btn:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .icon-btn.on {
    color: #e0a92b;
  }
  .icon-btn.danger:hover {
    color: var(--danger);
  }
  .icon-btn.small {
    width: 20px;
    height: 20px;
    opacity: 0;
  }

  .body {
    flex: 1;
    padding: var(--sp-4);
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
  }

  .hero {
    display: flex;
    gap: var(--sp-3);
  }
  .art {
    width: 116px;
    flex: none;
  }
  .cover-fetch {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 4px;
    width: 100%;
    margin-top: var(--sp-1);
    padding: 3px 0;
    border-radius: var(--radius-sm);
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    background: var(--bg-hover);
  }
  .cover-fetch:hover {
    color: var(--fg);
  }

  .head {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .title,
  .creator {
    width: 100%;
    border: 0;
    background: none;
    border-radius: var(--radius-sm);
    padding: 2px 4px;
    margin-left: -4px;
    user-select: text;
  }
  .title {
    font-family: var(--font-read);
    font-size: var(--text-lg);
    font-weight: 650;
    line-height: var(--leading-tight);
    color: var(--fg);
  }
  .creator {
    font-size: var(--text-base);
    color: var(--fg-muted);
  }
  .title:hover,
  .creator:hover {
    background: var(--bg-hover);
  }
  .title:focus,
  .creator:focus {
    outline: none;
    background: var(--bg-hover);
  }

  .mine {
    margin-top: var(--sp-2);
  }
  .theirs {
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }

  .statuses {
    display: flex;
    flex-wrap: wrap;
    gap: 3px;
    padding: 3px;
    border-radius: var(--radius);
    background: var(--bg-active);
  }
  .status {
    flex: 1 1 auto;
    height: 26px;
    padding: 0 var(--sp-2);
    border-radius: calc(var(--radius) - 3px);
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-subtle);
    white-space: nowrap;
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .status:hover {
    color: var(--fg);
  }
  .status.on {
    background: var(--tint);
    color: #fff;
    box-shadow: var(--shadow-sm);
  }

  .dates {
    margin: calc(var(--sp-4) * -1 + 2px) 0 0;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }

  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  h3 {
    margin: 0;
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }
  .head-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2);
  }
  .log-actions {
    display: flex;
    gap: var(--sp-1);
  }
  .mini {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 22px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-xs);
    font-weight: 550;
    color: var(--fg-subtle);
    background: var(--bg-hover);
  }
  .mini:hover:not(:disabled) {
    color: var(--fg);
    background: var(--bg-active);
  }
  .mini:disabled {
    opacity: 0.45;
  }

  .progress {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .num {
    width: 68px;
    height: 28px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    font-size: var(--text-base);
    font-variant-numeric: tabular-nums;
    color: var(--fg);
    user-select: text;
  }
  .of,
  .unit {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .track {
    height: 5px;
    border-radius: 999px;
    background: var(--bg-active);
    overflow: hidden;
  }
  .track span {
    display: block;
    height: 100%;
    background: var(--tint);
    transition: width var(--med) var(--ease);
  }

  .summary {
    margin: 0;
    font-family: var(--font-read);
    font-size: var(--text-base);
    line-height: var(--leading-normal);
    color: var(--fg-muted);
  }

  .notes,
  .facts textarea,
  .facts input {
    width: 100%;
    padding: var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    font: inherit;
    font-size: var(--text-base);
    color: var(--fg);
    resize: vertical;
    user-select: text;
  }
  .notes {
    font-family: var(--font-read);
    line-height: var(--leading-normal);
  }
  .notes:focus,
  .facts textarea:focus,
  .facts input:focus {
    outline: none;
    border-color: var(--tint);
  }

  .facts {
    display: grid;
    grid-template-columns: 96px 1fr;
    align-items: center;
    gap: var(--sp-2);
    margin: 0;
  }
  dt {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  dt.orphan {
    font-style: italic;
    color: var(--fg-faint);
  }
  dd {
    margin: 0;
    min-width: 0;
  }

  .tags {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-1);
    align-items: center;
  }
  .tag {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 22px;
    padding: 0 var(--sp-2);
    border-radius: 999px;
    font-size: var(--text-xs);
    color: var(--fg-muted);
    background: var(--bg-active);
  }
  .tag:hover {
    color: var(--danger);
  }
  .tag-add {
    height: 22px;
    min-width: 90px;
    flex: 1;
    padding: 0 var(--sp-2);
    border: 1px dashed var(--border-strong);
    border-radius: 999px;
    background: none;
    font-size: var(--text-xs);
    color: var(--fg);
    user-select: text;
  }
  .tag-add:focus {
    outline: none;
    border-style: solid;
    border-color: var(--tint);
  }

  .links {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .links a {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: var(--text-sm);
    color: var(--tint);
    text-decoration: none;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .links a:hover {
    text-decoration: underline;
  }

  .log {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }
  .log li {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    padding: var(--sp-1) 0;
    /* The rail: a history reads as one column of days, not as a table. */
    border-left: 2px solid var(--bg-active);
    padding-left: var(--sp-3);
    margin-left: 3px;
  }
  .log li:hover .icon-btn.small {
    opacity: 1;
  }
  .when {
    flex: none;
    width: 76px;
    font-size: var(--text-xs);
    color: var(--fg-faint);
    padding-top: 3px;
  }
  .what {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .event {
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-muted);
  }
  .at {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .log-note {
    width: 100%;
    border: 0;
    background: none;
    padding: 1px 0;
    font-size: var(--text-sm);
    color: var(--fg);
    user-select: text;
  }
  .log-note:focus {
    outline: none;
  }
  .log-note::placeholder {
    color: var(--fg-faint);
  }

  .empty {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
    color: var(--fg-subtle);
  }
  .empty.err {
    color: var(--danger);
  }

  .matches {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .match {
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding: var(--sp-2);
    border-radius: var(--radius-sm);
    text-align: left;
    background: var(--bg-hover);
  }
  .match:hover {
    background: var(--bg-active);
  }
  .m-title {
    font-size: var(--text-base);
    font-weight: 550;
    color: var(--fg);
  }
  .m-sub {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
</style>
