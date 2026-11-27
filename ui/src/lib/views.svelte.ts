/**
 * Each app's view and sidebar, loaded the first time it is shown.
 *
 * Every view used to be imported by `App.svelte` and every nav by
 * `Sidebar.svelte`, so the whole interface -- the editor and TipTap, the
 * calendar grid, Money's charts, every Settings page -- was one script parsed
 * on every launch, by every window of one app and by a phone's cold start
 * alike, to draw the one app that was open. Now each is its own chunk, asked
 * for here.
 *
 * The stores are not: `App.svelte` still imports every one of them, because
 * importing a store is what registers its lock and flush hooks. Only what
 * draws is loaded late.
 *
 * Switching to an app whose view has not loaded yet keeps the app being left
 * on screen until it has -- `stage` below -- for `STAGE_WAIT_MS` at most, so
 * a switch is never a blank pane in between. The apps used most are fetched
 * when the window has nothing else to do (`warm`), and an app's button in the
 * bar fetches its own when the pointer reaches it, so that wait is rarely
 * there to be had.
 */
import { untrack, type Component } from 'svelte'
import { APP_ORDER, type Section } from './apps'
import { pinned } from './appwindow'

/** What is drawn in the stage, or over it, and loaded on first use. */
export type ViewKey = Section | 'reader' | 'settings' | 'shortcuts'

type Loader = () => Promise<{ default: Component }>

const VIEWS: Record<ViewKey, Loader> = {
  overview: () => import('../components/OverviewView.svelte'),
  notes: () => import('../components/NotesView.svelte'),
  todo: () => import('../components/TodoView.svelte'),
  calendar: () => import('../components/CalendarView.svelte'),
  library: () => import('../components/LibraryView.svelte'),
  mail: () => import('../components/MailView.svelte'),
  feed: () => import('../components/FeedView.svelte'),
  money: () => import('../components/MoneyView.svelte'),
  screentime: () => import('../components/ScreenTimeView.svelte'),
  files: () => import('../components/FilesView.svelte'),
  health: () => import('../components/HealthView.svelte'),
  assistant: () => import('../components/AssistantView.svelte'),
  journal: () => import('../components/JournalView.svelte'),
  reader: () => import('../components/ReaderView.svelte'),
  settings: () => import('../components/SettingsView.svelte'),
  shortcuts: () => import('../components/ShortcutsHelp.svelte'),
}

/** The sidebars that are a component of their own. The journal's is drawn by
 *  `Sidebar.svelte` itself, and the Overview has none. */
type NavKey = Exclude<Section, 'journal' | 'overview'>

const NAVS: Record<NavKey, Loader> = {
  notes: () => import('../components/NotesNav.svelte'),
  todo: () => import('../components/TodoNav.svelte'),
  calendar: () => import('../components/CalendarNav.svelte'),
  library: () => import('../components/LibraryNav.svelte'),
  mail: () => import('../components/MailNav.svelte'),
  feed: () => import('../components/FeedNav.svelte'),
  money: () => import('../components/MoneyNav.svelte'),
  screentime: () => import('../components/ScreenTimeNav.svelte'),
  files: () => import('../components/FilesNav.svelte'),
  health: () => import('../components/HealthNav.svelte'),
  assistant: () => import('../components/AssistantNav.svelte'),
}

function hasNav(key: ViewKey): key is NavKey {
  return key in NAVS
}

/** A view's chunk, and its sidebar's when it has one. */
type Chunks = [{ default: Component }, { default: Component } | null]

function fetchChunks(key: ViewKey): Promise<Chunks> {
  return Promise.all([VIEWS[key](), hasNav(key) ? NAVS[key]() : null])
}

/** The longest the app being left stays on screen while the next one loads. */
const STAGE_WAIT_MS = 150

/** The apps last shown in the main window, most recent first. */
const RECENT = 'jatayu.recentApps'
const RECENT_KEPT = 4
/** How many of those are fetched ahead once the window has settled. */
const WARMED = 3

/**
 * Run `fn` once the window has nothing else to do, or after `ms` at the
 * latest. WebKit has no `requestIdleCallback`, so a timer stands in there.
 */
export function whenIdle(fn: () => void, ms = 1500): void {
  if (typeof requestIdleCallback === 'function') requestIdleCallback(() => fn(), { timeout: ms })
  else setTimeout(fn, ms)
}

/** Where `reloadForNewBuild` notes the build it last reloaded from. */
const RELOADED = 'jatayu.reloadedFrom'
/** The longest a reload waits for what is outstanding to be written. */
const FLUSH_WAIT_MS = 2000

/**
 * Reload the page to pick up a newer build: once per build in this tab.
 *
 * Web access serves the build running now, and a tab opened before an update
 * still holds the page it was given then, which names each app's chunk by a
 * hash the server no longer has. The first app the tab had not opened yet
 * fails to load, and asking again asks for the same missing file. A reload
 * fetches the page afresh, chunk names and all.
 *
 * Only once for the build this page came from -- this module's own address,
 * which carries its chunk's hash, and that hash changes with the name of any
 * chunk it imports. A page reloaded from the same build that still cannot
 * load a chunk would reload for ever; it shows the failure instead. `flush`
 * writes anything outstanding first. Returns whether it is reloading.
 */
