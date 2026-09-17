// What the person is looking at, in one line -- the sentence `ChatPanel`
// sends with every message so "this", "here" and "that one" resolve.
//
// Pulled out of `ChatPanel.svelte`'s `$derived.by`, which is the same move
// `dashboard.ts` made for the Overview: the rules are a plain function of a
// few facts, and the way they fail is quiet -- a case that falls through to
// the journal's answer does not throw or fail to compile, it just tells the
// assistant the wrong app is open. That is worth a test that does not need a
// component, a store, or a browser to run.
//
// `ChatContext` takes *values*, not store objects: a resolved title, a
// resolved label, never `todo` or `purpose` themselves. Two reasons. First,
// testability -- every branch below is reachable with a plain object literal.
// Second, this module is imported from `apps.ts`, which every app's store
// imports back through `state.svelte.ts`; a store passed in here instead of
// read out of one would make this file part of that cycle. See `apps.ts`'s
// own note on the same shape of problem.

import type { Section } from './state.svelte'
import type { WidgetType } from './dashboard'
import { specOf } from './dashboard'

export interface ChatContext {
  section: Section
  todo: {
    showingGoals: boolean
    /** The selected goal's title, only meaningful while `showingGoals`. */
    goalTitle: string | null
    projectName: string | null
  }
  library: { shelfName: string | null }
  notes: { openTitle: string | null }
  overview: { widgets: { type: WidgetType }[] }
  assistant: { paneLabel: string }
  mail: {
    /** The open thread's subject, or `null` when none is selected. */
    subject: string | null
    /** The current mailbox or label's name. */
    mailboxName: string | null
  }
  /** The journal's selected entry, or `null` when there is none. */
  entry: { title?: string | null; localDate: string } | null
}

/**
 * The one line sent with every message. See this module's own header for why
 * it takes resolved values rather than the stores that hold them.
 */
export function chatContext(ctx: ChatContext): string {
  switch (ctx.section) {
    case 'todo':
      if (ctx.todo.showingGoals) {
        return ctx.todo.goalTitle
          ? `the todo app's goals, the goal "${ctx.todo.goalTitle}"`
          : "the todo app's goals, grouped by role"
      }
      return ctx.todo.projectName
        ? `the todo app, project "${ctx.todo.projectName}"`
        : 'the todo app'
    case 'calendar':
      return 'the calendar'
    case 'library':
      return ctx.library.shelfName ? `the library, shelf "${ctx.library.shelfName}"` : 'the library'
    case 'notes':
      return ctx.notes.openTitle
        ? `the notes app, the note "${ctx.notes.openTitle}"`
        : 'the notes app'
    case 'overview': {
      // Named rather than described: it is a page of whatever cards its
      // owner put on it now, so "where the week adds up by role" would be a
      // claim about somebody else's page. An empty page is one somebody is
      // allowed to have, and saying "showing" followed by nothing at all
      // would be the assistant told a sentence that stops mid-word.
      const cards = ctx.overview.widgets.map((w) => specOf(w.type).label.toLowerCase()).slice(0, 6)
      return cards.length > 0
        ? `their overview page, showing ${cards.join(', ')}`
        : 'their overview page, which they have not put anything on yet'
    }
    case 'assistant':
      return `your own routines and what they did, on the "${ctx.assistant.paneLabel}" page`
    // TODO(0.7): no `case 'mail'` yet -- an open thread or a mailbox falls
    // through to the journal's answer below, which is wrong on a screen that
    // has no entry at all. Phase 1 adds the case; this phase only pins the
    // bug down so it cannot get worse by accident on the way there.
    default: {
      const entry = ctx.entry
      if (!entry) return 'the journal'
      const title = entry.title?.trim()
      return title
        ? `the journal, entry "${title}" dated ${entry.localDate}`
        : `the journal, an entry dated ${entry.localDate}`
    }
  }
}
