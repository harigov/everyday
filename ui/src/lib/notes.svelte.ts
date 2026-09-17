// State for the notes app.
//
// The simplest of the app stores: a list, one open note, and a tag filter.
// What is worth knowing is what it does *not* do.
//
// It does not own the editor. `Editor.svelte` takes a document and a callback
// and knows nothing about which record it came from, so the journal and this
// app draw the same editor over the same rich text with the same media
// pipeline, and a photograph dropped into a note goes through exactly the
// path a photograph dropped into an entry goes through.
//
// It does not own its own search either. `/` in here narrows the vault's one
// index to notes; the same box in the journal narrows it to entries. There is
// one index and one ranking, because a half-remembered phrase should be found
// wherever it was written down.

import { api, newRequestId } from './api'
import { Autosave } from './autosave'
import { registerApply, type ChangeWithIds } from './live-apply'
import { proposals, recordAs } from './proposals.svelte'
import { app, handle, isConflict, isLocked } from './state.svelte'
import { debounce } from './store/debounce'
import { DocBinding } from './store/doc-binding'
import { latest } from './store/latest'
import { applySingleChange, patchOneFromChange } from './store/live-patch'
import { guardedRefresh } from './store/refresh'
import type { Note, NoteHit, NoteId, NoteSort, NoteSummary, Proposal, ProposalId } from './types'

/** How many notes a list loads at once. A drawer, not a database. */
const PAGE = 500

/** How long after a keystroke the open note is written. */
const SAVE_MS = 700

/** How the list is ordered, in the order the picker offers. */
export const SORTS: { value: NoteSort; label: string }[] = [
  { value: 'updatedDesc', label: 'Last changed' },
  { value: 'createdDesc', label: 'Newest' },
  { value: 'titleAsc', label: 'By title' },
]

class NotesState {
  /** The list, pinned first. */
  list = $state<NoteSummary[]>([])
  /** The note in the editor, whole. `null` when nothing is open. */
  open = $state<Note | null>(null)
  selected = $state<NoteId | null>(null)
  /**
   * The pending note proposal open in place of a real one, by id -- so
   * answering it anywhere closes this pane too. See `todo`'s
   * `#selectedProposalId`.
   */
  #selectedProposalId = $state<ProposalId | null>(null)
  sort = $state<NoteSort>('updatedDesc')
  /** `null` is every note; a tag narrows the list to notes carrying it. */
  tag = $state<string | null>(null)
  tags = $state<string[]>([])
  loading = $state(false)
  /** Search inside this app. Narrows the vault's index to notes. */
  query = $state('')
  results = $state<NoteHit[]>([])
  searching = $state(false)

  /**
   * The `updatedAt` the open note was read at.
   *
   * What makes the save conditional. Two windows on one vault, or a window
   * and the assistant writing a report, will meet here rather than one of
   * them losing its work silently.
   */
  #base: string | null = null
  /**
   * The id of the write currently being attempted, held across its retries.
   *
   * Minted once per *logical* write and reused by every retry of it, which is
   * the whole contract `newRequestId` documents. It was minted inside
   * `api.saveNote` instead, so every retry carried a fresh one, the backend's
   * idempotency map never recognised the second attempt as the first, and a
   * save whose answer was lost to a dropped connection came back as a conflict
   * against the version it had itself just written.
   *
   * Cleared on success, and by `#edited` when the text moves under it -- a
   * retry of superseded text must not be answered from the record, because the
   * record holds the older version.
   */
  #stamp: string | null = null
  /** The search box's debounce. See `setQuery`. */
  #search = debounce((q: string) => void this.#runSearch(q), 140)
  /** How to ask the editor for its document. See `bindBody`. */
  #body = new DocBinding<Note['body']>()
  /** Which load is the current one. See `refresh`. */
  #generation = latest()