export function reloadForNewBuild(flush: () => Promise<unknown>): boolean {
  const build = import.meta.url
  try {
    if (sessionStorage.getItem(RELOADED) === build) return false
    sessionStorage.setItem(RELOADED, build)
  } catch {
    // Nowhere to note it, so no way to know it has not been tried already.
    return false
  }
  const waited = new Promise((resolve) => setTimeout(resolve, FLUSH_WAIT_MS))
  void Promise.race([flush().catch(() => {}), waited]).then(() => location.reload())
  return true
}

function recent(): Section[] {
  try {
    const raw: unknown = JSON.parse(localStorage.getItem(RECENT) ?? '[]')
    const known: readonly unknown[] = APP_ORDER
    return Array.isArray(raw) ? raw.filter((s): s is Section => known.includes(s)) : []
  } catch {
    return []
  }
}

export class Views {
  /** Replaced whole as each arrives, so reading one key is reactive. */
  #views = $state.raw<Partial<Record<ViewKey, Component>>>({})
  #navs = $state.raw<Partial<Record<NavKey, Component>>>({})
  #failed = $state.raw<Partial<Record<ViewKey, true>>>({})
  #loading = new Map<ViewKey, Promise<void>>()
  #warmed = false
  #load: (key: ViewKey) => Promise<Chunks>

  /** `load` is the chunks' own `import()`s; the tests hand in their own. */
  constructor(load: (key: ViewKey) => Promise<Chunks> = fetchChunks) {
    this.#load = load
  }

  /**
   * The app whose view and sidebar are drawn: `app.section`, once what it
   * draws has loaded. `App.svelte` sets it at start and keeps it following
   * through `follow`; the app bar and the window's title follow
   * `app.section` itself, at once.
   */
  stage = $state<Section>('journal')

  /** The view for `key`, once loaded. */
  view(key: ViewKey): Component | undefined {
    return this.#views[key]
  }

  /** The sidebar for `section`, once loaded; `undefined` for one with none of its own. */
  nav(section: Section): Component | undefined {
    return hasNav(section) ? this.#navs[section] : undefined
  }

  /** The last load of `key` failed, and no other has started since. */
  failed(key: ViewKey): boolean {
    return this.#failed[key] === true
  }

  #ready(key: ViewKey): boolean {
    return !!this.#views[key] && (!hasNav(key) || !!this.#navs[key])
  }

  /**
   * Load `key`'s view, and its sidebar with it. Resolves once both are in,
   * or once loading has failed -- never rejects, so a caller can always go
   * on to show the app (`app.goTo`), and the stage says what went wrong.
   *
   * Untracked: it is called from effects, and what it reads to decide is not
   * something they should run again for.
   */
  ensure(key: ViewKey): Promise<void> {
    return untrack(() => this.#ensure(key))
  }

  #ensure(key: ViewKey): Promise<void> {
    if (this.#ready(key)) return Promise.resolve()
    let loading = this.#loading.get(key)
    if (!loading) {
      // A failure is about the load that failed: left standing, the stage
      // would say this one had failed while it is still on its way.
      this.#forget(key)
      loading = this.#load(key)
        .then(([view, nav]) => {
          this.#views = { ...this.#views, [key]: view.default }
          if (nav && hasNav(key)) this.#navs = { ...this.#navs, [key]: nav.default }
        })
        .catch((e: unknown) => {
          console.error(`The ${key} view could not be loaded`, e)
          this.#failed = { ...this.#failed, [key]: true }
        })
        .finally(() => this.#loading.delete(key))
      this.#loading.set(key, loading)
    }
    return loading
  }

  /** Ask again for a view that failed to load. */
  retry(key: ViewKey): void {
    void this.ensure(key)
  }

  #forget(key: ViewKey) {
    if (!this.#failed[key]) return
    const failed = { ...this.#failed }
    delete failed[key]
    this.#failed = failed
  }

  /**
   * Move the stage to `section`: at once if it has loaded, otherwise when it
   * has or after `STAGE_WAIT_MS`, whichever is first. Returns what stops the
   * wait, for when the section changes again before it is over.
   */
  follow(section: Section): (() => void) | undefined {
    if (this.stage === section) return
    if (this.#ready(section)) {
      this.#show(section)
      return
    }
    let done = false
    const show = () => {
      if (done) return
      done = true
      this.#show(section)
    }
    const timer = setTimeout(show, STAGE_WAIT_MS)
    void this.ensure(section).then(show)
    return () => {
      done = true
      clearTimeout(timer)
    }
  }

  #show(section: Section) {
    this.stage = section
    // The main window's choices alone: a window of one app shows nothing
    // else, and would crowd out the apps the main window does switch to.
    if (pinned) return
    try {
      const list = [section, ...recent().filter((s) => s !== section)].slice(0, RECENT_KEPT)
      localStorage.setItem(RECENT, JSON.stringify(list))
    } catch {
      // Storage refused: nothing is fetched ahead next time, which is all.
    }
  }

  /**
   * Fetch the apps used most lately, one at a time and only when the window
   * is idle, so switching to them is instant. Once per window, after the
   * first app has drawn; not in a window of one app, which switches to none.
   */
  warm(showing: Section) {
    if (this.#warmed || pinned) return
    this.#warmed = true
    const queue = recent()
      .filter((s) => s !== showing)
      .slice(0, WARMED)
    const next = () => {
      const section = queue.shift()
      if (section) whenIdle(() => void this.ensure(section).then(next))
    }
    next()
  }
}

export const views = new Views()
