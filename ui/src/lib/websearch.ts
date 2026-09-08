// Web search, for any component that wants it.
//
// The interface half of `everyday_core::websearch`. The Rust side builds
// every URL and parses every reply; the Tauri commands open the socket; this
// file is what components actually hold, and it exists so that the four
// things every caller of a network search needs -- debouncing, cancellation,
// caching and an error you can render -- are written once.
//
// Two shapes, because there are two kinds of caller:
//
//   web.search(...)            one-shot. A button was pressed.
//   web.live(onResult)         as-you-type. Debounced, and every answer that
//                              is not the latest is discarded.
//
// The second is the one that matters. Without it, typing "dune" fires four
// requests and the answer to "dun" can arrive after the answer to "dune" and
// win -- a race that shows up as a results list flickering to the wrong thing
// and staying there.

import { api } from './api'
import type { KindId, SearchRequest, SearchResult, SearchSource, SourceInfo } from './types'
import { VaultError } from './types'

/**
 * The vault locking under a search, as opposed to the search failing.
 *
 * Defined here rather than imported from `state.svelte.ts`, which is what
 * `autosave.ts` does and for a related reason: this module is a *facility*,
 * usable by anything, and importing the application store would mean nothing
 * could use web search without pulling the entire vault state machine in
 * behind it. Two three-line functions are a smaller price than that
 * dependency, and the lock *policy* -- what to do about it -- still lives in
 * one place, because callers hand it to `handle`.
 */
function isLocked(e: unknown): boolean {
  return e instanceof VaultError && e.code === 'locked'
}

function errorMessage(e: unknown): string {
  if (e instanceof Error) return e.message
  return String(e)
}

/**
 * What a source is called, from the slug an item or a shelf stores.
 *
 * Mirrors `Source::label` in the core. A four-line map rather than a round
 * trip through `searchSources()`, because this is wanted while drawing a
 * detail panel and "Filled in from openLibrary" is the sort of thing that
 * ships if the alternative is asynchronous.
 */
export function sourceLabel(slug: string): string {
  switch (slug) {
    case 'wikipedia':
      return 'Wikipedia'
    case 'openLibrary':
      return 'Open Library'
    case 'itunes':
      return 'iTunes'
    case 'nominatim':
      return 'OpenStreetMap'
    default:
      return 'the web'
  }
}

/** How long to wait after the last keystroke before asking the network. */
export const SEARCH_DEBOUNCE_MS = 420

/** How many answers to keep. Small: this is a convenience, not a database. */
const CACHE_LIMIT = 32

/** What a caller may ask for. Everything but the query has a default. */
export interface SearchOptions {
  /** Which source. Omit for a plain web search. */
  source?: SearchSource
  /** A `Kind.slug`, which lets a source pick the right media filter. */
  hint?: string
  limit?: number
}

/** An outcome a component can render without knowing what went wrong. */
export interface SearchOutcome {
  query: string
  results: SearchResult[]
  /** Human-readable, ready to put on screen. Null when nothing went wrong. */
  error: string | null
}

/** The same, plus whether another answer is still coming. */
export type LiveOutcome = SearchOutcome & { searching: boolean }

function keyOf(request: SearchRequest): string {
  return `${request.source} ${request.hint} ${request.limit} ${request.query}`
}

class WebSearch {
  /**
   * Answers already fetched this session, oldest first.
   *
   * A plain capped `Map`, cleared when the vault locks. Two reasons for the
   * clearing, and the second is the important one: a search result is a
   * blurb about something the person is interested in, which is exactly the
   * sort of thing a lock screen is meant to put away.
   */
  private cache = new Map<string, SearchResult[]>()
  /** Requests in flight, so two components asking at once cost one request. */
  private inflight = new Map<string, Promise<SearchResult[]>>()
  private sourceList: Promise<SourceInfo[]> | null = null

  /** Drop everything held. Registered with `app.onLock` by the library store. */
  clear() {
    this.cache.clear()
    this.inflight.clear()
  }

  /** Fill in the defaults a caller did not care about. */
  request(query: string, options: SearchOptions = {}): SearchRequest {
    return {
      query: query.trim(),
      source: options.source ?? 'web',
      hint: options.hint ?? '',
      limit: options.limit ?? 8,
    }
  }

  /**
   * Run one search, reusing an identical one already asked or in flight.
   *
   * Rejects for one reason only: a vault that locked under us, which is
   * routed to the lock screen like every other call in the app. Everything
   * else comes back as `error`, because every caller of this wants to put
   * the reason on screen beside an empty list rather than unwind.
   */
  async search(query: string, options: SearchOptions = {}): Promise<SearchOutcome> {
    const request = this.request(query, options)
    if (!request.query) return { query: '', results: [], error: null }
    return this.run(keyOf(request), request.query, () => api.webSearch(request))
  }

