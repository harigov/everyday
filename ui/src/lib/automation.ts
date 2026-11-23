// What a source does by itself, and how long it keeps its stories: the rules
// behind the "Automation" and "Clean up after" sections of a source's
// settings.
//
// Both are a choice between the kind's default and the source's own, and a
// form has to be able to say "the default" as a choice of its own. Saving
// that as no choice at all -- `summarize: null`, `retention: null` -- is what
// lets a source nobody has set anything on go on following the default when it
// changes, instead of keeping a copy of whatever the default was the day the
// form was last saved. That is also what the service's `NewsSource::summarizes`
// and `NewsSource::retention_with` do in reading them back.

import { DEFAULT_RETENTION, clampDays, kindOf } from './feed'
import type { Automation, CleanupDefaults, NewsSource, Retention } from './types'

// ── Automation ──────────────────────────────────────────────────────────

/** Whether a source of this kind summarises its stories when it has not been told: a podcast does. */
export function summarizeDefault(source: NewsSource): boolean {
  return kindOf(source) === 'podcast'
}

/** Whether the source summarises each new story: what it was told, else the kind's default. */
export function summarizes(source: NewsSource): boolean {
  return source.automation?.summarize ?? summarizeDefault(source)
}

export interface AutomationForm {
  summarize: boolean
  audio: boolean
}

export function automationForm(source: NewsSource): AutomationForm {
  return { summarize: summarizes(source), audio: source.automation?.audio === true }
}

/**
 * The automation to store for this form. `summarize` is `null` when the
 * switch agrees with the kind's default -- never a copy of it -- and `audio`
 * is off for a podcast, which has no article to make into audio.
 */
export function automationFromForm(source: NewsSource, form: AutomationForm): Automation {
  return {
    summarize: form.summarize === summarizeDefault(source) ? null : form.summarize,
    audio: kindOf(source) === 'podcast' ? false : form.audio,
  }
}

// ── Clean-up ────────────────────────────────────────────────────────────

/**
 * What a source keeps its stories for when it has not said: the default for
 * its kind. Newsletters have their own; feeds, Hacker News and podcasts share
 * the other. Before the settings have been read, the stock numbers.
 */
export function defaultRetention(
  source: NewsSource,
  cleanup: CleanupDefaults | null | undefined,
): Retention {
  if (!cleanup) return { ...DEFAULT_RETENTION }
  return kindOf(source) === 'newsletter' ? cleanup.newsletters : cleanup.feeds
}

/** "30 days read, 90 unread": a retention in the words the options use. */
export function retentionLabel(retention: Retention): string {
  const days = (n: number) => `${n} ${n === 1 ? 'day' : 'days'}`
  return `${days(retention.readDays)} read, ${retention.unreadDays} unread`
}

export interface RetentionForm {
  /** The source's own numbers, rather than the default's. */
  custom: boolean
  readDays: number | null
  unreadDays: number | null
}

/** A source with a retention of its own opens on it; any other on the default, with its numbers ready to edit. */
export function retentionForm(
  source: NewsSource,
  cleanup: CleanupDefaults | null | undefined,
): RetentionForm {
  const own = source.retention ?? null
  const fallback = defaultRetention(source, cleanup)
  return {
    custom: own !== null,
    readDays: (own ?? fallback).readDays,
    unreadDays: (own ?? fallback).unreadDays,
  }
}

/** The retention to store: none for "the default", else the numbers, each whole and within a year. */
export function retentionFromForm(
  form: RetentionForm,
  source: NewsSource,
  cleanup: CleanupDefaults | null | undefined,
): Retention | null {
  if (!form.custom) return null
  const fallback = defaultRetention(source, cleanup)
  return {
    readDays: clampDays(form.readDays, fallback.readDays),
    unreadDays: clampDays(form.unreadDays, fallback.unreadDays),
  }
}

// ── Mail ────────────────────────────────────────────────────────────────

/** Whether the source is one sender's mail -- followed or found -- as opposed to every newsletter at once. */
export function isMailSender(source: NewsSource): boolean {
  return source.kind.type === 'mail'
}

/** Whether `a` and `b` are the same newsletter: one sender, one display name, compared as the service does. */
export function sameNewsletter(
  a: { sender: string; name?: string },
  b: { sender: string; name?: string },
): boolean {
  const norm = (s: string | undefined) => (s ?? '').trim().toLowerCase()
  return norm(a.sender) === norm(b.sender) && norm(a.name) === norm(b.name)
}
