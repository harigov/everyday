<!--
  The transcript: a collapsible panel below a meeting note's body.
  `getTranscript` returns nothing for an ordinary note, so a note without one
  never grows this panel at all -- see the parent's own `{#if}`.

  One colour per speaker, the house rule (`meetings-format.ts`'s
  `speakerColor`) applied to a person instead of a role. Naming an "Unknown"
  speaker offers the event's attendees first, because in a two- or three-
  person call that is almost always the right guess, and free text for
  whoever it is not.
-->
<script lang="ts">
  import { plural } from '../lib/format'
  import { getRecording, getTranscript, nameSpeaker, rewriteMeetingNote } from '../lib/meetings-api'
  import { formatOffset, speakerColor, speakerNameStyle } from '../lib/meetings-format'
  import { meetings } from '../lib/meetings.svelte'
  import type { EventRef, NoteId, RecordingId, Segment, Speaker, Transcript } from '../lib/types'
  import Icon from './Icon.svelte'

  let {
    noteId,
    onreplacebody,
  }: {
    noteId: NoteId
    /** Applies a proposed markdown body to the note, after confirmation. */
    onreplacebody: (markdown: string) => void
  } = $props()

  let transcript = $state<Transcript | null>(null)
  let loading = $state(true)
  let expanded = $state(false)
  let query = $state('')
  let event = $state<EventRef | null | undefined>(undefined)
  let copiedAt = $state<number | null>(null)

  let namingKey = $state<number | null>(null)
  let nameDraft = $state('')

  let offerRewrite = $state(false)
  let rewriting = $state(false)
  let rewriteTemplateId = $state<string | null>(null)
  let rewriteError = $state<string | null>(null)
  let rewritePreview = $state<string | null>(null)

  $effect(() => {
    void load(noteId)
  })

  async function load(id: NoteId) {
    loading = true
    transcript = null
    event = undefined
    try {
      transcript = await getTranscript(id)
    } catch {
      transcript = null
    }
    loading = false
    const recordingId: RecordingId | null | undefined = transcript?.recordingId
    if (recordingId) {
      try {
        const r = await getRecording(recordingId)
        event = r.event ?? null
      } catch {
        event = null
      }
    }
  }

  const speakerById = $derived(new Map((transcript?.speakers ?? []).map((s) => [s.key, s])))

  const shownSegments = $derived.by(() => {
    const segs = transcript?.segments ?? []
    const q = query.trim().toLowerCase()
    if (!q) return segs
    return segs.filter((s) => s.text.toLowerCase().includes(q))
  })

  function labelOf(speaker: Speaker | undefined): string {
    if (!speaker) return 'Unknown'
    return speaker.label
  }

  async function copyLine(seg: Segment) {
    const speaker = speakerById.get(seg.speaker)
    const line = `${formatOffset(seg.startMs)} ${labelOf(speaker)}: ${seg.text}`
    try {
      await navigator.clipboard.writeText(line)
      copiedAt = seg.startMs
      setTimeout(() => {
        if (copiedAt === seg.startMs) copiedAt = null
      }, 1200)
    } catch {
      // No clipboard permission -- nothing this panel can do about it, and
      // not worth a banner over a note.
    }
  }

  function startNaming(key: number) {
    namingKey = key
    nameDraft = ''
  }

  async function confirmName(name: string) {
    if (namingKey === null || !transcript || !name.trim()) return
    const key = namingKey
    namingKey = null
    try {
      transcript = await nameSpeaker({ noteId, speakerKey: key, name: name.trim() })
      offerRewrite = true
    } catch (e) {
      // The chip did the asking; a failed rename is quiet enough to retry.
      console.error(e)
    }
  }

  void meetings.load()
  const templates = $derived(meetings.view?.settings.templates ?? [])

  function openRewrite() {
    offerRewrite = false
    rewriteError = null
    rewritePreview = null
    rewriteTemplateId = rewriteTemplateId ?? templates[0]?.id ?? null
  }

  async function runRewrite() {
    if (!rewriteTemplateId) return
    rewriting = true
    rewriteError = null
    try {
      rewritePreview = await rewriteMeetingNote(noteId, rewriteTemplateId)
    } catch (e) {
      rewriteError = e instanceof Error ? e.message : String(e)
    } finally {
      rewriting = false
    }
  }

  function applyRewrite() {
    if (rewritePreview) onreplacebody(rewritePreview)
    rewritePreview = null
    rewriteTemplateId = null
  }
