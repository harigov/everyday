// Pure helpers for writing help: no store, no `Editor`, no network -- see
// `scripts/mailwrite.test.mjs`. Three unrelated small things live here
// rather than three files, the way `mail.ts` already holds several:
//
//   - `splitQuoted`/`joinQuoted` -- where a reply or forward draft's own
//     text ends and the quoted parent begins, so `MailCompose.svelte` can
//     rewrite the one without touching the other.
//   - `htmlToPlainText`/`escapeHtml` -- the two directions between a
//     `Draft.bodyHtml` fragment and the plain text `draft_with_ai`,
//     `improve_writing` and a TipTap selection all actually deal in.
//   - `sendLaterChoices` -- the send-later popover's fixed presets, each a
//     label and the instant it means right now.

/**
 * Where a reply/forward draft's own text ends and the quoted parent begins.
 *
 * `new_draft` always writes the quoted parent as one paragraph -- "On
 * {date}, {name} wrote:" -- immediately followed by a `<blockquote>` with
 * nothing of the person's own after it (`compose::quote_html` in
 * `everyday-mail/src/compose.rs`; `compose::quote_reply` in
 * `everyday-core/src/mail/compose.rs` for the assistant's own tool, in the
 * same shape). That pair, anchored to the *end* of the string so a
 * blockquote nested inside an older quote cannot end the match early, is
 * the only seam worth matching on: the attribution text itself is a name
 * and a date, both untrusted and neither worth parsing.
 *
 * Once this has been through TipTap once, `<blockquote type="cite">` comes
 * back as a bare `<blockquote>` -- StarterKit's node has no attribute spec
 * for `type` -- so the pattern does not ask for one. TipTap also leaves an
 * empty `<p></p>` after a blockquote that ends the document, so the caret
 * has somewhere to go below it; those trailing empty paragraphs are part of
 * the quote's tail, not the person's own words, and without allowing for
 * them nothing that had been through the editor ever matched at all.
 *
 * The attribution is one paragraph, and the match never runs past its end:
 * a reply that itself begins "On Monday we could…" would otherwise start
 * the match there, and everything the person wrote would be taken for the
 * quote -- folded away under it, out of reach of their own editor.
 */
const ATTRIBUTION_AND_QUOTE =
  /<p>On (?:(?!<\/p>)[^])*? wrote:<\/p>\s*<blockquote[^>]*>[^]*<\/blockquote>(?:\s*<p>(?:<br\s*\/?>)?<\/p>)*\s*$/i

export interface SplitBody {
  own: string
  quoted: string
}

/** No quote found -- an ordinary new message, or a draft not yet quoting
 *  anything -- answers `{ own: html, quoted: '' }`, so `joinQuoted` round-trips
 *  it unchanged. */
export function splitQuoted(html: string): SplitBody {
  const m = ATTRIBUTION_AND_QUOTE.exec(html)
  if (!m) return { own: html, quoted: '' }
  return { own: html.slice(0, m.index), quoted: html.slice(m.index) }
}

/** The exact inverse of `splitQuoted`: `own` and `quoted` are concatenated
 *  with nothing between them, the same way `quote_html` builds `body_html`
 *  by prefixing it to the quote rather than joining the two with a separator. */
export function joinQuoted(own: string, quoted: string): string {
  return own + quoted
}

/**
 * A rough plain-text reading of an HTML fragment -- `currentText` for
 * `draft_with_ai`, and the own part of a draft fed to `improve_writing`
 * when nothing is selected.
 *
 * Regex rather than a detached `<div>`: this runs in `scripts/*.test.mjs`
 * under plain Node, with no DOM to parse into, and the own part of a
 * compose draft is never more than a few short paragraphs -- exactly what a
 * handful of substitutions are good enough for. Block closings become a
 * newline before their tags are stripped, so "one paragraph, then another"
 * survives as two lines rather than running together.
 */
