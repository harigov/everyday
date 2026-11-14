<script lang="ts">
  // Templates: the shapes a journal entry, a note or a meeting's notes start
  // from. One list for all three, because "how I want my Monday page" and
  // "how I want a one-to-one written up" are the same kind of thing --
  // `karmayoga_core::template` has the rules. Where a template is *chosen* is
  // elsewhere: a journal's settings (a default, and rules for days), the
  // Meetings tab (the default for a recording), the notes app's New menu.
  //
  // The shape is Skills': a list of cards, and an explicit editor with Save
  // and Cancel rather than an autosaving form, because a half-typed template
  // that a journal picked up mid-edit would start somebody's day with it.

  import { notify } from '../lib/notify.svelte'
  import { renderMarkdown } from '../lib/markdown'
  import { sliceToMarkdown, type PMNode } from '../lib/markdown-copy'
  import { meetings } from '../lib/meetings.svelte'
  import { app } from '../lib/state.svelte'
  import { ALL_USES, USE_LABELS, templates } from '../lib/templates.svelte'
  import { todayIso } from '../lib/time'
  import type { RenderedTemplate, Template, TemplateUse } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'

  let editing = $state<Template | null>(null)
  let pendingDelete = $state<Template | null>(null)
  let saving = $state(false)

  void templates.load()
  // For the card that says which template meetings are written up from.
  if (meetings.supported) void meetings.load()

  /** Which journals start a day from each template -- said on its card. */
  const journalUse = $derived.by(() => {
    const out = new Map<string, string[]>()
    for (const j of app.ownJournals) {
      const named = new Set<string>()
      if (j.templates?.default) named.add(j.templates.default)
      for (const r of j.templates?.rules ?? []) named.add(r.template)
      for (const id of named) out.set(id, [...(out.get(id) ?? []), j.name])
    }
    return out
  })

  const meetingDefault = $derived(meetings.view?.settings.defaultTemplate ?? null)

  async function draft(uses: TemplateUse[] = ['journal', 'notes']) {
    try {
      editing = await templates.draft(uses)
      tagsText = ''
      resetPreview()
    } catch (e) {
      notify.error(e instanceof Error ? e.message : String(e))
    }
  }

  function edit(t: Template) {
    editing = structuredClone($state.snapshot(t))
    tagsText = t.tags.join(', ')
    resetPreview()
  }

  async function duplicate(t: Template) {
    try {
      const fresh = await templates.draft(t.uses)
      editing = {
        ...fresh,
        name: `${t.name} (copy)`,
        title: t.title,
        body: t.body,
        tags: [...t.tags],
      }
      tagsText = t.tags.join(', ')
      resetPreview()
    } catch (e) {
      notify.error(e instanceof Error ? e.message : String(e))
    }
  }

  function toggleUse(place: TemplateUse) {
    if (!editing) return
    editing.uses = editing.uses.includes(place)
      ? editing.uses.filter((u) => u !== place)
      : ALL_USES.filter((u) => u === place || editing!.uses.includes(u))
  }

  // Tags typed as one line, split on commas when saved.
  let tagsText = $state('')

  async function save() {
    const current = editing
    if (!current) return
    saving = true
    try {
      const tags = tagsText
        .split(',')
        .map((t) => t.trim())
        .filter(Boolean)
      await templates.save({ ...$state.snapshot(current), tags })
      editing = null
    } catch (e) {
      notify.error(e instanceof Error ? e.message : String(e))
    } finally {
      saving = false
    }
  }

  async function remove() {
    const target = pendingDelete
    pendingDelete = null
    if (!target) return
    try {
      await templates.remove(target.id)
      if (editing?.id === target.id) editing = null
    } catch (e) {
      notify.error(e instanceof Error ? e.message : String(e))
    }
  }

  // ── Problems, checked as it is typed ────────────────────────────────

  let issues = $state<string[]>([])
  let lintTimer: ReturnType<typeof setTimeout> | null = null
  $effect(() => {
    const current = editing
    if (!current) {
      issues = []
      return
    }
    // Read every field the lint depends on, so a change to any re-runs it.
    const snapshot = { ...current, uses: [...current.uses] }
    if (lintTimer) clearTimeout(lintTimer)
    lintTimer = setTimeout(() => {
      templates
        .lint(snapshot)
        .then((found) => {
          if (editing?.id === snapshot.id) issues = found
        })
        .catch(() => {})
    }, 300)
    return () => {
      if (lintTimer) clearTimeout(lintTimer)
    }
  })

  // ── Preview ─────────────────────────────────────────────────────────

  let previewDate = $state(todayIso())
  let previewJournal = $state<string | null>(null)
  let preview = $state<RenderedTemplate | null>(null)
  let meetingPreview = $state<string | null>(null)
  let previewing = $state(false)
  let previewError = $state<string | null>(null)

  function resetPreview() {
    preview = null
    meetingPreview = null
    previewError = null
  }

  async function previewDay() {
    if (!editing) return
    previewing = true
    previewError = null
    meetingPreview = null
    try {
      preview = await templates.render(
        $state.snapshot(editing),
        previewDate || null,
        previewJournal,
      )
    } catch (e) {
      previewError = e instanceof Error ? e.message : String(e)
    } finally {
      previewing = false
    }
  }

  async function previewMeeting() {
    if (!editing) return
    previewing = true
    previewError = null
    preview = null
    try {
      meetingPreview = await templates.previewMeeting(editing.body)
    } catch (e) {
      previewError = e instanceof Error ? e.message : String(e)
    } finally {
      previewing = false
    }
  }

  const previewHtml = $derived(
    preview ? renderMarkdown(sliceToMarkdown((preview.body.content ?? []) as PMNode[], 0, 0)) : '',
  )

  const forWriting = $derived(
    editing ? editing.uses.includes('journal') || editing.uses.includes('notes') : false,
  )

  /** The template's shape at a glance: its headings, or its first line. */
  function outline(body: string): string {
    const lines = body.split('\n')
    const headings = lines
      .filter((l) => /^\s{0,3}#{1,6}\s/.test(l))
      .map((l) => l.replace(/^\s*#+\s*/, '').trim())
    if (headings.length) return headings.join(' · ')
    return lines.map((l) => l.trim()).find(Boolean) ?? ''
  }
</script>

<div class="panel">
  {#if !editing}
    <header class="head">
      <p class="lead">
        A template is the page a journal entry, a note or a meeting's notes starts with. Journals
        choose theirs in their own settings; meetings in the Meetings tab.
      </p>
      <span class="spacer"></span>
      <button class="btn btn-primary" onclick={() => void draft()}>New template</button>
    </header>
  {/if}

  <div class="body">
    {#if editing}
      <section class="editor" aria-label="Edit template">
        <label class="field-row">
          <span class="eyebrow">Name</span>
          <input class="field" placeholder="Weekly review" bind:value={editing.name} />
        </label>

        <div class="field-row">
          <span class="eyebrow">Use it for</span>
          <div class="uses">
            {#each ALL_USES as place (place)}
              <label class="use">
                <input
                  type="checkbox"
                  checked={editing.uses.includes(place)}
                  onchange={() => toggleUse(place)}
                />
                <span>{USE_LABELS[place]}</span>
              </label>
            {/each}
          </div>
        </div>

        {#if forWriting}
          <label class="field-row">
            <span class="eyebrow">Title</span>
            <input
              class="field"
              placeholder="Week {'{{week}}'} review — leave empty to start untitled"
              bind:value={editing.title}
              spellcheck="false"
            />
          </label>
        {/if}

        <label class="field-row">
          <span class="eyebrow">Text</span>
          <textarea
            class="field area"
            rows="14"
            placeholder={'## {{weekday}}\nWhat today is for, in a sentence.\n\n## Grateful for'}
            bind:value={editing.body}
            spellcheck="false"
          ></textarea>
          <span class="hint">
            Markdown. Headings are kept as written, and the line under a heading is a prompt for
            whoever writes that part: you, in an entry or a note; the model, in meeting notes.
          </span>
        </label>

        <details class="placeholders">
          <summary>Placeholders</summary>
          <dl>
            <dt>Every template</dt>
            <dd>
              <code>{'{{date}}'}</code> <code>{'{{weekday}}'}</code> <code>{'{{day}}'}</code>
              <code>{'{{month}}'}</code> <code>{'{{year}}'}</code> <code>{'{{week}}'}</code>
              <code>{'{{quarter}}'}</code> — the day it is used for. Add an offset to move it:
              <code>{'{{date+7d}}'}</code>, <code>{'{{weekday-1d}}'}</code>,
              <code>{'{{month+1m}}'}</code> (<code>d</code>, <code>w</code>, <code>m</code>,
              <code>y</code>). <code>{'{{date:%a %-d %b}}'}</code> picks its own format.
            </dd>
            <dt>Journal</dt>
            <dd><code>{'{{journal}}'}</code> — the journal's name.</dd>
            <dt>Meeting notes</dt>
            <dd>
              <code>{'{{title}}'}</code> <code>{'{{start}}'}</code> <code>{'{{end}}'}</code>
              <code>{'{{duration}}'}</code> <code>{'{{organizer}}'}</code>
              <code>{'{{attendees}}'}</code> <code>{'{{present}}'}</code>
              <code>{'{{calendar}}'}</code> — and a heading of <code>{'{{topics}}'}</code> becomes one
              section per topic discussed.
            </dd>
          </dl>
        </details>

        {#if forWriting}
          <label class="field-row">
            <span class="eyebrow">Tags</span>
            <input
              class="field"
              placeholder="review, weekly"
              bind:value={tagsText}
              spellcheck="false"
            />
            <span class="hint">What an entry or a note started from it is tagged with.</span>
          </label>
        {/if}

        {#if issues.length > 0}
          <ul class="issues" aria-label="Problems">
            {#each issues as issue (issue)}
              <li><Icon name="alert" size={13} /> {issue}</li>
            {/each}
          </ul>
        {/if}

        <div class="preview-bar">
          {#if forWriting}
            <input
              class="field date"
              type="date"
              bind:value={previewDate}
              aria-label="Preview day"
            />
            {#if editing.uses.includes('journal') && app.ownJournals.length > 0}
              <select class="field" bind:value={previewJournal} aria-label="Preview journal">
                <option value={null}>No journal</option>
                {#each app.ownJournals as j (j.id)}
                  <option value={j.id}>{j.name}</option>
                {/each}
              </select>
            {/if}
            <button class="btn btn-outline" disabled={previewing} onclick={() => void previewDay()}>
              Preview
            </button>
          {/if}
          {#if editing.uses.includes('meetings')}
            <button
              class="btn btn-outline"
              disabled={previewing}
              title="Writes up a sample call with your model"
              onclick={() => void previewMeeting()}
            >
              {previewing ? 'Writing…' : 'Preview meeting notes'}
            </button>
          {/if}
        </div>
        {#if previewError}<p class="notice err">{previewError}</p>{/if}
        {#if preview}
          <article class="preview">
            {#if preview.title}<h1 class="ptitle">{preview.title}</h1>{/if}
            <!-- Safe: `renderMarkdown` escapes before it formats, the same
                 argument the assistant's run summaries make. -->
            <!-- eslint-disable-next-line svelte/no-at-html-tags -->
            <div class="md">{@html previewHtml}</div>
          </article>
        {/if}
        {#if meetingPreview}<pre class="preview mono">{meetingPreview}</pre>{/if}

        <footer class="editorfoot">
          {#if templates.all.some((t) => t.id === editing?.id)}
            <button class="btn btn-danger" onclick={() => (pendingDelete = editing)}>Delete</button>
          {/if}
          <span class="spacer"></span>
          <button class="btn" onclick={() => (editing = null)}>Cancel</button>
          <button
            class="btn btn-primary"
            disabled={saving ||
              !editing.name.trim() ||
              (!editing.body.trim() && !editing.title.trim())}
            onclick={() => void save()}
          >
            Save
          </button>
        </footer>
      </section>
    {:else if templates.loaded && templates.all.length === 0}
      <EmptyState lead="No templates yet">
        {#snippet icon()}<Icon name="layers" size={34} weight={1.4} />{/snippet}
        {#snippet note()}
          A template is the page you start from: a daily page with the same three headings, a weekly
          review on Sundays, the shape of a one-to-one's notes. Placeholders like {'{{weekday}}'} are
          filled in for the day.
        {/snippet}
        {#snippet action()}
          <button class="btn btn-primary" onclick={() => void draft()}>Add a template</button>
        {/snippet}
      </EmptyState>
    {:else}
      {#each templates.all as t (t.id)}
        <article class="card" class:off={t.uses.length === 0}>
          <header class="cardhead">
            <span class="name">{t.name}</span>
            <span class="spacer"></span>
            {#each t.uses as place (place)}
              <span class="pill">{USE_LABELS[place]}</span>
            {/each}
            {#if t.uses.length === 0}<span class="pill dim">Not offered</span>{/if}
          </header>
          {#if outline(t.body)}<p class="said">{outline(t.body)}</p>{/if}
          {#if journalUse.get(t.id) || meetingDefault === t.id}
            <p class="where">
              {#if journalUse.get(t.id)}Starts days in {journalUse.get(t.id)!.join(', ')}.{/if}
              {#if meetingDefault === t.id}The default for meeting notes.{/if}
            </p>
          {/if}
          <footer class="cardfoot">
            <button class="link" onclick={() => edit(t)}>Edit</button>
            <button class="link" onclick={() => void duplicate(t)}>Duplicate</button>
            <button class="link" onclick={() => (pendingDelete = t)}>Delete</button>
          </footer>
        </article>
      {/each}
    {/if}
  </div>
</div>

{#if pendingDelete}
  <ConfirmDialog
    title="Delete this template?"
    detail={'“' +
      pendingDelete.name +
      '” will be gone. Entries, notes and meeting notes already written from it keep their text.'}
    confirmLabel="Delete template"
    onconfirm={remove}
    oncancel={() => (pendingDelete = null)}
  />
{/if}

<style>
  .panel {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }

  .head {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-4);
  }

  .spacer {
    flex: 1;
  }

  .lead {
    color: var(--fg-muted);
    font-size: var(--text-sm);
    line-height: 1.55;
    max-width: 44ch;
  }

  .body {
    display: grid;
    align-content: start;
    gap: var(--sp-3);
    min-width: 0;
  }

  .card {
    display: grid;
    gap: var(--sp-2);
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
  }

  .card.off {
    opacity: 0.65;
  }

  .cardhead,
  .cardfoot {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }

  .cardhead {
    gap: var(--sp-2);
  }

  .name {
    font-weight: 600;
  }

  .pill {
    padding: 1px var(--sp-2);
    border: 1px solid var(--border);
    border-radius: 999px;
    color: var(--fg-muted);
    font-size: var(--text-xs);
    white-space: nowrap;
  }

  .pill.dim {
    color: var(--fg-faint);
  }

  .said,
  .where {
    color: var(--fg-muted);
    font-size: var(--text-sm);
    line-height: 1.55;
  }

  .where {
    color: var(--fg-faint);
  }

  .link {
    color: var(--accent);
    font-size: var(--text-sm);
  }

  .link:hover {
    text-decoration: underline;
  }

  .editor {
    display: grid;
    gap: var(--sp-4);
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
  }

  .field-row {
    display: grid;
    gap: var(--sp-2);
  }

  .uses {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-4);
  }

  .use {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-sm);
  }

  .area {
    resize: vertical;
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    line-height: 1.5;
    white-space: pre-wrap;
  }

  .hint {
    color: var(--fg-faint);
    font-size: var(--text-xs);
    line-height: 1.5;
  }

  .placeholders {
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }

  .placeholders summary {
    cursor: pointer;
    color: var(--fg-muted);
  }

  .placeholders dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: var(--sp-2) var(--sp-4);
    margin-top: var(--sp-3);
    line-height: 1.7;
  }

  .placeholders dt {
    color: var(--fg-faint);
  }

  .placeholders code {
    font-size: var(--text-xs);
  }

  .issues {
    display: grid;
    gap: var(--sp-1);
    padding: 0;
    list-style: none;
    color: var(--accent);
    font-size: var(--text-sm);
  }

  .issues li {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    line-height: 1.5;
  }

  .issues li :global(svg) {
    flex: none;
    transform: translateY(2px);
  }

  .preview-bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
  }

  .preview-bar .field {
    width: auto;
  }

  .preview {
    padding: var(--sp-4);
    border: 1px dashed var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    overflow-x: auto;
  }

  .preview.mono {
    font-size: var(--text-sm);
    white-space: pre-wrap;
  }

  .ptitle {
    margin-bottom: var(--sp-2);
    font-size: var(--text-lg);
    font-weight: 600;
  }

  .md :global(h1),
  .md :global(h2),
  .md :global(h3) {
    margin: var(--sp-3) 0 var(--sp-1);
    font-weight: 600;
  }

  .md :global(p),
  .md :global(ul),
  .md :global(ol),
  .md :global(table) {
    margin: 0 0 var(--sp-2);
    line-height: 1.55;
  }

  .md :global(ul),
  .md :global(ol) {
    padding-left: var(--sp-5);
  }

  .notice.err {
    color: var(--danger);
    font-size: var(--text-sm);
  }

  .editorfoot {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }
</style>