  /**
   * Look a title up using whatever source a shelf prefers.
   *
   * Distinct from `search` because the *policy* -- which source, and the
   * fallback to a plain web search when that source draws a blank -- belongs
   * to the Rust core, where it is one list that both of its callers walk.
   * See `SearchRequest::attempts`.
   */
  async lookup(kindId: KindId, query: string, limit?: number): Promise<SearchOutcome> {
    const trimmed = query.trim()
    if (!trimmed) return { query: '', results: [], error: null }
    return this.run(`lookup ${kindId} ${limit ?? ''} ${trimmed}`, trimmed, () =>
      api.lookupMetadata(kindId, trimmed, limit),
    )
  }

  /**
   * A debounced searcher for a field somebody is typing into.
   *
   * `onResult` is called with the outcome of the *latest* query only. An
   * answer that arrives after a newer query has been asked is dropped rather
   * than rendered, which is the whole reason this is a class and not a
   * function: without the generation counter, a slow answer to "dun" lands
   * after the fast answer to "dune" and the list ends up showing sand dunes.
   *
   * ```ts
   * const search = web.live((outcome) => (hits = outcome.results))
   * search.type(value, { source: 'openLibrary' }) // in an input handler
   * search.stop() // on unmount
   * ```
   */
  live(onResult: (outcome: LiveOutcome) => void): LiveSearch {
    return new LiveSearch(this, onResult)
  }

  /** The sources the picker offers. Fetched once and kept. */
  async sources(): Promise<SourceInfo[]> {
    if (!this.sourceList) this.sourceList = api.searchSources().catch(() => [])
    return this.sourceList
  }

  private async run(
    key: string,
    query: string,
    fetch: () => Promise<SearchResult[]>,
  ): Promise<SearchOutcome> {
    const cached = this.cache.get(key)
    if (cached) return { query, results: cached, error: null }
    try {
      let pending = this.inflight.get(key)
      if (!pending) {
        pending = fetch()
        this.inflight.set(key, pending)
      }
      const results = await pending
      this.remember(key, results)
      return { query, results, error: null }
    } catch (e) {
      if (isLocked(e)) throw e
      return { query, results: [], error: errorMessage(e) }
    } finally {
      this.inflight.delete(key)
    }
  }

  private remember(key: string, results: SearchResult[]) {
    this.cache.set(key, results)
    // Oldest out first. `Map` iterates in insertion order, so the first key
    // is the least recently *added* -- good enough for a session cache, and a
    // proper LRU would be more bookkeeping than the thing is worth.
    while (this.cache.size > CACHE_LIMIT) {
      const oldest = this.cache.keys().next().value
      if (oldest === undefined) break
      this.cache.delete(oldest)
    }
  }
}

/** A debounced, race-free searcher bound to one field. See `WebSearch.live`. */
export class LiveSearch {
  private timer: ReturnType<typeof setTimeout> | null = null
  /** Bumped per query; an answer from an older generation is discarded. */
  private generation = 0
  private stopped = false

  constructor(
    private readonly web: WebSearch,
    private readonly onResult: (outcome: LiveOutcome) => void,
  ) {}

  /** Somebody typed. Schedules a search, replacing any already scheduled. */
  type(query: string, options: SearchOptions = {}) {
    this.schedule(query, () => this.web.search(query, options))
  }

  /** The same, for a shelf-aware lookup. */
  lookup(kindId: KindId, query: string, limit?: number) {
    this.schedule(query, () => this.web.lookup(kindId, query, limit))
  }

  /** Search now, without waiting out the debounce. What Enter does. */
  now(query: string, run: () => Promise<SearchOutcome>) {
    if (this.timer) clearTimeout(this.timer)
    this.timer = null
    void this.execute(query, run)
  }

  /** Cancel anything pending and ignore anything still in flight. */
  stop() {
    if (this.timer) clearTimeout(this.timer)
    this.timer = null
    // Nothing already awaited can be cancelled, but everything can be
    // *ignored*, which is the part that matters for correctness.
    this.generation++
    this.stopped = true
  }

  private schedule(query: string, run: () => Promise<SearchOutcome>) {
    if (this.timer) clearTimeout(this.timer)
    this.timer = null
    if (!query.trim()) {
      // An empty field clears at once. Waiting out the debounce to show
      // nothing is the one case where the delay is felt purely as lag.
      this.generation++
      this.onResult({ query: '', results: [], error: null, searching: false })
      return
    }
    this.onResult({ query, results: [], error: null, searching: true })
    this.timer = setTimeout(() => {
      this.timer = null
      void this.execute(query, run)
    }, SEARCH_DEBOUNCE_MS)
  }

  private async execute(query: string, run: () => Promise<SearchOutcome>) {
    const generation = ++this.generation
    this.onResult({ query, results: [], error: null, searching: true })
    try {
      const outcome = await run()
      // The guard this class exists for.
      if (generation !== this.generation || this.stopped) return
      this.onResult({ ...outcome, searching: false })
    } catch (e) {
      if (generation !== this.generation || this.stopped) return
      this.onResult({ query, results: [], error: errorMessage(e), searching: false })
    }
  }
}

/** The one instance. Import this; there is no reason for a second. */
export const web = new WebSearch()