export function htmlToPlainText(html: string): string {
  if (!html) return ''
  return html
    .replace(/<br\s*\/?>/gi, '\n')
    .replace(/<\/(p|div|li|h[1-6]|blockquote|tr)>/gi, '\n\n')
    .replace(/<[^>]*>/g, '')
    .replace(/&nbsp;/gi, ' ')
    .replace(/&lt;/gi, '<')
    .replace(/&gt;/gi, '>')
    .replace(/&quot;/gi, '"')
    .replace(/&#39;/gi, "'")
    .replace(/&amp;/gi, '&')
    .replace(/\n{3,}/g, '\n\n')
    .trim()
}

/** The other direction -- plain text (a selection read through
 *  `editor.state.doc.textBetween`, or an AI answer's `bodyText`) made safe
 *  to hand back to TipTap as content, which otherwise reads a stray `&` or
 *  `<` in what somebody wrote as the start of a tag. */
export function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;')
}

// ── Send later ───────────────────────────────────────────────────────────

export interface SendLaterChoice {
  key: string
  label: string
  at: Date
}

function atClock(base: Date, hours: number, minutes = 0): Date {
  const d = new Date(base)
  d.setHours(hours, minutes, 0, 0)
  return d
}

function addDays(base: Date, days: number): Date {
  const d = new Date(base)
  d.setDate(d.getDate() + days)
  return d
}

/** `at`, to the nearest hour -- half past or later rounds up, same as a
 *  reader would say "half six" is "closer to seven" when asked for a round
 *  number. What "Later today"'s `+3h` is shown as, rather than a preset
 *  that reads "4:37 PM". */
function roundToHour(at: Date): Date {
  const rounded = new Date(at)
  rounded.setMinutes(0, 0, 0)
  if (at.getMinutes() >= 30) rounded.setHours(rounded.getHours() + 1)
  return rounded
}

/** Days from `now` to the coming Monday -- 7 when `now` already is one, so
 *  "Monday morning" always means a week out rather than today. Callers
 *  exclude Sunday themselves, since "Monday morning" and "Tomorrow morning"
 *  would otherwise be the same preset twice. */
function daysUntilNextMonday(now: Date): number {
  return (1 - now.getDay() + 7) % 7 || 7
}

/**
 * The send-later popover's fixed presets -- "Later today", "This evening",
 * "Tomorrow morning", "Tomorrow afternoon" and, except on a Sunday, "Monday
 * morning" -- each a plain label and the instant it means right now; a
 * caller shows that instant however it likes (`MailCompose.svelte` reads it
 * through `timeOfDay`). Exactly this signature: the scheduled-send agent
 * reuses it for rescheduling an already-queued draft, not only for a fresh
 * one about to be sent.
 *
 * "Later today" and "This evening" drop off the list once they would read
 * as backwards -- past the hour named in parentheses below, not past the
 * time each would actually land on, so a `+3h` that happens to roll past
 * dinner is still offered right up until it would have been offered a
 * moment too late to mean anything by "today".
 */
export function sendLaterChoices(now: Date = new Date()): SendLaterChoice[] {
  const choices: SendLaterChoice[] = []

  if (now < atClock(now, 18)) {
    const at = roundToHour(new Date(now.getTime() + 3 * 3_600_000))
    choices.push({ key: 'laterToday', label: 'Later today', at })
  }
  if (now < atClock(now, 17)) {
    choices.push({ key: 'thisEvening', label: 'This evening', at: atClock(now, 18) })
  }

  const tomorrow = addDays(now, 1)
  choices.push({ key: 'tomorrowMorning', label: 'Tomorrow morning', at: atClock(tomorrow, 8) })
  choices.push({
    key: 'tomorrowAfternoon',
    label: 'Tomorrow afternoon',
    at: atClock(tomorrow, 13),
  })

  if (now.getDay() !== 0) {
    const monday = addDays(now, daysUntilNextMonday(now))
    choices.push({ key: 'mondayMorning', label: 'Monday morning', at: atClock(monday, 8) })
  }

  return choices
}