</script>

{#if loading}
  <p class="hint">Loading the transcript…</p>
{:else if transcript}
  <section class="panel">
    <button class="head" onclick={() => (expanded = !expanded)} aria-expanded={expanded}>
      <Icon name="chevron" size={14} weight={1.6} />
      <span class="title">Transcript</span>
      <span class="count">{plural(transcript.segments.length, 'line')}</span>
    </button>

    {#if expanded}
      <div class="body">
        <div class="chips">
          {#each transcript.speakers as s (s.key)}
            {@const style = speakerNameStyle(s.how)}
            <span class="chip" style="--c: {speakerColor(s.key, s.how.type === 'owner')}">
              <span class="swatch" aria-hidden="true"></span>
              {#if style === 'unknown'}
                <span class="name">{s.label}</span>
                {#if namingKey === s.key}
                  <span class="who">
                    {#each event?.attendees ?? [] as a (a)}
                      <button class="option" onclick={() => void confirmName(a)}>{a}</button>
                    {/each}
                    <input
                      class="who-input"
                      bind:value={nameDraft}
                      placeholder="Name"
                      onkeydown={(e) => e.key === 'Enter' && void confirmName(nameDraft)}
                    />
                    <button class="option" onclick={() => void confirmName(nameDraft)}>Set</button>
                  </span>
                {:else}
                  <button class="whois" onclick={() => startNaming(s.key)}>Who is this?</button>
                {/if}
              {:else if style === 'guessed'}
                <span class="name guessed">{s.label} <em>(guessed)</em></span>
              {:else}
                <span class="name">{s.label}</span>
              {/if}
            </span>
          {/each}
        </div>

        {#if offerRewrite}
          <div class="offer">
            <span>Rewrite the summary now this speaker has a name?</span>
            <button class="link" onclick={() => (offerRewrite = false)}>Not now</button>
            <button class="btn btn-primary" onclick={openRewrite}>Rewrite summary</button>
          </div>
        {/if}

        <div class="toolbar">
          <div class="search">
            <Icon name="search" size={13} />
            <input placeholder="Search the transcript" bind:value={query} spellcheck="false" />
          </div>
          {#if !offerRewrite}
            <button class="btn" onclick={openRewrite}>Rewrite summary…</button>
          {/if}
        </div>

        {#if rewriteTemplateId !== null || rewritePreview !== null}
          <div class="rewrite">
            {#if !rewritePreview}
              <label class="setting">
                <span>Template</span>
                <select bind:value={rewriteTemplateId}>
                  {#each templates as t (t.id)}
                    <option value={t.id}>{t.name}</option>
                  {/each}
                </select>
              </label>
              <div class="row">
                <button class="btn" onclick={() => (rewriteTemplateId = null)}>Cancel</button>
                <button
                  class="btn btn-primary"
                  disabled={rewriting}
                  onclick={() => void runRewrite()}
                >
                  {rewriting ? 'Writing…' : 'Preview'}
                </button>
              </div>
              {#if rewriteError}<p class="error">{rewriteError}</p>{/if}
            {:else}
              <p class="hint">This will replace the note's body:</p>
              <pre class="preview">{rewritePreview}</pre>
              <div class="row">
                <button class="btn" onclick={() => (rewritePreview = null)}>Cancel</button>
                <button class="btn btn-primary" onclick={applyRewrite}>Replace body</button>
              </div>
            {/if}
          </div>
        {/if}

        <ol class="lines">
          {#each shownSegments as seg (seg.startMs + '-' + seg.speaker)}
            {@const speaker = speakerById.get(seg.speaker)}
            <li>
              <button
                class="line"
                style="--c: {speakerColor(seg.speaker, speaker?.how.type === 'owner')}"
                onclick={() => void copyLine(seg)}
              >
                <span class="swatch" aria-hidden="true"></span>
                <span class="time">{formatOffset(seg.startMs)}</span>
                <span class="who-name">{labelOf(speaker)}</span>
                <span class="text">{seg.text}</span>
                {#if copiedAt === seg.startMs}<span class="copied">Copied</span>{/if}
              </button>
            </li>
          {/each}
        </ol>
        {#if shownSegments.length === 0}
          <p class="hint">Nothing matched.</p>
        {/if}
      </div>
    {/if}
  </section>
{/if}

<style>
  .panel {
    margin-top: var(--sp-6);
    border-top: 1px solid var(--border);
    padding-top: var(--sp-4);
  }
  .head {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    color: var(--fg-muted);
  }
  .head :global(svg) {
    transition: rotate var(--fast) var(--ease);
  }
  .head[aria-expanded='true'] :global(svg) {
    rotate: 90deg;
  }
  .title {
    font-weight: 600;
    color: var(--fg);
  }
  .count {
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }

  .body {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    margin-top: var(--sp-3);
  }

  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }
  .chip {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px var(--sp-2);
    border-radius: 999px;
    background: color-mix(in oklab, var(--c) 14%, transparent);
    font-size: var(--text-sm);
  }
  .swatch {
    width: 8px;
    height: 8px;
    flex: none;
    border-radius: 50%;
    background: var(--c);
  }
  .name.guessed em {
    font-style: italic;
    color: var(--fg-faint);
  }
  .whois {
    color: var(--accent);
    font-weight: 550;
  }
  .who {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px;
  }
  .option {
    padding: 2px 6px;
    border-radius: var(--radius-sm);
    background: var(--bg-hover);
    font-size: var(--text-xs);
  }
  .who-input {
    width: 8rem;
    height: 22px;
    padding: 0 6px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg);
  }

  .offer {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--radius);
    background: var(--bg-hover);
    font-size: var(--text-sm);
  }
  .offer span:first-child {
    flex: 1;
    min-width: 0;
  }
  .link {
    color: var(--accent);
    font-size: var(--text-sm);
  }

  .toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
  }
  .search {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    flex: 1;
    max-width: 320px;
    height: 30px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    background: var(--bg-hover);
    color: var(--fg-faint);
  }
  .search input {
    flex: 1;
    min-width: 0;
    border: 0;
    background: none;
    color: var(--fg);
    font-size: var(--text-sm);
  }
  .search input:focus {
    outline: none;
  }

  .rewrite {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .setting {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
  }
  .setting select {
    padding: var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg);
    color: var(--fg);
  }
  .row {
    display: flex;
    justify-content: flex-end;
    gap: var(--sp-2);
  }
  .error {
    color: var(--danger);
    font-size: var(--text-sm);
  }
  pre.preview {
    max-height: 240px;
    overflow: auto;
    padding: var(--sp-3);
    border-radius: var(--radius);
    background: var(--bg-sunken);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    white-space: pre-wrap;
  }

  .lines {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .line {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    width: 100%;
    padding: var(--sp-1) var(--sp-2);
    border-radius: var(--radius-sm);
    text-align: left;
  }
  .line:hover {
    background: var(--bg-hover);
  }
  .line .swatch {
    align-self: center;
  }
  .time {
    flex: none;
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
  .who-name {
    flex: none;
    color: var(--c);
    font-size: var(--text-sm);
    font-weight: 600;
  }
  .text {
    flex: 1;
    min-width: 0;
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }
  .copied {
    flex: none;
    color: var(--accent);
    font-size: var(--text-xs);
  }

  .hint {
    color: var(--fg-subtle);
    font-size: var(--text-sm);
  }
</style>
