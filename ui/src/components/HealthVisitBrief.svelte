<script lang="ts">
  // A page to bring to an appointment: what the readings, tests and
  // medicines have been doing since the last visit, written out as plain
  // Markdown from what is in the app.
  //
  // It is made on request, for a stretch of time chosen here, and nothing in
  // it is the model's opinion -- the numbers are the vault's own. It can be
  // read here, copied as Markdown into a message or a note, or printed.
  //
  // The Markdown is drawn by the same renderer the assistant's replies use,
  // which escapes everything before it writes a single tag, so what is put in
  // the page is safe by construction. Printing goes through a hidden frame
  // holding only the rendered brief and its own small stylesheet, so the
  // paper carries the brief and not the window around it.

  import { onDestroy } from 'svelte'
  import { health } from '../lib/health.svelte'
  import { renderMarkdown } from '../lib/markdown'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import { longDate } from '../lib/format'
  import { addMonths, todayIso } from '../lib/time'
  import type { ConditionId, VisitBrief } from '../lib/types'

  interface Props {
    /** The condition it is about; every condition when absent. */
    conditionId: ConditionId | null
    onclose: () => void
  }

  const { conditionId, onclose }: Props = $props()

  /** The last visit's date, from a visit note filed for this condition (or
   *  for none in particular), if there is one. Read once, when the dialog
   *  opens: the date field is the person's from then on. */
  function findLastVisit(): string | null {
    const today = todayIso()
    const dates = health.documents
      .filter(
        (d) =>
          d.kind === 'visitNote' &&
          !!d.date &&
          d.date <= today &&
          (conditionId === null ||
            d.conditionIds.length === 0 ||
            d.conditionIds.includes(conditionId)),
      )
      .map((d) => d.date ?? '')
      .sort()
    return dates.length > 0 ? (dates[dates.length - 1] ?? null) : null
  }
  const lastVisit = findLastVisit()

  let since = $state(lastVisit ?? addMonths(todayIso(), -3))
  let brief = $state<VisitBrief | null>(null)
  let busy = $state(false)
  let failed = $state(false)
  let copied = $state(false)
  let copiedTimer: ReturnType<typeof setTimeout> | null = null

  async function generate() {
    if (!since || busy) return
    busy = true
    failed = false
    try {
      brief = await health.visitBrief(conditionId, since)
      failed = brief === null
    } finally {
      busy = false
    }
  }

  async function copy() {
    if (!brief) return
    try {
      await navigator.clipboard.writeText(brief.markdown)
      copied = true
      if (copiedTimer) clearTimeout(copiedTimer)
      copiedTimer = setTimeout(() => {
        copiedTimer = null
        copied = false
      }, 1600)
    } catch {
      /* a clipboard the webview refuses is not worth a dialog */
    }
  }
  onDestroy(() => {
    if (copiedTimer) clearTimeout(copiedTimer)
  })

  const PRINT_CSS = `
    body { font: 12pt/1.5 system-ui, sans-serif; color: #111; margin: 2cm; }
    h1 { font-size: 18pt; margin: 0 0 0.6em; }
    h2 { font-size: 14pt; margin: 1.4em 0 0.4em; }
    h3 { font-size: 12pt; margin: 1.2em 0 0.3em; }
    ul, ol { padding-left: 1.4em; }
    li + li { margin-top: 0.2em; }
    table { border-collapse: collapse; width: 100%; margin: 0.6em 0; }
    th, td { border-bottom: 1px solid #bbb; padding: 4px 8px 4px 0; text-align: left; vertical-align: top; }
    hr { border: none; border-top: 1px solid #bbb; margin: 1.2em 0; }
    blockquote { margin-left: 0; padding-left: 0.9em; border-left: 2px solid #bbb; color: #444; }
    code { font-family: ui-monospace, monospace; font-size: 0.9em; }
  `

  function print() {
    if (!brief) return
    const frame = document.createElement('iframe')
    frame.setAttribute('aria-hidden', 'true')
    frame.setAttribute('tabindex', '-1')
    frame.style.cssText = 'position:fixed;right:0;bottom:0;width:0;height:0;border:0;'
    frame.srcdoc =
      '<!doctype html><html><head><meta charset="utf-8"><title>Visit brief</title>' +
      `<style>${PRINT_CSS}</style></head><body>${renderMarkdown(brief.markdown)}</body></html>`
    frame.onload = () => {
      try {
        frame.contentWindow?.focus()
        frame.contentWindow?.print()
      } finally {
        setTimeout(() => frame.remove(), 60_000)
      }
    }
    document.body.append(frame)
  }
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape' && !health.question) onclose()
  }}