  #saver = new Autosave<NoteId>(async () => {
    const note = this.open
    if (!note) return
    // The one place the editor's document is walked. See `bindBody`.
    this.syncBody()
    // Nothing is written while a conflict is unresolved. Retrying would only
    // be refused again, and the author has a choice in front of them -- so
    // without this, every pause in typing under the banner cost another
    // doomed round trip. The same guard the journal's `#write` has.
    if (this.conflict) return
    // The version this write is *sending*, captured before the await. Read
    // back afterwards it would be whatever the editor had reached by then: a
    // keystroke landing during the round trip stamps a new `updatedAt` on the
    // same live object, `#base` would take that value, and the next save would
    // send a version the vault has never held -- a conflict banner over an
    // edit nobody else touched. This is what `#writeStamp` guards in the
    // journal, for the same reason.
    const sending = note.updatedAt
    this.#stamp ??= newRequestId()
    try {
      await api.saveNote($state.snapshot(note), this.#base, this.#stamp)
      this.#stamp = null
      this.#base = sending
      await this.refresh()
    } catch (e) {
      if (isConflict(e)) {
        // The same policy the journal has: say so and stop writing. Silently
        // winning would throw away whatever the other writer did, and a retry
        // would only be refused again.
        this.conflict = true
        return
      }
      // Reported *and* rethrown. `Autosave` clears its dirty set before
      // awaiting and only puts the ids back if this rejects, so swallowing a
      // transient failure here would report "saved" for a write that did not
      // land and never try again. See `autosave.ts`.
      await handle(e)
      throw e
    }
  }, SAVE_MS)

  /** True while an edit is refusing to save because somebody else wrote. */
  conflict = $state(false)

  constructor() {
    app.onLock(() => this.reset())
    app.onFlush(() => this.flush())
    // `live.svelte.ts`'s opt-in -- see `#applyChanges`. First user of it, so
    // a change from another window patches this list instead of reloading it
    // whole.
    registerApply('notes', (changes) => this.#applyChanges(changes))
    // This window's own accept does not come back as a change event -- see
    // `proposals.svelte.ts` -- so the store that just gained a note tells
    // itself to reload.
    proposals.onAccepted('note', () => this.refresh())
  }

  reset() {
    // Nothing loaded before the lock may land after it.
    this.#generation.next()
    this.list = []
    this.open = null
    this.selected = null
    this.#selectedProposalId = null
    this.tag = null
    this.tags = []
    this.clearSearch()
    this.conflict = false
    this.#base = null
  }

  /**
   * Load the list. Called by `NotesNav`, which is where the list is drawn.
   *
   * Named and shaped as every other app store's entry point: idempotent, so
   * switching back into the app refreshes it rather than blanking it on the
   * way in, and doing nothing of its own beyond showing that a first load is
   * under way.
   */
  async start() {
    if (!app.supportsNotes) return
    // See `todo.start` for why this is idempotent and safe to ask twice.
    if (!proposals.loaded) await proposals.refresh()
    this.loading = true
    try {
      await this.refresh()
    } finally {
      this.loading = false
    }
  }

  /**
   * Reload the list without the loading state, after a write.
   *
   * Generation-guarded like the todo and library lists: switching the tag
   * filter twice in quick succession put two of these in the air at once,
   * and nothing here stopped the older answer from landing last and sitting
   * on screen showing the wrong tag's notes.
   */
  async refresh() {
    if (!app.supportsNotes) return
    await guardedRefresh(
      this.#generation,
      async (isCurrent) => {
        // Each lands as it arrives rather than both at the end: the notes
        // and the tag sidebar are two calls, and holding a good list back
        // until the second one answers means a failed `noteTags` throws
        // away notes that loaded perfectly well, leaving the previous list
        // -- or nothing at all, on a first load -- under an error banner.
        const list = await api.notes({
          tags: this.tag ? [this.tag] : [],
          sort: this.sort,
          limit: PAGE,
        })
        if (!isCurrent()) return
        this.list = list

        const tags = await api.noteTags()
        if (!isCurrent()) return
        this.tags = tags
      },
      { onError: (e) => (isLocked(e) ? undefined : handle(e)) },
    )
  }

  /**
   * `live.svelte.ts`'s opt-in: patch the list instead of reloading it whole.
   *
   * Handles exactly the shape that is common and cheap to get right -- a
   * single write, created or updated or deleted. Anything wider -- several
   * ids in one batch, a kind of change this has not been taught -- answers
   * `false`, and `live` falls back to `refresh()`, which is always correct
   * if slower.
   */
  #applyChanges(changes: ChangeWithIds[]): boolean {
    return applySingleChange(changes, {
      onDeleted: (id) => {
        this.list = this.list.filter((n) => n.id !== id)
      },
      // Answered synchronously -- see `Applier` -- with the fetch running
      // after. `#patchOne` falls back to `refresh()` itself if it fails, so
      // nothing here has to wait for it to decide.
      onUpserted: (id) => void this.#patchOne(id),
    })
  }

  /** The fetch-and-patch `#applyChanges` starts and does not wait for. */
  async #patchOne(id: NoteId) {
    await patchOneFromChange({
      fetch: () => api.note(id),
      apply: (note) => {
        if (this.tag && !note.tags.includes(this.tag)) {
          // No longer -- or never -- under the tag this list is filtered to.
          // Drop it if it was showing under an earlier tag; do nothing if it
          // was never in this list to begin with.
          this.list = this.list.filter((n) => n.id !== id)
          return
        }
        const summary = summarize(note)
        const at = this.list.findIndex((n) => n.id === id)
        // Replaced in place when already shown, so its position in the sort
        // order is left alone rather than guessed at; inserted at the front
        // otherwise, which is right for the default "last changed" sort and
        // an approximation everywhere else that the next real `refresh`
        // corrects.
        this.list =
          at >= 0 ? this.list.map((n, i) => (i === at ? summary : n)) : [summary, ...this.list]
      },
      // A lock is not a failure to fall back from -- the same policy
      // everywhere else a background read meets a vault that just shut.
      onLocked: () => {},
      // Fetching the one row failed in some way patching cannot reason
      // about -- ask for all of them rather than risk this list disagreeing
      // with the vault.
      fallback: () => this.refresh(),
    })
  }

  async setSort(sort: NoteSort) {
    this.sort = sort
    await this.refresh()
  }

  async setTag(tag: string | null) {
    this.tag = tag
    await this.refresh()
  }

  async openNote(id: NoteId) {
    // Write what is in the editor before the editor is handed a different
    // document, or a save taken inside the autosave window lands against the
    // note that has just been closed.
    await this.flush()
    this.#selectedProposalId = null
    try {
      const note = await api.note(id)
      this.open = note
      this.selected = id
      this.#base = note.updatedAt
      this.conflict = false
    } catch (e) {
      if (isLocked(e)) return
      await handle(e)
    }
  }

  /** Make one, save it, and open it. */
  async create(title = '') {
    if (!app.supportsNotes) return
    await this.flush()
    try {
      const note = await api.newNote()
      note.title = title
      await api.saveNote(note, null)
      this.open = note
      this.selected = note.id
      this.#base = note.updatedAt
      this.conflict = false
      await this.refresh()
    } catch (e) {
      await handle(e)
    }
  }

  /**
   * Stamp the open note as changed and put it on the autosave timer.
   *
   * The four editors below all did this by hand, and each had to remember the
   * `updatedAt` line as well as the `touch`. Now that a request id has to be
   * retired here too -- see `#stamp` -- one of the four forgetting a line
   * would be a write answered from the record with the *older* text in it.
   */
  #edited() {
    if (!this.open) return
    this.open.updatedAt = new Date().toISOString()
    // The text has moved, so any write still being retried is superseded and
    // must not be answered as though it were this one.
    this.#stamp = null
    this.#saver.touch(this.open.id)
  }

  /**
   * Register (or with `null`, retire) the editor's document getter.
   *
   * The same arrangement the journal has, and adopted here for the same
   * measured reason. This app used to take the *document* on every change --
   * `edited(body)`, called from the editor's `onUpdate` -- which walked the
   * whole of ProseMirror's tree and rebuilt it as JSON once per character.
   * That is work proportional to everything already written, paid on every
   * keystroke, so a note grew slower to type into the longer it got: on a
   * two-hundred-thousand-character note it was some seven times what the
   * journal spent in the same callback, and the journal was drawing the same
   * editor over the same text.
   *
   * So the store keeps a way to *ask* instead, and asks once per save.
   */
  bindBody(fn: (() => Note['body']) | null) {
    this.#body.bind(fn)
  }

  /**
   * Pull the editor's current document into the open note.
   *
   * Called before every write and by the editor on the way out, which is
   * what makes it safe for the note in memory to lag the caret in between.
   */
  syncBody() {
    const body = this.#body.read()
    if (this.open && body !== undefined) this.open.body = body
  }

  /** Called by the editor on every change. The document is left where it is. */
  edited() {
    this.#edited()
  }

  setTitle(title: string) {
    if (!this.open) return
    this.open.title = title
    this.#edited()
  }

  async setTags(tags: string[]) {
    if (!this.open) return
    this.open.tags = tags
    this.#edited()
  }

  async togglePinned() {
    if (!this.open) return
    this.open.pinned = !this.open.pinned
    this.#edited()
    await this.flush()
  }

  /**
   * Take this window's copy, whatever the other writer did.
   *
   * The deliberate resolution of the conflict above, and the only path that
   * goes through the unconditional save.
   */
  async keepMine() {
    const note = this.open
    if (!note) return
    // Not on the autosave path, so it does its own pull -- this is a save
    // like any other and must send what is on screen. See `bindBody`.
    this.syncBody()
    try {
      await api.saveNoteForce(note)
      this.#base = note.updatedAt
      this.conflict = false
      await this.refresh()
    } catch (e) {
      await handle(e)
    }
  }

  /** Take theirs: throw away this window's edit and reload. */
  async takeTheirs() {
    const id = this.open?.id
    if (!id || !this.conflict) return
    this.#saver.forget(id)
    this.conflict = false
    // Dropped before it is re-opened, so the editor is rebuilt rather than
    // left exactly as it was. `RichText` replaces its document only when
    // `docId` changes, and re-opening the same note does not change it -- so
    // without this line the pane goes on showing the text this window has
    // just agreed to discard, while `#base` moves to the other writer's
    // version. The next keystroke would then overwrite them, with no second
    // conflict to stop it. The journal's `takeTheirs` drops `entry` for
    // exactly this reason.
    this.open = null
    await this.openNote(id)
  }

  async remove(id: NoteId) {
    this.#saver.forget(id)
    try {
      await api.deleteNote(id)
      if (this.selected === id) {
        this.open = null
        this.selected = null
        this.#base = null
      }
      await this.refresh()
    } catch (e) {
      await handle(e)
    }
  }

  async flush() {
    await this.#saver.flush()
  }

  setQuery(q: string) {
    this.query = q
    if (!q.trim()) {
      this.#search.cancel()
      this.results = []
      this.searching = false
      return
    }
    this.searching = true
    this.#search.call(q)
  }

  async #runSearch(q: string) {
    try {
      const hits = await api.search(q, null, 50, 'note')
      // Narrowed rather than cast: the rows drawn from this have to
      // genuinely carry the fields they read.
      this.results = hits.filter((h): h is NoteHit => h.type === 'note')
    } catch (e) {
      await handle(e)
    } finally {
      this.searching = false
    }
  }

  clearSearch() {
    // The armed timer goes too, or a search typed a moment ago lands after
    // the box was emptied and refills a list that was cleared on purpose.
    this.#search.cancel()
    this.query = ''
    this.results = []
    this.searching = false
  }

  /** What the window title and the header show for the open note. */
  get title(): string {
    const note = this.open
    if (!note) return ''
    if (note.title.trim()) return note.title.trim()
    return 'Untitled note'
  }

  // ── proposals ────────────────────────────────────────────────────────
  //
  // Ghosts sit at the top of the list, not the foot -- a note is written
  // forward from nothing, so there is no "section" for a proposed one to
  // join the way a task joins a due date. See `docs/plans/dreaming.md`.

  /** Pending `create` proposals for a note. */
  get ghostNotes(): Proposal[] {
    return proposals.forKind('note').filter((p) => p.payload.type === 'create')
  }

  /** The pending `replace` proposal for a note, drawn beneath its row. */
  replaceProposalFor(id: NoteId): Proposal | null {
    return (
      proposals
        .forKind('note')
        .find((p) => p.payload.type === 'replace' && recordAs(p, 'note')?.id === id) ?? null
    )
  }

  /** The pending `delete` proposal for a note, drawn as a chip on its row. */
  deleteProposalFor(id: NoteId): Proposal | null {
    return (
      proposals.forKind('note').find((p) => p.payload.type === 'delete' && p.payload.id === id) ??
      null
    )
  }

  /**
   * The pending note proposal open in the pane, or `null` once it has been
   * answered from anywhere -- see `#selectedProposalId`'s own doc.
   */
  get selectedProposal(): Proposal | null {
    return this.#selectedProposalId
      ? (proposals.pending.find((p) => p.id === this.#selectedProposalId) ?? null)
      : null
  }

  /** Open a pending note proposal in the pane, in place of a real note. */
  openProposal(p: Proposal) {
    this.selected = null
    this.#selectedProposalId = p.id
  }

  closeProposal() {
    this.#selectedProposalId = null
  }
}

export const notes = new NotesState()

/** Plain text out of a note's rich body -- what a read-only preview draws. */
export function previewText(body: Note['body']): string {
  return plainText(body)
}

/**
 * The list's condensed shape, computed from a record fetched whole.
 *
 * Close enough to what the backend computes for `listNotes` that a row
 * patched in by `#patchOne` looks right -- plain text out of the rich
 * document for the excerpt and the word count, the first image attachment as
 * the cover. Not identical by construction, because the alternative is a
 * backend command that answers with one record's summary, which nothing
 * needs badly enough yet to justify -- and the mock backend already carries
 * the same approximation for the same reason (`mock.ts`'s own `summarize`).
 */
function summarize(note: Note): NoteSummary {
  const text = plainText(note.body)
  return {
    id: note.id,
    title: note.title,
    excerpt: text.slice(0, 240),
    tags: note.tags,
    pinned: note.pinned,
    purpose: note.purpose,
    wordCount: text.split(/\s+/).filter(Boolean).length,
    attachmentCount: note.attachments.length,
    cover: note.attachments.find((a) => a.kind === 'image')?.blob,
    createdAt: note.createdAt,
    updatedAt: note.updatedAt,
  }
}

/** Every word a rich document holds, media captions included, space-joined. */
function plainText(node: unknown): string {
  if (!node || typeof node !== 'object') return ''
  const n = node as Record<string, unknown>
  if (n.type === 'text') return typeof n.text === 'string' ? n.text : ''
  const attrs = n.attrs as Record<string, unknown> | undefined
  const caption = attrs?.caption
  const own = n.type === 'media' && typeof caption === 'string' ? caption : ''
  const kids = Array.isArray(n.content) ? n.content.map(plainText).join(' ') : ''
  return [own, kids].filter(Boolean).join(' ')
}
