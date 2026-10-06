// The one table of per-app facts, replacing the half-dozen lists that used
// to repeat them: `SECTIONS` and `canShow` in `state.svelte.ts`, `APPS` in
// `AppBar.svelte`, `ACCENTS` in `App.svelte`, and `CREATE` in
// `shortcuts.svelte.ts`. Adding an app used to mean finding all of those by
// hand; this does not compile until the new section is in `APPS` below.
//
// Phase 3 of docs/plans/architecture-refactor.md. What did *not* move here:
// the eight "Go to" keyboard rows and `GROUPS`' own ordering in
// `shortcuts.svelte.ts` are a different order from `APP_ORDER` below (the
// tray and help sheet read top to bottom by *menu* heading, not by app-bar
// position), and generating one from the other would reorder a menu nobody
// asked to have reordered. They stay hand-written.
//
// # Why this file imports no store
//
// Every app's own store -- `todo`, `library`, `mail`, ... -- imports `app`
// from `state.svelte.ts`, the way `App.svelte`'s own note on `purpose`
// explains: "a store `app` also read would be a cycle, and a cycle between
// two modules that both construct singletons at import time is how a screen
// ends up blank with one line in the console." `state.svelte.ts` has to
// import *this* file, for `canShow`, so this file importing any store back
// would close exactly that loop.
//
// The way out: take values, or take a *type* rather than a value. `import type { app } from './state.svelte'`
// below costs nothing at runtime -- type-only imports are erased -- so
// `AppLike` can describe the shape of `app` without this module ever
// evaluating `state.svelte.ts`. Every function here is called by a
// component or a store that already imported the real thing directly, and
// hands it over as a plain argument.

import type { Group } from './shortcuts.svelte'
import type { IconName } from './icons'
import type { app as appSingleton } from './state.svelte'
import type { todo as todoSingleton } from './todo.svelte'
import type { library as librarySingleton } from './library.svelte'
import type { notes as notesSingleton } from './notes.svelte'
import type { calendar as calendarSingleton } from './calendar.svelte'
import type { mail as mailSingleton } from './mail.svelte'
import type { overview as overviewSingleton } from './overview.svelte'
import type { assistant as assistantSingleton } from './assistant.svelte'
import type { agent as agentSingleton } from './agent.svelte'
import type { Showing } from './onscreen'

type AppLike = typeof appSingleton
type TodoLike = typeof todoSingleton
type LibraryLike = typeof librarySingleton
type NotesLike = typeof notesSingleton
type CalendarLike = typeof calendarSingleton
type MailLike = typeof mailSingleton
type OverviewLike = typeof overviewSingleton
type AssistantLike = typeof assistantSingleton
type AgentLike = typeof agentSingleton

/**
 * The order every app appears in -- the app bar's cycling order
 * (`app.nextSection`), and the order `Record<Section, AppModule>` below is
 * keyed by. `AppBar.svelte`'s own on-screen order and `GROUPS`' menu order
 * are separate lists, on purpose: see this file's own header.
 */
export const APP_ORDER = [
  'overview',
  'notes',
  'todo',
  'calendar',
  'library',
  'mail',
  'assistant',
  'journal',
] as const
export type Section = (typeof APP_ORDER)[number]

/** The stores an app's own entry reads what it is showing from -- handed
 *  over by `screen.svelte.ts`, which already imported every one of them. */
export interface AppDeps {
  app: AppLike
  notes: NotesLike
  todo: TodoLike
  calendar: CalendarLike
  library: LibraryLike
  mail: MailLike
  overview: OverviewLike
}

/**
 * An app's own search, as the bar at the top of the window types into it.
 *
 * Built fresh from the stores each time it is asked for, so it can depend
 * on where in the app somebody is -- the todo app's goals pane has no task
 * list to filter, and says so by having no search. `query` is the store's
 * own, so each app keeps what was typed into it and switching back puts it
 * back in the bar; `set` is the store's own debounced setter, exactly what
 * the field it replaced called.
 */
export interface AppSearch {
  /** What the empty bar says, e.g. "Search mail". */
  placeholder: string
  query: string
  set(query: string): void
}

/** What `App.svelte`'s accent needs from the stores that hold a colour. */
export interface AccentDeps {
  app: AppLike
  todo: TodoLike
  library: LibraryLike
}

/** What `shortcuts.svelte.ts`'s "start the next thing" needs from every
 *  store an app's create action might call. */
export interface CreateDeps {
  app: AppLike
  notes: NotesLike
  todo: TodoLike
  calendar: CalendarLike
  library: LibraryLike
  mail: MailLike
  overview: OverviewLike
  assistant: AssistantLike
  /** The conversation, which is what the Assistant app starts a new one of. */
  agent: AgentLike
  /** The todo app's goals pane has no task line to put a cursor in -- see
   *  `shortcuts.svelte.ts`'s own `focusNewGoal`. */
  focusNewGoal: () => void
}

