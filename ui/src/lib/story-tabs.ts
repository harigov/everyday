// What a story's pane has tabs for, and what each tab shows before there is
// anything to show in it.
//
// An episode is read as Notes (the feed's show notes), Transcript and Summary
// (the article written from the transcript); an article -- a web feed's
// story, a newsletter issue -- as Article and Summary. Each tab is a document
// from the news body route (`?part=`), drawn in the one sandboxed frame, so
// what this file decides is only which tabs there are, which one a pane opens
// on, and, for a tab whose document does not exist yet, whether it shows the
// work that is making it or offers to start that work. The pane's component
// draws; the rules are here so a Summary that says "Summarise" for an episode,
// or never offers to try again after a failure, is something a test can see.

import { workActive, workLine } from './podcast'
import type { Story, StoryPart, WorkItem } from './types'

export type StoryTab = 'article' | 'notes' | 'transcript' | 'summary'

/** Whether the story is a recording or something to read: the two have different tabs. */
export type TabKind = 'episode' | 'article'

export const TABS: Record<TabKind, { id: StoryTab; label: string }[]> = {
  episode: [
    { id: 'notes', label: 'Notes' },
    { id: 'transcript', label: 'Transcript' },
    { id: 'summary', label: 'Summary' },
  ],
  article: [
    { id: 'article', label: 'Article' },
    { id: 'summary', label: 'Summary' },
  ],
}

export function tabKind(story: Pick<Story, 'media'>): TabKind {
  return story.media ? 'episode' : 'article'
}

/** The tabs a story has, in order. */
export function tabsOf(story: Pick<Story, 'media'>): { id: StoryTab; label: string }[] {
  return TABS[tabKind(story)]
}

/** The tab a pane opens on when nothing is remembered: what the feed itself published. */
export function defaultTab(kind: TabKind): StoryTab {
  return kind === 'episode' ? 'notes' : 'article'
}

/** `raw` as a tab of this kind, or the default when it is not one -- a stale value, or another kind's. */
export function tabOr(kind: TabKind, raw: string | null | undefined): StoryTab {
  return TABS[kind].find((t) => t.id === raw)?.id ?? defaultTab(kind)
}

/**
 * Which document of the body route a tab is, or `undefined` for the one that
 * is the body as it is: an article's own tab is the whole of it, not a part.
 */
export function partOf(tab: StoryTab): StoryPart | undefined {
  return tab === 'article' ? undefined : tab
}

// ── Remembered on this device ───────────────────────────────────────────
//
// Per kind, since somebody who reads the summaries of articles and the notes
// of episodes should not find one of those choices has changed the other. Kept
// as chrome is -- in the browser's own storage, not the vault -- and read
// whole each time a pane opens rather than held, so a second window sees it.

const TAB_KEYS: Record<TabKind, string> = {
  episode: 'jatayu.feed.tab.episode',
  article: 'jatayu.feed.tab.article',
}

export function recallTab(kind: TabKind): StoryTab {
  try {
    return tabOr(kind, localStorage.getItem(TAB_KEYS[kind]))
  } catch {
    return defaultTab(kind)
  }
}

export function rememberTab(kind: TabKind, tab: StoryTab): void {
  try {
    localStorage.setItem(TAB_KEYS[kind], tab)
  } catch {
    // Not kept; this window still has it.
  }
}

// ── What a tab shows ────────────────────────────────────────────────────

/** What is known of a story that decides what its tabs hold. */
export interface TabFacts {
  /** The transcript is there to be read: linked, and its file still in Files. */
  transcript: boolean
  /** There is a summary: for an episode, its article has been written. */
  summary: boolean
  /** The work asked for the story, of each kind, if any. */
  transcribing?: WorkItem
  writing?: WorkItem
  summarising?: WorkItem
}

/** What an empty tab offers to do. */
export type TabAction = 'transcribe' | 'write' | 'summarise'

export type TabPanel =
  /** The document is there: draw it. */
  | { type: 'content' }
  /** The document is being made: say how far along it is. */
  | { type: 'working'; kind: WorkItem['kind']; line: string }
  /** There is none: offer to make it, and say why the last try failed if it did. */
  | { type: 'offer'; action: TabAction; failed: boolean; error: string | null }

function working(item: WorkItem): TabPanel {
  return { type: 'working', kind: item.kind, line: workStatus(item) }
}

/** Offer `action`, as a retry with the reason when `last` is the attempt that failed. */
function offer(action: TabAction, last: WorkItem | undefined): TabPanel {
  const failed = last?.state === 'failed'
  return { type: 'offer', action, failed, error: failed ? (last?.error ?? null) : null }
}

/**
 * What the tab `tab` of a story of this kind shows, given what there is.
 *
 * A piece of work still to be done wins over an offer to start it, so a
 * button is never drawn for what is already going; one that failed is offered
 * again, with the reason. An episode's article is written from its transcript,
 * so while the transcript is still being made that is what its Summary tab
 * shows, rather than a button that would only join the queue behind it.
 */
export function panelFor(kind: TabKind, tab: StoryTab, facts: TabFacts): TabPanel {
  if (tab === 'article' || tab === 'notes') return { type: 'content' }

  if (tab === 'transcript') {
    if (facts.transcript) return { type: 'content' }
    if (facts.transcribing && workActive(facts.transcribing)) return working(facts.transcribing)
    return offer('transcribe', facts.transcribing)
  }

  if (facts.summary) return { type: 'content' }
  if (kind === 'article') {
    if (facts.summarising && workActive(facts.summarising)) return working(facts.summarising)
    return offer('summarise', facts.summarising)
  }
  if (facts.writing && workActive(facts.writing)) return working(facts.writing)
  if (!facts.transcript && facts.transcribing && workActive(facts.transcribing)) {
    return working(facts.transcribing)
  }
  // No transcript for the article to come from and none being made: the reason
  // that failed is as much the article's as the transcript's.
  const last =
    facts.writing?.state === 'failed'
      ? facts.writing
      : !facts.transcript && facts.transcribing?.state === 'failed'
        ? facts.transcribing
        : undefined
  return offer('write', last)
}

/** The button's words: what it does, or that the last try failed and this is another. */
export function offerLabel(action: TabAction, failed: boolean): string {
  switch (action) {
    case 'transcribe':
      return failed ? 'Transcription failed, try again' : 'Transcribe'
    case 'write':
      return failed ? 'Writing failed, try again' : 'Write the article'
    case 'summarise':
      return failed ? 'Summarising failed, try again' : 'Summarise'
  }
}

/** What an empty tab says about itself, above its button. */
export function offerText(action: TabAction): string {
  switch (action) {
    case 'transcribe':
      return 'This episode has no transcript yet. Transcribing it writes down what was said, and keeps it in Files.'
    case 'write':
      return 'An article written from what was said in the episode. If it has not been transcribed yet, that is done first.'
    case 'summarise':
      return 'A short summary of this story, kept in Files.'
  }
}

/** "Summarising 40%", "Waiting to summarise": the words the Up Next work list uses too. */
export const workStatus = workLine