/>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="scrim" onclick={onclose}></div>
<div
  class="sheet wide"
  role="dialog"
  aria-modal="true"
  aria-label="Prepare for a visit"
  use:trapFocus
>
  <div class="sheet-scroll">
    <h2 class="title">Prepare for a visit</h2>
    <p class="hint">
      A summary of what this app holds, for you to read and bring along. It is written from your own
      readings, tests and medicines — nothing in it is added by a model.
    </p>

    <form
      class="ask"
      onsubmit={(e) => {
        e.preventDefault()
        void generate()
      }}
    >
      <div class="ask-field">
        <label class="field-label" for="brief-since">Since</label>
        <input
          id="brief-since"
          class="field"
          type="date"
          max={todayIso()}
          bind:value={since}
          use:focusOnMount
        />
      </div>
      <button class="btn btn-primary" type="submit" disabled={busy || !since}>
        {busy ? 'Writing…' : brief ? 'Write it again' : 'Write the summary'}
      </button>
    </form>
    <p class="hint small">
      {#if lastVisit && since === lastVisit}
        From your last visit note, {longDate(lastVisit)}.
      {:else if !lastVisit}
        Three months back, until you file a visit note under Records.
      {/if}
    </p>

    {#if failed}
      <p class="error" role="alert">The summary could not be written. Try again.</p>
    {/if}

    {#if brief}
      <article class="brief md" aria-label="The summary">
        <!-- Safe by construction: `renderMarkdown` HTML-escapes its input
             before any pattern runs, so every tag that comes back was written
             by that module (see `lib/markdown.ts`). -->
        <!-- eslint-disable-next-line svelte/no-at-html-tags -->
        {@html renderMarkdown(brief.markdown)}
      </article>
    {/if}

    <p class="hint small">
      Targets are yours to set with your clinician. This is not medical advice.
    </p>
  </div>

  <div class="sheet-row">
    {#if brief}
      <button class="btn btn-outline" onclick={() => void copy()}>
        {copied ? 'Copied' : 'Copy as Markdown'}
      </button>
      <button class="btn btn-outline" onclick={print}>Print</button>
    {/if}
    <span class="spacer"></span>
    <button class="btn" onclick={onclose}>Close</button>
  </div>
</div>

<style>
  .sheet.wide {
    top: 6%;
    width: min(720px, calc(100vw - var(--sp-8)));
    max-height: 88vh;
  }

  .title {
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
    margin-bottom: var(--sp-2);
  }

  .ask {
    display: flex;
    align-items: flex-end;
    gap: var(--sp-3);
    margin-top: var(--sp-4);
  }
  .ask-field {
    flex: 0 1 220px;
  }
  .ask-field .field-label {
    margin-bottom: var(--sp-1);
  }

  .hint.small {
    margin: var(--sp-2) 0 0;
    font-size: var(--text-xs);
  }

  .error {
    margin: var(--sp-3) 0 0;
  }

  .brief {
    margin-top: var(--sp-4);
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    font-size: var(--text-base);
    line-height: var(--leading-normal);
    color: var(--fg);
    user-select: text;
  }

  .md :global(> * + *) {
    margin-top: 0.7em;
  }
  .md :global(h1),
  .md :global(h2),
  .md :global(h3),
  .md :global(h4) {
    font-size: var(--text-base);
    font-weight: 650;
    line-height: var(--leading-snug);
    margin-top: 1.3em;
  }
  .md :global(h1) {
    font-size: var(--text-md);
    margin-top: 0;
  }
  .md :global(strong) {
    font-weight: 650;
  }
  .md :global(ul),
  .md :global(ol) {
    padding-left: 1.35em;
  }
  .md :global(li + li) {
    margin-top: 0.25em;
  }
  .md :global(hr) {
    border: none;
    border-top: 1px solid var(--border);
    margin: 1.2em 0;
  }
  .md :global(blockquote) {
    margin-left: 0;
    padding-left: 0.9em;
    border-left: 2px solid var(--border-strong);
    color: var(--fg-muted);
  }
  .md :global(table) {
    border-collapse: collapse;
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
  }
  .md :global(th),
  .md :global(td) {
    padding: 4px var(--sp-4) 4px 0;
    text-align: left;
    vertical-align: top;
    border-bottom: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }
  .md :global(code) {
    font-family: var(--font-mono);
    font-size: 0.88em;
    padding: 0.1em 0.35em;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-sunken);
  }
</style>