export interface AppModule {
  readonly section: Section
  /** The tab's label, and the heading its quick actions are filed under --
   *  see `AppBar.svelte`'s own note on why those are one word, typed once. */
  readonly label: Group
  readonly icon: IconName
  /** Can this vault's backend run the app? */
  supported(app: AppLike): boolean
  /** The colour the window tints itself while this app is open. */
  accent(deps: AccentDeps): string
  /** What "start the next thing" does here. */
  create(deps: CreateDeps): unknown
  /** What is on screen in this app, sent to the assistant with every
   *  message -- see `onscreen.ts`. Each store answers for itself; this is
   *  only where an app that forgot to cannot compile. */
  onScreen(deps: AppDeps): Showing
  /** The app's search, which the bar at the top of the window types into,
   *  or `null` where there is nothing to search -- the bar is then commands
   *  and quick capture only. See `AppSearch`. */
  search(deps: AppDeps): AppSearch | null
}

const CONSTANT_ACCENT = 'var(--accent)'

export const APPS: Record<Section, AppModule> = {
  overview: {
    section: 'overview',
    label: 'Overview',
    icon: 'compass',
    supported: (app) => app.supportsOverview,
    accent: () => CONSTANT_ACCENT,
    create: (d) => (d.overview.wantsLog = true),
    onScreen: (d) => d.overview.showing,
    search: () => null,
  },
  notes: {
    section: 'notes',
    label: 'Notes',
    icon: 'pencil',
    supported: (app) => app.supportsNotes,
    accent: () => CONSTANT_ACCENT,
    create: (d) => d.notes.create(),
    onScreen: (d) => d.notes.showing,
    search: (d) => ({
      placeholder: 'Search notes',
      query: d.notes.query,
      set: (q) => d.notes.setQuery(q),
    }),
  },
  todo: {
    section: 'todo',
    label: 'Todo',
    icon: 'check',
    supported: (app) => app.supportsTasks,
    accent: (d) => d.todo.accent,
    // The goals pane has no task line to put a cursor in, and the next
    // thing somebody wants there is a goal.
    create: (d) => (d.todo.showingGoals ? d.focusNewGoal() : d.todo.focusCapture()),
    onScreen: (d) => d.todo.showing,
    // The goals pane lists goals, not tasks, so there is nothing for a task
    // filter to narrow there.
    search: (d) =>
      d.todo.showingGoals
        ? null
        : {
            placeholder: d.todo.project ? `Filter tasks in ${d.todo.project.name}` : 'Filter tasks',
            query: d.todo.filter,
            set: (q) => d.todo.setFilter(q),
          },
  },
  calendar: {
    section: 'calendar',
    label: 'Calendar',
    icon: 'calendar',
    supported: (app) => app.supportsCalendar,
    accent: () => CONSTANT_ACCENT,
    create: (d) => d.calendar.bookNow(),
    onScreen: (d) => d.calendar.showing,
    search: () => null,
  },
  library: {
    section: 'library',
    label: 'Library',
    icon: 'book',
    supported: (app) => app.supportsLibrary,
    accent: (d) => d.library.accent,
    create: (d) => d.library.focusCapture(),
    onScreen: (d) => d.library.showing,
    search: (d) => ({
      placeholder: d.library.kind ? `Search ${d.library.kind.name}` : 'Search the library',
      query: d.library.query,
      set: (q) => d.library.setQuery(q),
    }),
  },
  mail: {
    section: 'mail',
    label: 'Mail',
    icon: 'mail',
    supported: (app) => app.supportsMail,
    accent: () => CONSTANT_ACCENT,
    create: (d) => d.mail.compose(),
    onScreen: (d) => d.mail.showing,
    search: (d) => ({
      placeholder: 'Search mail',
      query: d.mail.searchQuery,
      set: (q) => d.mail.setSearchQuery(q),
    }),
  },
  assistant: {
    section: 'assistant',
    label: 'Assistant',
    icon: 'sparkle',
    supported: (app) => app.supportsAssistant,
    accent: () => CONSTANT_ACCENT,
    // A new conversation. It was a new routine, back when routines were
    // what this app was made of; they are in Settings now, and the next
    // thing anybody starts on a page that is a conversation is another one.
    create: (d) => d.agent.startThread(),
    // The conversation is the whole page, so there is nothing else on
    // screen to name and nothing to search.
    onScreen: () => ({}),
    search: () => null,
  },
  journal: {
    section: 'journal',
    label: 'Journal',
    icon: 'quote',
    // The one app every backend can carry: a store that cannot hold
    // journals is not a vault.
    supported: () => true,
    accent: (d) => d.app.accent,
    create: (d) => d.app.newEntry(),
    onScreen: (d) => d.app.journalShowing,
    search: (d) => ({
      placeholder: d.app.journal ? `Search ${d.app.journal.name}` : 'Search the journal',
      query: d.app.query,
      set: (q) => d.app.setQuery(q),
    }),
  },
}
